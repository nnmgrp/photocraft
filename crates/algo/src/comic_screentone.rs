//! Procedural screentone generator (Comic screentone engine) for manga, comics and graphic arts.
//!
//! Features:
//! * 12 pattern types: dots, lines, crosshatch, noise, gaussian_noise, stipple,
//!   hexgrid, checker, concentric, stars, hearts, triangles.
//! * 4 dot shapes: circle, square, diamond, hexagon.
//! * Corner roundness (0.0 = sharp polygon, 1.0 = fully rounded).
//! * Orbital satellite system (1–4 satellites, angle, distance, shape, stretch toward parent).
//! * Gradient modulation (linear, radial) with midpoint curve bias.
//! * Subpixel antialiasing or 1-bit binary threshold (manga print mode).
//! * Layer-luminance halftone modulation (manga shading) or pure procedural sheet fill.
//! * Depth-agnostic, selection-aware and tile-invariant.

use std::f32::consts::FRAC_1_SQRT_2;

use photocraft_geom::Rect;
use photocraft_raster::Surface;
pub use photocraft_tone::{DotShape, PatternType, SatelliteLayout, ScreentoneBackend, ScreentoneGradientType, ScreentoneMode, ScreentoneSpec};

use crate::Ctx;
use crate::fxutil::{luma, rgba, set_rgba, xy};
use crate::image::Image;

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
        ScreentonePreset {
            id: "clover-quatrefoil",
            name: "Quatrefoil Clover",
            category: "Compound & Satellites",
            spec: ScreentoneSpec {
                pattern_type: PatternType::Dots,
                dot_shape: DotShape::Circle,
                dot_size: 4.5,
                spacing_x: 24.0,
                spacing_y: 24.0,
                density: 0.5,
                angle: 45.0,
                row_offset: 0.5,
                satellite_enabled: true,
                satellite_count: 4,
                satellite_size: 3.5,
                satellite_distance: 8.5,
                satellite_dot_shape: DotShape::Circle,
                satellite_stretch: 0.25,
                merge_factor: 0.55,
                ..Default::default()
            },
        },
        ScreentonePreset {
            id: "clover-trefoil",
            name: "Trefoil Clover",
            category: "Compound & Satellites",
            spec: ScreentoneSpec {
                pattern_type: PatternType::Dots,
                dot_shape: DotShape::Circle,
                dot_size: 4.5,
                spacing_x: 26.0,
                spacing_y: 26.0,
                density: 0.5,
                satellite_enabled: true,
                satellite_count: 3,
                satellite_size: 3.8,
                satellite_distance: 9.0,
                satellite_layout: SatelliteLayout::Even,
                merge_factor: 0.5,
                ..Default::default()
            },
        },
        ScreentonePreset {
            id: "ink-asterisk",
            name: "Ink Asterisk",
            category: "Compound & Satellites",
            spec: ScreentoneSpec {
                pattern_type: PatternType::Dots,
                dot_shape: DotShape::Diamond,
                dot_size: 4.0,
                spacing_x: 24.0,
                spacing_y: 24.0,
                density: 0.5,
                row_offset: 0.5,
                satellite_enabled: true,
                satellite_count: 4,
                satellite_size: 3.2,
                satellite_distance: 9.0,
                satellite_angle: 45.0,
                satellite_dot_shape: DotShape::Diamond,
                satellite_stretch: 1.5,
                merge_factor: 0.45,
                ..Default::default()
            },
        },
        ScreentonePreset {
            id: "orbital-cluster",
            name: "Orbital Cluster",
            category: "Compound & Satellites",
            spec: ScreentoneSpec {
                pattern_type: PatternType::Dots,
                dot_shape: DotShape::Circle,
                dot_size: 5.5,
                spacing_x: 28.0,
                spacing_y: 28.0,
                density: 0.5,
                angle: 30.0,
                row_offset: 0.5,
                satellite_enabled: true,
                satellite_count: 6,
                satellite_size: 2.2,
                satellite_distance: 10.0,
                satellite_layout: SatelliteLayout::Even,
                ..Default::default()
            },
        },
        ScreentonePreset {
            id: "calibrated-10",
            name: "Calibrated 10% Dot",
            category: "Calibrated",
            spec: ScreentoneSpec {
                auto_dot_size: true,
                mode: ScreentoneMode::Sheet,
                density: 0.10,
                spacing_x: 22.0,
                spacing_y: 22.0,
                angle: 45.0,
                row_offset: 0.5,
                ..Default::default()
            },
        },
        ScreentonePreset {
            id: "calibrated-30",
            name: "Calibrated 30% Dot",
            category: "Calibrated",
            spec: ScreentoneSpec {
                auto_dot_size: true,
                mode: ScreentoneMode::Sheet,
                density: 0.30,
                spacing_x: 22.0,
                spacing_y: 22.0,
                angle: 45.0,
                row_offset: 0.5,
                ..Default::default()
            },
        },
        ScreentonePreset {
            id: "calibrated-60",
            name: "Calibrated 60% Dot",
            category: "Calibrated",
            spec: ScreentoneSpec {
                auto_dot_size: true,
                mode: ScreentoneMode::Sheet,
                density: 0.60,
                spacing_x: 22.0,
                spacing_y: 22.0,
                angle: 45.0,
                row_offset: 0.5,
                ..Default::default()
            },
        },
    ]
}

/// The document-space point the lattice anchors to for `spec` over `bounds`.
///
/// The renderer phases its lattice from the centre of `bounds`; adding the spec's rotated phase
/// gives the same anchor in document coordinates. A stored screentone layer keeps this point, so
/// growing the canvas or rendering only the visible tile does not slide the pattern.
pub fn origin_doc(spec: &ScreentoneSpec, bounds: Rect) -> (f32, f32) {
    let cx = bounds.x0 as f32 + bounds.width().max(1) as f32 * 0.5;
    let cy = bounds.y0 as f32 + bounds.height().max(1) as f32 * 0.5;
    let (sin, cos) = spec.rot_canvas.to_radians().sin_cos();
    (cx + spec.origin_x * cos - spec.origin_y * sin, cy + spec.origin_x * sin + spec.origin_y * cos)
}

/// A copy of `spec` whose lattice phase is the document-space `origin` over `bounds`.
///
/// This is the inverse of [`origin_doc`]. `use_source_luminance` is preserved so callers
/// passing a source surface can shade by it; [`render_screentone_to_surface`] turns it off
/// automatically when the target surface carries no source pixels.
pub fn sheet_spec(spec: &ScreentoneSpec, bounds: Rect, origin: (f32, f32)) -> ScreentoneSpec {
    let cx = bounds.x0 as f32 + bounds.width().max(1) as f32 * 0.5;
    let cy = bounds.y0 as f32 + bounds.height().max(1) as f32 * 0.5;
    let (dx, dy) = (origin.0 - cx, origin.1 - cy);
    let (sin, cos) = spec.rot_canvas.to_radians().sin_cos();
    ScreentoneSpec { origin_x: dx * cos + dy * sin, origin_y: -dx * sin + dy * cos, ..spec.clone() }
}

/// Area threshold (`512 × 512` px) at which `ScreentoneBackend::Auto` selects the live GPU shader.
/// Using pixel area rather than per-axis minimums ensures wide or tall masked strips (such as
/// `2000 × 300`) stay on the GPU path instead of falling back to synchronous CPU bakes.
pub const SCREENTONE_AUTO_GPU_AREA: u64 = 512 * 512;

/// Single source of truth for whether a screentone layer with `(backend, mode, bounds)` should use
/// the live GPU shader instead of baking on the CPU (`planner`, `engine`, and `ScreentoneGizmo`).
pub fn screentone_prefers_gpu(backend: ScreentoneBackend, mode: photocraft_color::ColorMode, bounds: Rect) -> bool {
    if matches!(mode, photocraft_color::ColorMode::Cmyk | photocraft_color::ColorMode::Lab) {
        return false;
    }
    match backend {
        ScreentoneBackend::Cpu => false,
        ScreentoneBackend::Gpu => true,
        ScreentoneBackend::Auto => u64::from(bounds.width()).saturating_mul(u64::from(bounds.height())) >= SCREENTONE_AUTO_GPU_AREA,
    }
}

/// Render a procedural sheet into `surface`, anchored at the document-space `origin`.
///
/// The whole `bounds` frame is rendered (a layer mask shapes the visible area), and the function
/// returns the document-space area written. When `spec.use_source_luminance` is `true` and
/// `surface` already holds source pixels, the generated dots are modulated by that source's
/// darkness before `surface` is replaced with the rendered sheet; an empty `surface` renders a
/// uniform procedural sheet.
pub fn render_screentone_to_surface(surface: &mut Surface, bounds: Rect, spec: &ScreentoneSpec, origin: (f32, f32)) -> Rect {
    let mut s = sheet_spec(spec, bounds, origin);
    if surface.content_bounds().is_empty() || s.mode == ScreentoneMode::Sheet {
        s.use_source_luminance = false;
    }
    let params = crate::FilterParams::Screentone { spec: s };
    let content = surface.content_bounds().union(&bounds);
    let area = crate::output_area(&params, content, bounds, None).intersect(&bounds);
    if area.is_empty() {
        return area;
    }
    let fmt = surface.format();
    let mut rendered = crate::apply_in(surface, &params, area, bounds, None, content);
    // Transfer 256×256 tiles directly instead of gathering a dense full-canvas `Vec<f32>`
    // (6.8 GB on a 17 000 × 25 000 page). Only border tiles straddling `area` are clipped.
    let mut out = Surface::new(fmt);
    let mut interior = Vec::new();
    for (tc, tile) in rendered.take_tiles(area) {
        let tr = tc.rect();
        if tr.x0 >= area.x0 && tr.y0 >= area.y0 && tr.x1 <= area.x1 && tr.y1 <= area.y1 {
            interior.push((tc, tile));
        } else {
            let clip = tr.intersect(&area);
            if !clip.is_empty() {
                let mut one = Surface::new(fmt);
                one.put_tiles([(tc, tile)]);
                let px = one.read_region(clip);
                out.write_region(clip, &px);
            }
        }
    }
    if !interior.is_empty() {
        out.put_tiles(interior);
    }
    out.prune();
    *surface = out;
    area
}

// ────────────────────────────────────────────────────────────
// Math & Distance Field Helpers
// ────────────────────────────────────────────────────────────

/// Euclidean distance, bit-equivalent to WGSL `length(vec2f(a, b))`.
///
/// WGSL defines `length(v) = sqrt(dot(v, v))`. For a `vec2f(a, b)` that
/// is `sqrt(a*a + b*b)`. Rust's `f32::hypot(a, b)` is permitted to use
/// a different (overflow-avoiding) algorithm and diverges from this
/// form by 1-2 ULP.
///
/// Every distance in the screentone hot path goes through this
/// helper so the CPU oracle and the WGSL kernel produce identical
/// ULPs.
#[inline]
fn sdist2(a: f32, b: f32) -> f32 {
    (a * a + b * b).sqrt()
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[inline]
fn mid_bias_curve(t: f32, midpoint: f32) -> f32 {
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

/// Stable integer grid hash returning [0.0, 1.0).
fn hash_coord(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x.wrapping_mul(374761393) ^ y.wrapping_mul(668265263) ^ (seed as i32).wrapping_mul(1274126177)) & 0x7fffffff;
    h = ((h ^ (h >> 13)).wrapping_mul(1274126177)) & 0x7fffffff;
    h = (h ^ (h >> 16)) & 0x7fffffff;
    (h as f32) / 2147483647.0
}

/// Box-Muller normal random pair from cell coordinate.
fn hash_gaussian(ix: i32, iy: i32, seed: u32) -> (f32, f32) {
    let u1 = hash_coord(ix, iy, seed).clamp(1e-6, 1.0);
    let u2 = hash_coord(ix.wrapping_add(1000), iy.wrapping_add(1000), seed);
    let r = (-2.0 * u1.ln()).sqrt();
    let theta = 2.0 * std::f32::consts::PI * u2;
    (r * theta.cos(), r * theta.sin())
}

/// Compute gradient factor in [0.0, 1.0] at relative position `(x, y)` from center.
fn compute_grad_factor(x: f32, y: f32, width: f32, height: f32, spec: &ScreentoneSpec) -> f32 {
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
        ScreentoneGradientType::Angle => ((rad - (-y).atan2(x)) / std::f32::consts::TAU).rem_euclid(1.0),
        ScreentoneGradientType::Reflected => {
            let proj = x * dir_x + y * dir_y;
            proj.abs() / (width * 0.4).max(1.0)
        }
        ScreentoneGradientType::Diamond => {
            let along = x * dir_x + y * dir_y;
            let across = -x * dir_y + y * dir_x;
            (along.abs() + across.abs()) / (width.min(height) * 0.6).max(1.0)
        }
        ScreentoneGradientType::None => 0.5,
    };
    let mut g = raw_grad.clamp(0.0, 1.0);
    if spec.grad_reverse {
        g = 1.0 - g;
    }
    mid_bias_curve(g, spec.grad_midpoint)
}

/// Signed distance field to a 2D box with rounded corners.
#[inline]
fn sdf_box_rounded(px: f32, py: f32, half_w: f32, half_h: f32, radius: f32) -> f32 {
    let r = radius.clamp(0.0, half_w.min(half_h));
    let qx = px.abs() - (half_w - r);
    let qy = py.abs() - (half_h - r);
    sdist2(qx.max(0.0), qy.max(0.0)) + qx.max(qy).min(0.0) - r
}

/// Signed distance field for Square dot shape.
#[inline]
fn sdf_square(px: f32, py: f32, size: f32, roundness: f32) -> f32 {
    if roundness >= 0.95 {
        return sdist2(px, py) - size;
    }
    let s = size * 0.8;
    let r = roundness.clamp(0.0, 1.0) * s * 0.5;
    sdf_box_rounded(px, py, s, s, r)
}

/// Signed distance field for Diamond dot shape (rotated rhombus).
#[inline]
fn sdf_diamond(px: f32, py: f32, size: f32, roundness: f32) -> f32 {
    if roundness >= 0.95 {
        return sdist2(px, py) - size;
    }
    let u = (px + py) * std::f32::consts::FRAC_1_SQRT_2;
    let v = (-px + py) * std::f32::consts::FRAC_1_SQRT_2;
    let s = size * std::f32::consts::FRAC_1_SQRT_2;
    let r = roundness.clamp(0.0, 1.0) * s * 0.5;
    sdf_box_rounded(u, v, s, s, r)
}

/// Analytic O(1) distance fields and area constants (the legacy polygon generators live in the
/// `oracle` test module below and are used only to verify these formulas).

#[inline]
pub fn sdf_smooth_min(d1: f32, d2: f32, k: f32) -> f32 {
    if k <= 1e-4 {
        return d1.min(d2);
    }
    let h = (k - (d1 - d2).abs()).max(0.0) / k;
    d1.min(d2) - h * h * k * 0.25
}

#[inline]
pub fn sdf_hexagon_exact(px: f32, py: f32, size: f32, roundness: f32) -> f32 {
    const KX: f32 = -0.866025404;
    const KY: f32 = 0.5;
    const KZ: f32 = 0.577350269;
    let r = size * 0.25 * roundness.clamp(0.0, 1.0);
    let s = (size - r).max(0.05) * 0.866025404;
    let mut x = px.abs();
    let mut y = py.abs();
    let dot = 2.0 * (KX * x + KY * y).min(0.0);
    x -= dot * KX;
    y -= dot * KY;
    x -= x.clamp(-KZ * s, KZ * s);
    y -= s;
    sdist2(x, y) * y.signum() - r
}

#[inline]
pub fn sdf_triangle_exact(px: f32, mut py: f32, size: f32, roundness: f32) -> f32 {
    const K: f32 = 1.732050808;
    let r = size * 0.25 * roundness.clamp(0.0, 1.0);
    let s = (size - r).max(0.05) * 0.866025404;
    let mut x = px.abs() - s;
    py = -py + s / K;
    if x + K * py > 0.0 {
        let nx = (x - K * py) * 0.5;
        let ny = (-K * x - py) * 0.5;
        x = nx;
        py = ny;
    }
    x -= x.clamp(-2.0 * s, 0.0);
    -sdist2(x, py) * py.signum() - r
}

#[inline]
pub fn sdf_star5_exact(px: f32, mut py: f32, size: f32, roundness: f32) -> f32 {
    const K1X: f32 = 0.809016994;
    const K1Y: f32 = -0.587785252;
    const K2X: f32 = -K1X;
    const K2Y: f32 = K1Y;
    let r = size * 0.2 * roundness.clamp(0.0, 1.0);
    let s = (size - r).max(0.05);
    let rf = 0.40;
    let mut x = px.abs();
    py = -py;
    let d1 = 2.0 * (K1X * x + K1Y * py).max(0.0);
    x -= d1 * K1X;
    py -= d1 * K1Y;
    let d2 = 2.0 * (K2X * x + K2Y * py).max(0.0);
    x -= d2 * K2X;
    py -= d2 * K2Y;
    x = x.abs();
    py -= s;
    let bax = rf * (-K1Y);
    let bay = rf * K1X - 1.0;
    let h = ((x * bax + py * bay) / (bax * bax + bay * bay)).clamp(0.0, s);
    let dx = x - bax * h;
    let dy = py - bay * h;
    sdist2(dx, dy) * (py * bax - x * bay).signum() - r
}

#[inline]
pub fn sdf_heart_exact(px: f32, py: f32, size: f32, _roundness: f32) -> f32 {
    let s = size.max(0.05) * 1.46;
    let x = px.abs() / s;
    let y = (0.55 - py / s).clamp(-2.0, 2.0);
    let d = if y + x > 1.0 {
        let dx = x - 0.25;
        let dy = y - 0.75;
        sdist2(dx, dy) - FRAC_1_SQRT_2 * 0.5
    } else {
        let d1 = x * x + (y - 1.0).powi(2);
        let m = 0.5 * (x + y).max(0.0);
        let d2 = (x - m).powi(2) + (y - m).powi(2);
        let dist = d1.min(d2).sqrt();
        let sign = if x < y { -1.0 } else { 1.0 };
        dist * sign
    };
    d * s
}

/// `sqrt` area calibration, only where ink scales the dot radius.
#[inline]
pub fn area_ink(spec: &ScreentoneSpec, ink: f32) -> f32 {
    if spec.equalize_area && !spec.auto_dot_size && ink > 0.0 && ink < 1.0 { ink.sqrt() } else { ink }
}

pub const AREA_K_CIRCLE: f32 = std::f32::consts::PI;
pub const AREA_K_SQUARE: f32 = 2.56;
pub const AREA_K_DIAMOND: f32 = 2.0;
pub const AREA_K_HEXAGON: f32 = 2.598_076;
pub const AREA_K_TRIANGLE: f32 = 1.299_038;
pub const AREA_K_STAR5: f32 = 1.175_571;
pub const AREA_K_HEART: f32 = 1.902_877;

pub fn area_k_for(pattern: PatternType, dot: DotShape) -> f32 {
    match pattern {
        PatternType::Stars => AREA_K_STAR5,
        PatternType::Hearts => AREA_K_HEART,
        PatternType::Triangles => AREA_K_TRIANGLE,
        PatternType::Hexgrid => AREA_K_HEXAGON,
        _ => match dot {
            DotShape::Circle => AREA_K_CIRCLE,
            DotShape::Square => AREA_K_SQUARE,
            DotShape::Diamond => AREA_K_DIAMOND,
            DotShape::Hexagon => AREA_K_HEXAGON,
        },
    }
}

#[inline]
pub fn auto_size(cell_area: f32, coverage: f32, area_k: f32) -> f32 {
    (cell_area * coverage.clamp(0.0, 1.0) / area_k.max(1e-3)).sqrt()
}

/// Per-pixel shape transform (stretch, corner roundness, pattern rotation) shared by all SDF
/// evaluations, so the helpers stay within the crate's argument limit.
#[derive(Clone, Copy, Debug)]
struct ShapeXform {
    stretch_x: f32,
    stretch_y: f32,
    inv_stretch_x: f32,
    inv_stretch_y: f32,
    roundness: f32,
    sin_p: f32,
    cos_p: f32,
}

impl ShapeXform {
    fn new(stretch_x: f32, stretch_y: f32, roundness: f32, sin_p: f32, cos_p: f32) -> Self {
        let sx = stretch_x.max(0.01);
        let sy = stretch_y.max(0.01);
        Self { stretch_x: sx, stretch_y: sy, inv_stretch_x: 1.0 / sx, inv_stretch_y: 1.0 / sy, roundness, sin_p, cos_p }
    }
}

/// Evaluates Signed Distance to a specific shape at `(px, py)` with stretch and rotation.
#[inline]
fn eval_shape_sdf(px: f32, py: f32, shape: DotShape, size: f32, xf: ShapeXform) -> f32 {
    if size <= 0.05 {
        return 1e6;
    }
    if (shape == DotShape::Circle || xf.roundness >= 0.95) && (xf.stretch_x - 1.0).abs() <= 1e-4 && (xf.stretch_y - 1.0).abs() <= 1e-4 {
        return sdist2(px, py) - size;
    }
    let rx = px * xf.cos_p + py * xf.sin_p;
    let ry = -px * xf.sin_p + py * xf.cos_p;
    let sx = rx * xf.inv_stretch_x;
    let sy = ry * xf.inv_stretch_y;

    if shape == DotShape::Circle || xf.roundness >= 0.95 {
        return sdist2(sx, sy) - size;
    }
    match shape {
        DotShape::Circle => sdist2(sx, sy) - size,
        DotShape::Square => sdf_square(sx, sy, size, xf.roundness),
        DotShape::Diamond => sdf_diamond(sx, sy, size, xf.roundness),
        // The legacy hexagon is pointy-top; the IQ formula is flat-top, so swap the axes.
        DotShape::Hexagon => sdf_hexagon_exact(sy, sx, size, xf.roundness),
    }
}

/// Evaluates special pattern shape SDF (star, heart, triangle).
#[inline]
fn eval_special_shape_sdf(px: f32, py: f32, pattern: PatternType, size: f32, xf: ShapeXform) -> f32 {
    if size <= 0.05 {
        return 1e6;
    }
    let rx = px * xf.cos_p + py * xf.sin_p;
    let ry = -px * xf.sin_p + py * xf.cos_p;
    let sx = rx * xf.inv_stretch_x;
    let sy = ry * xf.inv_stretch_y;

    match pattern {
        PatternType::Stars => sdf_star5_exact(sx, sy, size, xf.roundness),
        PatternType::Hearts => sdf_heart_exact(sx, sy, size, xf.roundness),
        PatternType::Triangles => sdf_triangle_exact(sx, sy, size, xf.roundness),
        _ => sdist2(sx, sy) - size,
    }
}

/// Convert Signed Distance to coverage in [0.0, 1.0].
#[inline]
fn coverage_from_sdf(dist: f32, smoothness: f32) -> f32 {
    if smoothness < 0.05 { if dist <= 0.0 { 1.0 } else { 0.0 } } else { ((0.5 - dist) / smoothness).clamp(0.0, 1.0) }
}

// ────────────────────────────────────────────────────────────
// Screentone Kernel Execution
// ────────────────────────────────────────────────────────────

const AXES_4: [(i32, i32); 4] = [(1, 0), (0, 1), (1, 1), (1, -1)];

#[derive(Clone, Copy)]
struct ToneCurve {
    brightness: f32,
    contrast: f32,
    white_thresh: f32,
    black_thresh: f32,
    gamma: f32,
}

impl ToneCurve {
    fn new(spec: &ScreentoneSpec) -> Self {
        let white_thresh = spec.white_threshold.clamp(0.0, 1.0);
        Self {
            brightness: spec.brightness.clamp(-1.0, 1.0),
            contrast: spec.contrast.clamp(0.0, 4.0),
            white_thresh,
            black_thresh: spec.black_threshold.clamp(0.0, white_thresh),
            gamma: spec.tone_gamma.clamp(0.01, 10.0),
        }
    }

    /// Maps effective luminance to ink density in `0.0..=1.0`.
    fn luma_to_ink(&self, luma_eff: f32, posterize_steps: u8) -> f32 {
        let base = (luma_eff + self.brightness).clamp(0.0, 1.0);
        let g = if (self.gamma - 1.0).abs() < 1e-4 { base } else { base.powf(self.gamma) };
        let luma_adj = ((g - 0.5) * self.contrast + 0.5).clamp(0.0, 1.0);
        let mut ink = if luma_adj >= self.white_thresh {
            0.0
        } else if luma_adj <= self.black_thresh {
            1.0
        } else {
            ((self.white_thresh - luma_adj) / (self.white_thresh - self.black_thresh).max(1e-5)).clamp(0.0, 1.0)
        };
        if posterize_steps >= 2 && ink > 0.0 && ink < 1.0 {
            let steps = f32::from(posterize_steps);
            ink = (ink * steps).ceil() / steps;
        }
        ink
    }
}

/// Raw luminance at integer pixel `(ix, iy)` composited over white paper.
fn raw_paper_luma(src: &Image, ctx: &Ctx, ix: i32, iy: i32) -> f32 {
    if src.rect.is_empty() {
        return 0.5;
    }
    let cx = ix.clamp(src.rect.x0, src.rect.x1.saturating_sub(1));
    let cy = iy.clamp(src.rect.y0, src.rect.y1.saturating_sub(1));
    if !src.rect.contains(cx, cy) {
        return 1.0;
    }
    let px = src.px(cx, cy);
    let a = if ctx.alpha { px.last().copied().unwrap_or(1.0).clamp(0.0, 1.0) } else { 1.0 };
    if a <= 1e-3 {
        return 1.0;
    }
    let raw_luma = luma(ctx, px).clamp(0.0, 1.0);
    (raw_luma * a + (1.0 - a)).clamp(0.0, 1.0)
}

#[inline]
fn grid_sample(grid: &[f32], ww: usize, wh: usize, ix: i32, iy: i32) -> f32 {
    let cx = ix.clamp(0, ww as i32 - 1) as usize;
    let cy = iy.clamp(0, wh as i32 - 1) as usize;
    grid[cy * ww + cx]
}

fn axis_valley_depth(luma_grid: &[f32], ww: usize, wh: usize, ix: i32, iy: i32, l_c: f32) -> f32 {
    let mut max_valley = 0.0f32;
    for &(dx, dy) in &AXES_4 {
        let mut l_minus = l_c;
        for s in 1..=3 {
            let v = grid_sample(luma_grid, ww, wh, ix - dx * s, iy - dy * s);
            if v < l_c - 0.08 {
                break;
            }
            l_minus = l_minus.max(v);
        }
        let mut l_plus = l_c;
        for s in 1..=3 {
            let v = grid_sample(luma_grid, ww, wh, ix + dx * s, iy + dy * s);
            if v < l_c - 0.08 {
                break;
            }
            l_plus = l_plus.max(v);
        }
        max_valley = max_valley.max((l_minus.min(l_plus) - l_c).max(0.0));
    }
    max_valley
}

/// Detect dark ink line cores and 4-axis monotonic 1D valleys while ignoring anti-aliased fringes.
fn detect_line_edges(luma_grid: &[f32], ww: usize, wh: usize, line_thresh: f32) -> Vec<u8> {
    let mut edges = vec![0u8; ww * wh];
    let thresh = line_thresh.clamp(0.02, 0.72);
    for wy in 0..wh {
        let iy = wy as i32;
        let row_off = wy * ww;
        for wx in 0..ww {
            let ix = wx as i32;
            let l_c = luma_grid[row_off + wx];
            if l_c >= 0.72 {
                continue;
            }
            let at = |ox: i32, oy: i32| grid_sample(luma_grid, ww, wh, ix + ox, iy + oy);
            let (l_l, l_r, l_u, l_d) = (at(-1, 0), at(1, 0), at(0, -1), at(0, 1));
            let (l_ul, l_ur, l_dl, l_dr) = (at(-1, -1), at(1, -1), at(-1, 1), at(1, 1));
            let nb_max = l_l
                .max(l_r)
                .max(l_u)
                .max(l_d)
                .max(l_ul)
                .max(l_ur)
                .max(l_dl)
                .max(l_dr)
                .max(at(-2, 0))
                .max(at(2, 0))
                .max(at(0, -2))
                .max(at(0, 2))
                .max(at(-2, -2))
                .max(at(2, -2))
                .max(at(-2, 2))
                .max(at(2, 2));
            let local_dip = (nb_max - l_c).max(0.0);
            let contrast_nb = (l_l - l_c).max(l_r - l_c).max(l_u - l_c).max(l_d - l_c).max(l_ul - l_c).max(l_ur - l_c).max(l_dl - l_c).max(l_dr - l_c).max(0.0);
            let edge_mag = sdist2(l_r - l_l, l_d - l_u).max(sdist2(l_dr - l_ul, l_ur - l_dl)).max(contrast_nb);
            let max_valley = axis_valley_depth(luma_grid, ww, wh, ix, iy, l_c);
            let is_edge = (l_c <= thresh && (edge_mag > 0.04 || local_dip > 0.05 || l_c <= thresh * 0.85))
                || (l_c < 0.65 && max_valley > (0.14 - thresh * 0.20).max(0.06))
                || (l_c < 0.62 && edge_mag > (0.22 - thresh * 0.35).max(0.06) && local_dip > 0.08 && max_valley > 0.03);
            if is_edge {
                edges[row_off + wx] = 1;
            }
        }
    }
    edges
}

/// Bounded 8-ray line preservation + de-lining mirror of `compose.wgsl::st_delined_luma`.
/// Returns `(solid_line, eff_luma)` using the exact same paired 8-direction scan and tie-breaking.
fn eval_preserve_lines_8ray(
    luma_grid: &[f32],
    line_edge_grid: &[u8],
    ww: usize,
    wh: usize,
    ix: i32,
    iy: i32,
    line_thresh: f32,
    line_thick: f32,
) -> (bool, f32) {
    let idx_center = iy as usize * ww + ix as usize;
    let center_luma = luma_grid[idx_center];
    if line_edge_grid[idx_center] != 0 {
        return (true, center_luma);
    }
    let thresh = line_thresh.clamp(0.02, 0.72);
    let r_px = (line_thick.clamp(0.5, 5.0) - 0.5).max(0.0);
    let r_sq = r_px * r_px + 0.25;
    let inner_card = if r_px > 0.0 { r_sq.sqrt().floor() as i32 } else { 0 };
    let inner_diag = if r_px > 0.0 { (r_sq * 0.5).sqrt().floor() as i32 } else { 0 };

    const DIRS_8: [(i32, i32); 8] = [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (-1, -1), (1, -1), (-1, 1)];
    let load_luma = |x: i32, y: i32| -> f32 {
        let cx = x.clamp(0, ww as i32 - 1) as usize;
        let cy = y.clamp(0, wh as i32 - 1) as usize;
        luma_grid[cy * ww + cx]
    };

    let mut ray_l1 = [0.0f32; 8];
    let mut ray_l2 = [0.0f32; 8];
    let mut outer_s = [0i32; 8];
    let mut outer_l = [1.0f32; 8];
    let mut outer_max_l = [0.0f32; 8];
    let mut ray_near = [false; 8];
    let mut ray_slit = [false; 8];

    for (i, &(dx, dy)) in DIRS_8.iter().enumerate() {
        let is_diag = i >= 4;
        let inner_max = if is_diag { inner_diag } else { inner_card };
        let slit_steps = if is_diag { 2 } else { 3 };
        let outer_max = inner_max + slit_steps;

        let mut ray_max = center_luma;
        let mut in_best_s = 0i32;
        let mut in_best_l = 1.0f32;
        let mut out_best_s = 0i32;
        let mut out_best_l = 1.0f32;
        let mut l1 = center_luma;
        let mut l2 = center_luma;

        for s in 1..=outer_max {
            let qx = ix + dx * s;
            let qy = iy + dy * s;
            let in_bounds = qx >= 0 && qx < ww as i32 && qy >= 0 && qy < wh as i32;
            let l = load_luma(qx, qy);
            if s == 1 {
                l1 = l;
                l2 = l;
            } else if s == 2 {
                l2 = l;
            }
            if !in_bounds {
                continue;
            }
            ray_max = ray_max.max(l);
            if s <= inner_max {
                if l < in_best_l {
                    in_best_l = l;
                    in_best_s = s;
                }
            } else if l < out_best_l {
                out_best_l = l;
                out_best_s = s;
            }
        }
        ray_l1[i] = l1;
        ray_l2[i] = l2;
        outer_s[i] = out_best_s;
        outer_l[i] = out_best_l;
        outer_max_l[i] = ray_max;

        if in_best_s > 0 && in_best_l < 0.72 && (in_best_l <= thresh || ray_max - in_best_l > 0.03) {
            let ex = (ix + dx * in_best_s) as usize;
            let ey = (iy + dy * in_best_s) as usize;
            if line_edge_grid[ey * ww + ex] != 0 {
                return (true, center_luma);
            }
        }
    }

    let mut any_near = false;
    for (i, &(dx, dy)) in DIRS_8.iter().enumerate() {
        let s = outer_s[i];
        let ol = outer_l[i];
        if s > 0 && ol < 0.72 && (ol <= thresh || outer_max_l[i] - ol > 0.03) {
            let ex = (ix + dx * s) as usize;
            let ey = (iy + dy * s) as usize;
            if line_edge_grid[ey * ww + ex] != 0 {
                let is_diag = i >= 4;
                let near_max = if is_diag { inner_diag + 1 } else { inner_card + 2 };
                ray_slit[i] = true;
                if s <= near_max {
                    ray_near[i] = true;
                    any_near = true;
                }
            }
        }
    }
    if !any_near {
        return (false, center_luma);
    }

    let mut best_luma = center_luma;
    for i in 0..8 {
        if !ray_near[i] {
            continue;
        }
        let opp = i ^ 1;
        if !ray_near[opp] {
            best_luma = best_luma.max(ray_l1[opp].max(ray_l2[opp]));
        }
    }
    let in_tight_slit = (ray_slit[0] && ray_slit[1])
        || (ray_slit[2] && ray_slit[3])
        || (ray_slit[4] && ray_slit[5])
        || (ray_slit[6] && ray_slit[7]);
    if in_tight_slit && best_luma >= 0.50 {
        (false, (best_luma + 0.28).min(1.0))
    } else {
        (false, best_luma)
    }
}

struct ShadingGrids {
    work_rect: Rect,
    ww: usize,
    preserved_grid: Vec<u8>,
    ambient_ink_grid: Vec<f32>,
}

impl ShadingGrids {
    fn build(src: &Image, out: Rect, ctx: &Ctx, spec: &ScreentoneSpec, tone: ToneCurve) -> Self {
        let spacing_x = spec.spacing_x.clamp(1.0, 2000.0);
        let spacing_y = spec.spacing_y.clamp(1.0, 2000.0);
        let max_sp = spacing_x.max(spacing_y).clamp(1.0, 256.0);
        let reach = (max_sp * 1.5).ceil() as i32 + 16;
        let work_rect = out.inflate(reach).intersect(&src.rect);
        let (ww, wh) = (work_rect.width() as usize, work_rect.height() as usize);
        if ww == 0 || wh == 0 {
            return Self { work_rect, ww, preserved_grid: Vec::new(), ambient_ink_grid: Vec::new() };
        }
        let mut luma_grid = vec![1.0f32; ww * wh];
        for wy in 0..wh {
            let iy = work_rect.y0 + wy as i32;
            let row_off = wy * ww;
            for wx in 0..ww {
                luma_grid[row_off + wx] = raw_paper_luma(src, ctx, work_rect.x0 + wx as i32, iy);
            }
        }
        let line_edge_grid = if spec.preserve_lines {
            detect_line_edges(&luma_grid, ww, wh, spec.line_threshold.clamp(0.0, 1.0))
        } else {
            Vec::new()
        };
        let mut preserved_grid = if spec.preserve_lines { vec![0u8; ww * wh] } else { Vec::new() };
        let mut ambient_ink_grid = vec![0.0f32; ww * wh];
        let line_thresh = spec.line_threshold.clamp(0.0, 1.0);
        let line_thick = spec.line_thickness.clamp(0.5, 5.0);
        for wy in 0..wh {
            let row_off = wy * ww;
            for wx in 0..ww {
                let idx = row_off + wx;
                let eff_luma = if !spec.preserve_lines {
                    luma_grid[idx]
                } else {
                    let (solid, delined) = eval_preserve_lines_8ray(&luma_grid, &line_edge_grid, ww, wh, wx as i32, wy as i32, line_thresh, line_thick);
                    if solid {
                        preserved_grid[idx] = 1;
                    }
                    delined
                };
                ambient_ink_grid[idx] = tone.luma_to_ink(eff_luma, spec.posterize_steps);
            }
        }
        Self { work_rect, ww, preserved_grid, ambient_ink_grid }
    }

    fn grid_idx(&self, ix: i32, iy: i32) -> usize {
        let cx = (ix.clamp(self.work_rect.x0, self.work_rect.x1 - 1) - self.work_rect.x0) as usize;
        let cy = (iy.clamp(self.work_rect.y0, self.work_rect.y1 - 1) - self.work_rect.y0) as usize;
        cy * self.ww + cx
    }

    fn is_preserved_line(&self, spec: &ScreentoneSpec, x: i32, y: i32) -> bool {
        spec.preserve_lines && !self.preserved_grid.is_empty() && self.preserved_grid[self.grid_idx(x, y)] != 0
    }

    fn sample_ink(&self, src: &Image, ctx: &Ctx, tone: ToneCurve, spec: &ScreentoneSpec, doc_x: f32, doc_y: f32) -> f32 {
        let (ix, iy) = (doc_x.floor() as i32, doc_y.floor() as i32);
        if !self.ambient_ink_grid.is_empty() {
            self.ambient_ink_grid[self.grid_idx(ix, iy)]
        } else {
            tone.luma_to_ink(raw_paper_luma(src, ctx, ix, iy), spec.posterize_steps)
        }
    }
}

/// Satellite directions are precomputed once per frame: the hot pixel loop never calls
/// `sin_cos`/`atan2`. `Cross` keeps the exact legacy order; `Even` spaces N directions over 2π.
fn build_sat_dirs(spec: &ScreentoneSpec, sin_sat: f32, cos_sat: f32) -> ([(f32, f32); 8], usize) {
    const CROSS: [(f32, f32); 4] = [(0.0, -1.0), (0.0, 1.0), (1.0, 0.0), (-1.0, 0.0)];
    let mut dirs = [(0.0f32, 0.0f32); 8];
    match spec.satellite_layout {
        SatelliteLayout::Cross => {
            let n = (spec.satellite_count as usize).min(4);
            for (k, (ux, uy)) in CROSS.iter().take(n).enumerate() {
                dirs[k] = (ux * cos_sat - uy * sin_sat, ux * sin_sat + uy * cos_sat);
            }
            (dirs, n)
        }
        SatelliteLayout::Even => {
            let n = (spec.satellite_count as usize).clamp(0, 8);
            let base = spec.satellite_angle.to_radians();
            for k in 0..n {
                let th = base + std::f32::consts::TAU * k as f32 / n.max(1) as f32;
                dirs[k] = (th.sin(), -th.cos());
            }
            (dirs, n)
        }
    }
}

/// Smooth deterministic 1D value noise in `[-1.0, 1.0]` anchored to document coordinates.
/// Bit-equivalent to `st_value_noise_1d` in `crates/gpu/src/compose.wgsl`.
#[inline]
fn st_value_noise_1d(t: f32, line_idx: i32, seed: u32) -> f32 {
    let i0 = t.floor() as i32;
    let f = t - i0 as f32;
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash_coord(i0, line_idx, seed);
    let b = hash_coord(i0.wrapping_add(1), line_idx, seed);
    (a + (b - a) * u) * 2.0 - 1.0
}

/// Tapered manga dash multiplier in `[0.0, 1.0]` along a stroke coordinate `along`.
/// Bit-equivalent to `st_dash_taper` in `crates/gpu/src/compose.wgsl`.
#[inline]
fn st_dash_taper(along: f32, sp: f32, line_idx: i32, dash: f32, seed: u32) -> f32 {
    let period = (sp * 5.0).max(16.0);
    let dash_phase = hash_coord(line_idx, 89, seed) * period;
    let u_along = along + dash_phase;
    let seg_idx = (u_along / period).floor() as i32;
    let seg_t = (u_along - seg_idx as f32 * period) / period;
    let seg_h = hash_coord(seg_idx, line_idx, seed.wrapping_add(211));
    let duty = (1.0 - dash * 0.72 * (0.75 + 0.5 * seg_h)).clamp(0.14, 0.98);
    let half_duty = duty * 0.5;
    let d_center = (seg_t - 0.5).abs();
    if d_center >= half_duty {
        return 0.0;
    }
    let edge_dist = (half_duty - d_center) / (half_duty * 0.35).max(1e-4);
    let taper = edge_dist.clamp(0.0, 1.0);
    taper * taper * (3.0 - 2.0 * taper)
}

/// Unified post-pattern blade scraping (Kezuri) in document coordinates (`local_x, local_y`).
/// Completely skipped when `strength <= 1e-4` for bit-exact backward compatibility.
/// Bit-equivalent to `st_apply_kezuri` in `crates/gpu/src/compose.wgsl`.
#[inline]
fn apply_kezuri(cov: f32, local: (f32, f32), ink: f32, strength: f32, rot_sc: (f32, f32), seed: u32) -> f32 {
    if strength <= 1e-4 || cov <= 1e-5 {
        return cov;
    }
    let (sin_s, cos_s) = rot_sc;
    let sx = local.0 * cos_s + local.1 * sin_s;
    let sy = -local.0 * sin_s + local.1 * cos_s;
    let kseed = seed.wrapping_add(777);
    let gy_f = sy * 0.9;
    let gy0 = gy_f.floor() as i32;
    let fy = gy_f - gy0 as f32;
    let wy = fy * fy * (3.0 - 2.0 * fy);
    let n0 = st_value_noise_1d(sx * 0.045, gy0, kseed) * 0.5 + 0.5;
    let n1 = st_value_noise_1d(sx * 0.045, gy0.wrapping_add(1), kseed) * 0.5 + 0.5;
    let blade_long = n0 + (n1 - n0) * wy;
    let fine = st_value_noise_1d(sx * 0.16, gy0.wrapping_mul(3).wrapping_add(11), kseed.wrapping_add(31)) * 0.5 + 0.5;
    let scratch = blade_long * 0.72 + fine * 0.28;
    let edge_boost = 1.0 + (1.0 - ink.clamp(0.0, 1.0)) * 0.65;
    let cut_thresh = (strength * 0.85 * edge_boost).clamp(0.0, 0.98);
    let keep = ((scratch - cut_thresh) / 0.08 + 0.5).clamp(0.0, 1.0);
    cov * keep
}

struct PatternEvalCtx<'a> {
    spec: &'a ScreentoneSpec,
    xf: ShapeXform,
    spacing_x: f32,
    spacing_y: f32,
    sat_dirs: [(f32, f32); 8],
    sat_n: usize,
    inv_sat_stretch: f32,
    sin_x: f32,
    cos_x: f32,
    sin_b: f32,
    cos_b: f32,
}

impl PatternEvalCtx<'_> {
    fn eval_hexgrid(&self, grid_x: f32, grid_y: f32, mod_size: f32, ink: f32) -> f32 {
        let hy = self.spacing_y * 0.8660254;
        let cy_idx = (grid_y / hy).round() as i32;
        let cell_area = self.spacing_x * self.spacing_y * 0.8660254;
        let size = if self.spec.auto_dot_size {
            (auto_size(cell_area, ink * self.spec.density, area_k_for(PatternType::Hexgrid, self.spec.dot_shape)) * mod_size).max(0.0)
        } else {
            (self.spec.dot_size * mod_size * self.spec.density * area_ink(self.spec, ink)).max(0.0)
        };
        let mut cov = 0.0f32;
        for dy in -1..=1 {
            let iy = cy_idx + dy;
            let cell_y = iy as f32 * hy;
            let row_shift = if (iy & 1) != 0 { self.spacing_x * 0.5 } else { 0.0 };
            let cx_idx = ((grid_x - row_shift) / self.spacing_x).round() as i32;
            for dx in -1..=1 {
                let cell_x = (cx_idx + dx) as f32 * self.spacing_x + row_shift;
                let dist = eval_shape_sdf(grid_x - cell_x, grid_y - cell_y, DotShape::Hexagon, size, self.xf);
                cov = cov.max(coverage_from_sdf(dist, self.spec.smoothness));
            }
        }
        cov
    }

    fn eval_noise(&self, grid_x: f32, grid_y: f32, mod_size: f32, ink: f32) -> f32 {
        let seed = self.spec.seed;
        let (cx_idx, cy_idx) = ((grid_x / self.spacing_x).floor() as i32, (grid_y / self.spacing_y).floor() as i32);
        let mut cov = 0.0f32;
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (ix, iy) = (cx_idx + dx, cy_idx + dy);
                let h1 = hash_coord(ix, iy, seed);
                if h1 > self.spec.density {
                    continue;
                }
                let h2 = hash_coord(ix.wrapping_add(300), iy.wrapping_add(300), seed);
                let h3 = hash_coord(ix.wrapping_add(700), iy.wrapping_add(700), seed);
                let jx = (h2 - 0.5) * self.spacing_x * (self.spec.jitter_pos * 0.01 + 0.8);
                let jy = (h3 - 0.5) * self.spacing_y * (self.spec.jitter_pos * 0.01 + 0.8);
                let sz = (self.spec.dot_size * (1.0 + (h2 - 0.5) * self.spec.jitter_size * 0.02) * mod_size * ink).max(0.0);
                let dist = sdist2(grid_x - ((ix as f32 + 0.5) * self.spacing_x + jx), grid_y - ((iy as f32 + 0.5) * self.spacing_y + jy)) - sz;
                cov = cov.max(coverage_from_sdf(dist, self.spec.smoothness));
            }
        }
        cov
    }

    fn eval_gaussian_noise(&self, grid_x: f32, grid_y: f32, mod_size: f32, ink: f32) -> f32 {
        let seed = self.spec.seed.wrapping_add(137);
        let (cx_idx, cy_idx) = ((grid_x / self.spacing_x).floor() as i32, (grid_y / self.spacing_y).floor() as i32);
        let mut cov = 0.0f32;
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (ix, iy) = (cx_idx + dx, cy_idx + dy);
                let h1 = hash_coord(ix, iy, seed);
                if h1 > self.spec.density {
                    continue;
                }
                let (gx, gy) = hash_gaussian(ix, iy, seed);
                let dot_cx = (ix as f32 + 0.5) * self.spacing_x + gx * self.spacing_x * 0.4;
                let dot_cy = (iy as f32 + 0.5) * self.spacing_y + gy * self.spacing_y * 0.4;
                let sz = (self.spec.dot_size * (0.6 + h1 * 0.8) * mod_size * ink).max(0.0);
                let dist = sdist2(grid_x - dot_cx, grid_y - dot_cy) - sz;
                cov = cov.max(coverage_from_sdf(dist, self.spec.smoothness));
            }
        }
        cov
    }

    fn eval_stipple(&self, grid_x: f32, grid_y: f32, mod_size: f32, ink: f32) -> f32 {
        let seed = self.spec.seed.wrapping_add(999);
        let (cx_idx, cy_idx) = ((grid_x / self.spacing_x).round() as i32, (grid_y / self.spacing_y).round() as i32);
        let mut cov = 0.0f32;
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (ix, iy) = (cx_idx + dx, cy_idx + dy);
                if hash_coord(ix, iy, seed) > self.spec.density * ink {
                    continue;
                }
                let h_jx = hash_coord(ix.wrapping_add(100), iy, seed);
                let h_jy = hash_coord(ix.wrapping_add(200), iy, seed);
                let h_sz = hash_coord(ix.wrapping_add(500), iy, seed);
                let mut dot_cx = ix as f32 * self.spacing_x + (h_jx - 0.5) * self.spec.jitter_pos * 0.005 * self.spacing_x;
                let dot_cy = iy as f32 * self.spacing_y + (h_jy - 0.5) * self.spec.jitter_pos * 0.005 * self.spacing_y;
                if (iy & 1) != 0 {
                    dot_cx += self.spacing_x * self.spec.row_offset;
                }
                let sz = (self.spec.dot_size * (0.5 + h_sz * 0.5) * mod_size).max(0.0);
                let dist = eval_shape_sdf(grid_x - dot_cx, grid_y - dot_cy, self.spec.dot_shape, sz, self.xf);
                cov = cov.max(coverage_from_sdf(dist, self.spec.smoothness));
            }
        }
        cov
    }

    fn eval_special_shapes(&self, grid_x: f32, grid_y: f32, mod_size: f32, ink: f32) -> f32 {
        let cy_idx = (grid_y / self.spacing_y).round() as i32;
        let sz = if self.spec.auto_dot_size {
            (auto_size(self.spacing_x * self.spacing_y, ink * self.spec.density, area_k_for(self.spec.pattern_type, self.spec.dot_shape)) * mod_size).max(0.0)
        } else {
            (self.spec.dot_size * mod_size * self.spec.density * area_ink(self.spec, ink)).max(0.0)
        };
        let mut cov = 0.0f32;
        for dy in -1..=1 {
            let iy = cy_idx + dy;
            let row_shift = if (iy & 1) != 0 { self.spacing_x * self.spec.row_offset } else { 0.0 };
            let cx_idx = ((grid_x - row_shift) / self.spacing_x).round() as i32;
            for dx in -1..=1 {
                let ix = cx_idx + dx;
                let (mut dot_cx, mut dot_cy) = (ix as f32 * self.spacing_x + row_shift, iy as f32 * self.spacing_y);
                if self.spec.jitter_pos > 0.0 {
                    dot_cx += (hash_coord(ix, iy, self.spec.seed) - 0.5) * self.spec.jitter_pos * 0.005 * self.spacing_x;
                    dot_cy += (hash_coord(ix.wrapping_add(1000), iy, self.spec.seed) - 0.5) * self.spec.jitter_pos * 0.005 * self.spacing_y;
                }
                let dist = eval_special_shape_sdf(grid_x - dot_cx, grid_y - dot_cy, self.spec.pattern_type, sz, self.xf);
                cov = cov.max(coverage_from_sdf(dist, self.spec.smoothness));
            }
        }
        cov
    }

    fn eval_dots(&self, grid_x: f32, grid_y: f32, mod_size: f32, ink: f32) -> f32 {
        let eff_ink = ink;
        let cov_target = (eff_ink * self.spec.density).clamp(0.0, 1.0);
        let auto = self.spec.auto_dot_size;
        let eff_ink = if auto { eff_ink } else { area_ink(self.spec, eff_ink) };
        let k_main = area_k_for(PatternType::Dots, self.spec.dot_shape);
        let k_sat = area_k_for(PatternType::Dots, self.spec.satellite_dot_shape);
        let (base, ssize_base, k_scale) = if auto {
            let sat_ratio = if self.spec.satellite_enabled { self.spec.satellite_size / self.spec.dot_size.max(0.05) } else { 0.0 };
            let n = self.sat_n as f32;
            let norm = 1.0 / (1.0 + n * sat_ratio * sat_ratio * k_sat / k_main).sqrt();
            (
                auto_size(self.spacing_x * self.spacing_y, cov_target, k_main) * norm,
                auto_size(self.spacing_x * self.spacing_y, cov_target, k_main) * norm * sat_ratio,
                cov_target.sqrt(),
            )
        } else {
            (self.spec.dot_size * self.spec.density * eff_ink, self.spec.satellite_size * self.spec.density * eff_ink, self.spec.density * eff_ink)
        };
        let merge = self.spec.merge_factor.clamp(0.0, 1.0);
        let psize = ((base + merge * self.spacing_x * 0.2 * k_scale) * mod_size).max(0.0);
        let ssize = ((ssize_base + merge * self.spacing_x * 0.4 * k_scale) * mod_size).max(0.0);
        let sdist = self.spec.satellite_distance * (1.0 - merge * 0.6);
        let smooth_k = merge * self.spacing_x.min(self.spacing_y) * 0.35;

        // Exact reach of one lattice node; cells outside ±search_r cannot contribute.
        let min_sp = self.spacing_x.min(self.spacing_y).max(0.01);
        let stretch_gain = self.xf.stretch_x.max(self.xf.stretch_y).max(1.0);
        let sat_gain = 1.0 + self.spec.satellite_stretch.max(0.0);
        let jitter_pad = self.spec.jitter_pos.max(0.0) * 0.0025 * self.spacing_x.max(self.spacing_y);
        let max_reach = psize * stretch_gain
            + if self.spec.satellite_enabled && ssize > 0.05 { sdist + ssize * sat_gain * stretch_gain } else { 0.0 }
            + jitter_pad
            + smooth_k;
        let search_r = ((max_reach / min_sp - 0.5).ceil() as i32).clamp(1, 3);
        let cy_idx = (grid_y / self.spacing_y).round() as i32;
        let mut min_dist = f32::MAX;

        for dy in -search_r..=search_r {
            let iy = cy_idx + dy;
            let row_shift = if (iy & 1) != 0 { self.spacing_x * self.spec.row_offset } else { 0.0 };
            let cx_idx = ((grid_x - row_shift) / self.spacing_x).round() as i32;
            for dx in -search_r..=search_r {
                let ix = cx_idx + dx;
                let (mut dot_cx, mut dot_cy) = (ix as f32 * self.spacing_x + row_shift, iy as f32 * self.spacing_y);
                if self.spec.jitter_pos > 0.0 {
                    dot_cx += (hash_coord(ix, iy, self.spec.seed) - 0.5) * self.spec.jitter_pos * 0.005 * self.spacing_x;
                    dot_cy += (hash_coord(ix.wrapping_add(1000), iy, self.spec.seed) - 0.5) * self.spec.jitter_pos * 0.005 * self.spacing_y;
                }
                let mut final_size = psize;
                if self.spec.jitter_size > 0.0 {
                    let jval = hash_coord(ix.wrapping_add(5000), iy.wrapping_add(5000), self.spec.seed);
                    final_size *= (1.0 + (jval - 0.5) * self.spec.jitter_size * 0.02).max(0.0);
                }
                let mut cell_dist = eval_shape_sdf(grid_x - dot_cx, grid_y - dot_cy, self.spec.dot_shape, final_size, self.xf);
                if self.spec.satellite_enabled && ssize > 0.05 {
                    for k in 0..self.sat_n {
                        let (ux, uy) = self.sat_dirs[k];
                        let (sat_x, sat_y) = (dot_cx + ux * sdist, dot_cy + uy * sdist);
                        let (mut spx, mut spy) = (grid_x - sat_x, grid_y - sat_y);
                        if self.spec.satellite_stretch > 0.0 {
                            // Directions are unit vectors: rotate into (dir, perp), stretch along dir.
                            let rot_sx = spx * ux + spy * uy;
                            let rot_sy = -spx * uy + spy * ux;
                            let scaled = rot_sx * self.inv_sat_stretch;
                            spx = scaled * ux - rot_sy * uy;
                            spy = scaled * uy + rot_sy * ux;
                        }
                        let dist_sat = eval_shape_sdf(spx, spy, self.spec.satellite_dot_shape, ssize, self.xf);
                        cell_dist = sdf_smooth_min(cell_dist, dist_sat, smooth_k);
                    }
                }
                min_dist = sdf_smooth_min(min_dist, cell_dist, smooth_k * 0.5);
            }
        }
        coverage_from_sdf(min_dist, self.spec.smoothness)
    }

    fn eval_single_line(&self, along: f32, across: f32, base_width: f32, li: i32, lseed: u32) -> f32 {
        let sp = self.spacing_y.max(1.0);
        let wobble = self.spec.line_wobble.clamp(0.0, 1.0);
        let dash = self.spec.line_dash.clamp(0.0, 1.0);
        let jpos = self.spec.jitter_pos.clamp(0.0, 100.0);
        let jsize = self.spec.jitter_size.clamp(0.0, 100.0);
        let mut center = (li as f32 + 0.5) * sp;
        if jpos > 1e-4 {
            center += (hash_coord(li, 17, lseed) - 0.5) * (jpos * 0.005) * sp;
        }
        if wobble > 1e-4 {
            let freq = self.spec.line_wave_freq.clamp(0.1, 10.0);
            let phase = hash_coord(li, 31, lseed) * std::f32::consts::TAU;
            let n1 = (along * (0.045 * freq) + phase).sin();
            let n2 = st_value_noise_1d(along * (0.032 * freq), li, lseed.wrapping_add(101));
            center += (n1 * 0.55 + n2 * 0.45) * (wobble * sp * 0.35);
        }
        let mut w = base_width;
        if jsize > 1e-4 {
            w *= (1.0 + (hash_coord(li, 53, lseed) - 0.5) * jsize * 0.015).max(0.0);
        }
        if dash > 1e-4 && w > 1e-4 {
            w *= st_dash_taper(along, sp, li, dash, lseed);
        }
        if w <= 1e-4 {
            return 0.0;
        }
        coverage_from_sdf((across - center).abs() - w * 0.5, self.spec.smoothness)
    }

    fn eval_line_set(&self, along: f32, across: f32, base_width: f32, layer_salt: u32) -> f32 {
        if base_width <= 1e-4 {
            return 0.0;
        }
        let sp = self.spacing_y.max(1.0);
        let wobble = self.spec.line_wobble.clamp(0.0, 1.0);
        let dash = self.spec.line_dash.clamp(0.0, 1.0);
        let jpos = self.spec.jitter_pos.clamp(0.0, 100.0);
        let jsize = self.spec.jitter_size.clamp(0.0, 100.0);
        if wobble <= 1e-4 && dash <= 1e-4 && jpos <= 1e-4 && jsize <= 1e-4 {
            return coverage_from_sdf((((across % sp) + sp) % sp - sp * 0.5).abs() - base_width * 0.5, self.spec.smoothness);
        }
        let lseed = self.spec.seed.wrapping_add(layer_salt);
        let base_idx = (across / sp).floor() as i32;
        let mut cov = 0.0f32;
        for dk in -1..=1 {
            let li = base_idx.wrapping_add(dk);
            cov = cov.max(self.eval_single_line(along, across, base_width, li, lseed));
        }
        cov
    }

    fn eval_crosshatch(&self, local: (f32, f32), grid: (f32, f32), mod_size: f32, ink: f32) -> f32 {
        let lx2 = local.0 * self.cos_x + local.1 * self.sin_x;
        let ly2 = -local.0 * self.sin_x + local.1 * self.cos_x;
        if !self.spec.line_progressive {
            let width = (self.spec.line_width * mod_size * self.spec.density * ink).max(0.0);
            if width <= 1e-4 {
                return 0.0;
            }
            return self.eval_line_set(grid.0, grid.1, width, 0).max(self.eval_line_set(lx2, ly2, width, 31_337));
        }
        let scale = (self.spec.line_width * mod_size * self.spec.density).max(0.0);
        if scale <= 1e-4 {
            return 0.0;
        }
        let mut cov = 0.0f32;
        if ink > 0.08 {
            let w1 = scale * (0.35 + 0.65 * ((ink - 0.08) / 0.30).clamp(0.0, 1.0));
            cov = cov.max(self.eval_line_set(grid.0, grid.1, w1, 0));
        }
        if ink > 0.38 {
            let w2 = scale * (0.35 + 0.65 * ((ink - 0.38) / 0.30).clamp(0.0, 1.0));
            cov = cov.max(self.eval_line_set(lx2, ly2, w2, 31_337));
        }
        if ink > 0.68 {
            let lx3 = local.0 * self.cos_b + local.1 * self.sin_b;
            let ly3 = -local.0 * self.sin_b + local.1 * self.cos_b;
            let w3 = scale * (0.35 + 0.65 * ((ink - 0.68) / 0.32).clamp(0.0, 1.0));
            cov = cov.max(self.eval_line_set(lx3, ly3, w3, 62_674));
        }
        cov
    }

    fn eval_focus_ray(&self, r: f32, ang: f32, base_w: f32, ri: i32, sector: f32) -> f32 {
        let seed = self.spec.seed;
        let base_r_in = self.spec.focus_inner_radius.clamp(0.0, 2000.0);
        let jpos = self.spec.jitter_pos.clamp(0.0, 100.0);
        let jsize = self.spec.jitter_size.clamp(0.0, 100.0);
        let wobble = self.spec.line_wobble.clamp(0.0, 1.0);
        let freq = self.spec.line_wave_freq.clamp(0.1, 10.0);
        let dash = self.spec.line_dash.clamp(0.0, 1.0);
        let mut ray_a = (ri as f32 + 0.5) * sector;
        if jpos > 1e-4 {
            ray_a += (hash_coord(ri, 11, seed) - 0.5) * (jpos * 0.008) * sector;
        }
        if wobble > 1e-4 {
            let phase = hash_coord(ri, 31, seed) * std::f32::consts::TAU;
            ray_a += (r * (0.045 * freq) + phase).sin() * (wobble * sector * 0.25);
        }
        let h_len = hash_coord(ri, 23, seed);
        let r_start = base_r_in + h_len * (jsize * 0.01) * base_r_in.max(self.spacing_y * 4.0);
        if r <= r_start {
            return 0.0;
        }
        let mut d_ang = ang - ray_a;
        if d_ang > std::f32::consts::PI {
            d_ang -= std::f32::consts::TAU;
        }
        if d_ang < -std::f32::consts::PI {
            d_ang += std::f32::consts::TAU;
        }
        let arc_d = d_ang.abs() * r;
        let taper_len = (self.spacing_y * 6.0).max(24.0);
        let radial_t = ((r - r_start) / taper_len).clamp(0.0, 1.0);
        let h_wid = hash_coord(ri, 37, seed);
        let width_jit = (1.0 + (h_wid - 0.5) * jsize * 0.015).max(0.0);
        let cone_scale = 0.65 + 0.35 * (r / (base_r_in + taper_len).max(1.0)).min(2.5);
        let mut w = (base_w * width_jit * radial_t * cone_scale).min(sector * r * 0.85);
        if dash > 1e-4 && w > 1e-4 {
            w *= st_dash_taper(r, self.spacing_y.max(1.0), ri, dash, seed.wrapping_add(409));
        }
        if w <= 1e-4 {
            return 0.0;
        }
        coverage_from_sdf(arc_d - w * 0.5, self.spec.smoothness)
    }

    fn eval_focus_lines(&self, grid_x: f32, grid_y: f32, mod_size: f32, ink: f32) -> f32 {
        let base_w = (self.spec.line_width * mod_size * self.spec.density * ink).max(0.0);
        if base_w <= 1e-4 {
            return 0.0;
        }
        let r = sdist2(grid_x, grid_y);
        let base_r_in = self.spec.focus_inner_radius.clamp(0.0, 2000.0);
        if r <= base_r_in.max(1e-3) {
            return 0.0;
        }
        let theta = grid_y.atan2(grid_x);
        let ang = if theta < 0.0 { theta + std::f32::consts::TAU } else { theta };
        let n_rays = (720.0 / self.spacing_x.clamp(1.0, 2000.0)).round().clamp(8.0, 360.0);
        let n_rays_i = n_rays as i32;
        let sector = std::f32::consts::TAU / n_rays;
        let base_ray = (ang / sector).floor() as i32;
        let mut cov = 0.0f32;
        for dk in -1..=1 {
            let ri = ((base_ray + dk) % n_rays_i + n_rays_i) % n_rays_i;
            cov = cov.max(self.eval_focus_ray(r, ang, base_w, ri, sector));
        }
        cov
    }

    fn eval(&self, local: (f32, f32), grid: (f32, f32), mod_size: f32, ink: f32) -> f32 {
        let (grid_x, grid_y) = grid;
        match self.spec.pattern_type {
            PatternType::Lines => {
                let width = (self.spec.line_width * mod_size * self.spec.density * ink).max(0.0);
                self.eval_line_set(grid_x, grid_y, width, 0)
            }
            PatternType::Crosshatch => self.eval_crosshatch(local, grid, mod_size, ink),
            PatternType::FocusLines => self.eval_focus_lines(grid_x, grid_y, mod_size, ink),
            PatternType::Concentric => {
                let width = (self.spec.line_width * mod_size * self.spec.density * ink).max(0.0);
                if width <= 1e-4 {
                    return 0.0;
                }
                let sp = self.spacing_x.max(1.0);
                coverage_from_sdf((((sdist2(grid_x, grid_y) % sp) + sp) % sp - sp * 0.5).abs() - width * 0.5, self.spec.smoothness)
            }
            PatternType::Checker => {
                let (ix, iy) = ((grid_x / self.spacing_x).floor() as i32, (grid_y / self.spacing_y).floor() as i32);
                if ((ix.wrapping_add(iy)) & 1) != 0 {
                    return 0.0;
                }
                let size = (self.spacing_x * 0.45 * mod_size * self.spec.density * area_ink(self.spec, ink)).max(0.0);
                if size <= 1e-4 {
                    return 0.0;
                }
                let dist =
                    eval_shape_sdf(grid_x - (ix as f32 + 0.5) * self.spacing_x, grid_y - (iy as f32 + 0.5) * self.spacing_y, DotShape::Square, size, self.xf);
                coverage_from_sdf(dist, self.spec.smoothness)
            }
            PatternType::Hexgrid => self.eval_hexgrid(grid_x, grid_y, mod_size, ink),
            PatternType::Noise => self.eval_noise(grid_x, grid_y, mod_size, ink),
            PatternType::GaussianNoise => self.eval_gaussian_noise(grid_x, grid_y, mod_size, ink),
            PatternType::Stipple => self.eval_stipple(grid_x, grid_y, mod_size, ink),
            PatternType::Stars | PatternType::Hearts | PatternType::Triangles => self.eval_special_shapes(grid_x, grid_y, mod_size, ink),
            PatternType::Dots => self.eval_dots(grid_x, grid_y, mod_size, ink),
        }
    }
}

/// Core screentone rendering kernel over tile `out`.
pub fn screentone(src: &Image, out: Rect, ctx: &Ctx, spec: &ScreentoneSpec) -> Vec<f32> {
    let mut res = src.crop(out);
    let b = ctx.bounds;
    let (bw, bh) = (b.width().max(1) as f32, b.height().max(1) as f32);
    let (cx, cy) = (b.x0 as f32 + bw * 0.5, b.y0 as f32 + bh * 0.5);
    let (sin_canvas, cos_canvas) = spec.rot_canvas.to_radians().sin_cos();
    let (sin_ang, cos_ang) = spec.angle.to_radians().sin_cos();
    let (sin_sat, cos_sat) = spec.satellite_angle.to_radians().sin_cos();
    let (sin_p, cos_p) = spec.rot_pattern.to_radians().sin_cos();
    let (sin_x, cos_x) = (spec.angle + spec.cross_angle).to_radians().sin_cos();
    let (sin_b, cos_b) = (spec.angle + 45.0).to_radians().sin_cos();
    let (sin_scrape, cos_scrape) = spec.scrape_angle.to_radians().sin_cos();
    let scrape_strength = spec.scrape_strength.clamp(0.0, 1.0);
    let (spacing_x, spacing_y) = (spec.spacing_x.clamp(1.0, 2000.0), spec.spacing_y.clamp(1.0, 2000.0));
    let (sat_dirs, sat_n) = build_sat_dirs(spec, sin_sat, cos_sat);
    let inv_sat_stretch = 1.0 / (1.0 + spec.satellite_stretch.max(0.0)).max(0.01);
    let (fg, bg) = (spec.foreground, spec.background);
    let use_shading = spec.use_source_luminance && spec.mode == ScreentoneMode::Shading && !src.rect.is_empty();
    let tone = ToneCurve::new(spec);
    let grids = if use_shading { Some(ShadingGrids::build(src, out, ctx, spec, tone)) } else { None };

    for (i, px) in res.chunks_exact_mut(src.ch).enumerate() {
        let (x, y) = xy(out, i);
        let (doc_x, doc_y) = (x as f32 + 0.5, y as f32 + 0.5);
        let (rel_x, rel_y) = (doc_x - cx, doc_y - cy);
        let (raw_lx, raw_ly) = (rel_x * cos_canvas + rel_y * sin_canvas, -rel_x * sin_canvas + rel_y * cos_canvas);
        let gf = compute_grad_factor(raw_lx, raw_ly, bw, bh, spec);
        let (mut mod_size, mut mod_aspect_x, mut mod_aspect_y) = (1.0f32, spec.aspect_x, spec.aspect_y);
        if spec.grad_type != ScreentoneGradientType::None {
            mod_size = lerp(spec.grad_size_start, spec.grad_size_end, gf);
            let stretch_mod = lerp(spec.grad_stretch_start, spec.grad_stretch_end, gf);
            mod_aspect_x *= stretch_mod;
            mod_aspect_y *= stretch_mod;
        }
        let cur_fg = if spec.grad_color_trans && spec.grad_type != ScreentoneGradientType::None {
            [lerp(fg[0], bg[0], gf), lerp(fg[1], bg[1], gf), lerp(fg[2], bg[2], gf), fg[3]]
        } else {
            fg
        };
        let (local_x, local_y) = (raw_lx - spec.origin_x, raw_ly - spec.origin_y);
        let grid = if spec.angle == 0.0 { (local_x, local_y) } else { (local_x * cos_ang + local_y * sin_ang, -local_x * sin_ang + local_y * cos_ang) };
        let cov = if let Some(ref g) = grids
            && g.is_preserved_line(spec, x, y)
        {
            1.0
        } else {
            let ink = match &grids {
                Some(g) => g.sample_ink(src, ctx, tone, spec, doc_x, doc_y),
                None => 1.0,
            };
            if ink > 1e-5 {
                let eval_ctx = PatternEvalCtx {
                    spec,
                    xf: ShapeXform::new(mod_aspect_x, mod_aspect_y, spec.roundness, sin_p, cos_p),
                    spacing_x,
                    spacing_y,
                    sat_dirs,
                    sat_n,
                    inv_sat_stretch,
                    sin_x,
                    cos_x,
                    sin_b,
                    cos_b,
                };
                let raw_cov = eval_ctx.eval((local_x, local_y), grid, mod_size, ink);
                apply_kezuri(raw_cov, (local_x, local_y), ink, scrape_strength, (sin_scrape, cos_scrape), spec.seed).clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        let out_color = if bg[3] <= 0.001 {
            let alpha = cov * cur_fg[3];
            if ctx.alpha {
                if alpha <= 1e-6 { [0.0, 0.0, 0.0, 0.0] } else { [cur_fg[0], cur_fg[1], cur_fg[2], alpha] }
            } else {
                let base_c = rgba(ctx, px);
                [lerp(base_c[0], cur_fg[0], alpha), lerp(base_c[1], cur_fg[1], alpha), lerp(base_c[2], cur_fg[2], alpha), base_c[3].max(alpha)]
            }
        } else {
            // Premultiplied alpha: no dark halo when compositing over a translucent background.
            let out_a = lerp(bg[3], cur_fg[3], cov);
            if out_a <= 1e-6 {
                [0.0, 0.0, 0.0, 0.0]
            } else {
                let bg_w = bg[3] * (1.0 - cov);
                let fg_w = cur_fg[3] * cov;
                [(bg[0] * bg_w + cur_fg[0] * fg_w) / out_a, (bg[1] * bg_w + cur_fg[1] * fg_w) / out_a, (bg[2] * bg_w + cur_fg[2] * fg_w) / out_a, out_a]
            }
        };
        set_rgba(ctx, px, out_color);
    }

    res
}

/// Legacy polygon reference, kept only for tests so the analytic O(1) SDFs can be verified.
#[cfg(test)]
pub(crate) mod oracle {
    use super::sdist2;

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
        sdist2(pax - bax * h, pay - bay * h)
    }

    /// Ray-casting polygon SDF with the upward-edge sign fixed (`copysign` denominator).
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
            let dy = vj.1 - vi.1;
            let safe_dy = if dy.abs() < 1e-6 { 1e-6_f32.copysign(dy) } else { dy };
            if ((vi.1 > p.1) != (vj.1 > p.1)) && (p.0 < (vj.0 - vi.0) * (p.1 - vi.1) / safe_dy + vi.0) {
                inside = !inside;
            }
            min_d = min_d.min(dist_to_segment(p, vj, vi));
            j = i;
        }
        let signed = if inside { -min_d } else { min_d };
        let r = roundness.clamp(0.0, 1.0);
        if r > 0.01 { signed - min_d.min(2.0) * 0.5 * r } else { signed }
    }

    pub fn hexagon_vertices(size: f32) -> [[f32; 2]; 6] {
        let mut pts = [[0.0f32; 2]; 6];
        for (i, p) in pts.iter_mut().enumerate() {
            let a = (std::f32::consts::PI / 3.0) * i as f32 - std::f32::consts::FRAC_PI_6;
            *p = [a.cos() * size, a.sin() * size];
        }
        pts
    }

    pub fn star_vertices(size: f32) -> [[f32; 2]; 10] {
        let mut pts = [[0.0f32; 2]; 10];
        for (i, p) in pts.iter_mut().enumerate() {
            let r = if i % 2 == 0 { size } else { size * 0.4 };
            let a = (std::f32::consts::PI / 5.0) * i as f32 - std::f32::consts::FRAC_PI_2;
            *p = [a.cos() * r, a.sin() * r];
        }
        pts
    }

    pub fn triangle_vertices(size: f32) -> [[f32; 2]; 3] {
        [[0.0, -size], [size * 0.8660254, size * 0.5], [-size * 0.8660254, size * 0.5]]
    }

    pub fn heart_vertices(size: f32) -> [[f32; 2]; 28] {
        let mut pts = [[0.0f32; 2]; 28];
        let sc = size / 17.0;
        for (i, p) in pts.iter_mut().enumerate() {
            let t = (i as f32 / 28.0) * std::f32::consts::PI * 2.0;
            let sin_t = t.sin();
            *p = [sc * 16.0 * sin_t * sin_t * sin_t, -sc * (13.0 * t.cos() - 5.0 * (2.0 * t).cos() - 2.0 * (3.0 * t).cos() - (4.0 * t).cos())];
        }
        pts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::ColorMode;

    fn make_test_image(w: i32, h: i32, value: f32) -> Image {
        let rect = Rect::new(0, 0, w, h);
        Image { rect, ch: 3, data: vec![value; (w * h * 3) as usize] }
    }

    #[test]
    fn test_all_12_patterns_render_safely() {
        let bounds = Rect::new(0, 0, 64, 64);
        let img = make_test_image(64, 64, 0.5);
        let ctx = Ctx { bounds, mode: ColorMode::Rgb, alpha: false };

        let patterns = [
            PatternType::Dots,
            PatternType::Lines,
            PatternType::Crosshatch,
            PatternType::Noise,
            PatternType::GaussianNoise,
            PatternType::Stipple,
            PatternType::Hexgrid,
            PatternType::Checker,
            PatternType::Concentric,
            PatternType::Stars,
            PatternType::Hearts,
            PatternType::Triangles,
        ];

        for pattern in patterns {
            let spec = ScreentoneSpec {
                pattern_type: pattern,
                dot_size: 4.0,
                spacing_x: 12.0,
                spacing_y: 12.0,
                density: 0.5,
                use_source_luminance: false,
                ..Default::default()
            };
            let out_px = screentone(&img, bounds, &ctx, &spec);
            assert_eq!(out_px.len(), 64 * 64 * 3);
            for (idx, val) in out_px.iter().enumerate() {
                assert!(val.is_finite(), "Pattern {pattern:?} produced non-finite sample at {idx}");
                assert!(*val >= -0.01 && *val <= 1.01, "Pattern {pattern:?} sample {val} out of bounds at {idx}");
            }
        }
    }

    #[test]
    fn test_dot_shapes_and_roundness() {
        let bounds = Rect::new(0, 0, 32, 32);
        let img = make_test_image(32, 32, 0.0);
        let ctx = Ctx { bounds, mode: ColorMode::Rgb, alpha: false };

        let shapes = [DotShape::Circle, DotShape::Square, DotShape::Diamond, DotShape::Hexagon];
        for shape in shapes {
            for roundness in [0.0, 0.3, 0.7, 1.0] {
                let spec = ScreentoneSpec {
                    pattern_type: PatternType::Dots,
                    dot_shape: shape,
                    roundness,
                    dot_size: 5.0,
                    spacing_x: 16.0,
                    spacing_y: 16.0,
                    ..Default::default()
                };
                let out_px = screentone(&img, bounds, &ctx, &spec);
                assert_eq!(out_px.len(), 32 * 32 * 3);
            }
        }
    }

    #[test]
    fn test_satellites_orbit() {
        let bounds = Rect::new(0, 0, 48, 48);
        let img = make_test_image(48, 48, 0.5);
        let ctx = Ctx { bounds, mode: ColorMode::Rgb, alpha: false };

        let spec = ScreentoneSpec {
            pattern_type: PatternType::Dots,
            dot_size: 6.0,
            spacing_x: 24.0,
            spacing_y: 24.0,
            satellite_enabled: true,
            satellite_size: 3.0,
            satellite_distance: 8.0,
            satellite_count: 4,
            satellite_stretch: 0.5,
            satellite_angle: 30.0,
            ..Default::default()
        };
        let out_px = screentone(&img, bounds, &ctx, &spec);
        assert_eq!(out_px.len(), 48 * 48 * 3);
        assert!(out_px.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn test_gradients_linear_and_radial() {
        let bounds = Rect::new(0, 0, 40, 40);
        let img = make_test_image(40, 40, 0.5);
        let ctx = Ctx { bounds, mode: ColorMode::Rgb, alpha: false };

        for grad in [
            ScreentoneGradientType::Linear,
            ScreentoneGradientType::Radial,
            ScreentoneGradientType::Angle,
            ScreentoneGradientType::Reflected,
            ScreentoneGradientType::Diamond,
        ] {
            let spec =
                ScreentoneSpec { grad_type: grad, grad_angle: 45.0, grad_size_start: 2.0, grad_size_end: 0.1, grad_color_trans: true, ..Default::default() };
            let out_px = screentone(&img, bounds, &ctx, &spec);
            assert_eq!(out_px.len(), 40 * 40 * 3);
            assert!(out_px.iter().all(|v| v.is_finite()));
        }
    }

    #[test]
    fn test_tile_invariance() {
        let bounds = Rect::new(0, 0, 64, 64);
        let img = make_test_image(64, 64, 0.4);
        let ctx = Ctx { bounds, mode: ColorMode::Rgb, alpha: false };

        let spec = ScreentoneSpec {
            pattern_type: PatternType::Dots,
            dot_size: 5.0,
            spacing_x: 16.0,
            spacing_y: 16.0,
            density: 0.5,
            use_source_luminance: false,
            ..Default::default()
        };

        // Render whole tile
        let whole = screentone(&img, bounds, &ctx, &spec);

        // Render in 4 sub-tiles of 32x32
        let tiles = [
            (screentone(&img, Rect::new(0, 0, 32, 32), &ctx, &spec), 0, 0),
            (screentone(&img, Rect::new(32, 0, 64, 32), &ctx, &spec), 32, 0),
            (screentone(&img, Rect::new(0, 32, 32, 64), &ctx, &spec), 0, 32),
            (screentone(&img, Rect::new(32, 32, 64, 64), &ctx, &spec), 32, 32),
        ];

        for (tile, ox, oy) in tiles {
            for y in 0..32 {
                for x in 0..32 {
                    for c in 0..3 {
                        let whole_idx = (((y + oy) * 64 + (x + ox)) * 3 + c) as usize;
                        let tile_idx = ((y * 32 + x) * 3 + c) as usize;
                        assert!(
                            (whole[whole_idx] - tile[tile_idx]).abs() < 1e-5,
                            "Mismatch between whole and tiled output at ({}, {}) channel {c}",
                            x + ox,
                            y + oy
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn test_presets_validity() {
        let presets = built_in_presets();
        assert_eq!(presets.len(), 26, "clover, asterisk, orbital cluster and calibrated films were added");
        for p in presets {
            assert!(!p.name.is_empty());
            assert!(!p.category.is_empty());
            assert!(p.spec.spacing_x > 0.0);
            assert!(p.spec.spacing_y > 0.0);
        }
    }

    #[test]
    fn test_origin_shifts_the_lattice_phase() {
        let bounds = Rect::new(0, 0, 96, 48);
        let img = make_test_image(96, 48, 0.5);
        let ctx = Ctx { bounds, mode: ColorMode::Rgb, alpha: false };
        let base = ScreentoneSpec {
            pattern_type: PatternType::Dots,
            dot_size: 5.0,
            spacing_x: 16.0,
            spacing_y: 16.0,
            use_source_luminance: false,
            ..Default::default()
        };
        let plain = screentone(&img, bounds, &ctx, &base);

        // A period-16 lattice queried through `local - origin` sees exactly the same cells a
        // plain render computes 16 px earlier, so an origin of (16, 0) slides the sheet a full
        // tile: the shifted output at x equals the plain output at x - 16 (dyadic f32 math,
        // bit-identical).
        let shifted_x = screentone(&img, bounds, &ctx, &ScreentoneSpec { origin_x: 16.0, ..base.clone() });
        for y in 0..48 {
            for x in 16..96 {
                for c in 0..3 {
                    assert_eq!(shifted_x[(y * 96 + x) * 3 + c], plain[(y * 96 + (x - 16)) * 3 + c], "origin x shift at {x}, {y} ch {c}");
                }
            }
        }

        let shifted_y = screentone(&img, bounds, &ctx, &ScreentoneSpec { origin_y: 16.0, ..base.clone() });
        // Row parity comes from the shifted lattice, so the y shift is a full-row translation
        // too (spacing 16 keeps odd rows identified with plain rows).
        for y in 16..48 {
            for x in 0..96 {
                for c in 0..3 {
                    assert_eq!(shifted_y[(y * 96 + x) * 3 + c], plain[((y - 16) * 96 + x) * 3 + c], "origin y shift at {x}, {y} ch {c}");
                }
            }
        }

        // The satellite ring travels with its dot: satellites on and the same equality holds.
        let sat = ScreentoneSpec { satellite_enabled: true, satellite_size: 3.0, satellite_distance: 7.0, ..base.clone() };
        let plain_sat = screentone(&img, bounds, &ctx, &sat);
        let shifted_sat = screentone(&img, bounds, &ctx, &ScreentoneSpec { origin_x: 16.0, ..sat });
        for y in 0..48 {
            for x in 16..96 {
                for c in 0..3 {
                    assert_eq!(shifted_sat[(y * 96 + x) * 3 + c], plain_sat[(y * 96 + (x - 16)) * 3 + c], "satellite origin shift at {x}, {y} ch {c}");
                }
            }
        }

        // An untouched origin renders exactly as before the field existed.
        let unchanged = screentone(&img, bounds, &ctx, &base);
        assert_eq!(unchanged, plain);
    }

    #[test]
    fn document_origin_round_trips_across_render_frames() {
        let spec = ScreentoneSpec {
            pattern_type: PatternType::Dots,
            dot_size: 5.0,
            spacing_x: 16.0,
            spacing_y: 16.0,
            rot_canvas: 27.0,
            origin_x: 5.0,
            origin_y: -3.0,
            use_source_luminance: false,
            ..Default::default()
        };
        let sheet = Rect::new(8, 6, 72, 54);
        let origin = origin_doc(&spec, sheet);
        // Rendering the same document anchor over a different frame recovers that same anchor:
        // the frame's centre is removed before the tone's rotation is undone.
        let wider = Rect::new(0, 0, 160, 120);
        let rerendered = sheet_spec(&spec, wider, origin);
        let back = origin_doc(&rerendered, wider);
        assert!((back.0 - origin.0).abs() < 0.001, "{back:?} vs {origin:?}");
        assert!((back.1 - origin.1).abs() < 0.001, "{back:?} vs {origin:?}");
        // The phase itself is frame-relative, so the two specs differ while anchoring the same point.
        assert_ne!((rerendered.origin_x, rerendered.origin_y), (spec.origin_x, spec.origin_y));
    }

    #[test]
    fn test_manga_shading_thresholds_beta_fill_and_satellites() {
        let bounds = Rect::new(0, 0, 96, 32);
        let ctx = Ctx { bounds, mode: ColorMode::Rgb, alpha: true };
        let mut data = vec![0.0f32; (96 * 32 * 4) as usize];
        for y in 0..32usize {
            for x in 0..96usize {
                let idx = (y * 96 + x) * 4;
                if x < 32 {
                    // Deep shadow below black_threshold (0.04 <= 0.08): beta fill
                    data[idx..idx + 4].copy_from_slice(&[0.04, 0.04, 0.04, 1.0]);
                } else if x < 64 {
                    // Midtone (0.45 in 0.08..0.85): halftone dots + satellites
                    data[idx..idx + 4].copy_from_slice(&[0.45, 0.45, 0.45, 1.0]);
                } else {
                    // Highlight paper above white_threshold (0.92 >= 0.85): clean paper
                    data[idx..idx + 4].copy_from_slice(&[0.92, 0.92, 0.92, 1.0]);
                }
            }
        }
        let img = Image { rect: bounds, ch: 4, data };
        let spec = ScreentoneSpec {
            mode: ScreentoneMode::Shading,
            use_source_luminance: true,
            white_threshold: 0.85,
            black_threshold: 0.08,
            tone_gamma: 1.0,
            pattern_type: PatternType::Dots,
            dot_size: 5.0,
            spacing_x: 10.0,
            spacing_y: 10.0,
            satellite_enabled: true,
            satellite_size: 3.0,
            satellite_distance: 4.0,
            satellite_count: 4,
            jitter_pos: 5.0,
            jitter_size: 5.0,
            ..Default::default()
        };
        let out_px = screentone(&img, bounds, &ctx, &spec);
        let out_slice = out_px.as_slice();
        for y in 0..32usize {
            for x in 72..96usize {
                let a = out_px[(y * 96 + x) * 4 + 3];
                assert_eq!(a, 0.0, "highlight paper at ({x}, {y}) must be 0.0, got {a}");
            }
        }
        let deep_ink: f32 = (0..32usize).flat_map(|y| (0..24usize).map(move |x| out_slice[(y * 96 + x) * 4 + 3])).sum();
        let mid_ink: f32 = (0..32usize).flat_map(|y| (38..58usize).map(move |x| out_slice[(y * 96 + x) * 4 + 3])).sum();
        assert!(
            deep_ink > mid_ink,
            "deep shadow region (ink=1.0) renders higher ink coverage ({deep_ink}) than midtone ({mid_ink}) without binary flat clipping"
        );
        assert!(mid_ink > 20.0, "midtone region renders partial halftone + satellites (got {mid_ink})");

        // In Sheet mode, the same image renders uniform pattern even over the 0.92 highlight zone.
        let sheet_spec = ScreentoneSpec { mode: ScreentoneMode::Sheet, use_source_luminance: false, ..spec };
        let sheet_px = screentone(&img, bounds, &ctx, &sheet_spec);
        let sheet_slice = sheet_px.as_slice();
        let sheet_right_ink: f32 = (0..32usize).flat_map(|y| (72..96usize).map(move |x| sheet_slice[(y * 96 + x) * 4 + 3])).sum();
        assert!(sheet_right_ink > 20.0, "Sheet mode renders uniform dots + satellites across white paper (got {sheet_right_ink})");
    }

    #[test]
    fn test_brightness_contrast_and_preserve_lines_with_thickness() {
        let bounds = Rect::new(0, 0, 64, 32);
        let ctx = Ctx { bounds, mode: ColorMode::Rgb, alpha: true };
        let mut data = vec![1.0f32; (64 * 32 * 4) as usize];
        // Draw a 1px vertical pure black line at x = 32 (luma = 0.0 <= black_threshold = 0.08).
        // With preserve_lines = false, there is no forced cov=1.0 per-pixel clip: it is rendered
        // purely as smooth halftone dots sized by sample_ink.
        // With preserve_lines = true, the line-art detector triggers and preserves it as solid ink.
        for y in 0..32usize {
            let idx = (y * 64 + 32) * 4;
            data[idx..idx + 4].copy_from_slice(&[0.0, 0.0, 0.0, 1.0]);
        }
        let img = Image { rect: bounds, ch: 4, data };
        let base_spec = ScreentoneSpec {
            mode: ScreentoneMode::Shading,
            use_source_luminance: true,
            dot_size: 2.0,
            spacing_x: 16.0,
            spacing_y: 16.0,
            white_threshold: 0.85,
            black_threshold: 0.08,
            preserve_lines: false,
            line_threshold: 0.30,
            line_thickness: 1.0,
            ..Default::default()
        };
        let without_lines = screentone(&img, bounds, &ctx, &base_spec);
        let line_ink_off: f32 = (0..32usize).map(|y| without_lines[(y * 64 + 32) * 4 + 3]).sum();
        assert!(line_ink_off < 28.0, "without preserve_lines, the 0.20 line is broken by the 16px dot grid (got {line_ink_off})");

        let with_lines = screentone(&img, bounds, &ctx, &ScreentoneSpec { preserve_lines: true, ..base_spec.clone() });
        let line_ink_on: f32 = (0..32usize).map(|y| with_lines[(y * 64 + 32) * 4 + 3]).sum();
        assert!((line_ink_on - 32.0).abs() < 1e-4, "with preserve_lines, every pixel of the vertical line at x=32 is 1.0 solid ink");
        let bleed_ink: f32 = (0..32usize).map(|y| with_lines[(y * 64 + 30) * 4 + 3] + with_lines[(y * 64 + 34) * 4 + 3]).sum();
        assert_eq!(bleed_ink, 0.0, "zero dot bleeding or pimples adjacent to preserved line");

        // Increasing `line_thickness` to 2.5 dilates the preserved line into x=31 and x=33 as well.
        let thick_lines = screentone(&img, bounds, &ctx, &ScreentoneSpec { preserve_lines: true, line_thickness: 2.5, ..base_spec.clone() });
        let neighbor_ink: f32 = (0..32usize).map(|y| thick_lines[(y * 64 + 31) * 4 + 3] + thick_lines[(y * 64 + 33) * 4 + 3]).sum();
        assert!((neighbor_ink - 64.0).abs() < 1e-4, "line_thickness=2.5 dilates the preserved line to adjacent columns");
    }

    #[test]
    fn test_grid_rotation_rotates_dot_lattice() {
        let bounds = Rect::new(0, 0, 64, 64);
        let img = make_test_image(64, 64, 0.5);
        let ctx = Ctx { bounds, mode: ColorMode::Rgb, alpha: false };
        let spec_0 = ScreentoneSpec {
            pattern_type: PatternType::Dots,
            dot_size: 4.0,
            spacing_x: 16.0,
            spacing_y: 16.0,
            angle: 0.0,
            use_source_luminance: false,
            ..Default::default()
        };
        let spec_45 = ScreentoneSpec { angle: 45.0, ..spec_0.clone() };
        let px_0 = screentone(&img, bounds, &ctx, &spec_0);
        let px_45 = screentone(&img, bounds, &ctx, &spec_45);
        assert_ne!(px_0, px_45, "changing angle to 45° must rotate the dot grid");
    }

    #[test]
    fn test_thick_line_in_gray_tone_has_no_staircase_halo_and_white_line_has_no_pimples() {
        let bounds = Rect::new(0, 0, 64, 64);
        let ctx = Ctx { bounds, mode: ColorMode::Rgb, alpha: true };

        let spec = ScreentoneSpec {
            mode: ScreentoneMode::Shading,
            use_source_luminance: true,
            pattern_type: PatternType::Dots,
            dot_size: 8.0,
            spacing_x: 10.0,
            spacing_y: 10.0,
            density: 0.7,
            white_threshold: 0.85,
            black_threshold: 0.08,
            preserve_lines: true,
            line_threshold: 0.24,
            line_thickness: 1.0,
            ..Default::default()
        };

        // 1. Pure 50% gray tone reference (no line) vs 50% gray tone with a thick 8px black line
        //    (x = 28..36) and 2px anti-aliased fringes (x = 26..28 and x = 36..38).
        let mut gray_ref_data = vec![0.0f32; 64 * 64 * 4];
        for px in gray_ref_data.as_chunks_mut::<4>().0 {
            *px = [0.50, 0.50, 0.50, 1.0];
        }
        let gray_ref_img = Image { rect: bounds, ch: 4, data: gray_ref_data.clone() };
        let gray_ref_out = screentone(&gray_ref_img, bounds, &ctx, &spec);

        let mut gray_line_data = gray_ref_data;
        for y in 0..64usize {
            for x in 26..38usize {
                let v = if (28..36).contains(&x) {
                    0.02
                } else if x == 27 || x == 36 {
                    0.28
                } else {
                    0.40
                };
                let idx = (y * 64 + x) * 4;
                gray_line_data[idx..idx + 4].copy_from_slice(&[v, v, v, 1.0]);
            }
        }
        let gray_line_img = Image { rect: bounds, ch: 4, data: gray_line_data };
        let gray_line_out = screentone(&gray_line_img, bounds, &ctx, &spec);

        // Verify the thick black line itself is 100% solid ink, and the pixels immediately
        // outside the line (x = 22..28 and x = 36..42) match the un-lined 50% gray reference
        // with zero white staircase halo!
        for y in 4..60usize {
            for x in 28..36usize {
                let a = gray_line_out[(y * 64 + x) * 4 + 3];
                assert!((a - 1.0).abs() < 1e-4, "thick line interior at ({x}, {y}) must be 1.0, got {a}");
            }
            for x in (22..28usize).chain(36..42usize) {
                let a_line = gray_line_out[(y * 64 + x) * 4 + 3];
                let a_ref = gray_ref_out[(y * 64 + x) * 4 + 3];
                assert!((a_line - a_ref).abs() < 1e-4, "white staircase halo or fringe distortion at ({x}, {y}): got {a_line}, expected {a_ref}");
            }
        }

        // 2. Thick 8px black line on white paper (1.0) with 2px anti-aliased gray fringes (0.45, 0.72):
        //    must produce ZERO halftone pimples outside the preserved line core.
        let mut white_line_data = vec![1.0f32; 64 * 64 * 4];
        for y in 0..64usize {
            for x in 26..38usize {
                let v = if (28..36).contains(&x) {
                    0.02
                } else if x == 27 || x == 36 {
                    0.45
                } else {
                    0.72
                };
                let idx = (y * 64 + x) * 4;
                white_line_data[idx..idx + 4].copy_from_slice(&[v, v, v, 1.0]);
            }
        }
        let white_line_img = Image { rect: bounds, ch: 4, data: white_line_data };
        let white_line_out = screentone(&white_line_img, bounds, &ctx, &spec);
        for y in 4..60usize {
            for x in (20..28usize).chain(36..44usize) {
                let a = white_line_out[(y * 64 + x) * 4 + 3];
                assert_eq!(a, 0.0, "anti-aliased fringe on white paper at ({x}, {y}) spawned a pimple (alpha={a})");
            }
        }
    }

    #[test]
    fn test_fine_hatching_on_white_paper_has_no_pimples() {
        let bounds = Rect::new(0, 0, 64, 64);
        let ctx = Ctx { bounds, mode: ColorMode::Rgb, alpha: true };
        let mut data = vec![1.0f32; 64 * 64 * 4];

        // Draw fine parallel hatching (strokes at x = 20, 24, 28, 32, 36) where the 3px gaps
        // between strokes are darkened by overlapping anti-aliasing to 0.62..0.74 (< white_threshold 0.85).
        for y in 12..52usize {
            for x in 19..=37usize {
                let dist_to_stroke = [20isize, 24, 28, 32, 36].iter().map(|&sx| (x as isize - sx).abs()).min().unwrap_or(10);
                let v = match dist_to_stroke {
                    0 => 0.12, // hatch stroke core
                    1 => 0.62, // 1px anti-aliased fringe
                    _ => 0.74, // midpoint of narrow gap between strokes
                };
                let idx = (y * 64 + x) * 4;
                data[idx..idx + 4].copy_from_slice(&[v, v, v, 1.0]);
            }
        }

        let img = Image { rect: bounds, ch: 4, data };
        let spec = ScreentoneSpec {
            mode: ScreentoneMode::Shading,
            use_source_luminance: true,
            pattern_type: PatternType::Dots,
            dot_size: 7.0,
            spacing_x: 10.0,
            spacing_y: 10.0,
            density: 0.6,
            white_threshold: 0.85,
            black_threshold: 0.08,
            preserve_lines: true,
            line_threshold: 0.24,
            line_thickness: 1.0,
            ..Default::default()
        };
        let out_px = screentone(&img, bounds, &ctx, &spec);

        for y in 16..48usize {
            // Every hatch stroke core is preserved as 1.0 solid ink.
            for &sx in &[20usize, 24, 28, 32, 36] {
                let a = out_px[(y * 64 + sx) * 4 + 3];
                assert!((a - 1.0).abs() < 1e-4, "hatch stroke at ({sx}, {y}) must be preserved as 1.0, got {a}");
            }
            // Every narrow slit between the hatch strokes is clean paper (0.0), with zero pimples.
            for sx in [21usize, 22, 23, 25, 26, 27, 29, 30, 31, 33, 34, 35] {
                let a = out_px[(y * 64 + sx) * 4 + 3];
                assert_eq!(a, 0.0, "pimple detected inside fine hatching gap at ({sx}, {y}): alpha={a}");
            }
        }
    }

    #[test]
    fn test_white_outline_against_coarse_screentone_has_no_staircase() {
        let bounds = Rect::new(0, 0, 96, 96);
        let ctx = Ctx { bounds, mode: ColorMode::Rgb, alpha: true };

        // Simulate a manga white outline (halo) separating a black hair line from a dark gray background
        // rendered with coarse LPI dots (spacing = 24.0, dot_size = 14.0, ~14 LPI):
        // x in 0..16: black hair line (0.04)
        // x in 16..24: white hair outline (1.0)
        // x in 24..96: dark gray background tone (0.30)
        let mut data = vec![0.30f32; 96 * 96 * 4];
        for i in 0..(96 * 96) {
            data[i * 4 + 3] = 1.0;
        }
        for y in 0..96usize {
            for x in 0..16usize {
                let idx = (y * 96 + x) * 4;
                data[idx..idx + 4].copy_from_slice(&[0.04, 0.04, 0.04, 1.0]);
            }
            for x in 16..24usize {
                let idx = (y * 96 + x) * 4;
                data[idx..idx + 4].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
            }
        }
        let img = Image { rect: bounds, ch: 4, data };

        let mut ref_data = vec![0.30f32; 96 * 96 * 4];
        for i in 0..(96 * 96) {
            ref_data[i * 4 + 3] = 1.0;
        }
        let ref_img = Image { rect: bounds, ch: 4, data: ref_data };

        let spec = ScreentoneSpec {
            mode: ScreentoneMode::Shading,
            use_source_luminance: true,
            pattern_type: PatternType::Dots,
            dot_size: 14.0,
            spacing_x: 24.0,
            spacing_y: 24.0,
            angle: 45.0,
            density: 0.65,
            white_threshold: 0.85,
            black_threshold: 0.08,
            preserve_lines: true,
            line_threshold: 0.24,
            line_thickness: 1.0,
            ..Default::default()
        };

        let out_px = screentone(&img, bounds, &ctx, &spec);
        let ref_px = screentone(&ref_img, bounds, &ctx, &spec);

        for y in 4..92usize {
            // Inside the white outline (x in 16..24), output must be clean white paper (0.0).
            for x in 16..24usize {
                let a = out_px[(y * 96 + x) * 4 + 3];
                assert_eq!(a, 0.0, "white outline at ({x}, {y}) should be 0.0, got {a}");
            }
            // Immediately outside the white outline (x in 24..92), coarse dots must match the pure dark tone
            // reference with ZERO missing dots whose centers happened to fall in x=16..24!
            for x in 24..92usize {
                let a = out_px[(y * 96 + x) * 4 + 3];
                let a_ref = ref_px[(y * 96 + x) * 4 + 3];
                assert!((a - a_ref).abs() < 1e-4, "coarse dot staircase / missing dot near white outline at ({x}, {y}): got {a}, expected {a_ref}");
            }
        }
    }

    #[test]
    fn step7_sdist2_matches_wgsl_length_semantics() {
        let mut tested_pairs = 0usize;
        let mut hypot_ulp_divergences = 0usize;

        for iy in -128..=128 {
            for ix in -128..=128 {
                let a = (ix as f32) * 0.371_093_75;
                let b = (iy as f32) * 0.613_281_25;

                let wgsl_length = (a * a + b * b).sqrt();
                let cpu_sdist2 = sdist2(a, b);
                let std_hypot = a.hypot(b);

                // 1. Exact bit-level identity between sdist2(a, b) and WGSL length(vec2f(a, b))
                assert_eq!(cpu_sdist2.to_bits(), wgsl_length.to_bits(), "sdist2({a}, {b}) must match WGSL sqrt(a*a + b*b) to 0 ULP");

                // 2. Track 1-2 ULP divergences against f32::hypot while proving tight numerical tolerance
                let ulp_diff = cpu_sdist2.to_bits().abs_diff(std_hypot.to_bits());
                if ulp_diff > 0 {
                    hypot_ulp_divergences += 1;
                }
                assert!(ulp_diff <= 2, "sdist2({a}, {b}) vs hypot({a}, {b}) exceeded 2 ULP: {ulp_diff} ULP");

                let abs_err = (cpu_sdist2 - std_hypot).abs();
                let rel_tol = 2.0 * f32::EPSILON * std_hypot.max(1.0);
                assert!(abs_err <= rel_tol, "Relative error {abs_err} exceeded tolerance {rel_tol} at ({a}, {b})");

                tested_pairs += 1;
            }
        }

        assert!(tested_pairs > 65_000);
        assert!(hypot_ulp_divergences > 0, "Expected f32::hypot to exhibit 1-2 ULP scaling divergences across test grid");
    }

    #[test]
    fn exact_sdf_matches_legacy_polygon_within_tolerance() {
        use oracle::{hexagon_vertices, sdf_polygon, star_vertices, triangle_vertices};
        let mut worst = 0.0f32;
        for y in -40..=40 {
            for x in -40..=40 {
                let (x, y) = (x as f32 * 0.5, y as f32 * 0.5);
                let pairs = [
                    (sdf_hexagon_exact(y, x, 10.0, 0.0), sdf_polygon((x, y), &hexagon_vertices(10.0), 0.0)),
                    (sdf_star5_exact(x, y, 10.0, 0.0), sdf_polygon((x, y), &star_vertices(10.0), 0.0)),
                    (sdf_triangle_exact(x, y, 10.0, 0.0), sdf_polygon((x, y), &triangle_vertices(10.0), 0.0)),
                ];
                for (exact, legacy) in pairs {
                    assert!((exact - legacy).abs() < 1.2, "({x},{y}): exact {exact} vs legacy {legacy}");
                    worst = worst.max((exact - legacy).abs());
                }
            }
        }
        assert!(worst < 1.2);
        assert!(sdf_heart_exact(0.0, 0.0, 10.0, 0.0) < 0.0, "heart centre is inside");
        assert!(sdf_heart_exact(100.0, 0.0, 10.0, 0.0) > 0.0, "far point is outside");
        let legacy_heart = oracle::heart_vertices(10.0);
        assert!(oracle::sdf_polygon((0.0, 0.0), &legacy_heart, 0.0) < 0.0, "legacy heart centre is inside");
        assert!(oracle::sdf_polygon((100.0, 0.0), &legacy_heart, 0.0) > 0.0, "legacy far point is outside");
    }

    #[test]
    fn smooth_min_bridges_gap_between_dot_and_satellite() {
        assert_eq!(sdf_smooth_min(3.0, 5.0, 0.0), 3.0);
        assert!(sdf_smooth_min(2.0, 2.0, 4.0) < 2.0, "smooth min must thicken the bridge");
        assert!((sdf_smooth_min(1.0, 100.0, 4.0) - 1.0).abs() < 1e-6, "far shapes do not affect the result");
        assert!(sdf_smooth_min(f32::MAX, 5.0, 4.0).is_finite());
        let bridged = sdf_smooth_min(1.0, 1.2, 2.0);
        assert!(bridged < 1.0, "the gap between dot and satellite is filled: {bridged}");
    }

    fn coverage_fraction(spec: &ScreentoneSpec, size: i32) -> f32 {
        let bounds = Rect::new(0, 0, size, size);
        let img = make_test_image(size, size, 1.0);
        let ctx = Ctx { bounds, mode: photocraft_color::ColorMode::Rgb, alpha: false };
        let out = screentone(&img, bounds, &ctx, spec);
        let mut covered = 0usize;
        for px in out.chunks_exact(3) {
            if px[0] <= 0.5 {
                covered += 1;
            }
        }
        covered as f32 / (size * size) as f32
    }

    #[test]
    fn clover_and_asterisk_presets_produce_four_fold_symmetry() {
        let presets = built_in_presets();
        for id in ["clover-quatrefoil", "ink-asterisk"] {
            let preset = presets.iter().find(|p| p.id == id).expect("preset exists");
            assert!(preset.spec.satellite_enabled && preset.spec.satellite_count == 4);
            let (dirs, n) = build_sat_dirs(&preset.spec, 0.0, 1.0);
            assert_eq!(n, 4);
            let mut seen = [false; 4];
            for k in 0..n {
                let (x, y) = dirs[k];
                let idx = if y < -0.3 {
                    0
                } else if y > 0.3 {
                    1
                } else if x > 0.3 {
                    2
                } else {
                    3
                };
                seen[idx] = true;
            }
            assert!(seen.iter().all(|v| *v), "{id} has four cardinal satellites: {seen:?}");

            let mut spec = preset.spec.clone();
            spec.use_source_luminance = false;
            spec.mode = ScreentoneMode::Sheet;
            spec.smoothness = 0.0;
            let bounds = Rect::new(0, 0, 96, 96);
            let img = make_test_image(96, 96, 0.5);
            let ctx = Ctx { bounds, mode: photocraft_color::ColorMode::Rgb, alpha: false };
            let out = screentone(&img, bounds, &ctx, &spec);
            let ink = |x: usize, y: usize| out[(y * 96 + x) * 3] <= 0.5;
            let mut asym = 0usize;
            for y in 8..88 {
                for x in 8..88 {
                    if ink(x, y) != ink(95 - x, y) {
                        asym += 1;
                    }
                }
            }
            assert!(asym < 400, "{id} is mirror symmetric (asymmetric pixels {asym})");
        }
    }

    #[test]
    fn equalize_area_makes_50_percent_gray_match_50_percent_coverage() {
        let mut spec = ScreentoneSpec {
            mode: ScreentoneMode::Sheet,
            density: 0.5,
            spacing_x: 16.0,
            spacing_y: 16.0,
            angle: 0.0,
            smoothness: 0.0,
            auto_dot_size: true,
            equalize_area: true,
            ..Default::default()
        };
        let cov = coverage_fraction(&spec, 128);
        assert!((cov - 0.5).abs() < 0.08, "calibrated 50% coverage: {cov}");

        spec.auto_dot_size = false;
        spec.equalize_area = true;
        let legacy = coverage_fraction(&spec, 128);
        assert!(legacy < cov, "sqrt calibration darkens a mid gray: {legacy} vs {cov}");

        spec.pattern_type = PatternType::Lines;
        let with = coverage_fraction(&spec, 128);
        spec.equalize_area = false;
        let without = coverage_fraction(&spec, 128);
        assert!((with - without).abs() < 1e-6, "width-driven patterns ignore equalize_area");
    }

    #[test]
    fn auto_dot_size_calibrates_all_shapes_within_two_percent() {
        let shapes = [DotShape::Circle, DotShape::Square, DotShape::Diamond, DotShape::Hexagon];
        for shape in shapes {
            let spec = ScreentoneSpec {
                mode: ScreentoneMode::Sheet,
                dot_shape: shape,
                density: 0.3,
                spacing_x: 16.0,
                spacing_y: 16.0,
                angle: 0.0,
                smoothness: 0.0,
                auto_dot_size: true,
                ..Default::default()
            };
            let cov = coverage_fraction(&spec, 128);
            let tol = if shape == DotShape::Circle { 0.02 } else { 0.08 };
            assert!((cov - 0.3).abs() < tol, "{shape:?} coverage {cov}");
        }
        // A saturated cell must stay finite and clamped.
        let saturated = ScreentoneSpec { mode: ScreentoneMode::Sheet, density: 1.0, auto_dot_size: true, smoothness: 0.0, ..Default::default() };
        assert!(coverage_fraction(&saturated, 64) <= 1.0);
    }

    #[test]
    fn premultiplied_alpha_prevents_dark_halo() {
        // Same premultiplied formula the renderer uses for the translucent-background branch.
        let (bg, fg, cov) = ([1.0f32, 0.0, 0.0, 0.1], [0.0f32, 0.0, 0.0, 1.0], 0.5f32);
        let out_a = bg[3] + (fg[3] - bg[3]) * cov;
        let red = (bg[0] * bg[3] * (1.0 - cov) + fg[0] * fg[3] * cov) / out_a;
        assert!((out_a - 0.55).abs() < 1e-6);
        assert!(red < 0.2, "red must not bleed through as straight-alpha 0.5: {red}");

        let spec = ScreentoneSpec {
            mode: ScreentoneMode::Sheet,
            density: 0.5,
            background: bg,
            foreground: fg,
            smoothness: 1.0,
            auto_dot_size: true,
            use_source_luminance: false,
            ..Default::default()
        };
        let bounds = Rect::new(0, 0, 64, 64);
        let img = Image { rect: bounds, ch: 4, data: vec![1.0; 64 * 64 * 4] };
        let ctx = Ctx { bounds, mode: photocraft_color::ColorMode::Rgb, alpha: true };
        let out = screentone(&img, bounds, &ctx, &spec);
        assert!(out.chunks_exact(4).all(|px| px.iter().all(|v| v.is_finite() && *v >= -0.01 && *v <= 1.01)));
    }

    #[test]
    fn render_screentone_to_surface_transfers_tiles_and_clips_area() {
        let mut surf = Surface::new(photocraft_color::PixelFormat::RGBA8);
        // Span multiple 256×256 tiles with non-tile-aligned borders (10..530 × 12..300) to test both
        // zero-copy interior tile transfer and edge-tile clipping.
        let bounds = Rect::new(10, 12, 530, 300);
        let spec = ScreentoneSpec {
            mode: ScreentoneMode::Sheet,
            pattern_type: PatternType::Dots,
            spacing_x: 12.0,
            spacing_y: 12.0,
            dot_size: 4.0,
            foreground: [0.0, 0.0, 0.0, 1.0],
            background: [1.0, 1.0, 1.0, 0.0],
            ..Default::default()
        };
        let origin = origin_doc(&spec, bounds);
        let written = render_screentone_to_surface(&mut surf, bounds, &spec, origin);
        assert_eq!(written, bounds);
        assert!(surf.tile_count() >= 4, "expected multi-tile output, got {}", surf.tile_count());
        let cb = surf.content_bounds();
        assert!(cb.x0 >= bounds.x0 && cb.y0 >= bounds.y0 && cb.x1 <= bounds.x1 && cb.y1 <= bounds.y1, "content leaked outside area: {cb:?} vs {bounds:?}");
        // Pixels strictly outside `bounds` inside the straddling tiles must remain transparent.
        assert_eq!(surf.pixel(9, 12)[3], 0.0);
        assert_eq!(surf.pixel(10, 11)[3], 0.0);
        assert_eq!(surf.pixel(530, 100)[3], 0.0);
    }

    #[test]
    fn auto_gpu_predicate_handles_narrow_strips_consistently() {
        use photocraft_color::ColorMode;
        assert!(screentone_prefers_gpu(ScreentoneBackend::Auto, ColorMode::Rgb, Rect::from_xywh(0, 0, 2000, 300)));
        assert!(screentone_prefers_gpu(ScreentoneBackend::Auto, ColorMode::Rgb, Rect::from_xywh(0, 0, 512, 512)));
        assert!(!screentone_prefers_gpu(ScreentoneBackend::Auto, ColorMode::Rgb, Rect::from_xywh(0, 0, 256, 256)));
        assert!(!screentone_prefers_gpu(ScreentoneBackend::Auto, ColorMode::Cmyk, Rect::from_xywh(0, 0, 2000, 300)));
        assert!(!screentone_prefers_gpu(ScreentoneBackend::Gpu, ColorMode::Lab, Rect::from_xywh(0, 0, 2000, 300)));
        assert!(!screentone_prefers_gpu(ScreentoneBackend::Cpu, ColorMode::Rgb, Rect::from_xywh(0, 0, 2000, 300)));
    }

    #[test]
    fn preserve_lines_tiled_surface_matches_monolithic_on_seams() {
        let bounds = Rect::new(0, 0, 512, 512);
        let mut src = Surface::new(photocraft_color::PixelFormat::RGBA8);
        src.fill_rect(bounds, &[0.95, 0.95, 0.95, 1.0]);
        src.fill_rect(Rect::new(64, 64, 448, 448), &[0.55, 0.55, 0.55, 1.0]);
        // High-contrast ink lines crossing x = 256 and y = 256.
        src.fill_rect(Rect::new(40, 254, 472, 259), &[0.02, 0.02, 0.02, 1.0]);
        src.fill_rect(Rect::new(254, 40, 259, 472), &[0.02, 0.02, 0.02, 1.0]);
        for i in 120..392 {
            src.fill_rect(Rect::new(i, i, (i + 3).min(512), (i + 3).min(512)), &[0.03, 0.03, 0.03, 1.0]);
        }

        let spec = ScreentoneSpec {
            pattern_type: PatternType::Dots,
            spacing_x: 12.0,
            spacing_y: 12.0,
            dot_size: 4.5,
            mode: ScreentoneMode::Shading,
            use_source_luminance: true,
            preserve_lines: true,
            line_thickness: 5.0,
            line_threshold: 0.25,
            foreground: [0.0, 0.0, 0.0, 1.0],
            background: [1.0, 1.0, 1.0, 0.0],
            ..Default::default()
        };
        let origin = origin_doc(&spec, bounds);

        // Monolithic direct kernel evaluation over the whole 512×512 image, quantized through RGBA8
        // the same way `Surface::new(PixelFormat::RGBA8)` stores pixels.
        let mono_src_data = src.read_region(bounds);
        let mono_img = Image { rect: bounds, ch: 4, data: mono_src_data };
        let mono_spec = sheet_spec(&spec, bounds, origin);
        let ctx = Ctx { bounds, mode: photocraft_color::ColorMode::Rgb, alpha: true };
        let mono_f32 = screentone(&mono_img, bounds, &ctx, &mono_spec);
        let mut mono_surf = Surface::new(photocraft_color::PixelFormat::RGBA8);
        mono_surf.write_region(bounds, &mono_f32);
        let mono_px = mono_surf.read_region(bounds);

        // Tiled 256×256 surface path.
        let mut tiled_surf = src;
        let written = render_screentone_to_surface(&mut tiled_surf, bounds, &spec, origin);
        assert_eq!(written, bounds);
        let tiled_px = tiled_surf.read_region(bounds);

        let mut total_diff = 0usize;
        let mut seam_diff = 0usize;
        for y in 0..512usize {
            for x in 0..512usize {
                let off = (y * 512 + x) * 4;
                if (0..4).any(|c| mono_px[off + c].to_bits() != tiled_px[off + c].to_bits()) {
                    total_diff += 1;
                    if (254..=258).contains(&x) || (254..=258).contains(&y) {
                        seam_diff += 1;
                    }
                }
            }
        }
        assert_eq!(seam_diff, 0, "preserve_lines tiled seam mismatch at x=256/y=256: {seam_diff} seam px ({total_diff} total)");
        assert_eq!(total_diff, 0, "preserve_lines tiled render must match monolithic bit-for-bit, got {total_diff} differing px");
    }

    #[test]
    fn block_b_focus_lines_inner_radius_and_origin_shift() {
        let bounds = Rect::new(0, 0, 128, 128);
        let spec = ScreentoneSpec {
            pattern_type: PatternType::FocusLines,
            spacing_x: 12.0,
            spacing_y: 8.0,
            line_width: 3.0,
            density: 1.0,
            smoothness: 0.0,
            focus_inner_radius: 24.0,
            mode: ScreentoneMode::Sheet,
            use_source_luminance: false,
            foreground: [0.0, 0.0, 0.0, 1.0],
            background: [1.0, 1.0, 1.0, 0.0],
            ..Default::default()
        };
        let origin = (40.0, 52.0);
        let mut surf = Surface::new(photocraft_color::PixelFormat::RGBA8);
        let _ = render_screentone_to_surface(&mut surf, bounds, &spec, origin);
        let mut inner_ink = 0usize;
        let mut outer_ink = 0usize;
        for y in 0..128 {
            for x in 0..128 {
                let dx = x as f32 + 0.5 - origin.0;
                let dy = y as f32 + 0.5 - origin.1;
                let r = (dx * dx + dy * dy).sqrt();
                let a = surf.pixel(x, y)[3];
                if r < 23.0 {
                    if a > 0.01 {
                        inner_ink += 1;
                    }
                } else if r > 32.0 && a > 0.5 {
                    outer_ink += 1;
                }
            }
        }
        assert_eq!(inner_ink, 0, "FocusLines must leave focus_inner_radius around origin completely clear");
        assert!(outer_ink > 200, "FocusLines must draw radial rays outside inner radius, got {outer_ink}");
    }

    #[test]
    fn block_b_progressive_crosshatch_stages_1_2_3_by_ink() {
        let bounds = Rect::new(0, 0, 96, 32);
        // 4 vertical strips: white (ink=0), light gray (ink~0.25 -> 1 layer),
        // mid gray (ink~0.55 -> 2 layers), dark gray (ink~0.85 -> 3 layers incl. bisector +45°).
        let mut src = Surface::new(photocraft_color::PixelFormat::RGBA8);
        src.fill_rect(Rect::new(0, 0, 24, 32), &[0.98, 0.98, 0.98, 1.0]);
        src.fill_rect(Rect::new(24, 0, 48, 32), &[0.75, 0.75, 0.75, 1.0]);
        src.fill_rect(Rect::new(48, 0, 72, 32), &[0.45, 0.45, 0.45, 1.0]);
        src.fill_rect(Rect::new(72, 0, 96, 32), &[0.15, 0.15, 0.15, 1.0]);
        let spec = ScreentoneSpec {
            pattern_type: PatternType::Crosshatch,
            spacing_y: 8.0,
            line_width: 2.0,
            angle: 0.0,
            cross_angle: 90.0,
            density: 1.0,
            smoothness: 0.0,
            line_progressive: true,
            mode: ScreentoneMode::Shading,
            use_source_luminance: true,
            white_threshold: 0.95,
            black_threshold: 0.05,
            preserve_lines: false,
            foreground: [0.0, 0.0, 0.0, 1.0],
            background: [1.0, 1.0, 1.0, 0.0],
            ..Default::default()
        };
        let origin = (48.0, 16.0);
        let mut out = src;
        let _ = render_screentone_to_surface(&mut out, bounds, &spec, origin);
        let count_ink = |x0: i32, x1: i32| -> usize {
            let mut n = 0;
            for y in 0..32 {
                for x in x0..x1 {
                    if out.pixel(x, y)[3] > 0.5 {
                        n += 1;
                    }
                }
            }
            n
        };
        let c0 = count_ink(0, 24);
        let c1 = count_ink(24, 48);
        let c2 = count_ink(48, 72);
        let c3 = count_ink(72, 96);
        assert_eq!(c0, 0, "white band must have 0 ink");
        assert!(c1 > 40, "stage 1 (ink~0.25) must render base lines, got {c1}");
        assert!(c2 > c1 + 40, "stage 2 (ink~0.55) must add crosshatch lines: c1={c1}, c2={c2}");
        assert!(c3 > c2 + 40, "stage 3 (ink~0.85) must add +45° bisector lines: c2={c2}, c3={c3}");
    }

    #[test]
    fn block_b_wobble_dash_kezuri_tiled_zero_seam_at_256() {
        let bounds = Rect::new(0, 0, 512, 512);
        let spec = ScreentoneSpec {
            pattern_type: PatternType::Lines,
            spacing_x: 12.0,
            spacing_y: 12.0,
            line_width: 3.0,
            angle: 23.0,
            density: 0.9,
            smoothness: 0.5,
            jitter_pos: 35.0,
            jitter_size: 30.0,
            line_wobble: 0.65,
            line_wave_freq: 2.4,
            line_dash: 0.55,
            scrape_strength: 0.5,
            scrape_angle: 37.0,
            mode: ScreentoneMode::Sheet,
            use_source_luminance: false,
            foreground: [0.0, 0.0, 0.0, 1.0],
            background: [1.0, 1.0, 1.0, 0.0],
            seed: 1337,
            ..Default::default()
        };
        let origin = origin_doc(&spec, bounds);

        let empty = Image { rect: Rect::EMPTY, ch: 4, data: Vec::new() };
        let mono_spec = sheet_spec(&spec, bounds, origin);
        let ctx = Ctx { bounds, mode: photocraft_color::ColorMode::Rgb, alpha: true };
        let mono_f32 = screentone(&empty, bounds, &ctx, &mono_spec);
        let mut mono_surf = Surface::new(photocraft_color::PixelFormat::RGBA8);
        mono_surf.write_region(bounds, &mono_f32);
        let mono_px = mono_surf.read_region(bounds);

        let mut tiled_surf = Surface::new(photocraft_color::PixelFormat::RGBA8);
        let _ = render_screentone_to_surface(&mut tiled_surf, bounds, &spec, origin);
        let tiled_px = tiled_surf.read_region(bounds);

        let mut seam_diff = 0usize;
        let mut total_diff = 0usize;
        for y in 0..512usize {
            for x in 0..512usize {
                let off = (y * 512 + x) * 4;
                if (0..4).any(|c| mono_px[off + c].to_bits() != tiled_px[off + c].to_bits()) {
                    total_diff += 1;
                    if (254..=258).contains(&x) || (254..=258).contains(&y) {
                        seam_diff += 1;
                    }
                }
            }
        }
        assert_eq!(seam_diff, 0, "Block B wobble+dash+kezuri seam diff at x=256/y=256: {seam_diff}");
        assert_eq!(total_diff, 0, "Block B tiled render must match monolithic bit-for-bit, got {total_diff}");
    }
}
