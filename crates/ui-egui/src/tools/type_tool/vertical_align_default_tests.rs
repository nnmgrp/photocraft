//! Vertical alignment: point text is promoted to a paragraph box (so the layout's alignment pass
//! can move it), Box/Area text keeps its shape, and the last choice becomes the persisted default
//! for newly created type layers.

#![cfg(not(target_arch = "wasm32"))]

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use photocraft_doc::text::{TextShape, VerticalAlign};
use photocraft_doc::{LayerId, Path, Subpath};
use serde_json::json;

use crate::PhotocraftApp;
use crate::state::Tool;

use super::defaults::default_styles;

fn new_app() -> PhotocraftApp {
    let _ = photocraft_text::shared();
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 1000, "height": 700})).unwrap();
    app.sync_views();
    app.ui.tool = Tool::Type;
    app
}

fn create(app: &mut PhotocraftApp, params: serde_json::Value) -> LayerId {
    let made = app.run("type.create", params).unwrap();
    let id = LayerId(made["layer"].as_u64().unwrap());
    app.run("layer.select", json!({"layer": id.0, "mode": "replace"})).unwrap();
    id
}

fn baseline(app: &mut PhotocraftApp, id: LayerId) -> f32 {
    super::layout::layout(app, id).expect("a type layout").0.lines.first().expect("a line").baseline
}

fn remembered(app: &PhotocraftApp) -> Option<String> {
    app.ui.tool_options.type_style_defaults.get("verticalAlign").and_then(|v| v.as_str()).map(str::to_owned)
}

#[test]
fn point_text_is_promoted_to_a_box_and_its_baseline_moves() {
    let mut app = new_app();
    let id = create(&mut app, json!({"text": "Lorem ipsum", "x": 200, "y": 200}));
    assert!(matches!(super::text_layer(&app.session.active().unwrap().doc, id).unwrap().shape, TextShape::Point));
    let before = baseline(&mut app, id);

    super::panel::apply_vertical_align(&mut app, &egui::Context::default(), "bottom");

    let layer = super::text_layer(&app.session.active().unwrap().doc, id).unwrap();
    assert!(matches!(layer.shape, TextShape::Box { height, .. } if height >= 80.0), "{:?}", layer.shape);
    assert_eq!(layer.paragraphs[0].style.vertical_align, VerticalAlign::Bottom);
    let after = baseline(&mut app, id);
    assert!(after > before + 1.0, "bottom alignment moved the first baseline down: {before} → {after}");
}

#[test]
fn box_text_keeps_its_shape_and_moves_with_alignment() {
    let mut app = new_app();
    let id = create(&mut app, json!({"text": "Lorem ipsum", "box": [100.0, 100.0, 150.0, 180.0]}));
    let boxed = super::text_layer(&app.session.active().unwrap().doc, id).unwrap().shape.clone();
    assert!(matches!(boxed, TextShape::Box { .. }));
    super::panel::apply_vertical_align(&mut app, &egui::Context::default(), "top");
    let top = baseline(&mut app, id);

    super::panel::apply_vertical_align(&mut app, &egui::Context::default(), "center");
    let centered = baseline(&mut app, id);
    super::panel::apply_vertical_align(&mut app, &egui::Context::default(), "bottom");
    let bottom = baseline(&mut app, id);
    super::panel::apply_vertical_align(&mut app, &egui::Context::default(), "justify");
    let justified = baseline(&mut app, id);

    assert!(centered > top + 1.0, "center moved down: {top} → {centered}");
    assert!(bottom > centered, "bottom moved further: {centered} → {bottom}");
    assert!(justified >= top - 0.01, "justify keeps the first line at the top");
    assert_eq!(super::text_layer(&app.session.active().unwrap().doc, id).unwrap().shape, boxed, "the box survived");
}

#[test]
fn area_text_is_not_converted_to_a_box() {
    let mut app = new_app();
    let id = create(&mut app, json!({"text": "one two three four five six", "size": 24, "x": 300, "y": 420}));
    std::sync::Arc::make_mut(&mut app.session.active_mut().unwrap().doc).work_path =
        Some(Path::new(vec![Subpath::polygon(&[(300.0, 300.0), (600.0, 300.0), (600.0, 500.0), (300.0, 500.0)])]));
    app.run("type.fillPath", json!({})).unwrap();
    let area = super::text_layer(&app.session.active().unwrap().doc, id).unwrap().shape.clone();
    assert!(matches!(area, TextShape::Area { .. }));

    super::panel::apply_vertical_align(&mut app, &egui::Context::default(), "bottom");

    let layer = super::text_layer(&app.session.active().unwrap().doc, id).unwrap();
    assert_eq!(layer.shape, area, "an area path must not be destroyed by a vertical-align click");
    assert_eq!(layer.paragraphs[0].style.vertical_align, VerticalAlign::Bottom);
}

#[test]
fn the_last_choice_survives_a_restart() {
    let ctx = egui::Context::default();
    let mut app = new_app();
    create(&mut app, json!({"text": "Lorem ipsum", "x": 200, "y": 200}));

    super::panel::apply_vertical_align(&mut app, &ctx, "bottom");
    assert_eq!(remembered(&app).as_deref(), Some("bottom"));
    assert_eq!(default_styles(&app).1.vertical_align, VerticalAlign::Bottom);
    crate::tool_memory::persist(&mut app, &ctx);
    let saved = app.session.prefs().tool_options.clone();

    let mut restarted = new_app();
    restarted.session.prefs.edit(|p| p.tool_options = saved.clone());
    crate::tool_memory::restore(&mut restarted);
    assert_eq!(default_styles(&restarted).1.vertical_align, VerticalAlign::Bottom, "Bottom survived the restart");

    create(&mut restarted, json!({"text": "Lorem ipsum", "x": 200, "y": 200}));
    super::panel::apply_vertical_align(&mut restarted, &ctx, "top");
    assert_eq!(remembered(&restarted).as_deref(), Some("top"));
    crate::tool_memory::persist(&mut restarted, &ctx);
    let saved = restarted.session.prefs().tool_options.clone();

    let mut restarted = new_app();
    restarted.session.prefs.edit(|p| p.tool_options = saved);
    crate::tool_memory::restore(&mut restarted);
    assert_eq!(default_styles(&restarted).1.vertical_align, VerticalAlign::Top, "the later Top choice won");
}

#[test]
fn malformed_saved_alignment_falls_back_to_top() {
    let mut app = new_app();
    app.ui.tool_options.type_style_defaults.insert("verticalAlign".into(), json!("middle"));
    crate::tool_memory::sanitize(&mut app.ui.tool_options);
    assert_eq!(remembered(&app), None);
    assert_eq!(default_styles(&app).1.vertical_align, VerticalAlign::Top);
}

#[test]
fn creating_text_does_not_change_the_remembered_alignment() {
    let mut app = new_app();
    super::panel::apply_vertical_align(&mut app, &egui::Context::default(), "bottom");
    let before = remembered(&app);
    create(&mut app, json!({"text": "From a balloon", "x": 100, "y": 100}));
    assert_eq!(remembered(&app), before, "type.create must not rewrite the remembered default");
}

#[test]
fn the_paragraph_panel_shows_four_vertical_align_buttons() {
    let mut app = new_app();
    create(&mut app, json!({"text": "Lorem ipsum", "x": 200, "y": 200}));
    let mut h = Harness::builder().with_size(egui::vec2(360.0, 900.0)).with_max_steps(16).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            if ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                crate::type_tool::character_panel(app, ui, true);
            }
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, Default::default());
    h.run_steps(4);
    for tip in ["Top align text", "Center text vertically", "Bottom align text", "Justify text vertically"] {
        let b = h.get_by_role_and_label(egui::accesskit::Role::Button, tip);
        assert!(!b.accesskit_node().is_disabled(), "{tip} is shown and enabled");
    }
}
