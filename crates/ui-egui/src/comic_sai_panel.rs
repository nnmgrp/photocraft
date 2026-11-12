//! Dock panel with the PaintTool SAI Ver.2 Vector Linework parameters (`crates/ui-egui/src/comic_sai_panel.rs`).
//!
//! Provides the complete interactive SAI Ver.2 Studio experience from Gemini 4 Argon:
//! Live S-Curve preview on checkerboard, 5 tip profile presets, 38 quick brush sizes,
//! pressure dynamics (Width / Density), curve type (Normal Catmull-Rom / Bezier),
//! tension slider, auto-taper toggle, and 1-click layer creation.

use egui::{Color32, Grid, Painter, Rect, RichText, Slider, Ui, pos2, vec2};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TipProfileId {
    #[default]
    Round,
    Calligraphic,
    Soft,
    SemiFlat,
    Uniform,
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
        }
    }
}

pub fn handles(id: &str) -> bool {
    id == PANEL
}

pub const LINEWORK_LAYERS_TAB: usize = 3;

pub fn showing(app: &PhotocraftApp) -> bool {
    app.ui.panels.layers && app.ui.dock_tabs.layers == LINEWORK_LAYERS_TAB && !app.ui.dock.is_collapsed(crate::dock::Group::Layers)
}

pub fn checked(app: &PhotocraftApp, id: &str) -> Option<bool> {
    if handles(id) { Some(showing(app)) } else { None }
}

pub fn menu(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if !handles(id) {
        return None;
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

pub fn draw_sai_preview(ui: &mut Ui, base_width: f32, min_ratio: f32, base_density: f32, min_density: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().max(180.0), 38.0), egui::Sense::hover());
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
        egui::Stroke::new(eff_width, stroke_color),
    )));
}

/// Render the complete SAI Linework Studio panel.
pub fn panel(app: &mut PhotocraftApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let mut state = std::mem::take(&mut app.sai_panel);

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(4.0);

        // 1. Живой предпросмотр S-образной кривой
        ui.label(RichText::new("Предпросмотр кривой:").strong().color(t.text));
        draw_sai_preview(ui, state.base_width, state.min_size_ratio, state.base_density, state.min_density_ratio, t.accent);

        ui.add_space(6.0);
        ui.separator();

        // 2. Профили кончика пера (Tip Profiles)
        ui.label(RichText::new("Профиль сбега пера:").strong().color(t.text));
        ui.horizontal_wrapped(|ui| {
            let profiles = [
                (TipProfileId::Round, "● Round (0%)", 0.0),
                (TipProfileId::Calligraphic, "▲ Callig (5%)", 0.05),
                (TipProfileId::Soft, "■ Soft (25%)", 0.25),
                (TipProfileId::SemiFlat, "▬ Semi (50%)", 0.50),
                (TipProfileId::Uniform, "█ Uniform (100%)", 1.0),
            ];
            for (id, label, ratio) in profiles {
                let is_sel = state.tip_profile == id;
                if ui.selectable_label(is_sel, label).clicked() {
                    state.tip_profile = id;
                    state.min_size_ratio = ratio;
                }
            }
        });

        ui.add_space(6.0);
        ui.separator();

        // 3. Слайдеры толщины и плотности
        ui.horizontal(|ui| {
            ui.label("Базовый размер:");
            ui.add(Slider::new(&mut state.base_width, 0.7..=500.0).logarithmic(true).suffix(" px"));
        });
        ui.horizontal(|ui| {
            let mut pct = state.min_size_ratio * 100.0;
            ui.label("Мин. размер:");
            if ui.add(Slider::new(&mut pct, 0.0..=100.0).suffix("%")).changed() {
                state.min_size_ratio = pct / 100.0;
            }
        });
        ui.horizontal(|ui| {
            let mut pct = state.base_density * 100.0;
            ui.label("Плотность:");
            if ui.add(Slider::new(&mut pct, 0.0..=100.0).suffix("%")).changed() {
                state.base_density = pct / 100.0;
            }
        });
        ui.horizontal(|ui| {
            let mut pct = state.min_density_ratio * 100.0;
            ui.label("Мин. плотность:");
            if ui.add(Slider::new(&mut pct, 0.0..=100.0).suffix("%")).changed() {
                state.min_density_ratio = pct / 100.0;
            }
        });

        ui.add_space(6.0);
        ui.separator();

        // 4. Матрица 38 быстрых размеров PaintTool SAI Ver.2
        ui.label(RichText::new("Сетка размеров PaintTool SAI:").strong().color(t.text));
        Grid::new("sai_sizes_grid").spacing(vec2(3.0, 3.0)).show(ui, |ui| {
            for (idx, &sz) in SAI_QUICK_SIZES.iter().enumerate() {
                let is_sel = (state.base_width - sz).abs() < 0.05;
                let text = if sz < 10.0 { format!("{:.1}", sz) } else { format!("{:.0}", sz) };
                let btn = egui::Button::new(text).min_size(vec2(33.0, 22.0)).selected(is_sel);
                if ui.add(btn).clicked() {
                    state.base_width = sz;
                }
                if (idx + 1) % 8 == 0 {
                    ui.end_row();
                }
            }
        });

        ui.add_space(6.0);
        ui.separator();

        // 5. Динамика нажима и параметры сплайна
        ui.label(RichText::new("Динамика нажима пера:").strong().color(t.text));
        ui.horizontal(|ui| {
            ui.radio_value(&mut state.pressure_target, PressureTargetMode::Width, "● Width (Толщина 0..500%)");
            ui.radio_value(&mut state.pressure_target, PressureTargetMode::Density, "○ Density (Плотность 0..200%)");
        });

        ui.horizontal(|ui| {
            ui.label("Тип кривой:");
            egui::ComboBox::from_id_salt("sai_curve_combo")
                .selected_text(match state.curve_type {
                    SaiCurveType::Normal => "Normal (Catmull-Rom T=0.65)",
                    SaiCurveType::Bezier => "Besier (Cubic Bézier)",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut state.curve_type, SaiCurveType::Normal, "Normal (Catmull-Rom)");
                    ui.selectable_value(&mut state.curve_type, SaiCurveType::Bezier, "Besier (Cubic Bézier)");
                });
        });

        if state.curve_type == SaiCurveType::Normal {
            ui.horizontal(|ui| {
                ui.label("Натяжение T:");
                ui.add(Slider::new(&mut state.tension, 0.0..=1.0));
            });
        }
        ui.checkbox(&mut state.auto_taper_ends, "Auto Taper Ends (Авто-заострение концов)");
        ui.checkbox(&mut state.closed_loop, "Closed Loop (Замкнутый контур)");

        ui.add_space(10.0);
        ui.separator();

        // 6. Кнопки действий
        let new_layer_btn = ui.add_sized(
            [ui.available_width(), 28.0],
            egui::Button::new(RichText::new("＋ Создать новый Linework слой").strong().color(Color32::WHITE)).fill(t.accent),
        );
        if new_layer_btn.clicked() {
            let _ = app.run("layer.newLinework", json!({ "width": state.base_width }));
            app.ui.tool = Tool::VectorCurve;
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("⚡ Запечь баббл в Linework").clicked() {
                let _ = app.run("shape.balloon.convertToLinework", json!({}));
                app.ui.tool = Tool::VectorCurve;
            }
            if ui.button("🔄 Сбросить параметры пера").clicked() {
                state = LineworkPanelState::default();
            }
        });
    });

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
}
