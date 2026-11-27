//! Non-destructive Manga SFX Panel for `Group::Manga` (`crates/manga-ui`).
//!
//! Full `egui 0.37` port of the Manga SFX & Diagonal Gizmo dock UI.
//! Emits deferred [`OutCommand`]s (`sfx.create`, `sfx.update`, `sfx.convertFromText`,
//! `sfx.bakeToRaster`, `sfx.presets.*`) and syncs bi-directionally with [`SfxGizmoState`].

use egui::{Color32, RichText, Stroke, Ui, vec2};
use photocraft_algo::{DotShape, PatternType};
use photocraft_doc::sfx::{
    MAX_SFX_BLEND_EFFECTS, MAX_SFX_SPLINE_POINTS, ScreentoneSpec, SfxBlendEffect, SfxBlending, SfxChannelBlend, SfxData, SfxDistress,
    SfxDryBrushEngine, SfxDryBrushSpec, SfxFillSource, SfxFreeTransform, SfxGlyphTransform, SfxLineEffect, SfxLineJoin, SfxLineStyle,
    SfxMeshInterpolation, SfxPreset, SfxShibufudeProfile, SfxShtackFillMode, SfxShtackFillSpec, SfxShtackFlowOvershoot,
    SfxShtackOvershootMode, SfxShtackVariant, SfxSpikeShape, SfxStrokePosition, SfxStylePass, SfxSugomiDirection, SfxV3EdgeMode, SfxWarpMode,
};
use photocraft_doc::{Color, LayerId};
use photocraft_manga_core::sfx_geom::{PreparedSfx, sample_catmull_rom};
use serde_json::{Value, json};

use crate::sfx_gizmo::{SfxGizmoState, reset_mesh};
use crate::theme::Tokens;
use crate::{BridgeDocId, OutCommand};

const SCREENTONE_PATTERNS: [&str; 4] = ["dots", "lines", "crosshatch", "noise"];
const SCREENTONE_SHAPES: [&str; 4] = ["circle", "square", "diamond", "hexagon"];
const GLYPH_TOOL_KEY: &str = "photocraft-manga-ui-sfx-glyph-tool";
const FLOW_NODES_KEY: &str = "photocraft-manga-ui-sfx-flow-nodes";

const ACCENT_SKY: Color32 = Color32::from_rgb(2, 132, 199);
const ACCENT_AMBER: Color32 = Color32::from_rgb(217, 119, 6);
const TEXT_AMBER: Color32 = Color32::from_rgb(252, 211, 77);
const TEXT_SKY: Color32 = Color32::from_rgb(125, 211, 252);
const TEXT_CYAN: Color32 = Color32::from_rgb(103, 232, 249);
const TEXT_EMERALD: Color32 = Color32::from_rgb(110, 231, 183);
const TEXT_ROSE: Color32 = Color32::from_rgb(253, 164, 175);

/// Active sub-tool inside `SfxWarpMode::PerGlyph` (`Move` | `Skew` | `FT 4-Corner`),
/// shared with `sfx_gizmo` through `egui::Context` temp storage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SfxGlyphTool {
    #[default]
    Move,
    Skew,
    FreeTransform,
}

impl SfxGlyphTool {
    pub fn get(ctx: &egui::Context) -> Self {
        ctx.data(|d| d.get_temp::<Self>(egui::Id::unique(GLYPH_TOOL_KEY))).unwrap_or_default()
    }

    pub fn set(ctx: &egui::Context, tool: Self) {
        ctx.data_mut(|d| d.insert_temp(egui::Id::unique(GLYPH_TOOL_KEY), tool));
    }
}

/// Whether per-endpoint SAI needle gizmo dots are shown on the canvas (UI-only, default `true`).
pub fn flow_nodes_visible(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(egui::Id::unique(FLOW_NODES_KEY))).unwrap_or(true)
}

pub fn set_flow_nodes_visible(ctx: &egui::Context, visible: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique(FLOW_NODES_KEY), visible));
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DockTab {
    #[default]
    Sfx,
    Blending,
    Color,
}

/// Read-only view of the active layer & preset library supplied by the host app.
pub struct SfxTargetInfo<'a> {
    pub doc_id: Option<BridgeDocId>,
    pub layer_id: Option<LayerId>,
    pub data: Option<SfxData>,
    pub active_is_text: bool,
    pub active_is_smart: bool,
    /// Active document DPI (scales pixel-based outline & effect sliders accurately up to 2400 DPI).
    pub doc_dpi: f32,
    /// True when the hardware `wgpu` SFX pipeline is active for live preview / commits.
    pub gpu_active: bool,
    pub presets: &'a [SfxPreset],
}

/// Persistent panel UI state stored inside [`crate::MangaUiState`].
#[derive(Default)]
pub struct SfxPanelState {
    pub draft: SfxData,
    pub bound_target: Option<(BridgeDocId, LayerId)>,
    pub synced_data: Option<SfxData>,
    pub preset_name: String,
    /// Cached active document DPI for 2400-DPI-aware slider ranges.
    pub doc_dpi: f32,
    /// True while a slider/colour drag is in progress: the next committed (non-preview) render
    /// is deferred until the pointer is released. Transient only — `SfxPanelState` has no serde
    /// derives and is never persisted, so this field is intentionally not part of any saved state.
    pub pending_full_render: bool,
    /// `(doc_id, layer_id)` that needs a final `preview: false` commit once the pointer is released
    /// (shared across the SFX dock, standard Character dock, and Canvas Bar).
    pub pending_full_target: Option<(BridgeDocId, LayerId)>,
    active_tab: DockTab,
    /// Selected index inside `draft.blending.effects` for the Photoshop-style FX inspector.
    selected_blend_fx: Option<usize>,
}

impl SfxPanelState {
    /// Synchronizes `draft` from the active SFX layer when selection or undo/redo updates it.
    pub fn sync_from_host(&mut self, target: &SfxTargetInfo<'_>) {
        let next_key = target.layer_id.map(|id| (target.doc_id.unwrap_or(BridgeDocId(0)), id));
        match (&target.data, next_key) {
            (Some(data), Some(key)) => {
                if self.bound_target != Some(key) || self.synced_data.as_ref() != Some(data) {
                    self.draft = data.clone();
                    self.bound_target = Some(key);
                    self.synced_data = Some(data.clone());
                }
            }
            _ => {
                self.bound_target = None;
                self.synced_data = None;
            }
        }
    }
}

/// Deferred command output emitted by [`panel`].
#[derive(Default)]
pub struct SfxPanelOutput {
    pub commands: Vec<OutCommand>,
}

fn color_to_c32(c: Color) -> Color32 {
    let rgb = c.to_rgb();
    Color32::from_rgba_unmultiplied(
        (rgb[0].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
        (rgb[1].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
        (rgb[2].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
        (c.alpha.clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
    )
}

fn c32_to_color(c: Color32) -> Color {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    Color::rgba(f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0, f32::from(a) / 255.0)
}

fn color_swatch_compact(ui: &mut Ui, color: &mut Color) -> bool {
    let mut c32 = color_to_c32(*color);
    if ui.color_edit_button_srgba(&mut c32).changed() {
        *color = c32_to_color(c32);
        return true;
    }
    false
}

fn color_swatch(ui: &mut Ui, label: &str, color: &mut Color) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        if color_swatch_compact(ui, color) {
            changed = true;
        }
    });
    changed
}

fn dim_label(ui: &mut Ui, t: Tokens, text: &str) {
    ui.label(RichText::new(text).color(t.text_dim).size(11.0));
}

fn enum_id<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v).ok().and_then(|j| j.as_str().map(str::to_owned)).unwrap_or_default()
}

fn enum_from_id<T: serde::de::DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_value(Value::String(s.to_owned())).ok()
}

/// Pixel-effect slider ranges follow the rendered lettering height (`size_pt * dpi / 72`):
/// a 2400-DPI / 12 000 × 17 000 px manga page has large pixel glyphs even at 54 pt, so fixed
/// 14–64 px maxima would be unusably thin.
fn ui_scale(st: &SfxPanelState) -> f32 {
    let dpi_factor = (if st.doc_dpi > 1.0 { st.doc_dpi } else { 72.0 }) / 72.0;
    ((st.draft.size_pt * dpi_factor) / 54.0).clamp(1.0, 256.0)
}

fn section_card(ui: &mut Ui, t: Tokens, add_contents: impl FnOnce(&mut Ui)) {
    egui::Frame::group(ui.style())
        .fill(t.dock)
        .stroke(Stroke::new(1.0, t.card_border))
        .corner_radius(t.radius)
        .inner_margin(egui::Margin::same(7))
        .show(ui, |ui| {
            ui.set_width(ui.available_width().max(1.0));
            add_contents(ui);
        });
}

fn section_header(ui: &mut Ui, t: Tokens, title: &str, badge: &str, badge_color: Color32) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong().size(10.5).color(t.text_dim));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(badge).monospace().size(10.0).color(badge_color));
        });
    });
}

fn seg_button(ui: &mut Ui, active: bool, active_fill: Color32, label: &str, t: Tokens) -> egui::Response {
    let fill = if active { active_fill } else { t.field };
    let text_col = if active { Color32::WHITE } else { t.text_dim };
    let btn = egui::Button::new(RichText::new(label).size(10.5).strong().color(text_col))
        .fill(fill)
        .stroke(Stroke::new(1.0, if active { active_fill } else { t.card_border }))
        .min_size(vec2(ui.available_width().max(24.0), 22.0));
    ui.add(btn)
}

fn slider_row_f32(ui: &mut Ui, t: Tokens, label: &str, val: &mut f32, range: std::ops::RangeInclusive<f32>, suffix: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(10.5).color(t.text_dim));
        let readout = format!("{:.0}{suffix}", *val);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_sized([34.0, 18.0], egui::Label::new(RichText::new(readout).monospace().size(10.0).color(t.text)));
            let w = (ui.available_width() - 4.0).max(48.0);
            ui.spacing_mut().slider_width = w;
            if ui.add(egui::Slider::new(val, range).show_value(false)).changed() {
                changed = true;
            }
        });
    });
    changed
}

/// Like [`slider_row_f32`] but prints one decimal (percentage-of-glyph-height sliders need it).
fn slider_row_f32_1dp(ui: &mut Ui, t: Tokens, label: &str, val: &mut f32, range: std::ops::RangeInclusive<f32>, suffix: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(10.5).color(t.text_dim));
        let readout = format!("{:.1}{suffix}", *val);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_sized([40.0, 18.0], egui::Label::new(RichText::new(readout).monospace().size(10.0).color(t.text)));
            let w = (ui.available_width() - 4.0).max(48.0);
            ui.spacing_mut().slider_width = w;
            if ui.add(egui::Slider::new(val, range).show_value(false)).changed() {
                changed = true;
            }
        });
    });
    changed
}

fn slider_row_u8(ui: &mut Ui, t: Tokens, label: &str, val: &mut u8, range: std::ops::RangeInclusive<u8>, suffix: &str) -> bool {
    let mut f = f32::from(*val);
    let r = f32::from(*range.start())..=f32::from(*range.end());
    let changed = slider_row_f32(ui, t, label, &mut f, r, suffix);
    if changed {
        *val = f.round().clamp(f32::from(*range.start()), f32::from(*range.end())) as u8;
    }
    changed
}

fn slider_row_u16(ui: &mut Ui, t: Tokens, label: &str, val: &mut u16, range: std::ops::RangeInclusive<u16>, suffix: &str) -> bool {
    let mut f = f32::from(*val);
    let r = f32::from(*range.start())..=f32::from(*range.end());
    let changed = slider_row_f32(ui, t, label, &mut f, r, suffix);
    if changed {
        *val = f.round().clamp(f32::from(*range.start()), f32::from(*range.end())) as u16;
    }
    changed
}

fn reroll_seed_button(ui: &mut Ui, seed: &mut u32, tooltip: &str) {
    if ui.add_sized([24.0, 22.0], egui::Button::new("🎲")).on_hover_text(tooltip).clicked() {
        *seed = seed.wrapping_add(1).max(1);
    }
}

/// Lays out `add_left` in a vertical (`top_down`) column next to a compact `🎲` seed reroll button.
fn row_with_reroll(ui: &mut Ui, height: f32, seed: &mut u32, tooltip: &str, add_left: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        let spacing = ui.spacing().item_spacing.x;
        let avail = (ui.available_width() - 24.0 - spacing).max(120.0);
        ui.allocate_ui_with_layout(vec2(avail, height), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_width(avail);
            add_left(ui);
        });
        reroll_seed_button(ui, seed, tooltip);
    });
}

/// Initializes or subdivides the Catmull-Rom arc spline without squishing or shifting letters:
/// the first 3 control points span the exact first-to-last glyph rest centers at `y = 0`.
fn add_spline_point(draft: &mut SfxData, geom: &PreparedSfx) {
    if draft.spline.points.len() >= MAX_SFX_SPLINE_POINTS {
        return;
    }
    if draft.spline.points.len() < 2 {
        let rb = geom.rest_box;
        let box_cx = (rb[0] + rb[2]) * 0.5;
        let half_w = ((rb[2] - rb[0]) * 0.5).max(40.0);
        let (lx0, lx1) = if geom.glyphs.len() >= 2 {
            let a = geom.glyphs[0].rest_center[0] - box_cx;
            let b = geom.glyphs[geom.glyphs.len() - 1].rest_center[0] - box_cx;
            if (b - a).abs() >= 8.0 { (a, b) } else { (-half_w, half_w) }
        } else {
            (-half_w, half_w)
        };
        draft.spline.points = vec![[lx0, 0.0], [(lx0 + lx1) * 0.5, 0.0], [lx1, 0.0]];
        draft.spline.show_arc = true;
        return;
    }
    let segs = draft.spline.points.len() - 1;
    let mut best_i = 0usize;
    let mut best_d = -1.0_f64;
    for i in 0..segs {
        let a = draft.spline.points[i];
        let b = draft.spline.points[i + 1];
        let d = (b[0] - a[0]).hypot(b[1] - a[1]);
        if d > best_d {
            best_d = d;
            best_i = i;
        }
    }
    let t_mid = (best_i as f64 + 0.5) / segs as f64;
    let sp = sample_catmull_rom(&draft.spline.points, t_mid);
    draft.spline.points.insert(best_i + 1, [sp.lx, sp.ly]);
    draft.spline.show_arc = true;
}

fn face_color_mut(fill: &mut SfxFillSource) -> Option<&mut Color> {
    match fill {
        SfxFillSource::Solid { color } | SfxFillSource::DryBrush { color, .. } => Some(color),
        SfxFillSource::Gradient { start, .. } => Some(start),
        SfxFillSource::ShtackHatch { spec } => Some(&mut spec.color),
        SfxFillSource::None | SfxFillSource::Screentone { .. } => None,
    }
}

fn face_color_copy(fill: &SfxFillSource) -> Color {
    match fill {
        SfxFillSource::Solid { color } | SfxFillSource::DryBrush { color, .. } => *color,
        SfxFillSource::Gradient { start, .. } => *start,
        SfxFillSource::ShtackHatch { spec } => spec.color,
        SfxFillSource::None | SfxFillSource::Screentone { .. } => Color::BLACK,
    }
}

/// Automatically raises `line_width_px` from `0px` to a visible default (`5..64 px`) when the user
/// switches to a stylized outline mode or selects a hollow (`Полый`) fill so lettering never disappears.
fn ensure_visible_line_width(le: &mut photocraft_doc::sfx::SfxLineEffect, scale: f32) {
    if le.line_width_px < 1.0 {
        le.line_width_px = (5.0 * scale.sqrt()).round().clamp(5.0, 64.0);
    }
}

fn line_style_badge(draft: &SfxData) -> &'static str {
    match draft.line_effect.style {
        SfxLineStyle::Clean => "Clean",
        SfxLineStyle::OneBit => "1-Bit",
        SfxLineStyle::Spikes => "Шипы",
        SfxLineStyle::GPenNib => "G-Pen",
        SfxLineStyle::BrushStroke => "Кисть-штрих",
        SfxLineStyle::Shtack => match draft.line_effect.shtack_variant {
            SfxShtackVariant::SaiPenSketch => "SHTACK (SAI Pen)",
            SfxShtackVariant::DryPencil => "SHTACK (Dry Pencil)",
        },
        SfxLineStyle::ShibufudeV3 => match draft.line_effect.shibufude_profile {
            SfxShibufudeProfile::Kado => "Shibufude v3 (角)",
            SfxShibufudeProfile::Maru => "Shibufude v3 (丸)",
        },
        SfxLineStyle::SugomiV3 => match draft.line_effect.sugomi_direction {
            SfxSugomiDirection::Fude => "Sugomi v3 (筆)",
            SfxSugomiDirection::Tate => "Sugomi v3 (縦)",
            SfxSugomiDirection::Yoko => "Sugomi v3 (横)",
        },
    }
}

fn fill_style_badge(draft: &SfxData) -> &'static str {
    if draft.dry_brush.enabled && !matches!(draft.fill, SfxFillSource::None | SfxFillSource::ShtackHatch { .. }) {
        match draft.dry_brush.engine {
            SfxDryBrushEngine::SdfPhysics => "Dry: А SDF",
            SfxDryBrushEngine::SdfSeg => "Dry: Б1 Seg",
            SfxDryBrushEngine::SdfEtf => "Dry: Б2 ETF",
            SfxDryBrushEngine::Vector => "Dry: Вектор",
        }
    } else {
        match &draft.fill {
            SfxFillSource::None => "Полый (None)",
            SfxFillSource::Solid { .. } => "Solid",
            SfxFillSource::Gradient { .. } => "Gradient",
            SfxFillSource::DryBrush { spec, .. } => match spec.engine {
                SfxDryBrushEngine::SdfPhysics => "Dry: А SDF",
                SfxDryBrushEngine::SdfSeg => "Dry: Б1 Seg",
                SfxDryBrushEngine::SdfEtf => "Dry: Б2 ETF",
                SfxDryBrushEngine::Vector => "Dry: Вектор",
            },
            SfxFillSource::Screentone { .. } => "Screentone",
            SfxFillSource::ShtackHatch { spec } => match spec.mode {
                SfxShtackFillMode::Hatch => "Shtack Hatch",
                SfxShtackFillMode::StrokeFlow => "Shtack SAI",
                SfxShtackFillMode::UkiyoBrush => "Ukiyo-e Кисть",
            },
        }
    }
}

fn preset_title_color(preset: &SfxPreset, t: Tokens) -> Color32 {
    if preset.dry_brush.enabled {
        return TEXT_CYAN;
    }
    if matches!(preset.fill, SfxFillSource::Screentone { .. }) {
        return TEXT_ROSE;
    }
    if matches!(preset.fill, SfxFillSource::ShtackHatch { .. }) {
        return TEXT_AMBER;
    }
    match preset.line_effect.style {
        SfxLineStyle::Shtack => TEXT_AMBER,
        SfxLineStyle::GPenNib => TEXT_SKY,
        SfxLineStyle::BrushStroke | SfxLineStyle::ShibufudeV3 => TEXT_EMERALD,
        SfxLineStyle::Spikes | SfxLineStyle::SugomiV3 => TEXT_ROSE,
        SfxLineStyle::Clean | SfxLineStyle::OneBit => t.text,
    }
}

/// Renders the SFX authoring dock panel and emits deferred engine commands.
pub fn panel(ui: &mut Ui, st: &mut SfxPanelState, gizmo: &mut SfxGizmoState, target: &SfxTargetInfo<'_>) -> SfxPanelOutput {
    let mut out = SfxPanelOutput::default();
    st.doc_dpi = if target.doc_dpi > 1.0 { target.doc_dpi } else { 72.0 };
    st.sync_from_host(target);
    let t = Tokens::get(ui.ctx());
    let before = st.draft.clone();

    draw_header_and_tabs(ui, st, t, target);
    ui.add_space(4.0);

    egui::ScrollArea::vertical().id_salt("sfx-dock-scroll").auto_shrink([false, true]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        match st.active_tab {
            DockTab::Sfx => draw_sfx_tab(ui, st, gizmo, t, target, &mut out),
            DockTab::Blending => draw_blending_tab(ui, st, t, &mut out),
            DockTab::Color => draw_color_tab(ui, st, t),
        }
        crate::widgets::hairline(ui);
        draw_actions_section(ui, st, target, &mut out);
    });

    // Interactive drags render through the fast proxy preview; the final full-resolution render
    // is committed as soon as the mouse button is released.
    let pointer_down = ui.ctx().input(|i| i.pointer.primary_down());
    let has_target = target.doc_id.is_some() && target.layer_id.is_some() && target.data.is_some();
    if has_target && let (Some(doc_id), Some(layer_id)) = (target.doc_id, target.layer_id) {
        if st.draft != before {
            st.draft.validate();
            let coalesce = format!("sfx-panel:{}:{}", doc_id.0, layer_id.0);
            let preview = pointer_down;
            st.pending_full_render = pointer_down;
            st.pending_full_target = pointer_down.then_some((doc_id, layer_id));
            out.commands.push(OutCommand::new(
                "sfx.update",
                json!({
                    "layer": layer_id.0,
                    "sfx": st.draft,
                    "coalesce": coalesce,
                    "preview": preview,
                }),
            ));
            st.synced_data = Some(st.draft.clone());
        } else if st.pending_full_render && !pointer_down {
            // The user just released the mouse after dragging a slider / colour picker.
            st.pending_full_render = false;
            st.pending_full_target = None;
            let coalesce = format!("sfx-panel:{}:{}", doc_id.0, layer_id.0);
            out.commands.push(OutCommand::new(
                "sfx.update",
                json!({
                    "layer": layer_id.0,
                    "sfx": st.draft,
                    "coalesce": coalesce,
                    "preview": false,
                }),
            ));
        }
    }

    out
}

fn draw_header_and_tabs(ui: &mut Ui, st: &mut SfxPanelState, t: Tokens, target: &SfxTargetInfo<'_>) {
    let scale = ui_scale(st);
    ui.horizontal(|ui| {
        ui.strong("SFX Lettering");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let status = if let Some(id) = target.layer_id.filter(|_| target.data.is_some()) {
                format!("Live #{}", id.0)
            } else {
                "Draft (New)".to_owned()
            };
            ui.label(RichText::new(status).color(TEXT_SKY).monospace().size(10.5));
            let (badge_txt, badge_col, badge_tip) = if target.gpu_active {
                ("⚡ GPU", TEXT_EMERALD, "Аппаратное ускорение wgpu/WGSL активно (60 FPS превью + GPU-коммит)")
            } else {
                ("CPU", TEXT_AMBER, "Программный рендер CPU (GPU недоступен или активен несовместимый режим)")
            };
            ui.label(RichText::new(badge_txt).color(badge_col).monospace().strong().size(9.5))
                .on_hover_text(badge_tip);
        });
    });

    // Quick text + vertical orientation toggle + Halo/Line strip
    ui.horizontal(|ui| {
        let spacing = ui.spacing().item_spacing.x;
        let text_w = (ui.available_width() - 28.0 - spacing).max(80.0);
        ui.add(egui::TextEdit::singleline(&mut st.draft.text).hint_text("SFX текст (напр. БА-БАХ!)...").desired_width(text_w));
        if ui
            .add_sized([26.0, 22.0], egui::Button::selectable(st.draft.vertical, "↕"))
            .on_hover_text("Вертикальный текст (шрифт и размер — в стандартной панели Character)")
            .clicked()
        {
            st.draft.vertical = !st.draft.vertical;
        }
    });
    ui.horizontal(|ui| {
        dim_label(ui, t, "Ореол:");
        color_swatch_compact(ui, &mut st.draft.line_effect.halo_color);
        crate::widgets::value_field(ui, &mut st.draft.line_effect.halo_width_px, 0.0..=64.0 * scale, "px", 46.0);
        ui.add_space(4.0);
        dim_label(ui, t, "Лайн:");
        color_swatch_compact(ui, &mut st.draft.line_effect.line_color);
        crate::widgets::value_field(ui, &mut st.draft.line_effect.line_width_px, 0.0..=64.0 * scale, "px", 46.0);
    });

    ui.add_space(2.0);
    let blend_label = if st.draft.blending.is_default() { "Blending" } else { "Blending ●" };
    ui.columns(3, |cols| {
        if seg_button(&mut cols[0], st.active_tab == DockTab::Sfx, ACCENT_SKY, "SFX ●", t).clicked() {
            st.active_tab = DockTab::Sfx;
        }
        if seg_button(&mut cols[1], st.active_tab == DockTab::Blending, ACCENT_SKY, blend_label, t).clicked() {
            st.active_tab = DockTab::Blending;
        }
        if seg_button(&mut cols[2], st.active_tab == DockTab::Color, ACCENT_SKY, "Color", t).clicked() {
            st.active_tab = DockTab::Color;
        }
    });
}

fn draw_sfx_tab(ui: &mut Ui, st: &mut SfxPanelState, gizmo: &mut SfxGizmoState, t: Tokens, target: &SfxTargetInfo<'_>, out: &mut SfxPanelOutput) {
    draw_presets_card(ui, st, t, target, out);
    draw_gizmo_card(ui, st, gizmo, t);
    draw_line_effect_card(ui, st, t);
    draw_fill_card(ui, st, t, out);
    draw_vector_shadow_card(ui, st, t);
}

fn draw_presets_card(ui: &mut Ui, st: &mut SfxPanelState, t: Tokens, target: &SfxTargetInfo<'_>, out: &mut SfxPanelOutput) {
    section_card(ui, t, |ui| {
        section_header(ui, t, "БЫСТРЫЕ СТИЛИ (1-КЛИК)", "Manga B/W", t.text_faint);
        ui.add_space(3.0);

        for pair in target.presets.chunks(2) {
            ui.columns(2, |cols| {
                for (i, preset) in pair.iter().enumerate() {
                    let title_col = preset_title_color(preset, t);
                    let sub = if preset.description.is_empty() { "JSON Preset" } else { preset.description.as_str() };
                    let card_resp = egui::Frame::group(cols[i].style())
                        .fill(t.field)
                        .stroke(Stroke::new(1.0, t.card_border))
                        .corner_radius(t.radius_sm)
                        .inner_margin(egui::Margin::symmetric(6, 4))
                        .show(&mut cols[i], |ui| {
                            ui.set_width(ui.available_width().max(1.0));
                            ui.label(RichText::new(&preset.name).strong().size(10.5).color(title_col));
                            ui.label(RichText::new(sub).size(9.5).color(t.text_dim));
                        })
                        .response
                        .interact(egui::Sense::click());
                    if card_resp.hovered() {
                        cols[i].ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if card_resp.clicked() {
                        if let Some(id) = target.layer_id.filter(|_| target.data.is_some()) {
                            out.commands.push(OutCommand::new("sfx.presets.apply", json!({ "layer": id.0, "name": preset.name })));
                        } else {
                            st.draft.apply_preset(preset);
                        }
                    }
                }
            });
            ui.add_space(2.0);
        }

        egui::CollapsingHeader::new(RichText::new("SFX v3 (Shibufude / Sugomi)").size(10.0).strong().color(TEXT_EMERALD))
            .id_salt("sfx-v3-starter-presets")
            .default_open(true)
            .show(ui, |ui| {
                let starters = sfx_v3_starter_presets();
                for pair in starters.chunks(2) {
                    ui.columns(2, |cols| {
                        for (i, preset) in pair.iter().enumerate() {
                            let title_col = preset_title_color(preset, t);
                            let card_resp = egui::Frame::group(cols[i].style())
                                .fill(t.field)
                                .stroke(Stroke::new(1.0, t.card_border))
                                .corner_radius(t.radius_sm)
                                .inner_margin(egui::Margin::symmetric(6, 4))
                                .show(&mut cols[i], |ui| {
                                    ui.set_width(ui.available_width().max(1.0));
                                    ui.label(RichText::new(&preset.name).strong().size(10.0).color(title_col));
                                    ui.label(RichText::new(&preset.description).size(9.0).color(t.text_dim));
                                })
                                .response
                                .interact(egui::Sense::click());
                            if card_resp.hovered() {
                                cols[i].ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            if card_resp.clicked() {
                                st.draft.apply_preset(preset);
                            }
                        }
                    });
                    ui.add_space(2.0);
                }
            });

        egui::CollapsingHeader::new(RichText::new("Сохранить / удалить пресет").size(10.0).color(t.text_dim))
            .id_salt("sfx-preset-manage")
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut st.preset_name).hint_text("Имя пресета...").desired_width(116.0));
                    let name = st.preset_name.trim().to_owned();
                    if ui.add_enabled(!name.is_empty(), egui::Button::new("Save")).clicked() {
                        let mut preset = st.draft.to_preset(&name);
                        preset.validate();
                        out.commands.push(OutCommand::new("sfx.presets.save", json!({ "preset": preset })));
                    }
                    if ui.add_enabled(!name.is_empty() && target.presets.iter().any(|p| p.name == name), egui::Button::new("Del")).clicked() {
                        out.commands.push(OutCommand::new("sfx.presets.delete", json!({ "name": name })));
                    }
                });
            });
    });
}

fn sfx_v3_starter_presets() -> [SfxPreset; 4] {
    let ink = Color::rgb(0.05, 0.05, 0.07);
    [
        SfxPreset {
            name: "Shibufude · 角 1-Bit".to_owned(),
            description: "Угловая кисть + しぶき".to_owned(),
            fill: SfxFillSource::None,
            line_effect: SfxLineEffect {
                style: SfxLineStyle::ShibufudeV3,
                halo_width_px: 8.0,
                halo_color: Color::WHITE,
                line_width_px: 13.5,
                line_color: ink,
                shibufude_profile: SfxShibufudeProfile::Kado,
                shibufude_edge_mode: SfxV3EdgeMode::OneBit,
                ..SfxLineEffect::default()
            },
            ..SfxPreset::default()
        },
        SfxPreset {
            name: "Shibufude · 丸 Rough".to_owned(),
            description: "Мягкая кисть + касурэ".to_owned(),
            fill: SfxFillSource::None,
            line_effect: SfxLineEffect {
                style: SfxLineStyle::ShibufudeV3,
                halo_width_px: 8.0,
                halo_color: Color::WHITE,
                line_width_px: 14.0,
                line_color: ink,
                shibufude_profile: SfxShibufudeProfile::Maru,
                shibufude_edge_mode: SfxV3EdgeMode::Rough,
                ..SfxLineEffect::default()
            },
            ..SfxPreset::default()
        },
        SfxPreset {
            name: "Sugomi · 筆 Solid".to_owned(),
            description: "Залитый шипастый удар".to_owned(),
            fill: SfxFillSource::None,
            line_effect: SfxLineEffect {
                style: SfxLineStyle::SugomiV3,
                halo_width_px: 10.0,
                halo_color: Color::WHITE,
                line_color: ink,
                sugomi_direction: SfxSugomiDirection::Fude,
                sugomi_hollow: false,
                sugomi_edge_mode: SfxV3EdgeMode::OneBit,
                ..SfxLineEffect::default()
            },
            ..SfxPreset::default()
        },
        SfxPreset {
            name: "Sugomi · Полый Rough".to_owned(),
            description: "Полый контур + зубцы".to_owned(),
            fill: SfxFillSource::None,
            line_effect: SfxLineEffect {
                style: SfxLineStyle::SugomiV3,
                halo_width_px: 8.0,
                halo_color: Color::WHITE,
                line_color: ink,
                sugomi_direction: SfxSugomiDirection::Fude,
                sugomi_hollow: true,
                sugomi_edge_mode: SfxV3EdgeMode::Rough,
                ..SfxLineEffect::default()
            },
            ..SfxPreset::default()
        },
    ]
}

fn draw_gizmo_card(ui: &mut Ui, st: &mut SfxPanelState, gizmo: &mut SfxGizmoState, t: Tokens) {
    section_card(ui, t, |ui| {
        let mode_badge = match st.draft.warp_mode {
            SfxWarpMode::Resize => "Размер (рамка)",
            SfxWarpMode::FreeTransform => "Перспектива (4 угла)",
            SfxWarpMode::PerGlyph => "По буквам",
            SfxWarpMode::MeshLattice => "Сетка Mesh",
            SfxWarpMode::LiquifyField => "Пластика",
        };
        section_header(ui, t, "GIZMO НА ХОЛСТЕ", mode_badge, TEXT_SKY);
        ui.add_space(3.0);

        // Resize is the default & first selectable mode (FT / per-glyph / mesh / liquify follow).
        ui.columns(2, |cols| {
            if seg_button(&mut cols[0], st.draft.warp_mode == SfxWarpMode::Resize, ACCENT_SKY, "Resize", t).clicked() {
                st.draft.warp_mode = SfxWarpMode::Resize;
            }
            if seg_button(&mut cols[1], st.draft.warp_mode == SfxWarpMode::FreeTransform, ACCENT_SKY, "FT 4-Corner + Дуга", t).clicked() {
                st.draft.warp_mode = SfxWarpMode::FreeTransform;
            }
        });
        ui.add_space(2.0);
        ui.columns(2, |cols| {
            if seg_button(&mut cols[0], st.draft.warp_mode == SfxWarpMode::PerGlyph, ACCENT_SKY, "По точкам (Alt)", t).clicked() {
                st.draft.warp_mode = SfxWarpMode::PerGlyph;
            }
            if seg_button(&mut cols[1], st.draft.warp_mode == SfxWarpMode::MeshLattice, ACCENT_SKY, "Mesh Grid", t).clicked() {
                st.draft.warp_mode = SfxWarpMode::MeshLattice;
            }
        });
        ui.add_space(2.0);
        if seg_button(ui, st.draft.warp_mode == SfxWarpMode::LiquifyField, ACCENT_SKY, "Liquify", t).clicked() {
            st.draft.warp_mode = SfxWarpMode::LiquifyField;
        }

        ui.add_space(4.0);
        match st.draft.warp_mode {
            SfxWarpMode::Resize => {
                ui.horizontal(|ui| {
                    dim_label(ui, t, "Масштаб рамки:");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Сбросить масштаб").clicked() {
                            st.draft.free_transform.scale_x = 1.0;
                            st.draft.free_transform.scale_y = 1.0;
                        }
                    });
                });
                ui.label(RichText::new("Угол: размер; Alt — от центра; Shift — растяжение; Ctrl — 4 угла.").color(t.text_dim).size(10.0));
            }
            SfxWarpMode::FreeTransform => {
                ui.horizontal(|ui| {
                    dim_label(ui, t, "Перспектива рамки:");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Сбросить FT").clicked() {
                            st.draft.free_transform = SfxFreeTransform::default();
                        }
                        if ui.small_button("Сбросить 4 угла").clicked() {
                            st.draft.free_transform.reset_corners();
                        }
                    });
                });
                crate::widgets::hairline(ui);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut st.draft.spline.show_arc, "Видимая дуга");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!("{} тчк", st.draft.spline.points.len())).monospace().size(10.0).color(TEXT_CYAN));
                    });
                });
                ui.columns(2, |cols| {
                    if cols[0]
                        .add(
                            egui::Button::new(RichText::new("+ Точка на дуге").size(10.5).color(TEXT_CYAN))
                                .min_size(vec2(cols[0].available_width(), 22.0)),
                        )
                        .clicked()
                    {
                        let geom = gizmo.ensure_geom(&st.draft, gizmo.last_dpi());
                        add_spline_point(&mut st.draft, &geom);
                    }
                    if cols[1]
                        .add(egui::Button::new(RichText::new("Выпрямить дугу").size(10.5)).min_size(vec2(cols[1].available_width(), 22.0)))
                        .clicked()
                    {
                        st.draft.spline.straighten();
                    }
                });
            }
            SfxWarpMode::PerGlyph => {
                let mut gtool = SfxGlyphTool::get(ui.ctx());
                ui.columns(3, |cols| {
                    if seg_button(&mut cols[0], gtool == SfxGlyphTool::Move, ACCENT_AMBER, "Move", t).clicked() {
                        gtool = SfxGlyphTool::Move;
                    }
                    if seg_button(&mut cols[1], gtool == SfxGlyphTool::Skew, ACCENT_AMBER, "Skew", t).clicked() {
                        gtool = SfxGlyphTool::Skew;
                    }
                    if seg_button(&mut cols[2], gtool == SfxGlyphTool::FreeTransform, ACCENT_AMBER, "FT (4-Crn)", t).clicked() {
                        gtool = SfxGlyphTool::FreeTransform;
                    }
                });
                SfxGlyphTool::set(ui.ctx(), gtool);

                if let Some(g_idx) = gizmo.selected_glyph {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("Буква #{}", g_idx + 1)).strong().size(10.5).color(TEXT_AMBER));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Сбросить").clicked() {
                                st.draft.per_glyph.retain(|g| g.glyph_index != g_idx);
                            }
                        });
                    });
                    let entry_idx = st.draft.per_glyph.iter().position(|g| g.glyph_index == g_idx);
                    let mut cur = entry_idx.map_or(SfxGlyphTransform { glyph_index: g_idx, ..SfxGlyphTransform::default() }, |i| st.draft.per_glyph[i]);
                    let mut edited = false;
                    ui.horizontal(|ui| {
                        let (mut dx, mut dy, mut rot) = (cur.dx as f32, cur.dy as f32, cur.rotation_deg as f32);
                        if crate::widgets::value_field(ui, &mut dx, -2000.0..=2000.0, "x", 56.0).changed() {
                            cur.dx = f64::from(dx);
                            edited = true;
                        }
                        if crate::widgets::value_field(ui, &mut dy, -2000.0..=2000.0, "y", 56.0).changed() {
                            cur.dy = f64::from(dy);
                            edited = true;
                        }
                        if crate::widgets::value_field(ui, &mut rot, -360.0..=360.0, "°", 56.0).changed() {
                            cur.rotation_deg = f64::from(rot);
                            edited = true;
                        }
                    });
                    ui.horizontal(|ui| {
                        let (mut skx, mut sky) = (cur.skew_x_deg as f32, cur.skew_y_deg as f32);
                        dim_label(ui, t, "Skew:");
                        if crate::widgets::value_field(ui, &mut skx, -60.0..=60.0, "°", 56.0).changed() {
                            cur.skew_x_deg = f64::from(skx);
                            edited = true;
                        }
                        if crate::widgets::value_field(ui, &mut sky, -60.0..=60.0, "°", 56.0).changed() {
                            cur.skew_y_deg = f64::from(sky);
                            edited = true;
                        }
                    });
                    if edited {
                        if let Some(i) = entry_idx {
                            st.draft.per_glyph[i] = cur;
                        } else {
                            st.draft.per_glyph.push(cur);
                        }
                    }
                } else {
                    ui.label(RichText::new("Кликните по букве на холсте (Move / Skew / FT + вращение).").color(t.text_dim).size(10.5));
                }
                if !st.draft.per_glyph.is_empty() && ui.button("Сбросить все буквы").clicked() {
                    st.draft.per_glyph.clear();
                }
            }
            SfxWarpMode::MeshLattice => {
                ui.horizontal(|ui| {
                    let mut cols = f32::from(st.draft.mesh.cols);
                    let mut rows = f32::from(st.draft.mesh.rows);
                    dim_label(ui, t, "Grid:");
                    if crate::widgets::value_field(ui, &mut cols, 2.0..=8.0, "c", 48.0).changed() {
                        st.draft.mesh.cols = cols.round().clamp(2.0, 8.0) as u8;
                        reset_mesh(&mut st.draft);
                        gizmo.selected_nodes.clear();
                    }
                    if crate::widgets::value_field(ui, &mut rows, 2.0..=8.0, "r", 48.0).changed() {
                        st.draft.mesh.rows = rows.round().clamp(2.0, 8.0) as u8;
                        reset_mesh(&mut st.draft);
                        gizmo.selected_nodes.clear();
                    }
                });
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut st.draft.mesh.interpolation, SfxMeshInterpolation::Bilinear, "Bilinear");
                    ui.selectable_value(&mut st.draft.mesh.interpolation, SfxMeshInterpolation::Bicubic, "Bicubic");
                    ui.checkbox(&mut st.draft.mesh.link_h, "Link H");
                    ui.checkbox(&mut st.draft.mesh.link_v, "Link V");
                });
                if ui.button("Reset Mesh").clicked() {
                    reset_mesh(&mut st.draft);
                    gizmo.selected_nodes.clear();
                }
            }
            SfxWarpMode::LiquifyField => {
                ui.horizontal(|ui| {
                    let mut r = gizmo.liquify_radius_px as f32;
                    let mut strg = gizmo.liquify_strength as f32;
                    dim_label(ui, t, "Radius:");
                    if crate::widgets::value_field(ui, &mut r, 4.0..=500.0, "px", 58.0).changed() {
                        gizmo.liquify_radius_px = f64::from(r);
                    }
                    dim_label(ui, t, "Power:");
                    if crate::widgets::value_field(ui, &mut strg, 0.05..=2.0, "", 52.0).changed() {
                        gizmo.liquify_strength = f64::from(strg);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(format!("Strokes: {}", st.draft.liquify_strokes.len()));
                    if !st.draft.liquify_strokes.is_empty() && ui.small_button("Undo").clicked() {
                        st.draft.liquify_strokes.pop();
                    }
                    if !st.draft.liquify_strokes.is_empty() && ui.small_button("Clear").clicked() {
                        st.draft.liquify_strokes.clear();
                    }
                });
            }
        }
    });
}

fn draw_line_effect_card(ui: &mut Ui, st: &mut SfxPanelState, t: Tokens) {
    let scale = ui_scale(st);
    section_card(ui, t, |ui| {
        let badge = line_style_badge(&st.draft);
        section_header(ui, t, "ЛАЙН / ОБВОДКА", badge, TEXT_AMBER);
        ui.add_space(3.0);

        let cur = st.draft.line_effect.style;
        ui.columns(3, |cols| {
            if seg_button(&mut cols[0], cur == SfxLineStyle::Clean, ACCENT_SKY, "Clean", t).clicked() {
                st.draft.line_effect.style = SfxLineStyle::Clean;
            }
            if seg_button(&mut cols[1], cur == SfxLineStyle::OneBit, ACCENT_SKY, "1-Bit", t).clicked() {
                st.draft.line_effect.style = SfxLineStyle::OneBit;
                ensure_visible_line_width(&mut st.draft.line_effect, scale);
            }
            if seg_button(&mut cols[2], cur == SfxLineStyle::Spikes, ACCENT_SKY, "Шипы", t).clicked() {
                st.draft.line_effect.style = SfxLineStyle::Spikes;
                ensure_visible_line_width(&mut st.draft.line_effect, scale);
                if st.draft.line_effect.spike_length_px < 6.0 {
                    st.draft.line_effect.spike_length_px = (12.0 * scale.sqrt()).round().clamp(10.0, 96.0);
                    st.draft.line_effect.spike_size_px = (7.0 * scale.sqrt()).round().clamp(6.0, 64.0);
                }
            }
        });
        ui.add_space(2.0);
        ui.columns(3, |cols| {
            if seg_button(&mut cols[0], cur == SfxLineStyle::GPenNib, ACCENT_SKY, "✒ G-Pen", t).clicked() {
                st.draft.line_effect.style = SfxLineStyle::GPenNib;
                ensure_visible_line_width(&mut st.draft.line_effect, scale);
            }
            if seg_button(&mut cols[1], cur == SfxLineStyle::BrushStroke, ACCENT_SKY, "🖌 Кисть", t).clicked() {
                st.draft.line_effect.style = SfxLineStyle::BrushStroke;
                ensure_visible_line_width(&mut st.draft.line_effect, scale);
            }
            if seg_button(&mut cols[2], cur == SfxLineStyle::Shtack, ACCENT_SKY, "⚡ Shtack", t).clicked() {
                st.draft.line_effect.style = SfxLineStyle::Shtack;
                ensure_visible_line_width(&mut st.draft.line_effect, scale);
            }
        });
        ui.add_space(2.0);
        ui.columns(2, |cols| {
            if seg_button(&mut cols[0], cur == SfxLineStyle::ShibufudeV3, ACCENT_AMBER, "🖌 Shibufude v3", t).clicked() {
                st.draft.line_effect.style = SfxLineStyle::ShibufudeV3;
                st.draft.fill = SfxFillSource::None;
                if st.draft.line_effect.line_width_px < 4.0 {
                    st.draft.line_effect.line_width_px = (13.5 * scale.sqrt()).round().clamp(8.0, 96.0);
                }
            }
            if seg_button(&mut cols[1], cur == SfxLineStyle::SugomiV3, ACCENT_AMBER, "⚡ Sugomi v3", t).clicked() {
                st.draft.line_effect.style = SfxLineStyle::SugomiV3;
                st.draft.fill = SfxFillSource::None;
            }
        });

        ui.add_space(4.0);
        let le = &mut st.draft.line_effect;
        match le.style {
            SfxLineStyle::Clean => {}
            SfxLineStyle::OneBit => {
                slider_row_f32(ui, t, "Рубленый край:", &mut le.one_bit_intensity, 0.0..=100.0, "%");
            }
            SfxLineStyle::Spikes => {
                row_with_reroll(ui, 22.0, &mut le.spike_seed, "Новый Seed шипов", |ui| {
                    ui.columns(3, |cols| {
                        if seg_button(&mut cols[0], le.spike_shape == SfxSpikeShape::Triangle, ACCENT_SKY, "Треуг", t).clicked() {
                            le.spike_shape = SfxSpikeShape::Triangle;
                        }
                        if seg_button(&mut cols[1], le.spike_shape == SfxSpikeShape::Square, ACCENT_SKY, "Квадр", t).clicked() {
                            le.spike_shape = SfxSpikeShape::Square;
                        }
                        if seg_button(&mut cols[2], le.spike_shape == SfxSpikeShape::Circle, ACCENT_SKY, "Круги", t).clicked() {
                            le.spike_shape = SfxSpikeShape::Circle;
                        }
                    });
                });
                slider_row_f32(ui, t, "Длина шипов:", &mut le.spike_length_px, 2.0..=28.0 * scale, "px");
                slider_row_f32(ui, t, "Размер шипов:", &mut le.spike_size_px, 2.0..=20.0 * scale, "px");
                slider_row_u16(ui, t, "Кол-во шипов:", &mut le.spike_count, 2..=40, "");
                slider_row_f32(ui, t, "Рандом (Гаусс):", &mut le.spike_random_pct, 0.0..=100.0, "%");
            }
            SfxLineStyle::GPenNib => {
                row_with_reroll(ui, 22.0, &mut le.gpen_seed, "Новый Seed перекрестий G-Pen", |ui| {
                    slider_row_f32(ui, t, "Контраст нажима:", &mut le.gpen_contrast_pct, 0.0..=100.0, "%");
                });
                slider_row_f32(ui, t, "Угол пера:", &mut le.gpen_angle_deg, -90.0..=90.0, "°");
                slider_row_f32(ui, t, "Прогиб граней:", &mut le.gpen_pinch_pct, 0.0..=100.0, "%");
                crate::widgets::hairline(ui);
                slider_row_f32(ui, t, "Длина перекрестий:", &mut le.gpen_overshoot_px, 0.0..=14.0 * scale, "px");
                slider_row_f32(ui, t, "Кол-во перекрестий:", &mut le.gpen_overshoot_count_pct, 0.0..=100.0, "%");
                slider_row_f32(ui, t, "Рандом перекрестий:", &mut le.gpen_overshoot_random_pct, 0.0..=100.0, "%");
            }
            SfxLineStyle::BrushStroke => {
                row_with_reroll(ui, 22.0, &mut le.ink_brush_seed, "Новый Seed тушевой кисти", |ui| {
                    slider_row_f32(ui, t, "Рваность края:", &mut le.ink_brush_roughness_pct, 0.0..=100.0, "%");
                });
                slider_row_f32(ui, t, "Прорывы туши:", &mut le.ink_brush_dropout_pct, 0.0..=100.0, "%");
                slider_row_f32(ui, t, "Вар. нажима:", &mut le.ink_brush_pressure_pct, 0.0..=100.0, "%");
                crate::widgets::hairline(ui);
                slider_row_f32(ui, t, "Длина перекрестий:", &mut le.ink_brush_overshoot_px, 0.0..=14.0 * scale, "px");
                slider_row_f32(ui, t, "Кол-во перекрестий:", &mut le.ink_brush_overshoot_count_pct, 0.0..=100.0, "%");
                slider_row_f32(ui, t, "Рандом перекрестий:", &mut le.ink_brush_overshoot_random_pct, 0.0..=100.0, "%");
            }
            SfxLineStyle::Shtack => {
                row_with_reroll(ui, 22.0, &mut le.shtack_seed, "Новый Seed штрихов SHTACK", |ui| {
                    ui.columns(2, |cols| {
                        if seg_button(&mut cols[0], le.shtack_variant == SfxShtackVariant::SaiPenSketch, ACCENT_SKY, "1: SAI Pen Sketch", t).clicked() {
                            le.shtack_variant = SfxShtackVariant::SaiPenSketch;
                        }
                        if seg_button(&mut cols[1], le.shtack_variant == SfxShtackVariant::DryPencil, ACCENT_SKY, "2: Dry Pencil Shtack", t).clicked() {
                            le.shtack_variant = SfxShtackVariant::DryPencil;
                        }
                    });
                });
                slider_row_u8(ui, t, "Проходов пера:", &mut le.shtack_bristles, 3..=6, "");
                slider_row_f32(ui, t, "Разброс штрихов:", &mut le.shtack_split_pct, 0.0..=100.0, "%");
                // Needle length is a percentage of the glyph height, not a document pixel length.
                slider_row_f32(ui, t, "Вылет игл:", &mut le.shtack_overshoot_px, 0.0..=60.0, "%");
                ui.horizontal(|ui| {
                    dim_label(ui, t, "Вылет:");
                    ui.columns(3, |cols| {
                        if seg_button(
                            &mut cols[0],
                            le.shtack_overshoot_mode == SfxShtackOvershootMode::FreeEnds,
                            ACCENT_SKY,
                            "Края",
                            t,
                        )
                        .clicked()
                        {
                            le.shtack_overshoot_mode = SfxShtackOvershootMode::FreeEnds;
                        }
                        if seg_button(
                            &mut cols[1],
                            le.shtack_overshoot_mode == SfxShtackOvershootMode::OuterOnly,
                            ACCENT_SKY,
                            "Стыки",
                            t,
                        )
                        .clicked()
                        {
                            le.shtack_overshoot_mode = SfxShtackOvershootMode::OuterOnly;
                        }
                        if seg_button(
                            &mut cols[2],
                            le.shtack_overshoot_mode == SfxShtackOvershootMode::All,
                            ACCENT_SKY,
                            "Все",
                            t,
                        )
                        .clicked()
                        {
                            le.shtack_overshoot_mode = SfxShtackOvershootMode::All;
                        }
                    });
                });
                // Thickness is a percentage of glyph height (`gh`), not a document pixel length.
                slider_row_f32_1dp(ui, t, "Толщина штриха:", &mut le.shtack_thickness_px, 0.5..=8.0, "%");
                slider_row_f32(ui, t, "Сухость / Зерно:", &mut le.shtack_dryness_pct, 0.0..=100.0, "%");
                if le.shtack_variant == SfxShtackVariant::DryPencil {
                    slider_row_f32(ui, t, "Чёткость зерна:", &mut le.shtack_grain_sharpness_pct, 0.0..=100.0, "%");
                }
                ui.label(RichText::new("Внутренность буквы настраивается отдельно в секции «Заливка» (Полый / Solid / Shtack Fill).").size(9.5).color(t.text_dim));
            }
            SfxLineStyle::ShibufudeV3 => {
                draw_shibufude_v3_controls(ui, le, scale, t);
            }
            SfxLineStyle::SugomiV3 => {
                draw_sugomi_v3_controls(ui, le, t);
            }
        }
        if !matches!(le.style, SfxLineStyle::Shtack | SfxLineStyle::ShibufudeV3 | SfxLineStyle::SugomiV3)
            && (le.line_width_px > 0.05 || le.style != SfxLineStyle::Clean)
        {
            slider_row_f32(ui, t, "Зерно бумаги на лайне:", &mut le.line_grain_pct, 0.0..=100.0, "%");
        }
    });
}

fn draw_shibufude_v3_controls(ui: &mut Ui, le: &mut SfxLineEffect, scale: f32, t: Tokens) {
    row_with_reroll(ui, 22.0, &mut le.shibufude_seed, "Новый Seed Shibufude v3", |ui| {
        ui.columns(2, |cols| {
            if seg_button(&mut cols[0], le.shibufude_profile == SfxShibufudeProfile::Kado, ACCENT_SKY, "角 Kado", t).clicked() {
                le.shibufude_profile = SfxShibufudeProfile::Kado;
            }
            if seg_button(&mut cols[1], le.shibufude_profile == SfxShibufudeProfile::Maru, ACCENT_SKY, "丸 Maru", t).clicked() {
                le.shibufude_profile = SfxShibufudeProfile::Maru;
            }
        });
    });
    ui.add_space(2.0);
    ui.columns(2, |cols| {
        if seg_button(&mut cols[0], le.shibufude_edge_mode == SfxV3EdgeMode::OneBit, ACCENT_SKY, "1-Bit", t).clicked() {
            le.shibufude_edge_mode = SfxV3EdgeMode::OneBit;
        }
        if seg_button(&mut cols[1], le.shibufude_edge_mode == SfxV3EdgeMode::Rough, ACCENT_AMBER, "Rough (Касурэ)", t).clicked() {
            le.shibufude_edge_mode = SfxV3EdgeMode::Rough;
        }
    });
    ui.checkbox(&mut le.shibufude_knockout, "Выворотка (抜き · ×0.84)");
    slider_row_f32(ui, t, "Шероховатость:", &mut le.shibufude_roughness_pct, 0.0..=100.0, "%");
    slider_row_f32(ui, t, "Брызги (しぶき):", &mut le.shibufude_splatter_pct, 0.0..=100.0, "%");
    slider_row_f32(ui, t, "Разлёт брызг:", &mut le.shibufude_spread_px, 4.0..=120.0 * scale, "px");
    slider_row_f32(ui, t, "Размер капель:", &mut le.shibufude_splat_size_pct, 25.0..=250.0, "%");
    if le.shibufude_edge_mode == SfxV3EdgeMode::Rough {
        slider_row_f32(ui, t, "Сухость края:", &mut le.shibufude_dryness_pct, 0.0..=100.0, "%");
        slider_row_f32(ui, t, "Чёткость зерна:", &mut le.shibufude_sharpness_pct, 0.0..=100.0, "%");
    } else {
        slider_row_f32(ui, t, "Зерно 1-Bit:", &mut le.line_grain_pct, 0.0..=100.0, "%");
    }
}

fn draw_sugomi_v3_controls(ui: &mut Ui, le: &mut SfxLineEffect, t: Tokens) {
    row_with_reroll(ui, 22.0, &mut le.sugomi_seed, "Новый Seed Sugomi v3", |ui| {
        ui.columns(3, |cols| {
            if seg_button(&mut cols[0], le.sugomi_direction == SfxSugomiDirection::Fude, ACCENT_SKY, "筆 Fude", t).clicked() {
                le.sugomi_direction = SfxSugomiDirection::Fude;
            }
            if seg_button(&mut cols[1], le.sugomi_direction == SfxSugomiDirection::Tate, ACCENT_SKY, "縦 Tate", t).clicked() {
                le.sugomi_direction = SfxSugomiDirection::Tate;
            }
            if seg_button(&mut cols[2], le.sugomi_direction == SfxSugomiDirection::Yoko, ACCENT_SKY, "横 Yoko", t).clicked() {
                le.sugomi_direction = SfxSugomiDirection::Yoko;
            }
        });
    });
    ui.add_space(2.0);
    ui.columns(2, |cols| {
        if seg_button(&mut cols[0], !le.sugomi_hollow, ACCENT_SKY, "Залитый (Solid)", t).clicked() {
            le.sugomi_hollow = false;
        }
        if seg_button(&mut cols[1], le.sugomi_hollow, ACCENT_AMBER, "Полый (Hollow)", t).clicked() {
            le.sugomi_hollow = true;
        }
    });
    ui.add_space(2.0);
    ui.columns(2, |cols| {
        if seg_button(&mut cols[0], le.sugomi_edge_mode == SfxV3EdgeMode::OneBit, ACCENT_SKY, "1-Bit", t).clicked() {
            le.sugomi_edge_mode = SfxV3EdgeMode::OneBit;
        }
        if seg_button(&mut cols[1], le.sugomi_edge_mode == SfxV3EdgeMode::Rough, ACCENT_AMBER, "Rough (Касурэ)", t).clicked() {
            le.sugomi_edge_mode = SfxV3EdgeMode::Rough;
        }
    });
    slider_row_f32(ui, t, "Амплитуда шипов:", &mut le.sugomi_spike_amp_pct, 10.0..=160.0, "%");
    slider_row_f32(ui, t, "Глубина расщелин:", &mut le.sugomi_cleft_dip_pct, 0.0..=80.0, "%");
    if le.sugomi_edge_mode == SfxV3EdgeMode::Rough {
        slider_row_f32(ui, t, "Сухость края:", &mut le.sugomi_dryness_pct, 0.0..=100.0, "%");
        slider_row_f32(ui, t, "Чёткость зерна:", &mut le.sugomi_sharpness_pct, 0.0..=100.0, "%");
    } else {
        slider_row_f32(ui, t, "Зерно 1-Bit:", &mut le.line_grain_pct, 0.0..=100.0, "%");
    }
}

fn default_sfx_screentone() -> SfxFillSource {
    SfxFillSource::Screentone {
        spec: ScreentoneSpec {
            pattern_type: PatternType::Dots,
            dot_size: 3.0,
            spacing_x: 5.0,
            spacing_y: 5.0,
            density: 0.65,
            angle: 45.0,
            ..Default::default()
        },
        opacity: 1.0,
    }
}

fn draw_shtack_fill_editor(ui: &mut Ui, spec: &mut SfxShtackFillSpec, t: Tokens) {
    color_swatch(ui, "Цвет штрихов:", &mut spec.color);
    row_with_reroll(ui, 22.0, &mut spec.seed, "Новый Seed Shtack Fill", |ui| {
        ui.columns(3, |cols| {
            if seg_button(&mut cols[0], spec.mode == SfxShtackFillMode::Hatch, ACCENT_SKY, "Штриховка", t).clicked() {
                spec.mode = SfxShtackFillMode::Hatch;
                if spec.overshoot_pct > 25.0 {
                    spec.overshoot_pct = 25.0;
                }
            }
            if seg_button(&mut cols[1], spec.mode == SfxShtackFillMode::StrokeFlow, ACCENT_AMBER, "⚡ SAI", t).clicked() {
                if spec.mode != SfxShtackFillMode::StrokeFlow && spec.overshoot_pct <= 5.0 {
                    spec.overshoot_pct = 18.0;
                }
                spec.mode = SfxShtackFillMode::StrokeFlow;
            }
            if seg_button(&mut cols[2], spec.mode == SfxShtackFillMode::UkiyoBrush, ACCENT_SKY, "🖌 Ukiyo-e", t).clicked() {
                if spec.mode != SfxShtackFillMode::UkiyoBrush {
                    spec.overshoot_pct = spec.overshoot_pct.clamp(6.0, 22.0);
                    if spec.dryness_pct < 15.0 {
                        spec.dryness_pct = 32.0;
                    }
                }
                spec.mode = SfxShtackFillMode::UkiyoBrush;
            }
        });
    });
    match spec.mode {
        SfxShtackFillMode::Hatch => {
            slider_row_f32(ui, t, "Угол штрихов:", &mut spec.angle_deg, -90.0..=90.0, "°");
            ui.checkbox(&mut spec.cross_hatch, "Перекрёстная штриховка (Cross-hatch)");
            if spec.cross_hatch {
                slider_row_f32(ui, t, "Угол перекрёста:", &mut spec.cross_angle_deg, 15.0..=90.0, "°");
            }
            slider_row_f32(ui, t, "Плотность:", &mut spec.density_pct, 10.0..=100.0, "%");
            slider_row_f32_1dp(ui, t, "Толщина штриха:", &mut spec.thickness_pct, 0.4..=5.0, "%");
            slider_row_f32(ui, t, "Разброс / Живость:", &mut spec.roughness_pct, 0.0..=100.0, "%");
            slider_row_f32(ui, t, "Вылет за край:", &mut spec.overshoot_pct, 0.0..=25.0, "%");
        }
        SfxShtackFillMode::StrokeFlow => {
            ui.horizontal(|ui| {
                dim_label(ui, t, "Вылет:");
                ui.columns(3, |cols| {
                    if seg_button(&mut cols[0], spec.flow_overshoot == SfxShtackFlowOvershoot::FreeEnds, ACCENT_AMBER, "Края", t).clicked() {
                        spec.flow_overshoot = SfxShtackFlowOvershoot::FreeEnds;
                    }
                    if seg_button(&mut cols[1], spec.flow_overshoot == SfxShtackFlowOvershoot::OuterJunctions, ACCENT_AMBER, "Стыки", t).clicked() {
                        spec.flow_overshoot = SfxShtackFlowOvershoot::OuterJunctions;
                    }
                    if seg_button(&mut cols[2], spec.flow_overshoot == SfxShtackFlowOvershoot::All, ACCENT_AMBER, "Все", t).clicked() {
                        spec.flow_overshoot = SfxShtackFlowOvershoot::All;
                    }
                });
            });
            ui.add_space(2.0);
            let show_nodes = flow_nodes_visible(ui.ctx());
            ui.horizontal(|ui| {
                let lbl = if show_nodes { "🎯 Узлы вылетов (ВКЛ)" } else { "🎯 Узлы вылетов (ВЫКЛ)" };
                let resp = seg_button(ui, show_nodes, ACCENT_AMBER, lbl, t).on_hover_text(
                    "Клик: вкл/выкл иглу · Тяни узел: ручной угол и длина · Shift: шаг 15° · Alt+клик: сброс к авто",
                );
                if resp.clicked() {
                    set_flow_nodes_visible(ui.ctx(), !show_nodes);
                }
                if !spec.flow_overrides.is_empty() {
                    let reset_lbl = format!("Сброс ({})", spec.flow_overrides.len());
                    if ui
                        .small_button(RichText::new(reset_lbl).color(TEXT_ROSE))
                        .on_hover_text("Сбросить все ручные правки узлов на холсте")
                        .clicked()
                    {
                        spec.flow_overrides.clear();
                    }
                }
            });
            slider_row_f32(ui, t, "Полос / Плотность:", &mut spec.density_pct, 10.0..=100.0, "%");
            slider_row_f32_1dp(ui, t, "Толщина пера:", &mut spec.thickness_pct, 0.4..=5.0, "%");
            slider_row_f32(ui, t, "Живость / Разброс:", &mut spec.roughness_pct, 0.0..=100.0, "%");
            slider_row_f32(ui, t, "Вылет игл (SAI):", &mut spec.overshoot_pct, 0.0..=45.0, "%");
        }
        SfxShtackFillMode::UkiyoBrush => {
            slider_row_f32(ui, t, "Нажим туши:", &mut spec.density_pct, 10.0..=100.0, "%");
            slider_row_f32_1dp(ui, t, "Толщина кисти:", &mut spec.thickness_pct, 0.4..=5.0, "%");
            slider_row_f32(ui, t, "Ворсинки (Касурэ):", &mut spec.roughness_pct, 0.0..=100.0, "%");
            slider_row_f32(ui, t, "Хвост мазка:", &mut spec.overshoot_pct, 0.0..=35.0, "%");
        }
    }
    slider_row_f32(ui, t, "Сухость / Зерно:", &mut spec.dryness_pct, 0.0..=100.0, "%");
    if spec.dryness_pct > 0.5 || spec.mode == SfxShtackFillMode::UkiyoBrush {
        slider_row_f32(ui, t, "Чёткость зерна:", &mut spec.grain_sharpness_pct, 0.0..=100.0, "%");
    }
}

fn draw_open_gentonik_button(ui: &mut Ui, out: &mut SfxPanelOutput) {
    if ui
        .add(
            egui::Button::new(RichText::new("⚙ Открыть в Gentonik (Screentone)").size(10.5).color(TEXT_CYAN))
                .min_size(vec2(ui.available_width(), 22.0)),
        )
        .on_hover_text("Доработка паттерна (12 типов, сателлиты, дисторсия, градиент) в доке Screentone в реальном времени")
        .clicked()
    {
        out.commands.push(OutCommand::new("window.panel.activate", json!({ "panel": "screentone" })));
    }
}

/// Shared 4-engine Dry Brush editor used by both the primary `ЗАЛИВКА (FILL)` card
/// and Photoshop-style `Blending` overlays (100% parity, zero drift).
fn draw_dry_brush_editor(ui: &mut Ui, db: &mut SfxDryBrushSpec, scale: f32, t: Tokens) {
    let cur_engine = db.engine;
    let mut picked_engine: Option<SfxDryBrushEngine> = None;
    row_with_reroll(ui, 48.0, &mut db.seed, "Новый Seed щетины", |ui| {
        ui.columns(2, |cols| {
            if seg_button(&mut cols[0], cur_engine == SfxDryBrushEngine::SdfPhysics, ACCENT_SKY, "А: Физика SDF", t).clicked()
                && cur_engine != SfxDryBrushEngine::SdfPhysics
            {
                picked_engine = Some(SfxDryBrushEngine::SdfPhysics);
            }
            if seg_button(&mut cols[1], cur_engine == SfxDryBrushEngine::SdfSeg, ACCENT_SKY, "Б1: Штрихи (Seg)", t).clicked()
                && cur_engine != SfxDryBrushEngine::SdfSeg
            {
                picked_engine = Some(SfxDryBrushEngine::SdfSeg);
            }
        });
        ui.add_space(2.0);
        ui.columns(2, |cols| {
            if seg_button(&mut cols[0], cur_engine == SfxDryBrushEngine::SdfEtf, ACCENT_SKY, "Б2: Диффузия ETF", t).clicked()
                && cur_engine != SfxDryBrushEngine::SdfEtf
            {
                picked_engine = Some(SfxDryBrushEngine::SdfEtf);
            }
            if seg_button(&mut cols[1], cur_engine == SfxDryBrushEngine::Vector, ACCENT_SKY, "Вектор", t).clicked()
                && cur_engine != SfxDryBrushEngine::Vector
            {
                picked_engine = Some(SfxDryBrushEngine::Vector);
            }
        });
    });
    if let Some(next_engine) = picked_engine {
        db.apply_engine_defaults(next_engine);
    }

    slider_row_f32(ui, t, "Сухость (Порог):", &mut db.dryness_pct, 10.0..=85.0, "%");
    slider_row_f32(ui, t, "Размер щетины:", &mut db.bristle_size_pct, 10.0..=500.0, "%");

    match db.engine {
        SfxDryBrushEngine::SdfPhysics | SfxDryBrushEngine::SdfSeg | SfxDryBrushEngine::SdfEtf => {
            slider_row_f32(ui, t, "Плотность щетины:", &mut db.freq_pct, 10.0..=100.0, "%");
            if db.engine == SfxDryBrushEngine::SdfEtf {
                slider_row_f32(ui, t, "Длина LIC:", &mut db.lic_length_px, 6.0..=(96.0 * scale).max(96.0), "px");
            }
            slider_row_f32(ui, t, "Изгиб (Warp):", &mut db.warp_pct, 0.0..=100.0, "%");
            slider_row_f32(ui, t, "Зерно бумаги:", &mut db.paper_grain_pct, 0.0..=100.0, "%");
            slider_row_f32(ui, t, "Чёткость зерна:", &mut db.grain_sharpness_pct, 0.0..=100.0, "%");
            slider_row_f32(ui, t, "Износ краёв SDF:", &mut db.edge_wear_pct, 0.0..=100.0, "%");
        }
        SfxDryBrushEngine::Vector => {
            slider_row_u16(ui, t, "Щетинок:", &mut db.vec_bristle_count, 4..=40, "");
            slider_row_f32(ui, t, "Рандом щетины:", &mut db.vec_bristle_random_pct, 0.0..=100.0, "%");
            slider_row_f32(ui, t, "Наклон друг к другу:", &mut db.vec_mutual_tilt_deg, -10.0..=10.0, "°");
            slider_row_f32(ui, t, "Рандом наклона:", &mut db.vec_tilt_random_deg, 0.0..=10.0, "°");
            slider_row_f32(ui, t, "Гаусс. точки:", &mut db.vec_gauss_dots_pct, 0.0..=100.0, "%");
        }
    }

    crate::widgets::hairline(ui);
    if matches!(db.engine, SfxDryBrushEngine::SdfPhysics | SfxDryBrushEngine::Vector) {
        slider_row_f32(ui, t, "Угол мазка:", &mut db.angle_deg, -90.0..=90.0, "°");
    } else {
        let tag = if db.engine == SfxDryBrushEngine::SdfEtf { "Harmonic ETF" } else { "Segment Axis" };
        ui.horizontal(|ui| {
            ui.label(RichText::new("Угол: авто по штрихам буквы").size(10.0).color(TEXT_CYAN));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(tag).monospace().size(9.5).color(TEXT_CYAN));
            });
        });
    }
}

fn draw_fill_card(ui: &mut Ui, st: &mut SfxPanelState, t: Tokens, out: &mut SfxPanelOutput) {
    let scale = ui_scale(st);
    section_card(ui, t, |ui| {
        let badge = fill_style_badge(&st.draft);
        section_header(ui, t, "ЗАЛИВКА (FILL)", badge, TEXT_SKY);
        ui.add_space(3.0);

        let is_none = matches!(st.draft.fill, SfxFillSource::None);
        let is_hatch = matches!(st.draft.fill, SfxFillSource::ShtackHatch { .. });
        let is_dry = !is_none && !is_hatch && (st.draft.dry_brush.enabled || matches!(st.draft.fill, SfxFillSource::DryBrush { .. }));
        let is_tone = !is_none && !is_hatch && !is_dry && matches!(st.draft.fill, SfxFillSource::Screentone { .. });
        let is_solid = !is_none && !is_hatch && !is_dry && !is_tone;

        ui.columns(3, |cols| {
            if seg_button(&mut cols[0], is_none, ACCENT_SKY, "Полый", t).clicked() {
                st.draft.dry_brush.enabled = false;
                st.draft.fill = SfxFillSource::None;
                ensure_visible_line_width(&mut st.draft.line_effect, scale);
            }
            if seg_button(&mut cols[1], is_solid, ACCENT_SKY, "Solid", t).clicked() {
                let c = face_color_copy(&st.draft.fill);
                st.draft.dry_brush.enabled = false;
                if !matches!(st.draft.fill, SfxFillSource::Solid { .. } | SfxFillSource::Gradient { .. }) {
                    st.draft.fill = SfxFillSource::Solid { color: c };
                }
            }
            if seg_button(&mut cols[2], is_dry, ACCENT_SKY, "Dry Brush", t).clicked() {
                let c = face_color_copy(&st.draft.fill);
                st.draft.dry_brush.enabled = true;
                if matches!(
                    st.draft.fill,
                    SfxFillSource::None | SfxFillSource::Screentone { .. } | SfxFillSource::ShtackHatch { .. } | SfxFillSource::DryBrush { .. }
                ) {
                    st.draft.fill = SfxFillSource::Solid { color: c };
                }
            }
        });
        ui.add_space(2.0);
        ui.columns(2, |cols| {
            if seg_button(&mut cols[0], is_tone, ACCENT_SKY, "Screentone", t).clicked() {
                st.draft.dry_brush.enabled = false;
                if !matches!(st.draft.fill, SfxFillSource::Screentone { .. }) {
                    st.draft.fill = default_sfx_screentone();
                }
            }
            if seg_button(&mut cols[1], is_hatch, ACCENT_SKY, "⚡ Shtack Fill", t).clicked() {
                let c = face_color_copy(&st.draft.fill);
                st.draft.dry_brush.enabled = false;
                if !matches!(st.draft.fill, SfxFillSource::ShtackHatch { .. }) {
                    st.draft.fill = SfxFillSource::ShtackHatch { spec: SfxShtackFillSpec { color: c, ..SfxShtackFillSpec::default() } };
                }
            }
        });

        ui.add_space(4.0);
        if is_none {
            ui.label(RichText::new("Внутренность букв прозрачна — отображаются только обводка и ореол.").size(10.0).color(t.text_dim));
        } else if let SfxFillSource::ShtackHatch { spec } = &mut st.draft.fill {
            draw_shtack_fill_editor(ui, spec, t);
        } else if let SfxFillSource::DryBrush { color, spec } = &mut st.draft.fill {
            color_swatch(ui, "Цвет заливки:", color);
            draw_dry_brush_editor(ui, spec, scale, t);
        } else if st.draft.dry_brush.enabled {
            draw_dry_brush_editor(ui, &mut st.draft.dry_brush, scale, t);
        } else if let SfxFillSource::Screentone { spec, opacity } = &mut st.draft.fill {
            let mut spacing = spec.spacing_x;
            if slider_row_f32(ui, t, "Шаг точек:", &mut spacing, 3.0..=24.0, "px") {
                spec.spacing_x = spacing;
                spec.spacing_y = spacing;
            }
            egui::CollapsingHeader::new(RichText::new("Быстрые параметры скринтона").size(10.0).color(t.text_dim))
                .id_salt("sfx-tone-details")
                .default_open(false)
                .show(ui, |ui| {
                    draw_screentone_grid(ui, "sfx-base-tone", spec, opacity, t);
                });
            ui.add_space(2.0);
            draw_open_gentonik_button(ui, out);
        } else if let Some(col) = face_color_mut(&mut st.draft.fill) {
            color_swatch(ui, "Цвет заливки:", col);
        }
    });
}

fn draw_vector_shadow_card(ui: &mut Ui, st: &mut SfxPanelState, t: Tokens) {
    let scale = ui_scale(st);
    section_card(ui, t, |ui| {
        let sh = &mut st.draft.vector_shadow;
        ui.horizontal(|ui| {
            ui.checkbox(&mut sh.enabled, RichText::new("☀ Направление тени / 3D").strong().size(10.5).color(TEXT_AMBER));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let readout = format!("{:.0}° • {:.0}px", sh.angle_deg, sh.distance_px);
                ui.label(RichText::new(readout).monospace().size(10.0).color(t.text_dim));
            });
        });
        slider_row_f32(ui, t, "3D Extrude:", &mut sh.extrude_depth_px, 0.0..=32.0 * scale, "px");
        if sh.enabled {
            slider_row_f32(ui, t, "Угол тени:", &mut sh.angle_deg, -180.0..=180.0, "°");
            slider_row_f32(ui, t, "Дистанция тени:", &mut sh.distance_px, 0.0..=96.0 * scale, "px");
            color_swatch(ui, "Цвет тени:", &mut sh.color);
        }
    });
}

#[allow(dead_code)]
fn draw_advanced_fx_collapsible(ui: &mut Ui, st: &mut SfxPanelState, t: Tokens) {
    egui::CollapsingHeader::new(RichText::new("Доп. FX: Invert / Distress / Стек слоёв").size(10.5).color(t.text_dim))
        .id_salt("sfx-adv-fx")
        .default_open(false)
        .show(ui, |ui| {
            let has_inv = st.draft.has_negative_invert();
            if ui.selectable_label(has_inv, "Negative / Invert (BlendMode::Difference)").clicked() {
                if has_inv {
                    st.draft.styles.retain(|p| !matches!(p, SfxStylePass::NegativeInvert { .. }));
                } else {
                    st.draft.styles.push(SfxStylePass::NegativeInvert { enabled: true });
                }
            }
            crate::widgets::hairline(ui);
            draw_distress_section(ui, st, t);
            crate::widgets::hairline(ui);
            draw_styles_stack(ui, st, t);
        });
}

const BLEND_MODES: [(SfxChannelBlend, &str); 13] = [
    (SfxChannelBlend::Normal, "Normal"),
    (SfxChannelBlend::Multiply, "Multiply"),
    (SfxChannelBlend::ColorBurn, "Color Burn"),
    (SfxChannelBlend::Darken, "Darken"),
    (SfxChannelBlend::Screen, "Screen"),
    (SfxChannelBlend::ColorDodge, "Color Dodge"),
    (SfxChannelBlend::Lighten, "Lighten"),
    (SfxChannelBlend::Overlay, "Overlay"),
    (SfxChannelBlend::SoftLight, "Soft Light"),
    (SfxChannelBlend::HardLight, "Hard Light"),
    (SfxChannelBlend::Difference, "Difference"),
    (SfxChannelBlend::Exclusion, "Exclusion"),
    (SfxChannelBlend::Knockout, "Knockout (Вырез)"),
];

fn draw_blend_channel_controls(ui: &mut Ui, id_salt: &str, title: &str, blend: &mut SfxChannelBlend, opacity: &mut f32, t: Tokens) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong().size(10.5).color(t.text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let mut cur = enum_id(blend);
            let opts: Vec<(String, &str)> = BLEND_MODES.iter().map(|(m, lbl)| (enum_id(m), *lbl)).collect();
            if crate::widgets::dropdown_literal(ui, id_salt, &mut cur, &opts, 118.0)
                && let Some(parsed) = enum_from_id::<SfxChannelBlend>(&cur)
            {
                *blend = parsed;
            }
        });
    });
    let mut op_pct = *opacity * 100.0;
    if slider_row_f32(ui, t, "Непрозрачность:", &mut op_pct, 0.0..=100.0, "%") {
        *opacity = (op_pct / 100.0).clamp(0.0, 1.0);
    }
}

fn default_dry_brush_fill(color: Color) -> SfxFillSource {
    SfxFillSource::DryBrush { color, spec: SfxDryBrushSpec { enabled: true, ..SfxDryBrushSpec::default() } }
}

/// Full-parity Manga fill editor shared by `Blending` overlays, `Stroke` FX, and `Secondary Fill`.
fn draw_parity_fill_editor(ui: &mut Ui, id_salt: &str, fill: &mut SfxFillSource, scale: f32, t: Tokens, out: &mut SfxPanelOutput) {
    let is_solid = matches!(fill, SfxFillSource::Solid { .. });
    let is_grad = matches!(fill, SfxFillSource::Gradient { .. });
    let is_dry = matches!(fill, SfxFillSource::DryBrush { .. });
    let is_tone = matches!(fill, SfxFillSource::Screentone { .. });
    let is_hatch = matches!(fill, SfxFillSource::ShtackHatch { .. });

    ui.columns(3, |cols| {
        if seg_button(&mut cols[0], is_solid, ACCENT_AMBER, "Solid", t).clicked() && !is_solid {
            *fill = SfxFillSource::Solid { color: face_color_copy(fill) };
        }
        if seg_button(&mut cols[1], is_grad, ACCENT_AMBER, "Gradient", t).clicked() && !is_grad {
            *fill = SfxFillSource::Gradient { start: face_color_copy(fill), end: Color::WHITE, angle_deg: 90.0 };
        }
        if seg_button(&mut cols[2], is_dry, ACCENT_AMBER, "Dry Brush", t).clicked() && !is_dry {
            *fill = default_dry_brush_fill(face_color_copy(fill));
        }
    });
    ui.add_space(2.0);
    ui.columns(2, |cols| {
        if seg_button(&mut cols[0], is_tone, ACCENT_AMBER, "Screentone", t).clicked() && !is_tone {
            *fill = default_sfx_screentone();
        }
        if seg_button(&mut cols[1], is_hatch, ACCENT_AMBER, "⚡ Shtack Fill", t).clicked() && !is_hatch {
            *fill = SfxFillSource::ShtackHatch { spec: SfxShtackFillSpec { color: face_color_copy(fill), ..SfxShtackFillSpec::default() } };
        }
    });
    ui.add_space(3.0);
    match fill {
        SfxFillSource::None => {}
        SfxFillSource::Solid { color } => {
            color_swatch(ui, "Цвет:", color);
        }
        SfxFillSource::Gradient { start, end, angle_deg } => {
            ui.horizontal(|ui| {
                color_swatch(ui, "A:", start);
                color_swatch(ui, "B:", end);
                crate::widgets::value_field(ui, angle_deg, 0.0..=360.0, "°", 54.0);
            });
        }
        SfxFillSource::DryBrush { color, spec } => {
            color_swatch(ui, "Цвет кисти:", color);
            draw_dry_brush_editor(ui, spec, scale, t);
        }
        SfxFillSource::Screentone { spec, opacity } => {
            draw_screentone_grid(ui, id_salt, spec, opacity, t);
            ui.add_space(2.0);
            draw_open_gentonik_button(ui, out);
        }
        SfxFillSource::ShtackHatch { spec } => {
            draw_shtack_fill_editor(ui, spec, t);
        }
    }
}

fn blend_fx_title(fx: &SfxBlendEffect) -> &'static str {
    match fx {
        SfxBlendEffect::Bevel { .. } => "Тиснение",
        SfxBlendEffect::Stroke { .. } => "Stroke (Border)",
        SfxBlendEffect::InnerShadow { .. } => "Внутренняя тень",
        SfxBlendEffect::InnerGlow { .. } => "Внутреннее свечение",
        SfxBlendEffect::FillOverlay { is_pattern: false, .. } => "2-Я ЗАЛИВКА (БЫСТРЫЙ МИКС)",
        SfxBlendEffect::FillOverlay { is_pattern: true, .. } => "Наложение узора / Dry / Tone",
        SfxBlendEffect::OuterGlow { .. } => "Внешнее свечение",
        SfxBlendEffect::DropShadow { .. } => "☀ Направление тени / 3D",
    }
}

fn blend_fx_subtitle(fx: &SfxBlendEffect) -> String {
    match fx {
        SfxBlendEffect::FillOverlay { is_pattern: false, .. } => "Полный паритет с основной заливкой".to_owned(),
        SfxBlendEffect::FillOverlay { is_pattern: true, fill, .. } => match fill {
            SfxFillSource::DryBrush { .. } => "Dry Brush".to_owned(),
            SfxFillSource::Screentone { .. } => "Screentone (+ Gentonik)".to_owned(),
            SfxFillSource::ShtackHatch { .. } => "Shtack Fill".to_owned(),
            SfxFillSource::Gradient { .. } => "Gradient".to_owned(),
            SfxFillSource::Solid { .. } | SfxFillSource::None => "Solid".to_owned(),
        },
        SfxBlendEffect::Stroke { width_px, position, .. } => {
            let pos = match position {
                SfxStrokePosition::Outside => "Снаружи",
                SfxStrokePosition::Center => "Из центра",
                SfxStrokePosition::Inside => "Внутри",
            };
            format!("{pos} · {:.0}px", width_px)
        }
        SfxBlendEffect::DropShadow { angle_deg, distance_px, extrude_depth_px, .. } => {
            if *extrude_depth_px > 0.2 {
                format!("3D {:.0}px · {:.0}°", extrude_depth_px, angle_deg)
            } else {
                format!("{:.0}° · {:.0}px", angle_deg, distance_px)
            }
        }
        _ => String::new(),
    }
}

fn make_default_blend_fx(kind: u8) -> SfxBlendEffect {
    match kind {
        0 => SfxBlendEffect::Stroke {
            enabled: true,
            width_px: 4.0,
            position: SfxStrokePosition::Outside,
            join: SfxLineJoin::Round,
            fill: SfxFillSource::Solid { color: Color::BLACK },
            blend: SfxChannelBlend::Normal,
            opacity: 1.0,
        },
        1 => SfxBlendEffect::FillOverlay {
            enabled: true,
            is_pattern: false,
            fill: default_dry_brush_fill(Color::BLACK),
            blend: SfxChannelBlend::Normal,
            opacity: 1.0,
        },
        2 => SfxBlendEffect::FillOverlay {
            enabled: true,
            is_pattern: true,
            fill: default_sfx_screentone(),
            blend: SfxChannelBlend::Multiply,
            opacity: 0.85,
        },
        _ => SfxBlendEffect::DropShadow {
            enabled: true,
            angle_deg: 120.0,
            distance_px: 8.0,
            extrude_depth_px: 0.0,
            blur_px: 0.0,
            color: Color::BLACK,
            blend: SfxChannelBlend::Multiply,
            opacity: 0.85,
        },
    }
}

fn fx_row_pill(ui: &mut Ui, selected: bool, label: &str, text_color: Color32) -> egui::Response {
    if selected {
        ui.add(
            egui::Button::new(RichText::new(label).strong().size(10.5).color(Color32::WHITE))
                .fill(ACCENT_SKY)
                .stroke(Stroke::new(1.0, ACCENT_SKY))
                .corner_radius(3.0),
        )
    } else {
        ui.selectable_label(false, RichText::new(label).size(10.5).color(text_color))
    }
}

/// Curated Manga FX table matching the Photoshop-style Blending layout:
/// only mature, manga-ready passes are wired up; grey rows (`to be added`) stay disabled until designed for manga.
fn draw_blend_fx_catalog(ui: &mut Ui, st: &mut SfxPanelState, t: Tokens) {
    enum Row {
        Active { kind: u8, label: &'static str, accent_amber: bool },
        Placeholder { label: &'static str },
    }
    let rows = [
        Row::Active { kind: 0, label: "Stroke (Border)", accent_amber: false },
        Row::Placeholder { label: "Внутренняя тень" },
        Row::Active { kind: 1, label: "2-Я ЗАЛИВКА (БЫСТРЫЙ МИКС)", accent_amber: false },
        Row::Active { kind: 2, label: "Наложение узора / Dry / Tone", accent_amber: false },
        Row::Placeholder { label: "Внешнее свечение" },
        Row::Active { kind: 3, label: "☀ Направление тени / 3D", accent_amber: true },
    ];
    let can_add = st.draft.blending.effects.len() < MAX_SFX_BLEND_EFFECTS;
    for row in rows {
        match row {
            Row::Placeholder { label } => {
                ui.add_enabled_ui(false, |ui| {
                    ui.horizontal(|ui| {
                        let mut dummy = false;
                        ui.checkbox(&mut dummy, "");
                        ui.label(RichText::new(label).size(10.5).color(t.text_faint));
                        ui.add_space(4.0);
                        ui.label(RichText::new("to be added").italics().size(9.5).color(t.text_faint));
                    });
                });
            }
            Row::Active { kind, label, accent_amber } => {
                let first_idx = st.draft.blending.effects.iter().position(|fx| {
                    matches!(
                        (kind, fx),
                        (0, SfxBlendEffect::Stroke { .. })
                            | (1, SfxBlendEffect::FillOverlay { is_pattern: false, .. })
                            | (2, SfxBlendEffect::FillOverlay { is_pattern: true, .. })
                            | (3, SfxBlendEffect::DropShadow { .. })
                    )
                });
                let mut active = first_idx.is_some_and(|i| st.draft.blending.effects[i].enabled());
                let is_sel = first_idx.is_some() && st.selected_blend_fx == first_idx;
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut active, "").changed() {
                        if let Some(i) = first_idx {
                            *st.draft.blending.effects[i].enabled_mut() = active;
                            st.selected_blend_fx = Some(i);
                        } else if active && can_add {
                            st.draft.blending.effects.push(make_default_blend_fx(kind));
                            st.selected_blend_fx = Some(st.draft.blending.effects.len() - 1);
                        }
                    }
                    let text_col = if accent_amber {
                        TEXT_AMBER
                    } else if active {
                        t.text
                    } else {
                        t.text_dim
                    };
                    if fx_row_pill(ui, is_sel, label, text_col).clicked() {
                        if let Some(i) = first_idx {
                            st.selected_blend_fx = Some(i);
                        } else if can_add {
                            st.draft.blending.effects.push(make_default_blend_fx(kind));
                            st.selected_blend_fx = Some(st.draft.blending.effects.len() - 1);
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(can_add, egui::Button::new(RichText::new("+").strong().size(10.5)).min_size(vec2(20.0, 18.0))).clicked() {
                            st.draft.blending.effects.push(make_default_blend_fx(kind));
                            st.selected_blend_fx = Some(st.draft.blending.effects.len() - 1);
                        }
                    });
                });
            }
        }
    }
}

fn draw_blend_fx_inspector(ui: &mut Ui, fx: &mut SfxBlendEffect, idx: usize, scale: f32, t: Tokens, out: &mut SfxPanelOutput) {
    let salt = format!("sfx-bfx-{idx}");
    match fx {
        SfxBlendEffect::Bevel { depth_px, angle_deg, highlight_color, highlight_opacity, shadow_color, shadow_opacity, .. } => {
            slider_row_f32(ui, t, "Глубина:", depth_px, 1.0..=32.0 * scale, "px");
            slider_row_f32(ui, t, "Угол света:", angle_deg, -180.0..=180.0, "°");
            color_swatch(ui, "Блик:", highlight_color);
            let mut hp = *highlight_opacity * 100.0;
            if slider_row_f32(ui, t, "Яркость блика:", &mut hp, 0.0..=100.0, "%") {
                *highlight_opacity = (hp / 100.0).clamp(0.0, 1.0);
            }
            color_swatch(ui, "Тень:", shadow_color);
            let mut sp = *shadow_opacity * 100.0;
            if slider_row_f32(ui, t, "Плотность тени:", &mut sp, 0.0..=100.0, "%") {
                *shadow_opacity = (sp / 100.0).clamp(0.0, 1.0);
            }
        }
        SfxBlendEffect::Stroke { width_px, position, fill, blend, opacity, .. } => {
            draw_blend_channel_controls(ui, &format!("{salt}-bl"), "Режим обводки", blend, opacity, t);
            slider_row_f32(ui, t, "Размер обводки:", width_px, 0.5..=64.0 * scale, "px");
            ui.columns(3, |cols| {
                if seg_button(&mut cols[0], *position == SfxStrokePosition::Outside, ACCENT_SKY, "Снаружи", t).clicked() {
                    *position = SfxStrokePosition::Outside;
                }
                if seg_button(&mut cols[1], *position == SfxStrokePosition::Center, ACCENT_SKY, "Из центра", t).clicked() {
                    *position = SfxStrokePosition::Center;
                }
                if seg_button(&mut cols[2], *position == SfxStrokePosition::Inside, ACCENT_SKY, "Внутри", t).clicked() {
                    *position = SfxStrokePosition::Inside;
                }
            });
            ui.add_space(2.0);
            crate::widgets::hairline(ui);
            draw_parity_fill_editor(ui, &format!("{salt}-fill"), fill, scale, t, out);
        }
        SfxBlendEffect::DropShadow { angle_deg, distance_px, extrude_depth_px, blur_px, color, blend, opacity, .. } => {
            draw_blend_channel_controls(ui, &format!("{salt}-bl"), "Режим тени", blend, opacity, t);
            slider_row_f32(ui, t, "3D Extrude:", extrude_depth_px, 0.0..=48.0 * scale, "px");
            slider_row_f32(ui, t, "Угол тени:", angle_deg, -180.0..=180.0, "°");
            slider_row_f32(ui, t, "Дистанция тени:", distance_px, 0.0..=96.0 * scale, "px");
            slider_row_f32(ui, t, "Размытие:", blur_px, 0.0..=48.0 * scale, "px");
            color_swatch(ui, "Цвет тени:", color);
        }
        SfxBlendEffect::InnerShadow { angle_deg, distance_px, blur_px, color, blend, opacity, .. } => {
            draw_blend_channel_controls(ui, &format!("{salt}-bl"), "Режим тени", blend, opacity, t);
            color_swatch(ui, "Цвет тени:", color);
            slider_row_f32(ui, t, "Угол:", angle_deg, -180.0..=180.0, "°");
            slider_row_f32(ui, t, "Смещение:", distance_px, 0.0..=96.0 * scale, "px");
            slider_row_f32(ui, t, "Размытие:", blur_px, 0.0..=48.0 * scale, "px");
        }
        SfxBlendEffect::InnerGlow { width_px, color, blend, opacity, .. } => {
            draw_blend_channel_controls(ui, &format!("{salt}-bl"), "Режим свечения", blend, opacity, t);
            color_swatch(ui, "Цвет:", color);
            slider_row_f32(ui, t, "Размер:", width_px, 0.5..=48.0 * scale, "px");
        }
        SfxBlendEffect::OuterGlow { spread_px, blur_px, color, blend, opacity, .. } => {
            draw_blend_channel_controls(ui, &format!("{salt}-bl"), "Режим свечения", blend, opacity, t);
            color_swatch(ui, "Цвет:", color);
            slider_row_f32(ui, t, "Размах:", spread_px, 0.0..=48.0 * scale, "px");
            slider_row_f32(ui, t, "Размытие:", blur_px, 0.0..=48.0 * scale, "px");
        }
        SfxBlendEffect::FillOverlay { is_pattern, fill, blend, opacity, .. } => {
            let lbl = if *is_pattern { "Смешивание узора" } else { "Смешивание 2-й заливки" };
            draw_blend_channel_controls(ui, &format!("{salt}-bl"), lbl, blend, opacity, t);
            crate::widgets::hairline(ui);
            draw_parity_fill_editor(ui, &format!("{salt}-fill"), fill, scale, t, out);
        }
    }
}

fn draw_blending_tab(ui: &mut Ui, st: &mut SfxPanelState, t: Tokens, out: &mut SfxPanelOutput) {
    let scale = ui_scale(st);
    // 1. Unified Photoshop-style Manga FX table, active stack, and live inspector
    section_card(ui, t, |ui| {
        draw_blend_fx_catalog(ui, st, t);

        if !st.draft.blending.effects.is_empty() {
            crate::widgets::hairline(ui);
            dim_label(ui, t, "Активный стек FX (сверху вниз):");
            let total = st.draft.blending.effects.len();
            let mut swap_op: Option<(usize, usize)> = None;
            let mut del_idx: Option<usize> = None;
            for idx in 0..total {
                let is_sel = st.selected_blend_fx == Some(idx);
                let title = blend_fx_title(&st.draft.blending.effects[idx]);
                let sub = blend_fx_subtitle(&st.draft.blending.effects[idx]);
                ui.horizontal(|ui| {
                    ui.checkbox(st.draft.blending.effects[idx].enabled_mut(), "");
                    let lbl = format!("{}. {title}", idx + 1);
                    if fx_row_pill(ui, is_sel, &lbl, t.text).clicked() {
                        st.selected_blend_fx = Some(idx);
                    }
                    if !sub.is_empty() {
                        ui.label(RichText::new(sub).size(9.5).color(t.text_dim));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("🗑").on_hover_text("Удалить эффект").clicked() {
                            del_idx = Some(idx);
                        }
                        if idx + 1 < total && ui.small_button("↓").clicked() {
                            swap_op = Some((idx, idx + 1));
                        }
                        if idx > 0 && ui.small_button("↑").clicked() {
                            swap_op = Some((idx - 1, idx));
                        }
                    });
                });
            }
            if let Some((a, b)) = swap_op {
                st.draft.blending.effects.swap(a, b);
                if st.selected_blend_fx == Some(a) {
                    st.selected_blend_fx = Some(b);
                } else if st.selected_blend_fx == Some(b) {
                    st.selected_blend_fx = Some(a);
                }
            }
            if let Some(rm) = del_idx {
                st.draft.blending.effects.remove(rm);
                st.selected_blend_fx = if st.draft.blending.effects.is_empty() {
                    None
                } else {
                    Some(rm.min(st.draft.blending.effects.len() - 1))
                };
            }
            let sel = st.selected_blend_fx.filter(|&i| i < st.draft.blending.effects.len()).or(if st.draft.blending.effects.is_empty() {
                None
            } else {
                Some(0)
            });
            st.selected_blend_fx = sel;
            if let Some(idx) = sel {
                crate::widgets::hairline(ui);
                let hdr = format!("НАСТРОЙКИ: {}", blend_fx_title(&st.draft.blending.effects[idx]));
                ui.label(RichText::new(hdr).strong().size(10.0).color(TEXT_SKY));
                ui.add_space(2.0);
                draw_blend_fx_inspector(ui, &mut st.draft.blending.effects[idx], idx, scale, t, out);
            }
        }
    });

    // 2. Compact collapsible base channel ordering (Fill 1 / Line / Halo) — out of the way by default
    let bl = &mut st.draft.blending;
    egui::CollapsingHeader::new(RichText::new("Порядок и базовые каналы (Fill / Line / Halo)").size(10.0).color(t.text_dim))
        .id_salt("sfx-bl-base-channels")
        .default_open(false)
        .show(ui, |ui| {
            section_card(ui, t, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("ПОРЯДОК И КАНАЛЫ").strong().size(10.0).color(t.text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Сбросить всё").clicked() {
                            *bl = SfxBlending::default();
                            st.selected_blend_fx = None;
                        }
                    });
                });
                ui.add_space(3.0);
                ui.columns(2, |cols| {
                    if seg_button(&mut cols[0], !bl.line_behind_fill, ACCENT_SKY, "Лайн поверх заливки", t).clicked() {
                        bl.line_behind_fill = false;
                    }
                    if seg_button(&mut cols[1], bl.line_behind_fill, ACCENT_SKY, "Лайн под заливкой", t).clicked() {
                        bl.line_behind_fill = true;
                    }
                });
                ui.add_space(4.0);
                crate::widgets::hairline(ui);
                draw_blend_channel_controls(ui, "sfx-bl-fill", "Основная заливка (Fill 1)", &mut bl.fill_blend, &mut bl.fill_opacity, t);
                crate::widgets::hairline(ui);
                draw_blend_channel_controls(ui, "sfx-bl-line", "Лайн / Обводка (Line)", &mut bl.line_blend, &mut bl.line_opacity, t);
                crate::widgets::hairline(ui);
                draw_blend_channel_controls(ui, "sfx-bl-halo", "Ореол (Halo)", &mut bl.halo_blend, &mut bl.halo_opacity, t);
            });
        });
}

fn draw_color_tab(ui: &mut Ui, st: &mut SfxPanelState, t: Tokens) {
    let scale = ui_scale(st);
    section_card(ui, t, |ui| {
        section_header(ui, t, "ЦВЕТА И ТОЛЩИНЫ СЛОЁВ", "2-Pass Halo + Ink", TEXT_SKY);
        ui.add_space(4.0);

        draw_fill_source_editor(ui, "sfx-color-tab-fill", &mut st.draft.fill, t);
        crate::widgets::hairline(ui);
        color_swatch(ui, "Цвет ореола (Pass 1):", &mut st.draft.line_effect.halo_color);
        slider_row_f32(ui, t, "Толщина ореола:", &mut st.draft.line_effect.halo_width_px, 0.0..=36.0 * scale, "px");
        crate::widgets::hairline(ui);
        color_swatch(ui, "Цвет лайна (Pass 2):", &mut st.draft.line_effect.line_color);
        slider_row_f32(ui, t, "Толщина лайна:", &mut st.draft.line_effect.line_width_px, 0.0..=24.0 * scale, "px");
        crate::widgets::hairline(ui);
        color_swatch(ui, "Цвет тени / 3D:", &mut st.draft.vector_shadow.color);
    });

    egui::CollapsingHeader::new(RichText::new("</> Показать JSON-шов (SfxData)").monospace().size(10.0).color(TEXT_EMERALD))
        .id_salt("sfx-json-seam")
        .default_open(false)
        .show(ui, |ui| {
            let preview = serde_json::to_string_pretty(&st.draft).unwrap_or_default();
            egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(preview).monospace().size(9.5).color(TEXT_EMERALD)).wrap());
            });
        });
}

fn draw_screentone_grid(ui: &mut Ui, id_salt: &str, spec: &mut ScreentoneSpec, opacity: &mut f32, t: Tokens) {
    egui::Grid::new(format!("{id_salt}-tone")).num_columns(2).spacing(vec2(6.0, 3.0)).show(ui, |ui| {
        dim_label(ui, t, "Pattern:");
        let mut cur = enum_id(&spec.pattern_type);
        let opts: Vec<(String, &str)> = SCREENTONE_PATTERNS.iter().map(|s| ((*s).to_owned(), *s)).collect();
        if crate::widgets::dropdown_literal(ui, &format!("{id_salt}-pat"), &mut cur, &opts, 108.0)
            && let Some(p) = enum_from_id::<PatternType>(&cur)
        {
            spec.pattern_type = p;
        }
        ui.end_row();

        dim_label(ui, t, "Shape:");
        let mut cur_s = enum_id(&spec.dot_shape);
        let opts_s: Vec<(String, &str)> = SCREENTONE_SHAPES.iter().map(|s| ((*s).to_owned(), *s)).collect();
        if crate::widgets::dropdown_literal(ui, &format!("{id_salt}-shp"), &mut cur_s, &opts_s, 108.0)
            && let Some(d) = enum_from_id::<DotShape>(&cur_s)
        {
            spec.dot_shape = d;
        }
        ui.end_row();

        dim_label(ui, t, "Dot / Step:");
        ui.horizontal(|ui| {
            crate::widgets::value_field(ui, &mut spec.dot_size, 0.5..=256.0, "px", 54.0);
            if crate::widgets::value_field(ui, &mut spec.spacing_x, 2.0..=512.0, "px", 54.0).changed() {
                spec.spacing_y = spec.spacing_x;
            }
        });
        ui.end_row();

        dim_label(ui, t, "Angle / Op:");
        ui.horizontal(|ui| {
            crate::widgets::value_field(ui, &mut spec.angle, -180.0..=180.0, "°", 54.0);
            crate::widgets::value_field(ui, opacity, 0.0..=1.0, "", 54.0);
        });
        ui.end_row();
    });
}

fn draw_fill_source_editor(ui: &mut Ui, id_salt: &str, fill: &mut SfxFillSource, t: Tokens) {
    let kind_idx = match fill {
        SfxFillSource::None => 0u8,
        SfxFillSource::Solid { .. } => 1u8,
        SfxFillSource::Gradient { .. } => 2u8,
        SfxFillSource::DryBrush { .. } => 3u8,
        SfxFillSource::Screentone { .. } => 4u8,
        SfxFillSource::ShtackHatch { .. } => 5u8,
    };
    let mut picked = kind_idx;
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(&mut picked, 0, "None");
        ui.selectable_value(&mut picked, 1, "Solid");
        ui.selectable_value(&mut picked, 2, "Gradient");
        ui.selectable_value(&mut picked, 3, "Dry Brush");
        ui.selectable_value(&mut picked, 4, "Screentone");
        ui.selectable_value(&mut picked, 5, "Shtack Fill");
    });
    if picked != kind_idx {
        *fill = match picked {
            0 => SfxFillSource::None,
            1 => SfxFillSource::Solid { color: Color::BLACK },
            2 => SfxFillSource::Gradient { start: Color::BLACK, end: Color::WHITE, angle_deg: 90.0 },
            3 => default_dry_brush_fill(Color::BLACK),
            4 => default_sfx_screentone(),
            _ => SfxFillSource::ShtackHatch { spec: SfxShtackFillSpec::default() },
        };
    }

    match fill {
        SfxFillSource::None => {
            ui.label(RichText::new("Без заливки (полый)").size(10.0).color(t.text_dim));
        }
        SfxFillSource::Solid { color } => {
            color_swatch(ui, "Цвет заливки:", color);
        }
        SfxFillSource::Gradient { start, end, angle_deg } => {
            ui.horizontal(|ui| {
                color_swatch(ui, "A:", start);
                color_swatch(ui, "B:", end);
                crate::widgets::value_field(ui, angle_deg, 0.0..=360.0, "°", 54.0);
            });
        }
        SfxFillSource::DryBrush { color, spec } => {
            color_swatch(ui, "Цвет кисти:", color);
            draw_dry_brush_editor(ui, spec, 1.0, t);
        }
        SfxFillSource::Screentone { spec, opacity } => {
            draw_screentone_grid(ui, id_salt, spec, opacity, t);
        }
        SfxFillSource::ShtackHatch { spec } => {
            draw_shtack_fill_editor(ui, spec, t);
        }
    }
}

#[allow(dead_code)]
fn pass_title(pass: &SfxStylePass) -> &'static str {
    match pass {
        SfxStylePass::Stroke { .. } => "Stroke (Border)",
        SfxStylePass::Extrude3D { .. } => "3D Extrude",
        SfxStylePass::Shadow { .. } => "Drop Shadow",
        SfxStylePass::InnerGlow { .. } => "Inner Glow",
        SfxStylePass::NegativeInvert { .. } => "Negative / Invert (Difference)",
    }
}

#[allow(dead_code)]
fn draw_styles_stack(ui: &mut Ui, st: &mut SfxPanelState, t: Tokens) {
    ui.horizontal_wrapped(|ui| {
        if ui.small_button("+ Stroke").clicked() {
            st.draft.styles.push(SfxStylePass::Stroke {
                enabled: true,
                width_px: 4.0,
                join: SfxLineJoin::Round,
                miter_limit: 4.0,
                fill: SfxFillSource::Solid { color: Color::WHITE },
            });
        }
        if ui.small_button("+ 3D").clicked() {
            st.draft.styles.push(SfxStylePass::Extrude3D {
                enabled: true,
                dx_px: 8.0,
                dy_px: 8.0,
                steps: 14,
                fill: SfxFillSource::Solid { color: Color::BLACK },
            });
        }
        if ui.small_button("+ Shadow").clicked() {
            st.draft.styles.push(SfxStylePass::Shadow {
                enabled: true,
                dx_px: 6.0,
                dy_px: 6.0,
                blur_px: 3.0,
                color: Color::BLACK,
                opacity: 0.7,
            });
        }
        if ui.small_button("+ Glow").clicked() {
            st.draft.styles.push(SfxStylePass::InnerGlow { enabled: true, width_px: 3.0, color: Color::WHITE, opacity: 0.85 });
        }
    });

    let mut swap_op: Option<(usize, usize)> = None;
    let mut delete_idx: Option<usize> = None;
    let total = st.draft.styles.len();

    for (idx, pass) in st.draft.styles.iter_mut().enumerate() {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                match pass {
                    SfxStylePass::Stroke { enabled, .. }
                    | SfxStylePass::Extrude3D { enabled, .. }
                    | SfxStylePass::Shadow { enabled, .. }
                    | SfxStylePass::InnerGlow { enabled, .. }
                    | SfxStylePass::NegativeInvert { enabled } => {
                        ui.checkbox(enabled, "");
                    }
                }
                ui.strong(pass_title(pass));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("x").clicked() {
                        delete_idx = Some(idx);
                    }
                    if idx + 1 < total && ui.small_button("v").clicked() {
                        swap_op = Some((idx, idx + 1));
                    }
                    if idx > 0 && ui.small_button("^").clicked() {
                        swap_op = Some((idx - 1, idx));
                    }
                });
            });

            match pass {
                SfxStylePass::Stroke { width_px, join, miter_limit, fill, .. } => {
                    ui.horizontal(|ui| {
                        dim_label(ui, t, "Width:");
                        crate::widgets::value_field(ui, width_px, 0.0..=128.0, "px", 50.0);
                        let joins = [(SfxLineJoin::Round, "Round"), (SfxLineJoin::Miter, "Miter"), (SfxLineJoin::Bevel, "Bevel")];
                        crate::widgets::dropdown_literal(ui, &format!("sfx-join-{idx}"), join, &joins, 68.0);
                        if *join == SfxLineJoin::Miter {
                            crate::widgets::value_field(ui, miter_limit, 1.0..=24.0, "m", 42.0);
                        }
                    });
                    draw_fill_source_editor(ui, &format!("sfx-stroke-fill-{idx}"), fill, t);
                }
                SfxStylePass::Extrude3D { dx_px, dy_px, steps, fill, .. } => {
                    ui.horizontal(|ui| {
                        crate::widgets::value_field(ui, dx_px, -256.0..=256.0, "x", 48.0);
                        crate::widgets::value_field(ui, dy_px, -256.0..=256.0, "y", 48.0);
                        let mut st_f = f32::from(*steps);
                        if crate::widgets::value_field(ui, &mut st_f, 1.0..=64.0, "s", 44.0).changed() {
                            *steps = st_f.round().clamp(1.0, 64.0) as u16;
                        }
                    });
                    draw_fill_source_editor(ui, &format!("sfx-ext-fill-{idx}"), fill, t);
                }
                SfxStylePass::Shadow { dx_px, dy_px, blur_px, color, opacity, .. } => {
                    ui.horizontal(|ui| {
                        crate::widgets::value_field(ui, dx_px, -256.0..=256.0, "x", 46.0);
                        crate::widgets::value_field(ui, dy_px, -256.0..=256.0, "y", 46.0);
                        crate::widgets::value_field(ui, blur_px, 0.0..=64.0, "b", 46.0);
                        crate::widgets::value_field(ui, opacity, 0.0..=1.0, "a", 42.0);
                    });
                    color_swatch(ui, "Shadow:", color);
                }
                SfxStylePass::InnerGlow { width_px, color, opacity, .. } => {
                    ui.horizontal(|ui| {
                        crate::widgets::value_field(ui, width_px, 0.0..=64.0, "px", 50.0);
                        crate::widgets::value_field(ui, opacity, 0.0..=1.0, "a", 50.0);
                        color_swatch(ui, "Glow:", color);
                    });
                }
                SfxStylePass::NegativeInvert { .. } => {
                    ui.label(RichText::new("Composites glyph silhouette via Difference blend group.").color(t.text_dim).size(10.5));
                }
            }
        });
    }

    if let Some((a, b)) = swap_op {
        st.draft.styles.swap(a, b);
    }
    if let Some(rm) = delete_idx {
        st.draft.styles.remove(rm);
    }
}

#[allow(dead_code)]
fn draw_distress_section(ui: &mut Ui, st: &mut SfxPanelState, t: Tokens) {
    ui.horizontal(|ui| {
        ui.checkbox(&mut st.draft.distress.enabled, RichText::new("Deterministic Distress").strong().size(11.0));
        if ui.small_button("Reset").clicked() {
            st.draft.distress = SfxDistress::default();
        }
    });
    if !st.draft.distress.enabled {
        return;
    }
    egui::Grid::new("sfx-distress-grid").num_columns(2).spacing(vec2(8.0, 4.0)).show(ui, |ui| {
        dim_label(ui, t, "Seed:");
        ui.horizontal(|ui| {
            if ui.small_button("-").clicked() {
                st.draft.distress.seed = st.draft.distress.seed.saturating_sub(1);
            }
            let mut s = st.draft.distress.seed as f32;
            if crate::widgets::value_field(ui, &mut s, 0.0..=99_999.0, "", 62.0).changed() {
                st.draft.distress.seed = s.round().max(0.0) as u32;
            }
            if ui.small_button("+").clicked() {
                st.draft.distress.seed = st.draft.distress.seed.saturating_add(1);
            }
        });
        ui.end_row();

        dim_label(ui, t, "Roughness:");
        crate::widgets::value_field(ui, &mut st.draft.distress.roughness, 0.0..=16.0, "px", 62.0);
        ui.end_row();

        dim_label(ui, t, "Grain / Chip:");
        ui.horizontal(|ui| {
            crate::widgets::value_field(ui, &mut st.draft.distress.grain, 0.0..=1.0, "", 58.0);
            crate::widgets::value_field(ui, &mut st.draft.distress.edge_chip, 0.0..=1.0, "", 58.0);
        });
        ui.end_row();
    });
}

fn draw_actions_section(ui: &mut Ui, st: &mut SfxPanelState, target: &SfxTargetInfo<'_>, out: &mut SfxPanelOutput) {
    let has_doc = target.doc_id.is_some();
    let has_sfx = target.layer_id.is_some() && target.data.is_some();
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(has_doc, egui::Button::new("Create SFX Layer")).clicked() {
            let mut sfx = st.draft.clone();
            sfx.validate();
            out.commands.push(OutCommand::new("sfx.create", json!({ "sfx": sfx })));
        }
        if ui
            .add_enabled(target.active_is_text, egui::Button::new("Convert Text -> SFX"))
            .on_hover_text("Convert the active Type layer into a non-destructive SFX layer")
            .clicked()
            && let Some(id) = target.layer_id
        {
            out.commands.push(OutCommand::new("sfx.convertFromText", json!({ "layer": id.0 })));
        }
        if has_sfx
            && let Some(id) = target.layer_id
            && ui
                .button("To Smart Object")
                .on_hover_text("Wrap this SFX layer in a Smart Object (keeps live SFX editable via Edit Contents and exports as a Photoshop Smart Object in PSD)")
                .clicked()
        {
            out.commands.push(OutCommand::new("layer.smartObjects.convertToSmartObject", json!({ "layer": id.0 })));
        }
        if !has_sfx && target.active_is_smart && let Some(id) = target.layer_id {
            if ui
                .button("Edit Smart Contents")
                .on_hover_text("Open the embedded Smart Object document to edit its live SFX layer")
                .clicked()
            {
                out.commands.push(OutCommand::new("layer.smartObjects.editContents", json!({ "layer": id.0 })));
            }
            if ui
                .button("Unpack Smart")
                .on_hover_text("Convert Smart Object back into regular layers on this page")
                .clicked()
            {
                out.commands.push(OutCommand::new("layer.smartObjects.convertToLayers", json!({ "layer": id.0 })));
            }
        }
        if ui
            .add_enabled(has_sfx, egui::Button::new("Bake to Raster"))
            .on_hover_text("Freeze the SFX layer into standard pixels")
            .clicked()
            && let Some(id) = target.layer_id
        {
            out.commands.push(OutCommand::new("sfx.bakeToRaster", json!({ "layer": id.0 })));
        }
    });
}

#[cfg(test)]
mod tests {
    use photocraft_doc::LayerId;

    use super::*;

    #[test]
    fn panel_renders_idle_and_emits_update_on_style_edit() {
        let mut st = SfxPanelState::default();
        let mut gizmo = SfxGizmoState::default();
        let presets = vec![SfxPreset::default()];
        let target = SfxTargetInfo {
            doc_id: Some(BridgeDocId(1)),
            layer_id: Some(LayerId(9)),
            data: Some(SfxData::default()),
            active_is_text: false,
            active_is_smart: false,
            doc_dpi: 72.0,
            gpu_active: false,
            presets: &presets,
        };
        let ctx = egui::Context::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(320.0, 900.0))), ..Default::default() };
        let mut captured = None;
        let mut out = ctx.run_ui(input, |ui| {
            captured = Some(egui::CentralPanel::default().show(ui, |ui| panel(ui, &mut st, &mut gizmo, &target)).inner);
        });
        out.textures_delta.clear();
        assert!(captured.is_some());
    }

    #[test]
    fn sync_from_host_updates_draft_when_active_layer_changes() {
        let mut st = SfxPanelState::default();
        let presets = Vec::new();
        let d1 = SfxData { text: "CRACK".to_owned(), ..SfxData::default() };
        let t1 = SfxTargetInfo {
            doc_id: Some(BridgeDocId(1)),
            layer_id: Some(LayerId(2)),
            data: Some(d1),
            active_is_text: false,
            active_is_smart: false,
            doc_dpi: 72.0,
            gpu_active: false,
            presets: &presets,
        };
        st.sync_from_host(&t1);
        assert_eq!(st.draft.text, "CRACK");
    }

    #[test]
    fn panel_renders_all_tabs_line_styles_and_dry_brush_engines() {
        let mut st = SfxPanelState::default();
        let mut gizmo = SfxGizmoState::default();
        let presets = vec![SfxPreset::default()];
        let ctx = egui::Context::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(320.0, 900.0))), ..Default::default() };

        for tab in [DockTab::Sfx, DockTab::Blending, DockTab::Color] {
            st.active_tab = tab;
            for style in [
                SfxLineStyle::Clean,
                SfxLineStyle::OneBit,
                SfxLineStyle::Spikes,
                SfxLineStyle::GPenNib,
                SfxLineStyle::BrushStroke,
                SfxLineStyle::Shtack,
                SfxLineStyle::ShibufudeV3,
                SfxLineStyle::SugomiV3,
            ] {
                let mut data = SfxData::default();
                data.line_effect.style = style;
                if tab == DockTab::Blending {
                    data.blending.effects = vec![make_default_blend_fx(0), make_default_blend_fx(1), make_default_blend_fx(2), make_default_blend_fx(3)];
                } else if style == SfxLineStyle::Shtack {
                    data.fill = SfxFillSource::ShtackHatch { spec: SfxShtackFillSpec::default() };
                } else {
                    data.dry_brush.enabled = true;
                    data.dry_brush.engine = SfxDryBrushEngine::Vector;
                }
                let target = SfxTargetInfo {
                    doc_id: Some(BridgeDocId(1)),
                    layer_id: Some(LayerId(3)),
                    data: Some(data),
                    active_is_text: false,
                    active_is_smart: false,
                    doc_dpi: 72.0,
                    gpu_active: false,
                    presets: &presets,
                };
                let mut out = ctx.run_ui(input.clone(), |ui| {
                    let _ = egui::CentralPanel::default().show(ui, |ui| panel(ui, &mut st, &mut gizmo, &target));
                });
                out.textures_delta.clear();
            }
        }
    }

    /// Regression: clicking `+ Точка на дуге` on wide SFX used to hardcode `[-160, 160]` and crush
    /// letters together. Initializing from real glyph rest centers keeps every letter at its exact
    /// original position (zero squishing / zero jump).
    #[test]
    fn add_spline_point_preserves_glyph_spacing_on_wide_sfx() {
        let mut draft = SfxData { text: "BAM!".to_owned(), size_pt: 260.0, origin: [80.0, 80.0], ..SfxData::default() };
        let gizmo = SfxGizmoState::default();
        let before_geom = gizmo.ensure_geom(&draft, 72.0);
        assert!(before_geom.glyphs.len() >= 4);
        let before_centers: Vec<[f64; 2]> = before_geom.glyphs.iter().map(|g| g.warped_center).collect();

        add_spline_point(&mut draft, &before_geom);
        assert_eq!(draft.spline.points.len(), 3);
        let after_geom = gizmo.ensure_geom(&draft, 72.0);
        for (i, (a, b)) in before_centers.iter().zip(after_geom.glyphs.iter().map(|g| g.warped_center)).enumerate() {
            assert!(
                (a[0] - b[0]).abs() < 1.5 && (a[1] - b[1]).abs() < 1.5,
                "glyph {i} jumped/squished when adding spline point: before={a:?}, after={b:?}"
            );
        }
    }
}