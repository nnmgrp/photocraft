//! Manga vector tools: PaintTool SAI Linework Curve (`Tool::VectorCurve`) and Comic Balloon (`Tool::Balloon`).

use egui::{Color32, Pos2, Rect, Stroke, pos2, vec2};
use photocraft_doc::LayerContent;
use photocraft_vector::sai_curve::{
    SAI_DEFAULT_TENSION, SaiPoint, alpha_at, closest_point_on_curve, insert_node_on_curve, sample_sai_curve, tapered_rings_ex, width_at,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::canvas::ViewXform;
use crate::state::Tool;
use crate::theme::Tokens;

const MIN_BALLOON_SIZE: f64 = 24.0;
pub const NODE_HIT_RADIUS_PX: f64 = 9.5;
pub const SNAP_CLOSE_RADIUS_PX: f64 = 1.5;
pub const SEGMENT_INSERT_RADIUS_PX: f64 = 12.0;

/// Sub-mode of `Tool::VectorCurve`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VectorCurveMode {
    #[default]
    Draw,
    Weight,
    Edit,
}

/// Active gesture mode while dragging a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeDragKind {
    MovePosition,
    AdjustPressure,
}

/// Dragging a node or stroke on an already-committed Linework layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerDragKind {
    MoveNode,
    AdjustNodePressure,
    ScaleStrokeWidth,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActiveLayerDrag {
    pub layer_id: u64,
    pub stroke_idx: usize,
    pub node_idx: usize,
    pub kind: LayerDragKind,
    pub origin: [f64; 2],
    pub initial_val: f64,
    pub moved: bool,
}

fn default_snap_close() -> bool {
    true
}

/// PaintTool SAI Linework Curve tool state in `egui`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VectorCurveState {
    /// Nodes stored as `[x, y, pressure, corner_flag]`.
    pub points: Vec<[f64; 4]>,
    pub mode: VectorCurveMode,
    pub tension: f64,
    pub auto_taper_ends: bool,
    #[serde(default)]
    pub closed_loop: bool,
    pub selected_node: Option<usize>,
    #[serde(default)]
    pub selected_stroke: Option<usize>,
    #[serde(default)]
    pub selected_stroke_layer: Option<u64>,
    #[serde(skip)]
    pub drag_node: Option<usize>,
    #[serde(skip)]
    pub drag_kind: Option<NodeDragKind>,
    #[serde(skip)]
    pub drag_origin: [f64; 2],
    #[serde(skip)]
    pub drag_initial_pressure: f64,
    #[serde(skip)]
    pub drag_moved: bool,
    #[serde(skip)]
    pub layer_drag: Option<ActiveLayerDrag>,
    #[serde(skip)]
    pub hover_pos: Option<[f64; 2]>,
    #[serde(skip)]
    pub target_layer_id: Option<u64>,
    #[serde(default = "default_snap_close")]
    pub snap_close: bool,
}

impl Default for VectorCurveState {
    fn default() -> Self {
        Self {
            points: Vec::new(),
            mode: VectorCurveMode::Draw,
            tension: SAI_DEFAULT_TENSION,
            auto_taper_ends: true,
            closed_loop: false,
            selected_node: None,
            selected_stroke: None,
            selected_stroke_layer: None,
            drag_node: None,
            drag_kind: None,
            drag_origin: [0.0, 0.0],
            drag_initial_pressure: 1.0,
            drag_moved: false,
            layer_drag: None,
            hover_pos: None,
            target_layer_id: None,
            snap_close: true,
        }
    }
}

impl VectorCurveState {
    pub fn node_at(&self, pos: [f64; 2], hit_radius_doc: f64) -> Option<usize> {
        let r2 = hit_radius_doc * hit_radius_doc;
        self.points.iter().enumerate().rev().find_map(|(i, pt)| {
            let dx = pt[0] - pos[0];
            let dy = pt[1] - pos[1];
            (dx * dx + dy * dy <= r2).then_some(i)
        })
    }

    pub fn try_insert_node_on_segment(&mut self, x: f64, y: f64, max_dist_doc: f64, closed: bool) -> Option<usize> {
        let mut sai_pts: Vec<SaiPoint> = self.points.iter().map(|p| SaiPoint::with_pressure((p[0], p[1]), p[2] as f32, p[3] > 0.5)).collect();
        let inserted_idx = insert_node_on_curve(&mut sai_pts, (x, y), closed || self.closed_loop, self.tension, max_dist_doc)?;
        self.points = sai_pts.into_iter().map(|pt| [pt.p.0, pt.p.1, f64::from(pt.pressure), if pt.corner { 1.0 } else { 0.0 }]).collect();
        self.selected_node = Some(inserted_idx);
        Some(inserted_idx)
    }

    pub fn restore_closed_loop_pressures(&mut self) {
        if self.points.len() >= 2 {
            if (self.points[0][2] - 0.08).abs() < 0.05 {
                self.points[0][2] = 1.0;
            }
            let last_idx = self.points.len() - 1;
            if (self.points[last_idx][2] - 0.08).abs() < 0.05 {
                self.points[last_idx][2] = 1.0;
            }
        }
    }

    pub fn finish_at_cursor(&mut self, zoom: f64) -> bool {
        if self.points.len() < 2 {
            return false;
        }
        let snap_close_radius = SNAP_CLOSE_RADIUS_PX / zoom.max(0.05);
        if self.mode == VectorCurveMode::Draw
            && self.snap_close
            && self.points.len() >= 3
            && let Some(h) = self.hover_pos
            && (self.points[0][0] - h[0]).hypot(self.points[0][1] - h[1]) <= snap_close_radius
        {
            self.closed_loop = true;
            self.restore_closed_loop_pressures();
        }
        self.points.len() >= 2
    }

    pub fn on_primary_down(&mut self, pos: [f64; 2], mods: egui::Modifiers, double_clicked: bool, closed: bool, zoom: f64) -> bool {
        let [x, y] = pos;
        let hit_radius = NODE_HIT_RADIUS_PX / zoom.max(0.05);
        let snap_close_radius = SNAP_CLOSE_RADIUS_PX / zoom.max(0.05);
        let seg_radius = SEGMENT_INSERT_RADIUS_PX / zoom.max(0.05);
        self.hover_pos = Some(pos);

        if double_clicked && self.points.len() >= 2 {
            let last = self.points[self.points.len() - 1];
            if (last[0] - x).hypot(last[1] - y) <= hit_radius * 2.0 {
                if self.points.len() >= 3 {
                    let prev = self.points[self.points.len() - 2];
                    if (last[0] - prev[0]).hypot(last[1] - prev[1]) <= hit_radius * 1.2 {
                        self.points.pop();
                        if self.auto_taper_ends && !self.closed_loop && !closed && self.points.len() >= 2 {
                            let new_last = self.points.len() - 1;
                            self.points[new_last][2] = 0.08;
                        }
                    }
                }
                return true;
            }
        }

        if self.snap_close
            && self.mode == VectorCurveMode::Draw
            && !mods.ctrl
            && !mods.shift
            && !mods.alt
            && self.points.len() >= 3
            && (self.points[0][0] - x).hypot(self.points[0][1] - y) <= snap_close_radius
        {
            self.closed_loop = true;
            self.restore_closed_loop_pressures();
            return true;
        }

        if let Some(idx) = self.node_at(pos, hit_radius) {
            if mods.ctrl && mods.alt && self.points.len() > 2 {
                self.points.remove(idx);
                self.selected_node = Some(idx.min(self.points.len() - 1));
                return false;
            }
            self.selected_node = Some(idx);
            self.drag_node = Some(idx);
            self.drag_origin = pos;
            self.drag_initial_pressure = self.points[idx][2];
            self.drag_moved = false;
            self.drag_kind =
                if self.mode == VectorCurveMode::Weight || mods.alt { Some(NodeDragKind::AdjustPressure) } else { Some(NodeDragKind::MovePosition) };
            return false;
        }

        if (mods.shift || self.mode == VectorCurveMode::Edit)
            && self.points.len() >= 2
            && let Some(inserted_idx) = self.try_insert_node_on_segment(x, y, seg_radius, closed)
        {
            self.drag_node = Some(inserted_idx);
            self.drag_origin = pos;
            self.drag_initial_pressure = self.points[inserted_idx][2];
            self.drag_moved = false;
            self.drag_kind = Some(NodeDragKind::MovePosition);
            return false;
        }

        if self.mode != VectorCurveMode::Draw || mods.shift {
            self.selected_node = None;
            return false;
        }

        let initial_pressure = if self.auto_taper_ends && !closed { 0.08 } else { 1.0 };
        self.points.push([x, y, initial_pressure, 0.0]);
        self.apply_auto_taper_on_append();
        self.selected_node = Some(self.points.len() - 1);
        false
    }

    fn apply_auto_taper_on_append(&mut self) {
        if !self.auto_taper_ends {
            return;
        }
        let n = self.points.len();
        if n >= 3 {
            let prev_last = n - 2;
            if (self.points[prev_last][2] - 0.08).abs() < 0.05 {
                self.points[prev_last][2] = 1.0;
            }
        }
    }

    pub fn on_pointer_move(&mut self, x: f64, y: f64) {
        self.hover_pos = Some([x, y]);
        let (Some(idx), Some(kind)) = (self.drag_node, self.drag_kind) else { return };
        if (x - self.drag_origin[0]).hypot(y - self.drag_origin[1]) > 2.0 {
            self.drag_moved = true;
        }
        let Some(node) = self.points.get_mut(idx) else {
            self.drag_node = None;
            return;
        };
        match kind {
            NodeDragKind::MovePosition => {
                node[0] = x;
                node[1] = y;
            }
            NodeDragKind::AdjustPressure => {
                let delta = ((self.drag_origin[1] - y) + (x - self.drag_origin[0]) * 0.5) / 120.0;
                node[2] = (self.drag_initial_pressure + delta).clamp(0.0, 2.0);
            }
        }
    }

    pub fn on_primary_up(&mut self, alt_held: bool) {
        if let Some(idx) = self.drag_node.take()
            && alt_held
            && !self.drag_moved
            && self.mode != VectorCurveMode::Weight
            && let Some(node) = self.points.get_mut(idx)
        {
            node[3] = if node[3] > 0.5 { 0.0 } else { 1.0 };
        }
        self.drag_kind = None;
        self.drag_moved = false;
        self.layer_drag = None;
    }

    pub fn clear(&mut self) {
        self.points.clear();
        self.closed_loop = false;
        self.selected_node = None;
        self.drag_node = None;
        self.drag_kind = None;
        self.drag_moved = false;
        self.layer_drag = None;
        self.hover_pos = None;
        self.target_layer_id = None;
    }

    pub fn pop_last_point(&mut self) -> bool {
        if self.points.is_empty() {
            return false;
        }
        self.points.pop();
        if self.points.is_empty() {
            self.clear();
            return true;
        }
        if self.selected_node.is_some_and(|idx| idx >= self.points.len()) {
            self.selected_node = Some(self.points.len() - 1);
        }
        true
    }

    pub fn delete_active_node(&mut self) -> bool {
        if self.points.is_empty() {
            return false;
        }
        let target = self.selected_node.unwrap_or_else(|| self.points.len().saturating_sub(1));
        if target < self.points.len() {
            self.points.remove(target);
            self.selected_node = if self.points.is_empty() { None } else { Some(target.min(self.points.len() - 1)) };
            if self.points.is_empty() {
                self.clear();
            }
            return true;
        }
        false
    }

    pub fn to_command_params(&self, base_width: f64, min_ratio: f64, tapered: bool, closed: bool) -> std::result::Result<Value, String> {
        if self.points.len() < 2 {
            return Err("Vector Curve: at least two nodes are needed to commit a stroke".into());
        }
        let effective_closed = closed || self.closed_loop;
        let pts: Vec<Value> = self
            .points
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let mut pressure = p[2];
                if effective_closed && (i == 0 || i + 1 == self.points.len()) && (pressure - 0.08).abs() < 0.05 {
                    pressure = 1.0;
                }
                json!({ "x": p[0], "y": p[1], "pressure": pressure, "corner": p[3] > 0.5 })
            })
            .collect();
        Ok(json!({
            "points": pts,
            "width": base_width.clamp(0.1, 100_000.0),
            "minRatio": min_ratio.clamp(0.0, 1.0),
            "tapered": tapered,
            "closed": effective_closed,
            "tension": self.tension.clamp(0.0, 1.0),
        }))
    }
}

/// The Balloon tool: tails unioned with a balloon body and procedural cloud / spiky balloons.
pub fn is_balloon_tool(t: Tool) -> bool {
    t == Tool::Balloon
}

fn balloon_rect(start: [f64; 2], end: [f64; 2]) -> Option<[f64; 4]> {
    let (x0, y0) = (start[0].min(end[0]), start[1].min(end[1]));
    let (w, h) = ((start[0] - end[0]).abs(), (start[1] - end[1]).abs());
    (w >= 16.0 || h >= 16.0).then_some([x0, y0, w.max(MIN_BALLOON_SIZE), h.max(MIN_BALLOON_SIZE)])
}

fn click_near_active_balloon(app: &PhotocraftApp, click_doc: [f64; 2]) -> bool {
    let Some(active_bid) = crate::manga_seam::active_balloon_id(app) else { return false };
    let Some(st) = app.session.active() else { return false };
    let Some(lid) = st.active_layer else { return false };
    let Some(layer) = st.doc.layer(lid) else { return false };
    let LayerContent::Shape(sh) = &layer.content else { return false };
    let Some(photocraft_doc::LiveShape::BalloonGroup { group }) = &sh.live else { return false };
    let Some(b) = group.items.iter().find(|b| b.id == active_bid) else { return false };
    let r = b.body.rect();
    let pad = 36.0;
    click_doc[0] >= r[0] - pad && click_doc[0] <= r[0] + r[2] + pad && click_doc[1] >= r[1] - pad && click_doc[1] <= r[1] + r[3] + pad
}

fn balloon_body_at(app: &PhotocraftApp, p: [f64; 2]) -> Option<u64> {
    let st = app.session.active()?;
    if let Some(id) = st.active_layer
        && st.doc.layer(id).is_some_and(|l| matches!(l.content, LayerContent::Shape(_)))
    {
        return Some(id.0);
    }
    let (x, y) = (p[0].floor(), p[1].floor());
    for layer in st.doc.layers.iter().rev() {
        let LayerContent::Shape(shape) = &layer.content else { continue };
        let Some(cache) = shape.cache.as_ref() else { continue };
        let r = cache.content_bounds();
        if f64::from(r.x0) <= x && x < f64::from(r.x1) && f64::from(r.y0) <= y && y < f64::from(r.y1) {
            return Some(layer.id.0);
        }
    }
    None
}

/// Finish a Balloon-tool click or drag: creates a live `BalloonGroup` or unions a legacy tail.
pub fn finish_balloon(app: &mut PhotocraftApp, start: [f64; 2], end: [f64; 2]) {
    let o = app.ui.tool_options.clone();
    let w = (start[0] - end[0]).abs();
    let h = (start[1] - end[1]).abs();
    if w < 16.0 && h < 16.0 {
        if !click_near_active_balloon(app, start) {
            crate::manga_seam::create_live_balloon_in_rect(app, [start[0] - 120.0, start[1] - 80.0, 240.0, 160.0]);
        }
        return;
    }
    if o.balloon_mode == "tail"
        && let Some(layer) = balloon_body_at(app, start)
    {
        let length = (end[0] - start[0]).hypot(end[1] - start[1]);
        let curve = if o.balloon_preset == "arc" { 0.45 } else { 0.15 };
        let mut params = json!({
            "layer": layer,
            "preset": o.balloon_preset,
            "from": start,
            "tip": end,
            "mouthWidth": o.balloon_mouth,
            "curve": curve,
        });
        if o.balloon_preset == "thought" {
            params["bubbles"] = json!(3);
            params["bubbleRadius"] = json!((length * 0.12).clamp(3.0, 18.0));
        }
        if let Err(e) = app.run("shape.balloon.tail", params) {
            app.ui.status = e;
            app.ui.status_error = true;
        }
        return;
    }
    if let Some(rect) = balloon_rect(start, end) {
        crate::manga_seam::create_live_balloon_in_rect(app, rect);
    }
}

/// Balloon-tool drag preview: a dashed tail axis for tails on a shape, or the balloon box.
pub fn draw_balloon_preview(app: &PhotocraftApp, painter: &egui::Painter, xf: &ViewXform, start: [f64; 2], end: [f64; 2]) {
    let accent = Tokens::get(painter.ctx()).accent;
    let stroke = Stroke::new(1.5, accent);
    let a = xf.to_screen(start[0] as f32, start[1] as f32);
    let b = xf.to_screen(end[0] as f32, end[1] as f32);
    if app.ui.tool_options.balloon_mode == "tail" && balloon_body_at(app, start).is_some() {
        let steps = 12;
        for i in 0..steps {
            let t0 = i as f32 / steps as f32;
            let t1 = (i as f32 + 0.6) / steps as f32;
            painter.line_segment([a + (b - a) * t0, a + (b - a) * t1], stroke);
        }
        let dir = (b - a).normalized();
        let side = vec2(-dir.y, dir.x) * (app.ui.tool_options.balloon_mouth * 0.5 * xf.zoom);
        painter.line_segment([a - side, a + side], stroke);
    } else {
        painter.rect_stroke(Rect::from_two_pos(a, b), 2.0, stroke, egui::StrokeKind::Inside);
    }
}

fn active_linework(app: &PhotocraftApp) -> Option<(u64, photocraft_vector::sai_curve::LiveLinework)> {
    let st = app.session.active()?;
    let id = st.active_layer?;
    let lw = photocraft_engine::manga_cmds::vector_curve::get_live_linework(st.doc.layer(id)?)?;
    Some((id.0, lw))
}

fn try_committed_linework_down(app: &mut PhotocraftApp, pos: [f64; 2], mods: egui::Modifiers, zoom: f64, cur_mode: VectorCurveMode) -> bool {
    let Some((active_id, lw)) = active_linework(app) else { return false };
    let [x, y] = pos;
    let hit_radius = NODE_HIT_RADIUS_PX / zoom.max(0.05);
    let seg_radius = SEGMENT_INSERT_RADIUS_PX / zoom.max(0.05);

    if let Some((s_idx, n_idx)) = lw.node_at((x, y), hit_radius) {
        if mods.ctrl && mods.alt && lw.strokes[s_idx].nodes.len() > 2 {
            let _ = app.run("shape.vectorCurve.updateNode", json!({ "layer": active_id, "strokeIndex": s_idx, "index": n_idx, "deleteNode": true }));
            return true;
        }
        let initial_val = f64::from(lw.strokes[s_idx].nodes[n_idx].pressure);
        let kind = if cur_mode == VectorCurveMode::Weight || mods.alt { LayerDragKind::AdjustNodePressure } else { LayerDragKind::MoveNode };
        let vc = app.ui.vector_curve.get_or_insert_with(VectorCurveState::default);
        vc.selected_stroke = Some(s_idx);
        vc.selected_stroke_layer = Some(active_id);
        vc.layer_drag = Some(ActiveLayerDrag { layer_id: active_id, stroke_idx: s_idx, node_idx: n_idx, kind, origin: pos, initial_val, moved: false });
        return true;
    }

    if cur_mode == VectorCurveMode::Weight
        && let Some((s_idx, _)) = lw.closest_segment((x, y), seg_radius)
    {
        let initial_val = lw.strokes[s_idx].base_width;
        let vc = app.ui.vector_curve.get_or_insert_with(VectorCurveState::default);
        vc.selected_stroke = Some(s_idx);
        vc.selected_stroke_layer = Some(active_id);
        vc.layer_drag = Some(ActiveLayerDrag {
            layer_id: active_id,
            stroke_idx: s_idx,
            node_idx: 0,
            kind: LayerDragKind::ScaleStrokeWidth,
            origin: pos,
            initial_val,
            moved: false,
        });
        return true;
    }

    if (mods.shift || cur_mode == VectorCurveMode::Edit)
        && let Some((s_idx, _)) = lw.closest_segment((x, y), seg_radius)
        && let Ok(resp) = app.run("shape.vectorCurve.insertNode", json!({ "layer": active_id, "strokeIndex": s_idx, "x": x, "y": y, "maxDist": seg_radius }))
        && let Some(ins_idx) = resp.get("insertedIndex").and_then(Value::as_u64)
    {
        let vc = app.ui.vector_curve.get_or_insert_with(VectorCurveState::default);
        vc.selected_stroke = Some(s_idx);
        vc.selected_stroke_layer = Some(active_id);
        vc.layer_drag = Some(ActiveLayerDrag {
            layer_id: active_id,
            stroke_idx: s_idx,
            node_idx: ins_idx as usize,
            kind: LayerDragKind::MoveNode,
            origin: pos,
            initial_val: 1.0,
            moved: false,
        });
        return true;
    }
    false
}

pub fn vector_curve_down(app: &mut PhotocraftApp, pos: [f64; 2], mods: egui::Modifiers, double_clicked: bool, zoom: f64) {
    let closed = app.ui.tool_options.vector_curve_closed;
    let cur_mode = app.ui.vector_curve.as_ref().map_or(VectorCurveMode::Draw, |vc| vc.mode);
    let points_empty = app.ui.vector_curve.as_ref().is_none_or(|vc| vc.points.is_empty());

    if points_empty && (mods.shift || mods.alt || cur_mode != VectorCurveMode::Draw) && try_committed_linework_down(app, pos, mods, zoom, cur_mode) {
        return;
    }

    let active_layer_id = app.session.active().and_then(|st| st.active_layer).map(|id| id.0);
    let panel_snap_close = crate::manga_seam::sai_snap_close(app);
    let commit_now = {
        let vc = app.ui.vector_curve.get_or_insert_with(VectorCurveState::default);
        let was_empty = vc.points.is_empty();
        if was_empty {
            vc.snap_close = panel_snap_close;
        }
        let done = vc.on_primary_down(pos, mods, double_clicked, closed, zoom);
        if was_empty && !vc.points.is_empty() {
            vc.target_layer_id = active_layer_id;
            vc.selected_stroke = None;
            vc.selected_stroke_layer = None;
        }
        done
    };
    if commit_now {
        vector_curve_commit(app);
    }
}

pub fn reconcile_vector_curve_layer(app: &mut PhotocraftApp) {
    let Some(vc) = app.ui.vector_curve.as_mut() else { return };
    if vc.points.is_empty() {
        vc.target_layer_id = None;
        return;
    }
    let Some(target_id) = vc.target_layer_id else { return };
    let still_valid = app
        .session
        .active()
        .is_some_and(|st| st.active_layer.is_some_and(|id| id.0 == target_id) && st.doc.layer(photocraft_doc::LayerId(target_id)).is_some_and(|l| l.visible));
    if !still_valid {
        vc.clear();
    }
}

pub fn picked_stroke_width(vc: Option<&VectorCurveState>, session: &photocraft_engine::Session) -> Option<(u64, usize, f32)> {
    let vc = vc?;
    let idx = vc.selected_stroke?;
    let st = session.active()?;
    let layer_id = st.active_layer?;
    if vc.selected_stroke_layer != Some(layer_id.0) {
        return None;
    }
    let lw = photocraft_engine::manga_cmds::vector_curve::get_live_linework(st.doc.layer(layer_id)?)?;
    Some((layer_id.0, idx, lw.strokes.get(idx)?.base_width as f32))
}

pub fn selected_stroke_width(app: &PhotocraftApp) -> Option<(u64, usize, f32)> {
    picked_stroke_width(app.ui.vector_curve.as_ref(), &app.session)
}

pub fn apply_stroke_width(app: &mut PhotocraftApp, width: f32) {
    let Some((layer_id, idx, current)) = selected_stroke_width(app) else { return };
    if !width.is_finite() || (width - current).abs() <= 1e-4 {
        return;
    }
    let _ = app.run(
        "shape.vectorCurve.updateNode",
        json!({ "layer": layer_id, "strokeIndex": idx, "baseWidth": width, "coalesce": format!("sai-curve-width-{layer_id}-{idx}") }),
    );
}

pub fn vector_curve_move(app: &mut PhotocraftApp, x: f64, y: f64) {
    let layer_cmd = {
        let vc = app.ui.vector_curve.get_or_insert_with(VectorCurveState::default);
        vc.on_pointer_move(x, y);
        vc.layer_drag.as_mut().map(|ld| {
            if (x - ld.origin[0]).hypot(y - ld.origin[1]) > 1.5 {
                ld.moved = true;
            }
            let coalesce = format!("sai-curve-drag-{}-{}", ld.layer_id, ld.stroke_idx);
            match ld.kind {
                LayerDragKind::MoveNode => {
                    json!({ "layer": ld.layer_id, "strokeIndex": ld.stroke_idx, "index": ld.node_idx, "pos": [x, y], "coalesce": coalesce })
                }
                LayerDragKind::AdjustNodePressure => {
                    let delta = ((ld.origin[1] - y) + (x - ld.origin[0]) * 0.5) / 120.0;
                    let p = (ld.initial_val + delta).clamp(0.0, 2.0);
                    json!({ "layer": ld.layer_id, "strokeIndex": ld.stroke_idx, "index": ld.node_idx, "pressure": p, "coalesce": coalesce })
                }
                LayerDragKind::ScaleStrokeWidth => {
                    let delta = ((ld.origin[1] - y) + (x - ld.origin[0]) * 0.5) / 15.0;
                    let bw = (ld.initial_val + delta).clamp(0.5, 500.0);
                    json!({ "layer": ld.layer_id, "strokeIndex": ld.stroke_idx, "baseWidth": bw, "coalesce": coalesce })
                }
            }
        })
    };
    if let Some(params) = layer_cmd {
        let _ = app.run("shape.vectorCurve.updateNode", params);
    }
}

pub fn vector_curve_hover(app: &mut PhotocraftApp, x: f64, y: f64) {
    app.ui.vector_curve.get_or_insert_with(VectorCurveState::default).hover_pos = Some([x, y]);
}

pub fn vector_curve_up(app: &mut PhotocraftApp, alt: bool) {
    let toggle_cmd = {
        let Some(vc) = app.ui.vector_curve.as_mut() else { return };
        let cmd = if let Some(ld) = vc.layer_drag.take()
            && alt
            && !ld.moved
            && vc.mode != VectorCurveMode::Weight
        {
            Some(json!({ "layer": ld.layer_id, "strokeIndex": ld.stroke_idx, "index": ld.node_idx, "toggleCorner": true }))
        } else {
            None
        };
        vc.on_primary_up(alt);
        cmd
    };
    if let Some(params) = toggle_cmd {
        let _ = app.run("shape.vectorCurve.updateNode", params);
    }
}

pub fn vector_curve_escape(app: &mut PhotocraftApp) {
    let zoom = f64::from(app.point_zoom());
    let ready = app.ui.vector_curve.as_mut().is_some_and(|vc| vc.finish_at_cursor(zoom));
    if ready {
        vector_curve_commit(app);
    } else if let Some(vc) = app.ui.vector_curve.as_mut() {
        vc.clear();
    }
}

pub fn vector_curve_commit(app: &mut PhotocraftApp) {
    let Some(mut vc) = app.ui.vector_curve.take() else { return };
    if vc.points.len() < 2 {
        vc.clear();
        app.ui.vector_curve = Some(vc);
        return;
    }
    let o = &app.ui.tool_options;
    let saved = (vc.mode, vc.tension, vc.auto_taper_ends, vc.snap_close);
    let sai = crate::manga_seam::sai_curve_params(app, vc.tension, o.vector_curve_min_ratio);

    match vc.to_command_params(f64::from(o.vector_curve_width), sai.min_ratio, o.vector_curve_tapered, o.vector_curve_closed) {
        Ok(mut params) => {
            params["appendToActive"] = json!(true);
            params["curveType"] = json!(sai.curve_type_str);
            params["baseDensity"] = json!(sai.base_density);
            params["minDensityRatio"] = json!(sai.min_density_ratio);
            if sai.is_density {
                params["pressureTarget"] = json!("density");
            }
            if let Err(e) = app.run("shape.vectorCurve.create", params) {
                app.ui.status = e;
                app.ui.status_error = true;
                app.ui.vector_curve = Some(vc);
            } else {
                app.ui.vector_curve =
                    Some(VectorCurveState { mode: saved.0, tension: saved.1, auto_taper_ends: saved.2, snap_close: saved.3, ..VectorCurveState::default() });
            }
        }
        Err(err) => {
            app.ui.status = err;
            app.ui.status_error = true;
            app.ui.vector_curve = Some(vc);
        }
    }
}

fn draw_committed_linework(app: &PhotocraftApp, painter: &egui::Painter, to_scr: &dyn Fn([f64; 2]) -> Pos2, zoom: f64, mods: egui::Modifiers, accent: Color32) {
    let Some((_, lw)) = active_linework(app) else { return };
    let cur_mode = app.ui.vector_curve.as_ref().map_or(VectorCurveMode::Draw, |vc| vc.mode);
    let hover_pos = app.ui.vector_curve.as_ref().and_then(|vc| vc.hover_pos);
    let hit_radius = NODE_HIT_RADIUS_PX / zoom;
    let seg_radius = SEGMENT_INSERT_RADIUS_PX / zoom;
    for stroke in &lw.strokes {
        let samples = sample_sai_curve(&stroke.nodes, stroke.base_width, stroke.min_ratio, stroke.closed, stroke.effective_tension(), 2.5);
        if samples.len() >= 2 {
            let mut line_pts: Vec<Pos2> = samples.iter().map(|s| to_scr([s.p.0, s.p.1])).collect();
            if stroke.closed
                && let Some(&first) = line_pts.first()
            {
                line_pts.push(first);
            }
            painter.add(egui::Shape::line(line_pts, Stroke::new(1.25, accent.gamma_multiply(0.85))));
        }
        for pt in &stroke.nodes {
            let c = to_scr([pt.p.0, pt.p.1]);
            let r_scr = (width_at(stroke.base_width, stroke.min_ratio, pt.pressure) * 0.5 * zoom).max(3.0) as f32;
            if cur_mode == VectorCurveMode::Weight || mods.alt {
                painter.circle_stroke(c, r_scr, Stroke::new(1.0, accent.gamma_multiply(0.65)));
            }
            let rect = Rect::from_center_size(c, vec2(6.4, 6.4));
            painter.rect_filled(rect, 0.0, Color32::WHITE);
            painter.rect_stroke(rect, 0.0, Stroke::new(1.0, accent), egui::StrokeKind::Outside);
        }
    }
    if (mods.shift || cur_mode == VectorCurveMode::Edit)
        && let Some(h) = hover_pos
        && lw.node_at((h[0], h[1]), hit_radius).is_none()
        && let Some((_, proj)) = lw.closest_segment((h[0], h[1]), seg_radius)
    {
        let p_scr = to_scr([proj.projected_point.0, proj.projected_point.1]);
        painter.circle_filled(p_scr, 4.5, accent);
        painter.circle_stroke(p_scr, 4.5, Stroke::new(1.0, Color32::WHITE));
    }
}

fn draw_active_stroke_ribbon(
    app: &PhotocraftApp,
    painter: &egui::Painter,
    vc: &VectorCurveState,
    to_scr: &dyn Fn([f64; 2]) -> Pos2,
    zoom: f64,
    accent: Color32,
) -> bool {
    let o = &app.ui.tool_options;
    let mut preview_pts: Vec<SaiPoint> = vc.points.iter().map(|p| SaiPoint::with_pressure((p[0], p[1]), p[2] as f32, p[3] > 0.5)).collect();
    let mut preview_closed = o.vector_curve_closed || vc.closed_loop;
    let mut snapping_to_start = false;

    if vc.mode == VectorCurveMode::Draw
        && vc.drag_node.is_none()
        && let Some(h) = vc.hover_pos
    {
        let snap_close_radius = SNAP_CLOSE_RADIUS_PX / zoom;
        if vc.snap_close && !preview_closed && vc.points.len() >= 3 && (vc.points[0][0] - h[0]).hypot(vc.points[0][1] - h[1]) <= snap_close_radius {
            preview_closed = true;
            snapping_to_start = true;
        } else if !preview_closed {
            let last = vc.points[vc.points.len() - 1];
            if (last[0] - h[0]).hypot(last[1] - h[1]) > 1.5 / zoom {
                if vc.auto_taper_ends && preview_pts.len() >= 2 {
                    let prev_last = preview_pts.len() - 1;
                    if (preview_pts[prev_last].pressure - 0.08).abs() < 0.05 {
                        preview_pts[prev_last].pressure = 1.0;
                    }
                }
                preview_pts.push(SaiPoint::with_pressure((h[0], h[1]), if vc.auto_taper_ends { 0.08 } else { 1.0 }, false));
            }
        }
    }

    if vc.points.len() == 1 {
        if vc.mode == VectorCurveMode::Draw
            && let Some(h) = vc.hover_pos
        {
            painter.add(egui::Shape::line(vec![to_scr([vc.points[0][0], vc.points[0][1]]), to_scr(h)], Stroke::new(1.0, accent.gamma_multiply(0.6))));
        }
    } else if preview_pts.len() >= 2 {
        let sai = crate::manga_seam::sai_curve_params(app, vc.tension, o.vector_curve_min_ratio);
        let min_r = if o.vector_curve_tapered || sai.is_density { sai.min_ratio } else { 1.0 };
        let fg = app.session.tools.foreground;
        let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let stroke_col = Color32::from_rgb(b(fg[0]), b(fg[1]), b(fg[2])).gamma_multiply(0.85);
        let (left, right, samples) = tapered_rings_ex(&preview_pts, f64::from(o.vector_curve_width), min_r, preview_closed, sai.effective_tension, 2.0);
        let count = left.len().min(right.len());
        if count >= 2 {
            let segs = if preview_closed { count } else { count - 1 };
            for i in 0..segs {
                let next = (i + 1) % count;
                let density_alpha = if sai.is_density {
                    let p_avg = samples.get(i).zip(samples.get(next)).map_or(1.0, |(a, b)| (a.pressure + b.pressure) * 0.5);
                    alpha_at(sai.base_density, sai.min_density_ratio, p_avg)
                } else {
                    sai.base_density.clamp(0.0, 1.0)
                };
                let quad = vec![
                    to_scr([left[i].0, left[i].1]),
                    to_scr([left[next].0, left[next].1]),
                    to_scr([right[next].0, right[next].1]),
                    to_scr([right[i].0, right[i].1]),
                ];
                painter.add(egui::Shape::convex_polygon(quad, stroke_col.gamma_multiply(density_alpha), Stroke::NONE));
            }
        }
        if samples.len() >= 2 {
            let mut scr_line: Vec<Pos2> = samples.iter().map(|s| to_scr([s.p.0, s.p.1])).collect();
            if preview_closed && let Some(&first) = scr_line.first() {
                scr_line.push(first);
            }
            painter.add(egui::Shape::line(scr_line, Stroke::new(1.5, accent)));
        }
    }
    snapping_to_start
}

pub fn draw_vector_curve_overlay(app: &PhotocraftApp, painter: &egui::Painter, xf: &ViewXform) {
    if app.ui.tool != Tool::VectorCurve {
        return;
    }
    let to_scr = |q: [f64; 2]| xf.to_screen(q[0] as f32, q[1] as f32);
    let accent = Tokens::get(painter.ctx()).accent;
    let mods = painter.ctx().input(|i| i.modifiers);
    let cur_mode = app.ui.vector_curve.as_ref().map_or(VectorCurveMode::Draw, |vc| vc.mode);
    let zoom = f64::from(xf.zoom).max(0.05);
    let hit_radius = NODE_HIT_RADIUS_PX / zoom;
    let seg_radius = SEGMENT_INSERT_RADIUS_PX / zoom;

    let drawing_new = app.ui.vector_curve.as_ref().is_some_and(|vc| !vc.points.is_empty());
    if !drawing_new && (mods.shift || mods.alt || cur_mode != VectorCurveMode::Draw) {
        draw_committed_linework(app, painter, &to_scr, zoom, mods, accent);
    }

    let Some(vc) = app.ui.vector_curve.as_ref() else { return };
    if vc.points.is_empty() {
        return;
    }
    if let Some(target_id) = vc.target_layer_id
        && !app.session.active().is_some_and(|st| {
            st.active_layer.is_some_and(|id| id.0 == target_id) && st.doc.layer(photocraft_doc::LayerId(target_id)).is_some_and(|l| l.visible)
        })
    {
        return;
    }

    let snapping_to_start = draw_active_stroke_ribbon(app, painter, vc, &to_scr, zoom, accent);
    let o = &app.ui.tool_options;
    let preview_closed = o.vector_curve_closed || vc.closed_loop;
    if (mods.shift || vc.mode == VectorCurveMode::Edit)
        && vc.points.len() >= 2
        && let Some(h) = vc.hover_pos
        && vc.node_at(h, hit_radius).is_none()
    {
        let sai_pts: Vec<SaiPoint> = vc.points.iter().map(|p| SaiPoint::with_pressure((p[0], p[1]), p[2] as f32, p[3] > 0.5)).collect();
        if let Some(proj) = closest_point_on_curve(&sai_pts, (h[0], h[1]), preview_closed, vc.tension)
            && proj.distance <= seg_radius
        {
            let p_scr = to_scr([proj.projected_point.0, proj.projected_point.1]);
            painter.circle_filled(p_scr, 4.5, accent);
            painter.circle_stroke(p_scr, 4.5, Stroke::new(1.0, Color32::WHITE));
        }
    }
    if snapping_to_start {
        painter.circle_stroke(to_scr([vc.points[0][0], vc.points[0][1]]), 8.0, Stroke::new(2.0, accent));
    }
    for (i, pt) in vc.points.iter().enumerate() {
        let c = to_scr([pt[0], pt[1]]);
        let is_selected = vc.selected_node == Some(i);
        let r_scr = (width_at(f64::from(o.vector_curve_width), f64::from(o.vector_curve_min_ratio), pt[2] as f32) * 0.5 * zoom).max(3.0) as f32;
        if is_selected || vc.drag_node == Some(i) || vc.mode == VectorCurveMode::Weight {
            painter.circle_stroke(c, r_scr, Stroke::new(1.0, accent.gamma_multiply(0.6)));
        }
        let fill = if is_selected { accent } else { Color32::WHITE };
        if pt[3] > 0.5 {
            let d = 4.5;
            let diamond = vec![pos2(c.x, c.y - d), pos2(c.x + d, c.y), pos2(c.x, c.y + d), pos2(c.x - d, c.y)];
            painter.add(egui::Shape::convex_polygon(diamond, fill, Stroke::new(1.0, Color32::BLACK)));
        } else {
            let rect = Rect::from_center_size(c, vec2(7.0, 7.0));
            painter.rect_filled(rect, 0.0, fill);
            painter.rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::BLACK), egui::StrokeKind::Outside);
        }
    }
}

fn balloon_options(app: &mut PhotocraftApp, ui: &mut egui::Ui, dim: Color32) {
    let lbl = |ui: &mut egui::Ui, s: &str| {
        ui.label(egui::RichText::new(s).color(dim).size(12.0));
    };
    let o = &mut app.ui.tool_options;
    crate::widgets::vline(ui, 22.0);
    for (mode, label) in [("tail", "Tail"), ("cloud", "Cloud"), ("spiky", "Spiky")] {
        if crate::widgets::pill_tab(ui, tl!(label), o.balloon_mode == mode).clicked() {
            o.balloon_mode = mode.to_string();
        }
    }
    crate::widgets::vline(ui, 22.0);
    match o.balloon_mode.as_str() {
        "cloud" => {
            lbl(ui, tl!("Bumps:"));
            let mut bumps = o.balloon_bumps as f32;
            if crate::widgets::value_field(ui, &mut bumps, 3.0..=64.0, "", 42.0).changed() {
                o.balloon_bumps = bumps.round().clamp(3.0, 64.0) as u32;
            }
            lbl(ui, tl!("Depth:"));
            crate::widgets::value_field(ui, &mut o.balloon_amplitude, 0.0..=1.0, "", 52.0);
            lbl(ui, tl!("Jitter:"));
            crate::widgets::value_field(ui, &mut o.balloon_jitter, 0.0..=1.0, "", 52.0);
        }
        "spiky" => {
            lbl(ui, tl!("Spikes:"));
            let mut spikes = o.balloon_spikes as f32;
            if crate::widgets::value_field(ui, &mut spikes, 3.0..=128.0, "", 42.0).changed() {
                o.balloon_spikes = spikes.round().clamp(3.0, 128.0) as u32;
            }
            lbl(ui, tl!("Length:"));
            crate::widgets::value_field(ui, &mut o.balloon_length, 0.0..=2.0, "", 52.0);
            lbl(ui, tl!("Jitter:"));
            crate::widgets::value_field(ui, &mut o.balloon_jitter, 0.0..=1.0, "", 52.0);
            ui.checkbox(&mut o.balloon_smooth, tl!("Round"));
            ui.checkbox(&mut o.balloon_inward, tl!("In"));
        }
        _ => {
            const PRESETS: [(&str, &str); 4] = [("wedge", "Wedge"), ("arc", "Arc"), ("double", "Double"), ("thought", "Thought")];
            let idx = PRESETS.iter().position(|(id, _)| *id == o.balloon_preset).unwrap_or(0);
            let label = format!("{}: {}", tl!("Tail"), tl!(PRESETS[idx].1));
            if ui.button(label).on_hover_text(tl!("Drag from the balloon towards the speaker")).clicked() {
                o.balloon_preset = PRESETS[(idx + 1) % PRESETS.len()].0.to_string();
            }
            lbl(ui, tl!("Mouth:"));
            crate::widgets::value_field(ui, &mut o.balloon_mouth, 2.0..=400.0, "px", 56.0);
        }
    }
}

fn vector_curve_options(app: &mut PhotocraftApp, ui: &mut egui::Ui, dim: Color32) {
    let lbl = |ui: &mut egui::Ui, s: &str| {
        ui.label(egui::RichText::new(s).color(dim).size(12.0));
    };
    crate::widgets::vline(ui, 22.0);
    let cur_mode = app.ui.vector_curve.as_ref().map_or(VectorCurveMode::Draw, |vc| vc.mode);
    for (mode, label) in [(VectorCurveMode::Draw, "Draw"), (VectorCurveMode::Weight, "Weight"), (VectorCurveMode::Edit, "Edit")] {
        if crate::widgets::pill_tab(ui, tl!(label), cur_mode == mode).clicked() {
            app.ui.vector_curve.get_or_insert_with(VectorCurveState::default).mode = mode;
        }
    }
    crate::widgets::vline(ui, 22.0);
    lbl(ui, tl!("Width:"));
    let picked = picked_stroke_width(app.ui.vector_curve.as_ref(), &app.session);
    if let Some((_, _, w)) = picked {
        app.ui.tool_options.vector_curve_width = w;
        crate::manga_seam::sync_sai_base_width(app, w);
    }
    let width_changed = crate::widgets::value_field(ui, &mut app.ui.tool_options.vector_curve_width, 0.5..=500.0, "px", 54.0).changed();
    if width_changed {
        let w = app.ui.tool_options.vector_curve_width;
        crate::manga_seam::sync_sai_base_width(app, w);
        apply_stroke_width(app, w);
    }
    if ui.checkbox(&mut app.ui.tool_options.vector_curve_tapered, tl!("Taper")).changed() {
        let t = app.ui.tool_options.vector_curve_tapered;
        if let Some(vc) = app.ui.vector_curve.as_mut() {
            vc.auto_taper_ends = t;
        }
        crate::manga_seam::sync_sai_auto_taper(app, t);
    }
    lbl(ui, tl!("Tension:"));
    let tension_changed = crate::widgets::value_field(ui, &mut app.ui.tool_options.vector_curve_tension, 0.0..=1.0, "", 44.0).changed();
    let tension_val = f64::from(app.ui.tool_options.vector_curve_tension);
    let panel_snap_close = crate::manga_seam::sai_snap_close(app);
    let (auto_taper_changed, snap_close_changed, vc_auto_taper, vc_snap_close) = {
        let vc = app.ui.vector_curve.get_or_insert_with(|| VectorCurveState { snap_close: panel_snap_close, ..VectorCurveState::default() });
        let auto_taper_changed = ui.checkbox(&mut vc.auto_taper_ends, tl!("Auto Taper")).changed();
        let snap_close_changed = ui.checkbox(&mut vc.snap_close, tl!("Snap Close")).changed();
        vc.tension = tension_val;
        (auto_taper_changed, snap_close_changed, vc.auto_taper_ends, vc.snap_close)
    };
    if tension_changed {
        crate::manga_seam::sync_sai_tension(app, tension_val);
    }
    if auto_taper_changed {
        crate::manga_seam::sync_sai_auto_taper(app, vc_auto_taper);
        app.ui.tool_options.vector_curve_tapered = vc_auto_taper || app.ui.tool_options.vector_curve_min_ratio < 0.999;
    }
    if snap_close_changed {
        crate::manga_seam::sync_sai_snap_close(app, vc_snap_close);
    }
    crate::widgets::vline(ui, 22.0);
    lbl(ui, "Click: node · Start point: close · Enter/Esc/Dbl-Click: finish · Shift+Click/Drag: edit & insert node · Alt+Click: corner · Alt+Drag: weight");
}

pub(super) fn tool_options_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui, tool: Tool) {
    let dim = Tokens::get(ui.ctx()).text_dim;
    match tool {
        Tool::Balloon => balloon_options(app, ui, dim),
        Tool::VectorCurve => vector_curve_options(app, ui, dim),
        _ => {}
    }
}
