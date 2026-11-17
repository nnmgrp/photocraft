//! Screentone preset catalog and lattice origin math shared across engine, compose and UI.

use photocraft_geom::Rect;
pub use photocraft_tone::{DotShape, PatternType, ScreentoneBackend, ScreentoneGradientType, ScreentoneMode, ScreentoneSpec};

/// Built-in preset descriptor for manga screentone styles.
#[derive(Clone, Debug, PartialEq)]
pub struct ScreentonePreset {
    pub id: &'static str,
    pub name: &'static str,
    pub category: &'static str,
    pub spec: ScreentoneSpec,
}

/// Collection of classic manga screentone presets.
pub fn built_in_presets() -> Vec<ScreentonePreset> {
    vec![
        ScreentonePreset {
            id: "classic-10",
            name: "Deleter 10% Dot",
            category: "Classic Dots",
            spec: ScreentoneSpec { dot_size: 3.0, spacing_x: 25.0, spacing_y: 25.0, density: 0.1, ..Default::default() },
        },
        ScreentonePreset {
            id: "classic-30",
            name: "Deleter 30% Dot",
            category: "Classic Dots",
            spec: ScreentoneSpec { dot_size: 5.5, spacing_x: 22.0, spacing_y: 22.0, density: 0.3, ..Default::default() },
        },
        ScreentonePreset {
            id: "classic-60",
            name: "Deleter 60% Dot",
            category: "Classic Dots",
            spec: ScreentoneSpec { dot_size: 8.0, spacing_x: 20.0, spacing_y: 20.0, density: 0.6, ..Default::default() },
        },
        ScreentonePreset {
            id: "square-dots",
            name: "Square Dots 45°",
            category: "Classic Dots",
            spec: ScreentoneSpec {
                dot_shape: DotShape::Square,
                dot_size: 6.0,
                spacing_x: 22.0,
                spacing_y: 22.0,
                density: 0.4,
                angle: 45.0,
                ..Default::default()
            },
        },
        ScreentonePreset {
            id: "diamond-dots",
            name: "Diamond Dots",
            category: "Classic Dots",
            spec: ScreentoneSpec { dot_shape: DotShape::Diamond, dot_size: 7.0, spacing_x: 22.0, spacing_y: 22.0, density: 0.4, ..Default::default() },
        },
        ScreentonePreset {
            id: "lines-45",
            name: "Parallel Lines 45°",
            category: "Lines & Hatching",
            spec: ScreentoneSpec { pattern_type: PatternType::Lines, spacing_y: 12.0, line_width: 1.5, angle: 45.0, density: 0.5, ..Default::default() },
        },
        ScreentonePreset {
            id: "crosshatch",
            name: "Crosshatch 45°/90°",
            category: "Lines & Hatching",
            spec: ScreentoneSpec {
                pattern_type: PatternType::Crosshatch,
                spacing_y: 15.0,
                line_width: 1.0,
                angle: 45.0,
                cross_angle: 90.0,
                density: 0.5,
                ..Default::default()
            },
        },
        ScreentonePreset {
            id: "fine-lines",
            name: "Fine Lines 0°",
            category: "Lines & Hatching",
            spec: ScreentoneSpec { pattern_type: PatternType::Lines, spacing_y: 8.0, line_width: 0.8, angle: 0.0, density: 0.3, ..Default::default() },
        },
        ScreentonePreset {
            id: "gauze",
            name: "Gauze / Sand Noise",
            category: "Texture",
            spec: ScreentoneSpec {
                pattern_type: PatternType::Noise,
                dot_size: 2.0,
                spacing_x: 6.0,
                spacing_y: 6.0,
                density: 0.3,
                jitter_pos: 30.0,
                jitter_size: 50.0,
                ..Default::default()
            },
        },
        ScreentonePreset {
            id: "heavy-noise",
            name: "Heavy Noise",
            category: "Texture",
            spec: ScreentoneSpec {
                pattern_type: PatternType::GaussianNoise,
                dot_size: 2.0,
                spacing_x: 4.0,
                spacing_y: 4.0,
                density: 0.5,
                jitter_pos: 50.0,
                jitter_size: 60.0,
                ..Default::default()
            },
        },
        ScreentonePreset {
            id: "stipple",
            name: "Stipple Pointillism",
            category: "Texture",
            spec: ScreentoneSpec {
                pattern_type: PatternType::Stipple,
                dot_size: 2.0,
                spacing_x: 8.0,
                spacing_y: 8.0,
                density: 0.4,
                jitter_pos: 40.0,
                jitter_size: 40.0,
                ..Default::default()
            },
        },
        ScreentonePreset {
            id: "hex-grid",
            name: "Hex Grid Honeycomb",
            category: "Geometric",
            spec: ScreentoneSpec {
                pattern_type: PatternType::Hexgrid,
                dot_shape: DotShape::Hexagon,
                dot_size: 8.0,
                spacing_x: 20.0,
                spacing_y: 20.0,
                density: 0.5,
                ..Default::default()
            },
        },
        ScreentonePreset {
            id: "checker",
            name: "Checkerboard",
            category: "Geometric",
            spec: ScreentoneSpec { pattern_type: PatternType::Checker, spacing_x: 20.0, spacing_y: 20.0, density: 0.5, ..Default::default() },
        },
        ScreentonePreset {
            id: "triangles",
            name: "Triangles",
            category: "Geometric",
            spec: ScreentoneSpec { pattern_type: PatternType::Triangles, dot_size: 10.0, spacing_x: 30.0, spacing_y: 30.0, density: 0.4, ..Default::default() },
        },
        ScreentonePreset {
            id: "concentric",
            name: "Concentric Rings",
            category: "Effects",
            spec: ScreentoneSpec {
                pattern_type: PatternType::Concentric,
                spacing_x: 20.0,
                spacing_y: 20.0,
                line_width: 1.5,
                density: 0.5,
                ..Default::default()
            },
        },
        ScreentonePreset {
            id: "stars",
            name: "Shojo Stars",
            category: "Fun & Shojo",
            spec: ScreentoneSpec {
                pattern_type: PatternType::Stars,
                dot_size: 8.0,
                spacing_x: 40.0,
                spacing_y: 40.0,
                density: 0.4,
                jitter_pos: 20.0,
                jitter_size: 20.0,
                ..Default::default()
            },
        },
        ScreentonePreset {
            id: "hearts",
            name: "Romantic Hearts",
            category: "Fun & Shojo",
            spec: ScreentoneSpec {
                pattern_type: PatternType::Hearts,
                dot_size: 8.0,
                spacing_x: 35.0,
                spacing_y: 35.0,
                density: 0.3,
                jitter_pos: 15.0,
                jitter_size: 15.0,
                ..Default::default()
            },
        },
        ScreentonePreset {
            id: "radial-fade",
            name: "Radial Gradient Fade",
            category: "Gradient Effects",
            spec: ScreentoneSpec {
                dot_size: 6.0,
                spacing_x: 18.0,
                spacing_y: 18.0,
                grad_type: ScreentoneGradientType::Radial,
                grad_size_start: 1.5,
                grad_size_end: 0.0,
                ..Default::default()
            },
        },
        ScreentonePreset {
            id: "planets",
            name: "Planets & Satellites",
            category: "Original",
            spec: ScreentoneSpec {
                dot_size: 12.0,
                spacing_x: 70.0,
                spacing_y: 60.0,
                density: 0.5,
                satellite_enabled: true,
                satellite_size: 5.0,
                satellite_distance: 22.0,
                satellite_count: 4,
                row_offset: 0.5,
                ..Default::default()
            },
        },
    ]
}

/// The document-space point the lattice anchors to for `spec` over `bounds`.
pub fn origin_doc(spec: &ScreentoneSpec, bounds: Rect) -> (f32, f32) {
    let cx = bounds.x0 as f32 + bounds.width().max(1) as f32 * 0.5;
    let cy = bounds.y0 as f32 + bounds.height().max(1) as f32 * 0.5;
    let (sin, cos) = spec.rot_canvas.to_radians().sin_cos();
    (cx + spec.origin_x * cos - spec.origin_y * sin, cy + spec.origin_x * sin + spec.origin_y * cos)
}

/// A copy of `spec` whose lattice phase is the document-space `origin` over `bounds`.
pub fn sheet_spec(spec: &ScreentoneSpec, bounds: Rect, origin: (f32, f32)) -> ScreentoneSpec {
    let cx = bounds.x0 as f32 + bounds.width().max(1) as f32 * 0.5;
    let cy = bounds.y0 as f32 + bounds.height().max(1) as f32 * 0.5;
    let (dx, dy) = (origin.0 - cx, origin.1 - cy);
    let (sin, cos) = spec.rot_canvas.to_radians().sin_cos();
    ScreentoneSpec { origin_x: dx * cos + dy * sin, origin_y: -dx * sin + dy * cos, ..spec.clone() }
}

// ────────────────────────────────────────────────────────────
// Math & Distance Field Helpers (shared with CPU/GPU renderers)
// ────────────────────────────────────────────────────────────

/// Euclidean distance, bit-equivalent to WGSL `length(vec2f(a, b))`.
#[inline]
pub fn sdist2(a: f32, b: f32) -> f32 {
    (a * a + b * b).sqrt()
}

/// Linear interpolation between `a` and `b` at factor `t`.
#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Midpoint-biased contrast curve over normalized `t` in `[0.0, 1.0]`.
#[inline]
pub fn mid_bias_curve(t: f32, midpoint: f32) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    if midpoint <= 0.001 || midpoint >= 0.999 {
        return t;
    }
    let exponent = 0.5f32.ln() / midpoint.clamp(0.001, 0.999).ln();
    if exponent.is_finite() { t.powf(exponent).clamp(0.0, 1.0) } else { t }
}

/// Stable integer grid hash returning `[0.0, 1.0)`.
pub fn hash_coord(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x.wrapping_mul(374761393) ^ y.wrapping_mul(668265263) ^ (seed as i32).wrapping_mul(1274126177)) & 0x7fffffff;
    h = ((h ^ (h >> 13)).wrapping_mul(1274126177)) & 0x7fffffff;
    h = (h ^ (h >> 16)) & 0x7fffffff;
    (h as f32) / 2147483647.0
}

/// Box-Muller normal random pair from integer cell coordinates.
pub fn hash_gaussian(ix: i32, iy: i32, seed: u32) -> (f32, f32) {
    let u1 = hash_coord(ix, iy, seed).clamp(1e-6, 1.0);
    let u2 = hash_coord(ix.wrapping_add(1000), iy.wrapping_add(1000), seed);
    let r = (-2.0 * u1.ln()).sqrt();
    let theta = 2.0 * std::f32::consts::PI * u2;
    (r * theta.cos(), r * theta.sin())
}

/// Compute gradient factor in `[0.0, 1.0]` at relative position `(x, y)` from center.
pub fn compute_grad_factor(x: f32, y: f32, width: f32, height: f32, spec: &ScreentoneSpec) -> f32 {
    if spec.grad_type == ScreentoneGradientType::None {
        return 0.5;
    }
    let rad = spec.grad_angle.to_radians();
    let (dir_y, dir_x) = rad.sin_cos();
    let raw_grad = match spec.grad_type {
        ScreentoneGradientType::Linear => {
            let proj = x * dir_x + y * dir_y;
            proj / (width * 0.8).max(1.0) + 0.5
        }
        ScreentoneGradientType::Radial => {
            let dist = sdist2(x, y);
            dist / (width.min(height) * 0.6).max(1.0)
        }
        ScreentoneGradientType::None => 0.5,
    };
    let mut g = raw_grad.clamp(0.0, 1.0);
    if spec.grad_reverse {
        g = 1.0 - g;
    }
    mid_bias_curve(g, spec.grad_midpoint)
}

/// Distance from point `p` to line segment `a -> b`.
#[inline]
pub fn dist_to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let pax = p.0 - a.0;
    let pay = p.1 - a.1;
    let bax = b.0 - a.0;
    let bay = b.1 - a.1;
    let len_sq = bax * bax + bay * bay;
    if len_sq < 1e-8 {
        return sdist2(pax, pay);
    }
    let h = ((pax * bax + pay * bay) / len_sq).clamp(0.0, 1.0);
    let dx = pax - bax * h;
    let dy = pay - bay * h;
    sdist2(dx, dy)
}

/// Signed distance field to a 2D box with rounded corners.
#[inline]
pub fn sdf_box_rounded(px: f32, py: f32, half_w: f32, half_h: f32, radius: f32) -> f32 {
    let r = radius.clamp(0.0, half_w.min(half_h));
    let qx = px.abs() - (half_w - r);
    let qy = py.abs() - (half_h - r);
    sdist2(qx.max(0.0), qy.max(0.0)) + qx.max(qy).min(0.0) - r
}

/// Signed distance field for Square dot shape.
#[inline]
pub fn sdf_square(px: f32, py: f32, size: f32, roundness: f32) -> f32 {
    if roundness >= 0.95 {
        return sdist2(px, py) - size;
    }
    let s = size * 0.8;
    let r = roundness.clamp(0.0, 1.0) * s * 0.5;
    sdf_box_rounded(px, py, s, s, r)
}

/// Signed distance field for Diamond dot shape (rotated rhombus).
#[inline]
pub fn sdf_diamond(px: f32, py: f32, size: f32, roundness: f32) -> f32 {
    if roundness >= 0.95 {
        return sdist2(px, py) - size;
    }
    let u = (px + py) * std::f32::consts::FRAC_1_SQRT_2;
    let v = (-px + py) * std::f32::consts::FRAC_1_SQRT_2;
    let s = size * std::f32::consts::FRAC_1_SQRT_2;
    let r = roundness.clamp(0.0, 1.0) * s * 0.5;
    sdf_box_rounded(u, v, s, s, r)
}

/// Signed distance field for a general polygon with ray-casting winding check.
pub fn sdf_polygon(p: (f32, f32), pts: &[[f32; 2]], roundness: f32) -> f32 {
    let n = pts.len();
    if n < 3 {
        return f32::MAX;
    }
    let mut min_d = f32::MAX;
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let vi = (pts[i][0], pts[i][1]);
        let vj = (pts[j][0], pts[j][1]);
        if ((vi.1 > p.1) != (vj.1 > p.1)) && (p.0 < (vj.0 - vi.0) * (p.1 - vi.1) / (vj.1 - vi.1).abs().max(1e-6) + vi.0) {
            inside = !inside;
        }
        let seg_d = dist_to_segment(p, vj, vi);
        if seg_d < min_d {
            min_d = seg_d;
        }
        j = i;
    }
    let signed_d = if inside { -min_d } else { min_d };
    let r = roundness.clamp(0.0, 1.0);
    if r > 0.01 {
        let corner_rad = min_d.min(2.0) * 0.5 * r;
        signed_d - corner_rad
    } else {
        signed_d
    }
}

/// Polygon vertices for Hexagon.
pub fn hexagon_vertices(size: f32) -> [[f32; 2]; 6] {
    let mut pts = [[0.0f32; 2]; 6];
    for (i, p) in pts.iter_mut().enumerate() {
        let a = (std::f32::consts::PI / 3.0) * i as f32 - std::f32::consts::FRAC_PI_6;
        p[0] = a.cos() * size;
        p[1] = a.sin() * size;
    }
    pts
}

/// Polygon vertices for 5-pointed Star.
pub fn star_vertices(size: f32) -> [[f32; 2]; 10] {
    let mut pts = [[0.0f32; 2]; 10];
    for (i, p) in pts.iter_mut().enumerate() {
        let r = if i % 2 == 0 { size } else { size * 0.4 };
        let a = (std::f32::consts::PI / 5.0) * i as f32 - std::f32::consts::FRAC_PI_2;
        p[0] = a.cos() * r;
        p[1] = a.sin() * r;
    }
    pts
}

/// Polygon vertices for Equilateral Triangle.
pub fn triangle_vertices(size: f32) -> [[f32; 2]; 3] {
    [[0.0, -size], [size * 0.8660254, size * 0.5], [-size * 0.8660254, size * 0.5]]
}

/// Polygon vertices for Heart shape curve (28 sample points).
pub fn heart_vertices(size: f32) -> [[f32; 2]; 28] {
    let mut pts = [[0.0f32; 2]; 28];
    let sc = size / 17.0;
    for (i, p) in pts.iter_mut().enumerate() {
        let t = (i as f32 / 28.0) * std::f32::consts::PI * 2.0;
        let sin_t = t.sin();
        let x = sc * 16.0 * sin_t * sin_t * sin_t;
        let y = -sc * (13.0 * t.cos() - 5.0 * (2.0 * t).cos() - 2.0 * (3.0 * t).cos() - (4.0 * t).cos());
        p[0] = x;
        p[1] = y;
    }
    pts
}

/// Per-pixel shape transform (stretch, corner roundness, pattern rotation) shared by all SDF evaluations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeXform {
    pub stretch_x: f32,
    pub stretch_y: f32,
    pub roundness: f32,
    pub rot_pattern_rad: f32,
}

/// Evaluates Signed Distance to a specific shape at `(px, py)` with stretch and rotation.
#[inline]
pub fn eval_shape_sdf(px: f32, py: f32, shape: DotShape, size: f32, xf: ShapeXform) -> f32 {
    if size <= 0.05 {
        return 1e6;
    }
    if (shape == DotShape::Circle || xf.roundness >= 0.95) && (xf.stretch_x - 1.0).abs() <= 1e-4 && (xf.stretch_y - 1.0).abs() <= 1e-4 {
        return sdist2(px, py) - size;
    }
    let (sin_p, cos_p) = xf.rot_pattern_rad.sin_cos();
    let rx = px * cos_p + py * sin_p;
    let ry = -px * sin_p + py * cos_p;
    let sx = rx / xf.stretch_x.max(0.01);
    let sy = ry / xf.stretch_y.max(0.01);

    if shape == DotShape::Circle || xf.roundness >= 0.95 {
        return sdist2(sx, sy) - size;
    }
    match shape {
        DotShape::Circle => sdist2(sx, sy) - size,
        DotShape::Square => sdf_square(sx, sy, size, xf.roundness),
        DotShape::Diamond => sdf_diamond(sx, sy, size, xf.roundness),
        DotShape::Hexagon => {
            let verts = hexagon_vertices(size);
            sdf_polygon((sx, sy), &verts, xf.roundness)
        }
    }
}

/// Evaluates special pattern shape SDF (star, heart, triangle).
pub fn eval_special_shape_sdf(px: f32, py: f32, pattern: PatternType, size: f32, xf: ShapeXform) -> f32 {
    if size <= 0.05 {
        return 1e6;
    }
    let (sin_p, cos_p) = xf.rot_pattern_rad.sin_cos();
    let rx = px * cos_p + py * sin_p;
    let ry = -px * sin_p + py * cos_p;
    let sx = rx / xf.stretch_x.max(0.01);
    let sy = ry / xf.stretch_y.max(0.01);

    match pattern {
        PatternType::Stars => {
            let verts = star_vertices(size);
            sdf_polygon((sx, sy), &verts, xf.roundness)
        }
        PatternType::Hearts => {
            let verts = heart_vertices(size);
            sdf_polygon((sx, sy), &verts, xf.roundness)
        }
        PatternType::Triangles => {
            let verts = triangle_vertices(size);
            sdf_polygon((sx, sy), &verts, xf.roundness)
        }
        _ => sdist2(sx, sy) - size,
    }
}

/// Convert Signed Distance to coverage in `[0.0, 1.0]`.
#[inline]
pub fn coverage_from_sdf(dist: f32, smoothness: f32) -> f32 {
    if smoothness < 0.05 { if dist <= 0.0 { 1.0 } else { 0.0 } } else { ((0.5 - dist) / smoothness).clamp(0.0, 1.0) }
}

#[cfg(test)]
mod tests {
    use photocraft_geom::Rect;

    use super::{
        DotShape, PatternType, ScreentoneGradientType, ScreentoneSpec, ShapeXform, built_in_presets, compute_grad_factor, coverage_from_sdf, eval_shape_sdf,
        eval_special_shape_sdf, origin_doc, sheet_spec,
    };

    #[test]
    fn built_in_presets_have_unique_ids() {
        let presets = built_in_presets();
        assert_eq!(presets.len(), 19);
        let mut ids: Vec<&str> = presets.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), presets.len());
    }

    #[test]
    fn origin_doc_and_sheet_spec_roundtrip_with_rotation() {
        let bounds = Rect::new(100, 200, 500, 600);
        let base = ScreentoneSpec { rot_canvas: 35.0, ..Default::default() };
        let origin = (342.5, 417.25);
        let anchored = sheet_spec(&base, bounds, origin);
        let recovered = origin_doc(&anchored, bounds);
        assert!((recovered.0 - origin.0).abs() < 1e-4, "x mismatch: {} vs {}", recovered.0, origin.0);
        assert!((recovered.1 - origin.1).abs() < 1e-4, "y mismatch: {} vs {}", recovered.1, origin.1);
    }

    #[test]
    fn sdf_circle_and_square_sign_inside_vs_outside() {
        let xf = ShapeXform { stretch_x: 1.0, stretch_y: 1.0, roundness: 0.0, rot_pattern_rad: 0.0 };
        let size = 1.0;

        assert!(eval_shape_sdf(0.0, 0.0, DotShape::Circle, size, xf) < 0.0);
        assert!(eval_shape_sdf(20.0, 20.0, DotShape::Circle, size, xf) > 0.0);

        assert!(eval_shape_sdf(0.0, 0.0, DotShape::Square, size, xf) < 0.0);
        assert!(eval_shape_sdf(20.0, 20.0, DotShape::Square, size, xf) > 0.0);
    }

    #[test]
    fn special_shape_sdf_handles_stars_hearts_and_tiny_size() {
        let xf = ShapeXform { stretch_x: 1.0, stretch_y: 1.0, roundness: 0.0, rot_pattern_rad: 0.0 };

        assert!(eval_special_shape_sdf(0.0, 0.0, PatternType::Stars, 0.05, xf) > 1000.0);
        assert!(eval_special_shape_sdf(0.0, 0.0, PatternType::Hearts, 0.05, xf) > 1000.0);

        assert!(eval_special_shape_sdf(0.0, 0.0, PatternType::Stars, 8.0, xf) < 0.0);
        assert!(eval_special_shape_sdf(0.0, 0.0, PatternType::Hearts, 8.0, xf) < 0.0);
    }

    #[test]
    fn coverage_from_sdf_sharp_vs_smooth_and_linear_gradient() {
        assert_eq!(coverage_from_sdf(-1.0, 0.0), 1.0);
        assert_eq!(coverage_from_sdf(0.1, 0.0), 0.0);

        let smooth_cov = coverage_from_sdf(0.0, 1.0);
        assert!(smooth_cov > 0.0 && smooth_cov < 1.0);

        let spec = ScreentoneSpec { grad_type: ScreentoneGradientType::None, ..Default::default() };
        let grad = compute_grad_factor(0.0, 0.0, 100.0, 100.0, &spec);
        assert_eq!(grad, 0.5);
    }
}
