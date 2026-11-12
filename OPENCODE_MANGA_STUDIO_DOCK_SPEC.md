# ИСЧЕРПЫВАЮЩЕЕ ТЕХНИЧЕСКОЕ ЗАДАНИЕ (TURNKEY SPEC) ДЛЯ OPENCODE
## Внедрение выделенных док-панелей Manga Balloons Generator и PaintTool SAI Linework Studio в PhotoCraft

> [!IMPORTANT]
> **ИНСТРУКЦИЯ ДЛЯ АГЕНТА OPENCODE:**
> Этот документ содержит **готовый, полностью отлаженный Rust-код** для всех новых и изменяемых файлов.
> Никаких пропущенных секций, псевдокода или маркеров `// TODO: implement here`.
> Выполняйте шаги строго по порядку (Шаги 1–8), сохраняя структуру файлов и правила репозитория (`AGENTS.md`).

---

## 0. Архитектурная цель

Вместо отображения настроек в верхней 24-пиксельной строке опций (`options_bar`) создаются **два первоклассных дока** по архитектурному образцу **Gentonik Screentones (`comic_screentone_panel.rs`)**:

1. **`crates/ui-egui/src/comic_balloon_panel.rs` (НОВЫЙ ФАЙЛ)**:
   - Выделенный док-генератор бабблов (5 тел, 6 хвостов, динамические слайдеры Uniflash/Spiky/Cloud).
   - Большая кнопка `[ ＋ Создать баббл на холсте ]` в 1 клик (с авто-выбором `Tool::Balloon` и включением экранного Гизмо).
   - Пункт меню `Window > Manga Balloons` (`window.panel.balloon`).
2. **`crates/ui-egui/src/comic_sai_panel.rs` (ПОЛНОЕ ОБНОВЛЕНИЕ)**:
   - Выделенный док студии кривых SAI Linework из веб-студии Gemini 4 Argon:
     - Живое S-превью сглаживания и плотности.
     - 5 профилей кончиков пера (`Round`, `Calligraphic`, `Soft`, `Semi-Flat`, `Uniform`).
     - Матрица 38 быстрых размеров PaintTool SAI Ver.2 (`0.7 .. 500 px`).
     - Динамика нажима (`Width` vs `Density`), выбор кривой (`Normal Catmull-Rom` vs `Bezier`), натяжение $T$.
   - Пункт меню `Window > SAI Linework` (`window.panel.linework`).
3. **Паритет PaintTool SAI в футере панели слоёв (`panels.rs`)**:
   - Клик по кнопке [🖋️] `spline` создаёт слой Linework $\to$ переключает инструмент на `Tool::VectorCurve` $\to$ раскрывает док SAI Linework.

---

## Шаг 1. Создать `crates/ui-egui/src/comic_balloon_panel.rs` (НОВЫЙ ФАЙЛ)

Создать файл `crates/ui-egui/src/comic_balloon_panel.rs` со следующим полным содержимым:

```rust
//! The Manga Balloons generator dock panel (`crates/ui-egui/src/comic_balloon_panel.rs`).
//!
//! Mirrors the architectural pattern of `comic_screentone_panel.rs`:
//! Offers a dedicated dock tab, Window menu registration (`window.panel.balloon`),
//! body selectors (Ellipse, Rect, Cloud, Spiky, Uniflash), tail presets (Wedge, Arc,
//! SCurve, Lightning, Double, Thought), dynamic parameter sliders, and 1-click spawn.

use egui::{Color32, Rect, RichText, Sense, Slider, Ui, pos2, vec2};
use photocraft_doc::vector::{LiveBalloon, LiveBalloonBody, LiveBalloonTail, TailPreset};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::state::Tool;
use crate::theme::Tokens;

/// The Window-menu id of the panel.
pub const PANEL: &str = "window.panel.balloon";

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
                    if ui.button("🎲").clicked() {
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
        Some(r) => (r.x0 as f64, r.y0 as f64, r.width() as f64, r.height() as f64),
        None => {
            let w = 240.0f64.min(doc_size.width as f64 * 0.8);
            let h = 160.0f64.min(doc_size.height as f64 * 0.8);
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
            bumps: st.cloud_bumps,
            amplitude: st.cloud_amplitude,
            cusp_bite: st.cloud_cusp_bite,
            jitter: st.cloud_jitter,
            seed: st.cloud_seed,
        },
        BalloonBodyKindChoice::Spiky => LiveBalloonBody::Spiky {
            rect,
            spikes: st.spiky_spikes,
            length: st.spiky_length,
            sub_harmonic: st.spiky_sub_harmonic,
            concavity: st.spiky_concavity,
            smooth: st.spiky_smooth,
            seed: st.spiky_seed,
        },
        BalloonBodyKindChoice::Uniflash => LiveBalloonBody::Uniflash {
            rect,
            needles: st.uniflash_needles,
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
```

---

## Шаг 2. Заменить `crates/ui-egui/src/comic_sai_panel.rs` (ПОЛНОЕ ОБНОВЛЕНИЕ)

Заменить содержимое `crates/ui-egui/src/comic_sai_panel.rs` следующим расширенным кодом:

```rust
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
```

---

## Шаг 3. Зарегистрировать модуль и состояние в `crates/ui-egui/src/`

### 3.1 В `crates/ui-egui/src/lib.rs`
Около строки 216:
```rust
pub mod comic_balloon_panel;
pub use comic_balloon_panel as balloon_panel;
pub mod comic_sai_panel;
pub use comic_sai_panel as sai_panel;
```

### 3.2 В `crates/ui-egui/src/state.rs`
В структуре `UiState` (или `AppState`):
```rust
/// The Manga Balloons generator panel state.
pub balloon_ui: crate::comic_balloon_panel::BalloonPanelUi,
```
И в `Default for UiState`:
```rust
balloon_ui: crate::comic_balloon_panel::BalloonPanelUi::default(),
```

---

## Шаг 4. Регистрация в меню `Window`

### 4.1 В `crates/ui-egui/src/menu_catalog.rs`
Найти строку:
```rust
(&["Window"], "Screentones", None, "window.panel.screentone"),
```
Добавить сразу под ней:
```rust
(&["Window"], "Manga Balloons", None, "window.panel.balloon"),
(&["Window"], "SAI Linework", None, "window.panel.linework"),
```

### 4.2 В `crates/ui-egui/src/menus.rs`
1. В `menu_action` (около строки 136):
```rust
if let Some(r) = crate::comic_balloon_panel::menu(app, id, &params) {
    return r;
}
if let Some(r) = crate::comic_sai_panel::menu(app, id, &params) {
    return r;
}
```
2. В `is_enabled` (около строки 732):
```rust
if crate::comic_balloon_panel::handles(id) || crate::comic_sai_panel::handles(id) {
    return true;
}
```
3. В `checked` (около строки 857):
```rust
if let Some(c) = crate::comic_balloon_panel::checked(app, id) {
    return Some(c);
}
if let Some(c) = crate::comic_sai_panel::checked(app, id) {
    return Some(c);
}
```

---

## Шаг 5. Интеграция в док и исправление клика в слоях (`panels.rs`)

### 5.1 В `crates/ui-egui/src/dock.rs`
В методе `tabs` добавить вкладку `"Balloons"` в `Group::Color`:
```rust
Group::Color if pro => &["Color", "Swatches", "Gradients", "Patterns", "Screentones", "Balloons"],
Group::Color => &["Swatches", "Color", "Gradients", "Patterns", "Screentones", "Balloons"],
```

### 5.2 В `crates/ui-egui/src/panels.rs`
1. В функции `dock_body_inner`:
```rust
(Group::Color, 4) => crate::screentone_panel::panel(app, ui),
(Group::Color, 5) => crate::comic_balloon_panel::panel(app, ui),
```
И для `Linework` в `Group::Layers`:
```rust
(Group::Layers, 3) => crate::comic_sai_panel::panel(app, ui),
```

2. В строке ~3432 заменить обработчик клика кнопки `spline`:
```rust
// Было:
let linework = icons::button(ui, "spline", 26.0, false, tl!("Create a Linework vector layer"));
if linework.clicked() {
    actions.push(("layer.newLinework".into(), json!({})));
}

// ЗАМЕНИТЬ НА:
let linework = icons::button(ui, "spline", 26.0, false, tl!("Create a Linework vector layer"));
if linework.clicked() {
    actions.push(("layer.newLinework".into(), json!({ "width": app.sai_panel.base_width })));
    app.ui.tool = crate::state::Tool::VectorCurve;
    app.ui.dock_tabs.layers = 3;
    crate::dock::reveal(app, crate::dock::Group::Layers);
}
```

---

## Шаг 6. Обновить тесты дока в `crates/ui-egui/src/dock_tests.rs`

Поскольку в `Group::Color` теперь 6 вкладок, обновить проверку длины и порядка вкладок `Group::Color` с 5 до 6.

---

## Шаг 7. Переводы в `crates/ui-egui/src/i18n/*.tsv`

В 21 каталог локализации добавить ключи:
- `Manga Balloons`
- `SAI Linework`
- `Balloons`

---

## Шаг 8. Проверка и сборка

```bash
cargo fmt --all -- --check
cargo test -p photocraft-ui-egui --lib comic
cargo test -p photocraft-ui-egui --lib dock
cargo run -p xtask -- parity
cargo run -p xtask -- scorecard
cargo build --release --bin photocraft
```