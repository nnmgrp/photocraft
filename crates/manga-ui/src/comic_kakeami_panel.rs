//! Starter Kakeami / Nawa-ami procedural screentone panel (`kakeami-core`).
//!
//! Emits [`OutCommand`](crate::OutCommand)s (`layer.kakeami.create`, `layer.kakeami.update`,
//! `layer.rasterize.layer`) without touching engine internals directly.

use photocraft_doc::{KakeamiConfig, KakeamiLayer, LayerId};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::BridgeDocId;
use crate::theme::Tokens;

pub const CMD_CREATE: &str = "layer.kakeami.create";
pub const CMD_UPDATE: &str = "layer.kakeami.update";

/// Built-in Phase 2A starter presets (6 curated Kakeami v8 / Nawa-ami / Shibufude Pencil profiles).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KakeamiPreset {
    pub id: &'static str,
    pub label: &'static str,
}

pub const PRESETS: [KakeamiPreset; 6] = [
    KakeamiPreset { id: "kake-v8-closed-4l", label: "Kakeami v8 · Closed 4L" },
    KakeamiPreset { id: "kake-v8-open-bokeh", label: "Kakeami v8 · Open Bokeh" },
    KakeamiPreset { id: "nawa-60l-normal", label: "Nawa-ami · 60L Normal" },
    KakeamiPreset { id: "nawa-dense-white", label: "Nawa-ami · Dense White Swarm (濃白)" },
    KakeamiPreset { id: "nawa-pale-white", label: "Nawa-ami · Pale White Swarm (淡白)" },
    KakeamiPreset { id: "shibufude-v3-pencil", label: "Shibufude v3 · Pencil Rough" },
];

/// Build the canonical [`KakeamiConfig`] for one of the 6 built-in starter presets.
pub fn preset_config(id: &str) -> KakeamiConfig {
    let mut c = KakeamiConfig::default();
    match id {
        "kake-v8-closed-4l" => {
            c.pattern_family = "kakeami-v8".into();
            c.finish_mode = "sugomi".into();
            c.preset = "linear-bt".into();
            c.kake_patch_style = "closed-4l".into();
            c.kake_occlude_mode = "card".into();
            c.kake_fade_mode = "stipple".into();
            c.kake_fade_threshold = 0.36;
            c.darken_cap = 75;
            c.one_bit_mode = true;
        }
        "kake-v8-open-bokeh" => {
            c.pattern_family = "kakeami-v8".into();
            c.finish_mode = "clean".into();
            c.preset = "bokeh".into();
            c.kake_patch_style = "open".into();
            c.kake_occlude_mode = "transparent".into();
            c.kake_fade_mode = "gaussian".into();
            c.kake_fade_threshold = 0.42;
            c.darken_cap = 82;
            c.angle_randomness = 65;
            c.one_bit_mode = true;
        }
        "nawa-60l-normal" => {
            c.pattern_family = "nawa-ami".into();
            c.finish_mode = "clean".into();
            c.preset = "linear-bt".into();
            c.nawa_tone_style = "normal".into();
            c.nawa_lpi_preset = "60L".into();
            c.nawa_join_style = "smooth-60l".into();
            c.nawa_coil_style = "skein".into();
            c.nawa_rope_width = 42.0;
            c.nawa_coil_radius = 62.0;
            c.nawa_strands = 6;
            c.nawa_outline_pad = 0.0;
            c.darken_cap = 55;
            c.wobble = 6;
            c.broken_ink = 0;
            c.ink_pooling = 0.0;
            c.seamless = true;
            c.one_bit_mode = true;
        }
        "nawa-dense-white" => {
            c.pattern_family = "nawa-ami".into();
            c.finish_mode = "clean".into();
            c.preset = "linear-bt".into();
            c.nawa_tone_style = "denseWhite".into();
            c.nawa_lpi_preset = "60L".into();
            c.nawa_join_style = "smooth-60l".into();
            c.nawa_coil_style = "skein".into();
            c.nawa_rope_width = 42.0;
            c.nawa_coil_radius = 62.0;
            c.nawa_strands = 6;
            c.nawa_outline_pad = 0.0;
            c.darken_cap = 55;
            c.wobble = 8;
            c.broken_ink = 0;
            c.ink_pooling = 0.0;
            c.seamless = true;
            c.one_bit_mode = true;
        }
        "nawa-pale-white" => {
            c.pattern_family = "nawa-ami".into();
            c.finish_mode = "clean".into();
            c.preset = "linear-bt".into();
            c.nawa_tone_style = "paleWhite".into();
            c.nawa_lpi_preset = "60L".into();
            c.nawa_join_style = "smooth-60l".into();
            c.nawa_coil_style = "skein".into();
            c.nawa_rope_width = 38.0;
            c.nawa_coil_radius = 58.0;
            c.nawa_strands = 5;
            c.nawa_outline_pad = 0.0;
            c.darken_cap = 42;
            c.wobble = 8;
            c.broken_ink = 0;
            c.ink_pooling = 0.0;
            c.seamless = true;
            c.one_bit_mode = true;
        }
        "shibufude-v3-pencil" => {
            c.pattern_family = "kakeami-v8".into();
            c.finish_mode = "pencil".into();
            c.preset = "linear-bt".into();
            c.kake_patch_style = "open".into();
            c.kake_occlude_mode = "transparent".into();
            c.kake_fade_mode = "gaussian".into();
            c.kake_fade_threshold = 0.40;
            c.stroke_width = 1.1;
            c.darken_cap = 95;
            c.overlap_density = 90;
            c.angle_randomness = 75;
            c.wobble = 16;
            c.broken_ink = 0;
            c.ink_pooling = 0.0;
            c.one_bit_mode = true;
        }
        _ => {}
    }
    c
}

/// Active panel sub-tab inside the Screentones dock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToneDockTab {
    #[default]
    Screentones,
    Kakeami,
}

/// Persistent UI state for the starter Kakeami / Nawa-ami panel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KakeamiPanelUi {
    pub active_tab: ToneDockTab,
    pub preset_id: String,
    pub config: KakeamiConfig,
    pub color: [f32; 4],
    pub dpi_sync: bool,
    pub clipped: bool,
    pub name: String,
    #[serde(skip)]
    pub synced_layer: Option<(BridgeDocId, LayerId)>,
}

impl Default for KakeamiPanelUi {
    fn default() -> Self {
        Self {
            active_tab: ToneDockTab::Screentones,
            preset_id: PRESETS[0].id.to_string(),
            config: preset_config(PRESETS[0].id),
            color: [0.0, 0.0, 0.0, 1.0],
            dpi_sync: true,
            clipped: false,
            name: String::new(),
            synced_layer: None,
        }
    }
}

/// Live `LayerContent::Kakeami` layer currently selected in the host document.
pub struct KakeamiTargetInfo {
    pub doc_id: BridgeDocId,
    pub layer_id: LayerId,
    pub layer: KakeamiLayer,
    pub clipped: bool,
}

fn row_label(ui: &mut egui::Ui, t: Tokens, text: &str) {
    ui.label(egui::RichText::new(text).color(t.text_dim).size(11.5));
}

fn combo_str(ui: &mut egui::Ui, id: &str, value: &mut String, options: &[(&str, &str)]) -> bool {
    let mut changed = false;
    let current = options.iter().find(|(k, _)| *k == value.as_str()).map_or(value.as_str(), |(_, l)| *l);
    egui::ComboBox::from_id_salt(id).selected_text(current).width(ui.available_width()).show_ui(ui, |ui| {
        for &(k, label) in options {
            if ui.selectable_label(value.as_str() == k, label).clicked() && value.as_str() != k {
                *value = k.to_string();
                changed = true;
            }
        }
    });
    changed
}

/// Synchronises the panel draft when the user selects a different live Kakeami layer.
pub fn sync_from_target(st: &mut KakeamiPanelUi, target: Option<&KakeamiTargetInfo>) {
    match target {
        Some(info) => {
            let key = (info.doc_id, info.layer_id);
            if st.synced_layer != Some(key) {
                st.config = info.layer.config.clone();
                st.color = info.layer.color;
                st.dpi_sync = info.layer.dpi_sync;
                st.clipped = info.clipped;
                st.synced_layer = Some(key);
            }
        }
        None => {
            st.synced_layer = None;
        }
    }
}

/// Renders the starter Kakeami / Nawa-ami panel and returns commands to dispatch.
pub fn panel(ui: &mut egui::Ui, st: &mut KakeamiPanelUi, target: Option<&KakeamiTargetInfo>) -> Vec<crate::OutCommand> {
    let t = Tokens::get(ui.ctx());
    let mut cmds = Vec::new();
    let mut edited = false;

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;

        // 1. Starter Presets (6 curated presets)
        ui.label(egui::RichText::new("Presets").strong().size(12.0));
        egui::Grid::new("kakeami_presets_grid").num_columns(2).spacing([6.0, 5.0]).show(ui, |ui| {
            let btn_w = ((ui.available_width() - 6.0) * 0.5).max(80.0);
            for (idx, pr) in PRESETS.iter().enumerate() {
                let selected = st.preset_id == pr.id;
                let btn = egui::Button::new(egui::RichText::new(pr.label).size(11.0))
                    .selected(selected)
                    .min_size(egui::vec2(btn_w, 24.0));
                if ui.add(btn).clicked() {
                    st.preset_id = pr.id.to_string();
                    st.config = preset_config(pr.id);
                    edited = true;
                }
                if idx % 2 == 1 {
                    ui.end_row();
                }
            }
        });

        ui.separator();

        // 2. Core parameters
        egui::Grid::new("kakeami_params_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            row_label(ui, t, "Family");
            edited |= combo_str(
                ui,
                "kake_family",
                &mut st.config.pattern_family,
                &[
                    ("kakeami-v8", "Kakeami v8"),
                    ("nawa-ami", "Nawa-ami"),
                    ("kakeami-4", "Kakeami 4L"),
                    ("kakeami-cluster", "Kakeami Cluster"),
                ],
            );
            ui.end_row();

            row_label(ui, t, "Finish");
            edited |= combo_str(
                ui,
                "kake_finish",
                &mut st.config.finish_mode,
                &[
                    ("clean", "Clean"),
                    ("sugomi", "Sugomi Grain"),
                    ("pencil", "Shibufude v3 Pencil"),
                    ("macroDot", "Macro Stipple"),
                ],
            );
            ui.end_row();

            row_label(ui, t, "Density Map");
            edited |= combo_str(
                ui,
                "kake_density_preset",
                &mut st.config.preset,
                &[
                    ("linear-bt", "Linear Bottom→Top"),
                    ("linear-tb", "Linear Top→Bottom"),
                    ("radial", "Radial"),
                    ("vignette", "Vignette"),
                    ("diagonal", "Diagonal"),
                    ("bokeh", "Bokeh"),
                    ("ramp-9", "9-Step Ramp"),
                ],
            );
            ui.end_row();

            if st.config.pattern_family == "nawa-ami" {
                row_label(ui, t, "Nawa Style");
                edited |= combo_str(
                    ui,
                    "kake_nawa_style",
                    &mut st.config.nawa_tone_style,
                    &[
                        ("normal", "Normal (60L)"),
                        ("denseWhite", "Dense White (濃白)"),
                        ("paleWhite", "Pale White (淡白)"),
                    ],
                );
                ui.end_row();
            } else if st.config.pattern_family == "kakeami-v8" {
                row_label(ui, t, "Patch Style");
                edited |= combo_str(
                    ui,
                    "kake_patch_style",
                    &mut st.config.kake_patch_style,
                    &[
                        ("closed-4l", "Closed 4L"),
                        ("closed-2l", "Closed 2L"),
                        ("open", "Open"),
                        ("continuous", "Continuous"),
                    ],
                );
                ui.end_row();

                row_label(ui, t, "Fade Mode");
                edited |= combo_str(
                    ui,
                    "kake_fade_mode",
                    &mut st.config.kake_fade_mode,
                    &[("stipple", "Stipple"), ("gaussian", "Gaussian"), ("layers", "Layers")],
                );
                ui.end_row();
            }

            row_label(ui, t, "Density Cap");
            let mut cap = st.config.darken_cap;
            if ui.add(egui::Slider::new(&mut cap, 5..=100).suffix("%")).changed() {
                st.config.darken_cap = cap;
                edited = true;
            }
            ui.end_row();

            row_label(ui, t, "Scale");
            let mut scale = st.config.nawa_scale;
            if ui.add(egui::Slider::new(&mut scale, 0.4..=4.0).fixed_decimals(2)).changed() {
                st.config.nawa_scale = scale;
                edited = true;
            }
            ui.end_row();

            row_label(ui, t, "Angle");
            let mut angle = st.config.base_angle;
            if ui.add(egui::Slider::new(&mut angle, -90.0..=90.0).suffix("°").fixed_decimals(1)).changed() {
                st.config.base_angle = angle;
                edited = true;
            }
            ui.end_row();

            row_label(ui, t, "Stroke Width");
            let mut sw = st.config.stroke_width;
            if ui.add(egui::Slider::new(&mut sw, 0.4..=4.0).fixed_decimals(2)).changed() {
                st.config.stroke_width = sw;
                edited = true;
            }
            ui.end_row();

            row_label(ui, t, "Seed");
            ui.horizontal(|ui| {
                let mut seed = st.config.seed;
                if ui.add(egui::DragValue::new(&mut seed).speed(1)).changed() {
                    st.config.seed = seed;
                    edited = true;
                }
                if ui.button("Dice").clicked() {
                    st.config.seed = st.config.seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223) % 999_999;
                    edited = true;
                }
            });
            ui.end_row();

            row_label(ui, t, "Ink Color");
            if ui.color_edit_button_rgba_unmultiplied(&mut st.color).changed() {
                edited = true;
            }
            ui.end_row();
        });

        ui.horizontal_wrapped(|ui| {
            let mut pencil = st.config.finish_mode == "pencil";
            if ui.checkbox(&mut pencil, "Shibufude Pencil").changed() {
                st.config.finish_mode = if pencil { "pencil".into() } else { "clean".into() };
                edited = true;
            }
            if ui.checkbox(&mut st.config.one_bit_mode, "1-bit B&W").changed() {
                edited = true;
            }
            if ui.checkbox(&mut st.dpi_sync, "DPI Sync").changed() {
                edited = true;
            }
            if ui.checkbox(&mut st.clipped, "Clip").changed() {
                edited = true;
            }
        });

        ui.separator();

        // 3. Actions
        ui.horizontal(|ui| {
            let create_label = if target.is_some() { "New Kakeami Layer" } else { "Create Kakeami Layer" };
            if ui.button(egui::RichText::new(create_label).strong()).clicked() {
                let name = if st.name.trim().is_empty() {
                    PRESETS
                        .iter()
                        .find(|p| p.id == st.preset_id)
                        .map_or("Kakeami", |p| p.label)
                        .to_string()
                } else {
                    st.name.trim().to_string()
                };
                cmds.push(crate::OutCommand::new(
                    CMD_CREATE,
                    json!({
                        "name": name,
                        "config": st.config,
                        "color": st.color,
                        "dpiSync": st.dpi_sync,
                        "clipped": st.clipped,
                    }),
                ));
            }
            if let Some(info) = target {
                if ui.button("Rasterize").clicked() {
                    cmds.push(crate::OutCommand::new("layer.rasterize.layer", json!({"layer": info.layer_id.0})));
                }
            }
        });

        if edited
            && let Some(info) = target
        {
            cmds.push(crate::OutCommand::new(
                CMD_UPDATE,
                json!({
                    "layer": info.layer_id.0,
                    "config": st.config,
                    "color": st.color,
                    "dpiSync": st.dpi_sync,
                    "clipped": st.clipped,
                    "coalesce": format!("kakeami-panel:{}:{}", info.doc_id.0, info.layer_id.0),
                }),
            ));
        }
    });

    cmds
}
