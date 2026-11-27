//! Physical-space paper grain & DPI-invariant `kakeami-v8` stipple/gaussian fade.
//! 1:1 port of `valueNoise2D`, `crispPaperGrain`, `evalKakeV8Fade`, `evalKakeV8FadeFromT`,
//! and `buildFadeTileGrid` from `src/03-raster.js` (lines 124–296).

use crate::config::KakeamiConfig;
use crate::density::sample_density;
use crate::geometry::PreparedScene;
use crate::math::{clamp, smoothstep};

/// Raster-domain deterministic 2D value noise (Murmur3 fmix32, 1:1 with `03-raster.js:124`).
#[inline]
pub(crate) fn value_noise_2d(x: f64, y: f64, seed: u32) -> f64 {
    let ix_f = x.floor();
    let iy_f = y.floor();
    let fx = x - ix_f;
    let fy = y - iy_f;
    let ix = ix_f as i64 as i32;
    let iy = iy_f as i64 as i32;
    let ux = fx * fx * (3.0 - 2.0 * fx);
    let uy = fy * fy * (3.0 - 2.0 * fy);

    let hx0 = (ix as u32).wrapping_mul(374_761_393);
    let hx1 = hx0.wrapping_add(374_761_393);
    let hy0 = (iy as u32).wrapping_mul(668_265_263).wrapping_add(seed);
    let hy1 = hy0.wrapping_add(668_265_263);

    #[inline]
    fn mix(h: u32) -> f64 {
        let mut v = h ^ 0x9e37_79b9;
        v = (v ^ (v >> 16)).wrapping_mul(0x85eb_ca6b);
        v = (v ^ (v >> 13)).wrapping_mul(0xc2b2_ae35);
        (v ^ (v >> 16)) as f64
    }

    let v00 = mix(hx0.wrapping_add(hy0));
    let v10 = mix(hx1.wrapping_add(hy0));
    let v01 = mix(hx0.wrapping_add(hy1));
    let v11 = mix(hx1.wrapping_add(hy1));
    let a = v00 + (v10 - v00) * ux;
    let b = v01 + (v11 - v01) * ux;
    (a + (b - a) * uy) * 2.328_306_436_538_696_3e-10
}

/// Smooth continuous physical-space paper grain (`03-raster.js:159`).
#[inline]
pub(crate) fn crisp_paper_grain(wx: f64, wy: f64, seed: u32) -> f64 {
    let g_fine = value_noise_2d(wx * 1.15, wy * 1.15, seed);
    let g_micro = value_noise_2d(wx * 2.45, wy * 2.45, seed.wrapping_add(0x51a7));
    let g_mid = value_noise_2d(wx * 0.45, wy * 0.45, seed.wrapping_add(0x9e37));
    0.55 * g_fine + 0.25 * g_micro + 0.20 * g_mid
}

/// v8.0b DPI-invariant stipple / gaussian fade evaluator (`03-raster.js:173`).
#[allow(clippy::too_many_arguments)]
#[inline]
pub(crate) fn eval_kake_v8_fade(
    d_pix: f64,
    wx_phys: f64,
    wy_phys: f64,
    is_gauss: bool,
    fade_thr: f64,
    dot_freq: f64,
    seed: u32,
    one_bit: bool,
) -> f64 {
    if d_pix <= 0.005 {
        return 0.0;
    }
    if !is_gauss {
        if d_pix >= fade_thr {
            return 1.0;
        }
        let t_s = clamp((d_pix - 0.005) / (fade_thr - 0.005).max(0.01), 0.0, 1.0);
        let g_raw = crisp_paper_grain(wx_phys * dot_freq, wy_phys * dot_freq, seed ^ 0x571b_b1e8);
        let n_stip = clamp((g_raw - 0.18) / 0.64, 0.0, 1.0);
        if one_bit {
            return if n_stip < t_s { 1.0 } else { 0.0 };
        }
        return clamp((t_s - n_stip) * 6.0 + 0.5, 0.0, 1.0);
    }

    let top_thr = fade_thr * 1.30;
    if d_pix >= top_thr {
        return 1.0;
    }
    let s_c1 = seed ^ 0x4741_5553;
    let s_c2 = s_c1.wrapping_add(0x3c6e_f372);
    let s_c3 = s_c1.wrapping_add(0x9e37_79b9);
    let c1 = value_noise_2d(wx_phys * 0.14, wy_phys * 0.14, s_c1);
    let c2 = value_noise_2d(
        (wx_phys * 0.8660254 + wy_phys * 0.5) * 0.21,
        (-wx_phys * 0.5 + wy_phys * 0.8660254) * 0.21,
        s_c2,
    );
    let c3 = value_noise_2d(
        (wx_phys * 0.5 + wy_phys * 0.8660254) * 0.31,
        (-wx_phys * 0.8660254 + wy_phys * 0.5) * 0.31,
        s_c3,
    );
    let gauss_cloud = ((c1 + c2 + c3) * 0.333_333_333_333_333_3 - 0.5) * 2.0;
    let d_eff = clamp(
        d_pix + gauss_cloud * 0.15 * (1.0 - d_pix) * smoothstep(0.005, 0.08, d_pix),
        0.0,
        1.0,
    );
    let t_g = clamp((d_eff - 0.005) / (fade_thr * 1.25 - 0.005).max(0.01), 0.0, 1.0);
    if one_bit {
        if t_g <= 0.0 {
            return 0.0;
        }
        if t_g >= 1.0 {
            return 1.0;
        }
    }

    eval_kake_v8_fade_from_t(t_g, wx_phys, wy_phys, true, dot_freq, seed, one_bit)
}

/// High-frequency dot/grain evaluator from pre-evaluated smooth ramp `t_val` (`03-raster.js:225`).
#[inline]
pub(crate) fn eval_kake_v8_fade_from_t(
    t_val: f64,
    wx_phys: f64,
    wy_phys: f64,
    is_gauss: bool,
    dot_freq: f64,
    seed: u32,
    one_bit: bool,
) -> f64 {
    if t_val <= 0.0 {
        return 0.0;
    }
    if one_bit && t_val >= 1.0 {
        return 1.0;
    }
    if !is_gauss {
        if t_val >= 1.0 {
            return 1.0;
        }
        let g_raw = crisp_paper_grain(wx_phys * dot_freq, wy_phys * dot_freq, seed ^ 0x571b_b1e8);
        let n_stip = clamp((g_raw - 0.18) / 0.64, 0.0, 1.0);
        if one_bit {
            return if n_stip < t_val { 1.0 } else { 0.0 };
        }
        return clamp((t_val - n_stip) * 6.0 + 0.5, 0.0, 1.0);
    }

    let f_g = dot_freq * 1.15;
    let s_g1 = seed ^ 0x6761_7573;
    let s_g2 = s_g1.wrapping_add(0x51a7);
    let s_g3 = s_g1.wrapping_add(0x9e37);
    let g1 = value_noise_2d(wx_phys * f_g, wy_phys * f_g, s_g1);
    if one_bit {
        let lim_g = 3.0 * (0.18 + 0.64 * t_val);
        if g1 >= lim_g + 1e-5 {
            return 0.0;
        }
        if g1 + 2.0 < lim_g - 1e-5 {
            return 1.0;
        }
        let g2o = value_noise_2d(
            (wx_phys * 0.809017 + wy_phys * 0.587785) * f_g * 1.37,
            (-wx_phys * 0.587785 + wy_phys * 0.809017) * f_g * 1.37,
            s_g2,
        );
        let g12 = g1 + g2o;
        if g12 >= lim_g + 1e-5 {
            return 0.0;
        }
        if g12 + 1.0 < lim_g - 1e-5 {
            return 1.0;
        }
        let g3o = value_noise_2d(
            (wx_phys * 0.309017 + wy_phys * 0.951057) * f_g * 1.93,
            (-wx_phys * 0.951057 + wy_phys * 0.309017) * f_g * 1.93,
            s_g3,
        );
        let n_gauss_o = clamp(((g12 + g3o) * 0.333_333_333_333_333_3 - 0.18) / 0.64, 0.0, 1.0);
        return if n_gauss_o < t_val { 1.0 } else { 0.0 };
    }
    let g2 = value_noise_2d(
        (wx_phys * 0.809017 + wy_phys * 0.587785) * f_g * 1.37,
        (-wx_phys * 0.587785 + wy_phys * 0.809017) * f_g * 1.37,
        s_g2,
    );
    let g3 = value_noise_2d(
        (wx_phys * 0.309017 + wy_phys * 0.951057) * f_g * 1.93,
        (-wx_phys * 0.951057 + wy_phys * 0.309017) * f_g * 1.93,
        s_g3,
    );
    let n_gauss = clamp(((g1 + g2 + g3) * 0.333_333_333_333_333_3 - 0.18) / 0.64, 0.0, 1.0);
    clamp((t_val - n_gauss) * 5.0 + 0.5, 0.0, 1.0)
}

/// Global-coordinate-aligned physical 600-DPI fade ramp grid (seam-free across arbitrary tiles).
pub(crate) struct FadeGridInfo {
    pub(crate) gw: usize,
    pub(crate) shift: u32,
    pub(crate) mask: usize,
    pub(crate) inv_stride: f64,
    pub(crate) off_x: usize,
    pub(crate) off_y: usize,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_fade_tile_grid(
    buf: &mut Vec<f32>,
    cfg: &KakeamiConfig,
    scene: &PreparedScene,
    wx0: i64,
    wy0: i64,
    tw: usize,
    th: usize,
    inv_n_scale: f64,
    is_gauss: bool,
    fade_thr: f64,
    seed: u32,
    stride: usize,
) -> FadeGridInfo {
    let s_i64 = stride as i64;
    // Align grid origin to global multiples of `stride` so adjacent tiles share exact nodes
    let base_gx = wx0.div_euclid(s_i64) * s_i64;
    let base_gy = wy0.div_euclid(s_i64) * s_i64;
    let off_x = (wx0 - base_gx) as usize;
    let off_y = (wy0 - base_gy) as usize;

    let gw_f = (off_x + tw).div_ceil(stride) + 1;
    let gh_f = (off_y + th).div_ceil(stride) + 1;
    let need = gw_f * gh_f;
    if buf.len() < need {
        buf.resize(need, 0.0);
    }

    let top_thr = if is_gauss { fade_thr * 1.30 } else { fade_thr };
    let inv_span = 1.0 / ((if is_gauss { fade_thr * 1.25 } else { fade_thr }) - 0.005).max(0.01);
    let s_c1 = seed ^ 0x4741_5553;
    let s_c2 = s_c1.wrapping_add(0x3c6e_f372);
    let s_c3 = s_c1.wrapping_add(0x9e37_79b9);

    for iy in 0..gh_f {
        let gy_n = (base_gy + (iy as i64) * s_i64) as f64;
        let wy_n = (gy_n + 0.5) * inv_n_scale;
        let row_off = iy * gw_f;
        for ix in 0..gw_f {
            let gx_n = (base_gx + (ix as i64) * s_i64) as f64;
            let d_n = sample_density(cfg, &scene.grid, gx_n, gy_n, &scene.noise);
            let t_out = if d_n <= 0.005 {
                0.0
            } else if d_n >= top_thr {
                1.0
            } else if !is_gauss {
                clamp((d_n - 0.005) * inv_span, 0.0, 1.0)
            } else {
                let wx_n = (gx_n + 0.5) * inv_n_scale;
                let c1 = value_noise_2d(wx_n * 0.14, wy_n * 0.14, s_c1);
                let c2 = value_noise_2d(
                    (wx_n * 0.8660254 + wy_n * 0.5) * 0.21,
                    (-wx_n * 0.5 + wy_n * 0.8660254) * 0.21,
                    s_c2,
                );
                let c3 = value_noise_2d(
                    (wx_n * 0.5 + wy_n * 0.8660254) * 0.31,
                    (-wx_n * 0.8660254 + wy_n * 0.5) * 0.31,
                    s_c3,
                );
                let gauss_cloud = ((c1 + c2 + c3) * 0.333_333_333_333_333_3 - 0.5) * 2.0;
                let d_eff = clamp(
                    d_n + gauss_cloud * 0.15 * (1.0 - d_n) * smoothstep(0.005, 0.08, d_n),
                    0.0,
                    1.0,
                );
                clamp((d_eff - 0.005) * inv_span, 0.0, 1.0)
            };
            buf[row_off + ix] = t_out as f32;
        }
    }

    let shift = if stride == 4 { 2 } else { 1 };
    FadeGridInfo {
        gw: gw_f,
        shift,
        mask: stride - 1,
        inv_stride: 1.0 / stride as f64,
        off_x,
        off_y,
    }
}
