//! Dock panel with the PaintTool SAI Ver.2 Vector Linework parameters (`crates/ui-egui/src/comic_sai_panel.rs`).
//!
//! Rubber layout per the mockup (`TZ v5.0`, base dock width 349 px, usable content ~330 px):
//! a live S-curve preview on a checkerboard with a copper 2 px frame, 5 tip-profile presets,
//! integrated SAI parameter bars (the number and the `x0.1` fine-step badge are drawn *inside*
//! the 19 px track), the compact Width/Density pressure toggle synced with the Min Size / Min
//! Density checkboxes, three wrapping flags, a dense quick-sizes table rebuilt without
//! `egui::Grid` (36 px cells, 2..=9 columns, never a clipped column), and full-width action
//! buttons. 38 quick sizes, pressure dynamics (Width / Density), curve type
//! (Normal Catmull-Rom / Bezier), tension, auto-taper toggle, and 1-click layer creation.

use egui::{Align2, Color32, FontId, Painter, Rect, RichText, Slider, Stroke, Ui, pos2, vec2};
use photocraft_vector::comic_sai_curve::{SAI_DEFAULT_TENSION, SaiCurveType};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::state::Tool;
use crate::theme::Tokens;

/// The Window-menu id of the panel.
pub const PANEL: &str = "window.panel.linework";

/// The 38 quick sizes of PaintTool SAI (`0.7 .. 500 px`).
pub const SAI_QUICK_SIZES: [f32; 38] = [
    0.7, 0.8, 1.0, 1.5, 2.0, 2.3, 2.6, 3.0, // Row 1
    3.5, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, // Row 2
    12.0, 14.0, 16.0, 20.0, 25.0, 30.0, 35.0, 40.0, // Row 3
    50.0, 60.0, 70.0, 80.0, 100.0, 120.0, 160.0, 200.0, // Row 4
    250.0, 300.0, 350.0, 400.0, 450.0, 500.0, // Row 5
];

/// The mockup's copper-orange 2 px frame around the curve preview.
const COPPER_ORANGE: Color32 = Color32::from_rgb(200, 110, 45);
/// The gold 1 px frame of the active tip-profile button and quick-size cell.
const GOLD: Color32 = Color32::from_rgb(212, 175, 55);
/// The height of one SAI parameter bar (`sai_bar_slider`), as on the mockup.
const SAI_BAR_H: f32 = 19.0;
/// The width of a row's label column (an optional checkbox sits at its right edge).
const SAI_LABEL_COL_W: f32 = 110.0;
/// Below this usable width a row's control wraps under its label at full width.
const SAI_ROW_WRAP_W: f32 = 215.0;
/// The height of one quick-size cell of the dense table.
const SAI_QUICK_CELL_H: f32 = 35.0;
/// The minimum width of one quick-size cell: the column count follows the dock's width.
const SAI_QUICK_MIN_CELL_W: f32 = 36.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TipProfileId {
    #[default]
    Round,
    Calligraphic,
    Soft,
    SemiFlat,
    Uniform,
}

impl TipProfileId {
    /// The minimum-size ratio the preset arms (`Uniform` = no taper at all).
    fn ratio(self) -> f32 {
        match self {
            Self::Round => 0.0,
            Self::Calligraphic => 0.05,
            Self::Soft => 0.25,
            Self::SemiFlat => 0.50,
            Self::Uniform => 1.0,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Round => "Round",
            Self::Calligraphic => "Calligraphic",
            Self::Soft => "Soft",
            Self::SemiFlat => "SemiFlat",
            Self::Uniform => "Uniform",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::Round => "●",
            Self::Calligraphic => "▲",
            Self::Soft => "■",
            Self::SemiFlat => "▬",
            Self::Uniform => "█",
        }
    }

    const ALL: [TipProfileId; 5] = [TipProfileId::Round, TipProfileId::Calligraphic, TipProfileId::Soft, TipProfileId::SemiFlat, TipProfileId::Uniform];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PressureTargetMode {
    #[default]
    Width,
    Density,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LineworkPanelState {
    pub tip_profile: TipProfileId,
    pub base_width: f32,
    pub min_size_ratio: f32,
    pub base_density: f32,
    pub min_density_ratio: f32,
    pub curve_type: SaiCurveType,
    pub tension: f64,
    pub pressure_target: PressureTargetMode,
    pub auto_taper_ends: bool,
    pub closed_loop: bool,
    pub snap_close: bool,
    /// Whether the quick-sizes table is expanded (the `▾` on the Brush Size row).
    #[serde(default = "default_show_quick_sizes")]
    pub show_quick_sizes: bool,
}

fn default_show_quick_sizes() -> bool {
    true
}

impl Default for LineworkPanelState {
    fn default() -> Self {
        Self {
            tip_profile: TipProfileId::Round,
            base_width: 8.0,
            min_size_ratio: 0.0,
            base_density: 1.0,
            min_density_ratio: 0.0,
            curve_type: SaiCurveType::Normal,
            tension: SAI_DEFAULT_TENSION,
            pressure_target: PressureTargetMode::Width,
            auto_taper_ends: true,
            closed_loop: false,
            snap_close: true,
            show_quick_sizes: true,
        }
    }
}

pub fn handles(id: &str) -> bool {
    id == PANEL
}

pub const LINEWORK_LAYERS_TAB: usize = 3;

pub fn showing(app: &PhotocraftApp) -> bool {
    if app.ui.docking.is_some() || crate::panel_docking::panel_in_side_dock(app, "linework") {
        crate::panel_docking::panel_active_anywhere(app, "linework")
    } else {
        app.ui.panels.layers && app.ui.dock_tabs.layers == LINEWORK_LAYERS_TAB && !app.ui.dock.is_collapsed(crate::dock::Group::Layers)
    }
}

pub fn checked(app: &PhotocraftApp, id: &str) -> Option<bool> {
    if handles(id) { if let Some(c) = crate::panel_docking::checked(app, id) { Some(c) } else { Some(showing(app)) } } else { None }
}

pub fn menu(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if !handles(id) {
        return None;
    }
    log::info!("[Telemetry:Dock] comic_sai_panel::menu: id={id}, docking_present={}", app.ui.docking.is_some());
    if app.ui.docking.is_some() || crate::panel_docking::panel_in_side_dock(app, "linework") {
        let want = params.get("show").and_then(Value::as_bool);
        let show = want.unwrap_or(!showing(app));
        log::info!("[Telemetry:Dock] comic_sai_panel delegating to panel_docking: show={show}");
        return Some(crate::panel_docking::command(app, "window.panel.linework", &json!({"show": show})).unwrap_or(Ok(json!({"visible": show}))));
    }
    let want = params.get("show").and_then(Value::as_bool);
    let show = want.unwrap_or(!showing(app));
    app.ui.panels.layers = show || (app.ui.panels.layers && app.ui.dock_tabs.layers != LINEWORK_LAYERS_TAB);
    if show {
        app.ui.dock_tabs.layers = LINEWORK_LAYERS_TAB;
        crate::dock::reveal(app, crate::dock::Group::Layers);
    }
    Some(Ok(json!({ "showing": show })))
}

fn paint_checkers(painter: &Painter, rect: Rect) {
    painter.rect_filled(rect, 4.0, Color32::from_rgb(24, 24, 27));
    let tile = 8.0;
    let cols = (rect.width() / tile).ceil() as i32;
    let rows = (rect.height() / tile).ceil() as i32;
    for r in 0..rows {
        for c in 0..cols {
            if (r + c) % 2 == 0 {
                let x0 = rect.min.x + (c as f32) * tile;
                let y0 = rect.min.y + (r as f32) * tile;
                let x1 = (x0 + tile).min(rect.max.x);
                let y1 = (y0 + tile).min(rect.max.y);
                painter.rect_filled(Rect::from_min_max(pos2(x0, y0), pos2(x1, y1)), 0.0, Color32::from_rgb(34, 34, 40));
            }
        }
    }
}

fn build_adaptive_slider<'a, Num: egui::emath::Numeric>(val: &'a mut Num, range: std::ops::RangeInclusive<Num>, suffix: &str, logarithmic: bool) -> Slider<'a> {
    let mut s = Slider::new(val, range);
    if !suffix.is_empty() {
        s = s.suffix(suffix);
    }
    if logarithmic {
        s = s.logarithmic(true);
    }
    s
}

pub(crate) fn adaptive_slider<Num: egui::emath::Numeric>(
    ui: &mut Ui,
    label: &str,
    value: &mut Num,
    range: std::ops::RangeInclusive<Num>,
    suffix: &str,
    logarithmic: bool,
) -> egui::Response {
    let avail = ui.available_width();
    if avail < 265.0 {
        ui.vertical(|ui| {
            ui.label(label);
            let prev_w = ui.spacing().slider_width;
            ui.spacing_mut().slider_width = (avail - 72.0).max(56.0);
            let resp = ui.add(build_adaptive_slider(value, range, suffix, logarithmic));
            ui.spacing_mut().slider_width = prev_w;
            resp
        })
        .inner
    } else {
        ui.horizontal(|ui| {
            let lbl_resp = ui.label(label);
            let used = lbl_resp.rect.width() + ui.spacing().item_spacing.x;
            let rem = (avail - used).max(80.0);
            let prev_w = ui.spacing().slider_width;
            ui.spacing_mut().slider_width = (rem - 72.0).clamp(60.0, 200.0);
            let resp = ui.add(build_adaptive_slider(value, range, suffix, logarithmic));
            ui.spacing_mut().slider_width = prev_w;
            resp
        })
        .inner
    }
}

pub fn draw_sai_preview(ui: &mut Ui, base_width: f32, min_ratio: f32, base_density: f32, min_density: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().max(60.0), 53.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);

    paint_checkers(&painter, rect);

    let p0 = pos2(rect.min.x + 16.0, rect.max.y - 10.0);
    let p1 = pos2(rect.min.x + rect.width() * 0.35, rect.min.y + 8.0);
    let p2 = pos2(rect.min.x + rect.width() * 0.65, rect.max.y - 8.0);
    let p3 = pos2(rect.max.x - 16.0, rect.min.y + 10.0);

    let eff_density = (min_density + (1.0 - min_density) * base_density).clamp(0.0, 1.0);
    let alpha = (eff_density * 255.0).clamp(0.0, 255.0) as u8;
    let stroke_color = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha);
    let eff_width = (base_width * (0.35 + 0.65 * (1.0 - min_ratio * 0.5))).clamp(1.0, 28.0);

    painter.add(egui::Shape::CubicBezier(egui::epaint::CubicBezierShape::from_points_stroke(
        [p0, p1, p2, p3],
        false,
        Color32::TRANSPARENT,
        Stroke::new(eff_width, stroke_color),
    )));

    // The mockup's copper-orange frame, 2 px along the inner perimeter of the preview.
    painter.rect_stroke(rect, 0.0, Stroke::new(2.0, COPPER_ORANGE), egui::StrokeKind::Inside);
}

/// Synchronize `LineworkPanelState` into `ToolOptions` and the armed `VectorCurveState` on change.
pub fn sync_to_tool_options(app: &mut PhotocraftApp, state: &LineworkPanelState) {
    app.ui.tool_options.vector_curve_width = state.base_width;
    app.ui.tool_options.vector_curve_min_ratio = state.min_size_ratio;
    app.ui.tool_options.vector_curve_tension = state.tension as f32;
    app.ui.tool_options.vector_curve_tapered = state.auto_taper_ends || state.min_size_ratio < 0.999;
    app.ui.tool_options.vector_curve_closed = state.closed_loop;
    if let Some(vc) = app.ui.vector_curve.as_mut() {
        vc.tension = state.curve_type.tension(state.tension);
        vc.auto_taper_ends = state.auto_taper_ends;
        vc.snap_close = state.snap_close;
    }
}

/// The value format drawn inside a SAI bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BarFmt {
    /// `1.0` — the brush size in px.
    Px1,
    /// `0%` — a ratio stored as `0.0..=1.0`.
    Percent,
    /// `100` — the density as an integer `0..=100`.
    Int,
    /// `0.650` — the tension with three decimals.
    Tension,
}

impl BarFmt {
    fn show(self, v: f64) -> String {
        match self {
            Self::Px1 => format!("{v:.1}"),
            Self::Percent => format!("{:.0}%", (v * 100.0).round()),
            Self::Int => format!("{:.0}", v.round()),
            Self::Tension => format!("{v:.3}"),
        }
    }

    /// Parses the inline number entry (a `%` suffix is allowed for [`BarFmt::Percent`]).
    fn parse(self, s: &str) -> Option<f64> {
        let t = s.trim().trim_end_matches('%').trim();
        let v: f64 = t.parse().ok()?;
        match self {
            Self::Percent => Some((v / 100.0).clamp(0.0, 1.0)),
            _ => Some(v),
        }
    }
}

/// The static look/feel of one SAI parameter bar.
struct SaiBarSpec {
    /// Logarithmic value mapping (the Brush Size row).
    log: bool,
    /// The value format drawn inside the track.
    fmt: BarFmt,
    /// The `x0.1` fine-step badge (and the 0.1x drag) at the left edge.
    fine: bool,
    /// The checkerboard underlay inside the track (the Density row).
    checker: bool,
}

/// One SAI parameter bar (`h = 19 px`), like the mockup: the value and the `x0.1` fine-step badge
/// are drawn *inside* the track, there is no external drag-value box. Dragging sets the value
/// (logarithmic when `spec.log`); a drag with `Shift` held or started on the badge steps at 0.1x;
/// double-click or right-click opens the inline number entry; `spec.checker` underlays the track
/// with a checkerboard (the Density row). Returns `true` when the value changed.
fn sai_bar_slider(ui: &mut Ui, id_salt: &str, value: &mut f64, range: std::ops::RangeInclusive<f64>, spec: SaiBarSpec, accent: Color32) -> bool {
    let SaiBarSpec { log, fmt, fine, checker } = spec;
    let (min, max) = (*range.start(), *range.end());
    let id = ui.id().with(id_salt);
    let edit_key = id.with("inline_edit");
    let mut editing: Option<String> = ui.ctx().data(|d| d.get_temp(edit_key));
    let mut changed = false;

    if let Some(buf) = &mut editing {
        // The inline number entry: the bar becomes a one-line text field.
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().max(60.0), SAI_BAR_H), egui::Sense::hover());
        let editor = egui::TextEdit::singleline(buf).desired_width(rect.width() - 10.0).font(FontId::monospace(11.5));
        let resp = ui.put(rect, editor);
        resp.request_focus();
        let commit = resp.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter));
        let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
        if commit || cancel {
            if commit && let Some(v) = fmt.parse(buf) {
                let clamped = v.clamp(min, max);
                if (clamped - *value).abs() > f64::EPSILON {
                    *value = clamped;
                    changed = true;
                }
            }
            editing = None;
        }
    } else {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width().max(60.0), SAI_BAR_H), egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);

        let frac_of = |v: f64| -> f64 {
            if log && min > 0.0 && max > min {
                ((v.max(min).ln() - min.ln()) / (max.ln() - min.ln())).clamp(0.0, 1.0)
            } else {
                ((v - min) / (max - min)).clamp(0.0, 1.0)
            }
        };
        let val_of = |f: f64| -> f64 { if log && min > 0.0 && max > min { (min.ln() + f * (max.ln() - min.ln())).exp() } else { min + f * (max - min) } };

        // The track: a dark base, the checkerboard underlay (Density), the filled part.
        painter.rect_filled(rect, 3.0, Color32::from_rgb(28, 28, 32));
        if checker {
            paint_checkers(&painter, rect.shrink(2.0));
        }
        let fill_w = rect.width() as f64 * frac_of(*value);
        if fill_w >= 1.0 {
            painter.rect_filled(Rect::from_min_size(rect.min, vec2(fill_w as f32, rect.height())), 3.0, accent.gamma_multiply(0.55));
        }
        painter.rect_stroke(rect, 3.0, Stroke::new(1.0, Color32::from_rgb(58, 58, 64)), egui::StrokeKind::Inside);

        // The `x0.1` fine-step badge at the left edge (Brush Size, Tension).
        let mut badge = Rect::NOTHING;
        if fine {
            badge = Rect::from_min_size(rect.min + vec2(3.0, 3.0), vec2(30.0, SAI_BAR_H - 6.0));
            painter.rect_filled(badge, 2.0, Color32::from_rgb(52, 52, 58));
            painter.text(badge.center(), Align2::CENTER_CENTER, "x0.1", FontId::proportional(9.5), Color32::from_rgb(150, 150, 158));
        }
        painter.text(pos2(rect.max.x - 6.0, rect.center().y), Align2::RIGHT_CENTER, fmt.show(*value), FontId::proportional(11.0), Color32::WHITE);

        // Fine step: `Shift` while dragging, or a drag started on the badge.
        let fine_drag_key = id.with("fine_drag");
        if resp.drag_started() {
            let on_badge = resp.interact_pointer_pos().is_some_and(|p| badge.contains(p));
            let shift = ui.input(|i| i.modifiers.shift);
            ui.ctx().data_mut(|d| d.insert_temp(fine_drag_key, on_badge || shift));
        }
        if resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
            let (delta, _) = crate::scrub_wrap::drag_wrap(ui, &resp);
            let fine = ui.ctx().data(|d| d.get_temp::<bool>(fine_drag_key)).unwrap_or(false) || ui.input(|i| i.modifiers.shift);
            let speed = if fine { 0.1 } else { 1.0 };
            let delta_x = delta.x as f64 / rect.width().max(1.0) as f64 * speed;
            let v = val_of((frac_of(*value) + delta_x).clamp(0.0, 1.0));
            if (v - *value).abs() > f64::EPSILON {
                *value = v;
                changed = true;
            }
        } else if crate::scrub_wrap::is_holding(ui, &resp) {
            crate::scrub_wrap::drag_wrap(ui, &resp);
        }
        if resp.drag_stopped() {
            ui.ctx().data_mut(|d| d.remove::<bool>(fine_drag_key));
        }
        if resp.double_clicked() || resp.secondary_clicked() {
            editing = Some(fmt.show(*value));
        }
    }

    ui.ctx().data_mut(|d| {
        if let Some(e) = editing {
            d.insert_temp(edit_key, Some(e));
        } else {
            d.remove::<Option<String>>(edit_key);
        }
    });
    changed
}

/// One rubber SAI row: the label column (110 px, with an optional checkbox at its right edge) plus
/// the control filling the rest (`avail_w - 114`, i.e. 219 px on the mockup's 349 px dock). On
/// docks narrower than 215 px the control wraps onto its own line at full width. `checkbox`
/// reports whether its state flipped, so the caller can turn a click into "activate this mode".
fn sai_row<R>(ui: &mut Ui, mut checkbox: Option<&mut bool>, label: impl FnOnce(&mut Ui), content: impl FnOnce(&mut Ui) -> R) -> (Option<bool>, R) {
    let avail = ui.available_width();
    let mut flipped = None;
    let result = if avail < SAI_ROW_WRAP_W {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                label(ui);
                if let Some(on) = checkbox.take() {
                    let before = *on;
                    ui.add(egui::Checkbox::without_text(on));
                    flipped = Some(*on != before);
                }
            });
            content(ui)
        })
        .inner
    } else {
        ui.horizontal(|ui| {
            ui.allocate_ui(vec2(SAI_LABEL_COL_W, SAI_BAR_H + 6.0), |ui| {
                ui.horizontal(|ui| {
                    label(ui);
                    if let Some(on) = checkbox.take() {
                        // Push the checkbox to the right edge of the label column.
                        let used = ui.min_rect().width() + ui.spacing().item_spacing.x + 22.0;
                        ui.add_space((SAI_LABEL_COL_W - used).max(0.0));
                        let before = *on;
                        ui.add(egui::Checkbox::without_text(on));
                        flipped = Some(*on != before);
                    }
                });
            });
            content(ui)
        })
        .inner
    };
    (flipped, result)
}

/// The number of columns of the quick-sizes table at `avail_w`: 36 px cells, `2..=9` columns,
/// smoothly rebuilding `9 -> 8 -> ... -> 2` as the dock narrows (never a clipped column).
fn quick_grid_cols(avail_w: f32) -> usize {
    ((avail_w / SAI_QUICK_MIN_CELL_W).floor() as usize).clamp(2, 9)
}

/// Render the complete SAI Linework Studio panel.
pub fn panel(app: &mut PhotocraftApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let mut state = std::mem::take(&mut app.sai_panel);
    let initial_state = state.clone();

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(4.0);

        // 1. Живой предпросмотр S-образной кривой (шахматная область 53 px, медная рамка 2 px).
        ui.label(RichText::new("Предпросмотр кривой:").strong().color(t.text));
        draw_sai_preview(ui, state.base_width, state.min_size_ratio, state.base_density, state.min_density_ratio, t.accent);

        ui.add_space(6.0);
        ui.separator();

        // 2. Профиль сбега пера: метка + ComboBox + 5 компактных кнопок-иконок (золотая рамка
        //    у активной). В одну строку на ~330 px, переносится вниз на узких доках.
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Профиль сбега пера:").strong().color(t.text));
            let before_profile = state.tip_profile;
            egui::ComboBox::from_id_salt("sai_tip_profile_combo").width(92.0).selected_text(state.tip_profile.name()).show_ui(ui, |ui| {
                for id in TipProfileId::ALL {
                    ui.selectable_value(&mut state.tip_profile, id, id.name());
                }
            });
            if state.tip_profile != before_profile {
                let prev_r = state.min_size_ratio;
                state.min_size_ratio = state.tip_profile.ratio();
                sync_to_tool_options(app, &state);
                let to_min_ratio = app.ui.tool_options.vector_curve_min_ratio;
                log::info!(
                    "[Telemetry:SAI:TaperPreset] preset='{}', sai_panel.min_size_ratio: {prev_r} -> {}, \
                     tool_options.vector_curve_min_ratio={to_min_ratio}",
                    state.tip_profile.name(),
                    state.min_size_ratio
                );
            }
            for id in TipProfileId::ALL {
                let is_sel = state.tip_profile == id;
                let resp = ui.add_sized(vec2(17.5, 19.0), egui::Button::new(RichText::new(id.glyph()).size(11.0)).selected(is_sel));
                if is_sel {
                    ui.painter().rect_stroke(resp.rect, 2.0, Stroke::new(1.0, GOLD), egui::StrokeKind::Inside);
                }
                if resp.clicked() {
                    let prev_r = state.min_size_ratio;
                    state.tip_profile = id;
                    state.min_size_ratio = id.ratio();
                    sync_to_tool_options(app, &state);
                    let to_min_ratio = app.ui.tool_options.vector_curve_min_ratio;
                    log::info!(
                        "[Telemetry:SAI:TaperPreset] preset='{} {}', sai_panel.min_size_ratio: {prev_r} -> {}, \
                         tool_options.vector_curve_min_ratio={to_min_ratio}",
                        id.glyph(),
                        id.name(),
                        id.ratio()
                    );
                }
            }
        });

        ui.add_space(6.0);
        ui.separator();

        // 3. Интегрированные SAI-шкалы: число и множитель x0.1 — внутри самой полосы (19 px).
        let accent = t.accent;

        // Brush Size (0.5..=500 px, логарифмическая, x0.1). Кнопка-метка сворачивает/разворачивает
        // таблицу быстрых размеров.
        let mut bw = state.base_width as f64;
        let mut show_quick_sizes = state.show_quick_sizes;
        sai_row(
            ui,
            None,
            |ui| {
                let arrow = if show_quick_sizes { "▾" } else { "▸" };
                if ui.add(egui::Button::new(RichText::new(format!("Brush Size {arrow}")).size(12.0)).small()).clicked() {
                    show_quick_sizes = !show_quick_sizes;
                }
            },
            |ui| {
                sai_bar_slider(ui, "sai_bar_brush_size", &mut bw, 0.5..=500.0, SaiBarSpec { log: true, fmt: BarFmt::Px1, fine: true, checker: false }, accent);
            },
        );
        state.show_quick_sizes = show_quick_sizes;
        if (bw - f64::from(state.base_width)).abs() > f64::EPSILON {
            state.base_width = bw as f32;
            sync_to_tool_options(app, &state);
            let to_width = app.ui.tool_options.vector_curve_width;
            log::info!("[Telemetry:SAI:BaseWidthSlider] sai_panel.base_width={}, tool_options.vector_curve_width={to_width}", state.base_width);
        }

        // Min Size: чекбокс — индикатор цели нажима Width (клик активирует Width).
        let width_before = state.pressure_target == PressureTargetMode::Width;
        let mut min_ratio = state.min_size_ratio as f64;
        let mut width_on = width_before;
        sai_row(
            ui,
            Some(&mut width_on),
            |ui| {
                ui.label("Min Size");
            },
            |ui| {
                sai_bar_slider(
                    ui,
                    "sai_bar_min_size",
                    &mut min_ratio,
                    0.0..=1.0,
                    SaiBarSpec { log: false, fmt: BarFmt::Percent, fine: false, checker: false },
                    accent,
                );
            },
        );
        if (min_ratio - f64::from(state.min_size_ratio)).abs() > f64::EPSILON {
            state.min_size_ratio = min_ratio as f32;
        }
        if width_on != width_before {
            state.pressure_target = PressureTargetMode::Width;
            log::info!("[Telemetry:SAI:PressureTarget] changed to Width");
        }

        // Density: шахматная альфа-подложка внутри шкалы.
        let mut dens = state.base_density as f64 * 100.0;
        sai_row(
            ui,
            None,
            |ui| {
                ui.label("Density");
            },
            |ui| {
                sai_bar_slider(ui, "sai_bar_density", &mut dens, 0.0..=100.0, SaiBarSpec { log: false, fmt: BarFmt::Int, fine: false, checker: true }, accent);
            },
        );
        if (dens - state.base_density as f64 * 100.0).abs() > 1.0e-4 {
            state.base_density = (dens / 100.0) as f32;
        }

        // Min Density: чекбокс — индикатор цели нажима Density (клик активирует Density).
        let density_before = state.pressure_target == PressureTargetMode::Density;
        let mut min_dens_ratio = state.min_density_ratio as f64;
        let mut density_on = density_before;
        sai_row(
            ui,
            Some(&mut density_on),
            |ui| {
                ui.label("Min Density");
            },
            |ui| {
                sai_bar_slider(
                    ui,
                    "sai_bar_min_density",
                    &mut min_dens_ratio,
                    0.0..=1.0,
                    SaiBarSpec { log: false, fmt: BarFmt::Percent, fine: false, checker: false },
                    accent,
                );
            },
        );
        if (min_dens_ratio - f64::from(state.min_density_ratio)).abs() > f64::EPSILON {
            state.min_density_ratio = min_dens_ratio as f32;
        }
        if density_on != density_before {
            state.pressure_target = PressureTargetMode::Density;
            log::info!("[Telemetry:SAI:PressureTarget] changed to Density");
        }

        // Тип кривой: ComboBox во всю оставшуюся ширину строки.
        sai_row(
            ui,
            None,
            |ui| {
                ui.label("Тип кривой:");
            },
            |ui| {
                egui::ComboBox::from_id_salt("sai_curve_combo")
                    .width(160.0)
                    .selected_text(match state.curve_type {
                        SaiCurveType::Normal => "Normal (Catmull-Rom T=0.65)",
                        SaiCurveType::Bezier => "Bezier",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut state.curve_type, SaiCurveType::Normal, "Normal (Catmull-Rom T=0.65)");
                        ui.selectable_value(&mut state.curve_type, SaiCurveType::Bezier, "Bezier");
                    });
            },
        );

        // Натяжение T (0.0..=1.0, x0.1).
        if state.curve_type == SaiCurveType::Normal {
            sai_row(
                ui,
                None,
                |ui| {
                    ui.label("Натяжение T:");
                },
                |ui| {
                    let mut tension = state.tension;
                    sai_bar_slider(
                        ui,
                        "sai_bar_tension",
                        &mut tension,
                        0.0..=1.0,
                        SaiBarSpec { log: false, fmt: BarFmt::Tension, fine: true, checker: false },
                        accent,
                    );
                    state.tension = tension;
                },
            );
        }

        ui.add_space(6.0);

        // Компактный 1-строчный переключатель динамики нажима, синхронизированный с чекбоксами
        // у Min Size (Width) и Min Density (Density).
        ui.horizontal_wrapped(|ui| {
            ui.label("Нажим:");
            if ui.selectable_label(state.pressure_target == PressureTargetMode::Width, "Ширина").clicked() && state.pressure_target != PressureTargetMode::Width
            {
                state.pressure_target = PressureTargetMode::Width;
                log::info!("[Telemetry:SAI:PressureTarget] changed to Width");
            }
            if ui.selectable_label(state.pressure_target == PressureTargetMode::Density, "Плотность").clicked()
                && state.pressure_target != PressureTargetMode::Density
            {
                state.pressure_target = PressureTargetMode::Density;
                log::info!("[Telemetry:SAI:PressureTarget] changed to Density");
            }
        });

        ui.add_space(2.0);

        // 3 флага с переносом текста на узких доках.
        for (on, text) in [
            (&mut state.auto_taper_ends, "Auto Taper Ends (Авто-заострение концов)"),
            (&mut state.closed_loop, "Closed Loop (Замкнутый контур)"),
            (&mut state.snap_close, "Snap Close (Замыкание впритык 1.5 px)"),
        ] {
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::Checkbox::without_text(on));
                ui.label(RichText::new(text).color(t.text));
            });
        }

        ui.add_space(6.0);
        ui.separator();

        // 4. Динамическая плотная таблица "Быстрые размеры:" (0.7..=500 px). Свой рендер через
        //    painter_at + interact вместо egui::Grid: никаких скрытых отступов, обрезанных
        //    столбцов и пустой серой зоны снизу; столбцы плавно перестраиваются 9 -> 2.
        if state.show_quick_sizes {
            ui.label(RichText::new("Быстрые размеры:").strong().color(t.text));
            let avail_w = ui.available_width();
            let cols = quick_grid_cols(avail_w);
            let cell_w = (avail_w / cols as f32).floor().max(1.0);
            let rows = SAI_QUICK_SIZES.len().div_ceil(cols);
            let grid_w = cell_w * cols as f32;
            let (grid_rect, _) = ui.allocate_exact_size(vec2(grid_w, rows as f32 * SAI_QUICK_CELL_H), egui::Sense::hover());
            let painter = ui.painter_at(grid_rect);
            painter.rect_filled(grid_rect, 2.0, Color32::from_rgb(28, 28, 32));
            for c in 1..cols {
                let x = grid_rect.min.x + c as f32 * cell_w;
                painter.line_segment([pos2(x, grid_rect.min.y), pos2(x, grid_rect.max.y)], Stroke::new(1.0, Color32::from_rgb(44, 44, 50)));
            }
            for r in 1..rows {
                let y = grid_rect.min.y + r as f32 * SAI_QUICK_CELL_H;
                painter.line_segment([pos2(grid_rect.min.x, y), pos2(grid_rect.max.x, y)], Stroke::new(1.0, Color32::from_rgb(44, 44, 50)));
            }
            for (idx, &sz) in SAI_QUICK_SIZES.iter().enumerate() {
                let (r, c) = (idx / cols, idx % cols);
                let cell = Rect::from_min_size(
                    pos2(grid_rect.min.x + c as f32 * cell_w, grid_rect.min.y + r as f32 * SAI_QUICK_CELL_H),
                    vec2(cell_w, SAI_QUICK_CELL_H),
                );
                let resp = ui.interact(cell, ui.id().with(("sai_quick_cell", idx)), egui::Sense::click());
                let is_sel = (state.base_width - sz).abs() < 0.05;
                if resp.hovered() {
                    painter.rect_filled(cell, 0.0, Color32::from_rgba_unmultiplied(255, 255, 255, 12));
                }
                if is_sel {
                    painter.rect_stroke(cell.shrink(1.0), 2.0, Stroke::new(1.0, GOLD), egui::StrokeKind::Inside);
                }
                // Пропорциональный белый кружок сверху + подпись размера снизу.
                let ratio = (sz / 500.0).sqrt().clamp(0.05, 1.0);
                painter.circle_filled(pos2(cell.center().x, cell.min.y + 11.0), 2.0 + 7.0 * ratio, Color32::WHITE);
                let text = if sz < 10.0 { format!("{sz:.1}") } else { format!("{sz:.0}") };
                painter.text(
                    pos2(cell.center().x, cell.max.y - 8.0),
                    Align2::CENTER_CENTER,
                    &text,
                    FontId::proportional(11.0),
                    Color32::from_rgb(190, 190, 196),
                );
                if resp.clicked() {
                    let prev_w = state.base_width;
                    state.base_width = sz;
                    sync_to_tool_options(app, &state);
                    let to_width = app.ui.tool_options.vector_curve_width;
                    log::info!(
                        "[Telemetry:SAI:GridClick] clicked_sz={sz}, sai_panel.base_width: {prev_w} -> {sz}, \
                         tool_options.vector_curve_width={to_width} (synced={})",
                        (sz - to_width).abs() < f32::EPSILON
                    );
                }
            }
        }

        ui.add_space(10.0);
        ui.separator();

        // 5. Нижние кнопки действий — все во всю ширину (28 / 26 / 27 px, шаг 4 px).
        let full_w = ui.available_width();
        let new_layer_btn =
            ui.add_sized([full_w, 28.0], egui::Button::new(RichText::new("＋ Создать новый Linework слой").strong().color(Color32::WHITE)).fill(t.accent));
        if new_layer_btn.clicked() {
            let _ = app.run("layer.newLinework", json!({ "width": state.base_width }));
            app.ui.tool = Tool::VectorCurve;
        }

        ui.add_space(4.0);
        if ui.add_sized([full_w, 26.0], egui::Button::new("⚡ Запечь баббл в Linework")).clicked()
            && app.run("shape.balloon.convertToLinework", json!({})).is_ok()
        {
            app.ui.tool = Tool::VectorCurve;
        }

        ui.add_space(4.0);
        if ui.add_sized([full_w, 27.0], egui::Button::new("🔄 Сбросить параметры пера")).clicked() {
            state = LineworkPanelState::default();
        }
    });

    if state != initial_state {
        sync_to_tool_options(app, &state);
    }

    app.sai_panel = state;
}

// Обратная совместимость с вызовом из panels.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineworkAction {
    NewLayer,
    ConvertBalloon,
}

pub fn render_linework_dock_panel(ui: &mut Ui, state: &mut LineworkPanelState) -> Option<LineworkAction> {
    let mut act = None;
    ui.vertical(|ui| {
        let t = Tokens::get(ui.ctx());
        draw_sai_preview(ui, state.base_width, state.min_size_ratio, state.base_density, state.min_density_ratio, t.accent);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Размер:");
            ui.add(Slider::new(&mut state.base_width, 0.7..=500.0).logarithmic(true).suffix(" px"));
        });
        ui.horizontal(|ui| {
            if ui.button("＋ Новый Linework").clicked() {
                act = Some(LineworkAction::NewLayer);
            }
            if ui.button("⚡ Convert Balloon").clicked() {
                act = Some(LineworkAction::ConvertBalloon);
            }
        });
    });
    act
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> (PhotocraftApp, egui::Context) {
        let ctx = egui::Context::default();
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width": 200, "height": 200, "depth": 8})).unwrap();
        app.sync_views();
        (app, ctx)
    }

    #[test]
    fn window_menu_toggles_linework_dock_tab() {
        let (mut app, ctx) = test_app();
        for pro in [false, true] {
            assert_eq!(crate::dock::Group::Layers.tabs(pro).get(LINEWORK_LAYERS_TAB), Some(&"Linework"), "pro {pro}");
        }
        assert!(crate::menus::is_live(PANEL), "{PANEL}");
        crate::menus::invoke(&mut app, &ctx, PANEL, Value::Null).unwrap();
        assert!(app.ui.panels.layers, "the Layers group shows");
        assert_eq!(app.ui.dock_tabs.layers, LINEWORK_LAYERS_TAB, "the Linework tab is selected");
        assert_eq!(checked(&app, PANEL), Some(true));
        crate::menus::invoke(&mut app, &ctx, PANEL, json!({"show": false})).unwrap();
        assert_eq!(checked(&app, PANEL), Some(false));
    }

    #[test]
    fn sai_quick_sizes_are_sorted_and_valid() {
        assert_eq!(SAI_QUICK_SIZES.len(), 38);
        assert!((SAI_QUICK_SIZES[0] - 0.7).abs() < 1e-4);
        assert!((SAI_QUICK_SIZES[37] - 500.0).abs() < 1e-4);
        for i in 1..SAI_QUICK_SIZES.len() {
            assert!(SAI_QUICK_SIZES[i] > SAI_QUICK_SIZES[i - 1]);
        }
    }

    #[test]
    fn sai_grid_and_tip_profile_sync_to_tool_options() {
        let (mut app, _ctx) = test_app();
        // ToolOptions and LineworkPanelState start aligned.
        assert!((app.ui.tool_options.vector_curve_width - 8.0).abs() < 1e-4);
        assert!((app.ui.tool_options.vector_curve_min_ratio - 0.0).abs() < 1e-4);

        let mut panel = LineworkPanelState {
            base_width: 350.0,
            tip_profile: TipProfileId::SemiFlat,
            min_size_ratio: 0.50,
            curve_type: SaiCurveType::Bezier,
            tension: 0.4,
            base_density: 0.75,
            min_density_ratio: 0.2,
            ..LineworkPanelState::default()
        };
        sync_to_tool_options(&mut app, &panel);
        assert!((app.ui.tool_options.vector_curve_width - 350.0).abs() < 1e-4);
        assert!((app.ui.tool_options.vector_curve_min_ratio - 0.50).abs() < 1e-4);
        assert!(app.ui.tool_options.vector_curve_tapered);

        panel.base_width = 2.0;
        panel.tip_profile = TipProfileId::Calligraphic;
        panel.min_size_ratio = 0.05;
        sync_to_tool_options(&mut app, &panel);
        assert!((app.ui.tool_options.vector_curve_width - 2.0).abs() < 1e-4);
        assert!((app.ui.tool_options.vector_curve_min_ratio - 0.05).abs() < 1e-4);

        // Verify engine builder applies curve_type, base_density, and min_density_ratio.
        let out = photocraft_engine::vector_cmds::build_vector_curve_layer(&json!({
            "points": [{"x": 10.0, "y": 10.0, "pressure": 1.0}, {"x": 80.0, "y": 40.0, "pressure": 0.5}],
            "width": 350.0,
            "minRatio": 0.50,
            "curveType": "bezier",
            "baseDensity": 0.75,
            "minDensityRatio": 0.20,
        }))
        .unwrap();
        assert_eq!(out.live_stroke.curve_type, SaiCurveType::Bezier);
        assert!((out.live_stroke.base_width - 350.0).abs() < 1e-4);
        assert!((out.live_stroke.base_density - 0.75).abs() < 1e-4);
        assert!((out.live_stroke.min_density_ratio - 0.20).abs() < 1e-4);
    }

    #[test]
    fn quick_grid_columns_rebuild_rubber_2_to_9() {
        // Narrow docks keep at least 2 columns, wide docks stop at 9; in between the columns
        // follow the 36 px cell width (TZ v5.0, блок 4).
        assert_eq!(quick_grid_cols(20.0), 2);
        assert_eq!(quick_grid_cols(100.0), 2);
        assert_eq!(quick_grid_cols(150.0), 4);
        assert_eq!(quick_grid_cols(330.0), 9);
        assert_eq!(quick_grid_cols(2000.0), 9);
    }

    #[test]
    fn sai_bars_format_values_like_the_mockup() {
        assert_eq!(BarFmt::Px1.show(1.0), "1.0");
        assert_eq!(BarFmt::Percent.show(0.0), "0%");
        assert_eq!(BarFmt::Percent.show(1.0), "100%");
        assert_eq!(BarFmt::Int.show(100.0), "100");
        assert_eq!(BarFmt::Tension.show(0.65), "0.650");
        assert_eq!(BarFmt::Percent.parse("65%"), Some(0.65));
        assert_eq!(BarFmt::Px1.parse("12.5"), Some(12.5));
        assert_eq!(BarFmt::Px1.parse("not a number"), None);
    }
}
