//! On-screen gizmo for the parametric manga balloons (`crates/ui-egui/src/balloon_gizmo.rs` port).
//!
//! The gizmo is drawn over the canvas and handles the tail manipulators (`TailTip`, `TailBend`,
//! `TailMouthCenter`, `TailMouthWidth`), the vertex handles of the spikes and the cloud lobes
//! (`VertexHandle`), the radial parameter and the count dial of the special bodies
//! (`RadialParam`, `CountDial`), the corner resize (`BodyResize`), the body move (`BodyMove`)
//! and the spawn point of a new tail on the perimeter (`PerimeterSpawnTail`).
//!
//! Every drag is one gesture: the balloon is edited live while the pointer moves, and the whole
//! gesture collapses into a single history step (the command carries a `coalesce` key).
//!
//! This port never reaches into L6 `PhotocraftApp` / `photocraft-engine`. All document state comes
//! through [`BalloonView`], outbound edits are buffered as [`crate::OutCommand`]s, and a hydrated
//! [`LiveBalloon`] is returned in [`BalloonGizmoOutput::updated_balloon`] so the host can sync its
//! panel without a back-reference from this module.
#![forbid(unsafe_code)]

use egui::{Color32, Pos2, Rect, Stroke, vec2};
use photocraft_doc::vector::{LiveBalloon, LiveBalloonBody, LiveBalloonTail, StippleDotOverride, TailPreset, VertexOverride};
use photocraft_doc::{Document, LayerId};
use photocraft_vector::balloon::LiveBalloonGeom;
use serde_json::json;

use crate::theme::Tokens;
use crate::view::{CanvasToolEvent, CanvasViewXform};
use crate::{BridgeDocId, OutCommand};

/// What the pointer hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GizmoHit {
    /// The tip of a tail.
    TailTip { balloon_id: u32, tail_id: u32 },
    /// The sideways bend of a tail.
    TailBend { balloon_id: u32, tail_id: u32 },
    /// One side of the mouth of a tail.
    TailMouthWidth { balloon_id: u32, tail_id: u32, sign: f64 },
    /// The mouth of a tail (drags it along the perimeter).
    TailMouthCenter { balloon_id: u32, tail_id: u32 },
    /// One spike or cloud lobe.
    VertexHandle { balloon_id: u32, vertex_index: u32 },
    /// One manual dot in `BalloonLineStyle::StippleDots` (`Alt`+drag scales radius).
    StippleDot { balloon_id: u32, dot_index: u32 },
    /// The radial parameter of the cloud / spike / uniflash body.
    RadialParam { balloon_id: u32 },
    /// The dial that changes the number of spikes, lobes or needles.
    CountDial { balloon_id: u32 },
    /// One corner of the body (resizes it).
    BodyResize { balloon_id: u32, corner: usize },
    /// The perimeter, where a new tail is spawned.
    PerimeterSpawnTail { balloon_id: u32, t_param: u32 },
    /// The body itself (moves it).
    BodyMove { balloon_id: u32 },
}

/// The drag in progress.
#[derive(Clone, Debug)]
pub struct BalloonGizmoDrag {
    /// Monotonic id of the gesture, so the whole drag is one history step.
    pub gesture_id: u64,
    pub layer_id: u64,
    pub hit: GizmoHit,
    /// The balloon as it was when the drag started.
    pub initial_balloon: LiveBalloon,
    /// Where the pointer was when the drag started, in document px.
    pub start_pos_doc: [f64; 2],
    /// Whether Alt/Option was held at drag start (`Alt`+drag scales stipple dot radius).
    pub alt_held: bool,
}

/// The gizmo's state between frames.
#[derive(Default)]
pub struct BalloonGizmoState {
    pub active_balloon_id: Option<u32>,
    pub hovered: Option<GizmoHit>,
    pub drag: Option<BalloonGizmoDrag>,
    pub next_gesture_id: u64,
    /// Off by default: the gizmo manages the balloon's shape/body, not its 64 stipple dots or its
    /// 20–32 individual vertices. The panel's "Edit individual points" toggle opts in to vertices.
    pub show_vertex_handles: bool,
}

impl BalloonGizmoState {
    /// The next gesture id.
    pub fn alloc_gesture_id(&mut self) -> u64 {
        self.next_gesture_id = self.next_gesture_id.wrapping_add(1);
        self.next_gesture_id
    }
}

/// All view/document state the gizmo needs for one call, packed so no function takes more than
/// seven arguments. `doc_id` is host identity only (it is not used to index the document here).
pub struct BalloonView<'a> {
    pub doc_id: Option<BridgeDocId>,
    pub doc: Option<&'a Document>,
    pub active_layer: Option<LayerId>,
    pub tool_is_balloon: bool,
    pub view_xform: CanvasViewXform,
    /// Pairs of document-space centers `([cx_a, cy_a], [cx_b, cy_b])` for `Stories` chamber links.
    pub story_links: Vec<([f64; 2], [f64; 2])>,
}

/// Deferred result of one pointer event: whether the gizmo consumed it, the commands the host
/// should dispatch, and the live balloon a drag produced (for the host to hydrate its panel with).
#[derive(Default)]
pub struct BalloonGizmoOutput {
    pub handled: bool,
    pub commands: Vec<OutCommand>,
    pub updated_balloon: Option<LiveBalloon>,
}

/// The document positions of a tail's handles.
#[allow(clippy::type_complexity)]
fn tail_handle_positions_doc(balloon: &LiveBalloon, tail: &LiveBalloonTail) -> ([f64; 2], [f64; 2], [f64; 2], [f64; 2], [f64; 2], [f64; 2], f64) {
    let (mouth_center, normal) = balloon.point_and_normal_on_body(tail.perimeter_t);
    let tangent = [-normal[1], normal[0]];
    let half_w = tail.mouth_width * 0.5;
    let mouth_left = [mouth_center[0] - tangent[0] * half_w, mouth_center[1] - tangent[1] * half_w];
    let mouth_right = [mouth_center[0] + tangent[0] * half_w, mouth_center[1] + tangent[1] * half_w];

    let dx = tail.tip[0] - mouth_center[0];
    let dy = tail.tip[1] - mouth_center[1];
    let chord_len = (dx * dx + dy * dy).sqrt().max(1.0);
    let chord_perp = [-dy / chord_len, dx / chord_len];

    let mid = [(mouth_center[0] + tail.tip[0]) * 0.5, (mouth_center[1] + tail.tip[1]) * 0.5];
    let bend_dist = tail.bend * (chord_len * 0.35);
    let bend_handle = [mid[0] + chord_perp[0] * bend_dist, mid[1] + chord_perp[1] * bend_dist];

    (mouth_center, mouth_left, mouth_right, mid, bend_handle, chord_perp, chord_len)
}

/// The two handles the special bodies carry: the radial parameter and the count dial.
fn special_body_handles_doc(body: &LiveBalloonBody) -> Option<([f64; 2], [f64; 2])> {
    let r = body.rect();
    let cx = r[0] + r[2] * 0.5;
    let cy = r[1] + r[3] * 0.5;
    let rx = (r[2] * 0.5).abs();
    let ry = (r[3] * 0.5).abs();

    match body {
        LiveBalloonBody::Cloud { amplitude, .. } => Some(([cx + rx * (1.0 + amplitude), cy], [cx, cy - ry * 1.15])),
        LiveBalloonBody::Spiky { length, .. } => Some(([cx + rx * (1.0 + length * 0.5), cy], [cx, cy - ry * 0.72])),
        LiveBalloonBody::Burst { length, .. } => Some(([cx + rx * (1.0 + length * 0.5), cy], [cx, cy - ry * 0.76])),
        LiveBalloonBody::Uniflash { inner_ratio, .. } => Some(([cx + rx * inner_ratio, cy], [cx, cy - ry * 1.08])),
        _ => None,
    }
}

/// The document positions of the draggable vertices of the spikes and the cloud lobes.
fn vertex_handles_doc(balloon: &LiveBalloon) -> Vec<(u32, [f64; 2])> {
    balloon.vertex_handle_positions()
}

/// What the pointer hits on the gizmo of `balloon`.
pub fn hit_test_balloon_gizmo(xf: &CanvasViewXform, balloon: &LiveBalloon, cursor_scr: Pos2, show_vertex_handles: bool) -> Option<GizmoHit> {
    let hit_r = 8.5_f32;
    let bid = balloon.id;

    for tail in &balloon.tails {
        if tail.preset == TailPreset::None {
            // A disabled tail (`⊘ Нет`) has no geometry and no handles.
            continue;
        }
        let (m_center, m_left, m_right, _, bend_pt, _, _) = tail_handle_positions_doc(balloon, tail);

        let tip_scr = xf.to_screen(tail.tip[0] as f32, tail.tip[1] as f32);
        if cursor_scr.distance(tip_scr) <= hit_r {
            return Some(GizmoHit::TailTip { balloon_id: bid, tail_id: tail.id });
        }

        let bend_scr = xf.to_screen(bend_pt[0] as f32, bend_pt[1] as f32);
        if cursor_scr.distance(bend_scr) <= hit_r {
            return Some(GizmoHit::TailBend { balloon_id: bid, tail_id: tail.id });
        }

        if tail.preset != TailPreset::Thought {
            let ml_scr = xf.to_screen(m_left[0] as f32, m_left[1] as f32);
            if cursor_scr.distance(ml_scr) <= hit_r * 0.85 {
                return Some(GizmoHit::TailMouthWidth { balloon_id: bid, tail_id: tail.id, sign: -1.0 });
            }
            let mr_scr = xf.to_screen(m_right[0] as f32, m_right[1] as f32);
            if cursor_scr.distance(mr_scr) <= hit_r * 0.85 {
                return Some(GizmoHit::TailMouthWidth { balloon_id: bid, tail_id: tail.id, sign: 1.0 });
            }
        }

        let mc_scr = xf.to_screen(m_center[0] as f32, m_center[1] as f32);
        if cursor_scr.distance(mc_scr) <= hit_r {
            return Some(GizmoHit::TailMouthCenter { balloon_id: bid, tail_id: tail.id });
        }
    }

    if let Some((rad_pt, dial_pt)) = special_body_handles_doc(&balloon.body) {
        let rad_scr = xf.to_screen(rad_pt[0] as f32, rad_pt[1] as f32);
        if cursor_scr.distance(rad_scr) <= hit_r {
            return Some(GizmoHit::RadialParam { balloon_id: bid });
        }
        let dial_scr = xf.to_screen(dial_pt[0] as f32, dial_pt[1] as f32);
        if cursor_scr.distance(dial_scr) <= hit_r + 2.0 {
            return Some(GizmoHit::CountDial { balloon_id: bid });
        }
    }

    // The frame corners resize the body, so they win over a vertex handle sitting on the same
    // spot (an oval bulge dragged towards a corner must not steal the resize).
    let r = balloon.body.rect();
    let corners = [[r[0], r[1]], [r[0] + r[2], r[1]], [r[0] + r[2], r[1] + r[3]], [r[0], r[1] + r[3]]];
    for (idx, c) in corners.iter().enumerate() {
        let c_scr = xf.to_screen(c[0] as f32, c[1] as f32);
        if cursor_scr.distance(c_scr) <= hit_r {
            return Some(GizmoHit::BodyResize { balloon_id: bid, corner: idx });
        }
    }

    // Individual spike/cloud vertices are an explicit opt-in. Stipple dots are never draggable
    // handles: their positions are derived from the body each frame, so a point edit could leave
    // an override hanging after the dot count or the body changes.
    if show_vertex_handles {
        for (v_idx, v_pt) in vertex_handles_doc(balloon) {
            let v_scr = xf.to_screen(v_pt[0] as f32, v_pt[1] as f32);
            if cursor_scr.distance(v_scr) <= hit_r {
                return Some(GizmoHit::VertexHandle { balloon_id: bid, vertex_index: v_idx });
            }
        }
    }

    let cur_doc = xf.to_doc(cursor_scr);
    // Uniflash takes tails too (TZ v5.0): the mouth welds to the flash's inner ellipse.
    let t = balloon.closest_perimeter_t(cur_doc);
    let (on_perim, _) = balloon.point_and_normal_on_body(t);
    let perim_scr = xf.to_screen(on_perim[0] as f32, on_perim[1] as f32);
    if cursor_scr.distance(perim_scr) <= 7.0 {
        return Some(GizmoHit::PerimeterSpawnTail { balloon_id: bid, t_param: (t * 10000.0).round() as u32 });
    }

    let cx = r[0] + r[2] * 0.5;
    let cy = r[1] + r[3] * 0.5;
    let rx = (r[2] * 0.5).abs().max(1.0);
    let ry = (r[3] * 0.5).abs().max(1.0);
    let norm_dx = (cur_doc[0] - cx) / rx;
    let norm_dy = (cur_doc[1] - cy) / ry;
    if norm_dx * norm_dx + norm_dy * norm_dy <= 1.0 {
        return Some(GizmoHit::BodyMove { balloon_id: bid });
    }

    None
}

/// Draws the gizmo of `balloon` on the canvas.
pub fn draw_balloon_gizmo(painter: &egui::Painter, xf: &CanvasViewXform, balloon: &LiveBalloon, state: &BalloonGizmoState) {
    let accent = Tokens::get(painter.ctx()).accent;
    let white = Color32::WHITE;
    let yellow = Color32::from_rgb(255, 205, 55);
    let bid = balloon.id;

    // 1. The bounding frame.
    let r = balloon.body.rect();
    let tl = xf.to_screen(r[0] as f32, r[1] as f32);
    let br = xf.to_screen((r[0] + r[2]) as f32, (r[1] + r[3]) as f32);
    let box_scr = Rect::from_two_pos(tl, br);
    painter.rect_stroke(box_scr, 0.0, Stroke::new(1.0, accent.gamma_multiply(0.35)), egui::StrokeKind::Outside);

    let corners = [[r[0], r[1]], [r[0] + r[2], r[1]], [r[0] + r[2], r[1] + r[3]], [r[0], r[1] + r[3]]];
    for (idx, c) in corners.iter().enumerate() {
        let c_scr = xf.to_screen(c[0] as f32, c[1] as f32);
        let hov = state.hovered == Some(GizmoHit::BodyResize { balloon_id: bid, corner: idx });
        let sz = if hov { 8.0 } else { 6.0 };
        painter.rect_filled(Rect::from_center_size(c_scr, vec2(sz, sz)), 1.0, if hov { accent } else { white });
        painter.rect_stroke(Rect::from_center_size(c_scr, vec2(sz, sz)), 1.0, Stroke::new(1.0, accent), egui::StrokeKind::Outside);
    }

    // 2. The area a text is laid out in.
    let safe = balloon.safe_text_rect();
    let s_tl = xf.to_screen(safe[0] as f32, safe[1] as f32);
    let s_br = xf.to_screen((safe[0] + safe[2]) as f32, (safe[1] + safe[3]) as f32);
    painter.rect_stroke(Rect::from_two_pos(s_tl, s_br), 2.0, Stroke::new(0.8, Color32::from_rgba_unmultiplied(120, 200, 255, 70)), egui::StrokeKind::Inside);

    // 3. The "+" that spawns a new tail.
    if let Some(GizmoHit::PerimeterSpawnTail { balloon_id, t_param }) = state.hovered
        && balloon_id == bid
    {
        let (pt, _) = balloon.point_and_normal_on_body((t_param as f64) / 10000.0);
        let p_scr = xf.to_screen(pt[0] as f32, pt[1] as f32);
        painter.circle_filled(p_scr, 7.5, accent);
        painter.line_segment([p_scr - vec2(4.0, 0.0), p_scr + vec2(4.0, 0.0)], Stroke::new(1.5, white));
        painter.line_segment([p_scr - vec2(0.0, 4.0), p_scr + vec2(0.0, 4.0)], Stroke::new(1.5, white));
    }

    // 4. The tail manipulators (a disabled tail is skipped: it has no geometry).
    for tail in &balloon.tails {
        if tail.preset == TailPreset::None {
            continue;
        }
        let (m_center, m_left, m_right, mid, bend_pt, _, _) = tail_handle_positions_doc(balloon, tail);
        let tip_scr = xf.to_screen(tail.tip[0] as f32, tail.tip[1] as f32);
        let mc_scr = xf.to_screen(m_center[0] as f32, m_center[1] as f32);
        let mid_scr = xf.to_screen(mid[0] as f32, mid[1] as f32);
        let bend_scr = xf.to_screen(bend_pt[0] as f32, bend_pt[1] as f32);

        painter.line_segment([mc_scr, tip_scr], Stroke::new(1.0, accent.gamma_multiply(0.45)));
        painter.line_segment([mid_scr, bend_scr], Stroke::new(1.0, yellow));

        if tail.preset != TailPreset::Thought {
            for (pt, sign) in [(m_left, -1.0), (m_right, 1.0)] {
                let p_scr = xf.to_screen(pt[0] as f32, pt[1] as f32);
                let hov = state.hovered == Some(GizmoHit::TailMouthWidth { balloon_id: bid, tail_id: tail.id, sign });
                painter.circle_filled(p_scr, if hov { 5.0 } else { 3.5 }, white);
                painter.circle_stroke(p_scr, if hov { 5.0 } else { 3.5 }, Stroke::new(1.0, accent));
            }
        }

        let bend_hov = state.hovered == Some(GizmoHit::TailBend { balloon_id: bid, tail_id: tail.id });
        painter.circle_filled(bend_scr, if bend_hov { 6.0 } else { 4.5 }, yellow);
        painter.circle_stroke(bend_scr, if bend_hov { 6.0 } else { 4.5 }, Stroke::new(1.0, Color32::BLACK));

        let tip_hov = state.hovered == Some(GizmoHit::TailTip { balloon_id: bid, tail_id: tail.id });
        painter.circle_filled(tip_scr, if tip_hov { 7.0 } else { 5.0 }, accent);
        painter.circle_stroke(tip_scr, if tip_hov { 7.0 } else { 5.0 }, Stroke::new(1.5, white));

        let mc_hov = state.hovered == Some(GizmoHit::TailMouthCenter { balloon_id: bid, tail_id: tail.id });
        let r_m = Rect::from_center_size(mc_scr, vec2(8.0, 8.0));
        painter.rect_filled(r_m, 2.0, if mc_hov { accent } else { white });
        painter.rect_stroke(r_m, 2.0, Stroke::new(1.0, accent), egui::StrokeKind::Outside);
    }

    // 5. The handles of the cloud, spike and uniflash bodies.
    if let Some((rad_pt, dial_pt)) = special_body_handles_doc(&balloon.body) {
        let rad_scr = xf.to_screen(rad_pt[0] as f32, rad_pt[1] as f32);
        let rad_hov = state.hovered == Some(GizmoHit::RadialParam { balloon_id: bid });
        painter.circle_filled(rad_scr, if rad_hov { 6.5 } else { 4.5 }, accent);
        painter.circle_stroke(rad_scr, if rad_hov { 6.5 } else { 4.5 }, Stroke::new(1.0, white));

        let dial_scr = xf.to_screen(dial_pt[0] as f32, dial_pt[1] as f32);
        let dial_hov = state.hovered == Some(GizmoHit::CountDial { balloon_id: bid });
        let d_rect = Rect::from_center_size(dial_scr, vec2(16.0, 16.0));
        painter.rect_filled(d_rect, 3.0, if dial_hov { yellow } else { white });
        painter.rect_stroke(d_rect, 3.0, Stroke::new(1.0, Color32::BLACK), egui::StrokeKind::Outside);
    }

    // 6. Individual spike/cloud vertices, only with "Edit individual points" on.
    if state.show_vertex_handles {
        for (v_idx, v_pt) in vertex_handles_doc(balloon) {
            let v_scr = xf.to_screen(v_pt[0] as f32, v_pt[1] as f32);
            let v_hov = state.hovered == Some(GizmoHit::VertexHandle { balloon_id: bid, vertex_index: v_idx });
            let sz = if v_hov { 5.5 } else { 3.5 };
            painter.circle_filled(v_scr, sz, if v_hov { yellow } else { accent });
            painter.circle_stroke(v_scr, sz, Stroke::new(1.0, white));
        }
    }
    // Stipple dots are part of the line style, not gizmo handles: no overlay, no hit target.
}

/// Starts a gizmo drag. `Alt` + click on a tail tip removes the tail (emitted as a command).
fn on_pointer_down(
    state: &mut BalloonGizmoState,
    cmds: &mut Vec<OutCommand>,
    layer_id: u64,
    balloon: &LiveBalloon,
    hit: GizmoHit,
    pos_doc: [f64; 2],
    alt_held: bool,
) {
    state.active_balloon_id = Some(balloon.id);
    log::info!(
        "[Telemetry:Balloon:GizmoDown] hit={hit:?}, layer_id={layer_id}, pos_doc={pos_doc:?}, \
         body_rect={:?}, tails_count={}",
        balloon.body.rect(),
        balloon.tails.len()
    );
    if alt_held && let GizmoHit::TailTip { balloon_id, tail_id } = hit {
        cmds.push(OutCommand::new("shape.balloon.tail.remove", json!({ "layer": layer_id, "balloonId": balloon_id, "tailId": tail_id })));
        return;
    }

    let gesture_id = state.alloc_gesture_id();

    if let GizmoHit::PerimeterSpawnTail { balloon_id, t_param } = hit {
        let mut initial = balloon.clone();
        let new_tail_id = initial.next_tail_id;
        initial.next_tail_id += 1;
        initial.tails.push(LiveBalloonTail {
            id: new_tail_id,
            preset: TailPreset::Arc,
            perimeter_t: (t_param as f64) / 10000.0,
            mouth_width: 24.0,
            tip: pos_doc,
            bend: 0.15,
            bubbles: 3,
            bubble_radius: 10.0,
        });
        state.drag = Some(BalloonGizmoDrag {
            gesture_id,
            layer_id,
            hit: GizmoHit::TailTip { balloon_id, tail_id: new_tail_id },
            initial_balloon: initial,
            start_pos_doc: pos_doc,
            alt_held: false,
        });
        return;
    }

    state.drag = Some(BalloonGizmoDrag { gesture_id, layer_id, hit, initial_balloon: balloon.clone(), start_pos_doc: pos_doc, alt_held });
}

/// Applies the drag to a fresh copy of the balloon, buffers the update command and returns the
/// hydrated balloon. The whole gesture shares one `coalesce` key, so it is one history step.
fn on_drag_move(drag: &BalloonGizmoDrag, cur_doc: [f64; 2], cmds: &mut Vec<OutCommand>) -> Option<LiveBalloon> {
    let mut updated = drag.initial_balloon.clone();
    let dx = cur_doc[0] - drag.start_pos_doc[0];
    let dy = cur_doc[1] - drag.start_pos_doc[1];
    apply_drag(&mut updated, drag, dx, dy, cur_doc);
    log_tail_telemetry(&updated, drag, cur_doc, dx, dy);
    let coalesce_key = format!("balloon-gizmo-drag-{}-{}", drag.layer_id, drag.gesture_id);
    cmds.push(OutCommand::new("shape.balloon.update", json!({ "layer": drag.layer_id, "balloon": &updated, "coalesce": coalesce_key })));
    Some(updated)
}

/// Dispatches a move to the geometry helper that owns the grabbed handle family. Kept small so
/// each helper stays well under the workspace cognitive-complexity budget.
fn apply_drag(updated: &mut LiveBalloon, drag: &BalloonGizmoDrag, dx: f64, dy: f64, cur_doc: [f64; 2]) {
    match drag.hit {
        GizmoHit::TailTip { .. } | GizmoHit::TailBend { .. } | GizmoHit::TailMouthCenter { .. } | GizmoHit::TailMouthWidth { .. } => {
            apply_tail_drag(updated, drag, dx, dy, cur_doc)
        }
        GizmoHit::VertexHandle { vertex_index, .. } => apply_vertex_drag(updated, drag, vertex_index, dx, dy),
        GizmoHit::StippleDot { dot_index, .. } => apply_stipple_drag(updated, drag, dot_index, dx, dy),
        GizmoHit::RadialParam { .. } | GizmoHit::CountDial { .. } => apply_special_drag(updated, drag.hit, dx),
        GizmoHit::BodyResize { corner, .. } => updated.body.set_rect(apply_body_resize(drag.initial_balloon.body.rect(), corner, dx, dy)),
        GizmoHit::BodyMove { .. } => apply_body_move(updated, drag, dx, dy),
        GizmoHit::PerimeterSpawnTail { .. } => {}
    }
}

/// Tail drags: the tip, the sideways bend, the mouth position and the mouth width.
fn apply_tail_drag(updated: &mut LiveBalloon, drag: &BalloonGizmoDrag, dx: f64, dy: f64, cur_doc: [f64; 2]) {
    match drag.hit {
        GizmoHit::TailTip { tail_id, .. } => {
            if let Some(t) = updated.tails.iter_mut().find(|t| t.id == tail_id) {
                t.tip = [t.tip[0] + dx, t.tip[1] + dy];
            }
        }
        GizmoHit::TailBend { tail_id, .. } => {
            if let Some(init_t) = drag.initial_balloon.tails.iter().find(|t| t.id == tail_id) {
                let (_, _, _, mid, _, chord_perp, chord_len) = tail_handle_positions_doc(&drag.initial_balloon, init_t);
                let rel_x = cur_doc[0] - mid[0];
                let rel_y = cur_doc[1] - mid[1];
                let proj = rel_x * chord_perp[0] + rel_y * chord_perp[1];
                if let Some(t) = updated.tails.iter_mut().find(|t| t.id == tail_id) {
                    t.bend = (proj / (chord_len * 0.35).max(8.0)).clamp(-1.0, 1.0);
                }
            }
        }
        GizmoHit::TailMouthCenter { tail_id, .. } => {
            let new_t = updated.closest_perimeter_t(cur_doc);
            if let Some(t) = updated.tails.iter_mut().find(|t| t.id == tail_id) {
                t.perimeter_t = new_t;
            }
        }
        GizmoHit::TailMouthWidth { tail_id, sign, .. } => {
            if let Some(init_t) = drag.initial_balloon.tails.iter().find(|t| t.id == tail_id) {
                let (_, normal) = drag.initial_balloon.point_and_normal_on_body(init_t.perimeter_t);
                let tangent = [-normal[1], normal[0]];
                let tang_delta = (dx * tangent[0] + dy * tangent[1]) * sign;
                if let Some(t) = updated.tails.iter_mut().find(|t| t.id == tail_id) {
                    t.mouth_width = (init_t.mouth_width + tang_delta * 2.0).clamp(4.0, 300.0);
                }
            }
        }
        _ => {}
    }
}

/// A spike / cloud-lobe vertex drag, clamped on an oval so the spline does not fold through its
/// neighbours (~42 % of the body's short side).
fn apply_vertex_drag(updated: &mut LiveBalloon, drag: &BalloonGizmoDrag, vertex_index: u32, dx: f64, dy: f64) {
    let base_off = drag.initial_balloon.get_vertex_offset(vertex_index);
    let mut new_off = [base_off[0] + dx, base_off[1] + dy];
    if matches!(drag.initial_balloon.body, LiveBalloonBody::Ellipse { .. }) {
        let r = drag.initial_balloon.body.rect();
        let limit = (r[2].abs().min(r[3].abs()) * 0.42).max(4.0);
        new_off = [new_off[0].clamp(-limit, limit), new_off[1].clamp(-limit, limit)];
    }
    if let Some(ov) = updated.vertex_overrides.iter_mut().find(|v| v.index == vertex_index) {
        ov.offset = new_off;
    } else {
        updated.vertex_overrides.push(VertexOverride { index: vertex_index, offset: new_off });
    }
}

/// A stipple dot drag: `Alt` scales its radius, otherwise its position moves.
fn apply_stipple_drag(updated: &mut LiveBalloon, drag: &BalloonGizmoDrag, dot_index: u32, dx: f64, dy: f64) {
    let base_ov = drag.initial_balloon.stipple_overrides.iter().find(|s| s.index == dot_index).map(|s| (s.offset, s.radius_scale)).unwrap_or(([0.0, 0.0], 1.0));
    let (new_off, new_scale) = if drag.alt_held {
        let rx = (drag.initial_balloon.body.rect()[2] * 0.5).abs().max(20.0);
        (base_ov.0, (base_ov.1 * (1.0 + dx / rx)).clamp(0.2, 4.0))
    } else {
        ([base_ov.0[0] + dx, base_ov.0[1] + dy], base_ov.1)
    };
    if let Some(ov) = updated.stipple_overrides.iter_mut().find(|s| s.index == dot_index) {
        ov.offset = new_off;
        ov.radius_scale = new_scale;
    } else {
        updated.stipple_overrides.push(StippleDotOverride { index: dot_index, offset: new_off, radius_scale: new_scale });
    }
}

/// The radial parameter and the count dial of the special (cloud / spiky / burst / uniflash) bodies.
fn apply_special_drag(updated: &mut LiveBalloon, hit: GizmoHit, dx: f64) {
    match hit {
        GizmoHit::RadialParam { .. } => {
            let rx = (updated.body.rect()[2] * 0.5).abs().max(10.0);
            match &mut updated.body {
                LiveBalloonBody::Cloud { amplitude, .. } => *amplitude = (*amplitude + dx / rx).clamp(0.04, 0.55),
                LiveBalloonBody::Spiky { length, .. } => *length = (*length + (dx * 2.0) / rx).clamp(0.05, 1.50),
                LiveBalloonBody::Burst { length, .. } => *length = (*length + (dx * 2.0) / rx).clamp(0.10, 1.20),
                LiveBalloonBody::Uniflash { inner_ratio, .. } => *inner_ratio = (*inner_ratio + dx / rx).clamp(0.30, 0.90),
                _ => {}
            }
        }
        GizmoHit::CountDial { .. } => {
            let delta = (dx * 0.15).round() as i32;
            match &mut updated.body {
                LiveBalloonBody::Cloud { bumps, .. } => *bumps = ((*bumps as i32 + delta).clamp(4, 64)) as u32,
                LiveBalloonBody::Spiky { spikes, .. } => *spikes = ((*spikes as i32 + delta).clamp(5, 128)) as u32,
                LiveBalloonBody::Burst { rays, .. } => *rays = ((*rays as i32 + delta).clamp(5, 64)) as u32,
                LiveBalloonBody::Uniflash { needles, .. } => *needles = ((*needles as i32 + delta * 2).clamp(32, 450)) as u32,
                _ => {}
            }
        }
        _ => {}
    }
}

/// A body move: translate the body and carry every tail tip with it.
fn apply_body_move(updated: &mut LiveBalloon, drag: &BalloonGizmoDrag, dx: f64, dy: f64) {
    let mut r = drag.initial_balloon.body.rect();
    r[0] += dx;
    r[1] += dy;
    updated.body.set_rect(r);
    for t in &mut updated.tails {
        t.tip[0] += dx;
        t.tip[1] += dy;
    }
}

/// Emits the fork's diagnostic for a tail drag: how the tip sits relative to the body ellipse.
fn log_tail_telemetry(updated: &LiveBalloon, drag: &BalloonGizmoDrag, cur_doc: [f64; 2], dx: f64, dy: f64) {
    if (dx * dx + dy * dy) <= 0.01 {
        return;
    }
    let Some(tail_id) = (match drag.hit {
        GizmoHit::TailTip { tail_id, .. }
        | GizmoHit::TailBend { tail_id, .. }
        | GizmoHit::TailMouthCenter { tail_id, .. }
        | GizmoHit::TailMouthWidth { tail_id, .. } => Some(tail_id),
        _ => None,
    }) else {
        return;
    };
    let Some(t) = updated.tails.iter().find(|t| t.id == tail_id) else { return };
    let r = updated.body.rect();
    let cx = r[0] + r[2] * 0.5;
    let cy = r[1] + r[3] * 0.5;
    let rx = (r[2] * 0.5).abs().max(1.0);
    let ry = (r[3] * 0.5).abs().max(1.0);
    let norm_dx = (t.tip[0] - cx) / rx;
    let norm_dy = (t.tip[1] - cy) / ry;
    let ellipse_dist = (norm_dx * norm_dx + norm_dy * norm_dy).sqrt();
    let is_tip_inside_body = ellipse_dist < 1.0;
    let (mouth_center, _) = updated.point_and_normal_on_body(t.perimeter_t);
    log::info!(
        "[Telemetry:Balloon:GizmoDrag] hit={:?}, cur_doc=({:.1}, {:.1}), \
         center=({cx:.1}, {cy:.1}), radii=({rx:.1}, {ry:.1}), tip=({:.1}, {:.1}), mouth=({:.1}, {:.1}) | \
         ellipse_dist={ellipse_dist:.3}, is_tip_inside_body={is_tip_inside_body}",
        drag.hit,
        cur_doc[0],
        cur_doc[1],
        t.tip[0],
        t.tip[1],
        mouth_center[0],
        mouth_center[1]
    );
}

/// Ends the drag.
pub fn on_pointer_up(state: &mut BalloonGizmoState) {
    state.drag = None;
}

/// The balloons of the active layer, when it is a balloon group.
pub fn active_balloons_in_doc(doc: &Document, active_layer: Option<LayerId>) -> Option<(u64, Vec<LiveBalloon>)> {
    let lid = active_layer?;
    let layer = doc.layer(lid)?;
    let photocraft_doc::LayerContent::Shape(sh) = &layer.content else { return None };
    let Some(photocraft_doc::LiveShape::BalloonGroup { group }) = &sh.live else { return None };
    Some((lid.0, group.items.clone()))
}

/// Applies clamped opposite-edge resize for `GizmoHit::BodyResize` so dragging across an anchor never flips width/height or pushes `x`/`y` negative.
fn apply_body_resize(init: [f64; 4], corner: usize, dx: f64, dy: f64) -> [f64; 4] {
    if !dx.is_finite() || !dy.is_finite() {
        return init;
    }
    let mut r = init;
    let min_size = 24.0;
    let right = init[0] + init[2];
    let bottom = init[1] + init[3];
    match corner {
        0 => {
            r[2] = (init[2] - dx).max(min_size);
            r[3] = (init[3] - dy).max(min_size);
            r[0] = right - r[2];
            r[1] = bottom - r[3];
        }
        1 => {
            r[2] = (init[2] + dx).max(min_size);
            r[3] = (init[3] - dy).max(min_size);
            r[1] = bottom - r[3];
        }
        2 => {
            r[2] = (init[2] + dx).max(min_size);
            r[3] = (init[3] + dy).max(min_size);
        }
        3 => {
            r[2] = (init[2] - dx).max(min_size);
            r[3] = (init[3] + dy).max(min_size);
            r[0] = right - r[2];
        }
        _ => {}
    }
    r
}

/// The handle under the pointer, if any.
fn hover_at(balloons: &[LiveBalloon], xf: &CanvasViewXform, doc: [f64; 2], show_vertex_handles: bool) -> Option<GizmoHit> {
    let cursor = xf.to_screen(doc[0] as f32, doc[1] as f32);
    for b in balloons {
        if let Some(hit) = hit_test_balloon_gizmo(xf, b, cursor, show_vertex_handles) {
            return Some(hit);
        }
    }
    None
}

/// Whether the gizmo is live: the balloon tool is active, or a drag is still in flight (so it can
/// finish even if the tool changes mid-gesture).
fn showing(state: &BalloonGizmoState, view: &BalloonView<'_>) -> bool {
    view.tool_is_balloon || state.drag.is_some()
}

/// Maps a hit to the cursor that communicates its action.
fn cursor_of(hit: GizmoHit) -> egui::CursorIcon {
    match hit {
        GizmoHit::TailTip { .. }
        | GizmoHit::TailBend { .. }
        | GizmoHit::TailMouthWidth { .. }
        | GizmoHit::TailMouthCenter { .. }
        | GizmoHit::VertexHandle { .. }
        | GizmoHit::StippleDot { .. }
        | GizmoHit::RadialParam { .. }
        | GizmoHit::CountDial { .. }
        | GizmoHit::PerimeterSpawnTail { .. } => egui::CursorIcon::PointingHand,
        GizmoHit::BodyResize { corner: 0 | 2, .. } => egui::CursorIcon::ResizeNwSe,
        GizmoHit::BodyResize { .. } => egui::CursorIcon::ResizeNeSw,
        GizmoHit::BodyMove { .. } => egui::CursorIcon::Move,
    }
}

/// Takes the pointer events the gizmo needs. The output reports whether the gizmo handled the
/// event, the commands to dispatch, and the live balloon a drag produced (for the host to sync).
pub fn pointer(state: &mut BalloonGizmoState, view: &BalloonView<'_>, ev: CanvasToolEvent, mods: egui::Modifiers) -> BalloonGizmoOutput {
    let mut out = BalloonGizmoOutput::default();
    if !showing(state, view) {
        return out;
    }
    let Some(doc) = view.doc else { return out };
    let Some((layer_id, balloons)) = active_balloons_in_doc(doc, view.active_layer) else { return out };
    let xf = &view.view_xform;

    match ev {
        CanvasToolEvent::Down { x, y, .. } => {
            let cursor = xf.to_screen(x as f32, y as f32);
            for b in &balloons {
                if let Some(hit) = hit_test_balloon_gizmo(xf, b, cursor, state.show_vertex_handles) {
                    on_pointer_down(state, &mut out.commands, layer_id, b, hit, [x, y], mods.alt);
                    out.handled = true;
                    break;
                }
            }
        }
        CanvasToolEvent::Move { x, y, .. } => {
            if let Some(drag) = state.drag.clone() {
                out.updated_balloon = on_drag_move(&drag, [x, y], &mut out.commands);
                out.handled = true;
            } else if view.tool_is_balloon {
                // No drag: only remember what the pointer is over, so the handles light up.
                let hovered = hover_at(&balloons, xf, [x, y], state.show_vertex_handles);
                if state.hovered != hovered {
                    state.hovered = hovered;
                }
            }
        }
        CanvasToolEvent::Up { .. } => {
            if state.drag.is_some() {
                on_pointer_up(state);
                out.handled = true;
            }
        }
    }
    out
}

/// The cursor over the canvas while the gizmo lives: the grabbed handle's, or the hovered one's.
pub fn hover_cursor(state: &BalloonGizmoState, view: &BalloonView<'_>, cursor_scr: Option<egui::Pos2>) -> Option<egui::CursorIcon> {
    if !showing(state, view) {
        return None;
    }
    let hit = if let Some(drag) = &state.drag {
        Some(drag.hit)
    } else {
        let doc = view.doc?;
        let (_, balloons) = active_balloons_in_doc(doc, view.active_layer)?;
        let at = cursor_scr?;
        balloons.iter().find_map(|b| hit_test_balloon_gizmo(&view.view_xform, b, at, state.show_vertex_handles))
    }?;
    Some(cursor_of(hit))
}

/// Draws the dashed `Stories` link arrow between two chamber centers in document space.
fn draw_story_link(painter: &egui::Painter, xf: &CanvasViewXform, from_doc: [f64; 2], to_doc: [f64; 2]) {
    let a = xf.to_screen(from_doc[0] as f32, from_doc[1] as f32);
    let b = xf.to_screen(to_doc[0] as f32, to_doc[1] as f32);
    let diff = b - a;
    let dist = diff.length();
    if dist < 6.0 || !dist.is_finite() {
        return;
    }
    let dir = diff / dist;
    let perp = vec2(-dir.y, dir.x);
    let color = Color32::from_rgba_unmultiplied(70, 190, 255, 220);
    let stroke = Stroke::new(1.6, color);

    let dash = 7.0f32;
    let gap = 5.0f32;
    let mut pos = 0.0f32;
    let stop = (dist - 8.0).max(0.0);
    while pos < stop {
        let end = (pos + dash).min(stop);
        painter.line_segment([a + dir * pos, a + dir * end], stroke);
        pos += dash + gap;
    }

    let head = [b, b - dir * 9.0 + perp * 4.5, b - dir * 9.0 - perp * 4.5];
    painter.add(egui::Shape::convex_polygon(head.to_vec(), color, Stroke::NONE));
}

fn resolve_story_links(doc: &Document, balloons: &[LiveBalloon], explicit: &[([f64; 2], [f64; 2])]) -> Vec<([f64; 2], [f64; 2])> {
    if !explicit.is_empty() {
        return explicit.to_vec();
    }
    let center_of = |b: &LiveBalloon| {
        let r = b.body.rect();
        [r[0] + r[2] * 0.5, r[1] + r[3] * 0.5]
    };
    let mut links = Vec::new();
    for b in balloons {
        let Some(tid) = b.linked_text_layer else { continue };
        let Some(layer) = doc.layer(tid) else { continue };
        let photocraft_doc::LayerContent::Text(tl) = &layer.content else { continue };
        let Some(next_id) = tl.next_story_layer else { continue };
        if let Some(target_b) = balloons.iter().find(|cand| cand.linked_text_layer == Some(next_id)) {
            links.push((center_of(b), center_of(target_b)));
        }
    }
    links
}

/// Draws the gizmo for every balloon of the active balloon group, plus `Stories` link overlays.
pub fn draw(painter: &egui::Painter, state: &BalloonGizmoState, view: &BalloonView<'_>) {
    if !showing(state, view) {
        return;
    }
    let Some(doc) = view.doc else { return };
    let Some((_, balloons)) = active_balloons_in_doc(doc, view.active_layer) else { return };
    for b in &balloons {
        draw_balloon_gizmo(painter, &view.view_xform, b, state);
    }
    for (from, to) in resolve_story_links(doc, &balloons, &view.story_links) {
        draw_story_link(painter, &view.view_xform, from, to);
    }
}

#[cfg(test)]
mod tests {
    use egui::{Pos2, Rect, pos2, vec2};
    use photocraft_doc::vector::{BalloonLineStyle, LiveBalloon, LiveBalloonBody, StippleDotOverride, VertexOverride};
    use photocraft_vector::balloon::LiveBalloonGeom;

    use super::{BalloonGizmoState, GizmoHit, apply_body_resize, draw_balloon_gizmo, hit_test_balloon_gizmo, vertex_handles_doc};
    use crate::view::CanvasViewXform;

    fn balloon() -> LiveBalloon {
        LiveBalloon::default()
    }

    fn one_to_one() -> CanvasViewXform {
        CanvasViewXform {
            rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0)),
            zoom: 1.0,
            center: [0.0, 0.0],
            flip: false,
            rotation: 0.0,
            aspect: 1.0,
        }
    }

    #[test]
    fn a_vertex_override_is_read_back() {
        let mut b = balloon();
        assert_eq!(b.get_vertex_offset(3), [0.0, 0.0]);
        b.vertex_overrides.push(VertexOverride { index: 3, offset: [5.0, -2.0] });
        assert_eq!(b.get_vertex_offset(3), [5.0, -2.0]);
        assert_eq!(b.get_vertex_offset(4), [0.0, 0.0]);
    }

    #[test]
    fn the_perimeter_parameter_is_a_full_turn() {
        let b = balloon();
        let top = b.point_and_normal_on_body(0.0).0;
        let bottom = b.point_and_normal_on_body(0.5).0;
        assert!((top[1] - bottom[1]).abs() > 1.0, "{top:?} {bottom:?}");
        let t = b.closest_perimeter_t(top);
        assert!(!(0.01..=0.99).contains(&t), "{t}");
    }

    #[test]
    fn the_safe_text_rect_is_inside_the_body() {
        let b = balloon();
        let safe = b.safe_text_rect();
        let r = b.body.rect();
        assert!(safe[0] >= r[0] - 0.001 && safe[1] >= r[1] - 0.001, "{safe:?} {r:?}");
        assert!(safe[0] + safe[2] <= r[0] + r[2] + 0.001, "{safe:?} {r:?}");
    }

    #[test]
    fn test_body_resize_left_drag_clamps_without_negative_flip() {
        let init = [100.0, 120.0, 200.0, 140.0];
        let ne = apply_body_resize(init, 1, -500.0, 20.0);
        assert_eq!(ne[0], 100.0);
        assert_eq!(ne[2], 24.0);
        let se = apply_body_resize(init, 2, -500.0, -500.0);
        assert_eq!(se[0], 100.0);
        assert_eq!(se[1], 120.0);
        assert_eq!(se[2], 24.0);
        assert_eq!(se[3], 24.0);
        let nw = apply_body_resize(init, 0, 500.0, 500.0);
        assert_eq!(nw[0], 276.0);
        assert_eq!(nw[1], 236.0);
        assert_eq!(nw[2], 24.0);
        assert_eq!(nw[3], 24.0);
    }

    #[test]
    fn dragging_one_vertex_handle_or_stipple_dot_moves_only_that_handle_position() {
        let mut b = LiveBalloon {
            body: LiveBalloonBody::Spiky {
                rect: [40.0, 40.0, 220.0, 140.0],
                spikes: 14,
                length: 0.35,
                sub_harmonic: 0.4,
                concavity: 0.5,
                jitter: 0.2,
                smooth: false,
                seed: 7,
            },
            line_style: BalloonLineStyle::StippleDots,
            stipple_count: 32,
            ..LiveBalloon::default()
        };
        let handles_before = vertex_handles_doc(&b);
        assert!(handles_before.len() >= 10);
        b.vertex_overrides.push(VertexOverride { index: 2, offset: [18.0, -14.0] });
        let handles_after = vertex_handles_doc(&b);
        assert!((handles_after[2].1[0] - (handles_before[2].1[0] + 18.0)).abs() < 1e-4);
        assert!((handles_after[2].1[1] - (handles_before[2].1[1] - 14.0)).abs() < 1e-4);
        assert!((handles_after[3].1[0] - handles_before[3].1[0]).abs() < 1e-4);

        let dots_before = b.stipple_dot_positions();
        b.stipple_overrides.push(StippleDotOverride { index: 1, offset: [10.0, -6.0], radius_scale: 1.5 });
        let dots_after = b.stipple_dot_positions();
        assert!((dots_after[1].1[0] - (dots_before[1].1[0] + 10.0)).abs() < 1e-4);
        assert!((dots_after[1].1[1] - (dots_before[1].1[1] - 6.0)).abs() < 1e-4);
        assert!((dots_after[2].1[0] - dots_before[2].1[0]).abs() < 1e-4);
    }

    #[test]
    fn the_tail_tip_is_hit_at_its_screen_position() {
        let b = balloon();
        let xf = one_to_one();
        let tip = b.tails[0].tip;
        let at: Pos2 = xf.to_screen(tip[0] as f32, tip[1] as f32);
        assert_eq!(hit_test_balloon_gizmo(&xf, &b, at, false), Some(GizmoHit::TailTip { balloon_id: b.id, tail_id: b.tails[0].id }));
    }

    #[test]
    fn far_away_points_hit_nothing() {
        let b = balloon();
        let xf = one_to_one();
        assert_eq!(hit_test_balloon_gizmo(&xf, &b, pos2(780.0, 580.0), false), None);
    }

    /// The gizmo manages the balloon's shape and body by default: no 64 stipple dots, no 20–32
    /// individual vertices are drawn or hit-tested until "Edit individual points" is switched on.
    #[test]
    fn only_the_body_handles_are_live_until_individual_points_are_enabled() {
        for body in [
            LiveBalloonBody::Cloud { rect: [40.0, 40.0, 220.0, 140.0], bumps: 14, amplitude: 0.3, cusp_bite: 0.22, jitter: 0.2, seed: 7 },
            LiveBalloonBody::Spiky {
                rect: [40.0, 40.0, 220.0, 140.0],
                spikes: 14,
                length: 0.35,
                sub_harmonic: 0.4,
                concavity: 0.5,
                jitter: 0.2,
                smooth: false,
                seed: 7,
            },
        ] {
            let mut b = LiveBalloon { body, line_style: BalloonLineStyle::StippleDots, stipple_count: 64, ..LiveBalloon::default() };
            let xf = one_to_one();
            let v = vertex_handles_doc(&b);
            assert!(!v.is_empty(), "the body has vertices");
            assert!(!b.stipple_dot_positions().is_empty(), "the body has stipple dots");

            let v_at = xf.to_screen(v[0].1[0] as f32, v[0].1[1] as f32);
            let d_at = xf.to_screen(b.stipple_dot_positions()[0].1[0] as f32, b.stipple_dot_positions()[0].1[1] as f32);
            assert_ne!(hit_test_balloon_gizmo(&xf, &b, v_at, false), Some(GizmoHit::VertexHandle { balloon_id: b.id, vertex_index: v[0].0 }));
            assert_ne!(hit_test_balloon_gizmo(&xf, &b, d_at, false), Some(GizmoHit::StippleDot { balloon_id: b.id, dot_index: 0 }));
            assert_eq!(hit_test_balloon_gizmo(&xf, &b, v_at, true), Some(GizmoHit::VertexHandle { balloon_id: b.id, vertex_index: v[0].0 }));
            assert_ne!(hit_test_balloon_gizmo(&xf, &b, d_at, true), Some(GizmoHit::StippleDot { balloon_id: b.id, dot_index: 0 }));

            // Drawing the default gizmo is also free of the extra point overlays.
            let shapes_for = |show: bool| {
                let ctx = egui::Context::default();
                let state = BalloonGizmoState { show_vertex_handles: show, ..Default::default() };
                let mut out = ctx.run_ui(Default::default(), |ui| draw_balloon_gizmo(ui.painter(), &xf, &b, &state));
                let n = out.shapes.len();
                out.textures_delta.clear();
                n
            };
            assert!(shapes_for(true) > shapes_for(false), "the opt-in adds the vertex overlays");

            b.stipple_count = 8;
            assert!(vertex_handles_doc(&b).len() >= 10, "changing the dot count cannot leave dangling vertex overlays");
        }
    }

    #[test]
    fn story_links_render_dashed_segments_between_chained_chambers() {
        use photocraft_doc::{ColorMode, Document, Layer, LayerContent, LayerId, LiveShape, SampleType, ShapeLayer, Size, TextLayer};
        use photocraft_vector::balloon::LiveBalloonGroupGeom;

        use super::{BalloonView, draw};

        let mut doc = Document::new("StoryTest", Size { width: 500, height: 300 }, ColorMode::Rgb, SampleType::U8);
        let t1 = TextLayer { text: "First".into(), size_pt: 14.0, story_threaded: true, next_story_layer: Some(LayerId(2)), ..Default::default() };
        let t2 = TextLayer { text: "Second".into(), size_pt: 14.0, ..Default::default() };
        let mut l1 = Layer::new("T1", LayerContent::Text(t1));
        l1.id = LayerId(1);
        let mut l2 = Layer::new("T2", LayerContent::Text(t2));
        l2.id = LayerId(2);

        let b1 = LiveBalloon {
            id: 1,
            body: LiveBalloonBody::Ellipse { rect: [40.0, 40.0, 120.0, 80.0], squircle: 0.25 },
            linked_text_layer: Some(LayerId(1)),
            ..LiveBalloon::default()
        };
        let b2 = LiveBalloon {
            id: 2,
            body: LiveBalloonBody::Ellipse { rect: [220.0, 40.0, 120.0, 80.0], squircle: 0.25 },
            linked_text_layer: Some(LayerId(2)),
            ..LiveBalloon::default()
        };
        let group = photocraft_doc::vector::LiveBalloonGroup::new(vec![b1, b2]);
        let path = group.rebuild_group_path();
        let sh = ShapeLayer { path, live: Some(LiveShape::BalloonGroup { group }), ..Default::default() };
        let mut bl = Layer::new("B", LayerContent::Shape(sh));
        bl.id = LayerId(3);
        doc.layers.extend([l1, l2, bl]);

        let ctx = egui::Context::default();
        let state = BalloonGizmoState::default();
        let linked_shapes = {
            let view_linked = BalloonView {
                doc_id: None,
                doc: Some(&doc),
                active_layer: Some(LayerId(3)),
                tool_is_balloon: true,
                view_xform: one_to_one(),
                story_links: Vec::new(),
            };
            let mut out_linked = ctx.run_ui(Default::default(), |ui| draw(ui.painter(), &state, &view_linked));
            let n = out_linked.shapes.len();
            out_linked.textures_delta.clear();
            n
        };

        if let Some(LayerContent::Text(tl)) = doc.layer_mut(LayerId(1)).map(|l| &mut l.content) {
            tl.next_story_layer = None;
        }
        let view_unlinked = BalloonView {
            doc_id: None,
            doc: Some(&doc),
            active_layer: Some(LayerId(3)),
            tool_is_balloon: true,
            view_xform: one_to_one(),
            story_links: Vec::new(),
        };
        let mut out_unlinked = ctx.run_ui(Default::default(), |ui| draw(ui.painter(), &state, &view_unlinked));
        let unlinked_shapes = out_unlinked.shapes.len();
        out_unlinked.textures_delta.clear();
        assert!(linked_shapes > unlinked_shapes, "dashed story link must add overlay shapes: {linked_shapes} vs {unlinked_shapes}");
    }
}
