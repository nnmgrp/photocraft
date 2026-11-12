# Отчёт о слиянии TypeR Studio (296a3a568) — для Antigravity

Документ описывает состояние ветки `community/gpu-screentone-preview` после ручного слияния
с портом TypeR Studio, модуля манга-бабблов / SAI-Linework и правок стабилизации памяти SAI.
Цель документа — дать следующему агенту (Antigravity) карту изменений, проверяемые команды,
список известных оговорок и незакрытых пунктов, чтобы продолжить работу без повторного
разбора слияния.

---

## 0. Коротко

| Что | Статус |
|---|---|
| Слияние TypeR Studio (ядро + UI) | сделано, коммит `de1a2b327` |
| Манга-бабблы + Live Gizmo + SAI Linework | сделано, коммит `de1a2b327` |
| Память SAI (коалесценция undo + автоскролл линейки) | сделано, коммит `de1a2b327` |
| Перевод 6 новых строк в 21 каталог | сделано, отдельный коммит |
| Починка 3 упавших тестов дока (вкладка Linework) | сделано, отдельный коммит |
| Push в `origin/community/gpu-screentone-preview` | выполнен (3 коммита) |
| Открытых пунктов | 5 (раздел 9) — ни один не блокирует работу |

Ветка ушла в `origin` тремя коммитами поверх `6ea51781d`:

```
de1a2b327  feat(comic): merge TypeR Studio 296a3a568 with the manga balloons and the SAI linework
<hash-1>   i18n: translate the six new strings of the TypeR Studio merge
<hash-2>   test(ui-egui): keep the dock tests in step with the fourth Layers tab
```

---

## 1. Исходные данные и метод

**Репозиторий форка:** `F:\Downloads\Telegram Desktop\FORK\photocraft`
**Ветка:** `community/gpu-screentone-preview`
**База в origin на момент работы:** `6ea51781d`

**Источник (порт TypeR Studio):**

| Что | Значение |
|---|---|
| Бандл (эталон) | `F:\Downloads\photocraft-vector-curve-studio (3)\` |
| Коммит порта | `296a3a568` |
| Ветка порта в форке | `friend/community/gpu-screentone-preview` |
| Точка слияния (merge-base) | `9547800d2` |

**Метод — ручное 3-way слияние, без потерь:**

1. Для каждого конфликтного файла бралась база `9547800d2`, версия форка (`HEAD`) и версия
   порта (`296a3a568`).
2. Слияние выполнялось в три колонки (`git merge-file -p --diff3`), а не «слепым» `git merge`
   / `git checkout --theirs`: так видно, чья строка и в каком блоке.
3. `crates/doc/src/vector.rs` и `crates/vector/src/balloon.rs` **не перезаписывались целиком** —
   правки вносились точечно поверх общего текста (ТЗ-запрет).
4. Никаких моков вида `s.get_layer_custom_data` / `s.update_shape_layer`: работа с формами идёт
   только через `with_shape` и макрос `spec!` (правила `AGENTS.md`).
5. Доработки Pages (`comic_pages_panel.rs`, `activate_page`, `can_quick_save`,
   `photocraft_toggle_pages_cmd`, `photocraft_toggle_split_dock_cmd`) сохранены без изменений.
6. `TypesetterUi::default()` сохраняет `is_open: false`.

**Правило по UI (из решения владельца):** UI порта — эталон, но редактор стилей (Font Settings)
форка прицепляется поверх; плюсы обеих сторон соединяются. Отсюда: база UI — файл порта,
наш редактор стилей — прицепка, контур-обвязка (Bubble Contour) — возвращена в заголовок панели.

---

## 2. Блок А — TypeR Studio (ядро)

### 2.1 Движок

| Файл | Что сделано |
|---|---|
| `crates/engine/src/snow_knuth_plass.rs` | Файл порта взят целиком (пагинация, переносы, разбиение на абзацы) — это эталон разметки текста. |
| `crates/engine/src/snow_typesetter.rs` | 3-way: поля/импорты порта + наши `align`, `auto_clean` и блок контура (`BUBBLE_OUTLINE_LUMA`, `surface_luma`, `detect_speech_bubble_bounds_in_surface`, `contour_variants`, `push_contour_variant`) + его `contour_profile`. |
| `crates/engine/src/snow_typesetter.rs` | Добавлен `TextStylePreset::effective_align()` — снимает конфликт двух полей выравнивания (см. 9.2). |
| `crates/psd/src/tpl.rs` (новый) | Портированный TPL-читал порта перенесён в `photocraft-psd`; в `crates/psd/src/lib.rs` добавлен `pub mod tpl;`, в `crates/psd/src/descriptor.rs` — `Descriptor::parse_prefix`. |

### 2.2 UI

| Файл | Что сделано |
|---|---|
| `crates/ui-egui/src/snow_typesetter_ui.rs` | База — версия порта (~186 КБ). Наш редактор стилей (`render_font_settings_modal` и его хелперы) **прицеплен сверху**. Хелперы вынесены на модульный уровень (иначе `E0425`: они не могут быть методами `impl TypesetterUi`, т.к. используются вне трейта). Добавлены константы раскладки `FORM_LEFT_INSET / LABEL_W / COL_GAP / RIGHT_W / ROW_H / FOOTER_H / PREVIEW_H / PREVIEW_SAMPLE`, тип `PreviewEdit`, импорты `Caps`, `TextShape`, `photocraft_doc::TextLayer`. В `render_header_toolbar` возвращено меню **Bubble Contour** (сигнатура получила `action: &mut Option<TypesetterAction>`). `is_open: false` в `default()` сохранён. |
| `crates/ui-egui/src/lib.rs` | Перенесена обвязка: поля `typesetter_paste_on_click_was_enabled`, `typesetter_paste_fingerprint`, `typesetter_text_shape_source` + их инициализация; 10 методов (`typesetter_requests`, `refresh_text_shape_previews_from_page`, `refresh_typesetter_text_shape`, `apply_typesetter_text_shape`, `maybe_typesetter_paste_on_click`, `typesetter_clean_selection`, `capture_typesetter_bubble`, `fill_typesetter_bubbles`, `apply_typesetter_style_to_active_layer`, `align_typesetter_layer`); свободные функции `strip_typer_markdown`, `is_punctuation_only`; вызов `self.typesetter_requests();` в цикле кадра. |
| `crates/ui-egui/src/lib.rs` | `handle_typesetter_action` сохранён и расширен: markdown-сегменты, `punctuation_only_scale`, fit-to-contour, абзацные параметры, `effective_align()`. |
| `crates/ui-egui/src/font_preview.rs` | Наш предпросмотр шрифта сохранён, поправлен под clippy (см. 8.3). |
| `crates/ui-egui/src/corner_radius.rs`, `vector_ui.rs` | Точечная подгонка под новые типы. |

---

## 3. Блок В — Manga Balloons / Live Gizmo / SAI Linework

### 3.1 Модель документа

`crates/doc/src/vector.rs` (+267 строк):

* `TailPreset` — 6 пресетов хвоста;
* `VertexOverride` — переопределение вершины контура;
* `LiveBalloon` — параметрический бабл с 5 телами (`Ellipse`, `Rect`, `Cloud`, `Spiky`, `Uniflash`);
* `LiveBalloonTail`, `LiveBalloonGroup` — хвост и группа баблов;
* новый вариант `LiveShape::BalloonGroup` (учтён в `crates/io/src/vector_map.rs` → `None`).

### 3.2 Геометрия

`crates/vector/src/balloon.rs` (+835/−…):

* трейты `LiveBalloonGeom` / `LiveBalloonGroupGeom` — геометрия вынесена за пределы крейта
  `photocraft-doc` (обход orphan rule Rust: impl трейта для чужого типа должен жить рядом с ним);
* `merge_paths_union`, `polygons_bbox_overlap`, `build_superellipse_path`,
  `balloon_to_linework_strokes` — построение/объединение путей и конвертация бабла в штрихи;
* `LegacyTailPreset` — совместимость со старыми хвостами;
* `build_spiky_body` переведён на структуру `SpikyBody` (требование clippy `too_many_arguments`).

`crates/vector/src/comic_sai_curve.rs`:

* `SaiCurveType` (Normal / Bezier), плотность штриха `SaiPoint.density`,
  `LiveVectorStroke.{base_density, min_density_ratio, curve_type}`, `effective_tension()`, `alpha_at()`;
* лимит `MAX_TOTAL_SAMPLES = 200_000` в `sample_sai_curve` — защита от взрыва числа точек.

### 3.3 Команды движка

`crates/engine/src/balloon_cmds.rs`, `crates/engine/src/vector_cmds.rs` — новые команды
(зарегистрированы через `spec!` в `commands.rs`):

| Команда | Назначение |
|---|---|
| `shape.balloon.create` | **Полиморфная**: с payload `balloon` создаёт `LiveBalloon`, без payload — старое облако/шипы. |
| `shape.balloon.update` | Изменение параметров бабла. |
| `shape.balloon.tail.add` | Добавить хвост к баблу группы. |
| `shape.balloon.tail.remove` | Убрать хвост. |
| `shape.balloon.convertToLinework` | Перерисовать бабл штрихами SAI Linework на том же слое. |
| `layer.newLinework` | Пустой слой SAI Linework, готовый под инструмент Vector Curve. |

`shape.balloon.stroke`, `shape.balloon.presets`, `shape.balloon.tail`, `shape.vectorCurve.*`
существовали до слияния и сохранены.

### 3.4 UI

| Файл | Что сделано |
|---|---|
| `crates/ui-egui/src/balloon_gizmo.rs` (новый, 589 строк) | Живой гизмо бабла на холсте. |
| `crates/ui-egui/src/comic_sai_panel.rs` (новый, 223 строки) | Панель Linework/SAI-кривой. |
| `crates/ui-egui/src/canvas.rs` | Вывод гизмо на холст. |
| `crates/ui-egui/src/dock.rs` | 4-я вкладка **Linework** в группе `Layers` (`&["Layers", "Channels", "Paths", "Linework"]`). |
| `crates/ui-egui/src/panels.rs` | Кнопка `spline` «Create a Linework vector layer» в футере панели слоёв. |

---

## 4. Блок Б — память SAI

* **Коалесценция undo при перетаскивании узла.** Ключ
  `"coalesce": format!("sai-curve-drag-{}-{}", ld.layer_id, ld.stroke_idx)` проставлен во всех
  трёх payload'ах команды `vector_curve_move` — одно перетаскивание = один шаг истории.
* **Автоскролл линейки** портирован из бандла Argon в `crates/ui-egui/src/canvas_autoscroll.rs`
  (`TRIGGER_MARGIN = 16.0`, `edge_step_with_margin`, `scroll_with_options`); `Tool::Ruler`
  учитывается в `canvas.rs`.

---

## 5. Тесты

| Файл | Что проверяет |
|---|---|
| `crates/vector/tests/vector_curve_tests.rs` (новый, 4 теста) | Адаптация эталонных тестов Argon под наш API (кривые SAI, узлы, геометрия). Добавлен `[dev-dependencies] serde_json` в `crates/vector/Cargo.toml` (+ `Cargo.lock`). |
| `crates/engine/src/balloon_cmds/tests.rs` (+3) | Live-баллон; хвост + конвертация в Linework; graceful-failure для всех новых команд (документ не меняется при ошибке). |
| `crates/ui-egui/src/comic_vector_curve_ui_tests.rs` (+3) | Коалесценция: 200 движений = 1 шаг undo; разные жесты = разные шаги; undo возвращает узел. |
| `crates/ui-egui/src/dock_tests.rs` (исправлен) | 3 теста, упавшие из-за 4-й вкладки Linework (раздел 8.2). |

Результаты прогонов (актуальные):

```
cargo test -p photocraft-vector --lib              88 passed
cargo test -p photocraft-vector --test vector_curve_tests   4 passed
cargo test -p photocraft-engine --lib snow        59 passed
cargo test -p photocraft-engine --lib balloon_cmds 10 passed
cargo test -p photocraft-engine --lib vector_cmds  44 passed
cargo test -p photocraft-ui-egui --lib snow        48 passed
cargo test -p photocraft-ui-egui --lib comic_vector_curve_ui_tests 12 passed
cargo test -p photocraft-ui-egui --lib canvas_autoscroll 12 passed
cargo test -p photocraft-ui-egui --lib              3991 passed, 0 failed, 20 ignored
cargo test -p photocraft-ui-egui --lib dock::tests  36 passed
cargo test -p photocraft-ui-egui --lib i18n          77 passed
cargo test -p photocraft-ui-egui --test it workspace_presets  9 passed
cargo test -p xtask                                 65 passed
```

---

## 6. Приёмка

```
cargo fmt --all -- --check                                        OK
cargo clippy --workspace --all-targets --locked --features heif -- -D warnings   OK
cargo run -p xtask -- layers        35 крейтов, 0 нарушений         OK
cargo run -p xtask -- parity        640/640 пунктов меню живы       OK (docs/parity-checklist.md перегенерирован)
cargo run -p xtask -- scorecard     docs/scorecard.md перегенерирован OK
cargo run -p xtask -- wasm          OK
cargo run -p xtask -- i18n-coverage полные каталоги 99.18%         OK
cargo test -p photocraft-engine --test panic_hunt -- --ignored     OK (см. 9.4)
```

Замечания по clippy: для портированного файла `snow_typesetter_ui.rs` применён
`cargo clippy --fix` (38 машинных исправлений, в основном `collapsible_if`); остальное — точечные
правки без blanket-allow. Единственный `#[allow]` в коде — `clippy::too_many_arguments` в
`font_preview.rs` (сигнатура публичной функции-превью).

---

## 7. Переводы (отдельный коммит)

Шесть строк, добавленных слиянием, были внесены в `FORK_EXEMPT_KEYS` (`xtask/src/i18n_coverage.rs`),
то есть исключены из проверки покрытия. Теперь они переведены во всех 21 каталоге
(`crates/ui-egui/src/i18n/*.tsv`: ja, zh-hans, zh-hant, es, ru, uk, sr, cs, fr, id, ko, pl, de,
pt-br, el, nl, it, hu, tr, vi и частичный hi) и убраны из списка исключений:

| English | Русский |
|---|---|
| `Capture a selection first` | Сначала выделите область |
| `Bubble captured` | Бабл захвачен |
| `Filled bubbles` | Заполнено баблов |
| `Layer centered in the selection` | Слой выровнен по центру выделения |
| `Align failed` | Не удалось выровнять |
| `Create a Linework vector layer` | Создать векторный слой штрихов |

Формат записи — `context<TAB>source<TAB>translation` (пустой context для обычных строк),
файлы — CRLF, UTF-8 без BOM. Формулировки взяты из терминологии самих каталогов
(«бабл» в ru, «吹き出し» в ja, «말풍선» в ko, «bocadillo» в es, «fumetto» в it и т. д.).

Побочный эффект: покрытие полных каталогов выросло с 98.97% до 99.18%.

---

## 8. Дефекты, найденные после слияния и исправленные

### 8.1 Падающий гейт локализации

На `de1a2b327` тест `i18n::tests::every_tl_literal_is_translated` **падал**:
строка `Create a Linework vector layer` (из `panels.rs`) не была переведена ни в одном из 20
языков с `complete_menus = true`. Раньше она была скрыта исключением в xtask-гейте, но
ui-egui-гейт исключения не знает. Проверено: `git stash` переводов → тест падает;
после переводов → проходит.

### 8.2 Три упавших теста дока

Полный прогон `cargo test -p photocraft-ui-egui --lib` на `de1a2b327` давал 3 падения
(`dock::tests::double_clicking_a_tab_collapses_and_dragging_a_strip_reorders`,
`dragging_a_tab_reorders_the_group_and_the_order_persists`,
`move_tab_reorders_by_name_and_bad_saved_orders_fall_back`): в группе `Layers` теперь 4 вкладки,
а тесты ожидали 3.

Исправлено без изменения продуктового поведения:

* добавлен хелпер `strip_grip(h, group)` — точка на свободной части полосы вкладок
  (правее последней вкладки, левее кнопки меню). Раньше тест цеплялся за фиксированную точку
  `group.right() - 60.0`, которая с 4-й вкладкой попала на вкладку Linework, и перетаскивание
  двигало вкладку вместо группы;
* ожидания перестроены под 4 вкладки (`[2, 0, 1, 3]`, `tabOrder` =
  `{"layers": ["Paths", "Layers", "Channels", "Linework"]}`);
* проверка «мусор в сохранённом порядке» переписана на 4 вкладки; кейс «вернуть порядок по
  умолчанию» теперь использует `move_tab(Layers, 2, Some(3))` — «Paths» перед «Linework»,
  поэтому дефолтный порядок снова совпадает с `0..n` и `tab_order` остаётся пустым.

### 8.3 Прочее

* `cargo clippy --fix` по портированному UI (38 правок) — после него обязателен `cargo fmt --all`.
* В `crates/vector/src/balloon.rs` в тестовых литералах `SaiPoint` добавлено поле `density: 1.0`
  (4 места) — новое поле структуры.

---

## 9. Известные оговорки и открытые пункты

### 9.1 Свободное место на полосе вкладок (~6 px)

В колонке дока шириной 285 px четыре вкладки `Layers/Channels/Paths/Linework` занимают её
почти целиком: между правым краем вкладки Linework и кнопкой меню остаётся ~6 px. За эту полосу
по-прежнему можно перетащить группу (тест делает именно это), но «за хват» пользователю почти
не за что; основной способ переставить группу — вытащить вкладку за пределы полосы (это тоже
двигает группу) или расширить колонку. Если это мешает, разумные варианты: вынести Linework в
отдельную группу/на рельс иконок либо резервировать под хват минимум ~24 px (тогда в узкой
колонке вкладки уедут в шеврон «»).

### 9.2 Два поля выравнивания у пресета

В `TextStylePreset` есть и `align`, и `alignment` (у порта и у форка). Сведены через
`effective_align()`: приоритет у `align`, `alignment` используется, когда `align` — дефолтный
`center`. При добавлении новых полей выравнивания править надо `effective_align()`.

### 9.3 Межстрочный интервал в окне сценария

`SCRIPT_LINE_SPACING` в UI сценария стал `1.0` (значение порта) вместо `4.0` (значение форка) —
UI порта был эталоном, его версия и стала базой. Если форковская «воздушность» нужна, это
одноточечная правка в `crates/ui-egui/src/snow_typesetter_ui.rs`.

### 9.4 `panic_hunt` и `edit.generativeFill`

Первый прогон `cargo test -p photocraft-engine --test panic_hunt -- --ignored` упал на
`edit.generativeFill`: разовая инициализация ONNX. К правкам слияния отношения не имеет,
повторный прогон проходит. При CI-шных флаках учитывать это.

### 9.5 Сканер исходников в xtask

`xtask/src/i18n_coverage.rs` режет файл по **первому** `#[cfg(test)] mod …`, поэтому литералы
`tl!` в `crates/ui-egui/src/panels.rs` (после его тест-модуля на ~line 540) в набор ключей
xtask не попадают — их ловит ui-egui-гейт (`every_tl_literal_is_translated`, который режет
тест-модули по одному). Итог: «Create a Linework vector layer» не виден xtask-гейту, но виден
ui-egui-гейту. Проверено: с переводами оба гейта зелёные.

### 9.6 Неотслеживаемые файлы в рабочем дереве

`photocraft_0.14.0_2026-10-29_0459.exe`, `photocraft_0.14.0_2026-11-09_0314.exe`,
`snowe22_reference/` — не в индексе, в коммиты не попадают. Решение об их судьбе — за владельцем.

---

## 10. Как проверить вручную

1. **TypeR Studio.** Открыть панель TypeR (Window → TypeR Manga): загрузить сценарий `.txt`,
   выбрать стиль, применить к выделению (баблу). Проверить: разбиение на строки/абзацы по
   правилам порта, перенос по контуру бабла (Bubble Contour в заголовке панели), fit-to-contour,
   paste-on-click, выравнивание по центру выделения, multi-bubble fill (Capture bubble → Fill bubbles).
2. **Манга-бабл.** На холсте: создать бабл (5 тел), потянуть за узлы, добавить хвост
   (6 пресетов), прижать хвост к говорящему, конвертировать в Linework
   (`shape.balloon.convertToLinework`) — должен остаться редактируемый штриховой слой.
3. **Linework.** Кнопка `spline` в футере панели слоёв создаёт слой Linework; вкладка Linework
   в группе Layers открывает панель кривой. Перетаскивание узла = один шаг Undo (проверить
   Ctrl+Z, что узел возвращается целиком).
4. **Память.** Длинное перетаскивание узла, затем Undo — откат должен быть одним шагом;
   перетаскивание линейки к краю холста должно автоскроллить.
5. **Локализация.** Preferences → Interface language → переключить язык; новые надписи
   («Заполнено баблов», «Создать векторный слой штрихов», …) должны быть переведены, а не
   английскими.

---

## 11. Карта файлов

```
crates/doc/src/vector.rs                    модель живых баблов и групп
crates/vector/src/balloon.rs                геометрия баблов, конвертация в Linework
crates/vector/src/comic_sai_curve.rs        кривые SAI, плотность штриха, лимит сэмплов
crates/vector/tests/vector_curve_tests.rs   тесты кривых (адаптация Argon)
crates/engine/src/balloon_cmds.rs           команды shape.balloon.*
crates/engine/src/vector_cmds.rs            layer.newLinework, live-поля штриха
crates/engine/src/snow_typesetter.rs        ядро TypeR (3-way)
crates/engine/src/snow_knuth_plass.rs       пагинация и переносы (версия порта)
crates/psd/src/tpl.rs                       чтение Photoshop Tool Presets
crates/ui-egui/src/snow_typesetter_ui.rs    UI TypeR (база порта + наш редактор стилей)
crates/ui-egui/src/lib.rs                   обвязка: отложенные действия TypeR, статусы
crates/ui-egui/src/balloon_gizmo.rs         гизмо бабла
crates/ui-egui/src/comic_sai_panel.rs       панель Linework
crates/ui-egui/src/canvas_autoscroll.rs     автоскролл (в т.ч. линейки)
crates/ui-egui/src/dock.rs                  вкладка Linework в группе Layers
crates/ui-egui/src/panels.rs                кнопка «Create a Linework vector layer»
crates/ui-egui/src/i18n/*.tsv               21 каталог локализации
xtask/src/i18n_coverage.rs                  гейт покрытия локализации (+ FORK_EXEMPT_KEYS)
```

---

## 12. Правила репозитория, которые нужно соблюдать дальше

* команды движка — только через `macro_rules! spec!` в `crates/engine/src/*_cmds.rs` и
  регистрация в `commands.rs`;
* в не-тестовом коде запрещены `unwrap` / `expect` / `panic` (`#![deny(clippy::unwrap_used)]`);
* новая команда → тест на graceful-failure + прогон `panic_hunt`;
* добавление команд → `cargo xtask parity` с перегенерацией `docs/parity-checklist.md`;
* затрагивание L0–L6 → `cargo xtask wasm`;
* clippy строгий: `-D warnings`.
