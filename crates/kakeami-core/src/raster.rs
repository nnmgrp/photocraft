//! CPU rasterizer & post-processing pipeline (256-level analytic scanline AA + `Shibufude v3 Pencil` + `kakeami-v8` fade).
//! 1:1 port of `src/03-raster.js` and tile padding from `src/04-app-core.js`.
//!
//! Strictly `#![forbid(unsafe_code)]`, zero platform-specific APIs (`wasm32`-ready),
//! and zero per-tile allocations when reusing [`RasterScratch`].

mod analytic;
pub mod fade;
pub mod pencil;

use crate::config::KakeamiConfig;
use crate::density::sample_density;
use crate::geometry::PreparedScene;
use crate::grid::Rect;
use crate::math::{clamp, hash2d, js_round, mod_i, smoothstep};
use analytic::{rasterize_batch_to_gray, AnalyticScratch};
use fade::eval_kake_v8_fade;
use pencil::process_pencil_gray;
use std::sync::atomic::{AtomicBool, Ordering};

pub use pencil::{compute_pencil_edt, EdtResult, EDT_HALF_INF, EDT_INF};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// Reusable per-worker scratch buffers for zero-allocation tiled rasterization.
#[derive(Default)]
pub struct RasterScratch {
    pub(crate) aa: AnalyticScratch,
    pub(crate) gray: Vec<u8>,
    pub(crate) fade_grid_buf: Vec<f32>,
    pub(crate) edt_d_in: Vec<f32>,
    pub(crate) edt_d_out: Vec<f32>,
    pub(crate) edt_v: Vec<i32>,
    pub(crate) edt_z: Vec<f32>,
    pub(crate) edt_f_val: Vec<f32>,
    pub(crate) edt_f_sq: Vec<f32>,
    pub(crate) edt_strip: Vec<f32>,
}

impl RasterScratch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read-only view of interior squared EDT computed by [`compute_pencil_edt`].
    pub fn edt_d_in(&self) -> &[f32] {
        &self.edt_d_in
    }

    /// Read-only view of exterior squared EDT computed by [`compute_pencil_edt`].
    pub fn edt_d_out(&self) -> &[f32] {
        &self.edt_d_out
    }

    /// Read-only view of the pre-threshold scanline `gray` buffer from the last tile render.
    pub fn gray_buf(&self) -> &[u8] {
        &self.gray
    }

    /// Mutable access to the pre-threshold scanline `gray` buffer (useful for post-process parity tests).
    pub fn gray_buf_mut(&mut self) -> &mut Vec<u8> {
        &mut self.gray
    }
}

/// Exact tile margin in pixels (`src/04-app-core.js:685-707`, `src/06-ui.js:1721-1738`).
pub fn compute_tile_pad(cfg: &KakeamiConfig) -> u32 {
    let mut pad: u32 = 0;
    if cfg.ink_pooling > 0.0 {
        pad += 1;
    }
    let needs_round_stipple = cfg.finish_mode == "macroDot" && cfg.pattern_family != "nawa-ami";
    if needs_round_stipple {
        let dot_sz = clamp(cfg.stipple_dot_size, 1.2, 16.0);
        let st_cell = (dot_sz * 1.25).max(2.0);
        pad += st_cell.ceil() as u32 + 2;
    }
    if cfg.finish_mode == "pencil" {
        let n_sc_p = clamp(cfg.nawa_scale, 0.4, 4.5);
        let lw_ref_p = if cfg.pattern_family == "nawa-ami" {
            clamp(
                0.40 * (cfg.nawa_rope_width / js_round(cfg.nawa_strands as f64).max(2.0)),
                1.6,
                4.2,
            )
        } else if cfg.pattern_family == "kakeami-v8" {
            clamp(cfg.stroke_width * 1.4, 1.3, 3.8)
        } else {
            clamp(cfg.stroke_width * 2.0, 1.8, 4.2)
        };
        let pen_pad = ((lw_ref_p * 1.3).max(3.5) * n_sc_p).ceil() as u32 + 2;
        if pen_pad > pad {
            pad = pen_pad;
        }
    }
    pad
}

const BAYER8: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

#[inline]
fn bayer8(bx: i64, by: i64) -> f64 {
    let iy = by.rem_euclid(8) as usize;
    let ix = bx.rem_euclid(8) as usize;
    BAYER8[iy][ix] as f64 / 64.0
}

#[inline]
fn tooth(cfg: &KakeamiConfig, gx: i64, gy: i64) -> f64 {
    let x = if cfg.seamless {
        mod_i(gx, cfg.width.max(1) as i64) as i32
    } else {
        gx as i32
    };
    let y = if cfg.seamless {
        mod_i(gy, cfg.height.max(1) as i64) as i32
    } else {
        gy as i32
    };
    let h = hash2d(cfg.seed ^ 0x7a11_c0de, x, y);
    (h as f64 / 4_294_967_296.0 - 0.5) * 2.0 * (cfg.paper_tooth as f64 * 0.01) * 42.0
}

/// Render a single tile/region using a precomputed `&PreparedScene` into a flat grayscale
/// buffer of length `tw * th` (`0` = black ink, `255` = white paper; strictly `0` or `255`
/// when `cfg.one_bit_mode == true`).
///
/// Uses `i64` global pixel coordinates to stay overflow-safe up to 32768×32768+ documents.
pub fn render_tile_gray(
    scene: &PreparedScene,
    cfg: &KakeamiConfig,
    rect: Rect,
    scratch: &mut RasterScratch,
) -> Vec<u8> {
    let tw = rect.w.max(0.0).round() as usize;
    let th = rect.h.max(0.0).round() as usize;
    let mut out = vec![if cfg.invert_ink { 0 } else { 255 }; tw * th];
    if tw == 0 || th == 0 {
        return out;
    }
    render_tile_gray_into(scene, cfg, rect, scratch, &mut out);
    out
}

/// Same as [`render_tile_gray`], writing into a caller-provided slice of length `>= tw * th`.
pub fn render_tile_gray_into(
    scene: &PreparedScene,
    cfg: &KakeamiConfig,
    rect: Rect,
    scratch: &mut RasterScratch,
    out: &mut [u8],
) {
    let tw = rect.w.max(0.0).round() as usize;
    let th = rect.h.max(0.0).round() as usize;
    if tw == 0 || th == 0 {
        return;
    }
    assert!(
        out.len() >= tw * th,
        "output slice too small: len={} < tw*th={}",
        out.len(),
        tw * th
    );
    let out = &mut out[..tw * th];

    let pad = compute_tile_pad(cfg) as usize;
    let ss: usize = if cfg.supersample2x { 2 } else { 1 };
    let wx0 = rect.x.round() as i64;
    let wy0 = rect.y.round() as i64;
    let origin_x = wx0 - pad as i64;
    let origin_y = wy0 - pad as i64;
    let gw = tw + 2 * pad;
    let gh = th + 2 * pad;

    let expanded_rect = Rect {
        x: origin_x as f64,
        y: origin_y as f64,
        w: gw as f64,
        h: gh as f64,
    };

    // Hot path: strictly via PreparedScene::build_ribbons (never free build_ribbons)
    let batch = scene.build_ribbons(cfg, expanded_rect);

    rasterize_batch_to_gray(
        &mut scratch.aa,
        &mut scratch.gray,
        &batch,
        origin_x,
        origin_y,
        gw,
        gh,
        ss,
    );

    postprocess_gray_into(scene, cfg, origin_x, origin_y, pad, tw, th, scratch, out);
}

/// Run the full post-processing pipeline (`inkPooling`, `Shibufude v3 Pencil`, `kakeami-v8` fade,
/// `pixel1` / `macroDot`, `sugomi`, `clean` + `paperTooth`) on a pre-rasterized `scratch.gray`
/// buffer of size `(tw + 2*pad) × (th + 2*pad)` anchored at `(origin_x, origin_y)`.
#[allow(clippy::too_many_arguments)]
pub fn postprocess_gray_into(
    scene: &PreparedScene,
    cfg: &KakeamiConfig,
    origin_x: i64,
    origin_y: i64,
    pad: usize,
    tw: usize,
    th: usize,
    scratch: &mut RasterScratch,
    out: &mut [u8],
) {
    let gw = tw + 2 * pad;
    let gh = th + 2 * pad;

    // --- 2. Ink pooling (in-place 3x3 Gauss-Seidel, 1:1 with 03-raster.js:556-582) ---
    if cfg.ink_pooling > 0.0 {
        let pull_k = cfg.ink_pooling * 0.01 * 0.85;
        let gray = &mut scratch.gray;
        for y in 0..gh {
            for x in 0..gw {
                let idx = y * gw + x;
                let gc = gray[idx] as f64;
                if gc <= 60.0 {
                    continue;
                }
                let mut sum: u32 = 0;
                for dy in -1isize..=1 {
                    let ny = (y as isize + dy).clamp(0, gh as isize - 1) as usize;
                    for dx in -1isize..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let nx = (x as isize + dx).clamp(0, gw as isize - 1) as usize;
                        sum += gray[ny * gw + nx] as u32;
                    }
                }
                let avg8 = sum as f64 * 0.125;
                if avg8 < 130.0 {
                    let pull = (130.0 - avg8) * pull_k;
                    let nv = gc - pull;
                    gray[idx] = if nv < 0.0 {
                        0
                    } else if nv > 255.0 {
                        255
                    } else {
                        nv as u8
                    };
                }
            }
        }
    }

    // --- 3. Post-processing & output mode ---
    let one_bit = cfg.one_bit_mode;
    let threshold = cfg.one_bit_threshold as f64;
    let invert = cfg.invert_ink;
    let mode = cfg.finish_mode.as_str();
    let tooth_amt = cfg.paper_tooth > 0 && mode == "clean";
    let ink_g: u8 = if invert { 255 } else { 0 };
    let bg_g: u8 = if invert { 0 } else { 255 };

    let is_kake_v8 = cfg.pattern_family == "kakeami-v8";
    let kake_fade_mode = cfg.kake_fade_mode.as_str();
    let is_kake_v8_fade = is_kake_v8 && (kake_fade_mode == "stipple" || kake_fade_mode == "gaussian");
    let is_kake_v8_gauss = is_kake_v8_fade && kake_fade_mode == "gaussian";
    let kake_fade_thr = clamp(cfg.kake_fade_threshold, 0.05, 0.65);
    let n_scale_v8 = clamp(cfg.nawa_scale, 0.40, 4.50);
    let inv_n_scale_v8 = 1.0 / n_scale_v8;
    let dot_phys_v8 = clamp(cfg.stipple_dot_size * inv_n_scale_v8, 0.8, 8.0);
    let dot_freq_v8 = 1.85 / dot_phys_v8;

    let mut tile_needs_v8_fade = is_kake_v8_fade;
    if is_kake_v8_fade {
        let top_fade_guard = (if is_kake_v8_gauss {
            kake_fade_thr * 1.30
        } else {
            kake_fade_thr
        }) + 0.12;
        let mut all_above_fade = true;
        let tw_m1 = (tw.saturating_sub(1)) as f64;
        let th_m1 = (th.saturating_sub(1)) as f64;
        'probe: for py_g in 0..=8 {
            let sy_g = (origin_y + pad as i64) as f64 + (py_g as f64 * th_m1) * 0.125;
            for px_g in 0..=8 {
                let sx_g = (origin_x + pad as i64) as f64 + (px_g as f64 * tw_m1) * 0.125;
                if sample_density(cfg, &scene.grid, sx_g, sy_g, &scene.noise) < top_fade_guard {
                    all_above_fade = false;
                    break 'probe;
                }
            }
        }
        if all_above_fade {
            tile_needs_v8_fade = false;
        }
    }

    // 3a. Shibufude v3 Pencil
    if mode == "pencil" {
        process_pencil_gray(
            scratch,
            cfg,
            scene,
            origin_x,
            origin_y,
            pad,
            tw,
            th,
            gw,
            gh,
            tile_needs_v8_fade,
            is_kake_v8,
            is_kake_v8_gauss,
            kake_fade_thr,
            n_scale_v8,
            inv_n_scale_v8,
            dot_freq_v8,
            out,
        );
        return;
    }

    // 3b. kakeami-v8 stipple / gaussian fade (non-pencil)
    if tile_needs_v8_fade {
        let skip_gray_v8 = if one_bit { threshold } else { 252.0 };
        let seed_v8 = cfg.seed;
        let gray = &scratch.gray;
        for y in 0..th {
            let row_out = y * tw;
            let gy_row = y + pad;
            let wy_raw = origin_y + gy_row as i64;
            let wy_phys = (wy_raw as f64 + 0.5) * inv_n_scale_v8;
            let row_base = gy_row * gw;
            for x in 0..tw {
                let gx = x + pad;
                let mut gv = gray[row_base + gx] as f64;
                if tooth_amt {
                    gv = clamp(gv + tooth(cfg, origin_x + gx as i64, wy_raw), 0.0, 255.0);
                }
                if gv >= skip_gray_v8 {
                    out[row_out + x] = bg_g;
                    continue;
                }
                let wx_raw = origin_x + gx as i64;
                let d_pix = sample_density(cfg, &scene.grid, wx_raw as f64, wy_raw as f64, &scene.noise);
                if d_pix <= 0.005 {
                    out[row_out + x] = bg_g;
                    continue;
                }
                let wx_phys = (wx_raw as f64 + 0.5) * inv_n_scale_v8;
                let fade_w = eval_kake_v8_fade(
                    d_pix,
                    wx_phys,
                    wy_phys,
                    is_kake_v8_gauss,
                    kake_fade_thr,
                    dot_freq_v8,
                    seed_v8,
                    one_bit,
                );
                if one_bit {
                    out[row_out + x] = if fade_w >= 0.5 { ink_g } else { bg_g };
                } else {
                    let cov = ((255.0 - gv) / 255.0) * fade_w;
                    out[row_out + x] = if cov <= 0.002 {
                        bg_g
                    } else if invert {
                        js_round(cov * 255.0) as u8
                    } else {
                        js_round((1.0 - cov) * 255.0) as u8
                    };
                }
            }
        }
        return;
    }

    // 3c. Raster stipple (`macroDot` non-nawa, and `nawa-ami` + `pixel1`)
    let is_white_nawa = cfg.pattern_family == "nawa-ami"
        && (cfg.nawa_tone_style == "paleWhite" || cfg.nawa_tone_style == "denseWhite");
    let is_stipple_grad = cfg.pattern_family == "nawa-ami"
        && cfg.nawa_grad_mode == "stipple"
        && cfg.nawa_tone_style == "normal";
    let legacy_macro_fallback = mode == "macroDot" && cfg.pattern_family != "nawa-ami";
    let dot_shape = if legacy_macro_fallback {
        "round"
    } else if cfg.stipple_dot_shape == "pixel1"
        || cfg.stipple_dot_shape == "round"
        || cfg.stipple_dot_shape == "chain"
    {
        cfg.stipple_dot_shape.as_str()
    } else {
        "round"
    };
    let run_raster_stipple = mode != "sugomi"
        && (legacy_macro_fallback || ((is_white_nawa || is_stipple_grad) && dot_shape == "pixel1"));

    if run_raster_stipple {
        let gray = &scratch.gray;
        if dot_shape == "round" {
            let dot_diam = clamp(cfg.stipple_dot_size, 1.2, 16.0);
            let base_cell = (dot_diam * 1.18).max(2.0);
            let nx_per = if cfg.seamless {
                (js_round(cfg.width as f64 / base_cell) as i64).max(1)
            } else {
                0
            };
            let ny_per = if cfg.seamless {
                (js_round(cfg.height as f64 / base_cell) as i64).max(1)
            } else {
                0
            };
            let cell_x = if cfg.seamless {
                cfg.width as f64 / nx_per as f64
            } else {
                base_cell
            };
            let cell_y = if cfg.seamless {
                cfg.height as f64 / ny_per as f64
            } else {
                base_cell
            };
            let con = clamp(cfg.stipple_contrast / 100.0, 0.0, 1.0);
            let con_lo = 0.04 + 0.14 * con;
            let con_hi = 0.72 - 0.28 * con;
            let is_pale = cfg.nawa_tone_style == "paleWhite";
            let is_dense = cfg.nawa_tone_style == "denseWhite";

            let cx_min = ((origin_x + pad as i64) as f64 / cell_x).floor() as i64 - 1;
            let cx_max = ((origin_x + pad as i64 + tw as i64 - 1) as f64 / cell_x).floor() as i64 + 1;
            let cy_min = ((origin_y + pad as i64) as f64 / cell_y).floor() as i64 - 1;
            let cy_max = ((origin_y + pad as i64 + th as i64 - 1) as f64 / cell_y).floor() as i64 + 1;
            let cg_w = (cx_max - cx_min + 1) as usize;
            let cg_h = (cy_max - cy_min + 1) as usize;
            let c_count = cg_w * cg_h;

            let mut dot_xs = vec![0.0f64; c_count];
            let mut dot_ys = vec![0.0f64; c_count];
            let mut dot_r2s = vec![0.0f64; c_count];
            let mut dot_on = vec![false; c_count];
            let mut dot_dark_arr = vec![0.0f32; c_count];

            for cy in cy_min..=cy_max {
                let ncy = if cfg.seamless { mod_i(cy, ny_per) as i32 } else { cy as i32 };
                let c_row_off = (cy - cy_min) as usize * cg_w;
                for cx in cx_min..=cx_max {
                    let ncx = if cfg.seamless { mod_i(cx, nx_per) as i32 } else { cx as i32 };
                    let h1 = hash2d(cfg.seed ^ 0x2d8a_4f1b, ncx, ncy);
                    let h2 = hash2d(cfg.seed ^ 0x6f3b_9c41, ncx, ncy);
                    let jx_d = 0.22 + ((h1 & 0xffff) as f64 / 65535.0) * 0.56;
                    let jy_d = 0.22 + (((h1 >> 16) & 0xffff) as f64 / 65535.0) * 0.56;
                    let t_dot = (h2 & 0xffff) as f64 / 65535.0;
                    let r_jit = 0.85 + (((h2 >> 16) & 0xffff) as f64 / 65535.0) * 0.30;

                    let wx_d = (cx as f64 + jx_d) * cell_x;
                    let wy_d = (cy as f64 + jy_d) * cell_y;
                    let gx_d = ((js_round(wx_d) as i64 - origin_x).clamp(0, gw as i64 - 1)) as usize;
                    let gy_d = ((js_round(wy_d) as i64 - origin_y).clamp(0, gh as i64 - 1)) as usize;
                    let dot_dark = (255.0 - gray[gy_d * gw + gx_d] as f64) / 255.0;
                    let c_idx = c_row_off + (cx - cx_min) as usize;
                    dot_xs[c_idx] = wx_d;
                    dot_ys[c_idx] = wy_d;

                    if dot_dark <= 0.02 {
                        continue;
                    }
                    let d_dot = sample_density(cfg, &scene.grid, wx_d, wy_d, &scene.noise);
                    if d_dot <= 0.002 {
                        continue;
                    }
                    let dot_dark_cont = clamp(smoothstep(con_lo, con_hi, dot_dark), 0.0, 1.0);
                    if dot_dark_cont <= 0.01 {
                        continue;
                    }
                    let (p_dot, r_mul) = if is_pale {
                        (clamp(dot_dark_cont * (0.24 + 0.62 * d_dot), 0.0, 0.56), 0.86 + 0.18 * d_dot)
                    } else if is_dense {
                        (clamp(dot_dark_cont * (0.42 + 0.82 * d_dot), 0.0, 0.92), 0.92 + 0.26 * d_dot)
                    } else if legacy_macro_fallback && !is_stipple_grad {
                        (clamp(dot_dark_cont * 0.92, 0.0, 0.95), 0.94 + 0.16 * dot_dark_cont)
                    } else {
                        let u_n = smoothstep(0.0, 0.54, d_dot);
                        (clamp(dot_dark_cont * u_n.powf(1.05), 0.0, 0.98), 0.86 + 0.30 * u_n)
                    };
                    if p_dot > t_dot {
                        let rad = 0.5 * dot_diam * r_jit * r_mul;
                        dot_r2s[c_idx] = rad * rad;
                        dot_dark_arr[c_idx] = dot_dark_cont as f32;
                        dot_on[c_idx] = true;
                    }
                }
            }

            let w_mod = (cfg.width as i64).max(1);
            let h_mod = (cfg.height as i64).max(1);
            for y in 0..th {
                let row_out_r = y * tw;
                let gy_row_r = y + pad;
                let wy_r = origin_y + gy_row_r as i64;
                let c_base_y = (wy_r as f64 / cell_y).floor() as i64 - cy_min;
                for x in 0..tw {
                    let gx_r = x + pad;
                    let wx_r = origin_x + gx_r as i64;
                    let gv_r = gray[gy_row_r * gw + gx_r];

                    let mut d_pix_r = 0.0;
                    if is_stipple_grad {
                        d_pix_r = sample_density(cfg, &scene.grid, wx_r as f64, wy_r as f64, &scene.noise);
                        if d_pix_r <= 0.002 {
                            out[row_out_r + x] = bg_g;
                            continue;
                        }
                        if d_pix_r >= 0.56 {
                            out[row_out_r + x] = if one_bit {
                                if (gv_r as f64) < threshold { ink_g } else { bg_g }
                            } else if invert {
                                255 - gv_r
                            } else {
                                gv_r
                            };
                            continue;
                        }
                        if d_pix_r > 0.40 && (gv_r as f64) >= threshold {
                            out[row_out_r + x] = bg_g;
                            continue;
                        }
                    }

                    let c_base_x = (wx_r as f64 / cell_x).floor() as i64 - cx_min;
                    let mut hit_dot = false;
                    let mut hit_dark = 0.0f64;
                    'cell_search: for oy in -1i64..=1 {
                        let ry_c = c_base_y + oy;
                        if ry_c < 0 || ry_c >= cg_h as i64 {
                            continue;
                        }
                        let r_off_c = ry_c as usize * cg_w;
                        for ox in -1i64..=1 {
                            let rx_c = c_base_x + ox;
                            if rx_c < 0 || rx_c >= cg_w as i64 {
                                continue;
                            }
                            let ci_c = r_off_c + rx_c as usize;
                            if !dot_on[ci_c] {
                                continue;
                            }
                            let dx_c = wx_r as f64 - dot_xs[ci_c];
                            let dy_c = wy_r as f64 - dot_ys[ci_c];
                            if dx_c * dx_c + dy_c * dy_c <= dot_r2s[ci_c] {
                                hit_dot = true;
                                hit_dark = dot_dark_arr[ci_c] as f64;
                                break 'cell_search;
                            }
                        }
                    }

                    if !hit_dot && is_stipple_grad && d_pix_r > 0.40 && (gv_r as f64) < threshold {
                        let blend_t = smoothstep(0.40, 0.56, d_pix_r);
                        let hx_b = if cfg.seamless { mod_i(wx_r, w_mod) } else { wx_r };
                        let hy_b = if cfg.seamless { mod_i(wy_r, h_mod) } else { wy_r };
                        let t_b = 0.45 * bayer8(hx_b, hy_b)
                            + 0.55 * (hash2d(cfg.seed ^ 0x0571_bb1e, hx_b as i32, hy_b as i32) as f64
                                / 4_294_967_296.0);
                        if blend_t > t_b {
                            hit_dot = true;
                            hit_dark = (255.0 - gv_r as f64) / 255.0;
                        }
                    }

                    out[row_out_r + x] = if one_bit {
                        if hit_dot { ink_g } else { bg_g }
                    } else if !hit_dot {
                        bg_g
                    } else if invert {
                        js_round(clamp(hit_dark, 0.25, 1.0) * 255.0) as u8
                    } else {
                        js_round((1.0 - clamp(hit_dark, 0.25, 1.0)) * 255.0) as u8
                    };
                }
            }
            return;
        }

        // pixel1 mode (`03-raster.js:1232-1286`)
        let is_pale1 = cfg.nawa_tone_style == "paleWhite";
        let is_dense1 = cfg.nawa_tone_style == "denseWhite";
        let w_mod = (cfg.width as i64).max(1);
        let h_mod = (cfg.height as i64).max(1);
        for y in 0..th {
            let row_out1 = y * tw;
            let gy_row1 = y + pad;
            let wy1 = origin_y + gy_row1 as i64;
            for x in 0..tw {
                let gx1 = x + pad;
                let wx1 = origin_x + gx1 as i64;
                let mut gv1 = gray[gy_row1 * gw + gx1] as f64;
                if tooth_amt {
                    gv1 = clamp(gv1 + tooth(cfg, wx1, wy1), 0.0, 255.0);
                }
                if gv1 >= 252.0 {
                    out[row_out1 + x] = bg_g;
                    continue;
                }
                let d_loc1 = sample_density(cfg, &scene.grid, wx1 as f64, wy1 as f64, &scene.noise);
                if d_loc1 <= 0.002 {
                    out[row_out1 + x] = bg_g;
                    continue;
                }
                let hxs1 = if cfg.seamless { mod_i(wx1, w_mod) } else { wx1 };
                let hys1 = if cfg.seamless { mod_i(wy1, h_mod) } else { wy1 };
                let t_stip1 = 0.35 * bayer8(hxs1, hys1)
                    + 0.65
                        * (hash2d(cfg.seed ^ 0x0571_bb1e, hxs1 as i32, hys1 as i32) as f64
                            / 4_294_967_296.0);
                let keep1 = if is_pale1 {
                    clamp(((255.0 - gv1) / 255.0) * (0.20 + 0.50 * d_loc1), 0.0, 0.44)
                } else if is_dense1 {
                    clamp(((255.0 - gv1) / 255.0) * (0.36 + 0.68 * d_loc1), 0.0, 0.78)
                } else if d_loc1 < 0.46 {
                    clamp((d_loc1 / 0.46).powf(1.35), 0.0, 1.0)
                } else {
                    1.0
                };
                out[row_out1 + x] = if one_bit {
                    if gv1 < threshold && keep1 > t_stip1 {
                        ink_g
                    } else {
                        bg_g
                    }
                } else {
                    let eff_g1 = js_round(255.0 - (255.0 - gv1) * keep1) as u8;
                    if invert { 255 - eff_g1 } else { eff_g1 }
                };
            }
        }
        return;
    }

    // 3d. Sugomi finish (`03-raster.js:1288-1332`)
    if mode == "sugomi" {
        let grain = cfg.grain_strength * 0.01 * 145.0;
        let w_mod = (cfg.width as i64).max(1);
        let h_mod = (cfg.height as i64).max(1);
        let gray = &scratch.gray;
        for y in 0..th {
            let row_out2 = y * tw;
            let gy_row2 = y + pad;
            let gy3 = origin_y + gy_row2 as i64;
            for x in 0..tw {
                let gx3 = x + pad;
                let gv = gray[gy_row2 * gw + gx3];
                if gv >= 252 {
                    out[row_out2 + x] = bg_g;
                    continue;
                }
                let wx3 = if cfg.seamless {
                    mod_i(origin_x + gx3 as i64, w_mod) as i32
                } else {
                    (origin_x + gx3 as i64) as i32
                };
                let wy3 = if cfg.seamless {
                    mod_i(gy3, h_mod) as i32
                } else {
                    gy3 as i32
                };
                let n1 = hash2d(cfg.seed ^ 0x7a11_c0de, wx3, wy3) as f64 / 4_294_967_296.0;
                let n2 = hash2d(cfg.seed ^ 0x3c6e_f372, wx3 >> 1, wy3 >> 1) as f64 / 4_294_967_296.0;
                let tv = ((n1 * 0.62 + n2 * 0.38) - 0.5) * 2.0;
                let edge_weight = 0.45 + 0.55 * smoothstep(20.0, 220.0, gv as f64);
                let g_noisy = clamp(gv as f64 + tv * grain * edge_weight, 0.0, 255.0);
                out[row_out2 + x] = if one_bit {
                    if g_noisy < threshold { ink_g } else { bg_g }
                } else if invert {
                    (255.0 - g_noisy) as u8
                } else {
                    g_noisy as u8
                };
            }
        }
        return;
    }

    // 3e. Clean finish (`03-raster.js:1335-1363`)
    let gray = &scratch.gray;
    for y in 0..th {
        let row_out3 = y * tw;
        let gy_row3 = y + pad;
        let wy3 = origin_y + gy_row3 as i64;
        for x in 0..tw {
            let gx4 = x + pad;
            let wx4 = origin_x + gx4 as i64;
            let mut gv2 = gray[gy_row3 * gw + gx4] as f64;
            if tooth_amt {
                gv2 = clamp(gv2 + tooth(cfg, wx4, wy3), 0.0, 255.0);
            }
            out[row_out3 + x] = if one_bit {
                if gv2 < threshold { ink_g } else { bg_g }
            } else if invert {
                (255.0 - gv2) as u8
            } else {
                gv2 as u8
            };
        }
    }
}

/// Tiled single-threaded region renderer with cooperative cancellation (`cancel: Option<&AtomicBool>`).
///
/// Checks `cancel.load(Ordering::Relaxed)` before every tile and returns `None` immediately
/// when cancelled (protecting callers from runaway background CPU jobs).
pub fn render_region_tiled(
    scene: &PreparedScene,
    cfg: &KakeamiConfig,
    rect: Rect,
    tile_size: u32,
    cancel: Option<&AtomicBool>,
) -> Option<Vec<u8>> {
    let rw = rect.w.max(0.0).round() as usize;
    let rh = rect.h.max(0.0).round() as usize;
    if rw == 0 || rh == 0 {
        return Some(Vec::new());
    }
    let ts = tile_size.max(16) as usize;
    let rx0 = rect.x.round() as i64;
    let ry0 = rect.y.round() as i64;

    let mut full = vec![if cfg.invert_ink { 0 } else { 255 }; rw * rh];
    let mut scratch = RasterScratch::new();
    let mut tile_buf = vec![0u8; ts * ts];

    let mut ty = 0usize;
    while ty < rh {
        let th = ts.min(rh - ty);
        let mut tx = 0usize;
        while tx < rw {
            if let Some(flag) = cancel {
                if flag.load(Ordering::Relaxed) {
                    return None;
                }
            }
            let tw = ts.min(rw - tx);
            let tile_rect = Rect {
                x: (rx0 + tx as i64) as f64,
                y: (ry0 + ty as i64) as f64,
                w: tw as f64,
                h: th as f64,
            };
            render_tile_gray_into(scene, cfg, tile_rect, &mut scratch, &mut tile_buf[..tw * th]);
            for row in 0..th {
                let dst_off = (ty + row) * rw + tx;
                let src_off = row * tw;
                full[dst_off..dst_off + tw].copy_from_slice(&tile_buf[src_off..src_off + tw]);
            }
            tx += ts;
        }
        ty += ts;
    }

    Some(full)
}
