# ТЗ: Font Settings v2 — пиксель-перфект по макету, `preview_text`, локализация

**Проект:** Photocraft Community Fork (`nnmgrp/photocraft`)  
**Репозиторий:** `F:\Downloads\Telegram Desktop\FORK\photocraft`  
**Дата:** 29 октября 2026  
**Статус:** **ЧЕРНОВИК НА УТВЕРЖДЕНИЕ. Код не менять до письменного «добро».**  
**Заменяет:** `docs/TZ_TYPER_FONT_SETTINGS_PIXEL_PERFECT.md` и раздел 2 (Font Settings) из `docs/TZ_PAGE_CLOSE_AND_FONT_SETTINGS_MODAL.md`. Старые файлы не удалять, а пометить в шапке: `Font Settings superseded by docs/TZ_FONT_SETTINGS_V2_FINAL.md`.  
**Связанные документы:** `docs/TZ_PAGES_QUICK_SAVE_BUTTON_V2.md`, `docs/TZ_PAGE_CLOSE_AND_FONT_SETTINGS_MODAL.md` (раздел 1 — закрытие страниц, остаётся в силе).  
**Макет:** `F:\Downloads\Без имени-0744102826.png` (599×707, окно 585×681 + тень).

---

## 0. Главное

1. Окно приводится к макету **1:1 по внутренней геометрии**: ширина 585, сетка 545, колонка контролов 445, высота строки 26.
2. Сохраняется **вариант B**: метка `Color:` в строке `Thickness (px)` остаётся ступенькой левее оси `Leading:`/`H-Scale (%)`, как нарисовано. Выравнивать её по общей оси запрещено.
3. `Folder` остаётся **ComboBox** (не текстовым полем).
4. В пресет добавляется **`preview_text`** — необязательный ручной текст образца. Пустая строка означает автоматический образец `«<имя стиля>: Ах ты ж!.. Неужели это сработало?! 123»`.
5. Превью исправляется: рамка 40 px, картинка 28 px, **одна строка**, `TextShape::Point`, вписывание по ширине и высоте, центрирование, сохранение обводки-диска.
6. **Все видимые строки и тултипы Font Settings** проходят через `tl!`/`i18n::t` с английскими ключами. Русские литералы в моде и её хелперах не остаются. Переводы добавляются во **все 21 каталог**.
7. Оригинальный заголовок окна с системным `✖` сохраняется. Второго `✖` в футере нет. Ширина фиксирована, высота — только Y.
8. Пока идёт foreground save, кнопка быстрого сохранения страницы скрыта — это отдельное ТЗ, здесь не трогается.

---

## 1. Цель и границы

### 1.1. Цель

Привести окно `Font Settings` к макету `Без имени-0744102826.png`:

- выровнять сетку по одной оси контролов;
- убрать «поехавшие» ширины (38/124/196/88/76/96) и заменить их макетными (43/45/145/217/95/86/116);
- исправить обрезанный предпросмотр;
- дать пользователю редактировать текст предпросмотра;
- убрать сырые русские подписи и тултипы;
- сохранить `preview_text` в пресете, `.pcraft` и TypeR JSON.

### 1.2. Границы

В рамках ТЗ:

- `crates/ui-egui/src/snow_typesetter_ui.rs` — окно, сетка, превью, тесты;
- `crates/engine/src/snow_typesetter.rs` — `TextStylePreset`, TypeR import/export;
- `crates/ui-egui/src/i18n/*.tsv` — новые ключи и переводы;
- `crates/ui-egui/src/i18n/mod.rs` — только если нужен новый тест/список ключей.

Вне рамок:

- панель Pages и её локализация;
- кнопка `💾` (отдельное ТЗ);
- стиль-карточки, тулбар `+ Style`/`+ Folder`, контекстное меню карточек — это отдельная панель, не окно Font Settings;
- смена темы, шрифтов интерфейса, title bar;
- изменение формата `.pcraft`/TypeR за пределами нового поля;
- перевод демонстрационной фразы `Ах ты ж!..`.

---

## 2. Замеры макета

### 2.1. Окно и система координат

Макет: 599×707. Окно: `x = 7..591`, `y = 13..693` (585×681; нижняя тень не входит). Заголовок — 37 px, контент начинается под ним.

Внутренняя сетка:

| Параметр | Значение | Пояснение |
|---|---:|---|
| Ширина окна | **585** | `min_width = max_width = 585` |
| Ширина контента | **551** | 585 − 34 (поля темы ≈17+17) |
| Левая ось меток | **26** | от левого края окна; 6 px внутренний отступ формы |
| Ось контролов | **126** | 26 + 94 + 6 |
| Правый край | **571** | 126 + 445; рамка поля рисуется до 572 |
| Ширина колонки контролов | **445** | общая для всех строк |
| Ширина текстовой колонки | **94** | не 100: 100 — это шаг «метка + зазор» |
| Зазор метка→контрол | **6** | `COL_GAP` |
| Высота строки | **26** | поле/кнопка/комбо — 26 |
| Высота заголовка секции | **16** | strong, 13.5 |
| Толщина разделителя | **1** | `Separator::spacing(1.0)` |

> **Важно:** в черновике фигурировало `LABEL_W = 100`. Это ошибка округления: 100 — шаг от начала метки до контрола, а сама текстовая колонка — 94 px. При `LABEL_W = 100` и зазоре 6 контрол уезжает на x = 132 вместо 126. В этом ТЗ действует **94 + 6**.

Абсолютные x даны при левом отступе формы 6 px от контента. Если тема даст сдвиг всего окна на 2–3 px, это не ошибка; ошибка — если разъезжается внутренняя ось.

### 2.2. Горизонтальная геометрия (1:1)

Все ширины — внутри колонки 445 px. Зазоры указаны явно.

| Элемент | x | Ширина | Примечание |
|---|---:|---:|---|
| Метка строки | 26 | 94 | `Style name:`, `Folder:` и т.д. |
| Контрол строки | 126 | 445 | общая ось |
| `Style name` поле | 126 | 445 | TextEdit |
| `Folder` комбо | 126 | 445 | ComboBox, не поле |
| `Font` звезда | 126 | 43 | квадратная кнопка 43×26 |
| `Font` комбо | 175 | 396 | 43 + 6 + 396 = 445 |
| `PostScript` поле | 126 | 295 | TextEdit |
| `Style` комбо | 480 | 91 | справа; метка `Style:` естественной ширины перед ним |
| `Formatting` 9 кнопок | 126… | 45 каждая | 9×45 + 8×5 = 445 |
| `Alignment` 3 кнопки | 126, 276, 426 | 145 каждая | 3×145 + 2×5 = 445 |
| `Anti-alias` комбо | 126 | 217 | правый край 343 |
| `Color` свотч (текст) | 126 | 43 | высота 24 |
| `Color` hex (текст) | 176 | 167 | 43 + 7 + 167 = 217 |
| `System` Metrics | 126 | 95 | joined |
| `System` Optical | 221 | 95 | 95+95, зазор 0 |
| `Size (pt)` значение | 126 | 86 | DragValue |
| `A-` | 217 | 45 | зазор 5 |
| `A+` | 267 | 45 | зазор 5 |
| Link `🔗/⛓` | 317 | 46 | зазор 5; правый край 363 |
| `Leading:` метка | 394 | 85 | правая группа |
| `Leading` значение | 485 | 86 | правый край 571 |
| `Tracking` значение | 126 | 86 | DragValue |
| `H-Scale (%):` метка | 394 | 85 | та же ось, что `Leading:` |
| `H-Scale (%)` значение | 485 | 86 | правый край 571 |
| `Thickness (px)` значение | 126 | 116 | DragValue |
| `Color:` метка (обводка) | 290 | 43 | **ступенька варианта B** |
| Свотч обводки | 341 | 28 | высота 24 |
| Hex обводки | 377 | 194 | 116+48+43+8+28+8+194 = 445 |
| Превью-рамка | 26 | 545 | на всю ширину формы |
| `Duplicate` | 26 | 95 | высота 30 |
| `Delete` | 129 | 85 | зазор 8; высота 30 |
| `Done` | 494 | 77 | справа; высота 30 |

Проверки арифметики:

```text
9*45 + 8*5 = 445
3*145 + 2*5 = 445
43 + 6 + 396 = 445
43 + 7 + 167 = 217
95 + 95 = 190
86 + 5 + 45 + 5 + 45 + 5 + 46 = 237
237 + 31 + 85 + 6 + 86 = 445
116 + 48 + 43 + 8 + 28 + 8 + 194 = 445
```

`PostScript` строка: поле 295, остаток 150. Справа RTL-блок: комбо 91 + зазор 6 + метка `Style:` около 42 px; остаётся ~11 px воздуха между полем и меткой. Это соответствует макету.

### 2.3. Вертикальная геометрия

Отсчёт — от верха **первой строки** (`Style name`). Верх формы — ещё +11 px от верха контента. Допуск проверки: **±3 px**; высоты строк/рамки — точные.

| Блок | Y от первой строки | Высота | Примечание |
|---|---:|---:|---|
| `Style name` | 0 | 26 | |
| `Folder` | 31 | 26 | зазор 5 после имени |
| Разделитель 1 | 67 | 1 | |
| Заголовок `Typography and Font` | 74 | 16 | |
| `Font` | 97 | 26 | |
| `PostScript` / `Style` | 130 | 26 | |
| `Formatting` | 163 | 26 | |
| `Alignment` | 197 | 26 | |
| `Anti-alias` | 231 | 26 | |
| `Color` | 263 | 26 | |
| Разделитель 2 | 300 | 1 | |
| Заголовок `Typography Size (Line Spacing Sync)` | 309 | 16 | |
| `System` | 333 | 26 | |
| `Size (pt)` / `Leading` | 365 | 26 | |
| `Tracking` / `H-Scale (%)` | 398 | 26 | |
| Разделитель 3 | 435 | 1 | |
| Заголовок `Text Outline` | 444 | 16 | |
| `Thickness (px)` / `Color` | 465 | 26 | |
| Разделитель 4 | 502 | 1 | |
| Заголовок `Preview sample` | 511 | 16 | |
| Превью-рамка | 530 | **40** | 530..570 |
| Разделитель 5 | 595 | 1 | |
| Футер | 602 | **30** | 602..632 |
| Низ формы | 633 | — | +11 сверху = 644 контента |

Итог: контент 644 px + верхний отступ 11 + нижний 4 ≈ 659; при окне 685 и заголовке ≈37 остаётся 648–659 — на грани. Поэтому:

- `default_height = 685`, `min_height = 685`;
- `ScrollArea::vertical` остаётся, но в дефолтном состоянии при 100% **скроллбар не появляется**;
- если при 150–200% DPI появится скроллбар — это допустимо, ширина не меняется;
- при появлении строки «новая папка» контент растёт, скроллбар допустим.

Рецепт вертикали: `item_spacing.y = 7`, строка 26 → шаг 33; после `Style name` разово `item_spacing.y = 5` (шаг 31); разделители — `egui::Separator::default().spacing(1.0)`; заголовки — `RichText::strong().size(13.5)`. Точные `add_space` подбираются по kittest-замеру до таблицы ±3 px, а не «на глаз».

---

## 3. Окно

### 3.1. Параметры

`crates/ui-egui/src/snow_typesetter_ui.rs`, текущий блок ~464–482:

```rust
Window::new(title)
    .id(egui::Id::new("typer_font_settings_window"))
    .open(&mut window_open)
    .collapsible(false)
    .resizable([false, true])
    .default_width(585.0)
    .min_width(585.0)
    .max_width(585.0)
    .default_height(685.0)
    .min_height(685.0)
    .pivot(egui::Align2::CENTER_CENTER)
    .default_pos(ctx.content_rect().center())
    .show(ctx, |ui| { ... });
```

- `.open(&mut window_open)` **сохранить**: это системный заголовок с `✖` и перетаскиванием.
- `.resizable([false, true])` **сохранить**: X фиксирован, Y — нет.
- `.anchor()` не возвращать.
- Второй `✖` в футер не добавлять.
- `Done` и системный `✖` идут одним путём: `close_modal = true` → `editing_preset_idx = None` → `auto_save()`.
- Заголовок остаётся `Font Settings: <имя>`; имя не переводится.

### 3.2. Отклонение: центрирование заголовка

В макете заголовок по центру. Системный title bar egui может оставить его слева. Это **принятое отклонение**: перерисовывать title bar ради центрирования нельзя, иначе теряется системное перетаскивание и `✖`. В чек-листе отметить как известное.

### 3.3. Высота 685

Предыдущая высота — 640. 685 выбран по макету и оставляет запас под контент 644 + поля. Это **осознанное изменение**; Y-ресайз сохранён. Если пользователь захочет вернуть 640 — контент придётся скроллить, что ломает 1:1. Рекомендация: 685.

---

## 4. Сетка и общие правила

### 4.1. Константы

В `render_font_settings_modal` заменить:

```rust
const LABEL_W: f32 = 94.0;
const RIGHT_W: f32 = 445.0;
const ROW_H: f32 = 26.0;
const COL_GAP: f32 = 6.0;
const FORM_LEFT_INSET: f32 = 6.0;
```

Форма:

```rust
Frame::NONE
    .inner_margin(Margin { left: 6, right: 0, top: 11, bottom: 4 })
    .show(ui, |ui| { ... });
```

`6 + 94 + 6 + 445 = 551` — ровно ширина контента. Внутри формы — 545.

### 4.2. Строка

Каждая строка — `ui.horizontal`:

```text
[ метка 94 ][ зазор 6 ][ правый блок 445 ]
```

Метка — `allocate_ui_with_layout(vec2(LABEL_W, ROW_H), Layout::left_to_right(Align::Center), ...)`; текст — `format!("{}:", tl!("Key"))`. Двоеточие добавляется кодом, в ключе его нет.

Правый блок — `allocate_ui_with_layout(vec2(RIGHT_W, ROW_H), ...)`. Внутри блока можно менять `item_spacing.x` на 5/7/8, не ломая внешнюю ось.

### 4.3. Заголовок секции

```rust
fn section_header(ui: &mut egui::Ui, key: &str) {
    ui.add_space(3.0);
    ui.add(egui::Separator::default().spacing(1.0));
    ui.label(RichText::new(tl!(key)).strong().size(13.5));
}
```

(точные `add_space` — по замеру; важно, чтобы разделитель был 1 px, а не 6 px).

### 4.4. Тема

- Фон полей — `t.field`, рамка — `t.field_border`, радиус 6.
- `Done` — `t.accent` / `t.accent_text` (не хардкодить `30,136,229`).
- Выделенные кнопки (`Metrics`, выравнивание) — `Button::selected`.
- Никаких новых цветов «из макета»: макет использует тему приложения.

---

## 5. Построчно

### 5.1. `Style name`

- Метка `Style name:`.
- `TextEdit::singleline(&mut preset.name)` шириной 445, высота 26, `vertical_align(Align::Center)`.
- Изменение → `changed = true`.
- Имя входит в авто-образец превью; при пустом `preview_text` смена имени обновляет превью.

### 5.2. `Folder`

- Метка `Folder:`.
- **ComboBox** шириной 445. `selected_text` — папка или `tl!("No folder")`.
- Пункты: `No folder`, существующие папки, `separator`, `+ New folder...`.
- `+ New folder...` → строка новой папки (label `Folder name:`, поле 240, `OK` 54, `Cancel` 72). Строка появляется только в режиме создания и может увеличить высоту/вызвать скролл; на статичную геометрию макета она не влияет.
- Новые ключи: `+ New folder...`, `New folder name` (см. §8).
- После создания папка выбирается в пресете.

### 5.3. `Font`

- Метка `Font:`.
- Звезда 43×26 (не 38): `★`/`☆`, цвет избранного — как сейчас; тултип `Favorite font`.
- Комбо семейства 396×26; при смене семейства — текущая логика подстановки PostScript/Style.
- `43 + 6 + 396 = 445`.

### 5.4. `PostScript` + `Style`

- Метка `PostScript:` (новый ключ, значение `PostScript`).
- Поле 295×26.
- Справа RTL: комбо стиля 91×26, метка `Style:` естественной ширины, зазор 6.
- Комбо — существующий `widgets::dropdown_literal`.
- Сумма: 295 + остаток 150; RTL-блок 91 + 6 + ~42 = 139; ~11 px воздуха.

### 5.5. `Formatting`

- Метка `Formatting:` (новый ключ вместо `tl!("Оформление")`).
- 9 кнопок 45×26, зазор 5: `B`, `I`, `TT`, `Tт`, `T¹`, `T₁`, `U`, `S`, `«»`.
- `character_toggle` меняет `min_size` с 38 на **45**.
- Тултипы: `Faux bold`, `Faux italic`, `All Caps`, `Small Caps`, `Superscript`, `Subscript`, `Underline`, `Strikethrough`, `Hanging Punctuation` (все через `tl!`).
- Логика взаимоисключения all/small caps и super/sub — сохранить.
- `9*45 + 8*5 = 445`.

### 5.6. `Alignment`

- Метка `Alignment:` (существующий ключ).
- 3 кнопки 145×26, зазор 5.
- Подписи: `Align Left`, `Align Center`, `Align Right` (новые ключи; в ru — «По левому», «По центру», «По правому»).
- Перед подписью — иконка из трёх горизонтальных линий 12×12:
  - left: линии 7 px, прижаты влево;
  - center: линии 7 px, по центру;
  - right: линии 7 px, прижаты вправо;
  - толщина 1.5 px, цвет текста, зазор до подписи 8 px.
- Кнопка должна сохранять `selected`-состояние. Если кастомная отрисовка мешает доступности — использовать `egui::Button` с текстом-глифом `≡` как fallback, но геометрия 145×26 и подпись обязательны.
- Тултипы — те же ключи `Align Left/Center/Right`.

### 5.7. `Anti-alias`

- Метка `Anti-alias:` (существующий ключ).
- Комбо 217×26 (не 196).
- Значения: `crisp`, `sharp`, `smooth`, `strong`, `none`.
- Отображение:

```rust
fn anti_alias_label(key: &str) -> String {
    let t = crate::i18n::t(key);
    if t == key { t.to_string() } else { format!("{t} ({key})") }
}
```

  - ru: `Чёткий (crisp)`, `Резкий (sharp)`, `Плавный (smooth)`, `Жирный (strong)`, `Нет (none)`;
  - en: `Crisp`, `Sharp`, `Smooth`, `Strong`, `None` (без скобок);
  - ja/zh/… — перевод + `(key)`, если перевод отличается.
- Правый край 343 — совпадает с правым краем текстового Color-блока (43+7+167=217).

### 5.8. `Color` (текст)

- Метка `Color:` (существующий ключ) на общей оси x=26.
- Свотч 43×24, зазор 7, hex 167×26.
- `color_swatch_with_hex_sized` обязан **учитывать** `swatch_size`, а не игнорировать его. Реализация: временно выставить `ui.spacing_mut().interact_size = swatch_size` вокруг `color_edit_button_srgba` (эта функция берёт размер из `interact_size`) или нарисовать свой свотч с попапом `color_picker_color32`.
- Hex — моноширинный, `#RRGGBBAA`, парсинг 6/8 цифр; поведение сохранить.
- `43 + 7 + 167 = 217`.

### 5.9. `System`

- Метка `System type:` (новый ключ; **не** `System` — глобальный `System` переводится как «Системная» и здесь неверен).
- Joined segmented control: `Metrics` 95 + `Optical` 95, зазор 0, общая рамка/радиус.
- `Metrics` выбран → синий левый сегмент; `Optical` выбран → синий правый.
- Тултипы:
  - `Use standard font metrics and the baseline` → ru «Метрическая система: стандартные метрики шрифта и базовая линия»;
  - `Use visual kerning and bubble centering` → ru «Оптическая система: визуальный оптический кернинг и центровка баббла».
- Ширина 190, правый край 316 (не 571) — как в макете.

### 5.10. `Size (pt)` + `Leading`

- Метка `Size (pt):`.
- Значение 86×26, `A-` 45, `A+` 45, link 46; зазоры 5.
- `Leading:` — метка 85, зазор 6, значение 86, правый край 571.
- Левая цепочка 237, правый блок 177; между ними 31 px воздуха.
- `A-`/`A+`: шаг 1 pt, минимум 4; при `line_spacing_sync` — `sync_leading_with_size`.
- Link: `🔗` при включённой связке, `⛓` при выключенной; тултипы `Leading is linked to size` / `Leading is not linked to size`.
- Метка `Leading:` и значение выровнены по правому краю с `H-Scale` (§5.11).

### 5.11. `Tracking` + `H-Scale (%)`

- Метка `Tracking:`.
- Значение 86×26.
- `H-Scale (%):` — метка 85, зазор 6, значение 86; правый край 571.
- Ключ `H-Scale (%)` (новый); в ru — `H-Scale (%)` (как в макете), в других языках — перевод.
- Тест `font_settings_leading_and_hscale_columns_align` расширить: левые края меток совпадают, правые края значений совпадают.

### 5.12. `Thickness (px)` + `Color` (обводка)

- Метка `Thickness (px):`.
- Значение 116×26 (не 96).
- Зазор 48 → метка `Color:` 43 → зазор 8 → свотч 28×24 → зазор 8 → hex 194×26.
- Метка `Color:` стоит на x=290, **левее** оси `Leading:`/`H-Scale` (394) — это вариант B, ступенька сохранена.
- Hex — тот же helper; свотч 28, не 24 и не 38.
- `116 + 48 + 43 + 8 + 28 + 8 + 194 = 445`.

### 5.13. `Preview sample` + рамка

- Метка `Preview sample:` (новый ключ; ru «Предпросмотр»).
- Рамка 545×40, отступы 12 по горизонтали и 6 по вертикали; картинка 521×28.
- `Frame::NONE.fill(t.field).stroke(1.0, t.field_border).corner_radius(6).inner_margin(Margin::symmetric(12, 6))`.
- `ui.set_min_height(28.0)`; **не** 40 — иначе рамка станет 52.
- Внутри — `render_style_preview`.

### 5.14. Футер

- `Duplicate` 95×30, `Delete` 85×30, зазор 8; левый край формы x=26.
- `Done` 77×30, справа, правый край 571; фон `t.accent`, текст `t.accent_text`, strong.
- `Done` — `tl!("Done")`, не сырой `"Done"`.
- Тултипы `Duplicate this style` / `Delete this style`.
- Второго `✖` нет.
- После `Duplicate`/`Delete` окно закрывается, как сейчас.

---

## 6. Превью v2

### 6.1. Проблема текущего рендера

Сейчас:

```rust
let width = (ui.available_width() / 4.0).floor().max(40.0) * 4.0;
let height = 40.0;
...
shape: TextShape::Box { width: w, height: h }
let size = preset.font_size.clamp(10.0, 28.0);
```

Следствия:

- рамка `inner_margin(10,6)` + `min_height(40)` + картинка 40 = **52 px**, картинка обрезается;
- `TextShape::Box` переносит длинный образец на 2+ строки, вторая строка уходит за 40 px;
- фиксированный кегль не вписывает длинное имя;
- выравнивание `TextShape::Box` центрирует по X, но не по Y корректно при обрезке.

### 6.2. Целевой алгоритм

`render_style_preview(ui, preset)`:

1. `w = (ui.available_width() - 24.0).max(40.0)`; `h = 28.0`.
2. `scale = pixels_per_point().ceil().clamp(1,3)`.
3. `style_preview_texture(ctx, preset, w, h, scale)`; если `None` — `tl!("Preview unavailable")`.
4. `ui.add(egui::Image::new((texture.id(), vec2(w, h))))`; рамка уже даёт 40 px.

`style_preview_image`:

1. `sample = style_preview_text(preset)` (см. §7); заменить `\r`/`\n` на пробелы.
2. `base = preset.font_size.clamp(10.0, 28.0) * scale`.
3. Построить `TextLayer` с `TextShape::Point` (одна строка, без переносов), `size_pt = base`, `tracking`, `horizontal_scale`, `bold/italic/caps/baseline/underline/strikethrough`, `antialias`.
4. `layout = engine.layout(&layer, 72.0)`; `bounds = layout.bounds()?`.
5. `text_w = bounds[2]-bounds[0]`, `text_h = bounds[3]-bounds[1]`.
6. `pad = preset.stroke_width.clamp(0,30) * scale` — запас под обводку.
7. `fit = min(1.0, (w - 2)/(text_w + 2*pad), (h - 2)/(text_h + 2*pad))`.
   - `fit <= 0` или не finite → `None`;
   - `fit` не больше 1.0: превью не увеличивает мелкий кегль, только уменьшает длинный образец.
8. `cx = (bounds[0]+bounds[2])/2`, `cy = (bounds[1]+bounds[3])/2`.
9. `transform = Affine::translate(-cx,-cy).then(&Affine::scale(fit)).then(&Affine::translate(w/2, h/2))`.
10. `ink = render::ink_rect(&layout, &transform)`; пустой → `None`; защита от малформед-шрифтов (`ink.width() > w*4` и т.п.) остаётся.
11. `rasterize(&layout, &transform, ..., layer.antialias)`.
12. `radius = pad * fit` — обводка масштабируется вместе с текстом; диск-дилатация и композитинг **не менять** (тесты `style_preview_porter_duff_text_over_stroke`, `style_preview_stroke_band_has_no_holes` должны остаться зелёными).
13. Вернуть `ColorImage` w×h.

Ключевые моменты:

- `TextShape::Point` — гарантия одной строки; `Box` запрещён.
- Вписывание и по ширине, и по высоте; `w - 2`/`h - 2` — 1 px страховки от срезания антиалиаса.
- Центрирование по bounds, а не по baseline.
- Обводка входит в запас и масштабируется; иначе после `fit` она вылезет за кадр.

### 6.3. Кэш

`style_preview_key` уже хэширует `style_preview_text(preset)`, шрифт, кегль, leading/tracking/h-scale, bold/italic/caps/baseline/underline/strikethrough, stroke, цвета, align, AA, размер и scale. После того как `style_preview_text` начнёт учитывать `preview_text`, ключ автоматически станет правильным. Добавить тест: два пресета, отличающиеся только `preview_text`, дают разные ключи и разные картинки.

### 6.4. Редактирование текста превью

**Состояние покоя.** В рамке — картинка. При наведении (или фокусе) в правом верхнем углу рамки появляется кнопка `✎` 18×18 с тултипом `Edit preview text`. Кнопка рисуется поверх, layout не сдвигает. Двойной клик по рамке — тот же вход в редактирование.

**Режим редактирования.**

- Внутри рамки вместо картинки — `TextEdit::singleline`, ширина рамки − 24, высота 26, по центру.
- Поле предзаполняется **эффективным образцом** (авто или текущий `preview_text`), текст выделен, фокус запрошен.
- Hint при пустом значении: `Leave empty for the automatic sample`.
- Пока идёт редактирование, `✎` скрыт.
- `Enter` или потеря фокуса — коммит.
- `Esc` — восстановить исходное значение, выйти без изменений.
- Клик по `Done` или системному `✖` — сначала коммит, потом закрытие. Для этого нужен общий `commit_preview_edit(ui, preset)`; он вызывается из обработчика `Done`, из ветки `!window_open` и из `lost_focus`.
- При коммите:
  - `trim()`, `\r`/`\n` → пробел, обрезка до 200 символов по границе char;
  - пустая строка → `preview_text = ""` (авто);
  - если пользователь не менял авто-образец и `preview_text` был пуст — оставить пустым, чтобы имя продолжало обновлять образец;
  - иначе сохранить введённый текст;
  - `changed = true` → существующий `auto_save()`.
- Состояние редактирования можно хранить в `ui.data` (например, `PreviewEdit { idx, buffer, original, request_focus }`) или в поле `TypesetterUi`; на закрытие окна его очищать.
- Пока идёт редактирование, текст в пресет не пишется; превью обновится после коммита.

**Доступность.** У `✎` есть тултип; у поля — hint. Двойной клик — не единственный путь.

### 6.5. Пустой/битый шрифт

Если `layout.bounds()` пуст, `fit` не finite или `rasterize` не дал картинку — в рамке `tl!("Preview unavailable")` цветом `t.text_faint`, без паники и без изменения высоты рамки. Рамка остаётся 40 px.

---

## 7. `preview_text`: данные и TypeR

### 7.1. Поле пресета

`crates/engine/src/snow_typesetter.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextStylePreset {
    pub name: String,
    #[serde(default)]
    pub preview_text: String,
    pub folder: Option<String>,
    ...
}
```

- `#[serde(default)]` — старые `.pcraft`/JSON без поля читаются.
- `Default` — `preview_text: String::new()`.
- Поле входит в `PartialEq`/`Clone` автоматически; `duplicate_preset` копирует его.
- В `TypesetterPersistedState` отдельное поле не нужно: пресеты уже сериализуются целиком.

### 7.2. Эффективный образец

```rust
const PREVIEW_SAMPLE: &str = "Ах ты ж!.. Неужели это сработало?! 123";

fn style_preview_auto_text(preset: &TextStylePreset) -> String {
    let name = preset.name.trim();
    let mut s = if name.is_empty() {
        PREVIEW_SAMPLE.to_string()
    } else {
        format!("{name}: {PREVIEW_SAMPLE}")
    };
    truncate_chars(&mut s, 200);
    s
}

fn style_preview_text(preset: &TextStylePreset) -> String {
    let custom = preset.preview_text.trim().replace(['\r', '\n'], " ");
    if custom.is_empty() {
        style_preview_auto_text(preset)
    } else {
        let mut s = custom;
        truncate_chars(&mut s, 200);
        s
    }
}
```

- `truncate_chars` — по границе UTF-8, с `…` в конце.
- Демонстрационная фраза остаётся русской: это образец глифов, а не UI-строка. В i18n её не добавлять.
- Пустое имя → только фраза.
- Пробелы по краям `preview_text` срезаются; переводы строк превращаются в пробелы.

### 7.3. Импорт TypeR

В `import_typer_json` (текущий `~346–580`) рядом с `name`/`folder`:

```rust
let preview_text = item
    .get("preview_text")
    .or_else(|| item.get("previewText"))
    .and_then(|v| v.as_str())
    .unwrap_or("")
    .to_string();
```

И в литерал `self.presets.push(TextStylePreset { ... })` (текущий `~554`) добавить `preview_text,` — иначе компилятор поймает пропущенное поле.

### 7.4. Экспорт TypeR

В объекте стиля (текущий `~621`):

```rust
styles_arr.push(serde_json::json!({
    "name": preset.name,
    "folder": folder_id,
    "preview_text": preset.preview_text,
    "textProps": { ... }
}));
```

- Поле — на верхнем уровне стиля, не внутри `textProps`.
- Экспортируется всегда, в том числе пустая строка: это явный сигнал «авто».
- `previewText` при импорте — алиас для совместимости; экспорт — только `preview_text`.

### 7.5. Кэш и тесты

- `style_preview_key` хэширует `style_preview_text`, поэтому `preview_text` попадает в ключ.
- Тест JSON round-trip: пресет с `preview_text = "Custom"` → JSON → обратно → поле совпадает.
- Тест TypeR: экспорт содержит `preview_text`; импорт `preview_text` и `previewText` работает; отсутствие поля → пустая строка.
- Тест `.pcraft`-совместимости: serde JSON без поля читается, `preview_text == ""`.

---

## 8. Локализация

### 8.1. Правило

- Каждая видимая строка и каждый тултип в `render_font_settings_modal` и её хелперах — через `tl!("English key")` или `i18n::t(key)`.
- Сырые русские литералы в моде запрещены. Сырые английские — тоже, если это не символ (`B`, `I`, `TT`, `Tт`, `T¹`, `T₁`, `U`, `S`, `«»`, `A-`, `A+`, `✎`, `🔗`, `⛓`).
- `Formatting`, `PostScript`, `System type`, `H-Scale (%)`, `Align Left/Center/Right`, `Preview sample`, `Preview text`, `Edit preview text`, `Leave empty for the automatic sample`, `Preview unavailable`, `Faux bold`, `Faux italic`, `Leading is linked...`, `Leading is not linked...`, `Use standard...`, `Use visual...`, `+ New folder...`, `New folder name` — новые ключи.
- `System` **не использовать**: глобальный перевод «Системная» относится к системному шрифту. Для строки измерения — только `System type`.
- `Preview` **не использовать** для заголовка: он переводится как «Просмотр», а в макете — «Предпросмотр». Только `Preview sample`.
- `tl!("Оформление")` удалить; заменить на `tl!("Formatting")`.

### 8.2. Новые ключи и русские значения

| English key | Русский | Примечание |
|---|---|---|
| `+ New folder...` | `+ Новая папка...` | уже используется, но отсутствует в каталогах |
| `New folder name` | `Имя новой папки` | hint поля |
| `Formatting` | `Оформление` | заменить русский ключ |
| `PostScript` | `PostScript` | proper noun; entry обязателен |
| `System type` | `Система` | не путать с `System` |
| `H-Scale (%)` | `H-Scale (%)` | как в макете; в других языках можно перевести |
| `Align Left` | `По левому` | кнопка |
| `Align Center` | `По центру` | кнопка |
| `Align Right` | `По правому` | кнопка |
| `Preview sample` | `Предпросмотр` | заголовок |
| `Preview text` | `Текст предпросмотра` | доступность/поле |
| `Edit preview text` | `Изменить текст предпросмотра` | тултип `✎` |
| `Leave empty for the automatic sample` | `Оставьте пустым для автоматического образца` | hint |
| `Preview unavailable` | `Предпросмотр недоступен` | fallback |
| `Faux bold` | `Псевдожирный` | тултип B |
| `Faux italic` | `Псевдокурсив` | тултип I |
| `Leading is linked to size` | `Интерлиньяж связан с кеглем (меняется пропорционально)` | тултип link |
| `Leading is not linked to size` | `Интерлиньяж не связан с кеглем (независим)` | тултип link |
| `Use standard font metrics and the baseline` | `Метрическая система: стандартные метрики шрифта и базовая линия` | тултип Metrics |
| `Use visual kerning and bubble centering` | `Оптическая система: визуальный оптический кернинг и центровка баббла` | тултип Optical |

Существующие ключи, которые используются и **не** дублировать: `Font Settings`, `Style name`, `Folder`, `No folder`, `Folder name`, `OK`, `Cancel`, `Typography and Font`, `Font`, `Favorite font`, `Style`, `Alignment`, `Anti-alias`, `Crisp`, `Sharp`, `Smooth`, `Strong`, `None`, `Color`, `Typography Size (Line Spacing Sync)`, `Metrics`, `Optical`, `Size (pt)`, `Leading`, `Tracking`, `Text Outline`, `Thickness (px)`, `Duplicate`, `Delete`, `Done`, `All Caps`, `Small Caps`, `Superscript`, `Subscript`, `Underline`, `Strikethrough`, `Hanging Punctuation`, `Duplicate this style`, `Delete this style`.

### 8.3. Anti-alias

Ключи `crisp`/`sharp`/`smooth`/`strong`/`none` уже есть в каталогах как `Crisp`/`Sharp`/`Smooth`/`Strong`/`None`. Использовать `i18n::t(key)` и добавлять `(key)` только если перевод отличается от ключа. Это даёт ровно макетное `Чёткий (crisp)` и не даёт `Crisp (crisp)` в английском.

### 8.4. Все 21 каталог

Добавить новые ключи в:

```text
cs.tsv  de.tsv  el.tsv  es.tsv  fr.tsv  hi.tsv  hu.tsv  id.tsv  it.tsv
ja.tsv  ko.tsv  nl.tsv  pl.tsv  pt-br.tsv  ru.tsv  sr.tsv  tr.tsv  uk.tsv
vi.tsv  zh-hans.tsv  zh-hant.tsv
```

Требования:

- ни одного пустого значения/placeholder;
- перевод из смысла английского, в терминологии уже существующего каталога (clean-room, как в шапке `ja.tsv`);
- `PostScript` может остаться `PostScript`;
- `H-Scale (%)` в ru — `H-Scale (%)`, в de/fr и т.д. — перевод;
- `Metrics`/`Optical` — существующие значения, не менять;
- `hi.tsv` тоже заполнить, хотя `complete_menus = false`.

### 8.5. Тесты локализации

- `every_tl_literal_is_translated` — все новые `tl!("...")` есть в каждом каталоге с `complete_menus`; для `hi` проверить отдельным тестом/скриптом.
- `i18n_coverage` (`xtask`) — все ключи, все языки.
- Новый тест `font_settings_modal_has_no_raw_cyrillic`: взять `include_str!("snow_typesetter_ui.rs")`, вырезать `fn render_font_settings_modal` до следующего `fn`, проверить отсутствие кириллицы. Демонстрационная фраза находится в `style_preview_text` и в проверку не попадает.
- Новый тест `anti_alias_labels_are_localized`: для en/ru/ja проверить, что `anti_alias_label("crisp")` даёт `Crisp` / `Чёткий (crisp)` / `クリスプ (crisp)` (точный ja-перевод — из каталога).
- Ручная проверка: переключить en → ru → ja → de; в окне не должно остаться русского/английского сырья, кроме символов и `PostScript`.

---

## 9. Тесты

### 9.1. Геометрия (kittest)

`crates/ui-egui/src/snow_typesetter_ui.rs`, модуль tests.

1. **`font_settings_window_is_585_wide`**  
   Открыть окно, `area.width()` = 585 ± 2; `area.height()` = 685 ± 4; `min_width == max_width == 585`.

2. **`font_settings_character_toggles_stay_on_one_row`** (обновить существующий)  
   - 9 кнопок на одной строке;
   - ширина окна 585 (было 520);
   - каждая кнопка 45×26 ± 1;
   - правый край последней ≤ правого края окна.

3. **`font_settings_rows_share_control_axis`**  
   Проверить левые края меток `Style name:`, `Folder:`, `Font:`, `PostScript:`, `Formatting:`, `Alignment:`, `Anti-alias:`, `Color:` (первая), `Size (pt):`, `Tracking:`, `Thickness (px):` — все равны ±1 px.  
   Для контролов: если kittest даёт rect поля/кнопки — правые края 445-колонки равны ±1 px; иначе проверить арифметику констант отдельным unit-тестом.

4. **`font_settings_color_label_keeps_mockup_step`**  
   Два `Color:`: текстовая метка на x≈26, обводочная — на x≈290 (±3). Обводочная **не** равна оси `Leading:` (394). Hex обводки правый край = 571 ± 2.

5. **`font_settings_leading_and_hscale_columns_align`** (расширить)  
   Левые края `Leading:`/`H-Scale (%):` равны ±1; правые края их значений равны ±1; значения 86×26.

6. **`font_settings_preview_frame_is_40_high`**  
   Сохранить rect рамки в `ui.data` (например, `typesetter-preview-frame`) и проверить: высота 40 ± 1, ширина 545 ± 2, картинка внутри 28 ± 1.

7. **`font_settings_footer_button_sizes`**  
   `Duplicate` 95×30, `Delete` 85×30, `Done` 77×30 (±1); `Done` правый край 571 ± 2.

### 9.2. Превью

1. **`style_preview_fits_a_long_sample_on_one_line`** (усилить)  
   Длинное имя/`preview_text`; `style_preview_image(..., 180, 28, 1)`; проверить:
   - картинка 180×28;
   - `style_preview_layer(...).shape == TextShape::Point`;
   - `layout.lines.len() == 1`;
   - ink не выходит за [1, w-1] и [1, h-1];
   - есть непрозрачные пиксели.

2. **`style_preview_uses_preview_text_and_cache_key`**  
   Два пресета, отличающиеся только `preview_text`; ключи разные, картинки разные.

3. **`style_preview_empty_text_falls_back_to_auto_sample`**  
   `preview_text = ""` → sample содержит имя и фразу; `preview_text = "Custom"` → ровно `Custom`.

4. **`style_preview_porter_duff_text_over_stroke`**, **`style_preview_stroke_band_has_no_holes`** — оставить зелёными. Если `fit` меняет размер, проверить, что тестовые размеры 120×40/140×48 дают `fit == 1.0` (короткий `Aa`), иначе подобрать размеры так, чтобы тест проверял диск, а не масштабирование.

5. **`preview_text_commits_on_enter_and_esc_restores`**  
   Юнит-тест на `commit_preview_edit`/чистую функцию: Enter сохраняет trimmed, Esc возвращает original, пусто → `""`.

### 9.3. Данные

1. **`preview_text_round_trips_in_preset_json`** — serde JSON.
2. **`preview_text_survives_typer_export_import`** — export/import, оба алиаса.
3. **`preview_text_defaults_to_empty_for_old_json`** — без поля.
4. **`preview_text_is_in_preview_key`** — ключ меняется.

### 9.4. Локализация

1. `every_tl_literal_is_translated`.
2. `i18n_coverage`.
3. `font_settings_modal_has_no_raw_cyrillic`.
4. `anti_alias_labels_are_localized`.

### 9.5. Регрессии

- `font_settings_window_can_be_dragged` — остаётся.
- `docked_panel_opens_the_style_dialog` — остаётся.
- `font_settings_leading_and_hscale_columns_align` — обновить, не удалять.
- `style_preview_*` — все зелёные.
- Нет новых clippy-предупреждений.
- `.pcraft`/TypeR старых файлов читаются.

---

## 10. Аудит GLM и текущего рабочего дерева

В рабочем дереве уже есть незакоммиченная реализация GLM (`snow_typesetter_ui.rs` +1155 строк). Её нельзя принимать на веру. Что оставить и что переделать:

| Место | Сейчас (GLM/v1) | Вердикт |
|---|---|---|
| Окно | 520×640, `min/max_width=520`, `resizable([false,true])`, `.open` | **Ширину/высоту переделать** на 585×685; `.open` и Y-ресайз сохранить |
| Сетка | `LABEL_W=100`, `RIGHT_W=382`, `ROW_H=26`, `COL_GAP=6` | **Переделать** на 94/445/26/6 + left inset 6; иначе контролы на 126 не встанут |
| `Font` звезда | 38 | **43** |
| `PostScript` | поле 230, комбо 98 | **295 / 91** |
| `Formatting` | 9×38 + 8×5 = 382 | **9×45 + 8×5 = 445**; `character_toggle` 45 |
| `Alignment` | 3×124, только текст | **3×145 + иконки**; новые ключи |
| `Anti-alias` | 196, сырые русские подписи | **217**, `i18n::t` + `(key)` |
| `Color` текст | свотч 38, hex 152, helper игнорирует размер | **43 + 7 + 167**; helper обязан учитывать `swatch_size` |
| `System` | 2×88, сырое «Система:» | **2×95**, `System type` |
| `Size/Leading` | 76/36/36/38, Leading 76+82 | **86/45/45/46**, Leading 85+6+86 |
| `Tracking/H-Scale` | 76/76+82, сырое `H-Scale (%)` | **86 / 85+6+86**, ключ `H-Scale (%)` |
| `Thickness/Color` | 96, `add_space(32)`, динамический hex | **116 + 48 + 43 + 8 + 28 + 8 + 194**, ступенька сохраняется |
| Превью-рамка | `inner_margin(10,6)`, `min_height(40)`, картинка 40 → 52, обрезка | **inner_margin(12,6), картинка 28, рамка 40** |
| Превью-текст | `TextShape::Box`, фиксированный кегль, хардкод | **Point + fit + center + stroke**; `preview_text` |
| `preview_text` | отсутствует | **добавить** в пресет, импорт/экспорт, кэш |
| `Font Settings` строки | `tl!("Оформление")`, сырые «Выравнивание:», «Система:», «Предпросмотр:», тултипы | **все через `tl!`/`i18n::t`**, новые ключи |
| Футер | 88/78/72, height 28, сырой `"Done"` | **95/85/77, height 30, `tl!("Done")`** |
| `+ New folder...` / `New folder name` | используются, но отсутствуют во всех каталогах | **добавить во все 21** |
| `Preview unavailable` | используется, отсутствует в каталогах | **добавить** |
| Тест ширины | утверждает 520 | **585** |
| `style_preview_fits...` | проверяет только размер/непустоту | **усилить**: Point, одна строка, fit |
| Stroke-тесты | корректны | **сохранить зелёными** |
| `font_settings_leading_and_hscale_columns_align` | корректен, но старые ширины | **обновить** на 85/86 |
| `color_swatch_with_hex_sized` | `_swatch_size` игнорируется | **использовать размер** |
| `style_preview_key` | хэширует sample; sample пока без `preview_text` | после §7 ключ станет правильным; добавить тест |

Что в GLM сделано хорошо и **не** переписывать без нужды:

- диск-дилатация обводки и композитинг (`style_preview_*stroke*`);
- парсинг hex/alpha;
- kittest-харнесс и тест перетаскивания;
- хэширование всех параметров превью;
- структура импорта/экспорта TypeR;
- `auto_save` на изменениях.

---

## 11. Ручной чек-лист

- [ ] Окно 585×685, ширина не тянется, высота тянется; системный `✖` закрывает и сохраняет; второго `✖` нет.
- [ ] Заголовок перетаскивается; после перетаскивания не прыгает в центр.
- [ ] Все метки начинаются на одной оси; все контролы — на одной оси; правые края полей/кнопок прижаты к 571.
- [ ] `Formatting` 9 кнопок в одну строку, без переноса и горизонтального скролла.
- [ ] `Alignment` 3 кнопки 145 с иконками; выбранная подсвечена.
- [ ] `Color:` текста — на общей оси; `Color:` обводки — ступенькой, как в макете.
- [ ] `Anti-alias` показывает `Чёткий (crisp)` в ru и `Crisp` в en; в ja/zh — перевод + `(key)`.
- [ ] `System` — 2 сегмента по 95, без зазора; тултипы локализованы.
- [ ] `Size/Leading` и `Tracking/H-Scale` — правые значения на одной оси.
- [ ] Превью-рамка ровно 40 px; картинка не обрезана; длинное имя/текст сжимается в одну строку; обводка не рвётся.
- [ ] `✎` появляется по наведению; двойной клик открывает редактор; Enter/потеря фокуса сохраняют; Esc отменяет; `Done`/`✖` сохраняют.
- [ ] Пустой `preview_text` → авто-образец; ввод своего текста → он; очистка → снова авто.
- [ ] Смена имени стиля при пустом `preview_text` обновляет авто-образец; при своём тексте — не обновляет.
- [ ] `preview_text` виден в TypeR JSON и переживает импорт/экспорт; старый JSON без поля читается.
- [ ] Переключение en/ru/ja/de: в окне нет сырых русских/английских подписей и тултипов, кроме символов и `PostScript`.
- [ ] 100%/150%/200% DPI: ширина та же, ничего не разъезжается, при необходимости появляется вертикальный скролл.
- [ ] `+ New folder...` работает, новая строка не ломает окно.
- [ ] `Duplicate`/`Delete`/`Done` в футере, размеры 95/85/77×30.
- [ ] Старые stroke/fit/TypeR тесты зелёные.

---

## 12. Команды проверки

Все Cargo-команды — только через обёртку:

```powershell
powershell -ExecutionPolicy Bypass -Command "& 'F:\fork_home\run-cargo.ps1' check"
powershell -ExecutionPolicy Bypass -Command "& 'F:\fork_home\run-cargo.ps1' test -p photocraft-ui-egui --lib -- snow_typesetter_ui"
powershell -ExecutionPolicy Bypass -Command "& 'F:\fork_home\run-cargo.ps1' test -p photocraft-engine snow_typesetter"
powershell -ExecutionPolicy Bypass -Command "& 'F:\fork_home\run-cargo.ps1' test -p photocraft-ui-egui i18n"
powershell -ExecutionPolicy Bypass -Command "& 'F:\fork_home\run-cargo.ps1' test -p photocraft-ui-egui --lib"
```

Если `--lib -- snow_typesetter_ui` не запускает нужный модуль — использовать фильтр `snow_typesetter` и отдельно `font_settings`. Главное: прогнать новые тесты, stroke-тесты, i18n и TypeR.

---

## 13. Вне рамок

- Кнопка `💾` и панель Pages — `TZ_PAGES_QUICK_SAVE_BUTTON_V2.md`.
- Локализация панели Pages и стиль-карточек.
- Изменение темы/токенов, кроме использования `t.accent` для `Done`.
- Перерисовка системного title bar.
- Новые форматы экспорта, версия TypeR, миграции `.pcraft`.
- Автоперевод демонстрационной фразы.
- Изменение поведения `Duplicate`/`Delete`, кроме размеров и локализации.
- Горизонтальный ресайз окна.

---

## 14. Вопросы на утверждение

1. **Высота 685** вместо прежних 640 (Y-ресайз сохранён) — подтвердить, что это ок.
2. **`LABEL_W = 94` + зазор 6** (а не 100 + 6): 100 — это шаг до контрола; при 100 контрол уезжает на 132. Подтвердить.
3. **`H-Scale (%)` в ru остаётся латиницей** (как в макете), в остальных языках переводится. Подтвердить.
4. **Футер 30 px** (в макете замерено ~26): 30 выбрано для попадания и единообразия с кнопками. Подтвердить.
5. **Системный title bar** и возможное нецентрирование заголовка — принятое отклонение. Подтвердить.
6. **Переводы 19 языков** генерируются из английского смысла и добавляются во все каталоги; пользователь вычитывает после реализации. Подтвердить.
7. **`preview_text` до 200 символов**, пусто = авто; демонстрационная фраза остаётся русской. Подтвердить.

Если хотя бы один пункт неверен — сказать номер и желаемое значение; код после этого не начинать.

---

## 15. Итог

Font Settings v2 — это не «подкрасить окно», а точная сетка 545/445 с одной осью контролов, сохранённой ступенькой `Color:` и честным превью на одну строку. `preview_text` живёт в пресете, переживает `.pcraft` и TypeR, а пустое значение возвращает авто-образец. Все подписи и тултипы проходят через i18n во всех 21 языке. Системный заголовок с `✖` и Y-ресайз остаются; второй `✖` не добавляется.

Код не трогать до утверждения обоих ТЗ: этого и `TZ_PAGES_QUICK_SAVE_BUTTON_V2.md`.
