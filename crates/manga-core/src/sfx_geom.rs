//! Non-destructive geometry pipeline for Manga SFX lettering (`L3`, wasm-pure).
//!
//! Stages:
//! 1. Text shaping via `photocraft-text` -> vector `GlyphOutline` contours.
//! 2. Adaptive Bézier flattening + segment subdivision (when non-affine warps, arcs, or distress apply).
//! 3. `PerGlyph` local transform (including 4-corner projective FT) around each glyph's resting box.
//! 4. `Catmull-Rom` baseline arc placement & tangent alignment (`data.spline`).
//! 5. `MeshLattice` (`Bilinear` / `Bicubic` Catmull-Rom) parametric UV deformation.
//! 6. `LiquifyField` $C^2$ radial smoothstep brush displacements.
//! 7. `FreeTransform` 4-corner 3×3 homography + affine/skew/clamped keystone around rest bounds center.
//! 8. Deterministic `Distress` normal perturbation.

use std::sync::{Arc, Mutex};

use photocraft_doc::TextLayer;
use photocraft_doc::sfx::{SfxData, SfxFreeTransform, SfxGlyphTransform, SfxMeshInterpolation, SfxMeshLattice};
use photocraft_doc::text::{CharStyle, Orientation, TextRun};
use photocraft_geom::Affine;
use photocraft_text::TextEngine;
use photocraft_text::render::{PathEl, glyph_outlines};
use photocraft_vector::Polyline;

const MAX_FLATTEN_DEPTH: u8 = 7;
const MAX_CONTOUR_VERTS: usize = 4096;
const MAX_TOTAL_VERTS: usize = 120_000;
const SUBDIVIDE_STEP_PX: f64 = 4.0;
const MAX_SFX_DPI: f32 = 4800.0;
const SMOOTH_CORNER_COS: f64 = 0.9396926207859084; // cos(20 deg)

#[derive(Clone, PartialEq, Eq)]
struct FlattenedGlyphKey {
    text: String,
    font_family: String,
    font_style: String,
    postscript_name: Option<String>,
    weight: u16,
    italic: bool,
    size_pt_bits: u32,
    tracking_bits: u32,
    leading_bits: u32,
    vertical: bool,
    dpi_bits: u32,
    step_bits: u64,
    tol_bits: u64,
}

type FlattenedGlyphEntry = Arc<(Vec<(usize, Vec<Vec<ContourPt>>)>, [f64; 4])>;

static FLATTENED_CACHE: Mutex<Vec<(FlattenedGlyphKey, FlattenedGlyphEntry)>> = Mutex::new(Vec::new());

const IDENTITY_H: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

/// Sampled point and analytic tangent orientation along a Catmull-Rom baseline arc.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SplineSample {
    pub lx: f64,
    pub ly: f64,
    pub angle_rad: f64,
}

/// Prepared glyph outline in document space plus rest/warped hit boxes & 4-corner quad for `PerGlyph` gizmos.
#[derive(Clone, Debug)]
pub struct PreparedGlyph {
    pub glyph_index: usize,
    pub rest_bounds: [f64; 4],
    pub rest_center: [f64; 2],
    pub warped_bounds: [f64; 4],
    pub warped_center: [f64; 2],
    /// Baseline arc tangent angle in radians at this glyph (`0.0` when straight).
    pub arc_angle_rad: f64,
    /// Document-space 4 corners `[TL, TR, BR, BL]` of this glyph's local frame after all warps.
    pub quad_doc: [[f64; 2]; 4],
    pub polylines: Vec<Polyline>,
}

/// Complete warped & flattened geometry of an [`SfxData`] ready for rendering and 2D gizmos.
#[derive(Clone, Debug)]
pub struct PreparedSfx {
    /// Unwarped axis-aligned bounding box in document space (`origin` + layout size).
    pub rest_box: [f64; 4],
    /// Final deformed bounding box enclosing all contours before border strokes.
    pub warped_box: [f64; 4],
    /// Final document-space 4 corners `[TL, TR, BR, BL]` of `rest_box` under `free_transform`.
    pub world_corners: [[f64; 2]; 4],
    /// 3×3 projective homography mapping `rest_box` corners -> `world_corners`.
    pub homography: [f64; 9],
    /// Exact 3×3 inverse of `homography` (used by canvas gizmo to freeze `inv_h` during drags).
    pub inv_homography: [f64; 9],
    pub glyphs: Vec<PreparedGlyph>,
    pub polylines: Vec<Polyline>,
}

impl Default for PreparedSfx {
    fn default() -> Self {
        Self {
            rest_box: [0.0; 4],
            warped_box: [0.0; 4],
            world_corners: [[0.0; 2]; 4],
            homography: IDENTITY_H,
            inv_homography: IDENTITY_H,
            glyphs: Vec::new(),
            polylines: Vec::new(),
        }
    }
}

impl PreparedSfx {
    pub fn is_empty(&self) -> bool {
        self.polylines.is_empty()
    }
}

#[derive(Clone, Copy)]
struct ContourPt {
    p: [f64; 2],
    knot: bool,
}

/// Solves a 3×3 projective homography `H` (`src[i] -> dst[i]`) via Gaussian elimination with partial pivoting.
pub fn solve_homography(src: [[f64; 2]; 4], dst: [[f64; 2]; 4]) -> [f64; 9] {
    let mut a = [[0.0_f64; 9]; 8];
    for i in 0..4 {
        let sx = src[i][0];
        let sy = src[i][1];
        let dx = dst[i][0];
        let dy = dst[i][1];
        a[i * 2] = [sx, sy, 1.0, 0.0, 0.0, 0.0, -dx * sx, -dx * sy, dx];
        a[i * 2 + 1] = [0.0, 0.0, 0.0, sx, sy, 1.0, -dy * sx, -dy * sy, dy];
    }

    for col in 0..8 {
        let mut max_row = col;
        let mut max_val = a[col][col].abs();
        for (r, row) in a.iter().enumerate().skip(col + 1) {
            let v = row[col].abs();
            if v > max_val {
                max_val = v;
                max_row = r;
            }
        }
        if max_row != col {
            a.swap(col, max_row);
        }
        let pivot = a[col][col];
        if !pivot.is_finite() || pivot.abs() < 1e-10 {
            return IDENTITY_H;
        }
        for v in a[col].iter_mut().skip(col) {
            *v /= pivot;
        }
        let pivot_row = a[col];
        for (r, row) in a.iter_mut().enumerate() {
            if r == col {
                continue;
            }
            let factor = row[col];
            if factor.abs() < 1e-15 {
                continue;
            }
            for c in col..=8 {
                row[c] -= factor * pivot_row[c];
            }
        }
    }

    let h = [a[0][8], a[1][8], a[2][8], a[3][8], a[4][8], a[5][8], a[6][8], a[7][8], 1.0];
    if h.iter().all(|v| v.is_finite()) { h } else { IDENTITY_H }
}

/// Applies a 3×3 projective homography to a 2D point.
pub fn apply_homography(h: &[f64; 9], p: [f64; 2]) -> [f64; 2] {
    let w = h[6] * p[0] + h[7] * p[1] + h[8];
    let inv_w = if w.abs() < 1e-8 || !w.is_finite() { 1.0 } else { 1.0 / w };
    let x = (h[0] * p[0] + h[1] * p[1] + h[2]) * inv_w;
    let y = (h[3] * p[0] + h[4] * p[1] + h[5]) * inv_w;
    if x.is_finite() && y.is_finite() { [x, y] } else { p }
}

/// Computes the exact analytical inverse of a 3×3 matrix (returns identity if singular).
pub fn invert_matrix_3x3(m: &[f64; 9]) -> [f64; 9] {
    let [a, b, c, d, e, f, g, h, i] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if !det.is_finite() || det.abs() < 1e-10 {
        return IDENTITY_H;
    }
    let inv = 1.0 / det;
    let out = [
        (e * i - f * h) * inv,
        (c * h - b * i) * inv,
        (b * f - c * e) * inv,
        (f * g - d * i) * inv,
        (a * i - c * g) * inv,
        (c * d - a * f) * inv,
        (d * h - e * g) * inv,
        (b * g - a * h) * inv,
        (a * e - b * d) * inv,
    ];
    if out.iter().all(|v| v.is_finite()) { out } else { IDENTITY_H }
}

fn rest_corners(rb: [f64; 4]) -> [[f64; 2]; 4] {
    [[rb[0], rb[1]], [rb[2], rb[1]], [rb[2], rb[3]], [rb[0], rb[3]]]
}

fn corner_homography(rb: [f64; 4], corners: [[f64; 2]; 4]) -> [f64; 9] {
    let src = rest_corners(rb);
    let dst = [
        [src[0][0] + corners[0][0], src[0][1] + corners[0][1]],
        [src[1][0] + corners[1][0], src[1][1] + corners[1][1]],
        [src[2][0] + corners[2][0], src[2][1] + corners[2][1]],
        [src[3][0] + corners[3][0], src[3][1] + corners[3][1]],
    ];
    solve_homography(src, dst)
}

/// Evaluates the Catmull-Rom baseline spline and its analytic tangent angle at `t_norm` (`0..=1`).
pub fn sample_catmull_rom(pts: &[[f64; 2]], t_norm: f64) -> SplineSample {
    let n = pts.len();
    if n == 0 {
        return SplineSample::default();
    }
    if n == 1 {
        return SplineSample { lx: pts[0][0], ly: pts[0][1], angle_rad: 0.0 };
    }
    let segs = n - 1;
    let u = t_norm.clamp(0.0, 1.0) * segs as f64;
    let i = (u.floor() as usize).min(segs - 1);
    let t = u - i as f64;
    let p1 = pts[i];
    let p2 = pts[(i + 1).min(n - 1)];
    // Linearly reflect phantom endpoints (instead of clamping `p0 = p1`, which halves
    // endpoint speed and pinches letters toward the ends of a straight or curved arc).
    let p0 = if i == 0 {
        [2.0 * p1[0] - p2[0], 2.0 * p1[1] - p2[1]]
    } else {
        pts[i - 1]
    };
    let p3 = if i + 2 >= n {
        [2.0 * p2[0] - p1[0], 2.0 * p2[1] - p1[1]]
    } else {
        pts[i + 2]
    };
    let t2 = t * t;
    let t3 = t2 * t;

    let lx = 0.5 * ((2.0 * p1[0]) + (-p0[0] + p2[0]) * t + (2.0 * p0[0] - 5.0 * p1[0] + 4.0 * p2[0] - p3[0]) * t2 + (-p0[0] + 3.0 * p1[0] - 3.0 * p2[0] + p3[0]) * t3);
    let ly = 0.5 * ((2.0 * p1[1]) + (-p0[1] + p2[1]) * t + (2.0 * p0[1] - 5.0 * p1[1] + 4.0 * p2[1] - p3[1]) * t2 + (-p0[1] + 3.0 * p1[1] - 3.0 * p2[1] + p3[1]) * t3);
    let dtx = 0.5 * ((-p0[0] + p2[0]) + 2.0 * (2.0 * p0[0] - 5.0 * p1[0] + 4.0 * p2[0] - p3[0]) * t + 3.0 * (-p0[0] + 3.0 * p1[0] - 3.0 * p2[0] + p3[0]) * t2);
    let dty = 0.5 * ((-p0[1] + p2[1]) + 2.0 * (2.0 * p0[1] - 5.0 * p1[1] + 4.0 * p2[1] - p3[1]) * t + 3.0 * (-p0[1] + 3.0 * p1[1] - 3.0 * p2[1] + p3[1]) * t2);
    let angle_rad = if dtx.hypot(dty) > 1e-6 { dty.atan2(dtx) } else { 0.0 };
    SplineSample { lx, ly, angle_rad }
}

/// Maps a local spline control point `[lx, ly]` (relative to `rest_box` center) into document space.
pub fn spline_control_doc_pos(data: &SfxData, rest_box: [f64; 4], local_pt: [f64; 2]) -> [f64; 2] {
    let cx = (rest_box[0] + rest_box[2]) * 0.5;
    let cy = (rest_box[1] + rest_box[3]) * 0.5;
    apply_free_transform_point(&data.free_transform, rest_box, [cx + local_pt[0], cy + local_pt[1]])
}

/// Computes `(H, inv_H)` mapping `rest_box` corners to their final document-space `world_corners`.
pub fn free_transform_homography(ft: &SfxFreeTransform, rest_box: [f64; 4]) -> ([f64; 9], [f64; 9]) {
    let src = rest_corners(rest_box);
    let dst = [
        apply_free_transform_point(ft, rest_box, src[0]),
        apply_free_transform_point(ft, rest_box, src[1]),
        apply_free_transform_point(ft, rest_box, src[2]),
        apply_free_transform_point(ft, rest_box, src[3]),
    ];
    let h = solve_homography(src, dst);
    let inv_h = invert_matrix_3x3(&h);
    (h, inv_h)
}

fn push_pt(dst: &mut Vec<ContourPt>, p: [f64; 2], knot: bool) {
    if !p[0].is_finite() || !p[1].is_finite() || dst.len() >= MAX_CONTOUR_VERTS {
        return;
    }
    if let Some(last) = dst.last()
        && (last.p[0] - p[0]).hypot(last.p[1] - p[1]) < 1e-4
    {
        if knot && let Some(prev) = dst.last_mut() {
            prev.knot = true;
        }
        return;
    }
    dst.push(ContourPt { p, knot });
}

fn push_subdivided_line(dst: &mut Vec<ContourPt>, a: [f64; 2], b: [f64; 2], step: f64, end_knot: bool) {
    let dist = (b[0] - a[0]).hypot(b[1] - a[1]);
    if !dist.is_finite() || dist < 1e-4 {
        return;
    }
    if step > 0.0 && dist > step {
        let segs = ((dist / step).ceil() as usize).clamp(1, 128);
        for i in 1..segs {
            let t = i as f64 / segs as f64;
            push_pt(dst, [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t], false);
        }
    }
    push_pt(dst, b, end_knot);
}

#[allow(clippy::too_many_arguments)]
fn flatten_cubic(dst: &mut Vec<ContourPt>, p0: [f64; 2], p1: [f64; 2], p2: [f64; 2], p3: [f64; 2], tol: f64, step: f64, depth: u8, end_knot: bool) {
    if dst.len() >= MAX_CONTOUR_VERTS {
        return;
    }
    let d = point_line_dist(p1, p0, p3).max(point_line_dist(p2, p0, p3));
    if depth >= MAX_FLATTEN_DEPTH || d <= tol {
        push_subdivided_line(dst, p0, p3, step, end_knot);
        return;
    }
    let m01 = mid(p0, p1);
    let m12 = mid(p1, p2);
    let m23 = mid(p2, p3);
    let m012 = mid(m01, m12);
    let m123 = mid(m12, m23);
    let m0123 = mid(m012, m123);
    flatten_cubic(dst, p0, m01, m012, m0123, tol, step, depth + 1, false);
    flatten_cubic(dst, m0123, m123, m23, p3, tol, step, depth + 1, end_knot);
}

fn mid(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]
}

fn point_line_dist(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let len2 = dx * dx + dy * dy;
    if len2 < 1e-12 {
        return (p[0] - a[0]).hypot(p[1] - a[1]);
    }
    ((p[0] - a[0]) * dy - (p[1] - a[1]) * dx).abs() / len2.sqrt()
}

fn flatten_elements(elements: &[PathEl], subdivide_step: f64, flatten_tol: f64) -> Vec<Vec<ContourPt>> {
    let mut contours = Vec::new();
    let mut cur: Vec<ContourPt> = Vec::new();
    let mut start = [0.0_f64; 2];
    let mut pos = [0.0_f64; 2];
    let finish = |contours: &mut Vec<Vec<ContourPt>>, c: &mut Vec<ContourPt>, close_from: [f64; 2], close_to: [f64; 2]| {
        if c.len() >= 2 && subdivide_step > 0.0 {
            let d = (close_to[0] - close_from[0]).hypot(close_to[1] - close_from[1]);
            if d > subdivide_step {
                let segs = ((d / subdivide_step).ceil() as usize).clamp(1, 128);
                for i in 1..segs {
                    let t = i as f64 / segs as f64;
                    push_pt(c, [close_from[0] + (close_to[0] - close_from[0]) * t, close_from[1] + (close_to[1] - close_from[1]) * t], false);
                }
            }
        }
        if c.len() >= 3 {
            if let (Some(first), Some(last)) = (c.first(), c.last())
                && (first.p[0] - last.p[0]).hypot(first.p[1] - last.p[1]) < 1e-4
            {
                c.pop();
            }
            if c.len() >= 3 {
                contours.push(std::mem::take(c));
            }
        }
        c.clear();
    };

    for el in elements {
        match *el {
            PathEl::MoveTo(p) => {
                if !cur.is_empty() {
                    finish(&mut contours, &mut cur, pos, start);
                }
                start = p;
                pos = p;
                push_pt(&mut cur, p, true);
            }
            PathEl::LineTo(p) => {
                push_subdivided_line(&mut cur, pos, p, subdivide_step, true);
                pos = p;
            }
            PathEl::QuadTo(q, p) => {
                let c1 = [pos[0] + (2.0 / 3.0) * (q[0] - pos[0]), pos[1] + (2.0 / 3.0) * (q[1] - pos[1])];
                let c2 = [p[0] + (2.0 / 3.0) * (q[0] - p[0]), p[1] + (2.0 / 3.0) * (q[1] - p[1])];
                flatten_cubic(&mut cur, pos, c1, c2, p, flatten_tol, subdivide_step, 0, true);
                pos = p;
            }
            PathEl::CurveTo(c1, c2, p) => {
                flatten_cubic(&mut cur, pos, c1, c2, p, flatten_tol, subdivide_step, 0, true);
                pos = p;
            }
            PathEl::Close => {
                finish(&mut contours, &mut cur, pos, start);
                pos = start;
            }
        }
    }
    if !cur.is_empty() {
        finish(&mut contours, &mut cur, pos, start);
    }
    contours
}

fn contour_bounds(pts: impl Iterator<Item = [f64; 2]>) -> Option<[f64; 4]> {
    let mut b: Option<[f64; 4]> = None;
    for p in pts {
        if !p[0].is_finite() || !p[1].is_finite() {
            continue;
        }
        b = Some(match b {
            None => [p[0], p[1], p[0], p[1]],
            Some(r) => [r[0].min(p[0]), r[1].min(p[1]), r[2].max(p[0]), r[3].max(p[1])],
        });
    }
    b
}

/// Evaluates a per-glyph affine transform around `center`.
pub fn apply_glyph_transform(gt: &SfxGlyphTransform, center: [f64; 2], p: [f64; 2]) -> [f64; 2] {
    let lx = (p[0] - center[0]) * gt.scale_x;
    let ly = (p[1] - center[1]) * gt.scale_y;
    let sx = gt.skew_x_deg.to_radians().tan().clamp(-3.75, 3.75);
    let sy = gt.skew_y_deg.to_radians().tan().clamp(-3.75, 3.75);
    let kx = lx + ly * sx;
    let ky = ly + lx * sy;
    let (s, c) = gt.rotation_deg.to_radians().sin_cos();
    let rx = kx * c - ky * s;
    let ry = kx * s + ky * c;
    [center[0] + rx + gt.dx, center[1] + ry + gt.dy]
}

/// Evaluates a per-glyph transform including optional 4-corner projective homography (`gt.corners`)
/// around `rest_bounds`, without applying `gt.dx`/`gt.dy` yet (applied after arc rotation).
pub fn apply_glyph_local_warp(gt: &SfxGlyphTransform, rest_bounds: [f64; 4], p: [f64; 2]) -> [f64; 2] {
    let center = [(rest_bounds[0] + rest_bounds[2]) * 0.5, (rest_bounds[1] + rest_bounds[3]) * 0.5];
    let p_h = if gt.has_corner_warp() {
        let h = corner_homography(rest_bounds, gt.corners);
        apply_homography(&h, p)
    } else {
        p
    };
    let lx = (p_h[0] - center[0]) * gt.scale_x;
    let ly = (p_h[1] - center[1]) * gt.scale_y;
    let sx = gt.skew_x_deg.to_radians().tan().clamp(-3.75, 3.75);
    let sy = gt.skew_y_deg.to_radians().tan().clamp(-3.75, 3.75);
    let kx = lx + ly * sx;
    let ky = ly + lx * sy;
    let (s, c) = gt.rotation_deg.to_radians().sin_cos();
    let rx = kx * c - ky * s;
    let ry = kx * s + ky * c;
    [center[0] + rx, center[1] + ry]
}

fn catmull_rom_weights(t: f64) -> [f64; 4] {
    let t2 = t * t;
    let t3 = t2 * t;
    [0.5 * (-t + 2.0 * t2 - t3), 0.5 * (2.0 - 5.0 * t2 + 3.0 * t3), 0.5 * (t + 4.0 * t2 - 3.0 * t3), 0.5 * (-t2 + t3)]
}

fn sample_col_linear(mesh: &SfxMeshLattice, r: usize, c: isize) -> [f64; 2] {
    let rows = mesh.rows.max(2) as usize;
    let cols = mesh.cols.max(2) as isize;
    let get = |rr: usize, cc: usize| -> [f64; 2] {
        mesh.points.get(rr * cols as usize + cc).copied().unwrap_or([cc as f64 / (cols - 1) as f64, rr as f64 / (rows - 1) as f64])
    };
    if c < 0 {
        let p0 = get(r, 0);
        let p1 = get(r, 1);
        [p0[0] * 2.0 - p1[0], p0[1] * 2.0 - p1[1]]
    } else if c >= cols {
        let p0 = get(r, (cols - 1) as usize);
        let p1 = get(r, (cols - 2) as usize);
        [p0[0] * 2.0 - p1[0], p0[1] * 2.0 - p1[1]]
    } else {
        get(r, c as usize)
    }
}

fn sample_grid_pt(mesh: &SfxMeshLattice, r: isize, c: isize) -> [f64; 2] {
    let rows = mesh.rows.max(2) as isize;
    if r < 0 {
        let p0 = sample_col_linear(mesh, 0, c);
        let p1 = sample_col_linear(mesh, 1, c);
        [p0[0] * 2.0 - p1[0], p0[1] * 2.0 - p1[1]]
    } else if r >= rows {
        let p0 = sample_col_linear(mesh, (rows - 1) as usize, c);
        let p1 = sample_col_linear(mesh, (rows - 2) as usize, c);
        [p0[0] * 2.0 - p1[0], p0[1] * 2.0 - p1[1]]
    } else {
        sample_col_linear(mesh, r as usize, c)
    }
}

/// Evaluates deformed normalized `[u, v]` inside `mesh` for normalized rest `[u, v]` (`0..=1`).
pub fn eval_mesh_uv(mesh: &SfxMeshLattice, u: f64, v: f64) -> [f64; 2] {
    let cols = mesh.cols.max(2) as usize;
    let rows = mesh.rows.max(2) as usize;
    if mesh.points.len() != cols * rows {
        return [u, v];
    }
    let su = u.clamp(0.0, 1.0) * (cols - 1) as f64;
    let sv = v.clamp(0.0, 1.0) * (rows - 1) as f64;
    let ci = (su.floor() as usize).min(cols - 2);
    let ri = (sv.floor() as usize).min(rows - 2);
    let tu = su - ci as f64;
    let tv = sv - ri as f64;

    let out = match mesh.interpolation {
        SfxMeshInterpolation::Bilinear => {
            let p00 = sample_grid_pt(mesh, ri as isize, ci as isize);
            let p10 = sample_grid_pt(mesh, ri as isize, ci as isize + 1);
            let p01 = sample_grid_pt(mesh, ri as isize + 1, ci as isize);
            let p11 = sample_grid_pt(mesh, ri as isize + 1, ci as isize + 1);
            let top = [p00[0] + (p10[0] - p00[0]) * tu, p00[1] + (p10[1] - p00[1]) * tu];
            let bot = [p01[0] + (p11[0] - p01[0]) * tu, p01[1] + (p11[1] - p01[1]) * tu];
            [top[0] + (bot[0] - top[0]) * tv, top[1] + (bot[1] - top[1]) * tv]
        }
        SfxMeshInterpolation::Bicubic => {
            let wu = catmull_rom_weights(tu);
            let wv = catmull_rom_weights(tv);
            let mut acc = [0.0_f64; 2];
            for (dr, &wr) in wv.iter().enumerate() {
                let mut row_pt = [0.0_f64; 2];
                for (dc, &wc) in wu.iter().enumerate() {
                    let pt = sample_grid_pt(mesh, ri as isize + dr as isize - 1, ci as isize + dc as isize - 1);
                    row_pt[0] += pt[0] * wc;
                    row_pt[1] += pt[1] * wc;
                }
                acc[0] += row_pt[0] * wr;
                acc[1] += row_pt[1] * wr;
            }
            acc
        }
    };
    [out[0] + (u - u.clamp(0.0, 1.0)), out[1] + (v - v.clamp(0.0, 1.0))]
}

/// Applies a displacement delta to node `(row, col)` honoring MediBang-style `link_h` and `link_v`.
pub fn move_mesh_node_linked(mesh: &mut SfxMeshLattice, row: usize, col: usize, du: f64, dv: f64) {
    let cols = usize::from(mesh.cols.max(2));
    let rows = usize::from(mesh.rows.max(2));
    if mesh.points.len() != cols * rows || row >= rows || col >= cols {
        return;
    }
    for r in 0..rows {
        for c in 0..cols {
            let hit = (mesh.link_h || c == col) && r == row || (mesh.link_v && c == col);
            if hit {
                let p = &mut mesh.points[r * cols + c];
                p[0] = (p[0] + du).clamp(-3.0, 4.0);
                p[1] = (p[1] + dv).clamp(-3.0, 4.0);
            }
        }
    }
}

/// Applies `FreeTransform` (4-corner homography + scale, skew, rotation, clamped keystone) around `rest_box` center.
pub fn apply_free_transform_point(ft: &SfxFreeTransform, rest_box: [f64; 4], p: [f64; 2]) -> [f64; 2] {
    let p_in = if ft.has_corner_warp() {
        let h_c = corner_homography(rest_box, ft.corners);
        apply_homography(&h_c, p)
    } else {
        p
    };
    let w = (rest_box[2] - rest_box[0]).max(1.0);
    let h = (rest_box[3] - rest_box[1]).max(1.0);
    let cx = (rest_box[0] + rest_box[2]) * 0.5;
    let cy = (rest_box[1] + rest_box[3]) * 0.5;
    let dx = p_in[0] - cx;
    let dy = p_in[1] - cy;
    let nx = dx / (w * 0.5);
    let ny = dy / (h * 0.5);
    let denom = (1.0 + ft.perspective_x * nx + ft.perspective_y * ny).max(0.15);
    let px = dx / denom;
    let py = dy / denom;
    let lx = px * ft.scale_x;
    let ly = py * ft.scale_y;
    let sx = ft.skew_x_deg.to_radians().tan().clamp(-3.75, 3.75);
    let sy = ft.skew_y_deg.to_radians().tan().clamp(-3.75, 3.75);
    let kx = lx + ly * sx;
    let ky = ly + lx * sy;
    let (s, c) = ft.rotation_deg.to_radians().sin_cos();
    [cx + kx * c - ky * s, cy + kx * s + ky * c]
}

/// Exact analytical inverse of `apply_free_transform_point` (including 4-corner homography) for gizmo hit-testing.
pub fn invert_free_transform_point(ft: &SfxFreeTransform, rest_box: [f64; 4], q: [f64; 2]) -> Option<[f64; 2]> {
    let w = (rest_box[2] - rest_box[0]).max(1.0);
    let h = (rest_box[3] - rest_box[1]).max(1.0);
    let cx = (rest_box[0] + rest_box[2]) * 0.5;
    let cy = (rest_box[1] + rest_box[3]) * 0.5;
    let (s, c) = (-ft.rotation_deg).to_radians().sin_cos();
    let rx = q[0] - cx;
    let ry = q[1] - cy;
    let kx = rx * c - ry * s;
    let ky = rx * s + ry * c;
    let sx = ft.skew_x_deg.to_radians().tan().clamp(-3.75, 3.75);
    let sy = ft.skew_y_deg.to_radians().tan().clamp(-3.75, 3.75);
    let det = 1.0 - sx * sy;
    if det.abs() < 1e-6 || ft.scale_x.abs() < 1e-6 || ft.scale_y.abs() < 1e-6 {
        return None;
    }
    let px = ((kx - ky * sx) / det) / ft.scale_x;
    let py = ((ky - kx * sy) / det) / ft.scale_y;
    let denom = 1.0 - (ft.perspective_x * px / (w * 0.5)) - (ft.perspective_y * py / (h * 0.5));
    if denom.abs() < 0.05 {
        return None;
    }
    let dx = px / denom;
    let dy = py / denom;
    let p_pre = [cx + dx, cy + dy];
    if !p_pre[0].is_finite() || !p_pre[1].is_finite() {
        return None;
    }
    let out = if ft.has_corner_warp() {
        let inv_h = invert_matrix_3x3(&corner_homography(rest_box, ft.corners));
        apply_homography(&inv_h, p_pre)
    } else {
        p_pre
    };
    (out[0].is_finite() && out[1].is_finite()).then_some(out)
}

/// Full spatial warp (`MeshLattice -> LiquifyField -> FreeTransform`) for a point in `rest_box` space.
pub fn transform_point_full(data: &SfxData, rest_box: [f64; 4], mut p: [f64; 2]) -> [f64; 2] {
    let w = (rest_box[2] - rest_box[0]).max(1.0);
    let h = (rest_box[3] - rest_box[1]).max(1.0);
    if !data.mesh.is_identity() {
        let u = (p[0] - rest_box[0]) / w;
        let v = (p[1] - rest_box[1]) / h;
        let uv = eval_mesh_uv(&data.mesh, u, v);
        p = [rest_box[0] + uv[0] * w, rest_box[1] + uv[1] * h];
    }
    for brush in &data.liquify_strokes {
        let bx = rest_box[0] + brush.center_uv[0] * w;
        let by = rest_box[1] + brush.center_uv[1] * h;
        let d = (p[0] - bx).hypot(p[1] - by);
        if d < brush.radius_px && brush.radius_px > 1e-6 {
            let t = 1.0 - d / brush.radius_px;
            let weight = t * t * (3.0 - 2.0 * t) * brush.strength;
            p[0] += brush.delta_px[0] * weight;
            p[1] += brush.delta_px[1] * weight;
        }
    }
    apply_free_transform_point(&data.free_transform, rest_box, p)
}

/// Document-space center of mesh lattice node `(row, col)` after `FreeTransform`.
pub fn mesh_node_doc_pos(data: &SfxData, rest_box: [f64; 4], row: usize, col: usize) -> Option<[f64; 2]> {
    let cols = usize::from(data.mesh.cols.max(2));
    let rows = usize::from(data.mesh.rows.max(2));
    let uv = data.mesh.points.get(row.checked_mul(cols)?.checked_add(col)?)?;
    if row >= rows {
        return None;
    }
    let w = (rest_box[2] - rest_box[0]).max(1.0);
    let h = (rest_box[3] - rest_box[1]).max(1.0);
    let p = [rest_box[0] + uv[0] * w, rest_box[1] + uv[1] * h];
    Some(apply_free_transform_point(&data.free_transform, rest_box, p))
}

fn hash_unit(seed: u32, a: u32, b: u32) -> f64 {
    let mut x = seed ^ a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA77);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^= x >> 16;
    (x & 0xFFFF) as f64 / 32767.5 - 1.0
}

fn apply_distress_to_contour(pts: &mut [ContourPt], seed: u32, amp: f64, chip: f64, c_idx: usize) {
    let n = pts.len();
    if n < 3 || amp <= 1e-4 {
        return;
    }
    let orig: Vec<[f64; 2]> = pts.iter().map(|c| c.p).collect();
    for i in 0..n {
        let prev = orig[(i + n - 1) % n];
        let next = orig[(i + 1) % n];
        let tx = next[0] - prev[0];
        let ty = next[1] - prev[1];
        let len = tx.hypot(ty).max(1e-6);
        let nx = -ty / len;
        let ny = tx / len;
        let h1 = hash_unit(seed, c_idx as u32, i as u32);
        let notch = if chip > 0.05 && hash_unit(seed ^ 0x55AA_3311, c_idx as u32, i as u32) > 1.0 - chip * 0.45 { -amp * 1.4 } else { 0.0 };
        let disp = h1 * amp + notch;
        pts[i].p = [orig[i][0] + nx * disp, orig[i][1] + ny * disp];
    }
}

fn to_polyline(pts: &[ContourPt]) -> Polyline {
    let n = pts.len();
    let mut knot = Vec::with_capacity(n);
    for i in 0..n {
        if pts[i].knot {
            knot.push(true);
            continue;
        }
        let a = pts[(i + n - 1) % n].p;
        let b = pts[i].p;
        let c = pts[(i + 1) % n].p;
        let d1 = [b[0] - a[0], b[1] - a[1]];
        let d2 = [c[0] - b[0], c[1] - b[1]];
        let l1 = d1[0].hypot(d1[1]);
        let l2 = d2[0].hypot(d2[1]);
        let sharp = if l1 > 1e-6 && l2 > 1e-6 { ((d1[0] * d2[0] + d1[1] * d2[1]) / (l1 * l2)) < SMOOTH_CORNER_COS } else { false };
        knot.push(sharp);
    }
    Polyline { pts: pts.iter().map(|c| (c.p[0], c.p[1])).collect(), knot, closed: true }
}

fn build_temp_text_layer(data: &SfxData) -> TextLayer {
    let style = CharStyle {
        font_family: data.font_family.clone(),
        font_style: data.font_style.clone(),
        postscript_name: data.postscript_name.clone(),
        weight: data.weight,
        italic: data.italic,
        size_pt: data.size_pt,
        tracking: data.tracking,
        leading_pt: Some((data.size_pt * data.leading_scale).max(1.0)),
        ..CharStyle::default()
    };
    let len = data.text.len();
    TextLayer {
        text: data.text.clone(),
        font_family: data.font_family.clone(),
        size_pt: data.size_pt,
        runs: vec![TextRun { len, style }],
        orientation: if data.vertical { Orientation::Vertical } else { Orientation::Horizontal },
        ..TextLayer::default()
    }
}

/// Builds non-destructive deformed geometry and gizmo metadata from [`SfxData`].
pub fn build_sfx_geometry(data: &SfxData, engine: &mut TextEngine, dpi: f32) -> PreparedSfx {
    if data.text.is_empty() {
        return PreparedSfx::default();
    }
    let dpi_clamped = dpi.clamp(1.0, MAX_SFX_DPI);
    let em_px = f64::from(data.size_pt.max(1.0)) * f64::from(dpi_clamped) / 72.0;
    let flatten_tol = (em_px / 480.0).clamp(0.25, 6.0);

    let any_glyph_corner_warp = data.per_glyph.iter().any(SfxGlyphTransform::has_corner_warp);
    let has_spline = data.spline.points.len() >= 2;
    // NOTE: `has_spline` only translates + rotates each glyph rigidly as a whole, so it does NOT
    // require per-segment subdivision. Only non-affine per-vertex warps subdivide, and their step
    // scales proportionally with glyph height so 2400 DPI / 12K×17K lettering stays lightweight.
    let need_subdivide = !data.mesh.is_identity()
        || !data.liquify_strokes.is_empty()
        || data.free_transform.has_corner_warp()
        || any_glyph_corner_warp
        || data.free_transform.perspective_x.abs() > 1e-4
        || data.free_transform.perspective_y.abs() > 1e-4
        || (data.distress.enabled && data.distress.roughness > 1e-4);
    let step = if need_subdivide { (em_px / 28.0).clamp(SUBDIVIDE_STEP_PX, 96.0) } else { 0.0 };

    let cache_key = FlattenedGlyphKey {
        text: data.text.clone(),
        font_family: data.font_family.clone(),
        font_style: data.font_style.clone(),
        postscript_name: data.postscript_name.clone(),
        weight: data.weight,
        italic: data.italic,
        size_pt_bits: data.size_pt.to_bits(),
        tracking_bits: data.tracking.to_bits(),
        leading_bits: data.leading_scale.to_bits(),
        vertical: data.vertical,
        dpi_bits: dpi_clamped.to_bits(),
        step_bits: step.to_bits(),
        tol_bits: flatten_tol.to_bits(),
    };

    let cached_entry: Option<FlattenedGlyphEntry> = FLATTENED_CACHE
        .lock()
        .ok()
        .and_then(|g| g.iter().find(|(k, _)| *k == cache_key).map(|(_, v)| Arc::clone(v)));

    let entry = if let Some(hit) = cached_entry {
        hit
    } else {
        let layer = build_temp_text_layer(data);
        let layout = engine.layout(&layer, dpi_clamped);
        let raw_outlines = glyph_outlines(&layout, &Affine::IDENTITY, None);
        if raw_outlines.is_empty() {
            return PreparedSfx::default();
        }
        let mut raw_glyphs: Vec<(usize, Vec<Vec<ContourPt>>)> = Vec::with_capacity(raw_outlines.len());
        let mut total_verts = 0usize;
        let mut base_bounds: Option<[f64; 4]> = None;

        for go in raw_outlines {
            let mut contours = flatten_elements(&go.elements, step, flatten_tol);
            if total_verts >= MAX_TOTAL_VERTS {
                break;
            }
            contours.retain(|c| !c.is_empty());
            for c in &contours {
                total_verts += c.len();
                if let Some(cb) = contour_bounds(c.iter().map(|pt| pt.p)) {
                    base_bounds = Some(match base_bounds {
                        None => cb,
                        Some(b) => [b[0].min(cb[0]), b[1].min(cb[1]), b[2].max(cb[2]), b[3].max(cb[3])],
                    });
                }
            }
            if !contours.is_empty() {
                raw_glyphs.push((go.glyph_index, contours));
            }
        }
        let Some(base_b) = base_bounds else {
            return PreparedSfx::default();
        };
        let built: FlattenedGlyphEntry = Arc::new((raw_glyphs, base_b));
        if let Ok(mut g) = FLATTENED_CACHE.lock() {
            if g.len() >= 8 {
                g.remove(0);
            }
            g.push((cache_key, Arc::clone(&built)));
        }
        built
    };

    let (raw_glyphs_ref, base_b) = (&entry.0, entry.1);
    let raw_glyphs = raw_glyphs_ref.clone();
    let shift = [data.origin[0] - base_b[0], data.origin[1] - base_b[1]];
    let rest_box = [data.origin[0], data.origin[1], data.origin[0] + (base_b[2] - base_b[0]), data.origin[1] + (base_b[3] - base_b[1])];
    let box_cx = (rest_box[0] + rest_box[2]) * 0.5;
    let box_cy = (rest_box[1] + rest_box[3]) * 0.5;

    // Precompute rest centers so spline parameter `t_norm` spans cleanly from first glyph to last glyph.
    let mut glyph_rest_boxes: Vec<[f64; 4]> = Vec::with_capacity(raw_glyphs.len());
    for (_, contours) in &raw_glyphs {
        let rb = contour_bounds(contours.iter().flat_map(|c| c.iter().map(|pt| [pt.p[0] + shift[0], pt.p[1] + shift[1]]))).unwrap_or(rest_box);
        glyph_rest_boxes.push(rb);
    }
    let first_cx = glyph_rest_boxes.first().map_or(box_cx, |b| (b[0] + b[2]) * 0.5);
    let last_cx = glyph_rest_boxes.last().map_or(box_cx, |b| (b[0] + b[2]) * 0.5);
    let span_cx = (last_cx - first_cx).abs();

    let mut glyphs = Vec::with_capacity(raw_glyphs.len());
    let mut all_polylines = Vec::new();
    let mut warped_box: Option<[f64; 4]> = None;
    let mut contour_counter = 0usize;

    for ((g_idx, contours), g_rest_bounds) in raw_glyphs.into_iter().zip(glyph_rest_boxes) {
        let g_rest_center = [(g_rest_bounds[0] + g_rest_bounds[2]) * 0.5, (g_rest_bounds[1] + g_rest_bounds[3]) * 0.5];
        let per = data.per_glyph.iter().find(|pg| pg.glyph_index == g_idx).copied();

        let spline_sample = if has_spline {
            let t_norm = if span_cx > 1.0 { ((g_rest_center[0] - first_cx) / (last_cx - first_cx)).clamp(0.0, 1.0) } else { 0.5 };
            Some(sample_catmull_rom(&data.spline.points, t_norm))
        } else {
            None
        };
        let arc_angle_rad = spline_sample.map_or(0.0, |s| s.angle_rad);

        let map_glyph_pt = |raw_p: [f64; 2]| -> [f64; 2] {
            let mut p = raw_p;
            if let Some(pg) = &per {
                p = apply_glyph_local_warp(pg, g_rest_bounds, p);
            }
            if let Some(sp) = spline_sample {
                let (sin_a, cos_a) = sp.angle_rad.sin_cos();
                let lx = p[0] - g_rest_center[0];
                let ly = p[1] - g_rest_center[1];
                let rx = lx * cos_a - ly * sin_a;
                let ry = lx * sin_a + ly * cos_a;
                let base_y_off = g_rest_center[1] - box_cy;
                p = [box_cx + sp.lx + rx, box_cy + sp.ly + base_y_off + ry];
            }
            if let Some(pg) = &per {
                p[0] += pg.dx;
                p[1] += pg.dy;
            }
            transform_point_full(data, rest_box, p)
        };

        let mut g_polys = Vec::with_capacity(contours.len());
        let mut g_warped_bounds: Option<[f64; 4]> = None;

        for mut c in contours {
            for pt in &mut c {
                pt.p = map_glyph_pt([pt.p[0] + shift[0], pt.p[1] + shift[1]]);
            }
            if data.distress.enabled {
                apply_distress_to_contour(&mut c, data.distress.seed, f64::from(data.distress.roughness), f64::from(data.distress.edge_chip), contour_counter);
            }
            contour_counter += 1;
            if let Some(cb) = contour_bounds(c.iter().map(|pt| pt.p)) {
                g_warped_bounds = Some(match g_warped_bounds {
                    None => cb,
                    Some(b) => [b[0].min(cb[0]), b[1].min(cb[1]), b[2].max(cb[2]), b[3].max(cb[3])],
                });
                warped_box = Some(match warped_box {
                    None => cb,
                    Some(b) => [b[0].min(cb[0]), b[1].min(cb[1]), b[2].max(cb[2]), b[3].max(cb[3])],
                });
            }
            let pl = to_polyline(&c);
            g_polys.push(pl.clone());
            all_polylines.push(pl);
        }

        let g_src_quad = rest_corners(g_rest_bounds);
        let quad_doc = [
            map_glyph_pt(g_src_quad[0]),
            map_glyph_pt(g_src_quad[1]),
            map_glyph_pt(g_src_quad[2]),
            map_glyph_pt(g_src_quad[3]),
        ];
        let wb = g_warped_bounds.unwrap_or(g_rest_bounds);
        let wc = [(wb[0] + wb[2]) * 0.5, (wb[1] + wb[3]) * 0.5];
        glyphs.push(PreparedGlyph {
            glyph_index: g_idx,
            rest_bounds: g_rest_bounds,
            rest_center: g_rest_center,
            warped_bounds: wb,
            warped_center: wc,
            arc_angle_rad,
            quad_doc,
            polylines: g_polys,
        });
    }

    let (homography, inv_homography) = free_transform_homography(&data.free_transform, rest_box);
    let src_box = rest_corners(rest_box);
    let world_corners = [
        apply_free_transform_point(&data.free_transform, rest_box, src_box[0]),
        apply_free_transform_point(&data.free_transform, rest_box, src_box[1]),
        apply_free_transform_point(&data.free_transform, rest_box, src_box[2]),
        apply_free_transform_point(&data.free_transform, rest_box, src_box[3]),
    ];

    PreparedSfx {
        rest_box,
        warped_box: warped_box.unwrap_or(rest_box),
        world_corners,
        homography,
        inv_homography,
        glyphs,
        polylines: all_polylines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bicubic_and_bilinear_identity_mesh_preserve_points() {
        for interp in [SfxMeshInterpolation::Bilinear, SfxMeshInterpolation::Bicubic] {
            let mut mesh = SfxMeshLattice::identity(4, 4);
            mesh.interpolation = interp;
            for iu in 0..=10 {
                for iv in 0..=10 {
                    let u = iu as f64 / 10.0;
                    let v = iv as f64 / 10.0;
                    let out = eval_mesh_uv(&mesh, u, v);
                    assert!((out[0] - u).abs() < 1e-9 && (out[1] - v).abs() < 1e-9, "{interp:?} ({u},{v}) -> {out:?}");
                }
            }
        }
    }

    #[test]
    fn free_transform_inverse_roundtrips_with_skew_and_perspective() {
        let ft = SfxFreeTransform {
            scale_x: 1.4,
            scale_y: 0.85,
            rotation_deg: -18.0,
            skew_x_deg: 22.0,
            skew_y_deg: -9.0,
            perspective_x: 0.18,
            perspective_y: -0.12,
            ..SfxFreeTransform::default()
        };
        let rest_box = [100.0, 200.0, 340.0, 290.0];
        let p = [175.0, 240.0];
        let q = apply_free_transform_point(&ft, rest_box, p);
        let back = invert_free_transform_point(&ft, rest_box, q).unwrap();
        assert!((back[0] - p[0]).abs() < 1e-7 && (back[1] - p[1]).abs() < 1e-7, "{p:?} -> {q:?} -> {back:?}");
    }

    #[test]
    fn free_transform_4_corner_homography_roundtrips_analytically() {
        let ft = SfxFreeTransform {
            rotation_deg: -14.0,
            skew_x_deg: 10.0,
            corners: [[18.0, -12.0], [-22.0, 9.0], [30.0, 16.0], [-14.0, -8.0]],
            ..SfxFreeTransform::default()
        };
        let rest_box = [100.0, 120.0, 420.0, 260.0];
        for p in [[100.0, 120.0], [260.0, 190.0], [390.0, 245.0]] {
            let q = apply_free_transform_point(&ft, rest_box, p);
            let back = invert_free_transform_point(&ft, rest_box, q).unwrap();
            assert!((back[0] - p[0]).abs() < 1e-6 && (back[1] - p[1]).abs() < 1e-6, "{p:?} -> {q:?} -> {back:?}");
        }
        let (h, inv_h) = free_transform_homography(&ft, rest_box);
        let q = apply_homography(&h, [210.0, 175.0]);
        let back = apply_homography(&inv_h, q);
        assert!((back[0] - 210.0).abs() < 1e-6 && (back[1] - 175.0).abs() < 1e-6);
    }

    #[test]
    fn catmull_rom_spline_evaluates_endpoints_and_tangent() {
        let pts = [[-160.0, 20.0], [0.0, -30.0], [160.0, 20.0]];
        let s0 = sample_catmull_rom(&pts, 0.0);
        let sm = sample_catmull_rom(&pts, 0.5);
        let s1 = sample_catmull_rom(&pts, 1.0);
        assert!((s0.lx + 160.0).abs() < 1e-9 && (s0.ly - 20.0).abs() < 1e-9);
        assert!(sm.lx.abs() < 1e-9 && (sm.ly + 30.0).abs() < 1e-9);
        assert!(sm.angle_rad.abs() < 1e-6);
        assert!((s1.lx - 160.0).abs() < 1e-9 && (s1.ly - 20.0).abs() < 1e-9);
    }

    #[test]
    fn move_mesh_node_linked_propagates_along_row_and_column() {
        let mut mesh = SfxMeshLattice::identity(4, 4);
        mesh.link_h = true;
        mesh.link_v = false;
        move_mesh_node_linked(&mut mesh, 1, 2, 0.0, -0.1);
        for c in 0..4 {
            assert!((mesh.points[4 + c][1] - (1.0 / 3.0 - 0.1)).abs() < 1e-9);
        }
        assert!((mesh.points[2 * 4 + 2][1] - (2.0 / 3.0)).abs() < 1e-9);

        mesh.link_h = false;
        mesh.link_v = true;
        move_mesh_node_linked(&mut mesh, 1, 2, 0.08, 0.0);
        for r in 0..4 {
            assert!((mesh.points[r * 4 + 2][0] - (2.0 / 3.0 + 0.08)).abs() < 1e-9);
        }
    }

    #[test]
    fn build_sfx_geometry_produces_nonempty_warped_contours() {
        let mut engine = TextEngine::new();
        let data = SfxData { text: "BAM!".to_owned(), origin: [50.0, 80.0], ..SfxData::default() };
        let geom = build_sfx_geometry(&data, &mut engine, 72.0);
        assert!(!geom.is_empty());
        assert_eq!(geom.glyphs.len(), 4);
        assert!((geom.rest_box[0] - 50.0).abs() < 1e-4);
        assert!((geom.rest_box[1] - 80.0).abs() < 1e-4);
        assert!(geom.world_corners.iter().all(|c| c[0].is_finite() && c[1].is_finite()));
        assert!(geom.glyphs[0].quad_doc.iter().all(|c| c[0].is_finite() && c[1].is_finite()));

        // Document space is Y-down; the TL→TR→BR→BL loop must keep a positive signed area. A
        // mirrored or upside-down default would flip this sign and make fresh lettering vanish.
        let signed_area = |q: &[[f64; 2]; 4]| -> f64 {
            let mut acc = 0.0;
            for i in 0..4 {
                let p = q[i];
                let n = q[(i + 1) % 4];
                acc += p[0] * n[1] - n[0] * p[1];
            }
            acc * 0.5
        };
        assert!(signed_area(&geom.world_corners) > 0.0, "default FT quad must not be mirrored");
        assert!(signed_area(&geom.glyphs[0].quad_doc) > 0.0, "default glyph quad must not be mirrored");
    }

    #[test]
    fn spline_at_2400_dpi_does_not_explode_vertex_count_and_preserves_full_dpi() {
        let mut engine = TextEngine::new();
        let base = SfxData { text: "BAM!".to_owned(), size_pt: 72.0, origin: [100.0, 100.0], ..SfxData::default() };
        let mut curved = base.clone();
        curved.spline.points = vec![[-1200.0, 200.0], [0.0, -450.0], [1200.0, 200.0]];

        let g_1200 = build_sfx_geometry(&base, &mut engine, 1200.0);
        let g_2400 = build_sfx_geometry(&base, &mut engine, 2400.0);
        let g_spline = build_sfx_geometry(&curved, &mut engine, 2400.0);

        let h_1200 = (g_1200.rest_box[3] - g_1200.rest_box[1]).abs();
        let h_2400 = (g_2400.rest_box[3] - g_2400.rest_box[1]).abs();
        assert!(
            h_2400 > h_1200 * 1.8,
            "2400 DPI must NOT be clamped to 1200 DPI (h_1200={h_1200:.1}, h_2400={h_2400:.1})"
        );

        let verts_base: usize = g_2400.polylines.iter().map(|p| p.pts.len()).sum();
        let verts_spline: usize = g_spline.polylines.iter().map(|p| p.pts.len()).sum();
        assert_eq!(
            verts_spline, verts_base,
            "Catmull-Rom spline must NOT trigger per-segment subdivision (base={verts_base}, spline={verts_spline})"
        );
        assert!(
            verts_spline < 2000,
            "2400 DPI BAM! spline vertex count must stay lightweight (got {verts_spline})"
        );
    }
}