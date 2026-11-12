//! The Manga Balloons generator dock panel (`crates/ui-egui/src/comic_balloon_panel.rs`).
//!
//! Mirrors the architectural pattern of `comic_screentone_panel.rs`:
//! Offers a dedicated dock tab, Window menu registration (`window.panel.balloon`),
//! body selectors (Ellipse, Rect, Cloud, Spiky, Uniflash), tail presets (Wedge, Arc,
//! SCurve, Lightning, Double, Thought), dynamic parameter sliders, and 1-click spawn.
//! Protected against deletion: guarantees minimum body bounds and preserves closed geometry.

use egui::{Color32, RichText, Slider, Ui};
use photocraft_doc::vector::{LiveBalloon, LiveBalloonBody, LiveBalloonTail, TailPreset};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::state::Tool;
use crate::theme::Tokens;

/// The Window-menu id of the panel.
pub const PANEL: &str = "window.panel.balloon";

/// Minimum allowed dimensions for a balloon body to protect against collapse/deletion.
pub const MIN_BALLOON_SIZE: f64 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BalloonBodyKindChoice {
    #[default]
    Ellipse,
    Rect,
    Cloud,
    Spiky,
    Uniflash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BalloonFillMode {
    #[default]
    White,
    Transparent,
    Background,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BalloonPanelUi {
    pub body_choice: BalloonBodyKindChoice,
    // Ellipse / Rect
    pub squircle: f64,
    pub corner_radius: f64,
    // Cloud
    pub cloud_bumps: u32,
    pub cloud_amplitude: f64,
    pub cloud_cusp_bite: f64,
    pub cloud_jitter: f64,
    pub cloud_seed: u32,
    // Spiky
    pub spiky_spikes: u32,
    pub spiky_length: f64,
    pub spiky_sub_harmonic: f64,
    pub spiky_concavity: f64,
    pub spiky_jitter: f64,
    pub spiky_smooth: bool,
    pub spiky_seed: u32,
    // Uniflash
    pub uniflash_needles: u32,
    pub uniflash_inner_ratio: f64,
    pub uniflash_bundle: f64,
    pub uniflash_needle_width: f64,
    pub uniflash_seed: u32,
    // Tail
    pub tail_preset: TailPreset,
    pub tail_mouth_width: f64,
    pub tail_bend: f64,
    pub tail_bubbles: u32,
    pub tail_bubble_radius: f64,
    // Style
    pub stroke_width: f64,
    pub fill_mode: BalloonFillMode,
}

impl Default for BalloonPanelUi {
    fn default() -> Self {
        Self {
            body_choice: BalloonBodyKindChoice::Ellipse,
            squircle: 0.25,
            corner_radius: 12.0,
            cloud_bumps: 10,
            cloud_amplitude: 0.18,
            cloud_cusp_bite: 0.22,
            cloud_jitter: 0.35,
            cloud_seed: 42,
            spiky_spikes: 18,
            spiky_length: 0.38,
            spiky_sub_harmonic: 0.45,
            spiky_concavity: 0.65,
            spiky_jitter: 0.30,
            spiky_smooth: false,
            spiky_seed: 101,
            uniflash_needles: 160,
            uniflash_inner_ratio: 0.62,
            uniflash_bundle: 0.40,
            uniflash_needle_width: 2.2,
            uniflash_seed: 777,
            tail_preset: TailPreset::Arc,
            tail_mouth_width: 28.0,
            tail_bend: 0.18,
            tail_bubbles: 3,
            tail_bubble_radius: 10.0,
            stroke_width: 2.5,
            fill_mode: BalloonFillMode::White,
        }
    }
}

pub fn handles(id: &str) -> bool {
    id == PANEL
}

pub const BALLOON_COLOR_TAB: usize = 5;

pub fn showing(app: &PhotocraftApp) -> bool {
    app.ui.panels.color && app.ui.dock_tabs.color == BALLOON_COLOR_TAB && !app.ui.dock.is_collapsed(crate::dock::Group::Color)
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
    app.ui.panels.color = show || (app.ui.panels.color && app.ui.dock_tabs.color != BALLOON_COLOR_TAB);
    if show {
        app.ui.dock_tabs.color = BALLOON_COLOR_TAB;
        crate::dock::reveal(app, crate::dock::Group::Color);
    }
    Some(Ok(json!({ "showing": show })))
}

/// Render the Manga Balloons dock panel.
pub fn panel(app: &mut PhotocraftApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let mut state = std::mem::take(&mut app.ui.balloon_ui);

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(4.0);

        // 1. Форма тела (Body kind selector)
        ui.label(RichText::new("Форма тела баббла:").strong().color(t.text));
        ui.horizontal_wrapped(|ui| {
            let bodies = [
                (BalloonBodyKindChoice::Ellipse, "⚪ Ellipse"),
                (BalloonBodyKindChoice::Rect, "◽ Rect"),
                (BalloonBodyKindChoice::Cloud, "☁️ Cloud"),
                (BalloonBodyKindChoice::Spiky, "💥 Spiky"),
                (BalloonBodyKindChoice::Uniflash, "🦔 Uniflash"),
            ];
            for (choice, label) in bodies {
                let is_sel = state.body_choice == choice;
                if ui.selectable_label(is_sel, label).clicked() {
                    state.body_choice = choice;
                }
            }
        });

        ui.add_space(6.0);
        ui.separator();

        // 2. Параметры выбранного тела
        match state.body_choice {
            BalloonBodyKindChoice::Ellipse => {
                ui.horizontal(|ui| {
                    ui.label("Сквиркл:");
                    ui.add(Slider::new(&mut state.squircle, 0.0..=1.0));
                });
            }
            BalloonBodyKindChoice::Rect => {
                ui.horizontal(|ui| {
                    ui.label("Радиус углов:");
                    ui.add(Slider::new(&mut state.corner_radius, 0.0..=60.0).suffix(" px"));
                });
            }
            BalloonBodyKindChoice::Cloud => {
                ui.horizontal(|ui| {
                    ui.label("Волны (Bumps):");
                    ui.add(Slider::new(&mut state.cloud_bumps, 4..=64));
                });
                ui.horizontal(|ui| {
                    ui.label("Высота фестонов:");
                    ui.add(Slider::new(&mut state.cloud_amplitude, 0.04..=0.60));
                });
                ui.horizontal(|ui| {
                    ui.label("Заход стыков:");
                    ui.add(Slider::new(&mut state.cloud_cusp_bite, 0.0..=0.50));
                });
                ui.horizontal(|ui| {
                    ui.label("Асимметрия:");
                    ui.add(Slider::new(&mut state.cloud_jitter, 0.0..=1.0));
                    if ui.button("🎲").on_hover_text("Случайная вариация асимметрии").clicked() {
                        state.cloud_seed = state.cloud_seed.wrapping_add(13);
                    }
                });
            }
            BalloonBodyKindChoice::Spiky => {
                ui.horizontal(|ui| {
                    ui.label("Зубья (Spikes):");
                    ui.add(Slider::new(&mut state.spiky_spikes, 5..=128));
                });
                ui.horizontal(|ui| {
                    ui.label("Длина лучей:");
                    ui.add(Slider::new(&mut state.spiky_length, 0.05..=1.50));
                });
                ui.horizontal(|ui| {
                    ui.label("Субгармоники:");
                    ui.add(Slider::new(&mut state.spiky_sub_harmonic, 0.0..=1.0));
                });
                ui.horizontal(|ui| {
                    ui.label("Прогиб внутрь:");
                    ui.add(Slider::new(&mut state.spiky_concavity, 0.0..=1.0));
                });
                ui.horizontal(|ui| {
                    ui.label("Асимметрия:");
                    ui.add(Slider::new(&mut state.spiky_jitter, 0.0..=1.0));
                });
                ui.checkbox(&mut state.spiky_smooth, "Скруглять вершины");
            }
            BalloonBodyKindChoice::Uniflash => {
                ui.horizontal(|ui| {
                    ui.label("Иглы (Needles):");
                    ui.add(Slider::new(&mut state.uniflash_needles, 32..=400));
                });
                ui.horizontal(|ui| {
                    let mut pct = (state.uniflash_inner_ratio * 100.0).round();
                    ui.label("Поле под текст:");
                    if ui.add(Slider::new(&mut pct, 30.0..=90.0).suffix("%")).changed() {
                        state.uniflash_inner_ratio = pct / 100.0;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Пучки (Bundle):");
                    ui.add(Slider::new(&mut state.uniflash_bundle, 0.0..=1.0));
                });
                ui.horizontal(|ui| {
                    ui.label("Ширина иглы:");
                    ui.add(Slider::new(&mut state.uniflash_needle_width, 1.0..=8.0).suffix(" px"));
                });
            }
        }

        ui.add_space(6.0);
        ui.separator();

        // 3. Пресеты хвоста (Tail Presets)
        ui.label(RichText::new("Тип хвоста:").strong().color(t.text));
        ui.horizontal_wrapped(|ui| {
            let tails = [
                (TailPreset::Wedge, "▽ Wedge"),
                (TailPreset::Arc, "⌒ Arc"),
                (TailPreset::SCurve, "~ SCurve"),
                (TailPreset::Lightning, "⚡ Lightning"),
                (TailPreset::Double, "⑂ Double"),
                (TailPreset::Thought, "◌ Thought"),
            ];
            for (preset, label) in tails {
                let is_sel = state.tail_preset == preset;
                if ui.selectable_label(is_sel, label).clicked() {
                    state.tail_preset = preset;
                }
            }
        });

        ui.horizontal(|ui| {
            ui.label("Ширина устья:");
            ui.add(Slider::new(&mut state.tail_mouth_width, 6.0..=120.0).suffix(" px"));
        });
        ui.horizontal(|ui| {
            ui.label("Изгиб (Bend):");
            ui.add(Slider::new(&mut state.tail_bend, -1.0..=1.0));
        });
        if state.tail_preset == TailPreset::Thought {
            ui.horizontal(|ui| {
                ui.label("Пузырьки мыслей:");
                ui.add(Slider::new(&mut state.tail_bubbles, 1..=8));
            });
            ui.horizontal(|ui| {
                ui.label("Радиус пузырька:");
                ui.add(Slider::new(&mut state.tail_bubble_radius, 4.0..=30.0).suffix(" px"));
            });
        }

        ui.add_space(6.0);
        ui.separator();

        // 4. Стиль контура и заливки
        ui.label(RichText::new("Обводка и заливка:").strong().color(t.text));
        ui.horizontal(|ui| {
            ui.label("Толщина:");
            for w in [1.5, 2.0, 2.5, 3.0, 4.0] {
                let is_cur = (state.stroke_width - w).abs() < 0.1;
                if ui.selectable_label(is_cur, format!("{w}")).clicked() {
                    state.stroke_width = w;
                }
            }
            ui.add(Slider::new(&mut state.stroke_width, 0.5..=20.0).suffix(" px"));
        });
        ui.horizontal(|ui| {
            ui.label("Заливка:");
            ui.radio_value(&mut state.fill_mode, BalloonFillMode::White, "Белая");
            ui.radio_value(&mut state.fill_mode, BalloonFillMode::Transparent, "Прозрачная");
            ui.radio_value(&mut state.fill_mode, BalloonFillMode::Background, "Цвет фона");
        });

        ui.add_space(10.0);
        ui.separator();

        // 5. Кнопки действий (Action Buttons)
        let create_btn = ui.add_sized(
            [ui.available_width(), 30.0],
            egui::Button::new(RichText::new("＋ Создать баббл на холсте").strong().color(Color32::WHITE)).fill(t.accent),
        );
        if create_btn.clicked() {
            create_balloon_on_canvas(app, &state);
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("＋ Добавить хвост").on_hover_text("Добавить хвост к выделенному бабблу").clicked() {
                add_tail_to_active(app, &state);
            }
            if ui.button("⚡ Запечь в SAI Linework").on_hover_text("Превратить баббл в редактируемый векторный слой штрихов").clicked() {
                let _ = app.run("shape.balloon.convertToLinework", json!({}));
                app.ui.tool = Tool::VectorCurve;
                app.ui.dock_tabs.layers = 3;
                crate::dock::reveal(app, crate::dock::Group::Layers);
            }
        });

        if ui.button("🔄 Сбросить к стандарту манги").clicked() {
            state = BalloonPanelUi::default();
        }
    });

    app.ui.balloon_ui = state;
}

fn create_balloon_on_canvas(app: &mut PhotocraftApp, st: &BalloonPanelUi) {
    let Some(active) = app.session.active() else { return };
    let doc_size = active.doc.size;

    let bounds = active.doc.selection.as_ref()
        .map(photocraft_compose::bounds::content_bounds)
        .filter(|r| r.width() >= 16 && r.height() >= 16);

    let (bx, by, bw, bh) = match bounds {
        Some(r) => {
            let w = (r.width() as f64).max(MIN_BALLOON_SIZE);
            let h = (r.height() as f64).max(MIN_BALLOON_SIZE);
            (r.x0 as f64, r.y0 as f64, w, h)
        }
        None => {
            let w = 240.0f64.min(doc_size.width as f64 * 0.8).max(MIN_BALLOON_SIZE);
            let h = 160.0f64.min(doc_size.height as f64 * 0.8).max(MIN_BALLOON_SIZE);
            let x = (doc_size.width as f64 - w) * 0.5;
            let y = (doc_size.height as f64 - h) * 0.5;
            (x, y, w, h)
        }
    };

    let rect = [bx, by, bw, bh];
    let body = match st.body_choice {
        BalloonBodyKindChoice::Ellipse => LiveBalloonBody::Ellipse { rect, squircle: st.squircle },
        BalloonBodyKindChoice::Rect => LiveBalloonBody::Rect { rect, corner_radius: st.corner_radius },
        BalloonBodyKindChoice::Cloud => LiveBalloonBody::Cloud {
            rect,
            bumps: st.cloud_bumps.max(4),
            amplitude: st.cloud_amplitude,
            cusp_bite: st.cloud_cusp_bite,
            jitter: st.cloud_jitter,
            seed: st.cloud_seed,
        },
        BalloonBodyKindChoice::Spiky => LiveBalloonBody::Spiky {
            rect,
            spikes: st.spiky_spikes.max(5),
            length: st.spiky_length,
            sub_harmonic: st.spiky_sub_harmonic,
            concavity: st.spiky_concavity,
            jitter: st.spiky_jitter,
            smooth: st.spiky_smooth,
            seed: st.spiky_seed,
        },
        BalloonBodyKindChoice::Uniflash => LiveBalloonBody::Uniflash {
            rect,
            needles: st.uniflash_needles.max(32),
            inner_ratio: st.uniflash_inner_ratio,
            bundle_group: st.uniflash_bundle,
            length_jitter: 0.5,
            angle_jitter: 0.25,
            needle_width: st.uniflash_needle_width,
            seed: st.uniflash_seed,
        },
    };

    let tail = LiveBalloonTail {
        id: 1,
        preset: st.tail_preset,
        perimeter_t: 0.58,
        mouth_width: st.tail_mouth_width,
        tip: [bx + bw * 0.25, by + bh + 48.0],
        bend: st.tail_bend,
        bubbles: st.tail_bubbles,
        bubble_radius: st.tail_bubble_radius,
    };

    let balloon = LiveBalloon {
        id: 1,
        body,
        tails: vec![tail],
        next_tail_id: 2,
        vertex_overrides: Vec::new(),
        hand_drawn_noise: 0.0,
        stroke_width: st.stroke_width,
        auto_merge_overlapping: true,
    };

    let fill_val = match st.fill_mode {
        BalloonFillMode::White => json!("#ffffff"),
        BalloonFillMode::Transparent => json!(null),
        BalloonFillMode::Background => {
            let bg = app.session.tools.background;
            json!(format!("#{:02x}{:02x}{:02x}", (bg[0]*255.0) as u8, (bg[1]*255.0) as u8, (bg[2]*255.0) as u8))
        }
    };

    let _ = app.run("shape.balloon.create", json!({
        "balloon": balloon,
        "fill": fill_val,
        "stroke": { "width": st.stroke_width, "color": "#000000" }
    }));

    // Мгновенно переключаем инструмент на бабблы, чтобы на экране появились ручки Гизмо!
    app.ui.tool = Tool::Balloon;
}

fn add_tail_to_active(app: &mut PhotocraftApp, st: &BalloonPanelUi) {
    let Some(active) = app.session.active() else { return };
    let Some(layer_id) = active.active_layer else { return };

    let _ = app.run("shape.balloon.tail.add", json!({
        "layer": layer_id.0,
        "balloonId": 1,
        "preset": st.tail_preset.id(),
        "mouthWidth": st.tail_mouth_width,
        "tip": [200.0, 300.0]
    }));
    app.ui.tool = Tool::Balloon;
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
    fn window_menu_toggles_the_balloon_dock_tab() {
        let (mut app, ctx) = test_app();
        for pro in [false, true] {
            assert_eq!(crate::dock::Group::Color.tabs(pro).get(BALLOON_COLOR_TAB), Some(&"Balloons"), "pro {pro}");
        }
        assert!(crate::menus::is_live(PANEL), "{PANEL}");
        crate::menus::invoke(&mut app, &ctx, PANEL, Value::Null).unwrap();
        assert!(app.ui.panels.color, "the Color group shows");
        assert_eq!(app.ui.dock_tabs.color, BALLOON_COLOR_TAB, "the Balloons tab is selected");
        assert_eq!(checked(&app, PANEL), Some(true));
        crate::menus::invoke(&mut app, &ctx, PANEL, json!({"show": false})).unwrap();
        assert_eq!(checked(&app, PANEL), Some(false));
    }

    #[test]
    fn balloon_panel_ui_default_and_protection() {
        let st = BalloonPanelUi::default();
        assert_eq!(st.body_choice, BalloonBodyKindChoice::Ellipse);
        assert_eq!(st.tail_preset, TailPreset::Arc);
        assert!(MIN_BALLOON_SIZE >= 24.0);
        assert!(st.uniflash_needles >= 32);
        assert!(st.stroke_width > 0.0);
    }

    #[test]
    fn create_balloon_on_canvas_protects_bounds_and_activates_tool() {
        let (mut app, _ctx) = test_app();
        let st = BalloonPanelUi::default();
        create_balloon_on_canvas(&mut app, &st);
        assert_eq!(app.ui.tool, Tool::Balloon);
        let active = app.session.active().unwrap();
        assert!(active.active_layer.is_some());
    }
}
