//! PaintTool SAI-style Vector Curve & Weight tool geometry (`crates/vector/src/sai_curve.rs`).
//!
//! Fixes the critical defects found in the legacy prototype and implements
//! authentic PaintTool SAI curve physics:
//! 1. **Tight SAI Spline Physics & Neighbor Corner Clamping**:
//!    - In PaintTool SAI, the Curve tool has a tighter, more controlled feel (`SAI_DEFAULT_TENSION = 0.65`)
//!      than a standard loose Catmull-Rom spline (`1.0`), so curves don't wildly overshoot.
//!    - When an adjacent node `i - 1` or `i + 1` is marked `corner = true` (or is an endpoint),
//!      tangent handles damp toward the chord so a sharp corner doesn't cause neighboring smooth
//!      spans to bulge out.
//! 2. **Segment Hit-Testing & Subdivision (`Ctrl + Click` on Curve)**:
//!    - Provides `closest_point_on_curve` and `insert_node_on_curve` so `Ctrl + LMB` on any existing
//!      curve segment inserts a new `SaiPoint` at the exact projected position `(x, y)` with
//!      smoothly interpolated pressure `P(t)`.
//! 3. **Smooth Bézier Subdivision before Normal Offset**:
//!    - Subdivides each cubic Bézier segment by chord length (`<= 1.5 px`) before offset envelope
//!      generation, completely eliminating faceted straight segments when `tapered: true`.
//! 4. **Live Vector Stroke Parametrization**:
//!    - Preserves `LiveVectorStroke` metadata so the Weight and Edit tools can
//!      insert/delete nodes, move points, or adjust pressures non-destructively at any time.

use photocraft_doc::{Knot, Path, PathOp, Subpath};
use photocraft_geom::Point;
use serde::{Deserialize, Serialize};

/// 2D point tuple `(x, y)` in document pixels.
pub type P = (f64, f64);

/// Default PaintTool SAI curve stiffness (`0.65` gives the signature tight, non-overshooting SAI feel).
pub const SAI_DEFAULT_TENSION: f64 = 0.65;

/// One click-placed node of a PaintTool SAI vector stroke.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SaiPoint {
    pub p: P,
    /// Simulated pen pressure, `0.0..=2.0` (`1.0` is 100% base width; `0.0` tapers to `min_ratio`).
    pub pressure: f32,
    /// Sharp corner instead of a smoothed Catmull-Rom node (Alt-click toggles this in the UI).
    pub corner: bool,
}

impl SaiPoint {
    #[inline]
    pub fn new(p: P) -> Self {
        Self { p, pressure: 1.0, corner: false }
    }

    #[inline]
    pub fn with_pressure(p: P, pressure: f32, corner: bool) -> Self {
        Self { p, pressure: pressure.clamp(0.0, 2.0), corner }
    }
}

/// Result of projecting a cursor position onto a `LiveVectorStroke` spline (for `Ctrl + Click` insertion).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurveHitProjection {
    /// Segment index `i` (between node `i` and `(i + 1) % n`).
    pub segment_index: usize,
    /// Parameter `t in 0.0..=1.0` along segment `segment_index`.
    pub t: f64,
    /// Exact projected point `(x, y)` on the cubic Bézier spline.
    pub projected_point: P,
    /// Interpolated pen pressure `P(t)` at the projected point.
    pub interpolated_pressure: f32,
    /// Euclidean distance in document pixels from the cursor to `projected_point`.
    pub distance: f64,
}

/// Persistent parametric data stored so the stroke remains
/// editable with the Weight and Edit tools after creation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveVectorStroke {
    pub nodes: Vec<SaiPoint>,
    pub base_width: f64,
    pub min_ratio: f64,
    pub tapered: bool,
    pub closed: bool,
    pub tension: f64,
}

impl LiveVectorStroke {
    pub fn new(nodes: Vec<SaiPoint>, base_width: f64, tapered: bool) -> Self {
        Self { nodes, base_width: base_width.clamp(0.1, 100_000.0), min_ratio: 0.05, tapered, closed: false, tension: SAI_DEFAULT_TENSION }
    }

    /// Rebuilds the vector `Path` from the current live parameters.
    pub fn rebuild_path(&self) -> Path {
        if self.tapered {
            tapered_curve_path(&self.nodes, self.base_width, self.min_ratio, self.closed, self.tension)
        } else {
            sai_stroke_with_tension(&self.nodes, self.closed, self.tension)
        }
    }

    /// Inserts a new smooth node on the closest curve segment within `max_dist_px` (used by `Ctrl + Click`).
    /// Returns the index of the newly inserted node if within hit distance.
    pub fn insert_node_at_pos(&mut self, pos: P, max_dist_px: f64) -> Option<usize> {
        insert_node_on_curve(&mut self.nodes, pos, self.closed, self.tension, max_dist_px)
    }
}

/// A sampled point along the discretized spline centerline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurveSample {
    pub p: P,
    pub tangent: P,
    pub normal: P,
    pub pressure: f32,
    pub half_width: f64,
    pub is_corner: bool,
    pub segment_index: usize,
    pub t: f64,
}

// ---------------------------------------------------------------------------------------------
// Vector math helpers
// ---------------------------------------------------------------------------------------------

#[inline]
fn add(a: P, b: P) -> P {
    (a.0 + b.0, a.1 + b.1)
}

#[inline]
fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}

#[inline]
fn mul(a: P, s: f64) -> P {
    (a.0 * s, a.1 * s)
}

#[inline]
fn dot(a: P, b: P) -> f64 {
    a.0 * b.0 + a.1 * b.1
}

#[inline]
fn cross(a: P, b: P) -> f64 {
    a.0 * b.1 - a.1 * b.0
}

#[inline]
fn len(a: P) -> f64 {
    a.0.hypot(a.1)
}

#[inline]
fn dist(a: P, b: P) -> f64 {
    len(sub(b, a))
}

#[inline]
fn norm(a: P) -> P {
    let l = len(a);
    if l <= 1e-9 || !l.is_finite() { (0.0, 0.0) } else { (a.0 / l, a.1 / l) }
}

#[inline]
fn left_normal(dir: P) -> P {
    (-dir.1, dir.0)
}

/// Evaluates a cubic Bézier curve at `t in 0.0..=1.0`.
#[inline]
pub fn eval_cubic(p0: P, p1: P, p2: P, p3: P, t: f64) -> P {
    let u = 1.0 - t;
    let tt = t * t;
    let uu = u * u;
    let uuu = uu * u;
    let ttt = tt * t;

    let x = uuu * p0.0 + 3.0 * uu * t * p1.0 + 3.0 * u * tt * p2.0 + ttt * p3.0;
    let y = uuu * p0.1 + 3.0 * uu * t * p1.1 + 3.0 * u * tt * p2.1 + ttt * p3.1;
    (x, y)
}

/// Evaluates the first derivative `dB/dt` of a cubic Bézier at `t in 0.0..=1.0`.
#[inline]
pub fn eval_cubic_derivative(p0: P, p1: P, p2: P, p3: P, t: f64) -> P {
    let u = 1.0 - t;
    let d0 = sub(p1, p0);
    let d1 = sub(p2, p1);
    let d2 = sub(p3, p2);

    let x = 3.0 * u * u * d0.0 + 6.0 * u * t * d1.0 + 3.0 * t * t * d2.0;
    let y = 3.0 * u * u * d0.1 + 6.0 * u * t * d1.1 + 3.0 * t * t * d2.1;
    (x, y)
}

/// Hermite smoothstep interpolation (`3t² - 2t³`) for organic pressure transitions.
#[inline]
pub fn interpolate_pressure(p_start: f32, p_end: f32, t: f64) -> f32 {
    let t = t.clamp(0.0, 1.0) as f32;
    let s = t * t * (3.0 - 2.0 * t);
    (p_start + (p_end - p_start) * s).clamp(0.0, 2.0)
}

/// Width of a tapered line at `pressure`, respecting `min_ratio` when `pressure == 0`.
#[inline]
pub fn width_at(base_width: f64, min_ratio: f64, pressure: f32) -> f64 {
    let base = base_width.abs().max(0.05);
    let min_r = min_ratio.clamp(0.0, 1.0);
    let p = f64::from(pressure.clamp(0.0, 2.0));
    let effective_ratio = if p <= 1.0 { min_r + (1.0 - min_r) * p } else { p };
    (base * effective_ratio).max(0.05)
}

// ---------------------------------------------------------------------------------------------
// PaintTool SAI Centripetal / Tight Spline Knots (with Neighbor Corner Clamping)
// ---------------------------------------------------------------------------------------------

/// Builds cubic Bézier knots through `points` with PaintTool SAI's tight curve feel.
///
/// Key differences from naive uniform Catmull-Rom:
/// 1. Default tension is `0.65` (`SAI_DEFAULT_TENSION`), making the curve hug the control
///    polygon more closely without floppy overshoots.
/// 2. Each handle length is strictly bounded to at most `0.38 * tension * chord_length` of its
///    own segment (`d_prev` for `in_ctrl`, `d_next` for `out_ctrl`), preventing loops when a
///    short segment is next to a very long segment.
/// 3. If an adjacent node (`prev` or `next`) is a sharp corner (`corner == true`), the tangent
///    partially damps toward the chord so a sharp corner doesn't cause the neighboring smooth
///    span to bulge out.
pub fn sai_knots(points: &[SaiPoint], closed: bool, tension: f64) -> Vec<Knot> {
    let n = points.len();
    let tension = tension.clamp(0.0, 1.0);
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![Knot::corner(points[0].p.0, points[0].p.1)];
    }

    let mut knots = Vec::with_capacity(n);
    for i in 0..n {
        let anchor = points[i].p;
        if points[i].corner {
            knots.push(Knot::corner(anchor.0, anchor.1));
            continue;
        }

        let prev_idx = if i == 0 { if closed { n - 1 } else { 0 } } else { i - 1 };
        let next_idx = if i + 1 >= n { if closed { 0 } else { n - 1 } } else { i + 1 };

        let prev = points[prev_idx].p;
        let next = points[next_idx].p;

        let d_prev = dist(prev, anchor);
        let d_next = dist(anchor, next);

        let dir_in = if d_prev > 1e-6 { norm(sub(anchor, prev)) } else { (0.0, 0.0) };
        let dir_out = if d_next > 1e-6 { norm(sub(next, anchor)) } else { (0.0, 0.0) };

        // Centripetal bisector tangent direction: weight unit chord directions inversely by sqrt(chord)
        let w_in = if d_prev > 1e-6 { 1.0 / d_prev.sqrt() } else { 0.0 };
        let w_out = if d_next > 1e-6 { 1.0 / d_next.sqrt() } else { 0.0 };
        let tangent_dir = if w_in + w_out > 1e-6 { norm(add(mul(dir_in, w_in), mul(dir_out, w_out))) } else { (0.0, 0.0) };

        // SAI tight handle length: proportional to local segment length so short segments never loop
        let handle_factor = (tension / 3.0) * 1.15;
        let prev_corner_damp = if points[prev_idx].corner && prev_idx != i { 0.78 } else { 1.0 };
        let next_corner_damp = if points[next_idx].corner && next_idx != i { 0.78 } else { 1.0 };

        let in_len = d_prev * handle_factor * prev_corner_damp;
        let out_len = d_next * handle_factor * next_corner_damp;

        let in_ctrl = sub(anchor, mul(tangent_dir, in_len));
        let out_ctrl = add(anchor, mul(tangent_dir, out_len));
        knots.push(Knot::smooth(Point::new(anchor.0, anchor.1), Point::new(in_ctrl.0, in_ctrl.1), Point::new(out_ctrl.0, out_ctrl.1)));
    }
    knots
}

// ---------------------------------------------------------------------------------------------
// Segment Hit-Testing & Ctrl + Click Node Insertion on Existing Curve
// ---------------------------------------------------------------------------------------------

/// Finds the closest point on the cubic Bézier spline to `query_pos`.
///
/// Used in `egui` when the user holds `Ctrl` (or in `Edit` mode) to hover and `Ctrl + Click`
/// on a curve segment to insert a new node without deforming the curve.
pub fn closest_point_on_curve(points: &[SaiPoint], query_pos: P, closed: bool, tension: f64) -> Option<CurveHitProjection> {
    let n = points.len();
    if n < 2 {
        return None;
    }

    let knots = sai_knots(points, closed, tension);
    let seg_count = if closed { n } else { n - 1 };
    let mut best: Option<CurveHitProjection> = None;

    for seg in 0..seg_count {
        let next_idx = (seg + 1) % n;
        let k0 = knots[seg];
        let k1 = knots[next_idx];
        let p0 = (k0.anchor.x, k0.anchor.y);
        let p1 = (k0.out_ctrl.x, k0.out_ctrl.y);
        let p2 = (k1.in_ctrl.x, k1.in_ctrl.y);
        let p3 = (k1.anchor.x, k1.anchor.y);

        // Coarse search (32 subdivisions) followed by local ternary refinement
        let steps = 32;
        let mut best_seg_t = 0.5;
        let mut best_seg_d2 = f64::INFINITY;

        for s in 0..=steps {
            // Avoid inserting right on top of existing endpoints (`t < 0.04` or `t > 0.96`)
            let t = (s as f64 / steps as f64).clamp(0.04, 0.96);
            let pt = eval_cubic(p0, p1, p2, p3, t);
            let dx = pt.0 - query_pos.0;
            let dy = pt.1 - query_pos.1;
            let d2 = dx * dx + dy * dy;
            if d2 < best_seg_d2 {
                best_seg_d2 = d2;
                best_seg_t = t;
            }
        }

        // Local refinement around `best_seg_t`
        let mut left_t = (best_seg_t - 1.0 / steps as f64).max(0.03);
        let mut right_t = (best_seg_t + 1.0 / steps as f64).min(0.97);
        for _ in 0..8 {
            let m1 = left_t + (right_t - left_t) / 3.0;
            let m2 = right_t - (right_t - left_t) / 3.0;
            let d1 = dist(eval_cubic(p0, p1, p2, p3, m1), query_pos);
            let d2 = dist(eval_cubic(p0, p1, p2, p3, m2), query_pos);
            if d1 < d2 {
                right_t = m2;
            } else {
                left_t = m1;
            }
        }

        let final_t = (left_t + right_t) * 0.5;
        let projected_point = eval_cubic(p0, p1, p2, p3, final_t);
        let distance = dist(projected_point, query_pos);
        let interpolated_pressure = interpolate_pressure(points[seg].pressure, points[next_idx].pressure, final_t);

        if best.as_ref().is_none_or(|b| distance < b.distance) {
            best = Some(CurveHitProjection { segment_index: seg, t: final_t, projected_point, interpolated_pressure, distance });
        }
    }

    best
}

/// Inserts a new `SaiPoint` onto the spline at `query_pos` if the cursor is within `max_dist_px`
/// of any curve segment. Returns `Some(inserted_node_index)` on success.
pub fn insert_node_on_curve(points: &mut Vec<SaiPoint>, query_pos: P, closed: bool, tension: f64, max_dist_px: f64) -> Option<usize> {
    let hit = closest_point_on_curve(points, query_pos, closed, tension)?;
    if hit.distance > max_dist_px {
        return None;
    }

    let insert_idx = hit.segment_index + 1;
    let new_node = SaiPoint::with_pressure(hit.projected_point, hit.interpolated_pressure, false);
    if insert_idx <= points.len() {
        points.insert(insert_idx, new_node);
        Some(insert_idx)
    } else {
        points.push(new_node);
        Some(points.len() - 1)
    }
}

/// Centerline `Path` through click-placed points (used when `tapered == false` or for overlay wireframe).
pub fn sai_stroke(points: &[SaiPoint], closed: bool) -> Path {
    sai_stroke_with_tension(points, closed, SAI_DEFAULT_TENSION)
}

pub fn sai_stroke_with_tension(points: &[SaiPoint], closed: bool, tension: f64) -> Path {
    if points.len() < 2 {
        return Path::default();
    }
    let knots = sai_knots(points, closed, tension);
    Path::new(vec![Subpath { closed, knots, op: PathOp::Combine }])
}

// ---------------------------------------------------------------------------------------------
// Adaptive Spline Discretization & Variable-Width Envelope Generation
// ---------------------------------------------------------------------------------------------

fn estimate_cubic_length(p0: P, p1: P, p2: P, p3: P) -> f64 {
    let mut length = 0.0;
    let mut prev = p0;
    for step in 1..=4 {
        let t = step as f64 / 4.0;
        let curr = eval_cubic(p0, p1, p2, p3, t);
        length += dist(prev, curr);
        prev = curr;
    }
    length
}

fn safe_cubic_tangent(p0: P, p1: P, p2: P, p3: P, t: f64) -> P {
    let deriv = eval_cubic_derivative(p0, p1, p2, p3, t);
    let n_deriv = norm(deriv);
    if n_deriv != (0.0, 0.0) {
        return n_deriv;
    }
    let eps = 1e-3;
    let t_clamped = t.clamp(eps, 1.0 - eps);
    let deriv2 = eval_cubic_derivative(p0, p1, p2, p3, t_clamped);
    let n_deriv2 = norm(deriv2);
    if n_deriv2 != (0.0, 0.0) {
        return n_deriv2;
    }
    let chord = norm(sub(p3, p0));
    if chord != (0.0, 0.0) { chord } else { (1.0, 0.0) }
}

/// Samples the cubic Bézier spline into dense `CurveSample` points with interpolated pressure
/// and unit normals.
pub fn sample_sai_curve(points: &[SaiPoint], base_width: f64, min_ratio: f64, closed: bool, tension: f64, max_step_px: f64) -> Vec<CurveSample> {
    let n = points.len();
    if n < 2 {
        return Vec::new();
    }

    let knots = sai_knots(points, closed, tension);
    let seg_count = if closed { n } else { n - 1 };
    let step_px = max_step_px.clamp(0.5, 16.0);
    let mut samples = Vec::with_capacity(seg_count * 16 + 1);

    for seg in 0..seg_count {
        let next_idx = (seg + 1) % n;
        let k0 = knots[seg];
        let k1 = knots[next_idx];
        let p0 = (k0.anchor.x, k0.anchor.y);
        let p1 = (k0.out_ctrl.x, k0.out_ctrl.y);
        let p2 = (k1.in_ctrl.x, k1.in_ctrl.y);
        let p3 = (k1.anchor.x, k1.anchor.y);

        let is_straight = !k0.smooth && !k1.smooth;
        let arc_len = estimate_cubic_length(p0, p1, p2, p3);
        let subdivisions = if is_straight {
            let pressure_delta = (points[seg].pressure - points[next_idx].pressure).abs();
            if pressure_delta > 0.01 { ((arc_len / (step_px * 2.0)).ceil() as usize).clamp(4, 64) } else { 1 }
        } else {
            ((arc_len / step_px).ceil() as usize).clamp(8, 96)
        };

        let start_step = if seg == 0 || points[seg].corner { 0 } else { 1 };
        for s in start_step..=subdivisions {
            let t = s as f64 / subdivisions as f64;
            let pos = eval_cubic(p0, p1, p2, p3, t);
            let tangent = safe_cubic_tangent(p0, p1, p2, p3, t);
            let normal = left_normal(tangent);
            let pressure = interpolate_pressure(points[seg].pressure, points[next_idx].pressure, t);
            let half_width = width_at(base_width, min_ratio, pressure) * 0.5;
            let is_corner = (s == 0 && points[seg].corner) || (s == subdivisions && points[next_idx].corner);

            samples.push(CurveSample { p: pos, tangent, normal, pressure, half_width, is_corner, segment_index: seg, t });
        }
    }

    samples
}

fn miter_offset(n1: P, n2: P, half_width: f64, miter_limit: f64) -> Option<P> {
    let sum = add(n1, n2);
    let sum_len = len(sum);
    if sum_len <= 1e-6 {
        return None;
    }
    let miter_dir = mul(sum, 1.0 / sum_len);
    let cos_half = dot(miter_dir, n1);
    if cos_half <= 1e-4 {
        return None;
    }
    let scale = 1.0 / cos_half;
    if scale > miter_limit { None } else { Some(mul(miter_dir, half_width * scale)) }
}

/// Builds a smooth, closed polygon outline representing a variable-width stroke through `points`.
pub fn tapered_outline(points: &[SaiPoint], base_width: f64, min_ratio: f64) -> Vec<P> {
    tapered_outline_ex(points, base_width, min_ratio, false, SAI_DEFAULT_TENSION, 1.5)
}

pub fn tapered_outline_ex(points: &[SaiPoint], base_width: f64, min_ratio: f64, closed: bool, tension: f64, max_step_px: f64) -> Vec<P> {
    let samples = sample_sai_curve(points, base_width, min_ratio, closed, tension, max_step_px);
    let m = samples.len();
    if m < 2 {
        return Vec::new();
    }

    const MITER_LIMIT: f64 = 2.75;
    let mut left: Vec<P> = Vec::with_capacity(m + 16);
    let mut right: Vec<P> = Vec::with_capacity(m + 16);

    let mut i = 0;
    while i < m {
        let s = samples[i];
        if i + 1 < m && dist(s.p, samples[i + 1].p) < 1e-5 && s.is_corner {
            let s_in = s;
            let s_out = samples[i + 1];
            let hw = (s_in.half_width + s_out.half_width) * 0.5;
            let turn = cross(s_in.tangent, s_out.tangent);
            let miter = miter_offset(s_in.normal, s_out.normal, hw, MITER_LIMIT);
            let inner_clamp = miter_offset(s_in.normal, s_out.normal, hw, 1.35);

            if turn >= 0.0 {
                let inner_off = inner_clamp.unwrap_or_else(|| mul(norm(add(s_in.normal, s_out.normal)), hw));
                left.push(add(s_in.p, inner_off));
                if let Some(m_vec) = miter {
                    right.push(sub(s_in.p, m_vec));
                } else {
                    right.push(sub(s_in.p, mul(s_in.normal, hw)));
                    right.push(sub(s_out.p, mul(s_out.normal, hw)));
                }
            } else {
                let inner_off = inner_clamp.unwrap_or_else(|| mul(norm(add(s_in.normal, s_out.normal)), hw));
                right.push(sub(s_in.p, inner_off));
                if let Some(m_vec) = miter {
                    left.push(add(s_in.p, m_vec));
                } else {
                    left.push(add(s_in.p, mul(s_in.normal, hw)));
                    left.push(add(s_out.p, mul(s_out.normal, hw)));
                }
            }
            i += 2;
            continue;
        }

        let normal = if i > 0 && i + 1 < m && !s.is_corner {
            let n_avg = norm(add(samples[i - 1].normal, add(s.normal, samples[i + 1].normal)));
            if n_avg != (0.0, 0.0) { n_avg } else { s.normal }
        } else {
            s.normal
        };

        left.push(add(s.p, mul(normal, s.half_width)));
        right.push(sub(s.p, mul(normal, s.half_width)));
        i += 1;
    }

    let mut poly: Vec<P> = Vec::with_capacity(left.len() + right.len() + 24);
    poly.extend(left.iter().copied());

    if !closed {
        let last = samples[m - 1];
        let cap_dir = last.tangent;
        let cap_normal = last.normal;
        let cap_half = last.half_width;
        if cap_half > 0.15 {
            for step in 1..12 {
                let angle = std::f64::consts::PI * (step as f64 / 12.0);
                let offset = add(mul(cap_normal, cap_half * angle.cos()), mul(cap_dir, cap_half * angle.sin()));
                poly.push(add(last.p, offset));
            }
        }
    }

    poly.extend(right.iter().rev().copied());

    if !closed {
        let first = samples[0];
        let start_dir = mul(first.tangent, -1.0);
        let start_normal = mul(first.normal, -1.0);
        let start_half = first.half_width;
        if start_half > 0.15 {
            for step in 1..12 {
                let angle = std::f64::consts::PI * (step as f64 / 12.0);
                let offset = add(mul(start_normal, start_half * angle.cos()), mul(start_dir, start_half * angle.sin()));
                poly.push(add(first.p, offset));
            }
        }
    }

    clean_polygon(&poly)
}

fn clean_polygon(poly: &[P]) -> Vec<P> {
    let mut out: Vec<P> = Vec::with_capacity(poly.len());
    for &pt in poly {
        if !pt.0.is_finite() || !pt.1.is_finite() {
            continue;
        }
        if let Some(&last) = out.last()
            && dist(last, pt) < 1e-4
        {
            continue;
        }
        out.push(pt);
    }
    if out.len() > 2
        && let (Some(&first), Some(&last)) = (out.first(), out.last())
        && dist(first, last) < 1e-4
    {
        out.pop();
    }
    out
}

/// Builds a filled `Path` representing the variable-width PaintTool SAI curve.
pub fn tapered_curve_path(points: &[SaiPoint], base_width: f64, min_ratio: f64, closed: bool, tension: f64) -> Path {
    let outline = tapered_outline_ex(points, base_width, min_ratio, closed, tension, 1.5);
    if outline.len() < 3 { Path::default() } else { Path::new(vec![Subpath::polygon(&outline)]) }
}

/// Drop-in replacement for legacy `balloon::tapered_path`.
pub fn tapered_path(points: &[SaiPoint], base_width: f64, min_ratio: f64) -> Path {
    tapered_curve_path(points, base_width, min_ratio, false, SAI_DEFAULT_TENSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tapered_outline_subdivides_curve_smoothly() {
        let points = vec![
            SaiPoint::with_pressure((100.0, 300.0), 0.1, false),
            SaiPoint::with_pressure((220.0, 140.0), 1.0, false),
            SaiPoint::with_pressure((380.0, 140.0), 0.85, false),
            SaiPoint::with_pressure((500.0, 300.0), 0.1, false),
        ];

        let samples = sample_sai_curve(&points, 14.0, 0.05, false, SAI_DEFAULT_TENSION, 1.5);
        assert!(samples.len() > 80, "Expected dense cubic Bézier subdivision (>80 samples), got {}", samples.len());

        let mid_knot_a = points[1].p;
        let mid_knot_b = points[2].p;
        let chord_mid_y = (mid_knot_a.1 + mid_knot_b.1) * 0.5;
        let arch_samples: Vec<_> = samples.iter().filter(|s| s.segment_index == 1 && (s.t - 0.5).abs() < 0.15).collect();
        assert!(!arch_samples.is_empty());
        for s in arch_samples {
            assert!(s.p.1 < chord_mid_y - 3.0, "Sampled spline point {:?} must arch smoothly above straight chord y={}", s.p, chord_mid_y);
        }

        let outline = tapered_outline(&points, 14.0, 0.05);
        assert!(outline.len() > 160, "Tapered polygon outline should be smoothly discretized, got {} vertices", outline.len());
    }

    #[test]
    fn test_ctrl_click_inserts_node_on_curve_segment() {
        let mut points = vec![
            SaiPoint::with_pressure((100.0, 200.0), 0.2, false),
            SaiPoint::with_pressure((300.0, 100.0), 1.0, false),
            SaiPoint::with_pressure((500.0, 200.0), 0.4, false),
        ];

        let query = (200.0, 145.0);
        let hit = closest_point_on_curve(&points, query, false, SAI_DEFAULT_TENSION).expect("Should find closest projection on spline");
        assert_eq!(hit.segment_index, 0);
        assert!(hit.distance < 15.0);
        assert!(hit.interpolated_pressure > 0.25 && hit.interpolated_pressure < 0.95);

        let inserted_idx = insert_node_on_curve(&mut points, query, false, SAI_DEFAULT_TENSION, 20.0).expect("Ctrl+Click within 20px should insert a new node");
        assert_eq!(inserted_idx, 1);
        assert_eq!(points.len(), 4);
    }

    #[test]
    fn test_sai_tight_tension_reduces_overshoot_compared_to_loose_catmull() {
        let points = vec![
            SaiPoint::with_pressure((100.0, 300.0), 0.5, false),
            SaiPoint::with_pressure((220.0, 140.0), 1.0, false),
            SaiPoint::with_pressure((380.0, 140.0), 1.0, false),
            SaiPoint::with_pressure((500.0, 300.0), 0.5, false),
        ];

        let tight_knots = sai_knots(&points, false, SAI_DEFAULT_TENSION);
        let loose_knots = sai_knots(&points, false, 1.0);

        let tight_mid = eval_cubic(
            (tight_knots[1].anchor.x, tight_knots[1].anchor.y),
            (tight_knots[1].out_ctrl.x, tight_knots[1].out_ctrl.y),
            (tight_knots[2].in_ctrl.x, tight_knots[2].in_ctrl.y),
            (tight_knots[2].anchor.x, tight_knots[2].anchor.y),
            0.5,
        );
        let loose_mid = eval_cubic(
            (loose_knots[1].anchor.x, loose_knots[1].anchor.y),
            (loose_knots[1].out_ctrl.x, loose_knots[1].out_ctrl.y),
            (loose_knots[2].in_ctrl.x, loose_knots[2].in_ctrl.y),
            (loose_knots[2].anchor.x, loose_knots[2].anchor.y),
            0.5,
        );

        assert!(
            (tight_mid.1 - 140.0).abs() < (loose_mid.1 - 140.0).abs(),
            "Tight SAI curve ({}) must overshoot less than loose Catmull-Rom ({})",
            tight_mid.1,
            loose_mid.1
        );
    }

    #[test]
    fn test_hermite_pressure_interpolation_and_width() {
        let p0 = interpolate_pressure(0.2, 1.0, 0.0);
        let p_mid = interpolate_pressure(0.2, 1.0, 0.5);
        let p1 = interpolate_pressure(0.2, 1.0, 1.0);

        assert!((p0 - 0.2).abs() < 1e-5);
        assert!((p_mid - 0.6).abs() < 1e-5);
        assert!((p1 - 1.0).abs() < 1e-5);

        let min_w = width_at(20.0, 0.10, 0.0);
        let full_w = width_at(20.0, 0.10, 1.0);
        assert!((min_w - 2.0).abs() < 1e-6);
        assert!((full_w - 20.0).abs() < 1e-6);
    }

    #[test]
    fn test_corner_nodes_retract_handles_and_preserve_sharp_angle() {
        let points = vec![
            SaiPoint::with_pressure((50.0, 250.0), 0.8, false),
            SaiPoint::with_pressure((200.0, 80.0), 1.0, true),
            SaiPoint::with_pressure((350.0, 250.0), 0.8, false),
        ];

        let knots = sai_knots(&points, false, SAI_DEFAULT_TENSION);
        assert_eq!(knots.len(), 3);
        assert!(!knots[1].smooth, "Apex node must be marked non-smooth");
        assert_eq!(knots[1].anchor, knots[1].in_ctrl);
        assert_eq!(knots[1].anchor, knots[1].out_ctrl);

        let outline = tapered_outline(&points, 16.0, 0.05);
        assert!(outline.len() >= 20);
        for &(x, y) in &outline {
            assert!(x.is_finite() && y.is_finite(), "Outline vertex must be finite");
        }
    }

    #[test]
    fn test_live_vector_stroke_preserves_and_rebuilds_weights() {
        let nodes = vec![
            SaiPoint::with_pressure((10.0, 20.0), 0.3, false),
            SaiPoint::with_pressure((110.0, 60.0), 1.0, false),
            SaiPoint::with_pressure((210.0, 20.0), 0.4, false),
        ];
        let mut live = LiveVectorStroke::new(nodes, 12.0, true);
        let path_initial = live.rebuild_path();
        assert!(!path_initial.is_empty());

        live.nodes[1].pressure = 1.75;
        let path_updated = live.rebuild_path();
        assert!(!path_updated.is_empty());
        assert_ne!(path_initial, path_updated);
    }
}
