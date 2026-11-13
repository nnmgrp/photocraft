//! On-screen gizmo for the parametric manga balloons (`crates/ui-egui/src/balloon_gizmo.rs`).
//!
//! The gizmo is drawn over the canvas and handles the tail manipulators (`TailTip`, `TailBend`,
//! `TailMouthCenter`, `TailMouthWidth`), the vertex handles of the spikes and the cloud lobes
//! (`VertexHandle`), the radial parameter and the count dial of the special bodies
//! (`RadialParam`, `CountDial`), the corner resize (`BodyResize`), the body move (`BodyMove`)
//! and the spawn point of a new tail on the perimeter (`PerimeterSpawnTail`).
//!
//! Every drag is one gesture: the balloon is edited live while the pointer moves, and the whole
//! gesture collapses into a single history step (the command carries a `coalesce` key).

use egui::{Color32, Pos2, Rect, Stroke, vec2};
use photocraft_doc::vector::{LiveBalloon, LiveBalloonBody, LiveBalloonTail, TailPreset, VertexOverride};
use photocraft_vector::balloon::LiveBalloonGeom;
use serde_json::json;

use crate::PhotocraftApp;
use crate::canvas::ViewXform;
use crate::theme::Tokens;

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
}

/// The gizmo's state between frames.
#[derive(Default)]
pub struct BalloonGizmoState {
    pub active_balloon_id: Option<u32>,
    pub hovered: Option<GizmoHit>,
    pub drag: Option<BalloonGizmoDrag>,
    pub next_gesture_id: u64,
}

impl BalloonGizmoState {
    /// The next gesture id.
    pub fn alloc_gesture_id(&mut self) -> u64 {
        self.next_gesture_id = self.next_gesture_id.wrapping_add(1);
        self.next_gesture_id
    }
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
        LiveBalloonBody::Uniflash { inner_ratio, .. } => Some(([cx + rx * inner_ratio, cy], [cx, cy - ry * 1.08])),
        _ => None,
    }
}

/// The document positions of the draggable vertices of the spikes and the cloud lobes.
fn vertex_handles_doc(balloon: &LiveBalloon) -> Vec<(u32, [f64; 2])> {
    balloon.vertex_handle_positions()
}

/// What the pointer hits on the gizmo of `balloon`.
pub fn hit_test_balloon_gizmo(xf: &ViewXform, balloon: &LiveBalloon, cursor_scr: Pos2) -> Option<GizmoHit> {
    let hit_r = 8.5_f32;
    let bid = balloon.id;

    for tail in &balloon.tails {
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

    // The spikes and the cloud lobes.
    for (v_idx, v_pt) in vertex_handles_doc(balloon) {
        let v_scr = xf.to_screen(v_pt[0] as f32, v_pt[1] as f32);
        if cursor_scr.distance(v_scr) <= hit_r {
            return Some(GizmoHit::VertexHandle { balloon_id: bid, vertex_index: v_idx });
        }
    }

    let r = balloon.body.rect();
    let corners = [[r[0], r[1]], [r[0] + r[2], r[1]], [r[0] + r[2], r[1] + r[3]], [r[0], r[1] + r[3]]];
    for (idx, c) in corners.iter().enumerate() {
        let c_scr = xf.to_screen(c[0] as f32, c[1] as f32);
        if cursor_scr.distance(c_scr) <= hit_r {
            return Some(GizmoHit::BodyResize { balloon_id: bid, corner: idx });
        }
    }

    let cur_doc = xf.to_doc(cursor_scr);
    if !matches!(balloon.body, LiveBalloonBody::Uniflash { .. }) {
        let t = balloon.closest_perimeter_t(cur_doc);
        let (on_perim, _) = balloon.point_and_normal_on_body(t);
        let perim_scr = xf.to_screen(on_perim[0] as f32, on_perim[1] as f32);
        if cursor_scr.distance(perim_scr) <= 7.0 {
            return Some(GizmoHit::PerimeterSpawnTail { balloon_id: bid, t_param: (t * 10000.0).round() as u32 });
        }
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
pub fn draw_balloon_gizmo(painter: &egui::Painter, xf: &ViewXform, balloon: &LiveBalloon, state: &BalloonGizmoState) {
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

    // 4. The tail manipulators.
    for tail in &balloon.tails {
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

    // 6. The draggable spikes and cloud lobes.
    for (v_idx, v_pt) in vertex_handles_doc(balloon) {
        let v_scr = xf.to_screen(v_pt[0] as f32, v_pt[1] as f32);
        let v_hov = state.hovered == Some(GizmoHit::VertexHandle { balloon_id: bid, vertex_index: v_idx });
        let sz = if v_hov { 5.5 } else { 3.5 };
        painter.circle_filled(v_scr, sz, if v_hov { yellow } else { accent });
        painter.circle_stroke(v_scr, sz, Stroke::new(1.0, white));
    }
}

/// Starts a gizmo drag. `Alt` + click on a tail tip removes the tail.
pub fn on_pointer_down(app: &mut PhotocraftApp, layer_id: u64, balloon: &LiveBalloon, hit: GizmoHit, pos_doc: [f64; 2], alt_held: bool) {
    app.balloon_gizmo.active_balloon_id = Some(balloon.id);
    log::info!(
        "[Telemetry:Balloon:GizmoDown] hit={hit:?}, layer_id={layer_id}, pos_doc={pos_doc:?}, \
         body_rect={:?}, tails_count={}",
        balloon.body.rect(),
        balloon.tails.len()
    );
    if alt_held && let GizmoHit::TailTip { balloon_id, tail_id } = hit {
        let _ = app.run("shape.balloon.tail.remove", json!({ "layer": layer_id, "balloonId": balloon_id, "tailId": tail_id }));
        return;
    }

    let gesture_id = app.balloon_gizmo.alloc_gesture_id();

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
        app.balloon_gizmo.drag = Some(BalloonGizmoDrag {
            gesture_id,
            layer_id,
            hit: GizmoHit::TailTip { balloon_id, tail_id: new_tail_id },
            initial_balloon: initial,
            start_pos_doc: pos_doc,
        });
        return;
    }

    app.balloon_gizmo.drag = Some(BalloonGizmoDrag { gesture_id, layer_id, hit, initial_balloon: balloon.clone(), start_pos_doc: pos_doc });
}

/// Applies the drag to the balloon and sends it to the engine. The whole gesture shares one
/// `coalesce` key, so it collapses into a single history step.
pub fn on_drag_move(app: &mut PhotocraftApp, drag: &BalloonGizmoDrag, cur_doc: [f64; 2]) {
    let mut updated = drag.initial_balloon.clone();
    let dx = cur_doc[0] - drag.start_pos_doc[0];
    let dy = cur_doc[1] - drag.start_pos_doc[1];

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
        GizmoHit::VertexHandle { vertex_index, .. } => {
            let base_off = drag.initial_balloon.get_vertex_offset(vertex_index);
            let new_off = [base_off[0] + dx, base_off[1] + dy];
            if let Some(ov) = updated.vertex_overrides.iter_mut().find(|v| v.index == vertex_index) {
                ov.offset = new_off;
            } else {
                updated.vertex_overrides.push(VertexOverride { index: vertex_index, offset: new_off });
            }
        }
        GizmoHit::RadialParam { .. } => {
            let rx = (updated.body.rect()[2] * 0.5).abs().max(10.0);
            match &mut updated.body {
                LiveBalloonBody::Cloud { amplitude, .. } => *amplitude = (*amplitude + dx / rx).clamp(0.04, 0.60),
                LiveBalloonBody::Spiky { length, .. } => *length = (*length + (dx * 2.0) / rx).clamp(0.05, 1.50),
                LiveBalloonBody::Uniflash { inner_ratio, .. } => *inner_ratio = (*inner_ratio + dx / rx).clamp(0.30, 0.90),
                _ => {}
            }
        }
        GizmoHit::CountDial { .. } => {
            let delta = (dx * 0.15).round() as i32;
            match &mut updated.body {
                LiveBalloonBody::Cloud { bumps, .. } => *bumps = ((*bumps as i32 + delta).clamp(4, 64)) as u32,
                LiveBalloonBody::Spiky { spikes, .. } => *spikes = ((*spikes as i32 + delta).clamp(5, 128)) as u32,
                LiveBalloonBody::Uniflash { needles, .. } => *needles = ((*needles as i32 + delta * 2).clamp(32, 400)) as u32,
                _ => {}
            }
        }
        GizmoHit::BodyResize { corner, .. } => {
            let mut r = drag.initial_balloon.body.rect();
            match corner {
                0 => {
                    r[0] += dx;
                    r[1] += dy;
                    r[2] -= dx;
                    r[3] -= dy;
                }
                1 => {
                    r[1] += dy;
                    r[2] += dx;
                    r[3] -= dy;
                }
                2 => {
                    r[2] += dx;
                    r[3] += dy;
                }
                3 => {
                    r[0] += dx;
                    r[2] -= dx;
                    r[3] += dy;
                }
                _ => {}
            }
            let min_size = 24.0;
            if r[2].abs() >= min_size && r[3].abs() >= min_size {
                if r[2] < 0.0 {
                    r[0] += r[2];
                    r[2] = -r[2];
                }
                if r[3] < 0.0 {
                    r[1] += r[3];
                    r[3] = -r[3];
                }
                r[2] = r[2].max(min_size);
                r[3] = r[3].max(min_size);
                updated.body.set_rect(r);
            }
        }
        GizmoHit::BodyMove { .. } => {
            let mut r = drag.initial_balloon.body.rect();
            r[0] += dx;
            r[1] += dy;
            updated.body.set_rect(r);
            for t in &mut updated.tails {
                t.tip[0] += dx;
                t.tip[1] += dy;
            }
        }
        GizmoHit::PerimeterSpawnTail { .. } => {}
    }

    if (dx * dx + dy * dy) > 0.01
        && let Some(tail_id) = match drag.hit {
            GizmoHit::TailTip { tail_id, .. }
            | GizmoHit::TailBend { tail_id, .. }
            | GizmoHit::TailMouthCenter { tail_id, .. }
            | GizmoHit::TailMouthWidth { tail_id, .. } => Some(tail_id),
            _ => None,
        }
        && let Some(t) = updated.tails.iter().find(|t| t.id == tail_id)
    {
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
            drag.hit, cur_doc[0], cur_doc[1], t.tip[0], t.tip[1], mouth_center[0], mouth_center[1]
        );
    }

    let coalesce_key = format!("balloon-gizmo-drag-{}-{}", drag.layer_id, drag.gesture_id);
    if app.run("shape.balloon.update", json!({ "layer": drag.layer_id, "balloon": updated, "coalesce": coalesce_key })).is_ok() {
        crate::comic_balloon_panel::hydrate_geometry_from_balloon(&mut app.ui.balloon_ui, &updated);
    }
}

/// Ends the drag.
pub fn on_pointer_up(state: &mut BalloonGizmoState) {
    state.drag = None;
}

/// The balloons of the active layer, when it is a balloon group.
fn active_balloons(app: &PhotocraftApp) -> Option<(u64, Vec<LiveBalloon>)> {
    let st = app.session.active()?;
    let lid = st.active_layer?;
    let layer = st.doc.layer(lid)?;
    let photocraft_doc::LayerContent::Shape(sh) = &layer.content else { return None };
    let Some(photocraft_doc::LiveShape::BalloonGroup { group }) = &sh.live else { return None };
    Some((lid.0, group.items.clone()))
}

/// The handle under the pointer, if any.
fn hover_at(app: &PhotocraftApp, xf: &ViewXform, doc: [f64; 2]) -> Option<GizmoHit> {
    let (_, balloons) = active_balloons(app)?;
    let cursor = xf.to_screen(doc[0] as f32, doc[1] as f32);
    for b in &balloons {
        if let Some(hit) = hit_test_balloon_gizmo(xf, b, cursor) {
            return Some(hit);
        }
    }
    None
}

/// Takes the pointer events the gizmo needs. True when the gizmo handled the event.
pub fn pointer(app: &mut PhotocraftApp, ev: crate::canvas::ToolEvent, mods: egui::Modifiers) -> bool {
    match ev {
        crate::canvas::ToolEvent::Down { x, y, .. } => {
            let Some(xf) = ViewXform::active(app) else { return false };
            let Some((layer_id, balloons)) = active_balloons(app) else { return false };
            let cursor = xf.to_screen(x as f32, y as f32);
            for b in &balloons {
                if let Some(hit) = hit_test_balloon_gizmo(&xf, b, cursor) {
                    on_pointer_down(app, layer_id, b, hit, [x, y], mods.alt);
                    return true;
                }
            }
            false
        }
        crate::canvas::ToolEvent::Move { x, y, .. } => {
            if let Some(drag) = app.balloon_gizmo.drag.clone() {
                on_drag_move(app, &drag, [x, y]);
                return true;
            }
            // No drag: only remember what the pointer is over, so the handles light up.
            let hovered = ViewXform::active(app).and_then(|xf| hover_at(app, &xf, [x, y]));
            if app.balloon_gizmo.hovered != hovered {
                app.balloon_gizmo.hovered = hovered;
            }
            false
        }
        crate::canvas::ToolEvent::Up { .. } => {
            if app.balloon_gizmo.drag.is_some() {
                on_pointer_up(&mut app.balloon_gizmo);
                true
            } else {
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn balloon() -> LiveBalloon {
        LiveBalloon::default()
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
}
