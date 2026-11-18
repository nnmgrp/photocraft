//! Balloon commands: comic tails merged with a shape layer, procedural thought/shout balloons and
//! SAI-style click linework.
//!
//! All geometry comes from `photocraft_vector::balloon` and lands in an ordinary
//! [`ShapeLayer`](photocraft_doc::ShapeLayer) path, so Free Transform, history, PSD export and the
//! rest of the shape tooling keep working unchanged.

use std::collections::HashSet;

use photocraft_doc::vector::{BalloonVoice, LiveBalloon, LiveBalloonBody, LiveBalloonGroup, LiveBalloonTail, TailPreset, TextDirection};
use photocraft_doc::{Color, Document, Fill, Layer, LayerContent, LayerId, LiveShape, ShapeLayer, ShapeStroke, TextLayer};
use photocraft_geom::Point;
use photocraft_vector as vector;
use photocraft_vector::balloon::{self, BalloonTail, BurstParams, CloudParams, LegacyTailPreset, LiveBalloonGeom, LiveBalloonGroupGeom, SaiPoint, SpikeParams};
use photocraft_vector::sai_curve::LiveLinework;
use serde_json::{Value, json};

use super::vector_brush::is_safe_single_filename;
use super::vector_curve::set_live_linework;
use crate::cmd::type_cmds::{apply_box, refresh_at, replace_text};
use crate::cmd::vector_cmds::params::{layer_id, nums, parse_fill, pt};
use crate::cmd::vector_cmds::shape::{refresh_shape, shape_info, with_shape};
use crate::cmd::vector_cmds::{bad, has_doc};
use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};
use photocraft_manga_core::balloon_fit::{body_size_for_text_oriented, fit_text_directed};
use photocraft_manga_core::snow_knuth_plass::reflow_story_pair_text;

macro_rules! spec {
    ($id:literal, $label:literal, [$($m:literal),*], $params:expr, $en:expr, $run:expr) => {
        CommandSpec { id: $id, label: $label, menu: &[$($m),*], shortcut: None, params: $params, enabled: $en, run: $run, journal: true }
    };
}

#[allow(dead_code)]
fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// A finite finite-coordinate point parameter.
fn point(cmd: &str, p: &Value, key: &str) -> Result<Point> {
    p.get(key).and_then(pt).ok_or_else(|| bad(cmd, format!("`{key}` must be a finite [x, y] point")))
}

/// Two points from `[[x, y], [x, y]]`.
fn point_pair(v: Option<&Value>) -> Option<[(f64, f64); 2]> {
    let arr = v?.as_array()?;
    let [a, b] = arr.as_slice() else { return None };
    let pa = pt(a)?;
    let pb = pt(b)?;
    Some([(pa.x, pa.y), (pb.x, pb.y)])
}

fn finite_point(x: f64, y: f64) -> bool {
    x.is_finite() && y.is_finite() && x.abs() <= 1.0e7 && y.abs() <= 1.0e7
}

fn opt_f64(p: &Value, key: &str, cmd: &str) -> Result<Option<f64>> {
    match p.get(key) {
        None => Ok(None),
        Some(v) => v.as_f64().filter(|x| x.is_finite()).map(Some).ok_or_else(|| bad(cmd, format!("`{key}` must be a finite number"))),
    }
}

fn opt_u32(p: &Value, key: &str, cmd: &str) -> Result<Option<u32>> {
    match p.get(key) {
        None => Ok(None),
        Some(v) => u32::try_from(v.as_u64().ok_or_else(|| bad(cmd, format!("`{key}` must be a non-negative integer")))?)
            .map(Some)
            .map_err(|_| bad(cmd, format!("`{key}` is too large"))),
    }
}

fn opt_bool(p: &Value, key: &str, cmd: &str) -> Result<Option<bool>> {
    match p.get(key) {
        None => Ok(None),
        Some(v) => v.as_bool().map(Some).ok_or_else(|| bad(cmd, format!("`{key}` must be true or false"))),
    }
}

/// Projection of `p` onto the segment `a..b`, clamped to the segment.
fn project_on(a: (f64, f64), b: (f64, f64), p: (f64, f64)) -> (f64, f64) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    if len2 < 1.0e-12 {
        return a;
    }
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0);
    (a.0 + dx * t, a.1 + dy * t)
}

/// The point of the shape's contour nearest to `target` (used when a tail has a `tip` but no
/// explicit mouth): the tail then starts on the balloon's edge facing the words.
fn nearest_on_body(s: &Session, id: LayerId, target: Point, cmd: &str) -> Result<(f64, f64)> {
    let doc = s.active().map(|d| &d.doc).ok_or(EngineError::NoDocument)?;
    let layer = doc.layer(id).ok_or(EngineError::NoLayer(id))?;
    let LayerContent::Shape(shape) = &layer.content else {
        return Err(bad(cmd, "layer is not a shape layer"));
    };
    let target = (target.x, target.y);
    let mut best: Option<((f64, f64), f64)> = None;
    for poly in vector::flatten_path(&shape.path, 0.25) {
        if !poly.closed || poly.pts.len() < 2 {
            continue;
        }
        let n = poly.pts.len();
        for i in 0..n {
            let a = poly.pts[i];
            let b = poly.pts[(i + 1) % n];
            let candidate = project_on(a, b, target);
            let d = (candidate.0 - target.0).hypot(candidate.1 - target.1);
            if best.is_none_or(|(_, best_d)| d < best_d) {
                best = Some((candidate, d));
            }
        }
    }
    best.map(|(point, _)| point).ok_or_else(|| bad(cmd, "the shape has no closed contour to attach a tail to"))
}

fn has_shape(s: &Session) -> std::result::Result<(), String> {
    let st = s.active().ok_or_else(|| "no document open".to_string())?;
    let id = st.active_layer.ok_or_else(|| "no active layer".to_string())?;
    let layer = st.doc.layer(id).ok_or_else(|| "no active layer".to_string())?;
    if matches!(layer.content, photocraft_doc::LayerContent::Shape(_)) { Ok(()) } else { Err("the active layer is not a shape layer".to_string()) }
}

/// Rejects a balloon whose numbers are not finite or are out of range, so the geometry can never
/// produce NaN coordinates or an unbounded shape.
fn validate_balloon(b: &LiveBalloon) -> std::result::Result<(), String> {
    let rect = b.body.rect();
    if rect.iter().any(|v| !v.is_finite() || v.abs() > 1.0e7) {
        return Err("the balloon's rectangle must be finite and within 10000000 px".into());
    }
    if rect[2].abs() < 1.0 || rect[3].abs() < 1.0 {
        return Err("the balloon must have a non-zero width and height".into());
    }
    if !b.stroke_width.is_finite() || !(0.1..=1.0e4).contains(&b.stroke_width) {
        return Err("the balloon's stroke width must be between 0.1 and 10000 px".into());
    }
    if !b.hand_drawn_noise.is_finite() || !(0.0..=1.0).contains(&b.hand_drawn_noise) {
        return Err("the balloon's noise must be between 0.0 and 1.0".into());
    }
    if !b.hand_drawn_warp.is_finite() || !(0.0..=1.0).contains(&b.hand_drawn_warp) {
        return Err("the balloon's warp noise must be between 0.0 and 1.0".into());
    }
    if !b.noise_scale.is_finite() || !(0.25..=4.0).contains(&b.noise_scale) {
        return Err("the balloon's noise scale must be between 0.25 and 4.0".into());
    }
    if !b.voice_intensity.is_finite() || !(0.0..=1.0).contains(&b.voice_intensity) {
        return Err("the balloon's voice intensity must be between 0.0 and 1.0".into());
    }
    if let Some(u) = &b.underlay
        && (!u.offset.iter().all(|v| v.is_finite() && v.abs() <= 1.0e5)
            || !u.scale.is_finite()
            || !(0.5..=2.0).contains(&u.scale)
            || !u.rectness.is_finite()
            || !(0.0..=1.0).contains(&u.rectness))
    {
        return Err("the balloon's underlay parameters are out of range".into());
    }
    if let Some(ref preset_id) = b.brush_preset_id
        && !is_safe_single_filename(preset_id)
    {
        return Err("the balloon's `brush_preset_id` must be a valid preset identifier".into());
    }
    if let Some(ref svg) = b.custom_svg_contour
        && svg.len() > 262_144
    {
        return Err("the balloon's custom SVG contour exceeds 256 KiB".into());
    }
    let finite = |v: f64| v.is_finite();
    match &b.body {
        LiveBalloonBody::Ellipse { squircle, .. } => {
            if !finite(*squircle) {
                return Err("the balloon's squircle must be finite".into());
            }
        }
        LiveBalloonBody::Rect { corner_radius, .. } => {
            if !finite(*corner_radius) {
                return Err("the balloon's corner radius must be finite".into());
            }
        }
        LiveBalloonBody::ChamferRect { chamfer, concave, .. } => {
            if !finite(*chamfer) || !finite(*concave) || *chamfer < 0.0 || !(0.0..=1.0).contains(concave) {
                return Err("the chamfer rect's chamfer and concave must be valid".into());
            }
        }
        LiveBalloonBody::Cloud { bumps, amplitude, cusp_bite, jitter, .. } => {
            if !finite(*amplitude) || !finite(*cusp_bite) || !finite(*jitter) {
                return Err("the cloud's parameters must be finite".into());
            }
            if *bumps > 64 {
                return Err("the cloud may have at most 64 lobes".into());
            }
        }
        LiveBalloonBody::Spiky { spikes, length, sub_harmonic, concavity, jitter, .. } => {
            if !finite(*length) || !finite(*sub_harmonic) || !finite(*concavity) || !finite(*jitter) {
                return Err("the spike's parameters must be finite".into());
            }
            if *spikes > 128 {
                return Err("the balloon may have at most 128 spikes".into());
            }
        }
        LiveBalloonBody::Burst { rays, length, depth, jitter, .. } => {
            if !finite(*length) || !finite(*depth) || !finite(*jitter) {
                return Err("the burst's parameters must be finite".into());
            }
            if !(5..=64).contains(rays) {
                return Err("the burst must have between 5 and 64 rays".into());
            }
            if !(0.10..=1.20).contains(length) {
                return Err("the burst's ray length must be between 0.10 and 1.20".into());
            }
            if !(0.0..=0.60).contains(depth) {
                return Err("the burst's valley depth must be between 0.0 and 0.60".into());
            }
            if !(0.0..=1.0).contains(jitter) {
                return Err("the burst's jitter must be between 0.0 and 1.0".into());
            }
        }
        LiveBalloonBody::DoubleOutline { squircle, gap, .. } => {
            if !finite(*squircle) || !finite(*gap) {
                return Err("the double outline's parameters must be finite".into());
            }
            if !(0.0..=1.0).contains(squircle) {
                return Err("the double outline's squircle must be between 0.0 and 1.0".into());
            }
            if !(0.5..=64.0).contains(gap) {
                return Err("the double outline's gap must be between 0.5 and 64 px".into());
            }
        }
        LiveBalloonBody::Uniflash {
            needles, inner_ratio, bundle_group, length_jitter, angle_jitter, needle_width, outer_sharpness, inner_sharpness, ..
        } => {
            if !finite(*inner_ratio) || !finite(*bundle_group) || !finite(*length_jitter) || !finite(*angle_jitter) || !finite(*needle_width) {
                return Err("the uniflash's parameters must be finite".into());
            }
            if *needles > 450 {
                return Err("the uniflash may have at most 450 needles".into());
            }
            if !finite(*outer_sharpness) || !finite(*inner_sharpness) || !(0.0..=1.0).contains(outer_sharpness) || !(0.0..=1.0).contains(inner_sharpness) {
                return Err("the uniflash's outer and inner sharpness must be between 0.0 and 1.0".into());
            }
        }
    }
    if !b.double_gap_px.is_finite() || !(0.5..=64.0).contains(&b.double_gap_px) {
        return Err("double_gap_px must be between 0.5 and 64 px".into());
    }
    if !(6..=96).contains(&b.whisper_dash_count) || !b.whisper_duty.is_finite() || !(0.20..=0.85).contains(&b.whisper_duty) {
        return Err("whisper line style parameters (whisper_dash_count 6..=96, whisper_duty 0.20..=0.85) are out of range".into());
    }
    if !(1..=8).contains(&b.pencil_strands) || !b.pencil_roughness.is_finite() || !(0.0..=1.0).contains(&b.pencil_roughness) {
        return Err("pencil line style parameters are out of range".into());
    }
    if !b.pixel_size_px.is_finite() || !(1.0..=32.0).contains(&b.pixel_size_px) {
        return Err("pixel_size_px must be between 1 and 32 px".into());
    }
    if !b.ukiyo_contrast.is_finite() || !(0.0..=1.0).contains(&b.ukiyo_contrast) {
        return Err("ukiyo_contrast must be between 0.0 and 1.0".into());
    }
    if !b.haze_spread.is_finite()
        || !(0.0..=100.0).contains(&b.haze_spread)
        || !b.stipple_density.is_finite()
        || !(0.0..=100.0).contains(&b.stipple_density)
        || !(24..=600).contains(&b.stipple_count)
        || !b.stipple_randomness.is_finite()
        || !(0.0..=100.0).contains(&b.stipple_randomness)
        || !b.grain_size.is_finite()
        || !(5.0..=100.0).contains(&b.grain_size)
        || !b.edge_roughness.is_finite()
        || !(0.0..=100.0).contains(&b.edge_roughness)
    {
        return Err("stipple_dots line style parameters are out of range".into());
    }
    if !(0..=128).contains(&b.sparkle_count)
        || !b.sparkle_size.is_finite()
        || !(0.2..=4.0).contains(&b.sparkle_size)
        || !b.sparkle_randomness.is_finite()
        || !(0.0..=100.0).contains(&b.sparkle_randomness)
    {
        return Err("sparkle_glitter line style parameters are out of range".into());
    }
    if b.tails.len() > 64 {
        return Err("a balloon may have at most 64 tails".into());
    }
    for t in &b.tails {
        if !t.tip.iter().all(|v| v.is_finite() && v.abs() <= 1.0e7) {
            return Err("a tail's tip must be finite and within 10000000 px".into());
        }
        if !t.perimeter_t.is_finite() {
            return Err("a tail's perimeter parameter must be finite".into());
        }
        if !t.mouth_width.is_finite() || !(0.1..=1.0e4).contains(&t.mouth_width) {
            return Err("a tail's mouth width must be between 0.1 and 10000 px".into());
        }
        if !t.bend.is_finite() {
            return Err("a tail's bend must be finite".into());
        }
        if !t.bubble_radius.is_finite() || !(0.1..=1.0e4).contains(&t.bubble_radius) {
            return Err("a tail's bubble radius must be between 0.1 and 10000 px".into());
        }
    }
    for v in &b.vertex_overrides {
        if !v.offset.iter().all(|x| x.is_finite() && x.abs() <= 1.0e7) {
            return Err("a vertex offset must be finite and within 10000000 px".into());
        }
    }
    for s in &b.stipple_overrides {
        if !s.offset.iter().all(|x| x.is_finite() && x.abs() <= 1.0e7) || !s.radius_scale.is_finite() || !(0.1..=5.0).contains(&s.radius_scale) {
            return Err("a stipple override offset and radius_scale (0.1..=5.0) must be finite".into());
        }
    }
    Ok(())
}

/// Parses the `balloon` payload of a command.
fn parse_balloon(cmd: &str, p: &Value) -> Result<LiveBalloon> {
    let raw = p.get("balloon").cloned().ok_or_else(|| bad(cmd, "missing `balloon`"))?;
    let b: LiveBalloon = serde_json::from_value(raw).map_err(|e| bad(cmd, format!("`balloon` is not a valid balloon: {e}")))?;
    validate_balloon(&b).map_err(|e| bad(cmd, e))?;
    Ok(b)
}

/// `shape.balloon.create` with a `balloon` payload: a parametric manga balloon as a new shape layer.
fn balloon_create_live(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "shape.balloon.create";
    let b = parse_balloon(CMD, p)?;
    let mut group = LiveBalloonGroup { items: vec![b], next_balloon_id: 1 };
    group.next_balloon_id = group.items.iter().map(|it| it.id).max().unwrap_or(0).saturating_add(1);
    let path = group.rebuild_group_path();
    if path.is_empty() {
        return Err(bad(CMD, "the balloon produces no drawable path"));
    }
    vector::flatten::validate_shape(&path, None).map_err(|e| bad(CMD, e))?;

    let fg = s.tools.foreground;
    let fgc = Color::rgba(fg[0], fg[1], fg[2], fg[3]);
    let fill = match p.get("fill") {
        Some(v) => parse_fill(v).map_err(|e| bad(CMD, e))?,
        None => Some(Fill::Solid(Color::WHITE)),
    };
    let width = group.items.first().map(|it| it.stroke_width).unwrap_or(3.0) as f32;
    let base = ShapeStroke { width, paint: Fill::Solid(Color::BLACK), ..Default::default() };
    let stroke = match p.get("stroke") {
        Some(v) => crate::cmd::vector_cmds::params::parse_stroke(v, Some(base), fgc).map_err(|e| bad(CMD, e))?,
        None => Some(base),
    };

    let name = p.get("name").and_then(Value::as_str).map(str::to_string);
    let mut sh = ShapeLayer { path, fill, stroke, live: Some(LiveShape::BalloonGroup { group }), ..Default::default() };
    let id = s.edit("New Balloon", |doc, active| {
        refresh_shape(doc, &mut sh);
        let name = name.unwrap_or_else(|| doc.next_layer_name("Speech Balloon"));
        let id = doc.insert_above(*active, Layer::new(name, LayerContent::Shape(sh)));
        *active = Some(id);
        Ok(id)
    })?;
    shape_info(s, id)
}

fn check_layer_unlocked(doc: &Document, id: LayerId) -> Result<()> {
    let locks = doc.effective_locks(id);
    if locks.all || locks.pixels {
        return Err(EngineError::Other("shape layer is locked".into()));
    }
    Ok(())
}

fn text_box_dims(tl: &TextLayer) -> (f32, f32) {
    match tl.shape {
        photocraft_doc::text::TextShape::Box { width, height, .. } => (width.max(8.0), height.max(8.0)),
        _ => (120.0, 80.0),
    }
}

fn sync_linked_text_box(doc: &mut Document, b: &LiveBalloon) {
    if !b.auto_follow_text {
        return;
    }
    let Some(tid) = b.linked_text_layer else {
        return;
    };
    let locks = doc.effective_locks(tid);
    if locks.all || locks.pixels {
        return;
    }
    let r = b.safe_text_rect();
    if !r.iter().all(|v| v.is_finite()) {
        return;
    }
    let rect = [r[0], r[1], r[2].max(8.0), r[3].max(8.0)];
    let dpi = doc.resolution_dpi.max(1.0);
    let fmt = doc.pixel_format();
    if let Some(layer) = doc.layer_mut(tid)
        && let LayerContent::Text(ref mut tl) = layer.content
    {
        apply_box(tl, rect);
        refresh_at(dpi, fmt, tl);
    }
}

fn collect_story_pairs(doc: &Document, items: &[LiveBalloon]) -> Vec<(LayerId, LayerId)> {
    let mut pairs = Vec::new();
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let linked_ids: HashSet<LayerId> = items.iter().filter(|b| b.auto_follow_text).filter_map(|b| b.linked_text_layer).collect();
    for &tid in &linked_ids {
        if let Some(layer) = doc.layer(tid)
            && let LayerContent::Text(tl) = &layer.content
            && tl.story_threaded
            && let Some(next_id) = tl.next_story_layer
            && next_id != tid
            && seen.insert((tid.0, next_id.0))
        {
            pairs.push((tid, next_id));
        }
        for (_, _, l) in doc.walk() {
            if let LayerContent::Text(tl) = &l.content
                && tl.story_threaded
                && tl.next_story_layer == Some(tid)
                && l.id != tid
                && seen.insert((l.id.0, tid.0))
            {
                pairs.push((l.id, tid));
            }
        }
    }
    pairs
}

fn reflow_story_pair(doc: &mut Document, ta_id: LayerId, tb_id: LayerId) {
    let locks_a = doc.effective_locks(ta_id);
    let locks_b = doc.effective_locks(tb_id);
    if locks_a.all || locks_a.pixels || locks_b.all || locks_b.pixels {
        return;
    }
    let Some((text_a, box_a, size_pt, h_scale)) = doc.layer(ta_id).and_then(|l| match &l.content {
        LayerContent::Text(tl) => {
            let size = tl.runs.first().map_or(tl.size_pt, |r| r.style.size_pt).clamp(4.0, 200.0);
            let h_scale = tl.runs.first().map_or(100.0, |r| r.style.horizontal_scale).clamp(20.0, 400.0);
            Some((tl.text.clone(), text_box_dims(tl), size, h_scale))
        }
        _ => None,
    }) else {
        return;
    };
    let Some((text_b, box_b)) = doc.layer(tb_id).and_then(|l| match &l.content {
        LayerContent::Text(tl) => Some((tl.text.clone(), text_box_dims(tl))),
        _ => None,
    }) else {
        return;
    };
    let dpi = doc.resolution_dpi.max(1.0);
    let fmt = doc.pixel_format();
    let (new_a, new_b) = reflow_story_pair_text(&text_a, &text_b, box_a, box_b, size_pt, dpi, h_scale);
    if new_a != text_a
        && let Some(layer) = doc.layer_mut(ta_id)
        && let LayerContent::Text(ref mut tl) = layer.content
    {
        replace_text(tl, 0, tl.text.len(), &new_a);
        refresh_at(dpi, fmt, tl);
    }
    if new_b != text_b
        && let Some(layer) = doc.layer_mut(tb_id)
        && let LayerContent::Text(ref mut tl) = layer.content
    {
        replace_text(tl, 0, tl.text.len(), &new_b);
        refresh_at(dpi, fmt, tl);
    }
}

fn sync_balloon_group_linked_texts(doc: &mut Document, items: &[LiveBalloon], size_changed: bool) {
    for b in items {
        sync_linked_text_box(doc, b);
    }
    if !size_changed {
        return;
    }
    let pairs = collect_story_pairs(doc, items);
    for (ta_id, tb_id) in pairs {
        reflow_story_pair(doc, ta_id, tb_id);
    }
}

/// `shape.balloon.update`: replaces one balloon of the layer's group, rebuilds the outline, and
/// keeps any linked TypeR boxes / `Stories` threads synchronized in the same history step.
fn balloon_update(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "shape.balloon.update";
    let id = layer_id(s, p)?;
    let mut updated = parse_balloon(CMD, p)?;
    let link_explicit = p.get("balloon").and_then(Value::as_object).is_some_and(|o| o.contains_key("linkedTextLayer"));
    if let Some(st) = s.active() {
        check_layer_unlocked(&st.doc, id)?;
    }
    s.edit("Update Balloon", |doc, _| {
        check_layer_unlocked(doc, id)?;
        let snapshot = doc.clone();
        let (items_copy, geometry_moved, size_changed) = {
            let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
            let LayerContent::Shape(mut sh) = std::mem::replace(&mut l.content, LayerContent::Fill(Fill::Solid(Color::BLACK))) else {
                return Err(EngineError::Other(format!("layer {} is not a shape layer", id.0)));
            };
            let res = (|| -> Result<(Vec<LiveBalloon>, bool, bool)> {
                let Some(LiveShape::BalloonGroup { ref mut group }) = sh.live else {
                    return Err(bad(CMD, "the layer is not a balloon group"));
                };
                let (moved, resized) = match group.items.iter_mut().find(|it| it.id == updated.id) {
                    Some(slot) => {
                        if !link_explicit {
                            updated.linked_text_layer = slot.linked_text_layer;
                        }
                        let old_r = slot.safe_text_rect();
                        let new_r = updated.safe_text_rect();
                        let pos_changed = old_r.iter().zip(new_r.iter()).any(|(a, b)| (a - b).abs() > 0.05);
                        let dim_changed = (old_r[2] - new_r[2]).abs() > 0.5 || (old_r[3] - new_r[3]).abs() > 0.5;
                        let follow_toggled = !slot.auto_follow_text && updated.auto_follow_text;
                        *slot = updated;
                        (pos_changed || follow_toggled, dim_changed || follow_toggled)
                    }
                    None => {
                        group.items.push(updated);
                        (true, false)
                    }
                };
                let path = group.rebuild_group_path();
                vector::flatten::validate_shape(&path, sh.stroke.as_ref()).map_err(|e| bad(CMD, e))?;
                sh.path = path;
                Ok((group.items.clone(), moved, resized))
            })();
            match res {
                Ok(out) => {
                    refresh_shape(&snapshot, &mut sh);
                    l.content = LayerContent::Shape(sh);
                    out
                }
                Err(e) => {
                    l.content = LayerContent::Shape(sh);
                    return Err(e);
                }
            }
        };
        if geometry_moved {
            sync_balloon_group_linked_texts(doc, &items_copy, size_changed);
        }
        Ok(json!({ "ok": true }))
    })
}

/// `shape.balloon.add`: adds a chamber to an existing `BalloonGroup` layer with a fresh `balloonId`.
fn balloon_add(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "shape.balloon.add";
    let id = layer_id(s, p)?;
    let balloon = parse_balloon(CMD, p)?;
    let mut assigned = 0u32;
    with_shape(s, id, "Add Balloon Chamber", |sh, _l| {
        let Some(LiveShape::BalloonGroup { ref mut group }) = sh.live else {
            return Err(bad(CMD, "the layer is not a balloon group"));
        };
        if group.items.len() >= 64 {
            return Err(bad(CMD, "a balloon group may have at most 64 chambers"));
        }
        assigned = group.push(balloon);
        let path = group.rebuild_group_path();
        vector::flatten::validate_shape(&path, sh.stroke.as_ref()).map_err(|e| bad(CMD, e))?;
        sh.path = path;
        Ok(json!({ "balloonId": assigned }))
    })
}

fn parse_merge_layer_ids(cmd: &str, p: &Value) -> Result<Vec<LayerId>> {
    let arr = p.get("layers").and_then(Value::as_array).ok_or_else(|| bad(cmd, "missing `layers` array"))?;
    let mut ids = Vec::with_capacity(arr.len());
    for entry in arr {
        let raw = entry.as_u64().ok_or_else(|| bad(cmd, "every `layers` entry must be a layer id"))?;
        let lid = LayerId(raw);
        if !ids.contains(&lid) {
            ids.push(lid);
        }
    }
    if ids.len() < 2 || ids.len() > 64 {
        return Err(bad(cmd, "provide between 2 and 64 distinct balloon layers to merge"));
    }
    Ok(ids)
}

fn wire_merged_story_chain(doc: &mut Document, text_ids: &[LayerId]) {
    if text_ids.len() < 2 {
        return;
    }
    let dpi = doc.resolution_dpi.max(1.0);
    let fmt = doc.pixel_format();
    for pair in text_ids.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a == b || doc.effective_locks(a).all {
            continue;
        }
        if let Some(layer) = doc.layer_mut(a)
            && let LayerContent::Text(ref mut tl) = layer.content
            && tl.next_story_layer.is_none()
        {
            tl.story_threaded = true;
            tl.next_story_layer = Some(b);
            refresh_at(dpi, fmt, tl);
        }
    }
}

/// `shape.balloon.merge`: merges 2+ `BalloonGroup` layers into the first layer's group while
/// preserving each chamber's `linked_text_layer` and wiring consecutive texts as a `Stories` chain.
fn balloon_merge(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "shape.balloon.merge";
    let ids = parse_merge_layer_ids(CMD, p)?;
    s.edit("Merge Balloons", |doc, active| {
        for &lid in &ids {
            check_layer_unlocked(doc, lid)?;
            let l = doc.layer(lid).ok_or(EngineError::NoLayer(lid))?;
            let LayerContent::Shape(sh) = &l.content else {
                return Err(bad(CMD, format!("layer {} is not a shape layer", lid.0)));
            };
            if !matches!(sh.live, Some(LiveShape::BalloonGroup { .. })) {
                return Err(bad(CMD, format!("layer {} is not a balloon group", lid.0)));
            }
        }
        let target_id = ids[0];
        let mut absorbed_items: Vec<LiveBalloon> = Vec::new();
        for &src_id in &ids[1..] {
            let Some(removed) = doc.remove(src_id) else {
                return Err(EngineError::NoLayer(src_id));
            };
            if let LayerContent::Shape(sh) = removed.content
                && let Some(LiveShape::BalloonGroup { group }) = sh.live
            {
                absorbed_items.extend(group.items);
            }
        }
        let snapshot = doc.clone();
        let (count, linked_texts) = {
            let target_layer = doc.layer_mut(target_id).ok_or(EngineError::NoLayer(target_id))?;
            let LayerContent::Shape(mut sh) = std::mem::replace(&mut target_layer.content, LayerContent::Fill(Fill::Solid(Color::BLACK))) else {
                return Err(bad(CMD, "target layer is not a shape layer"));
            };
            let res = (|| -> Result<(usize, Vec<LayerId>)> {
                let Some(LiveShape::BalloonGroup { ref mut group }) = sh.live else {
                    return Err(bad(CMD, "target layer is not a balloon group"));
                };
                if group.items.len() + absorbed_items.len() > 64 {
                    return Err(bad(CMD, "merged balloon group may have at most 64 chambers"));
                }
                for it in &mut group.items {
                    it.auto_merge_overlapping = true;
                }
                for mut item in absorbed_items {
                    item.auto_merge_overlapping = true;
                    group.push(item);
                }
                let path = group.rebuild_group_path();
                vector::flatten::validate_shape(&path, sh.stroke.as_ref()).map_err(|e| bad(CMD, e))?;
                sh.path = path;
                let texts: Vec<LayerId> = group.items.iter().filter_map(|b| b.linked_text_layer).collect();
                Ok((group.items.len(), texts))
            })();
            match res {
                Ok(out) => {
                    refresh_shape(&snapshot, &mut sh);
                    target_layer.content = LayerContent::Shape(sh);
                    out
                }
                Err(e) => {
                    target_layer.content = LayerContent::Shape(sh);
                    return Err(e);
                }
            }
        };
        wire_merged_story_chain(doc, &linked_texts);
        *active = Some(target_id);
        Ok(json!({ "layer": target_id.0, "balloonCount": count }))
    })
}

/// `shape.balloon.split`: detaches one chamber from a multi-chamber `BalloonGroup` layer into a
/// new `BalloonGroup` shape layer above it, preserving `linked_text_layer`.
fn balloon_split(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "shape.balloon.split";
    let id = layer_id(s, p)?;
    let requested_bid = opt_u32(p, "balloonId", CMD)?;
    if let Some(st) = s.active() {
        check_layer_unlocked(&st.doc, id)?;
    }
    s.edit("Split Balloon Chamber", |doc, active| {
        check_layer_unlocked(doc, id)?;
        let snapshot = doc.clone();
        let (split_layer, remaining_bid) = {
            let src_layer = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
            let src_name = src_layer.name.clone();
            let src_blend = src_layer.blend;
            let src_opacity = src_layer.opacity;
            let src_fill_opacity = src_layer.fill_opacity;
            let LayerContent::Shape(mut sh) = std::mem::replace(&mut src_layer.content, LayerContent::Fill(Fill::Solid(Color::BLACK))) else {
                return Err(bad(CMD, "the layer is not a shape layer"));
            };
            let res = (|| -> Result<(Layer, u32)> {
                let Some(LiveShape::BalloonGroup { ref mut group }) = sh.live else {
                    return Err(bad(CMD, "the layer is not a balloon group"));
                };
                if group.items.len() < 2 {
                    return Err(bad(CMD, "cannot split a single-chamber balloon"));
                }
                let idx = match requested_bid {
                    Some(bid) => group.items.iter().position(|b| b.id == bid).ok_or_else(|| bad(CMD, "the layer has no such balloon"))?,
                    None => group.items.len() - 1,
                };
                let mut detached = group.items.remove(idx);
                detached.id = 1;
                let src_path = group.rebuild_group_path();
                vector::flatten::validate_shape(&src_path, sh.stroke.as_ref()).map_err(|e| bad(CMD, e))?;
                sh.path = src_path;

                let split_group = LiveBalloonGroup::new(vec![detached]);
                let split_path = split_group.rebuild_group_path();
                vector::flatten::validate_shape(&split_path, sh.stroke.as_ref()).map_err(|e| bad(CMD, e))?;
                let mut split_sh = ShapeLayer {
                    path: split_path,
                    fill: sh.fill.clone(),
                    stroke: sh.stroke.clone(),
                    live: Some(LiveShape::BalloonGroup { group: split_group }),
                    ..Default::default()
                };
                refresh_shape(&snapshot, &mut split_sh);
                let mut split_layer = Layer::new(format!("{src_name} Split"), LayerContent::Shape(split_sh));
                split_layer.blend = src_blend;
                split_layer.opacity = src_opacity;
                split_layer.fill_opacity = src_fill_opacity;
                let rem = group.items.first().map_or(1, |b| b.id);
                Ok((split_layer, rem))
            })();
            match res {
                Ok(pair) => {
                    refresh_shape(&snapshot, &mut sh);
                    src_layer.content = LayerContent::Shape(sh);
                    pair
                }
                Err(e) => {
                    src_layer.content = LayerContent::Shape(sh);
                    return Err(e);
                }
            }
        };
        let new_id = doc.insert_above(Some(id), split_layer);
        *active = Some(new_id);
        Ok(json!({ "layer": id.0, "newLayer": new_id.0, "remainingBalloonId": remaining_bid, "balloonId": 1 }))
    })
}

/// `shape.balloon.tail.add`: adds a tail to a balloon of the layer's group.
fn balloon_tail_add(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "shape.balloon.tail.add";
    let id = layer_id(s, p)?;
    let balloon_id = p.get("balloonId").and_then(Value::as_u64).ok_or_else(|| bad(CMD, "missing `balloonId`"))? as u32;
    let tip = p
        .get("tip")
        .and_then(Value::as_array)
        .and_then(|a| Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?]))
        .ok_or_else(|| bad(CMD, "`tip` must be a finite [x, y] point"))?;
    if !tip.iter().all(|v| v.is_finite() && v.abs() <= 1.0e7) {
        return Err(bad(CMD, "`tip` is out of range"));
    }
    let preset = match p.get("preset") {
        Some(v) => {
            let id = v.as_str().ok_or_else(|| bad(CMD, "`preset` must be a string"))?;
            TailPreset::from_id(id).ok_or_else(|| bad(CMD, format!("unknown tail preset `{id}` (none|wedge|arc|sCurve|lightning|double|thought)")))?
        }
        None => TailPreset::Arc,
    };
    let mouth_width = p.get("mouthWidth").and_then(Value::as_f64).unwrap_or(28.0);
    if !mouth_width.is_finite() || !(0.1..=1.0e4).contains(&mouth_width) {
        return Err(bad(CMD, "`mouthWidth` must be between 0.1 and 10000 px"));
    }
    let bend = p.get("bend").and_then(Value::as_f64).unwrap_or(0.18);
    if !bend.is_finite() {
        return Err(bad(CMD, "`bend` must be finite"));
    }
    let bubbles = p.get("bubbles").and_then(Value::as_u64).unwrap_or(3).clamp(1, 8) as u32;
    let bubble_radius = p.get("bubbleRadius").and_then(Value::as_f64).unwrap_or(10.0);
    if !bubble_radius.is_finite() || !(0.1..=1.0e4).contains(&bubble_radius) {
        return Err(bad(CMD, "`bubbleRadius` must be between 0.1 and 10000 px"));
    }

    with_shape(s, id, "Add Balloon Tail", |sh, _l| {
        let Some(LiveShape::BalloonGroup { ref mut group }) = sh.live else {
            return Err(bad(CMD, "the layer is not a balloon group"));
        };
        let b = group.items.iter_mut().find(|it| it.id == balloon_id).ok_or_else(|| bad(CMD, "the layer has no such balloon"))?;
        if b.tails.len() >= 64 {
            return Err(bad(CMD, "a balloon may have at most 64 tails"));
        }
        let tail_id = b.next_tail_id;
        b.next_tail_id = b.next_tail_id.saturating_add(1);
        let perimeter_t = b.closest_perimeter_t(tip);
        b.tails.push(LiveBalloonTail { id: tail_id, preset, perimeter_t, mouth_width, tip, bend, bubbles, bubble_radius });
        let path = group.rebuild_group_path();
        vector::flatten::validate_shape(&path, sh.stroke.as_ref()).map_err(|e| bad(CMD, e))?;
        sh.path = path;
        Ok(json!({ "tailId": tail_id }))
    })
}

/// `shape.balloon.tail.remove`: removes a tail of a balloon of the layer's group.
fn balloon_tail_remove(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "shape.balloon.tail.remove";
    let id = layer_id(s, p)?;
    let balloon_id = p.get("balloonId").and_then(Value::as_u64).unwrap_or(1) as u32;
    let tail_id = p.get("tailId").and_then(Value::as_u64).ok_or_else(|| bad(CMD, "missing `tailId`"))? as u32;

    with_shape(s, id, "Remove Balloon Tail", |sh, _l| {
        let Some(LiveShape::BalloonGroup { ref mut group }) = sh.live else {
            return Err(bad(CMD, "the layer is not a balloon group"));
        };
        if let Some(b) = group.items.iter_mut().find(|it| it.id == balloon_id) {
            b.tails.retain(|t| t.id != tail_id);
        }
        let path = group.rebuild_group_path();
        vector::flatten::validate_shape(&path, sh.stroke.as_ref()).map_err(|e| bad(CMD, e))?;
        sh.path = path;
        Ok(json!({ "ok": true }))
    })
}

/// `shape.balloon.convertToLinework`: redraws the balloon as SAI Linework strokes on the layer.
fn balloon_to_linework(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "shape.balloon.convertToLinework";
    let id = layer_id(s, p)?;
    let fg = s.tools.foreground;
    let fgc = Color::rgba(fg[0], fg[1], fg[2], fg[3]);
    s.edit("Convert Balloon to Linework", |doc, active| {
        let (value, fill_layer) = {
            let layer = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
            let source_name = layer.name.clone();
            let LayerContent::Shape(sh) = &mut layer.content else {
                return Err(bad(CMD, "layer is not a shape layer"));
            };
            let Some(LiveShape::BalloonGroup { ref group }) = sh.live else {
                return Err(bad(CMD, "the layer is not a balloon group"));
            };
            let mut strokes = Vec::new();
            for b in &group.items {
                strokes.extend(balloon::balloon_to_linework_strokes(b));
            }
            if strokes.is_empty() {
                return Err(bad(CMD, "the balloon produces no linework"));
            }
            // Keep the balloon's white/background interior as a separate lower fill layer before
            // the shape becomes ink-coloured linework; otherwise the whole balloon turns black.
            // Pure `Uniflash + Solid` layers have no interior fill (their fill was the black
            // needles), so they do not get a backing fill layer.
            let all_uniflash_solid = group
                .items
                .iter()
                .all(|b| matches!(b.body, LiveBalloonBody::Uniflash { .. }) && b.line_style == photocraft_doc::vector::BalloonLineStyle::Solid);
            let fill_layer = if all_uniflash_solid {
                None
            } else {
                let render = balloon::rebuild_group_render_paths(group);
                let body_fill_path = if render.fill_only_path.subpaths.is_empty() { sh.path.clone() } else { render.fill_only_path };
                sh.fill.as_ref().map(|fill| {
                    (
                        format!("{source_name} fill"),
                        ShapeLayer { path: body_fill_path, fill: Some(fill.clone()), stroke: None, live: None, ..ShapeLayer::default() },
                    )
                })
            };
            let lw = LiveLinework::new(strokes);
            let path = lw.rebuild_path();
            if path.is_empty() {
                return Err(bad(CMD, "the balloon produces no drawable path"));
            }
            vector::flatten::validate_shape(&path, None).map_err(|e| bad(CMD, e))?;
            sh.path = path;
            sh.fill = Some(Fill::Solid(fgc));
            sh.stroke = None;
            // The balloon is now clean linework, so the live balloon no longer describes it.
            sh.live = None;
            set_live_linework(layer, &lw);
            (json!({ "convertedStrokes": lw.strokes.len() }), fill_layer)
        };
        let (fmt, bounds) = (doc.pixel_format(), doc.bounds());
        if let Some(LayerContent::Shape(sh)) = doc.layer_mut(id).map(|l| &mut l.content) {
            sh.cache = Some(vector::render_shape(sh, fmt, bounds));
        }
        if let Some((name, mut shape)) = fill_layer {
            refresh_shape(doc, &mut shape);
            let layer = Layer::new(name, LayerContent::Shape(shape));
            crate::layer_place::insert_new(doc, Some(id), layer, crate::layer_place::Place::Below);
        }
        *active = Some(id);
        Ok(value)
    })
}

/// `shape.balloon.tail`: union a tail with the balloon body.
fn balloon_tail(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "shape.balloon.tail";
    let id = layer_id(s, p)?;
    let preset_id = match p.get("preset") {
        None => "wedge",
        Some(v) => v.as_str().ok_or_else(|| bad(CMD, "`preset` must be a string (wedge|arc|double|thought)"))?,
    };
    let preset = LegacyTailPreset::from_id(preset_id).ok_or_else(|| bad(CMD, format!("unknown tail preset `{preset_id}` (wedge|arc|double|thought)")))?;
    let tip = point(CMD, p, "tip")?;
    if !finite_point(tip.x, tip.y) {
        return Err(bad(CMD, "`tip` is out of range"));
    }
    let mouth_width = opt_f64(p, "mouthWidth", CMD)?.unwrap_or(18.0);
    if !(0.0..=1.0e4).contains(&mouth_width) {
        return Err(bad(CMD, "`mouthWidth` must be between 0 and 10000 px"));
    }
    let base = match p.get("base") {
        // The mouth is given explicitly.
        Some(v) => point_pair(Some(v)).ok_or_else(|| bad(CMD, "`base` must be [[x, y], [x, y]]"))?,
        // `from` is the drag start on the balloon; without it the mouth sits on the contour point
        // nearest the tip, so a plain `tip` already gives a usable tail.
        None => {
            let from = match p.get("from") {
                Some(_) => {
                    let from = point(CMD, p, "from")?;
                    (from.x, from.y)
                }
                None => nearest_on_body(s, id, tip, CMD)?,
            };
            let tail = BalloonTail::from_drag(preset, from, (tip.x, tip.y), mouth_width);
            if (tail.base[0].0 - tail.base[1].0).abs() < 1.0e-9 && (tail.base[0].1 - tail.base[1].1).abs() < 1.0e-9 {
                return Err(bad(CMD, "`from` and `tip` must not coincide (the tail mouth would be empty)"));
            }
            tail.base
        }
    };
    if !finite_point(base[0].0, base[0].1) || !finite_point(base[1].0, base[1].1) {
        return Err(bad(CMD, "`base` is out of range"));
    }
    let curve = opt_f64(p, "curve", CMD)?.map(|c| c.clamp(0.0, 1.0));
    let bubbles = opt_u32(p, "bubbles", CMD)?.map_or(3, |b| b.clamp(1, 12));
    let bubble_radius = opt_f64(p, "bubbleRadius", CMD)?.unwrap_or(6.0);
    if !(0.5..=200.0).contains(&bubble_radius) {
        return Err(bad(CMD, "`bubbleRadius` must be between 0.5 and 200 px"));
    }
    let mut tail = BalloonTail { preset, base, tip: (tip.x, tip.y), curve: curve.unwrap_or(0.12), bubbles, bubble_radius };
    if let Some(curve) = curve {
        tail.curve = curve;
    }

    with_shape(s, id, "Balloon Tail", |sh, _| {
        let merged = balloon::attach_tail(&sh.path, &tail);
        vector::flatten::validate_shape(&merged, sh.stroke.as_ref()).map_err(|e| bad(CMD, e))?;
        sh.path = merged;
        // A merged outline no longer matches the live-shape parameters that produced the body.
        sh.live = None;
        sh.psd_raw = None;
        Ok(())
    })?;
    shape_info(s, id)
}

/// `shape.balloon.create`: a procedural thought cloud or shout balloon as a new shape layer, or
/// (with a `balloon` payload) a parametric manga balloon.
fn balloon_create(s: &mut Session, p: &Value) -> Result<Value> {
    if p.get("balloon").is_some() {
        return balloon_create_live(s, p);
    }
    const CMD: &str = "shape.balloon.create";
    let kind = p.get("kind").and_then(Value::as_str).unwrap_or("cloud");
    let rect = nums::<4>(p, "rect").ok_or_else(|| bad(CMD, "`rect` must be [x, y, w, h]"))?;
    if rect.iter().any(|v| !v.is_finite() || v.abs() > 1.0e7) || rect[2].abs() < 1.0 || rect[3].abs() < 1.0 {
        return Err(bad(CMD, "`rect` must be finite with a non-zero width and height"));
    }
    let (path, default_name) = match kind {
        "cloud" | "thought" => {
            let params = CloudParams {
                bumps: opt_u32(p, "bumps", CMD)?.map_or(9, |v| v.clamp(3, 64)),
                amplitude: opt_f64(p, "amplitude", CMD)?.map_or(0.18, |v| v.clamp(0.0, 1.0)),
                jitter: opt_f64(p, "jitter", CMD)?.map_or(0.35, |v| v.clamp(0.0, 1.0)),
                seed: opt_u32(p, "seed", CMD)?.unwrap_or(1),
            };
            (balloon::cloud(rect, params), "Thought Cloud")
        }
        "spiky" | "shout" | "flash" => {
            let params = SpikeParams {
                spikes: opt_u32(p, "spikes", CMD)?.map_or(12, |v| v.clamp(3, 128)),
                length: opt_f64(p, "length", CMD)?.map_or(0.45, |v| v.clamp(0.0, 2.0)),
                jitter: opt_f64(p, "jitter", CMD)?.map_or(0.35, |v| v.clamp(0.0, 1.0)),
                inward: opt_bool(p, "inward", CMD)?.unwrap_or(false),
                smooth: opt_bool(p, "smooth", CMD)?.unwrap_or(false),
                seed: opt_u32(p, "seed", CMD)?.unwrap_or(1),
            };
            (balloon::spiky(rect, params), "Shout Balloon")
        }
        "burst" | "explosion" | "star" => {
            let params = BurstParams {
                rays: opt_u32(p, "rays", CMD)?.map_or(14, |v| v.clamp(5, 64)),
                length: opt_f64(p, "length", CMD)?.map_or(0.60, |v| v.clamp(0.10, 1.20)),
                depth: opt_f64(p, "depth", CMD)?.map_or(0.26, |v| v.clamp(0.0, 0.60)),
                jitter: opt_f64(p, "jitter", CMD)?.map_or(0.35, |v| v.clamp(0.0, 1.0)),
                seed: opt_u32(p, "seed", CMD)?.unwrap_or(1),
            };
            (balloon::burst(rect, params), "Burst Balloon")
        }
        "doubleOutline" | "double-outline" | "double" => {
            let squircle = opt_f64(p, "squircle", CMD)?.map_or(0.0, |v| v.clamp(0.0, 1.0));
            let gap = opt_f64(p, "gap", CMD)?.map_or(6.0, |v| v.clamp(0.5, 64.0));
            (balloon::double_outline(rect, squircle, gap), "Double Outline Balloon")
        }
        "whisper" | "dashed" => {
            let dash_count = opt_u32(p, "dashCount", CMD)?.map_or(18, |v| v.clamp(6, 48));
            let dash_duty = opt_f64(p, "dashDuty", CMD)?.map_or(0.58, |v| v.clamp(0.20, 0.85));
            let squircle = opt_f64(p, "squircle", CMD)?.map_or(0.25, |v| v.clamp(0.0, 1.0));
            let seed = opt_u32(p, "seed", CMD)?.unwrap_or(1);
            (balloon::whisper(rect, dash_count, dash_duty, squircle, seed), "Whisper Balloon")
        }
        "chamferRect" | "chamfer-rect" => {
            let chamfer = opt_f64(p, "chamfer", CMD)?.map_or(16.0, |v| v.max(1.0));
            let concave = opt_f64(p, "concave", CMD)?.map_or(0.40, |v| v.clamp(0.0, 1.0));
            (balloon::chamfer_rect(rect, chamfer, concave), "Chamfered Rect Balloon")
        }
        other => {
            return Err(bad(CMD, format!("unknown balloon kind `{other}` (cloud|spiky|burst|doubleOutline|whisper|chamferRect)")));
        }
    };
    let fg = s.tools.foreground;
    let fgc = Color::rgba(fg[0], fg[1], fg[2], fg[3]);
    let fill = match p.get("fill") {
        Some(v) => parse_fill(v).map_err(|e| bad(CMD, e))?,
        None => Some(Fill::Solid(fgc)),
    };
    let stroke = match p.get("stroke") {
        Some(v) => crate::cmd::vector_cmds::params::parse_stroke(v, None, fgc).map_err(|e| bad(CMD, e))?,
        None => None,
    };
    vector::flatten::validate_shape(&path, stroke.as_ref()).map_err(|e| bad(CMD, e))?;

    let name = p.get("name").and_then(Value::as_str).map(str::to_string);
    let mut sh = ShapeLayer { path, fill, stroke, live: None, ..Default::default() };
    let id = s.edit("New Balloon", |doc, active| {
        refresh_shape(doc, &mut sh);
        let name = name.unwrap_or_else(|| doc.next_layer_name(default_name));
        let id = doc.insert_above(*active, Layer::new(name, photocraft_doc::LayerContent::Shape(sh)));
        *active = Some(id);
        Ok(id)
    })?;
    shape_info(s, id)
}

/// `shape.balloon.batchFromScript`: creates a `BalloonGroup` layer of N balloons from a multiline
/// script (`script: "Line 1\nLine 2"` or `lines: ["Line 1", ...]`), sizing each balloon to its
/// line and staggering them down the panel (`TZ 4.3`).
fn balloon_batch_from_script(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "shape.balloon.batchFromScript";
    let lines: Vec<String> = if let Some(arr) = p.get("lines").and_then(Value::as_array) {
        arr.iter().filter_map(|v| v.as_str().map(str::trim)).filter(|l| !l.is_empty()).map(str::to_string).collect()
    } else if let Some(raw) = p.get("script").and_then(Value::as_str) {
        raw.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
    } else {
        return Err(bad(CMD, "provide `lines` ([str, ...]) or `script` (multiline string)"));
    };
    if lines.is_empty() || lines.len() > 64 {
        return Err(bad(CMD, "script must contain between 1 and 64 non-empty lines"));
    }

    let size_pt = opt_f64(p, "sizePt", CMD)?.unwrap_or(14.0).clamp(4.0, 72.0) as f32;
    let vertical = opt_bool(p, "vertical", CMD)?.unwrap_or(false);
    let direction = if vertical { TextDirection::Vertical } else { TextDirection::Horizontal };
    let (doc_w, doc_h, dpi) = s.active().map(|a| (a.doc.size.width as f64, a.doc.size.height as f64, a.doc.resolution_dpi)).ok_or(EngineError::NoDocument)?;

    let mut items = Vec::with_capacity(lines.len());
    let mut cur_y = (doc_h * 0.08).max(24.0);
    for (idx, line) in lines.iter().enumerate() {
        let [w, h] = body_size_for_text_oriented(line, size_pt, dpi, 0.72, direction);
        let w = w.clamp(64.0, (doc_w * 0.85).max(80.0));
        let h = h.clamp(48.0, (doc_h * 0.65).max(64.0));
        let x = if idx.is_multiple_of(2) {
            (doc_w * 0.68 - w * 0.5).clamp(16.0, (doc_w - w - 16.0).max(16.0))
        } else {
            (doc_w * 0.32 - w * 0.5).clamp(16.0, (doc_w - w - 16.0).max(16.0))
        };
        let rect = [x, cur_y, w, h];
        cur_y += h + 28.0;

        let id = (idx as u32) + 1;
        let is_shout = line.ends_with("!!") || line.ends_with("!?");
        let body = if is_shout {
            LiveBalloonBody::Spiky { rect, spikes: 18, length: 0.36, sub_harmonic: 0.45, concavity: 0.65, jitter: 0.30, smooth: false, seed: 100 + id }
        } else {
            LiveBalloonBody::Ellipse { rect, squircle: 0.25 }
        };
        items.push(LiveBalloon {
            id,
            body,
            tails: vec![LiveBalloonTail {
                id: 1,
                preset: TailPreset::Arc,
                perimeter_t: 0.56,
                mouth_width: 24.0,
                tip: [x + w * 0.35, rect[1] + h + 36.0],
                bend: 0.16,
                bubbles: 3,
                bubble_radius: 10.0,
            }],
            next_tail_id: 2,
            voice: if is_shout { BalloonVoice::Shout } else { BalloonVoice::Neutral },
            text_direction: direction,
            auto_merge_overlapping: false,
            ..LiveBalloon::default()
        });
    }

    let group = LiveBalloonGroup { next_balloon_id: (items.len() as u32) + 1, items };
    let path = group.rebuild_group_path();
    vector::flatten::validate_shape(&path, None).map_err(|e| bad(CMD, e))?;

    let mut sh = ShapeLayer {
        path,
        fill: Some(Fill::Solid(Color::WHITE)),
        stroke: Some(ShapeStroke { width: 2.5, paint: Fill::Solid(Color::BLACK), ..Default::default() }),
        live: Some(LiveShape::BalloonGroup { group }),
        ..Default::default()
    };
    let count = lines.len();
    let id = s.edit("Batch Balloons from Script", |doc, active| {
        refresh_shape(doc, &mut sh);
        let name = doc.next_layer_name("Script Balloons");
        let id = doc.insert_above(*active, Layer::new(name, LayerContent::Shape(sh)));
        *active = Some(id);
        Ok(id)
    })?;
    Ok(json!({ "layer": id.0, "createdBalloons": count }))
}

/// `shape.balloon.importSvg`: imports and validates a simple closed SVG contour onto a balloon of
/// the active layer (`TZ 4.3`).
fn balloon_import_svg(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "shape.balloon.importSvg";
    let id = layer_id(s, p)?;
    let balloon_id = p.get("balloonId").and_then(Value::as_u64).unwrap_or(1) as u32;
    let svg = p.get("svg").or_else(|| p.get("d")).and_then(Value::as_str).ok_or_else(|| bad(CMD, "missing `svg` or `d` string"))?;
    if svg.len() > 262_144 {
        return Err(bad(CMD, "SVG contour exceeds 256 KiB limit"));
    }

    with_shape(s, id, "Import Balloon SVG Contour", |sh, _l| {
        let Some(LiveShape::BalloonGroup { ref mut group }) = sh.live else {
            return Err(bad(CMD, "the layer is not a balloon group"));
        };
        let b = group.items.iter_mut().find(|it| it.id == balloon_id).ok_or_else(|| bad(CMD, "the layer has no such balloon"))?;
        let validated = balloon::parse_simple_svg_contour(svg, b.body.rect()).map_err(|e| bad(CMD, e))?;
        let knots = validated.subpaths.first().map_or(0, |sp| sp.knots.len());
        b.custom_svg_contour = Some(svg.to_string());
        let path = group.rebuild_group_path();
        vector::flatten::validate_shape(&path, sh.stroke.as_ref()).map_err(|e| bad(CMD, e))?;
        sh.path = path;
        Ok(json!({ "ok": true, "importedKnots": knots }))
    })
}

/// `shape.balloon.exportSvg`: exports the active balloon to a standalone SVG string (`TZ 4.3`).
fn balloon_export_svg(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "shape.balloon.exportSvg";
    let id = layer_id(s, p)?;
    let balloon_id = p.get("balloonId").and_then(Value::as_u64).unwrap_or(1) as u32;
    let text = p.get("text").and_then(Value::as_str).unwrap_or("");
    let size_pt = opt_f64(p, "sizePt", CMD)?.unwrap_or(14.0).clamp(4.0, 72.0) as f32;

    let doc_state = s.active().ok_or(EngineError::NoDocument)?;
    let dpi = doc_state.doc.resolution_dpi;
    let layer = doc_state.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
    let LayerContent::Shape(sh) = &layer.content else {
        return Err(bad(CMD, "the layer is not a shape layer"));
    };
    let Some(LiveShape::BalloonGroup { ref group }) = sh.live else {
        return Err(bad(CMD, "the layer is not a balloon group"));
    };
    let b = group.items.iter().find(|it| it.id == balloon_id).or_else(|| group.items.first()).ok_or_else(|| bad(CMD, "no balloon found"))?;

    let svg = if text.trim().is_empty() {
        balloon::export_balloon_svg(b, None, size_pt)
    } else {
        let safe = b.safe_text_rect();
        let fit = fit_text_directed(text, safe[2] as f32, safe[3] as f32, size_pt, dpi, b.text_direction);
        balloon::export_balloon_svg(b, Some(&fit.lines), fit.size_pt)
    };
    Ok(json!({ "svg": svg }))
}

/// One click-placed node of `shape.balloon.stroke`.
fn sai_point(v: &Value) -> Option<SaiPoint> {
    let (x, y, pressure, corner) = if let Some(a) = v.as_array() {
        let x = a.first().and_then(Value::as_f64)?;
        let y = a.get(1).and_then(Value::as_f64)?;
        (x, y, 1.0f64, a.get(2).is_some_and(|c| c.as_bool().unwrap_or(false)))
    } else {
        let x = v.get("x").and_then(Value::as_f64)?;
        let y = v.get("y").and_then(Value::as_f64)?;
        let pressure = v.get("pressure").and_then(Value::as_f64).unwrap_or(1.0);
        let corner = v.get("corner").and_then(Value::as_bool).unwrap_or(false);
        (x, y, pressure, corner)
    };
    if !finite_point(x, y) || !pressure.is_finite() {
        return None;
    }
    Some(SaiPoint { p: (x, y), pressure: pressure.clamp(0.0, 1.0) as f32, density: 1.0, corner })
}

/// `shape.balloon.stroke`: Catmull-Rom smoothed click linework, tapered by point pressure.
fn balloon_stroke(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "shape.balloon.stroke";
    let raw = p.get("points").and_then(Value::as_array).ok_or_else(|| bad(CMD, "`points` must be an array"))?;
    if raw.len() < 2 || raw.len() > 4096 {
        return Err(bad(CMD, "`points` must contain 2..=4096 nodes"));
    }
    let mut points = Vec::with_capacity(raw.len());
    for item in raw {
        points.push(sai_point(item).ok_or_else(|| bad(CMD, "every point needs finite `x`, `y` and `pressure`"))?);
    }
    let width = opt_f64(p, "width", CMD)?.unwrap_or(6.0);
    if !(0.01..=1.0e5).contains(&width) {
        return Err(bad(CMD, "`width` must be between 0.01 and 100000 px"));
    }
    let min_ratio = opt_f64(p, "minRatio", CMD)?.unwrap_or(0.05).clamp(0.0, 1.0);
    let tapered = opt_bool(p, "tapered", CMD)?.unwrap_or(true);
    let fg = s.tools.foreground;
    let fgc = Color::rgba(fg[0], fg[1], fg[2], fg[3]);
    let path = if tapered { balloon::tapered_path(&points, width, min_ratio) } else { balloon::sai_stroke(&points, false) };
    if path.is_empty() {
        return Err(bad(CMD, "the points produce no drawable path"));
    }
    let (fill, stroke) = if tapered {
        let fill = match p.get("fill") {
            Some(v) => parse_fill(v).map_err(|e| bad(CMD, e))?,
            None => Some(Fill::Solid(fgc)),
        };
        (fill, None)
    } else {
        let base = photocraft_doc::ShapeStroke { width: width as f32, paint: Fill::Solid(fgc), ..Default::default() };
        let stroke = match p.get("stroke") {
            Some(v) => crate::cmd::vector_cmds::params::parse_stroke(v, Some(base), fgc).map_err(|e| bad(CMD, e))?,
            None => Some(base),
        };
        (None, stroke)
    };
    vector::flatten::validate_shape(&path, stroke.as_ref()).map_err(|e| bad(CMD, e))?;
    let name = p.get("name").and_then(Value::as_str).unwrap_or("Vector Stroke").to_string();
    let mut sh = ShapeLayer { path, fill, stroke, live: None, ..Default::default() };
    let id = s.edit("Vector Stroke", |doc, active| {
        refresh_shape(doc, &mut sh);
        let id = doc.insert_above(*active, Layer::new(name, photocraft_doc::LayerContent::Shape(sh)));
        *active = Some(id);
        Ok(id)
    })?;
    shape_info(s, id)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            "shape.balloon.tail",
            "Add Balloon Tail",
            [],
            r##"{{"layer":id?,"preset":"wedge|arc|double|thought"="wedge","tip":[x,y] (required),"base":[[x,y],[x,y]]? or "from":[x,y] + "mouthWidth":px=18 (the drag points),"curve":0..1?,"bubbles":1..12=3,"bubbleRadius":px=6 (thought)}} -> shape.info. The tail and body are unioned into ONE contour: one fill, one stroke, no seam. {fill} stays as the layer's; the layer must be a shape layer."##,
            has_shape,
            balloon_tail
        ),
        spec!(
            "shape.balloon.create",
            "New Balloon",
            [],
            r##"{{"kind":"cloud|spiky|burst|doubleOutline|whisper|chamferRect"="cloud","rect":[x,y,w,h] (required),"bumps":3..64=9,"amplitude":0..1=0.18,"jitter":0..1=0.35,"spikes":3..128=12,"length":0..2=0.45,"inward":bool=false,"smooth":bool=false,"rays":5..64=14,"depth":0..0.6=0.26,"squircle":0..1=0,"gap":0.5..64=6,"seed":int=1,{fill}=foreground,{stroke}=none,"name":str?}} -> shape.info. With "balloon" instead: {balloon:{id,body,line_style,tails,...}} = a parametric manga balloon."##,
            has_doc,
            balloon_create
        ),
        spec!(
            "shape.balloon.update",
            "Update Balloon",
            [],
            r##"{{"layer":id (required),"balloon":{id,body,tails,...} (required),"coalesce":str?}} -> {ok:true}. Replaces the balloon with the given id in the layer's group (or adds it), rebuilds the outline and syncs linked TypeR/Stories text boxes. A drag sends the same "coalesce" key every frame, so the whole gesture is one history step."##,
            has_doc,
            balloon_update
        ),
        spec!(
            "shape.balloon.add",
            "Add Balloon Chamber",
            [],
            r##"{{"layer":id (required),"balloon":{body,tails,...} (required)}} -> {balloonId}. Appends a chamber to an existing BalloonGroup shape layer."##,
            has_shape,
            balloon_add
        ),
        spec!(
            "shape.balloon.merge",
            "Merge Balloons",
            [],
            r##"{{"layers":[id1,id2,...] (2..=64 required)}} -> {layer,balloonCount}. Merges multiple BalloonGroup layers into one group while preserving linked text layers and wiring Stories links."##,
            has_doc,
            balloon_merge
        ),
        spec!(
            "shape.balloon.split",
            "Split Balloon Chamber",
            [],
            r##"{{"layer":id (required),"balloonId":int?}} -> {layer,newLayer,balloonId}. Detaches a chamber from a multi-chamber balloon layer into a new shape layer while preserving its linked text layer."##,
            has_shape,
            balloon_split
        ),
        spec!(
            "shape.balloon.tail.add",
            "Add Balloon Tail",
            [],
            r##"{{"layer":id (required),"balloonId":int=1,"tip":[x,y] (required),"preset":"none|wedge|arc|sCurve|lightning|double|thought"="arc","mouthWidth":px=28,"bend":-1..1=0.18,"bubbles":1..8=3,"bubbleRadius":px=10}} -> {tailId}. The mouth sits on the body's perimeter nearest the tip."##,
            has_doc,
            balloon_tail_add
        ),
        spec!(
            "shape.balloon.tail.remove",
            "Remove Balloon Tail",
            [],
            r##"{{"layer":id (required),"balloonId":int=1,"tailId":int (required)}} -> {ok:true}"##,
            has_doc,
            balloon_tail_remove
        ),
        spec!(
            "shape.balloon.convertToLinework",
            "Convert Balloon to Linework",
            [],
            r##"{{"layer":id (required)}} -> {convertedStrokes}. The balloon is redrawn as SAI Linework strokes on the same layer."##,
            has_doc,
            balloon_to_linework
        ),
        spec!(
            "shape.balloon.batchFromScript",
            "Batch Balloons from Script",
            [],
            r##"{{"script":str? or "lines":[str,...],"sizePt":4..72=14,"vertical":bool=false}} -> {layer,createdBalloons}. Creates a BalloonGroup layer with one sized balloon per line of script."##,
            has_doc,
            balloon_batch_from_script
        ),
        spec!(
            "shape.balloon.importSvg",
            "Import Balloon SVG Contour",
            [],
            r##"{{"layer":id?,"balloonId":int=1,"svg":str (required)}} -> {ok:true,importedKnots}. Validates and attaches a simple closed SVG contour to the balloon."##,
            has_shape,
            balloon_import_svg
        ),
        spec!(
            "shape.balloon.exportSvg",
            "Export Balloon SVG",
            [],
            r##"{{"layer":id?,"balloonId":int=1,"text":str?,"sizePt":4..72=14}} -> {svg:str}. Exports the balloon contour (and optional fitted dialogue) as standalone SVG."##,
            has_shape,
            balloon_export_svg
        ),
        spec!(
            "shape.balloon.stroke",
            "New Vector Stroke",
            [],
            r##"{{"points":[{{"x":px,"y":px,"pressure":0..1=1,"corner":bool=false}} | [x,y,corner?]] (2..4096, click nodes smoothed with a Catmull-Rom spline),"width":px=6,"minRatio":0..1=0.05 (taper floor),"tapered":bool=true (filled outline, weight per node) | false (uniform stroke path),"name":str?}} -> shape.info"##,
            has_doc,
            balloon_stroke
        ),
        CommandSpec {
            id: "shape.balloon.presets",
            label: "Balloon Tail Presets",
            menu: &[],
            shortcut: None,
            params: r##"{} -> {presets:[{id,label,mouth}]}"##,
            enabled: |_: &Session| Ok(()),
            run: |_, _| {
                let names = [
                    (LegacyTailPreset::Wedge, "Wedge", "straight tapered wedge"),
                    (LegacyTailPreset::Arc, "Arc", "bow-sided tail"),
                    (LegacyTailPreset::Double, "Double", "two tails, one mouth"),
                    (LegacyTailPreset::Thought, "Thought", "shrinking bubbles"),
                ];
                Ok(
                    json!({"presets": names.iter().map(|(p, label, about)| json!({"id": p.id(), "label": label, "mouth": p.mouth(), "about": about})).collect::<Vec<_>>()}),
                )
            },
            journal: false,
        },
    ]
}

#[cfg(test)]
mod tests;
