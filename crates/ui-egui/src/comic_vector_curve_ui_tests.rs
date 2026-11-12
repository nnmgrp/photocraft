use super::*;

#[test]
fn test_vector_curve_state_on_primary_down_and_auto_taper() {
    let mut state = VectorCurveState::default();
    assert_eq!(state.points.len(), 0);
    assert_eq!(state.mode, VectorCurveMode::Draw);
    assert!(state.auto_taper_ends);

    // Click 1: start point (100, 100) -> initial tapered tip (0.08)
    let commit = state.on_primary_down(100.0, 100.0, false, false, false, false, false, 1.0);
    assert!(!commit);
    assert_eq!(state.points.len(), 1);
    assert_eq!(state.points[0][2], 0.08);

    // Click 2: next point (200, 200) -> both tips tapered (0.08)
    let commit = state.on_primary_down(200.0, 200.0, false, false, false, false, false, 1.0);
    assert!(!commit);
    assert_eq!(state.points.len(), 2);
    assert_eq!(state.points[1][2], 0.08);

    // Click 3: next point (300, 100) -> middle node expands to full 1.0, end node tapers to 0.08
    let commit = state.on_primary_down(300.0, 100.0, false, false, false, false, false, 1.0);
    assert!(!commit);
    assert_eq!(state.points.len(), 3);
    assert_eq!(state.points[0][2], 0.08, "start node must remain tapered tip");
    assert_eq!(state.points[1][2], 1.0, "interior node must restore to 1.0 width");
    assert_eq!(state.points[2][2], 0.08, "end node must remain tapered tip");
}

#[test]
fn test_vector_curve_double_click_commit() {
    let mut state = VectorCurveState::default();
    state.on_primary_down(100.0, 100.0, false, false, false, false, false, 1.0);
    state.on_primary_down(200.0, 200.0, false, false, false, false, false, 1.0);
    state.on_primary_down(300.0, 100.0, false, false, false, false, false, 1.0);

    // Double-click near the last node (300.0, 100.0)
    let commit = state.on_primary_down(300.5, 100.5, false, false, false, true, false, 1.0);
    assert!(commit, "Double clicking near last node must commit immediately");
    // Ensure no duplicate node was added
    assert_eq!(state.points.len(), 3);
}

#[test]
fn test_vector_curve_ctrl_click_insert_and_drag() {
    let mut state = VectorCurveState::default();
    state.on_primary_down(100.0, 200.0, false, false, false, false, false, 1.0);
    state.on_primary_down(300.0, 200.0, false, false, false, false, false, 1.0);
    assert_eq!(state.points.len(), 2);

    // Ctrl + Click near segment middle (200, 200)
    let commit = state.on_primary_down(200.0, 200.0, true, false, false, false, false, 1.0);
    assert!(!commit);
    assert_eq!(state.points.len(), 3, "Ctrl+Click must insert node on segment");
    assert_eq!(state.drag_kind, Some(NodeDragKind::MovePosition));

    // Drag the inserted node upward to (200, 150)
    state.on_pointer_move(200.0, 150.0);
    assert_eq!(state.points[state.drag_node.unwrap()][1], 150.0);

    state.on_primary_up(false);
    assert_eq!(state.drag_node, None);
}

#[test]
fn test_vector_curve_weight_mode_and_alt_drag() {
    let mut state = VectorCurveState::default();
    state.on_primary_down(100.0, 200.0, false, false, false, false, false, 1.0);
    state.on_primary_down(300.0, 200.0, false, false, false, false, false, 1.0);

    // Alt + Click-drag node 0 to adjust pressure
    state.on_primary_down(100.0, 200.0, false, true, false, false, false, 1.0);
    assert_eq!(state.drag_kind, Some(NodeDragKind::AdjustPressure));

    // Drag up/down adjusts pressure
    state.on_pointer_move(100.0, 140.0);
    let p = state.points[0][2];
    assert!(p > 0.08, "Pressure should increase when dragging upward");
    state.on_primary_up(true);
}

#[test]
fn test_vector_curve_alt_click_toggles_corner() {
    let mut state = VectorCurveState::default();
    state.on_primary_down(100.0, 200.0, false, false, false, false, false, 1.0);
    assert_eq!(state.points[0][3], 0.0, "default smooth node");

    // Alt + Click without moving pointer toggles corner
    state.on_primary_down(100.0, 200.0, false, true, false, false, false, 1.0);
    state.on_primary_up(true);
    assert_eq!(state.points[0][3], 1.0, "Alt+Click toggles to sharp corner");

    state.on_primary_down(100.0, 200.0, false, true, false, false, false, 1.0);
    state.on_primary_up(true);
    assert_eq!(state.points[0][3], 0.0, "Alt+Click toggles back to smooth");
}

#[test]
fn test_vector_curve_delete_active_node() {
    let mut state = VectorCurveState::default();
    state.on_primary_down(100.0, 200.0, false, false, false, false, false, 1.0);
    state.on_primary_down(200.0, 200.0, false, false, false, false, false, 1.0);
    state.on_primary_down(300.0, 200.0, false, false, false, false, false, 1.0);
    assert_eq!(state.points.len(), 3);

    // Delete node 1
    state.selected_node = Some(1);
    assert!(state.delete_active_node());
    assert_eq!(state.points.len(), 2);
    assert_eq!(state.points[1][0], 300.0);
}

#[test]
fn test_vector_curve_to_command_params() {
    let mut state = VectorCurveState::default();
    state.on_primary_down(100.0, 200.0, false, false, false, false, false, 1.0);
    state.on_primary_down(300.0, 200.0, false, false, false, false, false, 1.0);

    let params = state.to_command_params(8.0, 0.05, true, false).unwrap();
    assert_eq!(params["width"], 8.0);
    assert_eq!(params["tapered"], true);
    assert_eq!(params["points"].as_array().unwrap().len(), 2);
}

#[test]
fn test_vector_curve_click_start_node_closes_loop_and_restores_pressures() {
    let mut state = VectorCurveState::default();
    state.on_primary_down(100.0, 200.0, false, false, false, false, false, 1.0);
    state.on_primary_down(200.0, 100.0, false, false, false, false, false, 1.0);
    state.on_primary_down(300.0, 200.0, false, false, false, false, false, 1.0);
    state.on_primary_down(200.0, 300.0, false, false, false, false, false, 1.0);
    assert!(!state.closed_loop);
    assert_eq!(state.points[0][2], 0.08);

    // Clicking on the starting node (100, 200) closes the loop and commits immediately
    let commit = state.on_primary_down(101.0, 199.5, false, false, false, false, false, 1.0);
    assert!(commit, "Clicking start node with >= 3 points must close loop and commit");
    assert!(state.closed_loop);
    assert_eq!(state.points.len(), 4);
    assert_eq!(state.points[0][2], 1.0, "Closed ring start pressure must restore from 0.08 to 1.0");
    assert_eq!(state.points[3][2], 1.0, "Closed ring end pressure must restore from 0.08 to 1.0");
}

#[test]
fn test_vector_curve_escape_finishes_at_cursor_without_discarding() {
    let mut state = VectorCurveState::default();
    state.on_primary_down(100.0, 100.0, false, false, false, false, false, 1.0);
    state.on_primary_down(200.0, 150.0, false, false, false, false, false, 1.0);
    // Move mouse to (320, 110) — pulling the rubber-band preview
    state.on_pointer_move(320.0, 110.0);

    // Pressing Esc stops the extension right at the mouse position (320, 110)
    assert!(state.finish_at_cursor(1.0));
    assert_eq!(state.points.len(), 3);
    assert_eq!(state.points[2][0], 320.0);
    assert_eq!(state.points[2][1], 110.0);
    assert_eq!(state.points[1][2], 1.0, "Interior node must expand to 1.0 pressure");
    assert_eq!(state.points[2][2], 0.08, "Terminal cursor node tapers to 0.08");
}

/// A document with a Linework layer that holds one tapered stroke.
fn app_with_linework() -> (crate::PhotocraftApp, u64) {
    let mut app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", serde_json::json!({"width": 400, "height": 400})).unwrap();
    app.run("layer.newLinework", serde_json::json!({})).unwrap();
    let layer = app.session.active().unwrap().active_layer.expect("the new layer is active").0;
    app.run(
        "shape.vectorCurve.create",
        serde_json::json!({
            "points": [
                {"x": 50.0, "y": 120.0},
                {"x": 200.0, "y": 60.0},
                {"x": 330.0, "y": 170.0}
            ],
            "width": 6.0,
            "tapered": true,
            "appendToActive": true,
        }),
    )
    .unwrap();
    (app, layer)
}

/// The position of the second node of the first stroke of the layer's live linework.
fn node_position(app: &crate::PhotocraftApp, layer: u64) -> (f64, f64) {
    let st = app.session.active().unwrap();
    let l = st.doc.layer(photocraft_doc::LayerId(layer)).expect("the linework layer is there");
    let live = photocraft_engine::vector_cmds::get_live_linework(l).expect("the layer holds a live linework");
    let node = live.strokes[0].nodes[1];
    (node.p.0, node.p.1)
}

#[test]
fn test_vector_curve_coalesce_memory_stability() {
    let (mut app, layer) = app_with_linework();
    let before = app.session.active().unwrap().history.past_len();

    for step in 0..200 {
        app.run(
            "shape.vectorCurve.updateNode",
            serde_json::json!({
                "layer": layer,
                "strokeIndex": 0,
                "index": 1,
                "pos": [200.0 + f64::from(step) * 0.5, 60.0 + f64::from(step) * 0.25],
                "coalesce": format!("sai-curve-drag-{layer}-0"),
            }),
        )
        .unwrap();
    }

    let after = app.session.active().unwrap().history.past_len();
    assert_eq!(after, before + 1, "200 pointer moves must collapse into one history step");
}

#[test]
fn test_vector_curve_separate_gestures_are_separate_history_steps() {
    let (mut app, layer) = app_with_linework();
    let before = app.session.active().unwrap().history.past_len();

    // Two drags with different coalescing keys are two gestures: two history steps.
    for gesture in 0..2 {
        for step in 0..10 {
            app.run(
                "shape.vectorCurve.updateNode",
                serde_json::json!({
                    "layer": layer,
                    "strokeIndex": 0,
                    "index": 1,
                    "pos": [200.0 + f64::from(step) * 0.5, 60.0 + f64::from(gesture) * 40.0],
                    "coalesce": format!("sai-curve-drag-{layer}-{gesture}"),
                }),
            )
            .unwrap();
        }
    }

    let after = app.session.active().unwrap().history.past_len();
    assert_eq!(after, before + 2, "each gesture is one history step");
}

#[test]
fn test_vector_curve_drag_undo_restores_the_original_node() {
    let (mut app, layer) = app_with_linework();
    let node_before = node_position(&app, layer);

    for step in 0..20 {
        app.run(
            "shape.vectorCurve.updateNode",
            serde_json::json!({
                "layer": layer,
                "strokeIndex": 0,
                "index": 1,
                "pos": [240.0 + f64::from(step), 90.0 + f64::from(step)],
                "coalesce": format!("sai-curve-drag-{layer}-0"),
            }),
        )
        .unwrap();
    }
    assert_ne!(node_position(&app, layer), node_before, "the drag must move the node");

    app.run("edit.undo", serde_json::json!({})).unwrap();
    let node_after = node_position(&app, layer);
    assert!(
        (node_after.0 - node_before.0).abs() < 1e-6 && (node_after.1 - node_before.1).abs() < 1e-6,
        "one undo restores the node, got {node_after:?} instead of {node_before:?}"
    );
}
