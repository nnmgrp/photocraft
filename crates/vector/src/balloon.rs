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

use std::collections::{BTreeMap, HashSet};
use std::f64::consts::TAU;

use photocraft_doc::vector::{
    BalloonLineStyle, BalloonUnderlay, BalloonVoice, BokehShape, LiveBalloon, LiveBalloonBody, LiveBalloonGroup, LiveBalloonTail, TailPreset, UniflashMode,
};
use photocraft_doc::vector_brush::VectorBrushPreset;
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

/// Two-tier hand-drawn contour noise (TZ 4.3):
/// 1) `fine_amount` (`0.0..=1.0`): natural fine paper/pen tremor
/// 2) `warp_amount` (`0.0..=1.0`): rare deep hand bends / local silhouette warps
/// 3) `noise_scale` (`0.25..=4.0`): wavelength multiplier along the perimeter
pub fn hand_drawn_offsets_two_tier(points: &[P], fine_amount: f64, warp_amount: f64, noise_scale: f64, seed: u32) -> Vec<P> {
    let n = points.len();
    if n == 0 {
        return Vec::new();
    }
    let fine = if fine_amount.is_finite() { fine_amount.clamp(0.0, 1.0) } else { 0.0 };
    let warp = if warp_amount.is_finite() { warp_amount.clamp(0.0, 1.0) } else { 0.0 };
    let scale = if noise_scale.is_finite() { noise_scale.clamp(0.25, 4.0) } else { 1.0 };

    if fine <= 0.0 && warp <= 0.0 {
        return vec![(0.0, 0.0); n];
    }
    if warp <= 1e-9 && (scale - 1.0).abs() <= 1e-9 {
        return hand_drawn_offsets(points, fine, seed);
    }

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
    let min_dim = (max_x - min_x).min(max_y - min_y).max(0.0);
    let mean_edge = (perimeter / n as f64).max(0.0);
    let max_safe_disp = (min_dim * 0.05).min(mean_edge * 0.20);
    let fine_amp = max_safe_disp * fine;
    let warp_amp = (min_dim * 0.045).min(mean_edge * 0.18) * warp;

    let seed_phase = hash01(seed ^ 0x9E37_79B9, 0) * TAU;
    let warp_center_a = hash01(seed ^ 0xA5A5_5A5A, 1);
    let warp_center_b = (warp_center_a + 0.42 + hash01(seed ^ 0x3C6E_F372, 2) * 0.16).rem_euclid(1.0);
    let warp_sign_a = if hash01(seed, 3) >= 0.5 { 1.0 } else { -1.0 };
    let warp_sign_b = -warp_sign_a;

    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / n as f64;
        let a = hash01(seed, i as u32) - 0.5;
        let b = hash01(seed ^ 0x5F35_6495, i as u32) - 0.5;
        let phase = TAU * t * scale;
        let fine_wobble = (phase * 3.0 + 0.7 + seed_phase).sin() * 0.45 + (phase * 7.0 + 2.1 + seed_phase * 1.7).sin() * 0.20 + a * 0.60 + b * 0.25;

        let circ_dist = |t0: f64, tc: f64| {
            let d = (t0 - tc).abs();
            d.min(1.0 - d)
        };
        let da = circ_dist(t, warp_center_a) * (1.8 * scale.sqrt());
        let db = circ_dist(t, warp_center_b) * (2.1 * scale.sqrt());
        let deep_lobe = warp_sign_a * (-da * da * 18.0).exp() + 0.65 * warp_sign_b * (-db * db * 22.0).exp();

        let total = (fine_wobble * fine_amp + deep_lobe * warp_amp).clamp(-max_safe_disp, max_safe_disp);
        let prev = points[(i + n - 1) % n];
        let next = points[(i + 1) % n];
        let along = norm(sub(next, prev));
        let normal = if along == (0.0, 0.0) { (0.0, 0.0) } else { (-along.1, along.0) };
        out.push(mul(normal, total));
    }
    out
}

/// Effective `(fine, warp, scale)` parameters including the balloon's voice modulation.
fn effective_noise_params(balloon: &LiveBalloon) -> (f64, f64, f64) {
    let mut fine = balloon.hand_drawn_noise.clamp(0.0, 1.0);
    let mut warp = balloon.hand_drawn_warp.clamp(0.0, 1.0);
    let scale = if balloon.noise_scale.is_finite() && balloon.noise_scale > 0.0 { balloon.noise_scale.clamp(0.25, 4.0) } else { 1.0 };
    let intensity = balloon.voice_intensity.clamp(0.0, 1.0);
    if intensity > 0.0 {
        match balloon.voice {
            BalloonVoice::Irritated => {
                fine = (fine + 0.35 * intensity).clamp(0.0, 1.0);
                warp = (warp + 0.20 * intensity).clamp(0.0, 1.0);
            }
            BalloonVoice::Eerie => {
                fine = (fine + 0.20 * intensity).clamp(0.0, 1.0);
                warp = (warp + 0.45 * intensity).clamp(0.0, 1.0);
            }
            BalloonVoice::Shout => {
                fine = (fine + 0.15 * intensity).clamp(0.0, 1.0);
            }
            BalloonVoice::Whisper | BalloonVoice::Neutral | BalloonVoice::Broadcast => {}
        }
    }
    (fine, warp, scale)
}

/// Translates every knot by two-tier hand-drawn offsets.
fn perturbed_path_two_tier(path: &Path, fine_amount: f64, warp_amount: f64, noise_scale: f64, seed: u32) -> Path {
    if !(fine_amount.is_finite() && fine_amount > 0.0 || warp_amount.is_finite() && warp_amount > 0.0) {
        return path.clone();
    }
    let mut out = path.clone();
    for (sub_index, subpath) in out.subpaths.iter_mut().enumerate() {
        if !subpath.closed || subpath.knots.len() < 3 {
            continue;
        }
        let anchors: Vec<P> = subpath.knots.iter().map(|k| (k.anchor.x, k.anchor.y)).collect();
        let sub_seed = seed ^ (sub_index as u32).wrapping_mul(0x2545_F491);
        let offsets = hand_drawn_offsets_two_tier(&anchors, fine_amount, warp_amount, noise_scale, sub_seed);
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

/// Removes self-crossing loops from one closed polygon: splits its edges at proper crossings and
/// keeps the largest simple loop. A burst valley and an arc tail can meet almost tangentially;
/// without this the boolean union occasionally leaves a small crossed "bow tie" at the seam.
fn strip_self_intersections(poly: &[P]) -> Vec<P> {
    let poly = clean(poly);
    let n = poly.len();
    if n < 4 {
        return poly;
    }
    let mut pieces: Vec<[P; 2]> = Vec::with_capacity(n);
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        let mut cuts: Vec<f64> = vec![0.0, 1.0];
        for j in 0..n {
            // Adjacent edges share an endpoint; that is not a self-intersection.
            if j == i || (j + 1) % n == i || (i + 1) % n == j {
                continue;
            }
            if let Some(t) = crossing_param(a, b, poly[j], poly[(j + 1) % n]) {
                cuts.push(t);
            }
        }
        cuts.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
        for w in cuts.windows(2) {
            if w[1] - w[0] > 1e-9 {
                pieces.push([lerp(a, b, w[0]), lerp(a, b, w[1])]);
            }
        }
    }
    if pieces.len() <= n {
        // No extra cuts: the input is already simple.
        return poly;
    }
    stitch(&pieces).into_iter().max_by(|a, b| signed_area(a).abs().total_cmp(&signed_area(b).abs())).unwrap_or(poly)
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
    let loops: Vec<Vec<P>> = loops.iter().map(|l| strip_self_intersections(l)).collect();
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

/// Base-2 van der Corput radical inverse: `0 -> 0.0`, `1 -> 0.5`, `2 -> 0.25`, `3 -> 0.75`, …
///
/// Used to distribute particles inside a fixed contour sector so that growing the particle count
/// only *adds* particles: the first `N` positions never move when the count becomes larger.
#[inline]
fn van_der_corput_2(mut n: u32) -> f64 {
    let mut result = 0.0;
    let mut denom = 0.5;
    while n != 0 {
        if n & 1 != 0 {
            result += denom;
        }
        denom *= 0.5;
        n >>= 1;
    }
    result
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
    /// The manual offset and radius multiplier of one stipple dot (zero / 1.0 when not moved).
    fn get_stipple_override(&self, index: u32) -> ([f64; 2], f64);
    /// Canonical canvas-space handle positions `(index, [x, y], radius_px)` for draggable stipple dots.
    fn stipple_dot_positions(&self) -> Vec<(u32, [f64; 2], f64)>;
    /// The deterministic seed of the hand-drawn contour wobble.
    fn noise_seed(&self) -> u32;
    /// The balloon's closed outline, its tails merged in.
    fn rebuild_closed_path(&self) -> Path;
}

/// Render paths of a balloon group split for single-pass vs two-pass `ShapeLayer` compositing.
#[derive(Clone, Debug, Default)]
pub struct BalloonGroupRenderPaths {
    /// Contours rendered via standard `ShapeLayer` single pass (`fill` + `stroke`):
    /// `Solid`, `Double`, `DoubleOutline`, `Pixel1Bit`, `Uniflash` w/ `Solid`, plus `underlay`.
    pub standard_path: Path,
    /// Welded body + tail base contours for stylized two-pass styles (`Whisper`, `Pencil`,
    /// `UkiyoBrush`, non-interior `StippleDots`, `SparkleGlitter`) rendered with `shape.fill` (no stroke).
    pub fill_only_path: Path,
    /// Ink capsules/strands/ribbons/dots/stars rendered with `shape.stroke.paint` (no stroke).
    pub ink_from_stroke_path: Path,
    /// A stylized `Uniflash` group. Documents saved before the fill/stroke fix can carry a black
    /// fill and no stroke here: the renderer substitutes a white interior and black ink for that
    /// combination so the needles never become one solid black blob.
    pub uniflash_stylized: bool,
}

/// One sampled contour station along a balloon's welded outline (for raster dabbing / UV textures).
#[derive(Clone, Copy, Debug)]
pub struct BalloonContourStation {
    pub pos: [f64; 2],
    pub normal: [f64; 2],
    pub tangent: [f64; 2],
    pub arc_len: f64,
    pub total_len: f64,
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

        // A manually dragged ray tip or inner valley moves only its own knot. The valley
        // handles are indexed after the `n` tips (`n..2*n`), mirroring `vertex_handle_positions`.
        let tip_off = offset(i);
        let valley_off = offset(n + i);
        points.push((tip.0 + tip_off[0], tip.1 + tip_off[1]));
        points.push((valley.0 + valley_off[0], valley.1 + valley_off[1]));
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

/// Standalone constructor for `Whisper` dashed ellipse balloons.
pub fn whisper(rect: [f64; 4], dash_count: u32, dash_duty: f64, squircle: f64, seed: u32) -> Path {
    let mut b = LiveBalloon {
        id: seed.max(1),
        body: LiveBalloonBody::Ellipse { rect, squircle },
        line_style: BalloonLineStyle::Whisper,
        whisper_dash_count: dash_count.clamp(6, 96),
        whisper_duty: dash_duty.clamp(0.20, 0.85),
        tails: Vec::new(),
        ..LiveBalloon::default()
    };
    b.hand_drawn_noise = 0.0;
    b.rebuild_closed_path()
}

struct ConcaveOrFlat {
    concave: f64,
}

/// The base geometry of one chamfer corner: its two bevel endpoints and the quadratic control
/// point of the scooped bevel. Endpoints are exposed as `VertexHandle`s `c*2` / `c*2+1`; each
/// bevel midpoint is the handle `8+c`.
struct ChamferCorner {
    a: P,
    b: P,
    ctrl: P,
}

struct ChamferBodyGeometry {
    scoop: f64,
    corners: [ChamferCorner; 4],
}

fn chamfer_body_geometry(rect: [f64; 4], chamfer: f64, ConcaveOrFlat { concave }: ConcaveOrFlat) -> ChamferBodyGeometry {
    let [x, y, w, h] = rect;
    let (x0, y0) = (x.min(x + w), y.min(y + h));
    let (bw, bh) = (w.abs().max(4.0), h.abs().max(4.0));
    let (x1, y1) = (x0 + bw, y0 + bh);
    let max_c = (bw.min(bh) * 0.42).max(1.0);
    let c = chamfer.clamp(1.0, max_c);
    let scoop = concave.clamp(0.0, 1.0) * c * 0.55;
    ChamferBodyGeometry {
        scoop,
        corners: [
            ChamferCorner { a: (x0, y0 + c), b: (x0 + c, y0), ctrl: (x0 + c - scoop, y0 + c - scoop) },
            ChamferCorner { a: (x1 - c, y0), b: (x1, y0 + c), ctrl: (x1 - c + scoop, y0 + c - scoop) },
            ChamferCorner { a: (x1, y1 - c), b: (x1 - c, y1), ctrl: (x1 - c + scoop, y1 - c + scoop) },
            ChamferCorner { a: (x0 + c, y1), b: (x0, y1 - c), ctrl: (x0 + c - scoop, y1 - c + scoop) },
        ],
    }
}

/// ChamferRect body: narration / system plaque with 45-degree beveled or inward-scooped corners.
///
/// `offset` maps `0..8` to the dragged bevel endpoints and `8..12` to the dragged bevel bends, so
/// the same geometry drives both the contour and the gizmo handles.
fn build_chamfer_rect_body(rect: [f64; 4], chamfer: f64, concavity: ConcaveOrFlat, offset: impl Fn(u32) -> [f64; 2]) -> Path {
    let geo = chamfer_body_geometry(rect, chamfer, concavity);
    let mut knots = Vec::with_capacity(8);
    for (c, corner) in geo.corners.into_iter().enumerate() {
        let c = c as u32;
        let oa = offset(c * 2);
        let ob = offset(c * 2 + 1);
        let a = (corner.a.0 + oa[0], corner.a.1 + oa[1]);
        let b = (corner.b.0 + ob[0], corner.b.1 + ob[1]);
        let obend = offset(8 + c);

        // The bend handle sits on the bevel's quadratic midpoint in both the flat and the
        // scooped case. Dragging it moves that midpoint; dragging an endpoint moves the base.
        let base_mid = if geo.scoop > 0.05 {
            (0.25 * a.0 + 0.5 * corner.ctrl.0 + 0.25 * b.0, 0.25 * a.1 + 0.5 * corner.ctrl.1 + 0.25 * b.1)
        } else {
            (0.5 * (a.0 + b.0), 0.5 * (a.1 + b.1))
        };
        let target_mid = (base_mid.0 + obend[0], base_mid.1 + obend[1]);

        if geo.scoop <= 0.05 && obend[0].hypot(obend[1]) <= 0.05 {
            knots.push(Knot::corner(a.0, a.1));
            knots.push(Knot::corner(b.0, b.1));
        } else {
            // Quadratic `a -> b` through `target_mid`, as a cubic so the curve's t=0.5 point is
            // exactly the bend handle (the handle never floats away from the contour).
            let ctrl = (2.0 * target_mid.0 - 0.5 * (a.0 + b.0), 2.0 * target_mid.1 - 0.5 * (a.1 + b.1));
            let c1 = ((a.0 + 2.0 * ctrl.0) / 3.0, (a.1 + 2.0 * ctrl.1) / 3.0);
            let c2 = ((b.0 + 2.0 * ctrl.0) / 3.0, (b.1 + 2.0 * ctrl.1) / 3.0);
            knots.push(Knot { anchor: to_point(a), in_ctrl: to_point(a), out_ctrl: to_point(c1), smooth: false });
            knots.push(Knot { anchor: to_point(b), in_ctrl: to_point(c2), out_ctrl: to_point(b), smooth: false });
        }
    }
    Path::new(vec![Subpath { knots, closed: true, op: PathOp::Combine }])
}

/// Standalone constructor for `ChamferRect` balloons.
pub fn chamfer_rect(rect: [f64; 4], chamfer: f64, concave: f64) -> Path {
    build_chamfer_rect_body(rect, chamfer, ConcaveOrFlat { concave }, |_| [0.0, 0.0])
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
            | LiveBalloonBody::ChamferRect { .. }
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
pub const OVAL_BULGES: u32 = 8;

/// How many of a Uniflash body's needles get a drag handle (the rest follow the shape sliders).
pub const UNIFLASH_HANDLES: u32 = 14;

/// Maximum number of individual stipple dot handles exposed on canvas for `BalloonLineStyle::StippleDots`.
pub const MAX_EDITABLE_STIPPLE_DOTS: u32 = 96;

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
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let a = &knots[i];
        let b = &knots[(i + 1) % n];
        let p0 = (a.anchor.x, a.anchor.y);
        let c1 = (a.out_ctrl.x, a.out_ctrl.y);
        let c2 = (b.in_ctrl.x, b.in_ctrl.y);
        let p3 = (b.anchor.x, b.anchor.y);
        // The four cardinal anchors (N, E, S, W) interleaved with the four quadrant midpoints
        // (NE, SE, SW, NW), clockwise from the top: eight evenly spread handles, one per bulge.
        out.push(p0);
        out.push(cubic_at(p0, c1, c2, p3, 0.5));
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
    /// Half width at the middle of the ray: the needle's body is as wide as `needle_width`, even
    /// when both ends are sharp. Without it a 4-point wedge with two sharp ends has zero area.
    half_mid: f64,
    half_inner: f64,
    perp: [f64; 2],
}

/// Shortest signed angular difference `a - b` folded into `(-PI, PI]`.
fn angle_delta(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(TAU);
    if d > std::f64::consts::PI { d - TAU } else { d }
}

fn uniflash_needle_count(balloon: &LiveBalloon) -> u32 {
    match &balloon.body {
        LiveBalloonBody::Uniflash { needles, .. } => (*needles).clamp(24, 450),
        _ => 160,
    }
}

/// The angular cone (base direction plus relative min/max offsets) covered by one tail's mouth
/// and tip, measured from the Uniflash centre. Using relative offsets makes the cone robust to
/// the `[-PI, PI]` wrap-around at the left edge.
fn tail_wedge_range(balloon: &LiveBalloon, t: &LiveBalloonTail) -> Option<(f64, f64, f64)> {
    if t.preset == TailPreset::None {
        return None;
    }
    let r = balloon.body.rect();
    let cx = r[0] + r[2] * 0.5;
    let cy = r[1] + r[3] * 0.5;
    let (mouth, normal) = balloon.point_and_normal_on_body(t.perimeter_t);
    let base = (mouth[1] - cy).atan2(mouth[0] - cx);
    let tangent = [-normal[1], normal[0]];
    let min_half = (r[2].abs().min(r[3].abs()) * 0.5).max(4.0);
    let half_w = t.mouth_width.clamp(4.0, min_half * 1.6) * 0.5;
    let left = [mouth[0] - tangent[0] * half_w, mouth[1] - tangent[1] * half_w];
    let right = [mouth[0] + tangent[0] * half_w, mouth[1] + tangent[1] * half_w];
    let mut lo = 0.0f64;
    let mut hi = 0.0f64;
    for p in [left, right, t.tip] {
        let d = angle_delta((p[1] - cy).atan2(p[0] - cx), base);
        lo = lo.min(d);
        hi = hi.max(d);
    }
    Some((base, lo, hi))
}

/// Whether a radial needle at `theta` sits inside an active tail's angular cone. Needles in the
/// cone are dropped so they part around the tail opening instead of stabbing through its walls.
fn needle_angle_in_tail(balloon: &LiveBalloon, theta: f64) -> bool {
    let margin = (TAU / (uniflash_needle_count(balloon) as f64)).clamp(0.06, 0.6);
    balloon.tails.iter().filter(|t| t.preset != TailPreset::None).any(|t| {
        tail_wedge_range(balloon, t).is_some_and(|(base, lo, hi)| {
            let d = angle_delta(theta, base);
            d >= lo - margin && d <= hi + margin
        })
    })
}

/// Test hook: how many of a Uniflash body's radial needles still cross an explicit tail's cone.
/// A correctly carved flash reports zero.
#[cfg(test)]
fn needle_intersects_tail(balloon: &LiveBalloon, tail: &LiveBalloonTail) -> usize {
    let Some((base, lo, hi)) = tail_wedge_range(balloon, tail) else {
        return 0;
    };
    let r = balloon.body.rect();
    let cx = r[0] + r[2] * 0.5;
    let cy = r[1] + r[3] * 0.5;
    let margin = (TAU / (uniflash_needle_count(balloon) as f64)).clamp(0.06, 0.6);
    uniflash_needles(balloon).map_or(0, |needles| {
        needles
            .iter()
            .filter(|n| {
                let theta = (n.outer.y - cy).atan2(n.outer.x - cx);
                let d = angle_delta(theta, base);
                d >= lo - margin && d <= hi + margin
            })
            .count()
    })
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
        mode,
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
    let half_mid = half_w;
    let half_outer = match mode {
        UniflashMode::InwardNeedles => half_w * (1.0 - outer_sharpness.clamp(0.0, 1.0)).max(0.05),
        UniflashMode::SolidRingSpikes => half_w * (1.0 - outer_sharpness.clamp(0.0, 1.0)).clamp(0.05, 0.12),
    };
    let half_inner = match mode {
        UniflashMode::InwardNeedles => half_w * (1.0 - inner_sharpness.clamp(0.0, 1.0)).max(0.05),
        UniflashMode::SolidRingSpikes => (half_w * 2.15).max(rx.min(ry) * step * 0.85),
    };

    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let rand_a = hash_f64(*seed, i * 3) - 0.5;
        let rand_in = hash_f64(*seed, i * 3 + 1);
        let rand_out = hash_f64(*seed, i * 3 + 2);

        let bundle_phase = ((i as f64) * 0.35 * std::f64::consts::TAU).sin();
        let bundle_shift = bundle_phase * step * 0.35 * bundle_group.clamp(0.0, 1.0);

        let theta = (i as f64) * step + bundle_shift + rand_a * step * angle_jitter.clamp(0.0, 1.0);
        // Carve the tail opening out of the ray fan: needles whose angle falls inside an explicit
        // tail's cone are omitted so they part around the tail instead of crossing its walls.
        if needle_angle_in_tail(balloon, theta) {
            continue;
        }
        let cos = theta.cos();
        let sin = theta.sin();

        let harmonic_in = if i % 2 == 0 { 1.0 } else { 0.85 };
        let (r_in, r_out) = match mode {
            UniflashMode::InwardNeedles => (inner_ratio * (1.0 - 0.32 * length_jitter * rand_in * harmonic_in), 1.0 - 0.14 * length_jitter * rand_out),
            UniflashMode::SolidRingSpikes => {
                let collar_outer = (inner_ratio.clamp(0.30, 0.85) + 0.06).min(0.90);
                let spike_reach = collar_outer + (1.12 - collar_outer) * (0.45 + 0.55 * (1.0 - 0.65 * length_jitter * rand_out * harmonic_in));
                (collar_outer * 0.96, spike_reach)
            }
        };

        let user_off = balloon.get_vertex_offset(i);
        let (outer, inner) = match mode {
            UniflashMode::InwardNeedles => {
                (Point::new(cx + rx * r_out * cos, cy + ry * r_out * sin), Point::new(cx + rx * r_in * cos + user_off[0], cy + ry * r_in * sin + user_off[1]))
            }
            UniflashMode::SolidRingSpikes => {
                (Point::new(cx + rx * r_out * cos + user_off[0], cy + ry * r_out * sin + user_off[1]), Point::new(cx + rx * r_in * cos, cy + ry * r_in * sin))
            }
        };

        out.push(UniflashNeedle { outer, inner, half_outer, half_mid, half_inner, perp: [-sin, cos] });
    }
    Some(out)
}

/// A sea-urchin flash: every needle is a closed wedge polygon (4 knots) filled with the stroke
/// colour, plus optional solid black collar annulus in [`UniflashMode::SolidRingSpikes`].
fn build_uniflash_paths(balloon: &LiveBalloon) -> Option<Path> {
    let LiveBalloonBody::Uniflash { rect, inner_ratio, mode, .. } = &balloon.body else {
        return None;
    };
    let needles = uniflash_needles(balloon)?;
    let mut subpaths: Vec<Subpath> = Vec::with_capacity(needles.len() + 1);

    if *mode == UniflashMode::SolidRingSpikes {
        let cx = rect[0] + rect[2] * 0.5;
        let cy = rect[1] + rect[3] * 0.5;
        let rx = (rect[2] * 0.5).abs().max(4.0);
        let ry = (rect[3] * 0.5).abs().max(4.0);
        let r_inner = inner_ratio.clamp(0.30, 0.85);
        let r_collar = (r_inner + 0.08).min(0.92);
        let segs = 48usize;
        let mut ring_pts = Vec::with_capacity(segs * 2);
        for k in 0..segs {
            let a = TAU * (k as f64) / (segs as f64);
            ring_pts.push((cx + rx * r_collar * a.cos(), cy + ry * r_collar * a.sin()));
        }
        for k in (0..segs).rev() {
            let a = TAU * (k as f64) / (segs as f64);
            ring_pts.push((cx + rx * r_inner * a.cos(), cy + ry * r_inner * a.sin()));
        }
        subpaths.push(Subpath::polygon(&ring_pts));
    }

    for n in &needles {
        // A six-point spindle: sharp ends, full `needle_width` at the middle. The 4-point wedge
        // collapsed to zero area when both sharpness values were 1.0, leaving only the blue vector
        // skeleton visible instead of black ink.
        let mid = Point::new((n.outer.x + n.inner.x) * 0.5, (n.outer.y + n.inner.y) * 0.5);
        let at = |p: Point, h: f64, sign: f64| (p.x + n.perp[0] * h * sign, p.y + n.perp[1] * h * sign);
        subpaths.push(Subpath::polygon(&[
            at(n.outer, n.half_outer, 1.0),
            at(mid, n.half_mid, 1.0),
            at(n.inner, n.half_inner, 1.0),
            at(n.inner, n.half_inner, -1.0),
            at(mid, n.half_mid, -1.0),
            at(n.outer, n.half_outer, -1.0),
        ]));
    }
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

/// How far a tail's base is buried inside the body, proportional to the balloon's smaller half
/// side. The old fixed `min(32.0)` cap made 600-DPI tails detach from large bodies.
fn tail_base_inset(balloon: &LiveBalloon) -> f64 {
    let r = balloon.body.rect();
    let min_half = (r[2].abs().min(r[3].abs()) * 0.5).max(4.0);
    (min_half * 0.28).clamp(0.0, min_half * 0.45)
}

/// For axis-aligned bodies, the usable half-length of the straight wall the mouth sits on. The
/// mouth half-width is clamped to this so a tail near a corner cannot wrap around it and grow a
/// backward "ear" outside the wall. Curved bodies return `None` (no straight-wall constraint).
fn tail_wall_half_extent(balloon: &LiveBalloon, mouth: [f64; 2], normal: [f64; 2]) -> Option<f64> {
    match &balloon.body {
        LiveBalloonBody::Rect { .. } | LiveBalloonBody::ChamferRect { .. } => {
            let r = balloon.body.rect();
            let cx = r[0] + r[2] * 0.5;
            let cy = r[1] + r[3] * 0.5;
            let rx = (r[2] * 0.5).abs().max(1.0);
            let ry = (r[3] * 0.5).abs().max(1.0);
            let chamfer = match &balloon.body {
                LiveBalloonBody::ChamferRect { chamfer, .. } => chamfer.abs().max(0.0),
                _ => 0.0,
            };
            let extent = if normal[0].abs() >= normal[1].abs() {
                ry - (mouth[1] - cy).abs() - chamfer
            } else {
                rx - (mouth[0] - cx).abs() - chamfer
            };
            Some(extent.max(0.0))
        }
        _ => None,
    }
}

/// Clamps a bezier control handle so it cannot sit behind its base anchor relative to the tip
/// (which would fold the tail wall backward), and bounds its lateral excursion.
fn clamp_tail_handle(anchor: Point, tip: Point, handle: Point, max_lateral: f64) -> Point {
    let dx = tip.x - anchor.x;
    let dy = tip.y - anchor.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-6 {
        return anchor;
    }
    let dir = (dx / len, dy / len);
    let perp = (-dir.1, dir.0);
    let rel = (handle.x - anchor.x, handle.y - anchor.y);
    let along = (rel.0 * dir.0 + rel.1 * dir.1).clamp(0.0, len);
    let lateral = (rel.0 * perp.0 + rel.1 * perp.1).clamp(-max_lateral.abs(), max_lateral.abs());
    Point::new(anchor.x + dir.0 * along + perp.0 * lateral, anchor.y + dir.1 * along + perp.1 * lateral)
}

/// The two Arc bezier handles, clamped so they never cross each other (the S-pinch seen when a
/// tip is short or sideways) and never fold behind their base anchors.
fn arc_tail_handles(base_l: Point, base_r: Point, tip: Point, ctrl: Point, half_w: f64) -> (Point, Point) {
    // Both handles start from the bend control. Clamping each against its own base→tip axis keeps
    // them in the outward half-space (never behind the base) for short or sideways tips.
    let lateral = half_w * 0.5;
    let mut c_l = clamp_tail_handle(base_l, tip, ctrl, lateral);
    let mut c_r = clamp_tail_handle(base_r, tip, ctrl, lateral);

    // Enforce left-to-right ordering along the base chord with a minimum separation, so the two
    // controls cannot collapse or swap sides — the crossover that produced the S-pinch.
    let ax = base_r.x - base_l.x;
    let ay = base_r.y - base_l.y;
    let al = (ax * ax + ay * ay).sqrt().max(1e-6);
    let axis = (ax / al, ay / al);
    let sep = half_w * 0.5;
    let proj = |p: Point| (p.x - base_l.x) * axis.0 + (p.y - base_l.y) * axis.1;
    let (pl, pr) = (proj(c_l), proj(c_r));
    if pr - pl < sep {
        let mid = (pl + pr) * 0.5;
        let shift_l = (mid - sep * 0.5) - pl;
        let shift_r = (mid + sep * 0.5) - pr;
        c_l = clamp_tail_handle(base_l, tip, Point::new(c_l.x + axis.0 * shift_l, c_l.y + axis.1 * shift_l), lateral);
        c_r = clamp_tail_handle(base_r, tip, Point::new(c_r.x + axis.0 * shift_r, c_r.y + axis.1 * shift_r), lateral);
    }
    (c_l, c_r)
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
    let (eff_fine, eff_warp, _) = effective_noise_params(balloon);
    let (mouth_center, normal) = if (eff_fine > 0.0 || eff_warp > 0.0) && noise_applies(&balloon.body) {
        nearest_mouth_on_noisy_body(balloon, analytic_center, analytic_normal)
    } else {
        (analytic_center, analytic_normal)
    };
    let tangent = [-normal[1], normal[0]];
    let r = balloon.body.rect();
    let min_half = (r[2].abs().min(r[3].abs()) * 0.5).max(4.0);
    let mut half_w = t.mouth_width.clamp(4.0, min_half * 1.6) * 0.5;
    // On Rect / ChamferRect the mouth must stay on its straight wall. Near a corner the raw
    // mouth width would push an anchor around the corner and grow a backward "ear" outside the
    // wall; clamping to the wall's usable half-length keeps the base on the wall.
    if let Some(wall_half) = tail_wall_half_extent(balloon, mouth_center, normal) {
        half_w = half_w.min((wall_half * 0.92).max(1.0));
    }

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

    let inset = tail_base_inset(balloon);
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
            // Clamp the handles into the outward half-space and keep them ordered; without this a
            // short or sideways tip makes them cross and the closed bezier folds into an S-pinch.
            let (c_l, c_r) = arc_tail_handles(p_left, p_right, p_tip, Point::new(ctrl[0], ctrl[1]), half_w);
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

/// A rectangle whose four corners were dragged individually, with each corner rounded by
/// `corner_radius` (clamped to half the shorter adjacent edge, like CSS `border-radius`).
///
/// The four corner handles are the indices `0..4` of `vertex_handle_positions`; when none of them
/// has an offset the untouched `rounded_rect` path is kept instead (see `body_outline_with_noise`).
fn build_dragged_rect(balloon: &LiveBalloon, rect: [f64; 4], corner_radius: f64) -> Path {
    let (x0, y0) = (rect[0].min(rect[0] + rect[2]), rect[1].min(rect[1] + rect[3]));
    let (x1, y1) = (rect[0].max(rect[0] + rect[2]), rect[1].max(rect[1] + rect[3]));
    let base = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
    let corners: Vec<P> = base
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let off = balloon.get_vertex_offset(i as u32);
            (p[0] + off[0], p[1] + off[1])
        })
        .collect();

    let mut knots = Vec::with_capacity(8);
    for i in 0..4 {
        let cur = corners[i];
        let prev = corners[(i + 3) % 4];
        let next = corners[(i + 1) % 4];
        let in_len = dist(prev, cur);
        let out_len = dist(next, cur);
        let r = corner_radius.max(0.0).min(0.48 * in_len.min(out_len));
        if r <= 0.1 {
            knots.push(Knot::corner(cur.0, cur.1));
            continue;
        }
        // Unit vectors from the corner towards its two neighbours along the incoming/outgoing
        // edges. The arc starts/ends r px along those edges, its handles sit KAPPA*r closer in.
        let toward_prev = ((prev.0 - cur.0) / in_len, (prev.1 - cur.1) / in_len);
        let toward_next = ((next.0 - cur.0) / out_len, (next.1 - cur.1) / out_len);
        let arc_start = (cur.0 + toward_prev.0 * r, cur.1 + toward_prev.1 * r);
        let arc_end = (cur.0 + toward_next.0 * r, cur.1 + toward_next.1 * r);
        let c_start = (arc_start.0 - toward_prev.0 * crate::shapes::KAPPA * r, arc_start.1 - toward_prev.1 * crate::shapes::KAPPA * r);
        let c_end = (arc_end.0 - toward_next.0 * crate::shapes::KAPPA * r, arc_end.1 - toward_next.1 * crate::shapes::KAPPA * r);
        knots.push(Knot { anchor: to_point(arc_start), in_ctrl: to_point(arc_start), out_ctrl: to_point(c_start), smooth: false });
        knots.push(Knot { anchor: to_point(arc_end), in_ctrl: to_point(c_end), out_ctrl: to_point(arc_end), smooth: false });
    }
    Path::new(vec![Subpath { knots, closed: true, op: PathOp::Combine }])
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
            if balloon.vertex_overrides.iter().any(|v| v.index < 4) {
                build_dragged_rect(balloon, *rect, *corner_radius)
            } else {
                let r = *corner_radius;
                crate::shapes::rounded_rect(rect[0], rect[1], rect[2], rect[3], [r, r, r, r])
            }
        }
        LiveBalloonBody::ChamferRect { rect, chamfer, concave } => {
            build_chamfer_rect_body(*rect, *chamfer, ConcaveOrFlat { concave: *concave }, |i| balloon.get_vertex_offset(i))
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
        LiveBalloonBody::Uniflash { .. } => build_uniflash_paths(balloon).unwrap_or_default(),
    };

    if noise_applies(&balloon.body) {
        let (fine, warp, scale) = effective_noise_params(balloon);
        if fine > 0.0 || warp > 0.0 {
            return perturbed_path_two_tier(&path, fine, warp, scale, balloon.noise_seed());
        }
    }
    path
}

/// Outer envelope of a `Uniflash` balloon when combined with stylized line styles.
///
/// The old contour alternated outer tips with inner needle ends, producing a deeply notched star.
/// Offset-based styles (`UkiyoBrush`, `Pencil`, mitred dashes) follow those notches and shoot long
/// lines well past the body. Sampling only the outer rim, pulled slightly inward and lightly
/// smoothed, keeps the ink on the balloon and leaves the interior free for the white fill.
fn build_uniflash_contour_for_line_style(balloon: &LiveBalloon) -> Path {
    let Some(needles) = uniflash_needles(balloon) else {
        return Path::default();
    };
    if needles.len() < 3 {
        return Path::default();
    }
    let rect = balloon.body.rect();
    let cx = rect[0] + rect[2] * 0.5;
    let cy = rect[1] + rect[3] * 0.5;
    let stride = (needles.len() / 72).max(1);
    let rx = (rect[2] * 0.5).abs().max(4.0);
    let ry = (rect[3] * 0.5).abs().max(4.0);
    let mut ring: Vec<P> = Vec::with_capacity(needles.len().div_ceil(stride));
    for n in needles.iter().step_by(stride) {
        // SolidRingSpikes can reach past the body; keep the sampling envelope inside it so
        // offset-based styles cannot shoot long ink lines outside the balloon.
        let nx = (n.outer.x - cx) / rx;
        let ny = (n.outer.y - cy) / ry;
        let r = nx.hypot(ny);
        let k = if r > 0.94 { 0.94 / r } else { 1.0 };
        let inward = (cx + (n.outer.x - cx) * k * 0.94, cy + (n.outer.y - cy) * k * 0.94);
        if ring.last().is_none_or(|q| dist(*q, inward) > 1e-4) {
            ring.push(inward);
        }
    }
    if ring.len() < 3 {
        return Path::default();
    }
    // One light averaging pass rounds needle tips without collapsing the envelope.
    let n = ring.len();
    let smoothed: Vec<P> = (0..n)
        .map(|i| {
            let a = ring[(i + n - 1) % n];
            let b = ring[i];
            let c = ring[(i + 1) % n];
            (b.0 * 0.5 + (a.0 + c.0) * 0.25, b.1 * 0.5 + (a.1 + c.1) * 0.25)
        })
        .collect();
    let pts = oriented(&smoothed);
    Path::new(vec![Subpath::polygon(&pts)])
}

/// Builds the welded body + tail base path and detached tail subpaths for contour sampling.
fn build_welded_contour_for_sampling(balloon: &LiveBalloon) -> (Path, Vec<Subpath>) {
    let is_stylized_uniflash = matches!(balloon.body, LiveBalloonBody::Uniflash { .. }) && balloon.line_style != BalloonLineStyle::Solid;
    let mut base_path = if is_stylized_uniflash { build_uniflash_contour_for_line_style(balloon) } else { body_outline_with_noise(balloon) };

    let mut detached_subpaths = Vec::new();
    for tail in &balloon.tails {
        if is_stylized_uniflash && tail.preset != TailPreset::None {
            let mut proxy = balloon.clone();
            proxy.body = LiveBalloonBody::Ellipse { rect: balloon.body.rect(), squircle: 0.15 };
            let (welded, detached) = build_single_tail_path(&proxy, tail);
            if let Some(w_path) = welded {
                base_path = merge_paths_union(&base_path, &w_path);
            }
            if let Some(d_path) = detached {
                detached_subpaths.extend(d_path.subpaths);
            }
        } else {
            let (welded, detached) = build_single_tail_path(balloon, tail);
            if let Some(w_path) = welded {
                base_path = merge_paths_union(&base_path, &w_path);
            }
            if let Some(d_path) = detached {
                detached_subpaths.extend(d_path.subpaths);
            }
        }
    }
    (base_path, detached_subpaths)
}

#[derive(Clone, Copy, Debug)]
struct ContourArcSeg {
    s0: f64,
    len: f64,
    a: P,
    b: P,
    normal: [f64; 2],
    tangent: [f64; 2],
}

struct ContourArcTable {
    segs: Vec<ContourArcSeg>,
    total_len: f64,
    /// +1 / -1: the winding used for the stored segment normals.
    orientation: f64,
}

impl ContourArcTable {
    fn build(segs: Vec<ContourArcSeg>, total_len: f64) -> Self {
        let orientation = segs.first().map(|s| -(s.tangent[0] * s.normal[1] - s.tangent[1] * s.normal[0])).unwrap_or(1.0);
        Self { segs, total_len, orientation }
    }

    fn from_subpath(sp: &Subpath) -> Self {
        let mut segs = Vec::new();
        let mut total_len = 0.0;
        Self::push_subpath(&mut segs, &mut total_len, sp);
        Self::build(segs, total_len)
    }

    fn from_paths(welded_base: &Path, detached: &[Subpath]) -> Self {
        let mut segs = Vec::new();
        let mut total_len = 0.0;
        for sp in welded_base.subpaths.iter().chain(detached.iter()) {
            Self::push_subpath(&mut segs, &mut total_len, sp);
        }
        Self::build(segs, total_len)
    }

    fn push_subpath(segs: &mut Vec<ContourArcSeg>, total_len: &mut f64, sp: &Subpath) {
        if sp.knots.len() < 2 {
            return;
        }
        let flat = flatten_subpath(sp, 0.35);
        let poly = clean(&flat.pts);
        let n = poly.len();
        if n < 3 {
            return;
        }
        let area_sign = if signed_area(&poly) >= 0.0 { 1.0 } else { -1.0 };
        for i in 0..n {
            let a = poly[i];
            let b = poly[(i + 1) % n];
            let dx = b.0 - a.0;
            let dy = b.1 - a.1;
            let len = dx.hypot(dy);
            if len <= 1e-5 {
                continue;
            }
            let tx = dx / len;
            let ty = dy / len;
            let nx = ty * area_sign;
            let ny = -tx * area_sign;
            segs.push(ContourArcSeg { s0: *total_len, len, a, b, normal: [nx, ny], tangent: [tx, ty] });
            *total_len += len;
        }
    }

    fn from_balloon(balloon: &LiveBalloon) -> Self {
        let (welded, detached) = build_welded_contour_for_sampling(balloon);
        Self::from_paths(&welded, &detached)
    }

    fn sample_at_u(&self, u: f64, fallback_balloon: &LiveBalloon) -> ([f64; 2], [f64; 2], [f64; 2]) {
        if self.segs.is_empty() || self.total_len <= 1e-5 {
            let t = u.rem_euclid(1.0);
            let (pt, n) = fallback_balloon.point_and_normal_on_body(t);
            return (pt, n, [-n[1], n[0]]);
        }
        let target = u.rem_euclid(1.0) * self.total_len;
        let idx = match self.segs.binary_search_by(|seg| seg.s0.partial_cmp(&target).unwrap_or(std::cmp::Ordering::Equal)) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        };
        let seg = &self.segs[idx.min(self.segs.len() - 1)];
        let local_t = ((target - seg.s0) / seg.len.max(1e-6)).clamp(0.0, 1.0);
        let px = seg.a.0 + (seg.b.0 - seg.a.0) * local_t;
        let py = seg.a.1 + (seg.b.1 - seg.a.1) * local_t;
        ([px, py], seg.normal, seg.tangent)
    }

    /// Samples every intermediate polyline vertex inside the arc interval `[u0, u0 + span]`.
    /// Unlike three-point chords this keeps sharp spike tips and burst valleys on the outline,
    /// which is essential for dash ribbons that must hug (and not cut through) the contour.
    fn sample_range(&self, u0: f64, span: f64) -> Vec<P> {
        if self.segs.is_empty() || self.total_len <= 1e-5 {
            return Vec::new();
        }
        let s0 = u0.rem_euclid(1.0) * self.total_len;
        let s1 = s0 + span.clamp(0.0, 1.0) * self.total_len;
        let mut out: Vec<P> = Vec::new();
        // The interval can wrap past the end of the contour (the last dash of a ring).
        for pass in 0..2 {
            let (a, b) = if pass == 0 { (s0, s1.min(self.total_len)) } else { (0.0, (s1 - self.total_len).min(self.total_len)) };
            if b <= a {
                continue;
            }
            for seg in &self.segs {
                let lo = a.max(seg.s0);
                let hi = b.min(seg.s0 + seg.len);
                if hi <= lo {
                    continue;
                }
                for s in [lo, hi] {
                    let t = ((s - seg.s0) / seg.len.max(1e-6)).clamp(0.0, 1.0);
                    let p = (seg.a.0 + (seg.b.0 - seg.a.0) * t, seg.a.1 + (seg.b.1 - seg.a.1) * t);
                    if out.last().is_none_or(|q| dist(*q, p) > 1e-4) {
                        out.push(p);
                    }
                }
            }
        }
        out
    }
}

/// Samples evenly-spaced contour stations around each closed component of `balloon` (for raster
/// dab-chain strokes or UV ribbon mapping).
pub fn sample_balloon_contour_for_brush(balloon: &LiveBalloon, step_px: f64) -> Vec<Vec<BalloonContourStation>> {
    let step = step_px.clamp(0.5, 64.0);
    let (welded, detached) = build_welded_contour_for_sampling(balloon);
    let mut out = Vec::new();
    for sp in welded.subpaths.iter().chain(detached.iter()) {
        let table = ContourArcTable::from_subpath(sp);
        if table.total_len <= 2.0 {
            continue;
        }
        let count = ((table.total_len / step).ceil() as usize).clamp(8, 2048);
        let mut ring = Vec::with_capacity(count);
        for i in 0..count {
            let u = (i as f64) / (count as f64);
            let (pos, normal, tangent) = table.sample_at_u(u, balloon);
            ring.push(BalloonContourStation { pos, normal, tangent, arc_len: u * table.total_len, total_len: table.total_len });
        }
        out.push(ring);
    }
    out
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

/// Offsets a closed polygon outward along its vertex normals by `distance` px (with miter clamp).
fn offset_polygon_outward(pts: &[P], distance: f64) -> Vec<P> {
    let n = pts.len();
    if n < 3 || distance.abs() < 1e-6 {
        return pts.to_vec();
    }
    let ccw = signed_area(pts) >= 0.0;
    let sign = if ccw { 1.0 } else { -1.0 };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let prev = pts[(i + n - 1) % n];
        let curr = pts[i];
        let next = pts[(i + 1) % n];
        let d1 = norm(sub(curr, prev));
        let d2 = norm(sub(next, curr));
        let n1 = (d1.1 * sign, -d1.0 * sign);
        let n2 = (d2.1 * sign, -d2.0 * sign);
        let bis = norm(add(n1, n2));
        let normal = if bis == (0.0, 0.0) { n1 } else { bis };
        let dot = (n1.0 * normal.0 + n1.1 * normal.1).max(1.0 / 3.0);
        // A concave corner (the tail/body seam) makes the two edge normals point away from each
        // other: a full miter there crosses the neighbouring edges and grows a long spike. Clip
        // the offset to one step there (a bevel), and keep sharp convex corners modest too.
        let turn = d1.0 * d2.1 - d1.1 * d2.0;
        let convex = if ccw { turn >= 0.0 } else { turn <= 0.0 };
        let miter = if convex { (1.0 / dot).min(1.8) } else { 1.0 };
        out.push(add(curr, mul(normal, distance * miter)));
    }
    out
}

/// `BalloonLineStyle::Double`: welds tails to the base contour and adds a concentric outer echo
/// `double_gap_px` away (`PathOp::Intersect` keeps the ring between the lines transparent).
fn apply_double_line_style(balloon: &LiveBalloon, mut welded_base: Path, detached: Vec<Subpath>) -> Path {
    let gap = balloon.double_gap_px.clamp(0.5, 64.0);
    let mut outer_subpaths = Vec::new();
    for sp in &mut welded_base.subpaths {
        sp.op = PathOp::Combine;
        if sp.closed && sp.knots.len() >= 3 {
            let flat = flatten_subpath(sp, UNION_TOL);
            let poly = clean(&flat.pts);
            if poly.len() >= 3 {
                let expanded = clean(&strip_self_intersections(&offset_polygon_outward(&poly, gap)));
                if expanded.len() >= 3 {
                    let mut echo = Subpath::polygon(&expanded);
                    echo.op = PathOp::Intersect;
                    outer_subpaths.push(echo);
                }
            }
        }
    }
    welded_base.subpaths.extend(outer_subpaths);
    welded_base.subpaths.extend(detached);
    welded_base
}

/// Bevel normals for the stations of one dash. The normal at a station comes from the chord
/// between its two neighbours, which averages the directions on either side of a spike tip or a
/// burst valley. Using a single segment's normal instead makes the ribbon twist inside out when a
/// dash crosses a corner: the old three-point dash cut straight across and made "butterflies".
fn dash_station_normals(stations: &[P], orientation: f64) -> Vec<[f64; 2]> {
    let m = stations.len();
    (0..m)
        .map(|i| {
            let a = if i > 0 { stations[i - 1] } else { stations[i] };
            let b = if i + 1 < m { stations[i + 1] } else { stations[i] };
            let dx = b.0 - a.0;
            let dy = b.1 - a.1;
            let len = dx.hypot(dy);
            if len < 1e-6 {
                return [0.0, 0.0];
            }
            let (tx, ty) = (dx / len, dy / len);
            [ty * orientation, -tx * orientation]
        })
        .collect()
}

/// `BalloonLineStyle::Whisper`: slices the actual welded body + tail contour into `whisper_dash_count`
/// dashed capsule polygons with duty ratio `whisper_duty` (`0.20..=0.85`). Each dash follows every
/// intermediate vertex of the flattened contour (no three-point chord) and is split at sharp
/// corners, so it hugs `Spiky`/`Burst` tips instead of cutting across and folding into a bow tie.
fn apply_whisper_line_style(balloon: &LiveBalloon, welded_base: Path, detached: Vec<Subpath>) -> Path {
    let table = ContourArcTable::from_paths(&welded_base, &detached);
    let count = balloon.whisper_dash_count.clamp(6, 96);
    let duty = balloon.whisper_duty.clamp(0.20, 0.85);
    let half_thick = (balloon.stroke_width.clamp(1.0, 24.0) * 0.75).max(1.4);
    let seed = balloon.noise_seed() ^ 0x5748_4953;

    let mut subpaths = Vec::with_capacity(count as usize);
    let step = 1.0 / (count as f64);
    for i in 0..count {
        let jit = (hash_f64(seed, i * 2) - 0.5) * 0.12 * step;
        let u0 = ((i as f64) * step + jit).rem_euclid(1.0);
        let span = step * duty;
        let mut stations = table.sample_range(u0, span);
        if stations.len() < 2 {
            // Degenerate/empty table: keep the old three-point fallback for that dash.
            let u_mid = (u0 + span * 0.5).rem_euclid(1.0);
            let u1 = (u0 + span).rem_euclid(1.0);
            stations = [u0, u_mid, u1]
                .into_iter()
                .map(|u| {
                    let (p, _, _) = table.sample_at_u(u, balloon);
                    (p[0], p[1])
                })
                .collect();
        }
        let m = stations.len();
        if m < 2 {
            continue;
        }
        // A ribbon can only wrap a gentle turn without its two sides crossing. Measure the turn at
        // every interior station and start a fresh dash piece whenever the outline bends sharply.
        let mut turns = vec![0.0_f64; m];
        for j in 1..m - 1 {
            let (a, p, b) = (stations[j - 1], stations[j], stations[j + 1]);
            let (ax, ay) = (p.0 - a.0, p.1 - a.1);
            let (bx, by) = (b.0 - p.0, b.1 - p.1);
            let (al, bl) = (ax.hypot(ay), bx.hypot(by));
            if al > 1e-6 && bl > 1e-6 {
                turns[j] = ((ax * bx + ay * by) / (al * bl)).clamp(-1.0, 1.0).acos();
            }
        }
        let mut start = 0;
        let mut pieces: Vec<(usize, usize)> = Vec::new();
        for (j, &turn) in turns.iter().enumerate().take(m - 1).skip(1) {
            if turn > 0.85 {
                pieces.push((start, j));
                start = j;
            }
        }
        pieces.push((start, m - 1));

        for (a, b) in pieces {
            if b <= a {
                continue;
            }
            let slice = &stations[a..=b];
            let normals = dash_station_normals(slice, table.orientation);
            let n = slice.len();
            let mut left = Vec::with_capacity(n);
            let mut right = Vec::with_capacity(n);
            for j in 0..n {
                let p = slice[j];
                let turn = if j == 0 || j + 1 == n { 0.0 } else { turns[a + j] };
                let run = [if j > 0 { dist(p, slice[j - 1]) } else { f64::INFINITY }, if j + 1 < n { dist(p, slice[j + 1]) } else { f64::INFINITY }]
                    .into_iter()
                    .fold(f64::INFINITY, f64::min);
                let turn_factor = (1.0 - turn / std::f64::consts::PI).max(0.25);
                let bulge = if j == 0 || j + 1 == n { 1.0 } else { 1.15 };
                let thick = (half_thick.min(run * 0.9) * turn_factor * bulge).max(0.35);
                left.push((p.0 + normals[j][0] * thick, p.1 + normals[j][1] * thick));
                right.push((p.0 - normals[j][0] * thick, p.1 - normals[j][1] * thick));
            }
            right.reverse();
            left.extend(right);
            let polygon = clean(&left);
            if polygon.len() < 3 {
                continue;
            }
            let mut sp = Subpath::polygon(&polygon);
            sp.op = PathOp::Combine;
            subpaths.push(sp);
        }
    }
    Path::new(subpaths)
}

/// Resolves an optional built-in [`VectorBrushPreset`] referenced by `balloon.brush_preset_id`.
fn resolve_balloon_brush_preset(balloon: &LiveBalloon) -> Option<VectorBrushPreset> {
    balloon.brush_preset_id.as_deref().and_then(VectorBrushPreset::builtin_by_id)
}

/// Evaluates `catmull_rom_knots` into a smooth polyline for ribbon generation.
fn resample_catmull_rom_half(pts: &[P]) -> Vec<P> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let knots = catmull_rom_knots(pts, false, 0.5);
    let mut smooth = Vec::with_capacity(knots.len() * 2);
    for i in 0..knots.len().saturating_sub(1) {
        let k0 = &knots[i];
        let k1 = &knots[i + 1];
        smooth.push((k0.anchor.x, k0.anchor.y));
        let mid = eval_cubic_bezier(k0.anchor, k0.out_ctrl, k1.in_ctrl, k1.anchor, 0.5);
        smooth.push((mid.x, mid.y));
    }
    if let Some(last) = knots.last() {
        smooth.push((last.anchor.x, last.anchor.y));
    }
    smooth
}

/// Builds coherent multi-pass graphite ribbons for `BalloonLineStyle::Pencil` (TZ v6.0 §3.1):
/// 3-harmonic normal spline wobble, tangent entry/exit overshoot, `catmull_rom_knots` smoothing,
/// and tapered grain ribbons without per-vertex white noise or `PathOp::Intersect`.
fn build_pencil_ink_subpaths(balloon: &LiveBalloon, welded_base: &Path, detached: &[Subpath]) -> Vec<Subpath> {
    let preset = resolve_balloon_brush_preset(balloon);
    let strands = preset.as_ref().map(|p| u32::from(p.strand_count).clamp(2, 7)).unwrap_or_else(|| balloon.pencil_strands.clamp(2, 7));
    let roughness = preset.as_ref().map(|p| f64::from(p.coherent_roughness).clamp(0.0, 1.0)).unwrap_or_else(|| balloon.pencil_roughness.clamp(0.0, 1.0));
    let grain_freq = preset.as_ref().map(|p| f64::from(p.grain_frequency).clamp(2.0, 64.0)).unwrap_or(14.0);

    let sw = balloon.stroke_width.clamp(1.0, 24.0);
    let w0 = sw * (0.48 + 0.14 * (1.0 - ((strands as f64 - 2.0) / 5.0).clamp(0.0, 1.0)));
    let half_w0 = (w0 * 0.5).max(0.45);
    let a_rough = sw * (0.25 + 1.1 * roughness);
    let seed = balloon.noise_seed() ^ 0x517C_C1B7;
    let h_thresh = 0.14 * half_w0;
    let grain_amp = 0.25 + 0.75 * roughness;

    let mut out_subpaths = Vec::new();
    for (s_idx, sp) in welded_base.subpaths.iter().chain(detached.iter()).enumerate() {
        if !sp.closed || sp.knots.len() < 3 {
            continue;
        }
        let table = ContourArcTable::from_subpath(sp);
        if table.total_len < 6.0 {
            continue;
        }
        let arcs_per_pass = |k: u32, pass_seed: u32| -> Vec<(f64, f64)> {
            let u0 = hash_f64(pass_seed, 10);
            if k == 0 || k.is_multiple_of(2) { vec![(u0, u0 + 1.04)] } else { vec![(u0, u0 + 0.56), (u0 + 0.48, u0 + 1.06)] }
        };

        for k in 0..strands {
            let pass_seed = seed ^ (k.wrapping_mul(0x9E37_79B9) ^ (s_idx as u32).wrapping_mul(0x85EB_CA6B));
            let bias_k = ((k as f64) - (strands as f64 - 1.0) * 0.5) * (0.38 * w0 + 0.55 * roughness);
            let phi0 = hash_f64(pass_seed, 1) * TAU;
            let phi1 = hash_f64(pass_seed, 2) * TAU;
            let phi2 = hash_f64(pass_seed, 3) * TAU;
            let psi_k = hash_f64(pass_seed, 4) * TAU;

            for (arc_idx, (u_a, u_b)) in arcs_per_pass(k, pass_seed).into_iter().enumerate() {
                let arc_seed = pass_seed ^ (arc_idx as u32).wrapping_mul(0xC2B2_AE35);
                let stations = 48usize;
                let overshoot = 2.0 + 3.0 * hash_f64(arc_seed, 77);
                let mut raw_pts = Vec::with_capacity(stations + 1);

                for st in 0..=stations {
                    let t = (st as f64) / (stations as f64);
                    let u = (u_a + (u_b - u_a) * t).rem_euclid(1.0);
                    let (pos, normal, tangent) = table.sample_at_u(u, balloon);
                    let disp = bias_k
                        + a_rough * (0.55 * (TAU * (2.0 * u) + phi0).sin() + 0.30 * (TAU * (5.0 * u) + phi1).sin() + 0.15 * (TAU * (11.0 * u) + phi2).sin());
                    let tang_shift = if t < 0.08 {
                        -overshoot * (1.0 - t / 0.08)
                    } else if t > 0.92 {
                        overshoot * ((t - 0.92) / 0.08)
                    } else {
                        0.0
                    };
                    raw_pts.push((pos[0] + normal[0] * disp + tangent[0] * tang_shift, pos[1] + normal[1] * disp + tangent[1] * tang_shift));
                }

                let smooth_pts = resample_catmull_rom_half(&raw_pts);
                let m = smooth_pts.len();
                if m < 3 {
                    continue;
                }

                let mut widths = Vec::with_capacity(m);
                let mut normals = Vec::with_capacity(m);
                for i in 0..m {
                    let t = (i as f64) / ((m - 1) as f64);
                    let u = (u_a + (u_b - u_a) * t).rem_euclid(1.0);
                    let prev = smooth_pts[i.saturating_sub(1)];
                    let next = smooth_pts[(i + 1).min(m - 1)];
                    let tang = norm(sub(next, prev));
                    normals.push((-tang.1, tang.0));

                    let taper = (t / 0.10).clamp(0.0, 1.0).min(((1.0 - t) / 0.10).clamp(0.0, 1.0)).max(0.18);
                    let prof = preset.as_ref().map(|p| f64::from(p.width_at(t as f32))).unwrap_or(1.0);
                    let grain = 0.6 * (TAU * grain_freq * u + psi_k).cos() + 0.4 * (TAU * (grain_freq * 0.5 + 3.0) * u + psi_k * 1.7).sin();
                    let factor = (0.45 + 0.55 * grain * grain_amp).clamp(0.08, 1.0);
                    widths.push(half_w0 * prof * taper * factor);
                }

                // Group contiguous visible segments where h_k(u) >= 0.14 * w0 / 2 into closed graphite ribbons.
                let mut run_start: Option<usize> = None;
                let flush_run = |start: usize, end: usize, subpaths: &mut Vec<Subpath>| {
                    let run_len = end - start + 1;
                    if run_len < 3 {
                        return;
                    }
                    let mut left = Vec::with_capacity(run_len);
                    let mut right = Vec::with_capacity(run_len);
                    for j in start..=end {
                        let tip_scale = if j == start || j == end { 0.30 } else { 1.0 };
                        let w = widths[j] * tip_scale;
                        let n = normals[j];
                        let p = smooth_pts[j];
                        left.push((p.0 + n.0 * w, p.1 + n.1 * w));
                        right.push((p.0 - n.0 * w, p.1 - n.1 * w));
                    }
                    right.reverse();
                    left.extend(right);
                    let cleaned = clean(&left);
                    if cleaned.len() >= 3 {
                        let mut ribbon_sp = Subpath::polygon(&cleaned);
                        ribbon_sp.op = PathOp::Combine;
                        subpaths.push(ribbon_sp);
                    }
                };

                for (i, &h) in widths.iter().enumerate() {
                    if h >= h_thresh {
                        if run_start.is_none() {
                            run_start = Some(i);
                        }
                    } else if let Some(s) = run_start.take() {
                        flush_run(s, i.saturating_sub(1), &mut out_subpaths);
                    }
                }
                if let Some(s) = run_start {
                    flush_run(s, m - 1, &mut out_subpaths);
                }
            }
        }
    }

    out_subpaths
}

/// `BalloonLineStyle::Pencil`: coherent multi-pass graphite sketch ribbons along the welded contour.
fn apply_pencil_line_style(balloon: &LiveBalloon, welded_base: Path, detached: Vec<Subpath>) -> Path {
    let subpaths = build_pencil_ink_subpaths(balloon, &welded_base, &detached);
    if subpaths.is_empty() {
        let mut fallback = welded_base;
        fallback.subpaths.extend(detached);
        return fallback;
    }
    Path::new(subpaths)
}

/// Integer Bresenham on the pixel grid (8-connected): mark every cell a flattened contour segment
/// crosses. Working in whole cells keeps every emitted rectangle on exact document pixels.
fn mark_pixel_line(a: (i32, i32), b: (i32, i32), cells: &mut HashSet<(i32, i32)>) {
    let (mut x, mut y) = a;
    let dx = (b.0 - a.0).abs();
    let sx = if a.0 < b.0 { 1 } else { -1 };
    let dy = -(b.1 - a.1).abs();
    let sy = if a.1 < b.1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        cells.insert((x, y));
        if x == b.0 && y == b.1 {
            return;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

/// `BalloonLineStyle::Pixel1Bit`: an honest 1-bit pixel line. The contour is rasterized onto an
/// integer grid with a whole-cell step; every occupied cell of a row is merged into one axis-aligned
/// rectangle with integer corners. Such a rectangle has strictly 0/1 coverage in the vector
/// rasterizer, so no antialiased grey fringe can appear around the black ink.
fn apply_pixel_1bit_line_style(balloon: &LiveBalloon, welded_base: Path, detached: Vec<Subpath>) -> Path {
    let cell = balloon.pixel_size_px.round().clamp(1.0, 24.0) as i32;
    let mut cells: HashSet<(i32, i32)> = HashSet::new();
    for sp in welded_base.subpaths.iter().chain(detached.iter()) {
        if !sp.closed || sp.knots.len() < 3 {
            continue;
        }
        let flat = flatten_subpath(sp, 0.35);
        let poly = clean(&flat.pts);
        if poly.len() < 2 {
            continue;
        }
        let grid = |p: P| ((p.0 / cell as f64).round() as i32, (p.1 / cell as f64).round() as i32);
        for pair in poly.windows(2) {
            mark_pixel_line(grid(pair[0]), grid(pair[1]), &mut cells);
        }
        if poly.len() > 2 {
            mark_pixel_line(grid(poly[poly.len() - 1]), grid(poly[0]), &mut cells);
        }
    }

    let mut rows: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
    for &(gx, gy) in &cells {
        rows.entry(gy).or_default().push(gx);
    }
    let mut out = Vec::new();
    for (gy, mut xs) in rows {
        xs.sort_unstable();
        let mut run = (xs[0], xs[0]);
        for &gx in &xs[1..] {
            if gx == run.1 + 1 {
                run.1 = gx;
            } else {
                push_pixel_rect(&mut out, run, gy, cell);
                run = (gx, gx);
            }
        }
        push_pixel_rect(&mut out, run, gy, cell);
    }
    Path::new(out)
}

fn push_pixel_rect(out: &mut Vec<Subpath>, run: (i32, i32), gy: i32, cell: i32) {
    let (x0, x1) = (run.0 as f64 * cell as f64, (run.1 + 1) as f64 * cell as f64);
    let (y0, y1) = (gy as f64 * cell as f64, (gy + 1) as f64 * cell as f64);
    out.push(Subpath::polygon(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1)]));
}

/// Subdivides polygon segments longer than `max_step` while preserving all sharp corners.
fn resample_preserve_corners(poly: &[P], max_step: f64, max_pts: usize) -> Vec<P> {
    let n = poly.len();
    if n < 3 {
        return poly.to_vec();
    }
    let mut total_len = 0.0;
    for i in 0..n {
        total_len += dist(poly[i], poly[(i + 1) % n]);
    }
    let eff_step = max_step.max(total_len / (max_pts.max(32) as f64)).max(0.5);
    let mut out = Vec::with_capacity(max_pts);
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        let d = dist(a, b);
        if d <= 1e-5 {
            continue;
        }
        out.push(a);
        let divs = (d / eff_step).ceil().clamp(1.0, max_pts.max(64) as f64) as usize;
        for s in 1..divs {
            let t = (s as f64) / (divs as f64);
            out.push(lerp(a, b, t));
        }
    }
    out
}

/// Builds Fukidashi-style calligraphic brush annulus rings (`outer CW Combine` + `inner CCW Join`)
/// for `BalloonLineStyle::UkiyoBrush` (TZ v6.0 §3.2).
fn build_ukiyo_ink_subpaths(balloon: &LiveBalloon, welded_base: &Path, detached: &[Subpath]) -> Vec<Subpath> {
    let preset = resolve_balloon_brush_preset(balloon);
    let alpha_nib = preset.as_ref().map(|p| f64::from(p.nib_angle_deg).to_radians()).unwrap_or_else(|| 35.0_f64.to_radians());
    let nib_anis = preset.as_ref().map(|p| (1.0 - f64::from(p.nib_roundness).clamp(0.1, 1.0)).max(0.25)).unwrap_or(0.55);
    let contrast = balloon.ukiyo_contrast.clamp(0.0, 1.0);
    let w_base = balloon.stroke_width.clamp(1.0, 24.0);
    let seed = balloon.noise_seed() ^ 0xB7E1_5163;

    let mut out_subpaths = Vec::new();
    for (s_idx, sp) in welded_base.subpaths.iter().chain(detached.iter()).enumerate() {
        if !sp.closed || sp.knots.len() < 3 {
            continue;
        }
        let flat = flatten_subpath(sp, 0.25);
        let poly = oriented(&flat.pts);
        if poly.len() < 3 {
            continue;
        }
        let pts = resample_preserve_corners(&poly, 1.5, 320);
        let n = pts.len();
        if n < 3 {
            continue;
        }

        let mut arc_s = Vec::with_capacity(n);
        let mut total_len = 0.0;
        for i in 0..n {
            arc_s.push(total_len);
            total_len += dist(pts[i], pts[(i + 1) % n]);
        }
        if total_len < 4.0 {
            continue;
        }

        let psi = hash_f64(seed ^ (s_idx as u32).wrapping_mul(0x9E37_79B9), 7) * TAU;
        let mut outer_pts = Vec::with_capacity(n);
        let mut inner_pts = Vec::with_capacity(n);

        for i in 0..n {
            let prev = pts[(i + n - 1) % n];
            let curr = pts[i];
            let next = pts[(i + 1) % n];
            let t1 = norm(sub(curr, prev));
            let t2 = norm(sub(next, curr));
            // `pts` is oriented CW in y-down coords (`signed_area >= 0`), so `(ty, -tx)` points outward.
            let n1 = (t1.1, -t1.0);
            let n2 = (t2.1, -t2.0);
            let bis_sum = add(n1, n2);
            let bis = if len(bis_sum) < 1e-6 { n1 } else { norm(bis_sum) };
            let dot = (n1.0 * bis.0 + n1.1 * bis.1).max(0.1);
            let miter_out = (1.0 / dot).min(2.5);
            let miter_in = (1.0 / dot).min(1.35);

            let theta_i = bis.1.atan2(bis.0);
            let u_i = arc_s[i] / total_len;
            let s_dry = 0.42 * (TAU * 7.0 * u_i + psi * 1.3).sin()
                + 0.28 * (TAU * 13.0 * u_i + psi * 2.1).cos()
                + 0.18 * (TAU * 23.0 * u_i + psi * 0.7).sin()
                + 0.12 * (TAU * 37.0 * u_i + psi * 3.1).cos();
            let profile_mult = preset.as_ref().map(|p| f64::from(p.width_at(u_i as f32))).unwrap_or(1.0);
            let w_i = (w_base
                * profile_mult
                * (0.85
                    + nib_anis * contrast * (2.0 * (theta_i - alpha_nib)).sin()
                    + 0.35 * contrast * (TAU * 3.0 * u_i + psi).cos()
                    + 0.25 * contrast * s_dry))
                .max(0.35);

            outer_pts.push(add(curr, mul(bis, 0.5 * w_i * miter_out)));
            inner_pts.push(sub(curr, mul(bis, 0.5 * w_i * miter_in)));
        }

        // Outer ring walks CW (`PathOp::Combine`, winding +1); inner cutout walks CCW (`PathOp::Join`, winding -1).
        inner_pts.reverse();
        let outer_clean = clean(&outer_pts);
        let inner_clean = clean(&inner_pts);
        if outer_clean.len() >= 3 {
            let mut outer_sp = Subpath::polygon(&outer_clean);
            outer_sp.op = PathOp::Combine;
            out_subpaths.push(outer_sp);
            if inner_clean.len() >= 3 && signed_area(&inner_clean) < -1.0 {
                let mut inner_sp = Subpath::polygon(&inner_clean);
                inner_sp.op = PathOp::Join;
                out_subpaths.push(inner_sp);
            }
        }
    }

    out_subpaths
}

/// `BalloonLineStyle::UkiyoBrush`: variable-width calligraphic ink annulus ring (Fukidashi style).
fn apply_ukiyo_brush_line_style(balloon: &LiveBalloon, welded_base: Path, detached: Vec<Subpath>) -> Path {
    let subpaths = build_ukiyo_ink_subpaths(balloon, &welded_base, &detached);
    if subpaths.is_empty() {
        let mut fallback = welded_base;
        fallback.subpaths.extend(detached);
        return fallback;
    }
    Path::new(subpaths)
}

/// Computes canonical stipple dot `(index, [x, y], radius_px)` along the welded body + tail contour.
fn compute_stipple_dots(balloon: &LiveBalloon) -> Vec<(u32, [f64; 2], f64)> {
    let table = ContourArcTable::from_balloon(balloon);
    compute_stipple_dots_on_table(balloon, &table)
}

fn compute_stipple_dots_on_table(balloon: &LiveBalloon, table: &ContourArcTable) -> Vec<(u32, [f64; 2], f64)> {
    let r = balloon.body.rect();
    let cx = r[0] + r[2] * 0.5;
    let cy = r[1] + r[3] * 0.5;
    let rx = (r[2] * 0.5).abs().max(8.0);
    let ry = (r[3] * 0.5).abs().max(8.0);
    let min_r = rx.min(ry);

    let density = balloon.stipple_density.clamp(0.0, 100.0) / 100.0;
    let base_count = balloon.stipple_count.clamp(24, 2000) as f64;
    let randomness = balloon.stipple_randomness.clamp(0.0, 100.0) / 100.0;
    let spread = balloon.haze_spread.clamp(0.0, 100.0) / 100.0;
    let grain = balloon.grain_size.clamp(5.0, 100.0) / 100.0;
    let roughness = balloon.edge_roughness.clamp(0.0, 100.0) / 100.0;
    let seed = balloon.noise_seed() ^ 0xA533_C0DE;
    // Only 1, 2 or 4 symmetric spawn sectors are meaningful; anything else degrades to 4.
    let spawns = match balloon.particle_spawns {
        1 => 1,
        2 => 2,
        _ => 4,
    };

    let count = ((base_count * (0.20 + 0.80 * density)).round() as u32).clamp(24, 2000);
    let band_px = (min_r * (0.04 + spread * 0.34)).max(3.0);
    let base_dot_r = (1.1 + grain * 4.6).clamp(0.9, 7.5);

    let mut dots = Vec::with_capacity(count as usize);
    for i in 0..count {
        let u1 = hash_f64(seed, i * 5);
        let u2 = hash_f64(seed, i * 5 + 1);
        let u3 = hash_f64(seed, i * 5 + 2);
        let u4 = hash_f64(seed, i * 5 + 3);
        let u5 = hash_f64(seed, i * 5 + 4);

        // The contour coordinate comes from the sector and a low-discrepancy sequence, never from
        // `i / count`, so growing `count` preserves the positions of already-present dots.
        let sector = i % spawns;
        let k = i / spawns;
        let base_u = (sector as f64 + van_der_corput_2(k)) / (spawns as f64);
        let tang_u = (u1 - 0.5) * 0.22 * randomness / (spawns as f64);
        let u = (base_u + tang_u).rem_euclid(1.0);
        let (base_pt, normal, tangent) = table.sample_at_u(u, balloon);

        let ring_layer = (i % 3) as f64;
        let radial_norm = if balloon.stipple_fill_interior {
            if i % 3 == 0 { -0.35 * (0.2 + 0.8 * u2 * randomness) } else { (0.08 + 0.92 * u2.powf(0.85)) * (0.45 + 0.55 * density + 0.45 * randomness) }
        } else {
            let ring_bias = (ring_layer - 1.0) * 0.28 * (1.0 - density * 0.45);
            let rand_radial = (u2 - 0.44) * (0.35 + 0.85 * randomness);
            ring_bias + rand_radial
        };
        let rough_wave = 1.0 + roughness * ((u4 - 0.5) * 1.4 + (u * TAU * 7.0).sin() * 0.35);
        let radial_signed = radial_norm * band_px * rough_wave;
        let tang_jitter = (u5 - 0.5) * band_px * 0.35 * randomness;

        let dist_norm = (radial_signed.abs() / band_px.max(1.0)).clamp(0.0, 1.4);
        let size_falloff = (1.18 - dist_norm * 0.68).clamp(0.25, 1.28);
        let size_rand = 1.0 + (u3 - 0.5) * 1.15 * randomness;
        let raw_radius = (base_dot_r * size_falloff * size_rand.clamp(0.35, 1.75)).clamp(0.75, 9.5);

        let (off, scale) = balloon.get_stipple_override(i);
        let x = base_pt[0] + normal[0] * radial_signed + tangent[0] * tang_jitter + off[0];
        let y = base_pt[1] + normal[1] * radial_signed + tangent[1] * tang_jitter + off[1];
        let radius = (raw_radius * scale.clamp(0.2, 4.0)).clamp(0.6, 18.0);

        dots.push((i, [x.clamp(cx - rx * 3.0, cx + rx * 3.0), y.clamp(cy - ry * 3.0, cy + ry * 3.0)], radius));
    }
    dots
}

/// `Uniflash + StippleDots`: dots live *near the needles*, not evenly around the body perimeter.
/// Every dot deterministically picks one needle ray, a position along it (denser near the inner
/// end and the middle) and a lateral offset clamped so it stays within `haze_spread` of the ray.
pub fn compute_uniflash_stipple_dots(balloon: &LiveBalloon) -> Vec<(u32, [f64; 2], f64)> {
    let Some(needles) = uniflash_needles(balloon) else { return Vec::new() };
    if needles.is_empty() {
        return Vec::new();
    }
    let count = balloon.stipple_count.clamp(24, 2000);
    let max_lat = (balloon.haze_spread.clamp(0.0, 100.0) / 100.0 * 12.0).clamp(1.2, 14.0);
    let grain = balloon.grain_size.clamp(5.0, 100.0) / 100.0;
    let seed = balloon.noise_seed() ^ 0x5EED_0123;
    let spawns = match balloon.particle_spawns {
        1 => 1,
        2 => 2,
        _ => 4,
    };
    let mut dots = Vec::with_capacity(count as usize);
    for i in 0..count {
        // Symmetric spawn sectors work on Uniflash too: map each sector's low-discrepancy position
        // onto the needle ring instead of scattering dots by a modular stride of `i`.
        let sector = i % spawns;
        let k = i / spawns;
        let ray_idx = (((sector as f64 + van_der_corput_2(k)) / (spawns as f64)) * needles.len() as f64).floor() as usize % needles.len();
        let n = &needles[ray_idx];
        let u = hash_f64(seed, i * 4);
        let s = u.powf(0.65) * 0.96;
        let lat = (hash_f64(seed, i * 4 + 1) - 0.5) * 2.0 * max_lat * (1.0 - 0.65 * s);
        let along = (hash_f64(seed, i * 4 + 2) - 0.5) * 4.0;
        let dir = (n.outer.x - n.inner.x, n.outer.y - n.inner.y);
        let len = dir.0.hypot(dir.1).max(1.0);
        let x = n.inner.x + dir.0 * s + n.perp[0] * lat + dir.0 / len * along;
        let y = n.inner.y + dir.1 * s + n.perp[1] * lat + dir.1 / len * along;
        let radius = (1.1 + grain * 4.6 * hash_f64(seed, i * 4 + 3)).clamp(0.9, 7.5);
        dots.push((i, [x, y], radius));
    }
    dots
}

/// The inner ellipse of a Uniflash: the only region that may be filled when stipple dots are used,
/// so the blob never spills past the needle rays.
fn uniflash_inner_fill_subpath(balloon: &LiveBalloon) -> Option<Subpath> {
    let LiveBalloonBody::Uniflash { rect, inner_ratio, .. } = &balloon.body else { return None };
    let cx = rect[0] + rect[2] * 0.5;
    let cy = rect[1] + rect[3] * 0.5;
    let rx = (rect[2] * 0.5).abs().max(4.0) * inner_ratio.clamp(0.30, 0.90);
    let ry = (rect[3] * 0.5).abs().max(4.0) * inner_ratio.clamp(0.30, 0.90);
    let mut pts = Vec::with_capacity(48);
    for k in 0..48 {
        let a = TAU * (k as f64) / 48.0;
        pts.push((cx + rx * a.cos(), cy + ry * a.sin()));
    }
    let mut sp = Subpath::polygon(&pts);
    sp.op = PathOp::Combine;
    Some(sp)
}

/// `BalloonLineStyle::StippleDots`: builds the stipple-dot contour path along the welded contour.
fn apply_stipple_dots_line_style(balloon: &LiveBalloon, mut welded_base: Path, mut detached: Vec<Subpath>) -> Path {
    let table = ContourArcTable::from_paths(&welded_base, &detached);
    let dots = compute_stipple_dots_on_table(balloon, &table);
    let extra_cap = if balloon.stipple_fill_interior { welded_base.subpaths.len() + detached.len() } else { 0 };
    let mut subpaths = Vec::with_capacity(dots.len() + extra_cap);

    if balloon.stipple_fill_interior {
        for mut sp in welded_base.subpaths.drain(..).chain(detached.drain(..)) {
            sp.op = PathOp::Combine;
            subpaths.push(sp);
        }
    }

    for (_, center, radius) in dots {
        let oct = circle_contour((center[0], center[1]), radius, 8);
        let mut sp = Subpath::polygon(&oct);
        sp.op = PathOp::Combine;
        subpaths.push(sp);
    }
    Path::new(subpaths)
}

/// One sparkle / Bokeh particle: its centre and outer radius in document px.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SparkleParticle {
    center: [f64; 2],
    radius: f64,
}

/// Computes the star and Bokeh particle data of `SparkleGlitter` from an already-built contour
/// table. Positions come from a fixed number of spawn sectors and a low-discrepancy sequence, so
/// raising the count only adds particles: none of the existing ones move.
fn compute_sparkle_particles(balloon: &LiveBalloon, table: &ContourArcTable) -> (Vec<SparkleParticle>, Vec<SparkleParticle>) {
    let spawns = match balloon.particle_spawns {
        1 => 1,
        2 => 2,
        _ => 4,
    };

    // ---- 4-point shoujo stars ----------------------------------------------------------------
    let count = balloon.sparkle_count.clamp(0, 400);
    let size_scale = balloon.sparkle_size.clamp(0.2, 2.5);
    let randomness = balloon.sparkle_randomness.clamp(0.0, 100.0) / 100.0;
    let size_jitter = balloon.sparkle_size_jitter.clamp(0.0, 100.0) / 100.0;
    let seed = balloon.noise_seed() ^ 0x7F4A_7C15;
    let mut stars = Vec::with_capacity(count as usize);

    for i in 0..count {
        let u1 = hash_f64(seed, i * 4);
        let u2 = hash_f64(seed, i * 4 + 1);
        let u3 = hash_f64(seed, i * 4 + 2);
        let u4 = hash_f64(seed, i * 4 + 3);
        let sector = i % spawns;
        let k = i / spawns;
        let base_u = (sector as f64 + van_der_corput_2(k)) / (spawns as f64);
        let tang_u = (u1 - 0.5) * 0.22 * randomness / (spawns as f64);
        let u = (base_u + tang_u).rem_euclid(1.0);
        let (pt, normal, tangent) = table.sample_at_u(u, balloon);
        let (off, scale_override) = balloon.get_stipple_override(i);
        let drift = (u2 - 0.45) * (4.0 + 14.0 * randomness) * size_scale;
        let tang_shift = (u4 - 0.5) * 8.0 * randomness * size_scale;
        let cx = pt[0] + normal[0] * drift + tangent[0] * tang_shift + off[0];
        let cy = pt[1] + normal[1] * drift + tangent[1] * tang_shift + off[1];

        // A skewed power law gives tiny glints most of the time and sparse hero stars, while 0%
        // keeps every star at the same size. `0.25x .. ~2.8x` at full jitter.
        let skew = u3.powf(2.2);
        let size_at_full = 0.25 + 2.55 * skew;
        let jitter_factor = 1.0 + size_jitter * (size_at_full - 1.0);
        // Size variation belongs to the dedicated `sparkle_size_jitter` control only; positional
        // `sparkle_randomness` never changes the size, so 0% jitter is exactly uniform.
        let outer_r = (4.5 * jitter_factor) * size_scale * scale_override.clamp(0.2, 4.0);
        stars.push(SparkleParticle { center: [cx, cy], radius: outer_r.max(0.2) });
    }

    // ---- Bokeh particles --------------------------------------------------------------------
    let bokeh_count = balloon.bokeh_count.clamp(0, 400);
    let bokeh_size = balloon.bokeh_size.clamp(2.0, 36.0);
    let bokeh_rand = balloon.bokeh_size_randomness.clamp(0.0, 100.0) / 100.0;
    let spread = balloon.haze_spread.clamp(0.0, 100.0) / 100.0;
    let bseed = balloon.noise_seed() ^ 0x2424_B0FE;
    let mut bokeh = Vec::with_capacity(bokeh_count as usize);

    for i in 0..bokeh_count {
        let b1 = hash_f64(bseed, i * 3);
        let b2 = hash_f64(bseed, i * 3 + 1);
        let b3 = hash_f64(bseed, i * 3 + 2);
        let sector = i % spawns;
        let k = i / spawns;
        let base_u = (sector as f64 + van_der_corput_2(k)) / (spawns as f64);
        let tang_u = (b1 - 0.5) * 0.22 * bokeh_rand / (spawns as f64);
        let u = (base_u + tang_u).rem_euclid(1.0);
        let (pt, normal, tangent) = table.sample_at_u(u, balloon);
        let haze = (b2 - 0.35) * (2.0 + 22.0 * spread);
        let tang_shift = (b3 - 0.5) * 8.0 * spread;
        let cx = pt[0] + normal[0] * haze + tangent[0] * tang_shift;
        let cy = pt[1] + normal[1] * haze + tangent[1] * tang_shift;
        let size_at_full = 0.30 + 1.70 * b3.powf(2.2);
        let radius = (bokeh_size * (1.0 + bokeh_rand * (size_at_full - 1.0))).clamp(0.5, 80.0);
        bokeh.push(SparkleParticle { center: [cx, cy], radius });
    }

    (stars, bokeh)
}

/// Star + Bokeh particles for a whole balloon, rebuilding its welded contour when the caller does
/// not already have one (used by SAI linework baking).
fn sparkle_glitter_particles(balloon: &LiveBalloon) -> (Vec<SparkleParticle>, Vec<SparkleParticle>) {
    let (welded, detached) = build_welded_contour_for_sampling(balloon);
    let table = ContourArcTable::from_paths(&welded, &detached);
    compute_sparkle_particles(balloon, &table)
}

/// 4-point shoujo stars and Bokeh particles as filled subpaths along the welded body + tail contour
/// (used by both single-pass `SparkleGlitter` and two-pass ink overlay).
fn build_sparkle_star_subpaths(balloon: &LiveBalloon, welded_base: &Path, detached: &[Subpath]) -> Vec<Subpath> {
    let table = ContourArcTable::from_paths(welded_base, detached);
    let (stars, bokeh) = compute_sparkle_particles(balloon, &table);
    let mut out = Vec::with_capacity(stars.len() + bokeh.len());

    for p in stars {
        let r = p.radius;
        let inner_r = r * 0.24;
        let [cx, cy] = p.center;
        let mut star = Vec::with_capacity(8);
        for k in 0..8 {
            let a = (k as f64) * std::f64::consts::FRAC_PI_4;
            let rad = if k % 2 == 0 { r } else { inner_r };
            star.push((cx + rad * a.cos(), cy + rad * a.sin()));
        }
        let mut sp = Subpath::polygon(&star);
        sp.op = PathOp::Combine;
        out.push(sp);
    }

    for p in bokeh {
        let [cx, cy] = p.center;
        let r = p.radius;
        let pts = match balloon.bokeh_shape {
            BokehShape::Circle => circle_contour((cx, cy), r, 16),
            BokehShape::Hexagon => (0..6)
                .map(|k| {
                    let a = std::f64::consts::TAU * (k as f64) / 6.0;
                    (cx + r * a.cos(), cy + r * a.sin())
                })
                .collect(),
            BokehShape::Ring => {
                let mut sp = hollow_ring(cx, cy, r, (r * 0.62).max(1.0));
                sp.op = PathOp::Combine;
                out.push(sp);
                continue;
            }
        };
        let mut sp = Subpath::polygon(&pts);
        sp.op = PathOp::Combine;
        out.push(sp);
    }

    out
}

/// `BalloonLineStyle::SparkleGlitter`: combines the welded body + detached tails with shoujo stars.
fn apply_sparkle_glitter_line_style(balloon: &LiveBalloon, mut welded_base: Path, detached: Vec<Subpath>) -> Path {
    let particles = build_sparkle_star_subpaths(balloon, &welded_base, &detached);
    for sp in &mut welded_base.subpaths {
        sp.op = PathOp::Combine;
    }
    welded_base.subpaths.extend(particles);
    welded_base.subpaths.extend(detached);
    welded_base
}

/// Applies `balloon.line_style` to its welded contour and detached tail subpaths.
fn apply_line_style_to_path(balloon: &LiveBalloon, welded_base: Path, detached: Vec<Subpath>) -> Path {
    let is_uniflash = matches!(balloon.body, LiveBalloonBody::Uniflash { .. });
    // Uniflash only supports Solid and StippleDots. Any other style degrades to Solid; in
    // particular we never replace the needles with the outer envelope contour.
    let style = if is_uniflash && !matches!(balloon.line_style, BalloonLineStyle::Solid | BalloonLineStyle::StippleDots) {
        BalloonLineStyle::Solid
    } else {
        balloon.line_style
    };
    if is_uniflash && style == BalloonLineStyle::Solid {
        let mut p = welded_base;
        p.subpaths.extend(detached);
        return p;
    }
    let (welded_base, detached) = if is_uniflash { build_welded_contour_for_sampling(balloon) } else { (welded_base, detached) };
    match style {
        BalloonLineStyle::Solid => {
            let mut p = welded_base;
            p.subpaths.extend(detached);
            p
        }
        BalloonLineStyle::Double => apply_double_line_style(balloon, welded_base, detached),
        BalloonLineStyle::Whisper => apply_whisper_line_style(balloon, welded_base, detached),
        BalloonLineStyle::Pencil => apply_pencil_line_style(balloon, welded_base, detached),
        BalloonLineStyle::Pixel1Bit => apply_pixel_1bit_line_style(balloon, welded_base, detached),
        BalloonLineStyle::UkiyoBrush => apply_ukiyo_brush_line_style(balloon, welded_base, detached),
        BalloonLineStyle::StippleDots => apply_stipple_dots_line_style(balloon, welded_base, detached),
        BalloonLineStyle::SparkleGlitter => apply_sparkle_glitter_line_style(balloon, welded_base, detached),
    }
}

/// Builds the optional underlay / shadow contour behind the balloon.
pub fn build_underlay_path(balloon: &LiveBalloon) -> Option<Path> {
    let underlay: &BalloonUnderlay = balloon.underlay.as_ref()?;
    if !underlay.enabled {
        return None;
    }
    let r = balloon.body.rect();
    let scale = underlay.scale.clamp(0.8, 1.5);
    let w = (r[2].abs() * scale).max(8.0);
    let h = (r[3].abs() * scale).max(8.0);
    let cx = r[0] + r[2] * 0.5 + underlay.offset[0];
    let cy = r[1] + r[3] * 0.5 + underlay.offset[1];
    let rectness = underlay.rectness.clamp(0.0, 1.0);
    let mut p = build_superellipse_path(cx, cy, w * 0.5, h * 0.5, rectness);
    for s in &mut p.subpaths {
        s.op = PathOp::Combine;
    }
    Some(p)
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
            LiveBalloonBody::ChamferRect { chamfer, .. } => {
                let pad = (*chamfer * 0.55).max(6.0);
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
                let f = (1.0 - length * 0.55 - depth * 0.35).clamp(0.28, 0.80) * 0.66;
                (rx * f, ry * f)
            }
            LiveBalloonBody::DoubleOutline { squircle, gap, .. } => {
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
            LiveBalloonBody::Rect { .. } | LiveBalloonBody::ChamferRect { .. } => {
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
            LiveBalloonBody::Burst { rays, length, depth, jitter, seed, .. } => {
                let n = (*rays).clamp(5, 64);
                let step = std::f64::consts::TAU / (n as f64);
                let jit = jitter.clamp(0.0, 1.0);
                let len = length.clamp(0.10, 1.20);
                let dep = depth.clamp(0.0, 0.60);
                let mut valleys = Vec::with_capacity(n as usize);
                for i in 0..n {
                    let a_tip = -std::f64::consts::FRAC_PI_2 + (i as f64) * step + (hash_f64(*seed, i * 4) - 0.5) * jit * step * 0.5;
                    let tip_r = 1.0 + len * (1.0 + (hash_f64(*seed, i * 4 + 1) - 0.5) * jit * 0.8);
                    let off = self.get_vertex_offset(i);
                    out.push((i, [cx + rx * tip_r * a_tip.cos() + off[0], cy + ry * tip_r * a_tip.sin() + off[1]]));

                    // The valley after each tip is dragged through indices `n..2*n`, matching
                    // the offsets `burst_path` applies to the polygon's odd knots.
                    let a_valley = a_tip + step * 0.5;
                    let valley_r = (1.0 - dep * (1.0 + (hash_f64(*seed, i * 4 + 2) - 0.5) * jit * 0.6)).clamp(0.30, 1.0);
                    let off = self.get_vertex_offset(n + i);
                    valleys.push((n + i, [cx + rx * valley_r * a_valley.cos() + off[0], cy + ry * valley_r * a_valley.sin() + off[1]]));
                }
                out.extend(valleys);
            }
            LiveBalloonBody::Rect { rect, .. } => {
                let (x0, y0) = (rect[0].min(rect[0] + rect[2]), rect[1].min(rect[1] + rect[3]));
                let (x1, y1) = (rect[0].max(rect[0] + rect[2]), rect[1].max(rect[1] + rect[3]));
                for (i, p) in [[x0, y0], [x1, y0], [x1, y1], [x0, y1]].into_iter().enumerate() {
                    let off = self.get_vertex_offset(i as u32);
                    out.push((i as u32, [p[0] + off[0], p[1] + off[1]]));
                }
            }
            LiveBalloonBody::ChamferRect { rect, chamfer, concave } => {
                let geo = chamfer_body_geometry(*rect, *chamfer, ConcaveOrFlat { concave: *concave });
                // The eight bevel endpoints come first (indices 0..8), then the four bends
                // (indices 8..12), so the handle index matches the drag offset index.
                let mut bends = Vec::with_capacity(4);
                for (c, corner) in geo.corners.into_iter().enumerate() {
                    let c = c as u32;
                    let oa = self.get_vertex_offset(c * 2);
                    let ob = self.get_vertex_offset(c * 2 + 1);
                    let a = (corner.a.0 + oa[0], corner.a.1 + oa[1]);
                    let b = (corner.b.0 + ob[0], corner.b.1 + ob[1]);
                    out.push((c * 2, [a.0, a.1]));
                    out.push((c * 2 + 1, [b.0, b.1]));
                    let base_mid = if geo.scoop > 0.05 {
                        (0.25 * a.0 + 0.5 * corner.ctrl.0 + 0.25 * b.0, 0.25 * a.1 + 0.5 * corner.ctrl.1 + 0.25 * b.1)
                    } else {
                        (0.5 * (a.0 + b.0), 0.5 * (a.1 + b.1))
                    };
                    let off = self.get_vertex_offset(8 + c);
                    bends.push((8 + c, [base_mid.0 + off[0], base_mid.1 + off[1]]));
                }
                out.extend(bends);
            }
            LiveBalloonBody::Ellipse { squircle, .. } => {
                for (i, p) in oval_bulge_points(cx, cy, rx, ry, *squircle).into_iter().enumerate() {
                    let off = self.get_vertex_offset(i as u32);
                    out.push((i as u32, [p.0 + off[0], p.1 + off[1]]));
                }
            }
            LiveBalloonBody::Uniflash { mode, needles, inner_ratio, bundle_group, length_jitter, angle_jitter, seed, .. } => {
                let n = (*needles).clamp(24, 450);
                let shown = n.min(UNIFLASH_HANDLES);
                let stride = n as f64 / shown as f64;
                let step = std::f64::consts::TAU / (n as f64);
                for slot in 0..shown {
                    let i = ((slot as f64) * stride).round() as u32;
                    let rand_a = hash_f64(*seed, i * 3) - 0.5;
                    let rand_in = hash_f64(*seed, i * 3 + 1);
                    let rand_out = hash_f64(*seed, i * 3 + 2);
                    let bundle_phase = ((i as f64) * 0.35 * std::f64::consts::TAU).sin();
                    let bundle_shift = bundle_phase * step * 0.35 * bundle_group.clamp(0.0, 1.0);
                    let theta = (i as f64) * step + bundle_shift + rand_a * step * angle_jitter.clamp(0.0, 1.0);
                    let off = self.get_vertex_offset(i);
                    let handle_r = match mode {
                        UniflashMode::InwardNeedles => {
                            let harmonic_in = if i.is_multiple_of(2) { 1.0 } else { 0.85 };
                            inner_ratio * (1.0 - 0.32 * length_jitter * rand_in * harmonic_in)
                        }
                        UniflashMode::SolidRingSpikes => {
                            let r_in = inner_ratio.clamp(0.25, 0.90);
                            let ring_outer = (r_in + 0.08).min(0.84);
                            let max_spike = (1.12 - ring_outer).max(0.16);
                            let harmonic = match i % 4 {
                                0 => 1.0,
                                2 => 0.72,
                                _ => 0.46,
                            };
                            ring_outer + max_spike * (1.0 - length_jitter * 0.65 * rand_out) * harmonic
                        }
                    };
                    out.push((i, [cx + rx * handle_r * theta.cos() + off[0], cy + ry * handle_r * theta.sin() + off[1]]));
                }
            }
            _ => {}
        }
        out
    }

    fn get_stipple_override(&self, index: u32) -> ([f64; 2], f64) {
        self.stipple_overrides.iter().find(|s| s.index == index).map(|s| (s.offset, s.radius_scale)).unwrap_or(([0.0, 0.0], 1.0))
    }

    fn stipple_dot_positions(&self) -> Vec<(u32, [f64; 2], f64)> {
        if self.line_style != BalloonLineStyle::StippleDots {
            return Vec::new();
        }
        let mut dots = compute_stipple_dots(self);
        dots.truncate(MAX_EDITABLE_STIPPLE_DOTS as usize);
        dots
    }

    fn noise_seed(&self) -> u32 {
        let body_seed = match &self.body {
            LiveBalloonBody::Cloud { seed, .. }
            | LiveBalloonBody::Spiky { seed, .. }
            | LiveBalloonBody::Burst { seed, .. }
            | LiveBalloonBody::Uniflash { seed, .. } => *seed,
            LiveBalloonBody::Ellipse { .. } | LiveBalloonBody::Rect { .. } | LiveBalloonBody::ChamferRect { .. } | LiveBalloonBody::DoubleOutline { .. } => {
                0x9E37_79B9
            }
        };
        body_seed ^ self.id.wrapping_mul(0x85EB_CA6B)
    }

    fn rebuild_closed_path(&self) -> Path {
        if let LiveBalloonBody::DoubleOutline { rect, squircle, gap } = &self.body {
            return rebuild_double_outline(self, *rect, *squircle, *gap);
        }

        let mut base_path = body_outline_with_noise(self);

        let mut detached_subpaths = Vec::new();
        for tail in &self.tails {
            let (welded, detached) = build_single_tail_path(self, tail);
            if let Some(w_path) = welded {
                base_path = merge_paths_union(&base_path, &w_path);
            }
            if let Some(d_path) = detached {
                detached_subpaths.extend(d_path.subpaths);
            }
        }
        apply_line_style_to_path(self, base_path, detached_subpaths)
    }
}

impl LiveBalloonGroupGeom for LiveBalloonGroup {
    fn rebuild_group_path(&self) -> Path {
        let mut merged_pool: Option<Path> = None;
        let mut isolated_subpaths = Vec::new();

        for b in &self.items {
            if let Some(underlay) = build_underlay_path(b) {
                isolated_subpaths.extend(underlay.subpaths);
            }
            let p = b.rebuild_closed_path();
            if b.auto_merge_overlapping
                && b.line_style == BalloonLineStyle::Solid
                && !matches!(b.body, LiveBalloonBody::Uniflash { .. } | LiveBalloonBody::DoubleOutline { .. })
            {
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

/// The clean interior fill path of a live balloon group: welded bodies plus detached tails, with
/// the dots / stars / Bokeh particles removed. `Uniflash + Solid` is skipped (the needles are the
/// fill); `Uniflash + StippleDots` uses only the inner ellipse so the fill never spills past the
/// needle rays. Used when baking a group to SAI linework so the backing fill layer is correct even
/// in Gloom mode, where the on-screen fill lives inside the stipple pass.
pub fn rebuild_group_fill_paths(group: &LiveBalloonGroup) -> Path {
    let mut subpaths = Vec::new();
    for b in &group.items {
        let is_uniflash = matches!(b.body, LiveBalloonBody::Uniflash { .. });
        if is_uniflash && b.line_style == BalloonLineStyle::Solid {
            continue;
        }
        if is_uniflash && b.line_style == BalloonLineStyle::StippleDots {
            if let Some(sp) = uniflash_inner_fill_subpath(b) {
                subpaths.push(sp);
            }
            continue;
        }
        let (welded, detached) = build_welded_contour_for_sampling(b);
        for mut sp in welded.subpaths.into_iter().chain(detached) {
            sp.op = PathOp::Combine;
            subpaths.push(sp);
        }
    }
    Path::new(subpaths)
}

/// Splits a [`LiveBalloonGroup`] into single-pass `standard_path` and two-pass
/// `(fill_only_path, ink_from_stroke_path)` for `CompiledShape` rendering (TZ v6.0 §2.3).
pub fn rebuild_group_render_paths(group: &LiveBalloonGroup) -> BalloonGroupRenderPaths {
    let mut merged_standard: Option<Path> = None;
    let mut isolated_standard = Vec::new();
    let mut out = BalloonGroupRenderPaths::default();
    let mut uniflash_stylized = false;

    for b in &group.items {
        if let Some(underlay) = build_underlay_path(b) {
            isolated_standard.extend(underlay.subpaths);
        }

        if matches!(b.body, LiveBalloonBody::DoubleOutline { .. }) {
            isolated_standard.extend(b.rebuild_closed_path().subpaths);
            continue;
        }

        match b.line_style {
            BalloonLineStyle::Solid => {
                let p = b.rebuild_closed_path();
                if b.auto_merge_overlapping && !matches!(b.body, LiveBalloonBody::Uniflash { .. }) {
                    merged_standard = Some(match merged_standard {
                        None => p,
                        Some(acc) => merge_paths_union(&acc, &p),
                    });
                } else {
                    isolated_standard.extend(p.subpaths);
                }
            }
            BalloonLineStyle::Double => {
                isolated_standard.extend(b.rebuild_closed_path().subpaths);
            }
            BalloonLineStyle::Pixel1Bit
            | BalloonLineStyle::Whisper
            | BalloonLineStyle::Pencil
            | BalloonLineStyle::UkiyoBrush
            | BalloonLineStyle::StippleDots
            | BalloonLineStyle::SparkleGlitter => {
                let is_uniflash = matches!(b.body, LiveBalloonBody::Uniflash { .. });
                if is_uniflash {
                    uniflash_stylized = true;
                }
                let (welded_base, detached) = build_welded_contour_for_sampling(b);
                let uniflash_dots = is_uniflash && b.line_style == BalloonLineStyle::StippleDots;
                if uniflash_dots {
                    if !b.stipple_fill_interior
                        && let Some(sp) = uniflash_inner_fill_subpath(b)
                    {
                        out.fill_only_path.subpaths.push(sp);
                    }
                } else if !b.stipple_fill_interior {
                    for sp in &welded_base.subpaths {
                        let mut fill_sp = sp.clone();
                        fill_sp.op = PathOp::Combine;
                        out.fill_only_path.subpaths.push(fill_sp);
                    }
                    for sp in &detached {
                        let mut fill_sp = sp.clone();
                        fill_sp.op = PathOp::Combine;
                        out.fill_only_path.subpaths.push(fill_sp);
                    }
                }
                if is_uniflash
                    && b.line_style != BalloonLineStyle::Pixel1Bit
                    && let Some(needles_path) = build_uniflash_paths(b)
                {
                    for mut sp in needles_path.subpaths {
                        sp.op = PathOp::Combine;
                        out.ink_from_stroke_path.subpaths.push(sp);
                    }
                }
                match b.line_style {
                    BalloonLineStyle::Pixel1Bit => {
                        // Never add these integer rectangles to `standard_path`: the smoothed
                        // `ShapeLayer::stroke` would paint a grey antialiasing fringe over them.
                        out.ink_from_stroke_path.subpaths.extend(apply_pixel_1bit_line_style(b, welded_base.clone(), detached.clone()).subpaths);
                    }
                    BalloonLineStyle::Whisper => out.ink_from_stroke_path.subpaths.extend(apply_whisper_line_style(b, welded_base, detached).subpaths),
                    BalloonLineStyle::Pencil => out.ink_from_stroke_path.subpaths.extend(build_pencil_ink_subpaths(b, &welded_base, &detached)),
                    BalloonLineStyle::UkiyoBrush => out.ink_from_stroke_path.subpaths.extend(build_ukiyo_ink_subpaths(b, &welded_base, &detached)),
                    BalloonLineStyle::StippleDots => {
                        if uniflash_dots {
                            for (_, center, radius) in compute_uniflash_stipple_dots(b) {
                                let mut sp = Subpath::polygon(&circle_contour((center[0], center[1]), radius, 8));
                                sp.op = PathOp::Combine;
                                out.ink_from_stroke_path.subpaths.push(sp);
                            }
                        } else {
                            out.ink_from_stroke_path.subpaths.extend(apply_stipple_dots_line_style(b, welded_base, detached).subpaths);
                        }
                    }
                    BalloonLineStyle::SparkleGlitter => out.ink_from_stroke_path.subpaths.extend(build_sparkle_star_subpaths(b, &welded_base, &detached)),
                    BalloonLineStyle::Solid | BalloonLineStyle::Double => {}
                }
            }
        }
    }

    out.standard_path = merged_standard.unwrap_or_default();
    out.standard_path.subpaths.extend(isolated_standard);
    out.uniflash_stylized = uniflash_stylized;
    out
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

    let subpaths = result_polys.into_iter().map(|c| strip_self_intersections(&c)).filter(|c| c.len() >= 3).map(|c| Subpath::polygon(&c)).collect();
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

/// Parses an SVG path `d` string (or `<svg><path d="..."/></svg>`) containing `M`/`L`/`Z`
/// commands into a normalized `Path` scaled into `target_rect`.
pub fn parse_simple_svg_contour(svg_or_d: &str, target_rect: [f64; 4]) -> Result<Path, String> {
    let d_str = if let Some(idx) = svg_or_d.find("d=\"") {
        let rest = &svg_or_d[idx + 3..];
        let end = rest.find('"').ok_or_else(|| "unclosed `d` attribute in SVG".to_string())?;
        &rest[..end]
    } else {
        svg_or_d
    };

    let mut nums: Vec<f64> = Vec::new();
    let mut cur = String::new();
    for ch in d_str.chars() {
        if ch.is_ascii_digit() || ch == '.' || ch == '-' || ch == '+' {
            cur.push(ch);
        } else if !cur.is_empty() {
            let val: f64 = cur.parse().map_err(|_| format!("invalid number `{cur}` in SVG path"))?;
            if !val.is_finite() {
                return Err("SVG coordinates must be finite".into());
            }
            nums.push(val);
            cur.clear();
        }
    }
    if !cur.is_empty() {
        let val: f64 = cur.parse().map_err(|_| format!("invalid number `{cur}` in SVG path"))?;
        if !val.is_finite() {
            return Err("SVG coordinates must be finite".into());
        }
        nums.push(val);
    }
    if nums.len() < 6 || !nums.len().is_multiple_of(2) {
        return Err("SVG contour needs at least 3 [x, y] vertices".into());
    }

    let raw_pts: Vec<P> = nums.chunks_exact(2).map(|c| (c[0], c[1])).collect();
    let cleaned = clean(&raw_pts);
    if cleaned.len() < 3 {
        return Err("SVG contour collapses to fewer than 3 distinct vertices".into());
    }

    let n = cleaned.len();
    for i in 0..n {
        for j in (i + 1)..n {
            let adjacent = j == i + 1 || (i == 0 && j == n - 1);
            if adjacent {
                continue;
            }
            if crossing_param(cleaned[i], cleaned[(i + 1) % n], cleaned[j], cleaned[(j + 1) % n]).is_some() {
                return Err(format!("SVG contour self-intersects between edges {i} and {j}"));
            }
        }
    }

    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for &(x, y) in &cleaned {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    let src_w = (max_x - min_x).max(1e-6);
    let src_h = (max_y - min_y).max(1e-6);
    let [tx, ty, tw, th] = target_rect;
    let mapped: Vec<P> = cleaned.into_iter().map(|(x, y)| (tx + ((x - min_x) / src_w) * tw, ty + ((y - min_y) / src_h) * th)).collect();

    Ok(Path::new(vec![Subpath::polygon(&mapped)]))
}

/// Exports one `LiveBalloon` to a standalone SVG string, optionally embedding the fitted dialogue
/// lines inside its safe text area.
pub fn export_balloon_svg(balloon: &LiveBalloon, dialogue_lines: Option<&[String]>, size_pt: f32) -> String {
    use photocraft_doc::vector::{BalloonTextAlign, TextDirection};

    let path = balloon.rebuild_closed_path();
    let r = balloon.body.rect();
    let pad = 72.0;
    let vx = (r[0] - pad).floor();
    let vy = (r[1] - pad).floor();
    let vw = (r[2].abs() + pad * 2.0).ceil().max(64.0);
    let vh = (r[3].abs() + pad * 2.0).ceil().max(64.0);

    let mut d = String::new();
    for sp in &path.subpaths {
        if sp.knots.is_empty() {
            continue;
        }
        let n = sp.knots.len();
        d.push_str(&format!("M {:.2} {:.2} ", sp.knots[0].anchor.x, sp.knots[0].anchor.y));
        let segs = if sp.closed { n } else { n.saturating_sub(1) };
        for i in 0..segs {
            let k0 = &sp.knots[i];
            let k1 = &sp.knots[(i + 1) % n];
            let curved = (k0.out_ctrl.x - k0.anchor.x).hypot(k0.out_ctrl.y - k0.anchor.y) > 0.05
                || (k1.in_ctrl.x - k1.anchor.x).hypot(k1.in_ctrl.y - k1.anchor.y) > 0.05;
            if curved {
                d.push_str(&format!(
                    "C {:.2} {:.2}, {:.2} {:.2}, {:.2} {:.2} ",
                    k0.out_ctrl.x, k0.out_ctrl.y, k1.in_ctrl.x, k1.in_ctrl.y, k1.anchor.x, k1.anchor.y
                ));
            } else {
                d.push_str(&format!("L {:.2} {:.2} ", k1.anchor.x, k1.anchor.y));
            }
        }
        if sp.closed {
            d.push_str("Z ");
        }
    }

    let mut text_svg = String::new();
    if let Some(lines) = dialogue_lines
        && !lines.is_empty()
    {
        let safe = balloon.safe_text_rect();
        let line_h = f64::from(size_pt.max(6.0)) * 1.25;
        let anchor_attr = match balloon.text_align {
            BalloonTextAlign::Center | BalloonTextAlign::Justify => "middle",
            BalloonTextAlign::Left => "start",
        };
        let tx = match balloon.text_align {
            BalloonTextAlign::Center | BalloonTextAlign::Justify => safe[0] + safe[2] * 0.5,
            BalloonTextAlign::Left => safe[0] + 4.0,
        };
        let total_h = (lines.len() as f64) * line_h;
        let start_y = safe[1] + (safe[3] - total_h) * 0.5 + f64::from(size_pt) * 0.85;
        let writing_mode = match balloon.text_direction {
            TextDirection::Horizontal => "",
            TextDirection::Vertical => " writing-mode=\"vertical-rl\"",
        };
        for (idx, line) in lines.iter().enumerate() {
            let escaped = line.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            let y = start_y + (idx as f64) * line_h;
            text_svg.push_str(&format!(
                "<text x=\"{tx:.1}\" y=\"{y:.1}\" text-anchor=\"{anchor_attr}\" font-family=\"sans-serif\" font-size=\"{size_pt:.1}\"{writing_mode}>{escaped}</text>"
            ));
        }
    }

    let is_uniflash = matches!(balloon.body, LiveBalloonBody::Uniflash { .. });
    let (fill, stroke, sw) = if is_uniflash { ("#000000", "none", 0.0) } else { ("#ffffff", "#000000", balloon.stroke_width) };

    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{vx:.0} {vy:.0} {vw:.0} {vh:.0}\" width=\"{vw:.0}\" height=\"{vh:.0}\"><path d=\"{}\" fill=\"{fill}\" stroke=\"{stroke}\" stroke-width=\"{sw:.2}\"/>{text_svg}</svg>",
        d.trim()
    )
}

/// A two-node, round-capped stroke that bakes into a solid filled vector dot. Both nodes are
/// corners so the curve sampler emits a single straight segment with two half-circle caps.
fn linework_dot_stroke(center: [f64; 2], radius: f64) -> LiveVectorStroke {
    LiveVectorStroke {
        nodes: vec![
            SaiPoint { p: (center[0] - 0.02, center[1]), pressure: 1.0, density: 1.0, corner: true },
            SaiPoint { p: (center[0] + 0.02, center[1]), pressure: 1.0, density: 1.0, corner: true },
        ],
        base_width: (radius * 2.0).max(1.0),
        min_ratio: 1.0,
        base_density: 1.0,
        min_density_ratio: 0.0,
        curve_type: SaiCurveType::Normal,
        tapered: false,
        closed: false,
        tension: SAI_DEFAULT_TENSION,
    }
}

/// A 4-point star baked as two crossed, tapered three-node strokes (a horizontal and a vertical
/// glint). Tapered ends keep the points sharp while the middle carries the full ink width.
fn linework_star_strokes(p: SparkleParticle) -> Vec<LiveVectorStroke> {
    let [cx, cy] = p.center;
    let r = p.radius;
    let make = |nodes: Vec<SaiPoint>| LiveVectorStroke {
        nodes,
        base_width: (r * 0.48).max(0.1),
        min_ratio: 0.05,
        base_density: 1.0,
        min_density_ratio: 0.0,
        curve_type: SaiCurveType::Normal,
        tapered: true,
        closed: false,
        tension: SAI_DEFAULT_TENSION,
    };
    let point = |x: f64, y: f64, pressure: f32| SaiPoint { p: (x, y), pressure, density: 1.0, corner: true };
    vec![
        make(vec![point(cx - r, cy, 0.05), point(cx, cy, 1.0), point(cx + r, cy, 0.05)]),
        make(vec![point(cx, cy - r, 0.05), point(cx, cy, 1.0), point(cx, cy + r, 0.05)]),
    ]
}

/// A Bokeh particle baked as a filled circle, a closed ring or a closed hexagon stroke.
fn linework_bokeh_stroke(p: SparkleParticle, shape: BokehShape) -> LiveVectorStroke {
    let [cx, cy] = p.center;
    let r = p.radius;
    match shape {
        BokehShape::Circle => linework_dot_stroke([cx, cy], r),
        BokehShape::Ring => {
            let nodes = (0..12)
                .map(|k| {
                    let a = std::f64::consts::TAU * (k as f64) / 12.0;
                    SaiPoint { p: (cx + r * a.cos(), cy + r * a.sin()), pressure: 1.0, density: 1.0, corner: false }
                })
                .collect();
            LiveVectorStroke {
                nodes,
                base_width: (r * 0.28).max(1.2),
                min_ratio: 1.0,
                base_density: 1.0,
                min_density_ratio: 0.0,
                curve_type: SaiCurveType::Normal,
                tapered: false,
                closed: true,
                tension: SAI_DEFAULT_TENSION,
            }
        }
        BokehShape::Hexagon => {
            let nodes = (0..6)
                .map(|k| {
                    let a = std::f64::consts::TAU * (k as f64) / 6.0;
                    SaiPoint { p: (cx + r * a.cos(), cy + r * a.sin()), pressure: 1.0, density: 1.0, corner: true }
                })
                .collect();
            LiveVectorStroke {
                nodes,
                base_width: (r * 0.55).max(1.4),
                min_ratio: 1.0,
                base_density: 1.0,
                min_density_ratio: 0.0,
                curve_type: SaiCurveType::Normal,
                tapered: false,
                closed: true,
                tension: SAI_DEFAULT_TENSION,
            }
        }
    }
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
        // `Uniflash + StippleDots`: the needles and tails stay as ink, and the dots are baked as
        // individual filled strokes so each one remains selectable in SAI.
        if b.line_style == BalloonLineStyle::StippleDots {
            strokes.extend(compute_uniflash_stipple_dots(b).into_iter().map(|(_, center, radius)| linework_dot_stroke(center, radius)));
        }
        return strokes;
    }

    // Stipple dots and sparkle / Bokeh particles are filled shapes. Sending them through
    // `rebuild_closed_path()` would turn every dot/star into a hollow stroked outline; bake the
    // actual filled particles instead. The body fill lives on the separate backing fill layer.
    match b.line_style {
        BalloonLineStyle::StippleDots => {
            return compute_stipple_dots(b).into_iter().map(|(_, center, radius)| linework_dot_stroke(center, radius)).collect();
        }
        BalloonLineStyle::SparkleGlitter => {
            let (stars, bokeh) = sparkle_glitter_particles(b);
            let mut strokes = Vec::with_capacity(stars.len() * 2 + bokeh.len());
            for p in stars {
                strokes.extend(linework_star_strokes(p));
            }
            for p in bokeh {
                strokes.push(linework_bokeh_stroke(p, b.bokeh_shape));
            }
            return strokes;
        }
        _ => {}
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
                    cos_a < 0.94 // a bend of more than about 20°: keep burst/spiky tips as corners
                } else {
                    false
                };

                let dist = (curr.x - pts[last_idx].x).hypot(curr.y - pts[last_idx].y);
                if is_sharp || dist >= 10.0 {
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
                min_ratio: 1.0,
                base_density: 1.0,
                min_density_ratio: 0.0,
                curve_type: SaiCurveType::Normal,
                // A baked balloon outline keeps the real stroke width everywhere; pressure
                // tapering would thin the tips and change the balloon's appearance.
                tapered: false,
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
            min_ratio: 1.0,
            base_density: 1.0,
            min_density_ratio: 0.0,
            curve_type: SaiCurveType::Normal,
            tapered: false,
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

    fn is_simple_polygon(poly: &[P]) -> bool {
        let n = poly.len();
        if n < 3 {
            return false;
        }
        for i in 0..n {
            for j in (i + 1)..n {
                if (i + 1) % n == j || (j + 1) % n == i {
                    continue;
                }
                if segments_cross(poly[i], poly[(i + 1) % n], poly[j], poly[(j + 1) % n]) {
                    return false;
                }
            }
        }
        true
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
                    ..LiveBalloon::default()
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
    fn rect_and_chamfer_tails_near_edges_and_corners_do_not_loop_backward() {
        let rect = [100.0, 100.0, 2400.0, 1600.0];
        for body in [
            LiveBalloonBody::Rect { rect, corner_radius: 12.0 },
            LiveBalloonBody::ChamferRect { rect, chamfer: 24.0, concave: 0.3 },
        ] {
            for perimeter_t in [0.50, 0.56, 0.62] {
                for tip in [
                    [rect[0] + rect[2] * 0.25, rect[1] + rect[3] + 48.0], // straight out
                    [rect[0] + rect[2] * 0.95, rect[1] + rect[3] + 12.0], // shallow + sideways near a corner
                    [rect[0] + rect[2] + 8.0, rect[1] + rect[3] * 0.98],  // beside the corner
                ] {
                    for preset in [TailPreset::Wedge, TailPreset::Arc] {
                        let balloon = LiveBalloon {
                            id: 1,
                            body: body.clone(),
                            tails: vec![LiveBalloonTail {
                                id: 1,
                                preset,
                                perimeter_t,
                                mouth_width: 640.0, // deliberately wider than the wall half-length
                                tip,
                                bend: 0.85,
                                bubbles: 3,
                                bubble_radius: 10.0,
                            }],
                            stroke_width: 2.5,
                            ..LiveBalloon::default()
                        };
                        let path = build_single_tail_path(&balloon, &balloon.tails[0]).0.expect("tail path");
                        let polys = closed_contours(&path);
                        assert_eq!(polys.len(), 1, "one closed tail polygon for {preset:?} t={perimeter_t} tip={tip:?}");
                        assert!(is_simple_polygon(&polys[0]), "tail must not self-intersect: {preset:?} t={perimeter_t} tip={tip:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn arc_tail_short_sideways_tip_keeps_handles_ordered() {
        let rect = [100.0, 100.0, 800.0, 600.0];
        let balloon = LiveBalloon {
            id: 1,
            body: LiveBalloonBody::Rect { rect, corner_radius: 0.0 },
            tails: vec![LiveBalloonTail {
                id: 1,
                preset: TailPreset::Arc,
                perimeter_t: 0.54,
                mouth_width: 60.0,
                tip: [rect[0] + rect[2] * 0.98, rect[1] + rect[3] + 6.0],
                bend: 0.8,
                bubbles: 3,
                bubble_radius: 10.0,
            }],
            stroke_width: 2.5,
            ..LiveBalloon::default()
        };
        let path = build_single_tail_path(&balloon, &balloon.tails[0]).0.expect("tail path");
        let knots = &path.subpaths[0].knots;
        assert_eq!(knots.len(), 3);
        let (base_l, base_r, tip) = (knots[0].anchor, knots[2].anchor, knots[1].anchor);
        let (c_l, c_r) = (knots[0].out_ctrl, knots[2].in_ctrl);
        let axis = (base_r.x - base_l.x, base_r.y - base_l.y);
        let axis_len = (axis.0 * axis.0 + axis.1 * axis.1).sqrt().max(1e-6);
        let proj = |p: Point| ((p.x - base_l.x) * axis.0 + (p.y - base_l.y) * axis.1) / axis_len;
        assert!(proj(c_l) < proj(c_r), "Arc controls crossed (S-pinch): c_l={c_l:?} c_r={c_r:?}");
        let forward = |p: Point| (p.x - base_l.x) * (tip.x - base_l.x) + (p.y - base_l.y) * (tip.y - base_l.y) >= -1e-6;
        assert!(forward(c_l) && forward(c_r), "Arc controls folded behind the base");
    }

    #[test]
    fn uniflash_explicit_tail_carves_radial_needles() {
        let rect = [0.0, 0.0, 400.0, 400.0];
        let body = || LiveBalloonBody::Uniflash {
            rect,
            mode: UniflashMode::InwardNeedles,
            needles: 160,
            inner_ratio: 0.62,
            bundle_group: 0.0,
            length_jitter: 0.0,
            angle_jitter: 0.0,
            needle_width: 4.0,
            outer_sharpness: 1.0,
            inner_sharpness: 1.0,
            seed: 777,
        };
        let plain = LiveBalloon { id: 1, body: body(), tails: Vec::new(), stroke_width: 4.0, ..LiveBalloon::default() };
        assert_eq!(uniflash_needles(&plain).map_or(0, |v| v.len()), 160);

        let tail = LiveBalloonTail {
            id: 1,
            preset: TailPreset::Wedge,
            perimeter_t: 0.0,
            mouth_width: 90.0,
            tip: [200.0, -80.0],
            bend: 0.0,
            bubbles: 0,
            bubble_radius: 10.0,
        };
        let with_tail = LiveBalloon { id: 1, body: body(), tails: vec![tail.clone()], stroke_width: 4.0, ..LiveBalloon::default() };
        let remaining = uniflash_needles(&with_tail).map_or(0, |v| v.len());
        assert!(remaining < 160, "needles inside the tail cone were omitted, got {remaining}");
        assert_eq!(needle_intersects_tail(&with_tail, &tail), 0, "no needle crosses the tail cone");
        assert!(balloon_to_linework_strokes(&with_tail).len() < 162, "linework bake also omits the carved needles");
    }

    #[test]
    fn tail_base_inset_scales_proportionally_on_large_balloons() {
        let small = LiveBalloon { id: 1, body: LiveBalloonBody::Rect { rect: [0.0, 0.0, 200.0, 140.0], corner_radius: 0.0 }, ..LiveBalloon::default() };
        let large = LiveBalloon { id: 1, body: LiveBalloonBody::Rect { rect: [0.0, 0.0, 4000.0, 4000.0], corner_radius: 0.0 }, ..LiveBalloon::default() };
        assert!(tail_base_inset(&large) > 32.0, "large tail inset must exceed the old 32 px cap");
        assert!(tail_base_inset(&large) > tail_base_inset(&small) * 10.0);
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
            ..LiveBalloon::default()
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
            ..LiveBalloon::default()
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
            ..LiveBalloon::default()
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
                mode: UniflashMode::InwardNeedles,
                seed: 777,
            },
            tails: Vec::new(),
            next_tail_id: 2,
            vertex_overrides: Vec::new(),
            hand_drawn_noise: 0.0,
            stroke_width: 2.5,
            auto_merge_overlapping: false,
            ..LiveBalloon::default()
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
            ..LiveBalloon::default()
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
            ..LiveBalloon::default()
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
            ..LiveBalloon::default()
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
            ..LiveBalloon::default()
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
        assert_eq!(OVAL_BULGES, 8, "the oval exposes the four cardinal points and four quadrant midpoints");
        assert_eq!(handles.len(), OVAL_BULGES as usize, "the oval exposes its bulge handles");
        for ((_, pos), knot) in handles.iter().zip(&bulged_path.subpaths[0].knots) {
            assert!((pos[0] - knot.anchor.x).abs() < 1e-6 && (pos[1] - knot.anchor.y).abs() < 1e-6, "a handle sits on its own knot");
        }
        assert!(crate::flatten::validate_shape(&bulged_path, None).is_ok());

        // Dragging a quadrant midpoint (not just a cardinal anchor) also bends the spline, and
        // only that bulge moves.
        let mid_dragged = LiveBalloon { vertex_overrides: vec![VertexOverride { index: 1, offset: [14.0, 9.0] }], ..plain.clone() };
        let mid_path = mid_dragged.rebuild_closed_path();
        let mid_handles = mid_dragged.vertex_handle_positions();
        assert_eq!(mid_handles.len(), 8);
        assert!((mid_handles[1].1[0] - (handles[1].1[0] + 14.0)).abs() < 1e-6);
        assert!((mid_handles[2].1[0] - handles[2].1[0]).abs() < 1e-6);
        assert!(crate::flatten::validate_shape(&mid_path, None).is_ok());
    }

    /// A rectangle exposes its four theoretical corners, and dragging one deforms the contour
    /// both with square corners and with a non-zero corner radius.
    #[test]
    fn rect_four_corner_handles_deform_the_rounded_contour() {
        use photocraft_doc::vector::VertexOverride;

        let rect = [100.0, 120.0, 200.0, 140.0];
        for corner_radius in [0.0, 12.0] {
            let plain = LiveBalloon { id: 14, body: LiveBalloonBody::Rect { rect, corner_radius }, tails: Vec::new(), ..LiveBalloon::default() };
            let handles = plain.vertex_handle_positions();
            assert_eq!(handles.len(), 4, "four corner handles (radius {corner_radius})");
            assert_eq!(handles.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
            assert!((handles[0].1[0] - 100.0).abs() < 1e-6 && (handles[0].1[1] - 120.0).abs() < 1e-6);
            assert!((handles[2].1[0] - 300.0).abs() < 1e-6 && (handles[2].1[1] - 260.0).abs() < 1e-6);

            let dragged = LiveBalloon { vertex_overrides: vec![VertexOverride { index: 2, offset: [30.0, 20.0] }], ..plain.clone() };
            let dragged_handles = dragged.vertex_handle_positions();
            assert!((dragged_handles[2].1[0] - 330.0).abs() < 1e-6 && (dragged_handles[2].1[1] - 280.0).abs() < 1e-6);
            assert!((dragged_handles[1].1[0] - handles[1].1[0]).abs() < 1e-6, "other corners stay put");

            let plain_path = plain.rebuild_closed_path();
            let dragged_path = dragged.rebuild_closed_path();
            let max_x = |p: &Path| p.subpaths[0].knots.iter().map(|k| k.anchor.x).fold(f64::NEG_INFINITY, f64::max);
            assert!(max_x(&dragged_path) > max_x(&plain_path) + 25.0, "dragging the corner widens the contour (radius {corner_radius})");
            assert!(crate::flatten::validate_shape(&dragged_path, None).is_ok());
        }

        // A stale override outside 0..4 must not switch to the bespoke rounded quad.
        let stale = LiveBalloon {
            id: 15,
            body: LiveBalloonBody::Rect { rect, corner_radius: 12.0 },
            tails: Vec::new(),
            vertex_overrides: vec![VertexOverride { index: 4, offset: [30.0, 20.0] }],
            ..LiveBalloon::default()
        };
        let plain = LiveBalloon { id: 15, body: LiveBalloonBody::Rect { rect, corner_radius: 12.0 }, tails: Vec::new(), ..LiveBalloon::default() };
        assert_eq!(stale.rebuild_closed_path().subpaths[0].knots.len(), plain.rebuild_closed_path().subpaths[0].knots.len());
    }

    /// ChamferRect exposes 8 bevel endpoints and 4 bend handles; both kinds deform the contour.
    #[test]
    fn chamfer_rect_twelve_handles_deform_endpoints_and_bends() {
        use photocraft_doc::vector::VertexOverride;

        let mut base = LiveBalloon {
            id: 16,
            body: LiveBalloonBody::ChamferRect { rect: [100.0, 120.0, 200.0, 140.0], chamfer: 20.0, concave: 0.5 },
            tails: Vec::new(),
            ..LiveBalloon::default()
        };
        let before = base.vertex_handle_positions();
        assert_eq!(before.len(), 12, "8 endpoints + 4 bevel bends");
        assert_eq!(before.iter().map(|(i, _)| *i).collect::<Vec<_>>(), (0..12).collect::<Vec<_>>());
        let path_before = base.rebuild_closed_path();
        assert_eq!(path_before.subpaths[0].knots.len(), 8);

        base.vertex_overrides.push(VertexOverride { index: 1, offset: [14.0, -11.0] });
        let after_endpoint = base.vertex_handle_positions();
        assert!((after_endpoint[1].1[0] - (before[1].1[0] + 14.0)).abs() < 1e-6);
        let path_endpoint = base.rebuild_closed_path();
        assert!((path_endpoint.subpaths[0].knots[1].anchor.x - (path_before.subpaths[0].knots[1].anchor.x + 14.0)).abs() < 1e-6);

        base.vertex_overrides.push(VertexOverride { index: 8, offset: [0.0, -18.0] });
        let after_bend = base.vertex_handle_positions();
        assert!((after_bend[8].1[1] - (after_endpoint[8].1[1] - 18.0)).abs() < 1e-6);
        let path_bend = base.rebuild_closed_path();
        // The bevel's control points carry the bend; a dragged bend must reshape the curve
        // between the two unchanged endpoints.
        assert!(
            (path_bend.subpaths[0].knots[0].out_ctrl.y - path_endpoint.subpaths[0].knots[0].out_ctrl.y).abs() > 5.0,
            "the bend handle reshapes its corner bevel"
        );
        assert!(crate::flatten::validate_shape(&path_bend, None).is_ok());

        // A flat chamfer can be bent too (the handle is live even with no scoop).
        let flat = LiveBalloon {
            id: 17,
            body: LiveBalloonBody::ChamferRect { rect: [100.0, 120.0, 200.0, 140.0], chamfer: 20.0, concave: 0.0 },
            tails: Vec::new(),
            vertex_overrides: vec![VertexOverride { index: 9, offset: [12.0, 10.0] }],
            ..LiveBalloon::default()
        };
        let flat_path = flat.rebuild_closed_path();
        assert_eq!(flat_path.subpaths[0].knots.len(), 8, "a dragged bend makes the flat bevel a curve");
        assert!(crate::flatten::validate_shape(&flat_path, None).is_ok());
    }

    /// A burst exposes `2 * rays` handles: the ray tips first, then the inner valleys, and a
    /// valley drag moves only the polygon's valley knot.
    #[test]
    fn burst_exposes_tip_and_valley_handles_and_a_valley_drag_moves_the_contour() {
        use photocraft_doc::vector::VertexOverride;

        let rays = 12;
        let plain = LiveBalloon {
            id: 18,
            body: LiveBalloonBody::Burst { rect: [100.0, 120.0, 200.0, 140.0], rays, length: 0.6, depth: 0.26, jitter: 0.35, seed: 303 },
            tails: Vec::new(),
            ..LiveBalloon::default()
        };
        let handles = plain.vertex_handle_positions();
        assert_eq!(handles.len(), (2 * rays) as usize);
        assert_eq!(handles.iter().map(|(i, _)| *i).collect::<Vec<_>>(), (0..2 * rays).collect::<Vec<_>>());
        let path_before = plain.rebuild_closed_path();
        assert_eq!(path_before.subpaths[0].knots.len(), (2 * rays) as usize);
        // The polygon interleaves tip/valley, while the handles list the tips first; handle `i`
        // is knot `2*i` and handle `n+i` is knot `2*i+1`.
        for (i, pos) in handles.iter().skip(rays as usize) {
            let ray = *i - rays;
            let knot = &path_before.subpaths[0].knots[(2 * ray + 1) as usize];
            assert!((pos[0] - knot.anchor.x).abs() < 1e-6 && (pos[1] - knot.anchor.y).abs() < 1e-6, "valley handle sits on its knot");
        }

        let ray = 0u32;
        let valley_handle = rays + ray; // handle index of ray 0's valley
        let dragged = LiveBalloon { vertex_overrides: vec![VertexOverride { index: valley_handle, offset: [18.0, -14.0] }], ..plain.clone() };
        let path_after = dragged.rebuild_closed_path();
        let moved = &path_after.subpaths[0].knots[(2 * ray + 1) as usize];
        let orig = &path_before.subpaths[0].knots[(2 * ray + 1) as usize];
        assert!((moved.anchor.x - (orig.anchor.x + 18.0)).abs() < 1e-6);
        assert!((moved.anchor.y - (orig.anchor.y - 14.0)).abs() < 1e-6);
        let tip = &path_after.subpaths[0].knots[0];
        let tip_orig = &path_before.subpaths[0].knots[0];
        assert!((tip.anchor.x - tip_orig.anchor.x).abs() < 1e-6, "a valley drag leaves the tips alone");
        assert!(crate::flatten::validate_shape(&path_after, None).is_ok());
    }

    /// Every one of the six editable body kinds exposes handles whose indices match the contour
    /// knots they move (so a gizmo circle always sits on the geometry it drags).
    #[test]
    fn all_six_body_kinds_expose_live_vertex_handles() {
        use photocraft_doc::vector::VertexOverride;

        let mut base = LiveBalloon { id: 19, tails: Vec::new(), ..LiveBalloon::default() };
        let bodies = [
            LiveBalloonBody::Ellipse { rect: [100.0, 120.0, 200.0, 140.0], squircle: 0.25 },
            LiveBalloonBody::Rect { rect: [100.0, 120.0, 200.0, 140.0], corner_radius: 12.0 },
            LiveBalloonBody::ChamferRect { rect: [100.0, 120.0, 200.0, 140.0], chamfer: 20.0, concave: 0.4 },
            LiveBalloonBody::Cloud { rect: [100.0, 120.0, 200.0, 140.0], bumps: 10, amplitude: 0.18, cusp_bite: 0.22, jitter: 0.35, seed: 42 },
            LiveBalloonBody::Spiky {
                rect: [100.0, 120.0, 200.0, 140.0],
                spikes: 16,
                length: 0.38,
                sub_harmonic: 0.45,
                concavity: 0.65,
                jitter: 0.3,
                smooth: false,
                seed: 101,
            },
            LiveBalloonBody::Burst { rect: [100.0, 120.0, 200.0, 140.0], rays: 12, length: 0.6, depth: 0.26, jitter: 0.35, seed: 303 },
        ];
        for body in bodies {
            base.body = body;
            let handles = base.vertex_handle_positions();
            assert!(!handles.is_empty(), "every editable body exposes handles");
            assert!(handles.iter().all(|(_, p)| p[0].is_finite() && p[1].is_finite()), "handles are finite");
            for (i, p) in &handles {
                let moved = LiveBalloon { vertex_overrides: vec![VertexOverride { index: *i, offset: [9.0, -7.0] }], ..base.clone() };
                let moved_pos = moved.vertex_handle_positions().into_iter().find(|(idx, _)| idx == i).map(|(_, p)| p);
                assert_eq!(moved_pos, Some([p[0] + 9.0, p[1] - 7.0]), "a handle follows its own offset");
            }
        }
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
            ..LiveBalloon::default()
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
                ..LiveBalloon::default()
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
            ..LiveBalloon::default()
        };
        let (p, n) = balloon.point_and_normal_on_body(0.0);
        let cy = rect[1] + rect[3] * 0.5;
        let ry = rect[3] * 0.5;
        assert!(cy - p[1] < ry * 0.95, "the mouth sits inside the ray tips: {}", cy - p[1]);
        assert!(cy - p[1] > ry * 0.5, "the mouth is not the fallback radius either: {}", cy - p[1]);
        assert!(-n[1] > 0.9, "the normal points outward: {n:?}");
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
                mode: UniflashMode::InwardNeedles,
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
                ..LiveBalloon::default()
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
            ..LiveBalloon::default()
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
            ..LiveBalloon::default()
        };
        assert_eq!(balloon.rebuild_closed_path().subpaths[0].knots.len(), 4, "a stale index must not switch to a spline");
    }

    #[test]
    fn all_8_line_styles_and_stipple_overrides_follow_contour_and_tail() {
        use photocraft_doc::vector::StippleDotOverride;

        let rect = [100.0, 120.0, 240.0, 160.0];
        for style in [
            BalloonLineStyle::Solid,
            BalloonLineStyle::Double,
            BalloonLineStyle::Whisper,
            BalloonLineStyle::Pencil,
            BalloonLineStyle::Pixel1Bit,
            BalloonLineStyle::UkiyoBrush,
            BalloonLineStyle::StippleDots,
            BalloonLineStyle::SparkleGlitter,
        ] {
            let b = LiveBalloon {
                id: 30,
                body: LiveBalloonBody::Rect { rect, corner_radius: 4.0 },
                line_style: style,
                double_gap_px: 5.0,
                whisper_dash_count: 20,
                whisper_duty: 0.58,
                pencil_strands: 3,
                pencil_roughness: 0.5,
                pixel_size_px: 5.0,
                ukiyo_contrast: 0.65,
                haze_spread: 30.0,
                stipple_density: 60.0,
                stipple_count: 160,
                stipple_randomness: 45.0,
                grain_size: 45.0,
                edge_roughness: 15.0,
                stipple_fill_interior: false,
                stipple_overrides: vec![StippleDotOverride { index: 0, offset: [8.0, -5.0], radius_scale: 1.8 }],
                sparkle_count: 14,
                sparkle_size: 1.0,
                sparkle_randomness: 50.0,
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
                ..LiveBalloon::default()
            };
            let p1 = b.rebuild_closed_path();
            let p2 = b.rebuild_closed_path();
            assert_eq!(p1, p2, "deterministic rebuild for {style:?}");
            assert!(!p1.subpaths.is_empty(), "non-empty path for {style:?}");
            assert!(crate::flatten::validate_shape(&p1, None).is_ok(), "validate_shape for {style:?}");

            if style == BalloonLineStyle::StippleDots {
                let dots = b.stipple_dot_positions();
                assert!(!dots.is_empty(), "StippleDots exposes editable dot handles");
                assert!(dots.iter().any(|(_, pt, _)| pt[1] > 300.0), "StippleDots must follow the welded tail contour down toward tip y=360");
                let mut gloom = b.clone();
                gloom.stipple_fill_interior = true;
                let gloom_path = gloom.rebuild_closed_path();
                assert!(gloom_path.subpaths.len() > p1.subpaths.len(), "Gloom mode adds interior fill subpath");
            }
        }
    }

    #[test]
    fn simple_svg_import_validates_and_rejects_self_intersections() {
        let rect = [50.0, 60.0, 200.0, 120.0];
        let valid_svg = r#"<svg><path d="M 0 0 L 100 0 L 100 80 L 0 80 Z"/></svg>"#;
        let path = parse_simple_svg_contour(valid_svg, rect).expect("valid rectangle SVG");
        assert_eq!(path.subpaths.len(), 1);
        assert_eq!(path.subpaths[0].knots.len(), 4);

        let bowtie_svg = "M 0 0 L 100 100 L 100 0 L 0 100 Z";
        assert!(parse_simple_svg_contour(bowtie_svg, rect).is_err(), "self-intersecting SVG must fail");
    }

    #[test]
    fn export_balloon_svg_emits_valid_svg_with_and_without_dialogue() {
        let b = LiveBalloon::default();
        let svg_plain = export_balloon_svg(&b, None, 14.0);
        assert!(svg_plain.starts_with("<svg ") && svg_plain.ends_with("</svg>"));
        assert!(svg_plain.contains("<path d=\"M "));
        assert!(!svg_plain.contains("<text "));

        let lines = vec!["Привет!".to_string(), "Тест SVG".to_string()];
        let svg_text = export_balloon_svg(&b, Some(&lines), 14.0);
        assert!(svg_text.contains("<text "));
        assert!(svg_text.contains("Привет!"));
    }

    #[test]
    fn pencil_and_ukiyo_brush_ribbons_produce_valid_closed_strokes() {
        let rect = [100.0, 120.0, 240.0, 160.0];
        let tail = vec![LiveBalloonTail {
            id: 1,
            preset: TailPreset::Wedge,
            perimeter_t: 0.5,
            mouth_width: 30.0,
            tip: [220.0, 355.0],
            bend: 0.0,
            bubbles: 3,
            bubble_radius: 10.0,
        }];

        // Pencil: coherent spline ribbons, no PathOp::Intersect, multiple closed strands welded along tail.
        let pencil = LiveBalloon {
            id: 40,
            body: LiveBalloonBody::Ellipse { rect, squircle: 0.15 },
            line_style: BalloonLineStyle::Pencil,
            pencil_strands: 4,
            pencil_roughness: 0.55,
            stroke_width: 3.0,
            tails: tail.clone(),
            ..LiveBalloon::default()
        };
        let pencil_path = pencil.rebuild_closed_path();
        assert!(pencil_path.subpaths.len() >= 4, "expected multiple coherent pencil ribbons, got {}", pencil_path.subpaths.len());
        assert!(pencil_path.subpaths.iter().all(|s| s.closed && s.op == PathOp::Combine));
        assert!(crate::flatten::validate_shape(&pencil_path, None).is_ok());

        // UkiyoBrush: closed annulus (outer CW Combine + inner CCW Join), center stays hollow while rim is inked.
        let ukiyo = LiveBalloon {
            id: 41,
            body: LiveBalloonBody::Ellipse { rect, squircle: 0.15 },
            line_style: BalloonLineStyle::UkiyoBrush,
            ukiyo_contrast: 0.75,
            stroke_width: 6.0,
            tails: tail,
            ..LiveBalloon::default()
        };
        let ukiyo_path = ukiyo.rebuild_closed_path();
        assert!(ukiyo_path.subpaths.len() >= 2, "expected outer + inner annulus cutout, got {}", ukiyo_path.subpaths.len());
        assert_eq!(ukiyo_path.subpaths[0].op, PathOp::Combine);
        assert_eq!(ukiyo_path.subpaths[1].op, PathOp::Join);
        assert!(crate::flatten::validate_shape(&ukiyo_path, None).is_ok());

        let raster = crate::fill_rasterizer(&ukiyo_path, 0.1);
        let center = raster.render(photocraft_geom::Rect::new(219, 199, 221, 201));
        assert!(center.iter().all(|c| *c < 0.05), "ukiyo brush interior cutout must remain transparent: {center:?}");
        // Tail tip reaches down to y ~ 355: ink must reach well below rect bottom y = 280.
        let max_y = ukiyo_path.subpaths[0].knots.iter().map(|k| k.anchor.y).fold(f64::NEG_INFINITY, f64::max);
        assert!(max_y > 340.0, "ukiyo annulus must trace welded tail tip near y=355, got max_y={max_y}");

        // Verify two-pass split preserves white body fill separately from ink overlay.
        let group = LiveBalloonGroup { items: vec![pencil, ukiyo], ..LiveBalloonGroup::default() };
        let split = rebuild_group_render_paths(&group);
        assert!(!split.fill_only_path.subpaths.is_empty(), "two-pass fill_only_path holds white balloon interiors");
        assert!(split.ink_from_stroke_path.subpaths.len() >= 6, "two-pass ink_from_stroke_path holds pencil + ukiyo ink");
    }

    /// Proper self-crossings of one closed polyline (adjacent edges are excluded).
    fn self_crossings(poly: &[P]) -> usize {
        let n = poly.len();
        let mut count = 0;
        for i in 0..n {
            if (i + 1) % n == i {
                continue;
            }
            for j in (i + 1)..n {
                if j == i || (j + 1) % n == i || (i + 1) % n == j {
                    continue;
                }
                if crossing_param(poly[i], poly[(i + 1) % n], poly[j], poly[(j + 1) % n]).is_some() {
                    count += 1;
                }
            }
        }
        count
    }

    fn arc_tail() -> LiveBalloonTail {
        LiveBalloonTail {
            id: 1,
            preset: TailPreset::Arc,
            perimeter_t: 0.55,
            mouth_width: 30.0,
            tip: [220.0, 350.0],
            bend: 0.2,
            bubbles: 3,
            bubble_radius: 10.0,
        }
    }

    #[test]
    fn whisper_dashes_hug_spiky_and_burst_tips_without_self_crossings() {
        let rect = [100.0, 120.0, 240.0, 160.0];
        let bodies = [
            LiveBalloonBody::Burst { rect, rays: 14, length: 0.6, depth: 0.28, jitter: 0.4, seed: 17 },
            LiveBalloonBody::Spiky { rect, spikes: 18, length: 0.4, sub_harmonic: 0.5, concavity: 0.6, jitter: 0.3, smooth: false, seed: 23 },
        ];
        for body in bodies {
            let b = LiveBalloon {
                id: 60,
                body,
                line_style: BalloonLineStyle::Whisper,
                whisper_dash_count: 24,
                whisper_duty: 0.7,
                stroke_width: 4.0,
                tails: vec![arc_tail()],
                ..LiveBalloon::default()
            };
            let path = b.rebuild_closed_path();
            assert!(path.subpaths.len() >= 20, "expected one dash per station, got {}", path.subpaths.len());
            let mut checked = 0;
            for sp in &path.subpaths {
                let flat = flatten_subpath(sp, 0.35);
                let poly = clean(&flat.pts);
                if poly.len() >= 3 {
                    assert_eq!(self_crossings(&poly), 0, "a whisper dash cuts through itself on {:?}", b.body);
                    checked += 1;
                }
            }
            assert!(checked >= 20);
        }
    }

    #[test]
    fn burst_with_arc_tail_union_has_no_self_crossing_loops() {
        let rect = [100.0, 120.0, 240.0, 160.0];
        let b = LiveBalloon {
            id: 61,
            body: LiveBalloonBody::Burst { rect, rays: 16, length: 0.6, depth: 0.3, jitter: 0.5, seed: 909 },
            tails: vec![arc_tail()],
            stroke_width: 2.5,
            ..LiveBalloon::default()
        };
        let path = b.rebuild_closed_path();
        assert!(!path.subpaths.is_empty());
        for sp in &path.subpaths {
            let flat = flatten_subpath(sp, 0.35);
            let poly = clean(&flat.pts);
            assert!(poly.len() >= 3);
            assert_eq!(self_crossings(&poly), 0, "the burst valley / arc tail seam left a crossed loop");
        }
    }

    #[test]
    fn pixel_1bit_uses_only_whole_cell_rectangles() {
        let b = LiveBalloon {
            id: 62,
            body: LiveBalloonBody::Ellipse { rect: [100.0, 120.0, 240.0, 160.0], squircle: 0.0 },
            line_style: BalloonLineStyle::Pixel1Bit,
            pixel_size_px: 2.0,
            ..LiveBalloon::default()
        };
        let path = b.rebuild_closed_path();
        assert!(path.subpaths.len() > 8, "the outline is made of merged whole-cell runs");
        for sp in &path.subpaths {
            assert_eq!(sp.knots.len(), 4, "one integer rectangle per run");
            for k in &sp.knots {
                assert!((k.anchor.x / 2.0 - (k.anchor.x / 2.0).round()).abs() < 1e-9, "x on the cell grid");
                assert!((k.anchor.y / 2.0 - (k.anchor.y / 2.0).round()).abs() < 1e-9, "y on the cell grid");
            }
        }
    }

    #[test]
    fn stylized_uniflash_contour_stays_inside_the_flash() {
        let rect = [100.0, 120.0, 240.0, 160.0];
        let b = LiveBalloon {
            id: 63,
            body: LiveBalloonBody::Uniflash {
                rect,
                mode: UniflashMode::InwardNeedles,
                needles: 160,
                inner_ratio: 0.62,
                bundle_group: 0.4,
                length_jitter: 0.5,
                angle_jitter: 0.25,
                needle_width: 2.2,
                outer_sharpness: 1.0,
                inner_sharpness: 1.0,
                seed: 777,
            },
            line_style: BalloonLineStyle::UkiyoBrush,
            stroke_width: 6.0,
            // This measures the stylized rim density, not tail carving: a flash has no default
            // tail, and a tail would deliberately leave a needle-free gap in the rim.
            tails: Vec::new(),
            ..LiveBalloon::default()
        };
        let contour = build_uniflash_contour_for_line_style(&b);
        assert_eq!(contour.subpaths.len(), 1);
        let pts: Vec<P> = contour.subpaths[0].knots.iter().map(|k| (k.anchor.x, k.anchor.y)).collect();
        assert!(pts.len() >= 48 && pts.len() <= 160, "sampled outer rim, got {}", pts.len());
        let (cx, cy) = (rect[0] + rect[2] * 0.5, rect[1] + rect[3] * 0.5);
        for &p in &pts {
            let nx = (p.0 - cx) / (rect[2] * 0.5);
            let ny = (p.1 - cy) / (rect[3] * 0.5);
            assert!(nx.hypot(ny) <= 1.0, "ink contour escaped the body: {p:?}");
        }
        let max_edge = pts.windows(2).map(|w| dist(w[0], w[1])).fold(0.0_f64, f64::max);
        assert!(max_edge < 20.0, "notched star would make a {max_edge} px ink stroke");

        let split = rebuild_group_render_paths(&LiveBalloonGroup { items: vec![b.clone()], ..LiveBalloonGroup::default() });
        assert!(split.uniflash_stylized, "the renderer must know this is a stylized Uniflash");
        assert!(!split.fill_only_path.subpaths.is_empty(), "the interior still needs a white fill");
        assert!(!split.ink_from_stroke_path.subpaths.is_empty(), "ink must stay in the stroke pass");

        let ring_spikes = LiveBalloon {
            body: LiveBalloonBody::Uniflash {
                rect,
                mode: UniflashMode::SolidRingSpikes,
                needles: 160,
                inner_ratio: 0.62,
                bundle_group: 0.4,
                length_jitter: 0.5,
                angle_jitter: 0.25,
                needle_width: 2.2,
                outer_sharpness: 1.0,
                inner_sharpness: 1.0,
                seed: 777,
            },
            ..b
        };
        let contour = build_uniflash_contour_for_line_style(&ring_spikes);
        for sp in &contour.subpaths {
            for k in &sp.knots {
                let nx = (k.anchor.x - cx) / (rect[2] * 0.5);
                let ny = (k.anchor.y - cy) / (rect[3] * 0.5);
                assert!(nx.hypot(ny) <= 1.0, "solid-ring spikes escaped the body: {:?}", (k.anchor.x, k.anchor.y));
            }
        }
    }

    #[test]
    fn linework_bake_keeps_real_width_and_burst_corners() {
        let rect = [100.0, 120.0, 240.0, 160.0];
        let b = LiveBalloon {
            id: 64,
            body: LiveBalloonBody::Burst { rect, rays: 14, length: 0.6, depth: 0.3, jitter: 0.4, seed: 31 },
            stroke_width: 3.5,
            ..LiveBalloon::default()
        };
        let strokes = balloon_to_linework_strokes(&b);
        assert!(!strokes.is_empty());
        assert!(strokes.iter().all(|s| !s.tapered), "a baked outline keeps its real width");
        assert!(strokes.iter().all(|s| (s.base_width - 3.5).abs() < 1e-6));
        assert!(strokes.iter().any(|s| s.nodes.iter().filter(|n| n.corner).count() >= 8), "burst tips must stay corners");
        let baked = strokes.first().unwrap().rebuild_outline_path();
        assert!(crate::flatten::validate_shape(&baked, None).is_ok());
    }

    /// `Double` on a tailed ellipse must not throw a long triangular spike out of the concave
    /// tail/body seam: the miter is clipped there and the expanded ring is de-self-intersected.
    #[test]
    fn double_line_has_no_long_spike_at_the_tail_seam() {
        let b = LiveBalloon {
            id: 70,
            body: LiveBalloonBody::Ellipse { rect: [100.0, 120.0, 240.0, 160.0], squircle: 0.0 },
            tails: vec![arc_tail()],
            line_style: BalloonLineStyle::Double,
            double_gap_px: 6.0,
            ..LiveBalloon::default()
        };
        let path = b.rebuild_closed_path();
        let bbox_of = |op: PathOp| {
            let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
            for sp in path.subpaths.iter().filter(|sp| sp.op == op) {
                for k in &sp.knots {
                    b[0] = b[0].min(k.anchor.x);
                    b[1] = b[1].min(k.anchor.y);
                    b[2] = b[2].max(k.anchor.x);
                    b[3] = b[3].max(k.anchor.y);
                }
            }
            b
        };
        let inner = bbox_of(PathOp::Combine);
        let outer = bbox_of(PathOp::Intersect);
        assert!(inner[0].is_finite() && outer[0].is_finite(), "both contours exist");
        let pad = 6.0 * 1.6;
        assert!(outer[0] >= inner[0] - pad && outer[1] >= inner[1] - pad, "no left/top spike: {outer:?} vs {inner:?}");
        assert!(outer[2] <= inner[2] + pad && outer[3] <= inner[3] + pad, "no right/bottom spike: {outer:?} vs {inner:?}");
    }

    #[test]
    fn pixel_1bit_ink_rasterization_is_strictly_binary() {
        let b = LiveBalloon {
            id: 71,
            body: LiveBalloonBody::Ellipse { rect: [20.0, 20.0, 160.0, 120.0], squircle: 0.0 },
            line_style: BalloonLineStyle::Pixel1Bit,
            pixel_size_px: 3.0,
            ..LiveBalloon::default()
        };
        let (welded, detached) = build_welded_contour_for_sampling(&b);
        let ink = apply_pixel_1bit_line_style(&b, welded, detached);
        let canvas = photocraft_geom::Rect::new(0, 0, 200, 160);
        let cov = crate::path_coverage(&ink, canvas);
        assert!(cov.iter().any(|v| *v > 0.5), "there is ink");
        for v in cov {
            assert!(v < 1e-6 || (v - 1.0).abs() < 1e-6, "no antialiased grey: {v}");
        }
    }

    #[test]
    fn uniflash_needles_have_area_and_stipple_dots_hug_them() {
        let b = LiveBalloon {
            id: 72,
            body: LiveBalloonBody::Uniflash {
                rect: [100.0, 100.0, 300.0, 300.0],
                inner_ratio: 0.62,
                needles: 60,
                needle_width: 3.0,
                outer_sharpness: 1.0,
                inner_sharpness: 1.0,
                mode: UniflashMode::InwardNeedles,
                bundle_group: 0.4,
                length_jitter: 0.5,
                angle_jitter: 0.25,
                seed: 7,
            },
            line_style: BalloonLineStyle::Solid,
            ..LiveBalloon::default()
        };
        let needles = uniflash_needles(&b).unwrap();
        let solid = build_uniflash_paths(&b).unwrap();
        let cov = crate::path_coverage(&solid, photocraft_geom::Rect::new(100, 100, 300, 300));
        assert!(cov.iter().any(|v| *v > 0.5), "the solid needles have non-zero area");

        let dots = compute_uniflash_stipple_dots(&b);
        assert_eq!(dots.len() as u32, b.stipple_count.clamp(24, 2000));
        for (_, p, _) in dots {
            let nearest = needles
                .iter()
                .map(|n| {
                    let vx = n.outer.x - n.inner.x;
                    let vy = n.outer.y - n.inner.y;
                    let t = (((p[0] - n.inner.x) * vx + (p[1] - n.inner.y) * vy) / (vx * vx + vy * vy).max(1.0)).clamp(0.0, 1.0);
                    ((p[0] - (n.inner.x + vx * t)).powi(2) + (p[1] - (n.inner.y + vy * t)).powi(2)).sqrt()
                })
                .fold(f64::MAX, f64::min);
            assert!(nearest <= 16.0, "a dot drifted {nearest} px from every needle: {p:?}");
        }
    }

    fn stipple_balloon(count: u32) -> LiveBalloon {
        LiveBalloon {
            id: 1,
            body: LiveBalloonBody::Ellipse { rect: [40.0, 40.0, 300.0, 220.0], squircle: 0.2 },
            line_style: BalloonLineStyle::StippleDots,
            stipple_count: count,
            stipple_density: 100.0,
            particle_spawns: 4,
            ..LiveBalloon::default()
        }
    }

    #[test]
    fn stipple_dot_count_is_honest_at_600_and_2000() {
        for count in [600, 2000] {
            let dots = compute_stipple_dots(&stipple_balloon(count));
            assert_eq!(dots.len(), count as usize, "100% density must produce exactly {count} dots");
        }
        // The old 260-dot cap is gone: even 0% density of 2000 is 400 dots.
        let mut b = stipple_balloon(2000);
        b.stipple_density = 0.0;
        assert!(compute_stipple_dots(&b).len() > 300);
    }

    #[test]
    fn stipple_dots_are_count_invariant_with_symmetric_spawn_origins() {
        for spawns in [1u8, 2, 4] {
            let mut small = stipple_balloon(120);
            small.particle_spawns = spawns;
            let mut large = small.clone();
            large.stipple_count = 400;
            let a = compute_stipple_dots(&small);
            let b = compute_stipple_dots(&large);
            assert_eq!(a.len(), 120);
            assert_eq!(b.len(), 400);
            for (i, ((idx_a, pa, ra), (idx_b, pb, rb))) in a.iter().zip(&b).enumerate() {
                assert_eq!(idx_a, idx_b);
                assert!((pa[0] - pb[0]).abs() < 1e-9 && (pa[1] - pb[1]).abs() < 1e-9, "spawns={spawns}: dot {i} moved when the count grew: {pa:?} vs {pb:?}");
                assert!((ra - rb).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn sparkle_glitter_scales_to_400_stars_and_all_bokeh_shapes() {
        use photocraft_doc::vector::BokehShape;

        let mut b = LiveBalloon {
            id: 3,
            body: LiveBalloonBody::Ellipse { rect: [40.0, 40.0, 300.0, 220.0], squircle: 0.1 },
            line_style: BalloonLineStyle::SparkleGlitter,
            sparkle_count: 400,
            bokeh_count: 400,
            particle_spawns: 4,
            ..LiveBalloon::default()
        };
        let (stars, bokeh) = sparkle_glitter_particles(&b);
        assert_eq!(stars.len(), 400);
        assert_eq!(bokeh.len(), 400);
        assert!(stars.iter().all(|p| p.radius > 0.0 && p.center.iter().all(|v| v.is_finite())));
        assert!(bokeh.iter().all(|p| p.radius > 0.0 && p.center.iter().all(|v| v.is_finite())));

        // 0% size jitter keeps every star at the same size; 100% mixes glints and hero stars.
        b.sparkle_size_jitter = 0.0;
        let (uniform, _) = sparkle_glitter_particles(&b);
        let r0 = uniform[0].radius;
        assert!(uniform.iter().all(|p| (p.radius - r0).abs() < 1e-6), "0% jitter is uniform");
        b.sparkle_size_jitter = 100.0;
        let (jittered, _) = sparkle_glitter_particles(&b);
        let rmin = jittered.iter().map(|p| p.radius).fold(f64::MAX, f64::min);
        let rmax = jittered.iter().map(|p| p.radius).fold(0.0f64, f64::max);
        assert!(rmax > rmin * 3.0, "100% jitter mixes tiny glints with hero stars: {rmin} .. {rmax}");

        let (welded, detached) = build_welded_contour_for_sampling(&b);
        for (shape, knots) in [(BokehShape::Circle, 16), (BokehShape::Hexagon, 6), (BokehShape::Ring, 40)] {
            b.bokeh_shape = shape;
            let subs = build_sparkle_star_subpaths(&b, &welded, &detached);
            assert_eq!(subs.len(), 800, "{shape:?}: 400 stars + 400 bokeh particles");
            assert_eq!(subs[400].knots.len(), knots, "{shape:?}: bokeh outline resolution");
        }
    }

    #[test]
    fn stipple_dots_bake_as_solid_strokes_even_at_2000() {
        use crate::comic_sai_curve::LiveLinework;

        let strokes = balloon_to_linework_strokes(&stipple_balloon(2000));
        assert_eq!(strokes.len(), 2000, "all dots must be baked, nothing truncated");
        assert!(strokes.iter().all(|s| {
            s.nodes.len() == 2
                && s.nodes.iter().all(|n| n.corner && n.pressure == 1.0 && n.density == 1.0)
                && s.base_width >= 1.0
                && s.min_ratio == 1.0
                && !s.tapered
                && !s.closed
        }));
        let lw = LiveLinework::new(strokes);
        assert!(crate::flatten::validate_shape(&lw.rebuild_path(), None).is_ok(), "2000 dots stay under the knot limit");
    }

    #[test]
    fn sparkle_glitter_and_bokeh_bake_as_valid_solid_strokes() {
        use crate::comic_sai_curve::LiveLinework;
        use photocraft_doc::vector::BokehShape;

        for (shape, bokeh_nodes) in [(BokehShape::Circle, 2), (BokehShape::Hexagon, 6), (BokehShape::Ring, 12)] {
            let b = LiveBalloon {
                id: 4,
                body: LiveBalloonBody::Ellipse { rect: [40.0, 40.0, 300.0, 220.0], squircle: 0.2 },
                line_style: BalloonLineStyle::SparkleGlitter,
                sparkle_count: 400,
                bokeh_count: 45,
                particle_spawns: 4,
                bokeh_shape: shape,
                ..LiveBalloon::default()
            };
            let strokes = balloon_to_linework_strokes(&b);
            assert_eq!(strokes.len(), 400 * 2 + 45, "{shape:?}: two crossed strokes per star plus one per Bokeh");
            assert!(strokes[..800].iter().all(|s| s.nodes.len() == 3 && s.tapered && !s.closed));
            assert!(strokes[800..].iter().all(|s| s.nodes.len() == bokeh_nodes));
            let lw = LiveLinework::new(strokes);
            assert!(crate::flatten::validate_shape(&lw.rebuild_path(), None).is_ok(), "{shape:?}: baked linework is valid");
        }
    }

    #[test]
    fn stylized_uniflash_with_black_fill_and_white_stroke_keeps_black_interior() {
        use photocraft_doc::{Color, Fill, LiveShape, ShapeLayer, ShapeStroke};
        use photocraft_geom::Rect;

        let b = LiveBalloon {
            id: 5,
            body: LiveBalloonBody::Uniflash {
                rect: [100.0, 100.0, 300.0, 300.0],
                inner_ratio: 0.62,
                needles: 60,
                needle_width: 3.0,
                outer_sharpness: 1.0,
                inner_sharpness: 1.0,
                mode: UniflashMode::InwardNeedles,
                bundle_group: 0.4,
                length_jitter: 0.5,
                angle_jitter: 0.25,
                seed: 7,
            },
            line_style: BalloonLineStyle::StippleDots,
            stipple_count: 60,
            ..LiveBalloon::default()
        };
        let group = LiveBalloonGroup { items: vec![b], ..LiveBalloonGroup::default() };
        let sh = ShapeLayer {
            path: group.rebuild_group_path(),
            fill: Some(Fill::Solid(Color::BLACK)),
            stroke: Some(ShapeStroke { paint: Fill::Solid(Color::WHITE), width: 2.0, ..Default::default() }),
            live: Some(LiveShape::BalloonGroup { group }),
            ..Default::default()
        };
        let canvas = Rect::new(0, 0, 400, 400);
        let rgba = crate::CompiledShape::new(&sh, crate::DEFAULT_TOLERANCE, canvas).render_rgba(canvas);
        let center = rgba[250 * 400 + 250];
        assert!(center[0] < 0.05 && center[1] < 0.05 && center[2] < 0.05 && center[3] > 0.9, "an inverted stylized Uniflash keeps its black fill: {center:?}");
    }
}
