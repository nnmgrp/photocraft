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
    Color::BLACK
}

fn default_font_family() -> String {
    "Inter".to_owned()
}

fn default_seed_one() -> u32 {
    1
}

fn default_65() -> f32 {
    65.0
}

fn default_100() -> f32 {
    100.0
}

fn default_2() -> f32 {
    2.0
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

fn default_39() -> f32 {
    39.0
}

fn default_36() -> f32 {
    36.0
}

fn default_96() -> f32 {
    96.0
}

fn default_47() -> f32 {
    47.0
}

fn default_37() -> f32 {
    37.0
}

fn default_90() -> f32 {
    90.0
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

fn default_26() -> f32 {
    26.0
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

fn default_74() -> f32 {
    74.0
}

fn default_38() -> f32 {
    38.0
}

fn default_110() -> f32 {
    110.0
}

fn default_50() -> f32 {
    50.0
}

fn default_34() -> f32 {
    34.0
}

fn default_48() -> f32 {
    48.0
}

fn default_58() -> f32 {
    58.0
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
    /// Simple proportional resize of the bounding box (`size_pt` / `origin` / edge scale).
    #[default]
    Resize,
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

/// Stylized manga ink outline / line modes (`Clean`..`Shtack` + SFX v3 `ShibufudeV3`, `SugomiV3`).
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
    /// Style 1 — しぶふでペン (`shibufude_v3.py`, rev 2): clean `Kado`/`Maru` band + 2D SDF roughness + clustered しぶき splatter.
    ShibufudeV3,
    /// Style 2 — 凄みベタ (`sugomi_v3.py`, rev 5): continuous jagged `fude`/`tate`/`yoko` ring + `Solid`/`Hollow` + compact inner teeth.
    SugomiV3,
}

/// Base band corner profile for [`SfxLineStyle::ShibufudeV3`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SfxShibufudeProfile {
    /// Angular miter-clip stroke band (`角`).
    #[default]
    Kado,
    /// Rounded analytical SDF band (`丸`).
    Maru,
}

/// Edge finish mode shared by SFX v3 styles (`ShibufudeV3` and `SugomiV3`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SfxV3EdgeMode {
    /// Crisp binary 1-bit ink with hard edge paper tooth.
    #[default]
    OneBit,
    /// Rough Pencil / Dry Edge kasure breakup with core protection.
    Rough,
}

/// Spike orientation strategy for [`SfxLineStyle::SugomiV3`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SfxSugomiDirection {
    /// Continuous contour-following brush wedges (`fude`).
    #[default]
    Fude,
    /// Vertical comb needles (`tate`).
    Tate,
    /// Horizontal comb needles (`yoko`).
    Yoko,
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

/// Where [`SfxLineStyle::Shtack`] corner needles (overshoot) are allowed to protrude.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SfxShtackOvershootMode {
    /// Only acute / free outer stroke tips shoot needles (`Края`); regular box corners stay clean.
    FreeEnds,
    /// Convex corners on the outer glyph silhouette shoot needles (`Стыки`); inner counters
    /// and concave junctions stay clean so strokes never slice through letterforms.
    #[default]
    #[serde(alias = "outerJunctions")]
    OuterOnly,
    /// Every corner segment shoots needles (`Все`, raw chaotic sketch).
    All,
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
    #[serde(default)]
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
    #[serde(default)]
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
    /// Restricts Shtack needle overshoots to free tips (`FreeEnds`), outer convex corners (`OuterOnly`), or all (`All`).
    #[serde(default)]
    pub shtack_overshoot_mode: SfxShtackOvershootMode,
    /// Thickness of a Shtack sketch pass, as a PERCENT of glyph height (`gh`) in `0.5..=8.0`.
    /// The legacy serde name is kept for document compatibility; the value is no longer a pixel length.
    #[serde(default = "default_2")]
    pub shtack_thickness_px: f32,
    #[serde(default = "default_26")]
    pub shtack_dryness_pct: f32,
    #[serde(default = "default_seed_one")]
    pub shtack_seed: u32,
    /// Dry Pencil / line grain sharpness (`0..=100%`). At 100% paper tooth punches clean `0.0` gaps.
    #[serde(default = "default_100")]
    pub shtack_grain_sharpness_pct: f32,
    /// `false` (default) = hollow sketch contour: `ЗАЛИВКА (FILL)` colours the Shtack strokes
    /// themselves and the letter interior stays transparent. `true` = fill the letter interior
    /// with `ЗАЛИВКА (FILL)` and draw the Shtack strokes on top in `line_color`.
    #[serde(default)]
    pub shtack_fill_interior: bool,
    /// Shared paper-grain / dry ink breakup on ANY outline mode (`0..=100%`, default `0%`),
    /// so outlines visually fuse with `Dry Brush` fills on the same paper tooth.
    #[serde(default)]
    pub line_grain_pct: f32,

    // --- Style 1: Shibufude v3 (しぶふでペン, rev 2) ---
    #[serde(default)]
    pub shibufude_profile: SfxShibufudeProfile,
    #[serde(default)]
    pub shibufude_edge_mode: SfxV3EdgeMode,
    #[serde(default = "default_74")]
    pub shibufude_roughness_pct: f32,
    #[serde(default = "default_62")]
    pub shibufude_splatter_pct: f32,
    /// Cluster spread distance for しぶき droplets in document pixels (scaled once by `scale_sfx_pixel_effects`).
    #[serde(default = "default_38")]
    pub shibufude_spread_px: f32,
    #[serde(default = "default_110")]
    pub shibufude_splat_size_pct: f32,
    #[serde(default = "default_60")]
    pub shibufude_dryness_pct: f32,
    #[serde(default = "default_50")]
    pub shibufude_sharpness_pct: f32,
    /// Knockout (`抜き`) mode: tightens band width by `0.84` for crisp white-on-black ink.
    #[serde(default)]
    pub shibufude_knockout: bool,
    #[serde(default = "default_seed_one")]
    pub shibufude_seed: u32,

    // --- Style 2: Sugomi Spiky Ink v3 (凄みベタ, rev 5) ---
    #[serde(default)]
    pub sugomi_direction: SfxSugomiDirection,
    #[serde(default)]
    pub sugomi_edge_mode: SfxV3EdgeMode,
    /// `false` = Solid spiky silhouette; `true` = Hollow band (`band_w <= 0.55 * stroke_w`) + inward teeth.
    #[serde(default)]
    pub sugomi_hollow: bool,
    /// Spike amplitude as a PERCENT of local glyph stroke width (`10..=160%`, default `78%`).
    #[serde(default = "default_78")]
    pub sugomi_spike_amp_pct: f32,
    /// Co-axial V-cleft dip depth as a PERCENT of local glyph stroke width (`0..=80%`, default `34%`).
    #[serde(default = "default_34")]
    pub sugomi_cleft_dip_pct: f32,
    #[serde(default = "default_48")]
    pub sugomi_dryness_pct: f32,
    #[serde(default = "default_58")]
    pub sugomi_sharpness_pct: f32,
    #[serde(default = "default_seed_one")]
    pub sugomi_seed: u32,
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
            gpen_overshoot_px: 0.0,
            gpen_overshoot_count_pct: 85.0,
            gpen_overshoot_random_pct: 45.0,
            gpen_seed: 1,
            ink_brush_roughness_pct: 45.0,
            ink_brush_dropout_pct: 35.0,
            ink_brush_pressure_pct: 60.0,
            ink_brush_overshoot_px: 0.0,
            ink_brush_overshoot_count_pct: 85.0,
            ink_brush_overshoot_random_pct: 45.0,
            ink_brush_seed: 1,
            shtack_variant: SfxShtackVariant::SaiPenSketch,
            shtack_bristles: 5,
            shtack_split_pct: 78.0,
            shtack_overshoot_px: 20.0,
            shtack_overshoot_mode: SfxShtackOvershootMode::OuterOnly,
            shtack_thickness_px: 2.0,
            shtack_dryness_pct: 26.0,
            shtack_seed: 1,
            shtack_grain_sharpness_pct: 100.0,
            shtack_fill_interior: false,
            line_grain_pct: 0.0,
            shibufude_profile: SfxShibufudeProfile::Kado,
            shibufude_edge_mode: SfxV3EdgeMode::OneBit,
            shibufude_roughness_pct: 74.0,
            shibufude_splatter_pct: 62.0,
            shibufude_spread_px: 38.0,
            shibufude_splat_size_pct: 110.0,
            shibufude_dryness_pct: 60.0,
            shibufude_sharpness_pct: 50.0,
            shibufude_knockout: false,
            shibufude_seed: 1,
            sugomi_direction: SfxSugomiDirection::Fude,
            sugomi_edge_mode: SfxV3EdgeMode::OneBit,
            sugomi_hollow: false,
            sugomi_spike_amp_pct: 78.0,
            sugomi_cleft_dip_pct: 34.0,
            sugomi_dryness_pct: 48.0,
            sugomi_sharpness_pct: 58.0,
            sugomi_seed: 1,
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
        self.gpen_overshoot_px = finite_f32(self.gpen_overshoot_px, 0.0).clamp(0.0, MAX_SFX_EFFECT_PX);
        self.gpen_overshoot_count_pct = finite_f32(self.gpen_overshoot_count_pct, 85.0).clamp(0.0, 100.0);
        self.gpen_overshoot_random_pct = finite_f32(self.gpen_overshoot_random_pct, 45.0).clamp(0.0, 100.0);

        self.ink_brush_roughness_pct = finite_f32(self.ink_brush_roughness_pct, 45.0).clamp(0.0, 100.0);
        self.ink_brush_dropout_pct = finite_f32(self.ink_brush_dropout_pct, 35.0).clamp(0.0, 100.0);
        self.ink_brush_pressure_pct = finite_f32(self.ink_brush_pressure_pct, 60.0).clamp(0.0, 100.0);
        self.ink_brush_overshoot_px = finite_f32(self.ink_brush_overshoot_px, 0.0).clamp(0.0, MAX_SFX_EFFECT_PX);
        self.ink_brush_overshoot_count_pct = finite_f32(self.ink_brush_overshoot_count_pct, 85.0).clamp(0.0, 100.0);
        self.ink_brush_overshoot_random_pct = finite_f32(self.ink_brush_overshoot_random_pct, 45.0).clamp(0.0, 100.0);

        self.shtack_bristles = self.shtack_bristles.clamp(3, 6);
        self.shtack_split_pct = finite_f32(self.shtack_split_pct, 78.0).clamp(0.0, 100.0);
        // Needle overshoot is now a PERCENT of the glyph height (0..=60%), not a pixel length.
        self.shtack_overshoot_px = finite_f32(self.shtack_overshoot_px, 20.0).clamp(0.0, 60.0);
        // Percentage of glyph height: never a document-pixel length, never double-scaled.
        self.shtack_thickness_px = finite_f32(self.shtack_thickness_px, 2.0).clamp(0.5, 8.0);
        self.shtack_dryness_pct = finite_f32(self.shtack_dryness_pct, 26.0).clamp(0.0, 100.0);
        self.shtack_grain_sharpness_pct = finite_f32(self.shtack_grain_sharpness_pct, 100.0).clamp(0.0, 100.0);
        self.line_grain_pct = finite_f32(self.line_grain_pct, 0.0).clamp(0.0, 100.0);

        self.shibufude_roughness_pct = finite_f32(self.shibufude_roughness_pct, 74.0).clamp(0.0, 100.0);
        self.shibufude_splatter_pct = finite_f32(self.shibufude_splatter_pct, 62.0).clamp(0.0, 100.0);
        self.shibufude_spread_px = finite_f32(self.shibufude_spread_px, 38.0).clamp(4.0, MAX_SFX_EFFECT_PX);
        self.shibufude_splat_size_pct = finite_f32(self.shibufude_splat_size_pct, 110.0).clamp(30.0, 250.0);
        self.shibufude_dryness_pct = finite_f32(self.shibufude_dryness_pct, 60.0).clamp(0.0, 100.0);
        self.shibufude_sharpness_pct = finite_f32(self.shibufude_sharpness_pct, 50.0).clamp(0.0, 100.0);

        self.sugomi_spike_amp_pct = finite_f32(self.sugomi_spike_amp_pct, 78.0).clamp(10.0, 160.0);
        self.sugomi_cleft_dip_pct = finite_f32(self.sugomi_cleft_dip_pct, 34.0).clamp(0.0, 80.0);
        self.sugomi_dryness_pct = finite_f32(self.sugomi_dryness_pct, 48.0).clamp(0.0, 100.0);
        self.sugomi_sharpness_pct = finite_f32(self.sugomi_sharpness_pct, 58.0).clamp(0.0, 100.0);
    }
}

/// Parameters for the 4 Dry Brush fill engines (preserved when toggling Solid / DryBrush / Screentone).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SfxDryBrushSpec {
    pub enabled: bool,
    pub engine: SfxDryBrushEngine,
    #[serde(default = "default_39")]
    pub dryness_pct: f32,
    #[serde(default = "default_60")]
    pub freq_pct: f32,
    #[serde(default = "default_96")]
    pub lic_length_px: f32,
    #[serde(default = "default_47")]
    pub warp_pct: f32,
    #[serde(default = "default_37")]
    pub paper_grain_pct: f32,
    #[serde(default = "default_65")]
    pub edge_wear_pct: f32,
    #[serde(default = "default_90")]
    pub angle_deg: f32,
    #[serde(default = "default_seed_one")]
    pub seed: u32,
    // --- Engine 4: Vector ---
    #[serde(default = "default_18_u16")]
    pub vec_bristle_count: u16,
    #[serde(default = "default_65")]
    pub vec_bristle_random_pct: f32,
    #[serde(default = "default_3")]
    pub vec_mutual_tilt_deg: f32,
    #[serde(default = "default_4")]
    pub vec_tilt_random_deg: f32,
    #[serde(default = "default_45")]
    pub vec_gauss_dots_pct: f32,
    /// Paper-grain / bristle threshold sharpness (`0..=100%`). 100% produces true binary gaps.
    #[serde(default = "default_100")]
    pub grain_sharpness_pct: f32,
    /// Relative bristle / grain scale (`10..=500%`, default `36%` for `SdfSeg`). Automatically multiplied by
    /// glyph height so 2400-DPI / 12K×17K lettering keeps bold, organic dry-brush strokes.
    #[serde(default = "default_36")]
    pub bristle_size_pct: f32,
}

impl Default for SfxDryBrushSpec {
    fn default() -> Self {
        Self::for_engine(SfxDryBrushEngine::SdfSeg)
    }
}

impl SfxDryBrushSpec {
    /// Returns calibrated out-of-the-box manga presets for each of the 4 Dry Brush engines.
    pub fn for_engine(engine: SfxDryBrushEngine) -> Self {
        let mut s = Self {
            enabled: false,
            engine,
            dryness_pct: 39.0,
            freq_pct: 60.0,
            lic_length_px: 96.0,
            warp_pct: 47.0,
            paper_grain_pct: 37.0,
            edge_wear_pct: 65.0,
            angle_deg: 90.0,
            seed: 1,
            vec_bristle_count: 18,
            vec_bristle_random_pct: 65.0,
            vec_mutual_tilt_deg: 3.0,
            vec_tilt_random_deg: 4.0,
            vec_gauss_dots_pct: 45.0,
            grain_sharpness_pct: 100.0,
            bristle_size_pct: 36.0,
        };
        s.apply_engine_defaults(engine);
        s
    }

    /// Switches `self.engine` to `engine` and loads its calibrated manga defaults
    /// (preserving `self.enabled` and `self.seed`).
    pub fn apply_engine_defaults(&mut self, engine: SfxDryBrushEngine) {
        self.engine = engine;
        self.grain_sharpness_pct = 100.0;
        match engine {
            SfxDryBrushEngine::SdfEtf => {
                self.dryness_pct = 54.0;
                self.bristle_size_pct = 100.0;
                self.freq_pct = 68.0;
                self.lic_length_px = 96.0;
                self.warp_pct = 60.0;
                self.paper_grain_pct = 65.0;
                self.edge_wear_pct = 8.0;
            }
            SfxDryBrushEngine::SdfSeg => {
                self.dryness_pct = 39.0;
                self.bristle_size_pct = 36.0;
                self.freq_pct = 60.0;
                self.warp_pct = 47.0;
                self.paper_grain_pct = 37.0;
                self.edge_wear_pct = 65.0;
            }
            SfxDryBrushEngine::SdfPhysics => {
                self.dryness_pct = 45.0;
                self.bristle_size_pct = 30.0;
                self.freq_pct = 27.0;
                self.warp_pct = 13.0;
                self.paper_grain_pct = 75.0;
                self.edge_wear_pct = 56.0;
                self.angle_deg = 90.0;
            }
            SfxDryBrushEngine::Vector => {
                self.dryness_pct = 45.0;
                self.bristle_size_pct = 100.0;
                self.vec_bristle_count = 18;
                self.vec_bristle_random_pct = 65.0;
                self.vec_mutual_tilt_deg = 3.0;
                self.vec_tilt_random_deg = 4.0;
                self.vec_gauss_dots_pct = 45.0;
                self.angle_deg = -24.0;
            }
        }
    }

    pub fn validate(&mut self) {
        self.dryness_pct = finite_f32(self.dryness_pct, 39.0).clamp(0.0, 100.0);
        self.freq_pct = finite_f32(self.freq_pct, 60.0).clamp(10.0, 100.0);
        self.lic_length_px = finite_f32(self.lic_length_px, 96.0).clamp(4.0, MAX_SFX_EFFECT_PX);
        self.warp_pct = finite_f32(self.warp_pct, 47.0).clamp(0.0, 100.0);
        self.paper_grain_pct = finite_f32(self.paper_grain_pct, 37.0).clamp(0.0, 100.0);
        self.edge_wear_pct = finite_f32(self.edge_wear_pct, 65.0).clamp(0.0, 100.0);
        self.angle_deg = finite_f32(self.angle_deg, 90.0).clamp(-90.0, 90.0);
        self.vec_bristle_count = self.vec_bristle_count.clamp(4, 40);
        self.vec_bristle_random_pct = finite_f32(self.vec_bristle_random_pct, 65.0).clamp(0.0, 100.0);
        self.vec_mutual_tilt_deg = finite_f32(self.vec_mutual_tilt_deg, 3.0).clamp(-10.0, 10.0);
        self.vec_tilt_random_deg = finite_f32(self.vec_tilt_random_deg, 4.0).clamp(0.0, 10.0);
        self.vec_gauss_dots_pct = finite_f32(self.vec_gauss_dots_pct, 45.0).clamp(0.0, 100.0);
        self.grain_sharpness_pct = finite_f32(self.grain_sharpness_pct, 100.0).clamp(0.0, 100.0);
        self.bristle_size_pct = finite_f32(self.bristle_size_pct, 36.0).clamp(10.0, 500.0);
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

fn default_62() -> f32 {
    62.0
}

fn default_1_3() -> f32 {
    1.3
}

fn default_18() -> f32 {
    18.0
}

fn default_70() -> f32 {
    70.0
}

/// Geometric stroke strategy for [`SfxFillSource::ShtackHatch`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SfxShtackFillMode {
    /// Analytic parallel / cross-hatch scanline strokes across the glyph interior.
    #[default]
    Hatch,
    /// PaintTool SAI-style multi-pass tapered pen strokes flowing along each glyph's
    /// smoothed medial-axis strokes, with rich overlapping core and sharp needle overshoots.
    StrokeFlow,
    /// Wet Japanese Sumi-e / Ukiyo-e calligraphy brush along smoothed glyph centerlines:
    /// juicy pressure-modulated ink core + split dry bristles (`kasure`) at edges and tails.
    UkiyoBrush,
}

/// Which stroke ends may shoot razor needles in [`SfxShtackFillMode::StrokeFlow`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SfxShtackFlowOvershoot {
    /// Only genuine free stroke tips shoot needles; all T/L-junctions stay clean.
    FreeEnds,
    /// Free tips plus exterior T/L-junction exits shoot needles; short bridges & counters stay clean.
    #[default]
    OuterJunctions,
    /// Every stroke end shoots needles (raw chaotic sketch).
    All,
}

/// Maximum per-endpoint overshoot overrides stored in one [`SfxShtackFillSpec`].
pub const MAX_SFX_FLOW_OVERRIDES: usize = 256;

/// Maximum Photoshop-style manga blending effects in [`SfxBlending::effects`].
pub const MAX_SFX_BLEND_EFFECTS: usize = 16;

/// Per-endpoint overshoot override edited interactively on the canvas via the SFX Gizmo.
///
/// Keyed by `(glyph_index, normalized_uv, base_outgoing_dir)` so overrides survive moving,
/// resizing, and recolouring the SFX layer without drifting. Dragging the handle updates
/// `angle_offset_deg` and `length_scale` in real time.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SfxFlowEndOverride {
    /// Index of the glyph in the SFX layout.
    pub glyph_index: u16,
    /// Endpoint position normalized to the glyph's warped bounding box (`[0..1, 0..1]`).
    pub uv: [f32; 2],
    /// Original base outgoing unit direction `[dx, dy]` (disambiguates multiple rays at one corner).
    pub dir: [f32; 2],
    /// `true` forces a needle ON at this endpoint; `false` forces it OFF.
    pub enabled: bool,
    /// User-dragged angle offset relative to `dir`, in degrees (`-165..=165`).
    #[serde(default)]
    pub angle_offset_deg: f32,
    /// User-dragged needle length multiplier (`0.15..=3.0`, default `1.0`).
    #[serde(default = "default_one_f32")]
    pub length_scale: f32,
}

/// Parameters for an independent interior Shtack fill (`SfxFillSource::ShtackHatch`),
/// completely decoupled from `SfxLineEffect` (works cleanly with `Clean 0px` outline or mixed
/// on top of Solid / Screentone via [`SfxBlending`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SfxShtackFillSpec {
    #[serde(default = "default_ink")]
    pub color: Color,
    /// Fill strategy (`Hatch` scanlines, `StrokeFlow` SAI pen, or `UkiyoBrush` wet sumi-e).
    #[serde(default)]
    pub mode: SfxShtackFillMode,
    /// Overshoot scope for `StrokeFlow` (`FreeEnds`, `OuterJunctions`, or `All`).
    #[serde(default)]
    pub flow_overshoot: SfxShtackFlowOvershoot,
    /// Per-endpoint needle ON/OFF overrides edited via the canvas Gizmo (`StrokeFlow` mode).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flow_overrides: Vec<SfxFlowEndOverride>,
    /// Primary hatch angle in degrees (`-90..=90`, used in `Hatch` mode).
    #[serde(default = "default_neg35")]
    pub angle_deg: f32,
    /// Adds a second crossing hatch pass at `angle_deg + cross_angle_deg` (`Hatch` mode).
    #[serde(default)]
    pub cross_hatch: bool,
    /// Relative angle of the crossing hatch pass (`15..=90` degrees, `Hatch` mode).
    #[serde(default = "default_65")]
    pub cross_angle_deg: f32,
    /// Stroke / stripe density (`10..=100%`, proportional to glyph height / local stem width).
    #[serde(default = "default_62")]
    pub density_pct: f32,
    /// Single stroke width as a PERCENT of glyph height (`0.4..=5.0%`).
    #[serde(default = "default_1_3")]
    pub thickness_pct: f32,
    /// Hand-drawn tilt / bow / spacing roughness (`0..=100%`).
    #[serde(default = "default_45")]
    pub roughness_pct: f32,
    /// How far stroke tips may protrude beyond the letter silhouette as a PERCENT of `gh` (`0..=45%`).
    /// At `0%` strokes are strictly clipped to the glyph interior.
    #[serde(default = "default_4")]
    pub overshoot_pct: f32,
    /// Paper-grain dry breakup (`0..=100%`).
    #[serde(default = "default_18")]
    pub dryness_pct: f32,
    /// Paper-grain sharpness (`0..=100%`).
    #[serde(default = "default_70")]
    pub grain_sharpness_pct: f32,
    #[serde(default = "default_seed_one")]
    pub seed: u32,
}

impl Default for SfxShtackFillSpec {
    fn default() -> Self {
        Self {
            color: default_ink(),
            mode: SfxShtackFillMode::Hatch,
            flow_overshoot: SfxShtackFlowOvershoot::OuterJunctions,
            flow_overrides: Vec::new(),
            angle_deg: -35.0,
            cross_hatch: false,
            cross_angle_deg: 65.0,
            density_pct: 62.0,
            thickness_pct: 1.3,
            roughness_pct: 45.0,
            overshoot_pct: 4.0,
            dryness_pct: 18.0,
            grain_sharpness_pct: 70.0,
            seed: 1,
        }
    }
}

impl SfxShtackFillSpec {
    pub fn validate(&mut self) {
        self.color = finite_color(self.color);
        self.angle_deg = finite_f32(self.angle_deg, -35.0).clamp(-90.0, 90.0);
        self.cross_angle_deg = finite_f32(self.cross_angle_deg, 65.0).clamp(15.0, 90.0);
        self.density_pct = finite_f32(self.density_pct, 62.0).clamp(10.0, 100.0);
        self.thickness_pct = finite_f32(self.thickness_pct, 1.3).clamp(0.4, 5.0);
        self.roughness_pct = finite_f32(self.roughness_pct, 45.0).clamp(0.0, 100.0);
        self.overshoot_pct = finite_f32(self.overshoot_pct, 4.0).clamp(0.0, 45.0);
        self.dryness_pct = finite_f32(self.dryness_pct, 18.0).clamp(0.0, 100.0);
        self.grain_sharpness_pct = finite_f32(self.grain_sharpness_pct, 70.0).clamp(0.0, 100.0);
        self.flow_overrides.retain_mut(|ov| {
            if !ov.uv[0].is_finite() || !ov.uv[1].is_finite() || !ov.dir[0].is_finite() || !ov.dir[1].is_finite() {
                return false;
            }
            let len = ov.dir[0].hypot(ov.dir[1]);
            if len < 1e-4 {
                return false;
            }
            ov.uv[0] = ov.uv[0].clamp(-0.5, 1.5);
            ov.uv[1] = ov.uv[1].clamp(-0.5, 1.5);
            ov.dir[0] /= len;
            ov.dir[1] /= len;
            ov.angle_offset_deg = finite_f32(ov.angle_offset_deg, 0.0).clamp(-165.0, 165.0);
            ov.length_scale = finite_f32(ov.length_scale, 1.0).clamp(0.15, 3.0);
            true
        });
        if self.flow_overrides.len() > MAX_SFX_FLOW_OVERRIDES {
            self.flow_overrides.truncate(MAX_SFX_FLOW_OVERRIDES);
        }
    }

    /// Finds the closest matching per-endpoint override for `(glyph_index, uv, dir)`.
    pub fn find_flow_override_idx(&self, glyph_index: u16, uv: [f32; 2], dir: [f32; 2]) -> Option<usize> {
        let dl = dir[0].hypot(dir[1]).max(1e-6);
        let (ux, uy) = (dir[0] / dl, dir[1] / dl);
        let mut best: Option<(f32, usize)> = None;
        for (idx, ov) in self.flow_overrides.iter().enumerate() {
            if ov.glyph_index != glyph_index {
                continue;
            }
            let du = ov.uv[0] - uv[0];
            let dv = ov.uv[1] - uv[1];
            let dist = du.hypot(dv);
            let dot = ov.dir[0] * ux + ov.dir[1] * uy;
            if dist <= 0.14 && dot >= 0.65 && best.is_none_or(|(bd, _)| dist < bd) {
                best = Some((dist, idx));
            }
        }
        best.map(|(_, idx)| idx)
    }

    /// Toggles one stroke endpoint's needle state relative to `default_enabled`.
    /// Toggling back to `default_enabled` with default angle/length removes the override cleanly.
    pub fn toggle_flow_override(&mut self, glyph_index: u16, uv: [f32; 2], dir: [f32; 2], default_enabled: bool) {
        let dl = dir[0].hypot(dir[1]).max(1e-6);
        let ndir = [dir[0] / dl, dir[1] / dl];
        if let Some(idx) = self.find_flow_override_idx(glyph_index, uv, ndir) {
            let next_enabled = !self.flow_overrides[idx].enabled;
            let has_custom_vec = self.flow_overrides[idx].angle_offset_deg.abs() > 0.5 || (self.flow_overrides[idx].length_scale - 1.0).abs() > 0.03;
            if next_enabled == default_enabled && !has_custom_vec {
                self.flow_overrides.remove(idx);
            } else {
                self.flow_overrides[idx].enabled = next_enabled;
            }
        } else if self.flow_overrides.len() < MAX_SFX_FLOW_OVERRIDES {
            self.flow_overrides.push(SfxFlowEndOverride {
                glyph_index,
                uv,
                dir: ndir,
                enabled: !default_enabled,
                angle_offset_deg: 0.0,
                length_scale: 1.0,
            });
        }
    }

    /// Sets a custom dragged angle offset and length scale on one endpoint (enabling it).
    pub fn set_flow_override_vector(
        &mut self,
        glyph_index: u16,
        uv: [f32; 2],
        base_dir: [f32; 2],
        angle_offset_deg: f32,
        length_scale: f32,
    ) {
        let dl = base_dir[0].hypot(base_dir[1]).max(1e-6);
        let ndir = [base_dir[0] / dl, base_dir[1] / dl];
        let ang = finite_f32(angle_offset_deg, 0.0).clamp(-165.0, 165.0);
        let len = finite_f32(length_scale, 1.0).clamp(0.15, 3.0);
        if let Some(idx) = self.find_flow_override_idx(glyph_index, uv, ndir) {
            self.flow_overrides[idx].enabled = true;
            self.flow_overrides[idx].angle_offset_deg = ang;
            self.flow_overrides[idx].length_scale = len;
        } else if self.flow_overrides.len() < MAX_SFX_FLOW_OVERRIDES {
            self.flow_overrides.push(SfxFlowEndOverride {
                glyph_index,
                uv,
                dir: ndir,
                enabled: true,
                angle_offset_deg: ang,
                length_scale: len,
            });
        }
    }

    /// Clears any manual override on one endpoint (e.g. `Alt + Click` in Gizmo).
    pub fn clear_flow_override(&mut self, glyph_index: u16, uv: [f32; 2], base_dir: [f32; 2]) -> bool {
        if let Some(idx) = self.find_flow_override_idx(glyph_index, uv, base_dir) {
            self.flow_overrides.remove(idx);
            true
        } else {
            false
        }
    }
}

/// Fill paint for either the base SFX face, a secondary blended fill, or a Blending effect pass.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SfxFillSource {
    /// Transparent / hollow glyph interior (only outline & halo render).
    None,
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
    /// Full 4-engine Dry Brush fill (full parity with primary Fill card in Blending overlays).
    DryBrush {
        #[serde(default = "default_ink")]
        color: Color,
        #[serde(default)]
        spec: SfxDryBrushSpec,
    },
    Screentone {
        #[serde(default)]
        spec: ScreentoneSpec,
        #[serde(default = "default_one_f32")]
        opacity: f32,
    },
    /// Independent interior Shtack fill (`Hatch`, `StrokeFlow` SAI, or `UkiyoBrush`).
    ShtackHatch {
        #[serde(default)]
        spec: SfxShtackFillSpec,
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
            Self::None => {}
            Self::Solid { color } => *color = finite_color(*color),
            Self::Gradient { start, end, angle_deg } => {
                *start = finite_color(*start);
                *end = finite_color(*end);
                *angle_deg = finite_f32(*angle_deg, 90.0).rem_euclid(360.0);
            }
            Self::DryBrush { color, spec } => {
                *color = finite_color(*color);
                spec.validate();
                spec.enabled = true;
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
            Self::ShtackHatch { spec } => spec.validate(),
        }
    }
}

/// Full Photoshop + Manga per-channel blend modes inside an SFX layer (`Blending` dock tab).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SfxChannelBlend {
    #[default]
    Normal,
    Multiply,
    ColorBurn,
    Darken,
    Screen,
    ColorDodge,
    Lighten,
    Overlay,
    SoftLight,
    HardLight,
    Difference,
    Exclusion,
    /// Cuts alpha out of the accumulated SFX pixels below (masking / stencil punch-out).
    Knockout,
}

/// Stroke placement inside [`SfxBlendEffect::Stroke`], matching Photoshop Layer Style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SfxStrokePosition {
    #[default]
    Outside,
    Center,
    Inside,
}

/// Photoshop-style Manga Layer Style effects inside [`SfxBlending::effects`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SfxBlendEffect {
    /// Тиснение / Фаска — crisp manga SDF bevel highlight & shadow.
    Bevel {
        #[serde(default = "default_true")]
        enabled: bool,
        #[serde(default = "default_4")]
        depth_px: f32,
        #[serde(default = "default_120")]
        angle_deg: f32,
        #[serde(default = "default_white")]
        highlight_color: Color,
        #[serde(default = "default_0_75")]
        highlight_opacity: f32,
        #[serde(default = "default_black")]
        shadow_color: Color,
        #[serde(default = "default_0_75")]
        shadow_opacity: f32,
    },
    /// Выполнить обводку — stackable stroke (Outside/Center/Inside) with full fill parity.
    Stroke {
        #[serde(default = "default_true")]
        enabled: bool,
        #[serde(default = "default_4")]
        width_px: f32,
        #[serde(default)]
        position: SfxStrokePosition,
        #[serde(default)]
        join: SfxLineJoin,
        #[serde(default)]
        fill: SfxFillSource,
        #[serde(default)]
        blend: SfxChannelBlend,
        #[serde(default = "default_one_f32")]
        opacity: f32,
    },
    /// Внутренняя тень — stackable directional inner shadow.
    InnerShadow {
        #[serde(default = "default_true")]
        enabled: bool,
        #[serde(default = "default_120")]
        angle_deg: f32,
        #[serde(default = "default_6")]
        distance_px: f32,
        #[serde(default = "default_4")]
        blur_px: f32,
        #[serde(default = "default_black")]
        color: Color,
        #[serde(default = "default_multiply")]
        blend: SfxChannelBlend,
        #[serde(default = "default_0_75")]
        opacity: f32,
    },
    /// Внутреннее свечение — inner contour glow.
    InnerGlow {
        #[serde(default = "default_true")]
        enabled: bool,
        #[serde(default = "default_6")]
        width_px: f32,
        #[serde(default = "default_white")]
        color: Color,
        #[serde(default = "default_screen")]
        blend: SfxChannelBlend,
        #[serde(default = "default_0_75")]
        opacity: f32,
    },
    /// Наложение заливки (Color / Gradient / Dry Brush / Screentone / Shtack Fill) — full parity!
    FillOverlay {
        #[serde(default = "default_true")]
        enabled: bool,
        /// `false` = «2-я заливка (быстрый микс)», `true` = «Наложение узора / Dry / Tone».
        #[serde(default)]
        is_pattern: bool,
        #[serde(default)]
        fill: SfxFillSource,
        #[serde(default)]
        blend: SfxChannelBlend,
        #[serde(default = "default_one_f32")]
        opacity: f32,
    },
    /// Внешнее свечение — manga aura / outer bloom behind letters.
    OuterGlow {
        #[serde(default = "default_true")]
        enabled: bool,
        #[serde(default = "default_4")]
        spread_px: f32,
        #[serde(default = "default_10")]
        blur_px: f32,
        #[serde(default = "default_white")]
        color: Color,
        #[serde(default = "default_screen")]
        blend: SfxChannelBlend,
        #[serde(default = "default_0_75")]
        opacity: f32,
    },
    /// Направление тени / 3D — stackable drop shadow + optional 3D extrude along the shadow ray.
    DropShadow {
        #[serde(default = "default_true")]
        enabled: bool,
        #[serde(default = "default_120")]
        angle_deg: f32,
        #[serde(default = "default_8")]
        distance_px: f32,
        #[serde(default)]
        extrude_depth_px: f32,
        #[serde(default)]
        blur_px: f32,
        #[serde(default = "default_black")]
        color: Color,
        #[serde(default = "default_multiply")]
        blend: SfxChannelBlend,
        #[serde(default = "default_0_75")]
        opacity: f32,
    },
}

fn default_120() -> f32 {
    120.0
}

fn default_0_75() -> f32 {
    0.75
}

fn default_10() -> f32 {
    10.0
}

fn default_8() -> f32 {
    8.0
}

fn default_multiply() -> SfxChannelBlend {
    SfxChannelBlend::Multiply
}

fn default_screen() -> SfxChannelBlend {
    SfxChannelBlend::Screen
}

impl SfxBlendEffect {
    pub fn enabled(&self) -> bool {
        match self {
            Self::Bevel { enabled, .. }
            | Self::Stroke { enabled, .. }
            | Self::InnerShadow { enabled, .. }
            | Self::InnerGlow { enabled, .. }
            | Self::FillOverlay { enabled, .. }
            | Self::OuterGlow { enabled, .. }
            | Self::DropShadow { enabled, .. } => *enabled,
        }
    }

    pub fn enabled_mut(&mut self) -> &mut bool {
        match self {
            Self::Bevel { enabled, .. }
            | Self::Stroke { enabled, .. }
            | Self::InnerShadow { enabled, .. }
            | Self::InnerGlow { enabled, .. }
            | Self::FillOverlay { enabled, .. }
            | Self::OuterGlow { enabled, .. }
            | Self::DropShadow { enabled, .. } => enabled,
        }
    }

    pub fn validate(&mut self) {
        match self {
            Self::Bevel { depth_px, angle_deg, highlight_color, highlight_opacity, shadow_color, shadow_opacity, .. } => {
                *depth_px = finite_f32(*depth_px, 4.0).clamp(1.0, 64.0);
                *angle_deg = finite_f32(*angle_deg, 120.0).clamp(-180.0, 180.0);
                *highlight_color = finite_color(*highlight_color);
                *highlight_opacity = finite_f32(*highlight_opacity, 0.75).clamp(0.0, 1.0);
                *shadow_color = finite_color(*shadow_color);
                *shadow_opacity = finite_f32(*shadow_opacity, 0.75).clamp(0.0, 1.0);
            }
            Self::Stroke { width_px, fill, opacity, .. } => {
                *width_px = finite_f32(*width_px, 4.0).clamp(0.0, MAX_SFX_EFFECT_PX);
                fill.validate();
                *opacity = finite_f32(*opacity, 1.0).clamp(0.0, 1.0);
            }
            Self::InnerShadow { angle_deg, distance_px, blur_px, color, opacity, .. } => {
                *angle_deg = finite_f32(*angle_deg, 120.0).clamp(-180.0, 180.0);
                *distance_px = finite_f32(*distance_px, 6.0).clamp(0.0, 128.0);
                *blur_px = finite_f32(*blur_px, 4.0).clamp(0.0, 64.0);
                *color = finite_color(*color);
                *opacity = finite_f32(*opacity, 0.75).clamp(0.0, 1.0);
            }
            Self::InnerGlow { width_px, color, opacity, .. } => {
                *width_px = finite_f32(*width_px, 6.0).clamp(0.0, 64.0);
                *color = finite_color(*color);
                *opacity = finite_f32(*opacity, 0.75).clamp(0.0, 1.0);
            }
            Self::FillOverlay { fill, opacity, .. } => {
                fill.validate();
                *opacity = finite_f32(*opacity, 1.0).clamp(0.0, 1.0);
            }
            Self::OuterGlow { spread_px, blur_px, color, opacity, .. } => {
                *spread_px = finite_f32(*spread_px, 4.0).clamp(0.0, 64.0);
                *blur_px = finite_f32(*blur_px, 10.0).clamp(0.0, 64.0);
                *color = finite_color(*color);
                *opacity = finite_f32(*opacity, 0.75).clamp(0.0, 1.0);
            }
            Self::DropShadow { angle_deg, distance_px, extrude_depth_px, blur_px, color, opacity, .. } => {
                *angle_deg = finite_f32(*angle_deg, 120.0).clamp(-180.0, 180.0);
                *distance_px = finite_f32(*distance_px, 8.0).clamp(0.0, MAX_SFX_EFFECT_PX);
                *extrude_depth_px = finite_f32(*extrude_depth_px, 0.0).clamp(0.0, MAX_SFX_EFFECT_PX);
                *blur_px = finite_f32(*blur_px, 0.0).clamp(0.0, 64.0);
                *color = finite_color(*color);
                *opacity = finite_f32(*opacity, 0.75).clamp(0.0, 1.0);
            }
        }
    }
}

/// Independent channel mixing, ordering & Photoshop-style Manga Layer Effects (`Blending` tab).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SfxBlending {
    pub fill_blend: SfxChannelBlend,
    #[serde(default = "default_one_f32")]
    pub fill_opacity: f32,
    /// Optional second fill composited on top of the primary fill (kept for backward compatibility).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary_fill: Option<SfxFillSource>,
    pub secondary_blend: SfxChannelBlend,
    #[serde(default = "default_one_f32")]
    pub secondary_opacity: f32,
    pub line_blend: SfxChannelBlend,
    #[serde(default = "default_one_f32")]
    pub line_opacity: f32,
    /// When `true`, the stylized outline renders behind the fill channels instead of on top.
    pub line_behind_fill: bool,
    pub halo_blend: SfxChannelBlend,
    #[serde(default = "default_one_f32")]
    pub halo_opacity: f32,
    /// Photoshop-style stackable Manga Blending Effects (`Bevel`, `Stroke`, `InnerShadow`,
    /// `InnerGlow`, `FillOverlay`, `OuterGlow`, `DropShadow`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<SfxBlendEffect>,
}

impl Default for SfxBlending {
    fn default() -> Self {
        Self {
            fill_blend: SfxChannelBlend::Normal,
            fill_opacity: 1.0,
            secondary_fill: None,
            secondary_blend: SfxChannelBlend::Normal,
            secondary_opacity: 1.0,
            line_blend: SfxChannelBlend::Normal,
            line_opacity: 1.0,
            line_behind_fill: false,
            halo_blend: SfxChannelBlend::Normal,
            halo_opacity: 1.0,
            effects: Vec::new(),
        }
    }
}

impl SfxBlending {
    pub fn validate(&mut self) {
        self.fill_opacity = finite_f32(self.fill_opacity, 1.0).clamp(0.0, 1.0);
        if let Some(sec) = &mut self.secondary_fill {
            sec.validate();
            if matches!(sec, SfxFillSource::None) {
                self.secondary_fill = None;
            }
        }
        self.secondary_opacity = finite_f32(self.secondary_opacity, 1.0).clamp(0.0, 1.0);
        self.line_opacity = finite_f32(self.line_opacity, 1.0).clamp(0.0, 1.0);
        self.halo_opacity = finite_f32(self.halo_opacity, 1.0).clamp(0.0, 1.0);
        if self.effects.len() > MAX_SFX_BLEND_EFFECTS {
            self.effects.truncate(MAX_SFX_BLEND_EFFECTS);
        }
        for fx in &mut self.effects {
            fx.validate();
        }
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
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
    pub blending: SfxBlending,
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
            warp_mode: SfxWarpMode::Resize,
            free_transform: SfxFreeTransform::default(),
            spline: SfxSplineArc::default(),
            per_glyph: Vec::new(),
            mesh: SfxMeshLattice::default(),
            liquify_strokes: Vec::new(),
            fill: SfxFillSource::Solid { color: default_ink() },
            blending: SfxBlending::default(),
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
        self.blending.validate();
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
            blending: self.blending.clone(),
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
        self.blending = preset.blending.clone();
        // A 450 pt SFX applying a 64 pt preset would otherwise blow a 10 px halo up to 70+ px
        // (past `dilate_coverage_approx`'s 64 px cap) and render as a gray fog. Grow sub-linearly
        // and cap during preset application; the manual halo slider stays free up to
        // `MAX_SFX_EFFECT_PX`.
        let preset_halo = finite_f32(preset.line_effect.halo_width_px, 0.0).max(0.0);
        self.line_effect = preset.line_effect;
        self.dry_brush = preset.dry_brush;
        self.vector_shadow = preset.vector_shadow;
        self.styles = preset.styles.clone();
        self.distress = preset.distress;
        if (scale - 1.0).abs() > 1e-4 {
            scale_sfx_pixel_effects(self, scale);
        }
        // Apply the sub-linear halo / sanity cap AFTER the generic pixel scaling so it cannot be
        // re-scaled by `scale_sfx_pixel_effects` on large documents.
        self.line_effect.halo_width_px = (f64::from(preset_halo) * scale.powf(0.7)).clamp(0.0, 64.0) as f32;
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
    data.line_effect.shibufude_spread_px = scaled(data.line_effect.shibufude_spread_px).max(4.0);
    // `shtack_overshoot_px`, `shtack_thickness_px`, `sugomi_spike_amp_pct`, `sugomi_cleft_dip_pct`,
    // and all percentage fields scale naturally with glyph geometry and must NEVER be double-scaled here.
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
    pub blending: SfxBlending,
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
        self.blending.validate();
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
        let expected_halo = (10.0_f64 * 4.0_f64.powf(0.7)).min(64.0) as f32;
        assert!((data.line_effect.halo_width_px - expected_halo).abs() < 1e-3, "halo must grow sub-linearly on preset apply");
        assert!(data.line_effect.halo_width_px <= 64.0);
        assert!((data.vector_shadow.distance_px - 48.0).abs() < 1e-3, "shadow distance scales 4x");
        assert!((data.line_effect.shtack_overshoot_px - 20.0).abs() < 1e-3, "needle overshoot % must NOT double-scale");
    }

    #[test]
    fn shtack_thickness_is_a_glyph_height_percentage_never_a_pixel_length() {
        let mut data = SfxData::default();
        assert_eq!(data.line_effect.shtack_thickness_px, 2.0);
        assert_eq!(data.warp_mode, SfxWarpMode::Resize, "Resize is the default on-canvas mode");

        // New-document scaling and presets must leave the percentage untouched.
        let mut large = data.clone();
        scale_new_sfx_for_document(&mut large, 12_303, 17_499);
        assert_eq!(large.line_effect.shtack_thickness_px, 2.0);
        assert_eq!(large.line_effect.shtack_overshoot_px, 20.0);

        let preset = SfxPreset {
            size_pt: 64.0,
            line_effect: SfxLineEffect { shtack_thickness_px: 2.0, halo_width_px: 10.0, ..SfxLineEffect::default() },
            ..SfxPreset::default()
        };
        let mut big = SfxData { size_pt: 450.0, ..SfxData::default() };
        big.apply_preset(&preset);
        assert_eq!(big.line_effect.shtack_thickness_px, 2.0, "preset must not double-scale thickness %");
        assert!(big.line_effect.halo_width_px <= 64.0, "preset halo must stay within the sane cap");

        // Validation clamps hostile values into the supported 0.5..=8.0 range.
        data.line_effect.shtack_thickness_px = 999.0;
        data.validate();
        assert_eq!(data.line_effect.shtack_thickness_px, 8.0);
        data.line_effect.shtack_thickness_px = f32::NAN;
        data.validate();
        assert_eq!(data.line_effect.shtack_thickness_px, 2.0);
        data.line_effect.shtack_thickness_px = 0.0;
        data.validate();
        assert_eq!(data.line_effect.shtack_thickness_px, 0.5);
    }

    #[test]
    fn apply_preset_bounds_halo_width_on_large_documents() {
        let preset = SfxPreset { size_pt: 64.0, line_effect: SfxLineEffect { halo_width_px: 10.0, ..SfxLineEffect::default() }, ..SfxPreset::default() };
        let mut data = SfxData { size_pt: 450.0, ..SfxData::default() };
        data.apply_preset(&preset);
        let naive = 10.0_f64 * (450.0 / 64.0);
        assert!(naive > 64.0, "the old linear scaling really did exceed the dilation cap");
        assert!(data.line_effect.halo_width_px <= 64.0);
        assert!(data.line_effect.halo_width_px > 10.0);

        // A manual slider value up to MAX_SFX_EFFECT_PX is still accepted untouched.
        let mut manual = SfxData::default();
        manual.line_effect.halo_width_px = 512.0;
        manual.validate();
        assert_eq!(manual.line_effect.halo_width_px, 512.0);
    }

    #[test]
    fn grain_sharpness_fields_deserialize_from_legacy_and_validate() {
        let legacy: SfxData = serde_json::from_str(r#"{"text":"BOOM"}"#).unwrap();
        assert_eq!(legacy.line_effect.shtack_grain_sharpness_pct, 100.0);
        assert_eq!(legacy.dry_brush.grain_sharpness_pct, 100.0);
        assert_eq!(legacy.line_effect.line_grain_pct, 0.0);

        let mut data = SfxData::default();
        data.line_effect.shtack_grain_sharpness_pct = 500.0;
        data.line_effect.line_grain_pct = 180.0;
        data.dry_brush.grain_sharpness_pct = f32::NAN;
        data.validate();
        assert_eq!(data.line_effect.shtack_grain_sharpness_pct, 100.0);
        assert_eq!(data.line_effect.line_grain_pct, 100.0);
        assert_eq!(data.dry_brush.grain_sharpness_pct, 100.0);
    }

    #[test]
    fn default_ink_is_pure_black_and_dry_brush_engine_defaults_are_calibrated() {
        let data = SfxData::default();
        assert_eq!(data.line_effect.line_color, Color::BLACK, "default line ink must be pure #000000");
        assert_eq!(data.fill, SfxFillSource::Solid { color: Color::BLACK }, "default fill ink must be pure #000000");
        assert_eq!(data.vector_shadow.color, Color::BLACK);
        assert_eq!(data.dry_brush.grain_sharpness_pct, 100.0);

        let etf = SfxDryBrushSpec::for_engine(SfxDryBrushEngine::SdfEtf);
        assert_eq!((etf.dryness_pct, etf.bristle_size_pct, etf.freq_pct, etf.lic_length_px), (54.0, 100.0, 68.0, 96.0));
        assert_eq!((etf.warp_pct, etf.paper_grain_pct, etf.grain_sharpness_pct, etf.edge_wear_pct), (60.0, 65.0, 100.0, 8.0));

        let seg = SfxDryBrushSpec::for_engine(SfxDryBrushEngine::SdfSeg);
        assert_eq!((seg.dryness_pct, seg.bristle_size_pct, seg.freq_pct, seg.warp_pct), (39.0, 36.0, 60.0, 47.0));
        assert_eq!((seg.paper_grain_pct, seg.grain_sharpness_pct, seg.edge_wear_pct), (37.0, 100.0, 65.0));

        let sdf = SfxDryBrushSpec::for_engine(SfxDryBrushEngine::SdfPhysics);
        assert_eq!((sdf.dryness_pct, sdf.bristle_size_pct, sdf.freq_pct, sdf.warp_pct), (45.0, 30.0, 27.0, 13.0));
        assert_eq!((sdf.paper_grain_pct, sdf.grain_sharpness_pct, sdf.edge_wear_pct, sdf.angle_deg), (75.0, 100.0, 56.0, 90.0));
    }

    #[test]
    fn shtack_fill_none_and_blending_roundtrip_and_validate() {
        let mut data = SfxData {
            fill: SfxFillSource::ShtackHatch {
                spec: SfxShtackFillSpec {
                    angle_deg: 200.0,
                    density_pct: 999.0,
                    thickness_pct: 0.0,
                    overshoot_pct: f32::NAN,
                    cross_hatch: true,
                    ..SfxShtackFillSpec::default()
                },
            },
            blending: SfxBlending {
                fill_blend: SfxChannelBlend::Multiply,
                fill_opacity: 1.5,
                secondary_fill: Some(SfxFillSource::None),
                secondary_blend: SfxChannelBlend::Knockout,
                secondary_opacity: -0.2,
                line_blend: SfxChannelBlend::Screen,
                line_opacity: f32::NAN,
                line_behind_fill: true,
                halo_blend: SfxChannelBlend::Normal,
                halo_opacity: 0.75,
                effects: vec![
                    SfxBlendEffect::DropShadow {
                        enabled: true,
                        angle_deg: 135.0,
                        distance_px: 12.0,
                        extrude_depth_px: 8.0,
                        blur_px: 0.0,
                        color: Color::BLACK,
                        blend: SfxChannelBlend::Multiply,
                        opacity: 0.9,
                    },
                    SfxBlendEffect::FillOverlay {
                        enabled: true,
                        is_pattern: true,
                        fill: SfxFillSource::DryBrush { color: Color::BLACK, spec: SfxDryBrushSpec::default() },
                        blend: SfxChannelBlend::SoftLight,
                        opacity: 0.8,
                    },
                ],
            },
            ..SfxData::default()
        };
        data.validate();
        let SfxFillSource::ShtackHatch { spec } = &data.fill else {
            panic!("expected ShtackHatch fill");
        };
        assert_eq!(spec.angle_deg, 90.0);
        assert_eq!(spec.density_pct, 100.0);
        assert_eq!(spec.thickness_pct, 0.4);
        assert_eq!(spec.overshoot_pct, 4.0);
        assert_eq!(data.blending.fill_opacity, 1.0);
        assert!(data.blending.secondary_fill.is_none(), "None secondary fill normalizes to Option::None");
        assert_eq!(data.blending.secondary_opacity, 0.0);
        assert_eq!(data.blending.line_opacity, 1.0);
        assert!(data.blending.line_behind_fill);

        data.blending.secondary_fill = Some(SfxFillSource::ShtackHatch {
            spec: SfxShtackFillSpec {
                mode: SfxShtackFillMode::StrokeFlow,
                overshoot_pct: 90.0,
                ..SfxShtackFillSpec::default()
            },
        });
        data.validate();
        let Some(SfxFillSource::ShtackHatch { spec: sec_spec }) = &data.blending.secondary_fill else {
            panic!("expected ShtackHatch secondary fill");
        };
        assert_eq!(sec_spec.mode, SfxShtackFillMode::StrokeFlow);
        assert_eq!(sec_spec.overshoot_pct, 45.0);
        let json = serde_json::to_string(&data).unwrap();
        let parsed: SfxData = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.fill, data.fill);
        assert_eq!(parsed.blending, data.blending);

        let hollow = SfxData { fill: SfxFillSource::None, ..SfxData::default() };
        let hj = serde_json::to_string(&hollow).unwrap();
        let hp: SfxData = serde_json::from_str(&hj).unwrap();
        assert_eq!(hp.fill, SfxFillSource::None);
    }
}