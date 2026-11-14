//! Balloon commands: comic tails merged with a shape layer, procedural thought/shout balloons and
//! SAI-style click linework.
//!
//! All geometry comes from `photocraft_vector::balloon` and lands in an ordinary
//! [`ShapeLayer`](photocraft_doc::ShapeLayer) path, so Free Transform, history, PSD export and the
//! rest of the shape tooling keep working unchanged.

use photocraft_doc::vector::{LiveBalloon, LiveBalloonBody, LiveBalloonGroup, LiveBalloonTail, TailPreset};
use photocraft_doc::{Color, Fill, Layer, LayerContent, LayerId, LiveShape, ShapeLayer, ShapeStroke};
use photocraft_geom::Point;
use photocraft_vector as vector;
use photocraft_vector::balloon::{self, BalloonTail, BurstParams, CloudParams, LegacyTailPreset, LiveBalloonGeom, LiveBalloonGroupGeom, SaiPoint, SpikeParams};
use photocraft_vector::sai_curve::LiveLinework;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::vector_cmds::{bad, has_doc, layer_id, nums, parse_fill, pt, refresh_shape, set_live_linework, shape_info, with_shape};
use crate::{EngineError, Result, Session};

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
    if !b.hand_drawn_noise.is_finite() {
        return Err("the balloon's noise must be finite".into());
    }
    if !(0.0..=1.0).contains(&b.hand_drawn_noise) {
        return Err("the balloon's noise must be between 0.0 and 1.0".into());
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
            if *needles > 400 {
                return Err("the uniflash may have at most 400 needles".into());
            }
            if !finite(*outer_sharpness) || !finite(*inner_sharpness) || !(0.0..=1.0).contains(outer_sharpness) || !(0.0..=1.0).contains(inner_sharpness) {
                return Err("the uniflash's outer and inner sharpness must be between 0.0 and 1.0".into());
            }
        }
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
        Some(v) => crate::vector_cmds::parse_stroke(v, Some(base), fgc).map_err(|e| bad(CMD, e))?,
        None => Some(base),
    };

    let name = p.get("name").and_then(Value::as_str).map(str::to_string);
    let mut sh = ShapeLayer { path, fill, stroke, live: Some(LiveShape::BalloonGroup { group }), ..Default::default() };
    let id = s.edit("New Balloon", |doc, active| {
        refresh_shape(doc, &mut sh);
        let name = name.unwrap_or_else(|| doc.next_layer_name("Speech Balloon"));
        let id = doc.insert_new(*active, Layer::new(name, LayerContent::Shape(sh)));
        *active = Some(id);
        Ok(id)
    })?;
    shape_info(s, id)
}

/// `shape.balloon.update`: replaces one balloon of the layer's group and rebuilds the outline.
fn balloon_update(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "shape.balloon.update";
    let id = layer_id(s, p)?;
    let updated = parse_balloon(CMD, p)?;
    with_shape(s, id, "Update Balloon", |sh, _l| {
        let Some(LiveShape::BalloonGroup { ref mut group }) = sh.live else {
            return Err(bad(CMD, "the layer is not a balloon group"));
        };
        match group.items.iter_mut().find(|it| it.id == updated.id) {
            Some(slot) => *slot = updated,
            None => group.items.push(updated),
        }
        let path = group.rebuild_group_path();
        vector::flatten::validate_shape(&path, sh.stroke.as_ref()).map_err(|e| bad(CMD, e))?;
        sh.path = path;
        Ok(json!({ "ok": true }))
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

    with_shape(s, id, "Convert Balloon to Linework", |sh, layer| {
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
        Ok(json!({ "convertedStrokes": lw.strokes.len() }))
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
        other => return Err(bad(CMD, format!("unknown balloon kind `{other}` (cloud|spiky|burst|doubleOutline)"))),
    };
    let fg = s.tools.foreground;
    let fgc = Color::rgba(fg[0], fg[1], fg[2], fg[3]);
    let fill = match p.get("fill") {
        Some(v) => parse_fill(v).map_err(|e| bad(CMD, e))?,
        None => Some(Fill::Solid(fgc)),
    };
    let stroke = match p.get("stroke") {
        Some(v) => crate::vector_cmds::parse_stroke(v, None, fgc).map_err(|e| bad(CMD, e))?,
        None => None,
    };
    vector::flatten::validate_shape(&path, stroke.as_ref()).map_err(|e| bad(CMD, e))?;

    let name = p.get("name").and_then(Value::as_str).map(str::to_string);
    let mut sh = ShapeLayer { path, fill, stroke, live: None, ..Default::default() };
    let id = s.edit("New Balloon", |doc, active| {
        refresh_shape(doc, &mut sh);
        let name = name.unwrap_or_else(|| doc.next_layer_name(default_name));
        let id = doc.insert_new(*active, Layer::new(name, photocraft_doc::LayerContent::Shape(sh)));
        *active = Some(id);
        Ok(id)
    })?;
    shape_info(s, id)
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
            Some(v) => crate::vector_cmds::parse_stroke(v, Some(base), fgc).map_err(|e| bad(CMD, e))?,
            None => Some(base),
        };
        (None, stroke)
    };
    vector::flatten::validate_shape(&path, stroke.as_ref()).map_err(|e| bad(CMD, e))?;
    let name = p.get("name").and_then(Value::as_str).unwrap_or("Vector Stroke").to_string();
    let mut sh = ShapeLayer { path, fill, stroke, live: None, ..Default::default() };
    let id = s.edit("Vector Stroke", |doc, active| {
        refresh_shape(doc, &mut sh);
        let id = doc.insert_new(*active, Layer::new(name, photocraft_doc::LayerContent::Shape(sh)));
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
            r##"{{"kind":"cloud|spiky|burst|doubleOutline"="cloud","rect":[x,y,w,h] (required),"bumps":3..64=9,"amplitude":0..1=0.18,"jitter":0..1=0.35,"spikes":3..128=12,"length":0..2=0.45,"inward":bool=false,"smooth":bool=false,"rays":5..64=14,"depth":0..0.6=0.26,"squircle":0..1=0,"gap":0.5..64=6,"seed":int=1,{fill}=foreground,{stroke}=none,"name":str?}} -> shape.info. cloud = scalloped thought balloon; spiky = shout/flash balloon; burst = straight-edged explosion star; doubleOutline = inner line plus an outer echo with transparent gap. With "balloon" instead: {balloon:{id,body,tails,...}} = a parametric manga balloon (ellipse|rect|cloud|spiky|burst|doubleOutline|uniflash body, none|wedge|arc|sCurve|lightning|double|thought tails)."##,
            has_doc,
            balloon_create
        ),
        spec!(
            "shape.balloon.update",
            "Update Balloon",
            [],
            r##"{{"layer":id (required),"balloon":{id,body,tails,...} (required),"coalesce":str?}} -> {ok:true}. Replaces the balloon with the given id in the layer's group (or adds it) and rebuilds the outline. A drag sends the same "coalesce" key every frame, so the whole gesture is one history step."##,
            has_doc,
            balloon_update
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
