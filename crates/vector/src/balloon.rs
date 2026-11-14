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
// Hand-drawn contour noise.
// ---------------------------------------------------------------------------------------------

/// One contour's deterministic outline noise: the same document always draws the same wobble.
///
/// The offset of every knot is a smooth (two-octave) function of its perimeter position, so the
/// contour keeps its proportions instead of turning into a ragged polygon; only the contour's
/// own vertices move, never the tails or the text area.
pub fn hand_drawn_offsets(points: &[P], amount: f64, seed: u32) -> Vec<P> {
    let n = points.len();
    let mut out = Vec::with_capacity(n);
    if n == 0 || !(amount.is_finite() && amount > 0.0) {
        return vec![(0.0, 0.0); n];
    }
    let amount = amount.clamp(0.0, 1.0);

    // The amplitude follows the balloon's own size, not the edge between its knots: a superellipse
    // has only four knots, so an edge-based amplitude would move its corners by tens of pixels and
    // fold the contour. The mean edge still caps it on a dense contour (a 128-spike shout or a
    // burst), where a size-based wobble alone would make neighbouring knots cross.
    let mut perimeter = 0.0;
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for i in 0..n {
        let p = points[i];
        perimeter += dist(p, points[(i + 1) % n]);
        min_x = min_x.min(p.0);
        min_y = min_y.min(p.1);
        max_x = max_x.max(p.0);
        max_y = max_y.max(p.1);
    }
    let min_dim = (max_x - min_x).min(max_y - min_y);
    let mean_edge = perimeter / n as f64;
    let amplitude = (min_dim.max(0.0) * 0.05).min(mean_edge.max(0.0) * 0.20) * amount;

    for i in 0..n {
        let t = i as f64 / n as f64;
        // Two octaves of the seeded hash read as a low-frequency wobble plus a small tremor. The
        // octaves carry different phase offsets (one of them seeded), so a four-knot contour does
        // not sample both sines at the same zeros.
        let a = hash01(seed, i as u32) - 0.5;
        let b = hash01(seed ^ 0x5F35_6495, i as u32) - 0.5;
        let phase = std::f64::consts::TAU * t;
        let seed_phase = hash01(seed ^ 0x9E37_79B9, 0) * std::f64::consts::TAU;
        let wobble = (phase * 3.0 + 0.7 + seed_phase).sin() * 0.45 + (phase * 7.0 + 2.1 + seed_phase * 1.7).sin() * 0.2 + a * 0.6 + b * 0.25;
        let prev = points[(i + n - 1) % n];
        let next = points[(i + 1) % n];
        let along = norm(sub(next, prev));
        let normal = if along == (0.0, 0.0) { (0.0, 0.0) } else { (-along.1, along.0) };
        out.push(mul(normal, wobble * amplitude));
    }
    out
}

/// The same contour translated by per-knot offsets, controls included, so every segment keeps
/// its shape and only its position wobbles.
fn perturbed_path(path: &Path, amount: f64, seed: u32) -> Path {
    if !(amount.is_finite() && amount > 0.0) {
        return path.clone();
    }
    let mut out = path.clone();
    for (sub_index, subpath) in out.subpaths.iter_mut().enumerate() {
        if !subpath.closed || subpath.knots.len() < 3 {
            continue;
        }
        let anchors: Vec<P> = subpath.knots.iter().map(|k| (k.anchor.x, k.anchor.y)).collect();
        let offsets = hand_drawn_offsets(&anchors, amount, seed ^ (sub_index as u32).wrapping_mul(0x2545_F491));
        for (knot, offset) in subpath.knots.iter_mut().zip(offsets) {
            knot.anchor = Point::new(knot.anchor.x + offset.0, knot.anchor.y + offset.1);
            knot.in_ctrl = Point::new(knot.in_ctrl.x + offset.0, knot.in_ctrl.y + offset.1);
            knot.out_ctrl = Point::new(knot.out_ctrl.x + offset.0, knot.out_ctrl.y + offset.1);
        }
    }
    out
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
    /// Canonical canvas-space handle positions `[x, y]` for draggable vertices (`Spiky` tips, `Cloud` cusps).
    fn vertex_handle_positions(&self) -> Vec<(u32, [f64; 2])>;
    /// The deterministic seed of the hand-drawn contour wobble.
    fn noise_seed(&self) -> u32;
    /// The balloon's closed outline, its tails merged in.
    fn rebuild_closed_path(&self) -> Path;
}

/// The geometry of the balloons of one layer.
pub trait LiveBalloonGroupGeom {
    /// The layer's outline: every balloon of the group, merged where they overlap.
    fn rebuild_group_path(&self) -> Path;
}

/// The parameters of a straight-edged burst, for the command that creates one without a live
/// balloon ([`burst`]).
#[derive(Clone, Copy, Debug)]
pub struct BurstParams {
    pub rays: u32,
    pub length: f64,
    pub depth: f64,
    pub jitter: f64,
    pub seed: u32,
}

impl Default for BurstParams {
    fn default() -> Self {
        Self { rays: 14, length: 0.60, depth: 0.26, jitter: 0.35, seed: 1 }
    }
}

/// A straight-edged explosion: a sharp star polygon whose tips and valleys both wander.
///
/// Unlike [`build_spiky_body`] every edge is a straight segment (no concave arc between the
/// rays) and the valleys bite well inside the radius, which is what makes an ink "burst" read as
/// an explosion rather than as a raised-voice balloon.
fn build_burst_body(balloon: &LiveBalloon, rect: [f64; 4], rays: u32, length: f64, depth: f64, jitter: f64, seed: u32) -> Path {
    burst_path(rect, rays, length, depth, jitter, seed, |i| balloon.get_vertex_offset(i))
}

/// A burst without live per-vertex drags (the plain `shape.balloon.create` path).
pub fn burst(rect: [f64; 4], params: BurstParams) -> Path {
    burst_path(rect, params.rays, params.length, params.depth, params.jitter, params.seed, |_| [0.0, 0.0])
}

fn burst_path(rect: [f64; 4], rays: u32, length: f64, depth: f64, jitter: f64, seed: u32, offset: impl Fn(u32) -> [f64; 2]) -> Path {
    let cx = rect[0] + rect[2] * 0.5;
    let cy = rect[1] + rect[3] * 0.5;
    let rx = (rect[2] * 0.5).abs().max(4.0);
    let ry = (rect[3] * 0.5).abs().max(4.0);
    let n = rays.clamp(5, 64);
    let step = std::f64::consts::TAU / (n as f64);
    let jit = jitter.clamp(0.0, 1.0);
    let length = length.clamp(0.10, 1.20);
    let depth = depth.clamp(0.0, 0.60);

    let mut points: Vec<P> = Vec::with_capacity(n as usize * 2);
    for i in 0..n {
        let a_tip = -std::f64::consts::FRAC_PI_2 + (i as f64) * step + (hash_f64(seed, i * 4) - 0.5) * jit * step * 0.5;
        let tip_r = 1.0 + length * (1.0 + (hash_f64(seed, i * 4 + 1) - 0.5) * jit * 0.8);
        let tip = (cx + rx * tip_r * a_tip.cos(), cy + ry * tip_r * a_tip.sin());

        let a_valley = a_tip + step * 0.5;
        let valley_r = (1.0 - depth * (1.0 + (hash_f64(seed, i * 4 + 2) - 0.5) * jit * 0.6)).clamp(0.30, 1.0);
        let valley = (cx + rx * valley_r * a_valley.cos(), cy + ry * valley_r * a_valley.sin());

        // A manually dragged tip moves the ray it belongs to.
        let off = offset(i);
        points.push((tip.0 + off[0], tip.1 + off[1]));
        points.push(valley);
    }

    Path::new(vec![Subpath::polygon(&points)])
}

/// The inner and the outer contour of a double outline, separately, so a tail can be welded into
/// each of them (see [`LiveBalloon::rebuild_closed_path`]).
fn double_outline_contours(rect: [f64; 4], squircle: f64, gap: f64) -> (Path, Path) {
    let cx = rect[0] + rect[2] * 0.5;
    let cy = rect[1] + rect[3] * 0.5;
    let rx = (rect[2] * 0.5).abs().max(4.0);
    let ry = (rect[3] * 0.5).abs().max(4.0);
    let gap = gap.clamp(0.5, 64.0).min(rx.min(ry) * 0.45);

    let inner = build_superellipse_path(cx, cy, rx - gap * 0.5, ry - gap * 0.5, squircle);
    let outer = build_superellipse_path(cx, cy, rx + gap * 0.5, ry + gap * 0.5, squircle);
    (inner, outer)
}

/// Two concentric outlines: the inner balloon contour and an outer echo `gap` px away.
///
/// The fill is the inner contour alone (the first subpath combines, the echo intersects it), so
/// the gap between the two lines stays transparent instead of being filled as a disc by the
/// NonZero rule. Both lines are stroked — the classic double-line balloon.
fn build_double_outline_body(rect: [f64; 4], squircle: f64, gap: f64) -> Path {
    let (mut inner, mut outer) = double_outline_contours(rect, squircle, gap);
    // The first subpath combines, every later one combines with it by op: fill = inner ∩ outer.
    for s in &mut inner.subpaths {
        s.op = PathOp::Combine;
    }
    for s in &mut outer.subpaths {
        s.op = PathOp::Intersect;
    }
    inner.subpaths.extend(outer.subpaths);
    Path::new(inner.subpaths)
}

/// A double outline without a live balloon (the plain `shape.balloon.create` path).
pub fn double_outline(rect: [f64; 4], squircle: f64, gap: f64) -> Path {
    build_double_outline_body(rect, squircle, gap)
}

/// Whether the hand-drawn wobble may move this body's contour.
///
/// Only bodies built from one closed contour are eligible. Uniflash is hundreds of independent
/// needle polygons and DoubleOutline is two contours: moving them by one shared offset either
/// tears the needles apart or makes the echo cross the inner line.
fn noise_applies(body: &LiveBalloonBody) -> bool {
    matches!(
        body,
        LiveBalloonBody::Ellipse { .. }
            | LiveBalloonBody::Rect { .. }
            | LiveBalloonBody::Cloud { .. }
            | LiveBalloonBody::Spiky { .. }
            | LiveBalloonBody::Burst { .. }
    )
}

/// The four knots of a superellipse (`squircle` 0 = ellipse, 1 = full squircle).
fn superellipse_knots(cx: f64, cy: f64, rx: f64, ry: f64, squircle: f64) -> Vec<Knot> {
    let k = 0.5522847498 + squircle.clamp(0.0, 0.85) * 0.28;
    let kx = rx * k;
    let ky = ry * k;
    vec![
        Knot { anchor: Point::new(cx, cy - ry), in_ctrl: Point::new(cx - kx, cy - ry), out_ctrl: Point::new(cx + kx, cy - ry), smooth: true },
        Knot { anchor: Point::new(cx + rx, cy), in_ctrl: Point::new(cx + rx, cy - ky), out_ctrl: Point::new(cx + rx, cy + ky), smooth: true },
        Knot { anchor: Point::new(cx, cy + ry), in_ctrl: Point::new(cx + kx, cy + ry), out_ctrl: Point::new(cx - kx, cy + ry), smooth: true },
        Knot { anchor: Point::new(cx - rx, cy), in_ctrl: Point::new(cx - rx, cy + ky), out_ctrl: Point::new(cx - rx, cy - ky), smooth: true },
    ]
}

/// A superellipse (`squircle` 0 = ellipse, 1 = full squircle) as a closed path.
fn build_superellipse_path(cx: f64, cy: f64, rx: f64, ry: f64, squircle: f64) -> Path {
    Path::new(vec![Subpath { knots: superellipse_knots(cx, cy, rx, ry, squircle), closed: true, op: PathOp::Combine }])
}

/// How many draggable bulges the oval body exposes. The same indices drive the geometry and the
/// gizmo handles, so a handle always moves the knot it is drawn on.
pub const OVAL_BULGES: u32 = 12;

/// How many of a Uniflash body's needles get a drag handle (the rest follow the shape sliders).
pub const UNIFLASH_HANDLES: u32 = 14;

/// One point of a cubic Bézier.
fn cubic_at(p0: P, c1: P, c2: P, p3: P, t: f64) -> P {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    (p0.0 * a + c1.0 * b + c2.0 * c + p3.0 * d, p0.1 * a + c1.1 * b + c2.1 * c + p3.1 * d)
}

/// The [`OVAL_BULGES`] bulge knots of the oval body, clockwise from the top.
///
/// They are sampled from the very cubics [`build_superellipse_path`] draws, so a bulge the user
/// grabs starts exactly on the contour instead of jumping to a different approximation of it.
fn oval_bulge_points(cx: f64, cy: f64, rx: f64, ry: f64, squircle: f64) -> Vec<P> {
    let knots = superellipse_knots(cx, cy, rx, ry, squircle);
    let n = knots.len();
    let mut out = Vec::with_capacity(n * 3);
    for i in 0..n {
        let a = &knots[i];
        let b = &knots[(i + 1) % n];
        let p0 = (a.anchor.x, a.anchor.y);
        let c1 = (a.out_ctrl.x, a.out_ctrl.y);
        let c2 = (b.in_ctrl.x, b.in_ctrl.y);
        let p3 = (b.anchor.x, b.anchor.y);
        // Three knots per quadrant: the corner plus two thirds points, so the twelve indices are
        // evenly spread over the contour.
        out.push(p0);
        out.push(cubic_at(p0, c1, c2, p3, 1.0 / 3.0));
        out.push(cubic_at(p0, c1, c2, p3, 2.0 / 3.0));
    }
    out
}

/// The outline of an oval whose bulges have been dragged: a smooth closed spline through the
/// sampled knots, each moved by its own manual offset.
fn build_bulged_oval(balloon: &LiveBalloon, cx: f64, cy: f64, rx: f64, ry: f64, squircle: f64) -> Path {
    let points: Vec<P> = oval_bulge_points(cx, cy, rx, ry, squircle)
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            let off = balloon.get_vertex_offset(i as u32);
            (p.0 + off[0], p.1 + off[1])
        })
        .collect();
    catmull_rom_path(&points, true)
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

        let out_angle = a0 + span * 0.28;
        let in_angle = a0 - span * 0.28;
        let r_out = 1.0 + cur_amp * 1.35;
        let r_in = 1.0 + prev_amp * 1.35;

        knots.push(Knot {
            anchor: Point::new(vx + cur_off[0], vy + cur_off[1]),
            in_ctrl: Point::new(cx + rx * r_in * in_angle.cos() + cur_off[0], cy + ry * r_in * in_angle.sin() + cur_off[1]),
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

/// The geometry of one Uniflash needle: the center line from the outer tip to the inner end, the
/// half width at each end (after the sharpness), and the unit perpendicular of the ray.
struct UniflashNeedle {
    outer: Point,
    inner: Point,
    half_outer: f64,
    half_inner: f64,
    perp: [f64; 2],
}

/// The needle geometry of a Uniflash body: straight rays from the outer rim to the inner mask
/// ellipse, with bundles and jitter. Shared by the closed-polygon renderer
/// ([`build_uniflash_paths`]) and the Linework bake ([`balloon_to_linework_strokes`]).
fn uniflash_needles(balloon: &LiveBalloon) -> Option<Vec<UniflashNeedle>> {
    let LiveBalloonBody::Uniflash {
        rect,
        needles,
        inner_ratio,
        bundle_group,
        length_jitter,
        angle_jitter,
        needle_width,
        outer_sharpness,
        inner_sharpness,
        seed,
    } = &balloon.body
    else {
        return None;
    };

    let cx = rect[0] + rect[2] * 0.5;
    let cy = rect[1] + rect[3] * 0.5;
    let rx = (rect[2] * 0.5).abs().max(4.0);
    let ry = (rect[3] * 0.5).abs().max(4.0);
    let count = (*needles).clamp(24, 450);
    let step = std::f64::consts::TAU / (count as f64);

    // The end widths of a needle follow the sharpness: 1.0 = a point, 0.0 = the full needle
    // width, both through the mean half width `needle_width / 2`.
    let half_w = needle_width.clamp(0.2, 64.0) * 0.5;
    let half_outer = half_w * (1.0 - outer_sharpness.clamp(0.0, 1.0));
    let half_inner = half_w * (1.0 - inner_sharpness.clamp(0.0, 1.0));

    let mut out = Vec::with_capacity(count as usize);
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
        let outer = Point::new(cx + rx * r_out * cos, cy + ry * r_out * sin);
        let inner = Point::new(cx + rx * r_in * cos + user_off[0], cy + ry * r_in * sin + user_off[1]);

        out.push(UniflashNeedle { outer, inner, half_outer, half_inner, perp: [-sin, cos] });
    }
    Some(out)
}

/// A sea-urchin flash: every needle is a closed wedge polygon (4 knots) filled with the stroke
/// colour. The white oval `bg_mask` is gone (TZ v5.0) — no parasitic oval inside the burst in
/// edit mode; the layer's fill carries the white under the needles instead. 400 needles stay far
/// below the 4096-knot budget (4 x 400 = 1600).
fn build_uniflash_paths(balloon: &LiveBalloon) -> Option<Path> {
    let needles = uniflash_needles(balloon)?;
    let subpaths = needles
        .into_iter()
        .map(|n| {
            Subpath::polygon(&[
                (n.outer.x + n.perp[0] * n.half_outer, n.outer.y + n.perp[1] * n.half_outer),
                (n.inner.x + n.perp[0] * n.half_inner, n.inner.y + n.perp[1] * n.half_inner),
                (n.inner.x - n.perp[0] * n.half_inner, n.inner.y - n.perp[1] * n.half_inner),
                (n.outer.x - n.perp[0] * n.half_outer, n.outer.y - n.perp[1] * n.half_outer),
            ])
        })
        .collect();
    Some(Path::new(subpaths))
}

/// A hollow ring (a keyhole annulus): the outer circle followed by the inner circle walked
/// backwards, as one closed polygon that any fill rule renders as a ring.
fn hollow_ring(cx: f64, cy: f64, r_outer: f64, r_inner: f64) -> Subpath {
    let n = 20usize;
    let mut pts = Vec::with_capacity(2 * n);
    for i in 0..n {
        let a = std::f64::consts::TAU * (i as f64) / (n as f64);
        pts.push((cx + r_outer * a.cos(), cy + r_outer * a.sin()));
    }
    for i in (0..n).rev() {
        let a = std::f64::consts::TAU * (i as f64) / (n as f64);
        pts.push((cx + r_inner * a.cos(), cy + r_inner * a.sin()));
    }
    Subpath::polygon(&pts)
}

/// The Uniflash tail geometry: the two wall segments (each `[start, tip]`) and the thought
/// bubbles as `(center, outer radius)`.
type UniflashTailGeoms = (Vec<[[f64; 2]; 2]>, Vec<(Point, f64)>);

/// The Uniflash tail geometry: the two wall centerlines (the left and the right mouth edge to the
/// tip) of a solid tail and the (center, outer radius) of every thought bubble. The whole
/// Uniflash layer is filled with the stroke colour (the needles are closed polygons, the white
/// mask is gone), so a solid tail wedge would become a black blob: the walls are thin wedges of
/// the balloon's stroke width and the bubbles are hollow rings (TZ v5.0, Нюанс №3).
fn uniflash_tail_geoms(balloon: &LiveBalloon, t: &LiveBalloonTail) -> UniflashTailGeoms {
    let (mouth_center, normal) = balloon.point_and_normal_on_body(t.perimeter_t);
    let tangent = [-normal[1], normal[0]];
    let r = balloon.body.rect();
    let min_half = (r[2].abs().min(r[3].abs()) * 0.5).max(4.0);
    let half_w = t.mouth_width.clamp(4.0, min_half * 1.6) * 0.5;

    let min_outward = (min_half * 0.12).max(4.0);
    let rel_mouth = [t.tip[0] - mouth_center[0], t.tip[1] - mouth_center[1]];
    let outward_proj = rel_mouth[0] * normal[0] + rel_mouth[1] * normal[1];
    let tang_proj = rel_mouth[0] * tangent[0] + rel_mouth[1] * tangent[1];
    let eff_tip = if outward_proj < min_outward {
        [mouth_center[0] + normal[0] * min_outward + tangent[0] * tang_proj, mouth_center[1] + normal[1] * min_outward + tangent[1] * tang_proj]
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
        let mut bubbles = Vec::with_capacity(count as usize);
        for i in 0..count {
            let s = if count == 1 { 1.0 } else { 0.22 + 0.78 * ((i as f64) / ((count - 1) as f64)) };
            let omt = 1.0 - s;
            let bx = omt * omt * mouth_center[0] + 2.0 * omt * s * ctrl[0] + s * s * eff_tip[0];
            let by = omt * omt * mouth_center[1] + 2.0 * omt * s * ctrl[1] + s * s * eff_tip[1];
            let rad = (t.bubble_radius * (1.0 - 0.65 * ((i as f64) / (count.max(1) as f64)))).max(2.0);
            bubbles.push((Point::new(bx, by), rad));
        }
        (Vec::new(), bubbles)
    } else {
        let mouth_left = [mouth_center[0] - tangent[0] * half_w, mouth_center[1] - tangent[1] * half_w];
        let mouth_right = [mouth_center[0] + tangent[0] * half_w, mouth_center[1] + tangent[1] * half_w];
        (vec![[[mouth_left[0], mouth_left[1]], [eff_tip[0], eff_tip[1]]], [[mouth_right[0], mouth_right[1]], [eff_tip[0], eff_tip[1]]]], Vec::new())
    }
}

/// One tail's geometry: the outline welded to the body (if any) and the detached thought
/// bubbles (if any).
fn build_single_tail_path(balloon: &LiveBalloon, t: &LiveBalloonTail) -> (Option<Path>, Option<Path>) {
    // An explicitly disabled tail (`⊘ Нет`): no geometry at all.
    if t.preset == TailPreset::None {
        return (None, None);
    }
    let (analytic_center, analytic_normal) = balloon.point_and_normal_on_body(t.perimeter_t);
    // The wobble moves the body's edge by a few px, so the mouth is re-seated on the contour that
    // is actually drawn; otherwise the tail would float off the noisy body at its own mouth.
    let (mouth_center, normal) = if balloon.hand_drawn_noise > 0.0 && noise_applies(&balloon.body) {
        nearest_mouth_on_noisy_body(balloon, analytic_center, analytic_normal)
    } else {
        (analytic_center, analytic_normal)
    };
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
        [mouth_center[0] + normal[0] * min_outward + tangent[0] * tang_proj, mouth_center[1] + normal[1] * min_outward + tangent[1] * tang_proj]
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

    // On a Uniflash body the whole layer is filled with the stroke colour (the needles are closed
    // polygons and the white mask is gone), so a solid tail wedge would turn into a black blob:
    // build the tail as two thin wedge walls (the left and the right mouth edge to the tip) of
    // the stroke width, and the thought bubbles as hollow rings (TZ v5.0, Нюанс №3).
    if matches!(balloon.body, LiveBalloonBody::Uniflash { .. }) {
        let (walls, bubbles) = uniflash_tail_geoms(balloon, t);
        if !bubbles.is_empty() {
            let subpaths =
                bubbles.into_iter().map(|(center, rad)| hollow_ring(center.x, center.y, rad, (rad - balloon.stroke_width).max(rad * 0.35).max(1.0))).collect();
            return (None, Some(Path::new(subpaths)));
        }
        let w = balloon.stroke_width.clamp(0.5, 64.0) * 0.5;
        let subpaths = walls
            .into_iter()
            .map(|seg| {
                let (a, b) = (seg[0], seg[1]);
                let dx = b[0] - a[0];
                let dy = b[1] - a[1];
                let len = (dx * dx + dy * dy).sqrt().max(1e-6);
                let (px, py) = (-dy / len * w, dx / len * w);
                Subpath::polygon(&[(a[0] + px, a[1] + py), (b[0] + px, b[1] + py), (b[0] - px, b[1] - py), (a[0] - px, a[1] - py)])
            })
            .collect();
        return (None, Some(Path::new(subpaths)));
    }

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
        // The disabled tail is handled at the top (the arm keeps the match exhaustive).
        TailPreset::None => return (None, None),
    };

    (Some(Path::new(vec![subpath])), None)
}

/// The mouth of a tail re-seated on the perturbed body contour.
///
/// The analytic [`LiveBalloonGeom::point_and_normal_on_body`] knows only the clean body; the
/// wobble moves every knot by a few px, so the tail is welded to the nearest point of the contour
/// that is actually drawn. The normal keeps the analytic orientation (outward), so the tip-side
/// logic in [`build_single_tail_path`] is unchanged.
fn nearest_mouth_on_noisy_body(balloon: &LiveBalloon, fallback: [f64; 2], fallback_normal: [f64; 2]) -> ([f64; 2], [f64; 2]) {
    let path = body_outline_with_noise(balloon);
    let rings = crate::flatten::flatten_path(&path, UNION_TOL);
    let Some(ring) = rings.first() else { return (fallback, fallback_normal) };
    let pts = &ring.pts;
    let n = pts.len();
    if n < 2 {
        return (fallback, fallback_normal);
    }
    let mut best = f64::INFINITY;
    let mut result = (fallback, fallback_normal);
    for i in 0..n {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = dx * dx + dy * dy;
        if len2 <= 1e-12 {
            continue;
        }
        let t = (((fallback[0] - a.0) * dx + (fallback[1] - a.1) * dy) / len2).clamp(0.0, 1.0);
        let p = (a.0 + dx * t, a.1 + dy * t);
        let d = (p.0 - fallback[0]).powi(2) + (p.1 - fallback[1]).powi(2);
        if d < best {
            best = d;
            let len = len2.sqrt();
            let mut normal = (dy / len, -dx / len);
            if normal.0 * fallback_normal[0] + normal.1 * fallback_normal[1] < 0.0 {
                normal = (-normal.0, -normal.1);
            }
            result = ([p.0, p.1], [normal.0, normal.1]);
        }
    }
    result
}

/// The body outline alone (no tails), with the hand-drawn wobble applied where it is allowed.
///
/// `LiveBalloon` lives in `photocraft-doc`, so this stays a free function: the vector crate only
/// sees the geometry through the traits.
fn body_outline_with_noise(balloon: &LiveBalloon) -> Path {
    let path = match &balloon.body {
        LiveBalloonBody::Ellipse { rect, squircle } => {
            let cx = rect[0] + rect[2] * 0.5;
            let cy = rect[1] + rect[3] * 0.5;
            let rx = (rect[2] * 0.5).abs();
            let ry = (rect[3] * 0.5).abs();
            // With no dragged bulge the exact superellipse is kept; the moment a real oval
            // bulge is moved the contour becomes a spline through the `OVAL_BULGES` knots.
            if !has_oval_bulge_override(balloon) {
                build_superellipse_path(cx, cy, rx, ry, *squircle)
            } else {
                build_bulged_oval(balloon, cx, cy, rx, ry, *squircle)
            }
        }
        LiveBalloonBody::Rect { rect, corner_radius } => {
            let r = *corner_radius;
            crate::shapes::rounded_rect(rect[0], rect[1], rect[2], rect[3], [r, r, r, r])
        }
        LiveBalloonBody::Cloud { rect, bumps, amplitude, cusp_bite, jitter, seed } => {
            build_cloud_body(balloon, *rect, *bumps, *amplitude, *cusp_bite, *jitter, *seed)
        }
        LiveBalloonBody::Spiky { rect, spikes, length, sub_harmonic, concavity, jitter, smooth, seed } => build_spiky_body(
            balloon,
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
        LiveBalloonBody::Burst { rect, rays, length, depth, jitter, seed } => build_burst_body(balloon, *rect, *rays, *length, *depth, *jitter, *seed),
        LiveBalloonBody::DoubleOutline { rect, squircle, gap } => build_double_outline_body(*rect, *squircle, *gap),
        // The needles are closed wedge polygons filled with the stroke colour; the white oval
        // mask is gone (TZ v5.0). The tails are appended below like for every other body,
        // so active Uniflash tails reach `balloon_to_linework_strokes` too.
        LiveBalloonBody::Uniflash { .. } => build_uniflash_paths(balloon).unwrap_or_default(),
    };

    // The wobble moves the body's own knots only, before the tails are welded, so the tail
    // edges stay straight and the union never sees two noisy boundaries. Multi-contour bodies
    // (Uniflash, DoubleOutline) are excluded: one shared offset would tear them apart.
    if balloon.hand_drawn_noise > 0.0 && noise_applies(&balloon.body) { perturbed_path(&path, balloon.hand_drawn_noise, balloon.noise_seed()) } else { path }
}

/// Whether an ellipse override index really belongs to one of the [`OVAL_BULGES`] sampled
/// knots. Older documents (or a leftover override from another body kind) can carry indices
/// past the oval, which must not silently switch the exact superellipse to a spline.
fn has_oval_bulge_override(balloon: &LiveBalloon) -> bool {
    balloon.vertex_overrides.iter().any(|v| v.index < OVAL_BULGES)
}

/// The double outline: both contours are welded to the tails independently, then composited as
/// a ring. Routing the pair through one polygonal union would flatten the echo into the inner
/// contour and lose the inner line.
fn rebuild_double_outline(balloon: &LiveBalloon, rect: [f64; 4], squircle: f64, gap: f64) -> Path {
    let (mut inner, mut outer) = double_outline_contours(rect, squircle, gap);
    let mut detached_subpaths = Vec::new();
    for tail in &balloon.tails {
        let (welded, detached) = build_single_tail_path(balloon, tail);
        if let Some(w) = welded {
            inner = merge_paths_union(&inner, &w);
            outer = merge_paths_union(&outer, &w);
        }
        if let Some(d) = detached {
            detached_subpaths.extend(d.subpaths);
        }
    }
    for s in &mut inner.subpaths {
        s.op = PathOp::Combine;
    }
    for s in &mut outer.subpaths {
        s.op = PathOp::Intersect;
    }
    let mut subpaths = inner.subpaths;
    subpaths.extend(outer.subpaths);
    subpaths.extend(detached_subpaths);
    Path::new(subpaths)
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
            LiveBalloonBody::Burst { length, depth, .. } => {
                // The text sits in the star's core: inside the valleys, not out at the rays.
                let f = (1.0 - length * 0.55 - depth * 0.35).clamp(0.28, 0.80) * 0.66;
                (rx * f, ry * f)
            }
            LiveBalloonBody::DoubleOutline { squircle, gap, .. } => {
                // Only the inner contour holds the words.
                let f = (0.71 + 0.17 * squircle.clamp(0.0, 1.0)) * 0.94;
                (((rx - gap * 0.5) * f).max(4.0), ((ry - gap * 0.5) * f).max(4.0))
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
            LiveBalloonBody::Burst { depth, .. } => {
                // The text and the tail sit in the star's core: anchor on the valley contour, not
                // at the ray tips (which would leave the mouth floating in a gap).
                let f = (1.0 - depth.clamp(0.0, 0.60) * 0.85).clamp(0.32, 1.0);
                let bx = rx * f;
                let by = ry * f;
                let px = cx + bx * angle.cos();
                let py = cy + by * angle.sin();
                let ux = angle.cos() / bx.max(1.0);
                let uy = angle.sin() / by.max(1.0);
                let len = (ux * ux + uy * uy).sqrt().max(1e-9);
                ([px, py], [ux / len, uy / len])
            }
            LiveBalloonBody::Uniflash { inner_ratio, .. } => {
                // Anchor the tail mouth on the flash's inner ellipse (TZ v5.0:
                // rx * inner_ratio.clamp(0.25, 0.92)), so it never hangs on the needles.
                let f = inner_ratio.clamp(0.25, 0.92);
                let bx = rx * f;
                let by = ry * f;
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

    fn vertex_handle_positions(&self) -> Vec<(u32, [f64; 2])> {
        let mut out = Vec::new();
        let r = self.body.rect();
        let cx = r[0] + r[2] * 0.5;
        let cy = r[1] + r[3] * 0.5;
        let rx = (r[2] * 0.5).abs().max(4.0);
        let ry = (r[3] * 0.5).abs().max(4.0);

        match &self.body {
            LiveBalloonBody::Cloud { bumps, cusp_bite, jitter, seed, .. } => {
                let n = (*bumps).clamp(4, 64);
                let step = std::f64::consts::TAU / (n as f64);
                let inward_factor = 1.0 - cusp_bite.clamp(0.0, 0.5) * 0.35;
                let jit = jitter.clamp(0.0, 0.8);
                for i in 0..n {
                    let j = (hash_f64(*seed, i * 2) - 0.5) * jit * step * 0.55;
                    let a = -std::f64::consts::FRAC_PI_2 + (i as f64) * step + j;
                    let off = self.get_vertex_offset(i);
                    out.push((i, [cx + rx * inward_factor * a.cos() + off[0], cy + ry * inward_factor * a.sin() + off[1]]));
                }
            }
            LiveBalloonBody::Spiky { spikes, length, sub_harmonic, jitter, seed, .. } => {
                let n = (*spikes).clamp(5, 128);
                let step = std::f64::consts::TAU / (n as f64);
                let sub_h = sub_harmonic.clamp(0.0, 1.0);
                for i in 0..n {
                    let a_jitter = (hash_f64(*seed, i * 3) - 0.5) * jitter * step * 0.4;
                    let angle = -std::f64::consts::FRAC_PI_2 + (i as f64) * step + a_jitter;
                    let harmonic_mult = match i % 3 {
                        0 => 1.0,
                        1 => 1.0 - sub_h * 0.48,
                        _ => 1.0 - sub_h * 0.28,
                    };
                    let rand_mult = 1.0 + (hash_f64(*seed, i * 3 + 1) - 0.5) * jitter * 0.6;
                    let tip_r = 1.0 + length * harmonic_mult * rand_mult;
                    let off = self.get_vertex_offset(i);
                    out.push((i, [cx + rx * tip_r * angle.cos() + off[0], cy + ry * tip_r * angle.sin() + off[1]]));
                }
            }
            LiveBalloonBody::Burst { rays, length, jitter, seed, .. } => {
                let n = (*rays).clamp(5, 64);
                let step = std::f64::consts::TAU / (n as f64);
                let jit = jitter.clamp(0.0, 1.0);
                let len = length.clamp(0.10, 1.20);
                for i in 0..n {
                    let a_tip = -std::f64::consts::FRAC_PI_2 + (i as f64) * step + (hash_f64(*seed, i * 4) - 0.5) * jit * step * 0.5;
                    let tip_r = 1.0 + len * (1.0 + (hash_f64(*seed, i * 4 + 1) - 0.5) * jit * 0.8);
                    let off = self.get_vertex_offset(i);
                    out.push((i, [cx + rx * tip_r * a_tip.cos() + off[0], cy + ry * tip_r * a_tip.sin() + off[1]]));
                }
            }
            LiveBalloonBody::Ellipse { squircle, .. } => {
                for (i, p) in oval_bulge_points(cx, cy, rx, ry, *squircle).into_iter().enumerate() {
                    let off = self.get_vertex_offset(i as u32);
                    out.push((i as u32, [p.0 + off[0], p.1 + off[1]]));
                }
            }
            LiveBalloonBody::Uniflash { needles, inner_ratio, bundle_group, length_jitter, angle_jitter, seed, .. } => {
                // Uniflash has up to 400 needles: a handle on every one would be an unreadable
                // wall of dots, so an evenly spread sample is exposed. The index stays the real
                // needle index, so a dragged handle still moves the needle it sits on.
                let n = (*needles).clamp(24, 450);
                let shown = n.min(UNIFLASH_HANDLES);
                let stride = n as f64 / shown as f64;
                let step = std::f64::consts::TAU / (n as f64);
                for slot in 0..shown {
                    let i = ((slot as f64) * stride).round() as u32;
                    let rand_a = hash_f64(*seed, i * 3) - 0.5;
                    let rand_in = hash_f64(*seed, i * 3 + 1);
                    let bundle_phase = ((i as f64) * 0.35 * std::f64::consts::TAU).sin();
                    let bundle_shift = bundle_phase * step * 0.35 * bundle_group.clamp(0.0, 1.0);
                    let theta = (i as f64) * step + bundle_shift + rand_a * step * angle_jitter.clamp(0.0, 1.0);
                    let harmonic_in = if i % 2 == 0 { 1.0 } else { 0.85 };
                    let r_in = inner_ratio * (1.0 - 0.32 * length_jitter * rand_in * harmonic_in);
                    let off = self.get_vertex_offset(i);
                    out.push((i, [cx + rx * r_in * theta.cos() + off[0], cy + ry * r_in * theta.sin() + off[1]]));
                }
            }
            _ => {}
        }
        out
    }

    fn noise_seed(&self) -> u32 {
        // The body's own seed when it has one, so a wobble follows the shape's variation; the
        // balloon id separates two otherwise identical bodies.
        let body_seed = match &self.body {
            LiveBalloonBody::Cloud { seed, .. }
            | LiveBalloonBody::Spiky { seed, .. }
            | LiveBalloonBody::Burst { seed, .. }
            | LiveBalloonBody::Uniflash { seed, .. } => *seed,
            LiveBalloonBody::Ellipse { .. } | LiveBalloonBody::Rect { .. } | LiveBalloonBody::DoubleOutline { .. } => 0x9E37_79B9,
        };
        body_seed ^ self.id.wrapping_mul(0x85EB_CA6B)
    }

    fn rebuild_closed_path(&self) -> Path {
        // A double outline is two contours with a ring fill: the tails are welded into each
        // contour separately and the pair is composited by path operations, never unioned.
        if let LiveBalloonBody::DoubleOutline { rect, squircle, gap } = &self.body {
            return rebuild_double_outline(self, *rect, *squircle, *gap);
        }

        let mut base_path = body_outline_with_noise(self);

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
            if b.auto_merge_overlapping && !matches!(b.body, LiveBalloonBody::Uniflash { .. } | LiveBalloonBody::DoubleOutline { .. }) {
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
    // The sea urchin: every needle becomes a 3-node stroke whose end pressures carry the
    // sharpness (`1.0 - 0.95 * sharpness`), and the active tails become wall/ring strokes.
    if let LiveBalloonBody::Uniflash { needle_width, outer_sharpness, inner_sharpness, .. } = &b.body
        && let Some(needles) = uniflash_needles(b)
    {
        let mut strokes: Vec<LiveVectorStroke> = needles
            .into_iter()
            .map(|n| LiveVectorStroke {
                nodes: vec![
                    SaiPoint { p: (n.outer.x, n.outer.y), pressure: (1.0 - 0.95 * outer_sharpness.clamp(0.0, 1.0)) as f32, density: 1.0, corner: true },
                    SaiPoint { p: ((n.outer.x + n.inner.x) * 0.5, (n.outer.y + n.inner.y) * 0.5), pressure: 1.0, density: 1.0, corner: true },
                    SaiPoint { p: (n.inner.x, n.inner.y), pressure: (1.0 - 0.95 * inner_sharpness.clamp(0.0, 1.0)) as f32, density: 1.0, corner: true },
                ],
                base_width: *needle_width,
                min_ratio: 0.0,
                base_density: 1.0,
                min_density_ratio: 0.0,
                curve_type: SaiCurveType::Normal,
                tapered: true,
                closed: false,
                tension: SAI_DEFAULT_TENSION,
            })
            .collect();

        // The active tails: the wedge walls become straight strokes of the stroke width, the
        // thought bubbles become closed ring strokes around each bubble.
        for t in &b.tails {
            if t.preset == TailPreset::None {
                continue;
            }
            let (walls, bubbles) = uniflash_tail_geoms(b, t);
            for seg in walls {
                let (a, c) = (seg[0], seg[1]);
                strokes.push(LiveVectorStroke {
                    nodes: vec![
                        SaiPoint { p: (a[0], a[1]), pressure: 1.0, density: 1.0, corner: true },
                        SaiPoint { p: (c[0], c[1]), pressure: 1.0, density: 1.0, corner: true },
                    ],
                    base_width: b.stroke_width,
                    min_ratio: 0.0,
                    base_density: 1.0,
                    min_density_ratio: 0.0,
                    curve_type: SaiCurveType::Normal,
                    tapered: true,
                    closed: false,
                    tension: SAI_DEFAULT_TENSION,
                });
            }
            for (center, rad) in bubbles {
                let ring_r = (rad - b.stroke_width * 0.5).max(1.0);
                let nodes = (0..12)
                    .map(|k| {
                        let a = std::f64::consts::TAU * (k as f64) / 12.0;
                        SaiPoint { p: (center.x + ring_r * a.cos(), center.y + ring_r * a.sin()), pressure: 1.0, density: 1.0, corner: false }
                    })
                    .collect();
                strokes.push(LiveVectorStroke {
                    nodes,
                    base_width: b.stroke_width,
                    min_ratio: 0.0,
                    base_density: 1.0,
                    min_density_ratio: 0.0,
                    curve_type: SaiCurveType::Normal,
                    tapered: true,
                    closed: true,
                    tension: SAI_DEFAULT_TENSION,
                });
            }
        }
        return strokes;
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
                let steps = if seg_count > 36 { 2 } else { 4 };
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

    #[test]
    fn vertex_handle_positions_match_spiky_and_cloud_knots_with_offsets() {
        use photocraft_doc::vector::VertexOverride;

        let rect = [100.0, 120.0, 240.0, 160.0];
        let spiky = LiveBalloon {
            id: 1,
            body: LiveBalloonBody::Spiky { rect, spikes: 18, length: 0.38, sub_harmonic: 0.45, concavity: 0.65, jitter: 0.30, smooth: false, seed: 101 },
            tails: Vec::new(),
            next_tail_id: 2,
            vertex_overrides: vec![VertexOverride { index: 0, offset: [12.0, -18.0] }, VertexOverride { index: 5, offset: [-7.0, 9.0] }],
            hand_drawn_noise: 0.0,
            stroke_width: 2.5,
            auto_merge_overlapping: false,
        };
        let spiky_path = spiky.rebuild_closed_path();
        let spiky_handles = spiky.vertex_handle_positions();
        assert_eq!(spiky_handles.len(), spiky_path.subpaths[0].knots.len());
        for ((_, pos), knot) in spiky_handles.iter().zip(&spiky_path.subpaths[0].knots) {
            assert!((pos[0] - knot.anchor.x).abs() < 1e-6 && (pos[1] - knot.anchor.y).abs() < 1e-6);
        }

        let cloud = LiveBalloon {
            id: 2,
            body: LiveBalloonBody::Cloud { rect, bumps: 10, amplitude: 0.18, cusp_bite: 0.22, jitter: 0.35, seed: 42 },
            tails: Vec::new(),
            next_tail_id: 2,
            vertex_overrides: vec![VertexOverride { index: 2, offset: [15.0, -11.0] }],
            hand_drawn_noise: 0.0,
            stroke_width: 2.5,
            auto_merge_overlapping: false,
        };
        let cloud_path = cloud.rebuild_closed_path();
        let cloud_handles = cloud.vertex_handle_positions();
        assert_eq!(cloud_handles.len(), cloud_path.subpaths[0].knots.len());
        for ((_, pos), knot) in cloud_handles.iter().zip(&cloud_path.subpaths[0].knots) {
            assert!((pos[0] - knot.anchor.x).abs() < 1e-6 && (pos[1] - knot.anchor.y).abs() < 1e-6);
        }
    }

    #[test]
    fn converting_large_spiky_and_uniflash_balloons_to_linework_passes_validate_shape() {
        use crate::comic_sai_curve::LiveLinework;

        let large_spiky = LiveBalloon {
            id: 1,
            body: LiveBalloonBody::Spiky {
                rect: [50.0, 50.0, 900.0, 700.0],
                spikes: 32,
                length: 0.45,
                sub_harmonic: 0.45,
                concavity: 0.65,
                jitter: 0.30,
                smooth: true,
                seed: 101,
            },
            tails: Vec::new(),
            next_tail_id: 2,
            vertex_overrides: Vec::new(),
            hand_drawn_noise: 0.0,
            stroke_width: 2.5,
            auto_merge_overlapping: false,
        };
        let lw_spiky = LiveLinework::new(balloon_to_linework_strokes(&large_spiky));
        let path_spiky = lw_spiky.rebuild_path();
        assert!(!path_spiky.is_empty());
        assert!(crate::flatten::validate_shape(&path_spiky, None).is_ok());

        let uniflash = LiveBalloon {
            id: 2,
            body: LiveBalloonBody::Uniflash {
                rect: [100.0, 100.0, 600.0, 480.0],
                needles: 160,
                inner_ratio: 0.62,
                bundle_group: 0.40,
                length_jitter: 0.5,
                angle_jitter: 0.25,
                needle_width: 2.2,
                outer_sharpness: 0.0,
                inner_sharpness: 1.0,
                seed: 777,
            },
            tails: Vec::new(),
            next_tail_id: 2,
            vertex_overrides: Vec::new(),
            hand_drawn_noise: 0.0,
            stroke_width: 2.5,
            auto_merge_overlapping: false,
        };
        let lw_uni = LiveLinework::new(balloon_to_linework_strokes(&uniflash));
        let path_uni = lw_uni.rebuild_path();
        assert!(!path_uni.is_empty());
        assert!(crate::flatten::validate_shape(&path_uni, None).is_ok());
    }

    /// The hand-drawn wobble moves the contour, never the tails, and the same seed always draws
    /// the same balloon.
    #[test]
    fn hand_drawn_noise_moves_the_contour_deterministically() {
        use photocraft_doc::vector::VertexOverride;

        let rect = [100.0, 120.0, 240.0, 160.0];
        let plain = LiveBalloon {
            id: 7,
            body: LiveBalloonBody::Ellipse { rect, squircle: 0.0 },
            tails: Vec::new(),
            next_tail_id: 2,
            vertex_overrides: Vec::new(),
            hand_drawn_noise: 0.0,
            stroke_width: 2.5,
            auto_merge_overlapping: false,
        };
        let noisy = LiveBalloon { hand_drawn_noise: 0.5, ..plain.clone() };
        let noisy_again = LiveBalloon { hand_drawn_noise: 0.5, ..plain.clone() };

        let plain_path = plain.rebuild_closed_path();
        let noisy_path = noisy.rebuild_closed_path();
        assert_eq!(noisy_path.subpaths[0].knots.len(), plain_path.subpaths[0].knots.len(), "the wobble keeps the knot count");
        assert!(
            noisy_path.subpaths[0]
                .knots
                .iter()
                .zip(&plain_path.subpaths[0].knots)
                .any(|(a, b)| ((a.anchor.x - b.anchor.x).powi(2) + (a.anchor.y - b.anchor.y).powi(2)).sqrt() > 0.5),
            "the noise must move the contour"
        );

        let again = noisy_again.rebuild_closed_path();
        assert!(
            noisy_path.subpaths[0]
                .knots
                .iter()
                .zip(&again.subpaths[0].knots)
                .all(|(a, b)| (a.anchor.x - b.anchor.x).abs() < 1e-9 && (a.anchor.y - b.anchor.y).abs() < 1e-9),
            "the same seed must draw the same balloon"
        );

        // Another balloon of the same shape gets its own wobble: the id separates them.
        let other = LiveBalloon { id: 8, hand_drawn_noise: 0.5, ..plain.clone() };
        let other_path = other.rebuild_closed_path();
        assert!(
            noisy_path.subpaths[0].knots.iter().zip(&other_path.subpaths[0].knots).any(|(a, b)| (a.anchor.x - b.anchor.x).abs() > 0.01),
            "two balloons must not share one wobble"
        );

        // The tail is welded after the wobble, so it stays attached to the moved body.
        let mut tailed = noisy.clone();
        tailed.tails = vec![LiveBalloonTail {
            id: 1,
            preset: TailPreset::Wedge,
            perimeter_t: 0.25,
            mouth_width: 28.0,
            tip: [40.0, 60.0],
            bend: 0.0,
            bubbles: 3,
            bubble_radius: 10.0,
        }];
        let tailed_path = tailed.rebuild_closed_path();
        assert!(crate::flatten::validate_shape(&tailed_path, None).is_ok());

        // A dragged bulge is not the noise: it is an explicit offset.
        let bulged = LiveBalloon { vertex_overrides: vec![VertexOverride { index: 0, offset: [10.0, -6.0] }], ..plain.clone() };
        let bulged_path = bulged.rebuild_closed_path();
        assert!(
            bulged_path.subpaths[0].knots.iter().zip(&plain_path.subpaths[0].knots).any(|(a, b)| (a.anchor.x - b.anchor.x).abs() > 0.5),
            "a dragged bulge must move the contour"
        );
    }

    /// The offsets of the wobble are a pure function of the contour, the amount and the seed.
    #[test]
    fn hand_drawn_offsets_are_a_pure_function() {
        let points = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        assert!(hand_drawn_offsets(&points, 0.0, 5).iter().all(|o| *o == (0.0, 0.0)), "no noise means no offsets");
        let a = hand_drawn_offsets(&points, 0.4, 5);
        let b = hand_drawn_offsets(&points, 0.4, 5);
        assert_eq!(a, b, "the same seed gives the same offsets");
        let c = hand_drawn_offsets(&points, 0.4, 6);
        assert_ne!(a, c, "another seed gives another wobble");
        assert!(a.iter().any(|o| o.0.abs() + o.1.abs() > 0.01), "the offsets are not all zero");
    }

    /// A burst is a closed star with straight edges whose rays reach past the body.
    #[test]
    fn a_burst_body_is_a_closed_star_with_reaching_rays() {
        let rect = [100.0, 120.0, 240.0, 160.0];
        let burst = LiveBalloon {
            id: 11,
            body: LiveBalloonBody::Burst { rect, rays: 14, length: 0.6, depth: 0.26, jitter: 0.35, seed: 909 },
            tails: Vec::new(),
            next_tail_id: 2,
            vertex_overrides: Vec::new(),
            hand_drawn_noise: 0.0,
            stroke_width: 2.5,
            auto_merge_overlapping: false,
        };
        let path = burst.rebuild_closed_path();
        assert_eq!(path.subpaths.len(), 1);
        assert!(path.subpaths[0].closed, "the burst is a closed contour");
        assert_eq!(path.subpaths[0].knots.len(), 28, "every ray has a tip and a valley");

        let (cx, cy) = (rect[0] + rect[2] * 0.5, rect[1] + rect[3] * 0.5);
        let reach = path.subpaths[0].knots.iter().map(|k| (k.anchor.x - cx).abs().max((k.anchor.y - cy).abs())).fold(0.0_f64, f64::max);
        assert!(reach > rect[3] * 0.5, "the rays must reach past the body: {reach}");

        let same = LiveBalloon { id: 11, ..burst.clone() }.rebuild_closed_path();
        assert!(
            path.subpaths[0].knots.iter().zip(&same.subpaths[0].knots).all(|(a, b)| (a.anchor.x - b.anchor.x).abs() < 1e-9),
            "the same seed must draw the same burst"
        );
        assert!(crate::flatten::validate_shape(&path, None).is_ok());
    }

    /// A double outline is two concentric contours sharing the layer's fill and stroke.
    #[test]
    fn a_double_outline_has_two_concentric_contours() {
        let rect = [100.0, 120.0, 240.0, 160.0];
        let balloon = LiveBalloon {
            id: 12,
            body: LiveBalloonBody::DoubleOutline { rect, squircle: 0.0, gap: 6.0 },
            tails: Vec::new(),
            next_tail_id: 2,
            vertex_overrides: Vec::new(),
            hand_drawn_noise: 0.0,
            stroke_width: 2.5,
            auto_merge_overlapping: false,
        };
        let path = balloon.rebuild_closed_path();
        assert_eq!(path.subpaths.len(), 2, "the echo is a second contour");
        assert!(path.subpaths.iter().all(|s| s.closed));

        let (cx, cy) = (rect[0] + rect[2] * 0.5, rect[1] + rect[3] * 0.5);
        let radius = |sub: &Subpath| sub.knots.iter().map(|k| ((k.anchor.x - cx).powi(2) + (k.anchor.y - cy).powi(2)).sqrt()).fold(0.0_f64, f64::max);
        let (a, b) = (radius(&path.subpaths[0]), radius(&path.subpaths[1]));
        assert!((a - b).abs() > 1.0, "the two contours must differ in size: {a} vs {b}");
        assert!(crate::flatten::validate_shape(&path, None).is_ok());
    }

    /// The oval exposes draggable bulges, and a dragged bulge moves the contour it sits on.
    #[test]
    fn an_oval_bulge_moves_the_contour_under_its_handle() {
        use photocraft_doc::vector::VertexOverride;

        let rect = [100.0, 120.0, 240.0, 160.0];
        let plain = LiveBalloon {
            id: 13,
            body: LiveBalloonBody::Ellipse { rect, squircle: 0.0 },
            tails: Vec::new(),
            next_tail_id: 2,
            vertex_overrides: Vec::new(),
            hand_drawn_noise: 0.0,
            stroke_width: 2.5,
            auto_merge_overlapping: false,
        };
        let plain_path = plain.rebuild_closed_path();
        assert_eq!(plain_path.subpaths[0].knots.len(), 4, "an untouched oval stays the exact superellipse");

        let bulged = LiveBalloon { vertex_overrides: vec![VertexOverride { index: 0, offset: [0.0, -20.0] }], ..plain.clone() };
        let bulged_path = bulged.rebuild_closed_path();
        assert!(bulged_path.subpaths[0].knots.len() > 4, "a dragged bulge turns the oval into a spline");
        assert!(
            bulged_path.subpaths[0].knots.iter().zip(&plain_path.subpaths[0].knots).any(|(a, b)| (a.anchor.y - b.anchor.y).abs() > 1.0),
            "the dragged bulge must move the contour"
        );

        let handles = bulged.vertex_handle_positions();
        assert_eq!(handles.len(), OVAL_BULGES as usize, "the oval exposes its bulge handles");
        for ((_, pos), knot) in handles.iter().zip(&bulged_path.subpaths[0].knots) {
            assert!((pos[0] - knot.anchor.x).abs() < 1e-6 && (pos[1] - knot.anchor.y).abs() < 1e-6, "a handle sits on its own knot");
        }
        assert!(crate::flatten::validate_shape(&bulged_path, None).is_ok());
    }

    #[test]
    fn the_noise_amplitude_follows_the_body_size() {
        // A four-knot contour must not be thrown tens of pixels off: the amplitude is a share of
        // the body's short side, not of the (very long) edge between two opposite knots.
        let square = square(0.0, 0.0, 100.0);
        for offset in hand_drawn_offsets(&square, 1.0, 3) {
            assert!(offset.0.hypot(offset.1) <= 5.01, "a four-knot contour folds: {offset:?}");
        }
    }

    /// Whether two segments cross strictly between their endpoints.
    fn segments_cross(a: P, b: P, c: P, d: P) -> bool {
        let orient = |p: P, q: P, r: P| (q.0 - p.0) * (r.1 - p.1) - (q.1 - p.1) * (r.0 - p.0);
        let (o1, o2) = (orient(a, b, c), orient(a, b, d));
        let (o3, o4) = (orient(c, d, a), orient(c, d, b));
        o1 * o2 < -1e-9 && o3 * o4 < -1e-9
    }

    #[test]
    fn the_noise_never_self_intersects_a_dense_contour() {
        let rect = [100.0, 120.0, 240.0, 160.0];
        let balloon = LiveBalloon {
            id: 21,
            body: LiveBalloonBody::Spiky { rect, spikes: 64, length: 0.35, sub_harmonic: 0.4, concavity: 0.65, jitter: 0.3, smooth: false, seed: 7 },
            tails: Vec::new(),
            next_tail_id: 2,
            vertex_overrides: Vec::new(),
            hand_drawn_noise: 1.0,
            stroke_width: 2.5,
            auto_merge_overlapping: false,
        };
        let path = balloon.rebuild_closed_path();
        let rings = crate::flatten::flatten_path(&path, 0.2);
        let pts = &rings.first().expect("one contour").pts;
        let n = pts.len();
        assert!(n > 60, "the dense contour is sampled: {n}");
        for i in 0..n {
            for j in (i + 1)..n {
                let adjacent = j == i + 1 || (i == 0 && j == n - 1);
                if adjacent {
                    continue;
                }
                assert!(!segments_cross(pts[i], pts[(i + 1) % n], pts[j], pts[(j + 1) % n]), "noisy segments {i} and {j} cross");
            }
        }
        // The wobble stays near the body: compare against the same body without noise, so the
        // test measures the wobble (5 % of the short side) and not the body's own overhang.
        let plain = LiveBalloon { hand_drawn_noise: 0.0, ..balloon };
        let plain_path = plain.rebuild_closed_path();
        let plain_rings = crate::flatten::flatten_path(&plain_path, 0.2);
        let plain_pts = &plain_rings.first().expect("one plain contour").pts;
        let (mut px0, mut py0, mut px1, mut py1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for p in plain_pts {
            px0 = px0.min(p.0);
            py0 = py0.min(p.1);
            px1 = px1.max(p.0);
            py1 = py1.max(p.1);
        }
        // 5 % of the 160 px short side is 8 px; the flattening of the noisy cubics adds a little.
        for p in pts {
            assert!(p.0 >= px0 - 10.0 && p.0 <= px1 + 10.0, "x {}", p.0);
            assert!(p.1 >= py0 - 10.0 && p.1 <= py1 + 10.0, "y {}", p.1);
        }
        let _ = rect;
    }

    #[test]
    fn a_noisy_body_keeps_its_tail_welded() {
        let rect = [100.0, 120.0, 240.0, 160.0];
        let bodies = [
            LiveBalloonBody::Ellipse { rect, squircle: 0.2 },
            LiveBalloonBody::Spiky { rect, spikes: 24, length: 0.4, sub_harmonic: 0.4, concavity: 0.65, jitter: 0.3, smooth: false, seed: 11 },
            LiveBalloonBody::Burst { rect, rays: 14, length: 0.6, depth: 0.26, jitter: 0.35, seed: 13 },
        ];
        for body in bodies {
            let balloon = LiveBalloon {
                id: 22,
                body,
                tails: vec![LiveBalloonTail {
                    id: 1,
                    preset: TailPreset::Wedge,
                    perimeter_t: 0.5,
                    mouth_width: 30.0,
                    tip: [220.0, 360.0],
                    bend: 0.0,
                    bubbles: 3,
                    bubble_radius: 10.0,
                }],
                next_tail_id: 2,
                vertex_overrides: Vec::new(),
                hand_drawn_noise: 0.8,
                stroke_width: 2.5,
                auto_merge_overlapping: false,
            };
            let path = balloon.rebuild_closed_path();
            let rings = crate::flatten::flatten_path(&path, 0.05);
            assert_eq!(rings.len(), 1, "the noisy body and its tail must union into one contour");
            assert!(crate::flatten::validate_shape(&path, None).is_ok());

            // The mouth really is read from the perturbed contour, not from the clean one.
            let (analytic, normal) = balloon.point_and_normal_on_body(0.5);
            let (mouth, mouth_normal) = nearest_mouth_on_noisy_body(&balloon, analytic, normal);
            assert!((mouth[0] - analytic[0]).hypot(mouth[1] - analytic[1]) > 0.02, "the noise must move the mouth: {mouth:?} vs {analytic:?}");
            assert!(mouth_normal[0] * normal[0] + mouth_normal[1] * normal[1] > 0.0, "the mouth normal stays outward");
        }
    }

    #[test]
    fn the_burst_tail_anchors_on_its_valley_core() {
        let rect = [100.0, 120.0, 240.0, 160.0];
        let balloon = LiveBalloon {
            id: 23,
            body: LiveBalloonBody::Burst { rect, rays: 14, length: 0.6, depth: 0.30, jitter: 0.35, seed: 3 },
            tails: Vec::new(),
            next_tail_id: 2,
            vertex_overrides: Vec::new(),
            hand_drawn_noise: 0.0,
            stroke_width: 2.5,
            auto_merge_overlapping: false,
        };
        let (p, n) = balloon.point_and_normal_on_body(0.0);
        let cy = rect[1] + rect[3] * 0.5;
        let ry = rect[3] * 0.5;
        assert!(cy - p[1] < ry * 0.95, "the mouth sits inside the ray tips: {}", cy - p[1]);
        assert!(cy - p[1] > ry * 0.5, "the mouth is not the fallback radius either: {}", cy - p[1]);
        assert!(n[0] * 0.0 + n[1] * -1.0 > 0.9, "the normal points outward: {n:?}");
    }

    #[test]
    fn the_noise_leaves_multi_contour_bodies_alone() {
        let rect = [100.0, 120.0, 240.0, 160.0];
        for body in [
            LiveBalloonBody::DoubleOutline { rect, squircle: 0.0, gap: 6.0 },
            LiveBalloonBody::Uniflash {
                rect,
                needles: 48,
                inner_ratio: 0.62,
                bundle_group: 0.4,
                length_jitter: 0.5,
                angle_jitter: 0.25,
                needle_width: 2.2,
                outer_sharpness: 1.0,
                inner_sharpness: 1.0,
                seed: 5,
            },
        ] {
            let plain = LiveBalloon {
                id: 24,
                body: body.clone(),
                tails: Vec::new(),
                next_tail_id: 2,
                vertex_overrides: Vec::new(),
                hand_drawn_noise: 0.0,
                stroke_width: 2.5,
                auto_merge_overlapping: false,
            };
            let noisy = LiveBalloon { hand_drawn_noise: 1.0, ..plain.clone() };
            assert_eq!(noisy.rebuild_closed_path(), plain.rebuild_closed_path(), "the wobble must not touch this body");
        }
    }

    #[test]
    fn a_double_outline_tail_keeps_the_ring() {
        let rect = [100.0, 120.0, 240.0, 160.0];
        let make = |tails: Vec<LiveBalloonTail>| LiveBalloon {
            id: 25,
            body: LiveBalloonBody::DoubleOutline { rect, squircle: 0.0, gap: 6.0 },
            tails,
            next_tail_id: 2,
            vertex_overrides: Vec::new(),
            hand_drawn_noise: 0.0,
            stroke_width: 2.5,
            auto_merge_overlapping: false,
        };
        let plain = make(Vec::new());
        let path = plain.rebuild_closed_path();
        assert_eq!(path.subpaths.len(), 2);
        assert_eq!(path.subpaths[0].op, PathOp::Combine, "the inner contour is the fill");
        assert_eq!(path.subpaths[1].op, PathOp::Intersect, "the echo only intersects: the gap is transparent");

        let (cx, cy) = (rect[0] + rect[2] * 0.5, rect[1] + rect[3] * 0.5);
        let raster = crate::fill_rasterizer(&path, 0.05);
        let centre = raster.render(photocraft_geom::Rect::new(cx as i32 - 1, cy as i32 - 1, cx as i32 + 1, cy as i32 + 1));
        assert!(centre.iter().all(|c| *c > 0.99), "the inner contour is filled: {centre:?}");
        let mid_gap = cx + 120.0;
        let ring = raster.render(photocraft_geom::Rect::new(mid_gap as i32, cy as i32, mid_gap as i32 + 1, cy as i32 + 1));
        assert!(ring.iter().all(|c| *c < 0.01), "the gap between the contours is not filled: {ring:?}");

        let with_tail = make(vec![LiveBalloonTail {
            id: 1,
            preset: TailPreset::Wedge,
            perimeter_t: 0.5,
            mouth_width: 30.0,
            tip: [220.0, 360.0],
            bend: 0.0,
            bubbles: 3,
            bubble_radius: 10.0,
        }]);
        let tailed = with_tail.rebuild_closed_path();
        assert!(tailed.subpaths.len() >= 2, "both contours survive the weld");
        assert_eq!(tailed.subpaths[0].op, PathOp::Combine);
        assert!(tailed.subpaths[1..].iter().any(|s| s.op == PathOp::Intersect), "the echo is still an intersect contour");
        let raster = crate::fill_rasterizer(&tailed, 0.05);
        let ring = raster.render(photocraft_geom::Rect::new(mid_gap as i32, cy as i32, mid_gap as i32 + 1, cy as i32 + 1));
        assert!(ring.iter().all(|c| *c < 0.01), "the welded ring keeps its transparent gap: {ring:?}");
    }

    #[test]
    fn an_override_outside_the_oval_knots_keeps_the_exact_superellipse() {
        use photocraft_doc::vector::VertexOverride;

        let rect = [100.0, 120.0, 240.0, 160.0];
        let balloon = LiveBalloon {
            id: 26,
            body: LiveBalloonBody::Ellipse { rect, squircle: 0.0 },
            tails: Vec::new(),
            next_tail_id: 2,
            vertex_overrides: vec![VertexOverride { index: OVAL_BULGES, offset: [8.0, 8.0] }],
            hand_drawn_noise: 0.0,
            stroke_width: 2.5,
            auto_merge_overlapping: false,
        };
        assert_eq!(balloon.rebuild_closed_path().subpaths[0].knots.len(), 4, "a stale index must not switch to a spline");
    }
}
