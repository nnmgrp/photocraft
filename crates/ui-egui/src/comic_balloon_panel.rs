//! The Manga Balloons generator dock panel (`crates/ui-egui/src/comic_balloon_panel.rs`).
//!
//! Mirrors the architectural pattern of `comic_screentone_panel.rs`:
//! Offers a dedicated dock tab, Window menu registration (`window.panel.balloon`),
//! body selectors (Ellipse, Rect, Cloud, Spiky, Uniflash), tail presets (Wedge, Arc,
//! SCurve, Lightning, Double, Thought), dynamic parameter sliders, and 1-click spawn.
//! Protected against deletion: guarantees minimum body bounds and preserves closed geometry.

use egui::{Color32, RichText, Ui, vec2};
use photocraft_doc::vector::{LiveBalloon, LiveBalloonBody, LiveBalloonTail, TailPreset};
use photocraft_vector::balloon::LiveBalloonGeom;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::comic_sai_panel::adaptive_slider;
use crate::state::Tool;
use crate::theme::Tokens;

/// The Window-menu id of the panel.
pub const PANEL: &str = "window.panel.balloon";

/// Minimum allowed dimensions for a balloon body to protect against collapse/deletion.
pub const MIN_BALLOON_SIZE: f64 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BalloonBodyKindChoice {
    #[default]
    Ellipse,
    Rect,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BalloonPanelUi {
    pub body_choice: BalloonBodyKindChoice,
    // Ellipse / Rect
    pub squircle: f64,
    pub corner_radius: f64,
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
    pub uniflash_needles: u32,
    pub uniflash_inner_ratio: f64,
    pub uniflash_bundle: f64,
    pub uniflash_needle_width: f64,
    pub uniflash_outer_sharpness: f64,
    pub uniflash_inner_sharpness: f64,
    pub uniflash_seed: u32,
    // Tail
    pub tail_preset: TailPreset,
    pub tail_mouth_width: f64,
    pub tail_bend: f64,
    pub tail_bubbles: u32,
    pub tail_bubble_radius: f64,
    // Style
    pub stroke_width: f64,
    pub fill_mode: BalloonFillMode,
    /// Hand-drawn contour wobble: 0.0 = a clean contour, 1.0 = the roughest.
    pub hand_drawn_noise: f64,
    /// The line of dialogue the fit is measured for (not stored in the balloon: the layer keeps
    /// the text, the panel keeps what it is fitting).
    pub fit_text: String,
    /// The type size the fit starts from, in points.
    pub fit_size_pt: f64,
}

impl Default for BalloonPanelUi {
    fn default() -> Self {
        Self {
            body_choice: BalloonBodyKindChoice::Ellipse,
            squircle: 0.25,
            corner_radius: 12.0,
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
            uniflash_needles: 160,
            uniflash_inner_ratio: 0.62,
            uniflash_bundle: 0.40,
            uniflash_needle_width: 2.2,
            uniflash_outer_sharpness: 1.0,
            uniflash_inner_sharpness: 1.0,
            uniflash_seed: 777,
            tail_preset: TailPreset::Arc,
            tail_mouth_width: 28.0,
            tail_bend: 0.18,
            tail_bubbles: 3,
            tail_bubble_radius: 10.0,
            stroke_width: 2.5,
            fill_mode: BalloonFillMode::White,
            hand_drawn_noise: 0.0,
            fit_text: String::new(),
            fit_size_pt: 14.0,
        }
    }
}

pub fn handles(id: &str) -> bool {
    id == PANEL
}

pub const BALLOON_COLOR_TAB: usize = 5;

pub fn showing(app: &PhotocraftApp) -> bool {
    if app.ui.docking.is_some() || crate::panel_docking::panel_in_side_dock(app, "balloon") {
        crate::panel_docking::panel_active_anywhere(app, "balloon")
    } else {
        app.ui.panels.color && app.ui.dock_tabs.color == BALLOON_COLOR_TAB && !app.ui.dock.is_collapsed(crate::dock::Group::Color)
    }
}

pub fn checked(app: &PhotocraftApp, id: &str) -> Option<bool> {
    if handles(id) { if let Some(c) = crate::panel_docking::checked(app, id) { Some(c) } else { Some(showing(app)) } } else { None }
}

pub fn menu(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if !handles(id) {
        return None;
    }
    log::info!("[Telemetry:Dock] comic_balloon_panel::menu: id={id}, docking_present={}", app.ui.docking.is_some());
    if app.ui.docking.is_some() || crate::panel_docking::panel_in_side_dock(app, "balloon") {
        let want = params.get("show").and_then(Value::as_bool);
        let show = want.unwrap_or(!showing(app));
        log::info!("[Telemetry:Dock] comic_balloon_panel delegating to panel_docking: show={show}");
        return Some(crate::panel_docking::command(app, "window.panel.balloon", &json!({"show": show})).unwrap_or(Ok(json!({"visible": show}))));
    }
    let want = params.get("show").and_then(Value::as_bool);
    let show = want.unwrap_or(!showing(app));
    app.ui.panels.color = show || (app.ui.panels.color && app.ui.dock_tabs.color != BALLOON_COLOR_TAB);
    if show {
        app.ui.dock_tabs.color = BALLOON_COLOR_TAB;
        crate::dock::reveal(app, crate::dock::Group::Color);
    }
    Some(Ok(json!({ "showing": show })))
}

struct ActiveBalloonTarget {
    doc_idx: usize,
    layer_id: u64,
    balloon: LiveBalloon,
    fill: Option<photocraft_doc::Fill>,
    stroke_width: Option<f32>,
}

/// Returns the currently active balloon target when the active layer is a `BalloonGroup`.
fn active_balloon_target(app: &PhotocraftApp) -> Option<ActiveBalloonTarget> {
    let doc_idx = app.session.active_index()?;
    let active = app.session.active()?;
    let lid = active.active_layer?;
    let layer = active.doc.layer(lid)?;
    let photocraft_doc::LayerContent::Shape(sh) = &layer.content else { return None };
    let Some(photocraft_doc::LiveShape::BalloonGroup { group }) = &sh.live else { return None };
    let balloon = app.balloon_gizmo.active_balloon_id.and_then(|bid| group.items.iter().find(|b| b.id == bid)).or_else(|| group.items.first())?;
    Some(ActiveBalloonTarget { doc_idx, layer_id: lid.0, balloon: balloon.clone(), fill: sh.fill.clone(), stroke_width: sh.stroke.as_ref().map(|s| s.width) })
}

fn body_from_state(st: &BalloonPanelUi, rect: [f64; 4]) -> LiveBalloonBody {
    match st.body_choice {
        BalloonBodyKindChoice::Ellipse => LiveBalloonBody::Ellipse { rect, squircle: st.squircle },
        BalloonBodyKindChoice::Rect => LiveBalloonBody::Rect { rect, corner_radius: st.corner_radius },
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

fn balloon_fill_value(app: &PhotocraftApp, mode: BalloonFillMode) -> Value {
    match mode {
        BalloonFillMode::White => json!("#ffffff"),
        BalloonFillMode::Transparent => json!(null),
        BalloonFillMode::Background => {
            let bg = app.session.tools.background;
            json!(format!("#{:02x}{:02x}{:02x}", (bg[0] * 255.0) as u8, (bg[1] * 255.0) as u8, (bg[2] * 255.0) as u8))
        }
    }
}

pub(crate) fn hydrate_geometry_from_balloon(state: &mut BalloonPanelUi, balloon: &LiveBalloon) {
    match &balloon.body {
        LiveBalloonBody::Ellipse { squircle, .. } => {
            state.body_choice = BalloonBodyKindChoice::Ellipse;
            state.squircle = *squircle;
        }
        LiveBalloonBody::Rect { corner_radius, .. } => {
            state.body_choice = BalloonBodyKindChoice::Rect;
            state.corner_radius = *corner_radius;
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
        LiveBalloonBody::Uniflash { needles, inner_ratio, bundle_group, needle_width, seed, outer_sharpness, inner_sharpness, .. } => {
            state.body_choice = BalloonBodyKindChoice::Uniflash;
            state.uniflash_needles = *needles;
            state.uniflash_inner_ratio = *inner_ratio;
            state.uniflash_bundle = *bundle_group;
            state.uniflash_needle_width = *needle_width;
            state.uniflash_outer_sharpness = *outer_sharpness;
            state.uniflash_inner_sharpness = *inner_sharpness;
            state.uniflash_seed = *seed;
        }
    }
    if let Some(t) = balloon.tails.first() {
        state.tail_preset = t.preset;
        state.tail_mouth_width = t.mouth_width;
        state.tail_bend = t.bend;
        state.tail_bubbles = t.bubbles;
        state.tail_bubble_radius = t.bubble_radius;
    }
    state.stroke_width = balloon.stroke_width.clamp(0.5, 20.0);
    state.hand_drawn_noise = balloon.hand_drawn_noise.clamp(0.0, 1.0);
}

fn hydrate_state_from_balloon(state: &mut BalloonPanelUi, balloon: &LiveBalloon, fill: Option<&photocraft_doc::Fill>, stroke_width: Option<f32>) {
    hydrate_geometry_from_balloon(state, balloon);
    state.stroke_width = stroke_width.map(f64::from).unwrap_or(balloon.stroke_width).clamp(0.5, 20.0);
    state.fill_mode = match fill {
        None => BalloonFillMode::Transparent,
        Some(photocraft_doc::Fill::Solid(c)) if c.c[0] > 0.98 && c.c[1] > 0.98 && c.c[2] > 0.98 => BalloonFillMode::White,
        Some(_) => BalloonFillMode::Background,
    };
}

/// Pushes panel edits from `state` onto the active live balloon layer (returns `true` if applied).
pub(crate) fn apply_panel_to_active_layer(app: &mut PhotocraftApp, state: &BalloonPanelUi, prev: &BalloonPanelUi) -> bool {
    let Some(ActiveBalloonTarget { layer_id, mut balloon, .. }) = active_balloon_target(app) else {
        return false;
    };
    let tail_changed = state.tail_preset != prev.tail_preset
        || (state.tail_mouth_width - prev.tail_mouth_width).abs() > 1e-4
        || (state.tail_bend - prev.tail_bend).abs() > 1e-4
        || state.tail_bubbles != prev.tail_bubbles
        || (state.tail_bubble_radius - prev.tail_bubble_radius).abs() > 1e-4;
    let fill_changed = state.fill_mode != prev.fill_mode;
    let stroke_changed = (state.stroke_width - prev.stroke_width).abs() > 1e-4;
    // The Uniflash render mode (TZ v5.0, Нюанс №3): the closed needle polygons are the ink, so
    // the layer is filled with the stroke colour and has no stroke; leaving Uniflash restores
    // the fill/stroke from the panel state.
    let body_became_uniflash = state.body_choice == BalloonBodyKindChoice::Uniflash && prev.body_choice != BalloonBodyKindChoice::Uniflash;
    let body_left_uniflash = state.body_choice != BalloonBodyKindChoice::Uniflash && prev.body_choice == BalloonBodyKindChoice::Uniflash;

    if state.body_choice != prev.body_choice {
        balloon.vertex_overrides.clear();
    }
    balloon.body = body_from_state(state, balloon.body.rect());
    balloon.stroke_width = state.stroke_width;
    balloon.hand_drawn_noise = state.hand_drawn_noise.clamp(0.0, 1.0);
    // Coming back from `⊘ Нет` on a balloon without tails: spawn the default tail automatically.
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

    let mut applied = false;
    match app.run(
        "shape.balloon.update",
        json!({
            "layer": layer_id,
            "balloon": balloon,
            "coalesce": "balloon-panel-edit",
        }),
    ) {
        Ok(_) => applied = true,
        Err(e) => {
            // Surface the refusal instead of silently leaving the panel and the layer out of sync.
            app.ui.status = e.clone();
            app.ui.status_error = true;
            log::warn!("[Telemetry:Balloon:PanelMutate] shape.balloon.update failed: {e}");
        }
    }

    if fill_changed || stroke_changed || body_became_uniflash || body_left_uniflash {
        let mut edit = json!({ "layer": layer_id });
        if body_became_uniflash {
            edit["fill"] = json!("#000000");
            edit["stroke"] = json!(null);
        } else if body_left_uniflash {
            edit["fill"] = balloon_fill_value(app, state.fill_mode);
            edit["stroke"] = json!({ "width": state.stroke_width });
        } else {
            if fill_changed {
                edit["fill"] = balloon_fill_value(app, state.fill_mode);
            }
            if stroke_changed {
                edit["stroke"] = json!({ "width": state.stroke_width });
            }
        }
        match app.run("shape.edit", edit) {
            Ok(_) => applied = true,
            Err(e) => {
                app.ui.status = e.clone();
                app.ui.status_error = true;
                log::warn!("[Telemetry:Balloon:PanelMutate] shape.edit failed: {e}");
            }
        }
    }
    applied
}

/// The safe text area of the active balloon's body, in document px.
fn active_safe_rect(app: &PhotocraftApp) -> Option<[f64; 4]> {
    let target = active_balloon_target(app)?;
    Some(photocraft_vector::balloon::LiveBalloonGeom::safe_text_rect(&target.balloon))
}

fn active_dpi(app: &PhotocraftApp) -> f32 {
    app.session.active().map_or(72.0, |s| s.doc.resolution_dpi)
}

/// Fits `state.fit_text` into the active balloon and reports `(fits, lines, size_pt)`.
///
/// The fit is the engine's deterministic estimator, so it needs no font and no window, and the
/// warning is the same every frame for the same document.
fn balloon_fit_status(app: &PhotocraftApp, state: &BalloonPanelUi) -> Option<(bool, usize, f64)> {
    let safe = active_safe_rect(app)?;
    let fit = photocraft_engine::balloon_fit::fit_text(&state.fit_text, safe[2] as f32, safe[3] as f32, state.fit_size_pt as f32, active_dpi(app));
    Some((fit.fits, fit.line_count(), f64::from(fit.size_pt)))
}

/// Grows the active balloon's body until its safe area holds `state.fit_text` at `state.fit_size_pt`.
///
/// The needed body comes from [`photocraft_engine::balloon_fit::body_size_for_text`], which
/// measures the real line of dialogue (not a 10 000 × 10 000 sheet, where every line is one line
/// and the balloon gets a flat, letterbox shape). The body grows around its own centre, so the
/// tail keeps its mouth on the same point of the contour.
fn fit_balloon_to_text(app: &mut PhotocraftApp, state: &mut BalloonPanelUi) {
    let Some(target) = active_balloon_target(app) else { return };
    let (doc_idx, layer_id) = (target.doc_idx, target.layer_id);
    let mut balloon = target.balloon;
    let rect = balloon.body.rect();
    // A malformed saved rectangle (negative or zero width) must not mirror the balloon when the
    // new rectangle is written: the centre is taken from the original rect and the size is
    // normalised to a positive one.
    let cx = rect[0] + rect[2] * 0.5;
    let cy = rect[1] + rect[3] * 0.5;
    let base_w = rect[2].abs().max(MIN_BALLOON_SIZE);
    let base_h = rect[3].abs().max(MIN_BALLOON_SIZE);

    // The safe area keeps this share of the body (the same share for both axes, so the shape
    // keeps its proportions).
    let safe = balloon.safe_text_rect();
    let fraction = ((safe[2].abs() / base_w).min(safe[3].abs() / base_h)).clamp(0.15, 0.95);
    let dpi = active_dpi(app);
    let [need_w, need_h] = photocraft_engine::balloon_fit::body_size_for_text(&state.fit_text, state.fit_size_pt as f32, dpi, fraction);
    let mut w = base_w.max(need_w).min(1.0e4);
    let mut h = base_h.max(need_h).min(1.0e4);

    // Keep growing until the real fit (measured on the enlarged safe area) agrees.
    for _ in 0..12 {
        balloon.body.set_rect([cx - w * 0.5, cy - h * 0.5, w, h]);
        let probe = balloon.safe_text_rect();
        let f = photocraft_engine::balloon_fit::fit_text(&state.fit_text, probe[2] as f32, probe[3] as f32, state.fit_size_pt as f32, dpi);
        if f.fits {
            break;
        }
        let grow_x = if probe[2] > 1e-6 { f.widest_px / probe[2] as f32 } else { 1.0 };
        let grow_y = if probe[3] > 1e-6 { f.needs_px / probe[3] as f32 } else { 1.0 };
        if grow_x <= 1.02 && grow_y <= 1.02 {
            break;
        }
        w = (w * f64::from(grow_x.max(1.02))).min(1.0e4);
        h = (h * f64::from(grow_y.max(1.02))).min(1.0e4);
    }

    balloon.body.set_rect([cx - w * 0.5, cy - h * 0.5, w.max(MIN_BALLOON_SIZE), h.max(MIN_BALLOON_SIZE)]);
    // The same coalesce key as the rest of the panel: a fit and the edits around it are one undo step.
    if let Err(e) = app.run("shape.balloon.update", json!({ "layer": layer_id, "balloon": balloon, "coalesce": "balloon-panel-edit" })) {
        app.ui.status = e.clone();
        app.ui.status_error = true;
        log::warn!("[Telemetry:Balloon:FitText] shape.balloon.update failed: {e}");
    }
    log::info!("[Telemetry:Balloon:FitText] doc={doc_idx}, layer={layer_id}, rect={:?}", balloon.body.rect());
}

/// Render the Manga Balloons dock panel.
pub fn panel(app: &mut PhotocraftApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let mut state = std::mem::take(&mut app.ui.balloon_ui);
    if let Some(ActiveBalloonTarget { doc_idx, layer_id, balloon, fill, stroke_width }) = active_balloon_target(app) {
        let target_id = egui::Id::new("balloon_panel_hydrated_target");
        let cur_key = (doc_idx, layer_id, balloon.id);
        let prev_key = ui.ctx().data_mut(|d| d.get_temp::<(usize, u64, u32)>(target_id));
        if prev_key != Some(cur_key) {
            ui.ctx().data_mut(|d| d.insert_temp(target_id, cur_key));
            hydrate_state_from_balloon(&mut state, &balloon, fill.as_ref(), stroke_width);
        }
    } else {
        ui.ctx().data_mut(|d| d.remove::<(usize, u64, u32)>(egui::Id::new("balloon_panel_hydrated_target")));
    }
    let initial_state = state.clone();

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(4.0);

        // 1. Body kind selector
        ui.label(RichText::new(tl!("Balloon body:")).strong().color(t.text));
        ui.horizontal_wrapped(|ui| {
            let bodies = [
                (BalloonBodyKindChoice::Ellipse, tl!("⚪ Ellipse")),
                (BalloonBodyKindChoice::Rect, tl!("◽ Rect")),
                (BalloonBodyKindChoice::Cloud, tl!("☁️ Cloud")),
                (BalloonBodyKindChoice::Spiky, tl!("💥 Spiky")),
                (BalloonBodyKindChoice::Burst, tl!("✴️ Burst")),
                (BalloonBodyKindChoice::DoubleOutline, tl!("◎ Double outline")),
                (BalloonBodyKindChoice::Uniflash, tl!("🦔 Uniflash")),
            ];
            for (choice, label) in bodies {
                let is_sel = state.body_choice == choice;
                if ui.selectable_label(is_sel, label).clicked() {
                    state.body_choice = choice;
                }
            }
        });

        ui.add_space(6.0);
        ui.separator();

        // 2. Parameters of the selected body
        match state.body_choice {
            BalloonBodyKindChoice::Ellipse => {
                adaptive_slider(ui, tl!("Squircle:"), &mut state.squircle, 0.0..=1.0, "", false);
            }
            BalloonBodyKindChoice::Rect => {
                adaptive_slider(ui, tl!("Corner radius:"), &mut state.corner_radius, 0.0..=60.0, " px", false);
            }
            BalloonBodyKindChoice::Cloud => {
                adaptive_slider(ui, tl!("Bumps:"), &mut state.cloud_bumps, 4..=64, "", false);
                adaptive_slider(ui, tl!("Lobe height:"), &mut state.cloud_amplitude, 0.04..=0.60, "", false);
                adaptive_slider(ui, tl!("Cusp bite:"), &mut state.cloud_cusp_bite, 0.0..=0.50, "", false);
                ui.horizontal(|ui| {
                    let slider_w = (ui.available_width() - 34.0).max(80.0);
                    ui.allocate_ui(vec2(slider_w, 0.0), |ui| {
                        adaptive_slider(ui, tl!("Jitter:"), &mut state.cloud_jitter, 0.0..=1.0, "", false);
                    });
                    if ui.button("🎲").on_hover_text(tl!("Random jitter variation")).clicked() {
                        state.cloud_seed = state.cloud_seed.wrapping_add(13);
                    }
                });
            }
            BalloonBodyKindChoice::Spiky => {
                adaptive_slider(ui, tl!("Spikes:"), &mut state.spiky_spikes, 5..=128, "", false);
                adaptive_slider(ui, tl!("Ray length:"), &mut state.spiky_length, 0.05..=1.50, "", false);
                adaptive_slider(ui, tl!("Sub-harmonics:"), &mut state.spiky_sub_harmonic, 0.0..=1.0, "", false);
                adaptive_slider(ui, tl!("Inward concavity:"), &mut state.spiky_concavity, 0.0..=1.0, "", false);
                adaptive_slider(ui, tl!("Jitter:"), &mut state.spiky_jitter, 0.0..=1.0, "", false);
                ui.checkbox(&mut state.spiky_smooth, tl!("Round the tips"));
            }
            BalloonBodyKindChoice::Burst => {
                adaptive_slider(ui, tl!("Rays:"), &mut state.burst_rays, 5..=64, "", false);
                adaptive_slider(ui, tl!("Ray extension:"), &mut state.burst_length, 0.10..=1.20, "", false);
                adaptive_slider(ui, tl!("Valley depth:"), &mut state.burst_depth, 0.0..=0.60, "", false);
                ui.horizontal(|ui| {
                    let slider_w = (ui.available_width() - 34.0).max(80.0);
                    ui.allocate_ui(vec2(slider_w, 0.0), |ui| {
                        adaptive_slider(ui, tl!("Jitter:"), &mut state.burst_jitter, 0.0..=1.0, "", false);
                    });
                    if ui.button("🎲").on_hover_text(tl!("Random ray variation")).clicked() {
                        state.burst_seed = state.burst_seed.wrapping_add(13);
                    }
                });
            }
            BalloonBodyKindChoice::DoubleOutline => {
                adaptive_slider(ui, tl!("Squircle:"), &mut state.squircle, 0.0..=1.0, "", false);
                adaptive_slider(ui, tl!("Outline gap:"), &mut state.double_gap, 0.5..=64.0, " px", false);
            }
            BalloonBodyKindChoice::Uniflash => {
                adaptive_slider(ui, tl!("Needles:"), &mut state.uniflash_needles, 32..=400, "", false);
                let mut pct = (state.uniflash_inner_ratio * 100.0).round();
                if adaptive_slider(ui, tl!("Text area:"), &mut pct, 30.0..=90.0, "%", false).changed() {
                    state.uniflash_inner_ratio = pct / 100.0;
                }
                adaptive_slider(ui, tl!("Bundle:"), &mut state.uniflash_bundle, 0.0..=1.0, "", false);
                adaptive_slider(ui, tl!("Needle width:"), &mut state.uniflash_needle_width, 1.0..=8.0, " px", false);
                adaptive_slider(ui, tl!("Outer sharpness:"), &mut state.uniflash_outer_sharpness, 0.0..=1.0, "", false);
                adaptive_slider(ui, tl!("Inner sharpness:"), &mut state.uniflash_inner_sharpness, 0.0..=1.0, "", false);
            }
        }

        ui.add_space(6.0);
        ui.separator();

        // 3. Tail presets
        ui.label(RichText::new(tl!("Tail type:")).strong().color(t.text));
        ui.horizontal_wrapped(|ui| {
            let tails = [
                (TailPreset::None, tl!("⊘ None")),
                (TailPreset::Wedge, tl!("▽ Wedge")),
                (TailPreset::Arc, tl!("⌒ Arc")),
                (TailPreset::SCurve, tl!("~ SCurve")),
                (TailPreset::Lightning, tl!("⚡ Lightning")),
                (TailPreset::Double, tl!("⑂ Double")),
                (TailPreset::Thought, tl!("◌ Thought")),
            ];
            for (preset, label) in tails {
                let is_sel = state.tail_preset == preset;
                if ui.selectable_label(is_sel, label).clicked() {
                    state.tail_preset = preset;
                }
            }
        });

        // With the disabled tail the tail parameters hide: the balloon is the body only.
        if state.tail_preset != TailPreset::None {
            adaptive_slider(ui, tl!("Mouth width:"), &mut state.tail_mouth_width, 6.0..=120.0, " px", false);
            adaptive_slider(ui, tl!("Bend:"), &mut state.tail_bend, -1.0..=1.0, "", false);
        }
        if state.tail_preset == TailPreset::Thought {
            adaptive_slider(ui, tl!("Thought bubbles:"), &mut state.tail_bubbles, 1..=8, "", false);
            adaptive_slider(ui, tl!("Bubble radius:"), &mut state.tail_bubble_radius, 4.0..=30.0, " px", false);
        }

        ui.add_space(6.0);
        ui.separator();

        // 4. Dialogue fit (auto-fit + overflow warning)
        ui.label(RichText::new(tl!("Dialogue and fit:")).strong().color(t.text));
        ui.add(egui::TextEdit::multiline(&mut state.fit_text).desired_rows(2).desired_width(ui.available_width()).hint_text(tl!("Dialogue text…")));
        ui.horizontal_wrapped(|ui| {
            adaptive_slider(ui, tl!("Type size:"), &mut state.fit_size_pt, 4.0..=72.0, " pt", false);
            if ui.button(tl!("＋ Fit balloon")).on_hover_text(tl!("Fit the shape to the text: the balloon grows when the line does not fit")).clicked() {
                fit_balloon_to_text(app, &mut state);
            }
        });
        if let Some((fits, line_count, need_pt)) = balloon_fit_status(app, &state) {
            if fits {
                let text =
                    crate::i18n::fmt(tl!("✓ Text fits: {lines} lines, {size} pt"), &[("lines", &line_count.to_string()), ("size", &format!("{need_pt:.1}"))]);
                ui.label(RichText::new(text).color(t.text));
            } else {
                ui.label(RichText::new(tl!("⚠ Text does not fit — enlarge the balloon or reduce the type size")).color(t.warning));
            }
        }

        ui.add_space(6.0);
        ui.separator();

        // 5. Contour and fill style
        ui.label(RichText::new(tl!("Stroke and fill:")).strong().color(t.text));
        adaptive_slider(ui, tl!("Width:"), &mut state.stroke_width, 0.5..=20.0, " px", false);
        ui.horizontal_wrapped(|ui| {
            for w in [1.5, 2.0, 2.5, 3.0, 4.0] {
                let is_cur = (state.stroke_width - w).abs() < 0.1;
                if ui.selectable_label(is_cur, format!("{w}")).clicked() {
                    state.stroke_width = w;
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(tl!("Fill:"));
            ui.radio_value(&mut state.fill_mode, BalloonFillMode::White, tl!("White"));
            ui.radio_value(&mut state.fill_mode, BalloonFillMode::Transparent, tl!("Transparent"));
            ui.radio_value(&mut state.fill_mode, BalloonFillMode::Background, tl!("Background colour"));
        });
        adaptive_slider(ui, tl!("Hand-drawn noise:"), &mut state.hand_drawn_noise, 0.0..=1.0, "", false);

        ui.add_space(10.0);
        ui.separator();

        // 6. Action buttons
        let create_btn = ui.add_sized(
            [ui.available_width(), 30.0],
            egui::Button::new(RichText::new(tl!("＋ Create balloon on canvas")).strong().color(Color32::WHITE)).fill(t.accent),
        );
        if create_btn.clicked() {
            create_balloon_on_canvas(app, &state);
        }

        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button(tl!("＋ Add tail")).on_hover_text(tl!("Add a tail to the selected balloon")).clicked() {
                add_tail_to_active(app, &state);
            }
            if ui.button(tl!("⚡ Bake to SAI Linework")).on_hover_text(tl!("Turn the balloon into an editable vector linework layer")).clicked()
                && app.run("shape.balloon.convertToLinework", json!({})).is_ok()
            {
                app.ui.tool = Tool::VectorCurve;
                app.ui.dock_tabs.layers = 3;
                crate::dock::reveal(app, crate::dock::Group::Layers);
                if app.ui.docking.is_some() || crate::panel_docking::panel_in_side_dock(app, "linework") {
                    let _ = crate::panel_docking::command(app, "window.panel.activate", &json!({"panel": "linework"}));
                }
            }
        });

        if ui.button(tl!("🔄 Reset to the manga default")).clicked() {
            state = BalloonPanelUi::default();
        }
    });

    if geometry_changed(&state, &initial_state) {
        let applied_to_active = apply_panel_to_active_layer(app, &state, &initial_state);
        let active_layer_id = app.session.active().and_then(|a| a.active_layer.map(|id| id.0));
        let is_balloon_layer = active_balloon_target(app).is_some();

        log::info!(
            "[Telemetry:Balloon:PanelMutate] body={:?}, tail={:?}, stroke={}, mouth_w={} | has_doc={}, active_layer_id={:?}, \
             active_layer_is_balloon={is_balloon_layer}, applied_to_active={applied_to_active}",
            state.body_choice,
            state.tail_preset,
            state.stroke_width,
            state.tail_mouth_width,
            app.session.active().is_some(),
            active_layer_id,
        );
    }

    app.ui.balloon_ui = state;
}

/// Whether the panel state changed anything the balloon stores.
///
/// The dialogue text and the fit type size live only in the panel (the layer's text lives in the
/// layer): typing into the dialogue field must not send `shape.balloon.update`, create an undo
/// step or touch the active balloon at all.
fn geometry_changed(a: &BalloonPanelUi, b: &BalloonPanelUi) -> bool {
    let mut a = a.clone();
    let mut b = b.clone();
    a.fit_text.clear();
    b.fit_text.clear();
    a.fit_size_pt = 0.0;
    b.fit_size_pt = 0.0;
    a != b
}

pub(crate) fn create_live_balloon_in_rect(app: &mut PhotocraftApp, st: &BalloonPanelUi, rect: [f64; 4]) -> bool {
    let [bx, by, bw, bh] = rect;
    log::info!(
        "[Telemetry:Balloon:Create] body_choice={:?}, tail_preset={:?}, rect={rect:?}, stroke_width={}",
        st.body_choice,
        st.tail_preset,
        st.stroke_width
    );
    let body = body_from_state(st, rect);

    // A `⊘ Нет` balloon is the body only; the Uniflash layer renders with the fill in the stroke
    // colour and no stroke (the closed needle polygons are the ink, TZ v5.0).
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

    let balloon = LiveBalloon {
        id: 1,
        body,
        tails,
        next_tail_id: 2,
        vertex_overrides: Vec::new(),
        hand_drawn_noise: st.hand_drawn_noise.clamp(0.0, 1.0),
        stroke_width: st.stroke_width,
        auto_merge_overlapping: true,
    };

    let fill_val = if is_uniflash { json!("#000000") } else { balloon_fill_value(app, st.fill_mode) };
    let stroke_val = if is_uniflash { json!(null) } else { json!({ "width": st.stroke_width, "color": "#000000" }) };

    let ok = app
        .run(
            "shape.balloon.create",
            json!({
                "balloon": balloon,
                "fill": fill_val,
                "stroke": stroke_val
            }),
        )
        .is_ok();
    if ok {
        app.balloon_gizmo.active_balloon_id = Some(balloon.id);
    }

    // Мгновенно переключаем инструмент на бабблы, чтобы на экране появились ручки Гизмо!
    app.ui.tool = Tool::Balloon;
    ok
}

fn create_balloon_on_canvas(app: &mut PhotocraftApp, st: &BalloonPanelUi) {
    let Some(active) = app.session.active() else { return };
    let doc_size = active.doc.size;

    let bounds = active.doc.selection.as_ref().map(photocraft_compose::bounds::content_bounds).filter(|r| r.width() >= 16 && r.height() >= 16);

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

    create_live_balloon_in_rect(app, st, [bx, by, bw, bh]);
}

fn add_tail_to_active(app: &mut PhotocraftApp, st: &BalloonPanelUi) {
    let Some(active) = app.session.active() else { return };
    let Some(layer_id) = active.active_layer else { return };
    let balloon_id = app.balloon_gizmo.active_balloon_id.unwrap_or(1);

    let _ = app.run(
        "shape.balloon.tail.add",
        json!({
            "layer": layer_id.0,
            "balloonId": balloon_id,
            "preset": st.tail_preset.id(),
            "mouthWidth": st.tail_mouth_width,
            "tip": [200.0, 300.0]
        }),
    );
    app.ui.tool = Tool::Balloon;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> (PhotocraftApp, egui::Context) {
        let ctx = egui::Context::default();
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width": 200, "height": 200, "depth": 8})).unwrap();
        app.sync_views();
        (app, ctx)
    }

    #[test]
    fn window_menu_toggles_the_balloon_dock_tab() {
        let (mut app, ctx) = test_app();
        for pro in [false, true] {
            assert_eq!(crate::dock::Group::Color.tabs(pro).get(BALLOON_COLOR_TAB), Some(&"Balloons"), "pro {pro}");
        }
        assert!(crate::menus::is_live(PANEL), "{PANEL}");
        crate::menus::invoke(&mut app, &ctx, PANEL, Value::Null).unwrap();
        assert!(app.ui.panels.color, "the Color group shows");
        assert_eq!(app.ui.dock_tabs.color, BALLOON_COLOR_TAB, "the Balloons tab is selected");
        assert_eq!(checked(&app, PANEL), Some(true));
        crate::menus::invoke(&mut app, &ctx, PANEL, json!({"show": false})).unwrap();
        assert_eq!(checked(&app, PANEL), Some(false));
    }

    #[test]
    fn balloon_panel_ui_default_and_protection() {
        let st = BalloonPanelUi::default();
        assert_eq!(st.body_choice, BalloonBodyKindChoice::Ellipse);
        assert_eq!(st.tail_preset, TailPreset::Arc);
        assert!(MIN_BALLOON_SIZE >= 24.0);
        assert!(st.uniflash_needles >= 32);
        assert!(st.stroke_width > 0.0);
    }

    #[test]
    fn create_balloon_on_canvas_protects_bounds_and_activates_tool() {
        let (mut app, _ctx) = test_app();
        let st = BalloonPanelUi::default();
        create_balloon_on_canvas(&mut app, &st);
        assert_eq!(app.ui.tool, Tool::Balloon);
        let active = app.session.active().unwrap();
        assert!(active.active_layer.is_some());
    }

    #[test]
    fn balloon_panel_write_through_updates_live_layer() {
        let (mut app, _ctx) = test_app();
        let prev = BalloonPanelUi::default();
        create_balloon_on_canvas(&mut app, &prev);

        let mut next = prev.clone();
        next.body_choice = BalloonBodyKindChoice::Spiky;
        next.spiky_spikes = 14;
        next.tail_preset = TailPreset::Lightning;
        next.tail_mouth_width = 40.0;
        next.stroke_width = 5.5;
        next.fill_mode = BalloonFillMode::Transparent;

        assert!(apply_panel_to_active_layer(&mut app, &next, &prev));

        let active = app.session.active().unwrap();
        let lid = active.active_layer.unwrap();
        let layer = active.doc.layer(lid).unwrap();
        let photocraft_doc::LayerContent::Shape(sh) = &layer.content else { panic!("expected shape layer") };
        assert!(sh.fill.is_none());
        assert_eq!(sh.stroke.as_ref().map(|s| s.width), Some(5.5));
        let Some(photocraft_doc::LiveShape::BalloonGroup { group }) = &sh.live else { panic!("expected balloon group") };
        let b = &group.items[0];
        assert!(matches!(b.body, LiveBalloonBody::Spiky { spikes: 14, .. }));
        assert_eq!(b.tails[0].preset, TailPreset::Lightning);
        assert!((b.tails[0].mouth_width - 40.0).abs() < 1e-4);

        let mut hydrated = BalloonPanelUi::default();
        hydrate_state_from_balloon(&mut hydrated, b, sh.fill.as_ref(), sh.stroke.as_ref().map(|s| s.width));
        assert_eq!(hydrated.body_choice, BalloonBodyKindChoice::Spiky);
        assert_eq!(hydrated.spiky_spikes, 14);
        assert_eq!(hydrated.tail_preset, TailPreset::Lightning);
        assert_eq!(hydrated.fill_mode, BalloonFillMode::Transparent);
        assert!((hydrated.stroke_width - 5.5).abs() < 1e-4);
    }

    /// The dialogue fit the panel offers knows when the text does not fit and grows the body for
    /// it. It is pinned here as well as in the engine, because the panel is where it is used.
    #[test]
    fn the_dialogue_fit_knows_when_the_text_does_not_fit() {
        use photocraft_engine::balloon_fit::{body_size_for_text, fit_text};

        let fits = fit_text("Hi!", 200.0, 80.0, 12.0, 72.0);
        assert!(fits.fits, "a short line fits: {fits:?}");
        assert_eq!(fits.line_count(), 1);
        assert!((fits.size_pt - 12.0).abs() < 0.01, "the preferred size is kept: {fits:?}");
        assert_eq!(fits, fit_text("Hi!", 200.0, 80.0, 12.0, 72.0), "the fit is deterministic");

        let overflow = fit_text("Эта реплика не влезает даже минимальным кеглем, потому что баббл крошечный", 30.0, 18.0, 14.0, 72.0);
        assert!(!overflow.fits, "a balloon far too small reports an overflow: {overflow:?}");

        let small = body_size_for_text("Коротко", 18.0, 72.0, 0.8);
        let long = body_size_for_text("Значительно больше текста в этой реплике", 18.0, 72.0, 0.8);
        assert!(long[0] >= small[0] && long[1] >= small[1], "more text needs a bigger body: {small:?} vs {long:?}");
    }
}
