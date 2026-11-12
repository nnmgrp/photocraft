//! Dock panel with the PaintTool SAI Ver.2 Vector Linework parameters (`crates/ui-egui/src/comic_sai_panel.rs`).
//!
//! The panel shows a live preview of the S-curve, the brush size and its minimum, the density and
//! its minimum, the curve type (`Normal` / `Bezier`), the 38 quick sizes of PaintTool SAI and the
//! two actions that create a Linework layer and convert a balloon into linework.

use egui::{Color32, Grid, Painter, Rect, Slider, Ui, pos2, vec2};
use photocraft_vector::comic_sai_curve::SaiCurveType;

/// The quick sizes of PaintTool SAI (`0.7 .. 500 px`).
pub const SAI_QUICK_SIZES: [f32; 38] = [
    0.7, 0.8, 1.0, 1.5, 2.0, 2.3, 2.6, 3.0, // Row 1
    3.5, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, // Row 2
    12.0, 14.0, 16.0, 20.0, 25.0, 30.0, 35.0, 40.0, // Row 3
    50.0, 60.0, 70.0, 80.0, 100.0, 120.0, 160.0, 200.0, // Row 4
    250.0, 300.0, 350.0, 400.0, 450.0, 500.0, // Row 5
];

/// What the pressure of the tool drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PressureTargetMode {
    #[default]
    Width,
    Density,
}

/// The state of the Linework panel.
pub struct LineworkPanelState {
    pub base_width: f32,
    pub min_size_ratio: f32,
    pub base_density: f32,
    pub min_density_ratio: f32,
    pub curve_type: SaiCurveType,
    pub pressure_target: PressureTargetMode,
}

impl Default for LineworkPanelState {
    fn default() -> Self {
        Self {
            base_width: 8.0,
            min_size_ratio: 0.0,
            base_density: 1.0,
            min_density_ratio: 0.0,
            curve_type: SaiCurveType::Normal,
            pressure_target: PressureTargetMode::Width,
        }
    }
}

/// The checkerboard behind the preview.
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

/// The live S-curve preview: the width, the tapered ends and the density are shown as they are.
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

/// What the panel asks the app to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineworkAction {
    /// Create a new Linework layer.
    NewLayer,
    /// Convert the balloon of the active layer into linework.
    ConvertBalloon,
}

/// The body of the Linework dock panel. Returns the action the user asked for, if any.
pub fn render_linework_dock_panel(ui: &mut Ui, state: &mut LineworkPanelState) -> Option<LineworkAction> {
    let mut action = None;
    ui.vertical(|ui| {
        // 1. The live S-curve preview.
        draw_sai_preview(ui, state.base_width, state.min_size_ratio, state.base_density, state.min_density_ratio, Color32::from_rgb(226, 232, 240));

        ui.add_space(8.0);

        // 2. The sliders of PaintTool SAI.
        ui.horizontal(|ui| {
            ui.label("Brush Size:");
            ui.add(Slider::new(&mut state.base_width, 0.1..=500.0).logarithmic(true).suffix(" px"));
        });

        ui.horizontal(|ui| {
            let mut min_pct = state.min_size_ratio * 100.0;
            ui.label("Min Size:");
            if ui.add(Slider::new(&mut min_pct, 0.0..=100.0).suffix("%")).changed() {
                state.min_size_ratio = min_pct / 100.0;
            }
        });

        ui.horizontal(|ui| {
            let mut dens_pct = state.base_density * 100.0;
            ui.label("Density:");
            if ui.add(Slider::new(&mut dens_pct, 0.0..=100.0).suffix("%")).changed() {
                state.base_density = dens_pct / 100.0;
            }
        });

        ui.horizontal(|ui| {
            let mut min_dens_pct = state.min_density_ratio * 100.0;
            ui.label("Min Density:");
            if ui.add(Slider::new(&mut min_dens_pct, 0.0..=100.0).suffix("%")).changed() {
                state.min_density_ratio = min_dens_pct / 100.0;
            }
        });

        ui.add_space(6.0);
        ui.separator();

        // 3. What the pressure of the tool drives.
        ui.label(egui::RichText::new("Режим нажима инструмента Pressure:").strong());
        ui.horizontal(|ui| {
            ui.radio_value(&mut state.pressure_target, PressureTargetMode::Width, "● Width (Толщина 0..500%)");
            ui.radio_value(&mut state.pressure_target, PressureTargetMode::Density, "○ Density (Плотность 0..200%)");
        });

        // 4. The curve type.
        ui.horizontal(|ui| {
            ui.label("Curve Type:");
            egui::ComboBox::from_id_salt("sai_curve_type_select")
                .selected_text(match state.curve_type {
                    SaiCurveType::Normal => "Normal (Catmull-Rom T=0.65)",
                    SaiCurveType::Bezier => "Besier (Cubic Bézier)",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut state.curve_type, SaiCurveType::Normal, "Normal (Catmull-Rom)");
                    ui.selectable_value(&mut state.curve_type, SaiCurveType::Bezier, "Besier (Cubic Bézier)");
                });
        });

        ui.add_space(8.0);
        ui.separator();

        // 5. The quick size matrix (38 sizes, 6 per row).
        ui.label(egui::RichText::new("Сетка быстрых пресетов размера:").strong());
        Grid::new("sai_quick_size_grid").spacing(vec2(4.0, 4.0)).show(ui, |ui| {
            for (idx, &size) in SAI_QUICK_SIZES.iter().enumerate() {
                let is_selected = (state.base_width - size).abs() < 0.05;
                let text = if size < 10.0 { format!("{:.1}", size) } else { format!("{:.0}", size) };
                let btn = egui::Button::new(text).min_size(vec2(34.0, 24.0)).selected(is_selected);
                if ui.add(btn).clicked() {
                    state.base_width = size;
                }
                if (idx + 1) % 6 == 0 {
                    ui.end_row();
                }
            }
        });

        ui.add_space(10.0);
        ui.separator();

        // 6. The actions.
        ui.horizontal(|ui| {
            if ui.button("＋ Новый Linework").clicked() {
                action = Some(LineworkAction::NewLayer);
            }
            if ui.button("⚡ Convert Balloon to Linework").clicked() {
                action = Some(LineworkAction::ConvertBalloon);
            }
        });
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_quick_sizes_are_the_sai_ones() {
        assert_eq!(SAI_QUICK_SIZES.len(), 38);
        assert_eq!(SAI_QUICK_SIZES[0], 0.7);
        assert_eq!(SAI_QUICK_SIZES[SAI_QUICK_SIZES.len() - 1], 500.0);
        let mut sorted = SAI_QUICK_SIZES;
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        assert_eq!(sorted, SAI_QUICK_SIZES, "the quick sizes are sorted");
    }

    #[test]
    fn the_panel_starts_with_the_sai_defaults() {
        let state = LineworkPanelState::default();
        assert_eq!(state.base_width, 8.0);
        assert_eq!(state.base_density, 1.0);
        assert_eq!(state.curve_type, SaiCurveType::Normal);
        assert_eq!(state.pressure_target, PressureTargetMode::Width);
    }
}
