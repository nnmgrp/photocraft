//! Painter's-order polygonal chunk emitter for `nawa-ami` (`Normal`, `Arcs`, `Chain`, `pixel1`, `pencil`).
//! 1:1 port of `nawaTipProfile`, `computeChordJointOvershoots`, `laneStrokeWidth`,
//! and `emitNawaPolygonalChunk` from `src/02-geometry.js` (lines 2455–3815).

use crate::config::KakeamiConfig;
use crate::density::sample_density;
use crate::geometry::{emit_dot_capsule, emit_polyline, Builder, PolylinePoint};
use crate::grid::{Grid, Rect};
use crate::math::{clamp, hash2d, js_round, mod_i, smoothstep, Mulberry32, ValueNoise2D};
use crate::nawa::skeleton::{NawaChunk, NawaSkeleton, StreamGeom};
use crate::nawa::swarm::{build_stream_swarm_cache, emit_swarm_tail_span};
use std::f64::consts::PI;

const TAU: f64 = PI * 2.0;

#[inline]
fn nawa_tip_profile(tip_style: &str, u_pos: f64, d_chord: f64) -> f64 {
    let light_blend = smoothstep(0.55, 0.85, d_chord);
    if tip_style == "blunt" {
        let b_min = 0.44 + 0.44 * light_blend;
        let bevel = smoothstep(0.0, 0.16, u_pos).min(smoothstep(0.0, 0.16, 1.0 - u_pos));
        return b_min + (1.0 - b_min) * bevel;
    }
    if tip_style == "asymmetric" {
        let start_ramp = 0.86 + 0.14 * smoothstep(0.0, 0.14, u_pos);
        let tail_min = 0.08 + 0.72 * light_blend;
        let tail_ramp = tail_min + (1.0 - tail_min) * smoothstep(0.0, 0.42, 1.0 - u_pos);
        return start_ramp.min(tail_ramp);
    }
    let taper_eff = 0.72 * (1.0 - light_blend * 0.80);
    let taper_min = 0.05 + 0.75 * light_blend;
    let taper_zone = (0.30 * (taper_eff / 0.72)).max(0.08);
    let edge_ramp = smoothstep(0.0, taper_zone, u_pos).min(smoothstep(0.0, taper_zone, 1.0 - u_pos));
    taper_min + (1.0 - taper_min) * edge_ramp
}

pub(crate) struct JointOvershoots {
    pub(crate) s_mit0: [f64; 10],
    pub(crate) s_mit1: [f64; 10],
    pub(crate) ov0: [f64; 10],
    pub(crate) ov1: [f64; 10],
}

pub(crate) fn compute_chord_joint_overshoots(
    geom: &StreamGeom,
    ci: usize,
    w: f64,
    k: usize,
    strand_pitch: f64,
    join_style: &str,
) -> JointOvershoots {
    let chord = geom.chords[ci];
    let seg_len = chord.len;
    let d_th0 = geom.d_theta[ci];
    let d_th1 = geom.d_theta[ci + 1];
    let ht0 = geom.half_tan[ci];
    let ht1 = geom.half_tan[ci + 1];

    let mut res = JointOvershoots {
        s_mit0: [0.0; 10],
        s_mit1: [0.0; 10],
        ov0: [0.0; 10],
        ov1: [0.0; 10],
    };

    for sj in 0..k {
        let v = (-0.5 + (sj as f64 + 0.5) / k as f64) * w;
        let mut a0 = v * ht0;
        let mut a1 = seg_len - v * ht1;
        if a1 < a0 {
            let mid = 0.5 * (a0 + a1);
            let min_half = 0.07 * strand_pitch;
            a0 = mid - min_half;
            a1 = mid + min_half;
        }
        res.s_mit0[sj] = a0;
        res.s_mit1[sj] = a1;

        let is_rim = sj == 0 || sj == k - 1;
        let inner_weight = clamp(v.abs() / (w * 0.5), 0.25, 1.0);

        let mut ov0 = 0.0;
        if join_style == "smooth-60l" {
            if is_rim && v * d_th0 <= 0.0 {
                ov0 = 0.0;
            } else if v * d_th0 > 1e-6 {
                ov0 = seg_len * (0.31 + 0.24 * inner_weight) * clamp(d_th0.abs() / 0.36, 0.62, 1.18);
            } else if !is_rim && d_th0.abs() < 0.36 && (geom.is_closed_loop || ci > 0) {
                let g_fac0 = 1.0 - smoothstep(0.18, 0.36, d_th0.abs());
                ov0 = seg_len * (0.24 * g_fac0);
            }
        } else if join_style == "crossed" {
            if geom.is_closed_loop || ci > 0 {
                ov0 = seg_len * 0.19;
            }
        } else if geom.is_closed_loop || ci > 0 {
            ov0 = seg_len * 0.20;
        }

        let mut ov1 = 0.0;
        if join_style == "smooth-60l" {
            if is_rim && v * d_th1 <= 0.0 {
                ov1 = 0.0;
            } else if v * d_th1 > 1e-6 {
                ov1 = seg_len * (0.31 + 0.24 * inner_weight) * clamp(d_th1.abs() / 0.36, 0.62, 1.18);
            } else if !is_rim && d_th1.abs() < 0.36 && (geom.is_closed_loop || ci + 1 < geom.m) {
                let g_fac1 = 1.0 - smoothstep(0.18, 0.36, d_th1.abs());
                ov1 = seg_len * (0.24 * g_fac1);
            }
        } else if join_style == "crossed" {
            if geom.is_closed_loop || ci + 1 < geom.m {
                ov1 = seg_len * 0.19;
            }
        } else if geom.is_closed_loop || ci + 1 < geom.m {
            ov1 = seg_len * 0.20;
        }

        let max_ov = seg_len * 0.68;
        res.ov0[sj] = ov0.min(max_ov);
        res.ov1[sj] = ov1.min(max_ov);
    }
    res
}

#[inline]
pub(crate) fn lane_stroke_width(
    cfg: &KakeamiConfig,
    strand_pitch: f64,
    d_geom: f64,
    r_wid: f64,
    user_scale: f64,
    white_mode: bool,
    join_style: &str,
) -> f64 {
    if white_mode {
        let fw = clamp(0.24 + 0.62 * d_geom, 0.22, 0.84);
        return clamp(strand_pitch * fw * (0.92 + r_wid * 0.16) * user_scale, 0.65, strand_pitch * 0.86);
    }
    let fn_v = clamp(0.045 + 0.89 * d_geom.powf(1.06), 0.04, 0.95);
    let cross_comp = if join_style == "smooth-60l" {
        0.93 + 0.07 * smoothstep(0.58, 0.86, d_geom)
    } else {
        1.0
    };
    let raw_w = strand_pitch * fn_v * cross_comp * (0.92 + r_wid * 0.16) * user_scale;
    let aa_comp = if cfg.one_bit_mode {
        0.294 * smoothstep(0.45, 1.15, raw_w)
    } else {
        0.0
    };
    clamp(raw_w - aa_comp, 0.38, strand_pitch * 0.96)
}

#[derive(Clone, Copy)]
struct StrokeFrame {
    tx: f64,
    ty: f64,
    nx: f64,
    ny: f64,
    tilted: bool,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_nawa_polygonal_chunk(
    b: &mut Builder,
    cfg: &KakeamiConfig,
    grid: &Grid,
    noise: &ValueNoise2D,
    skeleton: &NawaSkeleton,
    ck: &NawaChunk,
    rect: &Rect,
) {
    let geom = &skeleton.streams[ck.geom_idx];
    let pts = &geom.pts;
    let sx = ck.shift_x;
    let sy = ck.shift_y;
    let n_scale = clamp(cfg.nawa_scale, 0.40, 4.50);
    let w_base = cfg.nawa_rope_width * n_scale;
    let k = geom
        .k_strands
        .unwrap_or_else(|| clamp(js_round(cfg.nawa_strands as f64), 2.0, 10.0) as usize);
    let w_verts = geom.w_verts.as_deref();
    let ci_from = ck.ci_from;
    let ci_to = ck.ci_to;
    if ci_to < ci_from {
        return;
    }

    let width_at_vert = |vi: usize| -> f64 {
        match w_verts {
            Some(wv) => wv[vi],
            None => w_base,
        }
    };
    let width_at_chord = |ci: usize| -> f64 {
        match w_verts {
            Some(wv) => 0.5 * (wv[ci] + wv[ci + 1]),
            None => w_base,
        }
    };

    let sugomi = cfg.finish_mode == "sugomi";
    let pencil = cfg.finish_mode == "pencil";
    let is_pale_white = cfg.nawa_tone_style == "paleWhite";
    let is_dense_white = cfg.nawa_tone_style == "denseWhite";
    let white_mode = is_pale_white || is_dense_white;
    let join_style = cfg.nawa_join_style.as_str();
    let tip_style = cfg.nawa_tip_style.as_str();
    let stipple_mode = cfg.nawa_grad_mode == "stipple" && !white_mode && !pencil;
    let stipple_shape = cfg.stipple_dot_shape.as_str();
    let is_geom_stipple_shape = stipple_shape == "round" || stipple_shape == "chain";
    let is_swarm_style = stipple_shape == "round";
    let geom_round_white = !pencil && white_mode && is_geom_stipple_shape;
    let geom_round_grad = !pencil && stipple_mode && is_geom_stipple_shape;
    let con_stip = clamp(cfg.stipple_contrast / 100.0, 0.0, 1.0);
    let stip_dot_cfg = cfg.stipple_dot_size;
    let user_scale = cfg.stroke_width / 1.3;
    let pad_raw = cfg.nawa_outline_pad;
    let pad_scaled = pad_raw.max(0.0) * n_scale;
    let tear_amt = cfg.broken_ink as f64 * 0.01;
    let bow_scale = cfg.wobble as f64 * 0.01;
    let wrap_dx = if geom.is_closed_loop { pts[geom.m].x - pts[0].x } else { 0.0 };
    let wrap_dy = if geom.is_closed_loop { pts[geom.m].y - pts[0].y } else { 0.0 };

    let sample_d = |px: f64, py: f64| -> f64 { sample_density(cfg, grid, px + sx, py + sy, noise) };

    let map_d_geom = |d_raw: f64| -> f64 {
        if is_pale_white {
            clamp(0.20 + d_raw * 0.42, 0.18, 0.60)
        } else if is_dense_white {
            clamp(0.30 + d_raw * 0.56, 0.26, 0.82)
        } else if stipple_mode && !geom_round_grad {
            d_raw.max(0.38)
        } else {
            d_raw
        }
    };

    let dot_mul_for_pitch = |pitch_val: f64| -> f64 {
        clamp(stip_dot_cfg / (0.3286 * pitch_val).max(1e-3), 0.55, 1.75)
    };

    let swarm_band_width_white = |pitch_val: f64, d_val: f64, dm_val: f64| -> f64 {
        if is_pale_white {
            let u_lin_p = clamp((d_val - 0.16) / 0.44, 0.0, 1.0);
            let d_base_p = pitch_val * (0.180 + 0.030 * d_val) * dm_val;
            let w_max_p = pitch_val * 0.72;
            d_base_p + (w_max_p - d_base_p).max(0.0) * u_lin_p
        } else {
            let u_lin_d = clamp((d_val - 0.12) / 0.48, 0.0, 1.0);
            let d_base_d = pitch_val * (0.208 + 0.045 * d_val) * dm_val;
            let w_min_d = 1.05 * d_base_d;
            let w_max_d = pitch_val * 0.84;
            w_min_d + (w_max_d - w_min_d).max(0.0) * u_lin_d
        }
    };

    let stroke_frame = |chord_index: usize, sj: usize, r_ang: f64, lock_no_tilt: bool| -> StrokeFrame {
        let ch = geom.chords[chord_index];
        if lock_no_tilt {
            return StrokeFrame { tx: ch.tx, ty: ch.ty, nx: ch.nx, ny: ch.ny, tilted: false };
        }
        let is_rim = sj == 0 || sj == k - 1;
        let d_th0 = geom.d_theta[chord_index];
        let d_th1 = geom.d_theta[chord_index + 1];
        let mut tilt_deg = 0.0;
        if join_style == "crossed" {
            let sgn_ch = if (chord_index & 1) != 0 { 1.0 } else { -1.0 };
            tilt_deg = sgn_ch * 5.8 + (r_ang - 0.5) * 2.0 * if sugomi { 2.0 } else { 0.9 };
        } else if join_style == "open" {
            tilt_deg = (r_ang - 0.5) * 2.0 * if sugomi { 3.8 } else if pencil { 2.2 } else { 1.4 };
        } else if !is_rim {
            let max_abs_d = d_th0.abs().max(d_th1.abs());
            if max_abs_d < 0.36 {
                let g_tilt = 1.0 - smoothstep(0.18, 0.36, max_abs_d);
                let sgn_ch = if (chord_index & 1) != 0 { 1.0 } else { -1.0 };
                tilt_deg = sgn_ch * (3.2 * g_tilt) + (r_ang - 0.5) * 1.1 * g_tilt;
            }
        }
        if tilt_deg.abs() > 1e-4 {
            let rad_t = tilt_deg * PI / 180.0;
            let ct = rad_t.cos();
            let st = rad_t.sin();
            let stx = ch.tx * ct - ch.ty * st;
            let sty = ch.tx * st + ch.ty * ct;
            StrokeFrame { tx: stx, ty: sty, nx: -sty, ny: stx, tilted: true }
        } else {
            StrokeFrame { tx: ch.tx, ty: ch.ty, nx: ch.nx, ny: ch.ny, tilted: false }
        }
    };

    // --- 1. Calibrated polygonal white occluder bed (role = 1) ------------
    let mut occ = Vec::with_capacity(ci_to - ci_from + 2);
    let mut chunk_max_d = 0.0f64;
    for (vi, &pv) in pts.iter().enumerate().take(ci_to + 2).skip(ci_from) {
        let d_occ = sample_d(pv.x, pv.y);
        if d_occ > chunk_max_d {
            chunk_max_d = d_occ;
        }
        let w_vi = width_at_vert(vi);
        let pitch_vi = w_vi / k as f64;
        let v_outer_vi = (0.5 - 0.5 / k as f64) * w_vi;
        let d_pad = map_d_geom(d_occ);
        let mut occ_inset = 0.0;
        let w_ref = if geom_round_white {
            let dm_occ = dot_mul_for_pitch(pitch_vi);
            if is_swarm_style {
                let d_nom_occ = if is_pale_white {
                    pitch_vi * (0.180 + 0.030 * d_occ) * dm_occ
                } else {
                    pitch_vi * (0.208 + 0.045 * d_occ) * dm_occ
                };
                occ_inset = 0.42 * d_nom_occ * (1.0 - smoothstep(0.0, 0.6, pad_raw));
                swarm_band_width_white(pitch_vi, d_occ, dm_occ)
            } else if is_pale_white {
                pitch_vi * clamp(0.19 + 0.30 * d_occ, 0.17, 0.44) * dm_occ
            } else {
                pitch_vi * clamp(0.24 + 0.45 * d_occ, 0.22, 0.54) * dm_occ
            }
        } else {
            lane_stroke_width(cfg, pitch_vi, d_pad, 0.5, user_scale, white_mode, join_style)
        };
        let base_occ_gap_vi = if pad_raw > 0.0 { pad_scaled } else { 0.0 };
        let mut gap_eff = base_occ_gap_vi * (1.0 - smoothstep(0.72, 0.92, d_pad));
        if join_style == "crossed" && pad_raw > 0.0 && d_pad <= 0.72 {
            gap_eff += 0.05 * pitch_vi;
        }
        let pad_white = if geom_round_white && is_swarm_style {
            pad_scaled
        } else if geom_round_white {
            if pad_raw > 0.0 {
                pitch_vi * (0.11 + 0.14 * con_stip) * clamp(pad_raw / 1.8, 0.55, 1.45)
            } else {
                0.0
            }
        } else if pad_raw > 0.0 {
            pad_scaled.max(pitch_vi * (0.20 + 0.18 * con_stip))
        } else {
            0.0
        };
        let base_half = if white_mode {
            v_outer_vi + (0.5 * w_ref - occ_inset).max(0.0) + pad_white
        } else {
            v_outer_vi + 0.5 * w_ref + gap_eff
        };
        let mut tip_fade = 1.0;
        if !geom.is_closed_loop {
            if vi == 0 && !ck.clip_start_at_s {
                tip_fade = if ck.head_tucked { 0.0 } else { 0.45 };
            }
            if vi == geom.m && !ck.clip_end_at_s {
                tip_fade = if ck.tail_tucked { 0.0 } else { 0.45 };
            }
        }
        let occ_miter_sec = geom.miter_sec[vi].min(1.15);
        let hw_occ = if d_occ > 0.005 {
            (base_half * tip_fade * occ_miter_sec).max(0.0)
        } else {
            0.0
        };
        occ.push(PolylinePoint {
            x: pv.x + sx,
            y: pv.y + sy,
            nx: geom.miter_nx[vi],
            ny: geom.miter_ny[vi],
            hw: hw_occ,
        });
    }
    if chunk_max_d < 0.005 {
        return;
    }
    let occ_seed = ck.seed ^ ((ck.ck as u32 + 1).wrapping_mul(0x27d4_eb2f));
    emit_polyline(b, &occ, 1, rect, occ_seed);

    // Helper: emit straight chord stroke span (3-station polyline)
    #[allow(clippy::too_many_arguments)]
    let emit_chord_span = |b: &mut Builder,
                           chord_index: usize,
                           sj: usize,
                           w_chord_loc: f64,
                           pitch_loc: f64,
                           s_mit0: f64,
                           s_mit1: f64,
                           s0: f64,
                           s1: f64,
                           is_splice0: bool,
                           is_splice1: bool,
                           ext_corner0: bool,
                           ext_corner1: bool,
                           m_start: f64,
                           m_end: f64,
                           s_seed: u32,
                           w: f64,
                           r_ang: f64,
                           r_bow: f64,
                           r_rad: f64| {
        if s1 - s0 < 0.035 * pitch_loc {
            return;
        }
        let ch = geom.chords[chord_index];
        let p0_base = pts[chord_index];
        let is_rim = sj == 0 || sj == k - 1;
        let has_splice = is_splice0 || is_splice1;
        let rad_jit = if !has_splice && (ext_corner0 || ext_corner1 || is_rim) {
            (r_rad - 0.5) * 0.10 * pitch_loc
        } else {
            0.0
        };
        let v = (-0.5 + (sj as f64 + 0.5) / k as f64) * w_chord_loc + rad_jit;

        let lock_no_tilt = join_style == "smooth-60l" && (ext_corner0 || ext_corner1);
        let fr = stroke_frame(chord_index, sj, r_ang, lock_no_tilt);

        let cx = p0_base.x + sx + ch.nx * v;
        let cy = p0_base.y + sy + ch.ny * v;
        let s_ref = 0.5 * (s_mit0 + s_mit1);
        let ref_x = cx + ch.tx * s_ref;
        let ref_y = cy + ch.ty * s_ref;

        let (x0, y0, x1, y1) = if fr.tilted {
            (
                ref_x + fr.tx * (s0 - s_ref),
                ref_y + fr.ty * (s0 - s_ref),
                ref_x + fr.tx * (s1 - s_ref),
                ref_y + fr.ty * (s1 - s_ref),
            )
        } else {
            (
                cx + ch.tx * s0,
                cy + ch.ty * s0,
                cx + ch.tx * s1,
                cy + ch.ty * s1,
            )
        };

        let use_mit0 = is_splice0 && !fr.tilted;
        let use_mit1 = is_splice1 && !fr.tilted;
        let n0x = if use_mit0 { geom.miter_nx[chord_index] } else { fr.nx };
        let n0y = if use_mit0 { geom.miter_ny[chord_index] } else { fr.ny };
        let hw0 = 0.5 * w * (if use_mit0 { geom.miter_sec[chord_index] } else { 1.0 }) * m_start;

        let n1x = if use_mit1 { geom.miter_nx[chord_index + 1] } else { fr.nx };
        let n1y = if use_mit1 { geom.miter_ny[chord_index + 1] } else { fr.ny };
        let hw1 = 0.5 * w * (if use_mit1 { geom.miter_sec[chord_index + 1] } else { 1.0 }) * m_end;

        let bow_mul = if sugomi {
            0.16
        } else if pencil {
            0.12
        } else {
            0.03 * bow_scale
        };
        let bow_amp = (r_bow - 0.5) * 2.0 * bow_mul * pitch_loc;
        let mx_p = 0.5 * (x0 + x1) + fr.nx * bow_amp;
        let my_p = 0.5 * (y0 + y1) + fr.ny * bow_amp;
        let m_mid = if 0.5 * (m_start + m_end) < 0.99 {
            m_start.max(m_end).max(1.0)
        } else {
            1.0
        };

        let spts = [
            PolylinePoint { x: x0, y: y0, nx: n0x, ny: n0y, hw: hw0 },
            PolylinePoint { x: mx_p, y: my_p, nx: fr.nx, ny: fr.ny, hw: 0.5 * w * m_mid },
            PolylinePoint { x: x1, y: y1, nx: n1x, ny: n1y, hw: hw1 },
        ];
        emit_polyline(b, &spts, 0, rect, s_seed);
    };

    // Helper: v7.2b Chain organic bead stipple span
    #[allow(clippy::too_many_arguments)]
    let emit_stipple_span = |b: &mut Builder,
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
                             is_grad_mode: bool,
                             r_wid_val: f64,
                             tail_mask: bool,
                             lock_start_clamp: bool,
                             lock_end_clamp: bool| {
        if s1 - s0 < 0.035 * pitch_loc {
            return;
        }
        let dm = dot_mul_for_pitch(pitch_loc);
        let mut base_step = if is_grad_mode {
            pitch_loc * 0.72 * dm
        } else if is_pale_white {
            pitch_loc * 0.50 * dm
        } else {
            pitch_loc * 0.48 * dm
        };
        if base_step < 1e-3 {
            return;
        }

        let mut n_loop_dots = 0i64;
        if geom.is_closed_loop && geom.total_arc > base_step * 2.0 {
            n_loop_dots = (js_round(geom.total_arc / base_step) as i64).max(8);
            base_step = geom.total_arc / n_loop_dots as f64;
        }

        let arc_base = geom.cum_arc[chord_index];
        let arc0 = arc_base + s0;
        let arc1 = arc_base + s1;
        let k_start = (arc0 / base_step - 0.5).ceil() as i64;
        let k_end = (arc1 / base_step - 0.5).floor() as i64;
        if k_end < k_start {
            return;
        }

        let stream_hash_seed = ck.seed
            ^ ((sj as u32 + 1).wrapping_mul(0x85eb_ca6b))
            ^ if tail_mask { 0x3c6e_f372 } else { 0 };
        let con_keep_mul = 0.90 + 0.20 * con_stip;
        let is_rim0 = sj == 0;
        let is_rim1 = sj == k - 1;
        let is_rim_any = is_rim0 || is_rim1;

        let wob_phase = ((hash2d(ck.seed ^ 0x51a7_e901, sj as i32 + 1, 3) & 0xffff) as f64 / 65535.0) * TAU;
        let wob_freq = if geom.is_closed_loop && geom.total_arc > 1e-3 {
            js_round(geom.total_arc / (w_base * 3.8).max(1.0)).max(2.0) * TAU / geom.total_arc
        } else {
            TAU / (w_base * 3.8).max(1.0)
        };
        let wob_amp = if !is_grad_mode {
            pitch_loc * if is_rim_any { 0.14 * 0.35 } else { 0.14 }
        } else {
            0.0
        };

        let br_phase = ((hash2d(ck.seed ^ 0x6b2f_1103, sj as i32 + 1, 7) & 0xffff) as f64 / 65535.0) * TAU;
        let br_freq = if geom.is_closed_loop && geom.total_arc > 1e-3 {
            js_round(geom.total_arc / (w_base * 2.6).max(1.0)).max(2.0) * TAU / geom.total_arc
        } else {
            TAU / (w_base * 2.6).max(1.0)
        };

        let s_clamp_min = if lock_start_clamp { s0 } else { s0 - 0.35 * base_step };
        let s_clamp_max = if lock_end_clamp { s1 } else { s1 + 0.35 * base_step };

        for k_idx in k_start..=k_end {
            let k_wrap = if n_loop_dots > 0 { mod_i(k_idx, n_loop_dots) as i32 } else { k_idx as i32 };
            let s_arc_center = (k_idx as f64 + 0.5) * base_step;
            let s_local = s_arc_center - arc_base;
            if s_local < s0 || s_local > s1 {
                continue;
            }

            let h1 = hash2d(stream_hash_seed, k_wrap, sj as i32);
            let h2 = hash2d(stream_hash_seed ^ 0x27d4_eb2f, k_wrap, sj as i32 + 17);
            let h3 = hash2d(stream_hash_seed ^ 0x9e37_79b1, k_wrap, sj as i32 + 31);
            let r_keep = (h1 & 0xffff) as f64 / 65535.0;
            let r_j_along = (((h1 >> 16) & 0xffff) as f64 / 65535.0) - 0.5;
            let u_j_perp = (h2 & 0xffff) as f64 / 65535.0;
            let u_size = ((h2 >> 16) & 0xffff) as f64 / 65535.0;
            let u_aspect = (h3 & 0xffff) as f64 / 65535.0;
            let u_ang = (((h3 >> 16) & 0xffff) as f64 / 65535.0) - 0.5;

            let h_gap = hash2d(stream_hash_seed ^ 0x1656_67b1, k_wrap >> 1, sj as i32 + 5);
            let r_micro_gap = (h_gap & 0xffff) as f64 / 65535.0;

            let px0 = cx_base + stx * (s_local - s_ref);
            let py0 = cy_base + sty * (s_local - s_ref);
            let d_loc = sample_density(cfg, grid, px0 + shift_local_x, py0 + shift_local_y, noise);
            if d_loc <= 0.005 {
                continue;
            }

            let r_jit_full = (0.68 + u_size * 0.64) * 0.983;
            let aspect_full = 0.82 + u_aspect * 0.42;
            let ang_full = u_ang * (56.0 * PI / 180.0);

            let breath_wave = (s_arc_center * br_freq + br_phase).sin();
            let step_breath = (s_arc_center * br_freq + br_phase).cos();

            let (dot_d, dash_len, p_keep, jit_scale, aspect_eff, ang_eff) = if is_grad_mode {
                if d_loc >= 0.56 {
                    let w_sol = lane_stroke_width(cfg, pitch_loc, d_loc, r_wid_val, user_scale, false, join_style);
                    let raw_w_sol = if cfg.one_bit_mode {
                        w_sol + 0.294 * smoothstep(0.45, 1.15, w_sol)
                    } else {
                        w_sol
                    };
                    (raw_w_sol, base_step * 1.16, 1.0, 0.0, 1.0, 0.0)
                } else if d_loc >= 0.22 {
                    let u_g = smoothstep(0.22, 0.56, d_loc);
                    let org_g = 1.0 - u_g;
                    let w_top = lane_stroke_width(cfg, pitch_loc, 0.56, r_wid_val, user_scale, false, join_style);
                    let raw_w_top = if cfg.one_bit_mode {
                        w_top + 0.294 * smoothstep(0.45, 1.15, w_top)
                    } else {
                        w_top
                    };
                    let dot_bot = pitch_loc * 0.30 * dm;
                    let r_jit_eff = 1.0 + (r_jit_full - 1.0) * org_g;
                    let dd = (dot_bot + (raw_w_top - dot_bot) * u_g) * r_jit_eff;
                    let dl = dd + (base_step * 1.14 - dd) * u_g.powf(0.85);
                    let pk = clamp((0.86 + 0.14 * smoothstep(0.22, 0.36, d_loc)) * (1.0 + 0.14 * org_g * breath_wave), 0.0, 1.0);
                    (dd, dl, pk, org_g, 1.0 + (aspect_full - 1.0) * org_g, ang_full * org_g)
                } else {
                    let u_low = d_loc / 0.22;
                    let dd = pitch_loc * (0.20 + 0.10 * u_low) * dm * r_jit_full;
                    let pk = clamp(0.86 * smoothstep(0.0, 0.22, d_loc).powf(1.10) * (1.0 + 0.14 * breath_wave), 0.0, 0.98);
                    (dd, dd, pk, 1.0, aspect_full, ang_full)
                }
            } else if is_pale_white {
                let dd = pitch_loc * clamp(0.19 + 0.30 * d_loc, 0.17, 0.44) * dm * r_jit_full;
                let base_p = clamp((0.57 + 0.42 * d_loc) * con_keep_mul, 0.0, 0.96);
                let mut pk = clamp(base_p * (1.0 + 0.14 * breath_wave), 0.0, 0.98);
                if r_micro_gap < 0.075 * (1.0 - 0.5 * d_loc) {
                    pk *= 0.25;
                }
                (dd, dd, pk, 1.0, aspect_full, ang_full)
            } else {
                let dd = pitch_loc * clamp(0.24 + 0.45 * d_loc, 0.22, 0.54) * dm * r_jit_full;
                let base_pd = clamp((0.72 + 0.32 * d_loc) * con_keep_mul, 0.0, 0.98);
                let mut pk = clamp(base_pd * (1.0 + 0.14 * breath_wave), 0.0, 0.99);
                if r_micro_gap < 0.065 * (1.0 - 0.6 * d_loc) {
                    pk *= 0.25;
                }
                (dd, dd, pk, 1.0, aspect_full, ang_full)
            };

            if r_keep >= p_keep {
                continue;
            }

            let aa_comp_2d = if cfg.one_bit_mode {
                0.294 * smoothstep(0.45, 1.05, dot_d)
            } else {
                0.0
            };
            let dot_d_eff = (dot_d - aa_comp_2d).max(0.50);
            let len_eff = (dash_len - aa_comp_2d).max(dot_d_eff);
            let rad = 0.5 * dot_d_eff;
            let half_mid = (0.5 * len_eff - rad).max(0.0);

            let sq_asp = if aspect_eff == 1.0 { 1.0 } else { aspect_eff.sqrt() };
            let rx = rad * sq_asp;
            let ry = rad / sq_asp;

            let j_along = (r_j_along * 0.68 + step_breath * 0.12) * base_step * jit_scale;
            let j_perp = if is_rim0 {
                u_j_perp * 0.17 * pitch_loc * jit_scale
            } else if is_rim1 {
                -u_j_perp * 0.17 * pitch_loc * jit_scale
            } else {
                (u_j_perp - 0.5) * 0.34 * pitch_loc * jit_scale
            };

            let mut v_wob = 0.0;
            if wob_amp > 0.0 {
                v_wob = (s_arc_center * wob_freq + wob_phase).sin() * wob_amp;
                if (is_rim0 && v_wob < 0.0) || (is_rim1 && v_wob > 0.0) {
                    v_wob = -v_wob * 0.5;
                }
            }

            let s_final = clamp(s_local + j_along, s_clamp_min, s_clamp_max);
            let dx_f = cx_base + stx * (s_final - s_ref) + snx * (j_perp + v_wob);
            let dy_f = cy_base + sty * (s_final - s_ref) + sny * (j_perp + v_wob);

            emit_dot_capsule(
                b, rect, dx_f, dy_f, stx, sty, snx, sny,
                half_mid, rx, ry, ang_eff, 0, h1 ^ h2 ^ h3,
            );
        }
    };

    // --- 2. Transferred ×-tails from adjacent LOWER chunks at splices ------
    if !geom_round_white && ck.clip_start_at_s && ck.own_start_tail && (ci_from > 0 || geom.is_closed_loop) {
        let prev_ci = if ci_from > 0 { ci_from - 1 } else { geom.m - 1 };
        let prev_shift_x = if ci_from == 0 && geom.is_closed_loop { sx - wrap_dx } else { sx };
        let prev_shift_y = if ci_from == 0 && geom.is_closed_loop { sy - wrap_dy } else { sy };
        let prev_ch = geom.chords[prev_ci];
        let d_prev = sample_density(cfg, grid, prev_ch.mx + prev_shift_x, prev_ch.my + prev_shift_y, noise);
        if d_prev > 0.005 {
            let w_prev_loc = width_at_chord(prev_ci);
            let pitch_prev = w_prev_loc / k as f64;
            let d_geom_prev = map_d_geom(d_prev);
            let sp_prev = compute_chord_joint_overshoots(geom, prev_ci, w_prev_loc, k, pitch_prev, join_style);
            let ov_eps_prev = 0.035 * pitch_prev;
            for sj_p in 0..k {
                if sp_prev.ov1[sj_p] <= ov_eps_prev {
                    continue;
                }
                let s_seed_p = ck.seed
                    ^ ((prev_ci as u32 + 1).wrapping_mul(0x9e37_79b1))
                    ^ ((sj_p as u32 + 1).wrapping_mul(0x85eb_ca6b));
                let mut prng = Mulberry32::new(s_seed_p);
                let r_kp = prng.next_f64();
                let r_ap = prng.next_f64();
                let _r_bp = prng.next_f64();
                let r_ljp = prng.next_f64();
                let r_wp = prng.next_f64();
                if sugomi && d_prev < 0.36 && r_kp < (0.08 + 0.10 * (1.0 - d_prev / 0.36)) {
                    continue;
                }
                let w_p = lane_stroke_width(cfg, pitch_prev, d_geom_prev, r_wp, user_scale, white_mode, join_style);
                let tail_len_p = sp_prev.ov1[sj_p] * (1.0 + (r_ljp - 0.5) * 0.08);
                let root_s = sp_prev.s_mit1[sj_p];
                let s_ref_p = 0.5 * (sp_prev.s_mit0[sj_p] + sp_prev.s_mit1[sj_p]);
                let fr_p = stroke_frame(prev_ci, sj_p, r_ap, false);
                let p_base = pts[prev_ci];
                let v_p = (-0.5 + (sj_p as f64 + 0.5) / k as f64) * w_prev_loc;
                let cx_p = p_base.x + prev_shift_x + prev_ch.nx * v_p;
                let cy_p = p_base.y + prev_shift_y + prev_ch.ny * v_p;
                let ref_xp = cx_p + prev_ch.tx * s_ref_p;
                let ref_yp = cy_p + prev_ch.ty * s_ref_p;
                if geom_round_grad && d_prev < 0.58 {
                    if is_swarm_style {
                        emit_swarm_tail_span(
                            b, cfg, grid, noise, rect, ck.seed, w_base, k, stip_dot_cfg, user_scale, join_style,
                            prev_ci, sj_p, pitch_prev, ref_xp, ref_yp, fr_p.tx, fr_p.ty, fr_p.nx, fr_p.ny,
                            s_ref_p, root_s, root_s + tail_len_p, 0.0, 0.0, r_wp,
                        );
                    } else {
                        emit_stipple_span(
                            b, prev_ci, sj_p, pitch_prev, ref_xp, ref_yp, fr_p.tx, fr_p.ty, fr_p.nx, fr_p.ny,
                            s_ref_p, root_s, root_s + tail_len_p, 0.0, 0.0, true, r_wp, true, true, true,
                        );
                    }
                    continue;
                }
                let (rx0, ry0) = if fr_p.tilted {
                    (ref_xp + fr_p.tx * (root_s - s_ref_p), ref_yp + fr_p.ty * (root_s - s_ref_p))
                } else {
                    (cx_p + prev_ch.tx * root_s, cy_p + prev_ch.ty * root_s)
                };
                let rx1 = rx0 + fr_p.tx * tail_len_p;
                let ry1 = ry0 + fr_p.ty * tail_len_p;
                let use_mit_p = !fr_p.tilted;
                let hw_root = 0.5 * w_p * if use_mit_p { geom.miter_sec[prev_ci + 1] } else { 1.0 };
                let hw_tip = 0.5 * w_p * nawa_tip_profile(tip_style, 1.0, d_geom_prev);
                let pts_p = [
                    PolylinePoint {
                        x: rx0,
                        y: ry0,
                        nx: if use_mit_p { geom.miter_nx[prev_ci + 1] } else { fr_p.nx },
                        ny: if use_mit_p { geom.miter_ny[prev_ci + 1] } else { fr_p.ny },
                        hw: hw_root,
                    },
                    PolylinePoint {
                        x: 0.5 * (rx0 + rx1),
                        y: 0.5 * (ry0 + ry1),
                        nx: fr_p.nx,
                        ny: fr_p.ny,
                        hw: 0.5 * (hw_root + hw_tip),
                    },
                    PolylinePoint { x: rx1, y: ry1, nx: fr_p.nx, ny: fr_p.ny, hw: hw_tip },
                ];
                emit_polyline(b, &pts_p, 0, rect, s_seed_p ^ 0x1357);
            }
        }
    }

    if !geom_round_white && ck.clip_end_at_s && ck.own_end_tail && (ci_to + 1 < geom.m || geom.is_closed_loop) {
        let next_ci = if ci_to + 1 < geom.m { ci_to + 1 } else { 0 };
        let next_shift_x = if ci_to + 1 == geom.m && geom.is_closed_loop { sx + wrap_dx } else { sx };
        let next_shift_y = if ci_to + 1 == geom.m && geom.is_closed_loop { sy + wrap_dy } else { sy };
        let next_ch = geom.chords[next_ci];
        let d_next = sample_density(cfg, grid, next_ch.mx + next_shift_x, next_ch.my + next_shift_y, noise);
        if d_next > 0.005 {
            let w_next_loc = width_at_chord(next_ci);
            let pitch_next = w_next_loc / k as f64;
            let d_geom_next = map_d_geom(d_next);
            let sp_next = compute_chord_joint_overshoots(geom, next_ci, w_next_loc, k, pitch_next, join_style);
            let ov_eps_next = 0.035 * pitch_next;
            for sj_n in 0..k {
                if sp_next.ov0[sj_n] <= ov_eps_next {
                    continue;
                }
                let s_seed_n = ck.seed
                    ^ ((next_ci as u32 + 1).wrapping_mul(0x9e37_79b1))
                    ^ ((sj_n as u32 + 1).wrapping_mul(0x85eb_ca6b));
                let mut nrng = Mulberry32::new(s_seed_n);
                let r_kn = nrng.next_f64();
                let r_an = nrng.next_f64();
                let _r_bn = nrng.next_f64();
                let r_ljn = nrng.next_f64();
                let r_wn = nrng.next_f64();
                if sugomi && d_next < 0.36 && r_kn < (0.08 + 0.10 * (1.0 - d_next / 0.36)) {
                    continue;
                }
                let w_n = lane_stroke_width(cfg, pitch_next, d_geom_next, r_wn, user_scale, white_mode, join_style);
                let tail_len_n = sp_next.ov0[sj_n] * (1.0 + (r_ljn - 0.5) * 0.08);
                let root_sn = sp_next.s_mit0[sj_n];
                let s_ref_n = 0.5 * (sp_next.s_mit0[sj_n] + sp_next.s_mit1[sj_n]);
                let fr_n = stroke_frame(next_ci, sj_n, r_an, false);
                let p_base_n = pts[next_ci];
                let v_n = (-0.5 + (sj_n as f64 + 0.5) / k as f64) * w_next_loc;
                let cx_n = p_base_n.x + next_shift_x + next_ch.nx * v_n;
                let cy_n = p_base_n.y + next_shift_y + next_ch.ny * v_n;
                let ref_xn = cx_n + next_ch.tx * s_ref_n;
                let ref_yn = cy_n + next_ch.ty * s_ref_n;
                if geom_round_grad && d_next < 0.58 {
                    if is_swarm_style {
                        emit_swarm_tail_span(
                            b, cfg, grid, noise, rect, ck.seed, w_base, k, stip_dot_cfg, user_scale, join_style,
                            next_ci, sj_n, pitch_next, ref_xn, ref_yn, fr_n.tx, fr_n.ty, fr_n.nx, fr_n.ny,
                            s_ref_n, root_sn - tail_len_n, root_sn, 0.0, 0.0, r_wn,
                        );
                    } else {
                        emit_stipple_span(
                            b, next_ci, sj_n, pitch_next, ref_xn, ref_yn, fr_n.tx, fr_n.ty, fr_n.nx, fr_n.ny,
                            s_ref_n, root_sn - tail_len_n, root_sn, 0.0, 0.0, true, r_wn, true, true, true,
                        );
                    }
                    continue;
                }
                let (nx1, ny1) = if fr_n.tilted {
                    (ref_xn + fr_n.tx * (root_sn - s_ref_n), ref_yn + fr_n.ty * (root_sn - s_ref_n))
                } else {
                    (cx_n + next_ch.tx * root_sn, cy_n + next_ch.ty * root_sn)
                };
                let nx0 = nx1 - fr_n.tx * tail_len_n;
                let ny0 = ny1 - fr_n.ty * tail_len_n;
                let use_mit_n = !fr_n.tilted;
                let hw_root_n = 0.5 * w_n * if use_mit_n { geom.miter_sec[next_ci] } else { 1.0 };
                let hw_tip_n = 0.5 * w_n * nawa_tip_profile(tip_style, 0.0, d_geom_next);
                let pts_n = [
                    PolylinePoint { x: nx0, y: ny0, nx: fr_n.nx, ny: fr_n.ny, hw: hw_tip_n },
                    PolylinePoint {
                        x: 0.5 * (nx0 + nx1),
                        y: 0.5 * (ny0 + ny1),
                        nx: fr_n.nx,
                        ny: fr_n.ny,
                        hw: 0.5 * (hw_root_n + hw_tip_n),
                    },
                    PolylinePoint {
                        x: nx1,
                        y: ny1,
                        nx: if use_mit_n { geom.miter_nx[next_ci] } else { fr_n.nx },
                        ny: if use_mit_n { geom.miter_ny[next_ci] } else { fr_n.ny },
                        hw: hw_root_n,
                    },
                ];
                emit_polyline(b, &pts_n, 0, rect, s_seed_n ^ 0x2468);
            }
        }
    }

    // --- 3. Own chords [ci_from ..= ci_to] ---------------------------------
    // v7.4b: Swarm uses strand-outer continuous Strip Poisson-Disc cached per stream via `OnceLock`,
    // invariant across all Z-chunks, torus copies, and tiles (x=512/1024).
    let emit_swarm_chunk_from_cache = |b: &mut Builder, is_grad_mode: bool| {
        let cached = skeleton.swarm_cache[ck.geom_idx].get_or_init(|| {
            build_stream_swarm_cache(
                cfg,
                grid,
                noise,
                geom,
                ck.seed,
                geom.head_tucked,
                geom.tail_tucked,
                is_grad_mode,
            )
        });
        if cached.seeds.is_empty() {
            return;
        }
        let c_starts = &cached.chords_start;
        let d_buf = &cached.data;
        let h_buf = &cached.seeds;
        let stride_m = geom.m + 1;
        let r_pad = cached.max_dot_rad + 2.0;
        let rx0 = rect.x - r_pad;
        let rx1 = rect.x + rect.w + r_pad;
        let ry0 = rect.y - r_pad;
        let ry1 = rect.y + rect.h + r_pad;
        for sj_c in 0..k {
            let s_off = sj_c * stride_m;
            let d_from = c_starts[s_off + ci_from];
            let d_to = c_starts[s_off + ci_to + 1];
            for (di, &seed_dot) in h_buf.iter().enumerate().take(d_to).skip(d_from) {
                let o = di * 7;
                let dx_f = d_buf[o] as f64 + sx;
                if dx_f < rx0 || dx_f > rx1 {
                    continue;
                }
                let dy_f = d_buf[o + 1] as f64 + sy;
                if dy_f < ry0 || dy_f > ry1 {
                    continue;
                }
                let tx_f = d_buf[o + 2] as f64;
                let ty_f = d_buf[o + 3] as f64;
                emit_dot_capsule(
                    b,
                    rect,
                    dx_f,
                    dy_f,
                    tx_f,
                    ty_f,
                    -ty_f,
                    tx_f,
                    d_buf[o + 4] as f64,
                    d_buf[o + 5] as f64,
                    d_buf[o + 6] as f64,
                    0.0,
                    0,
                    seed_dot,
                );
            }
        }
    };

    if is_swarm_style && geom_round_white {
        emit_swarm_chunk_from_cache(b, false);
        return;
    }
    if is_swarm_style && geom_round_grad {
        emit_swarm_chunk_from_cache(b, true);
    }

    for ci in ci_from..=ci_to {
        let w_chord_loc = width_at_chord(ci);
        let p_ch_a = pts[ci];
        let p_ch_b = pts[ci + 1];
        let ch_pad = w_chord_loc * 1.15;
        let ch_min_x = p_ch_a.x.min(p_ch_b.x) + sx - ch_pad;
        let ch_max_x = p_ch_a.x.max(p_ch_b.x) + sx + ch_pad;
        if ch_max_x < rect.x - 2.0 || ch_min_x > rect.x + rect.w + 2.0 {
            continue;
        }
        let ch_min_y = p_ch_a.y.min(p_ch_b.y) + sy - ch_pad;
        let ch_max_y = p_ch_a.y.max(p_ch_b.y) + sy + ch_pad;
        if ch_max_y < rect.y - 2.0 || ch_min_y > rect.y + rect.h + 2.0 {
            continue;
        }

        let chord = geom.chords[ci];
        let d_a = sample_d(p_ch_a.x, p_ch_a.y);
        let d_m = sample_d(chord.mx, chord.my);
        let d_b = sample_d(p_ch_b.x, p_ch_b.y);
        let d_chord = (d_a + d_m + d_b) / 3.0;
        let d_max_ch = d_a.max(d_m).max(d_b);
        let d_min_ch = d_a.min(d_m).min(d_b);
        if d_max_ch <= 0.005 {
            continue;
        }
        // Skip chords already emitted by continuous Swarm strand cache in geom_round_grad
        if is_swarm_style && geom_round_grad && d_min_ch < 0.56 {
            continue;
        }

        let pitch_loc = w_chord_loc / k as f64;
        let d_geom = map_d_geom(d_chord);
        let sp_info = compute_chord_joint_overshoots(geom, ci, w_chord_loc, k, pitch_loc, join_style);
        let is_chunk_start_chord = ci == ci_from && ck.clip_start_at_s;
        let is_chunk_end_chord = ci == ci_to && ck.clip_end_at_s;
        let is_head_tucked_chord = !geom.is_closed_loop && ck.head_tucked && ci == 0;
        let is_tail_tucked_chord = !geom.is_closed_loop && ck.tail_tucked && ci == geom.m - 1;
        let dark_close = smoothstep(0.62, 0.88, d_geom);
        let m_ext_corner = clamp(0.72 + 0.24 * dark_close, 0.72, 0.96);
        let m_rim_corner = 0.32 + (0.96 - 0.32) * dark_close;
        let use_stipple_on_chord = geom_round_white || (geom_round_grad && d_min_ch < 0.56);

        for sj in 0..k {
            let s_seed = ck.seed
                ^ ((ci as u32 + 1).wrapping_mul(0x9e37_79b1))
                ^ ((sj as u32 + 1).wrapping_mul(0x85eb_ca6b));
            let mut srng = Mulberry32::new(s_seed);

            let r_keep = srng.next_f64();
            let r_ang = srng.next_f64();
            let r_bow = srng.next_f64();
            let r_len_jit = srng.next_f64();
            let r_wid = srng.next_f64();
            let r_rad = srng.next_f64();
            let r_pull = srng.next_f64();

            if sugomi && d_chord < 0.36 && r_keep < (0.08 + 0.10 * (1.0 - d_chord / 0.36)) {
                continue;
            }
            if sugomi && tear_amt > 0.0 && d_chord < 0.52 {
                let edge_drop = tear_amt * 0.12 * (1.0 - d_chord / 0.52);
                if srng.next_f64() < edge_drop {
                    continue;
                }
            }

            let is_rim_lane = sj == 0 || sj == k - 1;

            if use_stipple_on_chord {
                let (mut s0_s, mut s1_s) = if geom_round_white {
                    (sp_info.s_mit0[sj], sp_info.s_mit1[sj])
                } else {
                    let allow_start_s = (!is_chunk_start_chord || ck.own_start_tail) && !is_head_tucked_chord;
                    let allow_end_s = (!is_chunk_end_chord || ck.own_end_tail) && !is_tail_tucked_chord;
                    let ov0_s = if allow_start_s { sp_info.ov0[sj] * (1.0 + (r_len_jit - 0.5) * 0.08) } else { 0.0 };
                    let ov1_s = if allow_end_s { sp_info.ov1[sj] * (1.0 + (r_len_jit - 0.5) * 0.08) } else { 0.0 };
                    (sp_info.s_mit0[sj] - ov0_s, sp_info.s_mit1[sj] + ov1_s)
                };
                let min_span_ts = 0.22 * pitch_loc;
                if is_head_tucked_chord {
                    let t_in0_s = chord.len * if is_rim_lane { 0.36 } else { 0.20 };
                    if s1_s - s0_s > t_in0_s + min_span_ts {
                        s0_s += t_in0_s;
                    }
                }
                if is_tail_tucked_chord {
                    let t_in1_s = chord.len * if is_rim_lane { 0.36 } else { 0.20 };
                    if s1_s - s0_s > t_in1_s + min_span_ts {
                        s1_s -= t_in1_s;
                    }
                }
                let v_s = (-0.5 + (sj as f64 + 0.5) / k as f64) * w_chord_loc;
                let s_ref_s = 0.5 * (sp_info.s_mit0[sj] + sp_info.s_mit1[sj]);
                let p0_s = pts[ci];
                let ref_xs = p0_s.x + sx + chord.nx * v_s + chord.tx * s_ref_s;
                let ref_ys = p0_s.y + sy + chord.ny * v_s + chord.ty * s_ref_s;
                let fr_s = if geom_round_white {
                    StrokeFrame { tx: chord.tx, ty: chord.ty, nx: chord.nx, ny: chord.ny, tilted: false }
                } else {
                    stroke_frame(ci, sj, r_ang, join_style == "smooth-60l" && is_rim_lane)
                };
                let lock_st_s = is_head_tucked_chord || (!geom.is_closed_loop && ci == 0);
                let lock_en_s = is_tail_tucked_chord || (!geom.is_closed_loop && ci == geom.m - 1);
                emit_stipple_span(
                    b, ci, sj, pitch_loc, ref_xs, ref_ys, fr_s.tx, fr_s.ty, fr_s.nx, fr_s.ny,
                    s_ref_s, s0_s, s1_s, 0.0, 0.0, geom_round_grad, r_wid, false, lock_st_s, lock_en_s,
                );
                continue;
            }

            let w = lane_stroke_width(cfg, pitch_loc, d_geom, r_wid, user_scale, white_mode, join_style);

            let allow_start_ov = (!is_chunk_start_chord || ck.own_start_tail) && !is_head_tucked_chord;
            let allow_end_ov = (!is_chunk_end_chord || ck.own_end_tail) && !is_tail_tucked_chord;

            let ov0 = if allow_start_ov { sp_info.ov0[sj] * (1.0 + (r_len_jit - 0.5) * 0.08) } else { 0.0 };
            let ov1 = if allow_end_ov { sp_info.ov1[sj] * (1.0 + (r_len_jit - 0.5) * 0.08) } else { 0.0 };

            let mut s0 = sp_info.s_mit0[sj] - ov0;
            let mut s1 = sp_info.s_mit1[sj] + ov1;

            let ov_eps_loc = 0.035 * pitch_loc;
            let is_splice0 = is_chunk_start_chord && !ck.own_start_tail && sp_info.ov0[sj] > ov_eps_loc;
            let is_splice1 = is_chunk_end_chord && !ck.own_end_tail && sp_info.ov1[sj] > ov_eps_loc;
            let ext_corner0 = !is_splice0 && !is_head_tucked_chord && ov0 < ov_eps_loc;
            let ext_corner1 = !is_splice1 && !is_tail_tucked_chord && ov1 < ov_eps_loc;

            if join_style == "smooth-60l" && is_rim_lane && dark_close < 0.999 {
                let rim_pull = pitch_loc * (0.11 + r_pull * 0.03) * (1.0 - dark_close);
                let min_span_rim = 0.28 * pitch_loc;
                if ext_corner0 && (s1 - s0) > rim_pull + min_span_rim {
                    s0 += rim_pull;
                }
                if ext_corner1 && (s1 - s0) > rim_pull + min_span_rim {
                    s1 -= rim_pull;
                }
            }

            let min_span_tuck = 0.22 * pitch_loc;
            if is_head_tucked_chord {
                let tuck_in0 = chord.len * if is_rim_lane { 0.36 } else { 0.20 };
                if s1 - s0 > tuck_in0 + min_span_tuck {
                    s0 += tuck_in0;
                }
            }
            if is_tail_tucked_chord {
                let tuck_in1 = chord.len * if is_rim_lane { 0.36 } else { 0.20 };
                if s1 - s0 > tuck_in1 + min_span_tuck {
                    s1 -= tuck_in1;
                }
            }

            let corner_m = if join_style == "smooth-60l" && is_rim_lane {
                m_rim_corner
            } else {
                m_ext_corner
            };
            let m0 = if is_head_tucked_chord {
                0.0
            } else if is_splice0 {
                1.0
            } else if ext_corner0 {
                corner_m
            } else {
                nawa_tip_profile(tip_style, 0.0, d_geom)
            };
            let m1 = if is_tail_tucked_chord {
                0.0
            } else if is_splice1 {
                1.0
            } else if ext_corner1 {
                corner_m
            } else {
                nawa_tip_profile(tip_style, 1.0, d_geom)
            };

            emit_chord_span(
                b, ci, sj, w_chord_loc, pitch_loc, sp_info.s_mit0[sj], sp_info.s_mit1[sj],
                s0, s1, is_splice0, is_splice1, ext_corner0, ext_corner1,
                m0, m1, s_seed, w, r_ang, r_bow, r_rad,
            );
        }
    }
}
