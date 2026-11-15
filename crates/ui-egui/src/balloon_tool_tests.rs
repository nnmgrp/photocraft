//! The Balloon tool's drag gestures (`vector_ui::finish_balloon`): a tail unioned with the body
//! and procedural cloud / spiky balloons created from a drag rectangle.

use serde_json::json;

use crate::PhotocraftApp;
use crate::state::Tool;
use photocraft_doc::LayerContent;

fn app_with(w: u32, h: u32) -> PhotocraftApp {
    let mut session = photocraft_engine::Session::new();
    session.execute("file.new", json!({"width": w, "height": h, "background": "white"})).unwrap();
    let mut app = PhotocraftApp::new(session, crate::Services::default());
    app.ui.tool = Tool::Balloon;
    app
}

fn active_layer_id(app: &PhotocraftApp) -> u64 {
    app.session.active().unwrap().active_layer.unwrap().0
}

fn shape_subpaths(app: &PhotocraftApp, layer: u64) -> usize {
    let st = app.session.active().unwrap();
    match &st.doc.layer(photocraft_doc::LayerId(layer)).unwrap().content {
        LayerContent::Shape(sh) => sh.path.subpaths.len(),
        _ => 0,
    }
}

#[test]
fn a_tail_drag_unions_the_tail_with_the_active_balloon() {
    let mut app = app_with(400, 400);
    let id = app.run("shape.create", json!({"kind": "ellipse", "rect": [100, 100, 200, 150]})).unwrap()["layer"].as_u64().unwrap();
    let before = shape_subpaths(&app, id);
    let history = app.session.active().unwrap().history.past_len();
    app.ui.tool_options.balloon_mode = "tail".into();
    app.ui.tool_options.balloon_preset = "arc".into();
    crate::vector_ui::finish_balloon(&mut app, [200.0, 245.0], [210.0, 360.0]);
    assert!(!app.ui.status_error, "status: {}", app.ui.status);
    assert_eq!(shape_subpaths(&app, id), before, "the tail joins the balloon contour");
    assert_eq!(app.session.active().unwrap().history.past_len(), history + 1, "one undoable step");
}

#[test]
fn cloud_and_spiky_drags_create_balloon_layers() {
    use crate::comic_balloon_panel::BalloonBodyKindChoice;
    for (choice, knot_floor) in [(BalloonBodyKindChoice::Cloud, 10usize), (BalloonBodyKindChoice::Spiky, 12usize)] {
        let mut app = app_with(400, 300);
        let layers = app.session.active().unwrap().doc.layers.len();
        app.ui.balloon_ui.body_choice = choice;
        crate::vector_ui::finish_balloon(&mut app, [40.0, 40.0], [300.0, 240.0]);
        assert!(!app.ui.status_error, "{choice:?} status: {}", app.ui.status);
        let st = app.session.active().unwrap();
        assert_eq!(st.doc.layers.len(), layers + 1, "{choice:?} adds one shape layer");
        let id = active_layer_id(&app);
        let LayerContent::Shape(sh) = &st.doc.layer(photocraft_doc::LayerId(id)).unwrap().content else { panic!("{choice:?} did not create a shape layer") };
        assert!(matches!(sh.live, Some(photocraft_doc::LiveShape::BalloonGroup { .. })), "{choice:?} should create live BalloonGroup");
        assert!(sh.path.subpaths[0].knots.len() >= knot_floor, "{choice:?}: {} knots", sh.path.subpaths[0].knots.len());
    }
}

#[test]
fn a_click_creates_live_balloon_from_panel_and_guards_near_active_gizmo() {
    let mut app = app_with(600, 600);
    app.ui.balloon_ui.body_choice = crate::comic_balloon_panel::BalloonBodyKindChoice::Spiky;
    crate::vector_ui::finish_balloon(&mut app, [300.0, 300.0], [301.0, 301.0]);
    assert!(!app.ui.status_error, "status: {}", app.ui.status);
    assert_eq!(app.session.active().unwrap().doc.layers.len(), 2, "click creates live balloon layer");
    assert!(app.balloon_gizmo.active_balloon_id.is_some());

    // A slight miss near the active balloon's bounds should not spawn a duplicate layer.
    crate::vector_ui::finish_balloon(&mut app, [310.0, 310.0], [311.0, 311.0]);
    assert_eq!(app.session.active().unwrap().doc.layers.len(), 2, "near-active miss must not duplicate");
}

#[test]
fn chamfer_rect_and_double_outline_create_distinct_live_balloons() {
    let mut app = app_with(600, 600);
    app.ui.tool_options.balloon_mode = "body".into();
    app.ui.balloon_ui.body_choice = crate::comic_balloon_panel::BalloonBodyKindChoice::ChamferRect;
    crate::vector_ui::finish_balloon(&mut app, [60.0, 60.0], [220.0, 160.0]);
    {
        let doc = &app.session.active().unwrap().doc;
        assert_eq!(doc.layers.len(), 2);
        let photocraft_doc::LayerContent::Shape(sh) = &doc.layers[1].content else {
            panic!("expected shape layer for ChamferRect");
        };
        let Some(photocraft_doc::LiveShape::BalloonGroup { ref group }) = sh.live else {
            panic!("expected BalloonGroup");
        };
        assert!(matches!(group.items[0].body, photocraft_doc::vector::LiveBalloonBody::ChamferRect { .. }));
    }

    app.ui.balloon_ui.body_choice = crate::comic_balloon_panel::BalloonBodyKindChoice::DoubleOutline;
    crate::vector_ui::finish_balloon(&mut app, [320.0, 80.0], [500.0, 200.0]);
    let doc = &app.session.active().unwrap().doc;
    assert_eq!(doc.layers.len(), 3);
    let photocraft_doc::LayerContent::Shape(sh) = &doc.layers[2].content else {
        panic!("expected shape layer for DoubleOutline");
    };
    let Some(photocraft_doc::LiveShape::BalloonGroup { ref group }) = sh.live else {
        panic!("expected BalloonGroup");
    };
    assert!(matches!(group.items[0].body, photocraft_doc::vector::LiveBalloonBody::DoubleOutline { .. }));
}

#[test]
fn script_batch_svg_and_vector_brush_commands_roundtrip_via_app() {
    let mut app = app_with(800, 800);
    let batch = app.run("shape.balloon.batchFromScript", serde_json::json!({ "script": "Line 1\nLine 2!!", "sizePt": 14.0, "vertical": false })).unwrap();
    assert_eq!(batch["createdBalloons"].as_u64(), Some(2));
    let layer_id = batch["layer"].as_u64().unwrap();

    let exported = app.run("shape.balloon.exportSvg", serde_json::json!({ "layer": layer_id, "balloonId": 1, "text": "Hi!", "sizePt": 14.0 })).unwrap();
    let svg = exported["svg"].as_str().unwrap();
    assert!(svg.contains("<svg"), "exported SVG must include <svg root");

    let imported = app
        .run(
            "shape.balloon.importSvg",
            serde_json::json!({
                "layer": layer_id,
                "balloonId": 1,
                "svg": "M 0 0 L 120 0 L 120 80 L 0 80 Z"
            }),
        )
        .unwrap();
    assert_eq!(imported["ok"].as_bool(), Some(true));
    assert_eq!(imported["importedKnots"].as_u64(), Some(4));

    let list1 = app.run("shape.vectorBrush.list", serde_json::json!({})).unwrap();
    let base = list1["presets"].as_array().unwrap().first().unwrap().clone();
    let mut custom: photocraft_doc::vector_brush::VectorBrushPreset = serde_json::from_value(base).unwrap();
    custom.id = "custom:ui_roundtrip_test".to_string();
    custom.name = "UI Roundtrip Brush".to_string();

    let imp_brush = app.run("shape.vectorBrush.import", serde_json::json!({ "preset": custom })).unwrap();
    assert_eq!(imp_brush["ok"].as_bool(), Some(true));

    let list2 = app.run("shape.vectorBrush.list", serde_json::json!({})).unwrap();
    let has_custom = list2["presets"].as_array().unwrap().iter().any(|p| p.get("id").and_then(|v| v.as_str()) == Some("custom:ui_roundtrip_test"));
    assert!(has_custom, "imported vector brush preset must persist in session library");
}
