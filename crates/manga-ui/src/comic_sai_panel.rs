//! Dock panel with the PaintTool SAI Ver.2 Vector Linework parameters.
//!
//! Rubber layout per the mockup (`TZ v5.0`, base dock width 349 px, usable content ~330 px):
//! a live S-curve preview on a checkerboard with a copper 2 px frame, 5 tip-profile presets,
//! integrated SAI parameter bars (the number and the `x0.1` fine-step badge are drawn *inside*
//! the 19 px track), the compact Width/Density pressure toggle synced with the Min Size / Min
//! Density checkboxes, three wrapping flags, a dense quick-sizes table rebuilt without
//! `egui::Grid` (36 px cells, 2..=9 columns, never a clipped column), and full-width action
//! buttons. 38 quick sizes, pressure dynamics (Width / Density), curve type
//! (Normal Catmull-Rom / Bezier), tension, auto-taper toggle, and 1-click layer creation.
//!
//! This crate is decoupled from `PhotocraftApp` / `photocraft_engine`: all host state arrives via
//! [`LineworkHostContext`] and every side effect is returned in [`LineworkPanelOutput`].

use std::ops::RangeInclusive;

use egui::{Align2, Color32, FontId, Painter, Rect, RichText, Slider, Stroke, Ui, pos2, vec2};
use photocraft_vector::comic_sai_curve::{SAI_DEFAULT_TENSION, SaiCurveType};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::OutCommand;
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

/// The index of the Linework tab inside the Layers dock group.
pub const LINEWORK_LAYERS_TAB: usize = 3;

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
            Self::SemiFlat => "▪",
            Self::Uniform => "■",
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
    /// Whether the quick-sizes table is expanded (the `▼` on the Brush Size row).
    #[serde(default = "default_show_quick_sizes")]
    pub show_quick_sizes: bool,
    /// Optional custom or built-in vector brush preset applied to Linework strokes.
    #[serde(default)]
    pub brush_preset_id: Option<String>,
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
            brush_preset_id: None,
        }
    }
}

/// Parameters of the armed vector curve that the host can push into the panel on open.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SaiCurveSync {
    pub tension: f64,
    pub auto_taper_ends: bool,
    pub snap_close: bool,
}

/// Everything the panel reads from its host, without depending on the host's concrete type.
#[derive(Clone, Copy, Debug)]
pub struct LineworkHostContext<'a> {
    /// `(id, name)` pairs of the vector brush presets available for the Linework strokes.
    pub vector_brush_presets: &'a [(String, String)],
    /// The stroke selected in Edit or Weight, if any; it owns the displayed brush size.
    pub selected_stroke_width: Option<f32>,
    /// The active vector curve's parameters, used to seed the panel's curve controls.
    pub active_curve_sync: Option<SaiCurveSync>,
}

/// Everything the panel asks its host to do after a frame.
#[derive(Default)]
pub struct LineworkPanelOutput {
    /// Whether any panel field changed during the frame.
    pub changed: bool,
    /// Resize the picked stroke (in Edit or Weight) to this width, if the size changed.
    pub apply_stroke_width: Option<f32>,
    /// Activate the vector curve tool after the layer / conversion command runs.
    pub switch_to_vector_curve_tool: bool,
    /// Commands for the host to dispatch through the engine.
    pub commands: Vec<OutCommand>,
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
                let cell = Rect::from_min_max(pos2(x0, y0), pos2(x1, y1));
                painter.rect_filled(cell, 0.0, Color32::from_rgb(34, 34, 40));
            }
        }
    }
}

fn build_adaptive_slider<'a, Num: egui::emath::Numeric>(val: &'a mut Num, range: RangeInclusive<Num>, suffix: &str, logarithmic: bool) -> Slider<'a> {
    let mut s = Slider::new(val, range);
    if !suffix.is_empty() {
        s = s.suffix(suffix);
    }
    if logarithmic {
        s = s.logarithmic(true);
    }
    s
}

/// An adaptive label + slider: stacked on narrow docks, inline on wide ones.
pub fn adaptive_slider<Num: egui::emath::Numeric>(
    ui: &mut Ui,
    label: &str,
    value: &mut Num,
    range: RangeInclusive<Num>,
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

/// Draws the live S-curve preview on a checkerboard with the mockup's copper frame.
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
/// are drawn *inside* the track. Dragging sets the value (logarithmic when `spec.log`); a drag with
/// `Shift` held or started on the badge steps at 0.1x; double-click or right-click opens the inline
/// number entry; `spec.checker` underlays the track with a checkerboard. Returns `true` on change.
fn sai_bar_slider(ui: &mut Ui, id_salt: &str, value: &mut f64, range: RangeInclusive<f64>, spec: SaiBarSpec, accent: Color32) -> bool {
    let SaiBarSpec { log, fmt, fine, checker } = spec;
    let (min, max) = (*range.start(), *range.end());
    let id = ui.scope_id().with(id_salt);
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
            let fill = Rect::from_min_size(rect.min, vec2(fill_w as f32, rect.height()));
            painter.rect_filled(fill, 3.0, accent.gamma_multiply(0.55));
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

        // Click anywhere on the track outside the `x0.1` badge jumps directly to that fraction.
        let fine_drag_key = id.with("fine_drag");
        let precise_key = id.with("precise_frac");
        if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
            && !badge.contains(pos)
        {
            let frac = f64::from((pos.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
            ui.ctx().data_mut(|d| d.insert_temp(precise_key, frac));
            let v = val_of(frac).clamp(min, max);
            if (v - *value).abs() > f64::EPSILON {
                *value = v;
                changed = true;
            }
        }

        // Fine step: `Shift` while dragging, or a drag started on the `x0.1` badge.
        if resp.drag_started() {
            let on_badge = resp.interact_pointer_pos().is_some_and(|p| badge.contains(p));
            let shift = ui.input(|i| i.modifiers.shift);
            let initial_frac = frac_of(*value);
            ui.ctx().data_mut(|d| {
                d.insert_temp(fine_drag_key, on_badge || shift);
                d.insert_temp(precise_key, initial_frac);
            });
        }
        if resp.dragged() {
            let started_fine = ui.ctx().data(|d| d.get_temp::<bool>(fine_drag_key)).unwrap_or(false);
            let fine_mode = started_fine || ui.input(|i| i.modifiers.shift);
            if fine_mode {
                ui.ctx().set_cursor_icon(egui::CursorIcon::None);
                let (delta, _) = crate::scrub_wrap::drag_wrap(ui, &resp);
                let prev_frac = ui.ctx().data(|d| d.get_temp::<f64>(precise_key)).unwrap_or_else(|| frac_of(*value));
                let next_frac = (prev_frac + f64::from(delta.x) / f64::from(rect.width().max(1.0)) * 0.1).clamp(0.0, 1.0);
                ui.ctx().data_mut(|d| d.insert_temp(precise_key, next_frac));
                let v = val_of(next_frac).clamp(min, max);
                if (v - *value).abs() > f64::EPSILON {
                    *value = v;
                    changed = true;
                }
            } else if let Some(pos) = resp.interact_pointer_pos() {
                if crate::scrub_wrap::is_holding(ui, &resp) {
                    crate::scrub_wrap::release(ui, resp.id);
                }
                let frac = f64::from((pos.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
                ui.ctx().data_mut(|d| d.insert_temp(precise_key, frac));
                let v = val_of(frac).clamp(min, max);
                if (v - *value).abs() > f64::EPSILON {
                    *value = v;
                    changed = true;
                }
            }
        } else if crate::scrub_wrap::is_holding(ui, &resp) {
            crate::scrub_wrap::release(ui, resp.id);
        }
        if resp.drag_stopped() {
            ui.ctx().data_mut(|d| {
                d.remove::<bool>(fine_drag_key);
                d.remove::<f64>(precise_key);
            });
            crate::scrub_wrap::release(ui, resp.id);
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
/// the control filling the rest. On docks narrower than 215 px the control wraps to its own line.
/// `flipped` reports whether the checkbox state changed.
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

/// The number of columns of the quick-sizes table at `avail_w`: 36 px cells, `2..=9` columns.
fn quick_grid_cols(avail_w: f32) -> usize {
    ((avail_w / SAI_QUICK_MIN_CELL_W).floor() as usize).clamp(2, 9)
}

fn curve_type_label(curve_type: &SaiCurveType) -> &'static str {
    match curve_type {
        SaiCurveType::Normal => "Normal (Catmull-Rom T=0.65)",
        SaiCurveType::Bezier => "Bezier",
    }
}

/// The tip-profile combo plus its five compact icon buttons. Returns `true` on any change.
fn draw_tip_profile_row(ui: &mut Ui, tip_profile: &mut TipProfileId, min_size_ratio: &mut f32) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Профиль сбега пера:").strong().color(t.text));
        let before = *tip_profile;
        craft_ui::ComboBox::from_id_salt("sai_tip_profile_combo").width(92.0).selected_text(before.name()).show_ui(ui, |ui| {
            for id in TipProfileId::ALL {
                ui.selectable_value(&mut *tip_profile, id, id.name());
            }
        });
        if *tip_profile != before {
            *min_size_ratio = tip_profile.ratio();
        }
        for id in TipProfileId::ALL {
            let is_sel = *tip_profile == id;
            let btn = egui::Button::new(RichText::new(id.glyph()).size(11.0)).selected(is_sel);
            let resp = ui.add_sized(vec2(17.5, 19.0), btn);
            if is_sel {
                ui.painter().rect_stroke(resp.rect, 2.0, Stroke::new(1.0, GOLD), egui::StrokeKind::Inside);
            }
            if resp.clicked() {
                *tip_profile = id;
                *min_size_ratio = id.ratio();
            }
        }
    });
}

/// The four integrated SAI parameter bars, plus the curve-type combo and the tension bar.
fn draw_sai_sliders(ui: &mut Ui, state: &mut LineworkPanelState, output: &mut LineworkPanelOutput, accent: Color32) {
    // Brush Size (0.5..=500 px, logarithmic, x0.1). The label button toggles the quick-sizes table.
    let mut bw = state.base_width as f64;
    let mut show_quick_sizes = state.show_quick_sizes;
    sai_row(
        ui,
        None,
        |ui| {
            let arrow = if show_quick_sizes { "▼" } else { "▶" };
            let btn = egui::Button::new(RichText::new(format!("Brush Size {arrow}")).size(12.0)).small();
            if ui.add(btn).clicked() {
                show_quick_sizes = !show_quick_sizes;
            }
        },
        |ui| {
            let spec = SaiBarSpec { log: true, fmt: BarFmt::Px1, fine: true, checker: false };
            sai_bar_slider(ui, "sai_bar_brush_size", &mut bw, 0.5..=500.0, spec, accent);
        },
    );
    state.show_quick_sizes = show_quick_sizes;
    if (bw - f64::from(state.base_width)).abs() > f64::EPSILON {
        state.base_width = bw as f32;
        // In Edit or Weight this resizes the picked stroke, not just the next one.
        output.apply_stroke_width = Some(state.base_width);
    }

    // Min Size: checkbox is the Width target indicator (clicking it activates Width).
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
            let spec = SaiBarSpec { log: false, fmt: BarFmt::Percent, fine: false, checker: false };
            sai_bar_slider(ui, "sai_bar_min_size", &mut min_ratio, 0.0..=1.0, spec, accent);
        },
    );
    if (min_ratio - f64::from(state.min_size_ratio)).abs() > f64::EPSILON {
        state.min_size_ratio = min_ratio as f32;
    }
    if width_on != width_before {
        state.pressure_target = PressureTargetMode::Width;
    }

    // Density: checkerboard alpha underlay inside the track.
    let mut dens = state.base_density as f64 * 100.0;
    sai_row(
        ui,
        None,
        |ui| {
            ui.label("Density");
        },
        |ui| {
            let spec = SaiBarSpec { log: false, fmt: BarFmt::Int, fine: false, checker: true };
            sai_bar_slider(ui, "sai_bar_density", &mut dens, 0.0..=100.0, spec, accent);
        },
    );
    if (dens - state.base_density as f64 * 100.0).abs() > 1.0e-4 {
        state.base_density = (dens / 100.0) as f32;
    }

    // Min Density: checkbox is the Density target indicator (clicking it activates Density).
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
            let spec = SaiBarSpec { log: false, fmt: BarFmt::Percent, fine: false, checker: false };
            sai_bar_slider(ui, "sai_bar_min_density", &mut min_dens_ratio, 0.0..=1.0, spec, accent);
        },
    );
    if (min_dens_ratio - f64::from(state.min_density_ratio)).abs() > f64::EPSILON {
        state.min_density_ratio = min_dens_ratio as f32;
    }
    if density_on != density_before {
        state.pressure_target = PressureTargetMode::Density;
    }

    // Curve type: a craft_ui combo filling the remaining width of the row.
    sai_row(
        ui,
        None,
        |ui| {
            ui.label("Тип кривой:");
        },
        |ui| {
            craft_ui::ComboBox::from_id_salt("sai_curve_combo").width(160.0).selected_text(curve_type_label(&state.curve_type)).show_ui(ui, |ui| {
                ui.selectable_value(&mut state.curve_type, SaiCurveType::Normal, "Normal (Catmull-Rom T=0.65)");
                ui.selectable_value(&mut state.curve_type, SaiCurveType::Bezier, "Bezier");
            });
        },
    );

    // Tension T (0.0..=1.0, x0.1) only applies to the Normal curve type.
    if state.curve_type == SaiCurveType::Normal {
        sai_row(
            ui,
            None,
            |ui| {
                ui.label("Натяжение T:");
            },
            |ui| {
                let mut tension = state.tension;
                let spec = SaiBarSpec { log: false, fmt: BarFmt::Tension, fine: true, checker: false };
                sai_bar_slider(ui, "sai_bar_tension", &mut tension, 0.0..=1.0, spec, accent);
                state.tension = tension;
            },
        );
    }
}

/// The compact one-line pressure-target toggle and the three wrapping flags.
fn draw_pressure_and_flags(ui: &mut Ui, state: &mut LineworkPanelState, text: Color32) {
    ui.horizontal_wrapped(|ui| {
        ui.label("Нажим:");
        let width = ui.selectable_label(state.pressure_target == PressureTargetMode::Width, "Ширина");
        if width.clicked() && state.pressure_target != PressureTargetMode::Width {
            state.pressure_target = PressureTargetMode::Width;
        }
        let density = ui.selectable_label(state.pressure_target == PressureTargetMode::Density, "Плотность");
        if density.clicked() && state.pressure_target != PressureTargetMode::Density {
            state.pressure_target = PressureTargetMode::Density;
        }
    });

    ui.add_space(2.0);
    for (on, label) in [
        (&mut state.auto_taper_ends, "Auto Taper Ends (Авто-заострение концов)"),
        (&mut state.closed_loop, "Closed Loop (Замкнутый контур)"),
        (&mut state.snap_close, "Snap Close (Замыкание впритык 1.5 px)"),
    ] {
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::Checkbox::without_text(on));
            ui.label(RichText::new(label).color(text));
        });
    }
}

/// The dense, dynamically re-flowing quick-sizes table (0.7..=500 px).
fn draw_quick_sizes_grid(ui: &mut Ui, state: &mut LineworkPanelState, output: &mut LineworkPanelOutput) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new("Быстрые размеры:").strong().color(t.text));
    let avail_w = ui.available_width();
    let cols = quick_grid_cols(avail_w);
    let cell_w = (avail_w / cols as f32).floor().max(1.0);
    let rows = SAI_QUICK_SIZES.len().div_ceil(cols);
    let grid_w = cell_w * cols as f32;
    let size = vec2(grid_w, rows as f32 * SAI_QUICK_CELL_H);
    let (grid_rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(grid_rect);
    painter.rect_filled(grid_rect, 2.0, Color32::from_rgb(28, 28, 32));
    let grid_line = Stroke::new(1.0, Color32::from_rgb(44, 44, 50));
    for c in 1..cols {
        let x = grid_rect.min.x + c as f32 * cell_w;
        painter.line_segment([pos2(x, grid_rect.min.y), pos2(x, grid_rect.max.y)], grid_line);
    }
    for r in 1..rows {
        let y = grid_rect.min.y + r as f32 * SAI_QUICK_CELL_H;
        painter.line_segment([pos2(grid_rect.min.x, y), pos2(grid_rect.max.x, y)], grid_line);
    }
    for (idx, &sz) in SAI_QUICK_SIZES.iter().enumerate() {
        let (r, c) = (idx / cols, idx % cols);
        let cell =
            Rect::from_min_size(pos2(grid_rect.min.x + c as f32 * cell_w, grid_rect.min.y + r as f32 * SAI_QUICK_CELL_H), vec2(cell_w, SAI_QUICK_CELL_H));
        let cell_id = ui.scope_id().with(("sai_quick_cell", idx));
        let resp = ui.interact(cell, cell_id, egui::Sense::click());
        let is_sel = (state.base_width - sz).abs() < 0.05;
        if resp.hovered() {
            painter.rect_filled(cell, 0.0, Color32::from_rgba_unmultiplied(255, 255, 255, 12));
        }
        if is_sel {
            painter.rect_stroke(cell.shrink(1.0), 2.0, Stroke::new(1.0, GOLD), egui::StrokeKind::Inside);
        }
        let ratio = (sz / 500.0).sqrt().clamp(0.05, 1.0);
        painter.circle_filled(pos2(cell.center().x, cell.min.y + 11.0), 2.0 + 7.0 * ratio, Color32::WHITE);
        let text = if sz < 10.0 { format!("{sz:.1}") } else { format!("{sz:.0}") };
        let label_pos = pos2(cell.center().x, cell.max.y - 8.0);
        painter.text(label_pos, Align2::CENTER_CENTER, &text, FontId::proportional(11.0), Color32::from_rgb(190, 190, 196));
        if resp.clicked() {
            state.base_width = sz;
            // In Edit or Weight a preset resizes the picked stroke, not just the next one.
            output.apply_stroke_width = Some(sz);
        }
    }
}

/// The vector-brush preset combo and the import button, emitting an import command for the host.
fn draw_vector_brush_row(ui: &mut Ui, id_salt: &str, presets: &[(String, String)], selected_id: &mut Option<String>, commands: &mut Vec<OutCommand>) {
    let current_label = selected_id
        .as_deref()
        .and_then(|id| presets.iter().find(|(pid, _)| pid == id).map(|(_, n)| n.clone()))
        .unwrap_or_else(|| "Default (Style Engine)".to_string());

    ui.horizontal_wrapped(|ui| {
        ui.label("Brush preset:");
        craft_ui::ComboBox::from_id_salt(id_salt).width(142.0).selected_text(current_label).show_ui(ui, |ui| {
            if ui.selectable_label(selected_id.is_none(), "Default (Style Engine)").clicked() {
                *selected_id = None;
            }
            for (pid, pname) in presets {
                let is_sel = selected_id.as_deref() == Some(pid.as_str());
                if ui.selectable_label(is_sel, pname).clicked() {
                    *selected_id = Some(pid.clone());
                }
            }
        });
        let import = ui.button("+ Import (.json/.png)").on_hover_text("Import a VectorBrushPreset JSON or grayscale PNG ribbon texture");
        if import.clicked()
            && let Some(path) = rfd::FileDialog::new().add_filter("Vector Brush", &["json", "png"]).pick_file()
        {
            let path_str = path.to_string_lossy().into_owned();
            commands.push(OutCommand::new("shape.vectorBrush.import", json!({ "path": path_str })));
        }
    });
}

/// The full-width action buttons at the bottom of the panel.
fn draw_action_buttons(ui: &mut Ui, state: &mut LineworkPanelState, output: &mut LineworkPanelOutput, accent: Color32) {
    let full_w = ui.available_width();
    let label = RichText::new("+ Создать новый Linework слой").strong().color(Color32::WHITE);
    let new_layer = ui.add_sized([full_w, 28.0], egui::Button::new(label).fill(accent));
    if new_layer.clicked() {
        output.commands.push(OutCommand::new("layer.newLinework", json!({ "width": state.base_width })));
        output.switch_to_vector_curve_tool = true;
    }

    ui.add_space(4.0);
    if ui.add_sized([full_w, 26.0], egui::Button::new("⚡ Запечь баббл в Linework")).clicked() {
        output.commands.push(OutCommand::new("shape.balloon.convertToLinework", json!({})));
        output.switch_to_vector_curve_tool = true;
    }

    ui.add_space(4.0);
    if ui.add_sized([full_w, 27.0], egui::Button::new("🔄 Сбросить параметры пера")).clicked() {
        *state = LineworkPanelState::default();
    }
}

/// Render the complete SAI Linework Studio panel from host-supplied context.
pub fn panel(ui: &mut Ui, state: &mut LineworkPanelState, host: &LineworkHostContext<'_>) -> LineworkPanelOutput {
    // Headings here are labels, not selectable prose; TextEdit keeps its own selection.
    ui.style_mut().interaction.selectable_labels = false;
    let t = Tokens::get(ui.ctx());
    let mut output = LineworkPanelOutput::default();

    // A stroke picked in Edit or Weight owns the size: the bar, the preview and the quick-size
    // ring all read back that stroke instead of the width armed for the next one.
    if let Some(w) = host.selected_stroke_width {
        state.base_width = w;
    }
    if let Some(sync) = host.active_curve_sync {
        state.tension = sync.tension;
        state.auto_taper_ends = sync.auto_taper_ends;
        state.snap_close = sync.snap_close;
    }
    let initial_state = state.clone();

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(4.0);

        // 1. Живой предпросмотр S-образной кривой (шахматная область 53 px, медная рамка 2 px).
        ui.label(RichText::new("Предпросмотр кривой:").strong().color(t.text));
        draw_sai_preview(ui, state.base_width, state.min_size_ratio, state.base_density, state.min_density_ratio, t.accent);

        ui.add_space(6.0);
        ui.separator();

        // 2. Профиль сбега пера: ComboBox + 5 компактных кнопок-иконок (золотая рамка у активной).
        draw_tip_profile_row(ui, &mut state.tip_profile, &mut state.min_size_ratio);

        ui.add_space(6.0);
        ui.separator();

        // 3. Интегрированные SAI-шкалы: число и множитель x0.1 — внутри самой полосы (19 px).
        draw_sai_sliders(ui, state, &mut output, t.accent);
        draw_pressure_and_flags(ui, state, t.text);

        ui.add_space(6.0);
        ui.separator();

        // 4. Динамическая плотная таблица "Быстрые размеры:" (0.7..=500 px).
        if state.show_quick_sizes {
            draw_quick_sizes_grid(ui, state, &mut output);
        }

        ui.add_space(6.0);
        ui.separator();
        draw_vector_brush_row(ui, "sai_linework_brush_combo", host.vector_brush_presets, &mut state.brush_preset_id, &mut output.commands);

        ui.add_space(6.0);
        ui.separator();

        // 5. Нижние кнопки действий — все во всю ширину (28 / 26 / 27 px, шаг 4 px).
        draw_action_buttons(ui, state, &mut output, t.accent);
    });

    output.changed = *state != initial_state;
    output
}

/// Backwards-compatible action from the legacy dock panel entry point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineworkAction {
    NewLayer,
    ConvertBalloon,
}

/// Legacy compact dock entry point used by older panel wiring.
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
            if ui.button("+ Новый Linework").clicked() {
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
    use photocraft_vector::comic_sai_curve::SaiCurveType;

    use super::{LineworkAction, LineworkPanelState, PressureTargetMode, SAI_QUICK_SIZES, TipProfileId, quick_grid_cols, render_linework_dock_panel};

    #[test]
    fn linework_panel_state_defaults() {
        let s = LineworkPanelState::default();
        assert_eq!(s.tip_profile, TipProfileId::Round);
        assert_eq!(s.pressure_target, PressureTargetMode::Width);
        assert_eq!(s.curve_type, SaiCurveType::Normal);
        assert!((s.base_width - 8.0).abs() < 1e-4);
        assert!((s.base_density - 1.0).abs() < 1e-4);
        assert!((s.min_size_ratio - 0.0).abs() < 1e-4);
        assert!(s.auto_taper_ends, "Round tapers by default");
        assert!(s.snap_close);
        assert!(!s.closed_loop);
        assert!(s.show_quick_sizes);
        assert_eq!(s.brush_preset_id, None);
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
    fn tip_profile_ratios_match_presets() {
        assert!((TipProfileId::Round.ratio() - 0.0).abs() < 1e-6);
        assert!((TipProfileId::Calligraphic.ratio() - 0.05).abs() < 1e-6);
        assert!((TipProfileId::Soft.ratio() - 0.25).abs() < 1e-6);
        assert!((TipProfileId::SemiFlat.ratio() - 0.50).abs() < 1e-6);
        assert!((TipProfileId::Uniform.ratio() - 1.0).abs() < 1e-6);
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
    fn render_linework_dock_panel_renders_without_action() {
        let ctx = egui::Context::default();
        let mut state = LineworkPanelState::default();
        let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 640.0));
        let raw = egui::RawInput { screen_rect: Some(screen_rect), ..Default::default() };
        let mut action: Option<LineworkAction> = None;
        let mut out = ctx.run_ui(raw, |ui| {
            action = render_linework_dock_panel(ui, &mut state);
        });
        out.textures_delta.clear();
        assert!(action.is_none(), "no button was clicked");
    }
}
