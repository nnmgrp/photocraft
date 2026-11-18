//! 2D interactive canvas gizmo for `SfxData` (Transform/Skew/Perspective, Per-Glyph, Mesh Lattice + Marquee, Liquify Brush).
//!
//! All document mutations are emitted as `OutCommand` (`"sfx.update"` with `"coalesce"`) so dragging
//! collapses into a single history step and `Escape` restores the pre-drag snapshot cleanly.

use egui::{Color32, CursorIcon, Pos2, Rect, Stroke, pos2, vec2};
use photocraft_doc::sfx::{MAX_SFX_LIQUIFY_STROKES, SfxData, SfxFreeTransform, SfxGlyphTransform, SfxLiquifyBrush, SfxMeshLattice, SfxWarpMode};
use photocraft_doc::{Document, LayerId};
use photocraft_manga_core::sfx_geom::{
    PreparedSfx, apply_free_transform_point, build_sfx_geometry, eval_mesh_uv, invert_free_transform_point, mesh_node_doc_pos, move_mesh_node_linked,
    transform_point_full,
};
use photocraft_text::TextEngine;
use serde_json::json;

use crate::BridgeDocId;
use crate::OutCommand;
use crate::theme::Tokens;
use crate::view::{CanvasToolEvent, CanvasViewXform};

const HANDLE_HIT_PX: f32 = 10.0;
const ROTATE_STEM_PX: f32 = 24.0;
const MIN_LIQUIFY_STEP_PX: f64 = 12.0;

/// View context passed by `ui-egui` into [`SfxGizmoState`] per frame or pointer event.
pub struct SfxView<'a> {
    pub doc_id: Option<BridgeDocId>,
    pub doc: Option<&'a Document>,
    pub active_layer: Option<LayerId>,
    pub tool_is_sfx: bool,
    pub panel_open: bool,
    pub view_xform: CanvasViewXform,
}

/// Output produced by [`pointer`] or [`cancel_drag`].
#[derive(Default)]
pub struct SfxGizmoOutcome {
    pub handled: bool,
    pub commands: Vec<OutCommand>,
    pub updated_sfx: Option<SfxData>,
}

#[derive(Clone, Debug)]
struct CachedSfxGeom {
    data: SfxData,
    dpi_bits: u32,
    geom: PreparedSfx,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FreeHandle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
    Rotate,
    Body,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlyphHandle {
    Body(usize),
    Rotate(usize),
    Scale(usize),
}

#[derive(Clone, Debug)]
enum DragMode {
    Free(FreeHandle),
    Glyph(GlyphHandle),
    MeshNode { row: usize, col: usize, multi: bool },
    MeshMarquee { start_doc: [f64; 2], cur_doc: [f64; 2], additive: bool },
    Liquify { stroke_start_idx: usize, last_stamp_doc: [f64; 2] },
}

#[derive(Clone, Debug)]
struct ActiveDrag {
    doc_id: BridgeDocId,
    layer_id: LayerId,
    coalesce: String,
    start_doc: [f64; 2],
    initial_sfx: SfxData,
    reported_sfx: SfxData,
    rest_box: [f64; 4],
    mode: DragMode,
}

/// Persistent gizmo state owned inside [`crate::MangaUiState`].
pub struct SfxGizmoState {
    pub selected_glyph: Option<usize>,
    pub selected_nodes: Vec<(usize, usize)>,
    pub liquify_radius_px: f64,
    pub liquify_strength: f64,
    pub last_cursor_doc: Option<[f64; 2]>,
    drag: Option<ActiveDrag>,
    cached: std::sync::Mutex<Option<CachedSfxGeom>>,
    bound_target: Option<(BridgeDocId, LayerId)>,
}

impl Default for SfxGizmoState {
    fn default() -> Self {
        Self {
            selected_glyph: None,
            selected_nodes: Vec::new(),
            liquify_radius_px: 48.0,
            liquify_strength: 1.0,
            last_cursor_doc: None,
            drag: None,
            cached: std::sync::Mutex::new(None),
            bound_target: None,
        }
    }
}

impl SfxGizmoState {
    pub fn has_active_drag(&self) -> bool {
        self.drag.is_some()
    }

    /// Computes or returns the cached `PreparedSfx` geometry using `engine`.
    pub fn ensure_geom_with(&self, data: &SfxData, dpi: f32, engine: &mut TextEngine) -> PreparedSfx {
        let dpi = dpi.clamp(1.0, 1200.0);
        let dpi_bits = dpi.to_bits();
        let mut guard = self.cached.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = guard.as_ref().filter(|c| c.dpi_bits == dpi_bits && &c.data == data) {
            return c.geom.clone();
        }
        let geom = build_sfx_geometry(data, engine, dpi);
        *guard = Some(CachedSfxGeom { data: data.clone(), dpi_bits, geom: geom.clone() });
        geom
    }

    /// Computes or returns the cached `PreparedSfx` geometry using the global shared text engine.
    pub fn ensure_geom(&self, data: &SfxData, dpi: f32) -> PreparedSfx {
        let dpi = dpi.clamp(1.0, 1200.0);
        let dpi_bits = dpi.to_bits();
        let mut guard = self.cached.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = guard.as_ref().filter(|c| c.dpi_bits == dpi_bits && &c.data == data) {
            return c.geom.clone();
        }
        let geom = match photocraft_text::shared().lock() {
            Ok(mut engine) => build_sfx_geometry(data, &mut engine, dpi),
            Err(poisoned) => {
                let mut engine = poisoned.into_inner();
                build_sfx_geometry(data, &mut engine, dpi)
            }
        };
        *guard = Some(CachedSfxGeom { data: data.clone(), dpi_bits, geom: geom.clone() });
        geom
    }

    /// Synchronizes selection state when the active layer changes.
    pub fn sync_target(&mut self, view: &SfxView<'_>) {
        if self.drag.is_some() {
            return;
        }
        let target = active_sfx_target(view).map(|(id, _)| (view.doc_id.unwrap_or(BridgeDocId(0)), id));
        if self.bound_target != target {
            self.bound_target = target;
            self.selected_glyph = None;
            self.selected_nodes.clear();
        }
    }
}

fn active_sfx_target<'a>(view: &'a SfxView<'_>) -> Option<(LayerId, &'a SfxData)> {
    let doc = view.doc?;
    let id = view.active_layer?;
    let layer = doc.layer(id)?;
    let sfx = layer.sfx.as_deref()?;
    Some((id, sfx))
}

fn is_active_on_canvas(gizmo: &SfxGizmoState, view: &SfxView<'_>) -> bool {
    gizmo.drag.is_some() || view.tool_is_sfx || view.panel_open
}

fn doc_dpi(view: &SfxView<'_>) -> f32 {
    view.doc.map_or(72.0, |d| d.resolution_dpi.max(1.0))
}

fn rest_box_points(rb: [f64; 4]) -> [([f64; 2], FreeHandle); 8] {
    let cx = (rb[0] + rb[2]) * 0.5;
    let cy = (rb[1] + rb[3]) * 0.5;
    [
        ([rb[0], rb[1]], FreeHandle::TopLeft),
        ([cx, rb[1]], FreeHandle::Top),
        ([rb[2], rb[1]], FreeHandle::TopRight),
        ([rb[2], cy], FreeHandle::Right),
        ([rb[2], rb[3]], FreeHandle::BottomRight),
        ([cx, rb[3]], FreeHandle::Bottom),
        ([rb[0], rb[3]], FreeHandle::BottomLeft),
        ([rb[0], cy], FreeHandle::Left),
    ]
}

fn rotate_stem_doc(ft: &SfxFreeTransform, rb: [f64; 4], xf: &CanvasViewXform) -> [f64; 2] {
    let top_mid_doc = apply_free_transform_point(ft, rb, [(rb[0] + rb[2]) * 0.5, rb[1]]);
    let center_doc = apply_free_transform_point(ft, rb, [(rb[0] + rb[2]) * 0.5, (rb[1] + rb[3]) * 0.5]);
    let top_scr = xf.to_screen(top_mid_doc[0] as f32, top_mid_doc[1] as f32);
    let cen_scr = xf.to_screen(center_doc[0] as f32, center_doc[1] as f32);
    let mut dir = top_scr - cen_scr;
    let len = dir.length();
    if len > 1e-3 {
        dir /= len;
    } else {
        dir = vec2(0.0, -1.0);
    }
    let stem_scr = top_scr + dir * ROTATE_STEM_PX;
    xf.to_doc(stem_scr)
}

fn point_in_rect(r: [f64; 4], p: [f64; 2]) -> bool {
    p[0] >= r[0] && p[0] <= r[2] && p[1] >= r[1] && p[1] <= r[3]
}

fn hit_free_transform(data: &SfxData, rb: [f64; 4], xf: &CanvasViewXform, at_doc: [f64; 2]) -> Option<FreeHandle> {
    let p_scr = xf.to_screen(at_doc[0] as f32, at_doc[1] as f32);
    let stem_doc = rotate_stem_doc(&data.free_transform, rb, xf);
    let stem_scr = xf.to_screen(stem_doc[0] as f32, stem_doc[1] as f32);
    if stem_scr.distance(p_scr) <= HANDLE_HIT_PX {
        return Some(FreeHandle::Rotate);
    }
    for (pt, kind) in rest_box_points(rb) {
        let wp = apply_free_transform_point(&data.free_transform, rb, pt);
        let s = xf.to_screen(wp[0] as f32, wp[1] as f32);
        if s.distance(p_scr) <= HANDLE_HIT_PX {
            return Some(kind);
        }
    }
    if let Some(rest_pt) = invert_free_transform_point(&data.free_transform, rb, at_doc)
        && point_in_rect(rb, rest_pt)
    {
        return Some(FreeHandle::Body);
    }
    None
}

fn hit_mesh_node(data: &SfxData, rb: [f64; 4], xf: &CanvasViewXform, at_doc: [f64; 2]) -> Option<(usize, usize)> {
    let p_scr = xf.to_screen(at_doc[0] as f32, at_doc[1] as f32);
    let rows = usize::from(data.mesh.rows.max(2));
    let cols = usize::from(data.mesh.cols.max(2));
    let mut best: Option<(f32, usize, usize)> = None;
    for r in 0..rows {
        for c in 0..cols {
            if let Some(np) = mesh_node_doc_pos(data, rb, r, c) {
                let d = xf.to_screen(np[0] as f32, np[1] as f32).distance(p_scr);
                if d <= HANDLE_HIT_PX && best.is_none_or(|(bd, _, _)| d < bd) {
                    best = Some((d, r, c));
                }
            }
        }
    }
    best.map(|(_, r, c)| (r, c))
}

fn glyph_handles_screen(wb: [f64; 4], xf: &CanvasViewXform) -> (Pos2, Pos2) {
    let top_mid = xf.to_screen(((wb[0] + wb[2]) * 0.5) as f32, wb[1] as f32);
    let rot_pos = top_mid - vec2(0.0, ROTATE_STEM_PX * 0.8);
    let br_pos = xf.to_screen(wb[2] as f32, wb[3] as f32);
    (rot_pos, br_pos)
}

fn hit_glyph(geom: &PreparedSfx, selected: Option<usize>, xf: &CanvasViewXform, at_doc: [f64; 2]) -> Option<GlyphHandle> {
    let p_scr = xf.to_screen(at_doc[0] as f32, at_doc[1] as f32);
    if let Some(idx) = selected
        && let Some(g) = geom.glyphs.iter().find(|g| g.glyph_index == idx)
    {
        let (rot_scr, scale_scr) = glyph_handles_screen(g.warped_bounds, xf);
        if rot_scr.distance(p_scr) <= HANDLE_HIT_PX {
            return Some(GlyphHandle::Rotate(idx));
        }
        if scale_scr.distance(p_scr) <= HANDLE_HIT_PX {
            return Some(GlyphHandle::Scale(idx));
        }
    }
    for g in geom.glyphs.iter().rev() {
        let pad_doc = 4.0 / f64::from(xf.zoom.max(0.05));
        let expanded = [g.warped_bounds[0] - pad_doc, g.warped_bounds[1] - pad_doc, g.warped_bounds[2] + pad_doc, g.warped_bounds[3] + pad_doc];
        if point_in_rect(expanded, at_doc) {
            return Some(GlyphHandle::Body(g.glyph_index));
        }
    }
    None
}

fn to_pre_ft(ft: &SfxFreeTransform, rb: [f64; 4], p: [f64; 2]) -> [f64; 2] {
    invert_free_transform_point(ft, rb, p).unwrap_or(p)
}

fn free_corner_delta_signs(kind: FreeHandle) -> (f64, f64) {
    match kind {
        FreeHandle::TopLeft => (-1.0, -1.0),
        FreeHandle::Top => (0.0, -1.0),
        FreeHandle::TopRight => (1.0, -1.0),
        FreeHandle::Right => (1.0, 0.0),
        FreeHandle::BottomRight => (1.0, 1.0),
        FreeHandle::Bottom => (0.0, 1.0),
        FreeHandle::BottomLeft => (-1.0, 1.0),
        FreeHandle::Left => (-1.0, 0.0),
        FreeHandle::Rotate | FreeHandle::Body => (0.0, 0.0),
    }
}

fn apply_free_drag(drag: &ActiveDrag, at_doc: [f64; 2], mods: egui::Modifiers, next: &mut SfxData) {
    let rb = drag.rest_box;
    let w = (rb[2] - rb[0]).max(1.0);
    let h = (rb[3] - rb[1]).max(1.0);
    let center = [(rb[0] + rb[2]) * 0.5, (rb[1] + rb[3]) * 0.5];
    let DragMode::Free(handle) = drag.mode else { return };

    match handle {
        FreeHandle::Body => {
            let mut dx = at_doc[0] - drag.start_doc[0];
            let mut dy = at_doc[1] - drag.start_doc[1];
            if mods.shift {
                if dx.abs() >= dy.abs() {
                    dy = 0.0;
                } else {
                    dx = 0.0;
                }
            }
            next.origin = [drag.initial_sfx.origin[0] + dx, drag.initial_sfx.origin[1] + dy];
        }
        FreeHandle::Rotate => {
            let a0 = (drag.start_doc[1] - center[1]).atan2(drag.start_doc[0] - center[0]);
            let a1 = (at_doc[1] - center[1]).atan2(at_doc[0] - center[0]);
            let mut deg = drag.initial_sfx.free_transform.rotation_deg + (a1 - a0).to_degrees();
            if mods.shift {
                deg = (deg / 15.0).round() * 15.0;
            }
            next.free_transform.rotation_deg = deg;
        }
        edge_or_corner => {
            // Undo only the rotation into local frame so edge/corner directions feel natural.
            let (s, c) = (-drag.initial_sfx.free_transform.rotation_deg).to_radians().sin_cos();
            let rot = |p: [f64; 2]| -> [f64; 2] {
                let rx = p[0] - center[0];
                let ry = p[1] - center[1];
                [rx * c - ry * s, rx * s + ry * c]
            };
            let p0 = rot(drag.start_doc);
            let p1 = rot(at_doc);
            let d = [p1[0] - p0[0], p1[1] - p0[1]];
            let (sx_sign, sy_sign) = free_corner_delta_signs(edge_or_corner);
            let ft0 = drag.initial_sfx.free_transform;

            if mods.ctrl {
                match edge_or_corner {
                    FreeHandle::Top => {
                        let skew_delta = (d[0] / (h * 0.5)).atan().to_degrees();
                        next.free_transform.skew_x_deg = (ft0.skew_x_deg - skew_delta).clamp(-75.0, 75.0);
                    }
                    FreeHandle::Bottom => {
                        let skew_delta = (d[0] / (h * 0.5)).atan().to_degrees();
                        next.free_transform.skew_x_deg = (ft0.skew_x_deg + skew_delta).clamp(-75.0, 75.0);
                    }
                    FreeHandle::Left => {
                        let skew_delta = (d[1] / (w * 0.5)).atan().to_degrees();
                        next.free_transform.skew_y_deg = (ft0.skew_y_deg - skew_delta).clamp(-75.0, 75.0);
                    }
                    FreeHandle::Right => {
                        let skew_delta = (d[1] / (w * 0.5)).atan().to_degrees();
                        next.free_transform.skew_y_deg = (ft0.skew_y_deg + skew_delta).clamp(-75.0, 75.0);
                    }
                    FreeHandle::TopLeft | FreeHandle::TopRight | FreeHandle::BottomRight | FreeHandle::BottomLeft => {
                        next.free_transform.perspective_x = (ft0.perspective_x - sx_sign * d[0] / (w * 0.5)).clamp(-0.75, 0.75);
                        next.free_transform.perspective_y = (ft0.perspective_y - sy_sign * d[1] / (h * 0.5)).clamp(-0.75, 0.75);
                    }
                    FreeHandle::Rotate | FreeHandle::Body => {}
                }
            } else {
                let mut sx = ft0.scale_x;
                let mut sy = ft0.scale_y;
                if sx_sign != 0.0 {
                    sx = (ft0.scale_x + sx_sign * d[0] / (w * 0.5)).clamp(0.1, 16.0);
                }
                if sy_sign != 0.0 {
                    sy = (ft0.scale_y + sy_sign * d[1] / (h * 0.5)).clamp(0.1, 16.0);
                }
                if mods.shift && sx_sign != 0.0 && sy_sign != 0.0 {
                    let ratio_x = (sx / ft0.scale_x.max(0.05)).abs();
                    let ratio_y = (sy / ft0.scale_y.max(0.05)).abs();
                    let uni = if (ratio_x - 1.0).abs() >= (ratio_y - 1.0).abs() { ratio_x } else { ratio_y };
                    sx = (ft0.scale_x * uni).clamp(0.1, 16.0);
                    sy = (ft0.scale_y * uni).clamp(0.1, 16.0);
                }
                next.free_transform.scale_x = sx;
                next.free_transform.scale_y = sy;
            }
        }
    }
    next.validate();
}

fn find_or_insert_glyph_mut(data: &mut SfxData, glyph_index: usize) -> &mut SfxGlyphTransform {
    if let Some(pos) = data.per_glyph.iter().position(|g| g.glyph_index == glyph_index) {
        return &mut data.per_glyph[pos];
    }
    data.per_glyph.push(SfxGlyphTransform { glyph_index, ..SfxGlyphTransform::default() });
    let last = data.per_glyph.len() - 1;
    &mut data.per_glyph[last]
}

fn apply_glyph_drag(drag: &ActiveDrag, at_doc: [f64; 2], mods: egui::Modifiers, next: &mut SfxData) {
    let DragMode::Glyph(gh) = drag.mode else { return };
    let ft = &drag.initial_sfx.free_transform;
    let p0 = to_pre_ft(ft, drag.rest_box, drag.start_doc);
    let p1 = to_pre_ft(ft, drag.rest_box, at_doc);

    match gh {
        GlyphHandle::Body(idx) => {
            let base = drag
                .initial_sfx
                .per_glyph
                .iter()
                .find(|g| g.glyph_index == idx)
                .copied()
                .unwrap_or(SfxGlyphTransform { glyph_index: idx, ..SfxGlyphTransform::default() });
            let mut dx = p1[0] - p0[0];
            let mut dy = p1[1] - p0[1];
            if mods.shift {
                if dx.abs() >= dy.abs() {
                    dy = 0.0;
                } else {
                    dx = 0.0;
                }
            }
            let entry = find_or_insert_glyph_mut(next, idx);
            entry.dx = base.dx + dx;
            entry.dy = base.dy + dy;
        }
        GlyphHandle::Rotate(idx) => {
            let base = drag
                .initial_sfx
                .per_glyph
                .iter()
                .find(|g| g.glyph_index == idx)
                .copied()
                .unwrap_or(SfxGlyphTransform { glyph_index: idx, ..SfxGlyphTransform::default() });
            let c = [(drag.rest_box[0] + drag.rest_box[2]) * 0.5, (drag.rest_box[1] + drag.rest_box[3]) * 0.5];
            let a0 = (p0[1] - c[1]).atan2(p0[0] - c[0]);
            let a1 = (p1[1] - c[1]).atan2(p1[0] - c[0]);
            let mut deg = base.rotation_deg + (a1 - a0).to_degrees();
            if mods.shift {
                deg = (deg / 15.0).round() * 15.0;
            }
            find_or_insert_glyph_mut(next, idx).rotation_deg = deg;
        }
        GlyphHandle::Scale(idx) => {
            let base = drag
                .initial_sfx
                .per_glyph
                .iter()
                .find(|g| g.glyph_index == idx)
                .copied()
                .unwrap_or(SfxGlyphTransform { glyph_index: idx, ..SfxGlyphTransform::default() });
            let dy = p1[1] - p0[1];
            let dx = p1[0] - p0[0];
            let factor = (1.0 + (dx + dy) / 64.0).clamp(0.2, 6.0);
            let entry = find_or_insert_glyph_mut(next, idx);
            entry.scale_x = (base.scale_x * factor).clamp(0.1, 8.0);
            entry.scale_y = if mods.shift { entry.scale_x } else { (base.scale_y * (1.0 + dy / 64.0)).clamp(0.1, 8.0) };
        }
    }
    next.validate();
}

fn apply_mesh_drag(drag: &ActiveDrag, selected_nodes: &[(usize, usize)], at_doc: [f64; 2], next: &mut SfxData) {
    let DragMode::MeshNode { row, col, multi } = drag.mode else { return };
    let w = (drag.rest_box[2] - drag.rest_box[0]).max(1.0);
    let h = (drag.rest_box[3] - drag.rest_box[1]).max(1.0);
    let p0 = to_pre_ft(&drag.initial_sfx.free_transform, drag.rest_box, drag.start_doc);
    let p1 = to_pre_ft(&drag.initial_sfx.free_transform, drag.rest_box, at_doc);
    let du = (p1[0] - p0[0]) / w;
    let dv = (p1[1] - p0[1]) / h;
    next.mesh = drag.initial_sfx.mesh.clone();
    if multi && selected_nodes.len() > 1 {
        let cols = usize::from(next.mesh.cols.max(2));
        let rows = usize::from(next.mesh.rows.max(2));
        for &(r, c) in selected_nodes {
            if r < rows && c < cols {
                let p = &mut next.mesh.points[r * cols + c];
                p[0] = (p[0] + du).clamp(-3.0, 4.0);
                p[1] = (p[1] + dv).clamp(-3.0, 4.0);
            }
        }
    } else {
        move_mesh_node_linked(&mut next.mesh, row, col, du, dv);
    }
    next.validate();
}

fn apply_liquify_drag(drag: &mut ActiveDrag, at_doc: [f64; 2], radius_px: f64, strength: f64, next: &mut SfxData) {
    let DragMode::Liquify { stroke_start_idx, ref mut last_stamp_doc } = drag.mode else { return };
    let rb = drag.rest_box;
    let w = (rb[2] - rb[0]).max(1.0);
    let h = (rb[3] - rb[1]).max(1.0);
    let p0 = to_pre_ft(&drag.initial_sfx.free_transform, rb, *last_stamp_doc);
    let p1 = to_pre_ft(&drag.initial_sfx.free_transform, rb, at_doc);
    let dist = (p1[0] - p0[0]).hypot(p1[1] - p0[1]);
    if dist < 0.25 {
        return;
    }
    let delta = [p1[0] - p0[0], p1[1] - p0[1]];
    let brush = SfxLiquifyBrush {
        center_uv: [(p0[0] - rb[0]) / w, (p0[1] - rb[1]) / h],
        delta_px: delta,
        radius_px: radius_px.clamp(2.0, 4000.0),
        strength: strength.clamp(0.0, 2.0),
    };
    if let Some(slot) = next.liquify_strokes.get_mut(stroke_start_idx) {
        *slot = brush;
    } else if next.liquify_strokes.len() < MAX_SFX_LIQUIFY_STROKES {
        next.liquify_strokes.push(brush);
    }
    // Advance stamp along long brush sweeps so continuous painting deforms along the full path.
    if dist >= (radius_px * 0.45).max(MIN_LIQUIFY_STEP_PX) && next.liquify_strokes.len() < MAX_SFX_LIQUIFY_STROKES {
        *last_stamp_doc = at_doc;
        drag.mode = DragMode::Liquify { stroke_start_idx: next.liquify_strokes.len(), last_stamp_doc: at_doc };
    }
    next.validate();
}

fn select_nodes_in_marquee(data: &SfxData, rb: [f64; 4], a: [f64; 2], b: [f64; 2], additive: bool, selected: &mut Vec<(usize, usize)>) {
    let r = [a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])];
    if !additive {
        selected.clear();
    }
    let rows = usize::from(data.mesh.rows.max(2));
    let cols = usize::from(data.mesh.cols.max(2));
    for row in 0..rows {
        for col in 0..cols {
            if let Some(p) = mesh_node_doc_pos(data, rb, row, col)
                && point_in_rect(r, p)
                && !selected.contains(&(row, col))
            {
                selected.push((row, col));
            }
        }
    }
    selected.sort_unstable();
}

fn push_update(drag: &mut ActiveDrag, next: SfxData, out: &mut SfxGizmoOutcome) {
    if next != drag.reported_sfx {
        let mut s = next.clone();
        s.validate();
        out.commands.push(OutCommand::new(
            "sfx.update",
            json!({
                "layer": drag.layer_id.0,
                "sfx": s,
                "coalesce": drag.coalesce,
            }),
        ));
        drag.reported_sfx = next.clone();
        out.updated_sfx = Some(next);
    }
}

/// Handles canvas pointer events (`Down`, `Move`, `Up`) for the active SFX layer gizmo.
pub fn pointer(gizmo: &mut SfxGizmoState, view: &SfxView<'_>, ev: CanvasToolEvent, mods: egui::Modifiers) -> SfxGizmoOutcome {
    let mut out = SfxGizmoOutcome::default();
    if !is_active_on_canvas(gizmo, view) {
        return out;
    }

    match ev {
        CanvasToolEvent::Down { x, y, .. } => {
            let Some((layer_id, sfx_ref)) = active_sfx_target(view) else { return out };
            let sfx = sfx_ref.clone();
            let doc_id = view.doc_id.unwrap_or(BridgeDocId(0));
            let dpi = doc_dpi(view);
            let geom = gizmo.ensure_geom(&sfx, dpi);
            if geom.rest_box[2] <= geom.rest_box[0] && geom.rest_box[3] <= geom.rest_box[1] && sfx.warp_mode != SfxWarpMode::FreeTransform {
                return out;
            }
            let rb = geom.rest_box;
            let xf = &view.view_xform;
            let coalesce = format!("sfx-gizmo:{}:{}:{:?}", doc_id.0, layer_id.0, sfx.warp_mode);

            let mode = match sfx.warp_mode {
                SfxWarpMode::FreeTransform => {
                    let Some(h) = hit_free_transform(&sfx, rb, xf, [x, y]) else { return out };
                    DragMode::Free(h)
                }
                SfxWarpMode::PerGlyph => {
                    let Some(gh) = hit_glyph(&geom, gizmo.selected_glyph, xf, [x, y]) else {
                        gizmo.selected_glyph = None;
                        return out;
                    };
                    let g_idx = match gh {
                        GlyphHandle::Body(i) | GlyphHandle::Rotate(i) | GlyphHandle::Scale(i) => i,
                    };
                    gizmo.selected_glyph = Some(g_idx);
                    DragMode::Glyph(gh)
                }
                SfxWarpMode::MeshLattice => {
                    if let Some((row, col)) = hit_mesh_node(&sfx, rb, xf, [x, y]) {
                        if mods.shift {
                            if let Some(pos) = gizmo.selected_nodes.iter().position(|&n| n == (row, col)) {
                                gizmo.selected_nodes.remove(pos);
                            } else {
                                gizmo.selected_nodes.push((row, col));
                                gizmo.selected_nodes.sort_unstable();
                            }
                        } else if !gizmo.selected_nodes.contains(&(row, col)) {
                            gizmo.selected_nodes = vec![(row, col)];
                        }
                        let multi = gizmo.selected_nodes.len() > 1 && gizmo.selected_nodes.contains(&(row, col));
                        DragMode::MeshNode { row, col, multi }
                    } else if view.tool_is_sfx {
                        DragMode::MeshMarquee { start_doc: [x, y], cur_doc: [x, y], additive: mods.shift }
                    } else {
                        return out;
                    }
                }
                SfxWarpMode::LiquifyField => {
                    let inside = invert_free_transform_point(&sfx.free_transform, rb, [x, y]).is_some_and(|p| {
                        let pad = gizmo.liquify_radius_px;
                        point_in_rect([rb[0] - pad, rb[1] - pad, rb[2] + pad, rb[3] + pad], p)
                    });
                    if !inside && !view.tool_is_sfx {
                        return out;
                    }
                    let idx = sfx.liquify_strokes.len().min(MAX_SFX_LIQUIFY_STROKES);
                    DragMode::Liquify { stroke_start_idx: idx, last_stamp_doc: [x, y] }
                }
            };

            gizmo.drag = Some(ActiveDrag { doc_id, layer_id, coalesce, start_doc: [x, y], reported_sfx: sfx.clone(), initial_sfx: sfx, rest_box: rb, mode });
            out.handled = true;
            out
        }
        CanvasToolEvent::Move { x, y, .. } => {
            gizmo.last_cursor_doc = Some([x, y]);
            let Some(mut drag) = gizmo.drag.take() else { return out };
            if view.doc_id != Some(drag.doc_id) || view.active_layer != Some(drag.layer_id) {
                gizmo.drag = Some(drag);
                return out;
            }
            let mut next = drag.reported_sfx.clone();
            match &drag.mode {
                DragMode::Free(_) => {
                    next = drag.initial_sfx.clone();
                    apply_free_drag(&drag, [x, y], mods, &mut next);
                }
                DragMode::Glyph(_) => {
                    next = drag.initial_sfx.clone();
                    apply_glyph_drag(&drag, [x, y], mods, &mut next);
                }
                DragMode::MeshNode { .. } => {
                    apply_mesh_drag(&drag, &gizmo.selected_nodes, [x, y], &mut next);
                }
                DragMode::MeshMarquee { start_doc, additive, .. } => {
                    let start_doc = *start_doc;
                    let additive = *additive;
                    drag.mode = DragMode::MeshMarquee { start_doc, cur_doc: [x, y], additive };
                }
                DragMode::Liquify { .. } => {
                    let rad = gizmo.liquify_radius_px;
                    let strength = gizmo.liquify_strength;
                    apply_liquify_drag(&mut drag, [x, y], rad, strength, &mut next);
                }
            }
            push_update(&mut drag, next, &mut out);
            gizmo.drag = Some(drag);
            out.handled = true;
            out
        }
        CanvasToolEvent::Up { x, y } => {
            let Some(mut drag) = gizmo.drag.take() else { return out };
            let mut next = drag.reported_sfx.clone();
            match drag.mode {
                DragMode::MeshMarquee { start_doc, additive, .. } => {
                    let moved = (x - start_doc[0]).hypot(y - start_doc[1]) > 4.0 / f64::from(view.view_xform.zoom.max(0.05));
                    if moved {
                        select_nodes_in_marquee(&drag.initial_sfx, drag.rest_box, start_doc, [x, y], additive, &mut gizmo.selected_nodes);
                    } else if !additive {
                        gizmo.selected_nodes.clear();
                    }
                }
                DragMode::Free(_) => {
                    next = drag.initial_sfx.clone();
                    apply_free_drag(&drag, [x, y], mods, &mut next);
                    push_update(&mut drag, next, &mut out);
                }
                DragMode::Glyph(_) => {
                    next = drag.initial_sfx.clone();
                    apply_glyph_drag(&drag, [x, y], mods, &mut next);
                    push_update(&mut drag, next, &mut out);
                }
                DragMode::MeshNode { .. } => {
                    apply_mesh_drag(&drag, &gizmo.selected_nodes, [x, y], &mut next);
                    push_update(&mut drag, next, &mut out);
                }
                DragMode::Liquify { .. } => {
                    let rad = gizmo.liquify_radius_px;
                    let strength = gizmo.liquify_strength;
                    apply_liquify_drag(&mut drag, [x, y], rad, strength, &mut next);
                    push_update(&mut drag, next, &mut out);
                }
            }
            out.handled = true;
            out
        }
    }
}

/// Cancels an active SFX drag and restores its pre-drag snapshot via coalesced update.
pub fn cancel_drag(gizmo: &mut SfxGizmoState, view: &SfxView<'_>) -> SfxGizmoOutcome {
    let mut out = SfxGizmoOutcome::default();
    let Some(drag) = gizmo.drag.take() else { return out };
    if view.doc_id == Some(drag.doc_id) && drag.reported_sfx != drag.initial_sfx {
        let mut restored = drag.initial_sfx.clone();
        restored.validate();
        out.commands.push(OutCommand::new(
            "sfx.update",
            json!({
                "layer": drag.layer_id.0,
                "sfx": restored,
                "coalesce": drag.coalesce,
            }),
        ));
        out.updated_sfx = Some(drag.initial_sfx);
    }
    out.handled = true;
    out
}

/// Returns hover cursor feedback over SFX handles or brush circle.
pub fn hover_cursor(gizmo: &SfxGizmoState, view: &SfxView<'_>, cursor_scr: Pos2) -> Option<CursorIcon> {
    if !is_active_on_canvas(gizmo, view) {
        return None;
    }
    let at_doc = view.view_xform.to_doc(cursor_scr);
    if let Some(drag) = &gizmo.drag {
        return Some(match drag.mode {
            DragMode::Free(FreeHandle::Body) => CursorIcon::Grabbing,
            DragMode::Free(FreeHandle::Rotate) => CursorIcon::Alias,
            DragMode::Free(_) | DragMode::Glyph(_) | DragMode::MeshNode { .. } => CursorIcon::Grabbing,
            DragMode::MeshMarquee { .. } | DragMode::Liquify { .. } => CursorIcon::Crosshair,
        });
    }
    let (_, sfx_ref) = active_sfx_target(view)?;
    let sfx = sfx_ref.clone();
    let dpi = doc_dpi(view);
    let geom = gizmo.ensure_geom(&sfx, dpi);
    let rb = geom.rest_box;
    let xf = &view.view_xform;
    match sfx.warp_mode {
        SfxWarpMode::FreeTransform => match hit_free_transform(&sfx, rb, xf, at_doc)? {
            FreeHandle::Body => Some(CursorIcon::Move),
            FreeHandle::Rotate => Some(CursorIcon::Alias),
            FreeHandle::Top | FreeHandle::Bottom => Some(CursorIcon::ResizeVertical),
            FreeHandle::Left | FreeHandle::Right => Some(CursorIcon::ResizeHorizontal),
            FreeHandle::TopLeft | FreeHandle::BottomRight => Some(CursorIcon::ResizeNwSe),
            FreeHandle::TopRight | FreeHandle::BottomLeft => Some(CursorIcon::ResizeNeSw),
        },
        SfxWarpMode::PerGlyph => match hit_glyph(&geom, gizmo.selected_glyph, xf, at_doc)? {
            GlyphHandle::Body(_) => Some(CursorIcon::Move),
            GlyphHandle::Rotate(_) => Some(CursorIcon::Alias),
            GlyphHandle::Scale(_) => Some(CursorIcon::ResizeNwSe),
        },
        SfxWarpMode::MeshLattice => {
            if hit_mesh_node(&sfx, rb, xf, at_doc).is_some() {
                Some(CursorIcon::Grab)
            } else if view.tool_is_sfx {
                Some(CursorIcon::Crosshair)
            } else {
                None
            }
        }
        SfxWarpMode::LiquifyField => {
            if view.tool_is_sfx {
                Some(CursorIcon::Crosshair)
            } else {
                None
            }
        }
    }
}

/// Draws active SFX gizmos onto the canvas overlay painter.
pub fn draw(painter: &egui::Painter, gizmo: &SfxGizmoState, view: &SfxView<'_>) {
    if !is_active_on_canvas(gizmo, view) {
        return;
    }
    let Some((_, sfx_ref)) = active_sfx_target(view) else { return };
    let sfx = sfx_ref.clone();
    let dpi = doc_dpi(view);
    let geom = gizmo.ensure_geom(&sfx, dpi);
    let rb = geom.rest_box;
    if rb[2] <= rb[0] && rb[3] <= rb[1] {
        return;
    }

    let tokens = Tokens::get(painter.ctx());
    let accent = tokens.accent;
    let line_stroke = Stroke::new(1.5, accent);
    let fill_handle = Color32::WHITE;
    let xf = &view.view_xform;

    match sfx.warp_mode {
        SfxWarpMode::FreeTransform => {
            let corners = [[rb[0], rb[1]], [rb[2], rb[1]], [rb[2], rb[3]], [rb[0], rb[3]]];
            let scr: Vec<Pos2> = corners
                .iter()
                .map(|&p| {
                    let wp = apply_free_transform_point(&sfx.free_transform, rb, p);
                    xf.to_screen(wp[0] as f32, wp[1] as f32)
                })
                .collect();
            for i in 0..4 {
                painter.line_segment([scr[i], scr[(i + 1) % 4]], line_stroke);
            }
            let top_mid = apply_free_transform_point(&sfx.free_transform, rb, [(rb[0] + rb[2]) * 0.5, rb[1]]);
            let top_mid_scr = xf.to_screen(top_mid[0] as f32, top_mid[1] as f32);
            let stem_doc = rotate_stem_doc(&sfx.free_transform, rb, xf);
            let stem_scr = xf.to_screen(stem_doc[0] as f32, stem_doc[1] as f32);
            painter.line_segment([top_mid_scr, stem_scr], line_stroke);
            painter.circle_filled(stem_scr, 4.5, fill_handle);
            painter.circle_stroke(stem_scr, 4.5, line_stroke);

            for (pt, _) in rest_box_points(rb) {
                let wp = apply_free_transform_point(&sfx.free_transform, rb, pt);
                let sp = xf.to_screen(wp[0] as f32, wp[1] as f32);
                let r = Rect::from_center_size(sp, vec2(7.0, 7.0));
                painter.rect_filled(r, 1.0, fill_handle);
                painter.rect_stroke(r, 1.0, line_stroke, egui::StrokeKind::Outside);
            }
        }
        SfxWarpMode::PerGlyph => {
            for g in &geom.glyphs {
                let is_sel = gizmo.selected_glyph == Some(g.glyph_index);
                let r = Rect::from_two_pos(
                    xf.to_screen(g.warped_bounds[0] as f32, g.warped_bounds[1] as f32),
                    xf.to_screen(g.warped_bounds[2] as f32, g.warped_bounds[3] as f32),
                );
                let st = if is_sel { Stroke::new(1.8, accent) } else { Stroke::new(1.0, accent.gamma_multiply(0.55)) };
                painter.rect_stroke(r, 0.0, st, egui::StrokeKind::Outside);
                if is_sel {
                    let (rot_scr, scale_scr) = glyph_handles_screen(g.warped_bounds, xf);
                    let top_mid = pos2(r.center().x, r.top());
                    painter.line_segment([top_mid, rot_scr], st);
                    painter.circle_filled(rot_scr, 4.0, fill_handle);
                    painter.circle_stroke(rot_scr, 4.0, st);
                    let sr = Rect::from_center_size(scale_scr, vec2(7.0, 7.0));
                    painter.rect_filled(sr, 1.0, fill_handle);
                    painter.rect_stroke(sr, 1.0, st, egui::StrokeKind::Outside);
                }
            }
        }
        SfxWarpMode::MeshLattice => {
            draw_mesh_lattice(painter, &sfx, rb, xf, &gizmo.selected_nodes, accent);
            if let Some(ActiveDrag { mode: DragMode::MeshMarquee { start_doc, cur_doc, .. }, .. }) = &gizmo.drag {
                let r = Rect::from_two_pos(xf.to_screen(start_doc[0] as f32, start_doc[1] as f32), xf.to_screen(cur_doc[0] as f32, cur_doc[1] as f32));
                painter.rect_filled(r, 0.0, accent.gamma_multiply(0.14));
                painter.rect_stroke(r, 0.0, Stroke::new(1.2, accent), egui::StrokeKind::Inside);
            }
        }
        SfxWarpMode::LiquifyField => {
            let corners = [[rb[0], rb[1]], [rb[2], rb[1]], [rb[2], rb[3]], [rb[0], rb[3]]];
            let scr: Vec<Pos2> = corners
                .iter()
                .map(|&p| {
                    let wp = apply_free_transform_point(&sfx.free_transform, rb, p);
                    xf.to_screen(wp[0] as f32, wp[1] as f32)
                })
                .collect();
            let dim = Stroke::new(1.0, accent.gamma_multiply(0.5));
            for i in 0..4 {
                painter.line_segment([scr[i], scr[(i + 1) % 4]], dim);
            }
            let hover_doc = painter.ctx().input(|i| i.pointer.hover_pos()).map(|p| xf.to_doc(p)).or(gizmo.last_cursor_doc);
            if let Some(c) = hover_doc {
                let c_scr = xf.to_screen(c[0] as f32, c[1] as f32);
                let r_scr = (gizmo.liquify_radius_px as f32 * xf.zoom).max(4.0);
                painter.circle_stroke(c_scr, r_scr, Stroke::new(1.4, accent));
            }
        }
    }
}

fn draw_mesh_lattice(painter: &egui::Painter, data: &SfxData, rb: [f64; 4], xf: &CanvasViewXform, selected: &[(usize, usize)], accent: Color32) {
    let rows = usize::from(data.mesh.rows.max(2));
    let cols = usize::from(data.mesh.cols.max(2));
    let w = (rb[2] - rb[0]).max(1.0);
    let h = (rb[3] - rb[1]).max(1.0);
    let line_stroke = Stroke::new(1.2, accent.gamma_multiply(0.8));
    let segs = 8usize;

    let sample_uv = |u: f64, v: f64| -> Pos2 {
        let mut m = data.clone();
        m.liquify_strokes.clear();
        let uv = eval_mesh_uv(&data.mesh, u, v);
        let p = [rb[0] + uv[0] * w, rb[1] + uv[1] * h];
        let wp = apply_free_transform_point(&data.free_transform, rb, p);
        let _ = transform_point_full(&m, rb, p);
        xf.to_screen(wp[0] as f32, wp[1] as f32)
    };

    for r in 0..rows {
        let v = r as f64 / (rows - 1) as f64;
        for c in 0..(cols - 1) {
            let u0 = c as f64 / (cols - 1) as f64;
            let u1 = (c + 1) as f64 / (cols - 1) as f64;
            let mut prev = sample_uv(u0, v);
            for s in 1..=segs {
                let u = u0 + (u1 - u0) * (s as f64 / segs as f64);
                let next = sample_uv(u, v);
                painter.line_segment([prev, next], line_stroke);
                prev = next;
            }
        }
    }
    for c in 0..cols {
        let u = c as f64 / (cols - 1) as f64;
        for r in 0..(rows - 1) {
            let v0 = r as f64 / (rows - 1) as f64;
            let v1 = (r + 1) as f64 / (rows - 1) as f64;
            let mut prev = sample_uv(u, v0);
            for s in 1..=segs {
                let v = v0 + (v1 - v0) * (s as f64 / segs as f64);
                let next = sample_uv(u, v);
                painter.line_segment([prev, next], line_stroke);
                prev = next;
            }
        }
    }

    for r in 0..rows {
        for c in 0..cols {
            let Some(p) = mesh_node_doc_pos(data, rb, r, c) else { continue };
            let sp = xf.to_screen(p[0] as f32, p[1] as f32);
            let is_sel = selected.contains(&(r, c));
            let fill = if is_sel { accent } else { Color32::WHITE };
            let radius = if is_sel { 4.8 } else { 3.8 };
            painter.circle_filled(sp, radius, fill);
            painter.circle_stroke(sp, radius, Stroke::new(1.3, accent));
        }
    }
}

/// Resets mesh lattice nodes to an identity grid (`cols x rows`).
pub fn reset_mesh(data: &mut SfxData) {
    data.mesh = SfxMeshLattice {
        cols: data.mesh.cols,
        rows: data.mesh.rows,
        points: SfxMeshLattice::rest_grid(data.mesh.cols, data.mesh.rows),
        interpolation: data.mesh.interpolation,
        link_h: data.mesh.link_h,
        link_v: data.mesh.link_v,
    };
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use egui::{Modifiers, Rect, pos2};
    use photocraft_doc::sfx::{SfxData, SfxWarpMode};
    use photocraft_doc::{ColorMode, Document, Layer, LayerContent, PixelFormat, SampleType, Size};
    use photocraft_raster::Surface;

    use super::*;

    fn sfx_doc(mut data: SfxData) -> (Document, LayerId) {
        data.validate();
        let mut doc = Document::new("sfx", Size::new(512, 512), ColorMode::Rgb, SampleType::U8);
        let mut layer = Layer::new("SFX", LayerContent::Raster(Surface::new(PixelFormat::RGBA8)));
        layer.sfx = Some(Arc::new(data));
        let id = doc.insert_above(None, layer);
        (doc, id)
    }

    fn view_for(doc: &Document, layer_id: LayerId) -> SfxView<'_> {
        SfxView {
            doc_id: Some(BridgeDocId(doc.id.0)),
            doc: Some(doc),
            active_layer: Some(layer_id),
            tool_is_sfx: true,
            panel_open: true,
            view_xform: CanvasViewXform {
                rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(512.0, 512.0)),
                zoom: 1.0,
                center: [256.0, 256.0],
                flip: false,
                rotation: 0.0,
                aspect: 1.0,
            },
        }
    }

    #[test]
    fn free_transform_move_and_escape_restore() {
        let sfx = SfxData { origin: [100.0, 100.0], ..SfxData::default() };
        let (doc, id) = sfx_doc(sfx.clone());
        let mut gizmo = SfxGizmoState::default();
        let geom = gizmo.ensure_geom(&sfx, 72.0).clone();
        let center = [(geom.rest_box[0] + geom.rest_box[2]) * 0.5, (geom.rest_box[1] + geom.rest_box[3]) * 0.5];
        let view = view_for(&doc, id);

        let down = pointer(&mut gizmo, &view, CanvasToolEvent::Down { x: center[0], y: center[1], pressure: 1.0 }, Modifiers::NONE);
        assert!(down.handled);

        let moved = pointer(&mut gizmo, &view, CanvasToolEvent::Move { x: center[0] + 25.0, y: center[1] - 10.0, pressure: 1.0 }, Modifiers::NONE);
        assert!(moved.handled);
        let updated = moved.updated_sfx.expect("live move update");
        assert!((updated.origin[0] - 125.0).abs() < 1e-6);
        assert!((updated.origin[1] - 90.0).abs() < 1e-6);
        assert_eq!(moved.commands[0].id, "sfx.update");

        let cancelled = cancel_drag(&mut gizmo, &view);
        assert!(cancelled.handled);
        assert!(!gizmo.has_active_drag());
        let restored = cancelled.updated_sfx.expect("restore initial");
        assert_eq!(restored.origin, [100.0, 100.0]);
    }

    #[test]
    fn mesh_marquee_box_selects_and_moves_multiple_nodes() {
        let sfx = SfxData { origin: [100.0, 100.0], warp_mode: SfxWarpMode::MeshLattice, ..SfxData::default() };
        let (doc, id) = sfx_doc(sfx.clone());
        let mut gizmo = SfxGizmoState::default();
        let geom = gizmo.ensure_geom(&sfx, 72.0).clone();
        let view = view_for(&doc, id);

        let rb = geom.rest_box;
        // Pick an empty start point above-left of node (0, 0) beyond hit radius, drag over row 0 cols 0..=1.
        let p_n00 = mesh_node_doc_pos(&sfx, rb, 0, 0).unwrap();
        let p_n01 = mesh_node_doc_pos(&sfx, rb, 0, 1).unwrap();
        let start = [p_n00[0] - 25.0, p_n00[1] - 25.0];
        let end = [p_n01[0] + 10.0, p_n01[1] + 10.0];

        assert!(pointer(&mut gizmo, &view, CanvasToolEvent::Down { x: start[0], y: start[1], pressure: 1.0 }, Modifiers::NONE).handled);
        assert!(pointer(&mut gizmo, &view, CanvasToolEvent::Move { x: end[0], y: end[1], pressure: 1.0 }, Modifiers::NONE).handled);
        assert!(pointer(&mut gizmo, &view, CanvasToolEvent::Up { x: end[0], y: end[1] }, Modifiers::NONE).handled);
        assert_eq!(gizmo.selected_nodes, vec![(0, 0), (0, 1)]);

        // Now drag node (0, 0): both selected nodes must displace equally.
        assert!(pointer(&mut gizmo, &view, CanvasToolEvent::Down { x: p_n00[0], y: p_n00[1], pressure: 1.0 }, Modifiers::NONE).handled);
        let moved = pointer(&mut gizmo, &view, CanvasToolEvent::Move { x: p_n00[0], y: p_n00[1] - 20.0, pressure: 1.0 }, Modifiers::NONE);
        let u = moved.updated_sfx.unwrap();
        assert!(u.mesh.points[0][1] < -0.01);
        assert!(u.mesh.points[1][1] < -0.01);
        assert!((u.mesh.points[0][1] - u.mesh.points[1][1]).abs() < 1e-6);
    }

    #[test]
    fn per_glyph_drag_creates_override_entry() {
        let sfx = SfxData { text: "AB".to_owned(), origin: [100.0, 100.0], warp_mode: SfxWarpMode::PerGlyph, ..SfxData::default() };
        let (doc, id) = sfx_doc(sfx.clone());
        let mut gizmo = SfxGizmoState::default();
        let geom = gizmo.ensure_geom(&sfx, 72.0).clone();
        let g0 = &geom.glyphs[0];
        let gc = g0.warped_center;
        let view = view_for(&doc, id);

        assert!(pointer(&mut gizmo, &view, CanvasToolEvent::Down { x: gc[0], y: gc[1], pressure: 1.0 }, Modifiers::NONE).handled);
        let moved = pointer(&mut gizmo, &view, CanvasToolEvent::Move { x: gc[0] + 12.0, y: gc[1] + 6.0, pressure: 1.0 }, Modifiers::NONE);
        let u = moved.updated_sfx.unwrap();
        assert_eq!(u.per_glyph.len(), 1);
        assert!((u.per_glyph[0].dx - 12.0).abs() < 1e-6);
        assert!((u.per_glyph[0].dy - 6.0).abs() < 1e-6);
    }

    #[test]
    fn liquify_brush_appends_stroke() {
        let sfx = SfxData { origin: [100.0, 100.0], warp_mode: SfxWarpMode::LiquifyField, ..SfxData::default() };
        let (doc, id) = sfx_doc(sfx.clone());
        let mut gizmo = SfxGizmoState::default();
        let geom = gizmo.ensure_geom(&sfx, 72.0).clone();
        let c = [(geom.rest_box[0] + geom.rest_box[2]) * 0.5, (geom.rest_box[1] + geom.rest_box[3]) * 0.5];
        let view = view_for(&doc, id);

        assert!(pointer(&mut gizmo, &view, CanvasToolEvent::Down { x: c[0], y: c[1], pressure: 1.0 }, Modifiers::NONE).handled);
        let moved = pointer(&mut gizmo, &view, CanvasToolEvent::Move { x: c[0] + 15.0, y: c[1] - 8.0, pressure: 1.0 }, Modifiers::NONE);
        let u = moved.updated_sfx.unwrap();
        assert_eq!(u.liquify_strokes.len(), 1);
        assert!((u.liquify_strokes[0].delta_px[0] - 15.0).abs() < 1e-6);
    }
}
