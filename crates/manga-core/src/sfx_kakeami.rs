//! Style 4 — 凄み黒カケペン (`Kakeami v4`, Continuous Unsegmented Phase-Locked Lanes) 1:1 port of `kakeami_v4.py`.
//!
//! Includes all 3 mandatory Phase 2B-2 engineering fixes:
//! 1. **Zero per-lane angular jitter (`ang_jit == 0`):** every lane in layer `li` uses the exact
//!    parallel angle `LAYER_ANGLES[li] + angle_offset_deg` so long banners never suffer lever-arm
//!    lane convergence; hand-drawn plasticity comes purely from micro-offset `v0` (`±0.05 * pitch`)
//!    and synchronized low-freq 2D `v_flow` (`±0.07 * pitch`).
//! 2. **Rotatable `KakeEdge` gradient axis:** bound to `kakeami_gradient_angle_deg` (default `90°`
//!    = top→bottom matching `y_norm`) projected across glyph bounds, shared 1:1 with GPU `beta_mask`.
//! 3. **Short-span decimation & DPI-safe scale:** endpoint deduplication + zero-tangent guard in
//!    [`build_tapered_ribbon_strip`] (`0 NaN`/`Inf` on 3–4 sample spans) and uncapped
//!    `w_scale = max(0.75, stroke_w / 28.0)`.

use std::f64::consts::PI;

use photocraft_doc::sfx::{SfxKakeamiV4Mode, SfxLineEffect, SfxV3EdgeMode};
use photocraft_geom::Rect;

use crate::sfx_geom::PreparedSfx;
use crate::sfx_v3_common::{
    EdgeKasureParams, PolyRing, RibbonStrip, VectorGlyphSdf, apply_edge_kasure_breakup_doc, build_tapered_ribbon_strip,
    classify_glyph_contours, exact_sdf, h1, rasterize_ribbons_max, value_noise_2d,
};

const DEG: f64 = PI / 180.0;
const LAYER_ANGLES_DEG: [f64; 4] = [-34.0, 52.0, -76.0, 12.0];

/// Extracted vector geometry + exact gradient frame for `Kakeami v4`, shared by CPU & GPU.
#[derive(Clone, Debug, Default)]
pub struct KakeamiV4VectorGeometry {
    /// Continuous unsegmented crosshatch quad-strip ribbons (`BlendOperation::Max`).
    pub hatch_ribbons: Vec<RibbonStrip>,
    /// Estimated stroke width in document pixels (`[12.0, 400.0]`).
    pub stroke_w: f32,
    /// Uncapped DPI-safe width scale `max(0.75, stroke_w / 28.0)`.
    pub w_scale: f32,
    /// Unit directional gradient axis `(cos(a), sin(a))` for `KakeEdge`.
    pub grad_axis: [f32; 2],
    /// `[g_min, g_span]` projected along `grad_axis`.
    pub grad_range: [f32; 2],
    /// `[p_min, p_span]` projected along `perp_axis = (-sin(a), cos(a))`.
    pub perp_range: [f32; 2],
}

#[derive(Clone, Copy)]
struct DensityParams {
    mode: SfxKakeamiV4Mode,
    stroke_w: f32,
    w_scale: f32,
    seed: u32,
    grad_axis: [f32; 2],
    grad_range: [f32; 2],
    perp_range: [f32; 2],
}

fn split_hole_contours(groups: &[Vec<PolyRing>]) -> Vec<PolyRing> {
    let mut holes = Vec::new();
    for cs in groups {
        if cs.is_empty() {
            continue;
        }
        let classes = classify_glyph_contours(cs);
        for (c, cls) in cs.iter().zip(classes) {
            if cls.is_hole && c.len() >= 3 {
                holes.push(c.clone());
            }
        }
    }
    holes
}

fn compute_gradient_frame(groups: &[Vec<PolyRing>], grad_angle_deg: f32) -> ([f32; 2], [f32; 2], [f32; 2]) {
    let rad = f64::from(grad_angle_deg) * DEG;
    let (sg, cg) = rad.sin_cos();
    // At default 90°: grad = (0, 1) -> y_norm (top->bottom), perp = (-1, 0) -> (1 - x_norm) (right->left), matching Python 1:1!
    let gx = cg;
    let gy = sg;
    let px = -sg;
    let py = cg;

    let mut g_min = f64::INFINITY;
    let mut g_max = f64::NEG_INFINITY;
    let mut p_min = f64::INFINITY;
    let mut p_max = f64::NEG_INFINITY;
    for cs in groups {
        for c in cs {
            for &(x, y) in c {
                if x.is_finite() && y.is_finite() {
                    let g = x * gx + y * gy;
                    let p = x * px + y * py;
                    g_min = g_min.min(g);
                    g_max = g_max.max(g);
                    p_min = p_min.min(p);
                    p_max = p_max.max(p);
                }
            }
        }
    }
    if !g_min.is_finite() {
        return ([gx as f32, gy as f32], [0.0, 32.0], [0.0, 32.0]);
    }
    (
        [gx as f32, gy as f32],
        [g_min as f32, (g_max - g_min).max(32.0) as f32],
        [p_min as f32, (p_max - p_min).max(32.0) as f32],
    )
}

/// Evaluates `Kakeami v4` density at document coordinate `(x, y)` (1:1 identical to `sfx.wgsl:fs_kakeami_beta_mask`).
#[inline]
fn eval_density_doc(x: f32, y: f32, glyph_sdf: f32, dp: DensityParams) -> f32 {
    let inv_sc = 1.0 / dp.w_scale.max(0.5);
    let f1 = 0.017 * inv_sc;
    let f2 = 0.043 * inv_sc;
    let c1 = value_noise_2d(
        (x * 0.88 - y * 0.47) * f1,
        (x * 0.47 + y * 0.88) * f1,
        dp.seed.wrapping_mul(131).wrapping_add(17),
    );
    let c2 = value_noise_2d(
        (x * 0.39 + y * 0.92) * f2,
        (-x * 0.92 + y * 0.39) * f2,
        dp.seed.wrapping_mul(197).wrapping_add(53),
    );
    let cloud = c1 * 0.68 + c2 * 0.32;

    if dp.mode == SfxKakeamiV4Mode::Kake2 {
        return (0.40 + (cloud - 0.5) * 0.30).clamp(0.15, 0.70);
    }
    let depth_n = (-glyph_sdf / (dp.stroke_w * 0.48).max(6.0)).clamp(0.0, 1.0);
    if dp.mode == SfxKakeamiV4Mode::Kake4 {
        return (0.44 + depth_n * 0.18 + (cloud - 0.50) * 0.76).clamp(0.0, 1.0);
    }
    let gx = dp.grad_axis[0];
    let gy = dp.grad_axis[1];
    let px = -gy;
    let py = gx;
    let t_grad = ((x * gx + y * gy - dp.grad_range[0]) / dp.grad_range[1].max(32.0)).clamp(0.0, 1.0);
    let t_perp = ((x * px + y * py - dp.perp_range[0]) / dp.perp_range[1].max(32.0)).clamp(0.0, 1.0);
    let grad = 0.05 + t_grad * 0.84 + t_perp * 0.14;
    (grad + depth_n * 0.10 + (cloud - 0.50) * 0.28).clamp(0.0, 1.0)
}

#[inline]
fn edge_coherence_doc(pt: (f64, f64), inv_sc: f64, seed: u32) -> f64 {
    let f = (0.045 * inv_sc) as f32;
    let v = f64::from(value_noise_2d(
        (pt.0 as f32) * f,
        (pt.1 as f32) * f,
        seed.wrapping_mul(419).wrapping_add(71),
    ));
    0.22 + 0.78 * ((v - 0.24) / 0.58).clamp(0.0, 1.0)
}

fn compute_boundary_ext(
    pt: (f64, f64),
    t_out: (f64, f64),
    is_base_layer: bool,
    vsdf: &VectorGlyphSdf,
    hole_vsdf: &VectorGlyphSdf,
    w_scale: f64,
    inv_sc: f64,
    ext_scale: f64,
    seed: u32,
    r_seed: u32,
) -> f64 {
    if !hole_vsdf.is_empty() {
        let hs = hole_vsdf.signed_distance(pt);
        if hs < 5.5 * w_scale {
            if is_base_layer {
                let ext = (-1.1 + 2.3 * h1(r_seed)) * w_scale;
                return ext.min((hs * 0.22).max(-0.4));
            }
            return (-1.5 + 1.4 * h1(r_seed)) * w_scale;
        }
    }
    if !is_base_layer {
        return (-1.6 + 2.0 * h1(r_seed)) * w_scale;
    }
    let clr = vsdf.probe_air_clear(pt, t_out, 24.0 * w_scale);
    let coh = edge_coherence_doc(pt, inv_sc, seed);
    let accent = if h1(r_seed.wrapping_add(13)) > 0.87 { 1.4 } else { 0.0 };
    let raw_ext = (-1.0 + (3.6 * h1(r_seed).powf(1.05) + accent) * coh) * w_scale * ext_scale;
    if clr < 14.0 * w_scale {
        raw_ext.min((clr * 0.25 - 1.2 * w_scale).max(-0.5 * w_scale))
    } else {
        raw_ext
    }
}

/// Decimates and builds a single NaN-free [`RibbonStrip`] for an active lane span.
fn build_lane_span_ribbon(
    sub_p_raw: &[(f64, f64)],
    sub_dv_raw: &[f64],
    u_dir: (f64, f64),
    ext_a_in: f64,
    ext_b_in: f64,
    step_s: f64,
    decim_step: usize,
    w_scale: f64,
    base_hw: f64,
    lane_hw_mul: f64,
    li: usize,
) -> Option<RibbonStrip> {
    let mut ext_a = ext_a_in;
    let mut ext_b = ext_b_in;
    let trim_a = if ext_a < 0.0 { (-ext_a / step_s).round() as usize } else { 0 };
    let trim_b = if ext_b < 0.0 { (-ext_b / step_s).round() as usize } else { 0 };
    if trim_a + trim_b + 2 >= sub_p_raw.len() {
        return None;
    }
    let end_idx = sub_p_raw.len() - trim_b;
    let sp_slice = &sub_p_raw[trim_a..end_idx];
    let dv_slice = &sub_dv_raw[trim_a..end_idx];
    if trim_a > 0 {
        ext_a = 0.0;
    }
    if trim_b > 0 {
        ext_b = 0.0;
    }

    // Decimate interior samples gently; deduplication inside `build_tapered_ribbon_strip` protects short spans!
    let mut sub_p: Vec<(f64, f64)> = Vec::with_capacity(sp_slice.len() / decim_step.max(1) + 4);
    let mut sub_dv: Vec<f64> = Vec::with_capacity(sub_p.capacity());
    if ext_a > 0.4 {
        sub_p.push((sp_slice[0].0 - u_dir.0 * ext_a, sp_slice[0].1 - u_dir.1 * ext_a));
        sub_dv.push(dv_slice[0]);
    }
    if sp_slice.len() > 4 {
        let mut idx = 0usize;
        while idx < sp_slice.len() - 1 {
            sub_p.push(sp_slice[idx]);
            sub_dv.push(dv_slice[idx]);
            idx += decim_step;
        }
        sub_p.push(sp_slice[sp_slice.len() - 1]);
        sub_dv.push(dv_slice[dv_slice.len() - 1]);
    } else {
        sub_p.extend_from_slice(sp_slice);
        sub_dv.extend_from_slice(dv_slice);
    }
    if ext_b > 0.4 {
        let last = sp_slice[sp_slice.len() - 1];
        sub_p.push((last.0 + u_dir.0 * ext_b, last.1 + u_dir.1 * ext_b));
        sub_dv.push(dv_slice[dv_slice.len() - 1]);
    }

    let m = sub_p.len();
    if m < 2 {
        return None;
    }
    let mut arc_s = Vec::with_capacity(m);
    arc_s.push(0.0_f64);
    for w in sub_p.windows(2) {
        let prev = *arc_s.last().unwrap_or(&0.0);
        arc_s.push(prev + (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1));
    }
    let span_len = arc_s.last().copied().unwrap_or(0.0);
    if span_len < 2.6 * w_scale {
        return None;
    }

    let t_zone = (span_len * 0.22).min(7.5 * w_scale).max(2.0 * w_scale);
    let li_mul = if li >= 2 { 0.88 } else { 1.0 };
    let hws: Vec<f64> = (0..m)
        .map(|k| {
            let t_a = (arc_s[k] / t_zone).clamp(0.0, 1.0);
            let t_b = ((span_len - arc_s[k]) / t_zone).clamp(0.0, 1.0);
            let taper = t_a.min(t_b).powf(0.55);
            let hw_local = base_hw * lane_hw_mul * (0.94 + 0.24 * (sub_dv[k] - 0.45).max(0.0)) * li_mul;
            (hw_local * (0.14 + 0.86 * taper)).max(0.24 * w_scale)
        })
        .collect();
    build_tapered_ribbon_strip(&sub_p, &hws)
}

/// Extracts continuous unsegmented phase-locked crosshatch lanes (`ang_jit == 0`).
pub fn build_kakeami_v4_from_groups(groups: &[Vec<PolyRing>], le: &SfxLineEffect) -> KakeamiV4VectorGeometry {
    let vsdf = VectorGlyphSdf::from_contour_groups(groups);
    let Some(gb) = vsdf.global_aabb() else {
        return KakeamiV4VectorGeometry::default();
    };
    let glyph_em = (gb[3] - gb[1]).max(32.0);
    let stroke_w = vsdf.estimate_stroke_w(glyph_em).clamp(12.0, 400.0);
    // DPI-safe uncapped width scale (never double-scaled by scale_sfx_pixel_effects)
    let w_scale = (stroke_w / 28.0).max(0.75);
    let holes = split_hole_contours(groups);
    let hole_vsdf = VectorGlyphSdf::from_flat_contours(&holes);
    let (grad_axis, grad_range, perp_range) = compute_gradient_frame(groups, le.kakeami_gradient_angle_deg);

    let mode = le.kakeami_mode;
    let seed = le.kakeami_seed;
    let dp = DensityParams {
        mode,
        stroke_w: stroke_w as f32,
        w_scale: w_scale as f32,
        seed,
        grad_axis,
        grad_range,
        perp_range,
    };

    let pad_box = 16.0 * w_scale;
    let xmin = gb[0] - pad_box;
    let xmax = gb[2] + pad_box;
    let ymin = gb[1] - pad_box;
    let ymax = gb[3] + pad_box;
    let cx_g = 0.5 * (xmin + xmax);
    let cy_g = 0.5 * (ymin + ymax);
    let half_diag = 0.5 * (xmax - xmin).hypot(ymax - ymin) + pad_box;

    let (layer_defs, base_pitch, base_hw): (&[(usize, f64)], f64, f64) = match mode {
        SfxKakeamiV4Mode::Kake2 => (&[(0, 0.0), (1, 0.0)], 7.3 * w_scale, 1.06 * w_scale),
        SfxKakeamiV4Mode::Kake4 => (&[(0, 0.0), (1, 0.0), (2, 0.36), (3, 0.58)], 6.5 * w_scale, 1.02 * w_scale),
        SfxKakeamiV4Mode::KakeEdge => (&[(0, 0.04), (1, 0.24), (2, 0.46), (3, 0.66)], 6.5 * w_scale, 1.02 * w_scale),
    };
    let lane_pitch = base_pitch * if stroke_w < 23.0 { 0.88 } else { 1.0 };
    let ext_scale = f64::from((le.kakeami_feather_pct / 100.0).clamp(0.0, 2.0));
    let inv_sc = 1.0 / w_scale.max(0.5);
    let step_s = (1.1 * w_scale.min(2.0)).max(1.0);
    let n_samp = (((2.0 * half_diag) / step_s).round() as usize + 1).max(16);
    let warp_freq = (0.012 * inv_sc) as f32;
    let decim_step = ((3.0 * w_scale / step_s).round() as usize).max(2);
    let angle_off_rad = f64::from(le.kakeami_angle_offset_deg) * DEG;

    let nv_min = (-half_diag / lane_pitch).floor() as i32 - 1;
    let nv_max = (half_diag / lane_pitch).ceil() as i32 + 1;

    let mut hatch_ribbons = Vec::new();
    let mut pts_buf: Vec<(f64, f64)> = Vec::with_capacity(n_samp);
    let mut sd_buf: Vec<f64> = Vec::with_capacity(n_samp);
    let mut dv_buf: Vec<f64> = Vec::with_capacity(n_samp);
    let mut act_buf: Vec<bool> = Vec::with_capacity(n_samp);

    for &(li, d_thresh) in layer_defs {
        // FIX #1: ZERO per-lane angular jitter (`ang_jit == 0`) — every lane in layer `li` is strictly parallel!
        let ang = LAYER_ANGLES_DEG[li] * DEG + angle_off_rad;
        let u_dir = (ang.cos(), ang.sin());
        let v_dir = (-u_dir.1, u_dir.0);
        let is_base_layer = li <= 1;
        let seed_v = seed.wrapping_mul(1993).wrapping_add((li as u32).wrapping_mul(317)).wrapping_add(11);

        for iv in nv_min..=nv_max {
            let ls = seed
                .wrapping_mul(6133)
                .wrapping_add((li as u32).wrapping_mul(997))
                .wrapping_add(((iv + 1000) as u32).wrapping_mul(193));
            let v0 = f64::from(iv) * lane_pitch + (h1(ls.wrapping_add(1)) - 0.5) * 2.0 * (0.05 * lane_pitch);
            let ox = cx_g + v_dir.0 * v0;
            let oy = cy_g + v_dir.1 * v0;

            // Clip line `o + u * u_dir` against padded glyph AABB `[xmin..xmax, ymin..ymax]` in O(1)
            let (u_lo, u_hi) = match clip_ray_to_aabb(ox, oy, u_dir, xmin, xmax, ymin, ymax, -half_diag, half_diag) {
                Some(r) => r,
                None => continue,
            };
            let len_u = u_hi - u_lo;
            if len_u < step_s * 2.5 {
                continue;
            }
            let m_b = ((len_u / step_s).floor() as usize + 1).max(3);
            pts_buf.clear();
            sd_buf.clear();
            dv_buf.clear();
            act_buf.clear();
            let lane_thr = if d_thresh <= 0.0 { 0.0 } else { d_thresh + (h1(ls.wrapping_add(3)) - 0.5) * 0.07 };
            let mut any_active = false;

            for k in 0..m_b {
                let u = u_lo + (k as f64) * step_s;
                let bx = ox + u_dir.0 * u;
                let by = oy + u_dir.1 * u;
                let vn = f64::from(value_noise_2d((bx as f32) * warp_freq, (by as f32) * warp_freq, seed_v));
                let v_flow = (vn - 0.5) * 2.0 * (0.07 * lane_pitch);
                let px = bx + v_dir.0 * v_flow;
                let py = by + v_dir.1 * v_flow;
                let sd = vsdf.signed_distance((px, py));
                let dv = f64::from(eval_density_doc(px as f32, py as f32, sd as f32, dp));
                let active = if d_thresh <= 0.0 { sd <= 0.0 } else { sd <= 0.0 && dv >= lane_thr };
                any_active |= active;
                pts_buf.push((px, py));
                sd_buf.push(sd);
                dv_buf.push(dv);
                act_buf.push(active);
            }
            if !any_active {
                continue;
            }

            let lane_hw_mul = 0.88 + 0.24 * h1(ls.wrapping_add(4));
            let mut idx = 0usize;
            let mut si = 0u32;
            while idx < m_b {
                if !act_buf[idx] {
                    idx += 1;
                    continue;
                }
                let i0 = idx;
                while idx < m_b && act_buf[idx] {
                    idx += 1;
                }
                let i1 = idx - 1;
                let ss = ls.wrapping_add(50).wrapping_add(si.wrapping_mul(31));
                si = si.wrapping_add(1);
                if i1 < i0 + 2 {
                    continue;
                }
                let hit_b_a = i0 > 0 && sd_buf[i0 - 1] > 0.0;
                let hit_b_b = i1 + 1 < m_b && sd_buf[i1 + 1] > 0.0;
                let ext_a = if hit_b_a {
                    compute_boundary_ext(pts_buf[i0], (-u_dir.0, -u_dir.1), is_base_layer, &vsdf, &hole_vsdf, w_scale, inv_sc, ext_scale, seed, ss.wrapping_add(1))
                } else {
                    0.0
                };
                let ext_b = if hit_b_b {
                    compute_boundary_ext(pts_buf[i1], u_dir, is_base_layer, &vsdf, &hole_vsdf, w_scale, inv_sc, ext_scale, seed, ss.wrapping_add(2))
                } else {
                    0.0
                };
                if let Some(strip) = build_lane_span_ribbon(
                    &pts_buf[i0..=i1],
                    &dv_buf[i0..=i1],
                    u_dir,
                    ext_a,
                    ext_b,
                    step_s,
                    decim_step,
                    w_scale,
                    base_hw,
                    lane_hw_mul,
                    li,
                ) {
                    hatch_ribbons.push(strip);
                }
            }
        }
    }

    KakeamiV4VectorGeometry {
        hatch_ribbons,
        stroke_w: stroke_w as f32,
        w_scale: w_scale as f32,
        grad_axis,
        grad_range,
        perp_range,
    }
}

fn clip_ray_to_aabb(
    ox: f64,
    oy: f64,
    dir: (f64, f64),
    xmin: f64,
    xmax: f64,
    ymin: f64,
    ymax: f64,
    mut t0: f64,
    mut t1: f64,
) -> Option<(f64, f64)> {
    let mut slab = |o: f64, d: f64, lo: f64, hi: f64| -> bool {
        if d.abs() < 1e-12 {
            return o >= lo && o <= hi;
        }
        let inv = 1.0 / d;
        let a = (lo - o) * inv;
        let b = (hi - o) * inv;
        t0 = t0.max(a.min(b));
        t1 = t1.min(a.max(b));
        t0 <= t1
    };
    if slab(ox, dir.0, xmin, xmax) && slab(oy, dir.1, ymin, ymax) {
        Some((t0, t1))
    } else {
        None
    }
}

pub fn build_kakeami_v4_vector(geom: &PreparedSfx, le: &SfxLineEffect) -> KakeamiV4VectorGeometry {
    let groups: Vec<Vec<PolyRing>> = geom
        .glyphs
        .iter()
        .map(|g| g.polylines.iter().map(|pl| pl.pts.clone()).collect())
        .collect();
    build_kakeami_v4_from_groups(&groups, le)
}

/// Full CPU oracle render for Style 4 (`Kakeami v4`, Continuous Unsegmented Lanes).
pub fn render_kakeami_v4_cpu(geom: &PreparedSfx, face_cov: &[f32], bounds: Rect, le: &SfxLineEffect) -> Vec<f32> {
    let w = bounds.width() as usize;
    let h = bounds.height() as usize;
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let kg = build_kakeami_v4_vector(geom, le);
    let mut cov = rasterize_ribbons_max(&kg.hatch_ribbons, bounds);

    if le.kakeami_mode == SfxKakeamiV4Mode::KakeEdge {
        let glyph_sdf = exact_sdf(face_cov, w, h);
        let dp = DensityParams {
            mode: SfxKakeamiV4Mode::KakeEdge,
            stroke_w: kg.stroke_w,
            w_scale: kg.w_scale,
            seed: le.kakeami_seed,
            grad_axis: kg.grad_axis,
            grad_range: kg.grad_range,
            perp_range: kg.perp_range,
        };
        let f_b = 0.10 / kg.w_scale.max(0.5);
        let break_seed = le.kakeami_seed.wrapping_mul(317).wrapping_add(23);
        let edge_off = -1.5 * kg.w_scale;
        for y in 0..h {
            let ys = (bounds.y0 as f32) + (y as f32) + 0.5;
            let row = y * w;
            for x in 0..w {
                let i = row + x;
                let gs = glyph_sdf[i];
                let beta_edge = (edge_off - gs).clamp(0.0, 1.0);
                if beta_edge <= 1e-4 {
                    continue;
                }
                let xs = (bounds.x0 as f32) + (x as f32) + 0.5;
                let d_val = eval_density_doc(xs, ys, gs, dp);
                let n_break = value_noise_2d(xs * f_b, ys * f_b, break_seed);
                let beta_field = d_val + (n_break - 0.5) * 0.15;
                let beta_mask = ((beta_field - 0.82) * 7.0).clamp(0.0, 1.0) * beta_edge;
                if beta_mask > cov[i] {
                    cov[i] = beta_mask;
                }
            }
        }
    }

    let grain_seed = le.kakeami_seed.wrapping_add(53);
    match le.kakeami_edge_mode {
        SfxV3EdgeMode::OneBit => {
            let grain_pct = if le.line_grain_pct > 0.5 { le.line_grain_pct } else { 10.0 };
            apply_edge_kasure_breakup_doc(
                &mut cov,
                bounds,
                EdgeKasureParams {
                    dryness_pct: grain_pct,
                    sharpness_pct: 100.0,
                    seed: grain_seed,
                    core_protect_px: 0.9,
                    one_bit: true,
                },
            );
        }
        SfxV3EdgeMode::Rough => {
            let dry = le.kakeami_dryness_pct.max(le.line_grain_pct);
            apply_edge_kasure_breakup_doc(
                &mut cov,
                bounds,
                EdgeKasureParams {
                    dryness_pct: dry,
                    sharpness_pct: le.kakeami_sharpness_pct,
                    seed: grain_seed,
                    core_protect_px: 0.85,
                    one_bit: false,
                },
            );
        }
    }
    cov
}

#[cfg(test)]
mod tests {
    use photocraft_doc::sfx::{SfxKakeamiV4Mode, SfxLineEffect, SfxLineStyle};

    use super::{DEG, LAYER_ANGLES_DEG, build_kakeami_v4_from_groups, build_lane_span_ribbon, compute_gradient_frame, eval_density_doc, DensityParams};

    #[test]
    fn kakeami_v4_zero_ang_jit_preserves_parallel_lanes_on_1200px_banner() {
        // Wide 1200 px banner spanning x=0..1200, y=40..140: with ang_jit != 0 neighboring lanes
        // diverge/converge by up to 1200*tan(0.6°) ~= 12.6 px (> pitch!) and cross each other.
        let banner = vec![vec![vec![(0.0, 40.0), (1200.0, 40.0), (1200.0, 140.0), (0.0, 140.0)]]];
        let le = SfxLineEffect {
            style: SfxLineStyle::KakeamiV4,
            kakeami_mode: SfxKakeamiV4Mode::Kake2,
            kakeami_angle_offset_deg: 0.0,
            kakeami_seed: 5,
            ..SfxLineEffect::default()
        };
        let kg = build_kakeami_v4_from_groups(&banner, &le);
        assert!(kg.hatch_ribbons.len() > 20, "expected dense parallel lanes across 1200px banner");

        // Every ribbon's overall chord angle must closely match LAYER_ANGLES_DEG[0] (-34°) or [1] (52°)
        // (only tiny ±0.07*pitch v-flow deviation, ZERO ±0.6° per-lane tilt!).
        let a0 = LAYER_ANGLES_DEG[0] * DEG;
        let a1 = LAYER_ANGLES_DEG[1] * DEG;
        for r in &kg.hatch_ribbons {
            let n = r.len();
            assert!(n >= 2);
            let mx0 = 0.5 * f64::from(r.left[0][0] + r.right[0][0]);
            let my0 = 0.5 * f64::from(r.left[0][1] + r.right[0][1]);
            let mx1 = 0.5 * f64::from(r.left[n - 1][0] + r.right[n - 1][0]);
            let my1 = 0.5 * f64::from(r.left[n - 1][1] + r.right[n - 1][1]);
            let len = (mx1 - mx0).hypot(my1 - my0);
            if len < 60.0 {
                continue;
            }
            let ang = (my1 - my0).atan2(mx1 - mx0);
            let d0 = (ang - a0).sin().abs().min((ang - a1).sin().abs());
            assert!(d0 < 0.015, "lane chord drifted from parallel layer angle (sin_err={d0:.4}, len={len:.1})");
        }
    }

    #[test]
    fn kakeami_v4_short_3_sample_span_decimation_has_zero_nan_or_inf() {
        // 3-sample span with coinciding endpoints to test decimation & zero-tangent guard.
        let pts = [(10.0, 10.0), (10.0, 10.0), (15.0, 13.0)];
        let dvs = [0.6, 0.6, 0.6];
        let strip = build_lane_span_ribbon(&pts, &dvs, (0.8, 0.6), 0.0, 0.0, 1.1, 3, 1.0, 1.0, 1.0, 0)
            .expect("valid 3-sample span must produce finite ribbon");
        assert!(strip.len() >= 2);
        for i in 0..strip.len() {
            assert!(strip.left[i][0].is_finite() && strip.left[i][1].is_finite());
            assert!(strip.right[i][0].is_finite() && strip.right[i][1].is_finite());
        }
    }

    #[test]
    fn kakeami_v4_kake_edge_gradient_angle_rotates_cascade_axis() {
        let box_g = vec![vec![vec![(0.0, 0.0), (200.0, 0.0), (200.0, 200.0), (0.0, 200.0)]]];
        // Default 90° -> top (y=10) lighter than bottom (y=190).
        let (ga90, gr90, pr90) = compute_gradient_frame(&box_g, 90.0);
        let dp90 = DensityParams {
            mode: SfxKakeamiV4Mode::KakeEdge,
            stroke_w: 30.0,
            w_scale: 1.0,
            seed: 1,
            grad_axis: ga90,
            grad_range: gr90,
            perp_range: pr90,
        };
        let top90 = eval_density_doc(100.0, 10.0, -10.0, dp90);
        let bot90 = eval_density_doc(100.0, 190.0, -10.0, dp90);
        assert!(bot90 > top90 + 0.4, "90° cascade must increase top->bottom ({top90:.2} vs {bot90:.2})");

        // Rotated 0° -> left (x=10) lighter than right (x=190).
        let (ga0, gr0, pr0) = compute_gradient_frame(&box_g, 0.0);
        let dp0 = DensityParams {
            mode: SfxKakeamiV4Mode::KakeEdge,
            stroke_w: 30.0,
            w_scale: 1.0,
            seed: 1,
            grad_axis: ga0,
            grad_range: gr0,
            perp_range: pr0,
        };
        let left0 = eval_density_doc(10.0, 100.0, -10.0, dp0);
        let right0 = eval_density_doc(190.0, 100.0, -10.0, dp0);
        assert!(right0 > left0 + 0.4, "0° cascade must increase left->right ({left0:.2} vs {right0:.2})");
    }
}
