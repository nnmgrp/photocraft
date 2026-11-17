//! SAI-style vector curve & multi-stroke linework commands:
//! `shape.vectorCurve.{create,updateNode,insertNode}` and `layer.newLinework`.

use std::sync::Arc;

use photocraft_color::Color;
use photocraft_doc::{Fill, Layer, LayerContent, ShapeLayer, ShapeStroke};
use photocraft_vector as vector;
use serde_json::{Value, json};
use vector::sai_curve::{LiveLinework, LiveVectorStroke, SAI_DEFAULT_TENSION, SaiCurveType, SaiPoint};

use crate::cmd::vector_cmds::params::{layer_id, parse_fill, parse_stroke};
use crate::cmd::vector_cmds::shape::{refresh_shape, with_shape};
use crate::cmd::vector_cmds::{bad, has_doc};
use crate::commands::CommandSpec;
use crate::{Result, Session};

pub const SAI_CURVE_BLOCK: [u8; 4] = *b"saiC";
pub const SAI_LINEWORK_BLOCK: [u8; 4] = *b"saiL";

fn has_layer(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    d.active_layer.filter(|id| d.doc.layer(*id).is_some()).map(|_| ()).ok_or_else(|| "no active layer".into())
}

/// Retrieves `LiveVectorStroke` parametric data preserved on a layer's PSD blocks if present.
pub fn get_live_vector_stroke(layer: &Layer) -> Option<LiveVectorStroke> {
    for (k, data) in &layer.psd_blocks {
        if *k == SAI_CURVE_BLOCK
            && let Ok(stroke) = serde_json::from_slice::<LiveVectorStroke>(data)
        {
            return Some(stroke);
        }
    }
    if let Some(lw) = get_live_linework(layer) {
        return lw.strokes.last().cloned();
    }
    None
}

/// Retrieves the multi-stroke `LiveLinework` container preserved on a Linework layer's PSD blocks.
pub fn get_live_linework(layer: &Layer) -> Option<LiveLinework> {
    for (k, data) in &layer.psd_blocks {
        if *k == SAI_LINEWORK_BLOCK
            && let Ok(lw) = serde_json::from_slice::<LiveLinework>(data)
        {
            return Some(lw);
        }
    }
    for (k, data) in &layer.psd_blocks {
        if *k == SAI_CURVE_BLOCK
            && let Ok(stroke) = serde_json::from_slice::<LiveVectorStroke>(data)
        {
            return Some(LiveLinework::new(vec![stroke]));
        }
    }
    None
}

/// Stores or updates `LiveVectorStroke` on a layer's PSD blocks.
pub fn set_live_vector_stroke(layer: &mut Layer, live: &LiveVectorStroke) {
    if let Ok(bytes) = serde_json::to_vec(live) {
        let arc = Arc::new(bytes);
        if let Some(entry) = layer.psd_blocks.iter_mut().find(|(k, _)| *k == SAI_CURVE_BLOCK) {
            entry.1 = arc;
        } else {
            layer.psd_blocks.push((SAI_CURVE_BLOCK, arc));
        }
    }
}

/// Stores or updates a multi-stroke `LiveLinework` container on a layer's PSD blocks,
/// keeping `SAI_CURVE_BLOCK` synchronized with the latest stroke.
pub fn set_live_linework(layer: &mut Layer, linework: &LiveLinework) {
    if let Ok(bytes) = serde_json::to_vec(linework) {
        let arc = Arc::new(bytes);
        if let Some(entry) = layer.psd_blocks.iter_mut().find(|(k, _)| *k == SAI_LINEWORK_BLOCK) {
            entry.1 = arc;
        } else {
            layer.psd_blocks.push((SAI_LINEWORK_BLOCK, arc));
        }
    }
    if let Some(last) = linework.strokes.last() {
        set_live_vector_stroke(layer, last);
    }
}

/// Parses a single `SaiPoint` from JSON (`[x, y, pressure?, corner?]` or `{"x", "y", "pressure", "corner"}`).
pub fn parse_sai_point(v: &Value) -> Option<SaiPoint> {
    let (x, y, pressure, density, corner) = if let Some(a) = v.as_array() {
        let x = a.first().and_then(Value::as_f64)?;
        let y = a.get(1).and_then(Value::as_f64)?;
        let pressure = a.get(2).and_then(Value::as_f64).unwrap_or(1.0);
        let corner = a.get(3).and_then(Value::as_bool).or_else(|| a.get(2).and_then(Value::as_bool)).unwrap_or(false);
        (x, y, pressure, 1.0, corner)
    } else {
        let x = v.get("x").and_then(Value::as_f64)?;
        let y = v.get("y").and_then(Value::as_f64)?;
        let pressure = v.get("pressure").and_then(Value::as_f64).unwrap_or(1.0);
        let density = v.get("density").and_then(Value::as_f64).unwrap_or(1.0);
        let corner = v.get("corner").and_then(Value::as_bool).unwrap_or(false);
        (x, y, pressure, density, corner)
    };
    if !x.is_finite() || !y.is_finite() || !pressure.is_finite() || !density.is_finite() {
        return None;
    }
    Some(SaiPoint::with_dynamics((x, y), pressure as f32, density as f32, corner))
}

/// Validates and parses an array of `SaiPoint` nodes from command JSON parameters.
pub fn parse_sai_points(p: &Value, cmd: &str) -> std::result::Result<Vec<SaiPoint>, String> {
    let raw = p.get("points").and_then(Value::as_array).ok_or_else(|| format!("{cmd}: `points` must be an array"))?;
    if !(2..=4096).contains(&raw.len()) {
        return Err(format!("{cmd}: `points` must contain 2..=4096 nodes"));
    }
    let mut points = Vec::with_capacity(raw.len());
    for item in raw {
        points.push(parse_sai_point(item).ok_or_else(|| format!("{cmd}: every point needs finite `x`, `y` and `pressure`"))?);
    }
    Ok(points)
}

/// Result of building a live vector curve layer in the engine.
#[derive(Clone, Debug)]
pub struct VectorCurveCommandOutput {
    pub name: String,
    pub path: photocraft_doc::Path,
    pub live_stroke: LiveVectorStroke,
}

/// Core command handler for `shape.vectorCurve.create`.
pub fn build_vector_curve_layer(p: &Value) -> std::result::Result<VectorCurveCommandOutput, String> {
    const CMD: &str = "shape.vectorCurve.create";
    let mut points = parse_sai_points(p, CMD)?;

    let width = p.get("width").and_then(Value::as_f64).unwrap_or(6.0);
    if !(0.01..=1.0e5).contains(&width) || !width.is_finite() {
        return Err(format!("{CMD}: `width` must be between 0.01 and 100000 px"));
    }

    let min_ratio = p.get("minRatio").and_then(Value::as_f64).unwrap_or(0.05).clamp(0.0, 1.0);
    let base_density = p.get("baseDensity").and_then(Value::as_f64).unwrap_or(1.0).clamp(0.0, 2.0) as f32;
    let min_density_ratio = p.get("minDensityRatio").and_then(Value::as_f64).unwrap_or(0.0).clamp(0.0, 1.0) as f32;
    let curve_type = match p.get("curveType").and_then(Value::as_str) {
        Some("bezier" | "Bezier") => SaiCurveType::Bezier,
        _ => SaiCurveType::Normal,
    };
    if p.get("pressureTarget").and_then(Value::as_str) == Some("density") {
        for pt in &mut points {
            pt.density = pt.pressure;
            pt.pressure = 1.0;
        }
    }
    let tapered = p.get("tapered").and_then(Value::as_bool).unwrap_or(true);
    let closed = p.get("closed").and_then(Value::as_bool).unwrap_or(false);
    let tension = p.get("tension").and_then(Value::as_f64).unwrap_or(SAI_DEFAULT_TENSION).clamp(0.0, 1.0);

    let live_stroke = LiveVectorStroke { nodes: points, base_width: width, min_ratio, base_density, min_density_ratio, curve_type, tapered, closed, tension };

    let path = live_stroke.rebuild_path();
    if path.is_empty() {
        return Err(format!("{CMD}: the points produce no drawable path"));
    }

    let name = p.get("name").and_then(Value::as_str).unwrap_or("Linework").to_string();

    Ok(VectorCurveCommandOutput { name, path, live_stroke })
}

/// Non-destructively updates a node's pressure, position, or corner state on an existing
/// `LiveVectorStroke` and regenerates the smooth `Path`.
pub fn update_vector_curve_node(
    live: &mut LiveVectorStroke,
    node_index: usize,
    new_pos: Option<(f64, f64)>,
    new_pressure: Option<f32>,
    toggle_corner: bool,
) -> std::result::Result<photocraft_doc::Path, String> {
    let node = live.nodes.get_mut(node_index).ok_or_else(|| format!("shape.vectorCurve.updateNode: node index {node_index} out of bounds"))?;

    if let Some((x, y)) = new_pos
        && x.is_finite()
        && y.is_finite()
    {
        node.p = (x, y);
    }
    if let Some(pressure) = new_pressure
        && pressure.is_finite()
    {
        node.pressure = pressure.clamp(0.0, 2.0);
    }
    if toggle_corner {
        node.corner = !node.corner;
    }

    let path = live.rebuild_path();
    if path.is_empty() {
        return Err("shape.vectorCurve.updateNode: updated parameters produced an empty path".into());
    }
    Ok(path)
}

/// Projects `(x, y)` onto the closest cubic Bezier segment of `live`, inserts a new `SaiPoint`
/// with interpolated pressure `P(t)`, and rebuilds the `Path`.
pub fn insert_vector_curve_node_at(
    live: &mut LiveVectorStroke,
    x: f64,
    y: f64,
    max_dist_px: f64,
) -> std::result::Result<(usize, photocraft_doc::Path), String> {
    let idx = live.insert_node_at_pos((x, y), max_dist_px).ok_or_else(|| "shape.vectorCurve.insertNode: no curve segment within hit radius".to_string())?;
    let path = live.rebuild_path();
    Ok((idx, path))
}

/// Serializes `LiveVectorStroke` into the engine's JSON response format.
pub fn live_stroke_to_json(id: u64, name: &str, live: &LiveVectorStroke) -> Value {
    let nodes: Vec<Value> = live
        .nodes
        .iter()
        .map(|pt| {
            json!({
                "x": pt.p.0,
                "y": pt.p.1,
                "pressure": pt.pressure,
                "corner": pt.corner,
            })
        })
        .collect();

    json!({
        "id": id,
        "name": name,
        "command": "shape.vectorCurve.create",
        "baseWidth": live.base_width,
        "minRatio": live.min_ratio,
        "tapered": live.tapered,
        "closed": live.closed,
        "tension": live.tension,
        "nodes": nodes,
    })
}

/// `layer.newLinework`: an empty SAI Linework layer, ready for the Vector Curve tool.
fn layer_new_linework(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layer.newLinework";
    let fg = s.tools.foreground;
    let fgc = Color::rgba(fg[0], fg[1], fg[2], fg[3]);
    let fill = match p.get("fill") {
        Some(v) => parse_fill(v).map_err(|e| bad(CMD, e))?,
        None => Some(Fill::Solid(fgc)),
    };
    let name = p.get("name").and_then(Value::as_str).unwrap_or("Linework").to_string();
    let mut sh = ShapeLayer { fill, ..Default::default() };
    let id = s.edit("New Linework Layer", |doc, active| {
        refresh_shape(doc, &mut sh);
        let name = doc.next_layer_name(&name);
        let mut layer = Layer::new(name, LayerContent::Shape(sh));
        // The layer starts empty, but it already carries the Linework container, so the Vector
        // Curve tool appends its first stroke to it.
        set_live_linework(&mut layer, &LiveLinework::new(Vec::new()));
        let id = doc.insert_above(*active, layer);
        *active = Some(id);
        Ok(id)
    })?;
    Ok(json!({ "layer": id.0 }))
}

fn effective_stroke_fg(s: &Session, stroke: &LiveVectorStroke) -> Color {
    let fg = s.tools.foreground;
    let avg_density = if stroke.nodes.is_empty() { 1.0 } else { stroke.nodes.iter().map(|n| n.density).sum::<f32>() / stroke.nodes.len() as f32 };
    let effective_alpha = vector::sai_curve::alpha_at(stroke.base_density, stroke.min_density_ratio, avg_density);
    Color::rgba(fg[0], fg[1], fg[2], (fg[3] * effective_alpha).clamp(0.0, 1.0))
}

fn vector_curve_create(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "shape.vectorCurve.create";
    let output = build_vector_curve_layer(p).map_err(|e| bad(CMD, e))?;
    let append_to_active = p.get("appendToActive").and_then(Value::as_bool).unwrap_or(false);

    // If `appendToActive` is true and the currently active layer is already a SAI Linework layer,
    // append the stroke directly to that Linework layer instead of spawning a new layer.
    if append_to_active
        && let Some(active_id) = s.active().and_then(|st| st.active_layer)
        && let Some(existing_lw) = s.active().and_then(|st| st.doc.layer(active_id)).and_then(get_live_linework)
    {
        let mut lw = existing_lw;
        lw.strokes.push(output.live_stroke.clone());
        let combined_path = lw.rebuild_path();
        if !combined_path.is_empty() {
            let fgc = effective_stroke_fg(s, &output.live_stroke);
            with_shape(s, active_id, "Add Linework Stroke", |sh, layer| {
                vector::flatten::validate_shape(&combined_path, None).map_err(|e| bad(CMD, e))?;
                if sh.fill.is_none() {
                    sh.fill = Some(Fill::Solid(fgc));
                }
                sh.stroke = None;
                sh.path = combined_path;
                set_live_linework(layer, &lw);
                Ok(())
            })?;
            let layer_name = s.active().and_then(|st| st.doc.layer(active_id).map(|l| l.name.clone())).unwrap_or(output.name);
            let mut resp = live_stroke_to_json(active_id.0, &layer_name, &output.live_stroke);
            resp["strokeCount"] = json!(lw.strokes.len());
            return Ok(resp);
        }
    }

    let fgc = effective_stroke_fg(s, &output.live_stroke);
    let (fill, stroke) = if output.live_stroke.tapered || append_to_active {
        let fill = match p.get("fill") {
            Some(v) => parse_fill(v).map_err(|e| bad(CMD, e))?,
            None => Some(Fill::Solid(fgc)),
        };
        (fill, None)
    } else {
        let base = ShapeStroke { width: output.live_stroke.base_width as f32, paint: Fill::Solid(fgc), ..Default::default() };
        let stroke = match p.get("stroke") {
            Some(v) => parse_stroke(v, Some(base), fgc).map_err(|e| bad(CMD, e))?,
            None => Some(base),
        };
        (None, stroke)
    };
    let initial_path = if append_to_active { output.live_stroke.rebuild_outline_path() } else { output.path };
    vector::flatten::validate_shape(&initial_path, stroke.as_ref()).map_err(|e| bad(CMD, e))?;

    let name = output.name;
    let mut sh = ShapeLayer { path: initial_path, fill, stroke, live: None, ..Default::default() };

    let live_stroke_clone = output.live_stroke.clone();
    let id = s.edit("New Vector Curve", |doc, active| {
        refresh_shape(doc, &mut sh);
        let name = doc.next_layer_name(&name);
        let mut layer = Layer::new(name, LayerContent::Shape(sh));
        let lw = LiveLinework::new(vec![live_stroke_clone]);
        set_live_linework(&mut layer, &lw);
        let id = doc.insert_above(*active, layer);
        *active = Some(id);
        Ok(id)
    })?;

    Ok(live_stroke_to_json(id.0, &name, &output.live_stroke))
}

fn vector_curve_update_node(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "shape.vectorCurve.updateNode";
    let id = layer_id(s, p)?;
    let stroke_idx_opt = p.get("strokeIndex").and_then(Value::as_u64).map(|v| v as usize);
    let delete_node = p.get("deleteNode").and_then(Value::as_bool).unwrap_or(false);
    let new_base_width = p.get("baseWidth").and_then(Value::as_f64);
    let idx = p.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
    let new_pos = if let Some(pos) = p.get("pos").and_then(Value::as_array) {
        if pos.len() >= 2 {
            let x = pos[0].as_f64().ok_or_else(|| bad(CMD, "invalid pos.x"))?;
            let y = pos[1].as_f64().ok_or_else(|| bad(CMD, "invalid pos.y"))?;
            Some((x, y))
        } else {
            None
        }
    } else {
        None
    };
    let new_pressure = p.get("pressure").and_then(Value::as_f64).map(|v| v as f32);
    let toggle_corner = p.get("toggleCorner").and_then(Value::as_bool).unwrap_or(false);

    let updated_live = with_shape(s, id, "Update Vector Curve Node", |sh, layer| {
        let mut lw = get_live_linework(layer).ok_or_else(|| bad(CMD, "layer has no live vector curve data"))?;
        if lw.strokes.is_empty() {
            return Err(bad(CMD, "linework layer has no strokes"));
        }
        let s_idx = stroke_idx_opt.unwrap_or(lw.strokes.len() - 1);
        let multi = lw.strokes.len() > 1 || sh.stroke.is_none();
        let stroke = lw.strokes.get_mut(s_idx).ok_or_else(|| bad(CMD, format!("strokeIndex {s_idx} out of bounds")))?;

        if let Some(bw) = new_base_width
            && bw.is_finite()
        {
            stroke.base_width = bw.clamp(0.1, 100_000.0);
            if !multi && let Some(st) = sh.stroke.as_mut() {
                st.width = stroke.base_width as f32;
            }
        }

        if delete_node {
            if stroke.nodes.len() > 2 && idx < stroke.nodes.len() {
                stroke.nodes.remove(idx);
            }
        } else if p.get("index").is_some() {
            let _ = update_vector_curve_node(stroke, idx, new_pos, new_pressure, toggle_corner).map_err(|e| bad(CMD, e))?;
        }

        let updated = stroke.clone();
        let path = if multi { lw.rebuild_path() } else { updated.rebuild_path() };
        vector::flatten::validate_shape(&path, sh.stroke.as_ref()).map_err(|e| bad(CMD, e))?;
        sh.path = path;
        set_live_linework(layer, &lw);
        Ok(updated)
    })?;

    let name = s.active().and_then(|st| st.doc.layer(id).map(|l| l.name.clone())).unwrap_or_default();
    Ok(live_stroke_to_json(id.0, &name, &updated_live))
}

fn vector_curve_insert_node(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "shape.vectorCurve.insertNode";
    let id = layer_id(s, p)?;
    let stroke_idx_opt = p.get("strokeIndex").and_then(Value::as_u64).map(|v| v as usize);
    let x = p.get("x").and_then(Value::as_f64).ok_or_else(|| bad(CMD, "missing `x`"))?;
    let y = p.get("y").and_then(Value::as_f64).ok_or_else(|| bad(CMD, "missing `y`"))?;
    let max_dist = p.get("maxDist").and_then(Value::as_f64).unwrap_or(20.0);

    let (s_idx, inserted_idx, updated_live) = with_shape(s, id, "Insert Vector Curve Node", |sh, layer| {
        let mut lw = get_live_linework(layer).ok_or_else(|| bad(CMD, "layer has no live vector curve data"))?;
        if lw.strokes.is_empty() {
            return Err(bad(CMD, "linework layer has no strokes"));
        }
        let multi = lw.strokes.len() > 1 || sh.stroke.is_none();
        let (target_s, idx) = if let Some(s_i) = stroke_idx_opt {
            let stroke = lw.strokes.get_mut(s_i).ok_or_else(|| bad(CMD, format!("strokeIndex {s_i} out of bounds")))?;
            let (n_i, _) = insert_vector_curve_node_at(stroke, x, y, max_dist).map_err(|e| bad(CMD, e))?;
            (s_i, n_i)
        } else if lw.strokes.len() == 1 {
            let (n_i, _) = insert_vector_curve_node_at(&mut lw.strokes[0], x, y, max_dist).map_err(|e| bad(CMD, e))?;
            (0, n_i)
        } else {
            lw.insert_node_at_pos((x, y), max_dist).ok_or_else(|| bad(CMD, "shape.vectorCurve.insertNode: no curve segment within hit radius"))?
        };
        let updated = lw.strokes[target_s].clone();
        let path = if multi { lw.rebuild_path() } else { updated.rebuild_path() };
        vector::flatten::validate_shape(&path, sh.stroke.as_ref()).map_err(|e| bad(CMD, e))?;
        sh.path = path;
        set_live_linework(layer, &lw);
        Ok((target_s, idx, updated))
    })?;

    let name = s.active().and_then(|st| st.doc.layer(id).map(|l| l.name.clone())).unwrap_or_default();
    let mut resp = live_stroke_to_json(id.0, &name, &updated_live);
    resp["strokeIndex"] = json!(s_idx);
    resp["insertedIndex"] = json!(inserted_idx);
    Ok(resp)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "shape.vectorCurve.create",
            label: "New Vector Curve",
            menu: &[],
            shortcut: None,
            params: r##"{"points":[{"x":x,"y":y,"pressure":0..2=1,"corner":bool=false}],"width":px=6,"minRatio":0..1=0.05,"tapered":bool=true,"closed":bool=false,"tension":0..1=0.65,"name":str?} → {id,name,command,nodes,baseWidth,minRatio,tapered,closed,tension}"##,
            enabled: has_doc,
            run: vector_curve_create,
            journal: true,
        },
        CommandSpec {
            id: "shape.vectorCurve.updateNode",
            label: "Update Vector Curve Node",
            menu: &[],
            shortcut: None,
            params: r##"{"layer":id?,"index":n,"pos":[x,y]?,"pressure":0..2?,"toggleCorner":bool?} → {id,name,command,nodes,baseWidth,minRatio,tapered,closed,tension}"##,
            enabled: has_layer,
            run: vector_curve_update_node,
            journal: true,
        },
        CommandSpec {
            id: "shape.vectorCurve.insertNode",
            label: "Insert Vector Curve Node",
            menu: &[],
            shortcut: None,
            params: r##"{"layer":id?,"x":x,"y":y,"maxDist":px=20} → {id,name,command,nodes,insertedIndex,baseWidth,minRatio,tapered,closed,tension}"##,
            enabled: has_layer,
            run: vector_curve_insert_node,
            journal: true,
        },
        CommandSpec {
            id: "layer.newLinework",
            label: "New Linework Layer",
            menu: &[],
            shortcut: None,
            params: r##"{"name":str?,"width":px=3,"fill":{fill}?} → {layer,name}. An empty SAI Linework layer, ready for the Vector Curve tool."##,
            enabled: has_doc,
            run: layer_new_linework,
            journal: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use photocraft_doc::{LayerContent, LayerId, ShapeLayer};
    use serde_json::json;

    use crate::Session;

    fn session(w: u32, h: u32, depth: u8) -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": w, "height": h, "background": "white", "depth": depth})).unwrap();
        s
    }

    fn shape(s: &Session, id: u64) -> ShapeLayer {
        match &s.active().unwrap().doc.layer(LayerId(id)).unwrap().content {
            LayerContent::Shape(sh) => sh.clone(),
            other => panic!("not a shape layer: {}", other.kind_name()),
        }
    }

    #[test]
    fn test_vector_curve_create_update_and_insert() {
        let mut s = session(400, 400, 8);
        let pts = json!([
            {"x": 50.0, "y": 200.0, "pressure": 0.2, "corner": false},
            {"x": 150.0, "y": 100.0, "pressure": 1.0, "corner": false},
            {"x": 250.0, "y": 200.0, "pressure": 0.3, "corner": false}
        ]);
        let r = s
            .execute(
                "shape.vectorCurve.create",
                json!({
                    "points": pts,
                    "width": 10.0,
                    "minRatio": 0.05,
                    "tapered": true,
                    "closed": false,
                    "tension": 0.65,
                    "name": "My SAI Curve"
                }),
            )
            .unwrap();

        let layer_id = r["id"].as_u64().unwrap();
        assert_eq!(r["name"], "My SAI Curve");
        assert_eq!(r["baseWidth"], 10.0);
        assert_eq!(r["nodes"].as_array().unwrap().len(), 3);

        let info = s.execute("shape.info", json!({"layer": layer_id})).unwrap();
        assert!(info.get("liveVectorStroke").is_some());
        assert_eq!(info["liveVectorStroke"]["id"], layer_id);

        let ins = s
            .execute(
                "shape.vectorCurve.insertNode",
                json!({
                    "layer": layer_id,
                    "x": 100.0,
                    "y": 140.0,
                    "maxDist": 30.0
                }),
            )
            .unwrap();
        assert_eq!(ins["nodes"].as_array().unwrap().len(), 4);

        let upd = s
            .execute(
                "shape.vectorCurve.updateNode",
                json!({
                    "layer": layer_id,
                    "index": 1,
                    "pressure": 1.8,
                    "toggleCorner": true
                }),
            )
            .unwrap();
        let pressure = upd["nodes"][1]["pressure"].as_f64().unwrap();
        assert!((pressure - 1.8).abs() < 1e-6);
        assert_eq!(upd["nodes"][1]["corner"], true);
    }

    #[test]
    fn test_linework_layer_appends_multiple_strokes_to_single_layer() {
        let mut s = session(400, 400, 8);
        let r1 = s
            .execute(
                "shape.vectorCurve.create",
                json!({
                    "points": [
                        {"x": 40.0, "y": 100.0, "pressure": 0.1, "corner": false},
                        {"x": 140.0, "y": 80.0, "pressure": 1.0, "corner": false},
                        {"x": 240.0, "y": 100.0, "pressure": 0.1, "corner": false}
                    ],
                    "width": 8.0,
                    "tapered": true,
                    "appendToActive": true
                }),
            )
            .unwrap();
        let layer_id = r1["id"].as_u64().unwrap();

        let r2 = s
            .execute(
                "shape.vectorCurve.create",
                json!({
                    "points": [
                        {"x": 40.0, "y": 240.0, "pressure": 1.0, "corner": false},
                        {"x": 240.0, "y": 240.0, "pressure": 1.0, "corner": false}
                    ],
                    "width": 12.0,
                    "tapered": false,
                    "appendToActive": true
                }),
            )
            .unwrap();
        assert_eq!(r2["id"].as_u64().unwrap(), layer_id, "Second stroke must remain on the same Linework layer");
        assert_eq!(r2["strokeCount"], 2);

        let upd = s
            .execute(
                "shape.vectorCurve.updateNode",
                json!({
                    "layer": layer_id,
                    "strokeIndex": 0,
                    "baseWidth": 18.0
                }),
            )
            .unwrap();
        assert_eq!(upd["baseWidth"], 18.0);
    }

    #[test]
    fn single_stroke_update_node_keeps_centerline_and_stroke() {
        let mut s = session(400, 400, 8);
        let r1 = s
            .execute(
                "shape.vectorCurve.create",
                json!({
                    "points": [
                        {"x": 40.0, "y": 100.0, "pressure": 1.0, "corner": false},
                        {"x": 240.0, "y": 100.0, "pressure": 1.0, "corner": false}
                    ],
                    "width": 8.0,
                    "tapered": false
                }),
            )
            .unwrap();
        let layer_id = r1["id"].as_u64().unwrap();

        let upd = s
            .execute(
                "shape.vectorCurve.updateNode",
                json!({
                    "layer": layer_id,
                    "strokeIndex": 0,
                    "baseWidth": 22.0
                }),
            )
            .unwrap();
        assert_eq!(upd["baseWidth"], 22.0);

        let sh = shape(&s, layer_id);
        let stroke = sh.stroke.as_ref().expect("single non-tapered stroke must retain sh.stroke");
        assert!((stroke.width - 22.0).abs() < 1e-4, "sh.stroke.width must sync with baseWidth");
        assert_eq!(sh.path.subpaths.len(), 1);
        assert!(!sh.path.subpaths[0].closed, "single non-tapered stroke must stay open centerline rather than multi-stroke ribbon");
    }
}
