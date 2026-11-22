use super::editing::edit_home;
use super::insertion_style;
use super::{LayerId, PhotocraftApp, TextLayer, hex, json, text_layer};

/// Representative for the displayed properties: the edit layer, else a selected type layer.
pub(crate) fn target(app: &PhotocraftApp) -> Option<(u64, Option<[usize; 2]>)> {
    if let Some(ed) = &app.ui.text_edit {
        let (a, b) = (ed.caret.min(ed.anchor), ed.caret.max(ed.anchor));
        return Some((ed.layer, (a < b).then_some([a, b])));
    }
    let st = app.session.active()?;
    let id = st
        .active_layer
        .filter(|id| text_layer(&st.doc, *id).is_some())
        .or_else(|| st.selected_layers().into_iter().find(|id| text_layer(&st.doc, *id).is_some()))?;
    Some((id.0, None))
}

/// The type layer whose properties the Type controls and dialogs show (see [`target`]).
pub(crate) fn target_text(app: &PhotocraftApp) -> Option<&TextLayer> {
    let (id, _) = target(app)?;
    text_layer(&target_doc(app)?.doc, LayerId(id))
}

/// The document [`target`]'s layer is in: a text edit's own, which can stay in a background
/// document where the same layer id may name another layer, else the active one.
pub(super) fn target_doc(app: &PhotocraftApp) -> Option<&photocraft_engine::DocState> {
    match &app.ui.text_edit {
        Some(ed) => app.session.documents().get(edit_home(app, ed)?),
        None => app.session.active(),
    }
}

/// Snapshot the mutation scope separately from the representative used to display properties.
/// An inline edit remains on its layer/range; layer selection formats every selected type layer.
pub(crate) fn formatting_params(app: &PhotocraftApp) -> Option<serde_json::Value> {
    if let Some(ed) = &app.ui.text_edit {
        let mut params = json!({"layer": ed.layer, "coalesce": ed.session});
        let (a, b) = (ed.caret.min(ed.anchor), ed.caret.max(ed.anchor));
        if a < b {
            params["range"] = json!([a, b]);
        }
        return Some(params);
    }
    let st = app.session.active()?;
    let layers: Vec<u64> = st.selected_layers().into_iter().filter(|id| text_layer(&st.doc, *id).is_some()).map(|id| id.0).collect();
    (!layers.is_empty()).then(|| json!({"layers": layers}))
}

/// Scale of the target layer's transform. Like Photoshop, sizes show and edit as the layer
/// appears: a 12 pt layer scaled 200% (common in PSDs) reads 24 pt.
pub(super) fn shown_scale(app: &PhotocraftApp) -> f32 {
    let Some((id, _)) = target(app) else { return 1.0 };
    let Some(t) = app.session.active().and_then(|st| text_layer(&st.doc, LayerId(id))) else { return 1.0 };
    let m = t.transform.m;
    let k = (m[0] * m[3] - m[1] * m[2]).abs().sqrt() as f32;
    if k.is_finite() && k > 1e-3 { k } else { 1.0 }
}

/// The display unit for type metrics (Preferences ▸ Units & Rulers ▸ Type): the multiplier from
/// the model's points to the unit, and its label. Pixels follow the document's resolution.
pub(crate) fn type_unit(app: &PhotocraftApp) -> (f32, &'static str) {
    let dpi = app.session.active().map_or(72.0, |d| d.doc.resolution_dpi.max(1.0));
    match app.session.prefs().units_and_rulers.type_units {
        photocraft_engine::prefs::TypeUnit::Points => (1.0, "pt"),
        photocraft_engine::prefs::TypeUnit::Pixels => (dpi / 72.0, "px"),
        photocraft_engine::prefs::TypeUnit::Millimeters => (25.4 / 72.0, "mm"),
    }
}

/// Coalesce key while a field is being scrubbed: every step of one drag shares one history step
/// (Photoshop's single step per scrub, #124). Unique per press, so two drags are two steps.
fn drag_key(ctx: &egui::Context) -> Option<String> {
    let id = ctx.dragged_id()?;
    let t = ctx.input(|i| i.pointer.press_start_time())?;
    Some(format!("type-drag-{}-{}", id.value(), t.to_bits()))
}

/// The styles a new Type-tool layer will receive. Use the same command parameters for the
/// panels and creation so defaults also survive action recording/replay. The older options-bar
/// fields remain authoritative, and text colour continues to follow the foreground colour.
pub(super) fn default_style_params(app: &PhotocraftApp) -> serde_json::Value {
    let o = &app.ui.tool_options;
    let mut p = json!({});
    // Settings are editable JSON: only recognized formatting keys may reach type.create.
    // In particular a stray `text`, `box`, or `coalesce` key must not become a create parameter.
    let (mut c, mut para) = (photocraft_doc::text::CharStyle::default(), photocraft_doc::text::ParagraphStyle::default());
    for (key, value) in &o.type_style_defaults {
        // These belong to the existing options-bar fields. Apart from being redundant,
        // parsing a saved font key here would wait for the background font scan.
        if matches!(key.as_str(), "font" | "family" | "fontStyle" | "weight" | "italic" | "postscriptName") {
            continue;
        }
        let one = json!({ (key): value });
        if !finite_style_value(value)
            || photocraft_engine::type_cmds::check_size_tracking(&one).is_err()
            || photocraft_engine::type_cmds::check_kerning(&one).is_err()
        {
            continue;
        }
        if photocraft_engine::type_cmds::apply_char_props(&mut c, &one) | photocraft_engine::type_cmds::apply_para_props(&mut para, &one) {
            p[key] = value.clone();
        }
    }
    p["font"] = json!(o.type_font);
    p["fontStyle"] = json!(o.type_style);
    p["size"] = json!(o.type_size);
    p["align"] = json!(o.type_align);
    p["direction"] = json!(o.type_direction);
    p["color"] = json!(hex(app.session.tools.foreground));
    p
}

/// Style numbers become f32 in the engine. JSON itself permits finite f64 values too large
/// for that conversion; a hand-edited setting must not inject infinity into a new layer.
/// Style collections (features, axes, colour) contain only scalar values, so no recursion.
fn finite_style_value(value: &serde_json::Value) -> bool {
    let scalar = |v: &serde_json::Value| match v {
        serde_json::Value::Number(n) => n.as_f64().is_some_and(|n| (n as f32).is_finite()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => false,
        _ => true,
    };
    match value {
        serde_json::Value::Array(v) => v.iter().all(scalar),
        serde_json::Value::Object(v) => v.values().all(scalar),
        v => scalar(v),
    }
}

pub(super) fn default_styles(app: &PhotocraftApp) -> (photocraft_doc::text::CharStyle, photocraft_doc::text::ParagraphStyle) {
    let (mut c, mut para) = app.session.type_defaults.clone().unwrap_or_default();
    let mut p = default_style_params(app);
    // The saved panels can appear on the very first frame. Style parsing resolves the named
    // font through shared(), which waits for the font scan; display the saved names meanwhile,
    // and let fonts_ready/axis_sliders expose the font controls when try_shared is ready.
    if photocraft_text::try_shared().is_none() {
        c.font_family = app.ui.tool_options.type_font.clone();
        c.font_style = app.ui.tool_options.type_style.clone();
        c.postscript_name = None;
        if let Some(p) = p.as_object_mut() {
            p.remove("font");
            p.remove("fontStyle");
        }
    }
    photocraft_engine::type_cmds::apply_char_props(&mut c, &p);
    photocraft_engine::type_cmds::apply_para_props(&mut para, &p);
    (c, para)
}

/// Change tool defaults without creating a document edit. The engine owns the style schema;
/// keep its property names rather than maintaining another style parser in the shell.
pub(super) fn apply_defaults(app: &mut PhotocraftApp, props: &serde_json::Value) {
    let Some(props) = props.as_object() else { return };
    let o = &mut app.ui.tool_options;
    for (key, value) in props {
        match key.as_str() {
            "font" => {
                if let Some(v) = value.as_str() {
                    o.type_font = v.into();
                }
            }
            "fontStyle" => {
                if let Some(v) = value.as_str() {
                    o.type_style = v.into();
                    // A newly chosen named style replaces a weight-axis setting, as it does
                    // in apply_char_props. Other axes retain their independently chosen values.
                    if let Some(axes) = o.type_style_defaults.get_mut("variations").and_then(serde_json::Value::as_object_mut) {
                        axes.remove("wght");
                    }
                }
            }
            "size" => {
                if let Some(v) = value.as_f64() {
                    o.type_size = v as f32;
                }
            }
            "align" => {
                if let Some(v) = value.as_str() {
                    o.type_align = v.into();
                }
            }
            "direction" => {
                if let Some(v) = value.as_str() {
                    o.type_direction = v.into();
                }
            }
            "metricsAsShown" => {} // Defaults have no layer transform.
            "features" | "variations" => {
                if let Some(patch) = value.as_object() {
                    let old = o.type_style_defaults.entry(key.clone()).or_insert_with(|| json!({}));
                    if let Some(old) = old.as_object_mut() {
                        old.extend(patch.clone());
                    } else {
                        *old = value.clone();
                    }
                }
            }
            _ => {
                o.type_style_defaults.insert(key.clone(), value.clone());
            }
        }
    }
}

/// The active SFX layer when no inline text edit or standard Type layer is targeted.
/// Lets the native Character dock and Type Options Bar edit SFX font, size, leading,
/// tracking, scale, orientation and color directly.
pub(super) fn target_sfx(app: &PhotocraftApp) -> Option<(u64, photocraft_doc::sfx::SfxData)> {
    if app.ui.text_edit.is_some() {
        return None;
    }
    let st = app.session.active()?;
    let id = st.active_layer?;
    let l = st.doc.layer(id)?;
    if matches!(l.content, photocraft_doc::LayerContent::Text(_)) {
        return None;
    }
    l.sfx.as_deref().cloned().map(|sfx| (id.0, sfx))
}

fn sfx_style_label(weight: u16, italic: bool) -> String {
    let base = match weight {
        ..=150 => "Thin",
        151..=250 => "ExtraLight",
        251..=350 => "Light",
        351..=450 => "Regular",
        451..=550 => "Medium",
        551..=650 => "SemiBold",
        651..=750 => "Bold",
        751..=850 => "ExtraBold",
        _ => "Black",
    };
    match (base, italic) {
        ("Regular", true) => "Italic".to_owned(),
        (w, true) => format!("{w} Italic"),
        (w, false) => w.to_owned(),
    }
}

/// Synthesizes a `CharStyle` / `ParagraphStyle` view of the active SFX layer so the native
/// Character dock displays its typography metrics live.
pub(super) fn sfx_styles_at(app: &PhotocraftApp) -> Option<(photocraft_doc::text::CharStyle, photocraft_doc::text::ParagraphStyle)> {
    let (_, sfx) = target_sfx(app)?;
    let mut c = photocraft_doc::text::CharStyle::default();
    c.font_family = sfx.font_family.clone();
    c.font_style = if sfx.font_style.trim().is_empty() { sfx_style_label(sfx.weight, sfx.italic) } else { sfx.font_style.clone() };
    c.postscript_name = sfx.postscript_name.clone();
    c.weight = sfx.weight;
    c.italic = sfx.italic;
    c.size_pt = sfx.size_pt;
    c.leading_pt = Some((sfx.size_pt * sfx.leading_scale).max(0.1));
    c.tracking = sfx.tracking;
    c.horizontal_scale = (sfx.free_transform.scale_x as f32).clamp(0.1, 12.0);
    c.vertical_scale = (sfx.free_transform.scale_y as f32).clamp(0.1, 12.0);
    c.faux_bold = sfx.weight >= 700;
    c.faux_italic = sfx.italic;
    c.color = match &sfx.fill {
        photocraft_doc::sfx::SfxFillSource::Solid { color } => *color,
        photocraft_doc::sfx::SfxFillSource::Gradient { start, .. } => *start,
        photocraft_doc::sfx::SfxFillSource::ShtackHatch { spec } => spec.color,
        photocraft_doc::sfx::SfxFillSource::None | photocraft_doc::sfx::SfxFillSource::Screentone { .. } => sfx.line_effect.line_color,
    };
    Some((c, photocraft_doc::text::ParagraphStyle::default()))
}

/// Applies Character-dock property updates onto the active SFX layer via `sfx.update`.
fn apply_sfx(app: &mut PhotocraftApp, ctx: &egui::Context, layer_id: u64, mut sfx: photocraft_doc::sfx::SfxData, props: &serde_json::Value) {
    let Some(obj) = props.as_object() else { return };
    let mut c = sfx_styles_at(app).map(|(c, _)| c).unwrap_or_default();
    let changed_char = photocraft_engine::type_cmds::apply_char_props(&mut c, props);
    let mut touched = false;

    if obj.contains_key("font") || obj.contains_key("family") || obj.contains_key("fontStyle") || obj.contains_key("weight") || obj.contains_key("italic") || obj.contains_key("postscriptName") {
        sfx.font_family = c.font_family.clone();
        sfx.font_style = c.font_style.clone();
        sfx.postscript_name = c.postscript_name.clone();
        sfx.weight = c.weight;
        sfx.italic = c.italic;
        touched = true;
    }
    if obj.contains_key("variations")
        && let Some(wght) = c.variations.iter().find(|v| v.axis == "wght").map(|v| v.value)
    {
        sfx.weight = wght.round().clamp(100.0, 900.0) as u16;
        touched = true;
    }
    if obj.contains_key("fauxBold") {
        sfx.weight = if c.faux_bold { 800 } else { 400 };
        sfx.font_style = sfx_style_label(sfx.weight, sfx.italic);
        sfx.postscript_name = None;
        touched = true;
    }
    if obj.contains_key("fauxItalic") {
        sfx.italic = c.faux_italic;
        sfx.font_style = sfx_style_label(sfx.weight, sfx.italic);
        sfx.postscript_name = None;
        touched = true;
    }
    if obj.contains_key("size") {
        sfx.size_pt = c.size_pt;
        touched = true;
    }
    if obj.contains_key("leading") {
        sfx.leading_scale = match c.leading_pt {
            Some(pt) if sfx.size_pt > 1e-3 => (pt / sfx.size_pt).clamp(0.4, 4.0),
            _ => 1.05,
        };
        touched = true;
    }
    if obj.contains_key("tracking") {
        sfx.tracking = c.tracking;
        touched = true;
    }
    if obj.contains_key("horizontalScale") {
        sfx.free_transform.scale_x = f64::from(c.horizontal_scale).clamp(0.1, 12.0);
        touched = true;
    }
    if obj.contains_key("verticalScale") {
        sfx.free_transform.scale_y = f64::from(c.vertical_scale).clamp(0.1, 12.0);
        touched = true;
    }
    if obj.contains_key("color") && changed_char {
        match &mut sfx.fill {
            photocraft_doc::sfx::SfxFillSource::Solid { color } => *color = c.color,
            photocraft_doc::sfx::SfxFillSource::Gradient { start, .. } => *start = c.color,
            photocraft_doc::sfx::SfxFillSource::ShtackHatch { spec } => spec.color = c.color,
            photocraft_doc::sfx::SfxFillSource::None | photocraft_doc::sfx::SfxFillSource::Screentone { .. } => {
                sfx.line_effect.line_color = c.color;
            }
        }
        touched = true;
    }
    if !touched {
        return;
    }
    sfx.validate();
    let pointer_down = ctx.input(|i| i.pointer.primary_down());
    let doc_id = app.session.active().map(|st| photocraft_manga_ui::BridgeDocId(st.doc.id.0));
    let coalesce = drag_key(ctx).unwrap_or_else(|| format!("sfx-char:{layer_id}"));
    if pointer_down && let Some(did) = doc_id {
        app.manga.ui.sfx_panel.pending_full_render = true;
        app.manga.ui.sfx_panel.pending_full_target = Some((did, LayerId(layer_id)));
    }
    let _ = app.run(
        "sfx.update",
        json!({
            "layer": layer_id,
            "sfx": sfx,
            "coalesce": coalesce,
            "preview": pointer_down,
        }),
    );
}

/// Apply only the supplied properties to the text-edit scope or selected type layers; without
/// a type target, edit the active SFX layer if one is selected, else prepare the defaults for
/// the next Type-tool layer.
pub(super) fn apply(app: &mut PhotocraftApp, ctx: &egui::Context, props: serde_json::Value) {
    if insertion_style::set_font(app, &props) {
        return;
    }
    app.ui.type_insertion_style = None;
    let Some(mut p) = formatting_params(app) else {
        if let Some((layer_id, sfx)) = target_sfx(app) {
            apply_sfx(app, ctx, layer_id, sfx, &props);
            return;
        }
        apply_defaults(app, &props);
        return;
    };
    if let Some(props) = props.as_object() {
        for (key, value) in props {
            p[key] = value.clone();
        }
    }
    if let Some(key) = app.ui.text_edit.as_ref().map(|ed| ed.session.clone()).or_else(|| drag_key(ctx)) {
        p["coalesce"] = json!(key);
    }
    let _ = app.run("type.setStyle", p);
}

/// Open the shared Color Picker with a snapshot of the text target. Sampling or cancelling
/// the dialog never changes the text selection, tool colours, or document history.
pub fn open_color_picker(app: &mut PhotocraftApp, rgb: [f32; 3]) -> u64 {
    if let Some(params) = formatting_params(app) {
        crate::color_picker_ui::open_for_command(app, "Color Picker (Text Color)", rgb, "type.setStyle", params)
    } else if let Some((layer_id, _)) = target_sfx(app) {
        crate::color_picker_ui::open_for_command(app, "Color Picker (SFX Color)", rgb, "sfx.update", json!({"layer": layer_id}))
    } else {
        crate::color_picker_ui::open(app, "foreground")
    }
}

/// While characters are selected in a type layer, a new foreground colour (Color and Swatches
/// panels, the Color Picker) recolours them, in the editing session's history step.
pub fn foreground_changed(app: &mut PhotocraftApp) {
    let Some((layer, Some(range))) = target(app) else { return };
    let Some(ed) = app.ui.text_edit.as_ref() else { return };
    let p = json!({"layer": layer, "range": range, "color": hex(app.session.tools.foreground), "coalesce": ed.session});
    let _ = app.run("type.setStyle", p);
}
