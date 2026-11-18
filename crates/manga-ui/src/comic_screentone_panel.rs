//! The Screentones dock tab: the comic screentone generator's panel.
//!
//! This is the `manga-ui` port of the panel. It edits one [`ScreentoneSpec`] and emits
//! [`OutCommand`](crate::OutCommand)s instead of touching the host app or the engine:
//! `layer.screentone` (a procedural sheet on a new layer), `filter.screentone` (the same
//! generator applied to the active layer, where Use Source Luminance can halftone by that
//! layer's own darkness) or `pattern.screentone` (the sheet saved into the pattern library).
//! The spec's serde names are the commands' flat parameter notation, so the payloads are
//! exactly what a filter dialog or a saved preset would send.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use photocraft_algo::{SatelliteLayout, ScreentoneBackend, ScreentoneMode, ScreentoneSpec};

use crate::theme::Tokens;

/// The Window-menu id of the panel.
pub const PANEL: &str = "window.panel.screentone";

/// The command that applies the generator to the active layer.
pub const CMD_FILTER: &str = "filter.screentone";

/// The command that creates a procedural screentone layer.
pub const CMD_LAYER: &str = "layer.screentone";

/// The command that saves the current sheet into the pattern library.
pub const CMD_PATTERN: &str = "pattern.screentone";

/// The panel's tab index in the Color dock group (`dock`); both pro and Studio orders.
pub const COLOR_TAB: usize = 4;

/// The pattern types the panel offers, in the commands' own notation.
const PATTERNS: [&str; 12] =
    ["dots", "lines", "crosshatch", "noise", "gaussianNoise", "stipple", "hexgrid", "checker", "concentric", "stars", "hearts", "triangles"];
/// The dot shapes the panel offers (the dots' and the satellites').
const SHAPES: [&str; 4] = ["circle", "square", "diamond", "hexagon"];
/// The gradient types the panel offers for modulation.
const GRADIENTS: [&str; 3] = ["none", "linear", "radial"];

/// Panel state (serde, so the control channel can read and drive it).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ScreentoneUi {
    /// The generator's settings; picking a preset replaces them wholesale.
    pub spec: ScreentoneSpec,
    /// The built-in preset the settings came from ("" = the panel's own settings).
    pub preset: String,
    /// The new layer's name; empty = the preset's name, or "Screentone".
    pub name: String,
}

impl Default for ScreentoneUi {
    fn default() -> Self {
        Self {
            spec: ScreentoneSpec {
                mode: ScreentoneMode::Shading,
                use_source_luminance: true,
                dot_size: 4.5,
                spacing_x: 8.0,
                spacing_y: 8.0,
                angle: 45.0,
                density: 0.75,
                background: [1.0, 1.0, 1.0, 1.0],
                preserve_lines: true,
                line_threshold: 0.24,
                line_thickness: 1.0,
                ..ScreentoneSpec::default()
            },
            preset: String::new(),
            name: String::new(),
        }
    }
}

/// The live screentone layer the panel can restyle or freeze, as reported by the host.
pub struct ScreentoneTargetInfo {
    pub doc_id: crate::BridgeDocId,
    pub layer_id: photocraft_doc::LayerId,
    pub spec: ScreentoneSpec,
}

/// The dispatch buffer the panel fills: commands to send plus gizmo lifecycle hints.
pub struct ScreentonePanelOutput {
    pub commands: Vec<crate::OutCommand>,
    pub detach_gizmo: bool,
    pub expect_new_layer_bind: bool,
}

/// The enum's notation id, from its serde name (the commands' notation): `Dots` → `"dots"`.
fn id_of<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}

/// The enum value an id names, or `None` for anything else.
fn from_id<T: serde::de::DeserializeOwned>(id: &str) -> Option<T> {
    serde_json::from_value(json!(id)).ok()
}

/// A small, deterministic random seed derived from frame time and the previous seed.
fn random_seed(entropy: u64, current: i64) -> i64 {
    use std::collections::hash_map::RandomState;
    use std::hash::BuildHasher;
    let hasher = RandomState::new();
    (hasher.hash_one((entropy, current)) % 30_001) as i64
}

/// Draws one dim, small label used by every grid row.
fn row(ui: &mut egui::Ui, t: Tokens, text: &str) {
    ui.label(egui::RichText::new(text).color(t.text_dim).size(11.5));
}

/// The parameters for `layer.screentone`, `filter.screentone` and `pattern.screentone`: the
/// spec's own serde object (the commands' flat notation) plus the sheet name when the panel's
/// field holds one.
pub fn params(st: &ScreentoneUi) -> Value {
    let mut p = serde_json::to_value(&st.spec).unwrap_or_else(|_| json!({}));
    if let Some(map) = p.as_object_mut() {
        let name = st.name.trim();
        if !name.is_empty() {
            map.insert("name".into(), json!(name));
        } else {
            map.remove("name");
        }
    }
    p
}

/// Restyles the active `LayerContent::Screentone` layer in place when the panel's spec changes,
/// coalescing consecutive panel edits on that layer into one history step.
pub fn sync_spec_command(st: &ScreentoneUi, active: &ScreentoneTargetInfo) -> Option<crate::OutCommand> {
    if st.spec != active.spec {
        let mut p = params(st);
        if let Some(map) = p.as_object_mut() {
            map.insert("layer".into(), json!(active.layer_id.0));
            map.insert("coalesce".into(), json!(format!("screentone-panel:{}:{}", active.doc_id.0, active.layer_id.0)));
        }
        Some(crate::OutCommand::new(CMD_LAYER, p))
    } else {
        None
    }
}

/// Freezes (rasterizes) a live `LayerContent::Screentone` layer into a regular pixel layer.
pub fn freeze_layer_command(layer_id: photocraft_doc::LayerId) -> crate::OutCommand {
    crate::OutCommand::new("layer.rasterize.layer", json!({"layer": layer_id.0}))
}

/// Applies a built-in preset without losing the panel's own mode and tone-curve settings.
fn merge_preset(spec: &mut ScreentoneSpec, preset: &ScreentoneSpec) {
    let prev_mode = spec.mode;
    let prev_bg_alpha = spec.background[3];
    let prev_brightness = spec.brightness;
    let prev_contrast = spec.contrast;
    let prev_white = spec.white_threshold;
    let prev_black = spec.black_threshold;
    let prev_gamma = spec.tone_gamma;
    let prev_lines = spec.preserve_lines;
    let prev_line_thresh = spec.line_threshold;
    let prev_line_thick = spec.line_thickness;
    *spec = preset.clone();
    spec.mode = prev_mode;
    spec.use_source_luminance = prev_mode == ScreentoneMode::Shading;
    spec.background[3] = prev_bg_alpha;
    spec.brightness = prev_brightness;
    spec.contrast = prev_contrast;
    spec.white_threshold = prev_white;
    spec.black_threshold = prev_black;
    spec.tone_gamma = prev_gamma;
    spec.preserve_lines = prev_lines;
    spec.line_threshold = prev_line_thresh;
    spec.line_thickness = prev_line_thick;
}

fn draw_mode_row(ui: &mut egui::Ui, st: &mut ScreentoneUi, t: Tokens) {
    ui.horizontal(|ui| {
        row(ui, t, tl!("Mode:"));
        if ui.selectable_label(st.spec.mode == ScreentoneMode::Shading, tl!("Manga / Shading")).clicked() {
            st.spec.mode = ScreentoneMode::Shading;
            st.spec.use_source_luminance = true;
            st.spec.background[3] = 1.0;
        }
        if ui.selectable_label(st.spec.mode == ScreentoneMode::Sheet, tl!("Sheet / Pattern")).clicked() {
            st.spec.mode = ScreentoneMode::Sheet;
            st.spec.use_source_luminance = false;
            st.spec.background[3] = 0.0;
        }
    });
}

fn draw_engine_row(ui: &mut egui::Ui, st: &mut ScreentoneUi, t: Tokens) {
    ui.horizontal(|ui| {
        row(ui, t, tl!("Engine:"));
        if ui.selectable_label(st.spec.backend == ScreentoneBackend::Auto, tl!("Auto")).on_hover_text(tl!("GPU on large canvases, CPU on small")).clicked() {
            st.spec.backend = ScreentoneBackend::Auto;
        }
        if ui
            .selectable_label(st.spec.backend == ScreentoneBackend::Gpu, tl!("⚡ GPU"))
            .on_hover_text(tl!("Ultra-fast real-time rendering for giant files"))
            .clicked()
        {
            st.spec.backend = ScreentoneBackend::Gpu;
        }
        if ui.selectable_label(st.spec.backend == ScreentoneBackend::Cpu, tl!("◆ CPU")).on_hover_text(tl!("Exact reference CPU engine")).clicked() {
            st.spec.backend = ScreentoneBackend::Cpu;
        }
    });
}

fn draw_core_grid(ui: &mut egui::Ui, st: &mut ScreentoneUi, t: Tokens) {
    egui::Grid::new("screentone-grid").num_columns(2).spacing(egui::vec2(8.0, 4.0)).show(ui, |ui| {
        // The built-in presets seed the whole spec; "Custom" keeps what the panel has.
        row(ui, t, &crate::i18n::param_label("preset"));
        let presets = photocraft_algo::screentone::built_in_presets();
        let mut opts: Vec<(String, &str)> = std::iter::once((String::new(), tl!("Custom"))).collect();
        opts.extend(presets.iter().map(|p| (p.id.to_string(), p.name)));
        let mut cur = st.preset.clone();
        if crate::widgets::dropdown_literal(ui, "screentone-preset", &mut cur, &opts, 176.0) && cur != st.preset {
            st.preset = cur.clone();
            if let Some(p) = presets.iter().find(|p| p.id == cur) {
                merge_preset(&mut st.spec, &p.spec);
            }
        }
        ui.end_row();

        row(ui, t, &crate::i18n::param_label("patternType"));
        let mut cur = id_of(&st.spec.pattern_type);
        let labels: Vec<String> = PATTERNS.iter().map(|p| crate::i18n::source_label(p)).collect();
        let opts: Vec<(String, &str)> = PATTERNS.iter().map(|p| p.to_string()).zip(labels.iter().map(String::as_str)).collect();
        if crate::widgets::dropdown(ui, "screentone-pattern", &mut cur, &opts, 176.0)
            && let Some(p) = from_id(&cur)
        {
            st.spec.pattern_type = p;
        }
        ui.end_row();

        row(ui, t, &crate::i18n::param_label("dotShape"));
        let mut cur = id_of(&st.spec.dot_shape);
        let labels: Vec<String> = SHAPES.iter().map(|s| crate::i18n::source_label(s)).collect();
        let opts: Vec<(String, &str)> = SHAPES.iter().map(|s| s.to_string()).zip(labels.iter().map(String::as_str)).collect();
        if crate::widgets::dropdown(ui, "screentone-shape", &mut cur, &opts, 176.0)
            && let Some(s) = from_id(&cur)
        {
            st.spec.dot_shape = s;
        }
        ui.end_row();

        row(ui, t, tl!("Auto dot size"));
        crate::widgets::checkbox(ui, &mut st.spec.auto_dot_size, tl!("By area")).on_hover_text(tl!("Coverage = brightness × Density; dot size is ignored"));
        ui.end_row();

        row(ui, t, &crate::i18n::param_label("dotSize"));
        if st.spec.auto_dot_size {
            ui.label(tl!("Derived from coverage"));
        } else {
            crate::widgets::value_field(ui, &mut st.spec.dot_size, 0.0..=500.0, "px", 76.0);
        }
        ui.end_row();

        row(ui, t, tl!("Equalize area"));
        if st.spec.auto_dot_size {
            ui.label(tl!("Not used in auto mode"));
        } else {
            crate::widgets::checkbox(ui, &mut st.spec.equalize_area, tl!("Calibrate area by brightness"));
        }
        ui.end_row();

        row(ui, t, tl!("Posterize steps"));
        {
            let mut steps = st.spec.posterize_steps as f32;
            if crate::widgets::value_field(ui, &mut steps, 0.0..=8.0, "", 76.0).changed() {
                st.spec.posterize_steps = steps.round().clamp(0.0, 8.0) as u8;
            }
        }
        ui.end_row();

        row(ui, t, tl!("Scrape / Kezuri"));
        crate::widgets::value_field(ui, &mut st.spec.scrape_strength, 0.0..=1.0, "", 76.0);
        ui.end_row();

        row(ui, t, &crate::i18n::param_label("spacingX"));
        crate::widgets::value_field(ui, &mut st.spec.spacing_x, 1.0..=2000.0, "px", 76.0);
        ui.end_row();

        row(ui, t, &crate::i18n::param_label("spacingY"));
        crate::widgets::value_field(ui, &mut st.spec.spacing_y, 1.0..=2000.0, "px", 76.0);
        ui.end_row();

        row(ui, t, tl!("Row Offset:"));
        crate::widgets::value_field(ui, &mut st.spec.row_offset, -1.0..=1.0, "", 76.0);
        ui.end_row();

        row(ui, t, &crate::i18n::param_label("angle"));
        if crate::widgets::value_field(ui, &mut st.spec.angle, 0.0..=360.0, "°", 76.0).changed() {
            st.spec.angle = st.spec.angle.rem_euclid(360.0);
        }
        ui.end_row();

        row(ui, t, &crate::i18n::param_label("density"));
        crate::widgets::value_field(ui, &mut st.spec.density, 0.0..=1.0, "", 76.0);
        ui.end_row();

        row(ui, t, &crate::i18n::param_label("lineWidth"));
        crate::widgets::value_field(ui, &mut st.spec.line_width, 0.0..=200.0, "px", 76.0);
        ui.end_row();

        row(ui, t, &crate::i18n::param_label("crossAngle"));
        crate::widgets::value_field(ui, &mut st.spec.cross_angle, -180.0..=180.0, "°", 76.0);
        ui.end_row();

        row(ui, t, &crate::i18n::param_label("roundness"));
        crate::widgets::value_field(ui, &mut st.spec.roundness, 0.0..=1.0, "", 76.0);
        ui.end_row();

        row(ui, t, &crate::i18n::param_label("smoothness"));
        crate::widgets::value_field(ui, &mut st.spec.smoothness, 0.0..=1.0, "", 76.0);
        ui.end_row();

        row(ui, t, &crate::i18n::param_label("seed"));
        ui.horizontal(|ui| {
            let mut seed = st.spec.seed as f32;
            if crate::widgets::value_field(ui, &mut seed, 0.0..=30_000.0, "", 76.0).changed() {
                st.spec.seed = seed.round().max(0.0) as u32;
            }
            if ui.button(tl!("Random")).clicked() {
                let entropy = ui.input(|i| i.time).to_bits();
                st.spec.seed = random_seed(entropy, st.spec.seed as i64) as u32;
            }
        });
        ui.end_row();

        row(ui, t, "");
        let mut opaque_paper = st.spec.background[3] >= 0.5;
        if crate::widgets::checkbox(ui, &mut opaque_paper, tl!("Opaque Paper")).changed() {
            st.spec.background[3] = if opaque_paper { 1.0 } else { 0.0 };
        }
        ui.end_row();

        if st.spec.mode == ScreentoneMode::Shading {
            draw_shading_rows(ui, st, t);
        }
    });
}

fn draw_shading_rows(ui: &mut egui::Ui, st: &mut ScreentoneUi, t: Tokens) {
    row(ui, t, tl!("Brightness:"));
    crate::widgets::value_field(ui, &mut st.spec.brightness, -1.0..=1.0, "", 76.0);
    ui.end_row();

    row(ui, t, tl!("Contrast:"));
    crate::widgets::value_field(ui, &mut st.spec.contrast, 0.0..=4.0, "", 76.0);
    ui.end_row();

    row(ui, t, tl!("White Cutoff:"));
    crate::widgets::value_field(ui, &mut st.spec.white_threshold, 0.0..=1.0, "", 76.0);
    ui.end_row();

    row(ui, t, tl!("Black Cutoff:"));
    crate::widgets::value_field(ui, &mut st.spec.black_threshold, 0.0..=1.0, "", 76.0);
    ui.end_row();

    row(ui, t, tl!("Tone Gamma:"));
    crate::widgets::value_field(ui, &mut st.spec.tone_gamma, 0.1..=5.0, "", 76.0);
    ui.end_row();

    row(ui, t, "");
    crate::widgets::checkbox(ui, &mut st.spec.preserve_lines, tl!("Preserve Lines"));
    ui.end_row();

    if st.spec.preserve_lines {
        row(ui, t, tl!("Line Threshold:"));
        crate::widgets::value_field(ui, &mut st.spec.line_threshold, 0.05..=0.65, "", 76.0);
        ui.end_row();

        row(ui, t, tl!("Line Thickness:"));
        crate::widgets::value_field(ui, &mut st.spec.line_thickness, 0.5..=5.0, "px", 76.0);
        ui.end_row();
    }
}

fn draw_satellites(ui: &mut egui::Ui, st: &mut ScreentoneUi, t: Tokens) {
    egui::CollapsingHeader::new(tl!("Satellites")).id_salt("screentone-satellites").default_open(st.spec.satellite_enabled).show(ui, |ui| {
        egui::Grid::new("screentone-satellites-grid").num_columns(2).spacing(egui::vec2(8.0, 4.0)).show(ui, |ui| {
            row(ui, t, "");
            crate::widgets::checkbox(ui, &mut st.spec.satellite_enabled, tl!("Enable"));
            ui.end_row();

            row(ui, t, tl!("Layout:"));
            ui.horizontal(|ui| {
                if ui.selectable_label(st.spec.satellite_layout == SatelliteLayout::Cross, tl!("Cross")).clicked() {
                    st.spec.satellite_layout = SatelliteLayout::Cross;
                    st.spec.satellite_count = st.spec.satellite_count.min(4);
                }
                if ui.selectable_label(st.spec.satellite_layout == SatelliteLayout::Even, tl!("Even")).clicked() {
                    st.spec.satellite_layout = SatelliteLayout::Even;
                }
            });
            ui.end_row();

            row(ui, t, tl!("Count:"));
            let max_count = if st.spec.satellite_layout == SatelliteLayout::Even { 8.0 } else { 4.0 };
            let mut count = st.spec.satellite_count as f32;
            if crate::widgets::value_field(ui, &mut count, 1.0..=max_count, "", 76.0).changed() {
                st.spec.satellite_count = count.round().clamp(1.0, max_count) as u32;
            }
            ui.end_row();

            row(ui, t, tl!("Distance:"));
            crate::widgets::value_field(ui, &mut st.spec.satellite_distance, 0.0..=1000.0, "px", 76.0);
            ui.end_row();

            row(ui, t, tl!("Size:"));
            crate::widgets::value_field(ui, &mut st.spec.satellite_size, 0.0..=500.0, "px", 76.0);
            ui.end_row();

            row(ui, t, tl!("Angle:"));
            crate::widgets::value_field(ui, &mut st.spec.satellite_angle, -180.0..=180.0, "°", 76.0);
            ui.end_row();

            row(ui, t, tl!("Shape:"));
            let mut cur = id_of(&st.spec.satellite_dot_shape);
            let labels: Vec<String> = SHAPES.iter().map(|s| crate::i18n::source_label(s)).collect();
            let opts: Vec<(String, &str)> = SHAPES.iter().map(|s| s.to_string()).zip(labels.iter().map(String::as_str)).collect();
            if crate::widgets::dropdown(ui, "screentone-sat-shape", &mut cur, &opts, 176.0)
                && let Some(s) = from_id(&cur)
            {
                st.spec.satellite_dot_shape = s;
            }
            ui.end_row();

            row(ui, t, tl!("Stretch:"));
            crate::widgets::value_field(ui, &mut st.spec.satellite_stretch, 0.0..=10.0, "", 76.0);
            ui.end_row();
        });
    });
}

fn draw_distortion(ui: &mut egui::Ui, st: &mut ScreentoneUi, t: Tokens) {
    egui::CollapsingHeader::new(tl!("Distortion & Jitter")).id_salt("screentone-distortion").default_open(false).show(ui, |ui| {
        egui::Grid::new("screentone-distortion-grid").num_columns(2).spacing(egui::vec2(8.0, 4.0)).show(ui, |ui| {
            row(ui, t, tl!("Aspect X:"));
            crate::widgets::value_field(ui, &mut st.spec.aspect_x, 0.1..=10.0, "", 76.0);
            ui.end_row();

            row(ui, t, tl!("Aspect Y:"));
            crate::widgets::value_field(ui, &mut st.spec.aspect_y, 0.1..=10.0, "", 76.0);
            ui.end_row();

            row(ui, t, tl!("Merge:"));
            crate::widgets::value_field(ui, &mut st.spec.merge_factor, 0.0..=1.0, "", 76.0);
            ui.end_row();

            row(ui, t, tl!("Jitter Pos:"));
            crate::widgets::value_field(ui, &mut st.spec.jitter_pos, 0.0..=100.0, "%", 76.0);
            ui.end_row();

            row(ui, t, tl!("Jitter Size:"));
            crate::widgets::value_field(ui, &mut st.spec.jitter_size, 0.0..=100.0, "%", 76.0);
            ui.end_row();
        });
    });
}

fn draw_gradient(ui: &mut egui::Ui, st: &mut ScreentoneUi, t: Tokens) {
    egui::CollapsingHeader::new(tl!("Gradient"))
        .id_salt("screentone-gradient")
        .default_open(st.spec.grad_type != photocraft_algo::ScreentoneGradientType::None)
        .show(ui, |ui| {
            egui::Grid::new("screentone-gradient-grid").num_columns(2).spacing(egui::vec2(8.0, 4.0)).show(ui, |ui| {
                row(ui, t, tl!("Type:"));
                let mut cur = id_of(&st.spec.grad_type);
                let labels: Vec<String> = GRADIENTS.iter().map(|g| crate::i18n::source_label(g)).collect();
                let opts: Vec<(String, &str)> = GRADIENTS.iter().map(|g| g.to_string()).zip(labels.iter().map(String::as_str)).collect();
                if crate::widgets::dropdown(ui, "screentone-grad-type", &mut cur, &opts, 176.0)
                    && let Some(g) = from_id(&cur)
                {
                    st.spec.grad_type = g;
                }
                ui.end_row();

                row(ui, t, tl!("Angle:"));
                crate::widgets::value_field(ui, &mut st.spec.grad_angle, -180.0..=180.0, "°", 76.0);
                ui.end_row();

                row(ui, t, "");
                crate::widgets::checkbox(ui, &mut st.spec.grad_reverse, tl!("Reverse"));
                ui.end_row();

                row(ui, t, tl!("Midpoint:"));
                crate::widgets::value_field(ui, &mut st.spec.grad_midpoint, 0.0..=1.0, "", 76.0);
                ui.end_row();

                row(ui, t, tl!("Start Size:"));
                crate::widgets::value_field(ui, &mut st.spec.grad_size_start, 0.0..=10.0, "", 76.0);
                ui.end_row();

                row(ui, t, tl!("End Size:"));
                crate::widgets::value_field(ui, &mut st.spec.grad_size_end, 0.0..=10.0, "", 76.0);
                ui.end_row();
            });
        });
}

/// What the action buttons asked for in this frame.
struct ActionButtons {
    action: Option<&'static str>,
    freeze_clicked: bool,
}

fn draw_action_buttons(ui: &mut egui::Ui, st: &mut ScreentoneUi, can_freeze: bool) -> ActionButtons {
    ui.add_space(6.0);
    crate::widgets::hairline(ui);
    ui.add_space(4.0);
    let mut action = None;
    let mut freeze_clicked = false;
    ui.horizontal(|ui| {
        if ui.button(tl!("New Layer")).clicked() {
            action = Some(CMD_LAYER);
        }
        if ui.button(tl!("Apply")).clicked() {
            action = Some(CMD_FILTER);
        }
        if ui
            .add_enabled(can_freeze, egui::Button::new(tl!("Freeze Layer")))
            .on_hover_text(tl!("Bake the live screentone layer into standard pixels while keeping its mask"))
            .clicked()
        {
            freeze_clicked = true;
        }
    });
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        if ui.button(tl!("Save as Pattern")).clicked() {
            action = Some(CMD_PATTERN);
        }
        ui.add(egui::TextEdit::singleline(&mut st.name).hint_text(tl!("Pattern Name")).desired_width(116.0));
    });
    ActionButtons { action, freeze_clicked }
}

/// The command an action button dispatches; the same payload a preset or filter dialog sends.
fn action_command(action: &'static str, st: &ScreentoneUi) -> crate::OutCommand {
    crate::OutCommand::new(action, params(st))
}

/// Draws the tab body; the dock's own scrolling wraps it in both the native and shared docks.
///
/// No document is touched here: the host receives the returned commands and drives the gizmo
/// according to `detach_gizmo` / `expect_new_layer_bind`.
pub fn panel(ui: &mut egui::Ui, st: &mut ScreentoneUi, active_tone: Option<&ScreentoneTargetInfo>) -> ScreentonePanelOutput {
    // Headings here are labels, not selectable prose; TextEdit keeps its own selection.
    ui.style_mut().interaction.selectable_labels = false;
    let mut out = ScreentonePanelOutput { commands: Vec::new(), detach_gizmo: false, expect_new_layer_bind: false };
    // The dock does not always wrap a manga panel in a scroll area, so own one here: the tone
    // controls are taller than most docks and must always remain reachable.
    egui::ScrollArea::vertical().id_salt("comic_screentone_scroll").auto_shrink([false, false]).show(ui, |ui| {
        let t = Tokens::get(ui.ctx());
        draw_mode_row(ui, st, t);
        draw_engine_row(ui, st, t);
        ui.add_space(4.0);
        draw_core_grid(ui, st, t);
        ui.add_space(4.0);
        draw_satellites(ui, st, t);
        draw_distortion(ui, st, t);
        draw_gradient(ui, st, t);
        let buttons = draw_action_buttons(ui, st, active_tone.is_some());
        if buttons.freeze_clicked {
            if let Some(active) = active_tone {
                out.commands.push(freeze_layer_command(active.layer_id));
                out.detach_gizmo = true;
            }
        } else if let Some(action) = buttons.action {
            // A New Layer run binds the gizmo to the generated sheet; drags restyle that layer.
            out.expect_new_layer_bind = action == CMD_LAYER;
            out.commands.push(action_command(action, st));
        } else if (st.spec.backend != photocraft_algo::ScreentoneBackend::Cpu || !ui.input(|i| i.pointer.any_down()))
            && let Some(active) = active_tone
            && let Some(cmd) = sync_spec_command(st, active)
        {
            // A GPU/Auto restyle carries a coalesce key and is cheap, so it follows the slider
            // live; only the full-canvas CPU bake waits for the drag to end.
            out.commands.push(cmd);
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::{
        CMD_FILTER, CMD_LAYER, CMD_PATTERN, ScreentoneTargetInfo, ScreentoneUi, action_command, freeze_layer_command, from_id, id_of, panel, params,
        sync_spec_command,
    };
    use photocraft_algo::{DotShape, PatternType, ScreentoneGradientType};
    use serde_json::json;

    fn target(st: &ScreentoneUi) -> ScreentoneTargetInfo {
        ScreentoneTargetInfo { doc_id: crate::BridgeDocId(7), layer_id: photocraft_doc::LayerId(3), spec: st.spec.clone() }
    }

    #[test]
    fn default_state_matches_the_panel_preset() {
        let st = ScreentoneUi::default();
        assert_eq!(st.spec.mode, photocraft_algo::ScreentoneMode::Shading);
        assert!(st.spec.use_source_luminance);
        assert_eq!(st.spec.dot_size, 4.5);
        assert_eq!(st.spec.spacing_x, 8.0);
        assert_eq!(st.spec.spacing_y, 8.0);
        assert_eq!(st.spec.density, 0.75);
        assert_eq!(st.spec.background, [1.0, 1.0, 1.0, 1.0]);
        assert!(st.spec.preserve_lines);
        assert_eq!(st.spec.line_threshold, 0.24);
        assert_eq!(st.preset, "");
        assert_eq!(st.name, "");
    }

    #[test]
    fn params_use_the_commands_notation() {
        let mut st = ScreentoneUi::default();
        st.spec.dot_size = 9.0;
        st.spec.pattern_type = PatternType::Crosshatch;
        st.name = " Tone ".into();
        let p = params(&st);
        assert_eq!(p["dotSize"], json!(9.0));
        assert_eq!(p["patternType"], json!("crosshatch"));
        assert_eq!(p["dotShape"], json!("circle"));
        assert_eq!(p["name"], json!("Tone"));
        // No name field: the command falls back to the preset's name.
        st.name = "  ".into();
        assert!(params(&st).get("name").is_none());
        // A preset replaces the whole spec, so the params follow it.
        let preset = photocraft_algo::screentone::built_in_presets().into_iter().next().unwrap();
        st.spec = preset.spec.clone();
        assert_eq!(params(&st)["dotSize"], json!(preset.spec.dot_size));
        // Every id the panel offers round-trips through the commands' notation.
        for id in super::PATTERNS {
            assert_eq!(id_of(&from_id::<PatternType>(id).unwrap()), id);
        }
        for id in super::SHAPES {
            assert_eq!(id_of(&from_id::<DotShape>(id).unwrap()), id);
        }
        for id in super::GRADIENTS {
            assert_eq!(id_of(&from_id::<ScreentoneGradientType>(id).unwrap()), id);
        }
        assert!(from_id::<PatternType>("spiral").is_none());
    }

    #[test]
    fn sync_spec_command_targets_the_active_layer() {
        let mut st = ScreentoneUi::default();
        let active = target(&st);
        assert!(sync_spec_command(&st, &active).is_none(), "an unchanged spec sends nothing");
        st.spec.dot_size = 12.0;
        let cmd = sync_spec_command(&st, &active).expect("a changed spec syncs");
        assert_eq!(cmd.id, CMD_LAYER);
        assert_eq!(cmd.params["coalesce"], "screentone-panel:7:3");
        assert_eq!(cmd.params["layer"], 3);
    }

    #[test]
    fn freeze_layer_command_rasterizes_the_named_layer() {
        let cmd = freeze_layer_command(photocraft_doc::LayerId(42));
        assert_eq!(cmd.id, "layer.rasterize.layer");
        assert_eq!(cmd.params["layer"], 42);
    }

    #[test]
    fn panel_button_commands_use_the_command_notation() {
        let st = ScreentoneUi { name: "Tone".into(), ..Default::default() };
        for id in [CMD_LAYER, CMD_FILTER, CMD_PATTERN] {
            let cmd = action_command(id, &st);
            assert_eq!(cmd.id, id);
            assert_eq!(cmd.params["name"], "Tone");
        }
    }

    #[test]
    fn panel_draws_with_or_without_an_active_layer() {
        let mut st = ScreentoneUi::default();
        let active = target(&st);
        let ctx = egui::Context::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, 900.0))), ..Default::default() };
        let mut no_doc = None;
        let mut out = ctx.run_ui(input.clone(), |ui| {
            no_doc = Some(egui::CentralPanel::default().show(ui, |ui| panel(ui, &mut st, None)).inner);
        });
        out.textures_delta.clear();
        let no_doc = no_doc.unwrap();
        assert!(no_doc.commands.is_empty());
        assert!(!no_doc.detach_gizmo);
        assert!(!no_doc.expect_new_layer_bind);
        let mut with_doc = None;
        let mut out = ctx.run_ui(input, |ui| {
            with_doc = Some(egui::CentralPanel::default().show(ui, |ui| panel(ui, &mut st, Some(&active))).inner);
        });
        out.textures_delta.clear();
        let with_doc = with_doc.unwrap();
        assert!(!with_doc.detach_gizmo);
    }
}
