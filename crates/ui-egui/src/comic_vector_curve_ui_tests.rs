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
