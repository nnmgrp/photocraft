//! Manga SFX tool commands: non-destructive vector lettering with warp modes, 6 stylized ink line
//! modes, 4 Dry Brush engines, vector shadow/3D, screentone fills, and explicit JSON presets.

#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};
use std::sync::Arc;

use photocraft_algo::{PatternType, ScreentoneSpec};
use photocraft_color::{BlendMode, Color, PixelFormat};
use photocraft_doc::sfx::{
    SfxData, SfxDryBrushEngine, SfxDryBrushSpec, SfxFillSource, SfxLineEffect, SfxLineStyle, SfxPreset, SfxShtackVariant, SfxSpikeShape,
    SfxVectorShadow, reset_sfx_warps_for_new_layer, scale_new_sfx_for_document,
};
use photocraft_doc::text::Orientation;
use photocraft_doc::{Group, Layer, LayerContent, LayerId, Rect};
use photocraft_manga_core::sfx_render::{SfxRenderOutput, render_sfx};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, has_doc, layer_param};
use crate::{EngineError, Result, Session};

const MAX_PRESETS: usize = 256;
#[cfg(not(target_arch = "wasm32"))]
const MAX_PRESET_JSON_BYTES: u64 = 1 << 20;

fn ink_color() -> Color {
    Color::rgb(15.0 / 255.0, 23.0 / 255.0, 42.0 / 255.0)
}

fn sfx_bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    bad(cmd, msg)
}

fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn sanitize_slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if (ch == ' ' || ch == '-' || ch == '_') && !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() { "sfx-preset".to_owned() } else { out }
}

/// Deterministic explicit SFX style presets (zero random gacha).
#[derive(Clone, Debug)]
pub struct SfxPresetLibrary {
    presets: Vec<SfxPreset>,
}

impl Default for SfxPresetLibrary {
    fn default() -> Self {
        Self::new()
    }
}

impl SfxPresetLibrary {
    /// Initializes built-in starter manga SFX presets and optionally loads native JSON presets.
    pub fn new() -> Self {
        let mut lib = Self { presets: Self::built_in_presets() };
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(dir) = Self::user_presets_dir() {
            lib.load_user_dir(&dir);
        }
        lib
    }

    pub fn built_in_presets() -> Vec<SfxPreset> {
        let ink = ink_color();
        vec![
            SfxPreset {
                name: "⚡ SHTACK (Торцы)".to_owned(),
                description: "Вариант 1: Caps + иглы".to_owned(),
                font_family: "Impact".to_owned(),
                weight: 800,
                size_pt: 64.0,
                tracking: 4.0,
                fill: SfxFillSource::Solid { color: ink },
                line_effect: SfxLineEffect {
                    style: SfxLineStyle::Shtack,
                    halo_width_px: 10.0,
                    halo_color: Color::WHITE,
                    line_width_px: 0.0,
                    line_color: ink,
                    shtack_variant: SfxShtackVariant::Caps,
                    shtack_bristles: 3,
                    shtack_split_pct: 78.0,
                    shtack_overshoot_px: 20.0,
                    shtack_dryness_pct: 26.0,
                    ..SfxLineEffect::default()
                },
                dry_brush: SfxDryBrushSpec::default(),
                vector_shadow: SfxVectorShadow::default(),
                ..SfxPreset::default()
            },
            SfxPreset {
                name: "⚡ SHTACK (Лента)".to_owned(),
                description: "Вариант 2: Скелет-лента".to_owned(),
                font_family: "Impact".to_owned(),
                weight: 800,
                size_pt: 64.0,
                tracking: 4.0,
                fill: SfxFillSource::Solid { color: ink },
                line_effect: SfxLineEffect {
                    style: SfxLineStyle::Shtack,
                    halo_width_px: 8.0,
                    halo_color: Color::WHITE,
                    line_width_px: 0.0,
                    line_color: ink,
                    shtack_variant: SfxShtackVariant::Ribbon,
                    shtack_bristles: 3,
                    shtack_split_pct: 72.0,
                    shtack_overshoot_px: 16.0,
                    shtack_thickness_px: 13.0,
                    shtack_dryness_pct: 28.0,
                    ..SfxLineEffect::default()
                },
                dry_brush: SfxDryBrushSpec::default(),
                vector_shadow: SfxVectorShadow::default(),
                ..SfxPreset::default()
            },
            SfxPreset {
                name: "Manga Impact".to_owned(),
                description: "Двойной контур + 3D".to_owned(),
                font_family: "Impact".to_owned(),
                weight: 800,
                size_pt: 64.0,
                tracking: 6.0,
                fill: SfxFillSource::Solid { color: ink },
                line_effect: SfxLineEffect {
                    style: SfxLineStyle::Clean,
                    halo_width_px: 14.0,
                    halo_color: Color::WHITE,
                    line_width_px: 6.0,
                    line_color: ink,
                    ..SfxLineEffect::default()
                },
                dry_brush: SfxDryBrushSpec::default(),
                vector_shadow: SfxVectorShadow {
                    enabled: true,
                    angle_deg: 52.0,
                    distance_px: 12.0,
                    extrude_depth_px: 8.0,
                    color: ink,
                },
                ..SfxPreset::default()
            },
            SfxPreset {
                name: "1-Bit 600dpi".to_owned(),
                description: "Рубленый край + Solid".to_owned(),
                font_family: "Impact".to_owned(),
                weight: 800,
                size_pt: 64.0,
                tracking: 4.0,
                fill: SfxFillSource::Solid { color: ink },
                line_effect: SfxLineEffect {
                    style: SfxLineStyle::OneBit,
                    halo_width_px: 11.0,
                    halo_color: Color::WHITE,
                    line_width_px: 2.0,
                    line_color: ink,
                    one_bit_intensity: 68.0,
                    ..SfxLineEffect::default()
                },
                dry_brush: SfxDryBrushSpec::default(),
                vector_shadow: SfxVectorShadow::default(),
                ..SfxPreset::default()
            },
            SfxPreset {
                name: "✒ Наст. G-Pen".to_owned(),
                description: "Нажим + перекрестья #".to_owned(),
                font_family: "Impact".to_owned(),
                weight: 800,
                size_pt: 64.0,
                tracking: 4.0,
                fill: SfxFillSource::Solid { color: Color::WHITE },
                line_effect: SfxLineEffect {
                    style: SfxLineStyle::GPenNib,
                    halo_width_px: 10.0,
                    halo_color: Color::WHITE,
                    line_width_px: 5.0,
                    line_color: ink,
                    gpen_contrast_pct: 82.0,
                    gpen_angle_deg: -35.0,
                    gpen_pinch_pct: 48.0,
                    gpen_overshoot_px: 7.0,
                    gpen_overshoot_count_pct: 85.0,
                    gpen_overshoot_random_pct: 45.0,
                    ..SfxLineEffect::default()
                },
                dry_brush: SfxDryBrushSpec::default(),
                vector_shadow: SfxVectorShadow::default(),
                ..SfxPreset::default()
            },
            SfxPreset {
                name: "🖌 Кисть-штрих".to_owned(),
                description: "Живой лайн + перекрестья #".to_owned(),
                font_family: "Impact".to_owned(),
                weight: 800,
                size_pt: 64.0,
                tracking: 4.0,
                fill: SfxFillSource::Solid { color: Color::WHITE },
                line_effect: SfxLineEffect {
                    style: SfxLineStyle::BrushStroke,
                    halo_width_px: 11.0,
                    halo_color: Color::WHITE,
                    line_width_px: 5.0,
                    line_color: ink,
                    ink_brush_roughness_pct: 42.0,
                    ink_brush_dropout_pct: 35.0,
                    ink_brush_pressure_pct: 60.0,
                    ink_brush_overshoot_px: 8.0,
                    ink_brush_overshoot_count_pct: 85.0,
                    ink_brush_overshoot_random_pct: 45.0,
                    ..SfxLineEffect::default()
                },
                dry_brush: SfxDryBrushSpec::default(),
                vector_shadow: SfxVectorShadow::default(),
                ..SfxPreset::default()
            },
            SfxPreset {
                name: "Сухая кисть (А/Б1)".to_owned(),
                description: "1-Bit + Б1: Штрихи".to_owned(),
                font_family: "Impact".to_owned(),
                weight: 800,
                size_pt: 64.0,
                tracking: 4.0,
                fill: SfxFillSource::Solid { color: ink },
                line_effect: SfxLineEffect {
                    style: SfxLineStyle::OneBit,
                    halo_width_px: 12.0,
                    halo_color: Color::WHITE,
                    line_width_px: 2.0,
                    line_color: ink,
                    one_bit_intensity: 55.0,
                    ..SfxLineEffect::default()
                },
                dry_brush: SfxDryBrushSpec {
                    enabled: true,
                    engine: SfxDryBrushEngine::SdfSeg,
                    dryness_pct: 44.0,
                    freq_pct: 62.0,
                    warp_pct: 30.0,
                    paper_grain_pct: 38.0,
                    edge_wear_pct: 44.0,
                    ..SfxDryBrushSpec::default()
                },
                vector_shadow: SfxVectorShadow::default(),
                ..SfxPreset::default()
            },
            SfxPreset {
                name: "Скринтон Крик".to_owned(),
                description: "Шипы + Screentone".to_owned(),
                font_family: "Impact".to_owned(),
                weight: 800,
                size_pt: 64.0,
                tracking: 6.0,
                fill: SfxFillSource::Screentone {
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
                },
                line_effect: SfxLineEffect {
                    style: SfxLineStyle::Spikes,
                    halo_width_px: 12.0,
                    halo_color: Color::WHITE,
                    line_width_px: 3.0,
                    line_color: ink,
                    spike_shape: SfxSpikeShape::Triangle,
                    spike_length_px: 13.0,
                    spike_size_px: 7.0,
                    spike_count: 20,
                    spike_random_pct: 45.0,
                    ..SfxLineEffect::default()
                },
                dry_brush: SfxDryBrushSpec::default(),
                vector_shadow: SfxVectorShadow {
                    enabled: true,
                    angle_deg: 48.0,
                    distance_px: 8.0,
                    extrude_depth_px: 5.0,
                    color: ink,
                },
                ..SfxPreset::default()
            },
        ]
    }

    pub fn presets(&self) -> &[SfxPreset] {
        &self.presets
    }

    pub fn find(&self, name: &str) -> Option<&SfxPreset> {
        let q = name.trim();
        if let Some(p) = self.presets.iter().find(|p| p.name.eq_ignore_ascii_case(q)) {
            return Some(p);
        }
        // Backwards-compatible aliases for legacy preset names in tests/scripts.
        let alias = if q.eq_ignore_ascii_case("Screentone Shout") {
            Some("Скринтон Крик")
        } else if q.eq_ignore_ascii_case("Manga Impact (B/W)") {
            Some("Manga Impact")
        } else {
            None
        };
        alias.and_then(|target| self.presets.iter().find(|p| p.name.eq_ignore_ascii_case(target)))
    }

    pub fn insert_or_replace(&mut self, mut preset: SfxPreset) {
        preset.validate();
        if let Some(slot) = self.presets.iter_mut().find(|p| p.name.eq_ignore_ascii_case(&preset.name)) {
            *slot = preset;
            return;
        }
        if self.presets.len() < MAX_PRESETS {
            self.presets.push(preset);
        }
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let q = name.trim();
        let before = self.presets.len();
        self.presets.retain(|p| !p.name.eq_ignore_ascii_case(q));
        self.presets.len() != before
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn user_presets_dir() -> Option<PathBuf> {
        if cfg!(test) {
            return None;
        }
        let base = std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config").into_os_string()))?;
        Some(PathBuf::from(base).join("photocraft").join("sfx_presets"))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn load_user_dir(&mut self, dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten().take(MAX_PRESETS) {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()).is_none_or(|e| !e.eq_ignore_ascii_case("json")) {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            if !meta.is_file() || meta.len() > MAX_PRESET_JSON_BYTES {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            if let Ok(preset) = serde_json::from_str::<SfxPreset>(&text) {
                self.insert_or_replace(preset);
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn persist_preset_file(dir: &Path, preset: &SfxPreset) -> std::result::Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let filename = format!("{}.json", sanitize_slug(&preset.name));
        let path = dir.join(&filename);
        if path.parent() != Some(dir) {
            return Err("invalid preset path".into());
        }
        let body = serde_json::to_string_pretty(preset).map_err(|e| e.to_string())?;
        std::fs::write(&path, body).map_err(|e| e.to_string())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn delete_preset_file(dir: &Path, name: &str) {
        let filename = format!("{}.json", sanitize_slug(name));
        let path = dir.join(&filename);
        if path.parent() == Some(dir) && path.is_file() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn auto_layer_name(data: &SfxData) -> String {
    let clean: String = data.text.chars().filter(|c| !c.is_control()).take(20).collect();
    let trimmed = clean.trim();
    if trimmed.is_empty() { "SFX Layer".to_owned() } else { format!("SFX: {trimmed}") }
}

fn render_with_shared_text(data: &SfxData, format: PixelFormat, dpi: f32) -> SfxRenderOutput {
    let mut guard = photocraft_text::shared().lock().unwrap_or_else(|e| e.into_inner());
    render_sfx(data, &mut guard, format, dpi)
}

fn apply_render_to_layer(layer: &mut Layer, data: SfxData, rendered: SfxRenderOutput, fmt: PixelFormat) {
    layer.sfx = Some(Arc::new(data));
    match rendered {
        SfxRenderOutput::Single { surface, .. } => {
            layer.blend = BlendMode::Normal;
            layer.content = LayerContent::Raster(surface);
        }
        SfxRenderOutput::InvertPair { under, invert, .. } => {
            layer.blend = BlendMode::PassThrough;
            let under_layer = Layer::new("SFX Stroke / Shadow", LayerContent::Raster(under));
            let mut invert_layer = Layer::new("SFX Invert", LayerContent::Raster(invert));
            invert_layer.blend = BlendMode::Difference;
            let _ = fmt;
            layer.content = LayerContent::Group(Group {
                children: vec![under_layer, invert_layer],
                expanded: false,
                artboard: None,
                frame_stroke: None,
            });
        }
    }
}

fn bounds_json(r: Rect) -> Value {
    json!([r.x0, r.y0, r.x1, r.y1])
}

fn parse_sfx_data(cmd: &str, p: &Value) -> Result<SfxData> {
    let mut data: SfxData = if let Some(v) = p.get("sfx") {
        serde_json::from_value(v.clone()).map_err(|e| sfx_bad(cmd, format!("invalid `sfx` payload: {e}")))?
    } else {
        let mut d = SfxData::default();
        if let Some(txt) = p.get("text").and_then(Value::as_str) {
            d.text = txt.to_owned();
        }
        if let (Some(x), Some(y)) = (p.get("x").and_then(Value::as_f64), p.get("y").and_then(Value::as_f64)) {
            d.origin = [x, y];
        }
        d
    };
    data.validate();
    Ok(data)
}

fn sfx_create(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sfx.create";
    // An explicit `sfx` object is caller-owned: never overwrite values it supplied. Freshly derived
    // defaults (from `text`/`x`/`y`) scale to the document so 600-DPI canvases stay legible.
    let explicit_sfx = p.get("sfx").is_some();
    let mut data = parse_sfx_data(CMD, p)?;
    let custom_name = p.get("name").and_then(Value::as_str).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    let (fmt, dpi, width, height) = {
        let st = s.active().ok_or(EngineError::NoDocument)?;
        (st.doc.pixel_format(), st.doc.resolution_dpi, st.doc.size.width, st.doc.size.height)
    };
    if !explicit_sfx {
        reset_sfx_warps_for_new_layer(&mut data);
        scale_new_sfx_for_document(&mut data, width, height);
    }
    let rendered = render_with_shared_text(&data, fmt, dpi);
    let bounds = rendered.bounds();

    let id = s.edit("New SFX Layer", |doc, active| {
        let name = custom_name.clone().unwrap_or_else(|| auto_layer_name(&data));
        let mut layer = Layer::raster(name, fmt);
        apply_render_to_layer(&mut layer, data.clone(), rendered.clone(), fmt);
        let new_id = crate::layer_place::insert_new(doc, *active, layer, crate::layer_place::Place::AutoEditable);
        *active = Some(new_id);
        Ok(new_id)
    })?;
    Ok(json!({ "layer": id.0, "bounds": bounds_json(bounds) }))
}

fn check_layer_unlocked(doc: &photocraft_doc::Document, id: LayerId) -> Result<()> {
    let locks = doc.effective_locks(id);
    if locks.all || locks.pixels {
        return Err(EngineError::Other("SFX layer is locked".into()));
    }
    Ok(())
}

fn sfx_update(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sfx.update";
    let id = layer_param(s, p)?;
    let data = parse_sfx_data(CMD, p)?;
    let (fmt, dpi) = {
        let st = s.active().ok_or(EngineError::NoDocument)?;
        check_layer_unlocked(&st.doc, id)?;
        let l = st.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
        if l.sfx.is_none() {
            return Err(sfx_bad(CMD, "target layer is not an SFX layer"));
        }
        (st.doc.pixel_format(), st.doc.resolution_dpi)
    };
    let rendered = render_with_shared_text(&data, fmt, dpi);
    let bounds = rendered.bounds();

    s.edit("Update SFX", |doc, _| {
        check_layer_unlocked(doc, id)?;
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        if l.sfx.is_none() {
            return Err(sfx_bad(CMD, "target layer is not an SFX layer"));
        }
        if l.name.starts_with("SFX:") || l.name == "SFX Layer" {
            l.name = auto_layer_name(&data);
        }
        apply_render_to_layer(l, data.clone(), rendered.clone(), fmt);
        Ok(())
    })?;
    Ok(json!({ "ok": true, "layer": id.0, "bounds": bounds_json(bounds) }))
}

fn sfx_convert_from_text(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sfx.convertFromText";
    let keep_original = p.get("keepOriginal").and_then(Value::as_bool).unwrap_or(true);
    let (src_id, fmt, dpi, data) = {
        let st = s.active().ok_or(EngineError::NoDocument)?;
        let id = layer_param(s, p)?;
        let layer = st.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
        let LayerContent::Text(tl) = &layer.content else {
            return Err(sfx_bad(CMD, "target layer is not a type layer"));
        };
        let first_style = tl.char_runs().first().map(|r| r.style.clone()).unwrap_or_default();
        let origin = tl
            .cache
            .as_ref()
            .map(|c| c.content_bounds())
            .filter(|b| !b.is_empty())
            .map_or([tl.transform.m[4], tl.transform.m[5] - f64::from(first_style.size_pt) * 0.85], |b| [f64::from(b.x0), f64::from(b.y0)]);
        let mut sfx = SfxData {
            text: tl.text.clone(),
            origin,
            font_family: if first_style.font_family.trim().is_empty() { "Inter".to_owned() } else { first_style.font_family.clone() },
            font_style: first_style.font_style.clone(),
            postscript_name: first_style.postscript_name.clone(),
            weight: first_style.weight,
            italic: first_style.italic,
            size_pt: first_style.size_pt.max(8.0),
            tracking: first_style.tracking,
            vertical: tl.orientation == Orientation::Vertical,
            fill: SfxFillSource::Solid { color: first_style.color },
            ..SfxData::default()
        };
        // A converted type layer is a new SFX layer: never inherit warp deltas from tool state.
        reset_sfx_warps_for_new_layer(&mut sfx);
        sfx.validate();
        (id, st.doc.pixel_format(), st.doc.resolution_dpi, sfx)
    };
    let rendered = render_with_shared_text(&data, fmt, dpi);
    let bounds = rendered.bounds();

    let new_id = s.edit("Convert Type to SFX", |doc, active| {
        let mut layer = Layer::raster(auto_layer_name(&data), fmt);
        apply_render_to_layer(&mut layer, data.clone(), rendered.clone(), fmt);
        let inserted = crate::layer_place::insert_new(doc, Some(src_id), layer, crate::layer_place::Place::AutoEditable);
        if !keep_original {
            doc.remove(src_id);
        }
        *active = Some(inserted);
        Ok(inserted)
    })?;
    Ok(json!({ "layer": new_id.0, "bounds": bounds_json(bounds) }))
}

fn sfx_bake_to_raster(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sfx.bakeToRaster";
    let id = layer_param(s, p)?;
    s.edit("Bake SFX to Raster", |doc, _| {
        check_layer_unlocked(doc, id)?;
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        if l.sfx.is_none() {
            return Err(sfx_bad(CMD, "target layer is not an SFX layer"));
        }
        l.sfx = None;
        if let Some(rest) = l.name.strip_prefix("SFX: ") {
            l.name = rest.to_owned();
        }
        Ok(())
    })?;
    Ok(json!({ "ok": true, "layer": id.0 }))
}

fn sfx_presets_list(s: &mut Session, _p: &Value) -> Result<Value> {
    Ok(json!({ "presets": s.manga.sfx_presets.presets() }))
}

fn sfx_presets_save(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sfx.presets.save";
    let preset: SfxPreset = if let Some(v) = p.get("preset") {
        serde_json::from_value(v.clone()).map_err(|e| sfx_bad(CMD, format!("invalid `preset`: {e}")))?
    } else if let Some(name) = p.get("name").and_then(Value::as_str) {
        let layer_id = layer_param(s, p)?;
        let st = s.active().ok_or(EngineError::NoDocument)?;
        let l = st.doc.layer(layer_id).ok_or(EngineError::NoLayer(layer_id))?;
        let sfx = l.sfx.as_deref().ok_or_else(|| sfx_bad(CMD, "target layer is not an SFX layer"))?;
        sfx.to_preset(name)
    } else {
        return Err(sfx_bad(CMD, "missing `preset` object or preset `name`"));
    };
    let mut preset = preset;
    preset.validate();
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(dir) = SfxPresetLibrary::user_presets_dir() {
        let _ = SfxPresetLibrary::persist_preset_file(&dir, &preset);
    }
    let name = preset.name.clone();
    s.manga.sfx_presets.insert_or_replace(preset.clone());
    Ok(json!({ "ok": true, "name": name, "preset": preset }))
}

fn sfx_presets_delete(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sfx.presets.delete";
    let name = p.get("name").and_then(Value::as_str).ok_or_else(|| sfx_bad(CMD, "missing preset `name`"))?;
    let removed = s.manga.sfx_presets.remove(name);
    #[cfg(not(target_arch = "wasm32"))]
    if removed && let Some(dir) = SfxPresetLibrary::user_presets_dir() {
        SfxPresetLibrary::delete_preset_file(&dir, name);
    }
    Ok(json!({ "ok": removed }))
}

fn sfx_presets_apply(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sfx.presets.apply";
    let id = layer_param(s, p)?;
    let preset: SfxPreset = if let Some(v) = p.get("preset") {
        serde_json::from_value(v.clone()).map_err(|e| sfx_bad(CMD, format!("invalid `preset`: {e}")))?
    } else {
        let name = p.get("name").and_then(Value::as_str).ok_or_else(|| sfx_bad(CMD, "missing preset `name` or `preset`"))?;
        s.manga.sfx_presets.find(name).cloned().ok_or_else(|| sfx_bad(CMD, format!("unknown SFX preset `{name}`")))?
    };
    let (fmt, dpi, mut data) = {
        let st = s.active().ok_or(EngineError::NoDocument)?;
        check_layer_unlocked(&st.doc, id)?;
        let l = st.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
        let cur = l.sfx.as_deref().ok_or_else(|| sfx_bad(CMD, "target layer is not an SFX layer"))?;
        (st.doc.pixel_format(), st.doc.resolution_dpi, cur.clone())
    };
    data.apply_preset(&preset);
    let rendered = render_with_shared_text(&data, fmt, dpi);
    let bounds = rendered.bounds();

    s.edit("Apply SFX Preset", |doc, _| {
        check_layer_unlocked(doc, id)?;
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        apply_render_to_layer(l, data.clone(), rendered.clone(), fmt);
        Ok(())
    })?;
    Ok(json!({ "ok": true, "layer": id.0, "bounds": bounds_json(bounds) }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "sfx.create",
            label: "Create SFX Layer",
            menu: &[],
            shortcut: None,
            params: r##"{"sfx":SfxData?,"text":str?,"x":f64?,"y":f64?,"name":str?} -> {layer,bounds}"##,
            enabled: has_doc,
            run: sfx_create,
            journal: true,
        },
        CommandSpec {
            id: "sfx.update",
            label: "Update SFX Layer",
            menu: &[],
            shortcut: None,
            params: r##"{"layer":id?,"sfx":SfxData,"coalesce":str?} -> {ok:true,layer,bounds}"##,
            enabled: has_doc,
            run: sfx_update,
            journal: true,
        },
        CommandSpec {
            id: "sfx.convertFromText",
            label: "Convert Type to SFX",
            menu: &[],
            shortcut: None,
            params: r##"{"layer":id?,"keepOriginal":bool?} -> {layer,bounds}"##,
            enabled: has_doc,
            run: sfx_convert_from_text,
            journal: true,
        },
        CommandSpec {
            id: "sfx.bakeToRaster",
            label: "Bake SFX to Raster",
            menu: &[],
            shortcut: None,
            params: r##"{"layer":id?} -> {ok:true,layer}"##,
            enabled: has_doc,
            run: sfx_bake_to_raster,
            journal: true,
        },
        CommandSpec {
            id: "sfx.presets.list",
            label: "List SFX Presets",
            menu: &[],
            shortcut: None,
            params: r##"{} -> {presets:[SfxPreset...]}"##,
            enabled: always,
            run: sfx_presets_list,
            journal: false,
        },
        CommandSpec {
            id: "sfx.presets.save",
            label: "Save SFX Preset",
            menu: &[],
            shortcut: None,
            params: r##"{"preset":SfxPreset? | "name":str,"layer":id?} -> {ok:true,name,preset}"##,
            enabled: always,
            run: sfx_presets_save,
            journal: true,
        },
        CommandSpec {
            id: "sfx.presets.delete",
            label: "Delete SFX Preset",
            menu: &[],
            shortcut: None,
            params: r##"{"name":str} -> {ok:bool}"##,
            enabled: always,
            run: sfx_presets_delete,
            journal: true,
        },
        CommandSpec {
            id: "sfx.presets.apply",
            label: "Apply SFX Preset",
            menu: &[],
            shortcut: None,
            params: r##"{"layer":id?,"name":str?,"preset":SfxPreset?} -> {ok:true,layer,bounds}"##,
            enabled: has_doc,
            run: sfx_presets_apply,
            journal: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use photocraft_doc::sfx::SfxStylePass;

    use super::*;

    #[test]
    fn sfx_create_update_coalesce_convert_invert_and_bake_undo_cleanly() {
        let mut s = Session::new();
        s.execute("file.new", json!({ "width": 512, "height": 512, "background": "white" })).unwrap();

        let created = s.execute("sfx.create", json!({ "text": "BAM!", "x": 60.0, "y": 80.0 })).unwrap();
        let id = LayerId(created["layer"].as_u64().unwrap());
        let st = s.active().unwrap();
        let layer = st.doc.layer(id).unwrap();
        assert!(layer.sfx.is_some());
        assert!(matches!(layer.content, LayerContent::Raster(_)));
        assert!(!layer.surface().unwrap().content_bounds().is_empty());

        let mut updated = layer.sfx.as_deref().unwrap().clone();
        updated.text = "BOOM!".to_owned();
        updated.styles.push(SfxStylePass::NegativeInvert { enabled: true });
        for i in 0..3 {
            updated.origin[0] = 60.0 + f64::from(i) * 5.0;
            s.execute("sfx.update", json!({ "layer": id.0, "sfx": updated, "coalesce": "sfx-drag" })).unwrap();
        }
        let st = s.active().unwrap();
        let l2 = st.doc.layer(id).unwrap();
        assert_eq!(l2.blend, BlendMode::PassThrough);
        assert!(matches!(l2.content, LayerContent::Group(ref g) if g.children.len() == 2 && !g.expanded));
        assert_eq!(l2.sfx.as_deref().unwrap().text, "BOOM!");

        assert!(s.undo());
        let st = s.active().unwrap();
        let undone = st.doc.layer(id).unwrap();
        assert_eq!(undone.sfx.as_deref().unwrap().text, "BAM!");
        assert!(matches!(undone.content, LayerContent::Raster(_)));

        assert!(s.redo());
        s.execute("sfx.bakeToRaster", json!({ "layer": id.0 })).unwrap();
        assert!(s.active().unwrap().doc.layer(id).unwrap().sfx.is_none());

        // Type conversion + explicit preset round-trip
        let tr = s.execute("type.create", json!({ "x": 120.0, "y": 140.0, "text": "WHOOSH", "size": 36.0 })).unwrap();
        let tid = tr["layer"].as_u64().unwrap();
        let conv = s.execute("sfx.convertFromText", json!({ "layer": tid, "keepOriginal": true })).unwrap();
        let sfx_from_type = LayerId(conv["layer"].as_u64().unwrap());
        assert!(s.active().unwrap().doc.layer(LayerId(tid)).is_some());
        assert_eq!(s.active().unwrap().doc.layer(sfx_from_type).unwrap().sfx.as_deref().unwrap().text, "WHOOSH");

        let list = s.execute("sfx.presets.list", json!({})).unwrap();
        assert!(list["presets"].as_array().unwrap().len() >= 8);
        s.execute("sfx.presets.apply", json!({ "layer": sfx_from_type.0, "name": "Screentone Shout" })).unwrap();
        assert_eq!(s.active().unwrap().doc.layer(sfx_from_type).unwrap().sfx.as_deref().unwrap().text, "WHOOSH");

        // Preset save / overwrite / delete
        let mut custom = s.active().unwrap().doc.layer(sfx_from_type).unwrap().sfx.as_deref().unwrap().to_preset("Custom SFX Test");
        custom.size_pt = 64.0;
        s.execute("sfx.presets.save", json!({ "preset": custom })).unwrap();
        let after_save = s.execute("sfx.presets.list", json!({})).unwrap();
        assert!(after_save["presets"].as_array().unwrap().iter().any(|p| p["name"] == "Custom SFX Test"));
        let del = s.execute("sfx.presets.delete", json!({ "name": "Custom SFX Test" })).unwrap();
        assert_eq!(del["ok"], true);
    }

    #[test]
    fn sfx_create_scales_fresh_defaults_but_preserves_explicit_payload() {
        let mut s = Session::new();
        s.execute("file.new", json!({ "width": 12303, "height": 17499 })).unwrap();

        let fresh = s.execute("sfx.create", json!({ "text": "BAM", "x": 40.0, "y": 60.0 })).unwrap();
        let fresh_id = LayerId(fresh["layer"].as_u64().unwrap());
        let scaled = s.active().unwrap().doc.layer(fresh_id).unwrap().sfx.as_deref().unwrap().clone();
        assert!(scaled.size_pt > 54.0 * 5.0, "fresh defaults scale on a large page: {}", scaled.size_pt);
        assert!(scaled.line_effect.halo_width_px > 10.0, "halo scales too");

        let mut explicit = SfxData::default();
        explicit.text = "BOOM".to_owned();
        explicit.size_pt = 72.0;
        explicit.free_transform.corners[0] = [3.0, 4.0];
        let made = s.execute("sfx.create", json!({ "sfx": explicit })).unwrap();
        let explicit_id = LayerId(made["layer"].as_u64().unwrap());
        let got = s.active().unwrap().doc.layer(explicit_id).unwrap().sfx.as_deref().unwrap();
        assert_eq!(got.size_pt, 72.0, "explicit point size is never overwritten");
        assert_eq!(got.free_transform.corners[0], [3.0, 4.0], "explicit corner warp is preserved");
    }

    #[test]
    fn sfx_pcraft_roundtrip_preserves_layer_and_deformation() {
        let mut s = Session::new();
        s.execute("file.new", json!({ "width": 512, "height": 512, "background": "white" })).unwrap();
        let mut sfx = SfxData { text: "DOOM".to_owned(), origin: [90.0, 120.0], vertical: true, ..SfxData::default() };
        sfx.free_transform.skew_x_deg = 15.0;
        sfx.mesh.points[0] = [-0.08, 0.05];
        sfx.distress.enabled = true;
        sfx.distress.seed = 777;
        let created = s.execute("sfx.create", json!({ "sfx": sfx })).unwrap();
        let id = LayerId(created["layer"].as_u64().unwrap());

        let bytes = photocraft_format::save_to_bytes(&s.active().unwrap().doc, &Default::default()).unwrap();
        let reloaded = photocraft_format::load_from_bytes(&bytes).unwrap();
        let rt_layer = reloaded.layer(id).expect("SFX layer after .pcraft reload");
        let rt = rt_layer.sfx.as_deref().expect("sfx payload preserved");
        assert_eq!(rt.text, "DOOM");
        assert!(rt.vertical);
        assert!((rt.free_transform.skew_x_deg - 15.0).abs() < 1e-9);
        assert!((rt.mesh.points[0][0] + 0.08).abs() < 1e-9);
        assert!(rt.distress.enabled);
        assert_eq!(rt.distress.seed, 777);
    }
}