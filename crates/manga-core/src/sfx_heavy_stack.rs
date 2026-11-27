//! Style 3 — 重みベタ / FGSFDS (`Heavy Stacked Brush v3`, rev 3) 1:1 port of `heavy_stack_v3.py`.
//!
//! 1. Four modes (`Fude`, `FgsfdsStack`, `Kin`, `FgsfdsFence`) built strictly with NaN-safe
//!    quad-strip [`RibbonStrip`] geometry (zero centroid-fan chord artifacts on arcs/loops).
//! 2. Adjustable focal wall edge bites (`edge_bite_pct: 0..=100%`) windowed strictly to
//!    `t ∈ [0.16, 0.84]` on long safe walls (`min_wall = 0.24 * glyph_h`); corners, flat caps,
//!    T-junctions, and small dots (`!`, `゛`, `diag < 0.38 * glyph_h`) are 100% immune.
//! 3. Strict composition order: subtract `edge_bite` from `cov_body` BEFORE unioning (`max`)
//!    tangential `tails` / curved `flicks`, then subtract `slits`, then single unified
//!    `apply_edge_kasure_breakup_doc` (`OneBit` `core_protect_px = 1.4` / `Rough` `2.0`).

use std::f64::consts::PI;

use photocraft_doc::sfx::{SfxHeavyStackMode, SfxLineEffect, SfxV3EdgeMode};
use photocraft_geom::Rect;

use crate::sfx_geom::PreparedSfx;
use crate::sfx_v3_common::{
    EdgeKasureParams, PolyRing, RibbonStrip, VectorGlyphSdf, apply_edge_kasure_breakup_doc, build_tapered_ribbon_strip,
    classify_glyph_contours, clean_closed_ring, contour_aabb, exact_sdf, h1, polyline_resample, rasterize_polygons_max,
    rasterize_ribbons_max, resample_open, rot2, smoothstep, value_noise_1d, value_noise_2d,
};

const DEG: f64 = PI / 180.0;

/// Extracted vector geometry for `Heavy Stack v3`, shared between CPU oracle and GPU renderer.
#[derive(Clone, Debug, Default)]
pub struct HeavyStackVectorGeometry {
    /// Stacked stem / arc / `Kin` loop / `Fence` bar quad-strip ribbons (`BlendOperation::Max`).
    /// Empty for `Fude` (whose body comes from `Face` SDF perturbation).
    pub body_ribbons: Vec<RibbonStrip>,
    /// Focal dry-ink edge bite pentagons (subtracted from body BEFORE unioning `tail_ribbons`).
    pub bite_polys: Vec<PolyRing>,
    /// Tangential tails (`Fude`) or outer curve flicks (`FgsfdsStack`), immune to `edge_bite`.
    pub tail_ribbons: Vec<RibbonStrip>,
    /// Interior dry slits subtracted AFTER unioning `tail_ribbons`.
    pub slit_ribbons: Vec<RibbonStrip>,
    /// Estimated glyph stroke width in document pixels (scales `Fude` SDF perturbation).
    pub stroke_w: f32,
}

struct Chain {
    pts: Vec<(f64, f64)>,
    nrm: Vec<(f64, f64)>,
    is_straight: bool,
    closed_loop: bool,
    corner_start: bool,
    corner_end: bool,
    convex_start: bool,
    convex_end: bool,
    alen: f64,
}

fn split_outer_hole_groups(groups: &[Vec<PolyRing>]) -> (Vec<PolyRing>, Vec<PolyRing>) {
    let mut outers = Vec::new();
    let mut holes = Vec::new();
    for cs in groups {
        if cs.is_empty() {
            continue;
        }
        let classes = classify_glyph_contours(cs);
        for (c, cls) in cs.iter().zip(classes) {
            if c.len() < 3 {
                continue;
            }
            if cls.is_hole {
                holes.push(c.clone());
            } else {
                outers.push(c.clone());
            }
        }
    }
    (outers, holes)
}

fn estimate_glyph_h_and_top(groups: &[Vec<PolyRing>]) -> (f64, f64) {
    let mut ymin = f64::INFINITY;
    let mut ymax = f64::NEG_INFINITY;
    for cs in groups {
        for c in cs {
            for &(_, y) in c {
                if y.is_finite() {
                    ymin = ymin.min(y);
                    ymax = ymax.max(y);
                }
            }
        }
    }
    if !ymin.is_finite() || !ymax.is_finite() {
        return (150.0, 0.0);
    }
    ((ymax - ymin).max(32.0), ymin)
}

fn contour_diag(cont: &[(f64, f64)]) -> f64 {
    let bb = contour_aabb(cont);
    (bb[2] - bb[0]).hypot(bb[3] - bb[1])
}

fn median_f64(vals: &[f64]) -> f64 {
    if vals.is_empty() {
        return 24.0;
    }
    let mut sorted = vals.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 { sorted[mid] } else { 0.5 * (sorted[mid - 1] + sorted[mid]) }
}

fn probe_ink_vec(pts: &[(f64, f64)], nrm: &[(f64, f64)], vsdf: &VectorGlyphSdf, smax: f64) -> Vec<f64> {
    pts.iter()
        .zip(nrm.iter())
        .map(|(&p, &n)| vsdf.probe_ink_w(p, (-n.0, -n.1), smax, 2.0))
        .collect()
}

fn compute_open_normals(pts: &[(f64, f64)], orient: f64) -> Vec<(f64, f64)> {
    let m = pts.len();
    let mut out = Vec::with_capacity(m);
    for i in 0..m {
        let (tx, ty) = if i == 0 {
            (pts[1].0 - pts[0].0, pts[1].1 - pts[0].1)
        } else if i + 1 == m {
            (pts[m - 1].0 - pts[m - 2].0, pts[m - 1].1 - pts[m - 2].1)
        } else {
            (pts[i + 1].0 - pts[i - 1].0, pts[i + 1].1 - pts[i - 1].1)
        };
        let tl = tx.hypot(ty).max(1e-9);
        out.push(((ty / tl) * orient, (-tx / tl) * orient));
    }
    out
}

fn split_contour_chains(cont: &[(f64, f64)], vsdf: &VectorGlyphSdf) -> Vec<Chain> {
    let raw = clean_closed_ring(cont, 0.7, 0.9994);
    let n = raw.len();
    if n < 3 {
        return Vec::new();
    }
    let orient = vsdf.contour_orient_sign(&raw);
    let mut el = Vec::with_capacity(n);
    let mut t_e = Vec::with_capacity(n);
    let mut n_e = Vec::with_capacity(n);
    for i in 0..n {
        let a = raw[i];
        let b = raw[(i + 1) % n];
        let ex = b.0 - a.0;
        let ey = b.1 - a.1;
        let l = ex.hypot(ey).max(1e-9);
        el.push(l);
        let tx = ex / l;
        let ty = ey / l;
        t_e.push((tx, ty));
        n_e.push((ty * orient, -tx * orient));
    }

    let mut is_corner = vec![false; n];
    let mut turn_sign = vec![0.0_f64; n];
    let mut split_mask = vec![false; n];
    for i in 0..n {
        let prev = (i + n - 1) % n;
        let dot = (t_e[prev].0 * t_e[i].0 + t_e[prev].1 * t_e[i].1).clamp(-1.0, 1.0);
        let ang = dot.acos();
        turn_sign[i] = n_e[i].0 * t_e[prev].0 + n_e[i].1 * t_e[prev].1;
        is_corner[i] = ang > 26.0 * DEG;
        split_mask[i] = is_corner[i] || ((el[i] > 12.0) != (el[prev] > 12.0) && ang > 4.0 * DEG);
    }

    let idxs: Vec<usize> = (0..n).filter(|&i| split_mask[i]).collect();
    if idxs.is_empty() {
        let ring = polyline_resample(&raw, 2.5);
        let m = ring.len();
        if m < 3 {
            return Vec::new();
        }
        let mut nrm = Vec::with_capacity(m);
        for i in 0..m {
            let prev = ring[(i + m - 1) % m];
            let next = ring[(i + 1) % m];
            let tx = next.0 - prev.0;
            let ty = next.1 - prev.1;
            let tl = tx.hypot(ty).max(1e-9);
            nrm.push(((ty / tl) * orient, (-tx / tl) * orient));
        }
        for _ in 0..2 {
            let mut sm = Vec::with_capacity(m);
            for i in 0..m {
                let a = nrm[(i + m - 1) % m];
                let b = nrm[i];
                let c = nrm[(i + 1) % m];
                let nx = (a.0 + 2.0 * b.0 + c.0) * 0.25;
                let ny = (a.1 + 2.0 * b.1 + c.1) * 0.25;
                let nl = nx.hypot(ny).max(1e-9);
                sm.push((nx / nl, ny / nl));
            }
            nrm = sm;
        }
        return vec![Chain {
            alen: (m as f64) * 2.5,
            pts: ring,
            nrm,
            is_straight: false,
            closed_loop: true,
            corner_start: false,
            corner_end: false,
            convex_start: false,
            convex_end: false,
        }];
    }

    let mut chains = Vec::with_capacity(idxs.len());
    for ci in 0..idxs.len() {
        let i0 = idxs[ci];
        let i1 = idxs[(ci + 1) % idxs.len()];
        let mut seq = Vec::new();
        let mut j = i0;
        loop {
            seq.push(raw[j]);
            if j == i1 && seq.len() > 1 {
                break;
            }
            j = (j + 1) % n;
            if seq.len() > n + 2 {
                break;
            }
        }
        let Some(&first) = seq.first() else { continue };
        let Some(&last) = seq.last() else { continue };
        let chord = (last.0 - first.0).hypot(last.1 - first.1);
        let alen: f64 = seq.windows(2).map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1)).sum();
        if alen < 3.0 {
            continue;
        }
        let is_str = (chord / alen.max(1e-6)) > 0.988;
        let pts = resample_open(&seq, 2.5);
        if pts.len() < 2 {
            continue;
        }
        let nrm = compute_open_normals(&pts, orient);
        chains.push(Chain {
            pts,
            nrm,
            is_straight: is_str,
            closed_loop: false,
            corner_start: is_corner[i0],
            corner_end: is_corner[i1],
            convex_start: is_corner[i0] && turn_sign[i0] > 0.08,
            convex_end: is_corner[i1] && turn_sign[i1] > 0.08,
            alen,
        });
    }
    chains
}

fn find_arc_idx(arc: &[f64], s: f64) -> usize {
    let m = arc.len();
    if m <= 2 {
        return 0;
    }
    let pos = arc.partition_point(|&v| v <= s);
    pos.saturating_sub(1).min(m - 2)
}

// =============================================================================
// 1. FOCAL WALL EDGE BITES (`edge_bite_pct: 0..=100%`, rev 3b)
// =============================================================================

struct BiteCtx<'a> {
    vsdf: &'a VectorGlyphSdf,
    stroke_w: f64,
    glyph_h: f64,
    bite: f64,
    seed: u32,
    is_kin: bool,
    band_w: f64,
}

fn build_edge_bite_polys(outers: &[PolyRing], holes: &[PolyRing], ctx: &BiteCtx<'_>) -> Vec<PolyRing> {
    if ctx.bite <= 1e-4 {
        return Vec::new();
    }
    let min_wall = 0.24 * ctx.glyph_h;
    let mut bite_polys = Vec::new();
    let mut ci = 0u32;

    for (is_outer, list) in [(true, outers), (false, holes)] {
        for cont in list {
            ci = ci.wrapping_add(1);
            if is_outer && contour_diag(cont) < 0.38 * ctx.glyph_h {
                continue; // ! dot and dakuten ゛ are strictly immune
            }
            let chains = split_contour_chains(cont, ctx.vsdf);
            for (chi, ch) in chains.iter().enumerate() {
                emit_chain_bites(ch, is_outer, ci, chi as u32, min_wall, ctx, &mut bite_polys);
            }
        }
    }
    bite_polys
}

fn emit_chain_bites(
    ch: &Chain,
    is_outer: bool,
    ci: u32,
    chi: u32,
    min_wall: f64,
    ctx: &BiteCtx<'_>,
    out: &mut Vec<PolyRing>,
) {
    let m = ch.pts.len();
    if m < 6 || ch.alen < min_wall {
        return;
    }
    let ink_w = probe_ink_vec(&ch.pts, &ch.nrm, ctx.vsdf, ctx.stroke_w * 2.8);
    let raw_med = median_f64(&ink_w);
    // Flat foot / top caps are strictly immune
    if ch.is_straight && raw_med > ctx.stroke_w * 1.65 {
        return;
    }
    let mut arc = Vec::with_capacity(m);
    arc.push(0.0_f64);
    for w in ch.pts.windows(2) {
        let prev = *arc.last().unwrap_or(&0.0);
        arc.push(prev + (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1));
    }
    let total = arc.last().copied().unwrap_or(1.0).max(1.0);
    let b_seed = ctx
        .seed
        .wrapping_mul(7321)
        .wrapping_add(ci.wrapping_mul(223))
        .wrapping_add(chi.wrapping_mul(67))
        .wrapping_add(if is_outer { 0 } else { 5000 });

    let gate_off = h1(b_seed.wrapping_add(3)) * 35.0;
    let gate_seed = b_seed.wrapping_add(19);
    let mut base_cap = if ctx.is_kin { 0.42 * ctx.band_w } else { 0.30 * ctx.stroke_w };
    if !is_outer {
        base_cap *= 0.45;
    }
    let edge_shift = if ctx.is_kin {
        if is_outer { ctx.band_w * 0.62 + 1.5 } else { ctx.band_w * 0.18 + 1.0 }
    } else {
        0.0
    };
    let out_ext = if ctx.is_kin { 4.5 } else { 3.2 };

    let mut s_cur = 5.0 + 6.0 * h1(b_seed.wrapping_add(7));
    let mut k = 0u32;
    while s_cur < total - 5.0 {
        let ks = b_seed.wrapping_add(140).wrapping_add(k.wrapping_mul(37));
        k = k.wrapping_add(1);
        let t_param = s_cur / total;
        let step = 11.0 + 11.0 * h1(ks.wrapping_add(1));
        let win = if !ch.closed_loop {
            if t_param <= 0.16 || t_param >= 0.84 {
                s_cur += step;
                continue;
            }
            smoothstep(0.16, 0.23, t_param) * (1.0 - smoothstep(0.77, 0.84, t_param))
        } else {
            1.0
        };
        let idx = find_arc_idx(&arc, s_cur);
        let g_val = f64::from(value_noise_1d((arc[idx] * 0.022 + gate_off) as f32, gate_seed));
        let thr = 0.57 - 0.10 * ctx.bite;
        if g_val <= thr || win < 0.08 {
            s_cur += step;
            continue;
        }
        let loc_ink = ink_w[idx];
        let max_d = if ctx.is_kin {
            base_cap
        } else {
            base_cap.min(loc_ink * if is_outer { 0.34 } else { 0.16 })
        };
        let eff_s = ctx.bite.powf(0.82) * win;
        let patch_depth = max_d * eff_s * (0.68 + 0.32 * h1(ks.wrapping_add(2)));
        if patch_depth < 1.8 {
            s_cur += step;
            continue;
        }
        let patch_w = (6.5 + 8.5 * h1(ks.wrapping_add(3))) * (0.72 + 0.40 * ctx.bite);
        let n_sub = 2usize + usize::from(h1(ks.wrapping_add(4)) > 0.38);
        let ob_base = (h1(ks.wrapping_add(5)) - 0.5) * (54.0 * DEG);

        for si in 0..n_sub {
            let ss = ks.wrapping_add(60).wrapping_add((si as u32).wrapping_mul(19));
            let rel_s = (((si as f64) + 0.5) / (n_sub as f64) - 0.5) * patch_w;
            let s_q = (s_cur + rel_s).clamp(2.0, total - 2.0);
            if !ch.closed_loop {
                let t_q = s_q / total;
                if t_q <= 0.16 || t_q >= 0.84 {
                    continue;
                }
            }
            let qi = find_arc_idx(&arc, s_q);
            let nv = ch.nrm[qi];
            let p_e = (ch.pts[qi].0 + nv.0 * edge_shift, ch.pts[qi].1 + nv.1 * edge_shift);
            let p_prev = ch.pts[qi.saturating_sub(1)];
            let p_next = ch.pts[(qi + 1).min(m - 1)];
            let tx = p_next.0 - p_prev.0;
            let ty = p_next.1 - p_prev.1;
            let tl = tx.hypot(ty).max(1e-9);
            let tv = (tx / tl, ty / tl);

            let sub_d = patch_depth * (0.62 + 0.38 * h1(ss.wrapping_add(1)));
            let sub_hw = ((patch_w / n_sub as f64) * (0.65 + 0.35 * h1(ss.wrapping_add(2)))).max(2.2);
            let in_dir = rot2((-nv.0, -nv.1), ob_base + (h1(ss.wrapping_add(3)) - 0.5) * (20.0 * DEG));
            let tip_t = (h1(ss.wrapping_add(4)) - 0.5) * sub_hw * 0.65;

            let p_ol = (p_e.0 + nv.0 * out_ext - tv.0 * (sub_hw * 1.15), p_e.1 + nv.1 * out_ext - tv.1 * (sub_hw * 1.15));
            let p_or = (p_e.0 + nv.0 * out_ext + tv.0 * (sub_hw * 1.15), p_e.1 + nv.1 * out_ext + tv.1 * (sub_hw * 1.15));
            let p_ml = (p_e.0 + in_dir.0 * (sub_d * 0.48) - tv.0 * (sub_hw * 0.58), p_e.1 + in_dir.1 * (sub_d * 0.48) - tv.1 * (sub_hw * 0.58));
            let p_mr = (p_e.0 + in_dir.0 * (sub_d * 0.48) + tv.0 * (sub_hw * 0.58), p_e.1 + in_dir.1 * (sub_d * 0.48) + tv.1 * (sub_hw * 0.58));
            let p_tip = (p_e.0 + in_dir.0 * sub_d + tv.0 * tip_t, p_e.1 + in_dir.1 * sub_d + tv.1 * tip_t);
            out.push(vec![p_ol, p_or, p_mr, p_tip, p_ml]);
        }
        s_cur += step;
    }
}

// =============================================================================
// 2. COMPACT BLOB STROKES (! dot, ゴ dakuten ゛)
// =============================================================================

fn principal_axes_2d(cont: &[(f64, f64)]) -> ((f64, f64), (f64, f64), (f64, f64), f64, f64) {
    let n = cont.len().max(1) as f64;
    let cx = cont.iter().map(|p| p.0).sum::<f64>() / n;
    let cy = cont.iter().map(|p| p.1).sum::<f64>() / n;
    let mut sxx = 0.0;
    let mut syy = 0.0;
    let mut sxy = 0.0;
    for &(x, y) in cont {
        let dx = x - cx;
        let dy = y - cy;
        sxx += dx * dx;
        syy += dy * dy;
        sxy += dx * dy;
    }
    let ang = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let (sa, ca) = ang.sin_cos();
    let mut maj_v = (ca, sa);
    let mut min_v = (-sa, ca);
    let mut maj_r = 3.5_f64;
    let mut min_r = 3.5_f64;
    for &(x, y) in cont {
        let dx = x - cx;
        let dy = y - cy;
        maj_r = maj_r.max((dx * maj_v.0 + dy * maj_v.1).abs());
        min_r = min_r.max((dx * min_v.0 + dy * min_v.1).abs());
    }
    if maj_r < min_r {
        std::mem::swap(&mut maj_v, &mut min_v);
        std::mem::swap(&mut maj_r, &mut min_r);
    }
    if maj_r / min_r < 1.25 {
        maj_v = (0.32_f64.cos(), 0.32_f64.sin());
        min_v = (-maj_v.1, maj_v.0);
    }
    ((cx, cy), maj_v, min_v, maj_r, min_r)
}

fn emit_compact_blob_strokes(cont: &[(f64, f64)], vsdf: &VectorGlyphSdf, seed_b: u32, intensity: f64) -> Vec<RibbonStrip> {
    let (center, maj_v, min_v, maj_r, min_r) = principal_axes_2d(cont);
    let mut out = Vec::new();
    let n_fib = 9 + (3.0 * intensity).round() as usize;
    for j in 0..n_fib {
        let fs = seed_b.wrapping_add(17).wrapping_add((j as u32).wrapping_mul(23));
        let lat = (((j as f64) + 0.25 + 0.50 * h1(fs.wrapping_add(1))) / (n_fib as f64) - 0.5) * 1.82 * min_r;
        let ang = (h1(fs.wrapping_add(2)) - 0.5) * (18.0 * DEG);
        let dv = rot2(maj_v, ang);
        let anchor = (center.0 + min_v.0 * lat, center.1 + min_v.1 * lat);
        let Some((p0, p1, _, _)) = vsdf.clip_line_to_glyph(anchor, dv, maj_r * 1.5, 0.6) else {
            continue;
        };
        let os_a = (0.8 + 3.2 * h1(fs.wrapping_add(3))) * (0.5 + 0.6 * intensity);
        let os_b = (0.8 + 3.2 * h1(fs.wrapping_add(4))) * (0.5 + 0.6 * intensity);
        let cpts = resample_open(
            &[(p0.0 - dv.0 * os_a, p0.1 - dv.1 * os_a), (p1.0 + dv.0 * os_b, p1.1 + dv.1 * os_b)],
            2.2,
        );
        let hw = if j < 5 { (min_r * 0.24).max(1.5) } else { (1.0 + 0.9 * h1(fs.wrapping_add(5))).max(1.5) };
        let m = cpts.len();
        let hws: Vec<f64> = (0..m)
            .map(|k| {
                let u = if m > 1 { k as f64 / (m - 1) as f64 } else { 0.5 };
                hw * (0.38 + 0.62 * (u * PI).sin().max(0.0).powf(0.42))
            })
            .collect();
        if let Some(strip) = build_tapered_ribbon_strip(&cpts, &hws) {
            out.push(strip);
        }
    }
    out
}

// =============================================================================
// 3. MODE 1: 重みベタ筆 (`Fude`)
// =============================================================================

/// 2D SDF perturbation for `HeavyStackV3::Fude` in **document coordinates**
/// (1:1 identical to `sfx.wgsl:fs_heavy_stack_perturb`).
pub fn perturb_heavy_sdf_doc(sdf: &[f32], bounds: Rect, scale_px: f32, seed: u32, amp_mul: f32) -> Vec<f32> {
    let w = bounds.width() as usize;
    let h = bounds.height() as usize;
    if w == 0 || h == 0 || sdf.len() < w * h {
        return sdf.to_vec();
    }
    let sc = scale_px.max(8.0);
    let inv = 1.0 / sc;
    let s_mul = seed.wrapping_mul(139);
    let rn = |xs: f32, ys: f32, ang: f32, fm: f32, off: u32| -> f32 {
        let (sa, ca) = ang.sin_cos();
        let f = fm * inv;
        value_noise_2d((xs * ca - ys * sa) * f, (xs * sa + ys * ca) * f, s_mul.wrapping_add(off))
    };
    let lo = -sc * 0.09;
    let hi = sc * 0.11;
    let mut out = vec![0.0_f32; w * h];
    for y in 0..h {
        let ys = (bounds.y0 as f32) + (y as f32) + 0.5;
        let row = y * w;
        for x in 0..w {
            let xs = (bounds.x0 as f32) + (x as f32) + 0.5;
            let n1 = (rn(xs, ys, 0.33, 0.22, 11) - 0.5) * 2.0;
            let n2 = (rn(xs, ys, 1.21, 0.68, 37) - 0.5) * 2.0;
            let n3 = (rn(xs, ys, 2.19, 1.60, 73) - 0.5) * 2.0;
            let disp = ((n1 * 0.065 + n2 * 0.048 + n3 * 0.028) * sc * amp_mul).clamp(lo, hi);
            out[row + x] = sdf[row + x] - disp;
        }
    }
    out
}

fn emit_fude_tails(
    pt: (f64, f64),
    dir_v: (f64, f64),
    out_n: (f64, f64),
    loc_w: f64,
    glyph_h: f64,
    intensity: f64,
    t_seed: u32,
    len_mul: f64,
    tails: &mut Vec<RibbonStrip>,
) {
    let cnt = 2 + (2.0 * intensity).round() as usize;
    for j in 0..cnt {
        let ts = t_seed.wrapping_add(25).wrapping_add((j as u32).wrapping_mul(19));
        let rank = if j == 0 { 1.0 } else { 0.38 + 0.45 * h1(ts.wrapping_add(1)) };
        let ang = (h1(ts.wrapping_add(2)) - 0.5) * (4.5 * DEG);
        let dv = rot2(dir_v, ang);
        let in_off = loc_w * (0.08 + 0.34 * h1(ts.wrapping_add(3)));
        let t_len = (glyph_h * (0.04 + 0.11 * intensity) * rank * (0.72 + 0.45 * h1(ts.wrapping_add(4))) * len_mul).clamp(4.5, 34.0);
        let p0 = (pt.0 - out_n.0 * in_off - dv.0 * 10.0, pt.1 - out_n.1 * in_off - dv.1 * 10.0);
        let p1 = (pt.0 - out_n.0 * in_off + dv.0 * (t_len * 0.45), pt.1 - out_n.1 * in_off + dv.1 * (t_len * 0.45));
        let p2 = (pt.0 - out_n.0 * (in_off * 0.85) + dv.0 * t_len, pt.1 - out_n.1 * (in_off * 0.85) + dv.1 * t_len);
        let cpts = resample_open(&[p0, p1, p2], 2.5);
        let m = cpts.len();
        let hw = (1.4 + 1.5 * h1(ts.wrapping_add(5))).max(1.25);
        let hws: Vec<f64> = (0..m)
            .map(|k| {
                let u = if m > 1 { k as f64 / (m - 1) as f64 } else { 0.5 };
                let factor = if u < 0.28 { 0.90 } else { ((1.0 - u) / 0.72).max(0.0).powf(0.85) };
                (hw * factor).max(0.22)
            })
            .collect();
        if let Some(strip) = build_tapered_ribbon_strip(&cpts, &hws) {
            tails.push(strip);
        }
    }
}

fn emit_fude_small_dot(cont: &[(f64, f64)], ds: u32, tails: &mut Vec<RibbonStrip>, slits: &mut Vec<RibbonStrip>) {
    let bb = contour_aabb(cont);
    let n = cont.len().max(1) as f64;
    let cx = cont.iter().map(|p| p.0).sum::<f64>() / n;
    let cy = cont.iter().map(|p| p.1).sum::<f64>() / n;
    let rx = (0.5 * (bb[2] - bb[0])).max(3.5);
    let ry = (0.5 * (bb[3] - bb[1])).max(3.5);
    for k in 0..2u32 {
        let ks = ds.wrapping_add(k.wrapping_mul(29));
        let ang = 0.35 + f64::from(k) * 2.9 + (h1(ks.wrapping_add(1)) - 0.5) * 0.8;
        let (sa, ca) = ang.sin_cos();
        let px = cx + ca * rx * 0.92;
        let py = cy + sa * ry * 0.92;
        let tv = (-sa, ca);
        let nv = (ca, sa);
        let p_a = (px - tv.0 * (rx * 0.55) - nv.0 * 1.2, py - tv.1 * (rx * 0.55) - nv.1 * 1.2);
        let ext = rx * (0.75 + 0.45 * h1(ks.wrapping_add(2)));
        let p_b = (px + tv.0 * ext + nv.0 * 1.6, py + tv.1 * ext + nv.1 * 1.6);
        let cpts = resample_open(&[p_a, p_b], 2.0);
        let m = cpts.len();
        let hw = 1.2 + 0.8 * h1(ks.wrapping_add(3));
        let hws: Vec<f64> = (0..m)
            .map(|i| {
                let u = if m > 1 { i as f64 / (m - 1) as f64 } else { 0.5 };
                hw * (u * PI).sin().max(0.0).powf(0.6) + 0.2
            })
            .collect();
        if let Some(strip) = build_tapered_ribbon_strip(&cpts, &hws) {
            tails.push(strip);
        }
    }
    let na = 1.9 + (h1(ds.wrapping_add(88)) - 0.5) * 0.9;
    let (ny, nx) = na.sin_cos();
    let p_n = (cx + nx * rx * 0.96, cy + ny * ry * 0.96);
    let tv_n = (-ny, nx);
    let sn_pts = resample_open(
        &[
            (p_n.0 + nx * 2.5 - tv_n.0 * 2.5, p_n.1 + ny * 2.5 - tv_n.1 * 2.5),
            (p_n.0 - nx * 3.8 + tv_n.0 * 2.5, p_n.1 - ny * 3.8 + tv_n.1 * 2.5),
        ],
        1.8,
    );
    let m = sn_pts.len();
    let hws: Vec<f64> = (0..m)
        .map(|i| {
            let u = if m > 1 { i as f64 / (m - 1) as f64 } else { 0.5 };
            1.15 * (1.0 - u) + 0.15
        })
        .collect();
    if let Some(strip) = build_tapered_ribbon_strip(&sn_pts, &hws) {
        slits.push(strip);
    }
}

fn build_fude_details(
    outers: &[PolyRing],
    holes: &[PolyRing],
    vsdf: &VectorGlyphSdf,
    stroke_w: f64,
    glyph_h: f64,
    y_top_glyph: f64,
    intensity: f64,
    seed: u32,
) -> (Vec<RibbonStrip>, Vec<RibbonStrip>) {
    let min_struct = 0.26 * glyph_h;
    let mut tails = Vec::new();
    let mut slits = Vec::new();
    let mut ci = 0u32;

    for (is_outer, list) in [(true, outers), (false, holes)] {
        for cont in list {
            ci = ci.wrapping_add(1);
            if is_outer && contour_diag(cont) < 0.38 * glyph_h {
                let ds = seed.wrapping_mul(5119).wrapping_add(ci.wrapping_mul(137));
                emit_fude_small_dot(cont, ds, &mut tails, &mut slits);
                continue;
            }
            let chains = split_contour_chains(cont, vsdf);
            for (chi, ch) in chains.iter().enumerate() {
                let m = ch.pts.len();
                if m < 3 {
                    continue;
                }
                let ink_w = probe_ink_vec(&ch.pts, &ch.nrm, vsdf, stroke_w * 2.8);
                let raw_med = median_f64(&ink_w);
                let is_flat_cap = ch.is_straight && ch.alen < min_struct && raw_med > stroke_w * 1.65;
                let loc_w = raw_med.min(stroke_w * 1.45);
                let cseed = seed
                    .wrapping_mul(3571)
                    .wrapping_add(ci.wrapping_mul(193))
                    .wrapping_add((chi as u32).wrapping_mul(47))
                    .wrapping_add(if is_outer { 0 } else { 8000 });

                if is_outer && !ch.closed_loop && ch.alen >= min_struct {
                    let p_s2 = ch.pts[2.min(m - 1)];
                    let t0x = ch.pts[0].0 - p_s2.0;
                    let t0y = ch.pts[0].1 - p_s2.1;
                    let l0 = t0x.hypot(t0y).max(1e-9);
                    let t0 = (t0x / l0, t0y / l0);
                    if ch.convex_start {
                        let is_apex = ch.pts[0].1 < y_top_glyph + 10.0 && t0.0.abs() > 0.18 && t0.1 < -0.65;
                        emit_fude_tails(ch.pts[0], t0, ch.nrm[0], loc_w, glyph_h, intensity, cseed.wrapping_add(11), if is_apex { 0.30 } else { 1.0 }, &mut tails);
                    }
                    let p_e2 = ch.pts[m.saturating_sub(3)];
                    let t1x = ch.pts[m - 1].0 - p_e2.0;
                    let t1y = ch.pts[m - 1].1 - p_e2.1;
                    let l1 = t1x.hypot(t1y).max(1e-9);
                    let t1 = (t1x / l1, t1y / l1);
                    if ch.convex_end {
                        let is_apex = ch.pts[m - 1].1 < y_top_glyph + 10.0 && t1.0.abs() > 0.18 && t1.1 < -0.65;
                        emit_fude_tails(ch.pts[m - 1], t1, ch.nrm[m - 1], loc_w, glyph_h, intensity, cseed.wrapping_add(77), if is_apex { 0.30 } else { 1.0 }, &mut tails);
                    }
                }

                if !is_flat_cap && (ch.alen >= min_struct || !ch.is_straight) && loc_w >= 13.0 {
                    emit_fude_slits(ch, &ink_w, stroke_w, intensity, cseed, &mut slits);
                }
            }
        }
    }
    (tails, slits)
}

fn emit_fude_slits(ch: &Chain, ink_w: &[f64], stroke_w: f64, intensity: f64, cseed: u32, slits: &mut Vec<RibbonStrip>) {
    let m = ch.pts.len();
    let n_slits = ((ch.alen / 38.0) * (0.45 + 1.35 * intensity)).round().max(1.0) as usize;
    for si in 0..n_slits {
        let ss = cseed.wrapping_add(700).wrapping_add((si as u32).wrapping_mul(41));
        if h1(ss.wrapping_add(1)) < 0.42 - 0.30 * intensity {
            continue;
        }
        let seg_f = ((16.0 + 44.0 * h1(ss.wrapping_add(2))) / ch.alen.max(1.0)).min(0.55);
        let u0 = 0.05 + (0.90 - seg_f) * h1(ss.wrapping_add(3));
        let u1 = (u0 + seg_f).min(0.95);
        let i0 = (u0 * (m - 1) as f64) as usize;
        let i1 = ((u1 * (m - 1) as f64) as usize).max(i0 + 3).min(m - 1);
        let len_sub = i1 - i0 + 1;
        let mut min_w = f64::INFINITY;
        for &w in &ink_w[i0..=i1] {
            min_w = min_w.min(w.min(stroke_w * 1.45));
        }
        if min_w < 11.0 {
            continue;
        }
        let f0 = 0.08 + 0.38 * h1(ss.wrapping_add(4));
        let f1 = (f0 + (h1(ss.wrapping_add(5)) - 0.5) * 0.10).clamp(0.06, 0.46);
        let base_hw = (0.55 + 0.80 * h1(ss.wrapping_add(6))) * (0.65 + 0.65 * intensity);
        let mut s_pts = Vec::with_capacity(len_sub);
        let mut shw = Vec::with_capacity(len_sub);
        for k in 0..len_sub {
            let idx = i0 + k;
            let u = if len_sub > 1 { k as f64 / (len_sub - 1) as f64 } else { 0.5 };
            let sw = ink_w[idx].min(stroke_w * 1.45);
            let off = sw * (f0 + (f1 - f0) * u);
            s_pts.push((ch.pts[idx].0 - ch.nrm[idx].0 * off, ch.pts[idx].1 - ch.nrm[idx].1 * off));
            shw.push(base_hw * (u * PI).sin().max(0.0).powf(0.85));
        }
        if let Some(strip) = build_tapered_ribbon_strip(&s_pts, &shw) {
            slits.push(strip);
        }
        if intensity > 0.55 && h1(ss.wrapping_add(8)) > 0.55 && s_pts.len() >= 5 {
            let tw_off = (2.0 + 1.8 * h1(ss.wrapping_add(9))) * if h1(ss.wrapping_add(10)) > 0.5 { 1.0 } else { -1.0 };
            let n2 = s_pts.len() - 2;
            let mut s2 = Vec::with_capacity(n2);
            let mut h2 = Vec::with_capacity(n2);
            let bw2 = 0.48 + 0.45 * h1(ss.wrapping_add(11));
            for k in 0..n2 {
                let idx = i0 + 1 + k;
                let u = if n2 > 1 { k as f64 / (n2 - 1) as f64 } else { 0.5 };
                s2.push((s_pts[k + 1].0 + ch.nrm[idx].0 * tw_off, s_pts[k + 1].1 + ch.nrm[idx].1 * tw_off));
                h2.push(bw2 * (u * PI).sin().max(0.0).powf(0.9));
            }
            if let Some(strip) = build_tapered_ribbon_strip(&s2, &h2) {
                slits.push(strip);
            }
        }
    }
}

// =============================================================================
// 4. MODE 2: FGSFDS STACK (`FgsfdsStack`)
// =============================================================================

struct StackCtx<'a> {
    vsdf: &'a VectorGlyphSdf,
    stroke_w: f64,
    glyph_h: f64,
    intensity: f64,
    min_struct: f64,
    min_bar: f64,
}

fn emit_straight_stem(
    ch: &Chain,
    ink_w: &[f64],
    is_outer: bool,
    b_seed: u32,
    ctx: &StackCtx<'_>,
    body: &mut Vec<RibbonStrip>,
    slits: &mut Vec<RibbonStrip>,
) {
    let p0 = ch.pts[0];
    let p1 = ch.pts[ch.pts.len() - 1];
    let vx = p1.0 - p0.0;
    let vy = p1.1 - p0.1;
    let vlen = vx.hypot(vy);
    if vlen < 3.5 {
        return;
    }
    let tv = (vx / vlen, vy / vlen);
    let mut nvx = 0.0;
    let mut nvy = 0.0;
    for &(nx, ny) in &ch.nrm {
        nvx += nx;
        nvy += ny;
    }
    let nl = nvx.hypot(nvy).max(1e-9);
    let nv = (nvx / nl, nvy / nl);
    let raw_med = median_f64(ink_w);
    let loc_w = raw_med.min(ctx.stroke_w * 1.48);
    let mid_wall = (0.5 * (p0.0 + p1.0), 0.5 * (p0.1 + p1.1));
    let max_reach = ctx.glyph_h * 1.15;

    if ch.alen < ctx.min_struct && raw_med > ctx.stroke_w * 1.65 {
        return;
    }
    let is_interior_bridge = !is_outer || (!ch.convex_start && !ch.convex_end);
    let is_full_stem = is_outer && !is_interior_bridge && ch.alen >= ctx.min_bar;

    let (allow_os_a, allow_os_b, n_core, n_thin, span_lo, span_hi, sdf_lim) = if is_full_stem {
        if ctx
            .vsdf
            .clip_line_to_glyph((mid_wall.0 - nv.0 * loc_w * 0.5, mid_wall.1 - nv.1 * loc_w * 0.5), tv, max_reach, 0.4)
            .is_none()
        {
            return;
        }
        (
            ch.corner_start && (ch.convex_start || ch.alen >= ctx.min_struct),
            ch.corner_end && (ch.convex_end || ch.alen >= ctx.min_struct),
            5usize,
            5 + (3.0 * ctx.intensity).round() as usize,
            0.05_f64,
            0.95_f64,
            0.4_f64,
        )
    } else {
        (false, false, 3usize, 3usize, 0.08_f64, 0.56_f64, -2.0_f64)
    };

    let n_tot = n_core + n_thin;
    for j in 0..n_tot {
        let ss = b_seed.wrapping_add(37).wrapping_add((j as u32).wrapping_mul(29));
        let is_core = j < n_core;
        let (in_d, hw_base) = if is_core {
            let t_d = ((j as f64) + 0.20 + 0.60 * h1(ss.wrapping_add(1))) / (n_core.max(1) as f64);
            let hw = (loc_w * (0.072 + 0.036 * h1(ss.wrapping_add(2))) * (1.0 - 0.14 * ctx.intensity)).max(1.6);
            (loc_w * (span_lo + (span_hi - span_lo) * t_d), hw)
        } else {
            let t_d = (((j - n_core) as f64) + 0.15 + 0.70 * h1(ss.wrapping_add(1))) / (n_thin.max(1) as f64);
            let hw = (0.95 + 1.05 * h1(ss.wrapping_add(2))).max(0.85);
            (loc_w * (span_lo + (span_hi - span_lo) * t_d), hw)
        };
        let tilt = (h1(ss.wrapping_add(3)) - 0.5) * (if is_core { 1.5 } else { 3.2 } * DEG);
        let dv = rot2(tv, tilt);
        let anchor = (mid_wall.0 - nv.0 * in_d, mid_wall.1 - nv.1 * in_d);
        let Some((a_pt, b_pt, _, _)) = ctx.vsdf.clip_line_to_glyph(anchor, dv, max_reach, sdf_lim) else {
            continue;
        };
        if (b_pt.0 - a_pt.0).hypot(b_pt.1 - a_pt.1) < 4.0 {
            continue;
        }
        let os_max = ctx.glyph_h * (0.055 + 0.105 * ctx.intensity);
        let ext_a = if allow_os_a {
            if h1(ss.wrapping_add(4)) > 0.64 - 0.22 * ctx.intensity {
                os_max * (0.35 + 0.75 * h1(ss.wrapping_add(5)).powf(1.15))
            } else {
                (h1(ss.wrapping_add(5)) - 0.35) * 3.5
            }
        } else {
            0.0
        };
        let ext_b = if allow_os_b {
            if h1(ss.wrapping_add(6)) > 0.64 - 0.22 * ctx.intensity {
                os_max * (0.35 + 0.75 * h1(ss.wrapping_add(7)).powf(1.15))
            } else {
                (h1(ss.wrapping_add(7)) - 0.35) * 3.5
            }
        } else {
            0.0
        };

        let mut pts_list = Vec::with_capacity(5);
        if ext_a >= 2.0 {
            pts_list.push((a_pt.0 - dv.0 * ext_a, a_pt.1 - dv.1 * ext_a));
        }
        let ca = ext_a.clamp(-2.5, 2.0);
        pts_list.push((a_pt.0 - dv.0 * ca, a_pt.1 - dv.1 * ca));
        let wob = (h1(ss.wrapping_add(8)) - 0.5) * if is_core { 0.7 } else { 1.4 };
        pts_list.push((0.5 * (a_pt.0 + b_pt.0) + nv.0 * wob, 0.5 * (a_pt.1 + b_pt.1) + nv.1 * wob));
        let cb = ext_b.clamp(-2.5, 2.0);
        pts_list.push((b_pt.0 + dv.0 * cb, b_pt.1 + dv.1 * cb));
        if ext_b >= 2.0 {
            pts_list.push((b_pt.0 + dv.0 * ext_b, b_pt.1 + dv.1 * ext_b));
        }

        let cpts = resample_open(&pts_list, 3.0);
        let m_c = cpts.len();
        let mut arc_c = Vec::with_capacity(m_c);
        arc_c.push(0.0_f64);
        for w in cpts.windows(2) {
            let prev = *arc_c.last().unwrap_or(&0.0);
            arc_c.push(prev + (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1));
        }
        let tot_c = arc_c.last().copied().unwrap_or(1.0).max(1.0);
        let mut hws = vec![hw_base; m_c];
        for k in 0..m_c {
            if ext_a >= 2.0 && arc_c[k] < ext_a {
                let ta = (arc_c[k] / ext_a.max(1.0)).clamp(0.0, 1.0);
                hws[k] *= 0.14 + 0.86 * ta.powf(0.8);
            }
            let rb = tot_c - arc_c[k];
            if ext_b >= 2.0 && rb < ext_b {
                let tb = (rb / ext_b.max(1.0)).clamp(0.0, 1.0);
                hws[k] *= 0.14 + 0.86 * tb.powf(0.8);
            }
        }
        if is_core {
            let clamped: Vec<f64> = hws.into_iter().map(|v| v.max(0.35)).collect();
            if let Some(strip) = build_tapered_ribbon_strip(&cpts, &clamped) {
                body.push(strip);
            }
        } else {
            let thr = 0.16 + 0.30 * ctx.intensity;
            let n_off = h1(ss.wrapping_add(9)) * 30.0;
            let n_seed = ss.wrapping_add(91);
            emit_broken_ribbon(&cpts, &hws, &arc_c, 0.036, n_off, n_seed, thr, 0.28, body);
        }
    }

    if is_full_stem && loc_w >= 12.0 {
        let n_sl = 2 + (2.5 * ctx.intensity).round() as usize;
        for si in 0..n_sl {
            let ss = b_seed.wrapping_add(600).wrapping_add((si as u32).wrapping_mul(43));
            let in_d = loc_w * (0.14 + 0.72 * h1(ss.wrapping_add(1)));
            let dv = rot2(tv, (h1(ss.wrapping_add(2)) - 0.5) * (2.0 * DEG));
            let Some((sa, sb, _, _)) = ctx
                .vsdf
                .clip_line_to_glyph((mid_wall.0 - nv.0 * in_d, mid_wall.1 - nv.1 * in_d), dv, max_reach, -3.0)
            else {
                continue;
            };
            if (sb.0 - sa.0).hypot(sb.1 - sa.1) < 18.0 {
                continue;
            }
            let u0 = 0.08 + 0.42 * h1(ss.wrapping_add(3));
            let u1 = (u0 + 0.25 + 0.38 * h1(ss.wrapping_add(4))).min(0.92);
            let p_a = (sa.0 * (1.0 - u0) + sb.0 * u0, sa.1 * (1.0 - u0) + sb.1 * u0);
            let p_b = (sa.0 * (1.0 - u1) + sb.0 * u1, sa.1 * (1.0 - u1) + sb.1 * u1);
            let sp = resample_open(&[p_a, p_b], 3.0);
            let ms = sp.len();
            let base = (0.55 + 0.65 * h1(ss.wrapping_add(5))) * (0.7 + 0.5 * ctx.intensity);
            let shw: Vec<f64> = (0..ms)
                .map(|k| {
                    let u = if ms > 1 { k as f64 / (ms - 1) as f64 } else { 0.5 };
                    base * (u * PI).sin().max(0.0).powf(0.85)
                })
                .collect();
            if let Some(strip) = build_tapered_ribbon_strip(&sp, &shw) {
                slits.push(strip);
            }
        }
    }
}

fn emit_broken_ribbon(
    cpts: &[(f64, f64)],
    hws: &[f64],
    coord: &[f64],
    freq: f64,
    off: f64,
    seed: u32,
    thr: f64,
    min_hw: f64,
    out: &mut Vec<RibbonStrip>,
) {
    let m = cpts.len();
    let mut i = 0usize;
    while i < m {
        let alive = f64::from(value_noise_1d((coord[i] * freq + off) as f32, seed)) >= thr;
        if !alive {
            i += 1;
            continue;
        }
        let start = i;
        while i < m && f64::from(value_noise_1d((coord[i] * freq + off) as f32, seed)) >= thr {
            i += 1;
        }
        if i - start < 3 {
            continue;
        }
        let sub_p = &cpts[start..i];
        let n = sub_p.len();
        let sub_hw: Vec<f64> = (0..n)
            .map(|k| {
                let u = if n > 1 { k as f64 / (n - 1) as f64 } else { 0.5 };
                (hws[start + k] * (u * PI).sin().max(0.0).powf(0.45)).max(min_hw)
            })
            .collect();
        if let Some(strip) = build_tapered_ribbon_strip(sub_p, &sub_hw) {
            out.push(strip);
        }
    }
}

fn sample_chain_arc(
    ch: &Chain,
    arc: &[f64],
    total: f64,
    ink_w: &[f64],
    stroke_w: f64,
    s0: f64,
    s1: f64,
    nq: usize,
) -> (Vec<(f64, f64)>, Vec<(f64, f64)>, Vec<f64>) {
    let mut sp = Vec::with_capacity(nq);
    let mut sn = Vec::with_capacity(nq);
    let mut sw = Vec::with_capacity(nq);
    for qi in 0..nq {
        let sq = if nq > 1 { s0 + (s1 - s0) * (qi as f64) / ((nq - 1) as f64) } else { s0 };
        let sqm = if ch.closed_loop { sq.rem_euclid(total) } else { sq.clamp(0.0, total) };
        let idx = find_arc_idx(arc, sqm);
        let sl = (arc[idx + 1] - arc[idx]).max(1e-9);
        let u = (sqm - arc[idx]) / sl;
        let a = ch.pts[idx];
        let b = ch.pts[idx + 1];
        sp.push((a.0 * (1.0 - u) + b.0 * u, a.1 * (1.0 - u) + b.1 * u));
        let na = ch.nrm[idx];
        let nb = ch.nrm[idx + 1];
        let nx = na.0 * (1.0 - u) + nb.0 * u;
        let ny = na.1 * (1.0 - u) + nb.1 * u;
        let nl = nx.hypot(ny).max(1e-9);
        sn.push((nx / nl, ny / nl));
        sw.push((ink_w[idx] * (1.0 - u) + ink_w[idx + 1] * u).min(stroke_w * 1.48));
    }
    (sp, sn, sw)
}

fn emit_curved_arc(
    ch: &Chain,
    ink_w: &[f64],
    is_outer: bool,
    b_seed: u32,
    ctx: &StackCtx<'_>,
    body: &mut Vec<RibbonStrip>,
    flicks: &mut Vec<RibbonStrip>,
    slits: &mut Vec<RibbonStrip>,
) {
    let m = ch.pts.len();
    if m < 4 {
        return;
    }
    let mut arc = Vec::with_capacity(m);
    arc.push(0.0_f64);
    for w in ch.pts.windows(2) {
        let prev = *arc.last().unwrap_or(&0.0);
        arc.push(prev + (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1));
    }
    let total = arc.last().copied().unwrap_or(0.0);
    if total < 6.0 {
        return;
    }

    let tile_step = (ctx.stroke_w * 0.68).max(16.0);
    let tile_len = tile_step * 1.90;
    let n_tiles = (total / tile_step).round().max(2.0) as usize;
    for ti in 0..n_tiles {
        let ts = b_seed.wrapping_add(300).wrapping_add((ti as u32).wrapping_mul(31));
        let s0 = if ch.closed_loop {
            ((ti as f64) * tile_step).rem_euclid(total)
        } else {
            ((ti as f64) * tile_step - tile_step * 0.20).max(0.0)
        };
        let mut s1 = s0 + tile_len * (0.85 + 0.30 * h1(ts.wrapping_add(1)));
        if !ch.closed_loop {
            s1 = s1.min(total);
            if s1 - s0 < 6.0 {
                continue;
            }
        }
        let n_sub = (((s1 - s0) / 3.0) as usize).max(6);
        let (sub_p, sub_n, sub_w) = sample_chain_arc(ch, &arc, total, ink_w, ctx.stroke_w, s0, s1, n_sub);
        let n_lay = if is_outer { 4usize } else { 3usize };
        for li in 0..n_lay {
            let ls = ts.wrapping_add(60).wrapping_add((li as u32).wrapping_mul(17));
            let t_d = ((li as f64) + 0.18 + 0.64 * h1(ls.wrapping_add(1))) / (n_lay as f64);
            let frac = 0.05 + 0.50 * t_d;
            let cpts: Vec<(f64, f64)> = (0..n_sub)
                .map(|k| (sub_p[k].0 - sub_n[k].0 * (sub_w[k] * frac), sub_p[k].1 - sub_n[k].1 * (sub_w[k] * frac)))
                .collect();
            let is_c = li == 1 || li == 2;
            let hw = ((1.65 + 1.25 * h1(ls.wrapping_add(2))) * if is_c { 1.18 } else { 0.78 } * (ctx.stroke_w / 30.0)).max(1.25);
            let hws: Vec<f64> = (0..n_sub)
                .map(|k| {
                    let u = if n_sub > 1 { k as f64 / (n_sub - 1) as f64 } else { 0.5 };
                    hw * (0.24 + 0.76 * (u * PI).sin().max(0.0).powf(0.38))
                })
                .collect();
            if let Some(strip) = build_tapered_ribbon_strip(&cpts, &hws) {
                body.push(strip);
            }
        }
    }

    if is_outer && total >= 34.0 {
        let n_sl = ((total / 48.0) * (0.7 + 0.9 * ctx.intensity)).round().max(2.0) as usize;
        for si in 0..n_sl {
            let ss = b_seed.wrapping_add(740).wrapping_add((si as u32).wrapping_mul(37));
            let slen = (22.0 + 38.0 * h1(ss.wrapping_add(1))).min(total * 0.42);
            let s0 = h1(ss.wrapping_add(2)) * if ch.closed_loop { total } else { (total - slen).max(1.0) };
            let nq = ((slen / 3.0) as usize).max(6);
            let (sp, sn, sw) = sample_chain_arc(ch, &arc, total, ink_w, ctx.stroke_w, s0, s0 + slen, nq);
            if sw.iter().copied().fold(f64::INFINITY, f64::min) < 10.0 {
                continue;
            }
            let f0 = 0.14 + 0.62 * h1(ss.wrapping_add(3));
            let f1 = (f0 + (h1(ss.wrapping_add(4)) - 0.5) * 0.12).clamp(0.10, 0.80);
            let bw = 0.55 + 0.65 * h1(ss.wrapping_add(5));
            let s_pts: Vec<(f64, f64)> = (0..nq)
                .map(|k| {
                    let u = if nq > 1 { k as f64 / (nq - 1) as f64 } else { 0.5 };
                    let off = sw[k] * (f0 + (f1 - f0) * u);
                    (sp[k].0 - sn[k].0 * off, sp[k].1 - sn[k].1 * off)
                })
                .collect();
            let shw: Vec<f64> = (0..nq)
                .map(|k| {
                    let u = if nq > 1 { k as f64 / (nq - 1) as f64 } else { 0.5 };
                    bw * (u * PI).sin().max(0.0).powf(0.85)
                })
                .collect();
            if let Some(strip) = build_tapered_ribbon_strip(&s_pts, &shw) {
                slits.push(strip);
            }
        }
    }

    if is_outer {
        let flick_step = (ctx.stroke_w * 0.36 / (0.45 + 0.80 * ctx.intensity).max(0.55)).max(9.5);
        let n_fl = (total / flick_step).round().max(4.0) as usize;
        for fi in 0..n_fl {
            let fs = b_seed.wrapping_add(900).wrapping_add((fi as u32).wrapping_mul(37));
            if h1(fs.wrapping_add(1)) < 0.18 - 0.10 * ctx.intensity {
                continue;
            }
            let sc_f = ((fi as f64) + 0.2 + 0.6 * h1(fs.wrapping_add(2))) * flick_step;
            if !ch.closed_loop && (sc_f < 5.0 || sc_f > total - 5.0) {
                continue;
            }
            let span_in = ctx.stroke_w * (0.42 + 0.34 * h1(fs.wrapping_add(3)));
            let s_a = if ch.closed_loop { sc_f - span_in } else { (sc_f - span_in).max(0.0) };
            let (sp, sn, sw) = sample_chain_arc(ch, &arc, total, ink_w, ctx.stroke_w, s_a, sc_f, 6);
            let frac = 0.04 + 0.14 * h1(fs.wrapping_add(4));
            let mut cpts: Vec<(f64, f64)> = (0..6)
                .map(|k| (sp[k].0 - sn[k].0 * (sw[k] * frac), sp[k].1 - sn[k].1 * (sw[k] * frac)))
                .collect();
            let last = cpts[5];
            let prev = cpts[4];
            let tvx = last.0 - prev.0;
            let tvy = last.1 - prev.1;
            let tvl = tvx.hypot(tvy).max(1e-9);
            let tv = (tvx / tvl, tvy / tvl);
            let out_n = sn[5];
            let lift = 0.12 + 0.17 * h1(fs.wrapping_add(5));
            let sx = tv.0 + out_n.0 * lift;
            let sy = tv.1 + out_n.1 * lift;
            let sl = sx.hypot(sy).max(1e-9);
            let shoot = (sx / sl, sy / sl);
            let peel = ctx.glyph_h * (0.075 + 0.085 * h1(fs.wrapping_add(6))) * (0.60 + 0.60 * ctx.intensity);
            cpts.push((last.0 + shoot.0 * (peel * 0.52) + out_n.0 * (peel * 0.04), last.1 + shoot.1 * (peel * 0.52) + out_n.1 * (peel * 0.04)));
            cpts.push((last.0 + shoot.0 * peel + out_n.0 * (peel * 0.12), last.1 + shoot.1 * peel + out_n.1 * (peel * 0.12)));
            let nc = cpts.len();
            let hw = ((1.65 + 1.35 * h1(fs.wrapping_add(7))) * (ctx.stroke_w / 30.0)).max(1.35);
            let hws: Vec<f64> = (0..nc)
                .map(|k| {
                    let u = k as f64 / (nc - 1) as f64;
                    let fac = if u < 0.42 { 0.65 + 0.35 * (u / 0.42) } else { ((1.0 - u) / 0.58).max(0.0).powf(0.85) };
                    (hw * fac).max(0.22)
                })
                .collect();
            if let Some(strip) = build_tapered_ribbon_strip(&cpts, &hws) {
                flicks.push(strip);
            }
        }
    }
}

fn build_fgsfds_stack(
    outers: &[PolyRing],
    holes: &[PolyRing],
    vsdf: &VectorGlyphSdf,
    stroke_w: f64,
    glyph_h: f64,
    intensity: f64,
    seed: u32,
) -> (Vec<RibbonStrip>, Vec<RibbonStrip>, Vec<RibbonStrip>) {
    let ctx = StackCtx { vsdf, stroke_w, glyph_h, intensity, min_struct: 0.26 * glyph_h, min_bar: 0.19 * glyph_h };
    let mut body = Vec::new();
    let mut flicks = Vec::new();
    let mut slits = Vec::new();
    let mut ci = 0u32;

    for (is_outer, list) in [(true, outers), (false, holes)] {
        for cont in list {
            ci = ci.wrapping_add(1);
            let b_seed_c = seed
                .wrapping_mul(4993)
                .wrapping_add(ci.wrapping_mul(211))
                .wrapping_add(if is_outer { 0 } else { 9000 });
            if is_outer && contour_diag(cont) < 0.34 * glyph_h {
                body.extend(emit_compact_blob_strokes(cont, vsdf, b_seed_c, intensity));
                continue;
            }
            let chains = split_contour_chains(cont, vsdf);
            for (chi, ch) in chains.iter().enumerate() {
                if ch.pts.len() < 2 {
                    continue;
                }
                let ink_w = probe_ink_vec(&ch.pts, &ch.nrm, vsdf, stroke_w * 2.8);
                let b_seed = b_seed_c.wrapping_add((chi as u32).wrapping_mul(53));
                if ch.is_straight {
                    emit_straight_stem(ch, &ink_w, is_outer, b_seed, &ctx, &mut body, &mut slits);
                } else {
                    emit_curved_arc(ch, &ink_w, is_outer, b_seed, &ctx, &mut body, &mut flicks, &mut slits);
                }
            }
        }
    }
    (body, flicks, slits)
}

// =============================================================================
// 5. MODE 3: HEAVY HOLLOW KIN (`Kin`)
// =============================================================================

fn offset_miter_ring(raw: &[(f64, f64)], orient: f64, delta: f64, miter_limit: f64) -> PolyRing {
    let n = raw.len();
    let mut nrm = Vec::with_capacity(n);
    for i in 0..n {
        let a = raw[i];
        let b = raw[(i + 1) % n];
        let ex = b.0 - a.0;
        let ey = b.1 - a.1;
        let l = ex.hypot(ey).max(1e-9);
        nrm.push(((ey / l) * orient, (-ex / l) * orient));
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let n0 = nrm[(i + n - 1) % n];
        let n1 = nrm[i];
        let bx = n0.0 + n1.0;
        let by = n0.1 + n1.1;
        let bl = bx.hypot(by);
        if bl < 1e-5 {
            out.push((raw[i].0 + n1.0 * delta, raw[i].1 + n1.1 * delta));
        } else {
            let ux = bx / bl;
            let uy = by / bl;
            let cos_h = (ux * n1.0 + uy * n1.1).max(1.0 / miter_limit);
            let scale = delta * (1.0 / cos_h).min(miter_limit);
            out.push((raw[i].0 + ux * scale, raw[i].1 + uy * scale));
        }
    }
    out
}

fn build_hollow_kin_heavy(
    outers: &[PolyRing],
    holes: &[PolyRing],
    vsdf: &VectorGlyphSdf,
    stroke_w: f64,
    intensity: f64,
    seed: u32,
) -> (Vec<RibbonStrip>, Vec<RibbonStrip>, f64) {
    let band_w = (stroke_w * 0.56).clamp(16.5, 21.0);
    let mut body = Vec::new();
    let mut slits = Vec::new();
    let mut ci = 0u32;

    for (is_hole, list) in [(false, outers), (true, holes)] {
        for cont in list {
            ci = ci.wrapping_add(1);
            let raw = clean_closed_ring(cont, 0.8, 0.999);
            if raw.len() < 3 {
                continue;
            }
            let orient = vsdf.contour_orient_sign(&raw);
            let lo_off = -band_w * 0.42;
            let hi_off = if !is_hole { band_w * 0.58 } else { band_w * 0.16 };
            let n_bands = if !is_hole { 8usize } else { 5usize };

            for bi in 0..n_bands {
                let bs = seed
                    .wrapping_mul(2791)
                    .wrapping_add(ci.wrapping_mul(137))
                    .wrapping_add((bi as u32).wrapping_mul(31));
                let t_f = ((bi as f64) + 0.2 + 0.6 * h1(bs.wrapping_add(1))) / (n_bands as f64);
                let off_d = lo_off + (hi_off - lo_off) * t_f;
                let ring_m = offset_miter_ring(&raw, orient, off_d, 2.4);
                let r_sub = polyline_resample(&ring_m, 2.5);
                let m_r = r_sub.len();
                if m_r < 3 {
                    continue;
                }
                let mut c_loop = Vec::with_capacity(m_r + 2);
                let wob_off = h1(bs.wrapping_add(2)) * 30.0;
                let wob_seed = bs.wrapping_add(13);
                for i in 0..m_r {
                    let prev = r_sub[(i + m_r - 1) % m_r];
                    let next = r_sub[(i + 1) % m_r];
                    let tx = next.0 - prev.0;
                    let ty = next.1 - prev.1;
                    let tl = tx.hypot(ty).max(1e-9);
                    let nx = (ty / tl) * orient;
                    let ny = (-tx / tl) * orient;
                    let wob = (f64::from(value_noise_1d(((i as f64) * 2.5 * 0.032 + wob_off) as f32, wob_seed)) - 0.5) * 2.2;
                    c_loop.push((r_sub[i].0 + nx * wob, r_sub[i].1 + ny * wob));
                }
                c_loop.push(c_loop[0]);
                c_loop.push(c_loop[1]);

                let is_core = bi >= 1 && bi + 1 < n_bands;
                let mut hw_val = if is_core { 2.4 + 1.1 * h1(bs.wrapping_add(3)) } else { 1.4 + 0.9 * h1(bs.wrapping_add(3)) };
                if is_hole {
                    hw_val *= 0.80;
                }
                if is_core && h1(bs.wrapping_add(4)) > 0.35 * intensity {
                    let hws = vec![hw_val; c_loop.len()];
                    if let Some(strip) = build_tapered_ribbon_strip(&c_loop, &hws) {
                        body.push(strip);
                    }
                } else {
                    let hws = vec![hw_val; c_loop.len()];
                    let idx_coord: Vec<f64> = (0..c_loop.len()).map(|k| k as f64).collect();
                    let thr = 0.14 + 0.28 * intensity;
                    emit_broken_ribbon(&c_loop, &hws, &idx_coord, 0.09, h1(bs.wrapping_add(5)) * 25.0, bs.wrapping_add(71), thr, hw_val * 0.35, &mut body);
                }
            }

            if !is_hole {
                let chains = split_contour_chains(cont, vsdf);
                for (chi, ch) in chains.iter().enumerate() {
                    if ch.alen < 22.0 || ch.pts.len() < 5 {
                        continue;
                    }
                    let cs = seed
                        .wrapping_mul(6131)
                        .wrapping_add(ci.wrapping_mul(149))
                        .wrapping_add((chi as u32).wrapping_mul(41));
                    let n_tracks = 2usize + usize::from(intensity > 0.5);
                    for track_i in 0..n_tracks {
                        let ts = cs.wrapping_add(200).wrapping_add((track_i as u32).wrapping_mul(53));
                        let off_s = band_w * (-0.22 + 0.28 * (track_i as f64) + (h1(ts.wrapping_add(1)) - 0.5) * 0.12);
                        let n_seg = ((ch.alen / 44.0) as usize).max(1);
                        for si in 0..n_seg {
                            let ss = ts.wrapping_add(70).wrapping_add((si as u32).wrapping_mul(29));
                            if h1(ss.wrapping_add(1)) < 0.22 {
                                continue;
                            }
                            let sf = ((18.0 + 38.0 * h1(ss.wrapping_add(2))) / ch.alen.max(1.0)).min(0.62);
                            let u0 = 0.05 + (0.90 - sf) * h1(ss.wrapping_add(3));
                            let m = ch.pts.len();
                            let i0 = (u0 * (m - 1) as f64) as usize;
                            let i1 = (((u0 + sf) * (m - 1) as f64) as usize).max(i0 + 3).min(m - 1);
                            let nq = i1 - i0 + 1;
                            let sp: Vec<(f64, f64)> = (i0..=i1)
                                .map(|k| (ch.pts[k].0 + ch.nrm[k].0 * off_s, ch.pts[k].1 + ch.nrm[k].1 * off_s))
                                .collect();
                            let bw = (0.55 + 0.55 * h1(ss.wrapping_add(4))) * (0.7 + 0.5 * intensity);
                            let shw: Vec<f64> = (0..nq)
                                .map(|k| {
                                    let u = if nq > 1 { k as f64 / (nq - 1) as f64 } else { 0.5 };
                                    bw * (u * PI).sin().max(0.0).powf(0.85)
                                })
                                .collect();
                            if let Some(strip) = build_tapered_ribbon_strip(&sp, &shw) {
                                slits.push(strip);
                            }
                        }
                    }
                }
            }
        }
    }
    (body, slits, band_w)
}

// =============================================================================
// 6. MODE 4: FGSFDS FENCE (`FgsfdsFence`, analytical scanlines, zero raster!)
// =============================================================================

fn build_fgsfds_fence(vsdf: &VectorGlyphSdf, holes: &[PolyRing], stroke_w: f64, seed: u32) -> Vec<RibbonStrip> {
    let Some(gb) = vsdf.global_aabb() else {
        return Vec::new();
    };
    let hole_vsdf = VectorGlyphSdf::from_flat_contours(holes);
    let pitch0 = (stroke_w * 0.23).clamp(5.0, 9.2);
    let mut out = Vec::new();
    let mut x_cur = gb[0] + 1.5 * h1(seed.wrapping_mul(701).wrapping_add(3));
    let mut col_i = 0u32;

    while x_cur <= gb[2] + 1.0 {
        let cs = seed.wrapping_mul(6197).wrapping_add(col_i.wrapping_mul(41));
        let clump = f64::from(value_noise_1d((x_cur * 0.045) as f32, seed.wrapping_mul(331).wrapping_add(17)));
        let step = pitch0 * (0.62 + 0.78 * h1(cs.wrapping_add(1)));
        let runs = vsdf.vertical_runs_at_x(x_cur);
        for (ri, (y0, y1)) in runs.into_iter().enumerate() {
            let rs = cs.wrapping_add(100).wrapping_add((ri as u32).wrapping_mul(29));
            let run_h = y1 - y0;
            if run_h < 3.0 {
                continue;
            }
            let top_in_hole = !hole_vsdf.is_empty() && hole_vsdf.signed_distance((x_cur, y0 - 3.0)) < 2.0;
            let bot_in_hole = !hole_vsdf.is_empty() && hole_vsdf.signed_distance((x_cur, y1 + 3.0)) < 2.0;
            let r_scale = (run_h / 26.0).clamp(0.35, 1.0);
            let ext_t = if top_in_hole {
                (-1.2 + 3.6 * h1(rs.wrapping_add(2))) * r_scale
            } else {
                -2.5 + (8.0 + 10.5 * h1(rs.wrapping_add(2)).powf(1.15)) * r_scale
            };
            let ext_b = if bot_in_hole {
                (-1.2 + 3.6 * h1(rs.wrapping_add(3))) * r_scale
            } else {
                -2.5 + (8.0 + 10.5 * h1(rs.wrapping_add(3)).powf(1.15)) * r_scale
            };
            let yt = y0 - ext_t;
            let yb = y1 + ext_b;
            if yb - yt < 4.0 {
                continue;
            }
            let u_w = h1(rs.wrapping_add(4));
            let mut base_fill = (0.44 + 0.68 * u_w.powf(0.85)) * (0.78 + 0.42 * clump);
            if run_h > stroke_w * 2.2 {
                base_fill *= 1.12;
            }
            let bar_hw = (step * base_fill * 0.5).min(step * 0.62).max(1.1);
            let tilt_dx = (h1(rs.wrapping_add(5)) - 0.5) * ((yb - yt) * 0.045).min(3.8);
            let bend_dx = (h1(rs.wrapping_add(6)) - 0.5) * 1.8;
            let x_mid = x_cur + (h1(rs.wrapping_add(7)) - 0.5) * (step * 0.24);
            let cpts = resample_open(
                &[
                    (x_mid - tilt_dx * 0.5, yt),
                    (x_mid + bend_dx, 0.5 * (yt + yb)),
                    (x_mid + tilt_dx * 0.5, yb),
                ],
                4.0,
            );
            let m = cpts.len();
            let hws: Vec<f64> = (0..m)
                .map(|k| {
                    let u = if m > 1 { k as f64 / (m - 1) as f64 } else { 0.5 };
                    let taper = (u.min(1.0 - u) * 7.0).min(1.0);
                    bar_hw * (0.35 + 0.65 * taper.powf(0.55))
                })
                .collect();
            if let Some(strip) = build_tapered_ribbon_strip(&cpts, &hws) {
                out.push(strip);
            }
        }
        x_cur += step;
        col_i = col_i.wrapping_add(1);
    }
    out
}

// =============================================================================
// 7. PUBLIC VECTOR EXTRACTOR & CPU ORACLE RENDERER
// =============================================================================

pub fn build_heavy_stack_v3_from_groups(groups: &[Vec<PolyRing>], le: &SfxLineEffect) -> HeavyStackVectorGeometry {
    let vsdf = VectorGlyphSdf::from_contour_groups(groups);
    if vsdf.is_empty() {
        return HeavyStackVectorGeometry::default();
    }
    let (glyph_h, y_top_glyph) = estimate_glyph_h_and_top(groups);
    let stroke_w = vsdf.estimate_stroke_w(glyph_h).clamp(14.0, 90.0);
    let (outers, holes) = split_outer_hole_groups(groups);
    let intensity = f64::from((le.heavy_stack_intensity_pct / 100.0).clamp(0.0, 1.0));
    let bite = f64::from((le.heavy_stack_edge_bite_pct / 100.0).clamp(0.0, 1.0));
    let seed = le.heavy_stack_seed;

    match le.heavy_stack_mode {
        SfxHeavyStackMode::FgsfdsFence => HeavyStackVectorGeometry {
            body_ribbons: build_fgsfds_fence(&vsdf, &holes, stroke_w, seed),
            bite_polys: Vec::new(),
            tail_ribbons: Vec::new(),
            slit_ribbons: Vec::new(),
            stroke_w: stroke_w as f32,
        },
        SfxHeavyStackMode::Kin => {
            let (body_ribbons, slit_ribbons, band_w) = build_hollow_kin_heavy(&outers, &holes, &vsdf, stroke_w, intensity, seed);
            let bctx = BiteCtx { vsdf: &vsdf, stroke_w, glyph_h, bite, seed: seed.wrapping_add(29), is_kin: true, band_w };
            let bite_polys = build_edge_bite_polys(&outers, &holes, &bctx);
            HeavyStackVectorGeometry {
                body_ribbons,
                bite_polys,
                tail_ribbons: Vec::new(),
                slit_ribbons,
                stroke_w: stroke_w as f32,
            }
        }
        SfxHeavyStackMode::FgsfdsStack => {
            let (body_ribbons, tail_ribbons, slit_ribbons) =
                build_fgsfds_stack(&outers, &holes, &vsdf, stroke_w, glyph_h, intensity, seed);
            let bctx = BiteCtx { vsdf: &vsdf, stroke_w, glyph_h, bite, seed: seed.wrapping_add(29), is_kin: false, band_w: 18.0 };
            let bite_polys = build_edge_bite_polys(&outers, &holes, &bctx);
            HeavyStackVectorGeometry {
                body_ribbons,
                bite_polys,
                tail_ribbons,
                slit_ribbons,
                stroke_w: stroke_w as f32,
            }
        }
        SfxHeavyStackMode::Fude => {
            let bctx = BiteCtx { vsdf: &vsdf, stroke_w, glyph_h, bite, seed: seed.wrapping_add(29), is_kin: false, band_w: 18.0 };
            let bite_polys = build_edge_bite_polys(&outers, &holes, &bctx);
            let (tail_ribbons, slit_ribbons) =
                build_fude_details(&outers, &holes, &vsdf, stroke_w, glyph_h, y_top_glyph, intensity, seed);
            HeavyStackVectorGeometry {
                body_ribbons: Vec::new(),
                bite_polys,
                tail_ribbons,
                slit_ribbons,
                stroke_w: stroke_w as f32,
            }
        }
    }
}

pub fn build_heavy_stack_v3_vector(geom: &PreparedSfx, le: &SfxLineEffect) -> HeavyStackVectorGeometry {
    let groups: Vec<Vec<PolyRing>> = geom
        .glyphs
        .iter()
        .map(|g| g.polylines.iter().map(|pl| pl.pts.clone()).collect())
        .collect();
    build_heavy_stack_v3_from_groups(&groups, le)
}

/// Full CPU oracle render for Style 3 (`Heavy Stacked Brush v3`, rev 3).
/// Enforces exact composition order: `body - edge_bite`, THEN `max(tails/flicks)`, THEN `- slits`,
/// THEN single unified `apply_edge_kasure_breakup_doc`.
pub fn render_heavy_stack_v3_cpu(geom: &PreparedSfx, face_cov: &[f32], bounds: Rect, le: &SfxLineEffect) -> Vec<f32> {
    let w = bounds.width() as usize;
    let h = bounds.height() as usize;
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let hg = build_heavy_stack_v3_vector(geom, le);

    let mut cov: Vec<f32> = match le.heavy_stack_mode {
        SfxHeavyStackMode::Fude => {
            let glyph_sdf = exact_sdf(face_cov, w, h);
            let eff_sdf = perturb_heavy_sdf_doc(&glyph_sdf, bounds, hg.stroke_w * 0.68, le.heavy_stack_seed, 0.85);
            eff_sdf.into_iter().map(|d| (0.5 - d).clamp(0.0, 1.0)).collect()
        }
        SfxHeavyStackMode::FgsfdsStack | SfxHeavyStackMode::Kin | SfxHeavyStackMode::FgsfdsFence => {
            rasterize_ribbons_max(&hg.body_ribbons, bounds)
        }
    };

    // 1. Subtract focal edge bites strictly from cov_body BEFORE unioning tails/flicks
    if !hg.bite_polys.is_empty() {
        let bite_cov = rasterize_polygons_max(&hg.bite_polys, bounds);
        for (c, b) in cov.iter_mut().zip(bite_cov) {
            *c = (*c - b).clamp(0.0, 1.0);
        }
    }

    // 2. Union tangential tails / curved flicks (immune to edge_bite!)
    if !hg.tail_ribbons.is_empty() {
        let tail_cov = rasterize_ribbons_max(&hg.tail_ribbons, bounds);
        for (c, t) in cov.iter_mut().zip(tail_cov) {
            if t > *c {
                *c = t;
            }
        }
    }

    // 3. Subtract interior dry slits
    if !hg.slit_ribbons.is_empty() {
        let slit_cov = rasterize_ribbons_max(&hg.slit_ribbons, bounds);
        for (c, s) in cov.iter_mut().zip(slit_cov) {
            *c = (*c - s).clamp(0.0, 1.0);
        }
    }

    // 4. Unified single-pass edge kasure over the entire combined mask
    let grain_seed = le.heavy_stack_seed.wrapping_add(41);
    match le.heavy_stack_edge_mode {
        SfxV3EdgeMode::OneBit => {
            let grain_pct = if le.line_grain_pct > 0.5 { le.line_grain_pct } else { 14.0 };
            apply_edge_kasure_breakup_doc(
                &mut cov,
                bounds,
                EdgeKasureParams {
                    dryness_pct: grain_pct,
                    sharpness_pct: 100.0,
                    seed: grain_seed,
                    core_protect_px: 1.4,
                    one_bit: true,
                },
            );
        }
        SfxV3EdgeMode::Rough => {
            let dry = le.heavy_stack_dryness_pct.max(le.line_grain_pct);
            apply_edge_kasure_breakup_doc(
                &mut cov,
                bounds,
                EdgeKasureParams {
                    dryness_pct: dry,
                    sharpness_pct: le.heavy_stack_sharpness_pct,
                    seed: grain_seed,
                    core_protect_px: 2.0,
                    one_bit: false,
                },
            );
        }
    }
    cov
}

#[cfg(test)]
mod tests {
    use photocraft_doc::sfx::{SfxHeavyStackMode, SfxLineEffect, SfxLineStyle};

    use super::build_heavy_stack_v3_from_groups;

    fn sample_exclamation_groups() -> Vec<Vec<Vec<(f64, f64)>>> {
        // Tall vertical stem (x=20..56, y=10..150, h=140) + small '!' dot (x=24..52, y=166..190, diag=36.9 < 0.38*180=68.4).
        let stem = vec![(20.0, 10.0), (56.0, 10.0), (56.0, 150.0), (20.0, 150.0)];
        let dot = vec![(24.0, 166.0), (52.0, 166.0), (52.0, 190.0), (24.0, 190.0)];
        vec![vec![stem, dot]]
    }

    #[test]
    fn heavy_stack_edge_bite_zero_produces_no_bites_and_matches_base() {
        let groups = sample_exclamation_groups();
        for mode in [SfxHeavyStackMode::Fude, SfxHeavyStackMode::FgsfdsStack, SfxHeavyStackMode::Kin] {
            let mut le = SfxLineEffect {
                style: SfxLineStyle::HeavyStackV3,
                heavy_stack_mode: mode,
                heavy_stack_edge_bite_pct: 0.0,
                heavy_stack_seed: 7,
                ..SfxLineEffect::default()
            };
            let g0 = build_heavy_stack_v3_from_groups(&groups, &le);
            assert!(g0.bite_polys.is_empty(), "{mode:?}: edge_bite_pct=0 must emit 0 bite polygons");
            le.heavy_stack_edge_bite_pct = 65.0;
            let g65 = build_heavy_stack_v3_from_groups(&groups, &le);
            assert!(!g65.bite_polys.is_empty(), "{mode:?}: edge_bite_pct=65 must emit focal bite polygons on long walls");
        }
    }

    #[test]
    fn heavy_stack_edge_bite_65_keeps_corners_and_small_dot_immune_and_preserves_tails() {
        let groups = sample_exclamation_groups();
        let le = SfxLineEffect {
            style: SfxLineStyle::HeavyStackV3,
            heavy_stack_mode: SfxHeavyStackMode::Fude,
            heavy_stack_intensity_pct: 80.0,
            heavy_stack_edge_bite_pct: 65.0,
            heavy_stack_seed: 3,
            ..SfxLineEffect::default()
        };
        let hg = build_heavy_stack_v3_from_groups(&groups, &le);
        assert!(!hg.bite_polys.is_empty(), "expected focal edge bites on tall vertical walls");
        assert!(!hg.tail_ribbons.is_empty(), "expected tangential tails to be preserved independently of edge bites");

        // Verify ZERO bite vertices land on the small '!' dot (y >= 162) or near top/bottom corners (y < 22 or y > 138 of stem).
        for poly in &hg.bite_polys {
            for &(x, y) in poly {
                assert!(x.is_finite() && y.is_finite(), "non-finite bite vertex");
                assert!(y < 158.0, "small '!' dot must be 100% immune to edge bites (found bite at y={y:.1})");
                assert!(y > 24.0 && y < 136.0, "corner window t outside [0.16, 0.84] must be immune (found bite at y={y:.1})");
            }
        }
    }
}
