//! Cache-blocked 16-column Felzenszwalb 2D EDT & Shibufude v3 Pencil engine.
//! 1:1 port of `edt1DContig`, `edtPass2Blocked`, `computePencilEdt`, and the
//! `mode === 'pencil'` Stage A + Stage B pipeline from `src/03-raster.js`
//! (lines 298–455 and 616–981).

use crate::config::KakeamiConfig;
use crate::density::sample_density;
use crate::geometry::PreparedScene;
use crate::math::{clamp, js_round};
use crate::raster::fade::{
    build_fade_tile_grid, crisp_paper_grain, eval_kake_v8_fade, eval_kake_v8_fade_from_t,
    value_noise_2d,
};
use crate::raster::RasterScratch;

pub const EDT_INF: f32 = 1e12;
pub const EDT_HALF_INF: f32 = 5e11;
const EDT_BLOCK: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdtResult {
    pub has_ink: bool,
    pub has_paper: bool,
}

fn ensure_edt_buffers(scratch: &mut RasterScratch, n_pix: usize, max_dim: usize) {
    if scratch.edt_d_in.len() < n_pix {
        scratch.edt_d_in.resize(n_pix, 0.0);
        scratch.edt_d_out.resize(n_pix, 0.0);
    }
    if scratch.edt_v.len() < max_dim {
        scratch.edt_v.resize(max_dim, 0);
        scratch.edt_z.resize(max_dim + 1, 0.0);
        scratch.edt_f_val.resize(max_dim, 0.0);
        scratch.edt_f_sq.resize(max_dim, 0.0);
        scratch.edt_strip.resize(max_dim * EDT_BLOCK, 0.0);
    }
}

/// Exact 1D squared Euclidean distance transform on contiguous slice `[base..base+len]`
/// (Felzenszwalb & Huttenlocher, bit-exact with interior-zero skip).
fn edt_1d_contig(
    strip: &mut [f32],
    base: usize,
    len: usize,
    edt_v: &mut [i32],
    edt_z: &mut [f32],
    edt_f_val: &mut [f32],
    edt_f_sq: &mut [f32],
) {
    let mut q0: isize = -1;
    for y in 0..len {
        if strip[base + y] < EDT_HALF_INF {
            q0 = y as isize;
            break;
        }
    }
    if q0 < 0 {
        for y in 0..len {
            strip[base + y] = EDT_INF;
        }
        return;
    }
    let q0_u = q0 as usize;
    let mut k: usize = 0;
    let f0 = strip[base + q0_u];
    edt_v[0] = q0 as i32;
    edt_f_val[0] = f0;
    edt_f_sq[0] = (f0 as f64 + (q0_u * q0_u) as f64) as f32;
    edt_z[0] = -1e20;
    edt_z[1] = 1e20;

    for q in (q0_u + 1)..len {
        let fq = strip[base + q];
        if fq >= EDT_HALF_INF {
            continue;
        }
        if fq == 0.0
            && q + 1 < len
            && strip[base + q - 1] == 0.0
            && strip[base + q + 1] == 0.0
        {
            continue;
        }
        let q_f = q as f64;
        let q_sq = fq as f64 + q_f * q_f;
        let mut vk = edt_v[k] as f64;
        let mut s = 0.5 * (q_sq - edt_f_sq[k] as f64) / (q_f - vk);
        while s <= edt_z[k] as f64 {
            k -= 1;
            vk = edt_v[k] as f64;
            s = 0.5 * (q_sq - edt_f_sq[k] as f64) / (q_f - vk);
        }
        k += 1;
        edt_v[k] = q as i32;
        edt_f_val[k] = fq;
        edt_f_sq[k] = q_sq as f32;
        edt_z[k] = s as f32;
        edt_z[k + 1] = 1e20;
    }

    k = 0;
    for q_out in 0..len {
        if strip[base + q_out] == 0.0 {
            continue;
        }
        let q_out_f = q_out as f32;
        while edt_z[k + 1] < q_out_f {
            k += 1;
        }
        let vk_o = edt_v[k] as f64;
        let dq = q_out as f64 - vk_o;
        strip[base + q_out] = (dq * dq + edt_f_val[k] as f64) as f32;
    }
}

/// Cache-blocked 16-column Pass 2 (`03-raster.js:373`).
#[allow(clippy::too_many_arguments)]
fn edt_pass2_blocked(
    buf: &mut [f32],
    p: usize,
    tw: usize,
    gw: usize,
    gh: usize,
    edt_v: &mut [i32],
    edt_z: &mut [f32],
    edt_f_val: &mut [f32],
    edt_f_sq: &mut [f32],
    edt_strip: &mut [f32],
) {
    let x_end = p + tw;
    let mut bx = p;
    while bx < x_end {
        let n_cols = (x_end - bx).min(EDT_BLOCK);
        for y_r in 0..gh {
            let row_base = y_r * gw + bx;
            for c_r in 0..n_cols {
                edt_strip[c_r * gh + y_r] = buf[row_base + c_r];
            }
        }
        for c_e in 0..n_cols {
            edt_1d_contig(edt_strip, c_e * gh, gh, edt_v, edt_z, edt_f_val, edt_f_sq);
        }
        for y_w in 0..gh {
            let row_base_w = y_w * gw + bx;
            for c_w in 0..n_cols {
                buf[row_base_w + c_w] = edt_strip[c_w * gh + y_w];
            }
        }
        bx += EDT_BLOCK;
    }
}

/// Computes exact 2D squared EDT for both interior (`edt_d_in`) and exterior (`edt_d_out`)
/// of binary mask `(gray < thr)` on inner columns `[p .. p + tw - 1]`.
/// When `max_in_r > 0` / `max_out_r > 0`, applies bit-exact narrow-band pruning.
#[allow(clippy::too_many_arguments)]
pub fn compute_pencil_edt(
    scratch: &mut RasterScratch,
    gray: &[u8],
    gw: usize,
    gh: usize,
    p: usize,
    tw: usize,
    thr: f64,
    max_in_r: i32,
    max_out_r: i32,
) -> EdtResult {
    let n_pix = gw * gh;
    ensure_edt_buffers(scratch, n_pix, gw.max(gh));
    let mut has_ink = false;
    let mut has_paper = false;
    let max_dist = (gw + gh + 4) as i32;
    let lim_in = if max_in_r > 0 && max_in_r < max_dist {
        max_in_r
    } else {
        max_dist
    };
    let lim_out = if max_out_r > 0 && max_out_r < max_dist {
        max_out_r
    } else {
        max_dist
    };

    for y in 0..gh {
        let row_off = y * gw;
        let mut d_in_run = max_dist;
        let mut d_out_run = max_dist;
        for x in 0..gw {
            let is_ink = (gray[row_off + x] as f64) < thr;
            if is_ink {
                has_ink = true;
                d_out_run = 0;
                if d_in_run < max_dist {
                    d_in_run += 1;
                }
            } else {
                has_paper = true;
                d_in_run = 0;
                if d_out_run < max_dist {
                    d_out_run += 1;
                }
            }
            scratch.edt_d_in[row_off + x] = d_in_run as f32;
            scratch.edt_d_out[row_off + x] = d_out_run as f32;
        }

        d_in_run = max_dist;
        d_out_run = max_dist;
        for x_r in (0..gw).rev() {
            let idx_r = row_off + x_r;
            let is_ink_r = (gray[idx_r] as f64) < thr;
            if is_ink_r {
                d_out_run = 0;
                if d_in_run < max_dist {
                    d_in_run += 1;
                }
            } else {
                d_in_run = 0;
                if d_out_run < max_dist {
                    d_out_run += 1;
                }
            }
            let prev_in = scratch.edt_d_in[idx_r] as i32;
            let prev_out = scratch.edt_d_out[idx_r] as i32;
            let best_in = d_in_run.min(prev_in);
            let best_out = d_out_run.min(prev_out);
            scratch.edt_d_in[idx_r] = if best_in > lim_in {
                EDT_INF
            } else {
                (best_in as f64 * best_in as f64) as f32
            };
            scratch.edt_d_out[idx_r] = if best_out > lim_out {
                EDT_INF
            } else {
                (best_out as f64 * best_out as f64) as f32
            };
        }
    }

    if !has_ink {
        return EdtResult {
            has_ink: false,
            has_paper: true,
        };
    }
    if !has_paper {
        return EdtResult {
            has_ink: true,
            has_paper: false,
        };
    }

    let RasterScratch {
        edt_d_in,
        edt_d_out,
        edt_v,
        edt_z,
        edt_f_val,
        edt_f_sq,
        edt_strip,
        ..
    } = scratch;

    edt_pass2_blocked(
        edt_d_in, p, tw, gw, gh, edt_v, edt_z, edt_f_val, edt_f_sq, edt_strip,
    );
    edt_pass2_blocked(
        edt_d_out, p, tw, gw, gh, edt_v, edt_z, edt_f_val, edt_f_sq, edt_strip,
    );

    EdtResult {
        has_ink: true,
        has_paper: true,
    }
}

/// Shibufude v3 Pencil Stage A + Stage B (+ optional `kakeami-v8` fade chain).
/// Writes grayscale values (`0` = black ink, `255` = white paper, inverted if `cfg.invert_ink`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn process_pencil_gray(
    scratch: &mut RasterScratch,
    cfg: &KakeamiConfig,
    scene: &PreparedScene,
    origin_x: i64,
    origin_y: i64,
    p: usize,
    tw: usize,
    th: usize,
    gw: usize,
    gh: usize,
    tile_needs_v8_fade: bool,
    is_kake_v8: bool,
    is_kake_v8_gauss: bool,
    kake_fade_thr: f64,
    n_scale_p: f64,
    inv_n_scale: f64,
    dot_freq_v8: f64,
    out: &mut [u8],
) {
    let one_bit = cfg.one_bit_mode;
    let invert = cfg.invert_ink;
    let ink_g: u8 = if invert { 255 } else { 0 };
    let bg_g: u8 = if invert { 0 } else { 255 };

    let lw_ref = if cfg.pattern_family == "nawa-ami" {
        clamp(
            0.40 * (cfg.nawa_rope_width / js_round(cfg.nawa_strands as f64).max(2.0)),
            1.6,
            4.2,
        )
    } else if is_kake_v8 {
        clamp(cfg.stroke_width * 1.4, 1.3, 3.8)
    } else {
        clamp(cfg.stroke_width * 2.0, 1.8, 4.2)
    };
    let inv_w = 1.0 / lw_ref;
    let rough = clamp(cfg.grain_strength / 100.0, 0.0, 1.0);
    let dryness = if is_kake_v8 {
        clamp(
            0.32 + 0.54 * rough + 0.28 * (cfg.broken_ink as f64 / 100.0),
            0.0,
            0.95,
        )
    } else {
        clamp(
            0.24 + 0.48 * rough + 0.28 * (cfg.broken_ink as f64 / 100.0),
            0.0,
            0.95,
        )
    };
    let dry_thr_gain = if is_kake_v8 { 0.98 } else { 0.88 };
    let sharp = 0.55;
    let thr_p = if one_bit {
        cfg.one_bit_threshold as f64
    } else {
        127.5
    };

    let max_disp_out = (if is_kake_v8 { 0.09 } else { 0.22 }) * lw_ref;
    let max_disp_in = 0.34 * lw_ref;
    let aa_half_phys = if one_bit { 0.0 } else { 0.5 * inv_n_scale };
    let band_hi = max_disp_out + if one_bit { 1e-6 } else { aa_half_phys + 0.15 };
    let band_lo = -(max_disp_in + 0.15);
    let denom_b = lw_ref * (0.45 + 0.85 * dryness);
    let inv_denom_b = 1.0 / denom_b;
    let core_protect = (if is_kake_v8 { 0.18 } else { 0.28 }) * lw_ref;
    let inv_core_protect = 1.0 / core_protect.max(0.05);
    let core_skip = clamp((dryness - 0.55) / 0.45, 0.0, 1.0) * 0.38;
    let fast_core_one_bit = one_bit && dryness <= 0.65;
    let core_safe_threshold = -(core_protect + max_disp_in);
    let slope_b = 3.0 + sharp * sharp * 55.0;
    let band_hi_px = band_hi * n_scale_p + 0.5;
    let band_hi_sq = (band_hi_px * band_hi_px) as f32;
    let core_safe_px = (-core_safe_threshold) * n_scale_p + 0.5;
    let core_safe_sq = (core_safe_px * core_safe_px) as f32;

    let max_out_r = (band_hi * n_scale_p + 1.5).ceil() as i32;
    let max_in_phys = if fast_core_one_bit {
        -core_safe_threshold
    } else {
        core_protect.max(denom_b) + max_disp_in + 0.25
    };
    let max_in_r = (max_in_phys * n_scale_p + 1.5).ceil() as i32;

    // Move `gray` borrow cleanly by passing a temporary slice copy? No: `gray` is inside `scratch`,
    // so run EDT using disjoint field borrows directly.
    let edt_res = {
        let n_pix = gw * gh;
        ensure_edt_buffers(scratch, n_pix, gw.max(gh));
        let RasterScratch {
            gray,
            edt_d_in,
            edt_d_out,
            edt_v,
            edt_z,
            edt_f_val,
            edt_f_sq,
            edt_strip,
            ..
        } = scratch;
        compute_pencil_edt_disjoint(
            gray, edt_d_in, edt_d_out, edt_v, edt_z, edt_f_val, edt_f_sq, edt_strip, gw, gh, p, tw,
            thr_p, max_in_r, max_out_r,
        )
    };
    if !edt_res.has_ink {
        out.fill(bg_g);
        return;
    }
    let has_paper_p = edt_res.has_paper;

    let s_base = cfg.seed;
    let s_warp1 = s_base.wrapping_mul(97).wrapping_add(11);
    let s_warp2 = s_base.wrapping_mul(97).wrapping_add(29);
    let s_r_base = s_base.wrapping_mul(131);
    let s_o1 = s_r_base.wrapping_add(17);
    let s_o2 = s_r_base.wrapping_add(41);
    let s_o3 = s_r_base.wrapping_add(89);
    let s_o4 = s_r_base.wrapping_add(151);
    let s_c1 = s_r_base.wrapping_add(211);
    let s_c2 = s_r_base.wrapping_add(277);
    let grain_seed = s_base ^ 0x5041_5053;
    let grain_seed_micro = grain_seed.wrapping_add(0x51a7);
    let grain_seed_mid = grain_seed.wrapping_add(0x9e37);
    let streak_seed = grain_seed.wrapping_add(47);

    let wf = 0.32 * inv_w;
    let warp_amp = lw_ref * 0.38 * rough;
    let disp_gain = lw_ref * (0.36 + 0.64 * rough);
    let bump_amp = lw_ref * 0.24 * rough;
    let chip_amp = lw_ref * 0.40 * rough;
    let rem_pos1 = 0.40 * disp_gain + bump_amp + 1e-5;
    let rem_neg1 = 0.40 * disp_gain + chip_amp + core_protect + 1e-5;
    let rem_pos12 = 0.19 * disp_gain + bump_amp + 1e-5;
    let rem_neg12 = 0.19 * disp_gain + chip_amp + core_protect + 1e-5;
    let bump_eps = bump_amp + 1e-5;
    let chip_core_eps = chip_amp + core_protect + 1e-5;

    let c1 = 0.37f64.cos() * 0.18 * inv_w;
    let sn1 = 0.37f64.sin() * 0.18 * inv_w;
    let c2 = 1.09f64.cos() * 0.54 * inv_w;
    let sn2 = 1.09f64.sin() * 0.54 * inv_w;
    let c3 = 1.93f64.cos() * 1.38 * inv_w;
    let sn3 = 1.93f64.sin() * 1.38 * inv_w;
    let c4 = 2.61f64.cos() * 2.85 * inv_w;
    let sn4 = 2.61f64.sin() * 2.85 * inv_w;
    let c5 = 0.73f64.cos() * 0.78 * inv_w;
    let sn5 = 0.73f64.sin() * 0.78 * inv_w;
    let c6 = 2.29f64.cos() * 1.72 * inv_w;
    let sn6 = 2.29f64.sin() * 1.72 * inv_w;

    let f_stride_p: usize = if tile_needs_v8_fade && n_scale_p >= 1.75 {
        if n_scale_p >= 3.5 { 4 } else { 2 }
    } else {
        0
    };
    let f_grid_info = if f_stride_p > 0 {
        Some(build_fade_tile_grid(
            &mut scratch.fade_grid_buf,
            cfg,
            scene,
            origin_x + p as i64,
            origin_y + p as i64,
            tw,
            th,
            inv_n_scale,
            is_kake_v8_gauss,
            kake_fade_thr,
            s_base,
            f_stride_p,
        ))
    } else {
        None
    };

    let gray = &scratch.gray;
    let edt_d_in = &scratch.edt_d_in;
    let edt_d_out = &scratch.edt_d_out;
    let fade_buf = &scratch.fade_grid_buf;

    for y in 0..th {
        let row_out_p = y * tw;
        let gy_row_p = y + p;
        let row_off_p = gy_row_p * gw;
        let wy_raw_p = (origin_y + gy_row_p as i64) as f64;
        let wy_p = (wy_raw_p + 0.5) * inv_n_scale;
        let wy_sn1 = wy_p * sn1;
        let wy_c1 = wy_p * c1;
        let wy_sn4 = wy_p * sn4;
        let wy_c4 = wy_p * c4;
        let wy_wf = wy_p * wf;
        let wy_wf2 = wy_wf - 7.3;
        let wy_g1 = wy_p * 1.15;
        let wy_g2 = wy_p * 2.45;
        let wy_g3 = wy_p * 0.45;
        let wy_strk = wy_p * 0.72;

        let (fy_row0, fy_row1, fty) = match &f_grid_info {
            Some(fg) => {
                let gy_loc = y + fg.off_y;
                let r0 = (gy_loc >> fg.shift) * fg.gw;
                (r0, r0 + fg.gw, (gy_loc & fg.mask) as f64 * fg.inv_stride)
            }
            None => (0, 0, 0.0),
        };

        for x in 0..tw {
            let gx_p = x + p;
            let idx_p = row_off_p + gx_p;
            let gv_p = gray[idx_p];

            if has_paper_p && gv_p >= 235 && edt_d_out[idx_p] > band_hi_sq {
                out[row_out_p + x] = bg_g;
                continue;
            }

            let wx_raw_p = (origin_x + gx_p as i64) as f64;
            if tile_needs_v8_fade && one_bit {
                let wx_pre_f = (wx_raw_p + 0.5) * inv_n_scale;
                if let Some(fg) = &f_grid_info {
                    let gx_loc = x + fg.off_x;
                    let fx0 = gx_loc >> fg.shift;
                    let ftx = (gx_loc & fg.mask) as f64 * fg.inv_stride;
                    let ft00 = fade_buf[fy_row0 + fx0] as f64;
                    let ft10 = fade_buf[fy_row0 + fx0 + 1] as f64;
                    let ft01 = fade_buf[fy_row1 + fx0] as f64;
                    let ft11 = fade_buf[fy_row1 + fx0 + 1] as f64;
                    let t_fade_pre =
                        (ft00 + (ft10 - ft00) * ftx) * (1.0 - fty) + (ft01 + (ft11 - ft01) * ftx) * fty;
                    if t_fade_pre <= 0.0 {
                        out[row_out_p + x] = bg_g;
                        continue;
                    }
                    if t_fade_pre < 1.0
                        && eval_kake_v8_fade_from_t(
                            t_fade_pre,
                            wx_pre_f,
                            wy_p,
                            is_kake_v8_gauss,
                            dot_freq_v8,
                            s_base,
                            true,
                        ) < 0.5
                    {
                        out[row_out_p + x] = bg_g;
                        continue;
                    }
                } else {
                    let d_pre_f = sample_density(cfg, &scene.grid, wx_raw_p, wy_raw_p, &scene.noise);
                    if eval_kake_v8_fade(
                        d_pre_f,
                        wx_pre_f,
                        wy_p,
                        is_kake_v8_gauss,
                        kake_fade_thr,
                        dot_freq_v8,
                        s_base,
                        true,
                    ) < 0.5
                    {
                        out[row_out_p + x] = bg_g;
                        continue;
                    }
                }
            }

            if fast_core_one_bit && gv_p <= 20 && (!has_paper_p || edt_d_in[idx_p] >= core_safe_sq) {
                out[row_out_p + x] = ink_g;
                continue;
            }

            let sdf_px = if !has_paper_p {
                -1000.0
            } else if gv_p > 20 && gv_p < 235 {
                (gv_p as f64 - thr_p) / 255.0
            } else if (gv_p as f64) < thr_p {
                let d_in_sq = edt_d_in[idx_p];
                if d_in_sq >= EDT_HALF_INF {
                    -1000.0
                } else {
                    -((d_in_sq as f64).sqrt() - 0.5)
                }
            } else {
                let d_out_sq = edt_d_out[idx_p];
                if d_out_sq >= EDT_HALF_INF {
                    1000.0
                } else {
                    (d_out_sq as f64).sqrt() - 0.5
                }
            };
            let band_sdf_phys = sdf_px * inv_n_scale;

            if band_sdf_phys > band_hi {
                out[row_out_p + x] = bg_g;
                continue;
            }
            if fast_core_one_bit && band_sdf_phys <= core_safe_threshold {
                out[row_out_p + x] = ink_g;
                continue;
            }

            let wx_p = (wx_raw_p + 0.5) * inv_n_scale;

            // Stage A: Shibufude v3 2D SDF edge roughness with bit-exact interval pruning
            let eff_sdf = if band_sdf_phys < band_lo {
                band_sdf_phys
            } else {
                let o1 = (value_noise_2d(wx_p * c1 + wy_sn1, -wx_p * sn1 + wy_c1, s_o1) - 0.5) * 2.0;
                let disp1 = o1 * 0.26 * disp_gain;
                if one_bit {
                    if band_sdf_phys > disp1 + rem_pos1 {
                        out[row_out_p + x] = bg_g;
                        continue;
                    }
                    if fast_core_one_bit && band_sdf_phys <= disp1 - rem_neg1 {
                        out[row_out_p + x] = ink_g;
                        continue;
                    }
                }

                let wx_wf = wx_p * wf;
                let dwx = (value_noise_2d(wx_wf, wy_wf, s_warp1) - 0.5) * warp_amp;
                let dwy = (value_noise_2d(wx_wf + 13.7, wy_wf2, s_warp2) - 0.5) * warp_amp;
                let px_a = wx_p + dwx;
                let py_a = wy_p + dwy;

                let o2 = (value_noise_2d(px_a * c2 + py_a * sn2, -px_a * sn2 + py_a * c2, s_o2) - 0.5) * 2.0;
                let disp12 = disp1 + o2 * 0.21 * disp_gain;
                if one_bit {
                    if band_sdf_phys > disp12 + rem_pos12 {
                        out[row_out_p + x] = bg_g;
                        continue;
                    }
                    if fast_core_one_bit && band_sdf_phys <= disp12 - rem_neg12 {
                        out[row_out_p + x] = ink_g;
                        continue;
                    }
                }

                let o3 = (value_noise_2d(px_a * c3 + py_a * sn3, -px_a * sn3 + py_a * c3, s_o3) - 0.5) * 2.0;
                let o4 = (value_noise_2d(wx_p * c4 + wy_sn4, -wx_p * sn4 + wy_c4, s_o4) - 0.5) * 2.0;
                let mut disp = disp12 + (o3 * 0.125 + o4 * 0.065) * disp_gain;
                if one_bit {
                    if band_sdf_phys > disp + bump_eps {
                        out[row_out_p + x] = bg_g;
                        continue;
                    }
                    if fast_core_one_bit && band_sdf_phys <= disp - chip_core_eps {
                        out[row_out_p + x] = ink_g;
                        continue;
                    }
                }

                let c_mix = 0.62 * value_noise_2d(px_a * c5 + py_a * sn5, -px_a * sn5 + py_a * c5, s_c1)
                    + 0.38 * value_noise_2d(px_a * c6 + py_a * sn6, -px_a * sn6 + py_a * c6, s_c2);
                if c_mix > 0.65 {
                    let chip = (c_mix - 0.65) / 0.35;
                    disp -= chip.powf(1.6) * chip_amp;
                } else if c_mix < 0.30 {
                    let bump = (0.30 - c_mix) / 0.30;
                    disp += bump.powf(1.7) * bump_amp;
                }
                disp = clamp(disp, -max_disp_in, max_disp_out);
                band_sdf_phys - disp
            };

            let edge_cov = if one_bit {
                if eff_sdf > 0.0 {
                    out[row_out_p + x] = bg_g;
                    continue;
                }
                1.0
            } else {
                let ec = 0.5 - eff_sdf * n_scale_p;
                if ec <= 0.002 {
                    out[row_out_p + x] = bg_g;
                    continue;
                }
                ec.min(1.0)
            };

            // Stage B: edge kasure breakup
            let d_in_eff = if eff_sdf < 0.0 { -eff_sdf } else { 0.0 };
            if fast_core_one_bit && d_in_eff >= core_protect {
                out[row_out_p + x] = ink_g;
                continue;
            }
            let edge_norm = 1.0 - d_in_eff * inv_denom_b;
            let edge_weight = if edge_norm > 0.0 {
                edge_norm.min(1.0).powf(0.85)
            } else {
                0.0
            };
            let local_dry = (dryness * (edge_weight * 1.15 + core_skip)).min(1.0);

            if local_dry <= 0.02 {
                if one_bit {
                    out[row_out_p + x] = ink_g;
                    continue;
                }
                let mut cov_clean = edge_cov;
                if tile_needs_v8_fade {
                    let d_cln = sample_density(cfg, &scene.grid, wx_raw_p, wy_raw_p, &scene.noise);
                    cov_clean *= eval_kake_v8_fade(
                        d_cln,
                        wx_p,
                        wy_p,
                        is_kake_v8_gauss,
                        kake_fade_thr,
                        dot_freq_v8,
                        s_base,
                        false,
                    );
                }
                out[row_out_p + x] = if cov_clean >= 0.998 {
                    ink_g
                } else if cov_clean <= 0.002 {
                    bg_g
                } else if invert {
                    js_round(cov_clean * 255.0) as u8
                } else {
                    js_round((1.0 - cov_clean) * 255.0) as u8
                };
                continue;
            }

            let dry_thr = local_dry * dry_thr_gain;

            if one_bit {
                let max_mix = 1.0 - dry_thr;
                let g_fine1 = value_noise_2d(wx_p * 1.15, wy_g1, grain_seed);
                let streak1 = value_noise_2d(wx_p * 0.16, wy_strk, streak_seed);
                let base2 = 0.3575 * g_fine1 + 0.35 * streak1;
                if base2 > max_mix + 1e-5 {
                    out[row_out_p + x] = bg_g;
                    continue;
                }
                if base2 + 0.2925 <= max_mix - 1e-5 {
                    out[row_out_p + x] = ink_g;
                    continue;
                }
                let g_micro1 = value_noise_2d(wx_p * 2.45, wy_g2, grain_seed_micro);
                let base3 = base2 + 0.1625 * g_micro1;
                if base3 > max_mix + 1e-5 {
                    out[row_out_p + x] = bg_g;
                    continue;
                }
                if base3 + 0.13 <= max_mix - 1e-5 {
                    out[row_out_p + x] = ink_g;
                    continue;
                }
                let g_mid1 = value_noise_2d(wx_p * 0.45, wy_g3, grain_seed_mid);
                let mix_g1 = 0.65 * (0.55 * g_fine1 + 0.25 * g_micro1 + 0.20 * g_mid1) + 0.35 * streak1;
                let ink_score1 = 1.0 - clamp(mix_g1, 0.0, 1.0);
                out[row_out_p + x] = if ink_score1 >= dry_thr { ink_g } else { bg_g };
            } else {
                let grain_p = crisp_paper_grain(wx_p, wy_p, grain_seed);
                let streak_p = value_noise_2d(wx_p * 0.16, wy_strk, streak_seed);
                let mix_g = 0.65 * grain_p + 0.35 * streak_p;
                let ink_score = 1.0 - clamp(mix_g, 0.0, 1.0);
                let mut cov_p = clamp((ink_score - dry_thr) * slope_b + 0.5, 0.0, 1.0);
                if dryness <= 0.65 {
                    let core_w = clamp(d_in_eff * inv_core_protect, 0.0, 1.0);
                    if core_w > cov_p {
                        cov_p = core_w;
                    }
                }
                cov_p *= edge_cov;
                if cov_p > 0.002 && tile_needs_v8_fade {
                    let d_fade8 = sample_density(cfg, &scene.grid, wx_raw_p, wy_raw_p, &scene.noise);
                    cov_p *= eval_kake_v8_fade(
                        d_fade8,
                        wx_p,
                        wy_p,
                        is_kake_v8_gauss,
                        kake_fade_thr,
                        dot_freq_v8,
                        s_base,
                        false,
                    );
                }
                out[row_out_p + x] = if cov_p <= 0.002 {
                    bg_g
                } else if invert {
                    js_round(cov_p * 255.0) as u8
                } else {
                    js_round((1.0 - cov_p) * 255.0) as u8
                };
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn compute_pencil_edt_disjoint(
    gray: &[u8],
    edt_d_in: &mut [f32],
    edt_d_out: &mut [f32],
    edt_v: &mut [i32],
    edt_z: &mut [f32],
    edt_f_val: &mut [f32],
    edt_f_sq: &mut [f32],
    edt_strip: &mut [f32],
    gw: usize,
    gh: usize,
    p: usize,
    tw: usize,
    thr: f64,
    max_in_r: i32,
    max_out_r: i32,
) -> EdtResult {
    let mut has_ink = false;
    let mut has_paper = false;
    let max_dist = (gw + gh + 4) as i32;
    let lim_in = if max_in_r > 0 && max_in_r < max_dist {
        max_in_r
    } else {
        max_dist
    };
    let lim_out = if max_out_r > 0 && max_out_r < max_dist {
        max_out_r
    } else {
        max_dist
    };

    for y in 0..gh {
        let row_off = y * gw;
        let mut d_in_run = max_dist;
        let mut d_out_run = max_dist;
        for x in 0..gw {
            let is_ink = (gray[row_off + x] as f64) < thr;
            if is_ink {
                has_ink = true;
                d_out_run = 0;
                if d_in_run < max_dist {
                    d_in_run += 1;
                }
            } else {
                has_paper = true;
                d_in_run = 0;
                if d_out_run < max_dist {
                    d_out_run += 1;
                }
            }
            edt_d_in[row_off + x] = d_in_run as f32;
            edt_d_out[row_off + x] = d_out_run as f32;
        }

        d_in_run = max_dist;
        d_out_run = max_dist;
        for x_r in (0..gw).rev() {
            let idx_r = row_off + x_r;
            let is_ink_r = (gray[idx_r] as f64) < thr;
            if is_ink_r {
                d_out_run = 0;
                if d_in_run < max_dist {
                    d_in_run += 1;
                }
            } else {
                d_in_run = 0;
                if d_out_run < max_dist {
                    d_out_run += 1;
                }
            }
            let prev_in = edt_d_in[idx_r] as i32;
            let prev_out = edt_d_out[idx_r] as i32;
            let best_in = d_in_run.min(prev_in);
            let best_out = d_out_run.min(prev_out);
            edt_d_in[idx_r] = if best_in > lim_in {
                EDT_INF
            } else {
                (best_in as f64 * best_in as f64) as f32
            };
            edt_d_out[idx_r] = if best_out > lim_out {
                EDT_INF
            } else {
                (best_out as f64 * best_out as f64) as f32
            };
        }
    }

    if !has_ink {
        return EdtResult {
            has_ink: false,
            has_paper: true,
        };
    }
    if !has_paper {
        return EdtResult {
            has_ink: true,
            has_paper: false,
        };
    }

    edt_pass2_blocked(
        edt_d_in, p, tw, gw, gh, edt_v, edt_z, edt_f_val, edt_f_sq, edt_strip,
    );
    edt_pass2_blocked(
        edt_d_out, p, tw, gw, gh, edt_v, edt_z, edt_f_val, edt_f_sq, edt_strip,
    );
    EdtResult {
        has_ink: true,
        has_paper: true,
    }
}
