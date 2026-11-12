//! Balloon geometry for manga/comic lettering: editable tails, procedural balloon shapes and
//! mouse-friendly vector linework.
//!
//! Everything here produces plain [`Path`]s, so fill, stroke, Free Transform, history and export
//! keep working exactly as for the existing shape primitives. Nothing is rasterized until the
//! shape layer is composited.
//!
//! * [`BalloonTail`] + [`attach_tail`]: a tail merged with the balloon body by a real polygon
//!   union, so one fill and one stroke outline the whole balloon without a seam.
//! * [`cloud`]: a thought-cloud contour (scalloped bumps along an ellipse or rectangle).
//! * [`spiky`]: a shout/flash balloon with sharp spikes pointing out or in.
//! * [`catmull_rom_path`], [`sai_stroke`], [`tapered_outline`]: click-to-click vector linework in
//!   the Paint Tool SAI spirit С‚РђР¤ a click adds a node and the segment is smoothed automatically.
//!
//! Point-level editing of the result (Ctrl-drag, Alt-click toggling corners, deleting anchors)
//! already lives in [`crate::edit`], which works on any [`Path`] this module returns.

use std::f64::consts::TAU;

use photocraft_doc::vector::{LiveBalloon, LiveBalloonBody, LiveBalloonGroup, LiveBalloonTail, TailPreset};
use photocraft_doc::{FillRule, Knot, Path, PathOp, Subpath};
use photocraft_geom::Point;

use crate::comic_sai_curve::{LiveVectorStroke, SAI_DEFAULT_TENSION, SaiCurveType};
use crate::flatten::flatten_subpath;

/// A point in path space. Kept as a tuple internally; [`Path`] takes [`Point`] knots.
type P = (f64, f64);

/// Tolerance used when a curved balloon body is flattened for a boolean union (px).
const UNION_TOL: f64 = 0.05;
/// Points this close are treated as the same node when stitching a union boundary.
const JOIN_EPS: f64 = 1e-6;

// ---------------------------------------------------------------------------------------------
// Small tuple geometry helpers (no vector type of our own: `photocraft-geom` has no `Vec2`).
// ---------------------------------------------------------------------------------------------

fn add(a: P, b: P) -> P {
    (a.0 + b.0, a.1 + b.1)
}

fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}

fn mul(a: P, s: f64) -> P {
    (a.0 * s, a.1 * s)
}

fn cross(a: P, b: P) -> f64 {
    a.0 * b.1 - a.1 * b.0
}

fn len(a: P) -> f64 {
    a.0.hypot(a.1)
}

fn dist(a: P, b: P) -> f64 {
    len(sub(a, b))
}

fn norm(a: P) -> P {
    let l = len(a);
    if l < 1e-12 { (0.0, 0.0) } else { (a.0 / l, a.1 / l) }
}

fn lerp(a: P, b: P, t: f64) -> P {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

fn near(a: P, b: P) -> bool {
    dist(a, b) <= JOIN_EPS
}

fn to_point(p: P) -> Point {
    Point::new(p.0, p.1)
}

/// Deterministic pseudo-random value in `0..=1` from a seed and a node index (no RNG
/// dependency, and the same document always renders the same balloon).
fn hash01(seed: u32, index: u32) -> f64 {
    let mut x = seed ^ index.wrapping_mul(0x9E37_79B9);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^= x >> 16;
    f64::from(x) / f64::from(u32::MAX)
}

/// `value` scaled by a deterministic в”¬в–’`amount` jitter around 1.
fn jittered(value: f64, seed: u32, index: u32, amount: f64) -> f64 {
    value * (1.0 + amount * (hash01(seed, index) - 0.5) * 2.0)
}

// ---------------------------------------------------------------------------------------------
// Polygon union (the "С‚РРє" of a balloon body and its tail).
// ---------------------------------------------------------------------------------------------

/// Signed area of a closed polygon; positive is counter-clockwise in y-down path space.
fn signed_area(poly: &[P]) -> f64 {
    let n = poly.len();
    if n < 3 {
        return 0.0;
    }
    let mut area = 0.0;
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        area += a.0 * b.1 - b.0 * a.1;
    }
    area * 0.5
}

/// Drops repeated and closing points.
fn clean(poly: &[P]) -> Vec<P> {
    let mut out: Vec<P> = Vec::with_capacity(poly.len());
    for &p in poly {
        if out.last().is_none_or(|&q| dist(q, p) > JOIN_EPS) {
            out.push(p);
        }
    }
    while out.len() > 1 && dist(out[0], out[out.len() - 1]) <= JOIN_EPS {
        out.pop();
    }
    out
}

fn oriented(poly: &[P]) -> Vec<P> {
    let mut p = clean(poly);
    if signed_area(&p) < 0.0 {
        p.reverse();
    }
    p
}

/// Ray-casting point-in-polygon test; points exactly on the boundary are unspecified (callers
/// only test midpoints of pieces that do not lie on the other boundary by construction).
fn inside(p: P, poly: &[P]) -> bool {
    let n = poly.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        if (a.1 > p.1) != (b.1 > p.1) {
            let t = (p.1 - a.1) / (b.1 - a.1);
            let x = a.0 + t * (b.0 - a.0);
            if p.0 < x {
                inside = !inside;
            }
        }
    }
    inside
}

/// Proper interior crossing of two segments (endpoints and collinear overlaps are ignored; the
/// union only needs to split where one boundary truly enters the other).
fn crossing_param(a0: P, a1: P, b0: P, b1: P) -> Option<f64> {
    let r = sub(a1, a0);
    let s = sub(b1, b0);
    let denom = cross(r, s);
    if denom.abs() < 1e-12 {
        return None;
    }
    let qp = sub(b0, a0);
    let ta = cross(qp, s) / denom;
    let tb = cross(qp, r) / denom;
    let inside_segment = |t: f64| t > 1e-9 && t < 1.0 - 1e-9;
    if inside_segment(ta) && inside_segment(tb) { Some(ta) } else { None }
}

/// Splits every edge of `poly` at its crossings with `other`.
fn split_against(poly: &[P], other: &[P]) -> Vec<[P; 2]> {
    let n = poly.len();
    let m = other.len();
    let mut pieces = Vec::new();
    if n < 2 || m < 2 {
        return pieces;
    }
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        let mut cuts: Vec<f64> = vec![0.0, 1.0];
        for j in 0..m {
            if let Some(t) = crossing_param(a, b, other[j], other[(j + 1) % m]) {
                cuts.push(t);
            }
        }
        cuts.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
        for w in cuts.windows(2) {
            let (t0, t1) = (w[0], w[1]);
            if t1 - t0 > 1e-9 {
                pieces.push([lerp(a, b, t0), lerp(a, b, t1)]);
            }
        }
    }
    pieces
}

/// Chains directed boundary pieces into closed loops by matching their endpoints.
fn stitch(pieces: &[[P; 2]]) -> Vec<Vec<P>> {
    let mut pool: Vec<[P; 2]> = pieces.to_vec();
    let mut loops = Vec::new();
    while let Some(start) = pool.pop() {
        let mut poly = vec![start[0], start[1]];
        loop {
            let end = poly[poly.len() - 1];
            let Some(index) = pool.iter().position(|s| near(s[0], end)) else { break };
            let next = pool.swap_remove(index);
            let next_end = next[1];
            poly.push(next_end);
            if near(next_end, poly[0]) {
                break;
            }
        }
        poly.pop();
        let poly = clean(&poly);
        if poly.len() >= 3 && signed_area(&poly).abs() > JOIN_EPS {
            loops.push(poly);
        }
    }
    loops
}

/// Union of two simple closed polygons. Returns the boundary contours of the union (two contours
/// when the inputs do not touch, one when they overlap or one contains the other). The inputs are
/// treated as sets, so winding direction does not matter.
pub fn union_polygons(a: &[P], b: &[P]) -> Vec<Vec<P>> {
    let a = oriented(a);
    let b = oriented(b);
    if a.len() < 3 {
        return if b.len() >= 3 { vec![b] } else { Vec::new() };
    }
    if b.len() < 3 {
        return vec![a];
    }

    let mut pieces: Vec<[P; 2]> = Vec::new();
    for seg in split_against(&a, &b) {
        if !inside(lerp(seg[0], seg[1], 0.5), &b) {
            pieces.push(seg);
        }
    }
    for seg in split_against(&b, &a) {
        if !inside(lerp(seg[0], seg[1], 0.5), &a) {
            pieces.push(seg);
        }
    }
    if pieces.is_empty() {
        // Identical or fully coincident boundaries: nothing outside either polygon.
        return vec![a];
    }
    let loops = stitch(&pieces);
    if loops.is_empty() { vec![a] } else { loops }
}

/// Whether two closed polygons overlap or one contains the other (used to decide whether a new
/// tail joins an existing contour or becomes its own).
fn overlaps(a: &[P], b: &[P]) -> bool {
    if a.len() < 3 || b.len() < 3 {
        return false;
    }
    if inside(a[0], b) || inside(b[0], a) {
        return true;
    }
    for i in 0..a.len() {
        let a0 = a[i];
        let a1 = a[(i + 1) % a.len()];
        for j in 0..b.len() {
            if crossing_param(a0, a1, b[j], b[(j + 1) % b.len()]).is_some() {
                return true;
            }
        }
    }
    false
}

/// Merges one more shape into the evolving set of union contours.
fn merge_shape(contours: &mut Vec<Vec<P>>, shape: Vec<P>) {
    let shape = oriented(&shape);
    if shape.len() < 3 {
        return;
    }
    let mut i = 0;
    while i < contours.len() {
        if overlaps(&contours[i], &shape) {
            let merged = union_polygons(&contours[i], &shape);
            contours.remove(i);
            for (offset, contour) in merged.into_iter().enumerate() {
                contours.insert(i + offset, contour);
            }
            return;
        }
        i += 1;
    }
    contours.push(shape);
}

// ---------------------------------------------------------------------------------------------
// Stage 1: balloon tails and their union with the body.
// ---------------------------------------------------------------------------------------------

/// Prebuilt tail styles, mirroring the common manga tail vocabulary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LegacyTailPreset {
    /// Straight tapered wedge (the everyday speech tail).
    #[default]
    Wedge,
    /// Wedge whose sides bow outward, for a softer, hand-drawn look.
    Arc,
    /// Two thin tails leaving the same spot (one speaker speaking two lines, or emphasis).
    Double,
    /// A trail of shrinking circles: the thought-balloon tail.
    Thought,
}

impl LegacyTailPreset {
    /// Stable id for UI presets and the control protocol.
    pub const fn id(self) -> &'static str {
        match self {
            LegacyTailPreset::Wedge => "wedge",
            LegacyTailPreset::Arc => "arc",
            LegacyTailPreset::Double => "double",
            LegacyTailPreset::Thought => "thought",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "wedge" => Some(LegacyTailPreset::Wedge),
            "arc" => Some(LegacyTailPreset::Arc),
            "double" => Some(LegacyTailPreset::Double),
            "thought" => Some(LegacyTailPreset::Thought),
            _ => None,
        }
    }

    /// The offset of the two attachment points relative to the tail axis, in multiples of the
    /// base width: the tail's mouth on the balloon contour.
    pub const fn mouth(self) -> f64 {
        match self {
            LegacyTailPreset::Wedge | LegacyTailPreset::Arc => 0.5,
            LegacyTailPreset::Double => 0.35,
            LegacyTailPreset::Thought => 0.0,
        }
    }
}

/// A tail dragged from a balloon body towards the speaker.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BalloonTail {
    pub preset: LegacyTailPreset,
    /// The two points where the tail meets the balloon contour.
    pub base: [P; 2],
    /// Where the tail points (the character or the words).
    pub tip: P,
    /// Side bulge, `0..=1`; ignored by [`LegacyTailPreset::Thought`].
    pub curve: f64,
    /// Thought preset: number of bubbles in the trail.
    pub bubbles: u32,
    /// Thought preset: radius of the first (largest) bubble in px.
    pub bubble_radius: f64,
}

impl BalloonTail {
    /// A wedge tail between two attachment points and a tip.
    pub fn wedge(base: [P; 2], tip: P) -> Self {
        Self { preset: LegacyTailPreset::Wedge, base, tip, curve: 0.12, bubbles: 3, bubble_radius: 6.0 }
    }

    /// An arc tail between two attachment points and a tip.
    pub fn arc(base: [P; 2], tip: P) -> Self {
        Self { preset: LegacyTailPreset::Arc, base, tip, curve: 0.45, bubbles: 3, bubble_radius: 6.0 }
    }

    /// A thought-bubble trail leaving `base` towards `tip`.
    pub fn thought(base: [P; 2], tip: P) -> Self {
        Self { preset: LegacyTailPreset::Thought, base, tip, curve: 0.0, bubbles: 3, bubble_radius: 6.0 }
    }

    /// One tail built from a drag: the mouth is perpendicular to the drag direction at `start`,
    /// `mouth_width` wide, and the tail points towards `tip`.
    pub fn from_drag(preset: LegacyTailPreset, start: P, tip: P, mouth_width: f64) -> Self {
        let axis = norm(sub(tip, start));
        let side = (-axis.1, axis.0);
        let half = mouth_width.abs() * 0.5;
        let base = [sub(start, mul(side, half)), add(start, mul(side, half))];
        let curve = match preset {
            LegacyTailPreset::Wedge => 0.12,
            LegacyTailPreset::Arc => 0.45,
            LegacyTailPreset::Double | LegacyTailPreset::Thought => 0.0,
        };
        Self { preset, base, tip, curve, bubbles: 3, bubble_radius: 6.0 }
    }
}

/// Shape of one tail polygon: sampled quadratic sides bowed away from the tail's centroid.
fn tail_contour(b0: P, b1: P, tip: P, bulge: f64) -> Vec<P> {
    let steps = 14usize;
    let centroid = mul(add(add(b0, b1), tip), 1.0 / 3.0);
    let mut pts = vec![b0];
    // b0 -> tip
    pts.extend(quad_side(b0, tip, bulge, steps, centroid).into_iter().skip(1));
    // tip -> b1 (the sample already contains the tip)
    pts.extend(quad_side(tip, b1, bulge, steps, centroid).into_iter().skip(1));
    pts
}

/// Quadratic Bв”њР№zier from `s` to `e` with a control point pushed away from `centroid`.
fn quad_side(s: P, e: P, bulge: f64, steps: usize, centroid: P) -> Vec<P> {
    let mid = lerp(s, e, 0.5);
    let mut away = norm(sub(mid, centroid));
    if away == (0.0, 0.0) {
        let d = norm(sub(e, s));
        away = (-d.1, d.0);
    }
    let ctrl = add(mid, mul(away, bulge.clamp(0.0, 1.0) * dist(s, e) * 0.5));
    (0..=steps)
        .map(|i| {
            let t = i as f64 / steps as f64;
            let mt = 1.0 - t;
            add(add(mul(s, mt * mt), mul(ctrl, 2.0 * mt * t)), mul(e, t * t))
        })
        .collect()
}

fn circle_contour(center: P, radius: f64, steps: usize) -> Vec<P> {
    (0..steps)
        .map(|i| {
            let a = TAU * i as f64 / steps as f64;
            (center.0 + radius * a.cos(), center.1 + radius * a.sin())
        })
        .collect()
}

/// Flattens the closed subpaths of `path` into polygons (open subpaths are ignored).
fn closed_contours(path: &Path) -> Vec<Vec<P>> {
    let mut out = Vec::new();
    for subpath in &path.subpaths {
        if !subpath.closed || subpath.knots.len() < 3 {
            continue;
        }
        let flat = flatten_subpath(subpath, UNION_TOL);
        let poly = clean(&flat.pts);
        if poly.len() >= 3 {
            out.push(poly);
        }
    }
    out
}

/// Pushes a point towards `toward` by `amount` (used to bury the tail mouth inside the body so the
/// union cuts it away cleanly instead of leaving a seam on the contour).
fn push_inside(p: P, toward: P, amount: f64) -> P {
    let d = norm(sub(toward, p));
    if d == (0.0, 0.0) { p } else { add(p, mul(d, amount)) }
}

fn centroid(poly: &[P]) -> P {
    let n = poly.len();
    if n == 0 {
        return (0.0, 0.0);
    }
    let mut c = (0.0, 0.0);
    for &p in poly {
        c = add(c, p);
    }
    mul(c, 1.0 / n as f64)
}

/// Contours of the tail alone (one for wedge/arc, two for double, one per bubble for thought).
fn tail_shapes(tail: &BalloonTail, mouth_inset: f64) -> Vec<Vec<P>> {
    let mut base = tail.base;
    if tail.preset == LegacyTailPreset::Thought {
        // Bubbles leave the midpoint of the mouth and shrink towards the tip.
        let start = lerp(base[0], base[1], 0.5);
        let count = tail.bubbles.clamp(1, 12) as usize;
        let axis = norm(sub(tail.tip, start));
        let radius0 = tail.bubble_radius.max(0.5);
        return (0..count)
            .map(|i| {
                let t = (i + 1) as f64 / (count + 1) as f64;
                let center = add(lerp(start, tail.tip, t), mul(axis, radius0 * 0.6));
                let radius = radius0 * (1.0 - 0.55 * t);
                circle_contour(center, radius, 32)
            })
            .collect();
    }
    if mouth_inset > 0.0 {
        let body_center = lerp(base[0], base[1], 0.5);
        let away = norm(sub(body_center, tail.tip));
        base = [add(base[0], mul(away, mouth_inset)), add(base[1], mul(away, mouth_inset))];
    }
    match tail.preset {
        LegacyTailPreset::Double => {
            let axis = norm(sub(tail.tip, lerp(base[0], base[1], 0.5)));
            let side = (-axis.1, axis.0);
            let spread = dist(base[0], base[1]).max(1.0) * 0.3;
            let tip_a = add(tail.tip, mul(side, spread));
            let tip_b = sub(tail.tip, mul(side, spread));
            vec![tail_contour(base[0], lerp(base[0], base[1], 0.5), tip_a, tail.curve), tail_contour(lerp(base[0], base[1], 0.5), base[1], tip_b, tail.curve)]
        }
        _ => vec![tail_contour(base[0], base[1], tail.tip, tail.curve)],
    }
}

/// Attaches `tail` to `body` and returns the merged path: one contour (or several, for thought
/// bubbles), one fill, one stroke, no line where the tail meets the body.
///
/// The body's curves are flattened at [`UNION_TOL`] px for the boolean step С‚РђР¤ visually
/// indistinguishable at screen and print resolutions С‚РђР¤ and the merged result is a normal editable
/// path, so Free Transform, Direct Selection and PSD/`.pcraft` export are unaffected.
pub fn attach_tail(body: &Path, tail: &BalloonTail) -> Path {
    let mut contours = closed_contours(body);
    let open: Vec<Subpath> = body.subpaths.iter().filter(|s| !s.closed).cloned().collect();
    let shape_poly = contours.first().map(|c| centroid(c));
    let mouth_inset = shape_poly.map(|c| (dist(c, tail.tip) * 0.02).clamp(0.25, 8.0)).unwrap_or(0.0);
    let body_center = shape_poly.unwrap_or_else(|| lerp(tail.base[0], tail.base[1], 0.5));

    let mut base = tail.base;
    if tail.preset != LegacyTailPreset::Thought && mouth_inset > 0.0 {
        base = [push_inside(base[0], body_center, mouth_inset), push_inside(base[1], body_center, mouth_inset)];
    }
    let mut tail = *tail;
    tail.base = base;

    if contours.is_empty() {
        // No body to merge with: return the tail alone.
        let mut path = Path::new(Vec::new());
        for shape in tail_shapes(&tail, 0.0) {
            path.subpaths.push(Subpath::polygon(&shape));
        }
        path.subpaths.extend(open);
        return path;
    }

    let mut merged: Vec<Vec<P>> = Vec::new();
    if let Some(first) = contours.first().cloned() {
        merged.push(first);
    }
    for shape in tail_shapes(&tail, mouth_inset) {
        merge_shape(&mut merged, shape);
    }
    // Contours are one connected balloon; keep any extra body contours as they were.
    for contour in contours.drain(1..) {
        merged.push(contour);
    }

    let mut path = Path::new(Vec::new());
    for contour in merged {
        if contour.len() >= 3 {
            path.subpaths.push(Subpath::polygon(&contour));
        }
    }
    path.subpaths.extend(open);
    path.fill_rule = FillRule::NonZero;
    path
}

// ---------------------------------------------------------------------------------------------
// Stage 2: procedural balloons.
// ---------------------------------------------------------------------------------------------

/// Parameters of a thought cloud.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloudParams {
    /// Number of scalloped bumps around the contour (3..=64).
    pub bumps: u32,
    /// Bump height as a fraction of the balloon's smaller radius (0..=1).
    pub amplitude: f64,
    /// Per-bump size variation, 0 = perfectly regular (0..=1).
    pub jitter: f64,
    /// Seed of the deterministic jitter.
    pub seed: u32,
}

impl Default for CloudParams {
    fn default() -> Self {
        Self { bumps: 9, amplitude: 0.18, jitter: 0.35, seed: 1 }
    }
}

/// Parameters of a spiky shout/flash balloon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpikeParams {
    /// Number of spikes (3..=128).
    pub spikes: u32,
    /// Spike length as a fraction of the balloon radius (0..=2).
    pub length: f64,
    /// Per-spike length variation (0..=1).
    pub jitter: f64,
    /// Spikes point towards the centre (a burst-in outline) instead of outwards.
    pub inward: bool,
    /// Smooth the spikes into rounded rays instead of sharp corners.
    pub smooth: bool,
    pub seed: u32,
}

impl Default for SpikeParams {
    fn default() -> Self {
        Self { spikes: 12, length: 0.45, jitter: 0.35, inward: false, smooth: false, seed: 1 }
    }
}

fn rect_ellipse(rect: [f64; 4]) -> (P, f64, f64) {
    let [x, y, w, h] = rect;
    let (cx, cy) = (x + w * 0.5, y + h * 0.5);
    ((cx, cy), (w.abs() * 0.5).max(0.5), (h.abs() * 0.5).max(0.5))
}

fn ellipse_point(center: P, rx: f64, ry: f64, angle: f64) -> P {
    (center.0 + rx * angle.cos(), center.1 + ry * angle.sin())
}

/// Outward unit normal of the ellipse at `angle`.
fn ellipse_normal(rx: f64, ry: f64, angle: f64) -> P {
    norm((angle.cos() / rx, angle.sin() / ry))
}

/// Uniform Catmull-Rom spline through `points`, emitted as cubic Bв”њР№zier knots.
///
/// `tension` 1 is the standard spline, 0 retracts the handles into straight segments.
pub fn catmull_rom_knots(points: &[P], closed: bool, tension: f64) -> Vec<Knot> {
    let n = points.len();
    let tension = tension.clamp(0.0, 1.0);
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![Knot::corner(points[0].0, points[0].1)];
    }
    let tangent = |i: usize| -> P {
        let prev = if i == 0 { if closed { points[n - 1] } else { points[0] } } else { points[i - 1] };
        let next = if i + 1 >= n { if closed { points[0] } else { points[n - 1] } } else { points[i + 1] };
        mul(sub(next, prev), 0.5 * tension)
    };
    let mut knots = Vec::with_capacity(n);
    for (i, &anchor) in points.iter().enumerate().take(n) {
        let t = tangent(i);
        let c1 = add(anchor, mul(t, 1.0 / 3.0));
        let c2 = sub(anchor, mul(t, 1.0 / 3.0));
        knots.push(Knot::smooth(to_point(anchor), to_point(c2), to_point(c1)));
    }
    knots
}

/// Builds a closed [`Path`] from one contour, smoothing corners with a Catmull-Rom spline.
fn smooth_path(points: &[P]) -> Path {
    if points.len() < 3 {
        return Path::default();
    }
    let knots = catmull_rom_knots(points, true, 1.0);
    Path::new(vec![Subpath { closed: true, knots, op: PathOp::Combine }])
}

/// A thought-cloud balloon inscribed in `rect` (`[x, y, w, h]`), with `bumps` scalloped bumps.
pub fn cloud(rect: [f64; 4], params: CloudParams) -> Path {
    let (center, rx, ry) = rect_ellipse(rect);
    let bumps = params.bumps.clamp(3, 64);
    let amplitude = params.amplitude.clamp(0.0, 1.0);
    let jitter = params.jitter.clamp(0.0, 1.0);
    let steps = 6usize;
    let mut points: Vec<P> = Vec::new();
    for bump in 0..bumps {
        let a0 = TAU * bump as f64 / bumps as f64;
        let a1 = TAU * (bump + 1) as f64 / bumps as f64;
        let p0 = ellipse_point(center, rx, ry, a0);
        let p1 = ellipse_point(center, rx, ry, a1);
        let am = (a0 + a1) * 0.5;
        let outward = ellipse_normal(rx, ry, am);
        let scale = rx.min(ry);
        let height = jittered(scale * amplitude, params.seed, bump, jitter).max(0.0);
        for step in 0..steps {
            let t = step as f64 / steps as f64;
            let base = lerp(p0, p1, t);
            let bulge = (std::f64::consts::PI * t).sin() * height;
            points.push(add(base, mul(outward, bulge)));
        }
    }
    smooth_path(&points)
}

/// A spiky shout/flash balloon inscribed in `rect`.
///
/// Every spike alternates between a valley on the base ellipse and a tip pushed out by
/// `length` (or pulled in when `inward`), with deterministic per-spike jitter.
pub fn spiky(rect: [f64; 4], params: SpikeParams) -> Path {
    let (center, rx, ry) = rect_ellipse(rect);
    let spikes = params.spikes.clamp(3, 128);
    let length = params.length.clamp(0.0, 2.0);
    let jitter = params.jitter.clamp(0.0, 1.0);
    let mut points: Vec<P> = Vec::with_capacity(spikes as usize * 2);
    for i in 0..spikes {
        let a0 = TAU * i as f64 / spikes as f64;
        let am = a0 + TAU * 0.5 / spikes as f64;
        let valley = ellipse_point(center, rx, ry, a0);
        points.push(valley);
        let outward = ellipse_normal(rx, ry, am);
        let tip = ellipse_point(center, rx, ry, am);
        let reach = jittered(length, params.seed, i, jitter);
        let signed = if params.inward { -reach * 0.6 } else { reach };
        let factor = rx.min(ry) * signed;
        points.push(add(tip, mul(outward, factor)));
    }
    if params.smooth {
        smooth_path(&points)
    } else if points.len() >= 3 {
        Path::new(vec![Subpath::polygon(&points)])
    } else {
        Path::default()
    }
}

// ---------------------------------------------------------------------------------------------
// Stage 3: SAI-style click-to-click linework.
// ---------------------------------------------------------------------------------------------

/// One click-placed node of a vector stroke. The SAI curve module owns the type, so a node means
/// the same thing for the legacy linework and for the parametric balloons.
pub use crate::comic_sai_curve::SaiPoint;

/// A smooth path through click-placed points (Catmull-Rom С‚Р–Рў cubic Bв”њР№ziers), open by default.
///
/// Corner points keep sharp knots with retracted handles; the others become smooth knots, which
/// is exactly what SAI-style click drawing produces and what Direct Selection can keep editing.
pub fn sai_stroke(points: &[SaiPoint], closed: bool) -> Path {
    if points.len() < 2 {
        return Path::default();
    }
    let positions: Vec<P> = points.iter().map(|p| p.p).collect();
    let smooth = catmull_rom_knots(&positions, closed, 1.0);
    let knots = smooth
        .into_iter()
        .zip(points.iter())
        .map(|(knot, point)| if point.corner { Knot { anchor: knot.anchor, in_ctrl: knot.anchor, out_ctrl: knot.anchor, smooth: false } } else { knot })
        .collect();
    Path::new(vec![Subpath { closed, knots, op: PathOp::Combine }])
}

/// Uniform-width spline through `points` (no pressure).
pub fn catmull_rom_path(points: &[P], closed: bool) -> Path {
    if points.len() < 2 {
        return Path::default();
    }
    let knots = catmull_rom_knots(points, closed, 1.0);
    Path::new(vec![Subpath { closed, knots, op: PathOp::Combine }])
}

/// Width of a tapered line at `pressure`, never below `min_ratio` of the base width.
fn width_at(base_width: f64, min_ratio: f64, pressure: f32) -> f64 {
    let ratio = f64::from(pressure.clamp(0.0, 1.0)).clamp(min_ratio.clamp(0.0, 1.0), 1.0);
    (base_width * ratio).max(0.05)
}

/// A filled polygon approximating a variable-width stroke through `points` С‚РђР¤ the "weight per
/// node" of the SAI-style tool, used for ink lines with sharp tapers.
///
/// The polygon is built from the left and right offsets of the sampled centreline plus round
/// caps, so a pressure of 0 at the ends naturally gives pointed stroke tips.
pub fn tapered_outline(points: &[SaiPoint], base_width: f64, min_ratio: f64) -> Vec<P> {
    let pts: Vec<P> = points.iter().map(|p| p.p).collect();
    let n = pts.len();
    if n < 2 {
        return Vec::new();
    }
    let base_width = base_width.abs().max(0.05);
    let mut left: Vec<P> = Vec::with_capacity(n);
    let mut right: Vec<P> = Vec::with_capacity(n);
    for i in 0..n {
        let prev = if i == 0 { pts[0] } else { pts[i - 1] };
        let next = if i + 1 >= n { pts[n - 1] } else { pts[i + 1] };
        let mut dir = norm(sub(next, prev));
        if dir == (0.0, 0.0) {
            dir = norm(sub(pts[(i + 1).min(n - 1)], pts[i.saturating_sub(1)]));
        }
        if dir == (0.0, 0.0) {
            dir = (1.0, 0.0);
        }
        let normal = (-dir.1, dir.0);
        let half = width_at(base_width, min_ratio, points[i].pressure) * 0.5;
        left.push(add(pts[i], mul(normal, half)));
        right.push(sub(pts[i], mul(normal, half)));
    }
    let mut poly: Vec<P> = Vec::with_capacity(n * 2 + 16);
    poly.extend(left.iter().copied());
    // Round cap at the last point.
    let cap_dir = norm(sub(pts[n - 1], pts[n - 2]));
    let cap_normal = (-cap_dir.1, cap_dir.0);
    let cap_half = width_at(base_width, min_ratio, points[n - 1].pressure) * 0.5;
    for i in 1..8 {
        let angle = std::f64::consts::PI * i as f64 / 8.0;
        let offset = add(mul(cap_normal, cap_half * angle.cos()), mul(cap_dir, cap_half * angle.sin()));
        poly.push(add(pts[n - 1], offset));
    }
    poly.extend(right.iter().rev().copied());
    // Round cap at the first point.
    let start_dir = norm(sub(pts[0], pts[1]));
    let start_normal = (-start_dir.1, start_dir.0);
    let start_half = width_at(base_width, min_ratio, points[0].pressure) * 0.5;
    for i in 1..8 {
        let angle = std::f64::consts::PI * i as f64 / 8.0;
        let offset = add(mul(start_normal, start_half * angle.cos()), mul(start_dir, start_half * angle.sin()));
        poly.push(add(pts[0], offset));
    }
    clean(&poly)
}

/// A path for the tapered stroke (a filled outline, ready for a shape layer with no stroke).
pub fn tapered_path(points: &[SaiPoint], base_width: f64, min_ratio: f64) -> Path {
    let outline = tapered_outline(points, base_width, min_ratio);
    if outline.len() < 3 { Path::default() } else { Path::new(vec![Subpath::polygon(&outline)]) }
}

// ---------------------------------------------------------------------------------------------
// Parametric manga balloons (live shapes).
//
// The balloon data (bodies, tails, groups) lives in `photocraft_doc::vector`; the math lives
// here. Rust does not allow inherent impls for a type of another crate, so the balloon's
// methods are a trait the callers import (`use photocraft_vector::balloon::LiveBalloonGeom;`).
// ---------------------------------------------------------------------------------------------

/// Deterministic SplitMix32 hash mapped into `[0.0, 1.0]`: the seeded randomness of the
/// procedural balloon bodies (the same seed always yields the same balloon).
#[inline]
pub fn hash_f64(seed: u32, index: u32) -> f64 {
    let mut z = seed.wrapping_add(index.wrapping_mul(0x9E3779B9));
    z = (z ^ (z >> 16)).wrapping_mul(0x85EBCA6B);
    z = (z ^ (z >> 13)).wrapping_mul(0xC2B2AE35);
    z = z ^ (z >> 16);
    (z as f64) / (u32::MAX as f64)
}

/// The geometry of a parametric balloon.
pub trait LiveBalloonGeom {
    /// The inner area a text is laid out in: `[x, y, w, h]` in document px.
    fn safe_text_rect(&self) -> [f64; 4];
    /// The perimeter parameter `t` in `[0.0, 1.0)` of the body point nearest to `pt`.
    fn closest_perimeter_t(&self, pt: [f64; 2]) -> f64;
    /// The point of the body's outline at `t` and the outward normal there.
    fn point_and_normal_on_body(&self, t: f64) -> ([f64; 2], [f64; 2]);
    /// The manual offset of one spike or lobe vertex (zero when the user did not move it).
    fn get_vertex_offset(&self, index: u32) -> [f64; 2];
    /// The balloon's closed outline, its tails merged in.
    fn rebuild_closed_path(&self) -> Path;
}

/// The geometry of the balloons of one layer.
pub trait LiveBalloonGroupGeom {
    /// The layer's outline: every balloon of the group, merged where they overlap.
    fn rebuild_group_path(&self) -> Path;
}

/// A superellipse (`squircle` 0 = ellipse, 1 = full squircle) as a closed path.
fn build_superellipse_path(cx: f64, cy: f64, rx: f64, ry: f64, squircle: f64) -> Path {
    let k = 0.5522847498 + squircle.clamp(0.0, 0.85) * 0.28;
    let kx = rx * k;
    let ky = ry * k;
    let knots = vec![
        Knot { anchor: Point::new(cx, cy - ry), in_ctrl: Point::new(cx - kx, cy - ry), out_ctrl: Point::new(cx + kx, cy - ry), smooth: true },
        Knot { anchor: Point::new(cx + rx, cy), in_ctrl: Point::new(cx + rx, cy - ky), out_ctrl: Point::new(cx + rx, cy + ky), smooth: true },
        Knot { anchor: Point::new(cx, cy + ry), in_ctrl: Point::new(cx + kx, cy + ry), out_ctrl: Point::new(cx - kx, cy + ry), smooth: true },
        Knot { anchor: Point::new(cx - rx, cy), in_ctrl: Point::new(cx - rx, cy + ky), out_ctrl: Point::new(cx - rx, cy - ky), smooth: true },
    ];
    Path::new(vec![Subpath { knots, closed: true, op: PathOp::Combine }])
}

/// A thought cloud: scalloped lobes with cusped joints.
fn build_cloud_body(balloon: &LiveBalloon, rect: [f64; 4], bumps: u32, amplitude: f64, cusp_bite: f64, jitter: f64, seed: u32) -> Path {
    let cx = rect[0] + rect[2] * 0.5;
    let cy = rect[1] + rect[3] * 0.5;
    let rx = (rect[2] * 0.5).abs().max(4.0);
    let ry = (rect[3] * 0.5).abs().max(4.0);
    let n = bumps.clamp(4, 64);

    let mut angles = Vec::with_capacity(n as usize + 1);
    let step = std::f64::consts::TAU / (n as f64);
    for i in 0..n {
        let j = (hash_f64(seed, i * 2) - 0.5) * jitter.clamp(0.0, 0.8) * step * 0.55;
        angles.push(-std::f64::consts::FRAC_PI_2 + (i as f64) * step + j);
    }
    angles.push(angles[0] + std::f64::consts::TAU);

    let mut knots = Vec::with_capacity(n as usize);
    let inward_factor = 1.0 - cusp_bite.clamp(0.0, 0.5) * 0.35;

    for i in 0..n as usize {
        let a0 = angles[i];
        let a1 = angles[i + 1];
        let span = (a1 - a0).max(0.05);

        let vx = cx + rx * inward_factor * a0.cos();
        let vy = cy + ry * inward_factor * a0.sin();

        let prev_idx = if i == 0 { n - 1 } else { (i - 1) as u32 };
        let cur_amp = amplitude * (1.0 + (hash_f64(seed, (i as u32) * 2 + 1) - 0.5) * jitter);
        let prev_amp = amplitude * (1.0 + (hash_f64(seed, prev_idx * 2 + 1) - 0.5) * jitter);

        let cur_off = balloon.get_vertex_offset(i as u32);
        let prev_off = balloon.get_vertex_offset(prev_idx);

        let out_angle = a0 + span * 0.28;
        let in_angle = a0 - span * 0.28;
        let r_out = 1.0 + cur_amp * 1.35;
        let r_in = 1.0 + prev_amp * 1.35;

        knots.push(Knot {
            anchor: Point::new(vx, vy),
            in_ctrl: Point::new(cx + rx * r_in * in_angle.cos() + prev_off[0], cy + ry * r_in * in_angle.sin() + prev_off[1]),
            out_ctrl: Point::new(cx + rx * r_out * out_angle.cos() + cur_off[0], cy + ry * r_out * out_angle.sin() + cur_off[1]),
            smooth: false, // The joints between the lobes meet at a sharp angle.
        });
    }

    Path::new(vec![Subpath { knots, closed: true, op: PathOp::Combine }])
}

/// The parameters of a shout / explosion balloon, gathered so the builder keeps a short signature.
struct SpikyBody {
    rect: [f64; 4],
    spikes: u32,
    length: f64,
    sub_harmonic: f64,
    concavity: f64,
    jitter: f64,
    smooth: bool,
    seed: u32,
}

/// A shout / explosion balloon: concave arcs between the spikes.
fn build_spiky_body(balloon: &LiveBalloon, body: SpikyBody) -> Path {
    let SpikyBody { rect, spikes, length, sub_harmonic, concavity, jitter, smooth, seed } = body;
    let cx = rect[0] + rect[2] * 0.5;
    let cy = rect[1] + rect[3] * 0.5;
    let rx = (rect[2] * 0.5).abs().max(4.0);
    let ry = (rect[3] * 0.5).abs().max(4.0);
    let n = spikes.clamp(5, 128);
    let step = std::f64::consts::TAU / (n as f64);

    let mut knots = Vec::with_capacity(n as usize);
    let valley_depth = (1.0 - concavity.clamp(0.0, 1.0) * 0.28).clamp(0.55, 1.0);

    for i in 0..n {
        let a_jitter = (hash_f64(seed, i * 3) - 0.5) * jitter * step * 0.4;
        let angle = -std::f64::consts::FRAC_PI_2 + (i as f64) * step + a_jitter;

        let harmonic_mult = match i % 3 {
            0 => 1.0,
            1 => 1.0 - sub_harmonic.clamp(0.0, 1.0) * 0.48,
            _ => 1.0 - sub_harmonic.clamp(0.0, 1.0) * 0.28,
        };
        let rand_mult = 1.0 + (hash_f64(seed, i * 3 + 1) - 0.5) * jitter * 0.6;
        let tip_r = 1.0 + length * harmonic_mult * rand_mult;

        let user_off = balloon.get_vertex_offset(i);
        let tx = cx + rx * tip_r * angle.cos() + user_off[0];
        let ty = cy + ry * tip_r * angle.sin() + user_off[1];

        let a_in = angle - step * 0.42;
        let a_out = angle + step * 0.42;
        let in_pt = Point::new(cx + rx * valley_depth * a_in.cos(), cy + ry * valley_depth * a_in.sin());
        let out_pt = Point::new(cx + rx * valley_depth * a_out.cos(), cy + ry * valley_depth * a_out.sin());

        if smooth {
            knots.push(Knot { anchor: Point::new(tx, ty), in_ctrl: in_pt, out_ctrl: out_pt, smooth: true });
        } else {
            knots.push(Knot {
                anchor: Point::new(tx, ty),
                in_ctrl: Point::new(tx * 0.35 + in_pt.x * 0.65, ty * 0.35 + in_pt.y * 0.65),
                out_ctrl: Point::new(tx * 0.35 + out_pt.x * 0.65, ty * 0.35 + out_pt.y * 0.65),
                smooth: false,
            });
        }
    }

    Path::new(vec![Subpath { knots, closed: true, op: PathOp::Combine }])
}

/// A sea-urchin flash: the radial needles and the white mask under them.
fn build_uniflash_paths(balloon: &LiveBalloon) -> Option<(Path, Path)> {
    let LiveBalloonBody::Uniflash { rect, needles, inner_ratio, bundle_group, length_jitter, angle_jitter, seed, .. } = &balloon.body else {
        return None;
    };

    let cx = rect[0] + rect[2] * 0.5;
    let cy = rect[1] + rect[3] * 0.5;
    let rx = (rect[2] * 0.5).abs().max(4.0);
    let ry = (rect[3] * 0.5).abs().max(4.0);
    let count = (*needles).clamp(24, 450);
    let step = std::f64::consts::TAU / (count as f64);

    let mask_rx = rx * inner_ratio.clamp(0.25, 0.92);
    let mask_ry = ry * inner_ratio.clamp(0.25, 0.92);
    let bg_mask = build_superellipse_path(cx, cy, mask_rx, mask_ry, 0.15);

    let mut needle_subpaths = Vec::with_capacity(count as usize);
    for i in 0..count {
        let rand_a = hash_f64(*seed, i * 3) - 0.5;
        let rand_in = hash_f64(*seed, i * 3 + 1);
        let rand_out = hash_f64(*seed, i * 3 + 2);

        let bundle_phase = ((i as f64) * 0.35 * std::f64::consts::TAU).sin();
        let bundle_shift = bundle_phase * step * 0.35 * bundle_group.clamp(0.0, 1.0);

        let theta = (i as f64) * step + bundle_shift + rand_a * step * angle_jitter.clamp(0.0, 1.0);
        let cos = theta.cos();
        let sin = theta.sin();

        let harmonic_in = if i % 2 == 0 { 1.0 } else { 0.85 };
        let r_in = inner_ratio * (1.0 - 0.32 * length_jitter * rand_in * harmonic_in);
        let r_out = 1.0 - 0.14 * length_jitter * rand_out;

        let user_off = balloon.get_vertex_offset(i);
        let p_outer = Point::new(cx + rx * r_out * cos, cy + ry * r_out * sin);
        let p_inner = Point::new(cx + rx * r_in * cos + user_off[0], cy + ry * r_in * sin + user_off[1]);

        needle_subpaths.push(Subpath {
            knots: vec![Knot::corner(p_outer.x, p_outer.y), Knot::corner(p_inner.x, p_inner.y)],
            closed: false,
            op: PathOp::Combine,
        });
    }

    Some((bg_mask, Path::new(needle_subpaths)))
}

/// One tail's geometry: the outline welded to the body (if any) and the detached thought
/// bubbles (if any).
fn build_single_tail_path(balloon: &LiveBalloon, t: &LiveBalloonTail) -> (Option<Path>, Option<Path>) {
    let (mouth_center, normal) = balloon.point_and_normal_on_body(t.perimeter_t);
    let tangent = [-normal[1], normal[0]];
    let r = balloon.body.rect();
    let min_half = (r[2].abs().min(r[3].abs()) * 0.5).max(4.0);
    let half_w = t.mouth_width.clamp(4.0, min_half * 1.6) * 0.5;

    // Keep the effective tip strictly in front of the mouth along `normal` so drags
    // inside the balloon body cannot invert `chord_perp` against `tangent` (figure-8 cross).
    let min_outward = (min_half * 0.12).max(4.0);
    let rel_mouth = [t.tip[0] - mouth_center[0], t.tip[1] - mouth_center[1]];
    let outward_proj = rel_mouth[0] * normal[0] + rel_mouth[1] * normal[1];
    let tang_proj = rel_mouth[0] * tangent[0] + rel_mouth[1] * tangent[1];
    let eff_tip = if outward_proj < min_outward {
        [
            mouth_center[0] + normal[0] * min_outward + tangent[0] * tang_proj,
            mouth_center[1] + normal[1] * min_outward + tangent[1] * tang_proj,
        ]
    } else {
        t.tip
    };

    let chord = [eff_tip[0] - mouth_center[0], eff_tip[1] - mouth_center[1]];
    let chord_len = (chord[0] * chord[0] + chord[1] * chord[1]).sqrt().max(1.0);
    let chord_dir = [chord[0] / chord_len, chord[1] / chord_len];
    let mut chord_perp = [-chord_dir[1], chord_dir[0]];
    if chord_perp[0] * tangent[0] + chord_perp[1] * tangent[1] < 0.0 {
        chord_perp = [-chord_perp[0], -chord_perp[1]];
    }

    let bend_scale = (outward_proj.max(min_outward) / (half_w * 0.6).max(1.0)).clamp(0.15, 1.0);
    let bend_px = t.bend.clamp(-1.0, 1.0) * (chord_len * 0.42) * bend_scale;
    let ctrl = [(mouth_center[0] + eff_tip[0]) * 0.5 + chord_perp[0] * bend_px, (mouth_center[1] + eff_tip[1]) * 0.5 + chord_perp[1] * bend_px];

    if t.preset == TailPreset::Thought {
        let count = t.bubbles.clamp(1, 8);
        let mut subpaths = Vec::with_capacity(count as usize);
        for i in 0..count {
            let s = if count == 1 { 1.0 } else { 0.22 + 0.78 * ((i as f64) / ((count - 1) as f64)) };
            let omt = 1.0 - s;
            let bx = omt * omt * mouth_center[0] + 2.0 * omt * s * ctrl[0] + s * s * eff_tip[0];
            let by = omt * omt * mouth_center[1] + 2.0 * omt * s * ctrl[1] + s * s * eff_tip[1];
            let rad = (t.bubble_radius * (1.0 - 0.65 * ((i as f64) / (count.max(1) as f64)))).max(2.0);
            subpaths.extend(build_superellipse_path(bx, by, rad, rad, 0.0).subpaths);
        }
        return (None, Some(Path::new(subpaths)));
    }

    let inset = (min_half * 0.28).min(32.0);
    let anchor = [mouth_center[0] - normal[0] * inset, mouth_center[1] - normal[1] * inset];

    let p_left = Point::new(anchor[0] - tangent[0] * half_w, anchor[1] - tangent[1] * half_w);
    let p_right = Point::new(anchor[0] + tangent[0] * half_w, anchor[1] + tangent[1] * half_w);
    let p_tip = Point::new(eff_tip[0], eff_tip[1]);

    let subpath = match t.preset {
        TailPreset::Wedge => Subpath {
            knots: vec![Knot::corner(p_left.x, p_left.y), Knot::corner(p_tip.x, p_tip.y), Knot::corner(p_right.x, p_right.y)],
            closed: true,
            op: PathOp::Combine,
        },
        TailPreset::Arc => {
            let c_l = Point::new(ctrl[0] - chord_perp[0] * half_w * 0.25, ctrl[1] - chord_perp[1] * half_w * 0.25);
            let c_r = Point::new(ctrl[0] + chord_perp[0] * half_w * 0.25, ctrl[1] + chord_perp[1] * half_w * 0.25);
            Subpath {
                knots: vec![
                    Knot { anchor: p_left, in_ctrl: p_left, out_ctrl: c_l, smooth: false },
                    Knot { anchor: p_tip, in_ctrl: c_l, out_ctrl: c_r, smooth: false },
                    Knot { anchor: p_right, in_ctrl: c_r, out_ctrl: p_right, smooth: false },
                ],
                closed: true,
                op: PathOp::Combine,
            }
        }
        TailPreset::SCurve => {
            let c1 = [mouth_center[0] + chord[0] * 0.33 + chord_perp[0] * bend_px, mouth_center[1] + chord[1] * 0.33 + chord_perp[1] * bend_px];
            let c2 = [mouth_center[0] + chord[0] * 0.66 - chord_perp[0] * bend_px * 0.85, mouth_center[1] + chord[1] * 0.66 - chord_perp[1] * bend_px * 0.85];
            Subpath {
                knots: vec![
                    Knot {
                        anchor: p_left,
                        in_ctrl: p_left,
                        out_ctrl: Point::new(c1[0] - chord_perp[0] * half_w * 0.3, c1[1] - chord_perp[1] * half_w * 0.3),
                        smooth: false,
                    },
                    Knot {
                        anchor: p_tip,
                        in_ctrl: Point::new(c2[0] - chord_perp[0] * half_w * 0.15, c2[1] - chord_perp[1] * half_w * 0.15),
                        out_ctrl: Point::new(c2[0] + chord_perp[0] * half_w * 0.15, c2[1] + chord_perp[1] * half_w * 0.15),
                        smooth: false,
                    },
                    Knot {
                        anchor: p_right,
                        in_ctrl: Point::new(c1[0] + chord_perp[0] * half_w * 0.3, c1[1] + chord_perp[1] * half_w * 0.3),
                        out_ctrl: p_right,
                        smooth: false,
                    },
                ],
                closed: true,
                op: PathOp::Combine,
            }
        }
        TailPreset::Lightning => {
            let k1 = [
                mouth_center[0] + chord[0] * 0.44 + chord_perp[0] * (half_w * 0.75 + bend_px * 0.4),
                mouth_center[1] + chord[1] * 0.44 + chord_perp[1] * (half_w * 0.75 + bend_px * 0.4),
            ];
            let k2 = [
                mouth_center[0] + chord[0] * 0.56 - chord_perp[0] * (half_w * 0.55 - bend_px * 0.4),
                mouth_center[1] + chord[1] * 0.56 - chord_perp[1] * (half_w * 0.55 - bend_px * 0.4),
            ];
            let pts = [
                (p_left.x, p_left.y),
                (k1[0] - chord_perp[0] * half_w * 0.35, k1[1] - chord_perp[1] * half_w * 0.35),
                (k2[0] - chord_perp[0] * half_w * 0.20, k2[1] - chord_perp[1] * half_w * 0.20),
                (eff_tip[0], eff_tip[1]),
                (k2[0] + chord_perp[0] * half_w * 0.20, k2[1] + chord_perp[1] * half_w * 0.20),
                (k1[0] + chord_perp[0] * half_w * 0.35, k1[1] + chord_perp[1] * half_w * 0.35),
                (p_right.x, p_right.y),
            ];
            Subpath::polygon(&pts)
        }
        TailPreset::Double => {
            let spread = (half_w * 0.65).max(6.0);
            let tip1 = (eff_tip[0] - chord_perp[0] * spread, eff_tip[1] - chord_perp[1] * spread);
            let tip2 = (eff_tip[0] + chord_perp[0] * spread, eff_tip[1] + chord_perp[1] * spread);
            let notch = (mouth_center[0] + chord[0] * 0.62, mouth_center[1] + chord[1] * 0.62);
            Subpath::polygon(&[(p_left.x, p_left.y), tip1, notch, tip2, (p_right.x, p_right.y)])
        }
        // The thought bubbles are handled above.
        TailPreset::Thought => return (None, None),
    };

    (Some(Path::new(vec![subpath])), None)
}

impl LiveBalloonGeom for LiveBalloon {
    fn safe_text_rect(&self) -> [f64; 4] {
        let r = self.body.rect();
        let cx = r[0] + r[2] * 0.5;
        let cy = r[1] + r[3] * 0.5;
        let rx = (r[2] * 0.5).abs();
        let ry = (r[3] * 0.5).abs();

        let (in_rx, in_ry) = match &self.body {
            LiveBalloonBody::Ellipse { squircle, .. } => {
                let f = 0.71 + 0.17 * squircle.clamp(0.0, 1.0);
                (rx * f, ry * f)
            }
            LiveBalloonBody::Rect { corner_radius, .. } => {
                let pad = (*corner_radius * 0.35).max(6.0);
                ((rx - pad).max(4.0), (ry - pad).max(4.0))
            }
            LiveBalloonBody::Cloud { amplitude, cusp_bite, .. } => {
                let f = (1.0 - amplitude * 0.45 - cusp_bite * 0.25).clamp(0.4, 0.9) * 0.74;
                (rx * f, ry * f)
            }
            LiveBalloonBody::Spiky { length, concavity, .. } => {
                let f = (1.0 - length * 0.60 - concavity * 0.15).clamp(0.35, 0.88) * 0.72;
                (rx * f, ry * f)
            }
            LiveBalloonBody::Uniflash { inner_ratio, .. } => {
                let f = inner_ratio.clamp(0.30, 0.90) * 0.78;
                (rx * f, ry * f)
            }
        };

        [cx - in_rx, cy - in_ry, in_rx * 2.0, in_ry * 2.0]
    }

    fn closest_perimeter_t(&self, pt: [f64; 2]) -> f64 {
        let r = self.body.rect();
        let cx = r[0] + r[2] * 0.5;
        let cy = r[1] + r[3] * 0.5;
        let rx = (r[2] * 0.5).abs().max(1.0);
        let ry = (r[3] * 0.5).abs().max(1.0);

        let nx = (pt[0] - cx) / rx;
        let ny = (pt[1] - cy) / ry;
        let angle = ny.atan2(nx);
        ((angle + std::f64::consts::FRAC_PI_2) / std::f64::consts::TAU).rem_euclid(1.0)
    }

    fn point_and_normal_on_body(&self, t: f64) -> ([f64; 2], [f64; 2]) {
        let angle = t * std::f64::consts::TAU - std::f64::consts::FRAC_PI_2;
        let r = self.body.rect();
        let cx = r[0] + r[2] * 0.5;
        let cy = r[1] + r[3] * 0.5;
        let rx = (r[2] * 0.5).abs().max(1.0);
        let ry = (r[3] * 0.5).abs().max(1.0);

        match &self.body {
            LiveBalloonBody::Rect { .. } => {
                let cos = angle.cos();
                let sin = angle.sin();
                let sx = rx / cos.abs().max(1e-6);
                let sy = ry / sin.abs().max(1e-6);
                let s = sx.min(sy);
                let px = cx + cos * s;
                let py = cy + sin * s;
                let nx = if sx < sy { cos.signum() } else { 0.0 };
                let ny = if sy <= sx { sin.signum() } else { 0.0 };
                ([px, py], [nx, ny])
            }
            LiveBalloonBody::Cloud { cusp_bite, .. } => {
                // Anchor on the cusp contour so the tail mouth always welds inside the solid lobes.
                let base_factor = 1.0 - cusp_bite.clamp(0.0, 0.5) * 0.35;
                let bx = rx * base_factor;
                let by = ry * base_factor;
                let px = cx + bx * angle.cos();
                let py = cy + by * angle.sin();
                let ux = angle.cos() / bx.max(1.0);
                let uy = angle.sin() / by.max(1.0);
                let len = (ux * ux + uy * uy).sqrt().max(1e-9);
                ([px, py], [ux / len, uy / len])
            }
            LiveBalloonBody::Spiky { concavity, .. } => {
                // Anchor on the valley core contour so the tail never floats outside deep spike valleys.
                let valley_factor = (1.0 - concavity.clamp(0.0, 1.0) * 0.28).clamp(0.55, 1.0);
                let bx = rx * valley_factor;
                let by = ry * valley_factor;
                let px = cx + bx * angle.cos();
                let py = cy + by * angle.sin();
                let ux = angle.cos() / bx.max(1.0);
                let uy = angle.sin() / by.max(1.0);
                let len = (ux * ux + uy * uy).sqrt().max(1e-9);
                ([px, py], [ux / len, uy / len])
            }
            _ => {
                let px = cx + rx * angle.cos();
                let py = cy + ry * angle.sin();
                let ux = angle.cos() / rx;
                let uy = angle.sin() / ry;
                let len = (ux * ux + uy * uy).sqrt().max(1e-9);
                ([px, py], [ux / len, uy / len])
            }
        }
    }

    fn get_vertex_offset(&self, index: u32) -> [f64; 2] {
        self.vertex_overrides.iter().find(|v| v.index == index).map(|v| v.offset).unwrap_or([0.0, 0.0])
    }

    fn rebuild_closed_path(&self) -> Path {
        let mut base_path = match &self.body {
            LiveBalloonBody::Ellipse { rect, squircle } => {
                let cx = rect[0] + rect[2] * 0.5;
                let cy = rect[1] + rect[3] * 0.5;
                build_superellipse_path(cx, cy, (rect[2] * 0.5).abs(), (rect[3] * 0.5).abs(), *squircle)
            }
            LiveBalloonBody::Rect { rect, corner_radius } => {
                let r = *corner_radius;
                crate::shapes::rounded_rect(rect[0], rect[1], rect[2], rect[3], [r, r, r, r])
            }
            LiveBalloonBody::Cloud { rect, bumps, amplitude, cusp_bite, jitter, seed } => {
                build_cloud_body(self, *rect, *bumps, *amplitude, *cusp_bite, *jitter, *seed)
            }
            LiveBalloonBody::Spiky { rect, spikes, length, sub_harmonic, concavity, jitter, smooth, seed } => build_spiky_body(
                self,
                SpikyBody {
                    rect: *rect,
                    spikes: *spikes,
                    length: *length,
                    sub_harmonic: *sub_harmonic,
                    concavity: *concavity,
                    jitter: *jitter,
                    smooth: *smooth,
                    seed: *seed,
                },
            ),
            LiveBalloonBody::Uniflash { .. } => {
                // The white mask and the needles are returned together, so the manga background
                // does not show through the burst.
                match build_uniflash_paths(self) {
                    Some((mut bg_mask, needles_path)) => {
                        bg_mask.subpaths.extend(needles_path.subpaths);
                        return bg_mask;
                    }
                    None => return Path::default(),
                }
            }
        };

        let mut detached_subpaths = Vec::new();
        for tail in &self.tails {
            let (welded, detached) = build_single_tail_path(self, tail);
            if let Some(w_path) = welded {
                // A real boolean union, so one fill and one stroke outline the whole balloon.
                base_path = merge_paths_union(&base_path, &w_path);
            }
            if let Some(d_path) = detached {
                detached_subpaths.extend(d_path.subpaths);
            }
        }
        base_path.subpaths.extend(detached_subpaths);
        base_path
    }
}

impl LiveBalloonGroupGeom for LiveBalloonGroup {
    fn rebuild_group_path(&self) -> Path {
        let mut merged_pool: Option<Path> = None;
        let mut isolated_subpaths = Vec::new();

        for b in &self.items {
            let p = b.rebuild_closed_path();
            if b.auto_merge_overlapping && !matches!(b.body, LiveBalloonBody::Uniflash { .. }) {
                merged_pool = Some(match merged_pool {
                    None => p,
                    Some(acc) => merge_paths_union(&acc, &p),
                });
            } else {
                isolated_subpaths.extend(p.subpaths);
            }
        }

        let mut out = merged_pool.unwrap_or_default();
        out.subpaths.extend(isolated_subpaths);
        out
    }
}

/// A reliable polygonal union of two paths through [`union_polygons`]. Only contours that
/// overlap (AABB) are merged, so independent balloons and thought bubbles are not glued into
/// one giant polygon.
pub fn merge_paths_union(a: &Path, b: &Path) -> Path {
    let polys_a = crate::flatten::flatten_path(a, UNION_TOL);
    let polys_b = crate::flatten::flatten_path(b, UNION_TOL);

    if polys_a.is_empty() {
        return b.clone();
    }
    if polys_b.is_empty() {
        return a.clone();
    }

    let mut result_polys: Vec<Vec<P>> = polys_a.into_iter().map(|ring| ring.pts).collect();

    for ring_b in polys_b {
        let mut cur_b: Vec<P> = ring_b.pts;
        let mut next_result = Vec::with_capacity(result_polys.len() + 1);

        for existing in result_polys {
            if polygons_bbox_overlap(&existing, &cur_b) {
                let merged = union_polygons(&existing, &cur_b);
                if merged.len() == 1 {
                    match merged.into_iter().next() {
                        Some(single) => cur_b = single,
                        None => next_result.push(existing),
                    }
                } else {
                    // The contours overlap by AABB but do not really intersect.
                    next_result.push(existing);
                }
            } else {
                next_result.push(existing);
            }
        }
        next_result.push(cur_b);
        result_polys = next_result;
    }

    let subpaths = result_polys.into_iter().filter(|c| c.len() >= 3).map(|c| Subpath::polygon(&c)).collect();
    Path::new(subpaths)
}

/// Whether the bounding boxes of two polygons overlap.
fn polygons_bbox_overlap(a: &[P], b: &[P]) -> bool {
    let (mut min_ax, mut min_ay, mut max_ax, mut max_ay) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for &(x, y) in a {
        min_ax = min_ax.min(x);
        min_ay = min_ay.min(y);
        max_ax = max_ax.max(x);
        max_ay = max_ay.max(y);
    }
    let (mut min_bx, mut min_by, mut max_bx, mut max_by) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for &(x, y) in b {
        min_bx = min_bx.min(x);
        min_by = min_by.min(y);
        max_bx = max_bx.max(x);
        max_by = max_by.max(y);
    }
    max_ax >= min_bx && min_ax <= max_bx && max_ay >= min_by && min_ay <= max_by
}

/// Converts a balloon into SAI Linework strokes: every outline becomes a tapered stroke whose
/// width follows the balloon's pressure, so the balloon can be redrawn as clean linework.
pub fn balloon_to_linework_strokes(b: &LiveBalloon) -> Vec<LiveVectorStroke> {
    // The sea urchin: every needle tapers from 0.95 to 0.05 towards the centre.
    if let LiveBalloonBody::Uniflash { needle_width, .. } = &b.body
        && let Some((_, needles_path)) = build_uniflash_paths(b)
    {
        return needles_path
            .subpaths
            .into_iter()
            .filter(|sp| sp.knots.len() >= 2)
            .map(|sp| {
                let p_out = sp.knots[0].anchor;
                let p_in = sp.knots[1].anchor;
                let mid = Point::new((p_out.x + p_in.x) * 0.5, (p_out.y + p_in.y) * 0.5);
                LiveVectorStroke {
                    nodes: vec![
                        SaiPoint { p: (p_out.x, p_out.y), pressure: 0.95, density: 1.0, corner: true },
                        SaiPoint { p: (mid.x, mid.y), pressure: 0.65, density: 1.0, corner: false },
                        SaiPoint { p: (p_in.x, p_in.y), pressure: 0.05, density: 0.85, corner: true },
                    ],
                    base_width: *needle_width,
                    min_ratio: 0.0,
                    base_density: 1.0,
                    min_density_ratio: 0.0,
                    curve_type: SaiCurveType::Normal,
                    tapered: true,
                    closed: false,
                    tension: SAI_DEFAULT_TENSION,
                }
            })
            .collect();
    }

    // The other balloons: the Bézier segments are sampled, so an oval does not become a rhombus.
    let path = b.rebuild_closed_path();
    let mut strokes = Vec::with_capacity(path.subpaths.len());

    for subpath in path.subpaths {
        if subpath.knots.is_empty() {
            continue;
        }

        // A boolean union can leave a subpath with hundreds of tiny polygon edges; it is
        // thinned out by arc length, keeping only the real corners (a bend of more than 32°).
        let is_dense_polygon = subpath.knots.len() > 32 && subpath.knots.iter().all(|k| (k.out_ctrl.x - k.anchor.x).hypot(k.out_ctrl.y - k.anchor.y) < 0.2);

        if is_dense_polygon {
            let pts: Vec<Point> = subpath.knots.iter().map(|k| k.anchor).collect();
            let m = pts.len();
            let mut simplified: Vec<(Point, bool)> = Vec::new();
            simplified.push((pts[0], true));
            let mut last_idx = 0;

            for i in 1..m {
                let prev = pts[(i + m - 1) % m];
                let curr = pts[i];
                let next = pts[(i + 1) % m];

                let v1 = (curr.x - prev.x, curr.y - prev.y);
                let v2 = (next.x - curr.x, next.y - curr.y);
                let len1 = v1.0.hypot(v1.1);
                let len2 = v2.0.hypot(v2.1);

                let is_sharp = if len1 > 1e-4 && len2 > 1e-4 {
                    let cos_a = (v1.0 * v2.0 + v1.1 * v2.1) / (len1 * len2);
                    cos_a < 0.848 // a bend of more than about 32°
                } else {
                    false
                };

                let dist = (curr.x - pts[last_idx].x).hypot(curr.y - pts[last_idx].y);
                if is_sharp || dist >= 16.0 {
                    simplified.push((curr, is_sharp));
                    last_idx = i;
                }
            }

            let nodes = simplified
                .into_iter()
                .map(|(pt, sharp)| {
                    let is_tail_tip = b.tails.iter().any(|t| (pt.x - t.tip[0]).hypot(pt.y - t.tip[1]) < 3.0);
                    SaiPoint {
                        p: (pt.x, pt.y),
                        pressure: if is_tail_tip {
                            0.55
                        } else if sharp {
                            0.85
                        } else {
                            1.0
                        },
                        density: 1.0,
                        corner: sharp || is_tail_tip,
                    }
                })
                .collect();

            strokes.push(LiveVectorStroke {
                nodes,
                base_width: b.stroke_width,
                min_ratio: 0.05,
                base_density: 1.0,
                min_density_ratio: 0.0,
                curve_type: SaiCurveType::Normal,
                tapered: true,
                closed: subpath.closed,
                tension: SAI_DEFAULT_TENSION,
            });
            continue;
        }

        let n = subpath.knots.len();
        let seg_count = if subpath.closed { n } else { n.saturating_sub(1) };
        let mut nodes = Vec::new();

        for i in 0..seg_count {
            let k0 = &subpath.knots[i];
            let k1 = &subpath.knots[(i + 1) % n];

            let is_curved =
                (k0.out_ctrl.x - k0.anchor.x).hypot(k0.out_ctrl.y - k0.anchor.y) > 0.5 || (k1.in_ctrl.x - k1.anchor.x).hypot(k1.in_ctrl.y - k1.anchor.y) > 0.5;

            let is_tail_tip = b.tails.iter().any(|t| (k0.anchor.x - t.tip[0]).hypot(k0.anchor.y - t.tip[1]) < 2.5);

            let base_pressure = if is_tail_tip {
                0.55
            } else if matches!(b.body, LiveBalloonBody::Cloud { .. }) {
                0.75
            } else if matches!(b.body, LiveBalloonBody::Spiky { .. }) {
                1.18
            } else {
                1.0
            };

            nodes.push(SaiPoint { p: (k0.anchor.x, k0.anchor.y), pressure: base_pressure, density: 1.0, corner: is_tail_tip || !is_curved });

            // The intermediate points on the Bézier curve.
            if is_curved {
                let steps = 4;
                for step in 1..steps {
                    let t = (step as f64) / (steps as f64);
                    let pt = eval_cubic_bezier(k0.anchor, k0.out_ctrl, k1.in_ctrl, k1.anchor, t);
                    let mid_boost = if matches!(b.body, LiveBalloonBody::Cloud { .. }) && step == 2 { 1.15 } else { 1.0 };
                    nodes.push(SaiPoint { p: (pt.x, pt.y), pressure: mid_boost, density: 1.0, corner: false });
                }
            }
        }

        strokes.push(LiveVectorStroke {
            nodes,
            base_width: b.stroke_width,
            min_ratio: 0.05,
            base_density: 1.0,
            min_density_ratio: 0.0,
            curve_type: SaiCurveType::Normal,
            tapered: true,
            closed: subpath.closed,
            tension: SAI_DEFAULT_TENSION,
        });
    }

    strokes
}

/// A point on the cubic Bézier through `p0..p3` at `t`.
fn eval_cubic_bezier(p0: Point, p1: Point, p2: Point, p3: Point, t: f64) -> Point {
    let omt = 1.0 - t;
    let omt2 = omt * omt;
    let t2 = t * t;
    let c0 = omt2 * omt;
    let c1 = 3.0 * omt2 * t;
    let c2 = 3.0 * omt * t2;
    let c3 = t2 * t;
    Point::new(c0 * p0.x + c1 * p1.x + c2 * p2.x + c3 * p3.x, c0 * p0.y + c1 * p1.y + c2 * p2.y + c3 * p3.y)
}

// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, s: f64) -> Vec<P> {
        vec![(x, y), (x + s, y), (x + s, y + s), (x, y + s)]
    }

    fn area(contours: &[Vec<P>]) -> f64 {
        contours.iter().map(|c| signed_area(c).abs()).sum()
    }

    #[test]
    fn union_of_two_overlapping_squares_is_one_contour() {
        let a = square(0.0, 0.0, 10.0);
        let b = square(5.0, 5.0, 10.0);
        let merged = union_polygons(&a, &b);
        assert_eq!(merged.len(), 1, "overlapping squares join into one contour");
        // 10в”њР§10 + 10в”њР§10 С‚РРў 5в”њР§5 overlap = 175.
        assert!((area(&merged) - 175.0).abs() < 0.01, "area {}", area(&merged));
    }

    #[test]
    fn union_keeps_disjoint_shapes_and_containment() {
        let a = square(0.0, 0.0, 10.0);
        let b = square(40.0, 0.0, 10.0);
        assert_eq!(union_polygons(&a, &b).len(), 2, "disjoint squares stay apart");
        let inner = square(2.0, 2.0, 3.0);
        let merged = union_polygons(&a, &inner);
        assert_eq!(merged.len(), 1);
        assert!((area(&merged) - 100.0).abs() < 0.01, "contained shape adds nothing");
    }

    #[test]
    fn a_wedge_tail_is_merged_into_the_balloon_outline() {
        let body = crate::shapes::ellipse(0.0, 0.0, 100.0, 80.0);
        let tail = BalloonTail::wedge([(50.0, 80.0), (62.0, 80.0)], (80.0, 150.0));
        let merged = attach_tail(&body, &tail);
        assert_eq!(merged.subpaths.len(), 1, "the tail and body share one contour");
        assert!(merged.subpaths[0].closed);
        // The union keeps the body and adds the tail's area.
        let polygons = closed_contours(&merged);
        assert!((area(&polygons) - std::f64::consts::PI * 50.0 * 40.0).abs() > 100.0, "the tail adds area");
        // Every knot is finite.
        for knot in &merged.subpaths[0].knots {
            assert!(knot.anchor.x.is_finite() && knot.anchor.y.is_finite());
        }
    }

    #[test]
    fn a_thought_tail_makes_separate_bubbles() {
        let body = crate::shapes::ellipse(0.0, 0.0, 100.0, 80.0);
        let tail = BalloonTail::thought([(50.0, 78.0), (56.0, 78.0)], (90.0, 170.0));
        let merged = attach_tail(&body, &tail);
        assert!(merged.subpaths.len() >= 2, "body plus bubbles: {}", merged.subpaths.len());
    }

    #[test]
    fn cloud_is_closed_and_bounded_by_its_rect() {
        let path = cloud([0.0, 0.0, 200.0, 120.0], CloudParams::default());
        assert_eq!(path.subpaths.len(), 1);
        assert!(path.subpaths[0].closed);
        assert!(path.subpaths[0].knots.len() >= 9 * 6, "one sample run per bump");
        // The bumps bulge out of the inscribed ellipse, by at most the jittered amplitude.
        let pad = 60.0 * 0.18 * 1.35 + 1.0;
        for knot in &path.subpaths[0].knots {
            assert!((-pad..=200.0 + pad).contains(&knot.anchor.x), "x {}", knot.anchor.x);
            assert!((-pad..=120.0 + pad).contains(&knot.anchor.y), "y {}", knot.anchor.y);
        }
    }

    #[test]
    fn spiky_alternates_tips_and_valleys_deterministically() {
        let params = SpikeParams { spikes: 10, ..SpikeParams::default() };
        let a = spiky([0.0, 0.0, 100.0, 100.0], params);
        let b = spiky([0.0, 0.0, 100.0, 100.0], params);
        assert_eq!(a, b, "the same params and seed draw the same balloon");
        assert_eq!(a.subpaths[0].knots.len(), 20, "one valley and one tip per spike");
        let c = spiky([0.0, 0.0, 100.0, 100.0], SpikeParams { seed: 7, ..params });
        assert_ne!(a, c, "the seed changes the jitter");
    }

    #[test]
    fn sai_stroke_smooths_nodes_and_keeps_corners_sharp() {
        let points = [SaiPoint::new((0.0, 0.0)), SaiPoint::new((50.0, 40.0)), SaiPoint { p: (100.0, 0.0), pressure: 1.0, density: 1.0, corner: true }];
        let path = sai_stroke(&points, false);
        assert_eq!(path.subpaths.len(), 1);
        assert_eq!(path.subpaths[0].knots.len(), 3);
        assert!(path.subpaths[0].knots[1].smooth, "middle node is smoothed");
        assert!(!path.subpaths[0].knots[2].smooth, "corner node stays sharp");
        assert!(!path.subpaths[0].closed);
    }

    #[test]
    fn tapered_outline_tapers_with_pressure() {
        let points = [
            SaiPoint { p: (0.0, 0.0), pressure: 0.0, density: 1.0, corner: false },
            SaiPoint { p: (50.0, 0.0), pressure: 1.0, density: 1.0, corner: false },
            SaiPoint { p: (100.0, 0.0), pressure: 0.0, density: 1.0, corner: false },
        ];
        let outline = tapered_outline(&points, 10.0, 0.0);
        assert!(outline.len() > 6);
        assert!(outline.iter().all(|p| p.0.is_finite() && p.1.is_finite()));
        // The full-pressure middle is thicker than the tapered ends.
        let max_y = outline.iter().map(|p| p.1).fold(f64::MIN, f64::max);
        assert!(max_y > 4.0, "middle width reaches half the base width: {max_y}");
        let empty = tapered_outline(&points[..1], 10.0, 0.0);
        assert!(empty.is_empty(), "a single point has no stroke");
    }

    #[test]
    fn presets_round_trip_through_their_ids() {
        for preset in [LegacyTailPreset::Wedge, LegacyTailPreset::Arc, LegacyTailPreset::Double, LegacyTailPreset::Thought] {
            assert_eq!(LegacyTailPreset::from_id(preset.id()), Some(preset));
        }
        assert_eq!(LegacyTailPreset::from_id("nope"), None);
    }

    #[test]
    fn balloon_tail_geometry_stays_valid_when_tip_inside_body() {
        let rect = [100.0, 100.0, 240.0, 160.0];
        let center = [220.0, 180.0];
        let bodies = [
            LiveBalloonBody::Ellipse { rect, squircle: 0.2 },
            LiveBalloonBody::Cloud { rect, bumps: 10, amplitude: 0.2, cusp_bite: 0.25, jitter: 0.2, seed: 3 },
            LiveBalloonBody::Spiky { rect, spikes: 14, length: 0.45, sub_harmonic: 0.35, concavity: 0.7, jitter: 0.2, smooth: false, seed: 5 },
        ];
        for body in bodies {
            for preset in [TailPreset::Wedge, TailPreset::Arc, TailPreset::SCurve, TailPreset::Lightning, TailPreset::Double] {
                let balloon = LiveBalloon {
                    id: 1,
                    body: body.clone(),
                    tails: vec![LiveBalloonTail {
                        id: 1,
                        preset,
                        perimeter_t: 0.58,
                        mouth_width: 32.0,
                        tip: center,
                        bend: 0.45,
                        bubbles: 3,
                        bubble_radius: 10.0,
                    }],
                    next_tail_id: 2,
                    vertex_overrides: Vec::new(),
                    hand_drawn_noise: 0.0,
                    stroke_width: 2.5,
                    auto_merge_overlapping: true,
                };
                let path = balloon.rebuild_closed_path();
                assert!(!path.subpaths.is_empty(), "path should be non-empty for {preset:?}");
                for sp in &path.subpaths {
                    assert!(sp.closed);
                    for k in &sp.knots {
                        assert!(k.anchor.x.is_finite() && k.anchor.y.is_finite());
                        assert!(k.in_ctrl.x.is_finite() && k.in_ctrl.y.is_finite());
                        assert!(k.out_ctrl.x.is_finite() && k.out_ctrl.y.is_finite());
                    }
                }
                let polygons = closed_contours(&path);
                assert!(area(&polygons) > 1000.0, "positive contour area for {preset:?}");
            }
        }
    }
}
