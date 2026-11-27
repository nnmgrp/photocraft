//! Canonical configuration schema (see docs/config-schema.json).
//! Field order is part of the contract: `config_hash` hashes compact canonical
//! JSON in exactly this order and must match `KConfig.hash()` in JavaScript.
//!
//! Backward compatibility:
//! * `schemaVersion <= 2` vectors (`t01..t06.json`) serialize only `KEY_ORDER[0..38]`
//!   and deserialize missing v3+ fields with v2-compatible defaults (`patternFamily =
//!   "kakeami-cluster"`, `finishMode = "clean"`, `inkPooling = 0.0`).
//! * `schemaVersion >= 3` vectors serialize all 74 keys (`KEY_ORDER[0..74]`).

use serde::{Deserialize, Serialize};

fn default_pattern_family_v2() -> String { "kakeami-cluster".into() }
fn default_size_grade() -> String { "M".into() }
fn default_finish_mode_v2() -> String { "clean".into() }
fn default_ink_pooling_v2() -> f64 { 0.0 }
fn default_grain_strength() -> f64 { 55.0 }
fn default_macro_dot_px() -> u32 { 3 }
fn default_nawa_tone_style() -> String { "normal".into() }
fn default_nawa_rope_width() -> f64 { 28.0 }
fn default_nawa_coil_radius() -> f64 { 48.0 }
fn default_nawa_strands() -> u32 { 5 }
fn default_nawa_twist_step() -> f64 { 6.5 }
fn default_nawa_twist_angle() -> f64 { 42.0 }
fn default_nawa_outline_pad() -> f64 { 0.9 }
fn default_nawa_coil_style() -> String { "skein".into() }
fn default_nawa_join_style() -> String { "smooth-60l".into() }
fn default_nawa_tip_style() -> String { "blunt".into() }
fn default_nawa_grad_mode() -> String { "width".into() }
fn default_nawa_lpi_preset() -> String { "60L".into() }
fn default_nawa_scale() -> f64 { 1.0 }
fn default_nawa_curliness() -> f64 { 50.0 }
fn default_zero_f64() -> f64 { 0.0 }
fn default_nawa_vortex_mode() -> String { "off".into() }
fn default_half_f64() -> f64 { 0.5 }
fn default_nawa_vortex_strength() -> f64 { 65.0 }
fn default_nawa_vortex_radius() -> f64 { 0.38 }
fn default_stipple_dot_shape() -> String { "round".into() }
fn default_stipple_dot_size() -> f64 { 2.2 }
fn default_stipple_contrast() -> f64 { 55.0 }
fn default_kake_patch_style() -> String { "closed-4l".into() }
fn default_kake_grid_divs() -> u32 { 4 }
fn default_kake_occlude_mode() -> String { "card".into() }
fn default_kake_fade_mode() -> String { "stipple".into() }
fn default_kake_fade_threshold() -> f64 { 0.36 }

/// All numeric fields are f64 in the oracle: the builder performs f64 math and
/// only narrows the final vertex buffer to f32.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KakeamiConfig {
    // --- v2 canonical keys (0..38) -------------------------------------------
    pub schema_version: u32,
    pub seed: u32,
    pub preset: String,
    pub active_style: String,
    pub width: u32,
    pub height: u32,
    pub tile_size: u32,
    pub layer_cap: String,
    pub geometry_mode: String,
    pub parquet_weave: u32,
    pub darken_cap: u32,
    pub gamma: f64,
    pub grad_start: u32,
    pub grad_end: u32,
    pub line_spacing: f64,
    pub stroke_width: f64,
    pub cluster_size: f64,
    pub base_angle: f64,
    pub min_lines: u32,
    pub max_lines: u32,
    pub overlap_density: u32,
    pub angle_randomness: u32,
    pub cross_angle: u32,
    pub scatter_jitter: u32,
    pub nib_taper: u32,
    pub wobble: u32,
    pub broken_ink: u32,
    pub one_bit_mode: bool,
    pub transparent_bg: bool,
    pub invert_ink: bool,
    pub seamless: bool,
    pub one_bit_threshold: u32,
    pub paper_tooth: u32,
    pub supersample2x: bool,
    pub ribbon_samples: u32,
    pub taper_exponent: f64,
    pub fan_spread: f64,
    pub cluster_slant: f64,

    // --- v3 additions (38..51, strictly append-only) -------------------------
    #[serde(default = "default_pattern_family_v2")]
    pub pattern_family: String,
    #[serde(default = "default_size_grade")]
    pub size_grade: String,
    #[serde(default = "default_finish_mode_v2")]
    pub finish_mode: String,
    #[serde(default = "default_ink_pooling_v2")]
    pub ink_pooling: f64,
    #[serde(default = "default_grain_strength")]
    pub grain_strength: f64,
    #[serde(default = "default_macro_dot_px")]
    pub macro_dot_px: u32,
    #[serde(default = "default_nawa_tone_style")]
    pub nawa_tone_style: String,
    #[serde(default = "default_nawa_rope_width")]
    pub nawa_rope_width: f64,
    #[serde(default = "default_nawa_coil_radius")]
    pub nawa_coil_radius: f64,
    #[serde(default = "default_nawa_strands")]
    pub nawa_strands: u32,
    #[serde(default = "default_nawa_twist_step")]
    pub nawa_twist_step: f64,
    #[serde(default = "default_nawa_twist_angle")]
    pub nawa_twist_angle: f64,
    #[serde(default = "default_nawa_outline_pad")]
    pub nawa_outline_pad: f64,

    // --- v6 nawa-ami structural axes (51..55) --------------------------------
    #[serde(default = "default_nawa_coil_style")]
    pub nawa_coil_style: String,
    #[serde(default = "default_nawa_join_style")]
    pub nawa_join_style: String,
    #[serde(default = "default_nawa_tip_style")]
    pub nawa_tip_style: String,
    #[serde(default = "default_nawa_grad_mode")]
    pub nawa_grad_mode: String,

    // --- v7 LPI presets, morphing, canvas vortex, clean stipple (55..69) -----
    #[serde(default = "default_nawa_lpi_preset")]
    pub nawa_lpi_preset: String,
    #[serde(default = "default_nawa_scale")]
    pub nawa_scale: f64,
    #[serde(default = "default_nawa_curliness")]
    pub nawa_curliness: f64,
    #[serde(default = "default_zero_f64")]
    pub nawa_flow_bias: f64,
    #[serde(default = "default_zero_f64")]
    pub nawa_flow_angle: f64,
    #[serde(default = "default_zero_f64")]
    pub nawa_width_var: f64,
    #[serde(default = "default_nawa_vortex_mode")]
    pub nawa_vortex_mode: String,
    #[serde(default = "default_half_f64")]
    pub nawa_vortex_x: f64,
    #[serde(default = "default_half_f64")]
    pub nawa_vortex_y: f64,
    #[serde(default = "default_nawa_vortex_strength")]
    pub nawa_vortex_strength: f64,
    #[serde(default = "default_nawa_vortex_radius")]
    pub nawa_vortex_radius: f64,
    #[serde(default = "default_stipple_dot_shape")]
    pub stipple_dot_shape: String,
    #[serde(default = "default_stipple_dot_size")]
    pub stipple_dot_size: f64,
    #[serde(default = "default_stipple_contrast")]
    pub stipple_contrast: f64,

    // --- v8 Kakeami next-gen orthogonal axes (69..74) ------------------------
    #[serde(default = "default_kake_patch_style")]
    pub kake_patch_style: String,
    #[serde(default = "default_kake_grid_divs")]
    pub kake_grid_divs: u32,
    #[serde(default = "default_kake_occlude_mode")]
    pub kake_occlude_mode: String,
    #[serde(default = "default_kake_fade_mode")]
    pub kake_fade_mode: String,
    #[serde(default = "default_kake_fade_threshold")]
    pub kake_fade_threshold: f64,
}

/// Canonical key order used by JavaScript `KConfig.KEY_ORDER` (74 keys).
pub const KEY_ORDER: &[&str] = &[
    // v2 (0..38)
    "schemaVersion", "seed", "preset", "activeStyle", "width", "height", "tileSize",
    "layerCap", "geometryMode", "parquetWeave", "darkenCap", "gamma", "gradStart", "gradEnd",
    "lineSpacing", "strokeWidth", "clusterSize", "baseAngle", "minLines", "maxLines",
    "overlapDensity", "angleRandomness", "crossAngle", "scatterJitter", "nibTaper", "wobble",
    "brokenInk", "oneBitMode", "transparentBg", "invertInk", "seamless", "oneBitThreshold",
    "paperTooth", "supersample2x", "ribbonSamples", "taperExponent", "fanSpread", "clusterSlant",
    // v3 (38..51)
    "patternFamily", "sizeGrade", "finishMode", "inkPooling", "grainStrength", "macroDotPx",
    "nawaToneStyle", "nawaRopeWidth", "nawaCoilRadius", "nawaStrands", "nawaTwistStep",
    "nawaTwistAngle", "nawaOutlinePad",
    // v6 (51..55)
    "nawaCoilStyle", "nawaJoinStyle", "nawaTipStyle", "nawaGradMode",
    // v7 (55..69)
    "nawaLpiPreset", "nawaScale", "nawaCurliness", "nawaFlowBias", "nawaFlowAngle",
    "nawaWidthVar", "nawaVortexMode", "nawaVortexX", "nawaVortexY",
    "nawaVortexStrength", "nawaVortexRadius",
    "stippleDotShape", "stippleDotSize", "stippleContrast",
    // v8 (69..74)
    "kakePatchStyle", "kakeGridDivs", "kakeOccludeMode", "kakeFadeMode", "kakeFadeThreshold",
];

impl KakeamiConfig {
    /// FNV-1a over the low byte of every char of the canonical JSON string.
    pub fn fnv1a(s: &str) -> u32 {
        let mut h = 0x811c9dc5u32;
        for b in s.bytes() {
            h = (h ^ (b as u32)).wrapping_mul(0x0100_0193);
        }
        h
    }

    /// `JSON.stringify(canonical(config))` — no pretty printing, fixed order.
    /// `schema_version <= 2` emits the legacy 38-key slice so `t01..t06` hashes
    /// remain bit-exact; `schema_version >= 3` emits all 74 keys.
    #[allow(unused_assignments)]
    pub fn canonical_json(&self) -> String {
        let mut s = String::with_capacity(1400);
        s.push('{');
        let mut first = true;
        macro_rules! kv {
            ($key:expr, $value:expr) => {{
                if !first {
                    s.push(',');
                }
                first = false;
                s.push('"');
                s.push_str($key);
                s.push_str("\":");
                s.push_str(&$value);
            }};
        }
        macro_rules! num {
            ($v:expr) => {{
                let val: f64 = *$v;
                if val == 0.0 {
                    // Guards against -0.0 -> "-0" (JS JSON.stringify(-0) === "0")
                    "0".to_owned()
                } else if val.fract() == 0.0 && val.abs() < 9_007_199_254_740_992.0 {
                    format!("{}", val as i64)
                } else {
                    serde_json::to_string(&val).unwrap()
                }
            }};
        }
        macro_rules! uint {
            ($v:expr) => { $v.to_string() };
        }
        macro_rules! boolean {
            ($v:expr) => { if $v { "true".to_owned() } else { "false".to_owned() } };
        }
        macro_rules! string {
            ($v:expr) => { serde_json::to_string(&$v).unwrap() };
        }

        kv!(KEY_ORDER[0], uint!(self.schema_version));
        kv!(KEY_ORDER[1], uint!(self.seed));
        kv!(KEY_ORDER[2], string!(self.preset));
        kv!(KEY_ORDER[3], string!(self.active_style));
        kv!(KEY_ORDER[4], uint!(self.width));
        kv!(KEY_ORDER[5], uint!(self.height));
        kv!(KEY_ORDER[6], uint!(self.tile_size));
        kv!(KEY_ORDER[7], string!(self.layer_cap));
        kv!(KEY_ORDER[8], string!(self.geometry_mode));
        kv!(KEY_ORDER[9], uint!(self.parquet_weave));
        kv!(KEY_ORDER[10], uint!(self.darken_cap));
        kv!(KEY_ORDER[11], num!(&self.gamma));
        kv!(KEY_ORDER[12], uint!(self.grad_start));
        kv!(KEY_ORDER[13], uint!(self.grad_end));
        kv!(KEY_ORDER[14], num!(&self.line_spacing));
        kv!(KEY_ORDER[15], num!(&self.stroke_width));
        kv!(KEY_ORDER[16], num!(&self.cluster_size));
        kv!(KEY_ORDER[17], num!(&self.base_angle));
        kv!(KEY_ORDER[18], uint!(self.min_lines));
        kv!(KEY_ORDER[19], uint!(self.max_lines));
        kv!(KEY_ORDER[20], uint!(self.overlap_density));
        kv!(KEY_ORDER[21], uint!(self.angle_randomness));
        kv!(KEY_ORDER[22], uint!(self.cross_angle));
        kv!(KEY_ORDER[23], uint!(self.scatter_jitter));
        kv!(KEY_ORDER[24], uint!(self.nib_taper));
        kv!(KEY_ORDER[25], uint!(self.wobble));
        kv!(KEY_ORDER[26], uint!(self.broken_ink));
        kv!(KEY_ORDER[27], boolean!(self.one_bit_mode));
        kv!(KEY_ORDER[28], boolean!(self.transparent_bg));
        kv!(KEY_ORDER[29], boolean!(self.invert_ink));
        kv!(KEY_ORDER[30], boolean!(self.seamless));
        kv!(KEY_ORDER[31], uint!(self.one_bit_threshold));
        kv!(KEY_ORDER[32], uint!(self.paper_tooth));
        kv!(KEY_ORDER[33], boolean!(self.supersample2x));
        kv!(KEY_ORDER[34], uint!(self.ribbon_samples));
        kv!(KEY_ORDER[35], num!(&self.taper_exponent));
        kv!(KEY_ORDER[36], num!(&self.fan_spread));
        kv!(KEY_ORDER[37], num!(&self.cluster_slant));

        if self.schema_version >= 3 {
            kv!(KEY_ORDER[38], string!(self.pattern_family));
            kv!(KEY_ORDER[39], string!(self.size_grade));
            kv!(KEY_ORDER[40], string!(self.finish_mode));
            kv!(KEY_ORDER[41], num!(&self.ink_pooling));
            kv!(KEY_ORDER[42], num!(&self.grain_strength));
            kv!(KEY_ORDER[43], uint!(self.macro_dot_px));
            kv!(KEY_ORDER[44], string!(self.nawa_tone_style));
            kv!(KEY_ORDER[45], num!(&self.nawa_rope_width));
            kv!(KEY_ORDER[46], num!(&self.nawa_coil_radius));
            kv!(KEY_ORDER[47], uint!(self.nawa_strands));
            kv!(KEY_ORDER[48], num!(&self.nawa_twist_step));
            kv!(KEY_ORDER[49], num!(&self.nawa_twist_angle));
            kv!(KEY_ORDER[50], num!(&self.nawa_outline_pad));

            kv!(KEY_ORDER[51], string!(self.nawa_coil_style));
            kv!(KEY_ORDER[52], string!(self.nawa_join_style));
            kv!(KEY_ORDER[53], string!(self.nawa_tip_style));
            kv!(KEY_ORDER[54], string!(self.nawa_grad_mode));

            kv!(KEY_ORDER[55], string!(self.nawa_lpi_preset));
            kv!(KEY_ORDER[56], num!(&self.nawa_scale));
            kv!(KEY_ORDER[57], num!(&self.nawa_curliness));
            kv!(KEY_ORDER[58], num!(&self.nawa_flow_bias));
            kv!(KEY_ORDER[59], num!(&self.nawa_flow_angle));
            kv!(KEY_ORDER[60], num!(&self.nawa_width_var));
            kv!(KEY_ORDER[61], string!(self.nawa_vortex_mode));
            kv!(KEY_ORDER[62], num!(&self.nawa_vortex_x));
            kv!(KEY_ORDER[63], num!(&self.nawa_vortex_y));
            kv!(KEY_ORDER[64], num!(&self.nawa_vortex_strength));
            kv!(KEY_ORDER[65], num!(&self.nawa_vortex_radius));
            kv!(KEY_ORDER[66], string!(self.stipple_dot_shape));
            kv!(KEY_ORDER[67], num!(&self.stipple_dot_size));
            kv!(KEY_ORDER[68], num!(&self.stipple_contrast));

            kv!(KEY_ORDER[69], string!(self.kake_patch_style));
            kv!(KEY_ORDER[70], uint!(self.kake_grid_divs));
            kv!(KEY_ORDER[71], string!(self.kake_occlude_mode));
            kv!(KEY_ORDER[72], string!(self.kake_fade_mode));
            kv!(KEY_ORDER[73], num!(&self.kake_fade_threshold));
        }

        s.push('}');
        s
    }

    pub fn config_hash(&self) -> u32 {
        Self::fnv1a(&self.canonical_json())
    }
}

impl Default for KakeamiConfig {
    fn default() -> Self {
        Self {
            schema_version: 3,
            seed: 104829,
            preset: "linear-bt".into(),
            active_style: "tutorial".into(),
            width: 3840,
            height: 2160,
            tile_size: 512,
            layer_cap: "auto".into(),
            geometry_mode: "tutorial".into(),
            parquet_weave: 65,
            darken_cap: 68,
            gamma: 1.2,
            grad_start: 5,
            grad_end: 95,
            line_spacing: 8.5,
            stroke_width: 1.3,
            cluster_size: 110.0,
            base_angle: 10.0,
            min_lines: 4,
            max_lines: 9,
            overlap_density: 115,
            angle_randomness: 26,
            cross_angle: 90,
            scatter_jitter: 30,
            nib_taper: 50,
            wobble: 22,
            broken_ink: 20,
            one_bit_mode: true,
            transparent_bg: false,
            invert_ink: false,
            seamless: false,
            one_bit_threshold: 165,
            paper_tooth: 0,
            supersample2x: false,
            ribbon_samples: 8,
            taper_exponent: 1.0,
            fan_spread: 2.5,
            cluster_slant: 0.35,

            pattern_family: "kakeami-4".into(),
            size_grade: "M".into(),
            finish_mode: "sugomi".into(),
            ink_pooling: 35.0,
            grain_strength: 55.0,
            macro_dot_px: 3,
            nawa_tone_style: "normal".into(),
            nawa_rope_width: 28.0,
            nawa_coil_radius: 48.0,
            nawa_strands: 5,
            nawa_twist_step: 6.5,
            nawa_twist_angle: 42.0,
            nawa_outline_pad: 0.9,

            nawa_coil_style: "skein".into(),
            nawa_join_style: "smooth-60l".into(),
            nawa_tip_style: "blunt".into(),
            nawa_grad_mode: "width".into(),

            nawa_lpi_preset: "60L".into(),
            nawa_scale: 1.0,
            nawa_curliness: 50.0,
            nawa_flow_bias: 0.0,
            nawa_flow_angle: 0.0,
            nawa_width_var: 0.0,
            nawa_vortex_mode: "off".into(),
            nawa_vortex_x: 0.5,
            nawa_vortex_y: 0.5,
            nawa_vortex_strength: 65.0,
            nawa_vortex_radius: 0.38,
            stipple_dot_shape: "round".into(),
            stipple_dot_size: 2.2,
            stipple_contrast: 55.0,

            kake_patch_style: "closed-4l".into(),
            kake_grid_divs: 4,
            kake_occlude_mode: "card".into(),
            kake_fade_mode: "stipple".into(),
            kake_fade_threshold: 0.36,
        }
    }
}
