//! On-canvas Screentone gizmo, decoupled from `PhotocraftApp` and `photocraft_engine`.
//!
//! While the Screentones panel shows, the pending sheet's geometry lives on the canvas as three
//! handles — the *origin* (a crosshair where the lattice anchors), the *x arm* (its direction is
//! the pattern angle, its length the x spacing) and the *y arm* (its length the y spacing).
//!
//! The protocol (identical to `ui-egui`'s original gizmo):
//! * **Origin**: dragging translates the sheet's phase (`ScreentoneSpec::origin_x/y`, given in
//!   the tone's own rotated frame); `Shift` locks the drag to the dominant document axis.
//! * **X arm**: dragging sets the pattern's `angle` and the arm's length sets `spacingX`;
//!   `Shift` snaps the angle to 15° steps.
//! * **Y arm**: dragging sets `spacingY`.
//!
//! All document/engine state arrives through [`GizmoView`] / [`PreviewInput`] and the panel's own
//! [`ScreentoneUi`]; outbound edits are appended to an `OutCommand` dispatch buffer. Nothing in
//! this module reaches into L6 `photocraft-ui-egui` or the engine. The panel and the gizmo edit one
//! spec (`ScreentoneUi::spec`), so a drag's live preview and its committed render share a source.

use egui::{pos2, vec2};
use photocraft_algo::{FilterParams, ScreentoneMode, ScreentoneSpec};
use photocraft_compose::Buffer;
use photocraft_doc::{Document, Layer, LayerId, Rect};
use photocraft_raster::Surface;
use serde_json::json;

use crate::comic_screentone_panel::{CMD_LAYER, ScreentoneTargetInfo, ScreentoneUi, params};
use crate::view::{CanvasToolEvent, CanvasViewXform, screentone_sheet_bounds};
use crate::{BridgeDocId, OutCommand};

/// Screen pixels a handle answers the pointer within (a 10 px grab target, midway between the
/// 8-12 px hand targets Photoshop-class gizmos use).
const HIT_PX: f32 = 10.0;

/// Pixel count beyond which the preview skips rendering (the handles and the sync stay live;
/// a region past ~2.5 MP re-renders too slowly per drag frame, and the engine's committed
/// render takes over).
const PREVIEW_CAP: usize = 2_500_000;

/// The decoupled state of the gizmo: the sheet layer the panel's New Layer produced, the document
/// it belongs to, the last layer synced into the panel, the running drag and the preview cache.
#[derive(Default)]
pub struct ScreentoneGizmoState {
    /// The sheet layer the panel's New Layer produced — the drags' restyle target. Bound to one
    /// document (`target_doc`, so another document fronts it stale).
    target: Option<LayerId>,
    target_doc: Option<BridgeDocId>,
    /// Last active screentone layer whose `spec` was synced into `ScreentoneUi`.
    synced_layer: Option<(BridgeDocId, LayerId)>,
    /// The running drag, with the spec as it stood at the press (Escape restores it).
    drag: Option<Drag>,
    /// The preview texture cache (a keyed CPU surface).
    preview: Option<PreviewCache>,
}

impl ScreentoneGizmoState {
    /// Whether a handle drag is currently in flight.
    pub fn has_active_drag(&self) -> bool {
        self.drag.is_some()
    }

    /// Detaches from any previously bound screentone layer (e.g. after freezing/rasterizing it,
    /// so the next edit does not target a baked layer) and clears any drag/preview.
    pub fn detach(&mut self) {
        self.target = None;
        self.target_doc = None;
        self.synced_layer = None;
        self.drag = None;
        self.preview = None;
    }

    /// Binds the gizmo to the sheet layer a New Layer run produced. This is a binding, not an
    /// active-layer look-up: the panel knows exactly which sheet it just made.
    pub fn bind_layer(&mut self, doc_id: BridgeDocId, layer_id: LayerId) {
        self.target = Some(layer_id);
        self.target_doc = Some(doc_id);
        self.synced_layer = Some((doc_id, layer_id));
    }

    /// The gizmo's restyle target while it still names an existing live screentone layer in `doc`
    /// (prefers the active layer when it is a live screentone, otherwise the panel's bound layer).
    ///
    /// Document identity is checked by [`Self::hide_layer_during_drag`] (which receives the id)
    /// and by the host's bind/detach lifecycle; this helper only sees `doc` and `active_layer`.
    pub fn target_layer(&self, doc: &Document, active_layer: Option<LayerId>) -> Option<LayerId> {
        if let Some(id) = active_layer
            && doc.layer(id).is_some_and(Layer::is_screentone)
        {
            return Some(id);
        }
        if let Some(id) = self.target
            && doc.layer(id).is_some_and(Layer::is_screentone)
        {
            return Some(id);
        }
        None
    }

    /// The screentone layer to hide in the composite during an active drag so the live preview
    /// mesh replaces it cleanly without double Multiply blending. `None` when idle, when `doc_id`
    /// is not the bound document, or when there is no live target.
    pub fn hide_layer_during_drag(&self, doc_id: BridgeDocId, doc: &Document, active_layer: Option<LayerId>) -> Option<LayerId> {
        self.drag.as_ref()?;
        if self.target_doc.is_some_and(|bound| bound != doc_id) {
            return None;
        }
        self.target_layer(doc, active_layer)
    }

    /// Synchronizes `ScreentoneUi.spec` and the target binding when the user selects an existing
    /// procedural screentone layer. A running drag is left untouched.
    pub fn sync_active_layer(&mut self, st: &mut ScreentoneUi, active_tone: Option<&ScreentoneTargetInfo>) {
        if self.drag.is_some() {
            return;
        }
        match active_tone {
            Some(tone) => {
                if self.synced_layer != Some((tone.doc_id, tone.layer_id)) {
                    st.spec = tone.spec.clone();
                    self.target = Some(tone.layer_id);
                    self.target_doc = Some(tone.doc_id);
                    self.synced_layer = Some((tone.doc_id, tone.layer_id));
                }
            }
            None => self.synced_layer = None,
        }
    }

    /// Cancels the running drag and restores the spec it started from — no render, no journal
    /// entry. A no-op without a running drag.
    pub fn cancel_drag(&mut self, st: &mut ScreentoneUi) {
        if let Some(drag) = self.drag.take() {
            self.preview = None;
            st.spec = drag.spec;
        }
    }
}

/// Which end of the gizmo a pointer took.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Handle {
    /// The origin: the sheet's phase anchor inside the tone's rotation (`Shift` axis-locks).
    Origin,
    /// The x arm: direction sets the angle, distance sets the x spacing (`Shift` 15° snap).
    ArmX,
    /// The y arm: distance sets the y spacing.
    ArmY,
}

/// A grab in flight: the handle kind, where the press happened and the spec as it stood at the
/// press (the fields a drag must not disturb stay in it).
#[derive(Clone, Debug)]
struct Drag {
    kind: Handle,
    start_doc: [f64; 2],
    spec: ScreentoneSpec,
}

struct PreviewCache {
    key: String,
    rect: Rect,
    texture: egui::TextureHandle,
}

/// All document/view state the gizmo needs for one call, packed so no function takes more than
/// seven arguments.
pub struct GizmoView<'a> {
    pub panel_showing: bool,
    pub doc: &'a Document,
    pub doc_id: BridgeDocId,
    pub active_layer: Option<LayerId>,
    pub xf: &'a CanvasViewXform,
}

/// The gizmo and its canvas preview live only while a drag runs or the Screentones panel shows
/// with an active procedural screentone layer. Switching to the panel tab over a raster layer,
/// smart object or empty layer stack never projects an overlay.
pub fn showing(gizmo: &ScreentoneGizmoState, panel_showing: bool, doc: &Document, active_layer: Option<LayerId>) -> bool {
    if gizmo.has_active_drag() {
        return true;
    }
    panel_showing && active_layer.and_then(|id| doc.layer(id)).is_some_and(Layer::is_screentone)
}

/// The reference bounds the sheet renders over, exactly as the engine computes them: their
/// center is the lattice's anchor, so the gizmo reads them through the same seam function rather
/// than deriving them twice.
fn sheet_bounds(view: &GizmoView<'_>, gizmo: &ScreentoneGizmoState) -> Option<Rect> {
    let target = gizmo.target_layer(view.doc, view.active_layer);
    let bounds = screentone_sheet_bounds(view.doc, target);
    (!bounds.is_empty()).then_some(bounds)
}

/// The lattice anchor in document coordinates: the engine anchors the sheet at the render
/// bounds' center and phases the lattice by `origin` through the tone's own canvas rotation, so
/// the document anchor undoes that same rotation (plus the center offset).
fn anchor_doc(spec: &ScreentoneSpec, bounds: Rect) -> [f64; 2] {
    let (sin, cos) = spec.rot_canvas.to_radians().sin_cos();
    let (sin, cos) = (f64::from(sin), f64::from(cos));
    // Exactly the engine's center — the algo takes half of the max-1 sizes in f32, so an odd
    // width's half pixel keeps the handle on the true lattice center.
    let center = [f64::from(bounds.x0) + f64::from(bounds.width().max(1)) * 0.5, f64::from(bounds.y0) + f64::from(bounds.height().max(1)) * 0.5];
    let (ox, oy) = (f64::from(spec.origin_x), f64::from(spec.origin_y));
    [center[0] + ox * cos - oy * sin, center[1] + ox * sin + oy * cos]
}

/// A document-space offset through the tone's own canvas rotation: doc → local, the same mapping
/// the engine's lattice uses, so a drag's pointer delta lands in the spec's frame.
fn doc_to_local(spec: &ScreentoneSpec, rel: [f64; 2]) -> [f32; 2] {
    let (sin, cos) = spec.rot_canvas.to_radians().sin_cos();
    let (rx, ry) = (rel[0] as f32, rel[1] as f32);
    [rx * cos + ry * sin, -rx * sin + ry * cos]
}

/// The document-space direction an arm points: the tone's own axis through its canvas rotation
/// plus, for the x arm, the pattern's own angle — the same mapping a drag inverts.
fn arm_dir(spec: &ScreentoneSpec, kind: Handle) -> [f64; 2] {
    let deg = match kind {
        Handle::ArmX => spec.angle + spec.rot_canvas,
        Handle::ArmY => spec.angle + spec.rot_canvas + 90.0,
        Handle::Origin => spec.angle,
    };
    let (sin, cos) = f32::to_radians(deg).sin_cos();
    [f64::from(cos), f64::from(sin)]
}

/// The three handles in screen space: the origin at the lattice anchor, the arm ends at one
/// spacing along each axis (the spacings are document px, so the arms scale with the view).
fn handles(spec: &ScreentoneSpec, bounds: Rect, xf: &CanvasViewXform) -> [egui::Pos2; 3] {
    let a = anchor_doc(spec, bounds);
    let at = |dx: [f64; 2], spacing: f64| xf.to_screen((a[0] + dx[0] * spacing) as f32, (a[1] + dx[1] * spacing) as f32);
    let x = arm_dir(spec, Handle::ArmX);
    let y = arm_dir(spec, Handle::ArmY);
    [xf.to_screen(a[0] as f32, a[1] as f32), at(x, spec.spacing_x as f64), at(y, spec.spacing_y as f64)]
}

/// The handle under a document-space pointer point (the distances are screen px through the
/// view, so zoomed, rotated and flipped views all grab correctly) — the nearest within the grab
/// target, or `None` between handles.
fn hit(spec: &ScreentoneSpec, bounds: Rect, xf: &CanvasViewXform, at: [f64; 2]) -> Option<Handle> {
    let hs = handles(spec, bounds, xf);
    let p = xf.to_screen(at[0] as f32, at[1] as f32);
    let mut best: Option<(f32, Handle)> = None;
    for (kind, q) in [(Handle::Origin, hs[0]), (Handle::ArmX, hs[1]), (Handle::ArmY, hs[2])] {
        let d = q.distance(p);
        if d <= HIT_PX && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, kind));
        }
    }
    best.map(|(_, kind)| kind)
}

/// The canvas event handler for the gizmo's drags, before the tools see the events. Only a press
/// that took a handle (and its moves and release) returns true; everything else falls through.
pub fn pointer(
    ev: CanvasToolEvent,
    mods: egui::Modifiers,
    view: &GizmoView<'_>,
    st: &mut ScreentoneUi,
    gizmo: &mut ScreentoneGizmoState,
    out: &mut Vec<OutCommand>,
) -> bool {
    if !showing(gizmo, view.panel_showing, view.doc, view.active_layer) {
        return false;
    }
    match ev {
        CanvasToolEvent::Down { x, y, .. } => {
            let Some(bounds) = sheet_bounds(view, gizmo) else { return false };
            let Some(kind) = hit(&st.spec, bounds, view.xf, [x, y]) else { return false };
            gizmo.drag = Some(Drag { kind, start_doc: [x, y], spec: st.spec.clone() });
            true
        }
        CanvasToolEvent::Move { x, y, .. } => {
            let Some(drag) = gizmo.drag.clone() else { return false };
            let Some(bounds) = sheet_bounds(view, gizmo) else { return false };
            apply_drag(&drag, [x, y], mods.shift, bounds, st);
            true
        }
        CanvasToolEvent::Up { x, y } => {
            let Some(drag) = gizmo.drag.clone() else { return false };
            gizmo.drag = None;
            gizmo.preview = None;
            if let Some(bounds) = sheet_bounds(view, gizmo) {
                apply_drag(&drag, [x, y], mods.shift, bounds, st);
            }
            commit_drag(&drag, view, gizmo, st, out);
            true
        }
    }
}

/// Maps a pointer move onto the pending spec. The drag re-derives each move from its own snapshot,
/// so moves compose and the drag's fields never collide with anything the panel changes meanwhile.
fn apply_drag(drag: &Drag, at: [f64; 2], shift: bool, bounds: Rect, st: &mut ScreentoneUi) {
    let mut spec = drag.spec.clone();
    match drag.kind {
        Handle::Origin => {
            // `Shift` locks to the dominant *document* axis before the tone's own rotation, so
            // the slide stays on one document direction however the tone is turned.
            let mut d = [at[0] - drag.start_doc[0], at[1] - drag.start_doc[1]];
            if shift && d[0].abs() != d[1].abs() {
                if d[0].abs() < d[1].abs() {
                    d[0] = 0.0;
                } else {
                    d[1] = 0.0;
                }
            }
            let loc = doc_to_local(&drag.spec, d);
            spec.origin_x = (spec.origin_x + loc[0]).clamp(-100_000.0, 100_000.0);
            spec.origin_y = (spec.origin_y + loc[1]).clamp(-100_000.0, 100_000.0);
        }
        Handle::ArmX | Handle::ArmY => {
            let a = anchor_doc(&drag.spec, bounds);
            let loc = doc_to_local(&drag.spec, [at[0] - a[0], at[1] - a[1]]);
            let dist = (loc[0] * loc[0] + loc[1] * loc[1]).sqrt();
            match drag.kind {
                Handle::ArmX => {
                    let mut deg = loc[1].atan2(loc[0]).to_degrees();
                    if shift {
                        deg = (deg / 15.0).round() * 15.0;
                    }
                    // Normalize into the panel field's −180..180 range.
                    spec.angle = (deg + 180.0).rem_euclid(360.0) - 180.0;
                    spec.spacing_x = dist.clamp(1.0, 2000.0);
                }
                Handle::ArmY => spec.spacing_y = dist.clamp(1.0, 2000.0),
                Handle::Origin => {}
            }
        }
    }
    st.spec = spec;
}

/// The pointer-up commit: the pending spec runs the screentone layer command against the bound
/// sheet — one outbound edit. Without a valid target or a moved spec there is nothing to write.
fn commit_drag(drag: &Drag, view: &GizmoView<'_>, gizmo: &ScreentoneGizmoState, st: &ScreentoneUi, out: &mut Vec<OutCommand>) {
    if st.spec == drag.spec {
        return; // nothing moved: no render, no journal entry
    }
    let Some(target) = gizmo.target_layer(view.doc, view.active_layer) else { return };
    let mut p = params(st);
    if let Some(map) = p.as_object_mut() {
        map.insert("layer".into(), json!(target.0));
    }
    // `OutCommand` is the bridge's dispatch-buffer entry; the host runs `CMD_LAYER` and reports
    // the produced layer back through `ScreentoneGizmoState::bind_layer`.
    out.push(OutCommand::new(CMD_LAYER, p));
}

/// Escape (handled where the egui context is at hand) cancels the running drag and restores the
/// spec it started from — no render, no journal entry.
pub fn escape(ctx: &egui::Context, st: &mut ScreentoneUi, gizmo: &mut ScreentoneGizmoState) {
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        gizmo.cancel_drag(st);
    }
}

/// Everything the preview refresh needs that is not already in [`GizmoView`], packed so the
/// refresh call stays within the argument budget.
pub struct PreviewInput<'a> {
    pub view: &'a GizmoView<'a>,
    pub doc_index: usize,
    pub revision: u64,
    pub display_key: u64,
    pub texture_options: egui::TextureOptions,
}

/// Refreshes the live preview each canvas frame, ahead of the draw pass. The texture is built
/// **only** during an active handle drag; when idle, the document's own screentone layer renders
/// through the compositor without any duplicate overlay mesh.
pub fn refresh_preview(
    ctx: &egui::Context,
    input: &PreviewInput<'_>,
    st: &mut ScreentoneUi,
    gizmo: &mut ScreentoneGizmoState,
    to_image: &dyn Fn(&Buffer) -> egui::ColorImage,
) {
    let view = input.view;
    if !showing(gizmo, view.panel_showing, view.doc, view.active_layer) || gizmo.drag.is_none() {
        gizmo.preview = None;
        return;
    }
    let Some(bounds) = sheet_bounds(view, gizmo) else {
        gizmo.preview = None;
        return;
    };
    let Some(region) = preview_rect(view.xf, bounds) else {
        gizmo.preview = None;
        return;
    };
    let target = gizmo.target_layer(view.doc, view.active_layer);
    let coverage = target
        .and_then(|id| view.doc.layer(id))
        .and_then(|l| l.mask.as_ref())
        .filter(|m| m.enabled)
        .map(|m| m.surface.clone())
        .or_else(|| view.doc.selection.clone());
    // The engine restyles in the layer's own surface format; the sheet the panel made (and a
    // fresh creation) would render in the document's, so the preview prefers the layer's.
    let fmt =
        target.and_then(|id| view.doc.layer(id)).and_then(|layer| layer.surface()).map(|surface| surface.format()).unwrap_or_else(|| view.doc.pixel_format());
    let spec = st.spec.clone();
    let key = format!("{}|{}|{:?}|{:?}|{:?}|{:?}|{}|{}", view.doc_id.0, input.revision, target, spec, bounds, region, input.doc_index, input.display_key,);
    if gizmo.preview.as_ref().is_some_and(|c| c.key == key) {
        return;
    }
    let params = FilterParams::Screentone { spec: spec_with(spec) };
    let buf = render_preview(fmt, region, bounds, &params, coverage.as_ref().map(|c| (c, region)));
    let image = to_image(&buf);
    let tex = input.texture_options;
    match gizmo.preview.as_mut() {
        Some(cache) => {
            cache.texture.set(image, tex);
            cache.key = key;
            cache.rect = region;
        }
        None => gizmo.preview = Some(PreviewCache { key, rect: region, texture: ctx.load_texture("screentone-preview", image, tex) }),
    }
}

/// `useSourceLuminance` off: the preview shows a procedural sheet — the same thing the engine
/// renders on generation and restyle.
fn spec_with(mut spec: ScreentoneSpec) -> ScreentoneSpec {
    spec.use_source_luminance = false;
    spec.mode = ScreentoneMode::Sheet;
    spec
}

/// One tone render for `region` over the engine's `bounds`, coverage-multiplied by a gating
/// surface when one arrives (a mask's, or a selection's): every channel scales with the coverage,
/// and pixels outside `bounds` or where coverage is zero have strictly 0.0 alpha.
fn render_preview(fmt: photocraft_color::PixelFormat, region: Rect, bounds: Rect, params: &FilterParams, coverage: Option<(&Surface, Rect)>) -> Buffer {
    let mut buf = Buffer::transparent(region);
    let clip = region.intersect(&bounds);
    if clip.is_empty() {
        return buf;
    }
    let rendered = photocraft_algo::apply(&Surface::new(fmt), params, region, bounds, None);
    let channels = rendered.format().channels();
    let flat = rendered.read_region(region);
    let cover: Option<(usize, Vec<f32>)> = coverage.map(|(c, r)| (c.format().channels(), c.read_region(r)));
    let w = region.width() as usize;
    buf.px = flat
        .chunks_exact(channels)
        .enumerate()
        .map(|(i, v)| {
            let x = region.x0 + (i % w) as i32;
            let y = region.y0 + (i / w) as i32;
            if !bounds.contains(x, y) {
                return [0.0; 4];
            }
            let sample = |k: usize| v.get(k).copied().unwrap_or(1.0).clamp(0.0, 1.0);
            let mut px = [
                sample(0),
                if channels >= 3 { sample(1) } else { sample(0) },
                if channels >= 3 { sample(2) } else { sample(0) },
                if channels >= 4 { sample(3) } else { 1.0 },
            ];
            if let Some((mch, mflat)) = cover.as_ref() {
                let cov = mflat.get(i * mch).copied().unwrap_or(0.0).clamp(0.0, 1.0);
                if cov <= 1e-6 {
                    return [0.0; 4];
                }
                px[0] *= cov;
                px[1] *= cov;
                px[2] *= cov;
                px[3] *= cov;
            }
            px
        })
        .collect();
    buf
}

/// The sheet region the preview renders: the visible document area (the view's corners in
/// document space, boxed) cut down by the sheet's bounds. `None` when nothing is visible, or the
/// region would exceed the render cap.
fn preview_rect(xf: &CanvasViewXform, sheet: Rect) -> Option<Rect> {
    let corners = [xf.rect.left_top(), xf.rect.right_top(), xf.rect.right_bottom(), xf.rect.left_bottom()];
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, -f64::MAX, -f64::MAX);
    for c in corners {
        let p = xf.to_doc(c);
        x0 = x0.min(p[0]);
        x1 = x1.max(p[0]);
        y0 = y0.min(p[1]);
        y1 = y1.max(p[1]);
    }
    let r = Rect::new(
        x0.max(f64::from(sheet.x0)).floor() as i32,
        y0.max(f64::from(sheet.y0)).floor() as i32,
        x1.min(f64::from(sheet.x1)).ceil() as i32,
        y1.min(f64::from(sheet.y1)).ceil() as i32,
    );
    if r.x1 < r.x0 || r.y1 < r.y0 {
        return None;
    }
    (r.width() as usize * r.height() as usize <= PREVIEW_CAP).then_some(r)
}

/// The gizmo overlay: when idle (`drag.is_none()`), draws **only** the handle wireframe, leaving
/// pixel rendering to the layer's own compositor. Only during an active handle drag is the
/// temporary preview mesh drawn under the handles.
pub fn draw(painter: &egui::Painter, view: &GizmoView<'_>, st: &ScreentoneUi, gizmo: &ScreentoneGizmoState) {
    if !showing(gizmo, view.panel_showing, view.doc, view.active_layer) {
        return;
    }
    if gizmo.drag.is_some()
        && let Some(cache) = &gizmo.preview
    {
        let mut mesh = egui::Mesh::with_texture(cache.texture.id());
        let r = cache.rect;
        let vertex = |x: f64, y: f64| egui::epaint::Vertex {
            pos: view.xf.to_screen(x as f32, y as f32),
            uv: pos2(((x - f64::from(r.x0)) / f64::from(r.width())) as f32, ((y - f64::from(r.y0)) / f64::from(r.height())) as f32),
            color: egui::Color32::WHITE,
        };
        for (x, y) in [(r.x0, r.y0), (r.x1, r.y0), (r.x1, r.y1), (r.x0, r.y1)] {
            mesh.vertices.push(vertex(f64::from(x), f64::from(y)));
        }
        mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
        painter.add(egui::Shape::mesh(mesh));
    }
    let Some(bounds) = sheet_bounds(view, gizmo) else { return };
    let hs = handles(&st.spec, bounds, view.xf);
    let tokens = crate::theme::Tokens::get(painter.ctx());
    let stroke = egui::Stroke::new(1.5, tokens.accent);
    painter.line_segment([hs[0] - vec2(10.0, 0.0), hs[0] + vec2(10.0, 0.0)], stroke);
    painter.line_segment([hs[0] - vec2(0.0, 10.0), hs[0] + vec2(0.0, 10.0)], stroke);
    painter.circle_stroke(hs[0], 5.0, stroke);
    painter.line_segment([hs[0], hs[1]], stroke);
    painter.line_segment([hs[0], hs[2]], stroke);
    for q in [hs[1], hs[2]] {
        painter.circle_stroke(q, 4.0, stroke);
    }
}

/// The cursor over the canvas while the gizmo lives: the hovered handle's stand-in, or the
/// running drag's.
pub fn hover_cursor(view: &GizmoView<'_>, st: &ScreentoneUi, gizmo: &ScreentoneGizmoState, at: [f64; 2]) -> Option<egui::CursorIcon> {
    if !showing(gizmo, view.panel_showing, view.doc, view.active_layer) {
        return None;
    }
    let kind = gizmo.drag.as_ref().map(|d| d.kind).or_else(|| {
        let bounds = sheet_bounds(view, gizmo)?;
        hit(&st.spec, bounds, view.xf, at)
    })?;
    Some(cursor_of(kind))
}

fn cursor_of(kind: Handle) -> egui::CursorIcon {
    // egui 0.36 has no dedicated rotate or size-all cursor: the origin takes the precision
    // crosshair, the x arm the diagonal resize (an angle to set) and the y arm the four-way
    // arrow of the classic "size all".
    match kind {
        Handle::Origin => egui::CursorIcon::Crosshair,
        Handle::ArmX => egui::CursorIcon::ResizeNeSw,
        Handle::ArmY => egui::CursorIcon::AllScroll,
    }
}

#[cfg(test)]
mod tests {
    use egui::{Pos2, Rect, pos2, vec2};
    use photocraft_algo::{FilterParams, ScreentoneSpec};
    use photocraft_doc::{LayerMask, Rect as DocRect};

    use super::{
        Drag, HIT_PX, Handle, PREVIEW_CAP, ScreentoneGizmoState, anchor_doc, apply_drag, arm_dir, cursor_of, doc_to_local, escape, handles, hit, preview_rect,
        render_preview, spec_with,
    };
    use crate::comic_screentone_panel::ScreentoneUi;
    use crate::view::CanvasViewXform;

    fn spec() -> ScreentoneSpec {
        ScreentoneSpec { angle: 0.0, spacing_x: 8.0, spacing_y: 8.0, ..Default::default() }
    }

    /// A 1:1 view with the document centered — handle/doc points coincide on screen.
    fn one_to_one() -> CanvasViewXform {
        CanvasViewXform {
            rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(96.0, 72.0)),
            zoom: 1.0,
            center: [48.0, 36.0],
            flip: false,
            rotation: 0.0,
            aspect: 1.0,
        }
    }

    /// The source's 2:1 view with a 96x72 canvas magnified into a 192x144 viewport.
    fn zoomed() -> CanvasViewXform {
        CanvasViewXform {
            rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(192.0, 144.0)),
            zoom: 2.0,
            center: [48.0, 36.0],
            flip: false,
            rotation: 0.0,
            aspect: 1.0,
        }
    }

    /// Zoom 4 with a 90° camera rotation: grabs must still answer in screen px.
    fn rotated() -> CanvasViewXform {
        CanvasViewXform {
            rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(384.0, 288.0)),
            zoom: 4.0,
            center: [48.0, 36.0],
            flip: false,
            rotation: 90.0,
            aspect: 1.0,
        }
    }

    fn close(a: Pos2, b: [f32; 2]) -> bool {
        (a.x - b[0]).abs() < 1e-3 && (a.y - b[1]).abs() < 1e-3
    }

    #[test]
    fn the_anchor_starts_at_the_bounds_center() {
        assert_eq!(anchor_doc(&spec(), DocRect::new(0, 0, 96, 72)), [48.0, 36.0]);
    }

    #[test]
    fn the_anchor_phases_by_the_tone_rotation() {
        let mut s = spec();
        s.origin_x = 10.0;
        s.rot_canvas = 90.0;
        let a = anchor_doc(&s, DocRect::new(0, 0, 96, 72));
        assert!((a[0] - 48.0).abs() < 1e-4, "{a:?}");
        assert!((a[1] - 46.0).abs() < 1e-4, "{a:?}");
    }

    #[test]
    fn doc_to_local_inverts_the_canvas_rotation() {
        let mut s = spec();
        s.rot_canvas = 90.0;
        let loc = doc_to_local(&s, [26.0, 0.0]);
        assert!(loc[0].abs() < 1e-4, "{loc:?}");
        assert!((loc[1] + 26.0).abs() < 1e-4, "{loc:?}");
    }

    #[test]
    fn the_arms_point_along_the_tone_axes() {
        let s = spec();
        let x = arm_dir(&s, Handle::ArmX);
        let y = arm_dir(&s, Handle::ArmY);
        assert!(close(Pos2::new(x[0] as f32, x[1] as f32), [1.0, 0.0]));
        assert!(close(Pos2::new(y[0] as f32, y[1] as f32), [0.0, 1.0]));
    }

    #[test]
    fn handles_sit_at_one_spacing_from_the_anchor() {
        let hs = handles(&spec(), DocRect::new(0, 0, 96, 72), &one_to_one());
        assert!(close(hs[0], [48.0, 36.0]), "{:?}", hs[0]);
        assert!(close(hs[1], [56.0, 36.0]), "{:?}", hs[1]);
        assert!(close(hs[2], [48.0, 44.0]), "{:?}", hs[2]);
    }

    #[test]
    fn the_gizmo_grabs_through_a_rotated_zoomed_view() {
        let bounds = DocRect::new(0, 0, 96, 72);
        let xf = rotated();
        assert_eq!(hit(&spec(), bounds, &xf, [48.0, 36.0]), Some(Handle::Origin));
        assert_eq!(hit(&spec(), bounds, &xf, [56.0, 36.0]), Some(Handle::ArmX));
        assert_eq!(hit(&spec(), bounds, &xf, [52.0, 36.0]), None, "mid-arm space falls through");
        // The grab target is screen px: 2 doc px at zoom 4 is 8 px on screen (< HIT_PX).
        assert_eq!(
            hit(&spec(), bounds, &xf, [48.0 + f64::from(HIT_PX) / 8.0, 36.0]),
            Some(Handle::Origin),
            "a handle still answers inside the screen-space grab radius",
        );
    }

    #[test]
    fn cursors_follow_the_grabbed_handle() {
        assert_eq!(cursor_of(Handle::Origin), egui::CursorIcon::Crosshair);
        assert_eq!(cursor_of(Handle::ArmX), egui::CursorIcon::ResizeNeSw);
        assert_eq!(cursor_of(Handle::ArmY), egui::CursorIcon::AllScroll);
    }

    #[test]
    fn the_origin_shift_locks_the_dominant_document_axis() {
        let mut st = ScreentoneUi::default();
        let drag = Drag { kind: Handle::Origin, start_doc: [48.0, 36.0], spec: spec() };
        apply_drag(&drag, [74.0, 20.0], true, DocRect::new(0, 0, 96, 72), &mut st);
        assert!((st.spec.origin_x - 26.0).abs() < 1e-4, "{}", st.spec.origin_x);
        assert_eq!(st.spec.origin_y, 0.0, "the y component dropped on shift");
    }

    #[test]
    fn an_arm_drag_sets_angle_and_spacing_but_not_the_other_axis() {
        let mut st = ScreentoneUi::default();
        let before = spec();
        let drag = Drag { kind: Handle::ArmX, start_doc: [56.0, 36.0], spec: before.clone() };
        apply_drag(&drag, [68.0, 20.0], false, DocRect::new(0, 0, 96, 72), &mut st);
        assert!((st.spec.angle - 0.0).abs() > 1e-3, "the arm's direction set the angle");
        assert!((st.spec.spacing_x - 8.0).abs() > 1e-3, "the arm's length set the x spacing");
        assert_eq!(st.spec.spacing_y, before.spacing_y, "the y arm stayed alone");
    }

    #[test]
    fn shift_snaps_the_arm_angle_to_fifteen_degree_steps() {
        let mut st = ScreentoneUi::default();
        let drag = Drag { kind: Handle::ArmX, start_doc: [56.0, 36.0], spec: spec() };
        apply_drag(&drag, [68.0, 20.0], true, DocRect::new(0, 0, 96, 72), &mut st);
        assert!(((st.spec.angle / 15.0).round() * 15.0 - st.spec.angle).abs() < 1e-4, "{}", st.spec.angle);
    }

    #[test]
    fn escape_restores_the_spec_the_drag_started_from() {
        let mut st = ScreentoneUi::default();
        let mut g = ScreentoneGizmoState::default();
        let snapshot = st.spec.clone();
        g.drag = Some(Drag { kind: Handle::Origin, start_doc: [48.0, 36.0], spec: snapshot.clone() });
        st.spec.angle = -120.0;
        let ctx = egui::Context::default();
        // A context with no Escape keypress is a no-op.
        escape(&ctx, &mut st, &mut g);
        assert_eq!(st.spec.angle, -120.0);
        // Driving the state machine directly restores the snapshot.
        g.cancel_drag(&mut st);
        assert_eq!(st.spec, snapshot);
        assert!(!g.has_active_drag(), "the drag cleared");
    }

    #[test]
    fn preview_rect_clips_to_the_sheet_and_respects_the_cap() {
        let bounds = DocRect::new(0, 0, 96, 72);
        let r = preview_rect(&zoomed(), bounds).unwrap();
        assert_eq!(r, bounds, "the whole sheet is visible");
        assert!(r.width() as usize * r.height() as usize <= PREVIEW_CAP);
        let huge = CanvasViewXform {
            rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(3000.0, 2400.0)),
            zoom: 1.0,
            center: [1500.0, 1200.0],
            flip: false,
            rotation: 0.0,
            aspect: 1.0,
        };
        assert_eq!(preview_rect(&huge, DocRect::new(0, 0, 3000, 2400)), None, "the hypersheet is skipped");
    }

    #[test]
    fn spec_with_forces_a_procedural_sheet() {
        let mut s = spec();
        s.use_source_luminance = true;
        let out = spec_with(s);
        assert!(!out.use_source_luminance);
        assert_eq!(out.mode, photocraft_algo::ScreentoneMode::Sheet);
    }

    #[test]
    fn the_preview_multiplies_the_coverage() {
        let region = DocRect::new(0, 0, 16, 16);
        let fmt = photocraft_color::PixelFormat::new(photocraft_color::ColorMode::Rgb, photocraft_color::SampleType::U8, true);
        let mut s = spec();
        s.spacing_x = 4.0;
        s.spacing_y = 4.0;
        let params = FilterParams::Screentone { spec: spec_with(s) };
        let raw = render_preview(fmt, region, region, &params, None);

        let mut mask = LayerMask::reveal_all();
        let w = region.width();
        let hidden = DocRect::new(region.x0, region.y0, region.x0 + (w / 2) as i32, region.y1);
        mask.surface.write_region(hidden, &vec![0.0; w as usize / 2 * region.height() as usize]);
        let gated = render_preview(fmt, region, region, &params, Some((&mask.surface, region)));
        let mid_y = region.y0 + (region.height() / 2) as i32;
        assert_eq!(gated.get(region.x0 + 1, mid_y)[3], 0.0, "the hidden half shows nothing");
        assert_eq!(gated.get(region.x0 + (w / 2) as i32 + 1, mid_y), raw.get(region.x0 + (w / 2) as i32 + 1, mid_y), "the revealed half matches the raw sheet",);
    }

    #[test]
    fn detach_clears_the_running_drag() {
        let mut g = ScreentoneGizmoState::default();
        assert!(!g.has_active_drag());
        g.drag = Some(Drag { kind: Handle::Origin, start_doc: [0.0, 0.0], spec: spec() });
        assert!(g.has_active_drag());
        g.detach();
        assert!(!g.has_active_drag(), "detach drops the drag");
    }
}
