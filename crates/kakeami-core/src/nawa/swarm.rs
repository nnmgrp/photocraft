//! Strip Poisson-Disc `Swarm` cache & splice-tail emitter for `nawa-ami` (v7.4b).
//! 1:1 port of `buildStreamSwarmCache` and `emitSwarmTailSpan` from `src/02-geometry.js`
//! (lines 2575–2957, 3391–3503).
//!
//! Strictly `f64` math (narrowing output cache to `f32` matching JS `Float32Array`),
//! zero `hypot`, zero `static mut`, fully `Send + Sync`.

use crate::config::KakeamiConfig;
use crate::density::sample_density;
use crate::geometry::{emit_dot_capsule, Builder};
use crate::grid::{Grid, Rect};
use crate::math::{clamp, hash2d, js_round, mod_i, smoothstep, Mulberry32, ValueNoise2D};
use crate::nawa::emit::{compute_chord_joint_overshoots, lane_stroke_width};
use crate::nawa::skeleton::StreamGeom;
use std::f64::consts::PI;

const TAU: f64 = PI * 2.0;
const SWARM_RING_CAP: usize = 24;
const SWARM_HEAD_CAP: usize = 16;

/// Immutable per-stream Strip Poisson-Disc dot cache (computed once per stream via `OnceLock`).
#[derive(Debug, Clone)]
pub(crate) struct StreamSwarmCache {
    pub(crate) chords_start: Vec<usize>,
    /// Flat `[x, y, tx, ty, half_mid, rx, ry]` × `dot_count`, matching JS `Float32Array`.
    pub(crate) data: Vec<f32>,
    pub(crate) seeds: Vec<u32>,
    pub(crate) max_dot_rad: f64,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_stream_swarm_cache(
    cfg: &KakeamiConfig,
    grid: &Grid,
    noise: &ValueNoise2D,
    geom: &StreamGeom,
    stream_seed: u32,
    head_tucked: bool,
    tail_tucked: bool,
    is_grad_mode: bool,
) -> StreamSwarmCache {
    let n_scale = clamp(cfg.nawa_scale, 0.40, 4.50);
    let w_base = cfg.nawa_rope_width * n_scale;
    let k = geom
        .k_strands
        .unwrap_or_else(|| clamp(js_round(cfg.nawa_strands as f64), 2.0, 10.0) as usize);
    let w_verts = geom.w_verts.as_deref();
    let pts = &geom.pts;
    let m = geom.m;
    let is_pale_white = cfg.nawa_tone_style == "paleWhite";
    let join_style = cfg.nawa_join_style.as_str();
    let sugomi = cfg.finish_mode == "sugomi";
    let con_stip = clamp(cfg.stipple_contrast / 100.0, 0.0, 1.0);
    let stip_dot_cfg = cfg.stipple_dot_size;
    let user_scale = cfg.stroke_width / 1.3;
    let con_keep_mul = 0.92 + 0.16 * con_stip;
    let one_bit = cfg.one_bit_mode;
    let min_dot_floor = if one_bit { 0.65 } else { 0.25 };
    let is_closed_loop = geom.is_closed_loop;

    let pitch_base = w_base / k as f64;
    let dm_base = clamp(stip_dot_cfg / (0.3286 * pitch_base).max(1e-3), 0.55, 1.75);
    let slice_s_base = pitch_base * 0.14 * dm_base;

    let stride_m = m + 1;
    let mut chords_start = vec![0usize; k * stride_m];
    if slice_s_base < 1e-3 {
        return StreamSwarmCache {
            chords_start,
            data: Vec::new(),
            seeds: Vec::new(),
            max_dot_rad: 0.0,
        };
    }

    let mut data: Vec<f32> = Vec::new();
    let mut seeds: Vec<u32> = Vec::new();
    let mut dot_count: usize = 0;
    let mut max_dot_rad: f64 = 0.0;

    // Pre-evaluate chord density flags once in grad mode (shared across all K strands)
    let grad_chord_active = if is_grad_mode {
        let mut flags = vec![false; m];
        for cg in 0..m {
            let ch_g = geom.chords[cg];
            let d_ag = sample_density(cfg, grid, pts[cg].x, pts[cg].y, noise);
            let d_mg = sample_density(cfg, grid, ch_g.mx, ch_g.my, noise);
            let d_bg = sample_density(cfg, grid, pts[cg + 1].x, pts[cg + 1].y, noise);
            if d_ag.max(d_mg).max(d_bg) > 0.005 && d_ag.min(d_mg).min(d_bg) < 0.56 {
                flags[cg] = true;
            }
        }
        Some(flags)
    } else {
        None
    };

    // Stack-local ring & loop-head buffers (zero global mutable state)
    let mut ring_s = [0.0f64; SWARM_RING_CAP];
    let mut ring_v = [0.0f64; SWARM_RING_CAP];
    let mut ring_r = [0.0f64; SWARM_RING_CAP];
    let mut head_s = [0.0f64; SWARM_HEAD_CAP];
    let mut head_v = [0.0f64; SWARM_HEAD_CAP];
    let mut head_r = [0.0f64; SWARM_HEAD_CAP];

    for sj in 0..k {
        let is_rim0 = sj == 0;
        let is_rim1 = sj == k - 1;
        let is_rim_any = is_rim0 || is_rim1;
        let stream_hash_seed = stream_seed ^ ((sj as u32 + 1).wrapping_mul(0x85eb_ca6b));
        let slice_seed = stream_hash_seed ^ 0x51a7_f319;
        let h2_seed = stream_hash_seed ^ 0x27d4_eb2f;
        let sub_seed = stream_hash_seed ^ 0x9e37_79b1;
        let wob_phase =
            ((hash2d(stream_seed ^ 0x51a7_e901, sj as i32 + 1, 3) & 0xffff) as f64 / 65535.0) * TAU;
        let wob_amp = if !is_grad_mode {
            pitch_base * if is_rim_any { 0.08 * 0.35 } else { 0.08 }
        } else {
            0.0
        };
        let strand_off = sj * stride_m;

        // Exact closed-loop strand arc so slice_s divides total_strand_arc evenly
        let mut slice_s = slice_s_base;
        let mut n_loop_slices: i64 = 0;
        let mut total_strand_arc: f64 = 0.0;
        if is_closed_loop {
            for c_tot in 0..m {
                let ch_t = geom.chords[c_tot];
                let w_ch_t = match w_verts {
                    Some(wv) => 0.5 * (wv[c_tot] + wv[c_tot + 1]),
                    None => w_base,
                };
                let p_t = w_ch_t / k as f64;
                let v_t = (-0.5 + (sj as f64 + 0.5) / k as f64) * w_ch_t;
                let a0_t = v_t * geom.half_tan[c_tot];
                let a1_t = ch_t.len - v_t * geom.half_tan[c_tot + 1];
                let sp_t = if a1_t < a0_t { 0.14 * p_t } else { a1_t - a0_t };
                if sp_t > 0.0 {
                    total_strand_arc += sp_t;
                }
            }
            if total_strand_arc > slice_s_base * 4.0 {
                n_loop_slices = (js_round(total_strand_arc / slice_s_base) as i64).max(24);
                slice_s = total_strand_arc / n_loop_slices as f64;
            }
        }
        let wob_freq = if is_closed_loop && total_strand_arc > 1e-3 {
            js_round(total_strand_arc / (w_base * 3.8).max(1.0)).max(2.0) * TAU / total_strand_arc
        } else {
            TAU / (w_base * 3.8).max(1.0)
        };

        let mut r_head: usize = 0;
        let mut r_count: usize = 0;
        let mut head_count: usize = 0;
        let mut strand_arc: f64 = 0.0;

        for ci in 0..m {
            chords_start[strand_off + ci] = dot_count;

            let ch = geom.chords[ci];
            let w_ch = match w_verts {
                Some(wv) => 0.5 * (wv[ci] + wv[ci + 1]),
                None => w_base,
            };
            let p_loc = w_ch / k as f64;
            let v_s = (-0.5 + (sj as f64 + 0.5) / k as f64) * w_ch;
            let mut a0 = v_s * geom.half_tan[ci];
            let mut a1 = ch.len - v_s * geom.half_tan[ci + 1];
            if a1 < a0 {
                let mid_m = 0.5 * (a0 + a1);
                let min_h = 0.07 * p_loc;
                a0 = mid_m - min_h;
                a1 = mid_m + min_h;
            }
            let mut s0 = a0;
            let mut s1 = a1;
            let s_ref = 0.5 * (a0 + a1);
            let mut r_wid = 0.5;
            let mut stx = ch.tx;
            let mut sty = ch.ty;
            let mut snx = ch.nx;
            let mut sny = ch.ny;

            if is_grad_mode {
                let s_seed_b = stream_seed
                    ^ ((ci as u32 + 1).wrapping_mul(0x9e37_79b1))
                    ^ ((sj as u32 + 1).wrapping_mul(0x85eb_ca6b));
                let mut srng_b = Mulberry32::new(s_seed_b);
                let _r_keep = srng_b.next_f64();
                let r_ang = srng_b.next_f64();
                let _r_bow = srng_b.next_f64();
                let r_len_jit_b = srng_b.next_f64();
                r_wid = srng_b.next_f64();
                let sp_b = compute_chord_joint_overshoots(geom, ci, w_ch, k, p_loc, join_style);
                let is_hd_tk_g = !is_closed_loop && head_tucked && ci == 0;
                let is_tl_tk_g = !is_closed_loop && tail_tucked && ci == m - 1;
                if !is_hd_tk_g {
                    s0 -= sp_b.ov0[sj] * (1.0 + (r_len_jit_b - 0.5) * 0.08);
                }
                if !is_tl_tk_g {
                    s1 += sp_b.ov1[sj] * (1.0 + (r_len_jit_b - 0.5) * 0.08);
                }

                // Inline strokeFrame for grad mode
                let lock_no_tilt = join_style == "smooth-60l" && is_rim_any;
                if !lock_no_tilt {
                    let mut tilt_deg = 0.0;
                    if join_style == "crossed" {
                        let sgn = if (ci & 1) != 0 { 1.0 } else { -1.0 };
                        tilt_deg = sgn * 5.8 + (r_ang - 0.5) * 2.0 * if sugomi { 2.0 } else { 0.9 };
                    } else if join_style == "open" {
                        tilt_deg = (r_ang - 0.5) * 2.0 * if sugomi { 3.8 } else { 1.4 };
                    } else if !is_rim_any {
                        let max_abs_d = geom.d_theta[ci].abs().max(geom.d_theta[ci + 1].abs());
                        if max_abs_d < 0.36 {
                            let g_tilt = 1.0 - smoothstep(0.18, 0.36, max_abs_d);
                            let sgn = if (ci & 1) != 0 { 1.0 } else { -1.0 };
                            tilt_deg = sgn * (3.2 * g_tilt) + (r_ang - 0.5) * 1.1 * g_tilt;
                        }
                    }
                    if tilt_deg.abs() > 1e-4 {
                        let rad_t = tilt_deg * PI / 180.0;
                        let ct = rad_t.cos();
                        let st = rad_t.sin();
                        stx = ch.tx * ct - ch.ty * st;
                        sty = ch.tx * st + ch.ty * ct;
                        snx = -sty;
                        sny = stx;
                    }
                }
            }

            let is_head_tk = !is_closed_loop && head_tucked && ci == 0;
            let is_tail_tk = !is_closed_loop && tail_tucked && ci == m - 1;
            let min_span_t = 0.22 * p_loc;
            let tuck_frac = if is_rim_any { 0.36 } else { 0.20 };
            if is_head_tk && (s1 - s0 > ch.len * tuck_frac + min_span_t) {
                s0 += ch.len * tuck_frac;
            }
            if is_tail_tk && (s1 - s0 > ch.len * tuck_frac + min_span_t) {
                s1 -= ch.len * tuck_frac;
            }

            let span_len = s1 - s0;
            if span_len <= 1e-6 {
                continue;
            }
            let arc_end = strand_arc + span_len;

            if let Some(flags) = grad_chord_active.as_ref() {
                if !flags[ci] {
                    strand_arc = arc_end;
                    continue;
                }
            }
            if span_len < 0.035 * p_loc {
                strand_arc = arc_end;
                continue;
            }

            let sl_start = (strand_arc / slice_s).floor() as i64;
            let sl_end = ((arc_end - 1e-9) / slice_s).floor() as i64;
            if sl_end < sl_start {
                strand_arc = arc_end;
                continue;
            }

            let dm_loc = clamp(stip_dot_cfg / (0.3286 * p_loc).max(1e-3), 0.55, 1.75);
            let d_nom_max = if is_grad_mode {
                p_loc * 0.36 * dm_loc
            } else {
                p_loc * 0.30 * dm_loc
            };
            let max_search_s = 1.8 * d_nom_max + slice_s;
            let lock_st = is_head_tk || (!is_closed_loop && ci == 0);
            let lock_en = is_tail_tk || (!is_closed_loop && ci == m - 1);
            let s_clamp_min = if lock_st { s0 } else { s0 - 0.18 * slice_s };
            let s_clamp_max = if lock_en { s1 } else { s1 + 0.18 * slice_s };

            let p0_s = pts[ci];
            let ref_x = p0_s.x + ch.nx * v_s + ch.tx * s_ref;
            let ref_y = p0_s.y + ch.ny * v_s + ch.ty * s_ref;
            let mut record_head = is_closed_loop && ci <= 1 && head_count < SWARM_HEAD_CAP;
            let check_wrap = is_closed_loop && ci + 2 >= m && total_strand_arc > 0.0;

            for slice_idx in sl_start..=sl_end {
                let s_mid_arc = (slice_idx as f64 + 0.5) * slice_s;
                let s_mid_c = if s_mid_arc < strand_arc {
                    strand_arc
                } else if s_mid_arc > arc_end {
                    arc_end
                } else {
                    s_mid_arc
                };
                let s_local_mid = s0 + (s_mid_c - strand_arc);

                let d_loc = sample_density(
                    cfg,
                    grid,
                    ref_x + stx * (s_local_mid - s_ref),
                    ref_y + sty * (s_local_mid - s_ref),
                    noise,
                );
                if d_loc <= 0.005 {
                    continue;
                }

                let (d_nom, w_band, p_keep_base, min_dist_mul, dash_aspect, u_bridge) =
                    if is_grad_mode {
                        if d_loc >= 0.56 {
                            let w_sol_top =
                                lane_stroke_width(cfg, p_loc, d_loc, r_wid, user_scale, false, join_style);
                            let wb = if one_bit {
                                w_sol_top + 0.294 * smoothstep(0.45, 1.15, w_sol_top)
                            } else {
                                w_sol_top
                            };
                            (p_loc * 0.255 * dm_loc * 1.28, wb, 1.0, 0.57, 1.60, 1.0)
                        } else if d_loc >= 0.20 {
                            let w_sol_g =
                                lane_stroke_width(cfg, p_loc, d_loc, r_wid, user_scale, false, join_style);
                            let wb = if one_bit {
                                w_sol_g + 0.294 * smoothstep(0.45, 1.15, w_sol_g)
                            } else {
                                w_sol_g
                            };
                            let u_g = smoothstep(0.20, 0.56, d_loc);
                            let ub = smoothstep(0.38, 0.56, d_loc);
                            let dn = p_loc * (0.200 + 0.055 * u_g) * dm_loc * (1.0 + 0.28 * ub);
                            let da = 1.0 + 0.60 * ub;
                            let pk = clamp(0.84 + 0.16 * smoothstep(0.20, 0.45, d_loc), 0.0, 1.0);
                            let md = (1.14 - 0.30 * smoothstep(0.0, 0.38, d_loc)) * (1.0 - 0.32 * ub);
                            (dn, wb, pk, md, da, ub)
                        } else {
                            let dn = p_loc * (0.180 + 0.020 * (d_loc / 0.20)) * dm_loc;
                            let pk = 0.84 * smoothstep(0.0, 0.20, d_loc).powf(1.15);
                            (dn, dn, pk, 1.14, 1.0, 0.0)
                        }
                    } else if is_pale_white {
                        let dn = p_loc * (0.180 + 0.030 * d_loc) * dm_loc;
                        let u_lin_p = clamp((d_loc - 0.16) / 0.44, 0.0, 1.0);
                        let wb = dn + (p_loc * 0.72 - dn).max(0.0) * u_lin_p;
                        let pk = clamp((0.26 + 0.88 * d_loc) * con_keep_mul, 0.0, 0.85);
                        let md = 1.28 - 0.20 * d_loc;
                        (dn, wb, pk, md, 1.0, 0.0)
                    } else {
                        let dn = p_loc * (0.208 + 0.045 * d_loc) * dm_loc;
                        let u_lin_d = clamp((d_loc - 0.12) / 0.48, 0.0, 1.0);
                        let w_min_d = 1.05 * dn;
                        let wb = w_min_d + (p_loc * 0.84 - w_min_d).max(0.0) * u_lin_d;
                        let pk = clamp((0.46 + 0.92 * d_loc) * con_keep_mul, 0.0, 0.99);
                        let md = 1.14 - 0.36 * d_loc;
                        (dn, wb, pk, md, 1.0, 0.0)
                    };

                let v_limit_nom = (0.5 * w_band - 0.42 * d_nom).max(0.0);
                let v_scatter_lim = if v_limit_nom > 0.08 * p_loc {
                    v_limit_nom
                } else {
                    0.08 * p_loc
                };
                let slice_wrap = if n_loop_slices > 0 {
                    mod_i(slice_idx, n_loop_slices) as i32
                } else {
                    slice_idx as i32
                };

                let k_float = 2.5 + 3.5 * clamp((d_loc - 0.16) / 0.40, 0.0, 1.0);
                let k_base = k_float.floor() as i32;
                let h_sl = hash2d(slice_seed, slice_wrap, sj as i32 + 1);
                let k_trials = k_base
                    + if ((h_sl & 0xffff) as f64 / 65535.0) < (k_float - k_base as f64) {
                        1
                    } else {
                        0
                    };

                let mut v_wob_col = 0.0;
                if wob_amp > 0.0 {
                    let wob_damp = 1.0 - 0.5625 * smoothstep(0.25, 0.60, d_loc);
                    v_wob_col = (s_mid_c * wob_freq + wob_phase).sin() * wob_amp * wob_damp;
                    if (is_rim0 && v_wob_col < 0.0) || (is_rim1 && v_wob_col > 0.0) {
                        v_wob_col = 0.0;
                    }
                }

                let slice_arc0 = slice_idx as f64 * slice_s;
                let jit_fade = 1.0 - u_bridge;
                let v_scale_bridge = if u_bridge > 0.0 {
                    1.0 - 0.15 * u_bridge
                } else {
                    1.0
                };

                for trial in 0..k_trials {
                    let tr_key = (sj as i32 + 1) * 13 + trial;
                    let h1 = hash2d(stream_hash_seed, slice_wrap, tr_key);
                    let s_cand = slice_arc0 + ((h1 & 0xffff) as f64 / 65535.0) * slice_s;
                    if s_cand < strand_arc || s_cand >= arc_end {
                        continue;
                    }

                    let v_cand = ((((h1 >> 16) & 0xffff) as f64 / 65535.0) * 2.0 - 1.0)
                        * v_scatter_lim
                        * v_scale_bridge;
                    let h2 = hash2d(h2_seed, slice_wrap, tr_key + 53);
                    let u_packed = (h2 >> 16) & 0xffff;
                    let r_jit = if u_bridge >= 0.99 {
                        1.0
                    } else {
                        (0.89 + ((u_packed & 0xff) as f64 / 255.0) * 0.22) * 0.998
                    };
                    let dot_d_geom = d_nom * r_jit;
                    let r_geom = 0.5 * dot_d_geom;

                    // Backward ring scan (physical r_geom invariant across DPI & 1-bit/8-bit)
                    let mut conflict = false;
                    let mut idx_r = r_head as isize - 1;
                    for _ in 0..r_count {
                        if idx_r < 0 {
                            idx_r += SWARM_RING_CAP as isize;
                        }
                        let ur = idx_r as usize;
                        let ds = s_cand - ring_s[ur];
                        if ds > max_search_s {
                            break;
                        }
                        let min_d = (r_geom + ring_r[ur]) * min_dist_mul;
                        if ds < min_d && ds > -min_d {
                            let dv = v_cand - ring_v[ur];
                            if ds * ds + dv * dv < min_d * min_d {
                                conflict = true;
                                break;
                            }
                        }
                        idx_r -= 1;
                    }
                    if conflict {
                        continue;
                    }

                    if check_wrap && head_count > 0 {
                        let rem_s = total_strand_arc - s_cand;
                        if rem_s < max_search_s {
                            for hi in 0..head_count {
                                let ds_w = rem_s + head_s[hi];
                                let min_dw = (r_geom + head_r[hi]) * min_dist_mul;
                                if ds_w < min_dw {
                                    let dv_w = v_cand - head_v[hi];
                                    if ds_w * ds_w + dv_w * dv_w < min_dw * min_dw {
                                        conflict = true;
                                        break;
                                    }
                                }
                            }
                            if conflict {
                                continue;
                            }
                        }
                    }

                    ring_s[r_head] = s_cand;
                    ring_v[r_head] = v_cand;
                    ring_r[r_head] = r_geom;
                    r_head += 1;
                    if r_head == SWARM_RING_CAP {
                        r_head = 0;
                    }
                    if r_count < SWARM_RING_CAP {
                        r_count += 1;
                    }

                    if record_head {
                        head_s[head_count] = s_cand;
                        head_v[head_count] = v_cand;
                        head_r[head_count] = r_geom;
                        head_count += 1;
                        if head_count >= SWARM_HEAD_CAP {
                            record_head = false;
                        }
                    }

                    // Independent Bernoulli thinning after blue-noise acceptance
                    if ((h2 & 0xffff) as f64 / 65535.0) >= p_keep_base {
                        continue;
                    }

                    let mut dot_d = dot_d_geom;
                    if one_bit && dot_d < 1.32 {
                        let area_ratio = (dot_d * dot_d) / (1.32 * 1.32);
                        let h_sub = hash2d(sub_seed, slice_wrap, tr_key + 97);
                        if ((h_sub & 0xffff) as f64 / 65535.0) >= area_ratio {
                            continue;
                        }
                        dot_d = 1.32;
                    }

                    let aa_comp_2d = if one_bit {
                        0.225 * smoothstep(0.45, 1.65, dot_d)
                    } else {
                        0.0
                    };
                    let mut dot_d_eff = dot_d - aa_comp_2d;
                    if dot_d_eff < min_dot_floor {
                        dot_d_eff = min_dot_floor;
                    }
                    let mut len_eff = dot_d * dash_aspect - aa_comp_2d;
                    if len_eff < dot_d_eff {
                        len_eff = dot_d_eff;
                    }
                    let rad = 0.5 * dot_d_eff;
                    let mut half_mid = 0.5 * len_eff - rad;
                    if half_mid < 0.0 {
                        half_mid = 0.0;
                    }

                    let aspect = 0.92 + (((u_packed >> 8) & 0xff) as f64 / 255.0) * 0.16;
                    let asp_eff = if u_bridge > 0.0 {
                        aspect + (1.0 - aspect) * u_bridge
                    } else {
                        aspect
                    };
                    let sq_asp = if asp_eff == 1.0 { 1.0 } else { asp_eff.sqrt() };
                    let rx = rad * sq_asp;
                    let ry = rad / sq_asp;
                    let b_rad = half_mid + rx.max(ry);
                    if b_rad > max_dot_rad {
                        max_dot_rad = b_rad;
                    }

                    let mut v_total = v_cand + v_wob_col * jit_fade;
                    if is_rim0 && v_total < -v_scatter_lim {
                        v_total = -v_scatter_lim;
                    } else if is_rim1 && v_total > v_scatter_lim {
                        v_total = v_scatter_lim;
                    }

                    let mut s_local = s0 + (s_cand - strand_arc);
                    if s_local < s_clamp_min {
                        s_local = s_clamp_min;
                    } else if s_local > s_clamp_max {
                        s_local = s_clamp_max;
                    }

                    data.push((ref_x + stx * (s_local - s_ref) + snx * v_total) as f32);
                    data.push((ref_y + sty * (s_local - s_ref) + sny * v_total) as f32);
                    data.push(stx as f32);
                    data.push(sty as f32);
                    data.push(half_mid as f32);
                    data.push(rx as f32);
                    data.push(ry as f32);
                    seeds.push(h1 ^ h2);
                    dot_count += 1;
                }
            }

            strand_arc = arc_end;
        }
        chords_start[strand_off + m] = dot_count;
    }

    StreamSwarmCache {
        chords_start,
        data,
        seeds,
        max_dot_rad,
    }
}

/// Short transferred splice-tail helper for `normal + stipple + round` Swarm.
/// 1:1 port of `emitSwarmTailSpan` from `src/02-geometry.js` (lines 3391–3503).
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_swarm_tail_span(
    b: &mut Builder,
    cfg: &KakeamiConfig,
    grid: &Grid,
    noise: &ValueNoise2D,
    rect: &Rect,
    ck_seed: u32,
    w_base: f64,
    k: usize,
    stip_dot_cfg: f64,
    user_scale: f64,
    join_style: &str,
    chord_index: usize,
    sj: usize,
    pitch_loc: f64,
    cx_base: f64,
    cy_base: f64,
    stx: f64,
    sty: f64,
    snx: f64,
    sny: f64,
    s_ref: f64,
    s0: f64,
    s1: f64,
    shift_local_x: f64,
    shift_local_y: f64,
    r_wid_val: f64,
) {
    let span_len = s1 - s0;
    let dot_mul = |pitch_val: f64| clamp(stip_dot_cfg / (0.3286 * pitch_val).max(1e-3), 0.55, 1.75);
    let dm_loc = dot_mul(pitch_loc);
    let pitch_base = w_base / k as f64;
    let slice_s = pitch_base * 0.14 * dot_mul(pitch_base);
    if slice_s < 1e-3 || span_len < 0.035 * pitch_loc {
        return;
    }

    let tail_seed = ck_seed
        ^ ((sj as u32 + 1).wrapping_mul(0x85eb_ca6b))
        ^ 0x3c6e_f372
        ^ ((chord_index as u32 + 1).wrapping_mul(0x27d4_eb2f));
    let sl_end = ((span_len - 1e-9) / slice_s).floor() as i32;
    let one_bit_t = cfg.one_bit_mode;
    let min_dot_floor_t = if one_bit_t { 0.65 } else { 0.25 };
    let max_search_s = 1.8 * (pitch_loc * 0.36 * dm_loc) + slice_s;

    let mut ring_s = [0.0f64; SWARM_RING_CAP];
    let mut ring_v = [0.0f64; SWARM_RING_CAP];
    let mut ring_r = [0.0f64; SWARM_RING_CAP];
    let mut r_head: usize = 0;
    let mut r_count: usize = 0;

    for slice_idx in 0..=sl_end {
        let mut s_mid = (slice_idx as f64 + 0.5) * slice_s;
        if s_mid > span_len {
            s_mid = span_len;
        }
        let s_local_mid = s0 + s_mid;
        let d_loc = sample_density(
            cfg,
            grid,
            cx_base + stx * (s_local_mid - s_ref) + shift_local_x,
            cy_base + sty * (s_local_mid - s_ref) + shift_local_y,
            noise,
        );
        if d_loc <= 0.005 {
            continue;
        }

        let (d_nom, w_band, p_keep_base, min_dist_mul, dash_aspect, u_bridge) = if d_loc >= 0.56 {
            let w_sol_top =
                lane_stroke_width(cfg, pitch_loc, d_loc, r_wid_val, user_scale, false, join_style);
            let wb = if one_bit_t {
                w_sol_top + 0.294 * smoothstep(0.45, 1.15, w_sol_top)
            } else {
                w_sol_top
            };
            (pitch_loc * 0.255 * dm_loc * 1.28, wb, 1.0, 0.57, 1.60, 1.0)
        } else if d_loc >= 0.20 {
            let w_sol_g =
                lane_stroke_width(cfg, pitch_loc, d_loc, r_wid_val, user_scale, false, join_style);
            let wb = if one_bit_t {
                w_sol_g + 0.294 * smoothstep(0.45, 1.15, w_sol_g)
            } else {
                w_sol_g
            };
            let u_g = smoothstep(0.20, 0.56, d_loc);
            let ub = smoothstep(0.38, 0.56, d_loc);
            let dn = pitch_loc * (0.200 + 0.055 * u_g) * dm_loc * (1.0 + 0.28 * ub);
            let da = 1.0 + 0.60 * ub;
            let pk = clamp(0.84 + 0.16 * smoothstep(0.20, 0.45, d_loc), 0.0, 1.0);
            let md = (1.14 - 0.30 * smoothstep(0.0, 0.38, d_loc)) * (1.0 - 0.32 * ub);
            (dn, wb, pk, md, da, ub)
        } else {
            let dn = pitch_loc * (0.180 + 0.020 * (d_loc / 0.20)) * dm_loc;
            let pk = 0.84 * smoothstep(0.0, 0.20, d_loc).powf(1.15);
            (dn, dn, pk, 1.14, 1.0, 0.0)
        };

        let v_limit_nom = (0.5 * w_band - 0.42 * d_nom).max(0.0);
        let v_scatter_lim = if v_limit_nom > 0.08 * pitch_loc {
            v_limit_nom
        } else {
            0.08 * pitch_loc
        };
        let k_float = 2.5 + 3.5 * clamp((d_loc - 0.16) / 0.40, 0.0, 1.0);
        let k_base = k_float.floor() as i32;
        let k_trials = k_base
            + if ((hash2d(tail_seed ^ 0x51a7_f319, slice_idx, sj as i32 + 1) & 0xffff) as f64
                / 65535.0)
                < (k_float - k_base as f64)
            {
                1
            } else {
                0
            };
        let slice_arc0 = slice_idx as f64 * slice_s;
        let v_scale_bridge = if u_bridge > 0.0 {
            1.0 - 0.15 * u_bridge
        } else {
            1.0
        };

        for trial in 0..k_trials {
            let tr_key = (sj as i32 + 1) * 13 + trial;
            let h1 = hash2d(tail_seed, slice_idx, tr_key);
            let s_cand = slice_arc0 + ((h1 & 0xffff) as f64 / 65535.0) * slice_s;
            if s_cand < 0.0 || s_cand >= span_len {
                continue;
            }
            let v_cand = ((((h1 >> 16) & 0xffff) as f64 / 65535.0) * 2.0 - 1.0)
                * v_scatter_lim
                * v_scale_bridge;
            let h2 = hash2d(tail_seed ^ 0x27d4_eb2f, slice_idx, tr_key + 53);
            let u_packed = (h2 >> 16) & 0xffff;
            let r_jit = if u_bridge >= 0.99 {
                1.0
            } else {
                (0.89 + ((u_packed & 0xff) as f64 / 255.0) * 0.22) * 0.998
            };
            let dot_d_geom = d_nom * r_jit;
            let r_geom = 0.5 * dot_d_geom;

            let mut conflict = false;
            let mut idx_r = r_head as isize - 1;
            for _ in 0..r_count {
                if idx_r < 0 {
                    idx_r += SWARM_RING_CAP as isize;
                }
                let ur = idx_r as usize;
                let ds = s_cand - ring_s[ur];
                if ds > max_search_s {
                    break;
                }
                let min_d = (r_geom + ring_r[ur]) * min_dist_mul;
                if ds < min_d && ds > -min_d {
                    let dv = v_cand - ring_v[ur];
                    if ds * ds + dv * dv < min_d * min_d {
                        conflict = true;
                        break;
                    }
                }
                idx_r -= 1;
            }
            if conflict {
                continue;
            }

            ring_s[r_head] = s_cand;
            ring_v[r_head] = v_cand;
            ring_r[r_head] = r_geom;
            r_head += 1;
            if r_head == SWARM_RING_CAP {
                r_head = 0;
            }
            if r_count < SWARM_RING_CAP {
                r_count += 1;
            }

            if ((h2 & 0xffff) as f64 / 65535.0) >= p_keep_base {
                continue;
            }
            let mut dot_d = dot_d_geom;
            if one_bit_t && dot_d < 1.32 {
                let area_ratio = (dot_d * dot_d) / (1.32 * 1.32);
                if ((hash2d(tail_seed ^ 0x9e37_79b1, slice_idx, tr_key + 97) & 0xffff) as f64
                    / 65535.0)
                    >= area_ratio
                {
                    continue;
                }
                dot_d = 1.32;
            }
            let aa_comp_2d = if one_bit_t {
                0.225 * smoothstep(0.45, 1.65, dot_d)
            } else {
                0.0
            };
            let dot_d_eff = (dot_d - aa_comp_2d).max(min_dot_floor_t);
            let len_eff = (dot_d * dash_aspect - aa_comp_2d).max(dot_d_eff);
            let rad = 0.5 * dot_d_eff;
            let half_mid = (0.5 * len_eff - rad).max(0.0);
            let aspect = 0.92 + (((u_packed >> 8) & 0xff) as f64 / 255.0) * 0.16;
            let asp_eff = if u_bridge > 0.0 {
                aspect + (1.0 - aspect) * u_bridge
            } else {
                aspect
            };
            let sq_asp = if asp_eff == 1.0 { 1.0 } else { asp_eff.sqrt() };
            let s_local = clamp(s0 + s_cand, s0, s1);
            emit_dot_capsule(
                b,
                rect,
                cx_base + stx * (s_local - s_ref) + snx * v_cand,
                cy_base + sty * (s_local - s_ref) + sny * v_cand,
                stx,
                sty,
                snx,
                sny,
                half_mid,
                rad * sq_asp,
                rad / sq_asp,
                0.0,
                0,
                h1 ^ h2,
            );
        }
    }
}
