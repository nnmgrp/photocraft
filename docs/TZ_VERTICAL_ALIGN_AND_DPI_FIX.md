# ТЗ для DeepSeek: Исправление вертикального центрирования TypeR и DPI-безопасность

> **Цель:** Устранить баг сползания текста вниз (Скриншот 1) и баг обрезки строк на 300–600 DPI (Дефект H3), сохранив нативную выключку `verticalAlign: center` для бабблов.  
> **Целевая ветка:** `snowe22/latest` (или `community/gpu-screentone-preview`).  
> **Файлы для правок:** `crates/ui-egui/src/lib.rs`, `crates/engine/src/snow_typesetter.rs`, `crates/ui-egui/src/snow_typesetter_ui.rs`.

---

## 1. Суть проблемы и физика бага

1. **В старом коде (Point Text):**  
   Текст вставлялся как точечный (`"x": cx, "y": cy`). В PhotoCraft/Photoshop координата `y` для Point Text — это **базовая линия первой строки**. При 6 строках текста первая строка ложилась на середину баббла, а остальные 5 строк уходили вниз за пределы баббла (верх баббла оставался пустым).
2. **В правке SnowE22 (коммит 3f4442951):**  
   Вставка была переведена на Box Text (`"box": [bx, by, bw, bh]`, `"verticalAlign": "center"`).
   * **Для баббла с выделением:** Идея сработала отлично — рамка баббла и нативный `verticalAlign: center` центрируют блок идеально.
   * **При вставке БЕЗ выделения (Баг H3):** Создаётся искусственный бокс с формулой:  
     `bw = font_size * 18.0; bh = leading * 10.0;`  
     `font_size` взят в пунктах (`pt`), а координаты бокса в пикселях (`px`). На 300 DPI (1 pt = 4.167 px) этот бокс оказывается в 4 раза меньше нужного, движок видит `overflow` и **намертво обрезает нижние строки**.

---

## 2. Что требуется реализовать в `crates/ui-egui/src/lib.rs`

Необходимо реализовать **гибридный (адаптивный) режим вставки**:

### Сценарий А: Есть выделенная область (баббл через Magic Wand / Лассо)
1. Берём границы выделения: `let bounds = sel.content_bounds();`
2. Рассчитываем координаты бокса в пикселях:
   ```rust
   let box_coords = [
       bounds.x0 as f32,
       bounds.y0 as f32,
       bounds.width() as f32,
       bounds.height() as f32,
   ];
   ```
3. Применяем солвер Кнута—Пласса для переноса строк по форме баббла.
4. Создаём **Box Text** с нативным вертикальным выравниванием по центру:
   ```json
   "box": box_coords,
   "align": "center",
   "verticalAlign": "center"
   ```

### Сценарий Б: Выделения НЕТ (клик мышью по холсту или хоткей)
**Никаких искусственных тесных рамок с жесткими коэффициентами!**
1. Создаём **Point Text** (точечный текст без обрезки по границам `box`).
2. Рассчитываем DPI документа: `let dpi = st.doc.resolution_dpi.max(72.0);`
3. Переводим интерлиньяж из пунктов в пиксели документа:
   ```rust
   let leading_px = action.preset.leading * (dpi / 72.0);
   let font_size_px = action.preset.font_size * (dpi / 72.0);
   ```
4. Считаем количество строк во `final_text`:
   ```rust
   let line_count = final_text.lines().count().max(1);
   ```
5. Рассчитываем честную базовую линию первой строки, чтобы **весь многострочный блок** центрировался вокруг точки клика `(cx, cy)`:
   ```rust
   // Высота текстового блока от первой до последней базовой линии
   let text_block_height_px = (line_count - 1) as f32 * leading_px;
   // Оптическая высота заглавной буквы (~0.7 от кегля)
   let cap_height_px = font_size_px * 0.7;

   // Базовая линия первой строки:
   // Смещаем вверх на половину высоты блока, плюс половина высоты заглавной буквы
   let baseline_y = cy - (text_block_height_px * 0.5) + (cap_height_px * 0.5);
   ```
6. Отправляем в `type.create` чистый точечный текст:
   ```json
   "x": cx,
   "y": baseline_y,
   "align": "center"
   ```
   *(Параметр "box" не передаётся. Текст никогда не обрежется ни на 72 DPI, ни на 600 DPI!)*

---

## 3. Критическое исправление H1 в `crates/engine/src/snow_typesetter.rs`

В структуре `TypesetterOptions` поле `hanging_punctuation` добавлено без значения по умолчанию. Это разрушает `typesetter_state.json` старых пользователей!

**Обязательно исправить:**
```rust
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TypesetterOptions {
    // ...
    #[serde(default)]
    pub hanging_punctuation: bool,
    // ...
}
```
Добавить юнит-тест на десериализацию старого JSON с пятью полями, чтобы убедиться, что `serde_json::from_str` парсит его без ошибок.

---

## 4. Очистка / подключение Baseline Shift в `snow_typesetter_ui.rs`

Поле `optical_baseline_factor` (слайдер Baseline Shift в UI) сейчас ни на что не влияет (дефект H6):
* Добавьте его как оптическую коррекцию к `baseline_y` в Сценарии Б:
  ```rust
  let optical_offset = font_size_px * self.typesetter.state.options.optical_baseline_factor;
  let final_y = baseline_y + optical_offset;
  ```
  Или передавайте в `baselineShift` стиля при Box Text. Слайдер должен выполнять заявленную функцию точной подстройки.

---

## 5. Правила проверки и коммита (БЕЗ РЕЛИЗНОЙ СБОРКИ)

1. ⚠️ **ПОЛНУЮ РЕЛИЗНУЮ СБОРКУ (`cargo build --release`) НЕ ДЕЛАТЬ!**  
   Экономьте время и ресурсы. Финальную сборку и визуальный тест на реальных страницах манги проведёт SnowE22.
2. Проверка типов и тестов — **СТРОГО** через скрипт-обертку:
   ```powershell
   powershell -ExecutionPolicy Bypass -Command "& 'F:\fork_home\run-cargo.ps1' check -p photocraft-ui-egui"
   powershell -ExecutionPolicy Bypass -Command "& 'F:\fork_home\run-cargo.ps1' test -p photocraft-engine -p photocraft-ui-egui"
   ```
3. После успешного прохождения тестов сделать атомарный git-коммит:
   ```bash
   git add crates/ui-egui/src/lib.rs crates/engine/src/snow_typesetter.rs crates/ui-egui/src/snow_typesetter_ui.rs
   git commit -m "fix(typer): гибридное центрирование текста по Y, защита от обрезки на 300+ DPI и serde default"
   ```
