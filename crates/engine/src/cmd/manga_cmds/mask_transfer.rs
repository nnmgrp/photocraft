//! Dragging a mask thumbnail onto another layer (`layer.layerMask.transfer`).

use serde_json::{Value, json};

use crate::commands::{CommandSpec, check_all_unlocked};
use crate::{EngineError, LayerId, Result, Session};

pub const TRANSFER_ID: &str = "layer.layerMask.transfer";

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: TRANSFER_ID.into(), msg: msg.into() }
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MaskKind {
    Layer,
    Vector,
}

fn parse_layer_id(p: &Value, key: &str) -> Result<LayerId> {
    p.get(key).and_then(Value::as_u64).map(LayerId).ok_or_else(|| bad(format!("missing or invalid `{key}` layer id")))
}

fn parse_kind(p: &Value) -> Result<MaskKind> {
    match p.get("kind") {
        None | Some(Value::Null) => Ok(MaskKind::Layer),
        Some(Value::String(s)) if s == "layer" => Ok(MaskKind::Layer),
        Some(Value::String(s)) if s == "vector" => Ok(MaskKind::Vector),
        Some(_) => Err(bad("`kind` must be \"layer\" or \"vector\"")),
    }
}

fn parse_copy(p: &Value) -> Result<bool> {
    match p.get("copy") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(bad("`copy` must be true or false")),
    }
}

fn transfer(s: &mut Session, p: &Value) -> Result<Value> {
    let from = parse_layer_id(p, "from")?;
    let to = parse_layer_id(p, "to")?;
    if from == to {
        return Err(bad("`from` and `to` must be different layers"));
    }
    let copy = parse_copy(p)?;
    let kind = parse_kind(p)?;

    let label = match (kind, copy) {
        (MaskKind::Layer, false) => "Move Layer Mask",
        (MaskKind::Layer, true) => "Copy Layer Mask",
        (MaskKind::Vector, false) => "Move Vector Mask",
        (MaskKind::Vector, true) => "Copy Vector Mask",
    };

    s.edit(label, |doc, _| {
        if doc.layer(from).is_none() {
            return Err(EngineError::NoLayer(from));
        }
        if doc.layer(to).is_none() {
            return Err(EngineError::NoLayer(to));
        }
        if doc.is_background(to) {
            return Err(bad("cannot transfer a mask onto the Background layer"));
        }
        check_all_unlocked(doc, to)?;
        if !copy {
            check_all_unlocked(doc, from)?;
        }

        match kind {
            MaskKind::Layer => {
                let mask = doc.layer(from).and_then(|l| l.mask.clone()).ok_or_else(|| bad("source layer has no layer mask"))?;
                if !copy && let Some(src) = doc.layer_mut(from) {
                    src.mask = None;
                }
                if let Some(dst) = doc.layer_mut(to) {
                    dst.mask = Some(mask);
                }
            }
            MaskKind::Vector => {
                let mask = doc.layer(from).and_then(|l| l.vector_mask.clone()).ok_or_else(|| bad("source layer has no vector mask"))?;
                if !copy && let Some(src) = doc.layer_mut(from) {
                    src.vector_mask = None;
                }
                if let Some(dst) = doc.layer_mut(to) {
                    dst.vector_mask = Some(mask);
                }
            }
        }
        Ok(())
    })?;

    Ok(json!({
        "from": from.0,
        "to": to.0,
        "copy": copy,
        "kind": if kind == MaskKind::Layer { "layer" } else { "vector" },
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: TRANSFER_ID,
        label: "Transfer Layer Mask",
        menu: &[],
        shortcut: None,
        params: r##"{"from":layerId,"to":layerId,"copy":bool=false,"kind":"layer|vector"="layer"} -> {from,to,copy,kind}"##,
        enabled: has_doc,
        run: transfer,
        journal: true,
    }]
}

#[cfg(test)]
mod tests {
    use photocraft_doc::LayerId;
    use photocraft_geom::Rect;
    use serde_json::json;

    use super::*;

    fn session() -> (Session, LayerId, LayerId) {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 32, "height": 32, "background": "white"})).unwrap();
        s.execute("layer.new.layer", json!({"name": "A"})).unwrap();
        let a = s.active().unwrap().active_layer.unwrap();
        s.execute("layer.layerMask.revealAll", json!({})).unwrap();
        s.edit("paint", |doc, _| {
            doc.layer_mut(a).unwrap().mask.as_mut().unwrap().surface.fill_rect(Rect::new(0, 0, 10, 10), &[0.0]);
            Ok(())
        })
        .unwrap();
        s.execute("layer.new.layer", json!({"name": "B"})).unwrap();
        let b = s.active().unwrap().active_layer.unwrap();
        (s, a, b)
    }

    fn mask_at(s: &Session, id: LayerId, x: i32) -> Option<f32> {
        s.active().unwrap().doc.layer(id).unwrap().mask.as_ref().map(|m| m.surface.sample_channel(x, 5, 0))
    }

    #[test]
    fn a_dragged_mask_moves_and_with_alt_is_copied() {
        let (mut s, a, b) = session();
        s.execute(TRANSFER_ID, json!({"from": a.0, "to": b.0})).unwrap();
        assert_eq!((mask_at(&s, a, 2), mask_at(&s, b, 2), mask_at(&s, b, 15)), (None, Some(0.0), Some(1.0)), "moved, as it was");
        assert_eq!(s.active().unwrap().history.undo_label(), Some("Move Layer Mask"));
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!((mask_at(&s, a, 2), mask_at(&s, b, 2)), (Some(0.0), None), "one undo step puts it back");
        s.execute(TRANSFER_ID, json!({"from": a.0, "to": b.0, "copy": true})).unwrap();
        assert_eq!((mask_at(&s, a, 2), mask_at(&s, b, 2)), (Some(0.0), Some(0.0)), "copied");
        assert_eq!(s.active().unwrap().history.undo_label(), Some("Copy Layer Mask"));
    }

    #[test]
    fn a_vector_mask_moves_too() {
        let (mut s, a, b) = session();
        s.edit("vector", |doc, _| {
            doc.layer_mut(a).unwrap().vector_mask = Some(photocraft_doc::VectorMask::new(Default::default()));
            Ok(())
        })
        .unwrap();
        s.execute(TRANSFER_ID, json!({"from": a.0, "to": b.0, "kind": "vector"})).unwrap();
        let d = s.active().unwrap();
        assert!(d.doc.layer(a).unwrap().vector_mask.is_none() && d.doc.layer(b).unwrap().vector_mask.is_some());
        assert!(d.doc.layer(a).unwrap().mask.is_some(), "the layer mask stays");
    }

    #[test]
    fn bad_transfers_are_errors_that_change_nothing() {
        let (mut s, a, b) = session();
        let bg = s.active().unwrap().doc.layers[0].id;
        let steps = s.active().unwrap().history.past_len();
        for p in [
            json!({}),
            json!({"from": a.0}),
            json!({"from": a.0, "to": a.0}),
            json!({"from": b.0, "to": a.0}),
            json!({"from": a.0, "to": bg.0}),
            json!({"from": a.0, "to": 999_999}),
            json!({"from": a.0, "to": b.0, "copy": "yes"}),
            json!({"from": a.0, "to": b.0, "kind": "alpha"}),
            json!({"from": a.0, "to": b.0, "kind": 3}),
            json!({"from": a.0, "to": b.0, "kind": "vector"}),
            json!({"from": "a", "to": b.0}),
        ] {
            assert!(s.execute(TRANSFER_ID, p.clone()).is_err(), "{p}");
        }
        assert_eq!(s.active().unwrap().history.past_len(), steps);
        assert!(Session::new().execute(TRANSFER_ID, json!({"from": 1, "to": 2})).is_err());
    }
}
