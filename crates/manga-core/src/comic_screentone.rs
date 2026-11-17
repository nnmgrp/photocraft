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

#[cfg(test)]
mod tests {
    use photocraft_geom::Rect;

    use super::{ScreentoneSpec, built_in_presets, origin_doc, sheet_spec};

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
}
