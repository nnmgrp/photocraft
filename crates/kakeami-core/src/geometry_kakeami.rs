//! Port of `buildKakeami4` (v3/v4) and `buildKakeamiV8` (v8.0/v8.0b/v8.0c)
//! from `src/02-geometry.js`.
//!
//! All geometry calculations run strictly in `f64` (zero `hypot`, `js_round`
//! instead of `f64::round`, wrapping `u32` hashes, zero global mutable state)
//! and narrow to `f32` only when writing into `Builder::scratch`.

use crate::config::KakeamiConfig;
use crate::density::sample_density;
use crate::geometry::{emit_stroke_ex, Builder, RibbonMeta};
use crate::grid::{Grid, Rect, ROW};
use crate::math::{clamp, hash2d, js_round, mod_i, smoothstep, Mulberry32, ValueNoise2D};
use std::f64::consts::{FRAC_1_SQRT_2, PI, SQRT_2};

const DIAG: f64 = FRAC_1_SQRT_2;
const SQRT2: f64 = SQRT_2;

#[inline]
fn gcd_int(mut a: i64, mut b: i64) -> i64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

// =========================================================================
// kakeami-4 (v3/v4): 4 global line families + flow-field warp
// =========================================================================

fn fit_axis_spacing(want: f64, w: f64, h: f64) -> f64 {
    let mut k = ((w / want).ceil() as i64).max(1);
    while k < 8192 {
        let sp = w / (k as f64);
        if sp < want * 0.5 {
            break;
        }
        let nh = h / sp;
        if (nh - js_round(nh)).abs() < 1e-9 {
            return sp;
        }
        k += 1;
    }
    want
}

fn fit_axis_pitch(want: f64, len: f64) -> f64 {
    let k = (js_round(len / want) as i64).max(1);
    len / (k as f64)
}

fn fit_diag_spacing(want: f64, w: i64, h: i64) -> (f64, i64) {
    let g = gcd_int(w, h) as f64;
    let mut m = (js_round(DIAG * g / want) as i64).max(1);
    while m < 8192 && (DIAG * g) / (m as f64) > want {
        m += 1;
    }
    ((DIAG * g) / (m as f64), m)
}

fn fit_diag_pitch(want: f64, w: i64, h: i64) -> (f64, i64) {
    let g = gcd_int(w, h) as f64;
    let mut m = (js_round(DIAG * g / want) as i64).max(1);
    while m < 8192 && (DIAG * g) / (m as f64) > want {
        m += 1;
    }
    ((DIAG * g) / (m as f64), m)
}

struct FlowWarp {
    sx: f64,
    sy: f64,
    amp: f64,
}

impl FlowWarp {
    fn new(cfg: &KakeamiConfig, grid: &Grid, sp: f64) -> Self {
        let freq = 0.011;
        let amp = sp * (0.28 + 0.65 * (cfg.wobble as f64 / 100.0));
        let mut sx = freq;
        let mut sy = freq;

        if cfg.seamless {
            let px = (grid.period_x >> 1).max(1) as f64;
            let py = (grid.period_y >> 1).max(1) as f64;
            let kx = js_round(cfg.width as f64 * freq / px).max(1.0);
            let ky = js_round(cfg.height as f64 * freq / py).max(1.0);
            sx = kx * px / (cfg.width as f64);
            sy = ky * py / (cfg.height as f64);
        }

        Self { sx, sy, amp }
    }

    #[inline]
    fn eval(&self, noise: &ValueNoise2D, x: f64, y: f64) -> (f64, f64) {
        let wx = (noise.sample(x * self.sx, y * self.sy) - 0.5) * 2.0 * self.amp;
        let wy = (noise.sample(x * self.sx + 17.3, y * self.sy + 31.7) - 0.5) * 2.0 * self.amp;
        (wx, wy)
    }
}

pub(crate) fn build_kakeami_4(
    b: &mut Builder,
    cfg: &KakeamiConfig,
    grid: &Grid,
    noise: &ValueNoise2D,
    rect: &Rect,
) {
    let forced: usize = if cfg.layer_cap != "auto" && !cfg.layer_cap.is_empty() {
        let parsed = cfg.layer_cap.parse::<i64>().unwrap_or(0);
        if parsed > 0 {
            (parsed as usize).min(4)
        } else {
            0
        }
    } else {
        0
    };

    let want_sp = cfg.line_spacing.max(3.0);
    let seg_want = (want_sp * 4.0).max(cfg.cluster_size * 0.72);

    let base_rad = cfg.base_angle * PI / 180.0;
    let cross_rad = cfg.cross_angle as f64 * PI / 180.0;
    let mut angles = [
        base_rad,
        base_rad + cross_rad,
        base_rad + 0.5 * cross_rad,
        base_rad + 1.5 * cross_rad,
    ];
    let mut sps = [want_sp; 4];
    let mut segs = [seg_want; 4];
    let mut row_per = [0i64; 4];
    let mut seg_per = [0i64; 4];

    if cfg.seamless {
        let w_f = cfg.width as f64;
        let h_f = cfg.height as f64;
        let w_i = cfg.width as i64;
        let h_i = cfg.height as i64;
        let sp_axis = fit_axis_spacing(want_sp, w_f, h_f);
        let (d_sp, d_sp_m) = fit_diag_spacing(want_sp, w_i, h_i);
        let (d_seg, d_seg_m) = fit_diag_pitch(seg_want, w_i, h_i);
        angles[0] = 0.0;
        angles[1] = PI / 2.0;
        angles[2] = PI / 4.0;
        angles[3] = 3.0 * PI / 4.0;
        sps[0] = sp_axis;
        sps[1] = sp_axis;
        sps[2] = d_sp;
        sps[3] = d_sp;
        segs[0] = fit_axis_pitch(seg_want, w_f);
        segs[1] = fit_axis_pitch(seg_want, h_f);
        segs[2] = d_seg;
        segs[3] = d_seg;
        row_per[0] = js_round(h_f / sp_axis) as i64;
        row_per[1] = js_round(w_f / sp_axis) as i64;
        row_per[2] = d_sp_m;
        row_per[3] = d_sp_m;
        seg_per[0] = js_round(w_f / segs[0]) as i64;
        seg_per[1] = js_round(h_f / segs[1]) as i64;
        seg_per[2] = d_seg_m;
        seg_per[3] = d_seg_m;
    }

    let bleed = want_sp * 4.0 + cfg.cluster_size * 0.5;
    let ex0 = rect.x - bleed;
    let ex1 = rect.x + rect.w + bleed;
    let ey0 = rect.y - bleed;
    let ey1 = rect.y + rect.h + bleed;

    let expo = cfg.taper_exponent;
    let mut env_int = 0.0f64;
    for qi in 0..32 {
        env_int += (PI * ((qi as f64 + 0.5) / 32.0)).sin().powf(expo);
    }
    env_int /= 32.0;

    let user_scale = cfg.stroke_width / 1.3;
    let sugomi = cfg.finish_mode == "sugomi";
    let pencil = cfg.finish_mode == "pencil";
    let wob_k = (if pencil { cfg.wobble.max(28) } else { cfg.wobble }) as f64 / 25.0;
    let scatter_k = cfg.scatter_jitter as f64 / 30.0;
    let broken_amt = (if pencil { cfg.broken_ink.max(18) } else { cfg.broken_ink }) as f64 * 0.01;
    let ang_k = if sugomi {
        0.30
    } else if pencil {
        0.26
    } else {
        0.16
    };

    let warp = FlowWarp::new(cfg, grid, want_sp);

    for p in 0..4usize {
        let ang = angles[p];
        let dx = ang.cos();
        let dy = ang.sin();
        let nx = -dy;
        let ny = dx;
        let sp = sps[p];
        let seg_pitch = segs[p];

        let mut u_min = f64::INFINITY;
        let mut u_max = f64::NEG_INFINITY;
        let mut v_min = f64::INFINITY;
        let mut v_max = f64::NEG_INFINITY;
        for c in 0..4 {
            let cxp = if c == 0 || c == 2 { ex0 } else { ex1 };
            let cyp = if c < 2 { ey0 } else { ey1 };
            let uu = cxp * dx + cyp * dy;
            let vv = cxp * nx + cyp * ny;
            if uu < u_min { u_min = uu; }
            if uu > u_max { u_max = uu; }
            if vv < v_min { v_min = vv; }
            if vv > v_max { v_max = vv; }
        }

        let row_min = (v_min / sp).floor() as i64 - 1;
        let row_max = (v_max / sp).floor() as i64 + 1;
        let seg_min = (u_min / seg_pitch).floor() as i64 - 1;
        let seg_max = (u_max / seg_pitch).floor() as i64 + 1;

        let p_seed_xor = cfg.seed ^ (p as u32).wrapping_mul(0x9e37_79b9);

        for row in row_min..=row_max {
            let row_w = if row_per[p] != 0 {
                mod_i(row, row_per[p]) as i32
            } else {
                row as i32
            };
            let row_stagger = if (row & 1) != 0 { 0.35 } else { 0.0 };

            for seg in seg_min..=seg_max {
                let seg_w = if seg_per[p] != 0 {
                    mod_i(seg, seg_per[p]) as i32
                } else {
                    seg as i32
                };
                let seg_seed = hash2d(p_seed_xor, row_w, seg_w);
                let mut rng = Mulberry32::new(seg_seed);

                // Fixed draw order (1..6): uMid, vMid, dAng, bow, sCurve, lenF
                let u_mid = (seg as f64 + 0.5 + row_stagger + (rng.next_f64() - 0.5) * 0.42) * seg_pitch;
                let v_mid = (row as f64 + 0.5 + (rng.next_f64() - 0.5) * 0.28 * scatter_k) * sp;
                let mx = u_mid * dx + v_mid * nx;
                let my = u_mid * dy + v_mid * ny;

                let d_ang = (rng.next_f64() - 0.5) * (cfg.angle_randomness as f64 * ang_k) * PI / 180.0;
                let bow = (rng.next_f64() - 0.5) * sp * 0.28 * wob_k;
                let s_curve = (rng.next_f64() - 0.5) * sp * 0.18 * wob_k;
                let mut len_f = seg_pitch
                    * if sugomi || pencil {
                        0.85 + rng.next_f64() * 0.62
                    } else {
                        1.12 + rng.next_f64() * 0.24
                    };

                let ca = d_ang.cos();
                let sa = d_ang.sin();
                let tdx = dx * ca - dy * sa;
                let tdy = dx * sa + dy * ca;
                let tnx = -tdy;
                let tny = tdx;

                let mut ax = mx - 0.5 * len_f * tdx;
                let mut ay = my - 0.5 * len_f * tdy;
                let mut bx = mx + 0.5 * len_f * tdx;
                let mut by = my + 0.5 * len_f * tdy;

                let d_a = sample_density(cfg, grid, ax, ay, noise);
                let d_m = sample_density(cfg, grid, mx, my, noise);
                let d_b = sample_density(cfg, grid, bx, by, noise);
                let mut d = (d_a + d_m + d_b) / 3.0;
                if d < 0.012 {
                    continue;
                }

                if sugomi || pencil {
                    d = clamp(d + (noise.sample(mx * 0.018, my * 0.018) - 0.5) * 0.12, 0.0, 1.0);
                }

                if d < 0.20 {
                    len_f *= 0.55 + 0.45 * smoothstep(0.02, 0.20, d);
                    ax = mx - 0.5 * len_f * tdx;
                    ay = my - 0.5 * len_f * tdy;
                    bx = mx + 0.5 * len_f * tdx;
                    by = my + 0.5 * len_f * tdy;
                }

                let p0 = smoothstep(0.010, 0.14, d);
                let p1 = smoothstep(0.03, 0.18, d);
                let p2 = smoothstep(0.08, 0.36, d);
                let p3 = smoothstep(0.26, 0.58, d);
                let mut probs = [p0, p1, p2, p3];
                let n_eff = if forced > 0 {
                    for (pi, slot) in probs.iter_mut().enumerate() {
                        *slot = if pi < forced { 1.0 } else { 0.0 };
                    }
                    forced as f64
                } else {
                    (p0 + p1 + p2 + p3).max(1.0)
                };

                // Draw 7: layer presence (short-circuits when probs[p] >= 1)
                if probs[p] < 1.0 && rng.next_f64() > probs[p] {
                    continue;
                }

                // Draw 8: light-edge dissolve
                let keep_prob = if p >= 2 {
                    smoothstep(0.03, 0.20, d)
                } else {
                    smoothstep(0.015, 0.16, d)
                };
                if rng.next_f64() > keep_prob {
                    continue;
                }

                // Draw 9: sugomi / pencil tear breakup
                if (sugomi || pencil) && broken_amt > 0.0 && d < 0.48 {
                    let tear_mul = if pencil { 0.35 } else { 0.6 };
                    if rng.next_f64() < broken_amt * (1.0 - d / 0.48) * tear_mul {
                        continue;
                    }
                }

                let d_clamped = clamp(d, 0.01, 0.992);
                let f_layer = 1.0 - (1.0 - d_clamped).powf(1.0 / n_eff);
                let w_target = sp * f_layer * user_scale * if pencil { 1.15 } else { 1.0 };

                let alpha_scale = (if pencil { 0.85 } else { 1.0 }) * (1.0 - smoothstep(0.55, 0.85, d) * 0.7);
                let alpha = cfg.nib_taper as f64 * 0.01 * alpha_scale;
                let mean_env = (1.0 - alpha) + alpha * env_int;

                // Draw 10: width jitter
                let width = clamp(
                    w_target * (0.88 + rng.next_f64() * 0.24) / mean_env,
                    if pencil { 0.60 } else { 0.45 },
                    sp * 0.98,
                );

                let mut has_break = false;
                let mut ga = 0.0;
                let mut gb = 1.0;
                if (pencil || sugomi) && broken_amt > 0.0 && d < 0.46 {
                    let bk_h = hash2d(seg_seed ^ 0x517c_c1b7, row as i32, seg as i32) as f64 / 4_294_967_296.0;
                    let bk_thr = (1.0 - d / 0.46) * if pencil { 0.45 } else { 0.30 };
                    if bk_h < bk_thr {
                        let bk_h2 = hash2d(seg_seed ^ 0x9e37_79b1, seg as i32, row as i32) as f64 / 4_294_967_296.0;
                        has_break = true;
                        ga = 0.24 + bk_h2 * 0.34;
                        gb = (ga + 0.12 + bk_h * 0.12).min(0.88);
                    }
                }

                let p1x = mx - 0.17 * len_f * tdx + (bow + s_curve) * tnx;
                let p1y = my - 0.17 * len_f * tdy + (bow + s_curve) * tny;
                let p2x = mx + 0.17 * len_f * tdx + (bow - s_curve) * tnx;
                let p2y = my + 0.17 * len_f * tdy + (bow - s_curve) * tny;

                let (w0x, w0y) = warp.eval(noise, ax, ay);
                let (w1x, w1y) = warp.eval(noise, p1x, p1y);
                let (w2x, w2y) = warp.eval(noise, p2x, p2y);
                let (w3x, w3y) = warp.eval(noise, bx, by);

                emit_stroke_ex(
                    b,
                    cfg,
                    ax + w0x, ay + w0y,
                    p1x + w1x, p1y + w1y,
                    p2x + w2x, p2y + w2y,
                    bx + w3x, by + w3y,
                    width,
                    1.0,
                    has_break,
                    ga,
                    gb,
                    seg_seed,
                    rect,
                    0,
                    alpha_scale,
                    None,
                );
            }
        }
    }
}

// =========================================================================
// kakeami-v8 (v8.0 / v8.0b / v8.0c): 4 geometries × 3 occlusions × 3 fades
// =========================================================================

struct KakeV8Params<'a> {
    cfg: &'a KakeamiConfig,
    grid: &'a Grid,
    noise: &'a ValueNoise2D,
    rect: &'a Rect,
    base_w: f64,
    ov_area_base: f64,
    is_cont_style: bool,
    is_raster_fade: bool,
    is_transparent: bool,
    is_dark_blend: bool,
    fade_thr: f64,
    one_bit: bool,
    min_floor: f64,
}

impl<'a> KakeV8Params<'a> {
    #[inline]
    fn kake_v8_width(&self, d: f64, cell_step: f64, two_layer: bool) -> f64 {
        let w_close = if two_layer {
            1.02 * cell_step
        } else {
            0.415 * cell_step + 0.035 * cell_step * smoothstep(0.90, 0.99, d)
        };
        let r_base = clamp(self.base_w / w_close.max(1e-4), 0.0, 0.99);
        let ink_card_base = 1.0 - (1.0 - r_base) * (1.0 - r_base);

        let mut ov_eff = 1.0;
        if !self.is_cont_style {
            if self.is_transparent {
                ov_eff = (self.ov_area_base * 0.85).max(1.0);
            } else if self.is_dark_blend && d > 0.55 {
                let target_ov = (self.ov_area_base * 0.85).max(1.0);
                ov_eff = 1.0 + (target_ov - 1.0) * smoothstep(0.55, 0.82, d);
            }
        }

        let d_start = if self.is_raster_fade {
            if self.is_transparent && !self.is_cont_style {
                self.fade_thr.max(0.42)
            } else {
                self.fade_thr
            }
        } else if self.is_transparent && !self.is_cont_style {
            0.42
        } else {
            0.25
        };

        let raw_w = if d <= d_start {
            if self.is_raster_fade {
                self.base_w
            } else {
                self.base_w * (0.72 + 0.28 * smoothstep(0.02, 0.22, d))
            }
        } else {
            let ink_base_eff = 1.0 - (1.0 - ink_card_base).max(0.0).powf(ov_eff);
            let u = clamp((d - d_start) / (0.97 - d_start).max(0.02), 0.0, 1.0);
            let target_ink = ink_base_eff + (0.998 - ink_base_eff) * u;
            let target_card = if ov_eff > 1.001 {
                1.0 - (1.0 - target_ink).max(0.0).powf(1.0 / ov_eff)
            } else {
                target_ink
            };
            w_close * (1.0 - (1.0 - target_card).max(0.0).sqrt())
        };

        let aa_comp = if self.one_bit {
            0.294 * smoothstep(0.45, 1.15, raw_w)
        } else {
            0.0
        };
        let eff = raw_w - aa_comp;
        if eff < self.min_floor { self.min_floor } else { eff }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_v8_line(
        &self,
        b: &mut Builder,
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        cell_step: f64,
        two_layer: bool,
        bow_amp: f64,
        line_seed: u32,
    ) {
        let dx = x1 - x0;
        let dy = y1 - y0;
        let len = (dx * dx + dy * dy).sqrt();
        if len < 0.25 {
            return;
        }
        let tx = dx / len;
        let ty = dy / len;
        let nx = -ty;
        let ny = tx;

        let max_pad = self
            .base_w
            .max(if two_layer { 0.55 * cell_step } else { 0.28 * cell_step })
            + bow_amp.abs()
            + 2.5;
        let l_min_x = x0.min(x1) - max_pad;
        let l_max_x = x0.max(x1) + max_pad;
        if l_max_x < self.rect.x - 2.0 || l_min_x > self.rect.x + self.rect.w + 2.0 {
            return;
        }
        let l_min_y = y0.min(y1) - max_pad;
        let l_max_y = y0.max(y1) + max_pad;
        if l_max_y < self.rect.y - 2.0 || l_min_y > self.rect.y + self.rect.h + 2.0 {
            return;
        }

        const N: usize = 5;
        let base = b.used;
        let first_point = base / 4;
        b.ensure(base + N * 4);

        let mut max_d = 0.0f64;
        let min_d_line = if self.is_raster_fade { 0.005 } else { 0.02 };
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;

        for i in 0..N {
            let t = i as f64 * 0.25;
            let bow = if bow_amp != 0.0 { (t * PI).sin() * bow_amp } else { 0.0 };
            let px = x0 + dx * t + nx * bow;
            let py = y0 + dy * t + ny * bow;
            let d_loc = sample_density(self.cfg, self.grid, px, py, self.noise);
            if d_loc > max_d {
                max_d = d_loc;
            }
            let hw = if d_loc > min_d_line {
                0.5 * self.kake_v8_width(d_loc, cell_step, two_layer)
            } else {
                0.0
            };
            let xl = px + nx * hw;
            let yl = py + ny * hw;
            let xr = px - nx * hw;
            let yr = py - ny * hw;
            let o = base + i * 4;
            b.scratch[o] = xl as f32;
            b.scratch[o + 1] = yl as f32;
            b.scratch[o + 2] = xr as f32;
            b.scratch[o + 3] = yr as f32;

            if xl < min_x { min_x = xl; }
            if xl > max_x { max_x = xl; }
            if xr < min_x { min_x = xr; }
            if xr > max_x { max_x = xr; }
            if yl < min_y { min_y = yl; }
            if yl > max_y { max_y = yl; }
            if yr < min_y { min_y = yr; }
            if yr > max_y { max_y = yr; }
        }

        if max_d <= min_d_line {
            return;
        }
        if max_x < self.rect.x - 2.0
            || min_x > self.rect.x + self.rect.w + 2.0
            || max_y < self.rect.y - 2.0
            || min_y > self.rect.y + self.rect.h + 2.0
        {
            return;
        }

        b.used = base + N * 4;
        b.meta.push(RibbonMeta {
            id: b.meta.len() as u32,
            point_count: N as u32,
            stroke_seed: line_seed,
            offset: first_point as u32,
            gap_start: 0.0,
            gap_end: 1.0,
            role: 0,
        });
        for q in 0..(N - 1) {
            let a = (first_point * 2 + q * 2) as u32;
            let c = a + 2;
            b.indices.extend_from_slice(&[a, a + 1, c, a + 1, c + 1, c]);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_v8_card_occ(
        &self,
        b: &mut Builder,
        cx: f64,
        cy: f64,
        ux: f64,
        uy: f64,
        vx: f64,
        vy: f64,
        half_s: f64,
        seed: u32,
    ) {
        let x0l = cx - ux * half_s - vx * half_s;
        let y0l = cy - uy * half_s - vy * half_s;
        let x0r = cx + ux * half_s - vx * half_s;
        let y0r = cy + uy * half_s - vy * half_s;
        let x1l = cx - ux * half_s + vx * half_s;
        let y1l = cy - uy * half_s + vy * half_s;
        let x1r = cx + ux * half_s + vx * half_s;
        let y1r = cy + uy * half_s + vy * half_s;

        let min_x = x0l.min(x0r).min(x1l).min(x1r);
        let max_x = x0l.max(x0r).max(x1l).max(x1r);
        let min_y = y0l.min(y0r).min(y1l).min(y1r);
        let max_y = y0l.max(y0r).max(y1l).max(y1r);
        if max_x < self.rect.x - 2.0
            || min_x > self.rect.x + self.rect.w + 2.0
            || max_y < self.rect.y - 2.0
            || min_y > self.rect.y + self.rect.h + 2.0
        {
            return;
        }

        let base = b.used;
        let first_point = base / 4;
        b.ensure(base + 8);
        b.scratch[base] = x0l as f32;
        b.scratch[base + 1] = y0l as f32;
        b.scratch[base + 2] = x0r as f32;
        b.scratch[base + 3] = y0r as f32;
        b.scratch[base + 4] = x1l as f32;
        b.scratch[base + 5] = y1l as f32;
        b.scratch[base + 6] = x1r as f32;
        b.scratch[base + 7] = y1r as f32;
        b.used = base + 8;

        b.meta.push(RibbonMeta {
            id: b.meta.len() as u32,
            point_count: 2,
            stroke_seed: seed,
            offset: first_point as u32,
            gap_start: 0.0,
            gap_end: 1.0,
            role: 1,
        });
        let a = (first_point * 2) as u32;
        b.indices.extend_from_slice(&[a, a + 1, a + 2, a + 1, a + 3, a + 2]);
    }
}

struct V8Patch {
    cx: f64,
    cy: f64,
    gx: i64,
    gy: i64,
    seed: u32,
    z_key: u32,
}

pub(crate) fn build_kakeami_v8(
    b: &mut Builder,
    cfg: &KakeamiConfig,
    grid: &Grid,
    noise: &ValueNoise2D,
    rect: &Rect,
) {
    let n_scale = clamp(cfg.nawa_scale, 0.40, 4.50);
    let patch_style = cfg.kake_patch_style.as_str();
    let grid_divs = clamp(js_round(cfg.kake_grid_divs as f64), 3.0, 8.0) as i32;
    let occlude_mode = cfg.kake_occlude_mode.as_str();
    let fade_mode = cfg.kake_fade_mode.as_str();
    let fade_thr = clamp(cfg.kake_fade_threshold, 0.05, 0.65);
    let is_raster_fade = fade_mode == "stipple" || fade_mode == "gaussian";
    let one_bit = cfg.one_bit_mode;
    let base_w = (0.55 * n_scale).max(cfg.stroke_width * n_scale);
    let wob_frac = clamp(cfg.wobble as f64 / 100.0, 0.0, 1.0);
    let ang_rand = clamp(cfg.angle_randomness as f64 / 100.0, 0.0, 1.0);
    let base_ang_rad = cfg.base_angle * PI / 180.0;
    let is_2l = patch_style == "closed-2l";
    let is_cont_style = patch_style == "continuous";
    let min_floor = if one_bit { 0.38 } else { 0.25 };

    let ov_area_base = if !is_cont_style {
        let s_ov = cfg.cluster_size.max(24.0) * n_scale;
        let ov_fac = (cfg.overlap_density as f64 * 0.01).max(0.5).sqrt();
        let st_ov = (18.0 * n_scale).max((s_ov * 0.62) / ov_fac);
        ((s_ov * s_ov) / (ROW * st_ov * st_ov)).max(1.0)
    } else {
        1.0
    };

    let ctx = KakeV8Params {
        cfg,
        grid,
        noise,
        rect,
        base_w,
        ov_area_base,
        is_cont_style,
        is_raster_fade,
        is_transparent: occlude_mode == "transparent",
        is_dark_blend: occlude_mode == "dark-blend",
        fade_thr,
        one_bit,
        min_floor,
    };

    // Style D: 'continuous' — global 4-layer continuous grid (0/90/+45/-45)
    if is_cont_style {
        let sp_base = cfg.line_spacing.max(3.0) * n_scale;
        let angles_c = if cfg.seamless {
            [0.0, PI * 0.5, PI * 0.25, PI * 0.75]
        } else {
            [
                base_ang_rad,
                base_ang_rad + PI * 0.5,
                base_ang_rad + PI * 0.25,
                base_ang_rad + PI * 0.75,
            ]
        };
        let layer_thr_c = [0.02, 0.20, 0.42, 0.64];
        let g_wh = if cfg.seamless {
            gcd_int(js_round(cfg.width as f64) as i64, js_round(cfg.height as f64) as i64) as f64
        } else {
            0.0
        };

        for p_idx in 0..4usize {
            let is_diag_c = p_idx >= 2;
            let mut sp_lay = if is_diag_c { sp_base * DIAG } else { sp_base };
            if cfg.seamless {
                if !is_diag_c {
                    let n_axis = js_round(g_wh / sp_base).max(2.0);
                    sp_lay = g_wh / n_axis;
                } else {
                    let diag_unit = g_wh * DIAG;
                    let n_diag = js_round(diag_unit / sp_lay).max(2.0);
                    sp_lay = diag_unit / n_diag;
                }
            }
            let mut seg_len_c = (sp_base * 4.5).max(32.0 * n_scale);
            if cfg.seamless {
                let seg_span = if is_diag_c { g_wh * SQRT2 } else { g_wh };
                let n_seg_per = js_round(seg_span / seg_len_c).max(2.0);
                seg_len_c = seg_span / n_seg_per;
            }

            let ang_c = angles_c[p_idx];
            let dir_x = ang_c.cos();
            let dir_y = ang_c.sin();
            let nrm_x = -dir_y;
            let nrm_y = dir_x;
            let bleed_c = seg_len_c + sp_base * 2.0;

            let c_xs = [rect.x - bleed_c, rect.x + rect.w + bleed_c];
            let c_ys = [rect.y - bleed_c, rect.y + rect.h + bleed_c];
            let mut u_min = f64::INFINITY;
            let mut u_max = f64::NEG_INFINITY;
            let mut v_min = f64::INFINITY;
            let mut v_max = f64::NEG_INFINITY;
            for &cx_c in &c_xs {
                for &cy_c in &c_ys {
                    let pu = cx_c * dir_x + cy_c * dir_y;
                    let pv = cx_c * nrm_x + cy_c * nrm_y;
                    if pu < u_min { u_min = pu; }
                    if pu > u_max { u_max = pu; }
                    if pv < v_min { v_min = pv; }
                    if pv > v_max { v_max = pv; }
                }
            }
            let row_min = (v_min / sp_lay).floor() as i64 - 1;
            let row_max = (v_max / sp_lay).ceil() as i64 + 1;
            let seg_min = (u_min / seg_len_c).floor() as i64 - 1;
            let seg_max = (u_max / seg_len_c).ceil() as i64 + 1;
            let thr_c = layer_thr_c[p_idx];
            let layer_seed_xor = cfg.seed ^ ((p_idx as u32 + 1).wrapping_mul(0x9e37_79b1));

            for row_c in row_min..=row_max {
                let v_pos = row_c as f64 * sp_lay;
                for seg_c in seg_min..=seg_max {
                    let u0_c = seg_c as f64 * seg_len_c;
                    let u1_c = u0_c + seg_len_c;
                    let u_mid_c = 0.5 * (u0_c + u1_c);
                    let mx_c = u_mid_c * dir_x + v_pos * nrm_x;
                    let my_c = u_mid_c * dir_y + v_pos * nrm_y;
                    let d_mid_c = sample_density(cfg, grid, mx_c, my_c, noise);
                    let mut sx0 = u0_c * dir_x + v_pos * nrm_x;
                    let mut sy0 = u0_c * dir_y + v_pos * nrm_y;
                    let mut sx1 = u1_c * dir_x + v_pos * nrm_x;
                    let mut sy1 = u1_c * dir_y + v_pos * nrm_y;
                    let d0_c = sample_density(cfg, grid, sx0, sy0, noise);
                    let d1_c = sample_density(cfg, grid, sx1, sy1, noise);
                    let d_max_c = d0_c.max(d_mid_c).max(d1_c);
                    if d_max_c <= 0.005 {
                        continue;
                    }

                    let l_seed_c = hash2d(layer_seed_xor, row_c as i32, seg_c as i32);
                    if fade_mode == "layers" {
                        if d_max_c <= thr_c {
                            continue;
                        }
                        let d_avg_c = (d0_c + d_mid_c + d1_c) / 3.0;
                        if d_avg_c < thr_c + 0.08 {
                            let p_keep_c = smoothstep(thr_c, thr_c + 0.08, d_avg_c);
                            if ((l_seed_c & 0xffff) as f64 / 65535.0) >= p_keep_c {
                                continue;
                            }
                        }
                        if p_idx == 0 && d_avg_c < 0.15 {
                            let sc_len = 0.45 + 0.55 * smoothstep(0.02, 0.15, d_avg_c);
                            let h_span = 0.5 * seg_len_c * sc_len;
                            sx0 = (u_mid_c - h_span) * dir_x + v_pos * nrm_x;
                            sy0 = (u_mid_c - h_span) * dir_y + v_pos * nrm_y;
                            sx1 = (u_mid_c + h_span) * dir_x + v_pos * nrm_x;
                            sy1 = (u_mid_c + h_span) * dir_y + v_pos * nrm_y;
                        }
                    }
                    let bow_c = if wob_frac > 0.0 {
                        ((((l_seed_c >> 16) & 0xffff) as f64 / 65535.0) - 0.5) * 0.18 * sp_base * wob_frac
                    } else {
                        0.0
                    };
                    ctx.emit_v8_line(b, sx0, sy0, sx1, sy1, sp_base, false, bow_c, l_seed_c);
                }
            }
        }
        return;
    }

    // Styles A/B/C: 'closed-4l' | 'closed-2l' | 'open' on DPI-scaled hex lattice
    let s_base = cfg.cluster_size.max(24.0) * n_scale;
    let overlap_factor = (cfg.overlap_density as f64 * 0.01).max(0.5).sqrt();
    let base_step = (18.0 * n_scale).max((s_base * 0.62) / overlap_factor);

    let mut period_x = 0i64;
    let mut period_y = 0i64;
    let mut step_x = base_step;
    let mut step_y = base_step;
    if cfg.seamless {
        period_x = (js_round(cfg.width as f64 / base_step) as i64).max(2);
        period_y = (js_round(cfg.height as f64 / (base_step * ROW)) as i64).max(2);
        if (period_x & 1) != 0 { period_x += 1; }
        if (period_y & 1) != 0 { period_y += 1; }
        period_x = period_x.min(512);
        period_y = period_y.min(512);
        step_x = cfg.width as f64 / period_x as f64;
        step_y = cfg.height as f64 / (period_y as f64 * ROW);
    }
    let row_step = ROW * step_y;

    let bleed = s_base * 0.95 + 8.0 * n_scale;
    let x0_b = rect.x - bleed;
    let x1_b = rect.x + rect.w + bleed;
    let y0_b = rect.y - bleed;
    let y1_b = rect.y + rect.h + bleed;

    let gx_min = (x0_b / step_x).floor() as i64 - 1;
    let gx_max = (x1_b / step_x).floor() as i64 + 1;
    let gy_min = (y0_b / row_step).floor() as i64 - 1;
    let gy_max = (y1_b / row_step).floor() as i64 + 1;

    let mut patches: Vec<V8Patch> = Vec::new();
    let jit_amt = clamp(cfg.scatter_jitter as f64 / 100.0, 0.0, 1.0);
    let jit_span = 0.25 + 0.50 * jit_amt;
    let jit_off = 0.5 * (1.0 - jit_span);

    for gy in gy_min..=gy_max {
        let row_off = if (gy & 1) != 0 { 0.5 } else { 0.0 };
        let wy = if cfg.seamless { mod_i(gy, period_y) as i32 } else { gy as i32 };
        for gx in gx_min..=gx_max {
            let wx = if cfg.seamless { mod_i(gx, period_x) as i32 } else { gx as i32 };
            let p_seed = hash2d(cfg.seed ^ 0x5638_4b4b, wx, wy);
            let z_key = hash2d(cfg.seed ^ 0x4b41_4b45, wx, wy);
            let h_pos = hash2d(p_seed ^ 0x27d4_eb2f, 1, 7);
            let jx = (h_pos & 0xffff) as f64 / 65535.0;
            let jy = ((h_pos >> 16) & 0xffff) as f64 / 65535.0;

            let cx = (gx as f64 + row_off + jit_off + jx * jit_span) * step_x;
            let cy = (gy as f64 + jit_off + jy * jit_span) * row_step;
            if cx < x0_b || cx > x1_b || cy < y0_b || cy > y1_b {
                continue;
            }
            patches.push(V8Patch { cx, cy, gx, gy, seed: p_seed, z_key });
        }
    }

    // Strictly unique composite key (z_key, gy, gx) -> identical order across tiles & sort impls
    patches.sort_unstable_by_key(|p| (p.z_key, p.gy, p.gx));

    let is_open = patch_style == "open";
    let n_divs = grid_divs;

    for pt in &patches {
        let mut rng = Mulberry32::new(pt.seed);
        let r_ang = rng.next_f64();
        let r_scale = rng.next_f64();
        let r_occ = rng.next_f64();
        let r_light_keep = rng.next_f64();

        let s_p = s_base * (0.88 + r_scale * 0.24);
        let half_s = 0.5 * s_p;
        let cell_step = s_p / (n_divs as f64);
        let ang = base_ang_rad + (r_ang - 0.5) * PI * ang_rand;
        let ux = ang.cos();
        let uy = ang.sin();
        let vx = -uy;
        let vy = ux;

        let ext = half_s * (ux.abs() + vx.abs())
            + if is_open { 0.30 * cell_step } else { 0.0 }
            + 0.55 * cell_step
            + 2.0;
        if pt.cx + ext < rect.x - 2.0
            || pt.cx - ext > rect.x + rect.w + 2.0
            || pt.cy + ext < rect.y - 2.0
            || pt.cy - ext > rect.y + rect.h + 2.0
        {
            continue;
        }

        let d_center = sample_density(cfg, grid, pt.cx, pt.cy, noise);
        let d_c0 = sample_density(cfg, grid, pt.cx - ux * half_s - vx * half_s, pt.cy - uy * half_s - vy * half_s, noise);
        let d_c1 = sample_density(cfg, grid, pt.cx + ux * half_s - vx * half_s, pt.cy + uy * half_s - vy * half_s, noise);
        let d_c2 = sample_density(cfg, grid, pt.cx - ux * half_s + vx * half_s, pt.cy - uy * half_s + vy * half_s, noise);
        let d_c3 = sample_density(cfg, grid, pt.cx + ux * half_s + vx * half_s, pt.cy + uy * half_s + vy * half_s, noise);
        let d_max_patch = d_center.max(d_c0).max(d_c1).max(d_c2).max(d_c3);
        if d_max_patch <= 0.005 {
            continue;
        }

        let mut act0 = true;
        let mut act1 = true;
        let mut act2 = !is_2l;
        let mut act3 = !is_2l;
        let mut len_scale_light = 1.0;
        if fade_mode == "layers" {
            act0 = d_max_patch > 0.02;
            act1 = d_center > 0.20;
            act2 = !is_2l && d_center > 0.42;
            act3 = !is_2l && d_center > 0.64;
            if !act0 {
                continue;
            }
            if d_center < 0.15 {
                let p_patch = smoothstep(0.015, 0.15, d_max_patch);
                if r_light_keep >= p_patch {
                    continue;
                }
                len_scale_light = 0.50 + 0.50 * p_patch;
            }
        }

        let emit_occ = if occlude_mode == "card" {
            true
        } else if occlude_mode == "dark-blend" {
            if d_center <= 0.55 {
                true
            } else if d_center < 0.82 {
                r_occ < ((0.82 - d_center) / 0.27)
            } else {
                false
            }
        } else {
            false
        };
        if emit_occ {
            ctx.emit_v8_card_occ(b, pt.cx, pt.cy, ux, uy, vx, vy, half_s, pt.seed ^ 0x1111_1111);
        }

        let mut emit_local_seg = |lx0: f64, ly0: f64, lx1: f64, ly1: f64, layer_tag: u32, idx_tag: i32| {
            let l_seed = hash2d(
                pt.seed ^ (layer_tag + 1).wrapping_mul(0x9e37_79b1),
                idx_tag,
                17,
            );
            let mut ax0 = lx0;
            let mut ay0 = ly0;
            let mut ax1 = lx1;
            let mut ay1 = ly1;
            let mut bow_amp = 0.0;
            if is_open {
                let ldx = ax1 - ax0;
                let ldy = ay1 - ay0;
                let llen = (ldx * ldx + ldy * ldy).sqrt();
                if llen < 1e-4 {
                    return;
                }
                let ltx = ldx / llen;
                let lty = ldy / llen;
                let h_a = hash2d(l_seed ^ 0x6f3b_9c41, idx_tag, 3);
                let ov_a = (0.08 + ((h_a & 0xffff) as f64 / 65535.0) * 0.16) * cell_step;
                let ov_b = (0.08 + (((h_a >> 16) & 0xffff) as f64 / 65535.0) * 0.16) * cell_step;
                if len_scale_light < 0.999 {
                    let mid_x = 0.5 * (ax0 + ax1);
                    let mid_y = 0.5 * (ay0 + ay1);
                    let h_l = 0.5 * llen * len_scale_light;
                    ax0 = mid_x - ltx * h_l;
                    ay0 = mid_y - lty * h_l;
                    ax1 = mid_x + ltx * h_l;
                    ay1 = mid_y + lty * h_l;
                }
                ax0 -= ltx * ov_a;
                ay0 -= lty * ov_a;
                ax1 += ltx * ov_b;
                ay1 += lty * ov_b;

                let r_j = (((l_seed & 0xffff) as f64 / 65535.0) - 0.5) * 0.06 * (0.35 + 0.65 * wob_frac);
                if r_j.abs() > 1e-4 {
                    let m_x2 = 0.5 * (ax0 + ax1);
                    let m_y2 = 0.5 * (ay0 + ay1);
                    let cj = r_j.cos();
                    let sj = r_j.sin();
                    let rx0 = ax0 - m_x2;
                    let ry0 = ay0 - m_y2;
                    let rx1 = ax1 - m_x2;
                    let ry1 = ay1 - m_y2;
                    ax0 = m_x2 + rx0 * cj - ry0 * sj;
                    ay0 = m_y2 + rx0 * sj + ry0 * cj;
                    ax1 = m_x2 + rx1 * cj - ry1 * sj;
                    ay1 = m_y2 + rx1 * sj + ry1 * cj;
                }
                bow_amp = ((((l_seed >> 16) & 0xffff) as f64 / 65535.0) - 0.5) * 0.22 * cell_step * wob_frac;
            } else if wob_frac > 0.0 {
                bow_amp = ((((l_seed >> 16) & 0xffff) as f64 / 65535.0) - 0.5) * 0.08 * cell_step * wob_frac;
            }
            let wx0 = pt.cx + ux * ax0 + vx * ay0;
            let wy0 = pt.cy + uy * ax0 + vy * ay0;
            let wx1 = pt.cx + ux * ax1 + vx * ay1;
            let wy1 = pt.cy + uy * ax1 + vy * ay1;
            ctx.emit_v8_line(b, wx0, wy0, wx1, wy1, cell_step, is_2l, bow_amp, l_seed);
        };

        if act0 {
            for i0 in 0..=n_divs {
                let ly = -half_s + (i0 as f64) * cell_step;
                emit_local_seg(-half_s, ly, half_s, ly, 0, i0);
            }
        }
        if act1 {
            for j0 in 0..=n_divs {
                let lx = -half_s + (j0 as f64) * cell_step;
                emit_local_seg(lx, -half_s, lx, half_s, 1, j0);
            }
        }
        if act2 {
            for k_p in -(n_divs - 1)..=(n_divs - 1) {
                if k_p >= 0 {
                    emit_local_seg(
                        -half_s + (k_p as f64) * cell_step,
                        -half_s,
                        half_s,
                        half_s - (k_p as f64) * cell_step,
                        2,
                        k_p + n_divs,
                    );
                } else {
                    emit_local_seg(
                        -half_s,
                        -half_s - (k_p as f64) * cell_step,
                        half_s + (k_p as f64) * cell_step,
                        half_s,
                        2,
                        k_p + n_divs,
                    );
                }
            }
        }
        if act3 {
            for k_m in -(n_divs - 1)..=(n_divs - 1) {
                if k_m >= 0 {
                    emit_local_seg(
                        -half_s + (k_m as f64) * cell_step,
                        half_s,
                        half_s,
                        -half_s + (k_m as f64) * cell_step,
                        3,
                        k_m + n_divs,
                    );
                } else {
                    emit_local_seg(
                        -half_s,
                        half_s + (k_m as f64) * cell_step,
                        half_s + (k_m as f64) * cell_step,
                        -half_s,
                        3,
                        k_m + n_divs,
                    );
                }
            }
        }
    }
}
