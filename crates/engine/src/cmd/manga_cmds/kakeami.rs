//! Engine commands for live procedural Kakeami / Nawa-ami layers (`layer.kakeami.create`,
//! `layer.kakeami.update`) backed by `kakeami-core` and `photocraft_compose::kakeami`.

use photocraft_color::BlendMode;
use photocraft_doc::{KakeamiConfig, KakeamiLayer, Layer, LayerContent, LayerId, LayerMask, Rect};
use photocraft_raster::Surface;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

pub const CREATE: &str = "layer.kakeami.create";
pub const UPDATE: &str = "layer.kakeami.update";

/// Maximum pixels for eager synchronous `GRAY8` cache generation on create/update (16 MiB at 1 B/px).
/// Larger frames remain lazy (`cache = None`) and render tiled/banded on demand with zero OOM.
const EAGER_CACHE_MAX_PIXELS: i64 = 16_000_000;

const CREATE_PARAMS: &str = r##"{"config":object?,"color":color?,"origin":[f32,f32]?,"dpiSync":bool?,"fillFrame":[i32,i32,i32,i32]?,"clipped":bool?,"name":text?} → {layer,bounds}"##;
const UPDATE_PARAMS: &str = r##"{"layer":id?,"config":object?,"color":color?,"origin":[f32,f32]?,"dpiSync":bool?,"fillFrame":[i32,i32,i32,i32]|null?,"clipped":bool?,"coalesce":text?} → {layer,bounds}"##;

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

fn has_kakeami(s: &Session) -> std::result::Result<(), String> {
    let st = s.active().ok_or_else(|| "no document open".to_string())?;
    let id = st.active_layer.ok_or_else(|| "no active layer".to_string())?;
    if st.doc.layer(id).is_some_and(Layer::is_kakeami) {
        Ok(())
    } else {
        Err("active layer is not a Kakeami layer".into())
    }
}

fn parse_color(p: &Value, key: &str, default: [f32; 4]) -> [f32; 4] {
    match p.get(key) {
        Some(Value::Array(a)) if a.len() >= 3 => {
            let ch = |i: usize, dv: f32| a.get(i).and_then(Value::as_f64).map_or(dv, |v| (v as f32).clamp(0.0, 1.0));
            [ch(0, default[0]), ch(1, default[1]), ch(2, default[2]), ch(3, default[3])]
        }
        Some(Value::String(h)) if h.starts_with('#') => {
            let hex = h.trim_start_matches('#');
            let byte = |i: usize| hex.get(i..i + 2).and_then(|s| u8::from_str_radix(s, 16).ok()).map(|v| f32::from(v) / 255.0);
            let (Some(r), Some(g), Some(b)) = (byte(0), byte(2), byte(4)) else { return default };
            [r, g, b, byte(6).unwrap_or(default[3])]
        }
        _ => default,
    }
}

fn parse_origin(p: &Value, default: (f32, f32)) -> (f32, f32) {
    match p.get("origin").and_then(Value::as_array) {
        Some(a) if a.len() >= 2 => {
            let x = a[0].as_f64().map_or(default.0, |v| v as f32);
            let y = a[1].as_f64().map_or(default.1, |v| v as f32);
            if x.is_finite() && y.is_finite() { (x, y) } else { default }
        }
        _ => default,
    }
}

fn parse_rect(v: &Value) -> Option<Rect> {
    let a = v.as_array()?;
    if a.len() < 4 {
        return None;
    }
    let x0 = a[0].as_i64()? as i32;
    let y0 = a[1].as_i64()? as i32;
    let x1 = a[2].as_i64()? as i32;
    let y1 = a[3].as_i64()? as i32;
    let r = Rect::new(x0, y0, x1, y1);
    (!r.is_empty()).then_some(r)
}

fn maybe_build_cache(layer_id: LayerId, k: &KakeamiLayer, frame: Rect, doc_dpi: f32) -> Option<Surface> {
    if frame.is_empty() {
        return None;
    }
    let pixels = i64::from(frame.width()) * i64::from(frame.height());
    if pixels > EAGER_CACHE_MAX_PIXELS {
        return None;
    }
    photocraft_compose::kakeami::render_gray8_surface(layer_id, k, frame, doc_dpi, None)
}

fn run_create(s: &mut Session, p: &Value) -> Result<Value> {
    let (canvas, doc_dpi, sel_mask, sel_bounds) = {
        let st = s.active().ok_or(EngineError::NoDocument)?;
        let canvas = st.doc.bounds();
        let mask = st.doc.selection.as_ref().and_then(|sel| {
            let b = sel.content_bounds().intersect(&canvas);
            (!b.is_empty()).then(|| {
                let mut m = LayerMask::hide_all();
                m.surface = sel.clone();
                (m, b)
            })
        });
        let (m, b) = match mask {
            Some((m, b)) => (Some(m), Some(b)),
            None => (None, None),
        };
        (canvas, st.doc.resolution_dpi, m, b)
    };

    let config: KakeamiConfig = match p.get("config") {
        Some(v) if !v.is_null() => serde_json::from_value(v.clone()).map_err(|e| EngineError::BadParams {
            cmd: CREATE.into(),
            msg: format!("invalid KakeamiConfig: {e}"),
        })?,
        _ => KakeamiConfig::default(),
    };
    let color = parse_color(p, "color", [0.0, 0.0, 0.0, 1.0]);
    let origin = parse_origin(p, (0.0, 0.0));
    let dpi_sync = p.get("dpiSync").and_then(Value::as_bool).unwrap_or(true);
    let fill_frame = p.get("fillFrame").and_then(parse_rect).or(sel_bounds);
    let clipped = p.get("clipped").and_then(Value::as_bool).unwrap_or(false);
    let name = p
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or("Kakeami")
        .to_string();

    let mut k = KakeamiLayer { config, color, origin, dpi_sync, fill_frame, cache: None };
    let layer_id = LayerId::fresh();
    let frame = fill_frame.map(|f| f.intersect(&canvas)).filter(|r| !r.is_empty()).unwrap_or(canvas);
    k.cache = maybe_build_cache(layer_id, &k, frame, doc_dpi);

    let mut layer = Layer::new(name, LayerContent::Kakeami(k));
    layer.id = layer_id;
    layer.clipped = clipped;
    layer.blend = BlendMode::Normal;
    layer.mask = sel_mask;

    let id = s.edit("New Kakeami Layer", |doc, active| {
        let id = doc.insert_above(*active, layer);
        *active = Some(id);
        Ok(id)
    })?;

    Ok(json!({
        "layer": id.0,
        "bounds": [frame.x0, frame.y0, frame.x1, frame.y1],
    }))
}

fn run_update(s: &mut Session, p: &Value) -> Result<Value> {
    let target_id = {
        let st = s.active().ok_or(EngineError::NoDocument)?;
        match p.get("layer").and_then(Value::as_u64) {
            Some(raw) => LayerId(raw),
            None => st.active_layer.ok_or_else(|| EngineError::Other("no active layer".into()))?,
        }
    };

    let (mut next_k, canvas, doc_dpi, effective_frame, next_clipped) = {
        let st = s.active().ok_or(EngineError::NoDocument)?;
        let layer = st.doc.layer(target_id).ok_or(EngineError::NoLayer(target_id))?;
        let LayerContent::Kakeami(existing) = &layer.content else {
            return Err(EngineError::Other(format!("layer {} is not a Kakeami layer", target_id.0)));
        };
        let mut next = existing.clone();
        if let Some(cfg_v) = p.get("config").filter(|v| !v.is_null()) {
            next.config = serde_json::from_value(cfg_v.clone()).map_err(|e| EngineError::BadParams {
                cmd: UPDATE.into(),
                msg: format!("invalid KakeamiConfig: {e}"),
            })?;
        }
        if p.get("color").is_some() {
            next.color = parse_color(p, "color", next.color);
        }
        if p.get("origin").is_some() {
            next.origin = parse_origin(p, next.origin);
        }
        if let Some(ds) = p.get("dpiSync").and_then(Value::as_bool) {
            next.dpi_sync = ds;
        }
        if let Some(ff) = p.get("fillFrame") {
            next.fill_frame = if ff.is_null() { None } else { parse_rect(ff) };
        }
        // Invalidate cache whenever geometry/config/origin/dpi_sync/frame changes.
        let geom_changed = next.config != existing.config
            || next.origin != existing.origin
            || next.dpi_sync != existing.dpi_sync
            || next.fill_frame != existing.fill_frame;
        if geom_changed {
            next.cache = None;
        }
        let canvas = st.doc.bounds();
        let frame = photocraft_compose::kakeami::effective_frame(layer, &next, canvas);
        let clipped = p.get("clipped").and_then(Value::as_bool);
        (next, canvas, st.doc.resolution_dpi, frame, clipped)
    };

    if next_k.cache.is_none() {
        next_k.cache = maybe_build_cache(target_id, &next_k, effective_frame, doc_dpi);
    }
    let _ = canvas;

    let coalesce = p.get("coalesce").and_then(Value::as_str).map(str::to_string);
    if coalesce.is_some() {
        s.coalesce_request = coalesce;
    }
    let res = s.edit("Update Kakeami Layer", |doc, _| {
        let layer = doc.layer_mut(target_id).ok_or(EngineError::NoLayer(target_id))?;
        if !layer.is_kakeami() {
            return Err(EngineError::Other(format!("layer {} is not a Kakeami layer", target_id.0)));
        }
        layer.content = LayerContent::Kakeami(next_k);
        if let Some(c) = next_clipped {
            layer.clipped = c;
        }
        Ok(())
    });
    s.coalesce_request = None;
    res?;

    Ok(json!({
        "layer": target_id.0,
        "bounds": [effective_frame.x0, effective_frame.y0, effective_frame.x1, effective_frame.y1],
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: CREATE,
            label: "New Kakeami Layer",
            menu: &["Layer"],
            shortcut: None,
            params: CREATE_PARAMS,
            enabled: has_doc,
            journal: true,
            run: run_create,
        },
        CommandSpec {
            id: UPDATE,
            label: "Update Kakeami Layer",
            menu: &["Layer"],
            shortcut: None,
            params: UPDATE_PARAMS,
            enabled: has_kakeami,
            journal: true,
            run: run_update,
        },
    ]
}

#[cfg(test)]
mod tests {
    use photocraft_color::{ColorMode, PixelFormat, SampleType};
    use photocraft_doc::{KakeamiConfig, LayerContent, LayerId, LayerMask, Rect};
    use serde_json::json;

    use crate::Session;

    #[test]
    fn kakeami_create_update_preserves_mask_and_supports_undo_redo() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 128, "height": 128})).unwrap();

        let created = s
            .execute(
                super::CREATE,
                json!({
                    "name": "Kakeami Test",
                    "color": [0.0, 0.0, 0.0, 1.0],
                    "dpiSync": true
                }),
            )
            .unwrap();
        let lid = LayerId(created["layer"].as_u64().unwrap());

        // Attach a user mask to the Kakeami layer.
        s.edit("Attach Mask", |doc, _| {
            let mut m = LayerMask::hide_all();
            m.surface.fill_rect(Rect::new(16, 16, 96, 96), &[1.0]);
            doc.layer_mut(lid).unwrap().mask = Some(m);
            Ok(())
        })
        .unwrap();

        // Update config + color with coalesce key.
        let mut cfg = KakeamiConfig::default();
        cfg.pattern_family = "nawa-ami".into();
        cfg.finish_mode = "pencil".into();
        cfg.nawa_tone_style = "denseWhite".into();
        s.execute(
            super::UPDATE,
            json!({
                "layer": lid.0,
                "config": cfg,
                "color": [0.2, 0.3, 0.8, 0.9],
                "coalesce": "kake-drag"
            }),
        )
        .unwrap();

        let st = s.active().unwrap();
        let layer = st.doc.layer(lid).unwrap();
        assert!(layer.mask.is_some(), "user mask must survive layer.kakeami.update");
        let LayerContent::Kakeami(k) = &layer.content else {
            panic!("expected LayerContent::Kakeami");
        };
        assert_eq!(k.config.pattern_family, "nawa-ami");
        assert_eq!(k.color, [0.2, 0.3, 0.8, 0.9]);
        assert!(k.cache.as_ref().is_some_and(|c| c.format() == PixelFormat::GRAY8 && c.tile_count() > 0));

        // Undo reverts update while keeping layer; redo restores update.
        s.execute("edit.undo", json!({})).unwrap();
        let LayerContent::Kakeami(k_undo) = &s.active().unwrap().doc.layer(lid).unwrap().content else {
            panic!("expected Kakeami after undo");
        };
        assert_eq!(k_undo.color, [0.0, 0.0, 0.0, 1.0]);

        s.execute("edit.redo", json!({})).unwrap();
        let LayerContent::Kakeami(k_redo) = &s.active().unwrap().doc.layer(lid).unwrap().content else {
            panic!("expected Kakeami after redo");
        };
        assert_eq!(k_redo.color, [0.2, 0.3, 0.8, 0.9]);
    }

    #[test]
    fn kakeami_merge_down_and_flatten_produce_ink_without_panic() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 128, "height": 128})).unwrap();

        let mut cfg = KakeamiConfig::default();
        cfg.pattern_family = "kakeami-v8".into();
        cfg.finish_mode = "clean".into();
        cfg.darken_cap = 90;
        cfg.grad_start = 80;
        cfg.grad_end = 95;
        s.execute(super::CREATE, json!({"config": cfg, "color": [0.0, 0.0, 0.0, 1.0]})).unwrap();

        // Flatten Image
        s.execute("layer.flattenImage", json!({})).unwrap();
        let st = s.active().unwrap();
        assert_eq!(st.doc.mode, ColorMode::Rgb);
        assert_eq!(st.doc.depth, SampleType::U8);
        assert_eq!(st.doc.layers.len(), 1);
        let flat_surf = st.doc.layers[0].surface().expect("flattened raster layer");
        let px = flat_surf.read_region(st.doc.bounds());
        let dark_pixels = px.chunks_exact(4).filter(|c| c[0] < 0.2 && c[1] < 0.2 && c[2] < 0.2 && c[3] > 0.5).count();
        assert!(dark_pixels > 100, "flattened Kakeami document must contain rendered ink, got {dark_pixels}");

        // Undo flatten and test Merge Down as well.
        s.execute("edit.undo", json!({})).unwrap();
        s.execute("layer.mergeDown", json!({})).unwrap();
        let st2 = s.active().unwrap();
        assert_eq!(st2.doc.layers.len(), 1);
        let merged_surf = st2.doc.layers[0].surface().expect("merged raster layer");
        let px2 = merged_surf.read_region(st2.doc.bounds());
        let dark2 = px2.chunks_exact(4).filter(|c| c[0] < 0.2 && c[1] < 0.2 && c[2] < 0.2 && c[3] > 0.5).count();
        assert!(dark2 > 100, "merged Kakeami layer must contain rendered ink, got {dark2}");
    }
}
