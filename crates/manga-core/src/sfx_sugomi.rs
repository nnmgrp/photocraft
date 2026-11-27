//! Style 2 — 凄みベタ (`Sugomi Spiky Ink v3`, rev 5) 1:1 port of `sugomi_v3.py`.
//!
//! 1. Three direction modes (`Fude` continuous zigzag ring with corner boost `<= 1.32`, coaxial
//!    V-clefts, convex mid-flanks `w_len <= 2.55 * step`; `Tate` vertical comb; `Yoko` horizontal comb).
//! 2. Both `Solid` and `Hollow` fills with thin-font guard **`band_w <= 0.55 * stroke_w`**.
//! 3. Concave reflex bend guard (`turn_val < -0.2`) damping V-cleft depth so inner corners never fold.
//! 4. Compact irregular inner mini-teeth on counter holes (`<= 0.24 * void_chord`, nesting-depth
//!    classified) and hollow inner wall + 2D-SDF torn inner cutout.
//! 5. Unified single-pass `apply_edge_kasure_breakup_doc` over the entire combined mask.

use photocraft_doc::sfx::{SfxLineEffect, SfxSugomiDirection, SfxV3EdgeMode};
use photocraft_geom::Rect;

use crate::sfx_geom::PreparedSfx;
use crate::sfx_v3_common::{
    EdgeKasureParams, PolyRing, VectorGlyphSdf, apply_edge_kasure_breakup_doc, classify_glyph_contours, exact_sdf, h1, polyline_resample,
    rasterize_polygons_max, rot2, sample_base, smooth_scalar, smooth_vec, torn_1d, value_noise_1d, value_noise_2d,
};

const DEG: f64 = std::f64::consts::PI / 180.0;

/// Extracted vector geometry for `Sugomi v3`, shared between the CPU oracle and GPU renderer.
#[derive(Clone, Debug, Default)]
pub struct SugomiVectorGeometry {
    /// Continuous jagged outer rings (positive winding).
    pub outer_spiky_polys: Vec<PolyRing>,
    /// Compact jagged interior counter-hole cutout rings (positive winding).
    pub hole_jagged_polys: Vec<PolyRing>,
    /// Convex pentagon inward hollow mini-teeth (rendered directly with `BlendOperation::Max`).
    pub inner_teeth_polys: Vec<PolyRing>,
    pub stroke_w: f32,
    pub band_w: f32,
    pub hw_in: f32,
}

/// Computes hollow `band_w` with mandatory thin-font guard `band_w <= 0.55 * stroke_w`.
#[inline]
pub fn compute_sugomi_band_w(stroke_w: f64, glyph_em_px: f64) -> f64 {
    let band_cap = (glyph_em_px * 0.18).max(22.0);
    let raw = (stroke_w * 0.46).clamp(3.0, band_cap);
    raw.min(stroke_w * 0.55)
}

/// 2D-SDF torn cutout field (`sugomi_v3.py:110-137`) in **document coordinates**
/// (1:1 identical to `sfx.wgsl:fs_sugomi_hollow_cut`).
pub fn perturb_torn_sdf_doc(sdf: &[f32], bounds: Rect, scale_px: f32, seed: u32, rough_mul: f32, hollow: bool) -> Vec<f32> {
    let w = bounds.width() as usize;
    let h = bounds.height() as usize;
    if w == 0 || h == 0 {
        return sdf.to_vec();
    }
    let sc = scale_px.max(8.0);
    let inv = 1.0 / sc;
    let s_mul = seed.wrapping_mul(137);
    let amp = rough_mul * sc;
    let lo = -sc * if hollow { 0.14 } else { 0.12 };
    let hi = sc * if hollow { 0.14 } else { 0.11 };

    let rnoise = |xs: f32, ys: f32, ang: f32, fm: f32, off: u32| -> f32 {
        let (sa, ca) = ang.sin_cos();
        let f = fm * inv;
        value_noise_2d((xs * ca - ys * sa) * f, (xs * sa + ys * ca) * f, s_mul.wrapping_add(off))
    };

    let mut out = vec![0.0_f32; w * h];
    for y in 0..h {
        let ys = (bounds.y0 as f32) + (y as f32) + 0.5;
        let row = y * w;
        for x in 0..w {
            let xs = (bounds.x0 as f32) + (x as f32) + 0.5;
            let n_low = (rnoise(xs, ys, 0.41, 0.22, 13) - 0.5) * 2.0;
            let n_mid = (rnoise(xs, ys, 1.17, 0.68, 37) - 0.5) * 2.0;
            let n_fine = (rnoise(xs, ys, 2.03, 1.65, 79) - 0.5) * 2.0;
            let c_mix = rnoise(xs, ys, 0.83, 1.05, 131) * 0.6 + rnoise(xs, ys, 2.39, 2.15, 193) * 0.4;
            let chip = (c_mix - 0.67).max(0.0) / 0.33;
            let disp = ((n_low * 0.10 + n_mid * 0.09 + n_fine * 0.06) * amp - chip * chip * (amp * 0.18)).clamp(lo, hi);
            out[row + x] = sdf[row + x] - disp;
        }
    }
    out
}

/// Computes the concave-bend attenuation multiplier for V-valley depth `dip_v`.
/// When `turn_val < -0.2` (sharp concave reflex corner), damps `dip_v` down to `0.15×` so clefts
/// never fold inward on calligraphic / italic letters.
#[inline]
pub fn concave_dip_damp(turn_val: f64) -> f64 {
    if turn_val < -0.2 {
        (1.0 - ((-turn_val - 0.2) / 0.6).clamp(0.0, 1.0) * 0.85).clamp(0.15, 1.0)
    } else {
        1.0
    }
}

struct RingContext<'a> {
    base_pts: &'a [(f64, f64)],
    nrm_s: &'a [(f64, f64)],
    tang: &'a [(f64, f64)],
    arc: &'a [f64],
    total: f64,
    a_c: f64,
    max_dip_c: f64,
    hollow: bool,
    ci: u32,
    seed: u32,
}

fn build_fude_ring(ctx: &RingContext<'_>, clear_d: &[f64], ink_d: &[f64]) -> PolyRing {
    let n = ctx.base_pts.len();
    let mut turn_val = vec![0.0_f64; n];
    let mut dot_t = vec![1.0_f64; n];
    let mut corner_boost = vec![0.0_f64; n];
    let sigma = (ctx.a_c * 0.90).max(14.0);

    for i in 0..n {
        let prev = (i + n - 1) % n;
        let tp = ctx.tang[prev];
        let tc = ctx.tang[i];
        turn_val[i] = ctx.nrm_s[i].0 * tp.0 + ctx.nrm_s[i].1 * tp.1;
        dot_t[i] = (tp.0 * tc.0 + tp.1 * tc.1).clamp(-1.0, 1.0);
    }
    for ci_k in 0..n {
        if dot_t[ci_k] < 0.76 && turn_val[ci_k] > 0.10 {
            let strength = 0.32 + 0.36 * (1.0 - dot_t[ci_k]).clamp(0.0, 1.0);
            for i in 0..n {
                let da = (ctx.arc[i] - ctx.arc[ci_k]).abs();
                let d_wrap = da.min(ctx.total - da);
                let g = strength * (-0.5 * (d_wrap / sigma).powi(2)).exp();
                if g > corner_boost[i] {
                    corner_boost[i] = g;
                }
            }
        }
    }

    let s_slow = ctx.seed.wrapping_mul(409).wrapping_add(ctx.ci.wrapping_mul(29));
    let s_mid = ctx.seed.wrapping_mul(617).wrapping_add(ctx.ci.wrapping_mul(43));
    let s_lean = ctx.seed.wrapping_mul(823).wrapping_add(ctx.ci.wrapping_mul(59));
    let mut env = vec![0.0_f64; n];
    let mut lean_ang_arr = vec![0.0_f64; n];
    for i in 0..n {
        let a_f = ctx.arc[i] as f32;
        let w_slow = f64::from(value_noise_1d(a_f * 0.0115, s_slow));
        let w_mid = f64::from(value_noise_1d(a_f * 0.029, s_mid));
        env[i] = (0.20 + 0.72 * w_slow.powf(0.9) + 0.28 * (w_mid - 0.5) + corner_boost[i] * 0.55).clamp(0.0, 1.32);
        let lw = (f64::from(value_noise_1d(a_f * 0.0068, s_lean)) - 0.5) * 2.0;
        lean_ang_arr[i] = (lw * 2.8).tanh() * (34.0 * DEG);
    }

    let pitch0 = (ctx.a_c * 0.40).clamp(6.8, 11.6);
    let torn_amp = (ctx.a_c * 0.11).clamp(1.4, 3.0);
    let mut verts = Vec::with_capacity(n * 2);
    let mut s_cur = 0.0_f64;
    let mut k = 0u32;

    while s_cur < ctx.total - 1.5 {
        let ks = ctx.seed.wrapping_mul(4111).wrapping_add(ctx.ci.wrapping_mul(197)).wrapping_add(k.wrapping_mul(53));
        let idx = ctx.arc.partition_point(|&v| v <= s_cur).saturating_sub(1) % n;
        let e_loc = env[idx];
        let clr = clear_d[idx];
        let gap_gain = ((clr - 12.0) / 24.0).clamp(0.0, 1.0);

        if e_loc < 0.18 || gap_gain <= 0.12 || ctx.a_c < 4.5 {
            let step = (ctx.total - s_cur).min(3.8);
            let (p_q, n_q, _) = sample_base(ctx.base_pts, ctx.nrm_s, ctx.arc, ctx.total, s_cur);
            let d_t = torn_1d(s_cur, ctx.ci, ctx.seed, torn_amp).clamp(-ctx.max_dip_c * 0.35, (clr * 0.25).max(0.8));
            verts.push((p_q.0 + n_q.0 * d_t, p_q.1 + n_q.1 * d_t));
            s_cur += step;
            k = k.wrapping_add(1);
            continue;
        }

        let mut step = (pitch0 * (1.14 - 0.28 * e_loc.min(1.2)) * (0.80 + 0.42 * h1(ks.wrapping_add(1)))).max(4.4);
        let rem = ctx.total - s_cur;
        if rem < 4.0 {
            let (p_q, _, _) = sample_base(ctx.base_pts, ctx.nrm_s, ctx.arc, ctx.total, s_cur);
            verts.push(p_q);
            break;
        }
        if step > rem {
            step = rem;
        }

        // Open V-valley biting co-axially into the ink body (with concave bend guard!)
        let (p_v, n_v, idx_v) = sample_base(ctx.base_pts, ctx.nrm_s, ctx.arc, ctx.total, s_cur);
        let dir_v = rot2(n_v, lean_ang_arr[idx_v]);
        let lim_v = if ctx.hollow { ctx.max_dip_c } else { ctx.max_dip_c.min(0.34 * ink_d[idx_v]) };
        let raw_dip = if h1(ks.wrapping_add(7)) < 0.22 {
            lim_v * (0.06 + 0.16 * h1(ks.wrapping_add(8)))
        } else {
            lim_v * (0.45 + 0.55 * h1(ks.wrapping_add(8))) * (0.55 + 0.45 * e_loc).clamp(0.55, 1.0)
        };
        let dip_v = (raw_dip * concave_dip_damp(turn_val[idx_v])).min(lim_v);
        verts.push((p_v.0 - dir_v.0 * dip_v, p_v.1 - dir_v.1 * dip_v));

        // Sturdy blade tip + convex mid-flanks (`w_len <= 2.55 * step`)
        let s_t = s_cur + step * (0.43 + 0.14 * h1(ks.wrapping_add(2)));
        let (p_t, n_t, idx_t) = sample_base(ctx.base_pts, ctx.nrm_s, ctx.arc, ctx.total, s_t);
        let clr_t = clear_d[idx_t];
        let gg_t = ((clr_t - 12.0) / 24.0).clamp(0.0, 1.0);
        let ang_t = lean_ang_arr[idx_t] + (h1(ks.wrapping_add(3)) - 0.5) * (5.0 * DEG);
        let dir_t = rot2(n_t, ang_t);

        let jagged = 0.46 + 0.64 * h1(ks.wrapping_add(4)).powf(1.1);
        let w_len = (ctx.a_c * 0.90 * env[idx_t] * jagged * gg_t)
            .min(step * 2.55)
            .min((clr_t * 0.42).max(3.0));

        if w_len >= 3.8 {
            let (p_ml, _, _) = sample_base(ctx.base_pts, ctx.nrm_s, ctx.arc, ctx.total, s_cur + step * 0.22);
            let (p_mr, _, _) = sample_base(ctx.base_pts, ctx.nrm_s, ctx.arc, ctx.total, s_cur + step * 0.78);
            verts.push((p_ml.0 + dir_t.0 * (w_len * 0.54), p_ml.1 + dir_t.1 * (w_len * 0.54)));
            verts.push((p_t.0 + dir_t.0 * w_len, p_t.1 + dir_t.1 * w_len));
            verts.push((p_mr.0 + dir_t.0 * (w_len * 0.54), p_mr.1 + dir_t.1 * (w_len * 0.54)));
        } else {
            let wl = w_len.max(0.5);
            verts.push((p_t.0 + dir_t.0 * wl, p_t.1 + dir_t.1 * wl));
        }

        s_cur += step;
        k = k.wrapping_add(1);
    }

    if verts.len() >= 6 { verts } else { ctx.base_pts.to_vec() }
}

fn build_comb_ring(mode: SfxSugomiDirection, ctx: &RingContext<'_>, vsdf: &VectorGlyphSdf, hw_out: f64) -> PolyRing {
    let n = ctx.base_pts.len();
    let mut abs_c = Vec::with_capacity(n);
    let mut sgn = Vec::with_capacity(n);
    let mut comb_dirs = Vec::with_capacity(n);
    let mut clear_ax = Vec::with_capacity(n);
    let mut ink_ax = Vec::with_capacity(n);
    let mut turn_val = Vec::with_capacity(n);

    for i in 0..n {
        let prev = (i + n - 1) % n;
        let tp = ctx.tang[prev];
        turn_val.push(ctx.nrm_s[i].0 * tp.0 + ctx.nrm_s[i].1 * tp.1);
        let comp = if mode == SfxSugomiDirection::Tate { ctx.nrm_s[i].1 } else { ctx.nrm_s[i].0 };
        let ac = comp.abs();
        let sg = if comp >= 0.0 { 1.0 } else { -1.0 };
        let cd = if mode == SfxSugomiDirection::Tate { (0.0, sg) } else { (sg, 0.0) };
        abs_c.push(ac);
        sgn.push(sg);
        comb_dirs.push(cd);
        let raw_clr = vsdf.probe_ray(ctx.base_pts[i], cd, 2.5, 96.0);
        let eff_clr = if ctx.hollow { (raw_clr - hw_out).max(0.0) } else { raw_clr };
        let raw_ink = vsdf.probe_ray(ctx.base_pts[i], (-cd.0, -cd.1), 2.0, 96.0);
        clear_ax.push(eff_clr);
        ink_ax.push(raw_ink);
    }

    let pitch0 = (ctx.a_c * 0.32).clamp(5.6, 9.2);
    let torn_amp = (ctx.a_c * 0.10).clamp(1.4, 2.8);
    let mut verts = Vec::with_capacity(n * 2);
    let mut s_cur = 0.0_f64;
    let mut k = 0u32;

    while s_cur < ctx.total - 1.5 {
        let ks = ctx.seed.wrapping_mul(7919).wrapping_add(ctx.ci.wrapping_mul(151)).wrapping_add(k.wrapping_mul(37));
        let idx = ctx.arc.partition_point(|&v| v <= s_cur).saturating_sub(1) % n;
        let ac = abs_c[idx];
        let clr = clear_ax[idx];
        let gap_gain = ((clr - 10.0) / 22.0).clamp(0.0, 1.0);

        if ac <= 0.24 || gap_gain <= 0.12 || ctx.a_c < 4.5 {
            let step = (ctx.total - s_cur).min(3.6);
            let (p_q, n_q, _) = sample_base(ctx.base_pts, ctx.nrm_s, ctx.arc, ctx.total, s_cur);
            let d_t = torn_1d(s_cur, ctx.ci, ctx.seed.wrapping_add(13), torn_amp).clamp(-ctx.max_dip_c * 0.35, (clr * 0.25).max(0.8));
            verts.push((p_q.0 + n_q.0 * d_t, p_q.1 + n_q.1 * d_t));
            s_cur += step;
            k = k.wrapping_add(1);
            continue;
        }

        let proj_p = (pitch0 * (0.76 + 0.52 * h1(ks.wrapping_add(1)))).max(4.4);
        let mut step = (proj_p / ac.max(0.35)).min(pitch0 * 2.2).max(4.2);
        let rem = ctx.total - s_cur;
        if rem < 3.6 {
            let (p_q, _, _) = sample_base(ctx.base_pts, ctx.nrm_s, ctx.arc, ctx.total, s_cur);
            verts.push(p_q);
            break;
        }
        if step > rem {
            step = rem;
        }

        let s_end = (s_cur + step).rem_euclid(ctx.total);
        let idx_end = ctx.arc.partition_point(|&v| v <= s_end).saturating_sub(1) % n;
        if sgn[idx_end] != sgn[idx] || abs_c[idx_end] <= 0.22 {
            let (p_q, _, _) = sample_base(ctx.base_pts, ctx.nrm_s, ctx.arc, ctx.total, s_cur);
            verts.push(p_q);
            s_cur += (step * 0.5).min(3.0);
            k = k.wrapping_add(1);
            continue;
        }

        let ax_d = comb_dirs[idx];
        let (p_v, _, idx_v) = sample_base(ctx.base_pts, ctx.nrm_s, ctx.arc, ctx.total, s_cur);
        let lim_v = if ctx.hollow { ctx.max_dip_c } else { ctx.max_dip_c.min(0.35 * ink_ax[idx_v]) };
        let dip_v = lim_v
            * (0.35 + 0.65 * h1(ks.wrapping_add(6)).powf(1.05))
            * ac.powf(0.6)
            * concave_dip_damp(turn_val[idx_v]);
        verts.push((p_v.0 - ax_d.0 * dip_v, p_v.1 - ax_d.1 * dip_v));

        let s_t = s_cur + step * (0.44 + 0.12 * h1(ks.wrapping_add(2)));
        let (p_t, _, idx_t) = sample_base(ctx.base_pts, ctx.nrm_s, ctx.arc, ctx.total, s_t);
        let clr_t = clear_ax[idx_t];
        let gg_t = ((clr_t - 10.0) / 22.0).clamp(0.0, 1.0);
        let env_v = f64::from(value_noise_1d((s_t as f32) * 0.022, ctx.seed.wrapping_mul(313).wrapping_add(ctx.ci.wrapping_mul(17))));
        let h_mul = (0.30 + 0.72 * h1(ks.wrapping_add(3)).powf(1.2)) * (0.64 + 0.86 * env_v);
        let w_len = (ctx.a_c * 1.24 * h_mul * abs_c[idx_t].powf(0.70) * gg_t)
            .min(proj_p * 3.0)
            .min((clr_t * 0.45).max(3.5));

        let tip_dir = rot2(ax_d, (h1(ks.wrapping_add(4)) - 0.5) * (5.5 * DEG));
        if w_len >= 3.8 {
            let (p_ml, _, _) = sample_base(ctx.base_pts, ctx.nrm_s, ctx.arc, ctx.total, s_cur + step * 0.22);
            let (p_mr, _, _) = sample_base(ctx.base_pts, ctx.nrm_s, ctx.arc, ctx.total, s_cur + step * 0.78);
            verts.push((p_ml.0 + tip_dir.0 * (w_len * 0.52), p_ml.1 + tip_dir.1 * (w_len * 0.52)));
            verts.push((p_t.0 + tip_dir.0 * w_len, p_t.1 + tip_dir.1 * w_len));
            verts.push((p_mr.0 + tip_dir.0 * (w_len * 0.52), p_mr.1 + tip_dir.1 * (w_len * 0.52)));
        } else {
            let wl = w_len.max(0.5);
            verts.push((p_t.0 + tip_dir.0 * wl, p_t.1 + tip_dir.1 * wl));
        }

        s_cur += step;
        k = k.wrapping_add(1);
    }

    if verts.len() >= 6 { verts } else { ctx.base_pts.to_vec() }
}

struct HoleParams {
    a_inner: f64,
    hollow: bool,
    hw_out_h: f64,
    band_w: f64,
    ci: u32,
    seed: u32,
}

fn build_hole_jagged_ring(cont: &[(f64, f64)], vsdf: &VectorGlyphSdf, hp: &HoleParams) -> PolyRing {
    let ring = polyline_resample(cont, 2.0);
    let n = ring.len();
    if n < 10 {
        return cont.to_vec();
    }
    let orient = vsdf.contour_orient_sign(&ring); // +nrm points INTO hole (air)
    let mut nrm = Vec::with_capacity(n);
    for i in 0..n {
        let prev = ring[(i + n - 1) % n];
        let next = ring[(i + 1) % n];
        let tx = next.0 - prev.0;
        let ty = next.1 - prev.1;
        let tl = tx.hypot(ty).max(1e-9);
        nrm.push(((ty / tl) * orient, -(tx / tl) * orient));
    }
    let nrm_s = smooth_vec(&nrm, 3);

    let mut void_d = Vec::with_capacity(n);
    let mut ink_d = Vec::with_capacity(n);
    for i in 0..n {
        void_d.push(vsdf.probe_ray(ring[i], nrm_s[i], 2.0, 84.0));
        ink_d.push(vsdf.probe_ray(ring[i], (-nrm_s[i].0, -nrm_s[i].1), 2.0, 84.0));
    }

    let base_pts: PolyRing = if hp.hollow {
        let raw_off: Vec<f64> = void_d.iter().map(|&v| hp.hw_out_h.min((v * 0.14).max(0.5))).collect();
        let off_h = smooth_scalar(&raw_off, 2);
        for i in 0..n {
            void_d[i] = (void_d[i] - 2.0 * off_h[i]).max(4.0);
        }
        (0..n).map(|i| (ring[i].0 + nrm_s[i].0 * off_h[i], ring[i].1 + nrm_s[i].1 * off_h[i])).collect()
    } else {
        ring
    };

    let mut arc = Vec::with_capacity(n);
    let mut total = 0.0_f64;
    for i in 0..n {
        arc.push(total);
        let a = base_pts[i];
        let b = base_pts[(i + 1) % n];
        total += (b.0 - a.0).hypot(b.1 - a.1);
    }
    total = total.max(1e-6);

    let s_env = hp.seed.wrapping_mul(571).wrapping_add(hp.ci.wrapping_mul(37));
    let s_lean = hp.seed.wrapping_mul(683).wrapping_add(hp.ci.wrapping_mul(41));
    let env_h: Vec<f64> = arc.iter().map(|&a| f64::from(value_noise_1d((a as f32) * 0.032, s_env))).collect();
    let lean_h: Vec<f64> = arc.iter().map(|&a| (f64::from(value_noise_1d((a as f32) * 0.018, s_lean)) - 0.5) * (44.0 * DEG)).collect();

    let pitch0 = (hp.a_inner * 1.15).clamp(6.2, 11.0);
    let mut verts = Vec::with_capacity(n * 2);
    let mut s_cur = 0.0_f64;
    let mut k = 0u32;

    while s_cur < total - 1.5 {
        let ks = hp.seed.wrapping_mul(3187).wrapping_add(hp.ci.wrapping_mul(127)).wrapping_add(k.wrapping_mul(43));
        let idx = arc.partition_point(|&v| v <= s_cur).saturating_sub(1) % n;
        let ev = env_h[idx];
        let vd = void_d[idx];
        let id_loc = ink_d[idx];
        let max_tooth = (hp.a_inner * 1.25).min((vd * 0.24).max(1.2));
        let max_nick = (hp.a_inner * 0.45).min(if hp.hollow { hp.band_w * 0.16 } else { id_loc * 0.16 });

        if ev < 0.34 || max_tooth < 2.0 {
            let step = (total - s_cur).min(4.0);
            let (p_q, n_q, _) = sample_base(&base_pts, &nrm_s, &arc, total, s_cur);
            let dt = torn_1d(s_cur, hp.ci.wrapping_add(90), hp.seed, (max_tooth * 0.5).min(1.8)).clamp(-max_nick, max_tooth * 0.45);
            verts.push((p_q.0 + n_q.0 * dt, p_q.1 + n_q.1 * dt));
            s_cur += step;
            k = k.wrapping_add(1);
            continue;
        }

        let mut step = (pitch0 * (0.72 + 0.68 * h1(ks.wrapping_add(1)))).max(4.6);
        let rem = total - s_cur;
        if rem < 4.0 {
            let (p_q, _, _) = sample_base(&base_pts, &nrm_s, &arc, total, s_cur);
            verts.push(p_q);
            break;
        }
        if step > rem {
            step = rem;
        }

        let (p_v, n_v, idx_v) = sample_base(&base_pts, &nrm_s, &arc, total, s_cur);
        let dir_v = rot2(n_v, lean_h[idx_v]);
        let nick = max_nick * (0.25 + 0.75 * h1(ks.wrapping_add(2)));
        verts.push((p_v.0 - dir_v.0 * nick, p_v.1 - dir_v.1 * nick));

        let s_t = s_cur + step * (0.38 + 0.24 * h1(ks.wrapping_add(3)));
        let (p_t, n_t, idx_t) = sample_base(&base_pts, &nrm_s, &arc, total, s_t);
        let dir_t = rot2(n_t, lean_h[idx_t] + (h1(ks.wrapping_add(4)) - 0.5) * (10.0 * DEG));
        let vd_t = void_d[idx_t];
        let t_len = (hp.a_inner * (0.45 + 0.80 * h1(ks.wrapping_add(5))) * (0.65 + 0.60 * env_h[idx_t]))
            .min((vd_t * 0.24).max(1.5))
            .min(step * 1.45);
        verts.push((p_t.0 + dir_t.0 * t_len, p_t.1 + dir_t.1 * t_len));

        s_cur += step;
        k = k.wrapping_add(1);
    }

    if verts.len() >= 6 { verts } else { base_pts }
}

fn build_inner_hollow_teeth(
    contour_groups: &[Vec<PolyRing>],
    vsdf: &VectorGlyphSdf,
    hw_in: f64,
    a_inner: f64,
    band_w: f64,
    seed: u32,
) -> Vec<PolyRing> {
    let mut polys = Vec::new();
    let mut ci = 0u32;
    for group in contour_groups {
        for cont in group {
            ci = ci.wrapping_add(1);
            let ring = polyline_resample(cont, 2.2);
            let n = ring.len();
            if n < 10 {
                continue;
            }
            let orient = vsdf.contour_orient_sign(&ring);
            let mut nrm = Vec::with_capacity(n);
            for i in 0..n {
                let prev = ring[(i + n - 1) % n];
                let next = ring[(i + 1) % n];
                let tx = next.0 - prev.0;
                let ty = next.1 - prev.1;
                let tl = tx.hypot(ty).max(1e-9);
                nrm.push((-(ty / tl) * orient, (tx / tl) * orient)); // points INTO ink body
            }
            let in_dir = smooth_vec(&nrm, 3);
            let anchor: PolyRing = (0..n).map(|i| (ring[i].0 + in_dir[i].0 * hw_in, ring[i].1 + in_dir[i].1 * hw_in)).collect();

            let mut arc = Vec::with_capacity(n);
            let mut total = 0.0_f64;
            for i in 0..n {
                arc.push(total);
                let a = anchor[i];
                let b = anchor[(i + 1) % n];
                total += (b.0 - a.0).hypot(b.1 - a.1);
            }
            total = total.max(1e-6);

            let s_env = seed.wrapping_mul(739).wrapping_add(ci.wrapping_mul(53));
            let s_lean = seed.wrapping_mul(857).wrapping_add(ci.wrapping_mul(61));
            let env: Vec<f64> = arc.iter().map(|&a| f64::from(value_noise_1d((a as f32) * 0.036, s_env))).collect();
            let lean: Vec<f64> = arc.iter().map(|&a| (f64::from(value_noise_1d((a as f32) * 0.020, s_lean)) - 0.5) * (42.0 * DEG)).collect();

            let pitch0 = (a_inner * 1.25).clamp(6.8, 12.0);
            let mut s_cur = pitch0 * 0.4 * h1(seed.wrapping_mul(211).wrapping_add(ci.wrapping_mul(17)));
            let mut k = 0u32;

            while s_cur < total - 2.0 {
                let ks = seed.wrapping_mul(4999).wrapping_add(ci.wrapping_mul(139)).wrapping_add(k.wrapping_mul(41));
                let step = (pitch0 * (0.72 + 0.68 * h1(ks.wrapping_add(1)))).max(5.2);
                let idx = arc.partition_point(|&v| v <= s_cur).saturating_sub(1) % n;
                s_cur += step;
                k = k.wrapping_add(1);
                if env[idx] < 0.36 {
                    continue;
                }
                let raw_ink_d = vsdf.probe_ray(ring[idx], in_dir[idx], 1.5, 64.0 + 2.0 * hw_in);
                let vw = (raw_ink_d - 2.0 * hw_in).max(0.0);
                let test_pt = (anchor[idx].0 + in_dir[idx].0 * 2.0, anchor[idx].1 + in_dir[idx].1 * 2.0);
                // `inner_sdf <= 0` <=> `glyph_sdf + hw_in <= 0`
                if vw < 7.0 || (vsdf.signed_distance(test_pt) + hw_in) > 0.0 {
                    continue;
                }
                let t_len = (a_inner * (0.45 + 0.75 * h1(ks.wrapping_add(2))) * (0.65 + 0.60 * env[idx]))
                    .min(vw * 0.24)
                    .min(step * 1.15);
                if t_len < 2.2 {
                    continue;
                }
                let ax = rot2(in_dir[idx], lean[idx] + (h1(ks.wrapping_add(3)) - 0.5) * (10.0 * DEG));
                let perp = (-ax.1, ax.0);
                let bw = (step * 0.82).min(t_len * 0.95 + 2.2).max(3.6) * 0.5;
                let root_back = (band_w * 0.32).min(4.2);
                let bp = anchor[idx];
                polys.push(vec![
                    (bp.0 - ax.0 * root_back - perp.0 * bw, bp.1 - ax.1 * root_back - perp.1 * bw),
                    (bp.0 - perp.0 * (bw * 0.95), bp.1 - perp.1 * (bw * 0.95)),
                    (bp.0 + ax.0 * t_len, bp.1 + ax.1 * t_len),
                    (bp.0 + perp.0 * (bw * 0.95), bp.1 + perp.1 * (bw * 0.95)),
                    (bp.0 - ax.0 * root_back + perp.0 * bw, bp.1 - ax.1 * root_back + perp.1 * bw),
                ]);
            }
        }
    }
    polys
}

/// Builds all Sugomi v3 vector rings (outer spiky rings, jagged holes, and hollow inward mini-teeth)
/// using analytical `VectorGlyphSdf` (zero pixel allocation, zero GPU readback).
pub fn build_sugomi_v3_vector(geom: &PreparedSfx, glyph_em_px: f32, le: &SfxLineEffect) -> SugomiVectorGeometry {
    let groups: Vec<Vec<PolyRing>> = geom
        .glyphs
        .iter()
        .map(|g| g.polylines.iter().map(|pl| pl.pts.clone()).collect())
        .collect();
    let vsdf = VectorGlyphSdf::from_prepared(geom);
    build_sugomi_v3_from_groups(&groups, &vsdf, f64::from(glyph_em_px), le)
}

pub fn build_sugomi_v3_from_groups(
    groups: &[Vec<PolyRing>],
    vsdf: &VectorGlyphSdf,
    glyph_em_px: f64,
    le: &SfxLineEffect,
) -> SugomiVectorGeometry {
    let seed = le.sugomi_seed;
    let mode = le.sugomi_direction;
    let hollow = le.sugomi_hollow;

    let stroke_w = vsdf.estimate_stroke_w(glyph_em_px);
    let band_w = compute_sugomi_band_w(stroke_w, glyph_em_px);
    let hw_out = band_w * 0.64;
    let hw_in = band_w * 0.36;
    let hw_out_h = band_w * 0.16;

    let mut amp = stroke_w * f64::from(le.sugomi_spike_amp_pct) / 100.0;
    if hollow {
        amp = (amp * 0.88).max(band_w * 1.24);
    }
    let a_inner = (amp * 0.26).clamp(3.2, (glyph_em_px * 0.065).max(7.8));
    let max_dip = if hollow {
        band_w * 0.34
    } else {
        stroke_w * f64::from(le.sugomi_cleft_dip_pct) / 100.0
    };

    let mut outer_spiky_polys = Vec::new();
    let mut hole_jagged_polys = Vec::new();
    let mut ci = 0u32;
    let mut hi = 0u32;

    for group in groups {
        if group.is_empty() {
            continue;
        }
        let classes = classify_glyph_contours(group);
        for (cont, cls) in group.iter().zip(classes) {
            if cls.is_hole {
                hi = hi.wrapping_add(1);
                let hp = HoleParams { a_inner, hollow, hw_out_h, band_w, ci: hi, seed: seed.wrapping_add(17) };
                hole_jagged_polys.push(build_hole_jagged_ring(cont, vsdf, &hp));
                continue;
            }
            ci = ci.wrapping_add(1);
            let ring = polyline_resample(cont, 2.0);
            let n = ring.len();
            if n < 12 {
                outer_spiky_polys.push(cont.clone());
                continue;
            }
            let orient = vsdf.contour_orient_sign(&ring);
            let mut tang = Vec::with_capacity(n);
            let mut nrm = Vec::with_capacity(n);
            let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
            for i in 0..n {
                min_x = min_x.min(ring[i].0);
                min_y = min_y.min(ring[i].1);
                max_x = max_x.max(ring[i].0);
                max_y = max_y.max(ring[i].1);
                let prev = ring[(i + n - 1) % n];
                let next = ring[(i + 1) % n];
                let tx = next.0 - prev.0;
                let ty = next.1 - prev.1;
                let tl = tx.hypot(ty).max(1e-9);
                let ux = tx / tl;
                let uy = ty / tl;
                tang.push((ux, uy));
                nrm.push((uy * orient, -ux * orient));
            }
            let nrm_s = smooth_vec(&nrm, 3);
            let cont_diag = (max_x - min_x).hypot(max_y - min_y);
            let size_scale = ((cont_diag - 16.0) / 96.0).clamp(0.24, 1.0);
            let a_c = amp * size_scale;
            let max_dip_c = max_dip * (size_scale * 1.15).clamp(0.35, 1.0);

            let base_pts: PolyRing = if hollow {
                let raw_off: Vec<f64> = (0..n)
                    .map(|i| {
                        let clr_g = vsdf.probe_ray(ring[i], nrm_s[i], 2.0, 64.0);
                        hw_out.min((clr_g * 0.44).max(1.8))
                    })
                    .collect();
                let off = smooth_scalar(&raw_off, 2);
                (0..n).map(|i| (ring[i].0 + nrm_s[i].0 * off[i], ring[i].1 + nrm_s[i].1 * off[i])).collect()
            } else {
                ring
            };

            let mut arc = Vec::with_capacity(n);
            let mut total = 0.0_f64;
            for i in 0..n {
                arc.push(total);
                let a = base_pts[i];
                let b = base_pts[(i + 1) % n];
                total += (b.0 - a.0).hypot(b.1 - a.1);
            }
            total = total.max(1e-6);

            let rctx = RingContext { base_pts: &base_pts, nrm_s: &nrm_s, tang: &tang, arc: &arc, total, a_c, max_dip_c, hollow, ci, seed };
            let spiky = match mode {
                SfxSugomiDirection::Fude => {
                    let clear_d: Vec<f64> = (0..n)
                        .map(|i| {
                            let raw = vsdf.probe_ray(base_pts[i], nrm_s[i], 2.5, 96.0);
                            if hollow { (raw - hw_out).max(0.0) } else { raw }
                        })
                        .collect();
                    let ink_d: Vec<f64> = (0..n)
                        .map(|i| vsdf.probe_ray(base_pts[i], (-nrm_s[i].0, -nrm_s[i].1), 2.0, 96.0))
                        .collect();
                    build_fude_ring(&rctx, &clear_d, &ink_d)
                }
                SfxSugomiDirection::Tate | SfxSugomiDirection::Yoko => build_comb_ring(mode, &rctx, vsdf, hw_out),
            };
            outer_spiky_polys.push(spiky);
        }
    }

    let inner_teeth_polys = if hollow {
        build_inner_hollow_teeth(groups, vsdf, hw_in, a_inner, band_w, seed.wrapping_add(71))
    } else {
        Vec::new()
    };

    SugomiVectorGeometry {
        outer_spiky_polys,
        hole_jagged_polys,
        inner_teeth_polys,
        stroke_w: stroke_w as f32,
        band_w: band_w as f32,
        hw_in: hw_in as f32,
    }
}

/// Full CPU oracle render for Style 2 (`Sugomi v3`, rev 5).
pub fn render_sugomi_v3_cpu(geom: &PreparedSfx, face_cov: &[f32], bounds: Rect, glyph_em_px: f32, le: &SfxLineEffect) -> Vec<f32> {
    let w = bounds.width() as usize;
    let h = bounds.height() as usize;
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let sg = build_sugomi_v3_vector(geom, glyph_em_px, le);
    let mut cov = rasterize_polygons_max(&sg.outer_spiky_polys, bounds);

    // Interior cutouts: hollow torn body + jagged counter holes
    let mut cut_cov = vec![0.0_f32; w * h];
    if le.sugomi_hollow {
        let glyph_sdf = exact_sdf(face_cov, w, h);
        let inner_sdf: Vec<f32> = glyph_sdf.into_iter().map(|d| d + sg.hw_in).collect();
        let eff_inner = perturb_torn_sdf_doc(&inner_sdf, bounds, sg.band_w, le.sugomi_seed.wrapping_add(53), 0.82, true);
        for (dst, d) in cut_cov.iter_mut().zip(eff_inner) {
            *dst = dst.max((0.5 - d).clamp(0.0, 1.0));
        }
    }
    if !sg.hole_jagged_polys.is_empty() {
        let hole_cov = rasterize_polygons_max(&sg.hole_jagged_polys, bounds);
        for (dst, hc) in cut_cov.iter_mut().zip(hole_cov) {
            if hc > *dst {
                *dst = hc;
            }
        }
    }
    for (c, cut) in cov.iter_mut().zip(cut_cov) {
        *c = (*c - cut).clamp(0.0, 1.0);
    }
    if !sg.inner_teeth_polys.is_empty() {
        let teeth_cov = rasterize_polygons_max(&sg.inner_teeth_polys, bounds);
        for (c, tc) in cov.iter_mut().zip(teeth_cov) {
            if tc > *c {
                *c = tc;
            }
        }
    }

    // Single unified kasure pass over the ENTIRE combined mask (zero body/spike junction seam)
    let grain_seed = le.sugomi_seed.wrapping_add(37);
    match le.sugomi_edge_mode {
        SfxV3EdgeMode::OneBit => {
            let grain_pct = if le.line_grain_pct > 0.5 { le.line_grain_pct } else { 18.0 };
            apply_edge_kasure_breakup_doc(
                &mut cov,
                bounds,
                EdgeKasureParams {
                    dryness_pct: grain_pct,
                    sharpness_pct: 100.0,
                    seed: grain_seed,
                    core_protect_px: 1.6,
                    one_bit: true,
                },
            );
        }
        SfxV3EdgeMode::Rough => {
            let dry = le.sugomi_dryness_pct.max(le.line_grain_pct);
            apply_edge_kasure_breakup_doc(
                &mut cov,
                bounds,
                EdgeKasureParams {
                    dryness_pct: dry,
                    sharpness_pct: le.sugomi_sharpness_pct,
                    seed: grain_seed,
                    core_protect_px: 2.2,
                    one_bit: false,
                },
            );
        }
    }
    cov
}

#[cfg(test)]
mod tests {
    use photocraft_doc::sfx::{SfxLineEffect, SfxLineStyle, SfxSugomiDirection};

    use super::{build_sugomi_v3_from_groups, compute_sugomi_band_w, concave_dip_damp};
    use crate::sfx_v3_common::VectorGlyphSdf;

    #[test]
    fn thin_font_sugomi_hollow_enforces_band_w_le_055_stroke_w() {
        // Thin L-shaped stroke of width 10 px on a 120 px tall glyph.
        let thin_l = vec![
            (10.0, 10.0),
            (20.0, 10.0),
            (20.0, 110.0),
            (90.0, 110.0),
            (90.0, 120.0),
            (10.0, 120.0),
        ];
        let groups = vec![vec![thin_l]];
        let vsdf = VectorGlyphSdf::from_contour_groups(&groups);
        let mut le = SfxLineEffect::default();
        le.style = SfxLineStyle::SugomiV3;
        le.sugomi_direction = SfxSugomiDirection::Fude;
        le.sugomi_hollow = true;

        let sg = build_sugomi_v3_from_groups(&groups, &vsdf, 120.0, &le);
        assert!(
            sg.band_w <= sg.stroke_w * 0.55 + 1e-4,
            "thin font hollow band_w ({}) exceeded 0.55 * stroke_w ({})",
            sg.band_w,
            sg.stroke_w * 0.55
        );
        // Also verify formula directly on ultra-thin stroke_w = 8 px.
        assert!(compute_sugomi_band_w(8.0, 120.0) <= 8.0 * 0.55 + 1e-6);
    }

    #[test]
    fn concave_bend_guard_damps_dip_when_turn_val_below_neg_0_2() {
        assert!((concave_dip_damp(0.5) - 1.0).abs() < 1e-9);
        assert!((concave_dip_damp(-0.2) - 1.0).abs() < 1e-9);
        assert!(concave_dip_damp(-0.5) < 0.65);
        assert!(concave_dip_damp(-0.9) <= 0.16);
        assert!(concave_dip_damp(-0.9) >= 0.15);

        // Build a sharp concave L-corner glyph and verify Fude ring emits finite non-empty vertices.
        let l_shape = vec![
            (0.0, 0.0),
            (36.0, 0.0),
            (36.0, 84.0),
            (110.0, 84.0),
            (110.0, 120.0),
            (0.0, 120.0),
        ];
        let groups = vec![vec![l_shape]];
        let vsdf = VectorGlyphSdf::from_contour_groups(&groups);
        let mut le = SfxLineEffect::default();
        le.style = SfxLineStyle::SugomiV3;
        le.sugomi_direction = SfxSugomiDirection::Fude;
        le.sugomi_hollow = false;
        let sg = build_sugomi_v3_from_groups(&groups, &vsdf, 120.0, &le);
        assert!(!sg.outer_spiky_polys.is_empty());
        assert!(sg.outer_spiky_polys[0].iter().all(|p| p.0.is_finite() && p.1.is_finite()));
    }
}
