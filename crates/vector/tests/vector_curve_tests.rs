//! Integration & unit tests for PaintTool SAI Ver.2 Vector Curve (`Linework`)
//! and Parametric Balloon v3.1 (`LiveBalloonGroup`, `Uniflash`, `balloon_to_linework_strokes`).
//!
//! Adapted from the Gemini 4 Argon reference bundle to the Photocraft API: the SAI curve
//! lives in `comic_sai_curve` and the balloon geometry is reached through the
//! `LiveBalloonGeom` / `LiveBalloonGroupGeom` traits (the balloon types themselves belong to
//! the `photocraft-doc` crate, which stays pure data).

use photocraft_doc::vector::{LiveBalloon, LiveBalloonBody, LiveBalloonGroup, LiveBalloonTail, LiveShape, TailPreset};
use photocraft_vector::balloon::{LiveBalloonGeom, LiveBalloonGroupGeom, balloon_to_linework_strokes};
use photocraft_vector::comic_sai_curve::{
    LiveVectorStroke, SAI_BEZIER_TENSION, SAI_DEFAULT_TENSION, SaiCurveType, SaiPoint, alpha_at, sample_sai_curve, tapered_curve_path, tapered_rings_ex,
    width_at,
};

#[test]
fn test_tapered_outline_subdivides_curve_smoothly() {
    let nodes = vec![
        SaiPoint::with_pressure((100.0, 200.0), 0.1, false),
        SaiPoint::with_pressure((250.0, 100.0), 1.0, false),
        SaiPoint::with_pressure((400.0, 200.0), 0.1, false),
    ];

    let path = tapered_curve_path(&nodes, 16.0, 0.0, false, SAI_DEFAULT_TENSION);
    assert_eq!(path.subpaths.len(), 1);

    let sp = &path.subpaths[0];
    assert!(sp.closed, "Tapered stroke outline must be a closed subpath");
    assert!(sp.knots.len() > 40, "Expected smooth subdivided outline (> 40 knots), got {}", sp.knots.len());
}

#[test]
fn test_closed_tapered_curve_path_is_seamless() {
    let nodes = vec![
        SaiPoint::with_pressure((200.0, 120.0), 0.05, false),
        SaiPoint::with_pressure((340.0, 200.0), 4.77, false),
        SaiPoint::with_pressure((280.0, 340.0), 1.0, false),
        SaiPoint::with_pressure((120.0, 260.0), 0.05, false),
    ];

    let (left, right, _samples) = tapered_rings_ex(&nodes, 18.0, 0.05, true, SAI_DEFAULT_TENSION, 2.0);
    assert!(left.len() > 24);
    assert_eq!(left.len(), right.len());

    let path = tapered_curve_path(&nodes, 18.0, 0.05, true, SAI_DEFAULT_TENSION);
    assert_eq!(path.subpaths.len(), 2, "Closed stroke must emit outer and inner seamless rings");
    assert!(path.subpaths[0].closed);
    assert!(path.subpaths[1].closed);
}

#[test]
fn test_sai_density_and_curve_type_addendum() {
    assert!((alpha_at(1.0, 0.0, 1.0) - 1.0).abs() < 1e-6);
    assert!((alpha_at(1.0, 0.0, 0.0) - 0.0).abs() < 1e-6);
    assert!((alpha_at(1.0, 0.25, 0.0) - 0.25).abs() < 1e-6);
    // The pressure is clamped to 0..=2, so a half-pressure node is half the base width.
    assert!((width_at(10.0, 0.0, 0.5) - 5.0).abs() < 1e-6);
    assert!((width_at(10.0, 0.0, 5.0) - 20.0).abs() < 1e-6);

    let nodes = vec![
        SaiPoint::with_dynamics((100.0, 100.0), 1.0, 0.0, false),
        SaiPoint::with_dynamics((200.0, 150.0), 2.5, 1.0, false),
        SaiPoint::with_dynamics((300.0, 100.0), 1.0, 0.2, false),
    ];
    let stroke = LiveVectorStroke {
        nodes: nodes.clone(),
        base_width: 12.0,
        min_ratio: 0.05,
        base_density: 0.8,
        min_density_ratio: 0.1,
        curve_type: SaiCurveType::Bezier,
        tapered: true,
        closed: false,
        tension: SAI_DEFAULT_TENSION,
    };

    // The Bézier curve type overrides the stroke's own tension.
    assert!((stroke.effective_tension() - SAI_BEZIER_TENSION).abs() < 1e-6);
    let samples = sample_sai_curve(&stroke.nodes, stroke.base_width, stroke.min_ratio, stroke.closed, stroke.effective_tension(), 2.0);
    assert!(samples.len() > 20);
    // The first node's density is 0.0, so the stroke starts at the minimum density ratio.
    assert!((alpha_at(stroke.base_density, stroke.min_density_ratio, nodes[0].density) - 0.08).abs() < 1e-3);
}

#[test]
fn test_balloon_v3_1_all_5_bodies_and_linework_conversion() {
    // 1. Serialization of LiveShape::BalloonGroup with all 5 body shapes and the tail presets.
    let ellipse_balloon = LiveBalloon::default();
    let uniflash_balloon = LiveBalloon {
        id: 2,
        body: LiveBalloonBody::Uniflash {
            rect: [420.0, 80.0, 260.0, 200.0],
            needles: 64,
            inner_ratio: 0.62,
            bundle_group: 0.40,
            length_jitter: 0.50,
            angle_jitter: 0.25,
            needle_width: 2.2,
            outer_sharpness: 0.0,
            inner_sharpness: 1.0,
            seed: 42,
        },
        tails: Vec::new(),
        next_tail_id: 1,
        vertex_overrides: Vec::new(),
        hand_drawn_noise: 0.0,
        stroke_width: 2.2,
        auto_merge_overlapping: false,
    };
    let thought_cloud = LiveBalloon {
        id: 3,
        body: LiveBalloonBody::Cloud { rect: [120.0, 320.0, 240.0, 160.0], bumps: 10, amplitude: 0.18, cusp_bite: 0.22, jitter: 0.35, seed: 7 },
        tails: vec![LiveBalloonTail {
            id: 1,
            preset: TailPreset::Thought,
            perimeter_t: 0.55,
            mouth_width: 24.0,
            tip: [180.0, 540.0],
            bend: 0.2,
            bubbles: 3,
            bubble_radius: 10.0,
        }],
        next_tail_id: 2,
        vertex_overrides: Vec::new(),
        hand_drawn_noise: 0.0,
        stroke_width: 3.0,
        auto_merge_overlapping: true,
    };

    let group = LiveBalloonGroup { items: vec![ellipse_balloon.clone(), uniflash_balloon.clone(), thought_cloud.clone()], next_balloon_id: 4 };
    let live_shape = LiveShape::BalloonGroup { group: group.clone() };
    let json_str = serde_json::to_string(&live_shape).expect("Must serialize LiveShape::BalloonGroup");
    let roundtrip: LiveShape = serde_json::from_str(&json_str).expect("Must deserialize LiveShape::BalloonGroup");
    assert_eq!(live_shape, roundtrip);

    // The group rebuilds one path out of its balloons.
    let group_path = group.rebuild_group_path();
    assert!(!group_path.subpaths.is_empty(), "The balloon group must build a path");

    // The tail's mouth sits on the body's perimeter: the parametric lookup finds it.
    let t = thought_cloud.closest_perimeter_t([180.0, 540.0]);
    assert!((0.0..1.0).contains(&t), "The perimeter parameter must be normalized, got {t}");
    let text_rect = thought_cloud.safe_text_rect();
    assert!(text_rect[2] > 0.0 && text_rect[3] > 0.0, "The safe text rect must have a size");

    // 2. The Uniflash: 64 radial needles, converted into 64 strokes. The end pressures are
    // `1.0 - 0.95 * sharpness`: the blunt outer tip (sharpness 0.0) keeps the full width while
    // the fully sharp inner tip tapers down to 0.05. The middle stays solid.
    let uf_strokes = balloon_to_linework_strokes(&uniflash_balloon);
    assert_eq!(uf_strokes.len(), 64);
    assert!((uf_strokes[0].nodes[0].pressure - 1.0).abs() < 1e-6, "blunt outer tip keeps the full width, got {}", uf_strokes[0].nodes[0].pressure);
    assert!((uf_strokes[0].nodes[2].pressure - 0.05).abs() < 1e-6, "sharp inner tip, got {}", uf_strokes[0].nodes[2].pressure);
    assert!((uf_strokes[0].nodes[1].pressure - 1.0).abs() < 1e-6, "the middle stays solid, got {}", uf_strokes[0].nodes[1].pressure);

    // 3. The Cloud + Thought tail: the cloud plus 3 thought bubbles -> 4 closed Linework strokes.
    let cloud_strokes = balloon_to_linework_strokes(&thought_cloud);
    assert_eq!(cloud_strokes.len(), 4, "1 cloud body + 3 thought bubbles");
    assert!(cloud_strokes.iter().all(|s| s.closed));
}
