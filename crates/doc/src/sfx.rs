//! Non-destructive Manga SFX (onomatopoeia & display lettering) layer model.
//!
//! Pure data and validation only (`L1`): rendering, contour flattening, warps, and rasterization
//! live in `photocraft-manga-core`.

use serde::{Deserialize, Serialize};

use crate::Color;
pub use photocraft_tone::ScreentoneSpec;

/// Maximum supported UTF-8 byte length of an SFX string.
pub const MAX_SFX_TEXT_BYTES: usize = 4096;
/// Maximum number of mesh grid nodes per axis (`cols`, `rows`).
pub const MAX_SFX_MESH_DIM: u8 = 8;
/// Maximum number of per-glyph override entries stored on one layer.
pub const MAX_SFX_PER_GLYPH: usize = 1024;
/// Maximum number of liquify brush displacements stored on one layer.
pub const MAX_SFX_LIQUIFY_STROKES: usize = 512;
/// Maximum number of stacked style passes on one SFX layer.
pub const MAX_SFX_STYLE_PASSES: usize = 16;

fn default_one_f64() -> f64 {
    1.0
}

fn default_one_f32() -> f32 {
    1.0
}

fn default_true() -> bool {
    true
}

fn default_version() -> u32 {
    1
}

fn default_weight() -> u16 {
    700
}

fn default_size_pt() -> f32 {
    48.0
}

fn default_leading_scale() -> f32 {
    1.05
}

fn default_miter_limit() -> f32 {
    4.0
}

fn default_mesh_dim() -> u8 {
    4
}

fn default_extrude_steps() -> u16 {
    12
}

fn default_black() -> Color {
    Color::BLACK
}

fn default_font_family() -> String {
    "Inter".to_owned()
}

fn finite_f64(v: f64, fallback: f64) -> f64 {
    if v.is_finite() { v } else { fallback }
}

fn finite_f32(v: f32, fallback: f32) -> f32 {
    if v.is_finite() { v } else { fallback }
}

fn finite_color(c: Color) -> Color {
    Color { mode: c.mode, c: c.c.map(|v| finite_f32(v, 0.0).clamp(0.0, 1.0)), alpha: finite_f32(c.alpha, 1.0).clamp(0.0, 1.0) }
}

/// Active canvas manipulation mode for the SFX tool (`Mode`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SfxWarpMode {
    #[default]
    FreeTransform,
    PerGlyph,
    MeshLattice,
    LiquifyField,
}

/// Stroke corner join style for Clip-Studio-style vector border expansion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SfxLineJoin {
    Miter,
    #[default]
    Round,
    Bevel,
}

/// Interpolation kernel used by `MeshLattice`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SfxMeshInterpolation {
    Bilinear,
    #[default]
    Bicubic,
}

/// Fill paint for either the base SFX face or an extruded / bordered style pass.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SfxFillSource {
    Solid {
        #[serde(default = "default_black")]
        color: Color,
    },
    Gradient {
        #[serde(default = "default_black")]
        start: Color,
        #[serde(default = "default_black")]
        end: Color,
        #[serde(default)]
        angle_deg: f32,
    },
    Screentone {
        #[serde(default)]
        spec: ScreentoneSpec,
        #[serde(default = "default_one_f32")]
        opacity: f32,
    },
}

impl Default for SfxFillSource {
    fn default() -> Self {
        Self::Solid { color: Color::BLACK }
    }
}

impl SfxFillSource {
    pub fn validate(&mut self) {
        match self {
            Self::Solid { color } => *color = finite_color(*color),
            Self::Gradient { start, end, angle_deg } => {
                *start = finite_color(*start);
                *end = finite_color(*end);
                *angle_deg = finite_f32(*angle_deg, 90.0).rem_euclid(360.0);
            }
            Self::Screentone { spec, opacity } => {
                spec.dot_size = finite_f32(spec.dot_size, 6.0).clamp(0.5, 256.0);
                spec.spacing_x = finite_f32(spec.spacing_x, 16.0).clamp(2.0, 512.0);
                spec.spacing_y = finite_f32(spec.spacing_y, 16.0).clamp(2.0, 512.0);
                spec.density = finite_f32(spec.density, 0.5).clamp(0.0, 1.0);
                spec.angle = finite_f32(spec.angle, 45.0).clamp(-360.0, 360.0);
                spec.line_width = finite_f32(spec.line_width, 1.5).clamp(0.2, 128.0);
                *opacity = finite_f32(*opacity, 1.0).clamp(0.0, 1.0);
            }
        }
    }
}

/// Global affine / skew / mild keystone transform around the SFX local bounds center.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SfxFreeTransform {
    #[serde(default = "default_one_f64")]
    pub scale_x: f64,
    #[serde(default = "default_one_f64")]
    pub scale_y: f64,
    #[serde(default)]
    pub rotation_deg: f64,
    #[serde(default)]
    pub skew_x_deg: f64,
    #[serde(default)]
    pub skew_y_deg: f64,
    #[serde(default)]
    pub perspective_x: f64,
    #[serde(default)]
    pub perspective_y: f64,
}

impl Default for SfxFreeTransform {
    fn default() -> Self {
        Self { scale_x: 1.0, scale_y: 1.0, rotation_deg: 0.0, skew_x_deg: 0.0, skew_y_deg: 0.0, perspective_x: 0.0, perspective_y: 0.0 }
    }
}

impl SfxFreeTransform {
    pub fn validate(&mut self) {
        self.scale_x = finite_f64(self.scale_x, 1.0).clamp(-32.0, 32.0);
        if self.scale_x.abs() < 0.05 {
            self.scale_x = 0.05_f64.copysign(if self.scale_x == 0.0 { 1.0 } else { self.scale_x });
        }
        self.scale_y = finite_f64(self.scale_y, 1.0).clamp(-32.0, 32.0);
        if self.scale_y.abs() < 0.05 {
            self.scale_y = 0.05_f64.copysign(if self.scale_y == 0.0 { 1.0 } else { self.scale_y });
        }
        self.rotation_deg = finite_f64(self.rotation_deg, 0.0).clamp(-3600.0, 3600.0);
        self.skew_x_deg = finite_f64(self.skew_x_deg, 0.0).clamp(-75.0, 75.0);
        self.skew_y_deg = finite_f64(self.skew_y_deg, 0.0).clamp(-75.0, 75.0);
        self.perspective_x = finite_f64(self.perspective_x, 0.0).clamp(-0.75, 0.75);
        self.perspective_y = finite_f64(self.perspective_y, 0.0).clamp(-0.75, 0.75);
    }
}

/// Non-destructive per-glyph transform in unwarped layout space (`px` / degrees).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SfxGlyphTransform {
    pub glyph_index: usize,
    pub dx: f64,
    pub dy: f64,
    #[serde(default = "default_one_f64")]
    pub scale_x: f64,
    #[serde(default = "default_one_f64")]
    pub scale_y: f64,
    pub rotation_deg: f64,
    pub skew_x_deg: f64,
    pub skew_y_deg: f64,
}

impl Default for SfxGlyphTransform {
    fn default() -> Self {
        Self { glyph_index: 0, dx: 0.0, dy: 0.0, scale_x: 1.0, scale_y: 1.0, rotation_deg: 0.0, skew_x_deg: 0.0, skew_y_deg: 0.0 }
    }
}

impl SfxGlyphTransform {
    pub fn validate(&mut self) {
        self.dx = finite_f64(self.dx, 0.0).clamp(-20_000.0, 20_000.0);
        self.dy = finite_f64(self.dy, 0.0).clamp(-20_000.0, 20_000.0);
        self.scale_x = finite_f64(self.scale_x, 1.0).clamp(0.05, 16.0);
        self.scale_y = finite_f64(self.scale_y, 1.0).clamp(0.05, 16.0);
        self.rotation_deg = finite_f64(self.rotation_deg, 0.0).clamp(-3600.0, 3600.0);
        self.skew_x_deg = finite_f64(self.skew_x_deg, 0.0).clamp(-75.0, 75.0);
        self.skew_y_deg = finite_f64(self.skew_y_deg, 0.0).clamp(-75.0, 75.0);
    }

    pub fn is_identity(&self) -> bool {
        self.dx.abs() < 1e-6
            && self.dy.abs() < 1e-6
            && (self.scale_x - 1.0).abs() < 1e-6
            && (self.scale_y - 1.0).abs() < 1e-6
            && self.rotation_deg.abs() < 1e-6
            && self.skew_x_deg.abs() < 1e-6
            && self.skew_y_deg.abs() < 1e-6
    }
}

/// Parametric UV warp lattice (`cols × rows` nodes storing normalized deformed positions `[u, v]`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SfxMeshLattice {
    #[serde(default = "default_mesh_dim")]
    pub cols: u8,
    #[serde(default = "default_mesh_dim")]
    pub rows: u8,
    /// Row-major `rows * cols` control points in normalized local SFX space (identity = `[c/(cols-1), r/(rows-1)]`).
    pub points: Vec<[f64; 2]>,
    #[serde(default)]
    pub interpolation: SfxMeshInterpolation,
    #[serde(default)]
    pub link_h: bool,
    #[serde(default)]
    pub link_v: bool,
}

impl Default for SfxMeshLattice {
    fn default() -> Self {
        Self::identity(4, 4)
    }
}

impl SfxMeshLattice {
    pub fn identity(cols: u8, rows: u8) -> Self {
        let cols = cols.clamp(2, MAX_SFX_MESH_DIM);
        let rows = rows.clamp(2, MAX_SFX_MESH_DIM);
        let points = Self::rest_grid(cols, rows);
        Self { cols, rows, points, interpolation: SfxMeshInterpolation::Bicubic, link_h: false, link_v: false }
    }

    pub fn rest_grid(cols: u8, rows: u8) -> Vec<[f64; 2]> {
        let cols = cols.clamp(2, MAX_SFX_MESH_DIM);
        let rows = rows.clamp(2, MAX_SFX_MESH_DIM);
        let mut pts = Vec::with_capacity(usize::from(cols) * usize::from(rows));
        for r in 0..rows {
            let v = f64::from(r) / f64::from(rows - 1);
            for c in 0..cols {
                let u = f64::from(c) / f64::from(cols - 1);
                pts.push([u, v]);
            }
        }
        pts
    }

    pub fn is_identity(&self) -> bool {
        let rest = Self::rest_grid(self.cols, self.rows);
        self.points.len() == rest.len() && self.points.iter().zip(rest.iter()).all(|(a, b)| (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6)
    }

    pub fn validate(&mut self) {
        self.cols = self.cols.clamp(2, MAX_SFX_MESH_DIM);
        self.rows = self.rows.clamp(2, MAX_SFX_MESH_DIM);
        let expected = usize::from(self.cols) * usize::from(self.rows);
        let rest = Self::rest_grid(self.cols, self.rows);
        if self.points.len() != expected {
            self.points = rest;
            return;
        }
        for (p, r) in self.points.iter_mut().zip(rest.iter()) {
            p[0] = finite_f64(p[0], r[0]).clamp(-3.0, 4.0);
            p[1] = finite_f64(p[1], r[1]).clamp(-3.0, 4.0);
        }
    }
}

/// Non-destructive smooth radial displacement stroke evaluated before Border/Extrude passes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SfxLiquifyBrush {
    /// Center in local layout units relative to the layout bounds origin `[u, v]` (`0..1` inside bounds).
    pub center_uv: [f64; 2],
    /// Peak displacement at `center_uv` in unscaled layout pixels.
    pub delta_px: [f64; 2],
    /// Influence radius in unscaled layout pixels.
    pub radius_px: f64,
    /// Strength multiplier `0.0..=2.0`.
    #[serde(default = "default_one_f64")]
    pub strength: f64,
}

impl Default for SfxLiquifyBrush {
    fn default() -> Self {
        Self { center_uv: [0.5, 0.5], delta_px: [0.0, 0.0], radius_px: 40.0, strength: 1.0 }
    }
}

impl SfxLiquifyBrush {
    pub fn validate(&mut self) {
        self.center_uv[0] = finite_f64(self.center_uv[0], 0.5).clamp(-4.0, 5.0);
        self.center_uv[1] = finite_f64(self.center_uv[1], 0.5).clamp(-4.0, 5.0);
        self.delta_px[0] = finite_f64(self.delta_px[0], 0.0).clamp(-4000.0, 4000.0);
        self.delta_px[1] = finite_f64(self.delta_px[1], 0.0).clamp(-4000.0, 4000.0);
        self.radius_px = finite_f64(self.radius_px, 40.0).clamp(2.0, 4000.0);
        self.strength = finite_f64(self.strength, 1.0).clamp(0.0, 2.0);
    }
}

/// Non-destructive style layer around/inside the merged SFX silhouette.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SfxStylePass {
    /// Vector border expansion outward from the unioned glyph silhouette (`Border Effect`).
    Stroke {
        #[serde(default = "default_true")]
        enabled: bool,
        #[serde(default)]
        width_px: f32,
        #[serde(default)]
        join: SfxLineJoin,
        #[serde(default = "default_miter_limit")]
        miter_limit: f32,
        #[serde(default)]
        fill: SfxFillSource,
    },
    /// Deterministic swept 3D block extrusion along `(dx_px, dy_px)`.
    Extrude3D {
        #[serde(default = "default_true")]
        enabled: bool,
        #[serde(default)]
        dx_px: f32,
        #[serde(default)]
        dy_px: f32,
        #[serde(default = "default_extrude_steps")]
        steps: u16,
        #[serde(default)]
        fill: SfxFillSource,
    },
    /// Drop shadow offset behind the composite SFX stack.
    Shadow {
        #[serde(default = "default_true")]
        enabled: bool,
        #[serde(default)]
        dx_px: f32,
        #[serde(default)]
        dy_px: f32,
        #[serde(default)]
        blur_px: f32,
        #[serde(default = "default_black")]
        color: Color,
        #[serde(default = "default_one_f32")]
        opacity: f32,
    },
    /// Inner edge highlight/glow inside the base fill silhouette.
    InnerGlow {
        #[serde(default = "default_true")]
        enabled: bool,
        #[serde(default)]
        width_px: f32,
        #[serde(default = "default_black")]
        color: Color,
        #[serde(default = "default_one_f32")]
        opacity: f32,
    },
    /// Manga negative/invert mode: composites the fill silhouette via `BlendMode::Difference`.
    NegativeInvert {
        #[serde(default = "default_true")]
        enabled: bool,
    },
}

impl SfxStylePass {
    pub fn validate(&mut self) {
        match self {
            Self::Stroke { width_px, miter_limit, fill, .. } => {
                *width_px = finite_f32(*width_px, 4.0).clamp(0.0, 256.0);
                *miter_limit = finite_f32(*miter_limit, 4.0).clamp(1.0, 24.0);
                fill.validate();
            }
            Self::Extrude3D { dx_px, dy_px, steps, fill, .. } => {
                *dx_px = finite_f32(*dx_px, 6.0).clamp(-512.0, 512.0);
                *dy_px = finite_f32(*dy_px, 6.0).clamp(-512.0, 512.0);
                *steps = (*steps).clamp(1, 128);
                fill.validate();
            }
            Self::Shadow { dx_px, dy_px, blur_px, color, opacity, .. } => {
                *dx_px = finite_f32(*dx_px, 5.0).clamp(-512.0, 512.0);
                *dy_px = finite_f32(*dy_px, 5.0).clamp(-512.0, 512.0);
                *blur_px = finite_f32(*blur_px, 0.0).clamp(0.0, 128.0);
                *color = finite_color(*color);
                *opacity = finite_f32(*opacity, 0.75).clamp(0.0, 1.0);
            }
            Self::InnerGlow { width_px, color, opacity, .. } => {
                *width_px = finite_f32(*width_px, 3.0).clamp(0.0, 128.0);
                *color = finite_color(*color);
                *opacity = finite_f32(*opacity, 0.85).clamp(0.0, 1.0);
            }
            Self::NegativeInvert { .. } => {}
        }
    }
}

/// Deterministic surface & edge weathering (`seed` pinned so re-renders are reproducible).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SfxDistress {
    pub enabled: bool,
    pub seed: u32,
    /// Edge roughness amplitude in pixels (`0.0..=16.0`).
    pub roughness: f32,
    /// Interior ink speckle erosion strength (`0.0..=1.0`).
    pub grain: f32,
    /// Edge chipping density (`0.0..=1.0`).
    pub edge_chip: f32,
}

impl Default for SfxDistress {
    fn default() -> Self {
        Self { enabled: false, seed: 42, roughness: 1.5, grain: 0.25, edge_chip: 0.2 }
    }
}

impl SfxDistress {
    pub fn validate(&mut self) {
        self.roughness = finite_f32(self.roughness, 1.5).clamp(0.0, 16.0);
        self.grain = finite_f32(self.grain, 0.25).clamp(0.0, 1.0);
        self.edge_chip = finite_f32(self.edge_chip, 0.2).clamp(0.0, 1.0);
    }
}

/// Complete non-destructive specification of an SFX layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SfxData {
    #[serde(default = "default_version")]
    pub version: u32,
    pub text: String,
    /// Document-space top-left origin where the unwarped SFX layout starts.
    pub origin: [f64; 2],
    #[serde(default = "default_font_family")]
    pub font_family: String,
    pub font_style: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postscript_name: Option<String>,
    #[serde(default = "default_weight")]
    pub weight: u16,
    pub italic: bool,
    #[serde(default = "default_size_pt")]
    pub size_pt: f32,
    pub tracking: f32,
    #[serde(default = "default_leading_scale")]
    pub leading_scale: f32,
    pub vertical: bool,
    pub warp_mode: SfxWarpMode,
    pub free_transform: SfxFreeTransform,
    #[serde(default)]
    pub per_glyph: Vec<SfxGlyphTransform>,
    pub mesh: SfxMeshLattice,
    #[serde(default)]
    pub liquify_strokes: Vec<SfxLiquifyBrush>,
    pub fill: SfxFillSource,
    #[serde(default)]
    pub styles: Vec<SfxStylePass>,
    pub distress: SfxDistress,
}

impl Default for SfxData {
    fn default() -> Self {
        Self {
            version: 1,
            text: "BAM!".to_owned(),
            origin: [100.0, 100.0],
            font_family: default_font_family(),
            font_style: String::new(),
            postscript_name: None,
            weight: 800,
            italic: false,
            size_pt: 54.0,
            tracking: 0.0,
            leading_scale: 1.05,
            vertical: false,
            warp_mode: SfxWarpMode::FreeTransform,
            free_transform: SfxFreeTransform::default(),
            per_glyph: Vec::new(),
            mesh: SfxMeshLattice::default(),
            liquify_strokes: Vec::new(),
            fill: SfxFillSource::Solid { color: Color::BLACK },
            styles: vec![
                SfxStylePass::Stroke {
                    enabled: true,
                    width_px: 4.0,
                    join: SfxLineJoin::Round,
                    miter_limit: 4.0,
                    fill: SfxFillSource::Solid { color: Color::WHITE },
                },
                SfxStylePass::Stroke {
                    enabled: true,
                    width_px: 6.0,
                    join: SfxLineJoin::Round,
                    miter_limit: 4.0,
                    fill: SfxFillSource::Solid { color: Color::BLACK },
                },
            ],
            distress: SfxDistress::default(),
        }
    }
}

impl SfxData {
    /// Sanitizes numeric fields and caps collections to safe rendering limits.
    pub fn validate(&mut self) {
        if self.version == 0 {
            self.version = 1;
        }
        if self.text.len() > MAX_SFX_TEXT_BYTES {
            let mut cut = MAX_SFX_TEXT_BYTES;
            while cut > 0 && !self.text.is_char_boundary(cut) {
                cut -= 1;
            }
            self.text.truncate(cut);
        }
        self.origin[0] = finite_f64(self.origin[0], 100.0).clamp(-1_000_000.0, 1_000_000.0);
        self.origin[1] = finite_f64(self.origin[1], 100.0).clamp(-1_000_000.0, 1_000_000.0);
        if self.font_family.trim().is_empty() {
            self.font_family = default_font_family();
        }
        self.weight = self.weight.clamp(100, 900);
        self.size_pt = finite_f32(self.size_pt, 48.0).clamp(4.0, 1000.0);
        self.tracking = finite_f32(self.tracking, 0.0).clamp(-500.0, 2000.0);
        self.leading_scale = finite_f32(self.leading_scale, 1.05).clamp(0.4, 4.0);
        self.free_transform.validate();
        if self.per_glyph.len() > MAX_SFX_PER_GLYPH {
            self.per_glyph.truncate(MAX_SFX_PER_GLYPH);
        }
        for g in &mut self.per_glyph {
            g.validate();
        }
        self.per_glyph.retain(|g| !g.is_identity());
        self.mesh.validate();
        if self.liquify_strokes.len() > MAX_SFX_LIQUIFY_STROKES {
            self.liquify_strokes.truncate(MAX_SFX_LIQUIFY_STROKES);
        }
        for s in &mut self.liquify_strokes {
            s.validate();
        }
        self.fill.validate();
        if self.styles.len() > MAX_SFX_STYLE_PASSES {
            self.styles.truncate(MAX_SFX_STYLE_PASSES);
        }
        for pass in &mut self.styles {
            pass.validate();
        }
        self.distress.validate();
    }

    /// Returns whether any enabled pass uses `NegativeInvert`.
    pub fn has_negative_invert(&self) -> bool {
        self.styles.iter().any(|p| matches!(p, SfxStylePass::NegativeInvert { enabled: true }))
    }

    /// Exports only deterministic style parameters (no text/origin/per-instance deform) as a preset.
    pub fn to_preset(&self, name: impl Into<String>) -> SfxPreset {
        SfxPreset {
            name: name.into(),
            font_family: self.font_family.clone(),
            weight: self.weight,
            italic: self.italic,
            size_pt: self.size_pt,
            tracking: self.tracking,
            leading_scale: self.leading_scale,
            vertical: self.vertical,
            fill: self.fill.clone(),
            styles: self.styles.clone(),
            distress: self.distress,
        }
    }

    /// Applies an [`SfxPreset`] onto this instance while preserving text, origin, and warp geometry.
    pub fn apply_preset(&mut self, preset: &SfxPreset) {
        self.font_family = preset.font_family.clone();
        self.weight = preset.weight;
        self.italic = preset.italic;
        self.size_pt = preset.size_pt;
        self.tracking = preset.tracking;
        self.leading_scale = preset.leading_scale;
        self.vertical = preset.vertical;
        self.fill = preset.fill.clone();
        self.styles = preset.styles.clone();
        self.distress = preset.distress;
        self.validate();
    }
}

/// Reusable deterministic JSON preset for SFX styling (`sfx.presets.*`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SfxPreset {
    pub name: String,
    #[serde(default = "default_font_family")]
    pub font_family: String,
    #[serde(default = "default_weight")]
    pub weight: u16,
    pub italic: bool,
    #[serde(default = "default_size_pt")]
    pub size_pt: f32,
    pub tracking: f32,
    #[serde(default = "default_leading_scale")]
    pub leading_scale: f32,
    pub vertical: bool,
    pub fill: SfxFillSource,
    #[serde(default)]
    pub styles: Vec<SfxStylePass>,
    pub distress: SfxDistress,
}

impl Default for SfxPreset {
    fn default() -> Self {
        SfxData::default().to_preset("Manga Impact")
    }
}

impl SfxPreset {
    pub fn validate(&mut self) {
        if self.name.trim().is_empty() {
            self.name = "Untitled SFX".to_owned();
        }
        let mut tmp = SfxData::default();
        tmp.apply_preset(self);
        self.font_family = tmp.font_family;
        self.weight = tmp.weight;
        self.size_pt = tmp.size_pt;
        self.tracking = tmp.tracking;
        self.leading_scale = tmp.leading_scale;
        self.fill = tmp.fill;
        self.styles = tmp.styles;
        self.distress = tmp.distress;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sfx_data_roundtrips_and_validates_nans() {
        let mut data = SfxData {
            text: "ドカーン".to_owned(),
            origin: [f64::NAN, 42.0],
            size_pt: f32::INFINITY,
            mesh: SfxMeshLattice {
                cols: 20,
                rows: 1,
                points: vec![[f64::NAN, 0.5]],
                interpolation: SfxMeshInterpolation::Bicubic,
                link_h: true,
                link_v: false,
            },
            ..SfxData::default()
        };
        data.validate();
        assert!(data.origin[0].is_finite());
        assert_eq!(data.size_pt, 48.0);
        assert_eq!(data.mesh.cols, MAX_SFX_MESH_DIM);
        assert_eq!(data.mesh.rows, 2);
        assert_eq!(data.mesh.points.len(), usize::from(MAX_SFX_MESH_DIM) * 2);

        let json = serde_json::to_value(&data).unwrap();
        let back: SfxData = serde_json::from_value(json).unwrap();
        assert_eq!(back, data);
    }
}
