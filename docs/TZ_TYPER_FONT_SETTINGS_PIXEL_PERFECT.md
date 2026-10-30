> **Статус: устарело (superseded).** Актуальная версия — `docs/TZ_FONT_SETTINGS_V2_FINAL.md`. Раздел 2 этого файла (сетка 520 px / `RIGHT_W=382`) заменён; файл оставлен для истории.

# ТЗ: Пиксель-перфект редизайн окна «Font Settings» и починка предпросмотра (TypeR)

**Целевой файл:** `crates/ui-egui/src/snow_typesetter_ui.rs`  
**Основа:** Приведение к дизайну мокапа (Image 2) и устранение артефактов текущего рендера (Image 1).

---

## 1. Заголовок окна, кнопка закрытия `✖` и фиксированная ширина по X

### Файл: `crates/ui-egui/src/snow_typesetter_ui.rs` (~строка 463):
Заменить объявление окна:

```rust
// ДО:
            Window::new(title)
                .id(egui::Id::new("typer_font_settings_window"))
                .collapsible(false)
                .resizable(true)
                .default_width(480.0)
                .default_height(620.0)
                .min_width(430.0)
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ctx.content_rect().center())
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            self.render_font_settings_modal(ui, edit_idx, &mut close_modal);
                        });
                });

// ПОСЛЕ:
            let mut window_open = true;
            Window::new(title)
                .id(egui::Id::new("typer_font_settings_window"))
                .open(&mut window_open) // Добавляет стандартный крестик ✖ в правый верхний угол
                .collapsible(false)
                .resizable([false, true]) // Фиксированная ширина по X, ресайз только по Y
                .default_width(520.0)
                .min_width(520.0)
                .max_width(520.0)
                .default_height(640.0)
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ctx.content_rect().center())
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            self.render_font_settings_modal(ui, edit_idx, &mut close_modal);
                        });
                });

            if !window_open {
                close_modal = true;
            }
```

---

## 2. Единая колонка меток (сетка как в Photoshop) и верстка строк

В функции `render_font_settings_modal` (строка ~1895) используем константу ширины левой метки:
`const LABEL_W: f32 = 110.0;`

### 2.1. Имя стиля и Папка (строки ~1896–1926):
```rust
            ui.horizontal(|ui| {
                ui.add_sized([LABEL_W, 20.0], egui::Label::new(RichText::new(format!("{}:", tl!("Style name"))).strong()));
                if ui.add(egui::TextEdit::singleline(&mut preset.name).desired_width(380.0)).changed() {
                    changed = true;
                }
            });

            ui.horizontal(|ui| {
                ui.add_sized([LABEL_W, 20.0], egui::Label::new(format!("{}:", tl!("Folder"))));
                let folder_str = preset.folder.clone().unwrap_or_default();
                egui::ComboBox::from_id_salt("edit_preset_folder_combo")
                    .selected_text(if folder_str.is_empty() { tl!("No folder") } else { &folder_str })
                    .width(365.0)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(folder_str.is_empty(), tl!("No folder")).clicked() {
                            preset.folder = None;
                            changed = true;
                        }
                        for f in &available_folders {
                            if ui.selectable_label(folder_str == *f, f).clicked() {
                                preset.folder = Some(f.clone());
                                changed = true;
                            }
                        }
                    });
            });
```

---

### 2.2. Кнопки оформления (строка ~2012): все 9 кнопок строго в один ряд!
Заменить `ui.horizontal_wrapped` на `ui.horizontal` с компактным шагом 3 px:

```rust
            ui.horizontal(|ui| {
                ui.add_sized([LABEL_W, 20.0], egui::Label::new(format!("{}:", tl!("Оформление"))));
                ui.spacing_mut().item_spacing.x = 3.0;

                character_toggle(ui, &mut preset.bold, "B", "Псевдожирный / Bold");
                character_toggle(ui, &mut preset.italic, "I", "Псевдокурсив / Italic");
                character_toggle(ui, &mut preset.all_caps, "TT", "Все прописные / All Caps");
                character_toggle(ui, &mut preset.small_caps, "Tт", "Капитель / Small Caps");
                character_toggle(ui, &mut preset.superscript, "T¹", "Верхний индекс / Superscript");
                character_toggle(ui, &mut preset.subscript, "T₁", "Нижний индекс / Subscript");
                character_toggle(ui, &mut preset.underline, "U", "Подчёркивание / Underline");
                character_toggle(ui, &mut preset.strikethrough, "S", "Зачёркивание / Strikethrough");
                character_toggle(ui, &mut preset.hanging_punctuation, "«»", "Висячая пунктуация / Hanging Punctuation");
            });
```

Вспомогательная функция `character_toggle` (строка ~2465):
```rust
fn character_toggle(ui: &mut egui::Ui, value: &mut bool, label: &str, tooltip: &str) -> bool {
    let btn = egui::Button::new(RichText::new(label).strong())
        .selected(*value)
        .min_size(vec2(28.0, 22.0));
    let response = ui.add(btn).on_hover_text(tooltip);
    if response.clicked() {
        *value = !*value;
        true
    } else {
        false
    }
}
```

---

### 2.3. Кнопки выравнивания (строка ~2071): монолитные кнопки вместо чекбоксов `▢`
```rust
            ui.horizontal(|ui| {
                ui.add_sized([LABEL_W, 20.0], egui::Label::new(format!("{}:", tl!("Выравнивание"))));
                ui.spacing_mut().item_spacing.x = 4.0;
                for (value, label, tip) in [
                    ("left", "≡  По левому", "Выравнивание по левому краю"),
                    ("center", "≡  По центру", "Выравнивание по центру"),
                    ("right", "≡  По правому", "Выравнивание по правому краю"),
                ] {
                    let is_sel = preset.align.as_str() == value;
                    let btn = egui::Button::new(RichText::new(label).size(12.0))
                        .selected(is_sel)
                        .min_size(vec2(110.0, 24.0));
                    if ui.add(btn).on_hover_text(tip).clicked() {
                        preset.align = value.to_string();
                        changed = true;
                    }
                }
            });
```

---

### 2.4. Антиалиасинг и Цвет (строки ~2089–2130):
```rust
            ui.horizontal(|ui| {
                ui.add_sized([LABEL_W, 20.0], egui::Label::new("Anti-alias:"));
                let aa_labels = [
                    ("crisp", "Чёткий (crisp)"),
                    ("sharp", "Резкий (sharp)"),
                    ("smooth", "Плавный (smooth)"),
                    ("strong", "Жирный (strong)"),
                    ("none", "Без сглаживания (none)"),
                ];
                let cur_aa = preset.anti_alias.clone();
                let display_label = aa_labels.iter().find(|(k, _)| *k == cur_aa).map(|(_, l)| *l).unwrap_or(cur_aa.as_str());
                egui::ComboBox::from_id_salt("style_modal_aa")
                    .selected_text(display_label)
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        for (k, label) in aa_labels {
                            if ui.selectable_label(cur_aa == k, label).clicked() {
                                preset.anti_alias = k.to_string();
                                changed = true;
                            }
                        }
                    });
            });

            ui.horizontal(|ui| {
                ui.add_sized([LABEL_W, 20.0], egui::Label::new(format!("{}:", tl!("Color"))));
                let mut text_c = egui::Color32::from_rgba_unmultiplied(
                    preset.text_color[0], preset.text_color[1], preset.text_color[2], preset.text_color[3],
                );
                if egui::color_picker::color_edit_button_srgba(ui, &mut text_c, egui::color_picker::Alpha::BlendOrAdditive).changed() {
                    preset.text_color = [text_c.r(), text_c.g(), text_c.b(), text_c.a()];
                    changed = true;
                }
                ui.add_space(4.0);
                let mut hex = format!("#{:02X}{:02X}{:02X}{:02X}", preset.text_color[0], preset.text_color[1], preset.text_color[2], preset.text_color[3]);
                ui.add_sized([100.0, 20.0], egui::TextEdit::singleline(&mut hex));
            });
```

---

### 2.5. Размер, трекинг и контур (строки ~2147–2232):
Сократить подпись `Horizontal scale (%)` до `H-Scale (%)`, чтобы строка не расползалась:
```rust
            ui.horizontal(|ui| {
                ui.add_sized([LABEL_W, 20.0], egui::Label::new(format!("{}:", tl!("Tracking"))));
                if ui.add(egui::DragValue::new(&mut preset.tracking).speed(1.0).range(-200..=1000)).changed() {
                    changed = true;
                }
                ui.add_space(16.0);
                ui.label("H-Scale (%):");
                if ui.add(egui::DragValue::new(&mut preset.horizontal_scale).speed(1.0).range(20.0..=300.0)).changed() {
                    changed = true;
                }
            });
```

---

## 3. ПОЛНАЯ ПОЧИНКА ПРЕДПРОСМОТРА (Preview)

### Файл: `crates/ui-egui/src/snow_typesetter_ui.rs` (~строки 2556–2678)

Заменить реализацию `style_preview_image` на чистую однострочную отрисовку со сплошным дисковым контуром:

```rust
fn style_preview_image(preset: &TextStylePreset, width: f32, height: f32, scale: u32) -> Option<egui::ColorImage> {
    let w = (width * scale as f32).round().max(1.0) as usize;
    let h = (height * scale as f32).round().max(1.0) as usize;
    let sample = style_preview_text(preset);

    // Подбираем кегль так, чтобы строка всегда гарантированно помещалась в рамку в 1 строку
    let char_count = sample.chars().count().max(1) as f32;
    let max_fitting_size = (w as f32 * 1.5) / char_count;
    let size = (preset.font_size * 0.7).clamp(12.0, 20.0).min(max_fitting_size);

    let style_name = preset.font_style.to_ascii_lowercase();
    let native_bold = style_preview_weight(&preset.font_style) >= 600;
    let native_italic = style_name.contains("italic") || style_name.contains("oblique");

    let char_style = CharStyle {
        font_family: preset.font_family.clone(),
        font_style: preset.font_style.clone(),
        postscript_name: (!preset.font_postscript.is_empty()).then(|| preset.font_postscript.clone()),
        weight: style_preview_weight(&preset.font_style),
        italic: preset.italic || native_italic,
        size_pt: size * scale as f32,
        color: Color::WHITE,
        tracking: (preset.tracking as f32 * 0.5),
        horizontal_scale: (preset.horizontal_scale / 100.0).clamp(0.5, 1.5),
        underline: preset.underline,
        strikethrough: preset.strikethrough,
        faux_bold: preset.bold && !native_bold,
        faux_italic: preset.italic && !native_italic,
        caps: if preset.all_caps { Caps::AllCaps } else if preset.small_caps { Caps::SmallCaps } else { Caps::Normal },
        baseline: if preset.superscript { FontBaseline::Superscript } else if preset.subscript { FontBaseline::Subscript } else { FontBaseline::Normal },
        ..Default::default()
    };

    let paragraph = ParagraphStyle {
        align: TextAlign::Center, // В превью центрируем образец
        ..Default::default()
    };

    let layer = TextLayer {
        text: sample.clone(),
        font_family: preset.font_family.clone(),
        size_pt: size * scale as f32,
        color: Color::WHITE,
        runs: vec![TextRun { len: sample.len(), style: char_style }],
        paragraphs: vec![ParagraphRun { len: sample.len(), style: paragraph }],
        shape: TextShape::Box { x: 0.0, y: 0.0, width: w as f32, height: h as f32 },
        antialias: style_preview_antialias(&preset.anti_alias),
        ..Default::default()
    };

    let engine = photocraft_text::try_shared()?;
    let mut engine = engine.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let layout = engine.layout(&layer, 72.0);
    let bounds = layout.bounds()?;
    let text_height = bounds[3] - bounds[1];
    let transform = photocraft_geom::Affine::translate(0.0, f64::from((h as f32 - text_height) / 2.0 - bounds[1]));

    let rendered = photocraft_text::render::rasterize(
        &layout,
        &transform,
        photocraft_color::PixelFormat::new(photocraft_color::ColorMode::Rgb, photocraft_color::SampleType::U8, true),
        layer.antialias,
    );

    let mut pixels = vec![[0u8; 4]; w * h];
    rendered.surface.read_rgba8_into(photocraft_geom::Rect::new(0, 0, w as i32, h as i32), &mut pixels);

    // Сплошной дисковый контур (убирает артефакты пилы и 8 призраков букв)
    let radius = preset.stroke_width.clamp(0.0, 10.0) * scale as f32;
    let r_int = radius.ceil() as i32;
    let r_sq = radius * radius;
    let mut offsets = Vec::new();
    if radius >= 0.5 && preset.stroke_color[3] > 0 {
        for dy in -r_int..=r_int {
            for dx in -r_int..=r_int {
                let dist_sq = (dx * dx + dy * dy) as f32;
                if dist_sq <= r_sq {
                    offsets.push((dx, dy));
                }
            }
        }
    }

    let stroke_a = preset.stroke_color[3] as f32 / 255.0;
    let text_a = preset.text_color[3] as f32 / 255.0;
    let stroke_rgb = [preset.stroke_color[0] as f32 / 255.0, preset.stroke_color[1] as f32 / 255.0, preset.stroke_color[2] as f32 / 255.0];
    let text_rgb = [preset.text_color[0] as f32 / 255.0, preset.text_color[1] as f32 / 255.0, preset.text_color[2] as f32 / 255.0];
    let mut out = vec![0u8; w * h * 4];

    for y in 0..h {
        for x in 0..w {
            let idx = y * w + x;
            let ta = pixels[idx][3] as f32 / 255.0 * text_a;
            let mut sa = 0.0f32;
            for (dx, dy) in &offsets {
                let sx = x as i32 + dx;
                let sy = y as i32 + dy;
                if sx >= 0 && sy >= 0 && sx < w as i32 && sy < h as i32 {
                    sa = sa.max(pixels[sy as usize * w + sx as usize][3] as f32 / 255.0);
                }
            }
            sa *= stroke_a;
            let out_a = sa + ta * (1.0 - sa);
            if out_a > 0.0 {
                let text_weight = ta * (1.0 - sa);
                let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                let channel = |i: usize| q((stroke_rgb[i] * sa + text_rgb[i] * text_weight) / out_a);
                let o = idx * 4;
                out[o] = channel(0);
                out[o + 1] = channel(1);
                out[o + 2] = channel(2);
                out[o + 3] = q(out_a);
            }
        }
    }

    Some(egui::ColorImage::from_rgba_unmultiplied([w, h], &out))
}
```
