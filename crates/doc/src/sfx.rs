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
/// Maximum number of Catmull-Rom arc spline control points stored on one layer.
pub const MAX_SFX_SPLINE_POINTS: usize = 64;
/// Safe upper bound for pixel-sized effect fields on 600-DPI / gigapixel canvases.
///
/// Small canvases keep their original values; this only lifts the old low ceilings so an artist
/// can author lettering on a 12303×17499 document without effects becoming invisible.
pub const MAX_SFX_EFFECT_PX: f32 = 16_384.0;
/// Document min-side (px) at which new SFX defaults start to scale up.
pub const SFX_REFERENCE_MIN_SIDE: f64 = 1200.0;
/// Maximum point size of a newly created SFX layer.
pub const MAX_SFX_SIZE_PT: f32 = 10_000.0;

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

fn default_white() -> Color {
    Color::WHITE
}

fn default_ink() -> Color {
    Color::rgb(15.0 / 255.0, 23.0 / 255.0, 42.0 / 255.0)
}

fn default_font_family() -> String {
    "Inter".to_owned()
}

fn default_seed_one() -> u32 {
    1
}

fn default_55() -> f32 {
    55.0
}

fn default_12() -> f32 {
    12.0
}

fn default_7() -> f32 {
    7.0
}

fn default_18_u16() -> u16 {
    18
}

fn default_45() -> f32 {
    45.0
}

fn default_78() -> f32 {
    78.0
}

fn default_neg35() -> f32 {
    -35.0
}

fn default_6() -> f32 {
    6.0
}

fn default_85() -> f32 {
    85.0
}

fn default_35() -> f32 {
    35.0
}

fn default_60() -> f32 {
    60.0
}

/// Default number of independent sketch passes for the Shtack brush.
fn default_5_u8() -> u8 {
    5
}

fn default_20() -> f32 {
    20.0
}

fn default_14() -> f32 {
    14.0
}

fn default_26() -> f32 {
    26.0
}

fn default_44() -> f32 {
    44.0
}

fn default_32() -> f32 {
    32.0
}

fn default_40() -> f32 {
    40.0
}

fn default_42() -> f32 {
    42.0
}

fn default_neg24() -> f32 {
    -24.0
}

fn default_16_u16() -> u16 {
    16
}

fn default_4() -> f32 {
    4.0
}

fn default_3() -> f32 {
    3.0
}

fn default_52() -> f32 {
    52.0
}

fn finite_f64(v: f64, fallback: f64) -> f64 {
    if v.is_finite() { v } else { fallback }
}

fn finite_f32(v: f32, fallback: f32) -> f32 {
    if v.is_finite() { v } else { fallback }
}

fn finite_color(c: Color) -> Color {
    Color {
        mode: c.mode,
        c: c.c.map(|v| finite_f32(v, 0.0).clamp(0.0, 1.0)),
        alpha: finite_f32(c.alpha, 1.0).clamp(0.0, 1.0),
    }
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

/// 6 stylized manga ink outline / line modes from the SFX mockup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SfxLineStyle {
    #[default]
    Clean,
    OneBit,
    Spikes,
    GPenNib,
    BrushStroke,
    Shtack,
}

/// Geometric shape of contour spikes in [`SfxLineStyle::Spikes`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SfxSpikeShape {
    #[default]
    Triangle,
    Square,
    Circle,
}

/// Two hand-drawn submodes of the [`SfxLineStyle::Shtack`] brush.
///
/// Both submodes share the same multi-pass tapered-stroke geometry engine: merged
/// skeleton strokes, tangents overshooting at both ends, and several independent
/// organic passes per stroke. They differ only in finish.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SfxShtackVariant {
    /// Mode 1: sharp PaintTool SAI pen ink — long razor needles and crisp tapered passes.
    /// Legacy documents used the name `caps`, which is kept as a serde alias.
    #[default]
    #[serde(alias = "caps")]
    SaiPenSketch,
    /// Mode 2: dry pencil — stubby needles, micro-jitter and paper-grain density breakup.
    /// Legacy documents used the name `ribbon`, which is kept as a serde alias.
    #[serde(alias = "ribbon")]
    DryPencil,
}

/// 4 Rust-ready Dry Brush fill engines from the SFX mockup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SfxDryBrushEngine {
    /// Engine A: SDF Physics (2-pass Felzenszwalb EDT + paper grain).
    SdfPhysics,
    /// Engine B1: Segmented ink bristle strokes along stroke direction.
    #[default]
    SdfSeg,
    /// Engine B2: Structure-tensor ETF + Line Integral Convolution (LIC).
    SdfEtf,
    /// Engine 4: Vector multi-bristle streaks + Box-Muller Gaussian ink dots.
    Vector,
}

/// Complete non-destructive specification of the 6-mode Line/Outline & Halo pass.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SfxLineEffect {
    pub style: SfxLineStyle,
    /// Outer halo width (`Ореол`, px) rendered in Pass 1 behind all glyph ink.
    pub halo_width_px: f32,
    #[serde(default = "default_white")]
    pub halo_color: Color,
    /// Inner outline / ink stroke width (`Лайн`, px).
    pub line_width_px: f32,
    #[serde(default = "default_ink")]
    pub line_color: Color,

    // --- 1-Bit ---
    #[serde(default = "default_55")]
    pub one_bit_intensity: f32,

    // --- Spikes (Шипы) ---
    pub spike_shape: SfxSpikeShape,
    #[serde(default = "default_12")]
    pub spike_length_px: f32,
    #[serde(default = "default_7")]
    pub spike_size_px: f32,
    #[serde(default = "default_18_u16")]
    pub spike_count: u16,
    #[serde(default = "default_45")]
    pub spike_random_pct: f32,
    #[serde(default = "default_seed_one")]
    pub spike_seed: u32,

    // --- G-Pen Nib (✒ G-Pen) ---
    #[serde(default = "default_78")]
    pub gpen_contrast_pct: f32,
    #[serde(default = "default_neg35")]
    pub gpen_angle_deg: f32,
    #[serde(default = "default_45")]
    pub gpen_pinch_pct: f32,
    #[serde(default = "default_6")]
    pub gpen_overshoot_px: f32,
    #[serde(default = "default_85")]
    pub gpen_overshoot_count_pct: f32,
    #[serde(default = "default_45")]
    pub gpen_overshoot_random_pct: f32,
    #[serde(default = "default_seed_one")]
    pub gpen_seed: u32,

    // --- Brush Stroke (🖌 Кисть-штрих) ---
    #[serde(default = "default_45")]
    pub ink_brush_roughness_pct: f32,
    #[serde(default = "default_35")]
    pub ink_brush_dropout_pct: f32,
    #[serde(default = "default_60")]
    pub ink_brush_pressure_pct: f32,
    #[serde(default = "default_7")]
    pub ink_brush_overshoot_px: f32,
    #[serde(default = "default_85")]
    pub ink_brush_overshoot_count_pct: f32,
    #[serde(default = "default_45")]
    pub ink_brush_overshoot_random_pct: f32,
    #[serde(default = "default_seed_one")]
    pub ink_brush_seed: u32,

    // --- SHTACK (⚡ SAI Pen Sketch / Dry Pencil) ---
    pub shtack_variant: SfxShtackVariant,
    /// Number of independent variable-width tapered sketch passes per stroke (3..=6).
    #[serde(default = "default_5_u8")]
    pub shtack_bristles: u8,
    #[serde(default = "default_78")]
    pub shtack_split_pct: f32,
    #[serde(default = "default_20")]
    pub shtack_overshoot_px: f32,
    #[serde(default = "default_14")]
    pub shtack_thickness_px: f32,
    #[serde(default = "default_26")]
    pub shtack_dryness_pct: f32,
    #[serde(default = "default_seed_one")]
    pub shtack_seed: u32,
    /// `false` (default) = hollow sketch contour: `ЗАЛИВКА (FILL)` colours the Shtack strokes
    /// themselves and the letter interior stays transparent. `true` = fill the letter interior
    /// with `ЗАЛИВКА (FILL)` and draw the Shtack strokes on top in `line_color`.
    #[serde(default)]
    pub shtack_fill_interior: bool,
}

impl Default for SfxLineEffect {
    fn default() -> Self {
        Self {
            style: SfxLineStyle::Clean,
            halo_width_px: 0.0,
            halo_color: default_white(),
            line_width_px: 0.0,
            line_color: default_ink(),
            one_bit_intensity: 55.0,
            spike_shape: SfxSpikeShape::Triangle,
            spike_length_px: 12.0,
            spike_size_px: 7.0,
            spike_count: 18,
            spike_random_pct: 45.0,
            spike_seed: 1,
            gpen_contrast_pct: 78.0,
            gpen_angle_deg: -35.0,
            gpen_pinch_pct: 45.0,
            gpen_overshoot_px: 6.0,
            gpen_overshoot_count_pct: 85.0,
            gpen_overshoot_random_pct: 45.0,
            gpen_seed: 1,
            ink_brush_roughness_pct: 45.0,
            ink_brush_dropout_pct: 35.0,
            ink_brush_pressure_pct: 60.0,
            ink_brush_overshoot_px: 7.0,
            ink_brush_overshoot_count_pct: 85.0,
            ink_brush_overshoot_random_pct: 45.0,
            ink_brush_seed: 1,
            shtack_variant: SfxShtackVariant::SaiPenSketch,
            shtack_bristles: 5,
            shtack_split_pct: 78.0,
            shtack_overshoot_px: 20.0,
            shtack_thickness_px: 14.0,
            shtack_dryness_pct: 26.0,
            shtack_seed: 1,
            shtack_fill_interior: false,
        }
    }
}

impl SfxLineEffect {
    pub fn validate(&mut self) {
        self.halo_width_px = finite_f32(self.halo_width_px, 0.0).clamp(0.0, MAX_SFX_EFFECT_PX);
        self.halo_color = finite_color(self.halo_color);
        self.line_width_px = finite_f32(self.line_width_px, 0.0).clamp(0.0, MAX_SFX_EFFECT_PX);
        self.line_color = finite_color(self.line_color);

        self.one_bit_intensity = finite_f32(self.one_bit_intensity, 55.0).clamp(0.0, 100.0);

        self.spike_length_px = finite_f32(self.spike_length_px, 12.0).clamp(2.0, MAX_SFX_EFFECT_PX);
        self.spike_size_px = finite_f32(self.spike_size_px, 7.0).clamp(2.0, MAX_SFX_EFFECT_PX);
        self.spike_count = self.spike_count.clamp(2, 40);
        self.spike_random_pct = finite_f32(self.spike_random_pct, 45.0).clamp(0.0, 100.0);

        self.gpen_contrast_pct = finite_f32(self.gpen_contrast_pct, 78.0).clamp(0.0, 100.0);
        self.gpen_angle_deg = finite_f32(self.gpen_angle_deg, -35.0).clamp(-90.0, 90.0);
        self.gpen_pinch_pct = finite_f32(self.gpen_pinch_pct, 45.0).clamp(0.0, 100.0);
        self.gpen_overshoot_px = finite_f32(self.gpen_overshoot_px, 6.0).clamp(0.0, MAX_SFX_EFFECT_PX);
        self.gpen_overshoot_count_pct = finite_f32(self.gpen_overshoot_count_pct, 85.0).clamp(0.0, 100.0);
        self.gpen_overshoot_random_pct = finite_f32(self.gpen_overshoot_random_pct, 45.0).clamp(0.0, 100.0);

        self.ink_brush_roughness_pct = finite_f32(self.ink_brush_roughness_pct, 45.0).clamp(0.0, 100.0);
        self.ink_brush_dropout_pct = finite_f32(self.ink_brush_dropout_pct, 35.0).clamp(0.0, 100.0);
        self.ink_brush_pressure_pct = finite_f32(self.ink_brush_pressure_pct, 60.0).clamp(0.0, 100.0);
        self.ink_brush_overshoot_px = finite_f32(self.ink_brush_overshoot_px, 7.0).clamp(0.0, MAX_SFX_EFFECT_PX);
        self.ink_brush_overshoot_count_pct = finite_f32(self.ink_brush_overshoot_count_pct, 85.0).clamp(0.0, 100.0);
        self.ink_brush_overshoot_random_pct = finite_f32(self.ink_brush_overshoot_random_pct, 45.0).clamp(0.0, 100.0);

        self.shtack_bristles = self.shtack_bristles.clamp(3, 6);
        self.shtack_split_pct = finite_f32(self.shtack_split_pct, 78.0).clamp(0.0, 100.0);
        // Needle overshoot is now a PERCENT of the glyph height (0..=60%), not a pixel length.
        self.shtack_overshoot_px = finite_f32(self.shtack_overshoot_px, 20.0).clamp(0.0, 60.0);
        self.shtack_thickness_px = finite_f32(self.shtack_thickness_px, 14.0).clamp(6.0, MAX_SFX_EFFECT_PX);
        self.shtack_dryness_pct = finite_f32(self.shtack_dryness_pct, 26.0).clamp(0.0, 100.0);
    }
}

/// Parameters for the 4 Dry Brush fill engines (preserved when toggling Solid / DryBrush / Screentone).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SfxDryBrushSpec {
    pub enabled: bool,
    pub engine: SfxDryBrushEngine,
    #[serde(default = "default_44")]
    pub dryness_pct: f32,
    #[serde(default = "default_60")]
    pub freq_pct: f32,
    #[serde(default = "default_14")]
    pub lic_length_px: f32,
    #[serde(default = "default_32")]
    pub warp_pct: f32,
    #[serde(default = "default_40")]
    pub paper_grain_pct: f32,
    #[serde(default = "default_42")]
    pub edge_wear_pct: f32,
    #[serde(default = "default_neg24")]
    pub angle_deg: f32,
    #[serde(default = "default_seed_one")]
    pub seed: u32,
    // --- Engine 4: Vector ---
    #[serde(default = "default_16_u16")]
    pub vec_bristle_count: u16,
    #[serde(default = "default_45")]
    pub vec_bristle_random_pct: f32,
    #[serde(default = "default_4")]
    pub vec_mutual_tilt_deg: f32,
    #[serde(default = "default_3")]
    pub vec_tilt_random_deg: f32,
    #[serde(default = "default_40")]
    pub vec_gauss_dots_pct: f32,
}

impl Default for SfxDryBrushSpec {
    fn default() -> Self {
        Self {
            enabled: false,
            engine: SfxDryBrushEngine::SdfSeg,
            dryness_pct: 44.0,
            freq_pct: 60.0,
            lic_length_px: 14.0,
            warp_pct: 32.0,
            paper_grain_pct: 40.0,
            edge_wear_pct: 42.0,
            angle_deg: -24.0,
            seed: 1,
            vec_bristle_count: 16,
            vec_bristle_random_pct: 45.0,
            vec_mutual_tilt_deg: 4.0,
            vec_tilt_random_deg: 3.0,
            vec_gauss_dots_pct: 40.0,
        }
    }
}

impl SfxDryBrushSpec {
    pub fn validate(&mut self) {
        self.dryness_pct = finite_f32(self.dryness_pct, 44.0).clamp(0.0, 100.0);
        self.freq_pct = finite_f32(self.freq_pct, 60.0).clamp(10.0, 100.0);
        self.lic_length_px = finite_f32(self.lic_length_px, 14.0).clamp(4.0, MAX_SFX_EFFECT_PX);
        self.warp_pct = finite_f32(self.warp_pct, 32.0).clamp(0.0, 100.0);
        self.paper_grain_pct = finite_f32(self.paper_grain_pct, 40.0).clamp(0.0, 100.0);
        self.edge_wear_pct = finite_f32(self.edge_wear_pct, 42.0).clamp(0.0, 100.0);
        self.angle_deg = finite_f32(self.angle_deg, -24.0).clamp(-90.0, 90.0);
        self.vec_bristle_count = self.vec_bristle_count.clamp(4, 40);
        self.vec_bristle_random_pct = finite_f32(self.vec_bristle_random_pct, 45.0).clamp(0.0, 100.0);
        self.vec_mutual_tilt_deg = finite_f32(self.vec_mutual_tilt_deg, 4.0).clamp(-10.0, 10.0);
        self.vec_tilt_random_deg = finite_f32(self.vec_tilt_random_deg, 3.0).clamp(0.0, 10.0);
        self.vec_gauss_dots_pct = finite_f32(self.vec_gauss_dots_pct, 40.0).clamp(0.0, 100.0);
    }
}

/// Interactive vector shadow / 3D extrude controlled by the on-canvas `☀ Shadow` gizmo.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SfxVectorShadow {
    pub enabled: bool,
    #[serde(default = "default_52")]
    pub angle_deg: f32,
    pub distance_px: f32,
    pub extrude_depth_px: f32,
    #[serde(default = "default_ink")]
    pub color: Color,
}

impl Default for SfxVectorShadow {
    fn default() -> Self {
        Self {
            enabled: false,
            angle_deg: 52.0,
            distance_px: 0.0,
            extrude_depth_px: 0.0,
            color: default_ink(),
        }
    }
}

impl SfxVectorShadow {
    pub fn validate(&mut self) {
        self.angle_deg = finite_f32(self.angle_deg, 52.0).clamp(-360.0, 360.0);
        self.distance_px = finite_f32(self.distance_px, 0.0).clamp(0.0, MAX_SFX_EFFECT_PX);
        self.extrude_depth_px = finite_f32(self.extrude_depth_px, 0.0).clamp(0.0, MAX_SFX_EFFECT_PX);
        self.color = finite_color(self.color);
    }

    /// Shadow displacement vector `(dx, dy)` in pixels.
    pub fn offset_px(&self) -> [f64; 2] {
        let rad = f64::from(self.angle_deg).to_radians();
        let d = f64::from(self.distance_px);
        [rad.cos() * d, rad.sin() * d]
    }
}

/// Catmull-Rom baseline arc spline (`points` in local layout pixels relative to `rest_box` center).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SfxSplineArc {
    #[serde(default = "default_true")]
    pub show_arc: bool,
    /// Control points `[lx, ly]` in local layout pixels relative to rest bounds center.
    /// Empty or `< 2` points means straight baseline (identity).
    #[serde(default)]
    pub points: Vec<[f64; 2]>,
}

impl Default for SfxSplineArc {
    fn default() -> Self {
        Self { show_arc: true, points: Vec::new() }
    }
}

impl SfxSplineArc {
    pub fn is_curved(&self) -> bool {
        self.points.len() >= 2 && self.points.iter().any(|p| p[1].abs() > 1e-3)
    }

    pub fn straighten(&mut self) {
        for p in &mut self.points {
            p[1] = 0.0;
        }
    }

    pub fn validate(&mut self) {
        if self.points.len() > MAX_SFX_SPLINE_POINTS {
            self.points.truncate(MAX_SFX_SPLINE_POINTS);
        }
        for p in &mut self.points {
            p[0] = finite_f64(p[0], 0.0).clamp(-20_000.0, 20_000.0);
            p[1] = finite_f64(p[1], 0.0).clamp(-20_000.0, 20_000.0);
        }
    }
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

/// Global affine / skew / keystone / 4-corner homography transform around the SFX local bounds center.
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
    /// Independent 4-corner projective deltas `[TL, TR, BR, BL]` in local layout pixels (`FT 4-Corner`).
    #[serde(default)]
    pub corners: [[f64; 2]; 4],
}

impl Default for SfxFreeTransform {
    fn default() -> Self {
        Self {
            scale_x: 1.0,
            scale_y: 1.0,
            rotation_deg: 0.0,
            skew_x_deg: 0.0,
            skew_y_deg: 0.0,
            perspective_x: 0.0,
            perspective_y: 0.0,
            corners: [[0.0, 0.0]; 4],
        }
    }
}

impl SfxFreeTransform {
    pub fn has_corner_warp(&self) -> bool {
        self.corners.iter().any(|c| c[0].abs() > 1e-5 || c[1].abs() > 1e-5)
    }

    pub fn reset_corners(&mut self) {
        self.corners = [[0.0, 0.0]; 4];
    }

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
        for c in &mut self.corners {
            c[0] = finite_f64(c[0], 0.0).clamp(-20_000.0, 20_000.0);
            c[1] = finite_f64(c[1], 0.0).clamp(-20_000.0, 20_000.0);
        }
    }
}

/// Non-destructive per-glyph transform in unwarped layout space (`px` / degrees / 4-corner FT).
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
    /// Independent 4-corner projective deltas `[TL, TR, BR, BL]` in local glyph pixels.
    #[serde(default)]
    pub corners: [[f64; 2]; 4],
}

impl Default for SfxGlyphTransform {
    fn default() -> Self {
        Self {
            glyph_index: 0,
            dx: 0.0,
            dy: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            rotation_deg: 0.0,
            skew_x_deg: 0.0,
            skew_y_deg: 0.0,
            corners: [[0.0, 0.0]; 4],
        }
    }
}

impl SfxGlyphTransform {
    pub fn has_corner_warp(&self) -> bool {
        self.corners.iter().any(|c| c[0].abs() > 1e-5 || c[1].abs() > 1e-5)
    }

    pub fn validate(&mut self) {
        self.dx = finite_f64(self.dx, 0.0).clamp(-20_000.0, 20_000.0);
        self.dy = finite_f64(self.dy, 0.0).clamp(-20_000.0, 20_000.0);
        self.scale_x = finite_f64(self.scale_x, 1.0).clamp(0.05, 16.0);
        self.scale_y = finite_f64(self.scale_y, 1.0).clamp(0.05, 16.0);
        self.rotation_deg = finite_f64(self.rotation_deg, 0.0).clamp(-3600.0, 3600.0);
        self.skew_x_deg = finite_f64(self.skew_x_deg, 0.0).clamp(-75.0, 75.0);
        self.skew_y_deg = finite_f64(self.skew_y_deg, 0.0).clamp(-75.0, 75.0);
        for c in &mut self.corners {
            c[0] = finite_f64(c[0], 0.0).clamp(-20_000.0, 20_000.0);
            c[1] = finite_f64(c[1], 0.0).clamp(-20_000.0, 20_000.0);
        }
    }

    pub fn is_identity(&self) -> bool {
        self.dx.abs() < 1e-6
            && self.dy.abs() < 1e-6
            && (self.scale_x - 1.0).abs() < 1e-6
            && (self.scale_y - 1.0).abs() < 1e-6
            && self.rotation_deg.abs() < 1e-6
            && self.skew_x_deg.abs() < 1e-6
            && self.skew_y_deg.abs() < 1e-6
            && !self.has_corner_warp()
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
        self.points.len() == rest.len()
            && self.points.iter().zip(rest.iter()).all(|(a, b)| (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6)
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
                *width_px = finite_f32(*width_px, 4.0).clamp(0.0, MAX_SFX_EFFECT_PX);
                *miter_limit = finite_f32(*miter_limit, 4.0).clamp(1.0, 24.0);
                fill.validate();
            }
            Self::Extrude3D { dx_px, dy_px, steps, fill, .. } => {
                *dx_px = finite_f32(*dx_px, 6.0).clamp(-MAX_SFX_EFFECT_PX, MAX_SFX_EFFECT_PX);
                *dy_px = finite_f32(*dy_px, 6.0).clamp(-MAX_SFX_EFFECT_PX, MAX_SFX_EFFECT_PX);
                *steps = (*steps).clamp(1, 128);
                fill.validate();
            }
            Self::Shadow { dx_px, dy_px, blur_px, color, opacity, .. } => {
                *dx_px = finite_f32(*dx_px, 5.0).clamp(-MAX_SFX_EFFECT_PX, MAX_SFX_EFFECT_PX);
                *dy_px = finite_f32(*dy_px, 5.0).clamp(-MAX_SFX_EFFECT_PX, MAX_SFX_EFFECT_PX);
                *blur_px = finite_f32(*blur_px, 0.0).clamp(0.0, MAX_SFX_EFFECT_PX);
                *color = finite_color(*color);
                *opacity = finite_f32(*opacity, 0.75).clamp(0.0, 1.0);
            }
            Self::InnerGlow { width_px, color, opacity, .. } => {
                *width_px = finite_f32(*width_px, 3.0).clamp(0.0, MAX_SFX_EFFECT_PX);
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
    pub spline: SfxSplineArc,
    #[serde(default)]
    pub per_glyph: Vec<SfxGlyphTransform>,
    pub mesh: SfxMeshLattice,
    #[serde(default)]
    pub liquify_strokes: Vec<SfxLiquifyBrush>,
    pub fill: SfxFillSource,
    #[serde(default)]
    pub line_effect: SfxLineEffect,
    #[serde(default)]
    pub dry_brush: SfxDryBrushSpec,
    #[serde(default)]
    pub vector_shadow: SfxVectorShadow,
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
            spline: SfxSplineArc::default(),
            per_glyph: Vec::new(),
            mesh: SfxMeshLattice::default(),
            liquify_strokes: Vec::new(),
            fill: SfxFillSource::Solid { color: default_ink() },
            line_effect: SfxLineEffect {
                style: SfxLineStyle::Clean,
                halo_width_px: 10.0,
                halo_color: Color::WHITE,
                line_width_px: 4.0,
                line_color: default_ink(),
                ..SfxLineEffect::default()
            },
            dry_brush: SfxDryBrushSpec::default(),
            vector_shadow: SfxVectorShadow::default(),
            styles: Vec::new(),
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
        self.size_pt = finite_f32(self.size_pt, 48.0).clamp(4.0, MAX_SFX_SIZE_PT);
        self.tracking = finite_f32(self.tracking, 0.0).clamp(-500.0, 2000.0);
        self.leading_scale = finite_f32(self.leading_scale, 1.05).clamp(0.4, 4.0);
        self.free_transform.validate();
        self.spline.validate();
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
        self.line_effect.validate();
        self.dry_brush.validate();
        self.vector_shadow.validate();
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
            description: String::new(),
            font_family: self.font_family.clone(),
            weight: self.weight,
            italic: self.italic,
            size_pt: self.size_pt,
            tracking: self.tracking,
            leading_scale: self.leading_scale,
            vertical: self.vertical,
            fill: self.fill.clone(),
            line_effect: self.line_effect,
            dry_brush: self.dry_brush,
            vector_shadow: self.vector_shadow,
            styles: self.styles.clone(),
            distress: self.distress,
        }
    }

    /// Applies an [`SfxPreset`] onto this instance while preserving text, origin, size,
    /// orientation, font choice (when already set), and warp geometry. Pixel-sized effect widths
    /// from the preset are scaled proportionally to `self.size_pt / preset.size_pt`.
    pub fn apply_preset(&mut self, preset: &SfxPreset) {
        let kept_size = finite_f32(self.size_pt, 54.0).clamp(4.0, MAX_SFX_SIZE_PT);
        let base_size = finite_f32(preset.size_pt, 64.0).clamp(8.0, MAX_SFX_SIZE_PT);
        let scale = (f64::from(kept_size) / f64::from(base_size)).clamp(0.25, 32.0);
        if self.font_family.trim().is_empty() {
            self.font_family = preset.font_family.clone();
            self.weight = preset.weight;
            self.italic = preset.italic;
        }
        self.tracking = preset.tracking;
        self.fill = preset.fill.clone();
        self.line_effect = preset.line_effect;
        self.dry_brush = preset.dry_brush;
        self.vector_shadow = preset.vector_shadow;
        self.styles = preset.styles.clone();
        self.distress = preset.distress;
        if (scale - 1.0).abs() > 1e-4 {
            scale_sfx_pixel_effects(self, scale);
        }
        self.validate();
    }
}

/// Scales pixel-sized effect dimensions (leaving `size_pt` and percentage fields such as
/// `shtack_overshoot_px` untouched) by `scale`.
fn scale_sfx_pixel_effects(data: &mut SfxData, scale: f64) {
    let cap = f64::from(MAX_SFX_EFFECT_PX);
    let scaled = |v: f32| -> f32 { (f64::from(finite_f32(v, 0.0)) * scale).clamp(0.0, cap) as f32 };
    let scaled_signed = |v: f32| -> f32 { (f64::from(finite_f32(v, 0.0)) * scale).clamp(-cap, cap) as f32 };

    data.line_effect.halo_width_px = scaled(data.line_effect.halo_width_px);
    data.line_effect.line_width_px = scaled(data.line_effect.line_width_px);
    data.line_effect.spike_length_px = scaled(data.line_effect.spike_length_px).max(2.0);
    data.line_effect.spike_size_px = scaled(data.line_effect.spike_size_px).max(2.0);
    data.line_effect.gpen_overshoot_px = scaled(data.line_effect.gpen_overshoot_px);
    data.line_effect.ink_brush_overshoot_px = scaled(data.line_effect.ink_brush_overshoot_px);
    // `shtack_overshoot_px` is a percentage of glyph height, so it must NOT be scaled here.
    data.line_effect.shtack_thickness_px = scaled(data.line_effect.shtack_thickness_px).max(6.0);
    data.dry_brush.lic_length_px = scaled(data.dry_brush.lic_length_px).max(4.0);
    data.vector_shadow.distance_px = scaled(data.vector_shadow.distance_px);
    data.vector_shadow.extrude_depth_px = scaled(data.vector_shadow.extrude_depth_px);
    for pass in &mut data.styles {
        match pass {
            SfxStylePass::Stroke { width_px, .. } => *width_px = scaled(*width_px),
            SfxStylePass::Extrude3D { dx_px, dy_px, .. } => {
                *dx_px = scaled_signed(*dx_px);
                *dy_px = scaled_signed(*dy_px);
            }
            SfxStylePass::Shadow { dx_px, dy_px, blur_px, .. } => {
                *dx_px = scaled_signed(*dx_px);
                *dy_px = scaled_signed(*dy_px);
                *blur_px = scaled(*blur_px);
            }
            SfxStylePass::InnerGlow { width_px, .. } => *width_px = scaled(*width_px),
            SfxStylePass::NegativeInvert { .. } => {}
        }
    }
}

/// Scale the pixel-sized defaults of a **newly created** SFX layer to the document's min side.
///
/// `scale = (min_side / SFX_REFERENCE_MIN_SIDE).max(1.0)`, so canvases at or below 1200 px keep
/// their exact historical values. Call this only for freshly derived defaults: caller-supplied
/// values in an `sfx.create` payload are already explicit and must not be overwritten.
pub fn scale_new_sfx_for_document(data: &mut SfxData, doc_width: u32, doc_height: u32) {
    let scale = (f64::from(doc_width.min(doc_height)) / SFX_REFERENCE_MIN_SIDE).max(1.0);
    if scale <= 1.0 {
        return;
    }
    data.size_pt = (f64::from(finite_f32(data.size_pt, 54.0)) * scale).clamp(4.0, f64::from(MAX_SFX_SIZE_PT)) as f32;
    scale_sfx_pixel_effects(data, scale);
}

/// Clear per-instance warps when a *new* SFX layer is created from tool state.
///
/// A panel draft can still carry the 4-corner / per-glyph / mesh / liquify / arc deltas of a
/// previously edited layer; inheriting them makes a fresh lettering layer look mirrored or
/// collapsed. Style settings (font, size, effects) are intentionally preserved.
pub fn reset_sfx_warps_for_new_layer(data: &mut SfxData) {
    data.free_transform.corners = [[0.0; 2]; 4];
    data.free_transform.perspective_x = 0.0;
    data.free_transform.perspective_y = 0.0;
    data.spline.points.clear();
    data.per_glyph.clear();
    data.liquify_strokes.clear();
    let cols = data.mesh.cols;
    let rows = data.mesh.rows;
    data.mesh.points = SfxMeshLattice::rest_grid(cols, rows);
}

/// Reusable deterministic JSON preset for SFX styling (`sfx.presets.*`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SfxPreset {
    pub name: String,
    #[serde(default)]
    pub description: String,
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
    pub line_effect: SfxLineEffect,
    #[serde(default)]
    pub dry_brush: SfxDryBrushSpec,
    #[serde(default)]
    pub vector_shadow: SfxVectorShadow,
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
        if self.font_family.trim().is_empty() {
            self.font_family = default_font_family();
        }
        self.weight = self.weight.clamp(100, 900);
        self.size_pt = finite_f32(self.size_pt, 54.0).clamp(4.0, MAX_SFX_SIZE_PT);
        self.tracking = finite_f32(self.tracking, 0.0).clamp(-500.0, 2000.0);
        self.leading_scale = finite_f32(self.leading_scale, 1.05).clamp(0.4, 4.0);
        self.fill.validate();
        self.line_effect.validate();
        self.dry_brush.validate();
        self.vector_shadow.validate();
        if self.styles.len() > MAX_SFX_STYLE_PASSES {
            self.styles.truncate(MAX_SFX_STYLE_PASSES);
        }
        for pass in &mut self.styles {
            pass.validate();
        }
        self.distress.validate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sfx_defaults_scale_only_on_large_documents_and_warps_reset() {
        let mut small = SfxData::default();
        let original = small.clone();
        scale_new_sfx_for_document(&mut small, 1200, 1200);
        assert_eq!(small, original, "documents at or below the reference keep exact defaults");

        let mut large = SfxData::default();
        scale_new_sfx_for_document(&mut large, 12303, 17499);
        assert!(large.size_pt > 54.0 * 5.0, "size_pt must scale with min side");
        assert!(large.line_effect.halo_width_px > 10.0);
        assert!(large.line_effect.spike_size_px > 7.0);
        large.validate();
        assert!(large.size_pt <= MAX_SFX_SIZE_PT);

        large.free_transform.corners = [[9.0, -9.0]; 4];
        large.per_glyph.push(SfxGlyphTransform { glyph_index: 0, ..Default::default() });
        large.spline.points.push([1.0, 1.0]);
        large.liquify_strokes.push(Default::default());
        large.mesh.points[0] = [-3.0, -3.0];
        reset_sfx_warps_for_new_layer(&mut large);
        assert_eq!(large.free_transform.corners, [[0.0; 2]; 4]);
        assert!(large.per_glyph.is_empty());
        assert!(large.spline.points.is_empty());
        assert!(large.liquify_strokes.is_empty());
        assert!(large.mesh.is_identity());
        assert!(large.size_pt > 54.0 * 5.0, "reset must preserve scaled style defaults");
    }

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

    #[test]
    fn mockup_line_effect_dry_brush_spline_and_legacy_json_compat() {
        let mut data = SfxData::default();
        data.free_transform.corners = [[12.0, -8.0], [f64::NAN, 4.0], [0.0, 10.0], [-5.0, 0.0]];
        data.spline.points = vec![[-175.0, 22.0], [0.0, -28.0], [175.0, 16.0]];
        data.line_effect.style = SfxLineStyle::Shtack;
        data.line_effect.shtack_variant = SfxShtackVariant::DryPencil;
        data.line_effect.shtack_bristles = 99;
        data.line_effect.gpen_overshoot_px = 999.0;
        data.dry_brush.enabled = true;
        data.dry_brush.engine = SfxDryBrushEngine::Vector;
        data.dry_brush.vec_bristle_count = 1;
        data.vector_shadow.enabled = true;
        data.vector_shadow.distance_px = 14.0;
        data.validate();

        assert_eq!(data.free_transform.corners[1][0], 0.0);
        assert!(data.free_transform.has_corner_warp());
        assert!(data.spline.is_curved());
        assert_eq!(data.line_effect.shtack_bristles, 6);
        // Large-canvas effect fields are no longer capped at the old 14 px ceiling.
        assert_eq!(data.line_effect.gpen_overshoot_px, 999.0);
        assert!(data.line_effect.gpen_overshoot_px <= MAX_SFX_EFFECT_PX);
        assert_eq!(data.dry_brush.vec_bristle_count, 4);

        let json = serde_json::to_value(&data).unwrap();
        let back: SfxData = serde_json::from_value(json).unwrap();
        assert_eq!(back, data);

        // Legacy JSON without new mockup fields must deserialize cleanly via #[serde(default)].
        let legacy: SfxData = serde_json::from_str(r#"{"text":"BOOM","origin":[10,20]}"#).unwrap();
        assert_eq!(legacy.text, "BOOM");
        assert_eq!(legacy.line_effect.style, SfxLineStyle::Clean);
        assert!(!legacy.dry_brush.enabled);
    }

    #[test]
    fn shtack_variant_legacy_serde_aliases_deserialize() {
        let caps: SfxLineEffect =
            serde_json::from_str(r#"{"style":"shtack","shtackVariant":"caps"}"#).unwrap();
        assert_eq!(caps.shtack_variant, SfxShtackVariant::SaiPenSketch);
        let ribbon: SfxLineEffect =
            serde_json::from_str(r#"{"style":"shtack","shtackVariant":"ribbon"}"#).unwrap();
        assert_eq!(ribbon.shtack_variant, SfxShtackVariant::DryPencil);

        // New names round-trip cleanly.
        for v in [SfxShtackVariant::SaiPenSketch, SfxShtackVariant::DryPencil] {
            let json = serde_json::to_string(&v).unwrap();
            let back: SfxShtackVariant = serde_json::from_str(&json).unwrap();
            assert_eq!(back, v);
        }

        // A full legacy document JSON with `shtackVariant: "ribbon"` still loads.
        let legacy: SfxData = serde_json::from_str(
            r#"{"text":"BAM!","lineEffect":{"style":"shtack","shtackVariant":"ribbon"}}"#,
        )
        .unwrap();
        assert_eq!(legacy.line_effect.shtack_variant, SfxShtackVariant::DryPencil);
    }

    #[test]
    fn apply_preset_preserves_size_font_vertical_and_scales_effect_widths() {
        let mut data = SfxData {
            text: "BOOM!".to_owned(),
            font_family: "Anime Ace".to_owned(),
            size_pt: 256.0,
            vertical: true,
            ..SfxData::default()
        };
        let mut preset = SfxPreset {
            name: "Test Shtack".to_owned(),
            font_family: "Impact".to_owned(),
            size_pt: 64.0,
            vertical: false,
            ..SfxPreset::default()
        };
        preset.line_effect.style = SfxLineStyle::Shtack;
        preset.line_effect.halo_width_px = 10.0;
        preset.line_effect.shtack_overshoot_px = 20.0;
        preset.vector_shadow.enabled = true;
        preset.vector_shadow.distance_px = 12.0;

        data.apply_preset(&preset);
        assert_eq!(data.size_pt, 256.0, "applying a style preset must never reset SFX size_pt");
        assert_eq!(data.font_family, "Anime Ace", "applying a style preset must keep the chosen font");
        assert!(data.vertical, "applying a style preset must keep vertical orientation");
        assert_eq!(data.line_effect.style, SfxLineStyle::Shtack);
        assert!((data.line_effect.halo_width_px - 40.0).abs() < 1e-3, "halo scales 4x with 256/64 size");
        assert!((data.vector_shadow.distance_px - 48.0).abs() < 1e-3, "shadow distance scales 4x");
        assert!((data.line_effect.shtack_overshoot_px - 20.0).abs() < 1e-3, "needle overshoot % must NOT double-scale");
    }
}