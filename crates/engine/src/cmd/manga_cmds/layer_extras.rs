//! Manga/extra layer & adjustment commands missing from 0.23 base:
//! `layer.toggleLockAll`, `layer.selectToTop`, `layer.selectToBottom`,
//! `layer.newAdjustmentLayer.grain`, and `edit.generativeFill`.

use photocraft_algo::content_aware::{FillOptions, color_level, fill};
use photocraft_color::ColorMode;
use photocraft_doc::{Layer, LayerId};
use photocraft_raster::from_rgba_into;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, int_i32, layer_param};
use crate::layer_nav_cmds::panel_rows;
use crate::{EngineError, Result, Session};

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

fn has_layers(s: &Session) -> std::result::Result<(), String> {
    let st = s.active().ok_or("no document open")?;
    if st.doc.layers.is_empty() { Err("the document has no layers".into()) } else { Ok(()) }
}

fn has_selection(s: &Session) -> std::result::Result<(), String> {
    s.active().filter(|d| d.doc.selection.is_some()).map(|_| ()).ok_or_else(|| "no selection".into())
}

fn toggle_lock_all(s: &mut Session, p: &Value) -> Result<Value> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let raw_targets = if crate::layer_multi_cmds::multi(s, p) { crate::layer_multi_cmds::selected(s) } else { vec![layer_param(s, p)?] };
    for id in &raw_targets {
        if st.doc.layer(*id).is_none() {
            return Err(EngineError::NoLayer(*id));
        }
    }
    let targets: Vec<LayerId> = raw_targets.into_iter().filter(|id| !st.doc.is_background(*id)).collect();
    if targets.is_empty() {
        return Err(EngineError::Other("the Background layer cannot be locked or unlocked".into()));
    }
    let lock = !targets.iter().all(|id| st.doc.layer(*id).is_some_and(|l| l.locks.all));
    s.edit(if lock { "Lock All" } else { "Unlock All" }, |doc, _| {
        for id in &targets {
            doc.layer_mut(*id).ok_or(EngineError::NoLayer(*id))?.locks.all = lock;
        }
        Ok(())
    })?;
    Ok(json!({ "locked": lock }))
}

fn select_range_to_end(s: &mut Session, to_top: bool) -> Result<Value> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let rows = panel_rows(&st.doc);
    let Some(&end_id) = (if to_top { rows.first() } else { rows.last() }) else {
        return Err(EngineError::Other("the document has no layers".into()));
    };
    let (ids, next_active, next_anchor) = if let Some(cur) = st.active_layer
        && let Some(pos) = rows.iter().position(|r| *r == cur)
    {
        let slice = if to_top { &rows[..=pos] } else { &rows[pos..] };
        (slice.to_vec(), end_id, st.layer_anchor.or(Some(cur)))
    } else {
        (vec![end_id], end_id, end_id.into())
    };
    crate::layer_multi_cmds::set_selection(s, ids.clone(), Some(next_active), next_anchor)?;
    Ok(json!({
        "layer": next_active.0,
        "layers": ids.into_iter().map(|id| id.0).collect::<Vec<_>>(),
    }))
}

fn new_grain_adjustment(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layer.newAdjustmentLayer.grain";
    let mode = s.active().map_or(ColorMode::Rgb, |d| d.doc.mode);
    let adj = crate::adjust_params::from_params("grain", p, None, mode)?;
    crate::cmd::adjust_params::new_adjustment(s, adj, p, CMD)
}

fn generative_fill(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "edit.generativeFill";
    if !p.is_object() {
        return Err(bad(CMD, "params must be an object"));
    }
    if let Some(v) = p.get("prompt").filter(|v| !v.is_null())
        && !v.is_string()
    {
        return Err(bad(CMD, "`prompt` must be a string"));
    }
    let model = match p.get("model") {
        None | Some(Value::Null) => "auto",
        Some(Value::String(m)) => m.as_str(),
        Some(_) => return Err(bad(CMD, "`model` must be a string")),
    };
    if !matches!(model, "auto" | "patchMatch" | "contentAware" | "diffusion" | "lama" | "nanoBanana") {
        return Err(bad(CMD, format!("unknown model `{model}`")));
    }
    let st = s.active().ok_or(EngineError::NoDocument)?;
    if !matches!(st.doc.mode, ColorMode::Rgb | ColorMode::Grayscale) {
        return Err(bad(CMD, "Generative Fill requires an RGB or Grayscale document"));
    }
    let sel = st.doc.selection.as_ref().ok_or_else(|| bad(CMD, "make a selection first"))?;
    let hole_bounds = sel.content_bounds().intersect(&st.doc.bounds());
    if hole_bounds.is_empty() {
        return Err(bad(CMD, "the selection does not overlap the canvas"));
    }
    if !matches!(model, "patchMatch" | "contentAware") {
        return Err(EngineError::Other(format!("can't use that model (`{model}`): ML models are unavailable; pass \"patchMatch\" or \"contentAware\"")));
    }

    let canvas = st.doc.bounds();
    let window = crate::fill_cmds::sampling_window(hole_bounds, canvas);
    let (w, h) = crate::fill_cmds::window_size(window)?;
    let fmt = st.doc.pixel_format();
    let n = fmt.channels();
    let comp = photocraft_compose::render(&st.doc, window);
    let mut img = vec![0.0f32; w * h * n];
    let mut enc = [0.0f32; 8];
    for (i, px) in comp.px.iter().enumerate() {
        from_rgba_into(&fmt, *px, &mut enc);
        img[i * n..(i + 1) * n].copy_from_slice(&enc[..n]);
    }

    let mut hole = vec![false; w * h];
    let mut source = vec![false; w * h];
    let mut cover = vec![0.0f32; w * h];
    let sel_samples = photocraft_algo::selection::mask_from_surface(Some(sel), window);
    for (i, (&c, (hv, sv))) in sel_samples.iter().zip(hole.iter_mut().zip(source.iter_mut())).enumerate() {
        cover[i] = c;
        *hv = c > 0.0;
        *sv = !*hv;
    }
    if !source.iter().any(|&ok| ok) {
        return Err(bad(CMD, "nothing outside the selection to sample from"));
    }

    let seed = int_i32(CMD, p, "seed")?.map_or(1u64, |v| u64::from(v.unsigned_abs()));
    let opts = FillOptions { color_adaptation: color_level("default"), seed, ..Default::default() };
    let filled = fill(w, h, n, &img, &hole, &source, &opts);

    let name = p.get("name").and_then(Value::as_str).filter(|s| !s.trim().is_empty()).map(str::to_string).unwrap_or_else(|| "Generative Fill".to_string());
    let id = s.edit("Generative Fill", |doc, active| {
        let mut layer = Layer::raster(doc.next_layer_name(&name), fmt);
        if let Some(surf) = layer.surface_mut() {
            let mut out = vec![0.0f32; w * h * n];
            for ((dst, src), &c) in out.chunks_exact_mut(n).zip(filled.chunks_exact(n)).zip(&cover) {
                if c > 0.0 {
                    dst.copy_from_slice(src);
                    if fmt.alpha
                        && let Some(a) = dst.last_mut()
                    {
                        *a *= c;
                    }
                }
            }
            surf.try_write_region(window, &out)?;
        }
        let nid = crate::layer_place::insert_new(doc, *active, layer, crate::layer_place::Place::Auto);
        *active = Some(nid);
        Ok(nid)
    })?;
    Ok(json!({ "layer": id.0, "model": model, "bounds": [window.x0, window.y0, window.width(), window.height()] }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "layer.toggleLockAll",
            label: "Lock All Layers",
            menu: &["Layer"],
            shortcut: None,
            params: r##"{"layer":id?,"all":bool?=false} -> {locked:bool}"##,
            enabled: has_doc,
            run: toggle_lock_all,
            journal: true,
        },
        CommandSpec {
            id: "layer.selectToTop",
            label: "Select to Top Layer",
            menu: &[],
            shortcut: None,
            params: "{} -> {layer,layers}",
            enabled: has_layers,
            run: |s, _| select_range_to_end(s, true),
            journal: false,
        },
        CommandSpec {
            id: "layer.selectToBottom",
            label: "Select to Bottom Layer",
            menu: &[],
            shortcut: None,
            params: "{} -> {layer,layers}",
            enabled: has_layers,
            run: |s, _| select_range_to_end(s, false),
            journal: false,
        },
        CommandSpec {
            id: "layer.newAdjustmentLayer.grain",
            label: "Grain…",
            menu: &["Layer", "New Adjustment Layer"],
            shortcut: None,
            params: r##"{"amount":0..100=0,"size":0..100=34,"roughness":0..100=50}"##,
            enabled: has_doc,
            run: new_grain_adjustment,
            journal: true,
        },
        CommandSpec {
            id: "edit.generativeFill",
            label: "Generative Fill…",
            menu: &["Edit"],
            shortcut: None,
            params: r##"{"prompt":str?,"model":"auto|patchMatch|contentAware|diffusion|lama|nanoBanana"="auto","seed":u32?} -> {layer,model,bounds}"##,
            enabled: has_selection,
            run: generative_fill,
            journal: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use photocraft_doc::{Adjustment, LayerContent, LayerId};
    use serde_json::json;

    use super::*;

    fn session(size: u32) -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": size, "height": size, "background": "white"})).unwrap();
        s
    }

    #[test]
    fn command_slash_toggles_lock_all_but_not_on_the_background() {
        let mut s = session(8);
        let bg = s.active().unwrap().doc.layers[0].id;
        s.execute("layer.new.layer", json!({})).unwrap();
        let top = s.active().unwrap().active_layer.unwrap();
        assert_eq!(s.execute("layer.toggleLockAll", json!({})).unwrap()["locked"], true);
        assert!(s.active().unwrap().doc.layer(top).unwrap().locks.all);
        assert_eq!(s.execute("layer.toggleLockAll", json!({})).unwrap()["locked"], false);
        assert!(!s.active().unwrap().doc.layer(top).unwrap().locks.all);
        let err = s.execute("layer.toggleLockAll", json!({"layer": bg.0})).unwrap_err();
        assert!(err.to_string().contains("Background"));
        assert!(s.execute("layer.toggleLockAll", json!({"layer": u64::MAX})).is_err());
    }

    #[test]
    fn range_steps_select_the_run_to_the_top_or_bottom() {
        let mut s = session(8);
        let bg = s.active().unwrap().doc.layers[0].id;
        let id = |s: &mut Session, name: &str| LayerId(s.execute("layer.new.layer", json!({"name": name})).unwrap()["layer"].as_u64().unwrap());
        let a = id(&mut s, "A");
        let b = id(&mut s, "B");
        let c = id(&mut s, "C");
        s.execute("layer.select", json!({"layer": b.0})).unwrap();
        s.execute("layer.selectToTop", json!({})).unwrap();
        let mut sel = s.active().unwrap().selected_layers();
        sel.sort();
        let mut want = vec![b, c];
        want.sort();
        assert_eq!((s.active().unwrap().active_layer, sel), (Some(c), want));
        assert_eq!(s.active().unwrap().layer_anchor, Some(b));
        s.execute("layer.select", json!({"layer": b.0})).unwrap();
        s.execute("layer.selectToBottom", json!({})).unwrap();
        let mut sel = s.active().unwrap().selected_layers();
        sel.sort();
        let mut want = vec![bg, a, b];
        want.sort();
        assert_eq!((s.active().unwrap().active_layer, sel), (Some(bg), want));
    }

    #[test]
    fn grain_adjustment_layer_and_generative_fill_validation() {
        let mut s = session(32);
        s.execute("layer.newAdjustmentLayer.grain", json!({})).unwrap();
        let id = s.active().unwrap().active_layer.unwrap();
        assert_eq!(
            s.active().unwrap().doc.layer(id).unwrap().content,
            LayerContent::Adjustment(Adjustment::Grain { amount: 0.0, size: 34.0, roughness: 50.0 })
        );
        assert!(crate::commands::find("layer.newAdjustmentLayer.grain").is_some());

        assert!(s.execute("edit.generativeFill", json!({})).is_err());
        s.execute("select.rect", json!({"x": 4, "y": 4, "width": 12, "height": 12})).unwrap();
        for bad_req in [json!({"model": "nope"}), json!({"model": 1}), json!({"prompt": 5})] {
            assert!(s.execute("edit.generativeFill", bad_req).is_err());
        }
        let err = s.execute("edit.generativeFill", json!({"prompt": "a cat"})).unwrap_err().to_string();
        assert!(err.contains("can't use that model"), "{err}");
        let res = s.execute("edit.generativeFill", json!({"model": "patchMatch"})).unwrap();
        assert_eq!(res["model"], "patchMatch");
    }
}
