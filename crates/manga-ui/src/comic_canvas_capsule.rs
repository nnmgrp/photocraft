//! ✨ Canvas Capsule (CraftBar): floating, context-aware toolbar drawn on top of the canvas.
//!
//! This is the `manga-ui` port of the `ui-egui` capsule. It never touches the host app or the
//! engine: theme tokens come from [`crate::theme::Tokens`], document state is supplied through
//! [`CapsuleView`], icons and the inline type controls are delegated to [`CapsuleCallbacks`],
//! and every action is returned as a deferred [`CapsuleOutput`] for the host to dispatch.
//!
//! Workflows, in precedence order:
//! - **Screentone layer active**: LPI / angle / line-art edits plus Sync, Panel, Freeze and,
//!   when a selection coexists, the usual selection actions side-by-side.
//! - **Type tool / text layer**: the host draws its inline text controls via the callback.
//! - **Selection active**: generative-fill prompt, AI Remove, Invert / Feather / Deselect.
//! - **Idle**: Select Subject and Remove Background.
//!
//! The capsule can be pinned to the viewport bottom or float near the current context, be
//! dragged to a custom offset, fade via an opacity slider, and choose the generative AI backend.

use egui::{Color32, CornerRadius, DragValue, Frame, Id, Margin, Order, Pos2, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use photocraft_doc::sfx::{SfxData, SfxFreeTransform};
use photocraft_doc::vector::BalloonTextFitMode;
use photocraft_doc::{Document, LayerContent, LayerId};
use photocraft_geom::Rect as DocRect;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::comic_screentone_panel::{ScreentoneTargetInfo, ScreentoneUi, freeze_layer_command, sync_spec_command};
use crate::theme::Tokens;
use crate::{BridgeDocId, OutCommand};

/// The Window-menu id of this capsule panel.
pub const PANEL: &str = "window.panel.canvasCapsule";

/// Choice of AI backend used for generative fill and inpainting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum AiBackendChoice {
    #[default]
    DirectMl,
    SdCppRemote,
    LazySdUi,
}

impl AiBackendChoice {
    /// Human-readable label shown in the capsule settings menu.
    pub fn label(self) -> &'static str {
        match self {
            Self::DirectMl => "DirectML (Local ONNX)",
            Self::SdCppRemote => "SD.CPP / Tunnel (Remote)",
            Self::LazySdUi => "LazySD-UI (Local)",
        }
    }
}

/// Persistent and runtime state of the Canvas Capsule.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CanvasCapsuleState {
    /// Pinned to the bottom-center of the viewport, or floating near the active context.
    pub pinned: bool,
    /// User drag offset relative to the calculated base position.
    pub custom_offset: Option<[f32; 2]>,
    /// Background opacity (0.3 ..= 1.0, default 0.95).
    pub opacity: f32,
    /// Prompt text entered for generative fill or inpainting.
    pub prompt: String,
    /// Configured AI backend.
    pub backend: AiBackendChoice,
    /// Remote HTTP endpoint for SD.CPP or Cloudflare Tunnel.
    pub remote_endpoint: String,
    /// Temporary feedback or progress message.
    #[serde(skip)]
    pub status_text: Option<String>,
}

impl Default for CanvasCapsuleState {
    fn default() -> Self {
        Self {
            pinned: false,
            custom_offset: None,
            opacity: 0.95,
            prompt: String::new(),
            backend: AiBackendChoice::DirectMl,
            remote_endpoint: "http://127.0.0.1:8080".to_string(),
            status_text: None,
        }
    }
}

/// Read-only view of the host supplied to the capsule for one frame.
pub struct CapsuleView<'a> {
    pub visible: bool,
    pub view_xform: crate::view::CanvasViewXform,
    pub doc: Option<&'a Document>,
    pub doc_id: Option<BridgeDocId>,
    pub active_layer: Option<LayerId>,
    pub has_selection: bool,
    /// Stable tool identifier: `"type"`, `"marquee"`, `"lasso"`, `"magic_wand"`,
    /// `"remove"`, `"object_selection"`, `"move"`, `"screentone"`, ….
    pub tool_name: &'static str,
    pub is_desktop: bool,
    pub screentone_target: Option<ScreentoneTargetInfo>,
    /// `(balloon layer, chamber id, current mode)` when a balloon is active or the active text
    /// layer is linked to one. Shows the compact `Safe | Outer | Contour` strip.
    pub balloon_fit_mode: Option<(LayerId, u32, BalloonTextFitMode)>,
    /// Active SFX layer payload when an SFX layer is selected. Shows exact Free Transform controls.
    pub sfx_target: Option<(BridgeDocId, LayerId, SfxData)>,
}

/// Actions the host should perform after [`show`] returns.
#[derive(Default)]
pub struct CapsuleOutput {
    pub commands: Vec<OutCommand>,
    pub action_remove_selection: bool,
    pub open_screentone_panel: bool,
    pub detach_screentone_gizmo: bool,
    pub notice: Option<String>,
    /// The user picked a new text fit mode for this balloon chamber.
    pub set_balloon_fit_mode: Option<(LayerId, u32, BalloonTextFitMode)>,
    /// Set when an interactive SFX drag emitted a `preview: true` update from the capsule.
    pub sfx_preview_pending: Option<(BridgeDocId, LayerId)>,
}

/// Host-provided drawing hooks the capsule cannot implement without L6 dependencies.
pub trait CapsuleCallbacks {
    fn paint_icon(&mut self, ui: &mut Ui, rect: Rect, name: &str, size: f32, color: Color32);
    fn icon_button(&mut self, ui: &mut Ui, name: &str, size: f32, active: bool, tip: &str) -> egui::Response;
    fn draw_type_controls(&mut self, ui: &mut Ui);
}

/// Which contextual control set the capsule shows this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CapsuleMode {
    Idle,
    Selection,
    Type,
    Screentone,
    Balloon,
    Sfx,
}

/// Resolves the active workflow, with screentone and SFX taking precedence over idle/selection.
fn capsule_mode(view: &CapsuleView<'_>) -> CapsuleMode {
    let layer = view.active_layer.and_then(|id| view.doc.and_then(|doc| doc.layer(id)));
    let is_screentone = view.screentone_target.is_some() || layer.is_some_and(|l| l.is_screentone());
    if is_screentone {
        return CapsuleMode::Screentone;
    }
    if view.sfx_target.is_some() || layer.is_some_and(|l| l.sfx.is_some()) {
        return CapsuleMode::Sfx;
    }
    let is_text_layer = layer.is_some_and(|l| matches!(l.content, LayerContent::Text(_)));
    if view.tool_name == "type" || is_text_layer {
        return CapsuleMode::Type;
    }
    let is_balloon = layer.is_some_and(|l| matches!(&l.content, LayerContent::Shape(sh) if matches!(sh.live, Some(photocraft_doc::LiveShape::BalloonGroup { .. }))));
    if is_balloon || view.balloon_fit_mode.is_some() {
        return CapsuleMode::Balloon;
    }
    if view.has_selection { CapsuleMode::Selection } else { CapsuleMode::Idle }
}

/// Compact `Safe | Outer | Contour` strip, shown for a balloon layer and for a linked text layer.
fn draw_balloon_fit(ui: &mut Ui, view: &CapsuleView<'_>, out: &mut CapsuleOutput) {
    let Some((layer_id, balloon_id, current)) = view.balloon_fit_mode else { return };
    ui.label(RichText::new(tl!("Text fit:")).small());
    for (mode, label) in [
        (BalloonTextFitMode::Safe, tl!("Safe")),
        (BalloonTextFitMode::Outer, tl!("Outer")),
        (BalloonTextFitMode::Contour, tl!("Contour")),
    ] {
        if ui.selectable_label(mode == current, label).on_hover_text(tl!("Refit the linked lettering now")).clicked() && mode != current {
            out.set_balloon_fit_mode = Some((layer_id, balloon_id, mode));
        }
    }
}

/// Selection bounds in document space, when a non-empty selection exists.
fn selection_bounds(view: &CapsuleView<'_>) -> Option<DocRect> {
    view.doc.and_then(|doc| doc.selection.as_ref()).map(|selection| selection.content_bounds()).filter(|bounds| !bounds.is_empty())
}

/// Bounds of the active text layer's cached layout, when available.
fn text_bounds(view: &CapsuleView<'_>) -> Option<DocRect> {
    view.active_layer
        .and_then(|id| view.doc.and_then(|doc| doc.layer(id)))
        .and_then(|layer| match &layer.content {
            LayerContent::Text(text) => text.cache.as_ref().map(|cache| cache.content_bounds()),
            _ => None,
        })
        .filter(|bounds| !bounds.is_empty())
}

/// Places the capsule just below (or above) a document-space bounding box.
fn follow_bounds(view: &CapsuleView<'_>, bounds: DocRect, rect: Rect) -> Pos2 {
    let xf = &view.view_xform;
    let s_min = xf.to_screen(bounds.x0 as f32, bounds.y0 as f32);
    let s_max = xf.to_screen(bounds.x1 as f32, bounds.y1 as f32);
    let min_y = s_min.y.min(s_max.y);
    let max_y = s_min.y.max(s_max.y);
    let center_x = (s_min.x + s_max.x) * 0.5;
    let mut target_y = max_y + 16.0;
    if target_y > rect.max.y - 60.0 {
        target_y = (min_y - 48.0).max(rect.min.y + 36.0);
    }
    pos2(center_x, target_y)
}

/// Bounds of the active SFX layer's rendered pixels, when available.
fn sfx_bounds(view: &CapsuleView<'_>) -> Option<DocRect> {
    let layer = view.active_layer.and_then(|id| view.doc.and_then(|doc| doc.layer(id)))?;
    let bounds = match &layer.content {
        LayerContent::Group(g) => g
            .children
            .iter()
            .filter_map(|c| c.surface().map(|s| s.content_bounds()))
            .fold(DocRect::EMPTY, |a, b| a.union(&b)),
        _ => layer.surface().map_or(DocRect::EMPTY, |s| s.content_bounds()),
    };
    (!bounds.is_empty()).then_some(bounds)
}

/// Compact exact Free Transform controls for an active SFX layer (`SX / SY / Rot / Skew / Persp`).
fn draw_sfx_ft(ui: &mut Ui, view: &CapsuleView<'_>, out: &mut CapsuleOutput) {
    let Some((doc_id, layer_id, orig)) = view.sfx_target.as_ref() else { return };
    let mut sfx = orig.clone();
    let ft = &mut sfx.free_transform;
    let mut changed = false;

    ui.label(RichText::new("SFX FT").strong().color(Color32::from_rgb(125, 211, 252)));
    let (mut sx, mut sy) = (ft.scale_x as f32, ft.scale_y as f32);
    let sx_l = ui.label(RichText::new("SX:").small());
    if ui.add(DragValue::new(&mut sx).speed(0.02).range(0.1..=12.0).max_decimals(2).suffix("x")).labelled_by(sx_l.id).changed() {
        ft.scale_x = f64::from(sx);
        changed = true;
    }
    let sy_l = ui.label(RichText::new("SY:").small());
    if ui.add(DragValue::new(&mut sy).speed(0.02).range(0.1..=12.0).max_decimals(2).suffix("x")).labelled_by(sy_l.id).changed() {
        ft.scale_y = f64::from(sy);
        changed = true;
    }

    let mut rot = ft.rotation_deg as f32;
    let r_l = ui.label(RichText::new("Rot:").small());
    if ui.add(DragValue::new(&mut rot).speed(0.5).range(-360.0..=360.0).max_decimals(1).suffix("°")).labelled_by(r_l.id).changed() {
        ft.rotation_deg = f64::from(rot);
        changed = true;
    }

    let (mut skx, mut sky) = (ft.skew_x_deg as f32, ft.skew_y_deg as f32);
    let skx_l = ui.label(RichText::new("SkX:").small());
    if ui.add(DragValue::new(&mut skx).speed(0.5).range(-75.0..=75.0).max_decimals(1).suffix("°")).labelled_by(skx_l.id).changed() {
        ft.skew_x_deg = f64::from(skx);
        changed = true;
    }
    let sky_l = ui.label(RichText::new("SkY:").small());
    if ui.add(DragValue::new(&mut sky).speed(0.5).range(-75.0..=75.0).max_decimals(1).suffix("°")).labelled_by(sky_l.id).changed() {
        ft.skew_y_deg = f64::from(sky);
        changed = true;
    }

    let (mut px, mut py) = (ft.perspective_x as f32, ft.perspective_y as f32);
    let px_l = ui.label(RichText::new("PX:").small());
    if ui.add(DragValue::new(&mut px).speed(0.005).range(-0.75..=0.75).max_decimals(2)).labelled_by(px_l.id).changed() {
        ft.perspective_x = f64::from(px);
        changed = true;
    }
    let py_l = ui.label(RichText::new("PY:").small());
    if ui.add(DragValue::new(&mut py).speed(0.005).range(-0.75..=0.75).max_decimals(2)).labelled_by(py_l.id).changed() {
        ft.perspective_y = f64::from(py);
        changed = true;
    }

    if ui.small_button("Reset").on_hover_text("Reset SFX Free Transform").clicked() {
        *ft = SfxFreeTransform::default();
        changed = true;
    }

    if changed {
        sfx.validate();
        let pointer_down = ui.ctx().input(|i| i.pointer.primary_down());
        let coalesce = format!("sfx-capsule:{}:{}", doc_id.0, layer_id.0);
        if pointer_down {
            out.sfx_preview_pending = Some((*doc_id, *layer_id));
        }
        out.commands.push(OutCommand::new(
            "sfx.update",
            json!({
                "layer": layer_id.0,
                "sfx": sfx,
                "coalesce": coalesce,
                "preview": pointer_down,
            }),
        ));
    }
}

/// Computes the final, clamped capsule position from its state and the current context.
fn anchor_position(state: &CanvasCapsuleState, view: &CapsuleView<'_>, mode: CapsuleMode) -> Pos2 {
    let rect = view.view_xform.rect;
    let mut base = pos2(rect.center().x, rect.max.y - 56.0);
    if !state.pinned {
        if view.has_selection {
            if let Some(bounds) = selection_bounds(view) {
                base = follow_bounds(view, bounds, rect);
            }
        } else if mode == CapsuleMode::Sfx
            && let Some(bounds) = sfx_bounds(view)
        {
            base = follow_bounds(view, bounds, rect);
        } else if mode == CapsuleMode::Type
            && let Some(bounds) = text_bounds(view)
        {
            base = follow_bounds(view, bounds, rect);
        }
    }
    if let Some([dx, dy]) = state.custom_offset {
        base += vec2(dx, dy);
    }
    let half_w = 260.0;
    let min_x = rect.min.x + half_w + 10.0;
    let max_x = (rect.max.x - half_w - 10.0).max(min_x);
    let min_y = rect.min.y + 24.0;
    let max_y = (rect.max.y - 48.0).max(min_y);
    pos2(base.x.clamp(min_x, max_x), base.y.clamp(min_y, max_y))
}

/// Draws the six-dot drag handle and folds drag deltas into the user offset.
fn draw_grip(ui: &mut Ui, state: &mut CanvasCapsuleState, t: Tokens) {
    let (grip_rect, grip_resp) = ui.allocate_exact_size(vec2(14.0, 20.0), Sense::click_and_drag());
    if grip_resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    if grip_resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        let delta = grip_resp.drag_delta();
        let mut offset = state.custom_offset.unwrap_or([0.0, 0.0]);
        offset[0] += delta.x;
        offset[1] += delta.y;
        state.custom_offset = Some(offset);
    }
    for i in 0..3 {
        let cy = grip_rect.min.y + 4.0 + (i as f32) * 6.0;
        ui.painter().circle_filled(pos2(grip_rect.min.x + 4.0, cy), 1.5, t.text_dim);
        ui.painter().circle_filled(pos2(grip_rect.min.x + 10.0, cy), 1.5, t.text_dim);
    }
}

/// Draws the decorative sparkle icon through the host callback.
fn draw_sparkle(ui: &mut Ui, callbacks: &mut dyn CapsuleCallbacks) {
    let (icon_rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
    callbacks.paint_icon(ui, icon_rect, "wand-sparkles", 16.0, Color32::from_rgb(255, 215, 80));
}

/// Pushes a `layer.screentone` command restyling the active tone layer, if it changed.
fn push_sync(screentone: &ScreentoneUi, view: &CapsuleView<'_>, out: &mut CapsuleOutput) {
    if let Some(target) = view.screentone_target.as_ref()
        && let Some(command) = sync_spec_command(screentone, target)
    {
        out.commands.push(command);
    }
}

/// Invert / Feather / Deselect actions shared by the screentone and selection contexts.
fn draw_selection_actions(ui: &mut Ui, out: &mut CapsuleOutput) {
    if ui.button(tl!("Invert")).on_hover_text(tl!("Invert selection (Cmd+Shift+I)")).clicked() {
        out.commands.push(OutCommand::new("select.inverse", json!({})));
    }
    if ui.button(tl!("Feather")).on_hover_text(tl!("Feather selection edges")).clicked() {
        out.commands.push(OutCommand::new("select.modify.feather", json!({"radius": 5.0})));
    }
    if ui.button(tl!("Deselect")).on_hover_text(tl!("Deselect (Cmd+D)")).clicked() {
        out.commands.push(OutCommand::new("select.deselect", json!({})));
    }
}

/// Screentone-layer controls: quick tone edits, Sync, full panel, Freeze and selection actions.
fn draw_screentone(ui: &mut Ui, screentone: &mut ScreentoneUi, view: &CapsuleView<'_>, out: &mut CapsuleOutput) {
    ui.label(RichText::new(tl!("Tone")).strong().color(Color32::from_rgb(100, 200, 255)));
    let mut changed = false;
    let lpi_label = ui.label(tl!("LPI:"));
    changed |= ui.add(DragValue::new(&mut screentone.spec.spacing_x).speed(0.2).range(2.0..=64.0)).labelled_by(lpi_label.id).changed();
    let angle_label = ui.label(tl!("Angle:"));
    let angle = ui.add(DragValue::new(&mut screentone.spec.angle).speed(1.0).suffix("°")).labelled_by(angle_label.id);
    if angle.changed() {
        screentone.spec.angle = screentone.spec.angle.rem_euclid(360.0);
        changed = true;
    }
    changed |= ui.checkbox(&mut screentone.spec.preserve_lines, tl!("LineArt")).changed();
    if changed {
        push_sync(screentone, view, out);
    }
    ui.separator();
    if ui.button(tl!("Sync")).on_hover_text(tl!("Restyle the active tone layer with these settings")).clicked() {
        push_sync(screentone, view, out);
    }
    if ui.button(tl!("Panel")).on_hover_text(tl!("Open the full Screentones panel")).clicked() {
        out.open_screentone_panel = true;
    }
    if ui.button(tl!("Freeze")).on_hover_text(tl!("Bake screentone into a standard raster layer")).clicked()
        && let Some(target) = view.screentone_target.as_ref()
    {
        out.commands.push(freeze_layer_command(target.layer_id));
        out.detach_screentone_gizmo = true;
    }
    if view.has_selection {
        ui.separator();
        draw_selection_actions(ui, out);
    }
}

/// Selection context: generative fill, AI Remove and the shared selection actions.
fn draw_selection(ui: &mut Ui, state: &mut CanvasCapsuleState, out: &mut CapsuleOutput) {
    let edit = ui.add_sized([170.0, 22.0], egui::TextEdit::singleline(&mut state.prompt).hint_text(tl!("Generative Fill / Prompt...")));
    let prompt_non_empty = !state.prompt.trim().is_empty();
    let enter_pressed = edit.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
    let clicked_generate = ui.add_enabled(prompt_non_empty, egui::Button::new(tl!("Generate"))).clicked();
    if clicked_generate || (prompt_non_empty && enter_pressed) {
        let prompt_text = state.prompt.trim().to_owned();
        match state.backend {
            AiBackendChoice::DirectMl => {
                out.commands.push(OutCommand::new("edit.generativeFill", json!({"prompt": prompt_text, "model": "diffusion"})));
            }
            AiBackendChoice::SdCppRemote | AiBackendChoice::LazySdUi => {
                out.notice = Some(format!("Dispatched to {}: \"{prompt_text}\"", state.backend.label()));
            }
        }
    }
    if ui.button(tl!("AI Remove")).on_hover_text(tl!("Remove selected object with AI (DirectML LaMa / Nano Banana)")).clicked() {
        out.action_remove_selection = true;
    }
    ui.separator();
    draw_selection_actions(ui, out);
}

/// Idle context: quick subject selection and background removal.
fn draw_idle(ui: &mut Ui, out: &mut CapsuleOutput) {
    if ui.button(tl!("Select Subject")).on_hover_text(tl!("Automatically select the prominent subject")).clicked() {
        out.commands.push(OutCommand::new("select.subject", json!({})));
    }
    if ui.button(tl!("Remove Background")).on_hover_text(tl!("Automatically remove the background")).clicked() {
        out.commands.push(OutCommand::new("select.subject", json!({})));
        out.commands.push(OutCommand::new("select.inverse", json!({})));
        out.commands.push(OutCommand::new("edit.clear", json!({})));
    }
}

/// Quick pin / unpin button.
fn draw_pin(ui: &mut Ui, state: &mut CanvasCapsuleState, callbacks: &mut dyn CapsuleCallbacks) {
    ui.separator();
    let icon = if state.pinned { "lock" } else { "lock-open" };
    let tip = if state.pinned {
        tl!("Pinned to viewport bottom. Click to float near context (selection / text)")
    } else {
        tl!("Floating near context. Click to pin to viewport bottom")
    };
    if callbacks.icon_button(ui, icon, 16.0, state.pinned, tip).clicked() {
        state.pinned = !state.pinned;
        if !state.pinned {
            state.custom_offset = None;
        }
    }
}

/// The "···" settings menu: pinning, position, opacity, AI backend and hiding the bar.
fn draw_settings(ui: &mut Ui, state: &mut CanvasCapsuleState, t: Tokens, out: &mut CapsuleOutput) {
    let more = ui.button("···").on_hover_text(tl!("Canvas Capsule settings"));
    egui::Popup::menu(&more).show(|ui| {
        ui.set_min_width(200.0);
        ui.label(RichText::new(tl!("Canvas Capsule")).strong());
        ui.add_space(4.0);
        ui.checkbox(&mut state.pinned, tl!("Pin to Bottom of Viewport"));
        if ui.button(tl!("Reset Position")).clicked() {
            state.custom_offset = None;
        }
        ui.add_space(4.0);
        ui.add(egui::Slider::new(&mut state.opacity, 0.3..=1.0).text(tl!("Opacity")).custom_formatter(|value, _| format!("{:.0}%", value * 100.0)));
        ui.separator();
        ui.label(RichText::new(tl!("AI Engine")).small().color(t.text_dim));
        ui.radio_value(&mut state.backend, AiBackendChoice::DirectMl, AiBackendChoice::DirectMl.label());
        ui.radio_value(&mut state.backend, AiBackendChoice::SdCppRemote, "SD.CPP / Tunnel");
        ui.radio_value(&mut state.backend, AiBackendChoice::LazySdUi, "LazySD-UI (Local)");
        ui.separator();
        if ui.button(tl!("Hide Bar")).clicked() {
            out.commands.push(OutCommand::new(PANEL, json!({})));
        }
    });
}

/// Builds the capsule's translucent card frame from the current theme and opacity.
fn capsule_frame(t: Tokens, opacity: f32) -> Frame {
    Frame {
        inner_margin: Margin::symmetric(10, 6),
        corner_radius: CornerRadius::same(16),
        fill: t.card.gamma_multiply(opacity),
        stroke: Stroke::new(1.0, t.card_border.gamma_multiply(opacity)),
        shadow: egui::Shadow { offset: [0, 4], blur: 16, spread: 0, color: t.shadow.gamma_multiply(opacity) },
        ..Default::default()
    }
}

/// Dispatches to the control set for the active workflow.
fn draw_context(
    ui: &mut Ui,
    mode: CapsuleMode,
    state: &mut CanvasCapsuleState,
    screentone: &mut ScreentoneUi,
    view: &CapsuleView<'_>,
    callbacks: &mut dyn CapsuleCallbacks,
    out: &mut CapsuleOutput,
) {
    match mode {
        CapsuleMode::Screentone => draw_screentone(ui, screentone, view, out),
        CapsuleMode::Sfx => draw_sfx_ft(ui, view, out),
        CapsuleMode::Type => {
            callbacks.draw_type_controls(ui);
            if view.balloon_fit_mode.is_some() {
                ui.separator();
                draw_balloon_fit(ui, view, out);
            }
        }
        CapsuleMode::Balloon => draw_balloon_fit(ui, view, out),
        CapsuleMode::Selection => draw_selection(ui, state, out),
        CapsuleMode::Idle => draw_idle(ui, out),
    }
}

/// Renders the floating Canvas Capsule and returns the actions the host should perform.
pub fn show(
    ctx: &egui::Context,
    state: &mut CanvasCapsuleState,
    screentone: &mut ScreentoneUi,
    view: &CapsuleView<'_>,
    callbacks: &mut dyn CapsuleCallbacks,
) -> CapsuleOutput {
    let mut out = CapsuleOutput::default();
    if !view.visible {
        return out;
    }
    let t = Tokens::get(ctx);
    let mode = capsule_mode(view);
    let capsule_pos = anchor_position(state, view, mode);
    let frame = capsule_frame(t, state.opacity.clamp(0.3, 1.0));
    egui::Area::new(Id::unique("canvas_capsule_area")).order(Order::Middle).pivot(egui::Align2::CENTER_TOP).fixed_pos(capsule_pos).show(ctx, |ui| {
        frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 0.0);
                draw_grip(ui, state, t);
                draw_sparkle(ui, callbacks);
                ui.separator();
                draw_context(ui, mode, state, screentone, view, callbacks, &mut out);
                draw_pin(ui, state, callbacks);
                draw_settings(ui, state, t, &mut out);
            });
        });
    });
    out
}

#[cfg(test)]
mod tests {
    use egui::{Context, Rect, Response, Ui, pos2, vec2};
    use photocraft_doc::LayerId;

    use super::{AiBackendChoice, CapsuleMode, CanvasCapsuleState, CapsuleCallbacks, CapsuleOutput, CapsuleView, PANEL, capsule_mode, show};
    use crate::BridgeDocId;
    use crate::comic_screentone_panel::{ScreentoneTargetInfo, ScreentoneUi};
    use crate::view::CanvasViewXform;

    struct TestCallbacks;

    impl CapsuleCallbacks for TestCallbacks {
        fn paint_icon(&mut self, ui: &mut Ui, rect: Rect, _name: &str, _size: f32, color: egui::Color32) {
            ui.painter().rect_filled(rect, 0.0, color);
        }

        fn icon_button(&mut self, ui: &mut Ui, _name: &str, _size: f32, _active: bool, _tip: &str) -> Response {
            ui.button("icon")
        }

        fn draw_type_controls(&mut self, ui: &mut Ui) {
            ui.label("Inline type controls");
        }
    }

    fn sample_view<'a>(selection: bool, tool: &'static str, target: Option<ScreentoneTargetInfo>) -> CapsuleView<'a> {
        let xform = CanvasViewXform {
            rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0)),
            zoom: 1.0,
            center: [400.0, 300.0],
            flip: false,
            rotation: 0.0,
            aspect: 1.0,
        };
        CapsuleView {
            visible: true,
            view_xform: xform,
            doc: None,
            doc_id: Some(BridgeDocId(1)),
            active_layer: Some(LayerId(1)),
            has_selection: selection,
            tool_name: tool,
            is_desktop: true,
            screentone_target: target,
            balloon_fit_mode: None,
            sfx_target: None,
        }
    }

    fn tone_target() -> ScreentoneTargetInfo {
        ScreentoneTargetInfo { doc_id: BridgeDocId(1), layer_id: LayerId(1), spec: ScreentoneUi::default().spec }
    }

    #[test]
    fn default_state_is_unpinned_with_directml_and_full_opacity() {
        let state = CanvasCapsuleState::default();
        assert!(!state.pinned);
        assert_eq!(state.opacity, 0.95);
        assert_eq!(state.backend, AiBackendChoice::DirectMl);
        assert!(state.custom_offset.is_none());
        assert!(state.prompt.is_empty());
        assert_eq!(state.remote_endpoint, "http://127.0.0.1:8080");
        assert!(state.status_text.is_none());
        assert_eq!(PANEL, "window.panel.canvasCapsule");
    }

    #[test]
    fn ai_backend_labels_are_human_readable() {
        assert_eq!(AiBackendChoice::DirectMl.label(), "DirectML (Local ONNX)");
        assert_eq!(AiBackendChoice::SdCppRemote.label(), "SD.CPP / Tunnel (Remote)");
        assert_eq!(AiBackendChoice::LazySdUi.label(), "LazySD-UI (Local)");
    }

    #[test]
    fn a_balloon_context_shows_the_fit_strip() {
        let mut view = sample_view(false, "move", None);
        assert_eq!(capsule_mode(&view), CapsuleMode::Idle);
        view.balloon_fit_mode = Some((LayerId(1), 3, photocraft_doc::vector::BalloonTextFitMode::Contour));
        assert_eq!(capsule_mode(&view), CapsuleMode::Balloon);
        // The strip is also present in the type context, next to the inline type controls.
        view.tool_name = "type";
        assert_eq!(capsule_mode(&view), CapsuleMode::Type);
        let out = CapsuleOutput::default();
        assert!(out.set_balloon_fit_mode.is_none());
    }

    #[test]
    fn hidden_capsule_returns_an_empty_output() {
        let ctx = Context::default();
        let mut state = CanvasCapsuleState::default();
        let mut tone = ScreentoneUi::default();
        let mut view = sample_view(true, "move", Some(tone_target()));
        view.visible = false;
        let mut callbacks = TestCallbacks;
        let mut result = CapsuleOutput::default();
        let mut full = ctx.run_ui(Default::default(), |ui| {
            result = show(ui.ctx(), &mut state, &mut tone, &view, &mut callbacks);
        });
        full.textures_delta.clear();
        assert!(result.commands.is_empty());
        assert!(!result.action_remove_selection);
        assert!(!result.open_screentone_panel);
        assert!(!result.detach_screentone_gizmo);
        assert!(result.notice.is_none());
    }

    #[test]
    fn capsule_renders_in_all_workflow_contexts() {
        let ctx = Context::default();
        let mut state = CanvasCapsuleState::default();
        let mut tone = ScreentoneUi::default();
        let mut callbacks = TestCallbacks;

        // Idle.
        let view = sample_view(false, "move", None);
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let _ = show(ui.ctx(), &mut state, &mut tone, &view, &mut callbacks);
        });
        out.textures_delta.clear();

        // Selection.
        let view = sample_view(true, "marquee", None);
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let _ = show(ui.ctx(), &mut state, &mut tone, &view, &mut callbacks);
        });
        out.textures_delta.clear();

        // Type tool.
        let view = sample_view(false, "type", None);
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let _ = show(ui.ctx(), &mut state, &mut tone, &view, &mut callbacks);
        });
        out.textures_delta.clear();

        // Screentone layer with a selection: tone and selection controls co-exist.
        let view = sample_view(true, "move", Some(tone_target()));
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let _ = show(ui.ctx(), &mut state, &mut tone, &view, &mut callbacks);
        });
        out.textures_delta.clear();

        // SFX layer: shows exact Free Transform controls.
        let mut sfx_view = sample_view(false, "move", None);
        sfx_view.sfx_target = Some((BridgeDocId(1), LayerId(1), photocraft_doc::sfx::SfxData::default()));
        assert_eq!(capsule_mode(&sfx_view), CapsuleMode::Sfx);
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let _ = show(ui.ctx(), &mut state, &mut tone, &sfx_view, &mut callbacks);
        });
        out.textures_delta.clear();
    }
}
