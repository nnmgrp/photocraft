//! Extensible vector brush presets for manga ballons and SAI-style vector strokes.

use serde::{Deserialize, Serialize};

/// Category of a vector stroke brush preset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VectorBrushCategory {
    #[default]
    Pen,
    Pencil,
    CalligraphicBrush,
    DryBrush,
    TextureRibbon,
    Stipple,
}

impl VectorBrushCategory {
    /// Stable lowercase identifier for commands and serialization.
    pub fn id(self) -> &'static str {
        match self {
            Self::Pen => "pen",
            Self::Pencil => "pencil",
            Self::CalligraphicBrush => "calligraphicBrush",
            Self::DryBrush => "dryBrush",
            Self::TextureRibbon => "textureRibbon",
            Self::Stipple => "stipple",
        }
    }

    /// Parses a preset category identifier.
    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "pen" | "Pen" => Some(Self::Pen),
            "pencil" | "Pencil" => Some(Self::Pencil),
            "calligraphicBrush" | "CalligraphicBrush" | "brush" => Some(Self::CalligraphicBrush),
            "dryBrush" | "DryBrush" | "sumi" => Some(Self::DryBrush),
            "textureRibbon" | "TextureRibbon" | "ribbon" => Some(Self::TextureRibbon),
            "stipple" | "Stipple" => Some(Self::Stipple),
            _ => None,
        }
    }
}

/// Parametric and texture-backed vector brush definition.
///
/// Used both by [`crate::vector::LiveBalloon`] (to drive Fukidashi-style ink ribbons and
/// coherent graphite strokes) and by external `.json` / `.png` brush library presets loaded at
/// runtime on native platforms.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VectorBrushPreset {
    pub id: String,
    pub name: String,
    pub category: VectorBrushCategory,
    /// Relative stroke width envelope sampled along normalized arc position `0.0..=1.0`.
    pub width_profile: Vec<f32>,
    /// Calligraphic nib angle in degrees (`0.0..360.0`, e.g. `35.0` for Japanese fude).
    pub nib_angle_deg: f32,
    /// Nib aspect ratio (`0.05..=1.0`, `1.0` = isotropic circular nib).
    pub nib_roundness: f32,
    /// Number of coherent graphite/bristle strands (`1..=12`).
    pub strand_count: u8,
    /// Multi-harmonic coherent roughness amplitude (`0.0..=1.0`).
    pub coherent_roughness: f32,
    /// Spatial grain frequency multiplier (`0.1..=10.0`).
    pub grain_frequency: f32,
    /// Optional relative path to a grayscale alpha ribbon/tip PNG (`vector_brushes/*.png`).
    pub ribbon_texture_png: Option<String>,
    /// Optional ID of a companion `photocraft-paint` brush preset for dab-chain stamping.
    pub paint_brush_id: Option<String>,
}

impl Default for VectorBrushPreset {
    fn default() -> Self {
        Self::g_pen()
    }
}

impl VectorBrushPreset {
    /// Clean uniform G-Pen preset.
    pub fn g_pen() -> Self {
        Self {
            id: "g-pen".to_string(),
            name: "G-Pen".to_string(),
            category: VectorBrushCategory::Pen,
            width_profile: vec![0.92, 1.0, 1.0, 0.94],
            nib_angle_deg: 0.0,
            nib_roundness: 1.0,
            strand_count: 1,
            coherent_roughness: 0.0,
            grain_frequency: 1.0,
            ribbon_texture_png: None,
            paint_brush_id: None,
        }
    }

    /// Calligraphic manga Fukidashi fude brush (`35°` nib + tapered organic modulation).
    pub fn fukidashi_fude() -> Self {
        Self {
            id: "fukidashi-fude".to_string(),
            name: "Fukidashi Fude".to_string(),
            category: VectorBrushCategory::CalligraphicBrush,
            width_profile: vec![0.45, 0.95, 1.18, 0.85, 1.24, 0.72, 1.10, 0.50],
            nib_angle_deg: 35.0,
            nib_roundness: 0.42,
            strand_count: 1,
            coherent_roughness: 0.48,
            grain_frequency: 1.4,
            ribbon_texture_png: None,
            paint_brush_id: None,
        }
    }

    /// Multi-strand soft 2B graphite pencil preset.
    pub fn soft_pencil_2b() -> Self {
        Self {
            id: "soft-pencil-2b".to_string(),
            name: "Soft Pencil 2B".to_string(),
            category: VectorBrushCategory::Pencil,
            width_profile: vec![0.75, 1.0, 0.90, 1.05, 0.80],
            nib_angle_deg: 25.0,
            nib_roundness: 0.65,
            strand_count: 4,
            coherent_roughness: 0.55,
            grain_frequency: 2.2,
            ribbon_texture_png: None,
            paint_brush_id: None,
        }
    }

    /// Dry-ink Sumi brush with stronger edge break-up and calligraphic swell.
    pub fn dry_sumi() -> Self {
        Self {
            id: "dry-sumi".to_string(),
            name: "Dry Sumi".to_string(),
            category: VectorBrushCategory::DryBrush,
            width_profile: vec![0.60, 1.20, 0.78, 1.32, 0.68, 1.15, 0.55],
            nib_angle_deg: 42.0,
            nib_roundness: 0.32,
            strand_count: 2,
            coherent_roughness: 0.75,
            grain_frequency: 2.8,
            ribbon_texture_png: None,
            paint_brush_id: None,
        }
    }

    /// Broad uniform felt marker preset.
    pub fn marker() -> Self {
        Self {
            id: "marker".to_string(),
            name: "Marker".to_string(),
            category: VectorBrushCategory::Pen,
            width_profile: vec![0.96, 1.02, 0.98, 1.0],
            nib_angle_deg: 15.0,
            nib_roundness: 0.85,
            strand_count: 1,
            coherent_roughness: 0.08,
            grain_frequency: 1.0,
            ribbon_texture_png: None,
            paint_brush_id: None,
        }
    }

    /// All built-in vector brush presets.
    pub fn builtin_presets() -> Vec<Self> {
        vec![Self::g_pen(), Self::fukidashi_fude(), Self::soft_pencil_2b(), Self::dry_sumi(), Self::marker()]
    }

    /// Finds a built-in preset by `id`.
    pub fn builtin_by_id(id: &str) -> Option<Self> {
        Self::builtin_presets().into_iter().find(|p| p.id == id)
    }

    /// Evaluates the relative width multiplier at normalized stroke position `t in 0.0..=1.0`.
    pub fn width_at(&self, t: f32) -> f32 {
        if self.width_profile.is_empty() || !t.is_finite() {
            return 1.0;
        }
        let n = self.width_profile.len();
        if n == 1 {
            let v = self.width_profile[0];
            return if v.is_finite() { v.clamp(0.05, 4.0) } else { 1.0 };
        }
        let u = t.clamp(0.0, 1.0) * (n - 1) as f32;
        let idx = (u.floor() as usize).min(n - 2);
        let frac = (u - idx as f32).clamp(0.0, 1.0);
        let a = self.width_profile[idx];
        let b = self.width_profile[idx + 1];
        let va = if a.is_finite() { a.clamp(0.05, 4.0) } else { 1.0 };
        let vb = if b.is_finite() { b.clamp(0.05, 4.0) } else { 1.0 };
        va + (vb - va) * frac
    }

    /// Validates preset parameters for safe rendering and command admission.
    pub fn validate(&self) -> Result<(), String> {
        let id = self.id.trim();
        if id.is_empty() || id.chars().count() > 128 {
            return Err("vector brush `id` must be 1..=128 characters".into());
        }
        if id.contains('/') || id.contains('\\') || id.contains("..") {
            return Err("vector brush `id` cannot contain path separators".into());
        }
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 128 {
            return Err("vector brush `name` must be 1..=128 characters".into());
        }
        if !self.nib_angle_deg.is_finite() {
            return Err("vector brush `nibAngleDeg` must be finite".into());
        }
        if !self.nib_roundness.is_finite() || !(0.05..=1.0).contains(&self.nib_roundness) {
            return Err("vector brush `nibRoundness` must be within 0.05..=1.0".into());
        }
        if !(1..=12).contains(&self.strand_count) {
            return Err("vector brush `strandCount` must be within 1..=12".into());
        }
        if !self.coherent_roughness.is_finite() || !(0.0..=1.0).contains(&self.coherent_roughness) {
            return Err("vector brush `coherentRoughness` must be within 0.0..=1.0".into());
        }
        if !self.grain_frequency.is_finite() || !(0.1..=10.0).contains(&self.grain_frequency) {
            return Err("vector brush `grainFrequency` must be within 0.1..=10.0".into());
        }
        if self.width_profile.len() > 256 {
            return Err("vector brush `widthProfile` exceeds 256 samples".into());
        }
        for &w in &self.width_profile {
            if !w.is_finite() || !(0.05..=4.0).contains(&w) {
                return Err("vector brush `widthProfile` values must be finite within 0.05..=4.0".into());
            }
        }
        if let Some(ref tex) = self.ribbon_texture_png
            && (tex.trim().is_empty() || tex.contains("..") || tex.starts_with('/') || tex.starts_with('\\') || tex.contains(':'))
        {
            return Err("vector brush `ribbonTexturePng` must be a safe relative filename".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_presets_validate_and_roundtrip() {
        let presets = VectorBrushPreset::builtin_presets();
        assert_eq!(presets.len(), 5);
        for p in &presets {
            assert!(p.validate().is_ok(), "builtin preset {} failed validation", p.id);
            let json = serde_json::to_string(p).unwrap();
            let back: VectorBrushPreset = serde_json::from_str(&json).unwrap();
            assert_eq!(back, *p);
            let w0 = p.width_at(0.0);
            let w_mid = p.width_at(0.5);
            assert!(w0.is_finite() && w_mid.is_finite());
        }
        assert!(VectorBrushPreset::builtin_by_id("fukidashi-fude").is_some());
    }

    #[test]
    fn preset_validation_rejects_path_traversal_and_bad_bounds() {
        let mut p = VectorBrushPreset::fukidashi_fude();
        p.ribbon_texture_png = Some("../secret.png".into());
        assert!(p.validate().is_err());

        p.ribbon_texture_png = Some("ok_ribbon.png".into());
        p.nib_roundness = 0.01;
        assert!(p.validate().is_err());

        p.nib_roundness = 0.4;
        p.width_profile = vec![f32::NAN];
        assert!(p.validate().is_err());
    }
}
