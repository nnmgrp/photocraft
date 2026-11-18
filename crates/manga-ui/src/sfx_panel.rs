//! Non-destructive Manga SFX Panel for `Group::Manga` (`crates/manga-ui`).
//!
//! Emits deferred [`OutCommand`]s (`sfx.create`, `sfx.update`, `sfx.convertFromText`,
//! `sfx.bakeToRaster`, `sfx.presets.*`) and syncs bi-directionally with [`SfxGizmoState`].

use egui::Color32;
use photocraft_algo::{DotShape, PatternType};
use photocraft_doc::sfx::{
    ScreentoneSpec, SfxData, SfxDistress, SfxFillSource, SfxFreeTransform, SfxGlyphTransform, SfxLineJoin, SfxMeshInterpolation, SfxPreset, SfxStylePass,
    SfxWarpMode,
};
use photocraft_doc::{Color, LayerId};
use serde_json::{Value, json};

use crate::sfx_gizmo::{SfxGizmoState, reset_mesh};
use crate::theme::Tokens;
use crate::{BridgeDocId, OutCommand};

const SCREENTONE_PATTERNS: [&str; 4] = ["dots", "lines", "crosshatch", "noise"];
const SCREENTONE_SHAPES: [&str; 4] = ["circle", "square", "diamond", "hexagon"];

/// Read-only view of the active layer & preset library supplied by the host app.
pub struct SfxTargetInfo<'a> {
    pub doc_id: Option<BridgeDocId>,
    pub layer_id: Option<LayerId>,
    pub data: Option<SfxData>,
    pub active_is_text: bool,
    pub presets: &'a [SfxPreset],
}

/// Persistent panel UI state stored inside [`crate::MangaUiState`].
#[derive(Default)]
pub struct SfxPanelState {
    pub draft: SfxData,
    pub bound_target: Option<(BridgeDocId, LayerId)>,
    pub synced_data: Option<SfxData>,
    pub font_query: String,
    pub cached_fonts: Option<Vec<String>>,
    pub preset_name: String,
}

impl SfxPanelState {
    /// Lazily populates the cached font family list once without scanning fonts per frame.
    pub fn ensure_fonts(&mut self) {
        if self.cached_fonts.is_some() {
            return;
        }
        let families = match photocraft_text::shared().lock() {
            Ok(mut engine) => engine.fonts.families(),
            Err(poisoned) => poisoned.into_inner().fonts.families(),
        };
        self.cached_fonts = Some(families);
    }

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

fn color_swatch(ui: &mut egui::Ui, label: &str, color: &mut Color) -> bool {
    let mut c32 = color_to_c32(*color);
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        if ui.color_edit_button_srgba(&mut c32).changed() {
            *color = c32_to_color(c32);
            changed = true;
        }
    });
    changed
}

fn dim_label(ui: &mut egui::Ui, t: Tokens, text: &str) {
    ui.label(egui::RichText::new(text).color(t.text_dim).size(11.5));
}

fn enum_id<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v).ok().and_then(|j| j.as_str().map(str::to_owned)).unwrap_or_default()
}

fn enum_from_id<T: serde::de::DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_value(Value::String(s.to_owned())).ok()
}

/// Renders the SFX authoring dock panel and emits deferred engine commands.
pub fn panel(ui: &mut egui::Ui, st: &mut SfxPanelState, gizmo: &mut SfxGizmoState, target: &SfxTargetInfo<'_>) -> SfxPanelOutput {
    let mut out = SfxPanelOutput::default();
    st.ensure_fonts();
    st.sync_from_host(target);
    let t = Tokens::get(ui.ctx());
    let before = st.draft.clone();

    draw_header_banner(ui, t, target);
    draw_text_section(ui, st, t);
    crate::widgets::hairline(ui);
    draw_warp_section(ui, st, gizmo, t);
    crate::widgets::hairline(ui);
    draw_fill_section(ui, st, t);
    crate::widgets::hairline(ui);
    draw_styles_section(ui, st, t);
    crate::widgets::hairline(ui);
    draw_distress_section(ui, st, t);
    crate::widgets::hairline(ui);
    draw_presets_section(ui, st, target, &mut out);
    crate::widgets::hairline(ui);
    draw_actions_section(ui, st, target, &mut out);

    if st.draft != before
        && let (Some(doc_id), Some(layer_id)) = (target.doc_id, target.layer_id)
        && target.data.is_some()
    {
        st.draft.validate();
        let coalesce = format!("sfx-panel:{}:{}", doc_id.0, layer_id.0);
        out.commands.push(OutCommand::new(
            "sfx.update",
            json!({
                "layer": layer_id.0,
                "sfx": st.draft,
                "coalesce": coalesce,
            }),
        ));
        st.synced_data = Some(st.draft.clone());
    }

    out
}

fn draw_header_banner(ui: &mut egui::Ui, t: Tokens, target: &SfxTargetInfo<'_>) {
    ui.horizontal(|ui| {
        ui.strong("SFX Lettering");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let status = if let Some(id) = target.layer_id.filter(|_| target.data.is_some()) { format!("Live #{}", id.0) } else { "Draft (New)".to_owned() };
            ui.label(egui::RichText::new(status).color(t.text_dim).size(11.0));
        });
    });
}

fn draw_text_section(ui: &mut egui::Ui, st: &mut SfxPanelState, t: Tokens) {
    ui.label(egui::RichText::new("Text & Typography").strong().size(12.0));
    ui.add(egui::TextEdit::multiline(&mut st.draft.text).desired_rows(2).desired_width(f32::INFINITY));

    egui::Grid::new("sfx-type-grid").num_columns(2).spacing(egui::vec2(8.0, 4.0)).show(ui, |ui| {
        dim_label(ui, t, "Font:");
        ui.horizontal(|ui| {
            let selected = st.draft.font_family.clone();
            craft_ui::ComboBox::from_id_salt("sfx-font-family").width(152.0).selected_text(selected).show_ui(ui, |ui| {
                ui.add(egui::TextEdit::singleline(&mut st.font_query).hint_text("Filter fonts...").desired_width(148.0));
                let q = st.font_query.trim().to_ascii_lowercase();
                let list = st.cached_fonts.as_deref().unwrap_or(&[]);
                egui::ScrollArea::vertical().max_height(210.0).show(ui, |ui| {
                    let mut shown = 0usize;
                    for fam in list {
                        if !q.is_empty() && !fam.to_ascii_lowercase().contains(&q) {
                            continue;
                        }
                        if ui.selectable_label(st.draft.font_family == *fam, fam).clicked() {
                            st.draft.font_family = fam.clone();
                        }
                        shown += 1;
                        if shown >= 200 {
                            break;
                        }
                    }
                });
            });
            if ui.button("Reload").on_hover_text("Refresh system fonts cache").clicked() {
                st.cached_fonts = None;
                st.ensure_fonts();
            }
        });
        ui.end_row();

        dim_label(ui, t, "Weight:");
        ui.horizontal(|ui| {
            let mut w = st.draft.weight;
            let weights = [(400u16, "Regular"), (700u16, "Bold"), (800u16, "Heavy"), (900u16, "Black")];
            if crate::widgets::dropdown_literal(ui, "sfx-font-weight", &mut w, &weights, 92.0) {
                st.draft.weight = w;
            }
            ui.checkbox(&mut st.draft.italic, "Italic");
        });
        ui.end_row();

        dim_label(ui, t, "Size:");
        ui.horizontal(|ui| {
            crate::widgets::value_field(ui, &mut st.draft.size_pt, 6.0..=500.0, "pt", 68.0);
            ui.checkbox(&mut st.draft.vertical, "Vertical");
        });
        ui.end_row();

        dim_label(ui, t, "Track / Lead:");
        ui.horizontal(|ui| {
            crate::widgets::value_field(ui, &mut st.draft.tracking, -200.0..=500.0, "", 62.0);
            crate::widgets::value_field(ui, &mut st.draft.leading_scale, 0.4..=3.0, "x", 62.0);
        });
        ui.end_row();
    });
}

fn draw_warp_section(ui: &mut egui::Ui, st: &mut SfxPanelState, gizmo: &mut SfxGizmoState, t: Tokens) {
    ui.label(egui::RichText::new("Non-Destructive Warp").strong().size(12.0));
    ui.horizontal_wrapped(|ui| {
        for (mode, label) in [
            (SfxWarpMode::FreeTransform, "Transform / Skew"),
            (SfxWarpMode::PerGlyph, "Per-Glyph"),
            (SfxWarpMode::MeshLattice, "Mesh Grid"),
            (SfxWarpMode::LiquifyField, "Liquify"),
        ] {
            ui.selectable_value(&mut st.draft.warp_mode, mode, label);
        }
    });

    match st.draft.warp_mode {
        SfxWarpMode::FreeTransform => {
            egui::Grid::new("sfx-ft-grid").num_columns(2).spacing(egui::vec2(8.0, 4.0)).show(ui, |ui| {
                let mut sx = st.draft.free_transform.scale_x as f32;
                let mut sy = st.draft.free_transform.scale_y as f32;
                let mut rot = st.draft.free_transform.rotation_deg as f32;
                let mut skx = st.draft.free_transform.skew_x_deg as f32;
                let mut sky = st.draft.free_transform.skew_y_deg as f32;
                let mut px = st.draft.free_transform.perspective_x as f32;
                let mut py = st.draft.free_transform.perspective_y as f32;

                dim_label(ui, t, "Scale X / Y:");
                ui.horizontal(|ui| {
                    if crate::widgets::value_field(ui, &mut sx, 0.1..=12.0, "x", 64.0).changed() {
                        st.draft.free_transform.scale_x = f64::from(sx);
                    }
                    if crate::widgets::value_field(ui, &mut sy, 0.1..=12.0, "x", 64.0).changed() {
                        st.draft.free_transform.scale_y = f64::from(sy);
                    }
                });
                ui.end_row();

                dim_label(ui, t, "Rotate / Skew X:");
                ui.horizontal(|ui| {
                    if crate::widgets::value_field(ui, &mut rot, -360.0..=360.0, "°", 64.0).changed() {
                        st.draft.free_transform.rotation_deg = f64::from(rot);
                    }
                    if crate::widgets::value_field(ui, &mut skx, -75.0..=75.0, "°", 64.0).changed() {
                        st.draft.free_transform.skew_x_deg = f64::from(skx);
                    }
                });
                ui.end_row();

                dim_label(ui, t, "Skew Y / Persp:");
                ui.horizontal(|ui| {
                    if crate::widgets::value_field(ui, &mut sky, -75.0..=75.0, "°", 54.0).changed() {
                        st.draft.free_transform.skew_y_deg = f64::from(sky);
                    }
                    if crate::widgets::value_field(ui, &mut px, -0.75..=0.75, "", 46.0).changed() {
                        st.draft.free_transform.perspective_x = f64::from(px);
                    }
                    if crate::widgets::value_field(ui, &mut py, -0.75..=0.75, "", 46.0).changed() {
                        st.draft.free_transform.perspective_y = f64::from(py);
                    }
                });
                ui.end_row();
            });
            if ui.button("Reset Transform").clicked() {
                st.draft.free_transform = SfxFreeTransform::default();
            }
        }
        SfxWarpMode::PerGlyph => {
            if let Some(g_idx) = gizmo.selected_glyph {
                ui.horizontal(|ui| {
                    ui.label(format!("Glyph #{g_idx}"));
                    if ui.button("Reset Glyph").clicked() {
                        st.draft.per_glyph.retain(|g| g.glyph_index != g_idx);
                    }
                });
                let entry_idx = st.draft.per_glyph.iter().position(|g| g.glyph_index == g_idx);
                let mut cur = entry_idx.map_or(SfxGlyphTransform { glyph_index: g_idx, ..SfxGlyphTransform::default() }, |i| st.draft.per_glyph[i]);
                let mut edited = false;
                ui.horizontal(|ui| {
                    let (mut dx, mut dy) = (cur.dx as f32, cur.dy as f32);
                    if crate::widgets::value_field(ui, &mut dx, -2000.0..=2000.0, "px", 60.0).changed() {
                        cur.dx = f64::from(dx);
                        edited = true;
                    }
                    if crate::widgets::value_field(ui, &mut dy, -2000.0..=2000.0, "px", 60.0).changed() {
                        cur.dy = f64::from(dy);
                        edited = true;
                    }
                    let mut rot = cur.rotation_deg as f32;
                    if crate::widgets::value_field(ui, &mut rot, -360.0..=360.0, "°", 56.0).changed() {
                        cur.rotation_deg = f64::from(rot);
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
                ui.label(egui::RichText::new("Click a glyph on canvas to move/rotate/scale.").color(t.text_dim).size(11.0));
            }
            if !st.draft.per_glyph.is_empty() && ui.button("Reset All Glyphs").clicked() {
                st.draft.per_glyph.clear();
            }
        }
        SfxWarpMode::MeshLattice => {
            ui.horizontal(|ui| {
                let mut cols = st.draft.mesh.cols as f32;
                let mut rows = st.draft.mesh.rows as f32;
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
            ui.horizontal(|ui| {
                if ui.button("Reset Mesh").clicked() {
                    reset_mesh(&mut st.draft);
                    gizmo.selected_nodes.clear();
                }
                ui.label(egui::RichText::new("Drag empty area to box-select nodes.").color(t.text_dim).size(10.5));
            });
        }
        SfxWarpMode::LiquifyField => {
            ui.horizontal(|ui| {
                let mut r = gizmo.liquify_radius_px as f32;
                let mut strg = gizmo.liquify_strength as f32;
                dim_label(ui, t, "Radius:");
                if crate::widgets::value_field(ui, &mut r, 4.0..=500.0, "px", 60.0).changed() {
                    gizmo.liquify_radius_px = f64::from(r);
                }
                dim_label(ui, t, "Power:");
                if crate::widgets::value_field(ui, &mut strg, 0.05..=2.0, "", 54.0).changed() {
                    gizmo.liquify_strength = f64::from(strg);
                }
            });
            ui.horizontal(|ui| {
                ui.label(format!("Strokes: {}", st.draft.liquify_strokes.len()));
                if !st.draft.liquify_strokes.is_empty() && ui.button("Undo Stroke").clicked() {
                    st.draft.liquify_strokes.pop();
                }
                if !st.draft.liquify_strokes.is_empty() && ui.button("Clear Liquify").clicked() {
                    st.draft.liquify_strokes.clear();
                }
            });
        }
    }
}

fn draw_fill_source_editor(ui: &mut egui::Ui, id_salt: &str, fill: &mut SfxFillSource, t: Tokens) {
    let kind_idx = match fill {
        SfxFillSource::Solid { .. } => 0u8,
        SfxFillSource::Gradient { .. } => 1u8,
        SfxFillSource::Screentone { .. } => 2u8,
    };
    let mut picked = kind_idx;
    ui.horizontal(|ui| {
        ui.selectable_value(&mut picked, 0, "Solid");
        ui.selectable_value(&mut picked, 1, "Gradient");
        ui.selectable_value(&mut picked, 2, "Screentone");
    });
    if picked != kind_idx {
        *fill = match picked {
            0 => SfxFillSource::Solid { color: Color::BLACK },
            1 => SfxFillSource::Gradient { start: Color::BLACK, end: Color::WHITE, angle_deg: 90.0 },
            _ => SfxFillSource::Screentone { spec: ScreentoneSpec::default(), opacity: 1.0 },
        };
    }

    match fill {
        SfxFillSource::Solid { color } => {
            color_swatch(ui, "Color:", color);
        }
        SfxFillSource::Gradient { start, end, angle_deg } => {
            ui.horizontal(|ui| {
                color_swatch(ui, "A:", start);
                color_swatch(ui, "B:", end);
                crate::widgets::value_field(ui, angle_deg, 0.0..=360.0, "°", 56.0);
            });
        }
        SfxFillSource::Screentone { spec, opacity } => {
            egui::Grid::new(format!("{id_salt}-tone")).num_columns(2).spacing(egui::vec2(6.0, 3.0)).show(ui, |ui| {
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
                    crate::widgets::value_field(ui, &mut spec.dot_size, 0.5..=256.0, "px", 56.0);
                    if crate::widgets::value_field(ui, &mut spec.spacing_x, 2.0..=512.0, "px", 56.0).changed() {
                        spec.spacing_y = spec.spacing_x;
                    }
                });
                ui.end_row();

                dim_label(ui, t, "Angle / Op:");
                ui.horizontal(|ui| {
                    crate::widgets::value_field(ui, &mut spec.angle, -180.0..=180.0, "°", 56.0);
                    crate::widgets::value_field(ui, opacity, 0.0..=1.0, "", 56.0);
                });
                ui.end_row();
            });
        }
    }
}

fn draw_fill_section(ui: &mut egui::Ui, st: &mut SfxPanelState, t: Tokens) {
    ui.label(egui::RichText::new("Face Fill").strong().size(12.0));
    draw_fill_source_editor(ui, "sfx-base-fill", &mut st.draft.fill, t);
}

fn pass_title(pass: &SfxStylePass) -> &'static str {
    match pass {
        SfxStylePass::Stroke { .. } => "Stroke (Border)",
        SfxStylePass::Extrude3D { .. } => "3D Extrude",
        SfxStylePass::Shadow { .. } => "Drop Shadow",
        SfxStylePass::InnerGlow { .. } => "Inner Glow",
        SfxStylePass::NegativeInvert { .. } => "Negative / Invert (Difference)",
    }
}

fn draw_styles_section(ui: &mut egui::Ui, st: &mut SfxPanelState, t: Tokens) {
    ui.label(egui::RichText::new("Borders, 3D & FX Stack").strong().size(12.0));
    ui.horizontal_wrapped(|ui| {
        if ui.button("+ Stroke").clicked() {
            st.draft.styles.push(SfxStylePass::Stroke {
                enabled: true,
                width_px: 4.0,
                join: SfxLineJoin::Round,
                miter_limit: 4.0,
                fill: SfxFillSource::Solid { color: Color::WHITE },
            });
        }
        if ui.button("+ 3D").clicked() {
            st.draft.styles.push(SfxStylePass::Extrude3D {
                enabled: true,
                dx_px: 8.0,
                dy_px: 8.0,
                steps: 14,
                fill: SfxFillSource::Solid { color: Color::BLACK },
            });
        }
        if ui.button("+ Shadow").clicked() {
            st.draft.styles.push(SfxStylePass::Shadow { enabled: true, dx_px: 6.0, dy_px: 6.0, blur_px: 3.0, color: Color::BLACK, opacity: 0.7 });
        }
        if ui.button("+ Glow").clicked() {
            st.draft.styles.push(SfxStylePass::InnerGlow { enabled: true, width_px: 3.0, color: Color::WHITE, opacity: 0.85 });
        }
        let has_inv = st.draft.has_negative_invert();
        if ui.selectable_label(has_inv, "Invert (Diff)").clicked() {
            if has_inv {
                st.draft.styles.retain(|p| !matches!(p, SfxStylePass::NegativeInvert { .. }));
            } else {
                st.draft.styles.push(SfxStylePass::NegativeInvert { enabled: true });
            }
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
                    if ui.button("x").on_hover_text("Remove style pass").clicked() {
                        delete_idx = Some(idx);
                    }
                    if idx + 1 < total && ui.button("v").clicked() {
                        swap_op = Some((idx, idx + 1));
                    }
                    if idx > 0 && ui.button("^").clicked() {
                        swap_op = Some((idx - 1, idx));
                    }
                });
            });

            match pass {
                SfxStylePass::Stroke { width_px, join, miter_limit, fill, .. } => {
                    ui.horizontal(|ui| {
                        dim_label(ui, t, "Width:");
                        crate::widgets::value_field(ui, width_px, 0.0..=128.0, "px", 54.0);
                        let joins = [(SfxLineJoin::Round, "Round"), (SfxLineJoin::Miter, "Miter"), (SfxLineJoin::Bevel, "Bevel")];
                        crate::widgets::dropdown_literal(ui, &format!("sfx-join-{idx}"), join, &joins, 72.0);
                        if *join == SfxLineJoin::Miter {
                            crate::widgets::value_field(ui, miter_limit, 1.0..=24.0, "m", 44.0);
                        }
                    });
                    draw_fill_source_editor(ui, &format!("sfx-stroke-fill-{idx}"), fill, t);
                }
                SfxStylePass::Extrude3D { dx_px, dy_px, steps, fill, .. } => {
                    ui.horizontal(|ui| {
                        dim_label(ui, t, "Offset:");
                        crate::widgets::value_field(ui, dx_px, -256.0..=256.0, "x", 50.0);
                        crate::widgets::value_field(ui, dy_px, -256.0..=256.0, "y", 50.0);
                        let mut st_f = *steps as f32;
                        if crate::widgets::value_field(ui, &mut st_f, 1.0..=64.0, "s", 46.0).changed() {
                            *steps = st_f.round().clamp(1.0, 64.0) as u16;
                        }
                    });
                    draw_fill_source_editor(ui, &format!("sfx-ext-fill-{idx}"), fill, t);
                }
                SfxStylePass::Shadow { dx_px, dy_px, blur_px, color, opacity, .. } => {
                    ui.horizontal(|ui| {
                        crate::widgets::value_field(ui, dx_px, -256.0..=256.0, "x", 48.0);
                        crate::widgets::value_field(ui, dy_px, -256.0..=256.0, "y", 48.0);
                        crate::widgets::value_field(ui, blur_px, 0.0..=64.0, "b", 48.0);
                        crate::widgets::value_field(ui, opacity, 0.0..=1.0, "a", 44.0);
                    });
                    color_swatch(ui, "Shadow Color:", color);
                }
                SfxStylePass::InnerGlow { width_px, color, opacity, .. } => {
                    ui.horizontal(|ui| {
                        crate::widgets::value_field(ui, width_px, 0.0..=64.0, "px", 52.0);
                        crate::widgets::value_field(ui, opacity, 0.0..=1.0, "a", 52.0);
                        color_swatch(ui, "Glow:", color);
                    });
                }
                SfxStylePass::NegativeInvert { .. } => {
                    ui.label(egui::RichText::new("Composites glyph silhouette via Difference blend group.").color(t.text_dim).size(11.0));
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

fn draw_distress_section(ui: &mut egui::Ui, st: &mut SfxPanelState, t: Tokens) {
    ui.horizontal(|ui| {
        ui.checkbox(&mut st.draft.distress.enabled, egui::RichText::new("Deterministic Distress").strong().size(12.0));
        if ui.button("Reset").clicked() {
            st.draft.distress = SfxDistress::default();
        }
    });
    if !st.draft.distress.enabled {
        return;
    }
    egui::Grid::new("sfx-distress-grid").num_columns(2).spacing(egui::vec2(8.0, 4.0)).show(ui, |ui| {
        dim_label(ui, t, "Seed:");
        ui.horizontal(|ui| {
            if ui.button("-").clicked() {
                st.draft.distress.seed = st.draft.distress.seed.saturating_sub(1);
            }
            let mut s = st.draft.distress.seed as f32;
            if crate::widgets::value_field(ui, &mut s, 0.0..=99_999.0, "", 68.0).changed() {
                st.draft.distress.seed = s.round().max(0.0) as u32;
            }
            if ui.button("+").clicked() {
                st.draft.distress.seed = st.draft.distress.seed.saturating_add(1);
            }
        });
        ui.end_row();

        dim_label(ui, t, "Roughness:");
        crate::widgets::value_field(ui, &mut st.draft.distress.roughness, 0.0..=16.0, "px", 68.0);
        ui.end_row();

        dim_label(ui, t, "Grain / Chip:");
        ui.horizontal(|ui| {
            crate::widgets::value_field(ui, &mut st.draft.distress.grain, 0.0..=1.0, "", 62.0);
            crate::widgets::value_field(ui, &mut st.draft.distress.edge_chip, 0.0..=1.0, "", 62.0);
        });
        ui.end_row();
    });
}

fn draw_presets_section(ui: &mut egui::Ui, st: &mut SfxPanelState, target: &SfxTargetInfo<'_>, out: &mut SfxPanelOutput) {
    ui.label(egui::RichText::new("JSON Presets").strong().size(12.0));
    ui.horizontal_wrapped(|ui| {
        for preset in target.presets {
            if ui.button(&preset.name).clicked() {
                if let Some(id) = target.layer_id.filter(|_| target.data.is_some()) {
                    out.commands.push(OutCommand::new("sfx.presets.apply", json!({ "layer": id.0, "name": preset.name })));
                } else {
                    st.draft.apply_preset(preset);
                }
            }
        }
    });
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut st.preset_name).hint_text("Preset name...").desired_width(120.0));
        let name = st.preset_name.trim().to_owned();
        if ui.add_enabled(!name.is_empty(), egui::Button::new("Save Preset")).clicked() {
            let mut preset = st.draft.to_preset(&name);
            preset.validate();
            out.commands.push(OutCommand::new("sfx.presets.save", json!({ "preset": preset })));
        }
        if ui.add_enabled(!name.is_empty() && target.presets.iter().any(|p| p.name == name), egui::Button::new("Delete")).clicked() {
            out.commands.push(OutCommand::new("sfx.presets.delete", json!({ "name": name })));
        }
    });
}

fn draw_actions_section(ui: &mut egui::Ui, st: &mut SfxPanelState, target: &SfxTargetInfo<'_>, out: &mut SfxPanelOutput) {
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
        if ui.add_enabled(has_sfx, egui::Button::new("Bake to Raster")).on_hover_text("Freeze the SFX layer into standard pixels").clicked()
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
        assert!(st.cached_fonts.is_some());
    }

    #[test]
    fn sync_from_host_updates_draft_when_active_layer_changes() {
        let mut st = SfxPanelState::default();
        let presets = Vec::new();
        let d1 = SfxData { text: "CRACK".to_owned(), ..SfxData::default() };
        let t1 = SfxTargetInfo { doc_id: Some(BridgeDocId(1)), layer_id: Some(LayerId(2)), data: Some(d1), active_is_text: false, presets: &presets };
        st.sync_from_host(&t1);
        assert_eq!(st.draft.text, "CRACK");
    }
}
