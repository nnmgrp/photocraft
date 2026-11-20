//! Vector data: paths (Pen tool), shape layers, saved/work paths and vector masks. Pure data;
//! rasterization lives in `photocraft-vector`, PSD mapping in `photocraft-io`.
//!
//! Coordinates are **document pixels** (f64, y down), cubic Bézier. A subpath is a list of knots;
//! segment `i` runs from `knots[i].anchor` via `knots[i].out_ctrl` and `knots[i+1].in_ctrl` to
//! `knots[i+1].anchor` (closed subpaths add the segment back to the first knot). A knot whose
//! control points equal its anchor is a corner with straight segments.
//!
//! Path operations follow Photoshop: every subpath is filled on its own (with the path's
//! [`FillRule`]) and folded, in order, into the result with its [`PathOp`]; the first subpath
//! always acts as "combine" (see [`Path::effective_op`]; verified against Photoshop's pixels in
//! the PSD corpus). Subpaths marked [`PathOp::Join`] belong to the shape component before them
//! and are filled together with it (a custom shape's holes). [`Path::inverted`] inverts the final area. A vector mask without subpaths
//! reveals everything (Photoshop's fresh "Add Vector Mask").

use std::sync::Arc;

use photocraft_color::Color;
use photocraft_geom::{Affine, Point};
use serde::{Deserialize, Serialize};

use crate::{Fill, Surface};

/// A Bézier knot (anchor + two control points).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Knot {
    pub anchor: Point,
    /// Control point of the segment arriving at this knot.
    pub in_ctrl: Point,
    /// Control point of the segment leaving this knot.
    pub out_ctrl: Point,
    /// Smooth (linked) knot: the Pen tool keeps both handles collinear.
    #[serde(default)]
    pub smooth: bool,
}

impl Knot {
    /// A corner knot with retracted handles.
    pub fn corner(x: f64, y: f64) -> Self {
        let p = Point::new(x, y);
        Knot { anchor: p, in_ctrl: p, out_ctrl: p, smooth: false }
    }
    /// A smooth knot with explicit handles.
    pub fn smooth(anchor: Point, in_ctrl: Point, out_ctrl: Point) -> Self {
        Knot { anchor, in_ctrl, out_ctrl, smooth: true }
    }
    pub fn transform(&self, a: &Affine) -> Knot {
        Knot { anchor: a.apply(self.anchor), in_ctrl: a.apply(self.in_ctrl), out_ctrl: a.apply(self.out_ctrl), smooth: self.smooth }
    }
}

/// How a subpath combines with the subpaths before it (Photoshop's path operations).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathOp {
    /// Combine shapes (union).
    #[default]
    Combine,
    /// Subtract front shape.
    Subtract,
    /// Intersect shape areas.
    Intersect,
    /// Exclude overlapping shapes (xor).
    Exclude,
    /// Part of the previous subpath's shape component (PSD operation -1 after a component's
    /// first record): the component's subpaths are filled together, with the path's fill rule,
    /// so inner subpaths wound the other way cut holes (custom shapes).
    Join,
}

/// Winding rule used to fill each subpath.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FillRule {
    #[default]
    NonZero,
    EvenOdd,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Subpath {
    pub closed: bool,
    pub knots: Vec<Knot>,
    #[serde(default)]
    pub op: PathOp,
}

impl Subpath {
    /// Closed polygon through `pts` (corner knots).
    pub fn polygon(pts: &[(f64, f64)]) -> Self {
        Subpath { closed: true, knots: pts.iter().map(|&(x, y)| Knot::corner(x, y)).collect(), op: PathOp::Combine }
    }
    /// Open polyline through `pts`.
    pub fn polyline(pts: &[(f64, f64)]) -> Self {
        Subpath { closed: false, ..Self::polygon(pts) }
    }
    pub fn with_op(mut self, op: PathOp) -> Self {
        self.op = op;
        self
    }
    /// Cubic segments `[p0, c1, c2, p1]`.
    pub fn segments(&self) -> Vec<[Point; 4]> {
        let n = self.knots.len();
        if n < 2 {
            return Vec::new();
        }
        let count = if self.closed { n } else { n - 1 };
        (0..count)
            .map(|i| {
                let a = &self.knots[i];
                let b = &self.knots[(i + 1) % n];
                [a.anchor, a.out_ctrl, b.in_ctrl, b.anchor]
            })
            .collect()
    }
}

/// A vector path: subpaths plus how they are filled and combined.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Path {
    pub subpaths: Vec<Subpath>,
    #[serde(default)]
    pub fill_rule: FillRule,
    /// The final area is inverted (PSD vector-mask invert flag / "initial fill rule" = 1).
    #[serde(default)]
    pub inverted: bool,
}

impl Path {
    pub fn new(subpaths: Vec<Subpath>) -> Self {
        Path { subpaths, fill_rule: FillRule::NonZero, inverted: false }
    }
    pub fn is_empty(&self) -> bool {
        self.subpaths.iter().all(|s| s.knots.is_empty())
    }
    /// The operation actually applied for subpath `i`: Photoshop applies the first subpath as
    /// "combine" whatever its stored operation (a lone "intersect" shape still draws).
    pub fn effective_op(&self, i: usize) -> PathOp {
        if i == 0 { PathOp::Combine } else { self.subpaths[i].op }
    }
    /// Shape components: ranges of subpaths filled together (a subpath starts a new component
    /// unless it is a [`PathOp::Join`]), each folded in with its first subpath's
    /// [`Path::effective_op`].
    pub fn components(&self) -> Vec<std::ops::Range<usize>> {
        let mut out: Vec<std::ops::Range<usize>> = Vec::new();
        for (i, s) in self.subpaths.iter().enumerate() {
            match out.last_mut() {
                Some(r) if s.op == PathOp::Join => r.end = i + 1,
                _ => out.push(i..i + 1),
            }
        }
        out
    }
    pub fn transform(&self, a: &Affine) -> Path {
        let mut p = self.clone();
        for s in &mut p.subpaths {
            for k in &mut s.knots {
                *k = k.transform(a);
            }
        }
        p
    }
    /// Bounds of all anchors and control points `(x0, y0, x1, y1)` (a superset of the curve).
    pub fn control_bounds(&self) -> Option<(f64, f64, f64, f64)> {
        let mut it = self.subpaths.iter().flat_map(|s| s.knots.iter()).flat_map(|k| [k.anchor, k.in_ctrl, k.out_ctrl]);
        let first = it.next()?;
        Some(it.fold((first.x, first.y, first.x, first.y), |b, p| (b.0.min(p.x), b.1.min(p.y), b.2.max(p.x), b.3.max(p.y))))
    }
}

/// A document-level path (Paths panel entry).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NamedPath {
    pub name: String,
    pub path: Path,
    /// Original PSD path resource data; reused on export while it still decodes to `path`.
    pub psd_raw: Option<Arc<Vec<u8>>>,
}

/// The document's clipping path (PSD resource 2999): which saved path clips on export.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClippingPath {
    pub name: String,
    /// Output flatness in device pixels (0 = printer default).
    pub flatness: f32,
}

/// A layer's vector mask (Layer › Vector Mask).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VectorMask {
    pub path: Path,
    pub enabled: bool,
    /// Moves with the layer.
    pub linked: bool,
    /// Density `0..=1` (Properties panel), like [`crate::LayerMask::density`].
    pub density: f32,
    /// Feather radius in px (Properties panel); the compositor blurs the rasterised path by it
    /// (`photocraft_compose::masks::feather_sigma`), as it does a pixel mask's feather.
    pub feather: f32,
}

impl VectorMask {
    pub fn new(path: Path) -> Self {
        VectorMask { path, enabled: true, linked: true, density: 1.0, feather: 0.0 }
    }
}

/// Where a stroke sits relative to the path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StrokeAlign {
    Inside,
    #[default]
    Center,
    Outside,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

/// Shape stroke (Photoshop's shape "Stroke" options, not the layer-style stroke).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShapeStroke {
    /// Width in px.
    pub width: f32,
    pub paint: Fill,
    pub opacity: f32,
    pub align: StrokeAlign,
    pub cap: LineCap,
    pub join: LineJoin,
    /// Miter limit as a multiple of the width (Photoshop default 100 → effectively always
    /// mitered; SVG-style 4 is common elsewhere).
    pub miter_limit: f32,
    /// Dash pattern in multiples of the stroke width (Photoshop's convention); empty = solid.
    pub dashes: Vec<f32>,
    /// Dash offset in multiples of the width.
    pub dash_offset: f32,
}

impl Default for ShapeStroke {
    fn default() -> Self {
        ShapeStroke {
            width: 3.0,
            paint: Fill::Solid(Color::BLACK),
            opacity: 1.0,
            align: StrokeAlign::Center,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter_limit: 100.0,
            dashes: Vec::new(),
            dash_offset: 0.0,
        }
    }
}

impl ShapeStroke {
    /// Limits accepted by interactive commands. Importers retain original descriptor data;
    /// renderers must additionally bound their work for arbitrary file-provided geometry.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.width.is_finite() || !(0.0..=1_000_000.0).contains(&self.width) {
            return Err("stroke width must be finite and between 0 and 1000000 px");
        }
        if !self.opacity.is_finite() || !(0.0..=1.0).contains(&self.opacity) {
            return Err("stroke opacity must be between 0 and 100%");
        }
        if !self.miter_limit.is_finite() || !(1.0..=500.0).contains(&self.miter_limit) {
            return Err("miter limit must be between 1 and 500");
        }
        if !self.dash_offset.is_finite() || self.dash_offset.abs() > 1_000_000.0 {
            return Err("dash offset must be finite and within ±1000000 widths");
        }
        if self.dashes.len() > 32 || self.dashes.iter().any(|d| !d.is_finite() || !(0.0..=10000.0).contains(d)) {
            return Err("dash pattern must contain at most 32 finite lengths between 0 and 10000 widths");
        }
        if !self.dashes.is_empty() && self.dashes.iter().sum::<f32>() < 0.01 {
            return Err("dash pattern total must be at least 0.01 widths");
        }
        Ok(())
    }
}

/// Parametric ("live") shape the path was generated from, kept for re-editing (Properties panel).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LiveShape {
    /// Rectangle `[x, y, w, h]` with corner radii `[tl, tr, br, bl]` (all zero = sharp).
    Rect {
        rect: [f64; 4],
        radii: [f64; 4],
    },
    Ellipse {
        rect: [f64; 4],
    },
    /// Regular polygon or star inscribed in `rect`; `star_ratio` = inner/outer radius (1 = polygon).
    /// `corner_radius` rounds every vertex of a polygon and the outer points of a star (0 = sharp),
    /// each within what its edges allow.
    Polygon {
        rect: [f64; 4],
        sides: u32,
        star_ratio: f64,
        #[serde(default)]
        corner_radius: f64,
    },
    /// Isosceles triangle filling `rect`: the apex at the centre of the top edge, then the bottom
    /// right and the bottom left corner (the Triangle tool, Create Triangle). `corner_radius`
    /// rounds all three corners (0 = sharp), each within what its edges allow. Triangles saved
    /// before this kind existed are 3-sided `Polygon`s and keep that geometry.
    Triangle {
        rect: [f64; 4],
        #[serde(default)]
        corner_radius: f64,
    },
    /// Line from `from` to `to` drawn as a filled bar `weight` px thick.
    Line {
        from: [f64; 2],
        to: [f64; 2],
        weight: f64,
    },
    /// Editable vector arrow with parameters preserved by serde in the project document.
    ArrowLine {
        from: [f64; 2],
        to: [f64; 2],
        weight: f64,
        start: bool,
        end: bool,
        width: f64,
        length: f64,
        concavity: f64,
    },
    /// Parametric manga balloons on one layer (Speech Balloon tool). The geometry is rebuilt from
    /// the group.
    BalloonGroup {
        group: LiveBalloonGroup,
    },
}

/// A shape layer: a filled and/or stroked path.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeLayer {
    pub path: Path,
    /// `None` = no fill (Photoshop's fill "none").
    pub fill: Option<Fill>,
    pub stroke: Option<ShapeStroke>,
    pub live: Option<LiveShape>,
    /// Rasterized appearance (from PSD or `photocraft-vector`).
    pub cache: Option<Surface>,
    /// Data of the PSD vector mask block (`vsms`, else `vmsk`). On export it replaces that entry
    /// in [`crate::Layer::psd_blocks`] while it still decodes to `path`.
    pub psd_raw: Option<Arc<Vec<u8>>>,
    /// Density `0..=1` of the path as the fill's mask (Properties › Masks, like
    /// [`VectorMask::density`]): below 1 the fill shows beyond the path at `1 - density`.
    pub density: f32,
    /// Feather radius in px of the path as the fill's mask (like [`VectorMask::feather`]).
    pub feather: f32,
}

impl Default for ShapeLayer {
    fn default() -> Self {
        ShapeLayer { path: Path::default(), fill: None, stroke: None, live: None, cache: None, psd_raw: None, density: 1.0, feather: 0.0 }
    }
}

impl ShapeLayer {
    /// Whether the fill's edge is softened by a density below 100 % or a feather (non-finite
    /// values count as the defaults).
    pub fn has_soft_mask(&self) -> bool {
        (self.density.is_finite() && self.density < 1.0) || (self.feather.is_finite() && self.feather > 0.0)
    }
}

// --- Parametric manga balloons (pure data; balloon geometry is built by `photocraft-vector`) ---

/// Tail presets of a manga balloon (manga speech-balloon vocabulary).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TailPreset {
    /// Straight tapered wedge.
    #[default]
    Wedge,
    /// Bowed tail with a sideways bend (a cubic Bézier control point).
    Arc,
    /// S-shaped tail (a double bend).
    SCurve,
    /// Zigzag lightning tail (a shout, a phone, a loudspeaker).
    Lightning,
    /// Forked double tail (a chorus remark).
    Double,
    /// A chain of shrinking, detached bubbles (a character's thought).
    Thought,
    /// No tail at all: the balloon is the body only (the tail controls hide).
    None,
}

impl TailPreset {
    /// Stable id used by the UI and the command payloads.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Wedge => "wedge",
            Self::Arc => "arc",
            Self::SCurve => "sCurve",
            Self::Lightning => "lightning",
            Self::Double => "double",
            Self::Thought => "thought",
            Self::None => "none",
        }
    }

    /// Parses an id as [`TailPreset::id`] writes it (`sCurve` also accepts `scurve`).
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "wedge" => Some(Self::Wedge),
            "arc" => Some(Self::Arc),
            "sCurve" | "scurve" => Some(Self::SCurve),
            "lightning" => Some(Self::Lightning),
            "double" => Some(Self::Double),
            "thought" => Some(Self::Thought),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// A manual offset of one spike or cloud-lobe vertex, in document px.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VertexOverride {
    pub index: u32,
    pub offset: [f64; 2],
}

/// Voice preset of a manga line (controls type scale, slant, and contour roughness).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BalloonVoice {
    /// Calm everyday speech.
    #[default]
    Neutral,
    /// Quiet / whispered remark (smaller type, finer contour).
    Whisper,
    /// Loud shout / exclamation (larger type, italic slant, rougher contour).
    Shout,
    /// Irritated / tense remark (jagged tremor and deep bends).
    Irritated,
    /// Broadcast / speakerphone / telepathy (even mechanical cadence).
    Broadcast,
    /// Eerie / ominous remark (deep wavering contour warp).
    Eerie,
}

impl BalloonVoice {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Neutral => "neutral",
            Self::Whisper => "whisper",
            Self::Shout => "shout",
            Self::Irritated => "irritated",
            Self::Broadcast => "broadcast",
            Self::Eerie => "eerie",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "neutral" | "plain" => Some(Self::Neutral),
            "whisper" | "weak" => Some(Self::Whisper),
            "shout" => Some(Self::Shout),
            "irritated" => Some(Self::Irritated),
            "broadcast" | "distant" => Some(Self::Broadcast),
            "eerie" => Some(Self::Eerie),
            _ => None,
        }
    }
}

/// Reading and layout direction of text inside a balloon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextDirection {
    /// Standard horizontal left-to-right lines.
    #[default]
    Horizontal,
    /// Manga vertical top-to-bottom, right-to-left columns (tategaki).
    Vertical,
}

/// Horizontal/column alignment of text inside the balloon's safe area.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BalloonTextAlign {
    #[default]
    Center,
    Left,
    Justify,
}

/// How lettering is fitted into a vector balloon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BalloonTextFitMode {
    /// Fit inside the safe inner rectangle (the old balloon behaviour; default for old files).
    #[default]
    Safe,
    /// Fit inside the tight body bounds, excluding the tail.
    Outer,
    /// Wrap the text along the balloon's pure body contour, inset by [`LiveBalloon::text_inset_px`].
    Contour,
}

impl BalloonTextFitMode {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Outer => "outer",
            Self::Contour => "contour",
        }
    }
}

/// Optional underlay figure rendered behind the balloon body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BalloonUnderlay {
    pub enabled: bool,
    /// Offset `[dx, dy]` relative to the balloon's rect in document px.
    #[serde(default = "default_underlay_offset")]
    pub offset: [f64; 2],
    /// Scale multiplier relative to the balloon's rect (default `1.04`).
    #[serde(default = "default_underlay_scale")]
    pub scale: f64,
    /// 0.0 = ellipse, 1.0 = rounded rectangle.
    #[serde(default)]
    pub rectness: f64,
}

fn default_underlay_offset() -> [f64; 2] {
    [6.0, 6.0]
}

fn default_underlay_scale() -> f64 {
    1.04
}

impl Default for BalloonUnderlay {
    fn default() -> Self {
        Self { enabled: false, offset: default_underlay_offset(), scale: default_underlay_scale(), rectness: 0.35 }
    }
}

/// A manual position and radius override for an individual stipple dot in `BalloonLineStyle::StippleDots`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StippleDotOverride {
    pub index: u32,
    pub offset: [f64; 2],
    #[serde(default = "default_one_f64")]
    pub radius_scale: f64,
}

/// Display mode for a `Uniflash` (sea-urchin flash) balloon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UniflashMode {
    /// Classic Uniflash: separate needles tapering inward over a white mask.
    #[default]
    InwardNeedles,
    /// Solid black collar ring around the white text window with sharp spikes radiating outward.
    SolidRingSpikes,
}

impl UniflashMode {
    pub const fn id(self) -> &'static str {
        match self {
            Self::InwardNeedles => "inwardNeedles",
            Self::SolidRingSpikes => "solidRingSpikes",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "inwardNeedles" => Some(Self::InwardNeedles),
            "solidRingSpikes" => Some(Self::SolidRingSpikes),
            _ => None,
        }
    }
}

/// Dedicated stroke / line rendering style for a balloon contour (8 styles).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BalloonLineStyle {
    /// Clean single vector ink stroke.
    #[default]
    Solid,
    /// Concentric double manga outline (works on any body shape + tail).
    Double,
    /// Whisper dashed / broken contour along the actual body + tail perimeter.
    Whisper,
    /// Multi-strand sketchy graphite pencil stroke.
    Pencil,
    /// 1-bit pixel-art stepped contour (staircase rasterised vector outline).
    Pixel1Bit,
    /// Ukiyo-e / sumi-e manga brush stroke with variable pressure swells and ink flares.
    UkiyoBrush,
    /// Stipple dots contour (Aura ring or Gloom filled mass) with individually editable dots.
    StippleDots,
    /// Shoujo sparkle / glittery contour with 4-point stars, diamonds, and dust along the rim.
    SparkleGlitter,
}

impl BalloonLineStyle {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Solid => "solid",
            Self::Double => "double",
            Self::Whisper => "whisper",
            Self::Pencil => "pencil",
            Self::Pixel1Bit => "pixel1Bit",
            Self::UkiyoBrush => "ukiyoBrush",
            Self::StippleDots => "stippleDots",
            Self::SparkleGlitter => "sparkleGlitter",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "solid" => Some(Self::Solid),
            "double" => Some(Self::Double),
            "whisper" | "dashed" => Some(Self::Whisper),
            "pencil" => Some(Self::Pencil),
            "pixel1Bit" => Some(Self::Pixel1Bit),
            "ukiyoBrush" => Some(Self::UkiyoBrush),
            "stippleDots" => Some(Self::StippleDots),
            "sparkleGlitter" => Some(Self::SparkleGlitter),
            _ => None,
        }
    }
}

/// Shape of a Bokeh particle generated by `BalloonLineStyle::SparkleGlitter`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BokehShape {
    #[default]
    Circle,
    Hexagon,
    Ring,
}

impl BokehShape {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Circle => "circle",
            Self::Hexagon => "hexagon",
            Self::Ring => "ring",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "circle" => Some(Self::Circle),
            "hexagon" => Some(Self::Hexagon),
            "ring" => Some(Self::Ring),
            _ => None,
        }
    }
}

/// Shared numeric ranges for the manga balloon particle controls.
///
/// Kept next to [`LiveBalloon`] so the vector generators, the engine command validation and the
/// manga UI sliders/clamps all agree on one set of limits. The UI slider bounds are the source of
/// truth: a value the slider cannot produce must also be rejected by `validate_balloon`.
pub mod balloon_ranges {
    use std::ops::RangeInclusive;

    // The MIN/MAX pairs below are the single source of truth: the ranges and every clamp use them.
    pub const HAZE_SPREAD_MIN: f64 = 0.0;
    pub const HAZE_SPREAD_MAX: f64 = 100.0;
    pub const STIPPLE_DENSITY_MIN: f64 = 0.0;
    pub const STIPPLE_DENSITY_MAX: f64 = 100.0;
    pub const STIPPLE_COUNT_MIN: u32 = 24;
    pub const STIPPLE_COUNT_MAX: u32 = 2000;
    pub const STIPPLE_RANDOMNESS_MIN: f64 = 0.0;
    pub const STIPPLE_RANDOMNESS_MAX: f64 = 100.0;
    pub const GRAIN_SIZE_MIN: f64 = 5.0;
    pub const GRAIN_SIZE_MAX: f64 = 100.0;
    pub const EDGE_ROUGHNESS_MIN: f64 = 0.0;
    pub const EDGE_ROUGHNESS_MAX: f64 = 100.0;

    pub const SPARKLE_COUNT_MIN: u32 = 0;
    pub const SPARKLE_COUNT_MAX: u32 = 400;
    pub const SPARKLE_SIZE_MIN: f64 = 0.2;
    pub const SPARKLE_SIZE_MAX: f64 = 2.5;
    pub const SPARKLE_SIZE_JITTER_MIN: f64 = 0.0;
    pub const SPARKLE_SIZE_JITTER_MAX: f64 = 100.0;
    pub const SPARKLE_RANDOMNESS_MIN: f64 = 0.0;
    pub const SPARKLE_RANDOMNESS_MAX: f64 = 100.0;

    pub const BOKEH_COUNT_MIN: u32 = 0;
    pub const BOKEH_COUNT_MAX: u32 = 400;
    pub const BOKEH_SIZE_MIN: f64 = 2.0;
    pub const BOKEH_SIZE_MAX: f64 = 36.0;
    pub const BOKEH_SIZE_RANDOMNESS_MIN: f64 = 0.0;
    pub const BOKEH_SIZE_RANDOMNESS_MAX: f64 = 100.0;
    /// Inner padding of `Contour` mode lettering, in document px (slider range).
    pub const TEXT_INSET_PX_MIN: f64 = 0.0;
    pub const TEXT_INSET_PX_MAX: f64 = 48.0;

    pub const HAZE_SPREAD: RangeInclusive<f64> = HAZE_SPREAD_MIN..=HAZE_SPREAD_MAX;
    pub const STIPPLE_DENSITY: RangeInclusive<f64> = STIPPLE_DENSITY_MIN..=STIPPLE_DENSITY_MAX;
    pub const STIPPLE_COUNT: RangeInclusive<u32> = STIPPLE_COUNT_MIN..=STIPPLE_COUNT_MAX;
    pub const STIPPLE_RANDOMNESS: RangeInclusive<f64> = STIPPLE_RANDOMNESS_MIN..=STIPPLE_RANDOMNESS_MAX;
    pub const GRAIN_SIZE: RangeInclusive<f64> = GRAIN_SIZE_MIN..=GRAIN_SIZE_MAX;
    pub const EDGE_ROUGHNESS: RangeInclusive<f64> = EDGE_ROUGHNESS_MIN..=EDGE_ROUGHNESS_MAX;

    pub const SPARKLE_COUNT: RangeInclusive<u32> = SPARKLE_COUNT_MIN..=SPARKLE_COUNT_MAX;
    pub const SPARKLE_SIZE: RangeInclusive<f64> = SPARKLE_SIZE_MIN..=SPARKLE_SIZE_MAX;
    pub const SPARKLE_SIZE_JITTER: RangeInclusive<f64> = SPARKLE_SIZE_JITTER_MIN..=SPARKLE_SIZE_JITTER_MAX;
    pub const SPARKLE_RANDOMNESS: RangeInclusive<f64> = SPARKLE_RANDOMNESS_MIN..=SPARKLE_RANDOMNESS_MAX;

    pub const BOKEH_COUNT: RangeInclusive<u32> = BOKEH_COUNT_MIN..=BOKEH_COUNT_MAX;
    pub const BOKEH_SIZE: RangeInclusive<f64> = BOKEH_SIZE_MIN..=BOKEH_SIZE_MAX;
    pub const BOKEH_SIZE_RANDOMNESS: RangeInclusive<f64> = BOKEH_SIZE_RANDOMNESS_MIN..=BOKEH_SIZE_RANDOMNESS_MAX;
    pub const TEXT_INSET_PX: RangeInclusive<f64> = TEXT_INSET_PX_MIN..=TEXT_INSET_PX_MAX;

    /// Clamp `value` into one of the ranges above. Ranges are not `Copy`, so this avoids repeating
    /// `start()`/`end()` at every call site while keeping the bounds in a single place.
    pub fn clamp_u32(value: u32, range: &RangeInclusive<u32>) -> u32 {
        value.clamp(*range.start(), *range.end())
    }

    /// Floating-point counterpart of [`clamp_u32`].
    pub fn clamp_f64(value: f64, range: &RangeInclusive<f64>) -> f64 {
        value.clamp(*range.start(), *range.end())
    }
}

/// The parametric body of a balloon.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "bodyKind", rename_all = "camelCase")]
pub enum LiveBalloonBody {
    /// An oval or superelliptical (squircle) balloon.
    Ellipse {
        /// `[x, y, w, h]` in document px.
        rect: [f64; 4],
        /// 0 = plain ellipse, 1 = full squircle.
        #[serde(default)]
        squircle: f64,
    },
    /// A rectangular balloon (a caption box, narration).
    Rect {
        rect: [f64; 4],
        /// Corner radius in px.
        #[serde(default)]
        corner_radius: f64,
    },
    /// Chamfered rectangle (octagon / plaque box).
    ChamferRect {
        rect: [f64; 4],
        /// Corner chamfer size in px: 1.0..=60.0 (default 16.0).
        #[serde(default = "default_chamfer")]
        chamfer: f64,
        /// Corner concavity (`0.0` = flat 45 deg bevel, `1.0` = inward scoop).
        #[serde(default)]
        concave: f64,
    },
    /// A thought cloud with asymmetric lobes and cusped joints.
    Cloud {
        rect: [f64; 4],
        /// Number of lobes: 4..=64 (default 10).
        bumps: u32,
        /// Lobe height: 0.04..=0.60 (default 0.18).
        amplitude: f64,
        /// How deep the joints bite in: 0.0..=0.5 (default 0.22).
        cusp_bite: f64,
        /// Asymmetry of the lobes: 0.0..=1.0 (default 0.35).
        jitter: f64,
        seed: u32,
    },
    /// A shout / explosion balloon (concave arcs and sub-harmonics).
    Spiky {
        rect: [f64; 4],
        /// Number of spikes: 5..=128 (default 18).
        spikes: u32,
        /// Main ray length: 0.05..=1.5 (default 0.38).
        length: f64,
        /// Alternation of long and short spikes: 0.0..=1.0 (default 0.45).
        sub_harmonic: f64,
        /// Concavity of the arcs: 0.0..=1.0 (default 0.65).
        concavity: f64,
        /// Random spread: 0.0..=1.0 (default 0.30).
        jitter: f64,
        smooth: bool,
        seed: u32,
    },
    /// A straight-edged explosion star: rays drawn with straight segments between jittered tips
    /// and deep valleys, so it reads as an ink burst instead of a soft shout balloon.
    Burst {
        rect: [f64; 4],
        /// Number of rays: 5..=64 (default 14).
        rays: u32,
        /// Tip reach as a fraction of the radius: 0.10..=1.20 (default 0.60).
        length: f64,
        /// Valley depth as a fraction of the radius: 0.0..=0.60 (default 0.26).
        depth: f64,
        /// Per-ray irregularity: 0.0..=1.0 (default 0.35).
        jitter: f64,
        seed: u32,
    },
    /// Legacy concentric double-line squircle body retained for backward document compatibility;
    /// new authoring uses `BalloonLineStyle::Double` which works on any body shape + tail.
    DoubleOutline {
        rect: [f64; 4],
        /// 0 = plain ellipse, 1 = full squircle (the inner contour).
        squircle: f64,
        /// Distance between the inner and the outer outline: 0.5..=64 px (default 6.0).
        gap: f64,
    },
    /// A sea-urchin flash (uniflash): radial needles tapering towards the centre over a white mask,
    /// or a solid ink collar ring with spikes radiating outward.
    Uniflash {
        rect: [f64; 4],
        /// Number of needles: 32..=400 (default 160).
        needles: u32,
        /// Inner field for the text: 0.30..=0.90 (default 0.62).
        inner_ratio: f64,
        /// Grouping of the needles into bundles: 0.0..=1.0 (default 0.40).
        bundle_group: f64,
        /// Spread of the needle lengths: 0.0..=1.0 (default 0.50).
        length_jitter: f64,
        /// Spread of the needle angles: 0.0..=1.0 (default 0.25).
        angle_jitter: f64,
        /// Base thickness of a needle outside: px (default 2.2).
        needle_width: f64,
        /// Sharpness of the needle tips outside: 0.0 = a blunt end of the full needle width,
        /// 1.0 = a point (default 1.0).
        #[serde(default = "default_outer_sharpness")]
        outer_sharpness: f64,
        /// Sharpness of the needle ends inside, at the mask edge: 0.0..=1.0 (default 1.0).
        #[serde(default = "default_inner_sharpness")]
        inner_sharpness: f64,
        /// Display mode (`InwardNeedles` or `SolidRingSpikes`).
        #[serde(default)]
        mode: UniflashMode,
        seed: u32,
    },
}

impl LiveBalloonBody {
    /// The body's bounding rectangle `[x, y, w, h]` in document px.
    pub fn rect(&self) -> [f64; 4] {
        match self {
            Self::Ellipse { rect, .. }
            | Self::Rect { rect, .. }
            | Self::ChamferRect { rect, .. }
            | Self::Cloud { rect, .. }
            | Self::Spiky { rect, .. }
            | Self::Burst { rect, .. }
            | Self::DoubleOutline { rect, .. }
            | Self::Uniflash { rect, .. } => *rect,
        }
    }

    /// Moves the body to a new bounding rectangle.
    pub fn set_rect(&mut self, new_rect: [f64; 4]) {
        match self {
            Self::Ellipse { rect, .. }
            | Self::Rect { rect, .. }
            | Self::ChamferRect { rect, .. }
            | Self::Cloud { rect, .. }
            | Self::Spiky { rect, .. }
            | Self::Burst { rect, .. }
            | Self::DoubleOutline { rect, .. }
            | Self::Uniflash { rect, .. } => *rect = new_rect,
        }
    }
}

/// A parametric tail of a balloon.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveBalloonTail {
    pub id: u32,
    pub preset: TailPreset,
    /// Where the mouth sits on the body's perimeter: `t` in `[0.0, 1.0)` (0.0 = 12 o'clock,
    /// clockwise).
    pub perimeter_t: f64,
    /// Width of the mouth on the body's contour: px (default 28.0).
    pub mouth_width: f64,
    /// The tip of the tail `[x, y]` in document px.
    pub tip: [f64; 2],
    /// Sideways bend, perpendicular to the chord: -1.0..=1.0.
    pub bend: f64,
    /// Number of bubbles for the `Thought` preset: 1..=8 (default 3).
    pub bubbles: u32,
    /// Radius of the leading bubble for the `Thought` preset (default 10.0).
    pub bubble_radius: f64,
}

/// One parametric balloon.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveBalloon {
    pub id: u32,
    pub body: LiveBalloonBody,
    #[serde(default)]
    pub tails: Vec<LiveBalloonTail>,
    #[serde(default = "default_one")]
    pub next_tail_id: u32,
    #[serde(default)]
    pub vertex_overrides: Vec<VertexOverride>,
    /// Dedicated line / stroke rendering style (`Solid`, `Double`, `Whisper`, `Pencil`, `Pixel1Bit`,
    /// `UkiyoBrush`, `StippleDots`, `SparkleGlitter`).
    #[serde(default)]
    pub line_style: BalloonLineStyle,
    /// Gap between inner and outer contour in `BalloonLineStyle::Double` (`0.5..=64.0`, default `6.0`).
    #[serde(default = "default_double_gap_px")]
    pub double_gap_px: f64,
    /// Number of perimeter dashes in `BalloonLineStyle::Whisper` (`6..=96`, default `18`).
    #[serde(default = "default_whisper_dashes")]
    pub whisper_dash_count: u32,
    /// Duty cycle of each dash in `BalloonLineStyle::Whisper` (`0.20..=0.85`, default `0.58`).
    #[serde(default = "default_whisper_duty")]
    pub whisper_duty: f64,
    /// Number of graphite strands in `BalloonLineStyle::Pencil` (`2..=7`, default `3`).
    #[serde(default = "default_pencil_strands")]
    pub pencil_strands: u32,
    /// Graphite jitter / roughness in `BalloonLineStyle::Pencil` (`0.0..=1.0`, default `0.45`).
    #[serde(default = "default_pencil_roughness")]
    pub pencil_roughness: f64,
    /// Step size in px for `BalloonLineStyle::Pixel1Bit` (`2.0..=24.0`, default `5.0`).
    #[serde(default = "default_pixel_size_px")]
    pub pixel_size_px: f64,
    /// Pressure contrast for `BalloonLineStyle::UkiyoBrush` (`0.0..=1.0`, default `0.72`).
    #[serde(default = "default_ukiyo_contrast")]
    pub ukiyo_contrast: f64,
    /// Stipple haze spread (`0.0..=100.0`, default `63.0`).
    #[serde(default = "default_haze_spread")]
    pub haze_spread: f64,
    /// Stipple dot density / ring compactness (`0.0..=100.0`, default `96.0`).
    #[serde(default = "default_stipple_density")]
    pub stipple_density: f64,
    /// Explicit base dot count (`24..=2000`, default `180`) for `BalloonLineStyle::StippleDots`.
    #[serde(default = "default_stipple_count")]
    pub stipple_count: u32,
    /// Positional & radius randomness (`0.0..=100.0`, default `55.0`) for `BalloonLineStyle::StippleDots`.
    #[serde(default = "default_stipple_randomness")]
    pub stipple_randomness: f64,
    /// Stipple dot grain size (`5.0..=100.0`, default `45.0`).
    #[serde(default = "default_grain_size")]
    pub grain_size: f64,
    /// Stipple edge roughness (`0.0..=100.0`, default `15.0`).
    #[serde(default = "default_edge_roughness")]
    pub edge_roughness: f64,
    /// Whether `StippleDots` fills the interior solid black (`false` = Aura ring, `true` = Gloom filled mass).
    #[serde(default)]
    pub stipple_fill_interior: bool,
    /// Individual stipple dot position and radius overrides (editable on canvas).
    #[serde(default)]
    pub stipple_overrides: Vec<StippleDotOverride>,
    /// Number of sparkle stars around the contour in `BalloonLineStyle::SparkleGlitter` (`0..=400`, default `28`).
    #[serde(default = "default_sparkle_count")]
    pub sparkle_count: u32,
    /// Size scale of sparkle stars in `BalloonLineStyle::SparkleGlitter` (`0.2..=2.5`, default `1.0`).
    #[serde(default = "default_one_f64")]
    pub sparkle_size: f64,
    /// Star size jitter (`0.0..=100.0`, default `60.0`): `0` = uniform, `100` = tiny glints plus rare hero stars.
    #[serde(default = "default_sparkle_size_jitter")]
    pub sparkle_size_jitter: f64,
    /// Positional & scale randomness (`0.0..=100.0`, default `50.0`) for `BalloonLineStyle::SparkleGlitter`.
    #[serde(default = "default_sparkle_randomness")]
    pub sparkle_randomness: f64,
    /// Number of contour sectors particles spawn from (`1 | 2 | 4`, default `4`). Increasing the
    /// count adds particles symmetrically without shifting already-present ones.
    #[serde(default = "default_particle_spawns")]
    pub particle_spawns: u8,
    /// Number of Bokeh particles in `BalloonLineStyle::SparkleGlitter` (`0..=400`, default `45`).
    #[serde(default = "default_bokeh_count")]
    pub bokeh_count: u32,
    /// Bokeh particle radius in px (`2.0..=36.0`, default `10.0`).
    #[serde(default = "default_bokeh_size")]
    pub bokeh_size: f64,
    /// Bokeh radius randomness (`0.0..=100.0`, default `65.0`).
    #[serde(default = "default_bokeh_size_randomness")]
    pub bokeh_size_randomness: f64,
    /// Outline shape of a Bokeh particle (default `Circle`).
    #[serde(default)]
    pub bokeh_shape: BokehShape,
    /// Level-1 hand-drawn contour noise (fine pen tremor): `0.0..=1.0`.
    #[serde(default)]
    pub hand_drawn_noise: f64,
    /// Level-2 hand-drawn contour warp (rare deep bends / "wandering ink"): `0.0..=1.0`.
    #[serde(default)]
    pub hand_drawn_warp: f64,
    /// Frequency scale of the hand-drawn wobble (`0.25..=4.0`, default `1.0`).
    #[serde(default = "default_noise_scale")]
    pub noise_scale: f64,
    /// Voice preset (`Neutral`, `Whisper`, `Shout`, `Irritated`, `Broadcast`, `Eerie`).
    #[serde(default)]
    pub voice: BalloonVoice,
    /// Voice intensity (`0.0..=1.0`, default `0.5`).
    #[serde(default = "default_voice_intensity")]
    pub voice_intensity: f64,
    /// Text reading direction (`Horizontal` or `Vertical`).
    #[serde(default)]
    pub text_direction: TextDirection,
    /// Text alignment inside the safe rect (`Center`, `Left`, `Justify`).
    #[serde(default)]
    pub text_align: BalloonTextAlign,
    /// Which balloon geometry a linked text layer is fitted to (`Safe`, `Outer`, `Contour`).
    #[serde(default)]
    pub text_fit_mode: BalloonTextFitMode,
    /// Inner padding of `Contour` mode lettering, in document px (`0..=48`, default `6`).
    #[serde(default = "default_text_inset_px")]
    pub text_inset_px: f64,
    /// Whether handwritten manga lettering style is enabled.
    #[serde(default)]
    pub handwritten_font: bool,
    /// Optional underlay shape rendered behind the balloon body.
    #[serde(default)]
    pub underlay: Option<BalloonUnderlay>,
    /// Optional imported simple SVG path `d` string overriding the procedural contour.
    #[serde(default)]
    pub custom_svg_contour: Option<String>,
    /// Optional [`crate::VectorBrushPreset`] id driving variable-width / texture strokes.
    #[serde(default)]
    pub brush_preset_id: Option<String>,
    #[serde(default = "default_stroke_width")]
    pub stroke_width: f64,
    #[serde(default = "default_true")]
    pub auto_merge_overlapping: bool,
    /// Optional ID of the `TextLayer` paired with this chamber (`Stories` link).
    #[serde(default)]
    pub linked_text_layer: Option<crate::LayerId>,
    /// Automatically resize/reposition the linked `TextLayer`'s box when this balloon changes.
    #[serde(default = "default_true")]
    pub auto_follow_text: bool,
}

fn default_one() -> u32 {
    1
}

fn default_one_f64() -> f64 {
    1.0
}

fn default_text_inset_px() -> f64 {
    6.0
}

fn default_chamfer() -> f64 {
    16.0
}

fn default_whisper_dashes() -> u32 {
    18
}

fn default_whisper_duty() -> f64 {
    0.58
}

fn default_outer_sharpness() -> f64 {
    1.0
}

fn default_inner_sharpness() -> f64 {
    1.0
}

fn default_double_gap_px() -> f64 {
    6.0
}

fn default_pencil_strands() -> u32 {
    3
}

fn default_pencil_roughness() -> f64 {
    0.45
}

fn default_pixel_size_px() -> f64 {
    5.0
}

fn default_ukiyo_contrast() -> f64 {
    0.72
}

fn default_haze_spread() -> f64 {
    63.0
}

fn default_stipple_density() -> f64 {
    96.0
}

fn default_stipple_count() -> u32 {
    180
}

fn default_stipple_randomness() -> f64 {
    55.0
}

fn default_grain_size() -> f64 {
    45.0
}

fn default_edge_roughness() -> f64 {
    15.0
}

fn default_sparkle_count() -> u32 {
    28
}

fn default_sparkle_size_jitter() -> f64 {
    60.0
}

fn default_sparkle_randomness() -> f64 {
    50.0
}

fn default_particle_spawns() -> u8 {
    4
}

fn default_bokeh_count() -> u32 {
    45
}

fn default_bokeh_size() -> f64 {
    10.0
}

fn default_bokeh_size_randomness() -> f64 {
    65.0
}

fn default_noise_scale() -> f64 {
    1.0
}

fn default_voice_intensity() -> f64 {
    0.5
}

fn default_true() -> bool {
    true
}

fn default_stroke_width() -> f64 {
    3.0
}

impl Default for LiveBalloon {
    fn default() -> Self {
        Self {
            id: 1,
            body: LiveBalloonBody::Ellipse { rect: [100.0, 100.0, 240.0, 160.0], squircle: 0.25 },
            tails: vec![LiveBalloonTail {
                id: 1,
                preset: TailPreset::Arc,
                perimeter_t: 0.58,
                mouth_width: 28.0,
                tip: [130.0, 320.0],
                bend: 0.22,
                bubbles: 3,
                bubble_radius: 10.0,
            }],
            next_tail_id: 2,
            vertex_overrides: Vec::new(),
            line_style: BalloonLineStyle::Solid,
            double_gap_px: 6.0,
            whisper_dash_count: 18,
            whisper_duty: 0.58,
            pencil_strands: 3,
            pencil_roughness: 0.45,
            pixel_size_px: 5.0,
            ukiyo_contrast: 0.72,
            haze_spread: 63.0,
            stipple_density: 96.0,
            stipple_count: 180,
            stipple_randomness: 55.0,
            grain_size: 45.0,
            edge_roughness: 15.0,
            stipple_fill_interior: false,
            stipple_overrides: Vec::new(),
            sparkle_count: 28,
            sparkle_size: 1.0,
            sparkle_size_jitter: 60.0,
            sparkle_randomness: 50.0,
            particle_spawns: 4,
            bokeh_count: 45,
            bokeh_size: 10.0,
            bokeh_size_randomness: 65.0,
            bokeh_shape: BokehShape::Circle,
            hand_drawn_noise: 0.0,
            hand_drawn_warp: 0.0,
            noise_scale: 1.0,
            voice: BalloonVoice::Neutral,
            voice_intensity: 0.5,
            text_direction: TextDirection::Horizontal,
            text_align: BalloonTextAlign::Center,
            text_fit_mode: BalloonTextFitMode::Safe,
            text_inset_px: 6.0,
            handwritten_font: false,
            underlay: None,
            custom_svg_contour: None,
            brush_preset_id: None,
            stroke_width: 3.0,
            auto_merge_overlapping: true,
            linked_text_layer: None,
            auto_follow_text: true,
        }
    }
}

/// The balloons of one layer.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveBalloonGroup {
    pub items: Vec<LiveBalloon>,
    #[serde(default = "default_one")]
    pub next_balloon_id: u32,
}

impl LiveBalloonGroup {
    /// A group holding `items`, with the next ids past the highest one they use.
    pub fn new(items: Vec<LiveBalloon>) -> Self {
        let next_balloon_id = items.iter().map(|b| b.id.saturating_add(1)).max().unwrap_or(1);
        Self { items, next_balloon_id }
    }

    /// Whether the group holds no balloons.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The bounding rectangle `[x, y, w, h]` of every balloon body, tails excluded; `None` when
    /// empty or when a body's rect is not finite.
    pub fn bounds(&self) -> Option<[f64; 4]> {
        let mut it = self.items.iter().map(|b| b.body.rect());
        let first = it.next()?;
        if !first.iter().all(|v| v.is_finite()) {
            return None;
        }
        let mut b = [first[0], first[1], first[0] + first[2], first[1] + first[3]];
        for r in it {
            if !r.iter().all(|v| v.is_finite()) {
                return None;
            }
            b = [b[0].min(r[0]), b[1].min(r[1]), b[2].max(r[0] + r[2]), b[3].max(r[1] + r[3])];
        }
        Some([b[0], b[1], b[2] - b[0], b[3] - b[1]])
    }

    /// Adds `balloon` with a fresh id, returning that id.
    pub fn push(&mut self, mut balloon: LiveBalloon) -> u32 {
        let id = self.next_balloon_id.max(1);
        self.next_balloon_id = id.saturating_add(1);
        balloon.id = id;
        balloon.next_tail_id = balloon.tails.iter().map(|t| t.id.saturating_add(1)).max().unwrap_or(1);
        self.items.push(balloon);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_open_and_closed() {
        let s = Subpath::polygon(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)]);
        assert_eq!(s.segments().len(), 3);
        let o = Subpath::polyline(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)]);
        assert_eq!(o.segments().len(), 2);
        assert_eq!(o.segments()[1][3], Point::new(10.0, 10.0));
    }

    #[test]
    fn first_op_is_combine_and_bounds() {
        let p = Path::new(vec![Subpath::polygon(&[(1.0, 2.0), (5.0, 2.0), (5.0, 9.0)]).with_op(PathOp::Intersect)]);
        assert_eq!(p.effective_op(0), PathOp::Combine);
        assert_eq!(p.control_bounds(), Some((1.0, 2.0, 5.0, 9.0)));
        let t = p.transform(&Affine::translate(1.0, 1.0));
        assert_eq!(t.control_bounds(), Some((2.0, 3.0, 6.0, 10.0)));
    }

    #[test]
    fn a_polygon_saved_before_corner_radii_loads_sharp() {
        let old = r#"{"kind":"polygon","rect":[0.0,0.0,10.0,10.0],"sides":3,"star_ratio":1.0}"#;
        let back: LiveShape = serde_json::from_str(old).unwrap();
        assert_eq!(back, LiveShape::Polygon { rect: [0.0, 0.0, 10.0, 10.0], sides: 3, star_ratio: 1.0, corner_radius: 0.0 });
        let round = LiveShape::Polygon { rect: [0.0; 4], sides: 6, star_ratio: 1.0, corner_radius: 4.5 };
        assert_eq!(serde_json::from_str::<LiveShape>(&serde_json::to_string(&round).unwrap()).unwrap(), round);
    }

    #[test]
    fn serde_roundtrip() {
        let p = Path::new(vec![Subpath::polygon(&[(0.0, 0.0), (3.0, 4.0)]).with_op(PathOp::Exclude)]);
        let s = ShapeStroke { dashes: vec![2.0, 1.0], ..Default::default() };
        let j = serde_json::to_string(&(&p, &s, &LiveShape::Rect { rect: [0.0; 4], radii: [1.0; 4] })).unwrap();
        let back: (Path, ShapeStroke, LiveShape) = serde_json::from_str(&j).unwrap();
        assert_eq!(back.0, p);
        assert_eq!(back.1, s);
        let v: Path = serde_json::from_str(r#"{"subpaths":[{"closed":true,"knots":[]}]}"#).unwrap();
        assert_eq!(v.subpaths[0].op, PathOp::Combine);
    }

    #[test]
    fn a_balloon_group_survives_a_serde_roundtrip() {
        let group = LiveBalloonGroup { items: vec![LiveBalloon::default()], next_balloon_id: 2 };
        let live = LiveShape::BalloonGroup { group: group.clone() };
        let json = serde_json::to_string(&live).unwrap();
        assert!(json.contains("balloonGroup"), "{json}");
        let back: LiveShape = serde_json::from_str(&json).unwrap();
        assert_eq!(back, live);
        let LiveShape::BalloonGroup { group: back_group } = back else { panic!("a balloon group") };
        assert_eq!(back_group.items.len(), 1);
        assert_eq!(back_group.items[0].tails.len(), 1);
        assert_eq!(back_group.items[0].tails[0].preset, TailPreset::Arc);
    }

    #[test]
    fn all_7_balloon_bodies_and_8_line_styles_survive_serde_roundtrip() {
        let rect = [40.0, 50.0, 220.0, 150.0];
        let bodies = [
            LiveBalloonBody::Ellipse { rect, squircle: 0.25 },
            LiveBalloonBody::Rect { rect, corner_radius: 16.0 },
            LiveBalloonBody::ChamferRect { rect, chamfer: 18.0, concave: 0.35 },
            LiveBalloonBody::Cloud { rect, bumps: 10, amplitude: 0.18, cusp_bite: 0.22, jitter: 0.35, seed: 42 },
            LiveBalloonBody::Spiky { rect, spikes: 18, length: 0.38, sub_harmonic: 0.45, concavity: 0.65, jitter: 0.30, smooth: false, seed: 101 },
            LiveBalloonBody::Burst { rect, rays: 14, length: 0.60, depth: 0.26, jitter: 0.35, seed: 303 },
            LiveBalloonBody::Uniflash {
                rect,
                needles: 160,
                inner_ratio: 0.62,
                bundle_group: 0.40,
                length_jitter: 0.50,
                angle_jitter: 0.25,
                needle_width: 2.2,
                outer_sharpness: 1.0,
                inner_sharpness: 1.0,
                mode: UniflashMode::SolidRingSpikes,
                seed: 777,
            },
        ];
        assert_eq!(bodies.len(), 7);
        let styles = [
            BalloonLineStyle::Solid,
            BalloonLineStyle::Double,
            BalloonLineStyle::Whisper,
            BalloonLineStyle::Pencil,
            BalloonLineStyle::Pixel1Bit,
            BalloonLineStyle::UkiyoBrush,
            BalloonLineStyle::StippleDots,
            BalloonLineStyle::SparkleGlitter,
        ];
        assert_eq!(styles.len(), 8);
        for (idx, body) in bodies.into_iter().enumerate() {
            let mut b = LiveBalloon {
                body: body.clone(),
                line_style: styles[idx % styles.len()],
                whisper_dash_count: 20,
                whisper_duty: 0.58,
                stipple_count: 200,
                stipple_randomness: 60.0,
                sparkle_randomness: 45.0,
                stipple_overrides: vec![StippleDotOverride { index: 3, offset: [4.0, -5.0], radius_scale: 1.4 }],
                hand_drawn_noise: 0.3,
                hand_drawn_warp: 0.45,
                noise_scale: 1.25,
                voice: BalloonVoice::Shout,
                voice_intensity: 0.8,
                underlay: Some(BalloonUnderlay { enabled: true, offset: [8.0, 6.0], scale: 1.08, rectness: 0.2 }),
                brush_preset_id: Some("fukidashi-fude".to_string()),
                ..LiveBalloon::default()
            };
            assert_eq!(b.body.rect(), rect);
            b.body.set_rect([10.0, 20.0, 300.0, 200.0]);
            assert_eq!(b.body.rect(), [10.0, 20.0, 300.0, 200.0]);
            let s = serde_json::to_string(&b).unwrap();
            let back: LiveBalloon = serde_json::from_str(&s).unwrap();
            assert_eq!(back, b);
        }
        let legacy_double = LiveBalloonBody::DoubleOutline { rect, squircle: 0.4, gap: 8.0 };
        let s = serde_json::to_string(&legacy_double).unwrap();
        let back: LiveBalloonBody = serde_json::from_str(&s).unwrap();
        assert_eq!(back, legacy_double);
    }

    #[test]
    fn a_balloon_saved_before_the_optional_fields_loads_with_defaults() {
        let old = r#"{"bodyKind":"ellipse","rect":[0.0,0.0,100.0,80.0]}"#;
        let body: LiveBalloonBody = serde_json::from_str(old).unwrap();
        assert_eq!(body, LiveBalloonBody::Ellipse { rect: [0.0, 0.0, 100.0, 80.0], squircle: 0.0 });
        let old = r#"{"id":1,"body":{"bodyKind":"ellipse","rect":[0.0,0.0,100.0,80.0]}}"#;
        let balloon: LiveBalloon = serde_json::from_str(old).unwrap();
        assert!(balloon.tails.is_empty());
        assert_eq!(balloon.next_tail_id, 1);
        assert_eq!(balloon.line_style, BalloonLineStyle::Solid);
        assert_eq!(balloon.double_gap_px, 6.0);
        assert_eq!(balloon.whisper_dash_count, 18);
        assert_eq!(balloon.whisper_duty, 0.58);
        assert_eq!(balloon.haze_spread, 63.0);
        assert_eq!(balloon.stipple_density, 96.0);
        assert_eq!(balloon.stipple_count, 180);
        assert_eq!(balloon.stipple_randomness, 55.0);
        assert_eq!(balloon.grain_size, 45.0);
        assert_eq!(balloon.edge_roughness, 15.0);
        assert_eq!(balloon.sparkle_count, 28);
        assert_eq!(balloon.sparkle_size, 1.0);
        assert_eq!(balloon.sparkle_randomness, 50.0);
        assert_eq!(balloon.stroke_width, 3.0);
        assert_eq!(balloon.hand_drawn_warp, 0.0);
        assert_eq!(balloon.noise_scale, 1.0);
        assert!(balloon.brush_preset_id.is_none());
        assert!(balloon.auto_merge_overlapping);
        assert!(balloon.linked_text_layer.is_none());
        assert!(balloon.auto_follow_text);
        assert_eq!(balloon.text_fit_mode, BalloonTextFitMode::Safe);
        assert_eq!(balloon.text_inset_px, 6.0);

        let linked = LiveBalloon { linked_text_layer: Some(crate::LayerId(42)), auto_follow_text: false, ..LiveBalloon::default() };
        let raw = serde_json::to_string(&linked).unwrap();
        let decoded: LiveBalloon = serde_json::from_str(&raw).unwrap();
        assert_eq!(decoded.linked_text_layer, Some(crate::LayerId(42)));
        assert!(!decoded.auto_follow_text);
    }

    #[test]
    fn a_balloon_saved_before_bokeh_and_spawns_loads_with_defaults() {
        let old = r#"{"id":1,"body":{"bodyKind":"ellipse","rect":[0.0,0.0,100.0,80.0]}}"#;
        let balloon: LiveBalloon = serde_json::from_str(old).unwrap();
        assert_eq!(balloon.sparkle_size_jitter, 60.0);
        assert_eq!(balloon.particle_spawns, 4);
        assert_eq!(balloon.bokeh_count, 45);
        assert_eq!(balloon.bokeh_size, 10.0);
        assert_eq!(balloon.bokeh_size_randomness, 65.0);
        assert_eq!(balloon.bokeh_shape, BokehShape::Circle);
    }

    #[test]
    fn text_fit_mode_ids_and_serde_round_trip() {
        for mode in [BalloonTextFitMode::Safe, BalloonTextFitMode::Outer, BalloonTextFitMode::Contour] {
            let b = LiveBalloon { text_fit_mode: mode, text_inset_px: 12.5, ..LiveBalloon::default() };
            let j = serde_json::to_string(&b).unwrap();
            assert!(j.contains("\"textFitMode\""), "{j}");
            assert_eq!(serde_json::from_str::<LiveBalloon>(&j).unwrap(), b);
            assert_eq!(mode.id(), match mode { BalloonTextFitMode::Safe => "safe", BalloonTextFitMode::Outer => "outer", BalloonTextFitMode::Contour => "contour" });
        }
        assert_eq!(serde_json::from_str::<BalloonTextFitMode>("\"contour\"").unwrap(), BalloonTextFitMode::Contour);
        assert_eq!(balloon_ranges::TEXT_INSET_PX, 0.0..=48.0);
    }

    #[test]
    fn tail_preset_ids_round_trip() {
        for preset in [TailPreset::Wedge, TailPreset::Arc, TailPreset::SCurve, TailPreset::Lightning, TailPreset::Double, TailPreset::Thought] {
            assert_eq!(TailPreset::from_id(preset.id()), Some(preset));
        }
        assert_eq!(TailPreset::from_id("nope"), None);
    }

    #[test]
    fn voice_ids_round_trip() {
        for voice in [BalloonVoice::Neutral, BalloonVoice::Whisper, BalloonVoice::Shout, BalloonVoice::Irritated, BalloonVoice::Broadcast, BalloonVoice::Eerie]
        {
            assert_eq!(BalloonVoice::from_id(voice.id()), Some(voice));
        }
        assert_eq!(BalloonVoice::from_id("nope"), None);
    }

    #[test]
    fn a_balloon_group_reports_its_bounds_and_mints_fresh_ids() {
        let mut group = LiveBalloonGroup::default();
        assert!(group.is_empty());
        assert_eq!(group.bounds(), None, "an empty group covers nothing");
        let first = group.push(LiveBalloon { body: LiveBalloonBody::Ellipse { rect: [10.0, 20.0, 100.0, 50.0], squircle: 0.0 }, ..Default::default() });
        let second = group.push(LiveBalloon { body: LiveBalloonBody::Rect { rect: [150.0, 5.0, 30.0, 200.0], corner_radius: 4.0 }, ..Default::default() });
        assert_eq!((first, second), (1, 2));
        assert_eq!(group.next_balloon_id, 3);
        assert!(!group.is_empty());
        assert_eq!(group.bounds(), Some([10.0, 5.0, 170.0, 200.0]));
        let non_finite = LiveBalloonGroup::new(vec![LiveBalloon {
            body: LiveBalloonBody::Ellipse { rect: [f64::NAN, 0.0, 1.0, 1.0], squircle: 0.0 },
            ..Default::default()
        }]);
        assert_eq!(non_finite.bounds(), None, "hostile rects cover nothing");
    }
}
