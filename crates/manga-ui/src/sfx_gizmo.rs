//! 2D interactive canvas gizmo for `SfxData`:
//! - Non-affine `FT 4-Corner` (with frozen inverse homography during drag + `Shift` trapezoid)
//! - `Catmull-Rom` arc spline (drag points, click arc to add point, `Alt+click` point to delete)
//! - Vector `☀ Shadow` direction/distance ray handle
//! - `PerGlyph` 3-tool suite (`Move`, `Skew`, `FT 4-Corner`) with persistent rotation stem
//! - `MeshLattice` + marquee box select & `LiquifyField` brush
//!
//! All document mutations are emitted as `OutCommand` (`"sfx.update"` with `"coalesce"`) so dragging
//! collapses into a single history step and `Escape` restores the pre-drag snapshot cleanly.

use std::sync::atomic::{AtomicU8, Ordering};

use egui::{Color32, CursorIcon, Pos2, Rect, Stroke, pos2, vec2};
use photocraft_doc::sfx::{
    MAX_SFX_LIQUIFY_STROKES, MAX_SFX_SPLINE_POINTS, SfxData, SfxFreeTransform, SfxGlyphTransform, SfxLiquifyBrush, SfxMeshLattice, SfxWarpMode,
};
use photocraft_doc::{Document, LayerId};
use photocraft_manga_core::sfx_geom::{
    PreparedSfx, apply_free_transform_point, apply_homography, build_sfx_geometry, eval_mesh_uv, free_transform_homography,
    invert_free_transform_point, mesh_node_doc_pos, move_mesh_node_linked, sample_catmull_rom, spline_control_doc_pos,
};
use photocraft_text::TextEngine;
use serde_json::json;

use crate::sfx_panel::SfxGlyphTool;
use crate::theme::Tokens;
use crate::view::{CanvasToolEvent, CanvasViewXform};
use crate::{BridgeDocId, OutCommand};

const HANDLE_HIT_PX: f32 = 10.0;
const ARC_HIT_PX: f32 = 9.0;
const ROTATE_STEM_PX: f32 = 24.0;
const MIN_LIQUIFY_STEP_PX: f64 = 12.0;

const COLOR_CYAN: Color32 = Color32::from_rgb(34, 211, 238);
const COLOR_AMBER: Color32 = Color32::from_rgb(245, 158, 11);
const COLOR_SUN: Color32 = Color32::from_rgb(251, 191, 36);

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
    Corner(usize),
    Top,
    Right,
    Bottom,
    Left,
    Rotate,
    Body,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlyphHandle {
    Body(usize),
    Rotate(usize),
    Scale(usize),
    SkewX(usize),
    SkewY(usize),
    Corner(usize, usize),
}

#[derive(Clone, Debug)]
enum DragMode {
    Free(FreeHandle),
    SplinePoint { index: usize },
    VectorShadow,
    Glyph { handle: GlyphHandle, arc_angle_rad: f64, rest_center: [f64; 2] },
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
    /// Frozen inverse homography captured on pointer-down (prevents projective drift/NaN during drag).
    init_inv_h: [f64; 9],
    mode: DragMode,
}

fn glyph_tool_to_u8(tool: SfxGlyphTool) -> u8 {
    match tool {
        SfxGlyphTool::Move => 0,
        SfxGlyphTool::Skew => 1,
        SfxGlyphTool::FreeTransform => 2,
    }
}

fn glyph_tool_from_u8(v: u8) -> SfxGlyphTool {
    match v {
        1 => SfxGlyphTool::Skew,
        2 => SfxGlyphTool::FreeTransform,
        _ => SfxGlyphTool::Move,
    }
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
    glyph_tool_code: AtomicU8,
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
            glyph_tool_code: AtomicU8::new(0),
        }
    }
}

impl SfxGizmoState {
    pub fn has_active_drag(&self) -> bool {
        self.drag.is_some()
    }

    pub fn current_glyph_tool(&self) -> SfxGlyphTool {
        glyph_tool_from_u8(self.glyph_tool_code.load(Ordering::Relaxed))
    }

    pub fn set_glyph_tool(&self, tool: SfxGlyphTool) {
        self.glyph_tool_code.store(glyph_tool_to_u8(tool), Ordering::Relaxed);
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
        ([rb[0], rb[1]], FreeHandle::Corner(0)),
        ([cx, rb[1]], FreeHandle::Top),
        ([rb[2], rb[1]], FreeHandle::Corner(1)),
        ([rb[2], cy], FreeHandle::Right),
        ([rb[2], rb[3]], FreeHandle::Corner(2)),
        ([cx, rb[3]], FreeHandle::Bottom),
        ([rb[0], rb[3]], FreeHandle::Corner(3)),
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

fn shadow_center_and_knob_doc(data: &SfxData, rb: [f64; 4]) -> ([f64; 2], [f64; 2]) {
    let center = apply_free_transform_point(&data.free_transform, rb, [(rb[0] + rb[2]) * 0.5, (rb[1] + rb[3]) * 0.5]);
    let rad = f64::from(data.vector_shadow.angle_deg).to_radians();
    let vis_dist = f64::from(data.vector_shadow.distance_px).max(26.0);
    let knob = [center[0] + rad.cos() * vis_dist, center[1] + rad.sin() * vis_dist];
    (center, knob)
}

fn hit_shadow_knob(data: &SfxData, rb: [f64; 4], xf: &CanvasViewXform, at_doc: [f64; 2]) -> bool {
    if !data.vector_shadow.enabled {
        return false;
    }
    let (_, knob) = shadow_center_and_knob_doc(data, rb);
    let p_scr = xf.to_screen(at_doc[0] as f32, at_doc[1] as f32);
    let k_scr = xf.to_screen(knob[0] as f32, knob[1] as f32);
    k_scr.distance(p_scr) <= HANDLE_HIT_PX + 2.0
}

fn hit_spline_point(data: &SfxData, rb: [f64; 4], xf: &CanvasViewXform, at_doc: [f64; 2]) -> Option<usize> {
    if !data.spline.show_arc || data.spline.points.is_empty() {
        return None;
    }
    let p_scr = xf.to_screen(at_doc[0] as f32, at_doc[1] as f32);
    let mut best: Option<(f32, usize)> = None;
    for (i, &lp) in data.spline.points.iter().enumerate() {
        let dp = spline_control_doc_pos(data, rb, lp);
        let d = xf.to_screen(dp[0] as f32, dp[1] as f32).distance(p_scr);
        if d <= HANDLE_HIT_PX + 1.0 && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, i));
        }
    }
    best.map(|(_, i)| i)
}

fn dist_pt_to_seg_scr(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let l2 = ab.length_sq();
    if l2 < 1e-6 {
        return p.distance(a);
    }
    let t = ((p - a).dot(ab) / l2).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

/// Returns the spline segment index (`0..points.len()-1`) if the cursor clicks near the Catmull-Rom curve.
fn hit_spline_curve(data: &SfxData, rb: [f64; 4], xf: &CanvasViewXform, at_doc: [f64; 2]) -> Option<usize> {
    if !data.spline.show_arc || data.spline.points.len() < 2 || data.spline.points.len() >= MAX_SFX_SPLINE_POINTS {
        return None;
    }
    let p_scr = xf.to_screen(at_doc[0] as f32, at_doc[1] as f32);
    let segs = data.spline.points.len() - 1;
    let steps = (segs * 12).clamp(16, 128);
    let mut best: Option<(f32, usize)> = None;
    let mut prev_scr = {
        let s0 = sample_catmull_rom(&data.spline.points, 0.0);
        let d0 = spline_control_doc_pos(data, rb, [s0.lx, s0.ly]);
        xf.to_screen(d0[0] as f32, d0[1] as f32)
    };
    for st in 1..=steps {
        let t = st as f64 / steps as f64;
        let sp = sample_catmull_rom(&data.spline.points, t);
        let dp = spline_control_doc_pos(data, rb, [sp.lx, sp.ly]);
        let cur_scr = xf.to_screen(dp[0] as f32, dp[1] as f32);
        let d = dist_pt_to_seg_scr(p_scr, prev_scr, cur_scr);
        if d <= ARC_HIT_PX && best.is_none_or(|(bd, _)| d < bd) {
            let t_mid = (st as f64 - 0.5) / steps as f64;
            let seg_idx = ((t_mid * segs as f64).floor() as usize).min(segs - 1);
            best = Some((d, seg_idx));
        }
        prev_scr = cur_scr;
    }
    best.map(|(_, seg)| seg)
}

fn point_in_rect(r: [f64; 4], p: [f64; 2]) -> bool {
    p[0] >= r[0] && p[0] <= r[2] && p[1] >= r[1] && p[1] <= r[3]
}

fn point_in_quad(quad: [[f64; 2]; 4], p: [f64; 2]) -> bool {
    let mut inside = false;
    let mut j = 3usize;
    for i in 0..4 {
        let xi = quad[i][0];
        let yi = quad[i][1];
        let xj = quad[j][0];
        let yj = quad[j][1];
        let denom = yj - yi;
        let intersect = ((yi > p[1]) != (yj > p[1])) && denom.abs() > 1e-9 && (p[0] < (xj - xi) * (p[1] - yi) / denom + xi);
        if intersect {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn hit_free_transform(data: &SfxData, rb: [f64; 4], world_corners: [[f64; 2]; 4], xf: &CanvasViewXform, at_doc: [f64; 2]) -> Option<FreeHandle> {
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
    if point_in_quad(world_corners, at_doc)
        || invert_free_transform_point(&data.free_transform, rb, at_doc).is_some_and(|rest_pt| point_in_rect(rb, rest_pt))
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

struct GlyphScreenGizmo {
    corners: [Pos2; 4],
    top_mid: Pos2,
    rot_pos: Pos2,
    skew_x_pos: Pos2,
    skew_y_pos: Pos2,
    scale_pos: Pos2,
}

fn glyph_screen_gizmo(quad_doc: [[f64; 2]; 4], xf: &CanvasViewXform) -> GlyphScreenGizmo {
    let corners = [
        xf.to_screen(quad_doc[0][0] as f32, quad_doc[0][1] as f32),
        xf.to_screen(quad_doc[1][0] as f32, quad_doc[1][1] as f32),
        xf.to_screen(quad_doc[2][0] as f32, quad_doc[2][1] as f32),
        xf.to_screen(quad_doc[3][0] as f32, quad_doc[3][1] as f32),
    ];
    let top_mid = pos2((corners[0].x + corners[1].x) * 0.5, (corners[0].y + corners[1].y) * 0.5);
    let right_mid = pos2((corners[1].x + corners[2].x) * 0.5, (corners[1].y + corners[2].y) * 0.5);
    let center = pos2(
        (corners[0].x + corners[1].x + corners[2].x + corners[3].x) * 0.25,
        (corners[0].y + corners[1].y + corners[2].y + corners[3].y) * 0.25,
    );
    let mut up_dir = top_mid - center;
    let up_len = up_dir.length();
    if up_len > 1e-3 {
        up_dir /= up_len;
    } else {
        up_dir = vec2(0.0, -1.0);
    }
    let rot_pos = top_mid + up_dir * (ROTATE_STEM_PX * 0.85);
    GlyphScreenGizmo {
        corners,
        top_mid,
        rot_pos,
        skew_x_pos: top_mid,
        skew_y_pos: right_mid,
        scale_pos: corners[2],
    }
}

fn hit_glyph(geom: &PreparedSfx, selected: Option<usize>, tool: SfxGlyphTool, xf: &CanvasViewXform, at_doc: [f64; 2]) -> Option<GlyphHandle> {
    let p_scr = xf.to_screen(at_doc[0] as f32, at_doc[1] as f32);
    if let Some(idx) = selected
        && let Some(g) = geom.glyphs.iter().find(|g| g.glyph_index == idx)
    {
        let gs = glyph_screen_gizmo(g.quad_doc, xf);
        // Rotate handle is always available across Move, Skew, and FT (4-Crn).
        if gs.rot_pos.distance(p_scr) <= HANDLE_HIT_PX {
            return Some(GlyphHandle::Rotate(idx));
        }
        match tool {
            SfxGlyphTool::FreeTransform => {
                for (ci, &cp) in gs.corners.iter().enumerate() {
                    if cp.distance(p_scr) <= HANDLE_HIT_PX {
                        return Some(GlyphHandle::Corner(idx, ci));
                    }
                }
            }
            SfxGlyphTool::Skew => {
                if gs.skew_x_pos.distance(p_scr) <= HANDLE_HIT_PX {
                    return Some(GlyphHandle::SkewX(idx));
                }
                if gs.skew_y_pos.distance(p_scr) <= HANDLE_HIT_PX {
                    return Some(GlyphHandle::SkewY(idx));
                }
            }
            SfxGlyphTool::Move => {
                if gs.scale_pos.distance(p_scr) <= HANDLE_HIT_PX {
                    return Some(GlyphHandle::Scale(idx));
                }
            }
        }
    }
    for g in geom.glyphs.iter().rev() {
        let pad_doc = 4.0 / f64::from(xf.zoom.max(0.05));
        let expanded = [g.warped_bounds[0] - pad_doc, g.warped_bounds[1] - pad_doc, g.warped_bounds[2] + pad_doc, g.warped_bounds[3] + pad_doc];
        if point_in_quad(g.quad_doc, at_doc) || point_in_rect(expanded, at_doc) {
            return Some(GlyphHandle::Body(g.glyph_index));
        }
    }
    None
}

fn to_pre_ft(ft: &SfxFreeTransform, rb: [f64; 4], p: [f64; 2]) -> [f64; 2] {
    invert_free_transform_point(ft, rb, p).unwrap_or(p)
}

fn edge_delta_signs(kind: FreeHandle) -> (f64, f64) {
    match kind {
        FreeHandle::Top => (0.0, -1.0),
        FreeHandle::Right => (1.0, 0.0),
        FreeHandle::Bottom => (0.0, 1.0),
        FreeHandle::Left => (-1.0, 0.0),
        FreeHandle::Corner(0) => (-1.0, -1.0),
        FreeHandle::Corner(1) => (1.0, -1.0),
        FreeHandle::Corner(2) => (1.0, 1.0),
        FreeHandle::Corner(3) => (-1.0, 1.0),
        FreeHandle::Corner(_) | FreeHandle::Rotate | FreeHandle::Body => (0.0, 0.0),
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
        FreeHandle::Corner(c_idx) if c_idx < 4 && !mods.ctrl => {
            // Non-affine 4-Corner FT drag using frozen pre-corner inverse (zero feedback drift / NaN).
            let ft_no_corners = SfxFreeTransform { corners: [[0.0; 2]; 4], ..drag.initial_sfx.free_transform };
            let p0 = invert_free_transform_point(&ft_no_corners, rb, drag.start_doc).unwrap_or(drag.start_doc);
            let p1 = invert_free_transform_point(&ft_no_corners, rb, at_doc).unwrap_or(at_doc);
            let dp = [p1[0] - p0[0], p1[1] - p0[1]];
            let base = drag.initial_sfx.free_transform.corners;
            next.free_transform.corners = base;
            next.free_transform.corners[c_idx][0] = base[c_idx][0] + dp[0];
            next.free_transform.corners[c_idx][1] = base[c_idx][1] + dp[1];
            if mods.shift {
                // Symmetric trapezoid perspective on paired top/bottom corner.
                let pair = match c_idx {
                    0 => 1,
                    1 => 0,
                    2 => 3,
                    _ => 2,
                };
                next.free_transform.corners[pair][0] = base[pair][0] - dp[0];
                next.free_transform.corners[pair][1] = base[pair][1] + dp[1];
            }
        }
        edge_or_ctrl_corner => {
            let (s, c) = (-drag.initial_sfx.free_transform.rotation_deg).to_radians().sin_cos();
            let rot = |p: [f64; 2]| -> [f64; 2] {
                let rx = p[0] - center[0];
                let ry = p[1] - center[1];
                [rx * c - ry * s, rx * s + ry * c]
            };
            let p0 = rot(drag.start_doc);
            let p1 = rot(at_doc);
            let d = [p1[0] - p0[0], p1[1] - p0[1]];
            let (sx_sign, sy_sign) = edge_delta_signs(edge_or_ctrl_corner);
            let ft0 = drag.initial_sfx.free_transform;

            if mods.ctrl {
                match edge_or_ctrl_corner {
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
                    FreeHandle::Corner(_) => {
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

fn apply_spline_drag(drag: &ActiveDrag, at_doc: [f64; 2], next: &mut SfxData) {
    let DragMode::SplinePoint { index } = drag.mode else { return };
    let cx = (drag.rest_box[0] + drag.rest_box[2]) * 0.5;
    let cy = (drag.rest_box[1] + drag.rest_box[3]) * 0.5;
    // Frozen inverse homography ensures arc points never explode to NaN under strong 4-Corner FT.
    let local = apply_homography(&drag.init_inv_h, at_doc);
    if let Some(pt) = next.spline.points.get_mut(index) {
        pt[0] = local[0] - cx;
        pt[1] = local[1] - cy;
    }
    next.validate();
}

fn apply_shadow_drag(drag: &ActiveDrag, at_doc: [f64; 2], mods: egui::Modifiers, next: &mut SfxData) {
    let center = apply_free_transform_point(&drag.initial_sfx.free_transform, drag.rest_box, [
        (drag.rest_box[0] + drag.rest_box[2]) * 0.5,
        (drag.rest_box[1] + drag.rest_box[3]) * 0.5,
    ]);
    let dx = at_doc[0] - center[0];
    let dy = at_doc[1] - center[1];
    let mut deg = dy.atan2(dx).to_degrees();
    if mods.shift {
        deg = (deg / 15.0).round() * 15.0;
    }
    let dist = dx.hypot(dy).clamp(0.0, 256.0);
    next.vector_shadow.enabled = true;
    next.vector_shadow.angle_deg = deg as f32;
    next.vector_shadow.distance_px = dist as f32;
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
    let DragMode::Glyph { handle, arc_angle_rad, rest_center } = drag.mode else { return };
    // Frozen inverse homography maps document pointer back to pre-FT layout space without drift.
    let p0 = apply_homography(&drag.init_inv_h, drag.start_doc);
    let p1 = apply_homography(&drag.init_inv_h, at_doc);

    let base_for = |idx: usize| -> SfxGlyphTransform {
        drag.initial_sfx
            .per_glyph
            .iter()
            .find(|g| g.glyph_index == idx)
            .copied()
            .unwrap_or(SfxGlyphTransform { glyph_index: idx, ..SfxGlyphTransform::default() })
    };

    match handle {
        GlyphHandle::Body(idx) => {
            let base = base_for(idx);
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
            let base = base_for(idx);
            let c = [rest_center[0] + base.dx, rest_center[1] + base.dy];
            let a0 = (p0[1] - c[1]).atan2(p0[0] - c[0]);
            let a1 = (p1[1] - c[1]).atan2(p1[0] - c[0]);
            let mut deg = base.rotation_deg + (a1 - a0).to_degrees();
            if mods.shift {
                deg = (deg / 15.0).round() * 15.0;
            }
            find_or_insert_glyph_mut(next, idx).rotation_deg = deg;
        }
        GlyphHandle::Scale(idx) => {
            let base = base_for(idx);
            let dy = p1[1] - p0[1];
            let dx = p1[0] - p0[0];
            let factor = (1.0 + (dx + dy) / 64.0).clamp(0.2, 6.0);
            let entry = find_or_insert_glyph_mut(next, idx);
            entry.scale_x = (base.scale_x * factor).clamp(0.1, 8.0);
            entry.scale_y = if mods.shift { entry.scale_x } else { (base.scale_y * (1.0 + dy / 64.0)).clamp(0.1, 8.0) };
        }
        GlyphHandle::SkewX(idx) => {
            let base = base_for(idx);
            let dx = p1[0] - p0[0];
            find_or_insert_glyph_mut(next, idx).skew_x_deg = (base.skew_x_deg + dx * 0.45).clamp(-60.0, 60.0);
        }
        GlyphHandle::SkewY(idx) => {
            let base = base_for(idx);
            let dy = p1[1] - p0[1];
            find_or_insert_glyph_mut(next, idx).skew_y_deg = (base.skew_y_deg + dy * 0.45).clamp(-60.0, 60.0);
        }
        GlyphHandle::Corner(idx, c_idx) if c_idx < 4 => {
            let base = base_for(idx);
            let dlx = p1[0] - p0[0];
            let dly = p1[1] - p0[1];
            let total_angle = arc_angle_rad + base.rotation_deg.to_radians();
            let (s, c) = (-total_angle).sin_cos();
            let lx = (dlx * c - dly * s) / base.scale_x.abs().max(0.1);
            let ly = (dlx * s + dly * c) / base.scale_y.abs().max(0.1);
            let entry = find_or_insert_glyph_mut(next, idx);
            entry.corners = base.corners;
            entry.corners[c_idx][0] = base.corners[c_idx][0] + lx;
            entry.corners[c_idx][1] = base.corners[c_idx][1] + ly;
        }
        GlyphHandle::Corner(_, _) => {}
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
            let mut sfx = sfx_ref.clone();
            let doc_id = view.doc_id.unwrap_or(BridgeDocId(0));
            let dpi = doc_dpi(view);
            let geom = gizmo.ensure_geom(&sfx, dpi);
            if geom.rest_box[2] <= geom.rest_box[0] && geom.rest_box[3] <= geom.rest_box[1] && sfx.warp_mode != SfxWarpMode::FreeTransform {
                return out;
            }
            let rb = geom.rest_box;
            let xf = &view.view_xform;
            let (_, init_inv_h) = free_transform_homography(&sfx.free_transform, rb);
            let coalesce = format!("sfx-gizmo:{}:{}:{:?}", doc_id.0, layer_id.0, sfx.warp_mode);

            // 1. Vector `☀ Shadow` knob has top hit priority in FreeTransform / PerGlyph.
            if matches!(sfx.warp_mode, SfxWarpMode::FreeTransform | SfxWarpMode::PerGlyph) && hit_shadow_knob(&sfx, rb, xf, [x, y]) {
                gizmo.drag = Some(ActiveDrag {
                    doc_id,
                    layer_id,
                    coalesce: format!("sfx-shadow:{}:{}", doc_id.0, layer_id.0),
                    start_doc: [x, y],
                    reported_sfx: sfx.clone(),
                    initial_sfx: sfx,
                    rest_box: rb,
                    init_inv_h,
                    mode: DragMode::VectorShadow,
                });
                out.handled = true;
                return out;
            }

            let mode = match sfx.warp_mode {
                SfxWarpMode::FreeTransform => {
                    if let Some(sp_idx) = hit_spline_point(&sfx, rb, xf, [x, y]) {
                        if mods.alt && sfx.spline.points.len() > 2 {
                            sfx.spline.points.remove(sp_idx);
                            sfx.validate();
                            out.commands.push(OutCommand::new(
                                "sfx.update",
                                json!({ "layer": layer_id.0, "sfx": sfx, "coalesce": coalesce }),
                            ));
                            out.updated_sfx = Some(sfx);
                            out.handled = true;
                            return out;
                        }
                        DragMode::SplinePoint { index: sp_idx }
                    } else if let Some(h) = hit_free_transform(&sfx, rb, geom.world_corners, xf, [x, y]) {
                        // Allow clicking directly on the Catmull-Rom curve to insert a point when not on a frame handle.
                        if h == FreeHandle::Body
                            && let Some(seg_idx) = hit_spline_curve(&sfx, rb, xf, [x, y])
                        {
                            let cx = (rb[0] + rb[2]) * 0.5;
                            let cy = (rb[1] + rb[3]) * 0.5;
                            let local = apply_homography(&init_inv_h, [x, y]);
                            let insert_at = seg_idx + 1;
                            sfx.spline.points.insert(insert_at, [local[0] - cx, local[1] - cy]);
                            sfx.validate();
                            out.commands.push(OutCommand::new(
                                "sfx.update",
                                json!({ "layer": layer_id.0, "sfx": sfx, "coalesce": coalesce }),
                            ));
                            out.updated_sfx = Some(sfx.clone());
                            DragMode::SplinePoint { index: insert_at }
                        } else {
                            DragMode::Free(h)
                        }
                    } else if let Some(seg_idx) = hit_spline_curve(&sfx, rb, xf, [x, y]) {
                        let cx = (rb[0] + rb[2]) * 0.5;
                        let cy = (rb[1] + rb[3]) * 0.5;
                        let local = apply_homography(&init_inv_h, [x, y]);
                        let insert_at = seg_idx + 1;
                        sfx.spline.points.insert(insert_at, [local[0] - cx, local[1] - cy]);
                        sfx.validate();
                        out.commands.push(OutCommand::new(
                            "sfx.update",
                            json!({ "layer": layer_id.0, "sfx": sfx, "coalesce": coalesce }),
                        ));
                        out.updated_sfx = Some(sfx.clone());
                        DragMode::SplinePoint { index: insert_at }
                    } else {
                        return out;
                    }
                }
                SfxWarpMode::PerGlyph => {
                    let tool = gizmo.current_glyph_tool();
                    let Some(gh) = hit_glyph(&geom, gizmo.selected_glyph, tool, xf, [x, y]) else {
                        gizmo.selected_glyph = None;
                        return out;
                    };
                    let g_idx = match gh {
                        GlyphHandle::Body(i)
                        | GlyphHandle::Rotate(i)
                        | GlyphHandle::Scale(i)
                        | GlyphHandle::SkewX(i)
                        | GlyphHandle::SkewY(i)
                        | GlyphHandle::Corner(i, _) => i,
                    };
                    gizmo.selected_glyph = Some(g_idx);
                    let (arc_angle_rad, rest_center) = geom
                        .glyphs
                        .iter()
                        .find(|g| g.glyph_index == g_idx)
                        .map_or((0.0, [(rb[0] + rb[2]) * 0.5, (rb[1] + rb[3]) * 0.5]), |g| (g.arc_angle_rad, g.rest_center));
                    DragMode::Glyph { handle: gh, arc_angle_rad, rest_center }
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

            gizmo.drag = Some(ActiveDrag {
                doc_id,
                layer_id,
                coalesce,
                start_doc: [x, y],
                reported_sfx: sfx.clone(),
                initial_sfx: sfx,
                rest_box: rb,
                init_inv_h,
                mode,
            });
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
                DragMode::SplinePoint { .. } => {
                    next = drag.initial_sfx.clone();
                    apply_spline_drag(&drag, [x, y], &mut next);
                }
                DragMode::VectorShadow => {
                    next = drag.initial_sfx.clone();
                    apply_shadow_drag(&drag, [x, y], mods, &mut next);
                }
                DragMode::Glyph { .. } => {
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
                DragMode::SplinePoint { .. } => {
                    next = drag.initial_sfx.clone();
                    apply_spline_drag(&drag, [x, y], &mut next);
                    push_update(&mut drag, next, &mut out);
                }
                DragMode::VectorShadow => {
                    next = drag.initial_sfx.clone();
                    apply_shadow_drag(&drag, [x, y], mods, &mut next);
                    push_update(&mut drag, next, &mut out);
                }
                DragMode::Glyph { .. } => {
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

/// Returns hover cursor feedback over SFX handles, arc points, shadow knob, or brush circle.
pub fn hover_cursor(gizmo: &SfxGizmoState, view: &SfxView<'_>, cursor_scr: Pos2) -> Option<CursorIcon> {
    if !is_active_on_canvas(gizmo, view) {
        return None;
    }
    let at_doc = view.view_xform.to_doc(cursor_scr);
    if let Some(drag) = &gizmo.drag {
        return Some(match drag.mode {
            DragMode::Free(FreeHandle::Rotate) | DragMode::Glyph { handle: GlyphHandle::Rotate(_), .. } => CursorIcon::Alias,
            DragMode::Free(_) | DragMode::Glyph { .. } | DragMode::MeshNode { .. } | DragMode::SplinePoint { .. } | DragMode::VectorShadow => {
                CursorIcon::Grabbing
            }
            DragMode::MeshMarquee { .. } | DragMode::Liquify { .. } => CursorIcon::Crosshair,
        });
    }
    let (_, sfx_ref) = active_sfx_target(view)?;
    let sfx = sfx_ref.clone();
    let dpi = doc_dpi(view);
    let geom = gizmo.ensure_geom(&sfx, dpi);
    let rb = geom.rest_box;
    let xf = &view.view_xform;

    if matches!(sfx.warp_mode, SfxWarpMode::FreeTransform | SfxWarpMode::PerGlyph) && hit_shadow_knob(&sfx, rb, xf, at_doc) {
        return Some(CursorIcon::Grab);
    }

    match sfx.warp_mode {
        SfxWarpMode::FreeTransform => {
            if hit_spline_point(&sfx, rb, xf, at_doc).is_some() {
                return Some(CursorIcon::Grab);
            }
            if let Some(h) = hit_free_transform(&sfx, rb, geom.world_corners, xf, at_doc) {
                if h == FreeHandle::Body && hit_spline_curve(&sfx, rb, xf, at_doc).is_some() {
                    return Some(CursorIcon::Crosshair);
                }
                return Some(match h {
                    FreeHandle::Body => CursorIcon::Move,
                    FreeHandle::Rotate => CursorIcon::Alias,
                    FreeHandle::Top | FreeHandle::Bottom => CursorIcon::ResizeVertical,
                    FreeHandle::Left | FreeHandle::Right => CursorIcon::ResizeHorizontal,
                    FreeHandle::Corner(0) | FreeHandle::Corner(2) => CursorIcon::ResizeNwSe,
                    FreeHandle::Corner(_) => CursorIcon::ResizeNeSw,
                });
            }
            if hit_spline_curve(&sfx, rb, xf, at_doc).is_some() {
                return Some(CursorIcon::Crosshair);
            }
            None
        }
        SfxWarpMode::PerGlyph => match hit_glyph(&geom, gizmo.selected_glyph, gizmo.current_glyph_tool(), xf, at_doc)? {
            GlyphHandle::Body(_) => Some(CursorIcon::Move),
            GlyphHandle::Rotate(_) => Some(CursorIcon::Alias),
            GlyphHandle::Scale(_) | GlyphHandle::Corner(_, _) => Some(CursorIcon::ResizeNwSe),
            GlyphHandle::SkewX(_) => Some(CursorIcon::ResizeHorizontal),
            GlyphHandle::SkewY(_) => Some(CursorIcon::ResizeVertical),
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
        SfxWarpMode::LiquifyField => view.tool_is_sfx.then_some(CursorIcon::Crosshair),
    }
}

/// Draws active SFX gizmos onto the canvas overlay painter.
pub fn draw(painter: &egui::Painter, gizmo: &SfxGizmoState, view: &SfxView<'_>) {
    if !is_active_on_canvas(gizmo, view) {
        return;
    }
    // Sync active Per-Glyph sub-tool from panel's egui context temp storage.
    gizmo.set_glyph_tool(SfxGlyphTool::get(painter.ctx()));

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
            let scr: [Pos2; 4] = [
                xf.to_screen(geom.world_corners[0][0] as f32, geom.world_corners[0][1] as f32),
                xf.to_screen(geom.world_corners[1][0] as f32, geom.world_corners[1][1] as f32),
                xf.to_screen(geom.world_corners[2][0] as f32, geom.world_corners[2][1] as f32),
                xf.to_screen(geom.world_corners[3][0] as f32, geom.world_corners[3][1] as f32),
            ];
            for i in 0..4 {
                painter.line_segment([scr[i], scr[(i + 1) % 4]], line_stroke);
            }
            let top_mid = apply_free_transform_point(&sfx.free_transform, rb, [(rb[0] + rb[2]) * 0.5, rb[1]]);
            let top_mid_scr = xf.to_screen(top_mid[0] as f32, top_mid[1] as f32);
            let stem_doc = rotate_stem_doc(&sfx.free_transform, rb, xf);
            let stem_scr = xf.to_screen(stem_doc[0] as f32, stem_doc[1] as f32);
            painter.line_segment([top_mid_scr, stem_scr], line_stroke);
            painter.circle_filled(stem_scr, 4.8, fill_handle);
            painter.circle_stroke(stem_scr, 4.8, line_stroke);

            for (pt, kind) in rest_box_points(rb) {
                let wp = apply_free_transform_point(&sfx.free_transform, rb, pt);
                let sp = xf.to_screen(wp[0] as f32, wp[1] as f32);
                let is_corner = matches!(kind, FreeHandle::Corner(_));
                let sz = if is_corner { 8.0 } else { 6.0 };
                let r = Rect::from_center_size(sp, vec2(sz, sz));
                painter.rect_filled(r, 1.5, fill_handle);
                painter.rect_stroke(r, 1.5, line_stroke, egui::StrokeKind::Outside);
            }

            if sfx.spline.show_arc && !sfx.spline.points.is_empty() {
                draw_spline_arc(painter, &sfx, rb, xf);
            }
            if sfx.vector_shadow.enabled {
                draw_vector_shadow_ray(painter, &sfx, rb, xf);
            }
        }
        SfxWarpMode::PerGlyph => {
            let tool = gizmo.current_glyph_tool();
            for g in &geom.glyphs {
                let is_sel = gizmo.selected_glyph == Some(g.glyph_index);
                let gs = glyph_screen_gizmo(g.quad_doc, xf);
                let col = if is_sel { COLOR_AMBER } else { COLOR_AMBER.gamma_multiply(0.45) };
                let st = Stroke::new(if is_sel { 1.8 } else { 1.0 }, col);
                for i in 0..4 {
                    painter.line_segment([gs.corners[i], gs.corners[(i + 1) % 4]], st);
                }
                if is_sel {
                    // Always draw rotation stem handle across Move / Skew / FT (4-Crn).
                    painter.line_segment([gs.top_mid, gs.rot_pos], st);
                    painter.circle_filled(gs.rot_pos, 4.5, fill_handle);
                    painter.circle_stroke(gs.rot_pos, 4.5, st);

                    match tool {
                        SfxGlyphTool::FreeTransform => {
                            for &cp in &gs.corners {
                                let cr = Rect::from_center_size(cp, vec2(7.5, 7.5));
                                painter.rect_filled(cr, 1.0, COLOR_AMBER);
                                painter.rect_stroke(cr, 1.0, Stroke::new(1.2, Color32::WHITE), egui::StrokeKind::Outside);
                            }
                        }
                        SfxGlyphTool::Skew => {
                            for &dp in &[gs.skew_x_pos, gs.skew_y_pos] {
                                let pts = vec![dp + vec2(0.0, -5.5), dp + vec2(5.5, 0.0), dp + vec2(0.0, 5.5), dp + vec2(-5.5, 0.0)];
                                painter.add(egui::Shape::convex_polygon(pts, COLOR_AMBER, Stroke::new(1.2, Color32::WHITE)));
                            }
                        }
                        SfxGlyphTool::Move => {
                            let sr = Rect::from_center_size(gs.scale_pos, vec2(7.0, 7.0));
                            painter.rect_filled(sr, 1.0, fill_handle);
                            painter.rect_stroke(sr, 1.0, st, egui::StrokeKind::Outside);
                        }
                    }
                }
            }
            if sfx.vector_shadow.enabled {
                draw_vector_shadow_ray(painter, &sfx, rb, xf);
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
            let scr: [Pos2; 4] = [
                xf.to_screen(geom.world_corners[0][0] as f32, geom.world_corners[0][1] as f32),
                xf.to_screen(geom.world_corners[1][0] as f32, geom.world_corners[1][1] as f32),
                xf.to_screen(geom.world_corners[2][0] as f32, geom.world_corners[2][1] as f32),
                xf.to_screen(geom.world_corners[3][0] as f32, geom.world_corners[3][1] as f32),
            ];
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

fn draw_spline_arc(painter: &egui::Painter, data: &SfxData, rb: [f64; 4], xf: &CanvasViewXform) {
    let n = data.spline.points.len();
    if n >= 2 {
        let steps = ((n - 1) * 14).clamp(20, 140);
        let mut pts = Vec::with_capacity(steps + 1);
        for s in 0..=steps {
            let t = s as f64 / steps as f64;
            let sp = sample_catmull_rom(&data.spline.points, t);
            let dp = spline_control_doc_pos(data, rb, [sp.lx, sp.ly]);
            pts.push(xf.to_screen(dp[0] as f32, dp[1] as f32));
        }
        painter.add(egui::Shape::line(pts, Stroke::new(1.6, COLOR_CYAN.gamma_multiply(0.85))));
    }
    for &lp in &data.spline.points {
        let dp = spline_control_doc_pos(data, rb, lp);
        let sp = xf.to_screen(dp[0] as f32, dp[1] as f32);
        painter.circle_filled(sp, 4.8, COLOR_CYAN);
        painter.circle_stroke(sp, 4.8, Stroke::new(1.4, Color32::from_rgb(12, 74, 110)));
    }
}

fn draw_vector_shadow_ray(painter: &egui::Painter, data: &SfxData, rb: [f64; 4], xf: &CanvasViewXform) {
    let (center, knob) = shadow_center_and_knob_doc(data, rb);
    let c_scr = xf.to_screen(center[0] as f32, center[1] as f32);
    let k_scr = xf.to_screen(knob[0] as f32, knob[1] as f32);
    painter.line_segment([c_scr, k_scr], Stroke::new(1.6, COLOR_SUN));
    painter.circle_filled(c_scr, 3.0, COLOR_SUN);
    painter.circle_filled(k_scr, 6.0, COLOR_SUN);
    painter.circle_stroke(k_scr, 6.0, Stroke::new(1.4, Color32::from_rgb(120, 53, 15)));
}

fn draw_mesh_lattice(painter: &egui::Painter, data: &SfxData, rb: [f64; 4], xf: &CanvasViewXform, selected: &[(usize, usize)], accent: Color32) {
    let rows = usize::from(data.mesh.rows.max(2));
    let cols = usize::from(data.mesh.cols.max(2));
    let w = (rb[2] - rb[0]).max(1.0);
    let h = (rb[3] - rb[1]).max(1.0);
    let line_stroke = Stroke::new(1.2, accent.gamma_multiply(0.8));
    let segs = 8usize;

    let sample_uv = |u: f64, v: f64| -> Pos2 {
        let uv = eval_mesh_uv(&data.mesh, u, v);
        let p = [rb[0] + uv[0] * w, rb[1] + uv[1] * h];
        let wp = apply_free_transform_point(&data.free_transform, rb, p);
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
    fn free_transform_4_corner_drag_and_shift_trapezoid() {
        let sfx = SfxData { origin: [100.0, 100.0], ..SfxData::default() };
        let (doc, id) = sfx_doc(sfx.clone());
        let mut gizmo = SfxGizmoState::default();
        let geom = gizmo.ensure_geom(&sfx, 72.0).clone();
        let tl = geom.world_corners[0];
        let view = view_for(&doc, id);

        assert!(pointer(&mut gizmo, &view, CanvasToolEvent::Down { x: tl[0], y: tl[1], pressure: 1.0 }, Modifiers::NONE).handled);
        let moved = pointer(&mut gizmo, &view, CanvasToolEvent::Move { x: tl[0] + 18.0, y: tl[1] - 12.0, pressure: 1.0 }, Modifiers::SHIFT);
        let u = moved.updated_sfx.expect("4-corner shift trapezoid update");
        assert!((u.free_transform.corners[0][0] - 18.0).abs() < 1e-6);
        assert!((u.free_transform.corners[0][1] + 12.0).abs() < 1e-6);
        // Paired TR corner must mirror X delta symmetrically under Shift.
        assert!((u.free_transform.corners[1][0] + 18.0).abs() < 1e-6);
        assert!((u.free_transform.corners[1][1] + 12.0).abs() < 1e-6);
    }

    #[test]
    fn spline_control_point_drag_updates_arc_cleanly() {
        let mut sfx = SfxData { origin: [100.0, 100.0], ..SfxData::default() };
        sfx.spline.points = vec![[-100.0, 0.0], [0.0, -20.0], [100.0, 0.0]];
        let (doc, id) = sfx_doc(sfx.clone());
        let mut gizmo = SfxGizmoState::default();
        let geom = gizmo.ensure_geom(&sfx, 72.0).clone();
        let mid_doc = spline_control_doc_pos(&sfx, geom.rest_box, sfx.spline.points[1]);
        let view = view_for(&doc, id);

        assert!(pointer(&mut gizmo, &view, CanvasToolEvent::Down { x: mid_doc[0], y: mid_doc[1], pressure: 1.0 }, Modifiers::NONE).handled);
        let moved = pointer(&mut gizmo, &view, CanvasToolEvent::Move { x: mid_doc[0], y: mid_doc[1] - 15.0, pressure: 1.0 }, Modifiers::NONE);
        let u = moved.updated_sfx.expect("spline drag update");
        assert!((u.spline.points[1][1] + 35.0).abs() < 1e-5);
    }

    #[test]
    fn vector_shadow_handle_drag_updates_angle_and_distance() {
        let mut sfx = SfxData { origin: [100.0, 100.0], ..SfxData::default() };
        sfx.vector_shadow.enabled = true;
        sfx.vector_shadow.angle_deg = 0.0;
        sfx.vector_shadow.distance_px = 30.0;
        let (doc, id) = sfx_doc(sfx.clone());
        let mut gizmo = SfxGizmoState::default();
        let geom = gizmo.ensure_geom(&sfx, 72.0).clone();
        let (center, knob) = shadow_center_and_knob_doc(&sfx, geom.rest_box);
        let view = view_for(&doc, id);

        assert!(pointer(&mut gizmo, &view, CanvasToolEvent::Down { x: knob[0], y: knob[1], pressure: 1.0 }, Modifiers::NONE).handled);
        let moved = pointer(&mut gizmo, &view, CanvasToolEvent::Move { x: center[0], y: center[1] + 40.0, pressure: 1.0 }, Modifiers::NONE);
        let u = moved.updated_sfx.expect("shadow drag update");
        assert!((u.vector_shadow.angle_deg - 90.0).abs() < 1e-3);
        assert!((u.vector_shadow.distance_px - 40.0).abs() < 1e-3);
    }

    #[test]
    fn mesh_marquee_box_selects_and_moves_multiple_nodes() {
        let sfx = SfxData { origin: [100.0, 100.0], warp_mode: SfxWarpMode::MeshLattice, ..SfxData::default() };
        let (doc, id) = sfx_doc(sfx.clone());
        let mut gizmo = SfxGizmoState::default();
        let geom = gizmo.ensure_geom(&sfx, 72.0).clone();
        let view = view_for(&doc, id);

        let rb = geom.rest_box;
        let p_n00 = mesh_node_doc_pos(&sfx, rb, 0, 0).unwrap();
        let p_n01 = mesh_node_doc_pos(&sfx, rb, 0, 1).unwrap();
        let start = [p_n00[0] - 25.0, p_n00[1] - 25.0];
        let end = [p_n01[0] + 10.0, p_n01[1] + 10.0];

        assert!(pointer(&mut gizmo, &view, CanvasToolEvent::Down { x: start[0], y: start[1], pressure: 1.0 }, Modifiers::NONE).handled);
        assert!(pointer(&mut gizmo, &view, CanvasToolEvent::Move { x: end[0], y: end[1], pressure: 1.0 }, Modifiers::NONE).handled);
        assert!(pointer(&mut gizmo, &view, CanvasToolEvent::Up { x: end[0], y: end[1] }, Modifiers::NONE).handled);
        assert_eq!(gizmo.selected_nodes, vec![(0, 0), (0, 1)]);

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
    fn per_glyph_ft_corner_drag_updates_glyph_corners() {
        let sfx = SfxData { text: "AB".to_owned(), origin: [100.0, 100.0], warp_mode: SfxWarpMode::PerGlyph, ..SfxData::default() };
        let (doc, id) = sfx_doc(sfx.clone());
        let mut gizmo = SfxGizmoState::default();
        gizmo.set_glyph_tool(SfxGlyphTool::FreeTransform);
        let geom = gizmo.ensure_geom(&sfx, 72.0).clone();
        let g0 = &geom.glyphs[0];
        gizmo.selected_glyph = Some(g0.glyph_index);
        let tl = g0.quad_doc[0];
        let view = view_for(&doc, id);

        assert!(pointer(&mut gizmo, &view, CanvasToolEvent::Down { x: tl[0], y: tl[1], pressure: 1.0 }, Modifiers::NONE).handled);
        let moved = pointer(&mut gizmo, &view, CanvasToolEvent::Move { x: tl[0] - 8.0, y: tl[1] - 5.0, pressure: 1.0 }, Modifiers::NONE);
        let u = moved.updated_sfx.expect("glyph 4-corner update");
        assert_eq!(u.per_glyph.len(), 1);
        assert!((u.per_glyph[0].corners[0][0] + 8.0).abs() < 1e-5);
        assert!((u.per_glyph[0].corners[0][1] + 5.0).abs() < 1e-5);
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