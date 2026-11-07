use photocraft_doc::{Document, Rect};
use photocraft_raster::Surface;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MeasurementSystem {
    Metric,
    Optical,
}

impl Default for MeasurementSystem {
    fn default() -> Self {
        Self::Optical
    }
}

impl MeasurementSystem {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Metric => "metric",
            Self::Optical => "optical",
        }
    }

    pub fn is_optical(&self) -> bool {
        matches!(self, Self::Optical)
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "optical" | "opticalkern" | "visual" => Self::Optical,
            "metric" | "metrics" | "metricskern" | "font" => Self::Metric,
            _ => Self::Optical,
        }
    }
}

pub fn default_text_color() -> [u8; 4] {
    [0, 0, 0, 255]
}

pub fn default_true() -> bool {
    true
}

/// Default paragraph alignment of a text style preset: centered (Photoshop's TypeR default).
pub fn default_align_center() -> String {
    "center".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextStylePreset {
    pub name: String,
    /// Необязательный текст образца в окне Font Settings. Пустая строка — автоматический
    /// образец из имени стиля и демонстрационной фразы.
    #[serde(default)]
    pub preview_text: String,
    pub folder: Option<String>,
    pub font_family: String,
    pub font_postscript: String,
    pub font_style: String,
    #[serde(default = "default_align_center")]
    pub align: String,
    pub font_size: f32,
    pub leading: f32,
    pub tracking: i32,
    pub horizontal_scale: f32,
    pub bold: bool,
    pub italic: bool,
    pub all_caps: bool,
    #[serde(default)]
    pub small_caps: bool,
    #[serde(default)]
    pub superscript: bool,
    #[serde(default)]
    pub subscript: bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub strikethrough: bool,
    pub stroke_width: f32,
    pub stroke_color: [u8; 4],
    #[serde(default = "default_text_color")]
    pub text_color: [u8; 4],
    #[serde(default)]
    pub measurement_system: MeasurementSystem,
    #[serde(default = "default_true")]
    pub hanging_punctuation: bool,
    #[serde(default = "default_anti_alias")]
    pub anti_alias: String,
}

fn default_anti_alias() -> String {
    "crisp".to_string()
}

impl Default for TextStylePreset {
    fn default() -> Self {
        Self {
            name: "Дефолт".to_string(),
            preview_text: String::new(),
            folder: Some("Манга".to_string()),
            font_family: "Kurai Voice 2".to_string(),
            font_postscript: "KuraiVoice2".to_string(),
            font_style: "Regular".to_string(),
            align: default_align_center(),
            font_size: 26.0,
            leading: 30.0,
            tracking: 0,
            horizontal_scale: 100.0,
            bold: false,
            italic: false,
            all_caps: true,
            small_caps: false,
            superscript: false,
            subscript: false,
            underline: false,
            strikethrough: false,
            stroke_width: 2.0,
            stroke_color: [255, 255, 255, 255],
            text_color: [0, 0, 0, 255],
            measurement_system: MeasurementSystem::Optical,
            hanging_punctuation: true,
            anti_alias: "crisp".to_string(),
        }
    }
}

impl TextStylePreset {
    /// Синхронизирует интерлиньяж (leading) пропорционально изменению кегля шрифта (Line Spacing Sync).
    pub fn sync_leading_with_size(&mut self, new_size: f32) {
        let clamped = new_size.clamp(4.0, 500.0);
        if self.font_size > 0.0 {
            let ratio = clamped / self.font_size;
            self.leading = ((self.leading * ratio) * 10.0).round() / 10.0;
        }
        self.font_size = clamped;
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptLine {
    pub raw: String,
    pub text: String,
    pub is_applied: bool,
    pub page: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypesetterOptions {
    pub auto_page_switch: bool,
    pub line_spacing_sync: bool,
    pub strip_character_names: bool,
    pub strip_dash_prefix: bool,
    pub optical_baseline_factor: f32,
    #[serde(default = "default_true")]
    pub hanging_punctuation: bool,
}

impl Default for TypesetterOptions {
    fn default() -> Self {
        Self {
            auto_page_switch: true,
            line_spacing_sync: true,
            strip_character_names: true,
            strip_dash_prefix: true,
            optical_baseline_factor: 0.35,
            hanging_punctuation: true,
        }
    }
}

/// How a contour proposal arranges the dialogue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContourWrapKind {
    /// The line breaks written by the author.
    Original,
    /// A layout fitted to the sampled bubble contour.
    ContourFit,
    /// A rectangular wrap inside the bubble.
    BubbleWrap,
    /// Rows distributed over roughly equal optical lengths.
    Balanced,
}

/// One line-breaking proposal for the active dialogue line.
#[derive(Clone, Debug, PartialEq)]
pub struct ContourVariant {
    pub kind: ContourWrapKind,
    pub text: String,
    pub lines: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypesetterState {
    pub lines: Vec<ScriptLine>,
    pub current_line_idx: usize,
    pub current_page: usize,
    pub presets: Vec<TextStylePreset>,
    pub selected_preset_idx: usize,
    pub options: TypesetterOptions,
}

impl Default for TypesetterState {
    fn default() -> Self {
        Self {
            lines: Vec::new(),
            current_line_idx: 0,
            current_page: 1,
            presets: vec![
                TextStylePreset::default(),
                TextStylePreset {
                    name: "Крик / SFX".to_string(),
                    folder: Some("Звуки".to_string()),
                    font_family: "CC Wild Words".to_string(),
                    font_postscript: "CCWildWords".to_string(),
                    font_size: 24.0,
                    leading: 26.0,
                    tracking: 20,
                    bold: true,
                    all_caps: true,
                    stroke_width: 4.0,
                    ..TextStylePreset::default()
                },
                TextStylePreset {
                    name: "Мысли".to_string(),
                    folder: Some("Диалоги".to_string()),
                    font_family: "Comic Book".to_string(),
                    font_postscript: "ComicBook".to_string(),
                    font_size: 14.0,
                    leading: 16.0,
                    italic: true,
                    all_caps: false,
                    stroke_width: 0.0,
                    ..TextStylePreset::default()
                },
            ],
            selected_preset_idx: 0,
            options: TypesetterOptions::default(),
        }
    }
}

/// Парсер цвета для форматов TypeR и Photoshop CEP:
/// - Объект RGBA: `{"r": .., "g": .., "b": ..}` или `{"red": .., "green": .., "blue": ..}` (0..255 или 0.0..1.0)
/// - Объект Grayscale: `{"gray": 100}` (0..100% или 0..255)
/// - Строка Hex: `"#RRGGBB"` или `"#RRGGBBAA"`
/// - Массив: `[r, g, b]` или `[r, g, b, a]`
pub fn parse_color_value(val: &serde_json::Value) -> Option<[u8; 4]> {
    match val {
        serde_json::Value::Object(map) => {
            let r_val = map.get("r").or_else(|| map.get("red"));
            let g_val = map.get("g").or_else(|| map.get("green"));
            let b_val = map.get("b").or_else(|| map.get("blue"));

            if let (Some(r), Some(g), Some(b)) = (r_val, g_val, b_val) {
                let to_u8 = |v: &serde_json::Value| -> u8 {
                    if let Some(n) = v.as_u64() {
                        n.min(255) as u8
                    } else if let Some(f) = v.as_f64() {
                        if f <= 1.0 && f > 0.0 { (f * 255.0).round().clamp(0.0, 255.0) as u8 } else { f.round().clamp(0.0, 255.0) as u8 }
                    } else {
                        0
                    }
                };
                let r_u8 = to_u8(r);
                let g_u8 = to_u8(g);
                let b_u8 = to_u8(b);

                let a_u8 = if let Some(a) = map.get("a").or_else(|| map.get("alpha")) {
                    if let Some(n) = a.as_u64() {
                        n.min(255) as u8
                    } else if let Some(f) = a.as_f64() {
                        if f <= 1.0 { (f * 255.0).round().clamp(0.0, 255.0) as u8 } else { f.round().clamp(0.0, 255.0) as u8 }
                    } else {
                        255
                    }
                } else if let Some(op) = map.get("opacity").and_then(|v| v.as_f64()) {
                    ((op / 100.0) * 255.0).round().clamp(0.0, 255.0) as u8
                } else {
                    255
                };

                return Some([r_u8, g_u8, b_u8, a_u8]);
            }

            if let Some(g) = map.get("gray").or_else(|| map.get("grey")).and_then(|v| v.as_f64()) {
                let v = if g <= 1.0 && g > 0.0 {
                    (g * 255.0).round().clamp(0.0, 255.0) as u8
                } else if g <= 100.0 {
                    ((g / 100.0) * 255.0).round().clamp(0.0, 255.0) as u8
                } else {
                    g.round().clamp(0.0, 255.0) as u8
                };
                let a = if let Some(op) = map.get("opacity").and_then(|v| v.as_f64()) { ((op / 100.0) * 255.0).round().clamp(0.0, 255.0) as u8 } else { 255 };
                return Some([v, v, v, a]);
            }

            None
        }
        serde_json::Value::Array(arr) if arr.len() >= 3 => {
            let to_u8 = |v: &serde_json::Value| -> u8 {
                if let Some(n) = v.as_u64() {
                    n.min(255) as u8
                } else if let Some(f) = v.as_f64() {
                    if f <= 1.0 && f > 0.0 { (f * 255.0).round().clamp(0.0, 255.0) as u8 } else { f.round().clamp(0.0, 255.0) as u8 }
                } else {
                    0
                }
            };
            let r = to_u8(&arr[0]);
            let g = to_u8(&arr[1]);
            let b = to_u8(&arr[2]);
            let a = if arr.len() >= 4 { to_u8(&arr[3]) } else { 255 };
            Some([r, g, b, a])
        }
        serde_json::Value::String(s) => {
            let s = s.trim_start_matches('#');
            if s.len() == 6 {
                let r = u8::from_str_radix(&s[0..2], 16).ok()?;
                let g = u8::from_str_radix(&s[2..4], 16).ok()?;
                let b = u8::from_str_radix(&s[4..6], 16).ok()?;
                Some([r, g, b, 255])
            } else if s.len() == 8 {
                let r = u8::from_str_radix(&s[0..2], 16).ok()?;
                let g = u8::from_str_radix(&s[2..4], 16).ok()?;
                let b = u8::from_str_radix(&s[4..6], 16).ok()?;
                let a = u8::from_str_radix(&s[6..8], 16).ok()?;
                Some([r, g, b, a])
            } else {
                None
            }
        }
        _ => None,
    }
}

impl TypesetterState {
    /// Импортирует стили из JSON-строки (полная поддержка структуры TypeR / Photoshop CEP).
    pub fn import_styles_json(&mut self, json_str: &str) -> Result<usize, String> {
        let val: serde_json::Value = serde_json::from_str(json_str).map_err(|e| format!("Ошибка парсинга JSON: {}", e))?;

        // 1. Построение словаря папок: id -> название папки
        let mut folders_map = std::collections::HashMap::new();
        if let Some(folders) = val.get("folders").and_then(|f| f.as_array()) {
            for f in folders {
                if let (Some(id), Some(name)) = (f.get("id").and_then(|v| v.as_str()), f.get("name").and_then(|v| v.as_str())) {
                    folders_map.insert(id.to_string(), name.to_string());
                }
            }
        }

        // 2. Получение массива стилей
        let items = if let Some(arr) = val.as_array() {
            arr.clone()
        } else if let Some(styles) = val.get("styles").and_then(|s| s.as_array()) {
            styles.clone()
        } else {
            return Err("JSON не содержит массив стилей или ключ 'styles'".into());
        };

        let mut count = 0;
        for item in items {
            let raw_name = item.get("name").or_else(|| item.get("title")).and_then(|v| v.as_str()).unwrap_or("Новый стиль");

            let folder_id = item.get("folder").and_then(|v| v.as_str());
            let folder_name = folder_id.and_then(|id| folders_map.get(id)).cloned();
            let name = raw_name.to_string();
            let preview_text = item.get("preview_text").or_else(|| item.get("previewText")).and_then(|v| v.as_str()).unwrap_or("").to_string();

            // Разрешение вложенного пути TypeR: textProps -> layerText -> textStyleRange[0] -> textStyle
            let text_style = item.pointer("/textProps/layerText/textStyleRange/0/textStyle");

            let font_family = text_style
                .and_then(|ts| ts.get("fontName"))
                .or_else(|| item.get("font_family"))
                .or_else(|| item.get("fontFamily"))
                .and_then(|v| v.as_str())
                .unwrap_or("Kurai Voice 2")
                .to_string();

            let font_postscript = text_style.and_then(|ts| ts.get("fontPostScriptName")).and_then(|v| v.as_str()).unwrap_or(&font_family).to_string();

            let font_style = text_style.and_then(|ts| ts.get("fontStyleName")).and_then(|v| v.as_str()).unwrap_or("Regular").to_string();

            let font_size = text_style.and_then(|ts| ts.get("size")).or_else(|| item.get("size")).and_then(|v| v.as_f64()).unwrap_or(24.0) as f32;

            let auto_leading_pct =
                item.pointer("/textProps/layerText/paragraphStyleRange/0/paragraphStyle/autoLeadingPercentage").and_then(|v| v.as_f64()).unwrap_or(1.2) as f32;

            let leading = text_style
                .and_then(|ts| ts.get("leading"))
                .or_else(|| item.get("leading"))
                .and_then(|v| v.as_f64())
                .map(|v| v as f32)
                .unwrap_or_else(|| (font_size * auto_leading_pct).round());

            let tracking = text_style.and_then(|ts| ts.get("tracking")).or_else(|| item.get("tracking")).and_then(|v| v.as_i64()).unwrap_or(0) as i32;

            let horizontal_scale = text_style.and_then(|ts| ts.get("horizontalScale")).and_then(|v| v.as_f64()).unwrap_or(100.0) as f32;

            let synthetic_bold = text_style.and_then(|ts| ts.get("syntheticBold")).and_then(|v| v.as_bool()).unwrap_or(false);

            let synthetic_italic = text_style.and_then(|ts| ts.get("syntheticItalic")).and_then(|v| v.as_bool()).unwrap_or(false);

            let is_bold_style = font_style.to_lowercase().contains("bold");
            let is_italic_style = font_style.to_lowercase().contains("italic") || font_style.to_lowercase().contains("oblique");

            let bold = synthetic_bold || is_bold_style;
            let italic = synthetic_italic || is_italic_style;

            let caps_value = text_style.and_then(|ts| ts.get("fontCaps")).and_then(|v| v.as_str()).or_else(|| item.get("caps").and_then(|v| v.as_str()));
            let all_caps = matches!(caps_value, Some("allCaps" | "all"));
            let small_caps = matches!(caps_value, Some("smallCaps" | "small"))
                || item.get("smallCaps").or_else(|| item.get("small_caps")).and_then(|v| v.as_bool()).unwrap_or(false);

            let baseline = text_style.and_then(|ts| ts.get("baseline")).and_then(|v| v.as_str()).or_else(|| item.get("baseline").and_then(|v| v.as_str()));
            let superscript = matches!(baseline, Some("superscript")) || item.get("superscript").and_then(|v| v.as_bool()).unwrap_or(false);
            let subscript = matches!(baseline, Some("subscript")) || item.get("subscript").and_then(|v| v.as_bool()).unwrap_or(false);

            let underline = text_style
                .and_then(|ts| ts.get("underline"))
                .and_then(|v| v.as_bool())
                .or_else(|| item.get("underline").and_then(|v| v.as_bool()))
                .unwrap_or(false);
            let strikethrough = text_style
                .and_then(|ts| ts.get("strikethrough"))
                .and_then(|v| v.as_bool())
                .or_else(|| item.get("strikethrough").and_then(|v| v.as_bool()))
                .unwrap_or(false);

            let (stroke_width, stroke_color) = if let Some(stroke_obj) = item.get("stroke").and_then(|v| v.as_object()) {
                let enabled = stroke_obj.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                let size = stroke_obj.get("size").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                let width = if enabled { size } else { 0.0 };
                let color = if let Some(c) = stroke_obj.get("color") { parse_color_value(c).unwrap_or([255, 255, 255, 255]) } else { [255, 255, 255, 255] };
                (width, color)
            } else if let Some(stroke_val) = item.get("stroke_width").or_else(|| item.get("stroke")) {
                let width = stroke_val.as_f64().unwrap_or(0.0) as f32;
                let color = if let Some(c) = item.get("strokeColor") { parse_color_value(c).unwrap_or([255, 255, 255, 255]) } else { [255, 255, 255, 255] };
                (width, color)
            } else {
                (0.0, [255, 255, 255, 255])
            };

            let text_color = if let Some(c) = item.get("textColorRgba").or_else(|| item.get("textColor")).or_else(|| item.get("text_color")) {
                parse_color_value(c)
            } else if let Some(c) = text_style.and_then(|ts| ts.get("color")) {
                let mut col = parse_color_value(c);
                if let Some(ref mut col) = col {
                    if let Some(op) = text_style.and_then(|ts| ts.get("opacity")).or_else(|| item.get("opacity")).and_then(|v| v.as_f64()) {
                        if (0.0..=100.0).contains(&op) {
                            col[3] = ((op / 100.0) * 255.0).round().clamp(0.0, 255.0) as u8;
                        }
                    }
                }
                col
            } else if let Some(c) = item.get("color") {
                parse_color_value(c)
            } else {
                None
            }
            .unwrap_or([0, 0, 0, 255]);

            let measurement_system = if let Some(kern) = text_style.and_then(|ts| ts.get("autoKern")).and_then(|v| v.as_str()) {
                MeasurementSystem::from_str_loose(kern)
            } else if let Some(ms) = item
                .get("measurementSystem")
                .or_else(|| item.get("measurement_system"))
                .or_else(|| text_style.and_then(|ts| ts.get("measurementSystem")))
                .and_then(|v| v.as_str())
            {
                MeasurementSystem::from_str_loose(ms)
            } else if let Some(kern) = text_style.and_then(|ts| ts.get("kerning")).and_then(|v| v.as_str()) {
                MeasurementSystem::from_str_loose(kern)
            } else {
                MeasurementSystem::Optical
            };

            let para_style = item
                .get("textProps")
                .and_then(|tp| tp.get("layerText"))
                .and_then(|lt| lt.get("paragraphStyleRange"))
                .and_then(|psr| psr.as_array())
                .and_then(|arr| arr.first())
                .and_then(|ps| ps.get("paragraphStyle"));

            let hanging_punctuation = if let Some(hr) = para_style.and_then(|ps| ps.get("hangingRoman")).and_then(|v| v.as_bool()) {
                hr
            } else if let Some(hp) = item.get("hangingPunctuation").or_else(|| item.get("hanging_punctuation")).and_then(|v| v.as_bool()) {
                hp
            } else {
                true
            };

            let align = para_style
                .and_then(|ps| ps.get("alignment").or_else(|| ps.get("align")))
                .and_then(|v| v.as_str())
                .or_else(|| item.get("align").and_then(|v| v.as_str()))
                .map(|a| match a.to_ascii_lowercase().as_str() {
                    "right" | "justifyright" => "right",
                    "left" | "justifyleft" | "justify" => "left",
                    _ => "center",
                })
                .unwrap_or("center")
                .to_string();

            let anti_alias = if let Some(aa) = item.get("antiAlias").or_else(|| item.get("anti_alias")).and_then(|v| v.as_str()) {
                aa.to_string()
            } else if let Some(aa) = text_style.and_then(|ts| ts.get("antiAlias")).and_then(|v| v.as_str()) {
                aa.to_string()
            } else {
                "crisp".to_string()
            };

            self.presets.push(TextStylePreset {
                name,
                preview_text,
                folder: folder_name,
                font_family,
                font_postscript,
                font_style,
                align,
                font_size,
                leading,
                tracking,
                horizontal_scale,
                bold,
                italic,
                all_caps,
                small_caps,
                superscript,
                subscript,
                underline,
                strikethrough,
                stroke_width,
                stroke_color,
                text_color,
                measurement_system,
                hanging_punctuation,
                anti_alias,
            });
            count += 1;
        }

        if count > 0 {
            self.selected_preset_idx = self.presets.len() - 1;
        }

        Ok(count)
    }

    /// Экспортирует стили в JSON-строку формата TypeR (совместимо с Photoshop TypeR).
    pub fn export_styles_json(&self, folder_filter: Option<&str>) -> Result<String, String> {
        let mut folders_map = std::collections::HashMap::new();
        let mut folders_arr = Vec::new();

        let mut folder_id_counter = 1;
        for preset in &self.presets {
            if let Some(folder) = &preset.folder {
                if !folders_map.contains_key(folder) {
                    let id = format!("folder_{}", folder_id_counter);
                    folder_id_counter += 1;
                    folders_map.insert(folder.clone(), id.clone());
                    folders_arr.push(serde_json::json!({
                        "id": id,
                        "name": folder,
                        "order": folders_arr.len()
                    }));
                }
            }
        }

        let mut styles_arr = Vec::new();
        for preset in &self.presets {
            if let Some(target_f) = folder_filter {
                if preset.folder.as_deref() != Some(target_f) {
                    continue;
                }
            }

            let folder_id = preset.folder.as_ref().and_then(|f| folders_map.get(f));

            styles_arr.push(serde_json::json!({
                "name": preset.name,
                "preview_text": preset.preview_text,
                "folder": folder_id,
                "textProps": {
                    "layerText": {
                        "textStyleRange": [
                            {
                                "textStyle": {
                                    "fontName": preset.font_family,
                                    "fontPostScriptName": preset.font_postscript,
                                    "fontStyleName": preset.font_style,
                                    "size": preset.font_size,
                                    "leading": preset.leading,
                                    "tracking": preset.tracking,
                                    "horizontalScale": preset.horizontal_scale,
                                    "syntheticBold": preset.bold && !preset.font_style.to_lowercase().contains("bold"),
                                    "syntheticItalic": preset.italic && !preset.font_style.to_lowercase().contains("italic"),
                                    "fontCaps": if preset.all_caps { "allCaps" } else if preset.small_caps { "smallCaps" } else { "normal" },
                                    "baseline": if preset.superscript { "superscript" } else if preset.subscript { "subscript" } else { "normal" },
                                    "underline": preset.underline,
                                    "strikethrough": preset.strikethrough,
                                    "autoKern": if preset.measurement_system.is_optical() { "opticalKern" } else { "metricsKern" },
                                    "color": {
                                        "red": preset.text_color[0],
                                        "green": preset.text_color[1],
                                        "blue": preset.text_color[2]
                                    },
                                    "opacity": (preset.text_color[3] as f32 / 2.55).round() as u32
                                }
                            }
                        ],
                        "paragraphStyleRange": [
                            {
                                "paragraphStyle": {
                                    "alignment": preset.align,
                                    "autoLeadingPercentage": 1.2,
                                    "hangingRoman": preset.hanging_punctuation
                                }
                            }
                        ]
                    }
                },
                "antiAlias": preset.anti_alias,
                "hangingPunctuation": preset.hanging_punctuation,
                "stroke": {
                    "enabled": preset.stroke_width > 0.0,
                    "size": preset.stroke_width,
                    "opacity": (preset.stroke_color[3] as f32 / 2.55).round() as u32,
                    "position": "outer",
                    "color": {
                        "r": preset.stroke_color[0],
                        "g": preset.stroke_color[1],
                        "b": preset.stroke_color[2]
                    }
                },
                "stroke_width": preset.stroke_width,
                "strokeColor": preset.stroke_color,
                "textColor": format!("#{:02x}{:02x}{:02x}{:02x}", preset.text_color[0], preset.text_color[1], preset.text_color[2], preset.text_color[3]),
                "textColorRgba": preset.text_color,
                "measurementSystem": preset.measurement_system.as_str()
            }));
        }

        let out = serde_json::json!({
            "folders": folders_arr,
            "styles": styles_arr
        });

        serde_json::to_string_pretty(&out).map_err(|e| format!("Ошибка сериализации в JSON: {}", e))
    }

    /// Очищает и загружает сырой текст скрипта манги в очередь реплик с автоопределением страниц.
    pub fn load_script(&mut self, content: &str) {
        self.lines.clear();
        let mut page = 1;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if let Some(detected_page) = detect_page_header(trimmed) {
                page = detected_page;
                continue;
            }

            let cleaned = clean_script_line_with_options(trimmed, &self.options);
            if !cleaned.is_empty() {
                self.lines.push(ScriptLine { raw: trimmed.to_string(), text: cleaned, is_applied: false, page });
            }
        }

        self.current_line_idx = 0;
        self.current_page = self.lines.first().map_or(1, |l| l.page);
    }

    /// Возвращает список всех уникальных страниц в скрипте.
    pub fn pages(&self) -> Vec<usize> {
        let mut p: Vec<usize> = self.lines.iter().map(|l| l.page).collect();
        p.sort_unstable();
        p.dedup();
        if p.is_empty() { vec![1] } else { p }
    }

    /// Возвращает реплики для указанной страницы вместе с их индексами в общем списке.
    pub fn lines_for_page(&self, page: usize) -> Vec<(usize, &ScriptLine)> {
        self.lines.iter().enumerate().filter(|(_, line)| line.page == page).collect()
    }

    pub fn active_line(&self) -> Option<&ScriptLine> {
        self.lines.get(self.current_line_idx)
    }

    pub fn advance_line(&mut self) -> Option<&ScriptLine> {
        if let Some(line) = self.lines.get_mut(self.current_line_idx) {
            line.is_applied = true;
        }
        if self.current_line_idx + 1 < self.lines.len() {
            self.current_line_idx += 1;
            let next = self.lines.get(self.current_line_idx);
            if self.options.auto_page_switch {
                if let Some(nl) = next {
                    self.current_page = nl.page;
                }
            }
            next
        } else {
            None
        }
    }

    pub fn prev_line(&mut self) {
        if self.current_line_idx > 0 {
            self.current_line_idx -= 1;
            if self.options.auto_page_switch {
                if let Some(line) = self.lines.get(self.current_line_idx) {
                    self.current_page = line.page;
                }
            }
        }
    }

    pub fn set_current_line(&mut self, idx: usize) {
        if idx < self.lines.len() {
            self.current_line_idx = idx;
            if let Some(line) = self.lines.get(idx) {
                self.current_page = line.page;
            }
        }
    }

    pub fn active_preset(&self) -> Option<&TextStylePreset> {
        self.presets.get(self.selected_preset_idx)
    }

    pub fn active_preset_mut(&mut self) -> Option<&mut TextStylePreset> {
        self.presets.get_mut(self.selected_preset_idx)
    }

    /// Корректирует кегль активного стиля (с опциональной синхронизацией интерлиньяжа).
    pub fn adjust_active_preset_size(&mut self, delta: f32) {
        let sync = self.options.line_spacing_sync;
        if let Some(preset) = self.active_preset_mut() {
            let target_size = preset.font_size + delta;
            if sync {
                preset.sync_leading_with_size(target_size);
            } else {
                preset.font_size = target_size.clamp(4.0, 500.0);
            }
        }
    }

    /// Возвращает список уникальных названий папок стилей.
    pub fn folders(&self) -> Vec<String> {
        let mut list: Vec<String> = self.presets.iter().filter_map(|p| p.folder.clone()).collect();
        list.sort();
        list.dedup();
        list
    }

    /// Дублирует всю папку со всеми стилями (особенность TypeR: Duplicate Style Folders).
    pub fn duplicate_folder(&mut self, folder: &str, new_folder_name: &str) -> usize {
        let to_duplicate: Vec<TextStylePreset> = self.presets.iter().filter(|p| p.folder.as_deref() == Some(folder)).cloned().collect();

        let count = to_duplicate.len();
        for mut style in to_duplicate {
            style.folder = Some(new_folder_name.to_string());
            self.presets.push(style);
        }
        count
    }

    /// Дублирует выбранный стиль.
    pub fn duplicate_preset(&mut self, idx: usize) -> Option<usize> {
        if let Some(preset) = self.presets.get(idx).cloned() {
            let mut copy = preset;
            copy.name = format!("{} (Копия)", copy.name);
            self.presets.push(copy);
            let new_idx = self.presets.len() - 1;
            self.selected_preset_idx = new_idx;
            Some(new_idx)
        } else {
            None
        }
    }

    /// Удаляет стиль по индексу.
    pub fn delete_preset(&mut self, idx: usize) -> bool {
        if self.presets.len() <= 1 || idx >= self.presets.len() {
            return false;
        }
        self.presets.remove(idx);
        if self.selected_preset_idx >= self.presets.len() {
            self.selected_preset_idx = self.presets.len() - 1;
        }
        true
    }

    /// Ограничивает область вокруг якоря баббла, чтобы композит не строился на весь холст.
    const BUBBLE_SEARCH_RADIUS: i32 = 300;

    /// Ищет границы реплики-баббла вокруг точки `anchor` (документные пиксели).
    ///
    /// Из светлого пикселя рядом с якорем выпускаются четыре луча; каждый останавливается
    /// на первом контурном пикселе с яркостью ниже порога. Композитится только окрестность
    /// якоря, поэтому вызов пригоден для пересчёта по событию, а не каждый кадр.
    pub fn detect_speech_bubble_bounds(&self, doc: &Document, anchor: (f32, f32)) -> Option<Rect> {
        if !anchor.0.is_finite() || !anchor.1.is_finite() {
            return None;
        }
        let canvas = doc.bounds();
        if canvas.is_empty() {
            return None;
        }
        let max_x = canvas.x1.saturating_sub(1);
        let max_y = canvas.y1.saturating_sub(1);
        let anchor_x = (anchor.0.round() as i32).clamp(canvas.x0, max_x);
        let anchor_y = (anchor.1.round() as i32).clamp(canvas.y0, max_y);
        let radius = Self::BUBBLE_SEARCH_RADIUS;
        let region =
            Rect::new(anchor_x.saturating_sub(radius), anchor_y.saturating_sub(radius), anchor_x.saturating_add(radius), anchor_y.saturating_add(radius))
                .intersect(&canvas);
        if region.is_empty() {
            return None;
        }
        let surface = photocraft_compose::composite_to_surface(doc, region, photocraft_color::PixelFormat::RGBA32F, None);
        detect_speech_bubble_bounds_in_surface(&surface, anchor_x, anchor_y, radius)
    }
}

/// Распознает заголовок страницы манги (например: "Page 1", "P02", "Стр. 3", "=== Page 4 ===").
pub fn detect_page_header(line: &str) -> Option<usize> {
    let s = line.trim();
    let s = s.trim_matches(|c| c == '=' || c == '-' || c == '[' || c == ']' || c == '#' || c == '*').trim();
    let lower = s.to_lowercase();

    let prefixes = ["page", "p.", "p", "стр.", "стр", "страница"];
    for prefix in prefixes {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let digits: String = rest.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(num) = digits.parse::<usize>() {
                if num > 0 {
                    return Some(num);
                }
            }
        }
    }
    None
}

/// Очищает строку сценария с учетом заданных опций.
pub fn clean_script_line_with_options(line: &str, options: &TypesetterOptions) -> String {
    let mut s = line.trim();

    if options.strip_dash_prefix {
        if let Some(stripped) = s.strip_prefix("- ").or_else(|| s.strip_prefix("— ")) {
            s = stripped.trim();
        }
    }

    if options.strip_character_names {
        if let Some((_, text)) = s.split_once(':') {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                s = trimmed;
            }
        }
    }

    s.to_string()
}

/// Удаляет типичные префиксы реплик сценария: тире, нумерацию и имена персонажей ("HERO: ...").
pub fn clean_script_line(line: &str) -> String {
    clean_script_line_with_options(line, &TypesetterOptions::default())
}

pub use crate::snow_knuth_plass::char_width_factor;

/// Рассчитывает автоматический перенос слов (word wrap) для вставки текста в выделенный баббл/форму.
/// Использует типографическую привязку союзов/предлогов («висячка») и солвер Кнута—Пласса
/// для балансировки строк по форме овального или ромбовидного манга-баблона.
pub fn wrap_text_to_bubble(text: &str, bubble_width: f32, font_size_pt: f32, dpi: f32, horizontal_scale_pct: f32) -> String {
    crate::snow_knuth_plass::knuth_plass_bubble_wrap_ext(text, bubble_width, bubble_width * 1.2, font_size_pt, dpi, horizontal_scale_pct, true)
}

/// Вычисляет геометрический центр ограничивающего прямоугольника [min_x, min_y, max_x, max_y] баббла.
pub fn calculate_bubble_center(bounds: [f32; 4]) -> [f32; 2] {
    let [min_x, min_y, max_x, max_y] = bounds;
    [min_x + (max_x - min_x) * 0.5, min_y + (max_y - min_y) * 0.5]
}

/// Яркость, ниже которой пиксель считается контуром баббла.
pub const BUBBLE_OUTLINE_LUMA: f32 = 0.45;

/// Яркость, выше которой пиксель может быть внутренностью баббла.
pub const BUBBLE_FILL_LUMA: f32 = 0.50;

fn surface_luma(surface: &Surface, x: i32, y: i32) -> f32 {
    let pixel = surface.pixel(x, y);
    match pixel.as_slice() {
        [r, g, b, ..] => 0.299 * r + 0.587 * g + 0.114 * b,
        [value, ..] => *value,
        [] => 0.0,
    }
}

/// Трассирует границы баббла на готовой поверхности.
///
/// `anchor_*` задаёт точку внутри баббла (обычно центр реплики), `max_radius` ограничивает
/// длину каждого луча. Возвращает прямоугольник в координатах поверхности, если хотя бы по
/// одной оси обе границы найдены и размеры похожи на реплику.
pub fn detect_speech_bubble_bounds_in_surface(surface: &Surface, anchor_x: i32, anchor_y: i32, max_radius: i32) -> Option<Rect> {
    let bounds = surface.content_bounds();
    if !bounds.contains(anchor_x, anchor_y) {
        return None;
    }

    // Внутри баббла ищем светлый пиксель: якорь может попасть на букву или блик.
    let mut seed = None;
    'seed: for radius in [0, 4, 8, 12, 16, 20] {
        for dy in [-radius, 0, radius] {
            for dx in [-radius, 0, radius] {
                let (sx, sy) = (anchor_x.saturating_add(dx), anchor_y.saturating_add(dy));
                if bounds.contains(sx, sy) && surface_luma(surface, sx, sy) >= BUBBLE_FILL_LUMA {
                    seed = Some((sx, sy));
                    break 'seed;
                }
            }
        }
    }
    let (seed_x, seed_y) = seed?;

    let max_radius = max_radius.clamp(20, 300);
    let step_toward = |dx: i32, dy: i32| -> Option<i32> {
        let mut step = 1;
        while step <= max_radius {
            let x = seed_x.saturating_add(dx.saturating_mul(step));
            let y = seed_y.saturating_add(dy.saturating_mul(step));
            if !bounds.contains(x, y) {
                return None;
            }
            if surface_luma(surface, x, y) < BUBBLE_OUTLINE_LUMA {
                return Some(step - 1);
            }
            step += 1;
        }
        None
    };

    let left = step_toward(-1, 0);
    let right = step_toward(1, 0);
    let top = step_toward(0, -1);
    let bottom = step_toward(0, 1);
    // Баббл должен быть замкнут хотя бы по одной оси, иначе луч ушёл в открытое поле.
    if (left.is_none() && right.is_none()) || (top.is_none() && bottom.is_none()) {
        return None;
    }

    let min_x = seed_x.saturating_sub(left.unwrap_or(0));
    let max_x = seed_x.saturating_add(right.unwrap_or(0));
    let min_y = seed_y.saturating_sub(top.unwrap_or(0));
    let max_y = seed_y.saturating_add(bottom.unwrap_or(0));
    let width = max_x.saturating_sub(min_x).saturating_add(1);
    let height = max_y.saturating_sub(min_y).saturating_add(1);
    if width >= 20 && height >= 20 && width <= 500 && height <= 600 {
        Some(Rect::new(min_x, min_y, max_x.saturating_add(1), max_y.saturating_add(1)))
    } else {
        None
    }
}

/// Строит варианты контурной разбивки для меню.
///
/// Первым всегда идёт авторский текст с сохранёнными переносами. Затем генератор силуэта
/// предлагает разные количества строк; одинаковые разбивки отбрасываются. `bubble` задаёт
/// границы реплики (выделение или найденный баббл), `dpi` переводит кегль в пиксели.
pub fn contour_variants(text: &str, preset: &TextStylePreset, bubble: Option<Rect>, dpi: f32, limit: usize) -> Vec<ContourVariant> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.len() > 16_384 {
        return Vec::new();
    }
    let limit = limit.clamp(1, 24);
    let clean_text = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    let bound_clean = crate::snow_knuth_plass::bind_orphan_prepositions(&clean_text);
    let original = if trimmed.contains('\n') {
        trimmed.lines().map(str::trim).filter(|line| !line.is_empty()).map(crate::snow_knuth_plass::bind_orphan_prepositions).collect::<Vec<_>>().join("\n")
    } else {
        bound_clean.clone()
    };

    let mut variants = Vec::new();
    push_contour_variant(&mut variants, ContourWrapKind::Original, original, limit);

    let (bubble_w, bubble_h) = bubble.map_or((None, None), |rect| (Some(rect.width() as f32), Some(rect.height() as f32)));
    let generated = crate::snow_knuth_plass::generate_contour_variants(
        &bound_clean,
        bubble_w,
        bubble_h,
        preset.font_size,
        dpi,
        limit.saturating_sub(variants.len()).max(1),
    );
    for (idx, proposal) in generated.into_iter().enumerate() {
        let kind = if idx == 0 { ContourWrapKind::BubbleWrap } else { ContourWrapKind::Balanced };
        push_contour_variant(&mut variants, kind, proposal, limit);
    }
    variants
}

fn push_contour_variant(variants: &mut Vec<ContourVariant>, kind: ContourWrapKind, text: String, limit: usize) {
    let text = text.trim();
    if text.is_empty() || variants.len() >= limit || variants.iter().any(|variant| variant.text == text) {
        return;
    }
    let lines = text.lines().filter(|line| !line.trim().is_empty()).count().max(1);
    variants.push(ContourVariant { kind, text: text.to_string(), lines });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_typesetter_options_reads_state_saved_before_hanging_punctuation() {
        // `typesetter_state.json` saved by a build before the option existed has five fields.
        // Reading it must keep the new option's default instead of resetting the whole state.
        let old = r#"{
            "auto_page_switch": true,
            "line_spacing_sync": false,
            "strip_character_names": true,
            "strip_dash_prefix": true,
            "optical_baseline_factor": 0.35
        }"#;
        let options: TypesetterOptions = serde_json::from_str(old).unwrap();
        assert!(options.hanging_punctuation, "old state keeps the new option's default");
        assert_eq!(options.optical_baseline_factor, 0.35);
        assert!(!options.line_spacing_sync);
    }

    #[test]
    fn test_clean_script_line() {
        assert_eq!(clean_script_line("- Привет!"), "Привет!");
        assert_eq!(clean_script_line("ГЕРОЙ: Стой на месте!"), "Стой на месте!");
        assert_eq!(clean_script_line("Обычная строка"), "Обычная строка");
    }

    #[test]
    fn test_typesetter_queue() {
        let mut state = TypesetterState::default();
        state.load_script("Hero: Line 1\n\n- Line 2");
        assert_eq!(state.lines.len(), 2);
        assert_eq!(state.active_line().unwrap().text, "Line 1");

        state.advance_line();
        assert_eq!(state.active_line().unwrap().text, "Line 2");
        assert!(state.lines[0].is_applied);
    }

    #[test]
    fn test_bubble_center_calculation() {
        let center = calculate_bubble_center([100.0, 100.0, 300.0, 500.0]);
        assert_eq!(center, [200.0, 300.0]);
    }

    #[test]
    fn test_page_detection_and_grouping() {
        let script = "Page 1\nHero: Hello!\n\nPage 2\n- Next page line";
        let mut state = TypesetterState::default();
        state.load_script(script);
        assert_eq!(state.lines.len(), 2);
        assert_eq!(state.lines[0].page, 1);
        assert_eq!(state.lines[1].page, 2);

        let p2_lines = state.lines_for_page(2);
        assert_eq!(p2_lines.len(), 1);
        assert_eq!(p2_lines[0].1.text, "Next page line");
    }

    #[test]
    fn test_line_spacing_sync() {
        let mut preset = TextStylePreset { font_size: 20.0, leading: 24.0, ..TextStylePreset::default() };
        preset.sync_leading_with_size(30.0);
        assert_eq!(preset.font_size, 30.0);
        assert_eq!(preset.leading, 36.0); // 24 * (30/20) = 36
    }

    #[test]
    fn test_duplicate_folder() {
        let mut state = TypesetterState::default();
        let initial_count = state.presets.len();
        let manga_styles_count = state.presets.iter().filter(|p| p.folder.as_deref() == Some("Манга")).count();
        assert!(manga_styles_count > 0);

        let dup = state.duplicate_folder("Манга", "Манга (Копия)");
        assert_eq!(dup, manga_styles_count);
        assert_eq!(state.presets.len(), initial_count + manga_styles_count);
    }

    #[test]
    fn test_export_and_import_json() {
        let state = TypesetterState::default();
        let json = state.export_styles_json(None).unwrap();
        assert!(json.contains("folders"));
        assert!(json.contains("styles"));

        let mut new_state = TypesetterState { presets: Vec::new(), ..TypesetterState::default() };
        let imported = new_state.import_styles_json(&json).unwrap();
        assert_eq!(imported, state.presets.len());
        for i in 0..state.presets.len() {
            assert_eq!(new_state.presets[i].text_color, state.presets[i].text_color);
            assert_eq!(new_state.presets[i].measurement_system, state.presets[i].measurement_system);
        }
    }

    #[test]
    fn test_export_import_keeps_alignment_antialias_and_hanging_punctuation() {
        let mut state = TypesetterState { presets: Vec::new(), ..TypesetterState::default() };
        state.presets.push(TextStylePreset {
            name: "Left aligned".into(),
            align: "left".into(),
            anti_alias: "sharp".into(),
            hanging_punctuation: false,
            all_caps: false,
            small_caps: true,
            underline: true,
            strikethrough: true,
            superscript: true,
            ..TextStylePreset::default()
        });

        let exported = state.export_styles_json(None).unwrap();
        let value: serde_json::Value = serde_json::from_str(&exported).unwrap();
        assert_eq!(value["styles"][0]["textProps"]["layerText"]["paragraphStyleRange"][0]["paragraphStyle"]["alignment"], "left");
        assert_eq!(value["styles"][0]["antiAlias"], "sharp");
        assert_eq!(value["styles"][0]["hangingPunctuation"], false);

        let mut imported = TypesetterState { presets: Vec::new(), ..TypesetterState::default() };
        assert_eq!(imported.import_styles_json(&exported).unwrap(), 1);
        let preset = &imported.presets[0];
        assert_eq!(preset.align, "left");
        assert_eq!(preset.anti_alias, "sharp");
        assert!(!preset.hanging_punctuation);
        assert!(preset.small_caps);
        assert!(preset.underline);
        assert!(preset.strikethrough);
        assert!(preset.superscript);
    }

    #[test]
    fn test_preset_saved_before_alignment_gets_centered() {
        // A preset from a build before `align` existed keeps the new field's default.
        let old = r#"{
            "name": "Old",
            "font_family": "Inter",
            "font_postscript": "",
            "font_style": "Regular",
            "font_size": 24.0,
            "leading": 28.0,
            "tracking": 0,
            "horizontal_scale": 100.0,
            "bold": false,
            "italic": false,
            "all_caps": false,
            "stroke_width": 0.0,
            "stroke_color": [255, 255, 255, 255]
        }"#;
        let preset: TextStylePreset = serde_json::from_str(old).unwrap();
        assert_eq!(preset.align, "center");
        assert!(!preset.small_caps);
        assert!(!preset.underline);
    }

    #[test]
    fn test_import_typer_export_json_sample() {
        let json_data = r#"{
            "folders": [{"name": "Манга", "id": "zbtrk7i9"}],
            "styles": [
                {
                    "name": "Мысли",
                    "folder": "zbtrk7i9",
                    "textProps": {
                        "layerText": {
                            "textStyleRange": [{
                                "textStyle": {
                                    "fontName": "Kurai Voice 2",
                                    "fontPostScriptName": "KuraiVoice2-Italic",
                                    "fontStyleName": "Italic",
                                    "size": 26,
                                    "syntheticBold": false,
                                    "syntheticItalic": false,
                                    "fontCaps": "allCaps"
                                }
                            }],
                            "paragraphStyleRange": [{
                                "paragraphStyle": {
                                    "autoLeadingPercentage": 1.2
                                }
                            }]
                        }
                    },
                    "stroke": {"enabled": false, "size": 0, "color": {"r": 255, "g": 255, "b": 255}}
                },
                {
                    "name": "Обычный выделенный",
                    "folder": "zbtrk7i9",
                    "textProps": {
                        "layerText": {
                            "textStyleRange": [{
                                "textStyle": {
                                    "fontName": "Kurai Voice 2",
                                    "fontPostScriptName": "KuraiVoice2-Bold",
                                    "fontStyleName": "Bold",
                                    "size": 36,
                                    "syntheticBold": false,
                                    "syntheticItalic": false,
                                    "fontCaps": "allCaps"
                                }
                            }]
                        }
                    }
                },
                {
                    "name": "Жёсткий Крик",
                    "folder": "zbtrk7i9",
                    "textProps": {
                        "layerText": {
                            "textStyleRange": [{
                                "textStyle": {
                                    "fontName": "v_CCWiccanSansInt",
                                    "fontPostScriptName": "v_CCWiccanSansInt",
                                    "fontStyleName": "Regular",
                                    "size": 26,
                                    "syntheticBold": true,
                                    "syntheticItalic": true,
                                    "fontCaps": "allCaps"
                                }
                            }]
                        }
                    },
                    "stroke": {"enabled": false, "size": 0}
                }
            ]
        }"#;

        let mut state = TypesetterState { presets: Vec::new(), ..TypesetterState::default() };
        let count = state.import_styles_json(json_data).unwrap();
        assert_eq!(count, 3);

        // Мысли
        assert_eq!(state.presets[0].name, "Мысли");
        assert_eq!(state.presets[0].font_family, "Kurai Voice 2");
        assert_eq!(state.presets[0].font_postscript, "KuraiVoice2-Italic");
        assert_eq!(state.presets[0].font_style, "Italic");
        assert!(!state.presets[0].bold);
        assert!(state.presets[0].italic);
        assert_eq!(state.presets[0].stroke_width, 0.0);
        assert_eq!(state.presets[0].leading, 31.0); // 26 * 1.2 = 31.2 -> 31.0

        // Обычный выделенный
        assert_eq!(state.presets[1].name, "Обычный выделенный");
        assert_eq!(state.presets[1].font_style, "Bold");
        assert!(state.presets[1].bold);
        assert!(!state.presets[1].italic);
        assert_eq!(state.presets[1].leading, 43.0); // 36 * 1.2 = 43.2 -> 43.0

        // Жёсткий Крик
        assert_eq!(state.presets[2].name, "Жёсткий Крик");
        assert!(state.presets[2].bold);
        assert!(state.presets[2].italic);
        assert_eq!(state.presets[2].stroke_width, 0.0);
    }

    #[test]
    fn test_import_real_file_if_present() {
        if let Ok(content) = std::fs::read_to_string(r"d:\Work\umamusume\TypeR_Export.json") {
            let mut state = TypesetterState { presets: Vec::new(), ..TypesetterState::default() };
            let count = state.import_styles_json(&content).unwrap();
            assert!(count > 0);
            for p in &state.presets {
                if p.font_style.to_lowercase().contains("bold") {
                    assert!(p.bold, "Preset '{}' with style '{}' must be bold", p.name, p.font_style);
                }
                if p.font_style.to_lowercase().contains("italic") {
                    assert!(p.italic, "Preset '{}' with style '{}' must be italic", p.name, p.font_style);
                }
            }
        }
    }

    #[test]
    fn test_typer_font_color_and_measurement_system_import_export() {
        let json_data = r##"{
            "folders": [{"name": "Тест", "id": "f1"}],
            "styles": [
                {
                    "name": "RGB Optical",
                    "folder": "f1",
                    "textProps": {
                        "layerText": {
                            "textStyleRange": [{
                                "textStyle": {
                                    "fontName": "Inter",
                                    "size": 20,
                                    "color": {"red": 255, "green": 128, "blue": 64},
                                    "opacity": 80,
                                    "autoKern": "opticalKern"
                                }
                            }]
                        }
                    }
                },
                {
                    "name": "Grayscale Metric",
                    "folder": "f1",
                    "textProps": {
                        "layerText": {
                            "textStyleRange": [{
                                "textStyle": {
                                    "fontName": "Inter",
                                    "size": 24,
                                    "color": {"gray": 100},
                                    "autoKern": "metricsKern"
                                }
                            }]
                        }
                    }
                },
                {
                    "name": "Hex Metric",
                    "folder": "f1",
                    "textColor": "#112233aa",
                    "measurementSystem": "metric",
                    "textProps": {
                        "layerText": {
                            "textStyleRange": [{
                                "textStyle": {
                                    "fontName": "Inter",
                                    "size": 18
                                }
                            }]
                        }
                    }
                }
            ]
        }"##;

        let mut state = TypesetterState { presets: Vec::new(), ..TypesetterState::default() };
        let count = state.import_styles_json(json_data).unwrap();
        assert_eq!(count, 3);

        // 1. RGB Optical with 80% opacity: [255, 128, 64, 204]
        assert_eq!(state.presets[0].name, "RGB Optical");
        assert_eq!(state.presets[0].text_color[0], 255);
        assert_eq!(state.presets[0].text_color[1], 128);
        assert_eq!(state.presets[0].text_color[2], 64);
        assert_eq!(state.presets[0].text_color[3], 204);
        assert_eq!(state.presets[0].measurement_system, MeasurementSystem::Optical);

        // 2. Grayscale 100% Metric: [255, 255, 255, 255]
        assert_eq!(state.presets[1].name, "Grayscale Metric");
        assert_eq!(state.presets[1].text_color, [255, 255, 255, 255]);
        assert_eq!(state.presets[1].measurement_system, MeasurementSystem::Metric);

        // 3. Hex Metric: #112233aa
        assert_eq!(state.presets[2].name, "Hex Metric");
        assert_eq!(state.presets[2].text_color, [0x11, 0x22, 0x33, 0xaa]);
        assert_eq!(state.presets[2].measurement_system, MeasurementSystem::Metric);

        // Roundtrip export
        let exported = state.export_styles_json(None).unwrap();
        let mut reimported = TypesetterState { presets: Vec::new(), ..TypesetterState::default() };
        reimported.import_styles_json(&exported).unwrap();
        assert_eq!(reimported.presets.len(), 3);
        assert_eq!(reimported.presets[0].text_color, state.presets[0].text_color);
        assert_eq!(reimported.presets[0].measurement_system, MeasurementSystem::Optical);
        assert_eq!(reimported.presets[1].text_color, state.presets[1].text_color);
        assert_eq!(reimported.presets[1].measurement_system, MeasurementSystem::Metric);
        assert_eq!(reimported.presets[2].text_color, state.presets[2].text_color);
        assert_eq!(reimported.presets[2].measurement_system, MeasurementSystem::Metric);
    }

    #[test]
    fn preview_text_round_trips_in_preset_json_and_defaults_for_old_files() {
        let preset = TextStylePreset { name: "Custom".into(), preview_text: "Свой образец".into(), ..TextStylePreset::default() };
        let json = serde_json::to_string(&preset).unwrap();
        assert!(json.contains("\"preview_text\""), "the field is serialized: {json}");
        let parsed: TextStylePreset = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, preset);
        assert_eq!(parsed.preview_text, "Свой образец");

        // A .pcraft/state file saved before the field existed must still open.
        let mut old = serde_json::to_value(&preset).unwrap();
        old.as_object_mut().unwrap().remove("preview_text");
        let parsed: TextStylePreset = serde_json::from_value(old).unwrap();
        assert_eq!(parsed.preview_text, "");
    }

    #[test]
    fn preview_text_survives_typer_export_import() {
        let mut state = TypesetterState::default();
        state.presets =
            vec![TextStylePreset { name: "TypeR Custom".into(), preview_text: "Экспортный образец".into(), ..TextStylePreset::default() }];
        let exported = state.export_styles_json(None).unwrap();
        let value: serde_json::Value = serde_json::from_str(&exported).unwrap();
        assert_eq!(value["styles"][0]["preview_text"], "Экспортный образец");
        assert!(value["styles"][0].get("previewText").is_none(), "export writes only the documented snake_case field");

        let mut reimported = TypesetterState { presets: Vec::new(), ..TypesetterState::default() };
        reimported.import_styles_json(&exported).unwrap();
        assert_eq!(reimported.presets[0].preview_text, "Экспортный образец");

        // The camelCase alias is accepted for styles exported by other tools.
        let alias = r#"{"styles":[{"name":"Alias","previewText":"Alias sample"}]}"#;
        let mut aliased = TypesetterState { presets: Vec::new(), ..TypesetterState::default() };
        aliased.import_styles_json(alias).unwrap();
        assert_eq!(aliased.presets[0].preview_text, "Alias sample");

        let missing = r#"{"styles":[{"name":"Missing"}]}"#;
        let mut plain = TypesetterState { presets: Vec::new(), ..TypesetterState::default() };
        plain.import_styles_json(missing).unwrap();
        assert_eq!(plain.presets[0].preview_text, "", "missing preview_text means the automatic sample");
    }

    #[test]
    fn test_wrap_text_to_bubble() {
        let short_text = "Короткий текст";
        // Wide bubble: no wrap
        let res = wrap_text_to_bubble(short_text, 300.0, 14.0, 72.0, 100.0);
        assert_eq!(res, short_text);

        // Long text in narrow bubble: should wrap into multiple lines with \n
        let long_text = "Это длинная реплика персонажа которая должна красиво переноситься внутри баббла";
        let wrapped = wrap_text_to_bubble(long_text, 120.0, 14.0, 72.0, 100.0);
        assert!(wrapped.contains('\n'));
        let lines: Vec<&str> = wrapped.lines().collect();
        assert!(lines.len() >= 2);
        // Words should be preserved
        let words_orig: Vec<&str> = long_text.split_whitespace().collect();
        let words_wrapped: Vec<&str> = wrapped.split_whitespace().collect();
        assert_eq!(words_orig, words_wrapped);
    }

    #[test]
    fn test_speech_bubble_bounds_trace_a_light_bubble() {
        let mut surface = Surface::new(photocraft_color::PixelFormat::RGBA32F);
        surface.fill_rect(Rect::new(0, 0, 220, 220), &[0.0, 0.0, 0.0, 1.0]);
        surface.fill_rect(Rect::new(50, 40, 170, 180), &[1.0, 1.0, 1.0, 1.0]);

        let bounds = detect_speech_bubble_bounds_in_surface(&surface, 110, 110, 200).expect("the light bubble is closed on both axes");
        assert_eq!(bounds, Rect::new(50, 40, 170, 180));

        let dark = {
            let mut s = Surface::new(photocraft_color::PixelFormat::RGBA32F);
            s.fill_rect(Rect::new(0, 0, 64, 64), &[0.0, 0.0, 0.0, 1.0]);
            s
        };
        assert_eq!(detect_speech_bubble_bounds_in_surface(&dark, 32, 32, 64), None, "no light seed means no bubble");
    }

    #[test]
    fn test_speech_bubble_bounds_composites_a_document() {
        let size = photocraft_doc::Size::new(240, 240);
        let mut doc = Document::with_background(
            "Bubble",
            size,
            photocraft_doc::ColorMode::Rgb,
            photocraft_doc::SampleType::U8,
            photocraft_doc::Color::rgb(0.0, 0.0, 0.0),
        );
        let mut bubble = photocraft_doc::Layer::raster("Bubble fill", doc.pixel_format());
        if let Some(surface) = bubble.surface_mut() {
            let white = photocraft_raster::from_rgba(&surface.format(), [1.0, 1.0, 1.0, 1.0]);
            surface.fill_rect(Rect::new(50, 40, 170, 180), &white);
        }
        doc.layers.push(bubble);

        let state = TypesetterState::default();
        let bounds = state.detect_speech_bubble_bounds(&doc, (110.0, 110.0)).expect("the drawn bubble is detected");
        assert_eq!(bounds, Rect::new(50, 40, 170, 180));
        assert_eq!(state.detect_speech_bubble_bounds(&doc, (f32::NAN, 10.0)), None, "non-finite anchors are rejected");
    }

    #[test]
    fn test_contour_variants_keep_words_and_vary_height() {
        let text = "Это длинная реплика персонажа которая должна красиво переноситься внутри баббла";
        let preset = TextStylePreset { font_size: 12.0, ..TextStylePreset::default() };
        let variants = contour_variants(text, &preset, Some(Rect::new(0, 0, 260, 200)), 72.0, 8);

        assert!(!variants.is_empty());
        assert_eq!(variants[0].kind, ContourWrapKind::Original);
        assert!(variants.len() <= 8);
        let words_original: Vec<&str> = text.split_whitespace().collect();
        for variant in &variants {
            assert_eq!(variant.text.split_whitespace().collect::<Vec<_>>(), words_original, "a layout never drops or reorders words");
            assert_eq!(variant.lines, variant.text.lines().count().max(1));
        }
        let line_counts: std::collections::BTreeSet<usize> = variants.iter().map(|variant| variant.lines).collect();
        assert!(line_counts.len() >= 2, "the menu offers visibly different heights: {line_counts:?}");
    }
}
