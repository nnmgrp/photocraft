//! Secondary non-destructive pixel mask (`Layer::second_mask`) commands (`layer.secondMask.*`).

use photocraft_doc::{LayerMask, VectorMask};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, check_all_unlocked, layer_param, mask_at_depth, selection_mask};
use crate::{EngineError, Result, Session};

pub const ADD: &str = "layer.secondMask.add";
pub const DELETE: &str = "layer.secondMask.delete";
pub const ENABLED: &str = "layer.secondMask.enabled";
pub const TO_VECTOR: &str = "layer.secondMask.toVectorMask";
pub const LINKED: &str = "layer.secondMask.linked";

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    let seam = s.seam_flags();
    if !seam.second_mask {
        return Err("second layer mask is disabled (seam.second_mask)".into());
    }
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

fn ensure_enabled(s: &Session, cmd: &str) -> Result<()> {
    let seam = s.seam_flags();
    if !seam.second_mask {
        return Err(bad(cmd, "second layer mask is disabled by seam.second_mask"));
    }
    Ok(())
}

fn opt_bool(p: &Value, key: &str, cmd: &str) -> Result<Option<bool>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(bad(cmd, format!("`{key}` must be true or false"))),
    }
}

fn add(s: &mut Session, p: &Value) -> Result<Value> {
    ensure_enabled(s, ADD)?;
    let id = layer_param(s, p)?;
    let hide = opt_bool(p, "hide", ADD)?.unwrap_or(false);
    s.edit("Add Second Layer Mask", |doc, _| {
        check_all_unlocked(doc, id)?;
        let Some(layer) = doc.layer(id) else {
            return Err(EngineError::NoLayer(id));
        };
        if layer.mask.is_none() {
            return Err(bad(ADD, "add a layer mask before adding a second mask"));
        }
        if layer.second_mask.is_some() {
            return Err(bad(ADD, "the layer already has a second mask"));
        }
        let from_selection = selection_mask(doc)?;
        let mask = match from_selection {
            Some(m) => m,
            None => mask_at_depth(if hide { LayerMask::hide_all() } else { LayerMask::reveal_all() }, doc.depth)?,
        };
        let layer_mut = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        layer_mut.second_mask = Some(mask);
        doc.selection = None;
        Ok(())
    })?;
    Ok(json!({ "layer": id.0 }))
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    ensure_enabled(s, DELETE)?;
    let id = layer_param(s, p)?;
    s.edit("Delete Second Layer Mask", |doc, _| {
        check_all_unlocked(doc, id)?;
        let layer = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        if layer.second_mask.take().is_none() {
            return Err(bad(DELETE, "the layer has no second mask"));
        }
        Ok(())
    })?;
    Ok(json!({ "layer": id.0 }))
}

fn set_enabled(s: &mut Session, p: &Value) -> Result<Value> {
    ensure_enabled(s, ENABLED)?;
    let id = layer_param(s, p)?;
    let enabled = opt_bool(p, "enabled", ENABLED)?;
    let mut final_val = false;
    s.edit("Toggle Second Mask", |doc, _| {
        check_all_unlocked(doc, id)?;
        let layer = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        let mask = layer.second_mask.as_mut().ok_or_else(|| bad(ENABLED, "the layer has no second mask"))?;
        mask.enabled = enabled.unwrap_or(!mask.enabled);
        final_val = mask.enabled;
        Ok(())
    })?;
    Ok(json!({ "layer": id.0, "enabled": final_val }))
}

fn set_linked(s: &mut Session, p: &Value) -> Result<Value> {
    ensure_enabled(s, LINKED)?;
    let id = layer_param(s, p)?;
    let linked = opt_bool(p, "linked", LINKED)?;
    let mut final_val = false;
    s.edit("Link Second Mask", |doc, _| {
        check_all_unlocked(doc, id)?;
        let layer = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        let mask = layer.second_mask.as_mut().ok_or_else(|| bad(LINKED, "the layer has no second mask"))?;
        mask.linked = linked.unwrap_or(!mask.linked);
        final_val = mask.linked;
        Ok(())
    })?;
    Ok(json!({ "layer": id.0, "linked": final_val }))
}

fn to_vector_mask(s: &mut Session, p: &Value) -> Result<Value> {
    ensure_enabled(s, TO_VECTOR)?;
    let id = layer_param(s, p)?;
    let tol = match p.get("tolerance") {
        None | Some(Value::Null) => 2.0,
        Some(v) => v.as_f64().filter(|t| t.is_finite() && (0.5..=10.0).contains(t)).ok_or_else(|| bad(TO_VECTOR, "`tolerance` must be between 0.5 and 10"))?,
    };
    s.edit("Convert Second Mask to Vector Mask", |doc, _| {
        check_all_unlocked(doc, id)?;
        let area = doc.bounds();
        let layer = doc.layer(id).ok_or(EngineError::NoLayer(id))?;
        if layer.vector_mask.is_some() {
            return Err(bad(TO_VECTOR, "the layer already has a vector mask"));
        }
        let second = layer.second_mask.as_ref().ok_or_else(|| bad(TO_VECTOR, "the layer has no second mask"))?;
        let values = photocraft_algo::selection::mask_from_surface(Some(&second.surface), area);
        let path = photocraft_vector::trace::trace_mask(&values, area, 0.0, tol);
        let layer_mut = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        layer_mut.second_mask = None;
        layer_mut.vector_mask = Some(VectorMask::new(path));
        Ok(())
    })?;
    Ok(json!({ "layer": id.0 }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: ADD,
            label: "Add Second Layer Mask",
            menu: &["Layer", "Layer Mask"],
            shortcut: None,
            params: r##"{"layer":id?,"hide":bool=false}"##,
            enabled: has_doc,
            run: add,
            journal: true,
        },
        CommandSpec {
            id: DELETE,
            label: "Delete Second Layer Mask",
            menu: &["Layer", "Layer Mask"],
            shortcut: None,
            params: r##"{"layer":id?}"##,
            enabled: has_doc,
            run: delete,
            journal: true,
        },
        CommandSpec {
            id: ENABLED,
            label: "Enable Second Layer Mask",
            menu: &["Layer", "Layer Mask"],
            shortcut: None,
            params: r##"{"layer":id?,"enabled":bool?}"##,
            enabled: has_doc,
            run: set_enabled,
            journal: true,
        },
        CommandSpec {
            id: LINKED,
            label: "Link Second Layer Mask",
            menu: &["Layer", "Layer Mask"],
            shortcut: None,
            params: r##"{"layer":id?,"linked":bool?}"##,
            enabled: has_doc,
            run: set_linked,
            journal: true,
        },
        CommandSpec {
            id: TO_VECTOR,
            label: "Second Mask to Vector Mask",
            menu: &["Layer", "Layer Mask"],
            shortcut: None,
            params: r##"{"layer":id?,"tolerance":0.5..10 px=2}"##,
            enabled: has_doc,
            run: to_vector_mask,
            journal: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use photocraft_doc::LayerId;
    use serde_json::json;

    use super::*;

    fn session() -> (Session, LayerId) {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 40, "height": 20, "background": "white"})).unwrap();
        s.execute("layer.new.layer", json!({"name": "Red"})).unwrap();
        s.execute("edit.fill", json!({"contents": "color", "color": "#ff0000"})).unwrap();
        s.execute("layer.layerMask.revealAll", json!({})).unwrap();
        let id = s.active().unwrap().active_layer.unwrap();
        (s, id)
    }

    fn alpha_at(s: &Session, x: i32, y: i32) -> f32 {
        let doc = &s.active().unwrap().doc;
        let px = photocraft_compose::render(doc, photocraft_geom::Rect::from_xywh(x, y, 1, 1)).px[0];
        1.0 - px[1]
    }

    #[test]
    fn a_second_mask_hides_what_it_hides_and_both_masks_multiply() {
        let (mut s, id) = session();
        assert!(alpha_at(&s, 30, 10) > 0.99, "the red layer shows");
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 20, "height": 20})).unwrap();
        s.execute(ADD, json!({})).unwrap();
        assert!(s.active().unwrap().doc.selection.is_none(), "the selection became the mask");
        assert!(alpha_at(&s, 30, 10) < 0.01 && alpha_at(&s, 10, 10) > 0.99);
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 40, "height": 10})).unwrap();
        s.execute("edit.fill", json!({"contents": "color", "color": "#000000", "target": "mask"})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        assert!(alpha_at(&s, 10, 5) < 0.01 && alpha_at(&s, 10, 15) > 0.99 && alpha_at(&s, 30, 15) < 0.01);
        s.execute("select.rect", json!({"x": 20, "y": 10, "width": 20, "height": 10})).unwrap();
        s.execute("edit.fill", json!({"contents": "color", "color": "#ffffff", "target": "secondMask"})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        assert!(alpha_at(&s, 30, 15) > 0.99 && alpha_at(&s, 30, 5) < 0.01);
        let l = s.active().unwrap().doc.layer(id).unwrap().clone();
        assert_eq!(l.mask.as_ref().unwrap().value(30, 15), 1.0, "the first mask is untouched there");
        s.execute(ENABLED, json!({"enabled": false})).unwrap();
        assert!(alpha_at(&s, 30, 15) > 0.99 && alpha_at(&s, 10, 15) > 0.99);
        s.execute(ENABLED, json!({"enabled": true})).unwrap();
        s.execute(DELETE, json!({})).unwrap();
        assert!(s.active().unwrap().doc.layer(id).unwrap().second_mask.is_none());
        s.execute("edit.undo", json!({})).unwrap();
        assert!(s.active().unwrap().doc.layer(id).unwrap().second_mask.is_some());
    }

    #[test]
    fn moving_the_layer_moves_both_masks() {
        let (mut s, id) = session();
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 10, "height": 20})).unwrap();
        s.execute(ADD, json!({})).unwrap();
        s.execute("layer.translate", json!({"dx": 5, "dy": 0})).unwrap();
        let l = s.active().unwrap().doc.layer(id).unwrap().clone();
        let m = l.second_mask.unwrap();
        assert!(m.value(12, 5) > 0.99 && m.value(2, 5) < 0.01, "the second mask moved with the layer");
    }

    #[test]
    fn convert_to_vector_mask_traces_the_revealed_area() {
        let (mut s, id) = session();
        s.execute("select.rect", json!({"x": 10, "y": 4, "width": 20, "height": 12})).unwrap();
        s.execute(ADD, json!({})).unwrap();
        s.execute(TO_VECTOR, json!({})).unwrap();
        let l = s.active().unwrap().doc.layer(id).unwrap().clone();
        assert!(l.second_mask.is_none());
        let vm = l.vector_mask.expect("a vector mask");
        assert!(!vm.path.subpaths.is_empty());
        assert!(alpha_at(&s, 20, 10) > 0.99 && alpha_at(&s, 2, 2) < 0.01);
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 5, "height": 5})).unwrap();
        s.execute(ADD, json!({})).unwrap();
        assert!(s.execute(TO_VECTOR, json!({})).is_err());
    }

    #[test]
    fn bad_params_are_errors_not_panics() {
        let (mut s, _) = session();
        for (id, p) in [
            (DELETE, json!({})),
            (ENABLED, json!({"enabled": true})),
            (TO_VECTOR, json!({})),
            (ADD, json!({"hide": "yes"})),
            (ADD, json!({"layer": 999_999})),
            (ENABLED, json!({"enabled": 3})),
        ] {
            assert!(s.execute(id, p.clone()).is_err(), "{id} {p}");
        }
        s.execute(ADD, json!({"hide": true})).unwrap();
        assert!(s.execute(ADD, json!({})).is_err(), "one second mask a layer");
        assert!(s.execute(TO_VECTOR, json!({"tolerance": 99})).is_err());
        s.execute("layer.new.layer", json!({})).unwrap();
        assert!(s.execute(ADD, json!({})).is_err());
    }
}
