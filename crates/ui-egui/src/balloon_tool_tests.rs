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
