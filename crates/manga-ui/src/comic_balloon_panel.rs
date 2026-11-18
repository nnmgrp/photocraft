//! The Manga Balloons generator dock panel, ported into the decoupled manga UI suitcase.
//!
//! This is the `manga-ui` twin of `ui-egui`'s `comic_balloon_panel.rs`. It never touches
//! `PhotocraftApp`, `Session`, the engine, or any dock/menu/state module: every host fact arrives
//! through [`BalloonHostContext`] and every side effect is returned as [`OutCommand`]s in
//! [`BalloonPanelOutput`]. That keeps the panel reusable from both the dock host and a canvas
//! balloon drag-create seam.
//!
//! Offers body selectors (Ellipse, Rect, ChamferRect, Cloud, Spiky, Burst, Uniflash), the
//! `Line · Fill · Color` controls, tail presets, a TypeR text link, and 1-click spawn.
//! Protected against collapse: minimum body bounds are enforced everywhere a rect is built.

use egui::{Color32, RichText, Ui, vec2};
use photocraft_doc::vector::{
    BalloonLineStyle, BalloonTextAlign, BalloonUnderlay, BalloonVoice, LiveBalloon, LiveBalloonBody, LiveBalloonTail, TailPreset, TextDirection, UniflashMode,
};
use photocraft_manga_core::balloon_fit;
use photocraft_vector::balloon::LiveBalloonGeom;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::comic_sai_panel::adaptive_slider;
use crate::theme::Tokens;
use crate::{BridgeDocId, OutCommand};

/// The Window-menu id of the panel.
pub const PANEL: &str = "window.panel.balloon";

/// The Color group tab index this panel occupies in the legacy dock.
pub const BALLOON_COLOR_TAB: usize = 5;

/// Minimum allowed dimensions for a balloon body to protect against collapse/deletion.
pub const MIN_BALLOON_SIZE: f64 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BalloonBodyKindChoice {
    #[default]
    Ellipse,
    Rect,
    ChamferRect,
    Cloud,
    Spiky,
    Burst,
    DoubleOutline,
    Uniflash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BalloonFillMode {
    #[default]
    White,
    Transparent,
    Background,
}

/// A user-saved favorite balloon preset (`TZ 4.3`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BalloonFavoritePreset {
    pub name: String,
    pub body_choice: BalloonBodyKindChoice,
    pub line_style: BalloonLineStyle,
    pub tail_preset: TailPreset,
    pub voice: BalloonVoice,
    pub voice_intensity: f64,
    pub stroke_width: f64,
    pub hand_drawn_noise: f64,
    pub hand_drawn_warp: f64,
    pub noise_scale: f64,
    #[serde(default)]
    pub brush_preset_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BalloonPanelUi {
    pub body_choice: BalloonBodyKindChoice,
    // Ellipse / Rect / ChamferRect
    pub squircle: f64,
    pub corner_radius: f64,
    pub chamfer_size: f64,
    pub chamfer_concave: f64,
    // Cloud
    pub cloud_bumps: u32,
    pub cloud_amplitude: f64,
    pub cloud_cusp_bite: f64,
    pub cloud_jitter: f64,
    pub cloud_seed: u32,
    // Spiky
    pub spiky_spikes: u32,
    pub spiky_length: f64,
    pub spiky_sub_harmonic: f64,
    pub spiky_concavity: f64,
    pub spiky_jitter: f64,
    pub spiky_smooth: bool,
    pub spiky_seed: u32,
    // Burst
    pub burst_rays: u32,
    pub burst_length: f64,
    pub burst_depth: f64,
    pub burst_jitter: f64,
    pub burst_seed: u32,
    // Double outline
    pub double_gap: f64,
    // Uniflash
    pub uniflash_mode: UniflashMode,
    pub uniflash_needles: u32,
    pub uniflash_inner_ratio: f64,
    pub uniflash_bundle: f64,
    pub uniflash_needle_width: f64,
    pub uniflash_outer_sharpness: f64,
    pub uniflash_inner_sharpness: f64,
    pub uniflash_seed: u32,
    // Line / Stroke Style & Brush Preset
    pub line_style: BalloonLineStyle,
    pub double_gap_px: f64,
    pub whisper_dashes: u32,
    pub whisper_duty: f64,
    pub pencil_strands: u32,
    pub pencil_roughness: f64,
    pub pixel_size_px: f64,
    pub ukiyo_contrast: f64,
    pub haze_spread: f64,
    pub stipple_density: f64,
    pub stipple_count: u32,
    pub stipple_randomness: f64,
    pub grain_size: f64,
    pub edge_roughness: f64,
    pub stipple_fill_interior: bool,
    pub sparkle_count: u32,
    pub sparkle_size: f64,
    pub sparkle_randomness: f64,
    pub brush_preset_id: Option<String>,
    // Tail
    pub tail_preset: TailPreset,
    pub tail_mouth_width: f64,
    pub tail_bend: f64,
    pub tail_bubbles: u32,
    pub tail_bubble_radius: f64,
    // Voice & Typography (TZ 4.2)
    pub voice: BalloonVoice,
    pub voice_intensity: f64,
    pub text_direction: TextDirection,
    pub text_align: BalloonTextAlign,
    pub handwritten_font: bool,
    pub fit_mode_shrink: bool,
    // Style & Two-Tier Hand-Drawn Noise
    pub stroke_width: f64,
    pub fill_mode: BalloonFillMode,
    /// Hand-drawn contour wobble: 0.0 = a clean contour, 1.0 = the roughest.
    pub hand_drawn_noise: f64,
    pub hand_drawn_warp: f64,
    pub noise_scale: f64,
    // Underlay
    pub underlay_enabled: bool,
    pub underlay_offset_x: f64,
    pub underlay_offset_y: f64,
    pub underlay_scale: f64,
    pub underlay_rectness: f64,
    /// The line of dialogue the fit is measured for. The layer keeps the text; the panel keeps
    /// what it is fitting.
    pub fit_text: String,
    /// The type size the fit starts from, in points.
    pub fit_size_pt: f64,
    pub batch_script: String,
    pub svg_import_buffer: String,
    pub svg_export_buffer: String,
    pub favorites: Vec<BalloonFavoritePreset>,
    /// Opt in to the gizmo's per-vertex handles (off by default: no 20–32 dots on screen).
    #[serde(default)]
    pub show_vertex_handles: bool,
    /// Layer fill opacity in percent, applied through the layer-style blending options.
    #[serde(default = "default_fill_opacity")]
    pub fill_opacity: f64,
    #[serde(default = "default_ink_color")]
    pub stroke_color: [f32; 4],
    #[serde(default = "default_paper_color")]
    pub fill_color: [f32; 4],
    /// Automatically keep linked TypeR text layer boxes / Stories synced when the balloon resizes.
    #[serde(default = "default_true")]
    pub auto_follow_text: bool,
}

impl Default for BalloonPanelUi {
    fn default() -> Self {
        Self {
            body_choice: BalloonBodyKindChoice::Ellipse,
            squircle: 0.25,
            corner_radius: 12.0,
            chamfer_size: 16.0,
            chamfer_concave: 0.40,
            cloud_bumps: 10,
            cloud_amplitude: 0.18,
            cloud_cusp_bite: 0.22,
            cloud_jitter: 0.35,
            cloud_seed: 42,
            spiky_spikes: 18,
            spiky_length: 0.38,
            spiky_sub_harmonic: 0.45,
            spiky_concavity: 0.65,
            spiky_jitter: 0.30,
            spiky_smooth: false,
            spiky_seed: 101,
            burst_rays: 14,
            burst_length: 0.60,
            burst_depth: 0.26,
            burst_jitter: 0.35,
            burst_seed: 303,
            double_gap: 6.0,
            uniflash_mode: UniflashMode::InwardNeedles,
            uniflash_needles: 160,
            uniflash_inner_ratio: 0.62,
            uniflash_bundle: 0.40,
            uniflash_needle_width: 2.2,
            uniflash_outer_sharpness: 1.0,
            uniflash_inner_sharpness: 1.0,
            uniflash_seed: 777,
            line_style: BalloonLineStyle::Solid,
            double_gap_px: 6.0,
            whisper_dashes: 18,
            whisper_duty: 0.58,
            pencil_strands: 3,
            pencil_roughness: 0.45,
            pixel_size_px: 4.0,
            ukiyo_contrast: 0.72,
            haze_spread: 63.0,
            stipple_density: 80.0,
            stipple_count: 180,
            stipple_randomness: 55.0,
            grain_size: 45.0,
            edge_roughness: 15.0,
            stipple_fill_interior: false,
            sparkle_count: 28,
            sparkle_size: 1.0,
            sparkle_randomness: 50.0,
            brush_preset_id: None,
            tail_preset: TailPreset::Arc,
            tail_mouth_width: 28.0,
            tail_bend: 0.18,
            tail_bubbles: 3,
            tail_bubble_radius: 10.0,
            voice: BalloonVoice::Neutral,
            voice_intensity: 0.5,
            text_direction: TextDirection::Horizontal,
            text_align: BalloonTextAlign::Center,
            handwritten_font: false,
            fit_mode_shrink: false,
            stroke_width: 2.5,
            fill_mode: BalloonFillMode::White,
            hand_drawn_noise: 0.0,
            hand_drawn_warp: 0.0,
            noise_scale: 1.0,
            underlay_enabled: false,
            underlay_offset_x: 6.0,
            underlay_offset_y: 6.0,
            underlay_scale: 1.04,
            underlay_rectness: 0.35,
            fit_text: String::new(),
            fit_size_pt: 14.0,
            batch_script: String::new(),
            svg_import_buffer: String::new(),
            svg_export_buffer: String::new(),
            favorites: Vec::new(),
            show_vertex_handles: false,
            fill_opacity: 100.0,
            stroke_color: [0.0, 0.0, 0.0, 1.0],
            fill_color: [1.0, 1.0, 1.0, 1.0],
            auto_follow_text: true,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_fill_opacity() -> f64 {
    100.0
}

fn default_ink_color() -> [f32; 4] {
    [0.0, 0.0, 0.0, 1.0]
}

fn default_paper_color() -> [f32; 4] {
    [1.0, 1.0, 1.0, 1.0]
}

/// Everything the panel reads from its host, without depending on the host's concrete type.
#[derive(Clone, Copy, Debug)]
pub struct BalloonHostContext<'a> {
    pub doc: Option<&'a photocraft_doc::Document>,
    pub doc_id: Option<BridgeDocId>,
    pub active_layer: Option<photocraft_doc::LayerId>,
    pub selected_layers: &'a [photocraft_doc::LayerId],
    pub active_balloon_id: Option<u32>,
    pub doc_dpi: f32,
    pub fg_color: [f32; 4],
    pub bg_color: [f32; 4],
    pub vector_brush_presets: &'a [(String, String)],
    pub tool_is_balloon: bool,
}

/// Everything the panel asks its host to do after a frame.
#[derive(Default)]
pub struct BalloonPanelOutput {
    pub changed: bool,
    pub commands: Vec<OutCommand>,
    pub switch_to_balloon_tool: bool,
    pub open_linework_panel: bool,
    /// The "Edit individual points" toggle, forwarded to the on-canvas gizmo.
    pub show_vertex_handles: Option<bool>,
    /// Open the TypeR dock for the selected balloon.
    pub open_typer_panel: bool,
    /// Select another chamber id inside the active dual/multi-balloon layer.
    pub select_balloon_id: Option<u32>,
    /// `(message, is_error)` status text for the host's status bar.
    pub status_message: Option<(String, bool)>,
    /// The dialogue editor changed this frame (or `+ Fit balloon` was pressed): the paired Text
    /// layer must be created/updated once, with a coalescing key so typing stays one undo step.
    pub fit_text_dirty: bool,
    /// Safe rectangle of an already grown balloon, for the text command emitted by the Fit button.
    pub fit_text_safe_rect: Option<[f64; 4]>,
}

pub fn handles(id: &str) -> bool {
    id == PANEL
}

struct ActiveBalloonTarget {
    layer_id: u64,
    balloon: LiveBalloon,
    group_index: usize,
    group_len: usize,
    fill: Option<photocraft_doc::Fill>,
    fill_opacity: f32,
    stroke_width: Option<f32>,
    stroke_paint: Option<photocraft_doc::Fill>,
}

/// Returns the active balloon group layer id and group when the active layer is a `BalloonGroup`.
fn active_balloon_group<'a>(host: &BalloonHostContext<'a>) -> Option<(photocraft_doc::LayerId, &'a photocraft_doc::vector::LiveBalloonGroup)> {
    let doc = host.doc?;
    let lid = host.active_layer?;
    let layer = doc.layer(lid)?;
    let photocraft_doc::LayerContent::Shape(sh) = &layer.content else {
        return None;
    };
    let Some(photocraft_doc::LiveShape::BalloonGroup { group }) = &sh.live else {
        return None;
    };
    Some((lid, group))
}

/// Returns the currently active balloon target when the active layer is a `BalloonGroup`.
fn active_balloon_target(host: &BalloonHostContext<'_>) -> Option<ActiveBalloonTarget> {
    let doc = host.doc?;
    let (lid, group) = active_balloon_group(host)?;
    let layer = doc.layer(lid)?;
    let photocraft_doc::LayerContent::Shape(sh) = &layer.content else {
        return None;
    };
    let (group_index, balloon) = host
        .active_balloon_id
        .and_then(|bid| group.items.iter().enumerate().find(|(_, it)| it.id == bid))
        .map(|(idx, it)| (idx, it.clone()))
        .or_else(|| group.items.first().cloned().map(|it| (0, it)))?;
    Some(ActiveBalloonTarget {
        layer_id: lid.0,
        balloon,
        group_index,
        group_len: group.items.len(),
        fill: sh.fill.clone(),
        fill_opacity: layer.fill_opacity,
        stroke_width: sh.stroke.as_ref().map(|s| s.width),
        stroke_paint: sh.stroke.as_ref().map(|s| s.paint.clone()),
    })
}

fn selected_balloon_layer_ids(host: &BalloonHostContext<'_>) -> Vec<u64> {
    let Some(doc) = host.doc else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for lid in host.selected_layers {
        if ids.contains(&lid.0) {
            continue;
        }
        let Some(layer) = doc.layer(*lid) else {
            continue;
        };
        if let photocraft_doc::LayerContent::Shape(sh) = &layer.content
            && matches!(sh.live, Some(photocraft_doc::LiveShape::BalloonGroup { .. }))
        {
            ids.push(lid.0);
        }
    }
    ids
}

fn body_from_state(st: &BalloonPanelUi, rect: [f64; 4]) -> LiveBalloonBody {
    match st.body_choice {
        BalloonBodyKindChoice::Ellipse => LiveBalloonBody::Ellipse { rect, squircle: st.squircle },
        BalloonBodyKindChoice::Rect => LiveBalloonBody::Rect { rect, corner_radius: st.corner_radius },
        BalloonBodyKindChoice::ChamferRect => {
            LiveBalloonBody::ChamferRect { rect, chamfer: st.chamfer_size.clamp(1.0, 60.0), concave: st.chamfer_concave.clamp(0.0, 1.0) }
        }
        BalloonBodyKindChoice::Cloud => LiveBalloonBody::Cloud {
            rect,
            bumps: st.cloud_bumps.max(4),
            amplitude: st.cloud_amplitude,
            cusp_bite: st.cloud_cusp_bite,
            jitter: st.cloud_jitter,
            seed: st.cloud_seed,
        },
        BalloonBodyKindChoice::Spiky => LiveBalloonBody::Spiky {
            rect,
            spikes: st.spiky_spikes.max(5),
            length: st.spiky_length,
            sub_harmonic: st.spiky_sub_harmonic,
            concavity: st.spiky_concavity,
            jitter: st.spiky_jitter,
            smooth: st.spiky_smooth,
            seed: st.spiky_seed,
        },
        BalloonBodyKindChoice::Burst => LiveBalloonBody::Burst {
            rect,
            rays: st.burst_rays.clamp(5, 64),
            length: st.burst_length.clamp(0.10, 1.20),
            depth: st.burst_depth.clamp(0.0, 0.60),
            jitter: st.burst_jitter.clamp(0.0, 1.0),
            seed: st.burst_seed,
        },
        BalloonBodyKindChoice::DoubleOutline => LiveBalloonBody::DoubleOutline { rect, squircle: st.squircle, gap: st.double_gap.clamp(0.5, 64.0) },
        BalloonBodyKindChoice::Uniflash => LiveBalloonBody::Uniflash {
            rect,
            mode: st.uniflash_mode,
            needles: st.uniflash_needles.max(32),
            inner_ratio: st.uniflash_inner_ratio,
            bundle_group: st.uniflash_bundle,
            length_jitter: 0.5,
            angle_jitter: 0.25,
            needle_width: st.uniflash_needle_width,
            outer_sharpness: st.uniflash_outer_sharpness.clamp(0.0, 1.0),
            inner_sharpness: st.uniflash_inner_sharpness.clamp(0.0, 1.0),
            seed: st.uniflash_seed,
        },
    }
}

fn hex_color(c: [f32; 4]) -> String {
    let byte = |v: f32| -> u8 { (v.clamp(0.0, 1.0) * 255.0) as u8 };
    format!("#{:02x}{:02x}{:02x}", byte(c[0]), byte(c[1]), byte(c[2]))
}

fn balloon_fill_value(mode: BalloonFillMode, bg_color: [f32; 4]) -> Value {
    match mode {
        BalloonFillMode::White => json!("#ffffff"),
        BalloonFillMode::Transparent => json!(null),
        BalloonFillMode::Background => json!(hex_color(bg_color)),
    }
}

/// Copies the geometry and style of `balloon` into the panel state.
pub fn hydrate_geometry_from_balloon(state: &mut BalloonPanelUi, balloon: &LiveBalloon) {
    match &balloon.body {
        LiveBalloonBody::Ellipse { squircle, .. } => {
            state.body_choice = BalloonBodyKindChoice::Ellipse;
            state.squircle = *squircle;
        }
        LiveBalloonBody::Rect { corner_radius, .. } => {
            state.body_choice = BalloonBodyKindChoice::Rect;
            state.corner_radius = *corner_radius;
        }
        LiveBalloonBody::ChamferRect { chamfer, concave, .. } => {
            state.body_choice = BalloonBodyKindChoice::ChamferRect;
            state.chamfer_size = *chamfer;
            state.chamfer_concave = *concave;
        }
        LiveBalloonBody::Cloud { bumps, amplitude, cusp_bite, jitter, seed, .. } => {
            state.body_choice = BalloonBodyKindChoice::Cloud;
            state.cloud_bumps = *bumps;
            state.cloud_amplitude = *amplitude;
            state.cloud_cusp_bite = *cusp_bite;
            state.cloud_jitter = *jitter;
            state.cloud_seed = *seed;
        }
        LiveBalloonBody::Spiky { spikes, length, sub_harmonic, concavity, jitter, smooth, seed, .. } => {
            state.body_choice = BalloonBodyKindChoice::Spiky;
            state.spiky_spikes = *spikes;
            state.spiky_length = *length;
            state.spiky_sub_harmonic = *sub_harmonic;
            state.spiky_concavity = *concavity;
            state.spiky_jitter = *jitter;
            state.spiky_smooth = *smooth;
            state.spiky_seed = *seed;
        }
        LiveBalloonBody::Burst { rays, length, depth, jitter, seed, .. } => {
            state.body_choice = BalloonBodyKindChoice::Burst;
            state.burst_rays = *rays;
            state.burst_length = *length;
            state.burst_depth = *depth;
            state.burst_jitter = *jitter;
            state.burst_seed = *seed;
        }
        LiveBalloonBody::DoubleOutline { squircle, gap, .. } => {
            state.body_choice = BalloonBodyKindChoice::DoubleOutline;
            state.squircle = *squircle;
            state.double_gap = *gap;
        }
        LiveBalloonBody::Uniflash { mode, needles, inner_ratio, bundle_group, needle_width, seed, outer_sharpness, inner_sharpness, .. } => {
            state.body_choice = BalloonBodyKindChoice::Uniflash;
            state.uniflash_mode = *mode;
            state.uniflash_needles = *needles;
            state.uniflash_inner_ratio = *inner_ratio;
            state.uniflash_bundle = *bundle_group;
            state.uniflash_needle_width = *needle_width;
            state.uniflash_outer_sharpness = *outer_sharpness;
            state.uniflash_inner_sharpness = *inner_sharpness;
            state.uniflash_seed = *seed;
        }
    }
    state.line_style = balloon.line_style;
    state.double_gap_px = balloon.double_gap_px.clamp(0.5, 64.0);
    state.whisper_dashes = balloon.whisper_dash_count.clamp(6, 96);
    state.whisper_duty = balloon.whisper_duty.clamp(0.20, 0.85);
    state.pencil_strands = balloon.pencil_strands.clamp(2, 7);
    state.pencil_roughness = balloon.pencil_roughness.clamp(0.0, 1.0);
    state.pixel_size_px = balloon.pixel_size_px.clamp(1.0, 32.0);
    state.ukiyo_contrast = balloon.ukiyo_contrast.clamp(0.0, 1.0);
    state.haze_spread = balloon.haze_spread.clamp(0.0, 100.0);
    state.stipple_density = balloon.stipple_density.clamp(0.0, 100.0);
    state.stipple_count = balloon.stipple_count.clamp(24, 600);
    state.stipple_randomness = balloon.stipple_randomness.clamp(0.0, 100.0);
    state.grain_size = balloon.grain_size.clamp(5.0, 100.0);
    state.edge_roughness = balloon.edge_roughness.clamp(0.0, 100.0);
    state.stipple_fill_interior = balloon.stipple_fill_interior;
    state.sparkle_count = balloon.sparkle_count.clamp(0, 96);
    state.sparkle_size = balloon.sparkle_size.clamp(0.2, 2.5);
    state.sparkle_randomness = balloon.sparkle_randomness.clamp(0.0, 100.0);
    state.brush_preset_id = balloon.brush_preset_id.clone();
    if let Some(t) = balloon.tails.first() {
        state.tail_preset = t.preset;
        state.tail_mouth_width = t.mouth_width;
        state.tail_bend = t.bend;
        state.tail_bubbles = t.bubbles;
        state.tail_bubble_radius = t.bubble_radius;
    }
    state.stroke_width = balloon.stroke_width.clamp(0.5, 20.0);
    state.hand_drawn_noise = balloon.hand_drawn_noise.clamp(0.0, 1.0);
    state.hand_drawn_warp = balloon.hand_drawn_warp.clamp(0.0, 1.0);
    state.noise_scale = balloon.noise_scale.clamp(0.25, 4.0);
    state.voice = balloon.voice;
    state.voice_intensity = balloon.voice_intensity.clamp(0.0, 1.0);
    state.text_direction = balloon.text_direction;
    state.text_align = balloon.text_align;
    state.handwritten_font = balloon.handwritten_font;
    state.auto_follow_text = balloon.auto_follow_text;
    if let Some(u) = &balloon.underlay {
        state.underlay_enabled = u.enabled;
        state.underlay_offset_x = u.offset[0];
        state.underlay_offset_y = u.offset[1];
        state.underlay_scale = u.scale;
        state.underlay_rectness = u.rectness;
    } else {
        state.underlay_enabled = false;
    }
}

fn hydrate_state_from_balloon(state: &mut BalloonPanelUi, target: &ActiveBalloonTarget) {
    hydrate_geometry_from_balloon(state, &target.balloon);
    state.stroke_width = target.stroke_width.map(f64::from).unwrap_or(target.balloon.stroke_width).clamp(0.5, 20.0);
    state.fill_mode = match &target.fill {
        None => BalloonFillMode::Transparent,
        Some(photocraft_doc::Fill::Solid(c)) if c.c[0] > 0.98 && c.c[1] > 0.98 && c.c[2] > 0.98 => BalloonFillMode::White,
        Some(_) => BalloonFillMode::Background,
    };
    state.fill_opacity = (f64::from(target.fill_opacity.clamp(0.0, 1.0)) * 100.0).round();
    if let Some(photocraft_doc::Fill::Solid(c)) = &target.fill {
        state.fill_color = [c.c[0], c.c[1], c.c[2], c.alpha];
    }
    if let Some(photocraft_doc::Fill::Solid(c)) = &target.stroke_paint {
        state.stroke_color = [c.c[0], c.c[1], c.c[2], c.alpha];
    }
}

/// Whether the panel state changed anything the balloon stores (transient text/script buffers and
/// standalone paint/opacity controls that dispatch `shape.edit` / `blendingOptions` directly are
/// ignored so they do not emit duplicate `shape.balloon.update` steps).
fn geometry_changed(a: &BalloonPanelUi, b: &BalloonPanelUi) -> bool {
    let mut a = a.clone();
    let mut b = b.clone();
    a.fit_text.clear();
    b.fit_text.clear();
    a.fit_size_pt = 0.0;
    b.fit_size_pt = 0.0;
    a.batch_script.clear();
    b.batch_script.clear();
    a.svg_import_buffer.clear();
    b.svg_import_buffer.clear();
    a.svg_export_buffer.clear();
    b.svg_export_buffer.clear();
    a.favorites.clear();
    b.favorites.clear();
    a.fill_opacity = 0.0;
    b.fill_opacity = 0.0;
    a.stroke_color = [0.0; 4];
    b.stroke_color = [0.0; 4];
    a.fill_color = [0.0; 4];
    b.fill_color = [0.0; 4];
    a != b
}

fn balloon_update_command(layer_id: u64, balloon: &LiveBalloon) -> OutCommand {
    OutCommand::new("shape.balloon.update", json!({ "layer": layer_id, "balloon": balloon, "coalesce": "balloon-panel-edit" }))
}

/// Pushes panel edits for the active live balloon layer as deferred commands (returns `true` when
/// a balloon target was found and commands were emitted).
fn apply_panel_to_active_layer(state: &BalloonPanelUi, prev: &BalloonPanelUi, host: &BalloonHostContext<'_>, commands: &mut Vec<OutCommand>) -> bool {
    let Some(ActiveBalloonTarget { layer_id, mut balloon, .. }) = active_balloon_target(host) else {
        return false;
    };
    let tail_changed = state.tail_preset != prev.tail_preset
        || (state.tail_mouth_width - prev.tail_mouth_width).abs() > 1e-4
        || (state.tail_bend - prev.tail_bend).abs() > 1e-4
        || state.tail_bubbles != prev.tail_bubbles
        || (state.tail_bubble_radius - prev.tail_bubble_radius).abs() > 1e-4;
    let fill_changed = state.fill_mode != prev.fill_mode;
    let stroke_changed = (state.stroke_width - prev.stroke_width).abs() > 1e-4;
    let line_style_changed = state.line_style != prev.line_style;
    let is_uniflash = state.body_choice == BalloonBodyKindChoice::Uniflash;
    // Only `Uniflash + Solid` uses a black fill with no stroke: the needle polygons themselves
    // are the ink. Every stylized line style keeps the normal balloon fill (white by default)
    // and a black stroke, so its ink passes stay visible instead of becoming a black blob.
    let uniflash_solid = is_uniflash && state.line_style == BalloonLineStyle::Solid;
    let body_became_uniflash = is_uniflash && prev.body_choice != BalloonBodyKindChoice::Uniflash;
    let body_left_uniflash = !is_uniflash && prev.body_choice == BalloonBodyKindChoice::Uniflash;
    let uniflash_paint_changed = is_uniflash && line_style_changed;

    if state.body_choice != prev.body_choice {
        balloon.vertex_overrides.clear();
    }
    apply_geometry_to_balloon(state, &mut balloon);
    if state.tail_preset != TailPreset::None && balloon.tails.is_empty() {
        let rect = balloon.body.rect();
        balloon.tails.push(LiveBalloonTail {
            id: balloon.next_tail_id,
            preset: state.tail_preset,
            perimeter_t: 0.58,
            mouth_width: state.tail_mouth_width,
            tip: [rect[0] + rect[2] * 0.25, rect[1] + rect[3] + 48.0],
            bend: state.tail_bend,
            bubbles: state.tail_bubbles,
            bubble_radius: state.tail_bubble_radius,
        });
        balloon.next_tail_id = balloon.next_tail_id.saturating_add(1);
    }
    if tail_changed && let Some(t) = balloon.tails.first_mut() {
        t.preset = state.tail_preset;
        t.mouth_width = state.tail_mouth_width;
        t.bend = state.tail_bend;
        t.bubbles = state.tail_bubbles;
        t.bubble_radius = state.tail_bubble_radius;
    }
    commands.push(balloon_update_command(layer_id, &balloon));

    if fill_changed || stroke_changed || body_became_uniflash || body_left_uniflash || uniflash_paint_changed {
        let mut edit = json!({ "layer": layer_id });
        if uniflash_solid && (body_became_uniflash || uniflash_paint_changed) {
            edit["fill"] = json!("#000000");
            edit["stroke"] = json!(null);
        } else {
            if fill_changed || body_became_uniflash || body_left_uniflash || uniflash_paint_changed {
                edit["fill"] = balloon_fill_value(state.fill_mode, host.bg_color);
            }
            if stroke_changed || body_became_uniflash || body_left_uniflash || uniflash_paint_changed {
                edit["stroke"] = json!({ "width": state.stroke_width, "color": hex_color(state.stroke_color) });
            }
        }
        commands.push(OutCommand::new("shape.edit", edit));
    }
    true
}

fn apply_geometry_to_balloon(state: &BalloonPanelUi, balloon: &mut LiveBalloon) {
    balloon.body = body_from_state(state, balloon.body.rect());
    balloon.line_style = state.line_style;
    balloon.double_gap_px = state.double_gap_px.clamp(0.5, 64.0);
    balloon.whisper_dash_count = state.whisper_dashes.clamp(6, 96);
    balloon.whisper_duty = state.whisper_duty.clamp(0.20, 0.85);
    balloon.pencil_strands = state.pencil_strands.clamp(2, 7);
    balloon.pencil_roughness = state.pencil_roughness.clamp(0.0, 1.0);
    balloon.pixel_size_px = state.pixel_size_px.clamp(1.0, 32.0);
    balloon.ukiyo_contrast = state.ukiyo_contrast.clamp(0.0, 1.0);
    balloon.haze_spread = state.haze_spread.clamp(0.0, 100.0);
    balloon.stipple_density = state.stipple_density.clamp(0.0, 100.0);
    balloon.stipple_count = state.stipple_count.clamp(24, 600);
    balloon.stipple_randomness = state.stipple_randomness.clamp(0.0, 100.0);
    balloon.grain_size = state.grain_size.clamp(5.0, 100.0);
    balloon.edge_roughness = state.edge_roughness.clamp(0.0, 100.0);
    balloon.stipple_fill_interior = state.stipple_fill_interior;
    balloon.sparkle_count = state.sparkle_count.clamp(0, 96);
    balloon.sparkle_size = state.sparkle_size.clamp(0.2, 2.5);
    balloon.sparkle_randomness = state.sparkle_randomness.clamp(0.0, 100.0);
    balloon.brush_preset_id = state.brush_preset_id.clone();
    balloon.stroke_width = state.stroke_width;
    balloon.hand_drawn_noise = state.hand_drawn_noise.clamp(0.0, 1.0);
    balloon.hand_drawn_warp = state.hand_drawn_warp.clamp(0.0, 1.0);
    balloon.noise_scale = state.noise_scale.clamp(0.25, 4.0);
    balloon.voice = state.voice;
    balloon.voice_intensity = state.voice_intensity.clamp(0.0, 1.0);
    balloon.text_direction = state.text_direction;
    balloon.text_align = state.text_align;
    balloon.handwritten_font = state.handwritten_font;
    balloon.auto_follow_text = state.auto_follow_text;
    balloon.underlay = if state.underlay_enabled {
        Some(BalloonUnderlay {
            enabled: true,
            offset: [state.underlay_offset_x, state.underlay_offset_y],
            scale: state.underlay_scale.clamp(0.8, 1.5),
            rectness: state.underlay_rectness.clamp(0.0, 1.0),
        })
    } else {
        None
    };
}

/// Stable name of the Text layer paired with a balloon layer, so the panel can find it in the
/// document without storing a layer id that would go stale across undo/redo or document swaps.
fn balloon_dialogue_layer_name(layer_id: u64) -> String {
    format!("Balloon dialogue (L{layer_id})")
}

fn balloon_text_align_id(align: BalloonTextAlign) -> &'static str {
    match align {
        BalloonTextAlign::Center => "center",
        BalloonTextAlign::Left => "left",
        BalloonTextAlign::Justify => "justify",
    }
}

/// The point size actually used for the dialogue: the voice preset's font multiplier and the
/// shrink-to-fit result are both folded in, so the Text layer's glyphs match the fit estimate.
fn effective_dialogue_size_pt(state: &BalloonPanelUi) -> f64 {
    let mods = balloon_fit::voice_modifiers(state.voice, state.voice_intensity);
    (state.fit_size_pt * f64::from(mods.font_scale)).clamp(4.0, 1296.0)
}

/// Creates or updates the Text layer paired with the active balloon. `type.create` lays the text
/// out in the balloon's `safe_text_rect` (centred, oriented, sized); later edits use `type.edit`
/// on the same named layer. All commands share a coalescing key, so continuous typing is a single
/// history step instead of one command per frame.
fn sync_balloon_dialogue_text(state: &BalloonPanelUi, host: &BalloonHostContext<'_>, safe_override: Option<[f64; 4]>, commands: &mut Vec<OutCommand>) {
    let Some(target) = active_balloon_target(host) else {
        return;
    };
    let name = balloon_dialogue_layer_name(target.layer_id);
    let exists = host.doc.is_some_and(|doc| doc.walk().iter().any(|(_, _, l)| l.name == name));
    if !exists && state.fit_text.is_empty() {
        return;
    }
    let safe = safe_override.unwrap_or_else(|| LiveBalloonGeom::safe_text_rect(&target.balloon));
    if safe[2] <= 1.0 || safe[3] <= 1.0 {
        return;
    }
    let size = effective_dialogue_size_pt(state);
    let align = balloon_text_align_id(state.text_align);
    let coalesce = format!("balloon-dialogue-{}", target.layer_id);
    let mut params = json!({
        "box": [safe[0], safe[1], safe[2], safe[3]],
        "text": state.fit_text,
        "size": size,
        "align": align,
        "verticalAlign": "center",
        "coalesce": coalesce,
    });
    if exists {
        params["layer"] = json!(name);
        params["runs"] = json!([{ "start": 0, "end": state.fit_text.chars().count(), "size": size }]);
        if let Some(obj) = params.as_object_mut() {
            obj.remove("size");
        }
        commands.push(OutCommand::new("type.edit", params));
        // `type.edit` has no alignment/vertical-alignment keys; the sibling style command keeps
        // the paragraph centred when the user changes the balloon's alignment mid-edit.
        commands.push(OutCommand::new("type.setStyle", json!({ "layer": name, "align": align, "verticalAlign": "center", "coalesce": coalesce })));
    } else {
        params["name"] = json!(name);
        params["orientation"] = json!(if state.text_direction == TextDirection::Vertical { "vertical" } else { "horizontal" });
        params["color"] = json!("#000000");
        commands.push(OutCommand::new("type.create", params));
        // `type.create` makes the new text layer active. Keep the balloon active so later
        // keystrokes still find their target and the balloon panel stays bound to it.
        commands.push(OutCommand::new("layer.select", json!({ "layer": target.layer_id })));
    }
}

/// Shared vector-brush preset dropdown + `.json`/`.png` import button used by both the Balloons and
/// SAI Linework docks. Returns `Some(id)` when a preset entry is chosen; import commands are pushed
/// to `commands` for the host to dispatch.
pub fn vector_brush_picker_row(
    ui: &mut Ui,
    id_salt: &str,
    presets: &[(String, String)],
    selected_id: &mut Option<String>,
    commands: &mut Vec<OutCommand>,
) -> Option<String> {
    let current_label = selected_id
        .as_deref()
        .and_then(|id| presets.iter().find(|(pid, _)| pid == id).map(|(_, n)| n.as_str()))
        .unwrap_or("Default (Style Engine)")
        .to_string();
    let mut newly_picked: Option<String> = None;

    ui.horizontal_wrapped(|ui| {
        ui.label(tl!("Brush preset:"));
        craft_ui::ComboBox::from_id_salt(id_salt).width(142.0).selected_text(current_label).show_ui(ui, |ui| {
            if ui.selectable_label(selected_id.is_none(), tl!("Default (Style Engine)")).clicked() {
                *selected_id = None;
            }
            for (pid, pname) in presets {
                let is_sel = selected_id.as_deref() == Some(pid.as_str());
                if ui.selectable_label(is_sel, pname).clicked() {
                    *selected_id = Some(pid.clone());
                    newly_picked = Some(pid.clone());
                }
            }
        });
        let import = ui.button(tl!("+ Import (.json/.png)")).on_hover_text(tl!("Import a VectorBrushPreset JSON or grayscale PNG ribbon texture"));
        if import.clicked()
            && let Some(path) = rfd::FileDialog::new().add_filter("Vector Brush", &["json", "png"]).pick_file()
        {
            commands.push(OutCommand::new("shape.vectorBrush.import", json!({ "path": path.to_string_lossy().into_owned() })));
        }
    });
    newly_picked
}

/// Builds the complete `shape.balloon.create` command for a balloon in `rect`.
///
/// `doc` is accepted for seam symmetry (the host passes the active document when it has one);
/// the geometry itself is fully described by `rect` and the panel state.
pub fn create_live_balloon_in_rect_commands(
    st: &BalloonPanelUi,
    doc: Option<&photocraft_doc::Document>,
    rect: [f64; 4],
    fg_color: [f32; 4],
    bg_color: [f32; 4],
) -> Vec<OutCommand> {
    let _ = doc;
    let [bx, by, bw, bh] = rect;
    let body = body_from_state(st, rect);
    let is_uniflash = matches!(body, LiveBalloonBody::Uniflash { .. });
    let tails = if st.tail_preset == TailPreset::None {
        Vec::new()
    } else {
        vec![LiveBalloonTail {
            id: 1,
            preset: st.tail_preset,
            perimeter_t: 0.58,
            mouth_width: st.tail_mouth_width,
            tip: [bx + bw * 0.25, by + bh + 48.0],
            bend: st.tail_bend,
            bubbles: st.tail_bubbles,
            bubble_radius: st.tail_bubble_radius,
        }]
    };
    let underlay = if st.underlay_enabled {
        Some(BalloonUnderlay {
            enabled: true,
            offset: [st.underlay_offset_x, st.underlay_offset_y],
            scale: st.underlay_scale.clamp(0.8, 1.5),
            rectness: st.underlay_rectness.clamp(0.0, 1.0),
        })
    } else {
        None
    };
    let balloon = LiveBalloon {
        id: 1,
        body,
        line_style: st.line_style,
        double_gap_px: st.double_gap_px.clamp(0.5, 64.0),
        whisper_dash_count: st.whisper_dashes.clamp(6, 96),
        whisper_duty: st.whisper_duty.clamp(0.20, 0.85),
        pencil_strands: st.pencil_strands.clamp(2, 7),
        pencil_roughness: st.pencil_roughness.clamp(0.0, 1.0),
        pixel_size_px: st.pixel_size_px.clamp(1.0, 32.0),
        ukiyo_contrast: st.ukiyo_contrast.clamp(0.0, 1.0),
        haze_spread: st.haze_spread.clamp(0.0, 100.0),
        stipple_density: st.stipple_density.clamp(0.0, 100.0),
        stipple_count: st.stipple_count.clamp(24, 600),
        stipple_randomness: st.stipple_randomness.clamp(0.0, 100.0),
        grain_size: st.grain_size.clamp(5.0, 100.0),
        edge_roughness: st.edge_roughness.clamp(0.0, 100.0),
        stipple_fill_interior: st.stipple_fill_interior,
        stipple_overrides: Vec::new(),
        sparkle_count: st.sparkle_count.clamp(0, 96),
        sparkle_size: st.sparkle_size.clamp(0.2, 2.5),
        sparkle_randomness: st.sparkle_randomness.clamp(0.0, 100.0),
        tails,
        next_tail_id: 2,
        vertex_overrides: Vec::new(),
        hand_drawn_noise: st.hand_drawn_noise.clamp(0.0, 1.0),
        hand_drawn_warp: st.hand_drawn_warp.clamp(0.0, 1.0),
        noise_scale: st.noise_scale.clamp(0.25, 4.0),
        voice: st.voice,
        voice_intensity: st.voice_intensity.clamp(0.0, 1.0),
        text_direction: st.text_direction,
        text_align: st.text_align,
        handwritten_font: st.handwritten_font,
        underlay,
        custom_svg_contour: None,
        brush_preset_id: st.brush_preset_id.clone(),
        stroke_width: st.stroke_width,
        auto_merge_overlapping: true,
        linked_text_layer: None,
        auto_follow_text: st.auto_follow_text,
    };

    // Uniflash + Solid draws the needles themselves in the fill colour; stylized line styles
    // need the ordinary white (or chosen) balloon interior and a black stroke for their ink.
    let is_uniflash_solid = is_uniflash && st.line_style == BalloonLineStyle::Solid;
    let fill_val = if is_uniflash_solid {
        json!("#000000")
    } else if st.fill_mode == BalloonFillMode::White {
        json!(hex_color(st.fill_color))
    } else {
        balloon_fill_value(st.fill_mode, bg_color)
    };
    let ink_color = if st.stroke_color == [0.0, 0.0, 0.0, 1.0] { fg_color } else { st.stroke_color };
    let stroke_val = if is_uniflash_solid { json!(null) } else { json!({ "width": st.stroke_width, "color": hex_color(ink_color) }) };
    vec![OutCommand::new("shape.balloon.create", json!({ "balloon": balloon, "fill": fill_val, "stroke": stroke_val }))]
}

fn create_balloon_on_canvas_commands(st: &BalloonPanelUi, host: &BalloonHostContext<'_>) -> Vec<OutCommand> {
    let Some(doc) = host.doc else {
        return Vec::new();
    };
    let doc_size = doc.size;
    let bounds = doc.selection.as_ref().map(photocraft_compose::bounds::content_bounds).filter(|r| r.width() >= 16 && r.height() >= 16);

    let (bx, by, bw, bh) = match bounds {
        Some(r) => {
            let w = (r.width() as f64).max(MIN_BALLOON_SIZE);
            let h = (r.height() as f64).max(MIN_BALLOON_SIZE);
            (r.x0 as f64, r.y0 as f64, w, h)
        }
        None => {
            let w = 240.0f64.min(doc_size.width as f64 * 0.8).max(MIN_BALLOON_SIZE);
            let h = 160.0f64.min(doc_size.height as f64 * 0.8).max(MIN_BALLOON_SIZE);
            let x = (doc_size.width as f64 - w) * 0.5;
            let y = (doc_size.height as f64 - h) * 0.5;
            (x, y, w, h)
        }
    };
    create_live_balloon_in_rect_commands(st, Some(doc), [bx, by, bw, bh], host.fg_color, host.bg_color)
}

fn add_tail_commands(st: &BalloonPanelUi, host: &BalloonHostContext<'_>) -> Vec<OutCommand> {
    let Some(layer_id) = host.active_layer else {
        return Vec::new();
    };
    let balloon_id = active_balloon_target(host).map_or(1, |t| t.balloon.id);
    vec![OutCommand::new(
        "shape.balloon.tail.add",
        json!({
            "layer": layer_id.0,
            "balloonId": balloon_id,
            "preset": st.tail_preset.id(),
            "mouthWidth": st.tail_mouth_width,
            "tip": [200.0, 300.0],
        }),
    )]
}

fn draw_dual_chambers_row(ui: &mut Ui, state: &BalloonPanelUi, host: &BalloonHostContext<'_>, output: &mut BalloonPanelOutput) {
    let active = active_balloon_target(host);
    let selected_ids = selected_balloon_layer_ids(host);
    if active.is_none() && selected_ids.len() < 2 {
        return;
    }
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        if let Some(target) = &active {
            if ui.button(tl!("+ Chamber")).on_hover_text(tl!("Add an overlapping/connected chamber to this balloon layer")).clicked() {
                let r = target.balloon.body.rect();
                let nw = (r[2] * 0.90).max(MIN_BALLOON_SIZE);
                let nh = (r[3] * 0.90).max(MIN_BALLOON_SIZE);
                let nx = r[0] - nw * 0.72;
                let ny = r[1] + r[3] * 0.55;
                let next_b = LiveBalloon {
                    id: 0,
                    body: body_from_state(state, [nx, ny, nw, nh]),
                    line_style: state.line_style,
                    double_gap_px: state.double_gap_px.clamp(0.5, 64.0),
                    whisper_dash_count: state.whisper_dashes.clamp(6, 96),
                    whisper_duty: state.whisper_duty.clamp(0.20, 0.85),
                    pencil_strands: state.pencil_strands.clamp(2, 7),
                    pencil_roughness: state.pencil_roughness.clamp(0.0, 1.0),
                    pixel_size_px: state.pixel_size_px.clamp(1.0, 32.0),
                    ukiyo_contrast: state.ukiyo_contrast.clamp(0.0, 1.0),
                    haze_spread: state.haze_spread.clamp(0.0, 100.0),
                    stipple_density: state.stipple_density.clamp(0.0, 100.0),
                    stipple_count: state.stipple_count.clamp(24, 600),
                    stipple_randomness: state.stipple_randomness.clamp(0.0, 100.0),
                    grain_size: state.grain_size.clamp(5.0, 100.0),
                    edge_roughness: state.edge_roughness.clamp(0.0, 100.0),
                    stipple_fill_interior: state.stipple_fill_interior,
                    stipple_overrides: Vec::new(),
                    sparkle_count: state.sparkle_count.clamp(0, 96),
                    sparkle_size: state.sparkle_size.clamp(0.2, 2.5),
                    sparkle_randomness: state.sparkle_randomness.clamp(0.0, 100.0),
                    tails: Vec::new(),
                    next_tail_id: 2,
                    vertex_overrides: Vec::new(),
                    hand_drawn_noise: state.hand_drawn_noise.clamp(0.0, 1.0),
                    hand_drawn_warp: state.hand_drawn_warp.clamp(0.0, 1.0),
                    noise_scale: state.noise_scale.clamp(0.25, 4.0),
                    voice: state.voice,
                    voice_intensity: state.voice_intensity.clamp(0.0, 1.0),
                    text_direction: state.text_direction,
                    text_align: state.text_align,
                    handwritten_font: state.handwritten_font,
                    underlay: target.balloon.underlay.clone(),
                    custom_svg_contour: None,
                    brush_preset_id: state.brush_preset_id.clone(),
                    stroke_width: state.stroke_width,
                    auto_merge_overlapping: true,
                    linked_text_layer: None,
                    auto_follow_text: state.auto_follow_text,
                };
                output.commands.push(OutCommand::new("shape.balloon.add", json!({ "layer": target.layer_id, "balloon": next_b })));
            }
            if target.group_len > 1
                && let Some((_, group)) = active_balloon_group(host)
            {
                let ids: Vec<u32> = group.items.iter().map(|it| it.id).collect();
                if ui.button("◀").on_hover_text(tl!("Previous chamber")).clicked() && !ids.is_empty() {
                    let prev_idx = (target.group_index + ids.len() - 1) % ids.len();
                    output.select_balloon_id = Some(ids[prev_idx]);
                }
                ui.label(format!("{} / {}", target.group_index + 1, ids.len()));
                if ui.button("▶").on_hover_text(tl!("Next chamber")).clicked() && !ids.is_empty() {
                    let next_idx = (target.group_index + 1) % ids.len();
                    output.select_balloon_id = Some(ids[next_idx]);
                }
                if ui.button(tl!("Split")).on_hover_text(tl!("Detach active chamber into a new balloon layer")).clicked() {
                    output.commands.push(OutCommand::new("shape.balloon.split", json!({ "layer": target.layer_id, "balloonId": target.balloon.id })));
                }
            }
        }
        if selected_ids.len() >= 2
            && ui.button(tl!("Merge selected")).on_hover_text(tl!("Merge selected balloon layers into one multi-chamber group")).clicked()
        {
            output.commands.push(OutCommand::new("shape.balloon.merge", json!({ "layers": selected_ids })));
        }
    });
}

fn draw_body_section(ui: &mut Ui, state: &mut BalloonPanelUi, host: &BalloonHostContext<'_>, output: &mut BalloonPanelOutput) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new(tl!("Balloon body:")).strong().color(t.text));
    ui.horizontal_wrapped(|ui| {
        let bodies = [
            (BalloonBodyKindChoice::Ellipse, tl!("⚪ Ellipse")),
            (BalloonBodyKindChoice::Rect, tl!("◽ Rect")),
            (BalloonBodyKindChoice::ChamferRect, tl!("◇ Chamfer rect")),
            (BalloonBodyKindChoice::Cloud, tl!("☁️ Cloud")),
            (BalloonBodyKindChoice::Spiky, tl!("💥 Spiky")),
            (BalloonBodyKindChoice::Burst, tl!("✴️ Burst")),
            (BalloonBodyKindChoice::Uniflash, tl!("⚡ Uniflash")),
        ];
        for (choice, label) in bodies {
            if ui.selectable_label(state.body_choice == choice, label).clicked() {
                state.body_choice = choice;
                if choice == BalloonBodyKindChoice::Uniflash && !matches!(state.line_style, BalloonLineStyle::Solid | BalloonLineStyle::StippleDots) {
                    state.line_style = BalloonLineStyle::Solid;
                }
            }
        }
    });

    ui.add_space(6.0);
    ui.separator();
    match state.body_choice {
        BalloonBodyKindChoice::Ellipse => {
            adaptive_slider(ui, tl!("Squircle:"), &mut state.squircle, 0.0..=1.0, "", false);
        }
        BalloonBodyKindChoice::Rect => {
            adaptive_slider(ui, tl!("Corner radius:"), &mut state.corner_radius, 0.0..=60.0, " px", false);
        }
        BalloonBodyKindChoice::ChamferRect => {
            adaptive_slider(ui, tl!("Chamfer size:"), &mut state.chamfer_size, 1.0..=60.0, " px", false);
            adaptive_slider(ui, tl!("Corner scoop:"), &mut state.chamfer_concave, 0.0..=1.0, "", false);
        }
        BalloonBodyKindChoice::Cloud => draw_cloud_params(ui, state),
        BalloonBodyKindChoice::Spiky => draw_spiky_params(ui, state),
        BalloonBodyKindChoice::Burst => draw_burst_params(ui, state),
        BalloonBodyKindChoice::DoubleOutline => {
            adaptive_slider(ui, tl!("Squircle:"), &mut state.squircle, 0.0..=1.0, "", false);
            adaptive_slider(ui, tl!("Outline gap:"), &mut state.double_gap, 0.5..=64.0, " px", false);
        }
        BalloonBodyKindChoice::Uniflash => draw_uniflash_params(ui, state),
    }
    ui.add_space(4.0);
    if ui.checkbox(&mut state.show_vertex_handles, tl!("Edit individual points")).changed() {
        output.show_vertex_handles = Some(state.show_vertex_handles);
    }
    draw_dual_chambers_row(ui, state, host, output);
}

fn draw_jitter_row(ui: &mut Ui, value: &mut f64, seed: &mut u32, hover: &str) {
    ui.horizontal(|ui| {
        let slider_w = (ui.available_width() - 34.0).max(80.0);
        ui.allocate_ui(vec2(slider_w, 0.0), |ui| {
            adaptive_slider(ui, tl!("Jitter:"), value, 0.0..=1.0, "", false);
        });
        if ui.button("🎲").on_hover_text(hover).clicked() {
            *seed = seed.wrapping_add(13);
        }
    });
}

fn draw_cloud_params(ui: &mut Ui, state: &mut BalloonPanelUi) {
    adaptive_slider(ui, tl!("Bumps:"), &mut state.cloud_bumps, 4..=64, "", false);
    adaptive_slider(ui, tl!("Lobe height:"), &mut state.cloud_amplitude, 0.04..=0.55, "", false);
    adaptive_slider(ui, tl!("Cusp bite:"), &mut state.cloud_cusp_bite, 0.0..=0.50, "", false);
    draw_jitter_row(ui, &mut state.cloud_jitter, &mut state.cloud_seed, tl!("Random jitter variation"));
}

fn draw_spiky_params(ui: &mut Ui, state: &mut BalloonPanelUi) {
    adaptive_slider(ui, tl!("Spikes:"), &mut state.spiky_spikes, 5..=128, "", false);
    adaptive_slider(ui, tl!("Ray length:"), &mut state.spiky_length, 0.05..=1.50, "", false);
    adaptive_slider(ui, tl!("Sub-harmonics:"), &mut state.spiky_sub_harmonic, 0.0..=1.0, "", false);
    adaptive_slider(ui, tl!("Inward concavity:"), &mut state.spiky_concavity, 0.0..=1.0, "", false);
    adaptive_slider(ui, tl!("Jitter:"), &mut state.spiky_jitter, 0.0..=1.0, "", false);
    ui.checkbox(&mut state.spiky_smooth, tl!("Round the tips"));
}

fn draw_burst_params(ui: &mut Ui, state: &mut BalloonPanelUi) {
    adaptive_slider(ui, tl!("Rays:"), &mut state.burst_rays, 5..=64, "", false);
    adaptive_slider(ui, tl!("Ray extension:"), &mut state.burst_length, 0.10..=1.20, "", false);
    adaptive_slider(ui, tl!("Valley depth:"), &mut state.burst_depth, 0.0..=0.60, "", false);
    draw_jitter_row(ui, &mut state.burst_jitter, &mut state.burst_seed, tl!("Random ray variation"));
}

fn draw_uniflash_params(ui: &mut Ui, state: &mut BalloonPanelUi) {
    ui.horizontal_wrapped(|ui| {
        ui.label(tl!("Uniflash mode:"));
        ui.selectable_value(&mut state.uniflash_mode, UniflashMode::InwardNeedles, tl!("Inward needles"));
        ui.selectable_value(&mut state.uniflash_mode, UniflashMode::SolidRingSpikes, tl!("Solid ring + spikes"));
    });
    adaptive_slider(ui, tl!("Needles:"), &mut state.uniflash_needles, 32..=450, "", false);
    let mut pct = (state.uniflash_inner_ratio * 100.0).round();
    if adaptive_slider(ui, tl!("Text area:"), &mut pct, 30.0..=90.0, "%", false).changed() {
        state.uniflash_inner_ratio = pct / 100.0;
    }
    adaptive_slider(ui, tl!("Bundle:"), &mut state.uniflash_bundle, 0.0..=1.0, "", false);
    adaptive_slider(ui, tl!("Needle width:"), &mut state.uniflash_needle_width, 1.0..=8.0, " px", false);
    adaptive_slider(ui, tl!("Outer sharpness:"), &mut state.uniflash_outer_sharpness, 0.0..=1.0, "", false);
    adaptive_slider(ui, tl!("Inner sharpness:"), &mut state.uniflash_inner_sharpness, 0.0..=1.0, "", false);
}

fn draw_line_style_section(ui: &mut Ui, state: &mut BalloonPanelUi, host: &BalloonHostContext<'_>, output: &mut BalloonPanelOutput) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new(tl!("Line:")).strong().color(t.text));
    adaptive_slider(ui, tl!("Width:"), &mut state.stroke_width, 0.5..=20.0, " px", false);
    ui.horizontal_wrapped(|ui| {
        for w in [1.5, 2.0, 3.0, 4.0] {
            let is_cur = (state.stroke_width - w).abs() < 0.1;
            if ui.selectable_label(is_cur, format!("{w}")).clicked() {
                state.stroke_width = w;
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        let line_styles = [
            (BalloonLineStyle::Solid, tl!("— Solid")),
            (BalloonLineStyle::Double, tl!("● Double")),
            (BalloonLineStyle::Whisper, tl!("○ Whisper")),
            (BalloonLineStyle::Pencil, tl!("✏ Pencil stroke")),
            (BalloonLineStyle::Pixel1Bit, tl!("👾 1-bit pixel")),
            (BalloonLineStyle::UkiyoBrush, tl!("🎨 Ukiyo-e brush")),
            (BalloonLineStyle::StippleDots, tl!("⚫ Stipple dots")),
            (BalloonLineStyle::SparkleGlitter, tl!("✨ Sparkle / glitter")),
        ];
        let uniflash = state.body_choice == BalloonBodyKindChoice::Uniflash;
        for (style, label) in line_styles {
            let enabled = !uniflash || matches!(style, BalloonLineStyle::Solid | BalloonLineStyle::StippleDots);
            let resp = ui.add_enabled(enabled, egui::Button::selectable(state.line_style == style, label));
            let clicked = resp.clicked();
            if !enabled {
                resp.on_hover_text(tl!("Uniflash supports only Solid and Stipple dots"));
            }
            if clicked {
                state.line_style = style;
            }
        }
    });
    let _ = vector_brush_picker_row(ui, "balloon_vector_brush_combo", host.vector_brush_presets, &mut state.brush_preset_id, &mut output.commands);
    draw_line_style_params(ui, state);
}

fn draw_line_style_params(ui: &mut Ui, state: &mut BalloonPanelUi) {
    match state.line_style {
        BalloonLineStyle::Solid => {}
        BalloonLineStyle::Double => {
            adaptive_slider(ui, tl!("Outline gap:"), &mut state.double_gap_px, 0.5..=64.0, " px", false);
        }
        BalloonLineStyle::Whisper => {
            adaptive_slider(ui, tl!("Dashes:"), &mut state.whisper_dashes, 6..=96, "", false);
            adaptive_slider(ui, tl!("Dash duty:"), &mut state.whisper_duty, 0.20..=0.85, "", false);
        }
        BalloonLineStyle::Pencil => {
            adaptive_slider(ui, tl!("Pencil strands:"), &mut state.pencil_strands, 2..=7, "", false);
            adaptive_slider(ui, tl!("Pencil roughness:"), &mut state.pencil_roughness, 0.0..=1.0, "", false);
        }
        BalloonLineStyle::Pixel1Bit => {
            // Whole document pixels only: fractional cells are what produced the grey AA fringe.
            ui.add(egui::Slider::new(&mut state.pixel_size_px, 1.0..=16.0).step_by(1.0).text(tl!("Pixel size:")));
        }
        BalloonLineStyle::UkiyoBrush => {
            adaptive_slider(ui, tl!("Brush contrast:"), &mut state.ukiyo_contrast, 0.0..=1.0, "", false);
        }
        BalloonLineStyle::StippleDots => {
            adaptive_slider(ui, tl!("Dot count:"), &mut state.stipple_count, 24..=600, "", false);
            adaptive_slider(ui, tl!("Density:"), &mut state.stipple_density, 0.0..=100.0, "", false);
            adaptive_slider(ui, tl!("Dot randomness:"), &mut state.stipple_randomness, 0.0..=100.0, "%", false);
            adaptive_slider(ui, tl!("Haze spread:"), &mut state.haze_spread, 0.0..=100.0, "", false);
            adaptive_slider(ui, tl!("Grain size:"), &mut state.grain_size, 5.0..=100.0, "", false);
            adaptive_slider(ui, tl!("Edge roughness:"), &mut state.edge_roughness, 0.0..=100.0, "", false);
            ui.checkbox(&mut state.stipple_fill_interior, tl!("Fill interior (Gloom mode)"));
        }
        BalloonLineStyle::SparkleGlitter => {
            adaptive_slider(ui, tl!("Sparkle count:"), &mut state.sparkle_count, 4..=64, "", false);
            adaptive_slider(ui, tl!("Sparkle size:"), &mut state.sparkle_size, 0.3..=2.5, "×", false);
            adaptive_slider(ui, tl!("Sparkle randomness:"), &mut state.sparkle_randomness, 0.0..=100.0, "%", false);
        }
    }
}

fn draw_tail_section(ui: &mut Ui, state: &mut BalloonPanelUi) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new(tl!("Tail type:")).strong().color(t.text));
    ui.horizontal_wrapped(|ui| {
        let tails = [
            (TailPreset::None, tl!("⊘ None")),
            (TailPreset::Wedge, tl!("▽ Wedge")),
            (TailPreset::Arc, tl!("≈ Arc")),
            (TailPreset::SCurve, tl!("~ SCurve")),
            (TailPreset::Lightning, tl!("⚡ Lightning")),
            (TailPreset::Double, tl!("Y Double")),
            (TailPreset::Thought, tl!("○ Thought")),
        ];
        for (preset, label) in tails {
            if ui.selectable_label(state.tail_preset == preset, label).clicked() {
                state.tail_preset = preset;
            }
        }
    });
    if state.tail_preset != TailPreset::None {
        adaptive_slider(ui, tl!("Mouth width:"), &mut state.tail_mouth_width, 6.0..=120.0, " px", false);
        adaptive_slider(ui, tl!("Bend:"), &mut state.tail_bend, -1.0..=1.0, "", false);
    }
    if state.tail_preset == TailPreset::Thought {
        adaptive_slider(ui, tl!("Thought bubbles:"), &mut state.tail_bubbles, 1..=8, "", false);
        adaptive_slider(ui, tl!("Bubble radius:"), &mut state.tail_bubble_radius, 4.0..=30.0, " px", false);
    }
}

fn draw_stroke_fill_section(ui: &mut Ui, state: &mut BalloonPanelUi, host: &BalloonHostContext<'_>, output: &mut BalloonPanelOutput) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new(tl!("Fill · Color:")).strong().color(t.text));
    ui.horizontal_wrapped(|ui| {
        ui.label(tl!("Fill:"));
        if ui.radio_value(&mut state.fill_mode, BalloonFillMode::White, tl!("White")).clicked() {
            state.fill_color = [1.0, 1.0, 1.0, 1.0];
        }
        ui.radio_value(&mut state.fill_mode, BalloonFillMode::Transparent, tl!("Transparent"));
        if ui.radio_value(&mut state.fill_mode, BalloonFillMode::Background, tl!("Background colour")).clicked() {
            state.fill_color = host.bg_color;
        }
    });
    if adaptive_slider(ui, tl!("Fill Opacity:"), &mut state.fill_opacity, 0.0..=100.0, " %", false).changed()
        && let Some(id) = host.active_layer
    {
        output.commands.push(OutCommand::new(
            "layer.layerStyle.blendingOptions",
            json!({"layer": id.0, "fillOpacity": state.fill_opacity.clamp(0.0, 100.0), "coalesce": "balloon-fill-opacity"}),
        ));
    }
    let to_c32 = |c: [f32; 4]| {
        Color32::from_rgba_unmultiplied(
            (c[0].clamp(0.0, 1.0) * 255.0) as u8,
            (c[1].clamp(0.0, 1.0) * 255.0) as u8,
            (c[2].clamp(0.0, 1.0) * 255.0) as u8,
            (c[3].clamp(0.0, 1.0) * 255.0) as u8,
        )
    };
    let from_c32 = |c: Color32| [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, c.a() as f32 / 255.0];
    let hex = |c: Color32| format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b());
    ui.horizontal(|ui| {
        ui.label(tl!("Ink:"));
        let mut c = to_c32(state.stroke_color);
        if ui.color_edit_button_srgba(&mut c).changed() {
            state.stroke_color = from_c32(c);
            if let Some(id) = host.active_layer {
                output.commands.push(OutCommand::new("shape.edit", json!({"layer": id.0, "stroke": {"color": hex(c)}, "coalesce": "balloon-color-edit"})));
            }
        }
        ui.label(tl!("Paper:"));
        let mut c = to_c32(state.fill_color);
        if ui.color_edit_button_srgba(&mut c).changed() {
            state.fill_color = from_c32(c);
            if let Some(id) = host.active_layer {
                output.commands.push(OutCommand::new("shape.edit", json!({"layer": id.0, "fill": hex(c), "coalesce": "balloon-color-edit"})));
            }
        }
    });
    if ui.button(tl!("↔ Invert B/W")).clicked() {
        std::mem::swap(&mut state.stroke_color, &mut state.fill_color);
        if let Some(id) = host.active_layer {
            output.commands.push(OutCommand::new(
                "shape.edit",
                json!({"layer": id.0, "fill": hex(to_c32(state.fill_color)), "stroke": {"color": hex(to_c32(state.stroke_color))}}),
            ));
        }
    }
    adaptive_slider(ui, tl!("Hand-drawn noise:"), &mut state.hand_drawn_noise, 0.0..=1.0, "", false);
    adaptive_slider(ui, tl!("Deep contour warp:"), &mut state.hand_drawn_warp, 0.0..=1.0, "", false);
    adaptive_slider(ui, tl!("Noise scale:"), &mut state.noise_scale, 0.25..=4.0, "×", false);

    ui.checkbox(&mut state.underlay_enabled, tl!("Enable underlay / shadow contour"));
    if state.underlay_enabled {
        adaptive_slider(ui, tl!("Underlay X offset:"), &mut state.underlay_offset_x, -24.0..=24.0, " px", false);
        adaptive_slider(ui, tl!("Underlay Y offset:"), &mut state.underlay_offset_y, -24.0..=24.0, " px", false);
        adaptive_slider(ui, tl!("Underlay scale:"), &mut state.underlay_scale, 0.90..=1.25, "×", false);
        adaptive_slider(ui, tl!("Underlay rectness:"), &mut state.underlay_rectness, 0.0..=1.0, "", false);
    }
}

fn draw_favorites(ui: &mut Ui, state: &mut BalloonPanelUi) {
    if ui.button(tl!("★ Save current style to favorites")).clicked() {
        let next_idx = state.favorites.len() + 1;
        state.favorites.push(BalloonFavoritePreset {
            name: format!("Preset #{next_idx}"),
            body_choice: state.body_choice,
            line_style: state.line_style,
            tail_preset: state.tail_preset,
            voice: state.voice,
            voice_intensity: state.voice_intensity,
            stroke_width: state.stroke_width,
            hand_drawn_noise: state.hand_drawn_noise,
            hand_drawn_warp: state.hand_drawn_warp,
            noise_scale: state.noise_scale,
            brush_preset_id: state.brush_preset_id.clone(),
        });
    }
    let mut apply_fav: Option<BalloonFavoritePreset> = None;
    for fav in &state.favorites {
        if ui.button(&fav.name).clicked() {
            apply_fav = Some(fav.clone());
        }
    }
    if let Some(fav) = apply_fav {
        state.body_choice = fav.body_choice;
        state.line_style = fav.line_style;
        state.tail_preset = fav.tail_preset;
        state.voice = fav.voice;
        state.voice_intensity = fav.voice_intensity;
        state.stroke_width = fav.stroke_width;
        state.hand_drawn_noise = fav.hand_drawn_noise;
        state.hand_drawn_warp = fav.hand_drawn_warp;
        state.noise_scale = fav.noise_scale;
        state.brush_preset_id = fav.brush_preset_id;
    }
}

fn draw_workflow_section(ui: &mut Ui, state: &mut BalloonPanelUi, output: &mut BalloonPanelOutput) {
    ui.collapsing(tl!("Workflow: Presets, Script & SVG"), |ui| {
        draw_favorites(ui, state);
        ui.separator();
        ui.label(tl!("Batch balloons from script (one line per balloon):"));
        ui.add(egui::TextEdit::multiline(&mut state.batch_script).desired_rows(3).desired_width(ui.available_width()).hint_text(tl!("Line 1\nLine 2!!")));
        if ui.button(tl!("+ Create batch from script")).clicked() && !state.batch_script.trim().is_empty() {
            output.commands.push(OutCommand::new(
                "shape.balloon.batchFromScript",
                json!({
                    "script": state.batch_script,
                    "sizePt": state.fit_size_pt,
                    "vertical": state.text_direction == TextDirection::Vertical,
                }),
            ));
        }

        ui.separator();
        ui.add(egui::TextEdit::multiline(&mut state.svg_import_buffer).desired_rows(2).desired_width(ui.available_width()).hint_text("<svg>…</svg>"));
        draw_svg_buttons(ui, state, output);
        if !state.svg_export_buffer.is_empty() {
            ui.add(egui::TextEdit::multiline(&mut state.svg_export_buffer).desired_rows(2).desired_width(ui.available_width()));
        }
    });
}

fn draw_svg_buttons(ui: &mut Ui, state: &mut BalloonPanelUi, output: &mut BalloonPanelOutput) {
    ui.horizontal_wrapped(|ui| {
        if ui.button(tl!("⬇ Export balloon SVG")).clicked() {
            output.commands.push(OutCommand::new("shape.balloon.exportSvg", json!({ "text": state.fit_text, "sizePt": state.fit_size_pt })));
        }
        if ui.button(tl!("⬆ Import SVG contour")).clicked() && !state.svg_import_buffer.trim().is_empty() {
            output.commands.push(OutCommand::new("shape.balloon.importSvg", json!({ "svg": state.svg_import_buffer })));
        }
    });
}

fn draw_action_buttons(ui: &mut Ui, state: &mut BalloonPanelUi, host: &BalloonHostContext<'_>, output: &mut BalloonPanelOutput) {
    let t = Tokens::get(ui.ctx());
    let create_btn = ui.add_sized(
        [ui.available_width(), 30.0],
        egui::Button::new(RichText::new(tl!("+ Create balloon on canvas")).strong().color(Color32::WHITE)).fill(t.accent),
    );
    if create_btn.clicked() {
        output.commands.extend(create_balloon_on_canvas_commands(state, host));
        output.switch_to_balloon_tool = !host.tool_is_balloon;
    }

    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        if ui.button(tl!("+ Add tail")).on_hover_text(tl!("Add a tail to the selected balloon")).clicked() {
            output.commands.extend(add_tail_commands(state, host));
            output.switch_to_balloon_tool = !host.tool_is_balloon;
        }
        if ui.button(tl!("⚡ Bake to SAI Linework")).on_hover_text(tl!("Turn the balloon into an editable vector linework layer")).clicked() {
            output.commands.push(OutCommand::new("shape.balloon.convertToLinework", json!({})));
            output.open_linework_panel = true;
        }
    });

    if ui.button(tl!("🔄 Reset to the manga default")).clicked() {
        *state = BalloonPanelUi::default();
    }
}

/// Hydrates panel fields from the active balloon when the host points at a new target.
fn hydrate_active_target(ui: &Ui, state: &mut BalloonPanelUi, host: &BalloonHostContext<'_>) {
    let Some(target) = active_balloon_target(host) else {
        let key = ui.scope_id().with("balloon_panel_hydrated_target");
        ui.ctx().data_mut(|d| d.remove::<(u64, u64, u32)>(key));
        return;
    };
    let key = ui.scope_id().with("balloon_panel_hydrated_target");
    let doc_key = host.doc_id.map_or(0, |id| id.0);
    let cur_key = (doc_key, target.layer_id, target.balloon.id);
    let prev_key = ui.ctx().data_mut(|d| d.get_temp::<(u64, u64, u32)>(key));
    if prev_key != Some(cur_key) {
        ui.ctx().data_mut(|d| d.insert_temp(key, cur_key));
        hydrate_state_from_balloon(state, &target);
    }
}

fn draw_typer_section(ui: &mut Ui, state: &mut BalloonPanelUi, host: &BalloonHostContext<'_>, output: &mut BalloonPanelOutput) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new(tl!("TypeR & Text link:")).strong().color(t.text));
    if ui.button(tl!("🔤 Open / Send to TypeR")).clicked() {
        output.open_typer_panel = true;
    }
    ui.checkbox(&mut state.auto_follow_text, tl!("Auto-follow balloon box"));
    let linked_text = active_balloon_target(host).and_then(|target| {
        let tid = target.balloon.linked_text_layer?;
        let layer = host.doc.and_then(|doc| doc.layer(tid))?;
        let story = matches!(&layer.content, photocraft_doc::LayerContent::Text(txt) if txt.story_threaded || txt.next_story_layer.is_some());
        Some(if story { format!("{} (Stories)", layer.name) } else { layer.name.clone() })
    });
    let linked = linked_text.unwrap_or_else(|| tl!("No text layer linked yet").to_string());
    ui.label(RichText::new(linked).color(t.text_dim));
}

pub fn panel(ui: &mut Ui, state: &mut BalloonPanelUi, host: &BalloonHostContext<'_>) -> BalloonPanelOutput {
    // Headings here are labels, not selectable prose; TextEdit keeps its own selection.
    ui.style_mut().interaction.selectable_labels = false;
    let mut output = BalloonPanelOutput::default();
    let t = Tokens::get(ui.ctx());
    hydrate_active_target(ui, state, host);
    let initial_state = state.clone();

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(4.0);
        draw_body_section(ui, state, host, &mut output);
        ui.add_space(6.0);
        ui.separator();

        draw_line_style_section(ui, state, host, &mut output);
        ui.add_space(6.0);
        ui.separator();

        draw_tail_section(ui, state);
        ui.add_space(6.0);
        ui.separator();

        draw_stroke_fill_section(ui, state, host, &mut output);
        ui.add_space(6.0);
        ui.separator();

        draw_typer_section(ui, state, host, &mut output);
        ui.add_space(6.0);
        ui.separator();

        draw_workflow_section(ui, state, &mut output);
        ui.add_space(10.0);
        ui.separator();

        draw_action_buttons(ui, state, host, &mut output);
        ui.add_space(2.0);
        ui.label(RichText::new(tl!("Changes apply to the active balloon when one is selected.")).color(t.text_dim));
    });

    // Dialogue edits are deliberately excluded from `geometry_changed` (no balloon update every
    // frame while typing), so they are dispatched here with a shared coalescing key instead.
    let dialogue_changed = output.fit_text_dirty
        || state.fit_text != initial_state.fit_text
        || (state.fit_size_pt - initial_state.fit_size_pt).abs() > 1e-6
        || state.text_direction != initial_state.text_direction
        || state.text_align != initial_state.text_align
        || state.voice != initial_state.voice
        || (state.voice_intensity - initial_state.voice_intensity).abs() > 1e-6;
    if dialogue_changed {
        let safe_rect = output.fit_text_safe_rect;
        sync_balloon_dialogue_text(state, host, safe_rect, &mut output.commands);
    }

    if geometry_changed(state, &initial_state) {
        apply_panel_to_active_layer(state, &initial_state, host, &mut output.commands);
    }
    output.changed = *state != initial_state;
    output
}

#[cfg(test)]
mod tests {
    use photocraft_doc::vector::{BalloonLineStyle, LiveBalloon, UniflashMode};

    use super::{
        BALLOON_COLOR_TAB, BalloonBodyKindChoice, BalloonFillMode, BalloonPanelUi, MIN_BALLOON_SIZE, PANEL, body_from_state,
        create_live_balloon_in_rect_commands, handles, hydrate_geometry_from_balloon,
    };

    #[test]
    fn panel_ids_are_stable() {
        assert!(handles(PANEL));
        assert!(!handles("window.panel.linework"));
        assert_eq!(PANEL, "window.panel.balloon");
        assert_eq!(BALLOON_COLOR_TAB, 5);
        let min_sz = MIN_BALLOON_SIZE;
        assert!(min_sz >= 24.0);
    }

    #[test]
    fn balloon_panel_ui_default_and_protection() {
        let st = BalloonPanelUi::default();
        assert_eq!(st.body_choice, BalloonBodyKindChoice::Ellipse);
        assert_eq!(st.tail_preset, photocraft_doc::vector::TailPreset::Arc);
        assert!(st.uniflash_needles >= 32);
        assert!(st.stroke_width > 0.0);
        assert_eq!(st.fill_mode, BalloonFillMode::White);
        assert!(st.favorites.is_empty());
    }

    #[test]
    fn panel_state_round_trips_through_serde() {
        let st = BalloonPanelUi::default();
        let raw = serde_json::to_string(&st).unwrap();
        let back: BalloonPanelUi = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, st);
    }

    #[test]
    fn uniflash_uses_black_fill_only_with_the_solid_line_style() {
        let mut st = BalloonPanelUi { body_choice: BalloonBodyKindChoice::Uniflash, ..BalloonPanelUi::default() };
        let rect = [40.0, 40.0, 200.0, 140.0];
        let fg = [0.0, 0.0, 0.0, 1.0];
        let bg = [1.0, 1.0, 1.0, 1.0];

        st.line_style = BalloonLineStyle::Whisper;
        let cmds = create_live_balloon_in_rect_commands(&st, None, rect, fg, bg);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].params["fill"].as_str(), Some("#ffffff"), "stylized ink needs a white interior");
        assert!(cmds[0].params["stroke"].is_object(), "stylized ink needs a black stroke");

        st.line_style = BalloonLineStyle::UkiyoBrush;
        let cmds = create_live_balloon_in_rect_commands(&st, None, rect, fg, bg);
        assert_eq!(cmds[0].params["fill"].as_str(), Some("#ffffff"));
        assert!(cmds[0].params["stroke"].is_object());

        st.line_style = BalloonLineStyle::Solid;
        let cmds = create_live_balloon_in_rect_commands(&st, None, rect, fg, bg);
        assert_eq!(cmds[0].params["fill"].as_str(), Some("#000000"), "the solid needles are the ink");
        assert!(cmds[0].params["stroke"].is_null());
    }

    #[test]
    fn the_dialogue_fit_knows_when_the_text_does_not_fit() {
        use photocraft_manga_core::balloon_fit::{body_size_for_text, fit_text};

        let fits = fit_text("Hi!", 200.0, 80.0, 12.0, 72.0);
        assert!(fits.fits, "a short line fits: {fits:?}");
        assert_eq!(fits.line_count(), 1);
        assert!((fits.size_pt - 12.0).abs() < 0.01, "the preferred size is kept: {fits:?}");
        assert_eq!(fits, fit_text("Hi!", 200.0, 80.0, 12.0, 72.0), "the fit is deterministic");

        let long_line = "Эта реплика не влезает даже минимальным кеглем, потому что баббл крошечный";
        let overflow = fit_text(long_line, 30.0, 18.0, 14.0, 72.0);
        assert!(!overflow.fits, "a balloon far too small reports an overflow: {overflow:?}");

        let small = body_size_for_text("Коротко", 18.0, 72.0, 0.8);
        let long = body_size_for_text("Значительно больше текста в этой реплике", 18.0, 72.0, 0.8);
        assert!(long[0] >= small[0] && long[1] >= small[1], "more text needs a bigger body: {small:?} vs {long:?}");
    }

    #[test]
    fn all_8_panel_body_choices_and_8_line_styles_round_trip_through_hydration() {
        let choices = [
            BalloonBodyKindChoice::Ellipse,
            BalloonBodyKindChoice::Rect,
            BalloonBodyKindChoice::ChamferRect,
            BalloonBodyKindChoice::Cloud,
            BalloonBodyKindChoice::Spiky,
            BalloonBodyKindChoice::Burst,
            BalloonBodyKindChoice::DoubleOutline,
            BalloonBodyKindChoice::Uniflash,
        ];
        for choice in choices {
            for style in [
                BalloonLineStyle::Solid,
                BalloonLineStyle::Double,
                BalloonLineStyle::Whisper,
                BalloonLineStyle::Pencil,
                BalloonLineStyle::Pixel1Bit,
                BalloonLineStyle::UkiyoBrush,
                BalloonLineStyle::StippleDots,
                BalloonLineStyle::SparkleGlitter,
            ] {
                let st = BalloonPanelUi {
                    body_choice: choice,
                    line_style: style,
                    uniflash_mode: UniflashMode::SolidRingSpikes,
                    brush_preset_id: Some("fukidashi-fude".into()),
                    ..BalloonPanelUi::default()
                };
                let body = body_from_state(&st, [40.0, 40.0, 200.0, 140.0]);
                let b = LiveBalloon { body, line_style: style, brush_preset_id: st.brush_preset_id.clone(), ..LiveBalloon::default() };
                let mut hydrated = BalloonPanelUi::default();
                hydrate_geometry_from_balloon(&mut hydrated, &b);
                assert_eq!(hydrated.body_choice, choice);
                assert_eq!(hydrated.line_style, style);
                assert_eq!(hydrated.brush_preset_id.as_deref(), Some("fukidashi-fude"));
            }
        }
    }

    #[test]
    fn create_commands_carry_the_balloon_and_guard_the_rect() {
        let st = BalloonPanelUi::default();
        let cmds = create_live_balloon_in_rect_commands(&st, None, [10.0, 20.0, 240.0, 160.0], [0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].id, "shape.balloon.create");
        let balloon = cmds[0].params.get("balloon");
        assert!(balloon.is_some(), "the create command carries a serialized LiveBalloon");
        let rect = balloon.and_then(|b| b.get("body")).and_then(|v| v.get("rect"));
        assert!(rect.is_some());
        let fill = cmds[0].params.get("fill").and_then(|v| v.as_str());
        assert_eq!(fill, Some("#ffffff"));
    }

    #[test]
    fn uniflash_create_command_uses_black_fill_and_no_stroke() {
        let st = BalloonPanelUi { body_choice: BalloonBodyKindChoice::Uniflash, ..BalloonPanelUi::default() };
        let cmds = create_live_balloon_in_rect_commands(&st, None, [0.0, 0.0, 100.0, 100.0], [0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].params.get("fill").and_then(|v| v.as_str()), Some("#000000"));
        assert!(cmds[0].params.get("stroke").is_some_and(|v| v.is_null()));
    }

    #[test]
    fn active_target_hydrates_fill_opacity_stroke_paint_and_auto_follow_by_balloon_id() {
        use photocraft_doc::{Color, ColorMode, Document, Fill, Layer, LayerContent, LayerId, LiveShape, SampleType, ShapeLayer, ShapeStroke, Size};
        use photocraft_vector::balloon::LiveBalloonGroupGeom;

        let mut doc = Document::new("PanelHydrate", Size { width: 400, height: 300 }, ColorMode::Rgb, SampleType::U8);
        let b1 = LiveBalloon { id: 1, linked_text_layer: Some(LayerId(77)), auto_follow_text: true, ..LiveBalloon::default() };
        let b2 = LiveBalloon { id: 2, linked_text_layer: Some(LayerId(88)), auto_follow_text: false, ..LiveBalloon::default() };
        let group = photocraft_doc::vector::LiveBalloonGroup::new(vec![b1, b2]);
        let path = group.rebuild_group_path();
        let stroke = ShapeStroke { width: 3.5, paint: Fill::Solid(Color::rgba(0.2, 0.4, 0.6, 1.0)), ..Default::default() };
        let sh = ShapeLayer {
            path,
            fill: Some(Fill::Solid(Color::rgba(0.9, 0.8, 0.7, 1.0))),
            stroke: Some(stroke),
            live: Some(LiveShape::BalloonGroup { group }),
            ..Default::default()
        };
        let mut layer = Layer::new("Balloon", LayerContent::Shape(sh));
        layer.id = LayerId(10);
        layer.fill_opacity = 0.65;
        doc.layers.push(layer);

        let host = super::BalloonHostContext {
            doc: Some(&doc),
            doc_id: None,
            active_layer: Some(LayerId(10)),
            selected_layers: &[LayerId(10)],
            active_balloon_id: Some(2),
            doc_dpi: 72.0,
            fg_color: [0.0, 0.0, 0.0, 1.0],
            bg_color: [1.0, 1.0, 1.0, 1.0],
            vector_brush_presets: &[],
            tool_is_balloon: true,
        };
        let target = super::active_balloon_target(&host).expect("target");
        assert_eq!(target.balloon.id, 2);
        assert_eq!(target.group_index, 1);
        assert_eq!(target.group_len, 2);

        let mut st = BalloonPanelUi::default();
        super::hydrate_state_from_balloon(&mut st, &target);
        assert!((st.fill_opacity - 65.0).abs() < 1e-6);
        assert!(!st.auto_follow_text);
        assert!((st.stroke_color[0] - 0.2).abs() < 1e-6);
        assert!((st.fill_color[1] - 0.8).abs() < 1e-6);
    }
}
