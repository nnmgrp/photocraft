//! Extensible vector & texture brush preset library (`crates/engine/src/vector_brush_lib.rs`).
//!
//! Provides:
//! 1. Built-in presets (`g-pen`, `fukidashi-fude`, `soft-pencil-2b`, `dry-sumi`, `marker`) plus
//!    safe runtime loading/importing of user `vector_brushes/*.json` and grayscale `*.png` ribbons
//!    on native platforms (`#[cfg(not(target_arch = "wasm32"))]`).
//! 2. Mode 1 (Dab Chain): bridges [`sample_balloon_contour_for_brush`] stations to
//!    [`photocraft_paint::apply_stroke`] so any raster tip/texture brush can stamp along a balloon.
//! 3. Mode 2 (`(u, v)` Ribbon Texture Sampling): bilinear sampling of decoded grayscale ribbon
//!    alpha masks across arc-length `u` and ribbon cross-section `v in -1.0..=1.0`.

use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};

use photocraft_codecs::{DecodeOptions, Format, Image, Limits};
use photocraft_doc::vector::LiveBalloon;
use photocraft_doc::vector_brush::{VectorBrushCategory, VectorBrushPreset};
use photocraft_geom::Rect;
use photocraft_paint::{BrushPreset, BrushSettings, Stroke, StrokePoint, apply_stroke};
use photocraft_raster::Surface;
use photocraft_vector::balloon::sample_balloon_contour_for_brush;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::vector_cmds::bad;
use crate::{Result, Session};

const MAX_BRUSH_JSON_BYTES: u64 = 2 << 20;
const MAX_BRUSH_PNG_BYTES: u64 = 16 << 20;
const MAX_RIBBON_SIDE: u32 = 4096;
const MAX_DIR_ENTRIES: usize = 512;

/// Decoded normalized grayscale coverage strip for `(u, v)` ribbon texture sampling.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedRibbonTexture {
    pub width: u32,
    pub height: u32,
    /// Row-major normalized alpha coverage in `0.0..=1.0`.
    pub alpha: Vec<f32>,
}

impl LoadedRibbonTexture {
    /// Extracts coverage from an arbitrary decoded PNG image:
    /// * Images with an alpha channel use the normalized alpha channel directly.
    /// * Opaque grayscale/RGB images treat darker ink against bright paper as coverage (`1.0 - luma`).
    pub fn from_image(img: &Image) -> Self {
        let w = img.width().max(1);
        let h = img.height().max(1);
        let ch = img.layout().channels().max(1);
        let has_alpha = img.layout().has_alpha();
        let mut alpha = Vec::with_capacity(w as usize * h as usize);

        for y in 0..h {
            for x in 0..w {
                let cov = if has_alpha {
                    img.get(x, y, ch - 1)
                } else if ch >= 3 {
                    let r = img.get(x, y, 0);
                    let g = img.get(x, y, 1);
                    let b = img.get(x, y, 2);
                    let luma = 0.299 * r + 0.587 * g + 0.114 * b;
                    1.0 - luma
                } else {
                    1.0 - img.get(x, y, 0)
                };
                alpha.push(if cov.is_finite() { cov.clamp(0.0, 1.0) } else { 0.0 });
            }
        }
        Self { width: w, height: h, alpha }
    }

    /// Bilinear sample at normalized arc coordinate `u` (tiled periodically modulo 1.0) and
    /// cross-ribbon coordinate `v in -1.0..=1.0` (`-1` = right edge, `+1` = left edge).
    pub fn sample_bilinear(&self, u: f32, v: f32) -> f32 {
        if self.width == 0 || self.height == 0 || self.alpha.is_empty() || !u.is_finite() || !v.is_finite() {
            return 0.0;
        }
        let u_wrap = u.rem_euclid(1.0);
        let v_unit = (v * 0.5 + 0.5).clamp(0.0, 1.0);

        let fx = u_wrap * (self.width.saturating_sub(1)) as f32;
        let fy = v_unit * (self.height.saturating_sub(1)) as f32;

        let x0 = (fx.floor() as u32).min(self.width - 1);
        let y0 = (fy.floor() as u32).min(self.height - 1);
        let x1 = (x0 + 1).min(self.width - 1);
        let y1 = (y0 + 1).min(self.height - 1);

        let tx = (fx - x0 as f32).clamp(0.0, 1.0);
        let ty = (fy - y0 as f32).clamp(0.0, 1.0);

        let at = |x: u32, y: u32| -> f32 {
            let idx = y as usize * self.width as usize + x as usize;
            self.alpha.get(idx).copied().unwrap_or(0.0)
        };

        let c00 = at(x0, y0);
        let c10 = at(x1, y0);
        let c01 = at(x0, y1);
        let c11 = at(x1, y1);
        let top = c00 + (c10 - c00) * tx;
        let bot = c01 + (c11 - c01) * tx;
        (top + (bot - top) * ty).clamp(0.0, 1.0)
    }
}

/// Validates a safe single-segment filename (no path separators or `..`).
pub fn is_safe_single_filename(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && !name.contains(':')
        && !name.chars().any(char::is_control)
}

/// Converts an arbitrary file stem into a safe ASCII preset ID (`[a-z0-9_-]`).
pub fn sanitize_preset_id(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if (ch == '-' || ch == '_' || ch == ' ') && !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
        if out.len() >= 64 {
            break;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() { "custom-ribbon".to_string() } else { out }
}

/// Decodes a PNG byte slice into a validated [`LoadedRibbonTexture`].
pub fn decode_ribbon_png_bytes(bytes: &[u8]) -> std::result::Result<LoadedRibbonTexture, String> {
    if bytes.len() as u64 > MAX_BRUSH_PNG_BYTES {
        return Err(format!("ribbon PNG exceeds {} bytes limit", MAX_BRUSH_PNG_BYTES));
    }
    let opts = DecodeOptions {
        limits: Limits {
            max_width: MAX_RIBBON_SIDE,
            max_height: MAX_RIBBON_SIDE,
            max_pixels: u64::from(MAX_RIBBON_SIDE) * u64::from(MAX_RIBBON_SIDE),
            ..Limits::default()
        },
        ..DecodeOptions::default()
    };
    let img = photocraft_codecs::decode_as_with(Format::Png, bytes, &opts).map_err(|e| format!("invalid ribbon PNG: {e}"))?;
    if img.width() == 0 || img.height() == 0 || img.width() > MAX_RIBBON_SIDE || img.height() > MAX_RIBBON_SIDE {
        return Err(format!("ribbon PNG dimensions must be 1..={MAX_RIBBON_SIDE} px"));
    }
    Ok(LoadedRibbonTexture::from_image(&img))
}

/// Session-scoped registry of built-in and user-imported vector & texture brushes.
#[derive(Clone, Debug)]
pub struct VectorBrushLibrary {
    presets: Vec<VectorBrushPreset>,
    ribbons: HashMap<String, LoadedRibbonTexture>,
}

impl Default for VectorBrushLibrary {
    fn default() -> Self {
        Self::new()
    }
}

impl VectorBrushLibrary {
    /// Creates a library seeded with the 5 built-in presets (`g-pen`, `fukidashi-fude`,
    /// `soft-pencil-2b`, `dry-sumi`, `marker`).
    pub fn new() -> Self {
        Self { presets: VectorBrushPreset::builtin_presets(), ribbons: HashMap::new() }
    }

    pub fn presets(&self) -> &[VectorBrushPreset] {
        &self.presets
    }

    pub fn get(&self, id: &str) -> Option<&VectorBrushPreset> {
        self.presets.iter().find(|p| p.id == id)
    }

    pub fn ribbon_texture(&self, key: &str) -> Option<&LoadedRibbonTexture> {
        self.ribbons.get(key)
    }

    pub fn set_ribbon_texture(&mut self, key: impl Into<String>, tex: LoadedRibbonTexture) {
        self.ribbons.insert(key.into(), tex);
    }

    /// Validates and inserts (or updates by matching `id`) a vector brush preset.
    pub fn insert_or_replace(&mut self, preset: VectorBrushPreset) -> std::result::Result<(), String> {
        preset.validate()?;
        if let Some(slot) = self.presets.iter_mut().find(|p| p.id == preset.id) {
            *slot = preset;
        } else {
            self.presets.push(preset);
        }
        Ok(())
    }

    /// Imports an in-memory PNG ribbon strip (works on all platforms including `wasm32`).
    pub fn import_png_bytes(&mut self, id: &str, name: &str, png_bytes: &[u8]) -> std::result::Result<VectorBrushPreset, String> {
        let safe_id = sanitize_preset_id(id);
        let tex_name = format!("{safe_id}.png");
        if !is_safe_single_filename(&tex_name) {
            return Err("invalid brush filename derived from preset id".into());
        }
        let tex = decode_ribbon_png_bytes(png_bytes)?;
        let preset = VectorBrushPreset {
            id: safe_id,
            name: if name.trim().is_empty() { "Custom Texture Brush".into() } else { name.trim().into() },
            category: VectorBrushCategory::TextureRibbon,
            width_profile: vec![0.65, 1.0, 1.12, 0.92, 1.08, 0.70],
            nib_angle_deg: 30.0,
            nib_roundness: 0.45,
            strand_count: 1,
            coherent_roughness: 0.35,
            grain_frequency: 1.5,
            ribbon_texture_png: Some(tex_name.clone()),
            paint_brush_id: None,
        };
        self.insert_or_replace(preset.clone())?;
        self.ribbons.insert(tex_name, tex);
        Ok(preset)
    }

    /// Returns the default user profile directory `<config>/PhotoCraft/vector_brushes` on desktop.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn user_brushes_dir() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME")
            && !dir.is_empty()
        {
            return Some(PathBuf::from(dir).join("PhotoCraft").join("vector_brushes"));
        }
        if let Some(dir) = std::env::var_os("APPDATA")
            && !dir.is_empty()
        {
            return Some(PathBuf::from(dir).join("PhotoCraft").join("vector_brushes"));
        }
        std::env::var_os("HOME").filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".config").join("PhotoCraft").join("vector_brushes"))
    }

    /// Scans `dir` for `*.json` presets (and sibling `*.png` ribbon strips), skipping malformed
    /// or unsafe entries rather than failing the whole scan. Returns the number of loaded presets.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_dir(&mut self, dir: &Path) -> std::result::Result<usize, String> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(0);
        };
        let Ok(canon_dir) = std::fs::canonicalize(dir) else {
            return Ok(0);
        };
        let mut json_paths = Vec::new();
        for entry in entries.take(MAX_DIR_ENTRIES).flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("json")) {
                json_paths.push(path);
            }
        }
        json_paths.sort();

        let mut loaded = 0usize;
        for json_path in json_paths {
            let Ok(meta) = std::fs::metadata(&json_path) else {
                continue;
            };
            if meta.len() > MAX_BRUSH_JSON_BYTES {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&json_path) else {
                continue;
            };
            let Ok(preset) = serde_json::from_str::<VectorBrushPreset>(&text) else {
                continue;
            };
            if preset.validate().is_err() {
                continue;
            }
            if let Some(ref rel_png) = preset.ribbon_texture_png {
                if !is_safe_single_filename(rel_png) {
                    continue;
                }
                let png_path = canon_dir.join(rel_png);
                if let Ok(canon_png) = std::fs::canonicalize(&png_path)
                    && canon_png.starts_with(&canon_dir)
                    && let Ok(bytes) = std::fs::read(&canon_png)
                    && let Ok(tex) = decode_ribbon_png_bytes(&bytes)
                {
                    self.ribbons.insert(rel_png.clone(), tex);
                }
            }
            if self.insert_or_replace(preset).is_ok() {
                loaded += 1;
            }
        }
        Ok(loaded)
    }

    /// Imports a `.json` preset (and optional sibling PNG) or standalone `.png` ribbon strip.
    /// When `storage_dir` is `Some`, persists the preset JSON and ribbon PNG into that folder.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn import_file(&mut self, src_path: &Path, storage_dir: Option<&Path>) -> std::result::Result<VectorBrushPreset, String> {
        let meta = std::fs::metadata(src_path).map_err(|e| format!("cannot read brush file: {e}"))?;
        if !meta.is_file() {
            return Err("brush path is not a regular file".into());
        }
        let ext = src_path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .ok_or_else(|| "brush file must have a .json or .png extension".to_string())?;

        if ext == "json" {
            if meta.len() > MAX_BRUSH_JSON_BYTES {
                return Err(format!("brush JSON exceeds {} bytes", MAX_BRUSH_JSON_BYTES));
            }
            let raw = std::fs::read_to_string(src_path).map_err(|e| format!("failed to read brush JSON: {e}"))?;
            let preset: VectorBrushPreset = serde_json::from_str(&raw).map_err(|e| format!("invalid VectorBrushPreset JSON: {e}"))?;
            preset.validate()?;

            if let Some(ref rel_png) = preset.ribbon_texture_png {
                if !is_safe_single_filename(rel_png) {
                    return Err("ribbonTexturePng must be a safe filename without path traversal".into());
                }
                if let Some(parent) = src_path.parent()
                    && let (Ok(canon_parent), Ok(canon_png)) = (std::fs::canonicalize(parent), std::fs::canonicalize(parent.join(rel_png)))
                    && canon_png.starts_with(&canon_parent)
                {
                    let png_bytes = std::fs::read(&canon_png).map_err(|e| format!("failed to read ribbon PNG `{rel_png}`: {e}"))?;
                    let tex = decode_ribbon_png_bytes(&png_bytes)?;
                    if let Some(dst_dir) = storage_dir {
                        std::fs::create_dir_all(dst_dir).map_err(|e| format!("failed to create brush folder: {e}"))?;
                        std::fs::write(dst_dir.join(rel_png), &png_bytes).map_err(|e| format!("failed to copy ribbon PNG: {e}"))?;
                    }
                    self.ribbons.insert(rel_png.clone(), tex);
                }
            }
            if let Some(dst_dir) = storage_dir {
                std::fs::create_dir_all(dst_dir).map_err(|e| format!("failed to create brush folder: {e}"))?;
                let json_name = format!("{}.json", sanitize_preset_id(&preset.id));
                let pretty = serde_json::to_string_pretty(&preset).map_err(|e| format!("failed to serialize brush JSON: {e}"))?;
                std::fs::write(dst_dir.join(json_name), pretty).map_err(|e| format!("failed to save brush JSON: {e}"))?;
            }
            self.insert_or_replace(preset.clone())?;
            return Ok(preset);
        }

        if ext == "png" {
            if meta.len() > MAX_BRUSH_PNG_BYTES {
                return Err(format!("ribbon PNG exceeds {} bytes", MAX_BRUSH_PNG_BYTES));
            }
            let png_bytes = std::fs::read(src_path).map_err(|e| format!("failed to read ribbon PNG: {e}"))?;
            let stem = src_path.file_stem().and_then(|s| s.to_str()).unwrap_or("custom-ribbon");
            let preset = self.import_png_bytes(stem, stem, &png_bytes)?;
            if let Some(dst_dir) = storage_dir
                && let Some(ref rel_png) = preset.ribbon_texture_png
                && is_safe_single_filename(rel_png)
            {
                std::fs::create_dir_all(dst_dir).map_err(|e| format!("failed to create brush folder: {e}"))?;
                std::fs::write(dst_dir.join(rel_png), &png_bytes).map_err(|e| format!("failed to copy ribbon PNG: {e}"))?;
                let json_name = format!("{}.json", preset.id);
                let pretty = serde_json::to_string_pretty(&preset).map_err(|e| format!("failed to serialize brush JSON: {e}"))?;
                std::fs::write(dst_dir.join(json_name), pretty).map_err(|e| format!("failed to save brush JSON: {e}"))?;
            }
            return Ok(preset);
        }

        Err(format!("unsupported vector brush extension `.{ext}` (expected .json or .png)"))
    }

    #[cfg(target_arch = "wasm32")]
    pub fn load_dir(&mut self, _dir: &std::path::Path) -> std::result::Result<usize, String> {
        Ok(0)
    }

    #[cfg(target_arch = "wasm32")]
    pub fn import_file(&mut self, _src_path: &std::path::Path, _storage_dir: Option<&std::path::Path>) -> std::result::Result<VectorBrushPreset, String> {
        Err("importing vector brush files from disk is not supported in browser builds".into())
    }

    /// Converts a [`VectorBrushPreset`] (optionally linked to a `photocraft-paint` preset by
    /// `paint_brush_id`) into [`BrushSettings`] for dab-chain stamping along a contour.
    pub fn brush_settings_for_preset(&self, preset: &VectorBrushPreset, base_width_px: f32, color: [f32; 4], paint_presets: &[BrushPreset]) -> BrushSettings {
        let mut settings = preset
            .paint_brush_id
            .as_deref()
            .and_then(|id| paint_presets.iter().find(|bp| bp.name.eq_ignore_ascii_case(id)))
            .map(|bp| bp.brush.clone())
            .unwrap_or_default();

        settings.size = if base_width_px.is_finite() { base_width_px.clamp(1.0, 500.0) } else { 4.0 };
        settings.roundness = preset.nib_roundness.clamp(0.05, 1.0);
        settings.angle = if preset.nib_angle_deg.is_finite() { preset.nib_angle_deg } else { 0.0 };
        settings.pressure_size = true;
        settings.color = color;
        settings.spacing = match preset.category {
            VectorBrushCategory::Stipple => 0.40,
            VectorBrushCategory::DryBrush | VectorBrushCategory::TextureRibbon => 0.14,
            _ => 0.12,
        };
        settings.noise = settings.noise || preset.coherent_roughness > 0.25;
        settings
    }

    /// Mode 1 (Dab Chain): samples the contour of `balloon` with
    /// [`sample_balloon_contour_for_brush`] and renders raster dabs via
    /// [`photocraft_paint::apply_stroke`] onto `target`. Returns the damaged rectangle.
    pub fn apply_brush_dab_along_balloon(
        &self,
        target: &mut Surface,
        balloon: &LiveBalloon,
        preset: &VectorBrushPreset,
        color: [f32; 4],
        paint_presets: &[BrushPreset],
    ) -> Rect {
        let step_px = (balloon.stroke_width * 0.45).clamp(1.5, 12.0);
        let rings = sample_balloon_contour_for_brush(balloon, step_px);
        let brush = self.brush_settings_for_preset(preset, balloon.stroke_width as f32, color, paint_presets);

        let mut damaged: Option<Rect> = None;
        for ring in rings {
            if ring.len() < 2 {
                continue;
            }
            let n = ring.len();
            let mut pts = Vec::with_capacity(n + 1);
            for (idx, st) in ring.iter().enumerate() {
                let u = if st.total_len > 1e-6 { (st.arc_len / st.total_len).clamp(0.0, 1.0) as f32 } else { (idx as f32) / (n.max(1) as f32) };
                let profile_w = preset.width_at(u);
                let dry_mod = if preset.coherent_roughness > 0.0 {
                    let phase = u * std::f32::consts::TAU * preset.grain_frequency.max(0.1) * 3.0;
                    1.0 - preset.coherent_roughness * 0.28 * (0.5 + 0.5 * phase.sin())
                } else {
                    1.0
                };
                let pressure = (profile_w * dry_mod).clamp(0.10, 1.0);
                let mut sp = StrokePoint::new(st.pos[0], st.pos[1], pressure);
                sp.rotation = st.tangent[1].atan2(st.tangent[0]).to_degrees().rem_euclid(360.0) as f32;
                pts.push(sp);
            }
            if let Some(first) = pts.first().copied() {
                pts.push(first);
            }
            let r = apply_stroke(target, &Stroke { brush: brush.clone(), points: pts }, None, false);
            if !r.is_empty() {
                damaged = Some(match damaged {
                    None => r,
                    Some(prev) => prev.union(&r),
                });
            }
        }
        damaged.unwrap_or(Rect::EMPTY)
    }
}

fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}

/// `shape.vectorBrush.list`: returns every built-in and imported [`VectorBrushPreset`].
fn vector_brush_list(s: &mut Session, _p: &Value) -> Result<Value> {
    Ok(json!({ "presets": s.manga.vector_brushes.presets() }))
}

/// `shape.vectorBrush.import`: imports a `.json` or `.png` vector brush preset into the session
/// (and optionally user profile storage on desktop).
fn vector_brush_import(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "shape.vectorBrush.import";
    if let Some(raw_preset) = p.get("preset") {
        let preset: VectorBrushPreset = serde_json::from_value(raw_preset.clone()).map_err(|e| bad(CMD, format!("invalid `preset` object: {e}")))?;
        s.manga.vector_brushes.insert_or_replace(preset.clone()).map_err(|e| bad(CMD, e))?;
        return Ok(json!({ "ok": true, "preset": preset }));
    }
    let path_str = p
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| bad(CMD, "provide `path` (.json or .png file) or `preset` object"))?;

    #[cfg(not(target_arch = "wasm32"))]
    {
        let storage_dir = VectorBrushLibrary::user_brushes_dir();
        let preset = s.manga.vector_brushes.import_file(std::path::Path::new(path_str), storage_dir.as_deref()).map_err(|e| bad(CMD, e))?;
        Ok(json!({ "ok": true, "preset": preset }))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = path_str;
        Err(bad(CMD, "importing vector brush files by path is not supported in wasm builds"))
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "shape.vectorBrush.list",
            label: "List Vector Brushes",
            menu: &[],
            shortcut: None,
            params: r##"{} -> {presets:[VectorBrushPreset...]}"##,
            enabled: always,
            run: vector_brush_list,
            journal: false,
        },
        CommandSpec {
            id: "shape.vectorBrush.import",
            label: "Import Vector Brush",
            menu: &[],
            shortcut: None,
            params: r##"{"path":str? | "preset":VectorBrushPreset?} -> {ok:true, preset:VectorBrushPreset}"##,
            enabled: always,
            run: vector_brush_import,
            journal: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_codecs::ChannelLayout;
    use photocraft_color::PixelFormat;
    use photocraft_doc::vector::LiveBalloonBody;

    #[test]
    fn bilinear_ribbon_sampler_interpolates_smoothly() {
        // 2x2 ribbon alpha: left=0.0, right=1.0
        let tex = LoadedRibbonTexture { width: 2, height: 2, alpha: vec![0.0, 1.0, 0.0, 1.0] };
        assert!((tex.sample_bilinear(0.0, -1.0) - 0.0).abs() < 1e-4);
        assert!((tex.sample_bilinear(0.5, 0.0) - 0.5).abs() < 1e-4);
        assert!((tex.sample_bilinear(0.99, 1.0) - 0.99).abs() < 0.02);
    }

    #[test]
    fn png_ribbon_import_roundtrip_and_path_traversal_rejection() {
        let mut lib = VectorBrushLibrary::new();
        assert_eq!(lib.presets().len(), 5);

        // Encode a 16x8 grayscale PNG in memory and import it.
        let mut raw = Vec::with_capacity(16 * 8);
        for y in 0i32..8 {
            for _x in 0..16 {
                let edge_dist = (y - 4).abs();
                let luma = if edge_dist <= 2 { 16u8 } else { 245u8 };
                raw.push(luma);
            }
        }
        let img = Image::from_u8(16, 8, ChannelLayout::Gray, raw).unwrap();
        let png = photocraft_codecs::encode(&img, Format::Png, &photocraft_codecs::EncodeOptions::default()).unwrap();

        let preset = lib.import_png_bytes("dry_charcoal_01", "Dry Charcoal", &png).unwrap();
        assert_eq!(preset.id, "dry-charcoal-01");
        assert!(lib.get("dry-charcoal-01").is_some());
        let tex_key = preset.ribbon_texture_png.as_deref().unwrap();
        let tex = lib.ribbon_texture(tex_key).unwrap();
        assert!(tex.sample_bilinear(0.25, 0.0) > 0.85, "center of stroke should be opaque ink");
        assert!(tex.sample_bilinear(0.25, 1.0) < 0.15, "paper margin should stay transparent");

        // Unsafe preset / traversal must be rejected.
        let mut bad_preset = preset.clone();
        bad_preset.ribbon_texture_png = Some("../escape.png".into());
        assert!(lib.insert_or_replace(bad_preset).is_err());
    }

    #[test]
    fn dab_chain_stamps_visible_pixels_along_balloon_contour() {
        let lib = VectorBrushLibrary::new();
        let fude = lib.get("fukidashi-fude").cloned().unwrap();
        let balloon =
            LiveBalloon { body: LiveBalloonBody::Ellipse { rect: [20.0, 20.0, 120.0, 80.0], squircle: 0.2 }, stroke_width: 4.0, ..LiveBalloon::default() };
        let mut surface = Surface::new(PixelFormat::RGBA8);
        let dmg = lib.apply_brush_dab_along_balloon(&mut surface, &balloon, &fude, [0.0, 0.0, 0.0, 1.0], &[]);
        assert!(dmg.width() > 80 && dmg.height() > 40, "expected dab chain damage across balloon bbox, got {dmg:?}");
        // Near top perimeter (x=80, y=20) alpha should be painted; center (80, 60) stays transparent.
        assert!(surface.rgba(80, 20)[3] > 0.25, "perimeter should have dab coverage");
        assert!(surface.rgba(80, 60)[3] < 0.01, "interior stays clean for Fill");
    }

    #[test]
    fn commands_list_and_import_work_on_session() {
        let mut s = Session::new();
        let list = s.execute("shape.vectorBrush.list", json!({})).unwrap();
        assert!(list["presets"].as_array().is_some_and(|a| a.len() >= 5));

        let custom = VectorBrushPreset { id: "test-nib".into(), name: "Test Nib".into(), ..VectorBrushPreset::fukidashi_fude() };
        let res = s.execute("shape.vectorBrush.import", json!({ "preset": custom })).unwrap();
        assert_eq!(res["ok"].as_bool(), Some(true));
        let list2 = s.execute("shape.vectorBrush.list", json!({})).unwrap();
        assert!(list2["presets"].as_array().unwrap().iter().any(|p| p["id"] == "test-nib"));
    }
}
