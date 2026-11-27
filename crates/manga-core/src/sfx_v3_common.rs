//! Shared SFX v3 infrastructure (`sfx_core.py` 1:1 Rust port + analytical vector SDF + nesting-depth
//! contour classification) used by Style 1 (`Shibufude v3`) and Style 2 (`Sugomi v3`).
//!
//! All noise and perturbation functions operate strictly in **document coordinates** (`doc_x, doc_y`)
//! using `wrapping_mul` / `floor()` so CPU and GPU (`sfx.wgsl`) match bit-for-bit without grain drift
//! under pan/zoom/sub-viewport slicing.

use photocraft_doc::{FillRule, PathOp};
use photocraft_geom::Rect;
use photocraft_vector::Rasterizer;

use crate::sfx_geom::PreparedSfx;

pub type PolyRing = Vec<(f64, f64)>;

// =============================================================================
// 1. BIT-EXACT DETERMINISTIC HASH, VALUE NOISE & CRISP PAPER GRAIN
// =============================================================================

#[inline]
pub fn hash_u32(n: u32) -> f32 {
    let mut x = n ^ 0x9E37_79B9;
    x = (x ^ (x >> 16)).wrapping_mul(0x85EB_CA6B);
    x = (x ^ (x >> 13)).wrapping_mul(0xC2B2_AE35);
    let out = x ^ (x >> 16);
    (out as f32) / 4_294_967_296.0
}

#[inline]
pub fn h1(n: u32) -> f64 {
    f64::from(hash_u32(n))
}

#[inline]
pub fn value_noise_1d(x: f32, seed: u32) -> f32 {
    let i = x.floor() as i32;
    let f = x - i as f32;
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash_u32((i as u32).wrapping_mul(374_761_393).wrapping_add(seed));
    let b = hash_u32(((i.wrapping_add(1)) as u32).wrapping_mul(374_761_393).wrapping_add(seed));
    a + (b - a) * u
}

#[inline]
pub fn value_noise_2d(x: f32, y: f32, seed: u32) -> f32 {
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let fx = x - ix as f32;
    let fy = y - iy as f32;
    let ux = fx * fx * (3.0 - 2.0 * fx);
    let uy = fy * fy * (3.0 - 2.0 * fy);
    let h = |px: i32, py: i32| -> f32 {
        hash_u32((px as u32).wrapping_mul(374_761_393).wrapping_add((py as u32).wrapping_mul(668_265_263)).wrapping_add(seed))
    };
    let a = h(ix, iy);
    let b = h(ix.wrapping_add(1), iy);
    let c = h(ix, iy.wrapping_add(1));
    let d = h(ix.wrapping_add(1), iy.wrapping_add(1));
    (a + (b - a) * ux) * (1.0 - uy) + (c + (d - c) * ux) * uy
}

#[inline]
pub fn crisp_paper_grain(x: f32, y: f32, seed: u32) -> f32 {
    let micro = value_noise_2d(x * 1.15, y * 1.15, seed);
    let ix = x.round() as i32;
    let iy = y.round() as i32;
    let speckle = hash_u32(
        (ix as u32)
            .wrapping_mul(1_662_606_333)
            .wrapping_add((iy as u32).wrapping_mul(84_348_833))
            .wrapping_add(seed),
    );
    let fiber = value_noise_2d(x * 0.45, y * 0.45, seed.wrapping_add(0x9E37));
    (micro * 0.55 + speckle * 0.25 + fiber * 0.20).clamp(0.0, 1.0)
}

// =============================================================================
// 2. NESTING-DEPTH PARITY CONTOUR CLASSIFICATION (FOR 回 / 目 / ◎)
// =============================================================================

/// Classification of one closed contour inside a glyph by nesting-depth parity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContourClass {
    /// Number of other contours in the same glyph strictly containing this contour.
    pub depth: usize,
    /// `true` for odd depth (counter hole), `false` for even depth (outer ring or inner island).
    pub is_hole: bool,
}

#[inline]
pub fn signed_area(pts: &[(f64, f64)]) -> f64 {
    let n = pts.len();
    if n < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..n {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        sum += a.0 * b.1 - b.0 * a.1;
    }
    0.5 * sum
}

#[inline]
pub fn contour_aabb(pts: &[(f64, f64)]) -> [f64; 4] {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for &(x, y) in pts {
        if x < min_x {
            min_x = x;
        }
        if y < min_y {
            min_y = y;
        }
        if x > max_x {
            max_x = x;
        }
        if y > max_y {
            max_y = y;
        }
    }
    [min_x, min_y, max_x, max_y]
}

/// Ray-casting even-odd point-in-polygon test.
pub fn point_in_ring(pt: (f64, f64), ring: &[(f64, f64)]) -> bool {
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let (px, py) = pt;
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = ring[i];
        let (xj, yj) = ring[j];
        let crosses = (yi > py) != (yj > py);
        if crosses {
            let x_int = (xj - xi) * (py - yi) / (yj - yi) + xi;
            if px < x_int {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Classifies every closed contour in a glyph by **nesting-depth parity**:
/// even depth (`0, 2, 4...`) = outer contour / inner island; odd depth (`1, 3...`) = hole.
pub fn classify_glyph_contours(contours: &[PolyRing]) -> Vec<ContourClass> {
    let n = contours.len();
    let boxes: Vec<[f64; 4]> = contours.iter().map(|c| contour_aabb(c)).collect();
    let areas: Vec<f64> = contours.iter().map(|c| signed_area(c).abs()).collect();
    let mut out = Vec::with_capacity(n);

    for i in 0..n {
        let ring_i = &contours[i];
        if ring_i.len() < 3 {
            out.push(ContourClass { depth: 0, is_hole: false });
            continue;
        }
        // Pick a representative probe point on ring_i (midpoint of longest edge avoids vertex ties).
        let probe = representative_point(ring_i);
        let bi = boxes[i];
        let mut depth = 0usize;
        for j in 0..n {
            if i == j || contours[j].len() < 3 {
                continue;
            }
            // A containing contour must have strictly larger absolute area and enclosing AABB.
            if areas[j] <= areas[i] + 1e-6 {
                continue;
            }
            let bj = boxes[j];
            if bi[0] < bj[0] - 1e-3 || bi[1] < bj[1] - 1e-3 || bi[2] > bj[2] + 1e-3 || bi[3] > bj[3] + 1e-3 {
                continue;
            }
            if point_in_ring(probe, &contours[j]) {
                depth += 1;
            }
        }
        out.push(ContourClass { depth, is_hole: (depth & 1) == 1 });
    }
    out
}

fn representative_point(ring: &[(f64, f64)]) -> (f64, f64) {
    let n = ring.len();
    let mut best_len = -1.0;
    let mut best_pt = ring[0];
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        let l = (b.0 - a.0).hypot(b.1 - a.1);
        if l > best_len {
            best_len = l;
            best_pt = ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
        }
    }
    best_pt
}

/// Normalizes a glyph's contour windings using nesting-depth parity so NonZero winding fill
/// renders outer contours and nested islands (`回`, `目`, `◎`) solid and odd-depth contours as holes.
pub fn normalize_glyph_winding_by_depth(contours: &mut [PolyRing]) {
    let classes = classify_glyph_contours(contours);
    for (ring, cls) in contours.iter_mut().zip(classes) {
        if ring.len() < 3 {
            continue;
        }
        let area = signed_area(ring);
        let want_positive = !cls.is_hole;
        if (want_positive && area < 0.0) || (!want_positive && area > 0.0) {
            ring.reverse();
        }
    }
}

// =============================================================================
// 3. ANALYTICAL VECTOR GLYPH SDF & RAY PROBE (ZERO FULL-CANVAS RASTER ALLOCATION)
// =============================================================================

struct IndexedContour {
    pts: PolyRing,
    aabb: [f64; 4],
}

/// Exact analytical signed-distance & ray-intersection oracle over a [`PreparedSfx`] geometry.
///
/// Runs in O(vertices) memory with zero pixel buffers and zero GPU readback, even on 2400 DPI /
/// 12K×17K lettering.
pub struct VectorGlyphSdf {
    glyphs: Vec<([f64; 4], Vec<IndexedContour>)>,
}

impl VectorGlyphSdf {
    pub fn from_prepared(geom: &PreparedSfx) -> Self {
        let mut glyphs = Vec::with_capacity(geom.glyphs.len());
        for g in &geom.glyphs {
            let mut cs = Vec::with_capacity(g.polylines.len());
            let mut gb = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
            for pl in &g.polylines {
                if pl.pts.len() < 3 {
                    continue;
                }
                let bb = contour_aabb(&pl.pts);
                gb[0] = gb[0].min(bb[0]);
                gb[1] = gb[1].min(bb[1]);
                gb[2] = gb[2].max(bb[2]);
                gb[3] = gb[3].max(bb[3]);
                cs.push(IndexedContour { pts: pl.pts.clone(), aabb: bb });
            }
            if !cs.is_empty() {
                glyphs.push((gb, cs));
            }
        }
        Self { glyphs }
    }

    pub fn from_contour_groups(groups: &[Vec<PolyRing>]) -> Self {
        let mut glyphs = Vec::with_capacity(groups.len());
        for group in groups {
            let mut cs = Vec::with_capacity(group.len());
            let mut gb = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
            for ring in group {
                if ring.len() < 3 {
                    continue;
                }
                let bb = contour_aabb(ring);
                gb[0] = gb[0].min(bb[0]);
                gb[1] = gb[1].min(bb[1]);
                gb[2] = gb[2].max(bb[2]);
                gb[3] = gb[3].max(bb[3]);
                cs.push(IndexedContour { pts: ring.clone(), aabb: bb });
            }
            if !cs.is_empty() {
                glyphs.push((gb, cs));
            }
        }
        Self { glyphs }
    }

    /// Even-odd inside check across all glyphs (union of glyphs; each glyph uses even-odd parity,
    /// which handles `回`, `目`, `◎` accurately regardless of raw font winding).
    pub fn is_inside(&self, pt: (f64, f64)) -> bool {
        let (x, y) = pt;
        for (gb, cs) in &self.glyphs {
            if x < gb[0] || x > gb[2] || y < gb[1] || y > gb[3] {
                continue;
            }
            let mut inside_glyph = false;
            for c in cs {
                if x >= c.aabb[0] && x <= c.aabb[2] && y >= c.aabb[1] && y <= c.aabb[3] && point_in_ring(pt, &c.pts) {
                    inside_glyph = !inside_glyph;
                }
            }
            if inside_glyph {
                return true;
            }
        }
        false
    }

    /// Exact signed distance in document pixels (`> 0` outside ink, `< 0` inside ink).
    pub fn signed_distance(&self, pt: (f64, f64)) -> f64 {
        let mut best_d2 = f64::INFINITY;
        for (gb, cs) in &self.glyphs {
            let g_d2 = point_aabb_dist2(pt, *gb);
            if g_d2 >= best_d2 {
                continue;
            }
            for c in cs {
                if point_aabb_dist2(pt, c.aabb) >= best_d2 {
                    continue;
                }
                let n = c.pts.len();
                for i in 0..n {
                    let d2 = point_seg_dist2(pt, c.pts[i], c.pts[(i + 1) % n]);
                    if d2 < best_d2 {
                        best_d2 = d2;
                    }
                }
            }
        }
        if !best_d2.is_finite() {
            return 1e6;
        }
        let d = best_d2.sqrt();
        if self.is_inside(pt) { -d } else { d }
    }

    /// Single consistent outward-to-air normal sign (`+1.0` or `-1.0`) for an entire closed contour.
    pub fn contour_orient_sign(&self, ring: &[(f64, f64)]) -> f64 {
        let n = ring.len();
        if n < 3 {
            return 1.0;
        }
        let mut vote = 0.0;
        let stride = (n / 48).max(1);
        for i in (0..n).step_by(stride) {
            let a = ring[i];
            let b = ring[(i + 1) % n];
            let ex = b.0 - a.0;
            let ey = b.1 - a.1;
            let el = ex.hypot(ey);
            if el < 1e-6 {
                continue;
            }
            let mx = 0.5 * (a.0 + b.0);
            let my = 0.5 * (a.1 + b.1);
            let nx = ey / el;
            let ny = -ex / el;
            let s_plus = self.signed_distance((mx + nx * 1.5, my + ny * 1.5));
            let s_minus = self.signed_distance((mx - nx * 1.5, my - ny * 1.5));
            vote += el * (s_plus - s_minus);
        }
        if vote >= 0.0 { 1.0 } else { -1.0 }
    }

    /// Exact ray-segment distance probe from `origin` along unit `dir` in `[start, smax]`.
    /// Finds the smallest hit `t >= start` against any glyph boundary.
    pub fn probe_ray(&self, origin: (f64, f64), dir: (f64, f64), start: f64, smax: f64) -> f64 {
        let mut best_t = smax;
        let end_x = origin.0 + dir.0 * smax;
        let end_y = origin.1 + dir.1 * smax;
        let ray_bb = [origin.0.min(end_x), origin.1.min(end_y), origin.0.max(end_x), origin.1.max(end_y)];

        for (gb, cs) in &self.glyphs {
            if !aabb_overlap(ray_bb, *gb) {
                continue;
            }
            for c in cs {
                if !aabb_overlap(ray_bb, c.aabb) {
                    continue;
                }
                let n = c.pts.len();
                for i in 0..n {
                    if let Some(t) = ray_segment_hit(origin, dir, c.pts[i], c.pts[(i + 1) % n])
                        && t >= start
                        && t < best_t
                    {
                        best_t = t;
                    }
                }
            }
        }
        best_t
    }

    /// Estimates typical glyph stroke width (`2 * 75th-percentile interior distance`) using inward
    /// contour ray probes (scales cleanly from 48pt up to 2400 DPI without full-canvas rasterization).
    pub fn estimate_stroke_w(&self, glyph_em_px: f64) -> f64 {
        let max_probe = (glyph_em_px * 0.95).clamp(48.0, 2048.0);
        let mut widths = Vec::new();
        for (_, cs) in &self.glyphs {
            for c in cs {
                let n = c.pts.len();
                if n < 3 {
                    continue;
                }
                let orient = self.contour_orient_sign(&c.pts);
                let stride = (n / 32).max(1);
                for i in (0..n).step_by(stride) {
                    let a = c.pts[i];
                    let b = c.pts[(i + 1) % n];
                    let ex = b.0 - a.0;
                    let ey = b.1 - a.1;
                    let el = ex.hypot(ey);
                    if el < 1e-4 {
                        continue;
                    }
                    let mx = 0.5 * (a.0 + b.0);
                    let my = 0.5 * (a.1 + b.1);
                    // Inward unit normal pointing into the ink stroke.
                    let in_x = -(ey / el) * orient;
                    let in_y = (ex / el) * orient;
                    let w = self.probe_ray((mx, my), (in_x, in_y), 1.5, max_probe);
                    if w < max_probe - 0.5 {
                        widths.push(w);
                    }
                }
            }
        }
        let upper = (glyph_em_px * 0.75).max(90.0);
        if widths.is_empty() {
            return (glyph_em_px * 0.22).clamp(14.0, upper);
        }
        widths.sort_by(|a, b| a.total_cmp(b));
        let p60 = widths[(widths.len() * 60 / 100).min(widths.len() - 1)];
        p60.clamp(8.0, upper)
    }
}

#[inline]
fn aabb_overlap(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] <= b[2] && a[2] >= b[0] && a[1] <= b[3] && a[3] >= b[1]
}

#[inline]
fn point_aabb_dist2(pt: (f64, f64), bb: [f64; 4]) -> f64 {
    let dx = if pt.0 < bb[0] {
        bb[0] - pt.0
    } else if pt.0 > bb[2] {
        pt.0 - bb[2]
    } else {
        0.0
    };
    let dy = if pt.1 < bb[1] {
        bb[1] - pt.1
    } else if pt.1 > bb[3] {
        pt.1 - bb[3]
    } else {
        0.0
    };
    dx * dx + dy * dy
}

#[inline]
fn point_seg_dist2(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let l2 = dx * dx + dy * dy;
    if l2 < 1e-12 {
        let rx = p.0 - a.0;
        let ry = p.1 - a.1;
        return rx * rx + ry * ry;
    }
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0);
    let rx = p.0 - (a.0 + dx * t);
    let ry = p.1 - (a.1 + dy * t);
    rx * rx + ry * ry
}

#[inline]
fn ray_segment_hit(origin: (f64, f64), dir: (f64, f64), a: (f64, f64), b: (f64, f64)) -> Option<f64> {
    let sx = b.0 - a.0;
    let sy = b.1 - a.1;
    let denom = dir.0 * sy - dir.1 * sx;
    if denom.abs() < 1e-12 {
        return None;
    }
    let qx = a.0 - origin.0;
    let qy = a.1 - origin.1;
    let t = (qx * sy - qy * sx) / denom;
    if t <= 0.0 {
        return None;
    }
    let u = (qx * dir.1 - qy * dir.0) / denom;
    if (-1e-6..=1.0 + 1e-6).contains(&u) { Some(t) } else { None }
}

// =============================================================================
// 4. POLYLINE UTILITIES & PER-POLYGON MAX-UNION RASTERIZER
// =============================================================================

/// Drop micro-edges, duplicate wrap-around endpoints, and redundant collinear vertices.
pub fn clean_closed_ring(pts: &[(f64, f64)], min_edge: f64, col_tol: f64) -> PolyRing {
    let mut end = pts.len();
    while end > 1 && (pts[0].0 - pts[end - 1].0).hypot(pts[0].1 - pts[end - 1].1) < 1e-6 {
        end -= 1;
    }
    if end < 3 {
        return pts[..end].to_vec();
    }
    let mut kept = Vec::with_capacity(end);
    kept.push(pts[0]);
    for &pt in &pts[1..end] {
        if !pt.0.is_finite() || !pt.1.is_finite() {
            continue;
        }
        let last = *kept.last().unwrap_or(&pts[0]);
        if (pt.0 - last.0).hypot(pt.1 - last.1) >= min_edge {
            kept.push(pt);
        }
    }
    while kept.len() > 2 && (kept[0].0 - kept[kept.len() - 1].0).hypot(kept[0].1 - kept[kept.len() - 1].1) < min_edge {
        kept.pop();
    }
    if kept.len() < 3 {
        return kept;
    }
    let n = kept.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let a = kept[(i + n - 1) % n];
        let b = kept[i];
        let c = kept[(i + 1) % n];
        let v0 = (b.0 - a.0, b.1 - a.1);
        let v1 = (c.0 - b.0, c.1 - b.1);
        let l0 = v0.0.hypot(v0.1);
        let l1 = v1.0.hypot(v1.1);
        if l0 < 1e-9 || l1 < 1e-9 {
            continue;
        }
        if (v0.0 * v1.0 + v0.1 * v1.1) / (l0 * l1) > col_tol {
            continue;
        }
        out.push(b);
    }
    if out.len() >= 3 { out } else { kept }
}

/// Resample a closed polyline to near-uniform arc-length `step` (px).
pub fn polyline_resample(pts: &[(f64, f64)], step: f64) -> PolyRing {
    if pts.len() < 2 || step <= 1e-6 {
        return pts.to_vec();
    }
    let closed = (pts[0].0 - pts[pts.len() - 1].0).hypot(pts[0].1 - pts[pts.len() - 1].1) < 1e-9;
    let mut q = Vec::with_capacity(pts.len() + 1);
    q.extend_from_slice(pts);
    if !closed {
        q.push(pts[0]);
    }
    let mut cum = Vec::with_capacity(q.len());
    cum.push(0.0_f64);
    let mut total = 0.0_f64;
    for i in 0..(q.len() - 1) {
        total += (q[i + 1].0 - q[i].0).hypot(q[i + 1].1 - q[i].1);
        cum.push(total);
    }
    if total < 1e-9 {
        return pts.to_vec();
    }
    let n = ((total / step).round() as usize).max(8);
    let mut out = Vec::with_capacity(n);
    let mut j = 0usize;
    for k in 0..n {
        let t = total * (k as f64 / n as f64);
        while j + 2 < cum.len() && cum[j + 1] < t {
            j += 1;
        }
        let seg_len = cum[j + 1] - cum[j];
        let u = if seg_len < 1e-12 { 0.0 } else { (t - cum[j]) / seg_len };
        out.push((q[j].0 * (1.0 - u) + q[j + 1].0 * u, q[j].1 * (1.0 - u) + q[j + 1].1 * u));
    }
    out
}

pub fn smooth_vec(v: &[(f64, f64)], passes: usize) -> Vec<(f64, f64)> {
    let n = v.len();
    if n < 3 {
        return v.to_vec();
    }
    let mut cur = v.to_vec();
    let mut next = vec![(0.0, 0.0); n];
    for _ in 0..passes {
        for i in 0..n {
            let p = cur[(i + n - 1) % n];
            let c = cur[i];
            let q = cur[(i + 1) % n];
            next[i] = ((p.0 + 2.0 * c.0 + q.0) * 0.25, (p.1 + 2.0 * c.1 + q.1) * 0.25);
        }
        std::mem::swap(&mut cur, &mut next);
    }
    for item in &mut cur {
        let l = item.0.hypot(item.1);
        if l >= 1e-9 {
            item.0 /= l;
            item.1 /= l;
        }
    }
    cur
}

pub fn smooth_scalar(a: &[f64], passes: usize) -> Vec<f64> {
    let n = a.len();
    if n < 3 {
        return a.to_vec();
    }
    let mut cur = a.to_vec();
    let mut next = vec![0.0; n];
    for _ in 0..passes {
        for i in 0..n {
            next[i] = (cur[(i + n - 1) % n] + 2.0 * cur[i] + cur[(i + 1) % n]) * 0.25;
        }
        std::mem::swap(&mut cur, &mut next);
    }
    cur
}

#[inline]
pub fn rot2(v: (f64, f64), ang: f64) -> (f64, f64) {
    let (s, c) = ang.sin_cos();
    (v.0 * c - v.1 * s, v.0 * s + v.1 * c)
}

pub fn sample_base(base_pts: &[(f64, f64)], nrm_s: &[(f64, f64)], arc: &[f64], total: f64, s_q: f64) -> ((f64, f64), (f64, f64), usize) {
    let n = base_pts.len();
    let s = s_q.rem_euclid(total);
    let idx = arc.partition_point(|&v| v <= s).saturating_sub(1) % n;
    let nxt = (idx + 1) % n;
    let seg_a = (arc[nxt] - arc[idx]).rem_euclid(total);
    let u = if seg_a < 1e-9 { 0.0 } else { (s - arc[idx]).rem_euclid(total) / seg_a };
    let p = (base_pts[idx].0 * (1.0 - u) + base_pts[nxt].0 * u, base_pts[idx].1 * (1.0 - u) + base_pts[nxt].1 * u);
    let nv_raw = (nrm_s[idx].0 * (1.0 - u) + nrm_s[nxt].0 * u, nrm_s[idx].1 * (1.0 - u) + nrm_s[nxt].1 * u);
    let nl = nv_raw.0.hypot(nv_raw.1).max(1e-9);
    (p, (nv_raw.0 / nl, nv_raw.1 / nl), idx)
}

#[inline]
pub fn torn_1d(s_arc: f64, ci: u32, seed: u32, amp: f64) -> f64 {
    let s = s_arc as f32;
    let n1 = (value_noise_1d(s * 0.055, seed.wrapping_mul(251).wrapping_add(ci.wrapping_mul(19))) - 0.5) * 2.0;
    let n2 = (value_noise_1d(s * 0.145, seed.wrapping_mul(389).wrapping_add(ci.wrapping_mul(31))) - 0.5) * 2.0;
    f64::from(n1 * 0.65 + n2 * 0.35) * amp
}

/// Rasterizes independent polygons and unions them via elementwise `max`, so overlapping ribbons,
/// quads, splatters, or teeth never punch holes through each other.
pub fn rasterize_polygons_max(polys: &[PolyRing], bounds: Rect) -> Vec<f32> {
    let w = bounds.width() as usize;
    let h = bounds.height() as usize;
    let mut acc = vec![0.0_f32; w.saturating_mul(h)];
    if polys.is_empty() || w == 0 || h == 0 {
        return acc;
    }
    for ring in polys {
        if ring.len() < 3 {
            continue;
        }
        let bb = contour_aabb(ring);
        let rx0 = (bb[0].floor() as i32 - 1).max(bounds.x0);
        let ry0 = (bb[1].floor() as i32 - 1).max(bounds.y0);
        let rx1 = (bb[2].ceil() as i32 + 1).min(bounds.x1);
        let ry1 = (bb[3].ceil() as i32 + 1).min(bounds.y1);
        if rx0 >= rx1 || ry0 >= ry1 {
            continue;
        }
        let sub = Rect::new(rx0, ry0, rx1, ry1);
        let mut norm = ring.clone();
        if signed_area(&norm) < 0.0 {
            norm.reverse();
        }
        let mut ras = Rasterizer::new(false);
        ras.add_component(&[norm], PathOp::Combine, FillRule::NonZero);
        let cov = ras.render(sub);
        let sw = sub.width() as usize;
        let sh = sub.height() as usize;
        for row in 0..sh {
            let dst_y = (ry0 - bounds.y0) as usize + row;
            let dst_x0 = (rx0 - bounds.x0) as usize;
            let dst_off = dst_y * w + dst_x0;
            let src_off = row * sw;
            for x in 0..sw {
                let v = cov[src_off + x];
                if v > acc[dst_off + x] {
                    acc[dst_off + x] = v;
                }
            }
        }
    }
    acc
}

// =============================================================================
// 5. EXACT 2-PASS FELZENSZWALB EDT & DOCUMENT-SPACE EDGE KASURE BREAKUP
// =============================================================================

fn edt_1d(f: &[f32], d: &mut [f32], v: &mut [usize], z: &mut [f32], n: usize) {
    if n == 0 {
        return;
    }
    let mut k = 0usize;
    v[0] = 0;
    z[0] = -1e18;
    z[1] = 1e18;
    for q in 1..n {
        let qf = q as f32;
        let mut s = ((f[q] + qf * qf) - (f[v[k]] + (v[k] as f32) * (v[k] as f32))) / (2.0 * qf - 2.0 * v[k] as f32);
        while s <= z[k] && k > 0 {
            k -= 1;
            s = ((f[q] + qf * qf) - (f[v[k]] + (v[k] as f32) * (v[k] as f32))) / (2.0 * qf - 2.0 * v[k] as f32);
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = 1e18;
    }
    k = 0;
    for (q, dst) in d.iter_mut().enumerate().take(n) {
        let qf = q as f32;
        while z[k + 1] < qf {
            k += 1;
        }
        let dx = qf - v[k] as f32;
        *dst = dx * dx + f[v[k]];
    }
}

fn edt_2d_from_seeds(seeds: &[bool], w: usize, h: usize) -> Vec<f32> {
    let n = w * h;
    if n == 0 {
        return Vec::new();
    }
    const INF: f32 = 1e12;
    let mut grid = vec![INF; n];
    for (g, &is_seed) in grid.iter_mut().zip(seeds) {
        if is_seed {
            *g = 0.0;
        }
    }
    let max_dim = w.max(h);
    let mut f = vec![0.0_f32; max_dim];
    let mut d = vec![0.0_f32; max_dim];
    let mut v = vec![0usize; max_dim];
    let mut z = vec![0.0_f32; max_dim + 1];

    for x in 0..w {
        for y in 0..h {
            f[y] = grid[y * w + x];
        }
        edt_1d(&f, &mut d, &mut v, &mut z, h);
        for y in 0..h {
            grid[y * w + x] = d[y];
        }
    }
    for y in 0..h {
        let row = y * w;
        f[..w].copy_from_slice(&grid[row..row + w]);
        edt_1d(&f, &mut d, &mut v, &mut z, w);
        for x in 0..w {
            grid[row + x] = d[x].max(0.0).sqrt();
        }
    }
    grid
}

/// Exact signed distance field (`d_out - d_in`, positive outside ink, negative inside ink).
pub fn exact_sdf(cov: &[f32], w: usize, h: usize) -> Vec<f32> {
    let n = w * h;
    if n == 0 {
        return Vec::new();
    }
    let mut has_ink = false;
    let mut seed_ink = vec![false; n];
    let mut seed_air = vec![false; n];
    for i in 0..n {
        if cov[i] >= 0.45 {
            has_ink = true;
            seed_ink[i] = true;
        } else {
            seed_air[i] = true;
        }
    }
    if !has_ink {
        return vec![1e6; n];
    }
    let d_out = edt_2d_from_seeds(&seed_ink, w, h);
    let d_in = edt_2d_from_seeds(&seed_air, w, h);
    let mut sdf = vec![0.0_f32; n];
    for i in 0..n {
        sdf[i] = d_out[i] - d_in[i];
    }
    sdf
}

/// 1:1 port of `sfx_core.py:apply_edge_kasure_breakup` in **document coordinates** (matches GPU
/// `sfx.wgsl:fs_dry_grain_breakup` with `u0.x == 1` bit-for-bit, zero pan/zoom drift).
pub struct EdgeKasureParams {
    pub dryness_pct: f32,
    pub sharpness_pct: f32,
    pub seed: u32,
    pub core_protect_px: f32,
    pub one_bit: bool,
}

pub fn apply_edge_kasure_breakup_doc(cov: &mut [f32], bounds: Rect, params: EdgeKasureParams) {
    let w = bounds.width() as usize;
    let h = bounds.height() as usize;
    if w == 0 || h == 0 || cov.len() < w * h {
        return;
    }
    if params.one_bit {
        for c in cov.iter_mut() {
            *c = if *c >= 0.5 { 1.0 } else { 0.0 };
        }
    }
    let dryness = (params.dryness_pct / 100.0).clamp(0.0, 1.0);
    let sharp = (params.sharpness_pct / 100.0).clamp(0.0, 1.0);
    if dryness <= 0.005 {
        return;
    }

    let n = w * h;
    let mut seed_air = vec![false; n];
    let mut any_ink = false;
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let on_border = x == 0 || y == 0 || x + 1 == w || y + 1 == h;
            if cov[i] >= 0.45 && !on_border {
                any_ink = true;
            } else {
                seed_air[i] = true;
            }
        }
    }
    if !any_ink {
        return;
    }
    let d_in = edt_2d_from_seeds(&seed_air, w, h);
    let denom = 2.8 + dryness * 5.0;
    let core_skip = ((dryness - 0.55) / 0.45).clamp(0.0, 1.0) * 0.45;
    let thresh_scale = 0.88_f32;
    let slope = 3.0 + sharp * sharp * 55.0;

    for y in 0..h {
        let doc_y = (bounds.y0 as f32) + (y as f32) + 0.5;
        let row = y * w;
        for x in 0..w {
            let i = row + x;
            let cv = cov[i];
            if cv <= 1e-4 {
                continue;
            }
            let di = d_in[i];
            let edge_weight = (1.0 - ((di - 1.0) / denom).clamp(0.0, 1.0)).powf(0.85);
            let local_dry = (dryness * (edge_weight * 1.15 + core_skip)).clamp(0.0, 1.0);
            if local_dry <= 0.02 {
                continue;
            }
            let doc_x = (bounds.x0 as f32) + (x as f32) + 0.5;
            let grain = crisp_paper_grain(doc_x, doc_y, params.seed);
            let streak = value_noise_2d(doc_x * 0.16, doc_y * 0.72, params.seed.wrapping_add(47));
            let ink_score = 1.0 - (grain * 0.65 + streak * 0.35).clamp(0.0, 1.0);
            let thresh = local_dry * thresh_scale;
            let mut keep = if sharp >= 0.99 {
                if ink_score >= thresh { 1.0 } else { 0.0 }
            } else {
                ((ink_score - thresh) * slope + 0.5).clamp(0.0, 1.0)
            };
            if params.core_protect_px > 0.0 {
                let ramp = ((di - 1.0) / params.core_protect_px).clamp(0.0, 1.0).powf(0.8);
                keep = keep.max(ramp);
            }
            let mut out = cv * keep;
            if params.one_bit {
                out = if out >= 0.5 { 1.0 } else { 0.0 };
            }
            cov[i] = out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{classify_glyph_contours, normalize_glyph_winding_by_depth, signed_area};

    #[test]
    fn nested_3_level_glyph_classifies_outer_hole_island_by_depth_parity() {
        // Simulates 回 / ◎: level 0 outer square (0..100), level 1 hole (20..80), level 2 inner island (35..65).
        // Intentionally give ALL three the SAME winding to prove classification uses depth parity, not area sign!
        let outer = vec![(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)];
        let hole = vec![(20.0, 20.0), (80.0, 20.0), (80.0, 80.0), (20.0, 80.0)];
        let island = vec![(35.0, 35.0), (65.0, 35.0), (65.0, 65.0), (35.0, 65.0)];
        let mut contours = vec![outer, hole, island];

        let classes = classify_glyph_contours(&contours);
        assert_eq!(classes[0].depth, 0);
        assert!(!classes[0].is_hole, "depth 0 must be outer");
        assert_eq!(classes[1].depth, 1);
        assert!(classes[1].is_hole, "depth 1 must be hole");
        assert_eq!(classes[2].depth, 2);
        assert!(!classes[2].is_hole, "depth 2 inner island must NOT become a hole");

        normalize_glyph_winding_by_depth(&mut contours);
        assert!(signed_area(&contours[0]) > 0.0, "outer ring must be positive winding");
        assert!(signed_area(&contours[1]) < 0.0, "hole ring must be negative winding");
        assert!(signed_area(&contours[2]) > 0.0, "inner island must be positive winding");
    }
}
