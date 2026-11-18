//! A Ruler drag held at the canvas edge scrolls the camera, and the dragged end keeps following
//! the pointer even though no new `PointerMoved` arrives.

use egui::{Modifiers, Rect, pos2, vec2};
use serde_json::json;

use crate::PhotocraftApp;
use crate::canvas::{ToolEvent, ViewXform};
use crate::state::{Tool, View};
use crate::tools::analysis_ui::Drag;

fn ruler_app() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 2000, "height": 1000})).unwrap();
    app.run("image.analysis.rulerTool", json!({"start": [100, 100], "end": [300, 200], "protractor": null})).unwrap();
    app.sync_views();
    app.ui.tool = Tool::Ruler;
    app
}

fn ruler_length(app: &PhotocraftApp) -> f64 {
    let r = app.session.active().unwrap().doc.measurement.ruler.unwrap();
    ((r.end[0] - r.start[0]).powi(2) + (r.end[1] - r.start[1]).powi(2)).sqrt()
}

#[test]
fn ruler_drag_at_the_edge_scrolls_the_view_and_stretches_the_ruler() {
    let mut app = ruler_app();
    // Press the far end: from now on `analysis.drag` is held, with no further pointer events.
    crate::analysis_ui::pointer(&mut app, ToolEvent::Down { x: 300.0, y: 200.0, pressure: 1.0 }, Modifiers::NONE);
    assert_eq!(app.ui.analysis.drag, Some(Drag::RulerHandle(1)));
    let length_before = ruler_length(&app);

    let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 300.0));
    let mut view = View { zoom: 2.0, center: [1000.0, 500.0], doc_size: [2000, 1000], ..Default::default() };
    let mut xf = ViewXform { rect, zoom: view.zoom, center: view.center, flip: false, rotation: 0.0, aspect: 1.0 };
    let p = pos2(rect.right() + 40.0, rect.center().y);
    let step = crate::canvas_autoscroll::edge_step(p, rect, 1.0 / 60.0);
    assert!(step.x > 0.0, "the pointer is past the right edge");

    let center_before = view.center;
    assert!(super::ruler_autoscroll_move(&mut app, &mut view, &mut xf, p, Modifiers::NONE, step));
    assert!(view.center[0] > center_before[0], "the view scrolled toward the pointer");
    assert_eq!(xf.center, view.center, "the synthetic move used the new camera");
    assert!(ruler_length(&app) > length_before + 1.0, "the held end followed the scroll: {length_before} → {}", ruler_length(&app));
}

#[test]
fn without_a_held_ruler_drag_the_ruler_is_untouched() {
    let mut app = ruler_app();
    let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 300.0));
    let mut view = View { zoom: 2.0, center: [1000.0, 500.0], doc_size: [2000, 1000], ..Default::default() };
    let mut xf = ViewXform { rect, zoom: view.zoom, center: view.center, flip: false, rotation: 0.0, aspect: 1.0 };
    let p = pos2(rect.right() + 40.0, rect.center().y);
    let step = crate::canvas_autoscroll::edge_step(p, rect, 1.0 / 60.0);
    let before = ruler_length(&app);
    assert!(super::ruler_autoscroll_move(&mut app, &mut view, &mut xf, p, Modifiers::NONE, step), "the camera still scrolls");
    assert_eq!(ruler_length(&app), before, "but no drag means the ruler is untouched");
}
