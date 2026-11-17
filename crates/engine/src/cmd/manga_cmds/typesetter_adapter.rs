//! Adapter bridging `photocraft-manga-core`'s pure typesetter with `photocraft-io::tpl` and `photocraft-compose`.

use craft_psd::descriptor::{Descriptor, Value};
use photocraft_color::PixelFormat;
use photocraft_doc::Document;
use photocraft_geom::Rect;
use photocraft_io::tpl;
use photocraft_manga_core::snow_typesetter::{
    MeasurementSystem, TextStylePreset, TplImportReport, TypesetterState, detect_speech_bubble_bounds_in_surface, tpl_display_name,
};

fn normalize_orientation(value: &str) -> String {
    if value.trim().eq_ignore_ascii_case("vertical") || value.trim().eq_ignore_ascii_case("vrtc") { "vertical".to_string() } else { "horizontal".to_string() }
}

fn tpl_descriptor<'a>(descriptor: &'a Descriptor, key: &str) -> Option<&'a Descriptor> {
    match descriptor.get(key) {
        Some(Value::Descriptor(inner) | Value::GlobalObject(inner)) => Some(inner),
        _ => None,
    }
}

fn tpl_text(value: &Value) -> Option<String> {
    match value {
        Value::Text(s) => {
            let clean = s.to_string_lossy().trim_end_matches('\0').trim().to_string();
            (!clean.is_empty()).then_some(clean)
        }
        Value::Enumerated { value, .. } => {
            let clean = String::from_utf8_lossy(value.as_bytes()).trim().to_string();
            (!clean.is_empty()).then_some(clean)
        }
        Value::Class(c) | Value::GlobalClass(c) => {
            let clean = String::from_utf8_lossy(c.class_id.as_bytes()).trim().to_string();
            (!clean.is_empty()).then_some(clean)
        }
        _ => None,
    }
}

fn tpl_text_any(descriptor: &Descriptor, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| descriptor.get(key).and_then(tpl_text))
}

fn tpl_number_any(descriptor: &Descriptor, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| descriptor.get(key).and_then(Value::as_number))
}

fn tpl_bool_any(descriptor: &Descriptor, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|key| match key_val(descriptor, key) {
        Some(Value::Boolean(b)) => Some(*b),
        _ => None,
    })
}

fn key_val<'a>(descriptor: &'a Descriptor, key: &str) -> Option<&'a Value> {
    descriptor.get(key)
}

fn tpl_rgb(descriptor: &Descriptor) -> Option<[u8; 4]> {
    let color = tpl_descriptor(descriptor, "Clr ").or_else(|| tpl_descriptor(descriptor, "color"))?;
    let channel = |keys: &[&str], float_keys: &[&str]| -> Option<u8> {
        if let Some(v) = tpl_number_any(color, float_keys).filter(|v| v.is_finite()) {
            return Some((v * 255.0).round().clamp(0.0, 255.0) as u8);
        }
        tpl_number_any(color, keys).filter(|v| v.is_finite()).map(|v| v.round().clamp(0.0, 255.0) as u8)
    };
    let r = channel(&["Rd  ", "red"], &["redFloat"])?;
    let g = channel(&["Grn ", "green"], &["greenFloat"])?;
    let b = channel(&["Bl  ", "blue"], &["blueFloat"])?;
    Some([r, g, b, 255])
}

fn tpl_anti_alias(character: &Descriptor) -> &'static str {
    match tpl_number_any(character, &["AntA"]) {
        Some(value) if value.is_finite() => match value.round() as i64 {
            0 => "none",
            1 => "crisp",
            2 => "strong",
            8 => "smooth",
            16 => "sharp",
            _ => "smooth",
        },
        _ => "smooth",
    }
}

fn preset_from_tool(tool: &tpl::ToolPreset, folder: Option<&str>) -> Option<(TextStylePreset, bool)> {
    if !matches!(tool.tool.as_str(), "typeCreateOrEditTool" | "typeVerticalCreateOrEditTool") {
        return None;
    }
    let character = tpl_descriptor(&tool.settings, "textToolCharacterOptions")
        .and_then(|o| tpl_descriptor(o, "TxtS"))
        .or_else(|| tpl_descriptor(&tool.settings, "TxtS"))?;
    let font_size = tpl_number_any(character, &["Sz  ", "size"]).filter(|v| v.is_finite() && *v > 0.0)? as f32;
    let font_family = tpl_text_any(character, &["FntN", "fontName"]);
    let font_postscript = tpl_text_any(character, &["fontPostScriptName"]);
    if font_family.is_none() && font_postscript.is_none() {
        return None;
    }

    let paragraph_options = tpl_descriptor(&tool.settings, "textToolParagraphOptions");
    let raw_ornt = paragraph_options.and_then(|o| tpl_text_any(o, &["Ornt"])).unwrap_or_default();
    let is_vertical = tool.tool == "typeVerticalCreateOrEditTool" || raw_ornt.eq_ignore_ascii_case("Vrtc");
    let orientation = if is_vertical { "vertical".to_string() } else { normalize_orientation(&raw_ornt) };

    let auto_leading = tpl_bool_any(character, &["autoLeading"]);
    let leading = match tpl_number_any(character, &["Ldng", "leading"]) {
        Some(v) if v.is_finite() && v > 0.0 && auto_leading != Some(true) => (v as f32).clamp(0.1, 1296.0),
        _ => font_size * 1.2,
    };
    let tracking = tpl_number_any(character, &["Trck", "tracking"]).filter(|v| v.is_finite()).map_or(0, |v| v.round().clamp(-1000.0, 10000.0) as i32);
    let horizontal_scale =
        tpl_number_any(character, &["HrzS", "horizontalScale"]).filter(|v| v.is_finite() && *v > 0.0).map_or(100.0, |v| (v as f32).clamp(1.0, 1000.0));
    let font_style = tpl_text_any(character, &["FntS", "fontStyleName"]).unwrap_or_else(|| "Regular".into());
    let caps = tpl_text_any(character, &["fontCaps"]).unwrap_or_default();
    let kerning = tpl_text_any(character, &["AtKr", "autoKern"]).unwrap_or_default();
    let font_family = font_family.or_else(|| font_postscript.clone()).unwrap_or_default();
    let font_postscript = font_postscript.unwrap_or_else(|| font_family.clone());
    let lower_style = font_style.to_ascii_lowercase();
    let bold = tpl_bool_any(character, &["syntheticBold"]).unwrap_or(false) || lower_style.contains("bold");
    let italic = tpl_bool_any(character, &["syntheticItalic"]).unwrap_or(false) || lower_style.contains("italic") || lower_style.contains("oblique");

    let mut preset = TextStylePreset {
        name: tpl_display_name(&tool.name),
        folder: folder.map(str::to_string),
        font_family,
        font_postscript,
        font_style,
        font_size,
        leading,
        tracking,
        horizontal_scale,
        bold,
        italic,
        all_caps: caps.eq_ignore_ascii_case("allCaps") || caps.eq_ignore_ascii_case("all"),
        small_caps: caps.eq_ignore_ascii_case("smallCaps"),
        text_color: tpl_rgb(character).unwrap_or([0, 0, 0, 255]),
        stroke_width: 0.0,
        anti_alias: tpl_anti_alias(character).to_string(),
        measurement_system: if kerning.eq_ignore_ascii_case("opticalKern") { MeasurementSystem::Optical } else { MeasurementSystem::Metric },
        orientation,
        ..TextStylePreset::default()
    };
    if preset.name.trim().is_empty() {
        preset.name = "Imported Style".into();
    }
    Some((preset, is_vertical))
}

/// Imports text tool presets from an Adobe Photoshop `.tpl` buffer into `state`.
pub fn import_tool_presets_tpl(state: &mut TypesetterState, data: &[u8], folder: &str) -> Result<TplImportReport, String> {
    let parsed = tpl::parse(data).map_err(|e| format!("Failed to parse .tpl file: {e}"))?;
    let folder = (!folder.trim().is_empty()).then_some(folder.trim());
    let mut imported_list = Vec::new();
    let mut skipped = 0usize;
    let mut vertical_imported = 0usize;

    for tool in &parsed.presets {
        match preset_from_tool(tool, folder) {
            Some((preset, is_vertical)) => {
                if is_vertical {
                    vertical_imported = vertical_imported.saturating_add(1);
                }
                imported_list.push(preset);
            }
            None => skipped = skipped.saturating_add(1),
        }
    }

    if imported_list.is_empty() {
        return Err("No compatible text tool presets found in .tpl file".to_string());
    }
    let imported = state.push_imported_presets(imported_list);
    Ok(TplImportReport { imported, skipped, vertical_imported })
}

/// Detects the closed speech-bubble interior bounds around `anchor` in `doc`.
pub fn detect_speech_bubble_bounds(state: &TypesetterState, doc: &Document, anchor: (f32, f32)) -> Option<Rect> {
    let (region, anchor_x, anchor_y, radius) = state.speech_bubble_search_region(doc, anchor)?;
    let surface = photocraft_compose::composite_to_surface(doc, region, PixelFormat::RGBA32F, None);
    detect_speech_bubble_bounds_in_surface(&surface, anchor_x, anchor_y, radius)
}

#[cfg(test)]
mod tests {
    use craft_psd::descriptor::{Descriptor, Id, UnicodeString, Value};
    use photocraft_color::{ColorMode, PixelFormat, SampleType};
    use photocraft_doc::{Layer, LayerContent};
    use photocraft_geom::{Rect, Size};
    use photocraft_manga_core::snow_typesetter::detect_speech_bubble_bounds_in_surface;
    use photocraft_raster::Surface;

    use super::*;

    fn append_tpl_entry(section: &mut Vec<u8>, name_value: &str, descriptor: Descriptor) {
        let units: Vec<u16> = name_value.encode_utf16().collect();
        section.extend_from_slice(&(units.len() as u32).to_be_bytes());
        for unit in units {
            section.extend_from_slice(&unit.to_be_bytes());
        }
        section.extend_from_slice(&16u32.to_be_bytes());
        section.extend(descriptor.to_bytes());
    }

    fn tpl_file(section: Vec<u8>) -> Vec<u8> {
        let mut out = b"8BTP".to_vec();
        out.extend_from_slice(&2u32.to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(b"8BIMtptp");
        out.extend_from_slice(&(section.len() as u32).to_be_bytes());
        out.extend(section);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        out
    }

    #[test]
    fn tpl_import_maps_photoshop_text_preset_and_reports_skips() {
        let character = Descriptor::new("TxtS")
            .with("fontPostScriptName", Value::Text(UnicodeString::new_nul("ExampleSans-Bold")))
            .with("FntN", Value::Text(UnicodeString::new_nul("Example Sans")))
            .with("FntS", Value::Text(UnicodeString::new_nul("Bold")))
            .with("Sz  ", Value::UnitFloat { unit: *b"#Pnt", value: 18.0 })
            .with("Ldng", Value::UnitFloat { unit: *b"#Pnt", value: 22.0 })
            .with("autoLeading", Value::Boolean(false))
            .with("Trck", Value::Integer(25))
            .with("HrzS", Value::Double(92.0))
            .with("AntA", Value::Integer(16))
            .with("syntheticItalic", Value::Boolean(true))
            .with("fontCaps", Value::Enumerated { type_id: Id::new("fontCaps"), value: Id::new("AllCaps") })
            .with("AtKr", Value::Enumerated { type_id: Id::new("AtKr"), value: Id::new("opticalKern") })
            .with(
                "Clr ",
                Value::Descriptor(
                    Descriptor::new("RGBC").with("Rd  ", Value::Double(200.4)).with("Grn ", Value::Double(30.0)).with("Bl  ", Value::Double(10.0)),
                ),
            );
        let type_tool = Descriptor::new("typeCreateOrEditTool")
            .with("textToolCharacterOptions", Value::Descriptor(Descriptor::new("textToolCharacterOptions").with("TxtS", Value::Descriptor(character.clone()))))
            .with(
                "textToolParagraphOptions",
                Value::Descriptor(
                    Descriptor::new("textToolParagraphOptions").with("Ornt", Value::Enumerated { type_id: Id::new("Ornt"), value: Id::new("Hrzn") }),
                ),
            );
        let vertical_tool = Descriptor::new("typeVerticalCreateOrEditTool")
            .with("textToolCharacterOptions", Value::Descriptor(Descriptor::new("textToolCharacterOptions").with("TxtS", Value::Descriptor(character))))
            .with(
                "textToolParagraphOptions",
                Value::Descriptor(
                    Descriptor::new("textToolParagraphOptions").with("Ornt", Value::Enumerated { type_id: Id::new("Ornt"), value: Id::new("Vrtc") }),
                ),
            );

        let mut section = Vec::new();
        section.extend_from_slice(&3u32.to_be_bytes());
        append_tpl_entry(&mut section, "$$$/Presets/Tools/Text_tpl=Dialogue", type_tool);
        append_tpl_entry(&mut section, "Brush", Descriptor::new("paintbrushTool"));
        append_tpl_entry(&mut section, "Vertical", vertical_tool);
        let data = tpl_file(section);

        let mut state = TypesetterState::default();
        let report = import_tool_presets_tpl(&mut state, &data, "Chapter").unwrap();
        assert_eq!(report, TplImportReport { imported: 2, skipped: 1, vertical_imported: 1 });
        let imported = state.presets.iter().find(|preset| preset.name == "Dialogue").unwrap();
        assert_eq!(imported.folder.as_deref(), Some("Chapter"));
        assert_eq!(imported.font_family, "Example Sans");
        assert_eq!(imported.font_postscript, "ExampleSans-Bold");
        assert!(imported.bold && imported.italic && imported.all_caps);
        assert_eq!(imported.font_size, 18.0);
        assert_eq!(imported.leading, 22.0);
        assert_eq!(imported.tracking, 25);
        assert_eq!(imported.horizontal_scale, 92.0);
        assert_eq!(imported.text_color, [200, 30, 10, 255]);
        assert_eq!(imported.anti_alias, "sharp");
        assert_eq!(imported.measurement_system, MeasurementSystem::Optical);
        assert_eq!(imported.stroke_width, 0.0);
        assert!(state.presets.iter().any(|preset| preset.name == "Vertical" && preset.orientation == "vertical"));
    }

    #[test]
    fn tpl_import_reads_float_rgb_and_rejects_incomplete_or_non_text_libraries() {
        let complete = Descriptor::new("TxtS")
            .with("FntN", Value::Text(UnicodeString::new_nul("Float Colour")))
            .with("Sz  ", Value::UnitFloat { unit: *b"#Pnt", value: 12.0 })
            .with(
                "Clr ",
                Value::Descriptor(
                    Descriptor::new("RGBC").with("redFloat", Value::Double(0.5)).with("greenFloat", Value::Double(0.0)).with("blueFloat", Value::Double(1.0)),
                ),
            );
        let incomplete = Descriptor::new("TxtS").with("FntN", Value::Text(UnicodeString::new_nul("Only a name")));
        let make_tool = |character: Descriptor| {
            Descriptor::new("typeCreateOrEditTool")
                .with("textToolCharacterOptions", Value::Descriptor(Descriptor::new("textToolCharacterOptions").with("TxtS", Value::Descriptor(character))))
        };

        let mut section = Vec::new();
        section.extend_from_slice(&2u32.to_be_bytes());
        append_tpl_entry(&mut section, "Float", make_tool(complete));
        append_tpl_entry(&mut section, "Incomplete", make_tool(incomplete));

        let mut state = TypesetterState::default();
        let report = import_tool_presets_tpl(&mut state, &tpl_file(section), "Float").unwrap();
        assert_eq!(report, TplImportReport { imported: 1, skipped: 1, vertical_imported: 0 });
        assert_eq!(state.presets.last().unwrap().text_color, [128, 0, 255, 255]);

        let mut brush_section = Vec::new();
        brush_section.extend_from_slice(&1u32.to_be_bytes());
        append_tpl_entry(&mut brush_section, "Brush", Descriptor::new("paintbrushTool"));
        assert!(import_tool_presets_tpl(&mut TypesetterState::default(), &tpl_file(brush_section), "Brushes").is_err());
    }

    #[test]
    fn speech_bubble_detection_finds_closed_white_interior_on_document() {
        let mut surface = Surface::new(PixelFormat::RGBA32F);
        surface.fill_rect(Rect::new(0, 0, 240, 240), &[0.0, 0.0, 0.0, 1.0]);
        surface.fill_rect(Rect::new(60, 70, 160, 150), &[1.0, 1.0, 1.0, 1.0]);
        assert!(detect_speech_bubble_bounds_in_surface(&surface, 110, 110, 200).is_some());

        let mut doc = Document::new("Bubble", Size::new(240, 240), ColorMode::Rgb, SampleType::U8);
        doc.layers.push(Layer::new("Page", LayerContent::Raster(surface)));
        let state = TypesetterState::default();
        let bounds = detect_speech_bubble_bounds(&state, &doc, (110.0, 110.0)).expect("bubble bounds detected");
        assert!(bounds.width() >= 90 && bounds.height() >= 70);
        assert_eq!(detect_speech_bubble_bounds(&state, &doc, (f32::NAN, 10.0)), None);
    }
}
