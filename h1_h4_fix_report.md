# Отчёт об исправлении дефектов H1–H4 (мультидокинг, SAI Linework, горячие клавиши VectorCurve и бабблы)

Документ фиксирует выполненные исправления по спецификации **v3.0.0** на базе коммита `94c567926` в ветке `community/gpu-screentone-preview`, добавленные unit-тесты, локализацию и результаты полной верификации.

---

## 0. Краткая сводка

| Подсистема | Что исправлено | Ключевые файлы | Тест |
|---|---|---|---|
| **H1 (Мультидокинг)** | Инвариант сингулярности панелей между правым, левым и нижним доками; миграция из `legacy(app)` без дублей | `crates/ui-egui/src/panel_docking.rs`, `comic_sai_panel.rs`, `comic_balloon_panel.rs`, `panels.rs` | `panel_docking::tests::panel_singularity_across_left_and_right_docks` |
| **H2 (SAI Linework)** | Двусторонняя синхронизация сетки/профиля/слайдеров с `ToolOptions`; передача `curveType`, `baseDensity`, `minDensityRatio`, `pressureTarget` в превью и коммит | `crates/ui-egui/src/state.rs`, `comic_sai_panel.rs`, `vector_ui.rs`, `crates/engine/src/vector_cmds.rs` | `comic_sai_panel::tests::sai_grid_and_tip_profile_sync_to_tool_options` |
| **H3 (Хоткеи VectorCurve)** | `Ctrl+Z` / `Cmd+Z` удаляет последнюю точку при непустой кривой; `Backspace` / `Delete` очищает кривую в режиме `Draw` и удаляет узел в `Edit`/`Weight`; согласование `target_layer_id` | `crates/ui-egui/src/shortcuts.rs`, `vector_ui.rs`, `lib.rs` | `shortcuts::tests::vector_curve_undo_pops_last_point_while_delete_clears_curve` |
| **H4 (Manga Balloons)** | Гидратация `Layer -> UI` и write-through `UI -> Layer`; стабилизация геометрии хвоста для `Cloud`/`Spiky` и при `ellipse_dist < 1.0` | `crates/ui-egui/src/comic_balloon_panel.rs`, `balloon_gizmo.rs`, `crates/vector/src/balloon.rs` | `balloon::tests::balloon_tail_geometry_stays_valid_when_tip_inside_body`, `comic_balloon_panel::tests::balloon_panel_write_through_updates_live_layer` |
| **i18n (меню)** | Добавлены переводы `Manga Balloons`, `Balloons`, `SAI Linework`, `Linework` в 18 `.tsv`-каталогов с `complete_menus: true` | `crates/ui-egui/src/i18n/*.tsv` | `i18n::tests::complete_languages_translate_every_menu_string` |

Все диагностические зонды `[Telemetry:Dock:*]`, `[Telemetry:SAI:*]`, `[Telemetry:Keys:*]` и `[Telemetry:Balloon:*]` из коммита `94c567926` сохранены.

---

## 1. Детали исправлений

### H1. Инвариант сингулярности панелей в мультидокинге
- В [`crates/ui-egui/src/panel_docking.rs`](crates/ui-egui/src/panel_docking.rs):
  - Добавлены и задействованы helpers проверки всех доков (`panel_in_side_dock`, `panel_showing_anywhere`, `panel_active_anywhere`), чтобы `command()` и `checked()` учитывали `docking`, `docking_left` и `docking_bottom`.
  - В `change()` операции `"activate"` и `"close"` для панели, находящейся в `docking_left` или `docking_bottom`, выполняются на месте в соответствующем доке без клонирования копии в правый док.
  - При переходе от `app.ui.docking == None` через `legacy(app)` панели, уже размещённые в `docking_left`/`docking_bottom`, исключаются из создаваемого `layout` и `hidden`.
  - При размещении/активации панели в правом доке вызывается `remove_panel_from_side_docks`.
- В [`crates/ui-egui/src/comic_sai_panel.rs`](crates/ui-egui/src/comic_sai_panel.rs), [`crates/ui-egui/src/comic_balloon_panel.rs`](crates/ui-egui/src/comic_balloon_panel.rs) и [`crates/ui-egui/src/panels.rs`](crates/ui-egui/src/panels.rs) проверки открытия/активности панелей переведены на все доки.

### H2. Двусторонняя синхронизация SAI Linework и рендер-пайплайн
- В [`crates/ui-egui/src/state.rs`](crates/ui-egui/src/state.rs) дефолтные значения `ToolOptions` для `VectorCurve` выровнены с `LineworkPanelState` (`8.0` px ширина, `0.0` min ratio).
- В [`crates/ui-egui/src/comic_sai_panel.rs`](crates/ui-egui/src/comic_sai_panel.rs) добавлен метод `sync_to_tool_options`, синхронизирующий размер сетки, профиль сбега пера, плотность, `curve_type`, `tension`, `auto_taper`, `closed_loop` и кнопку сброса.
- В [`crates/ui-egui/src/vector_ui.rs`](crates/ui-egui/src/vector_ui.rs) расширен диапазон ширины (`0.5..=500.0`), добавлена обратная синхронизация из верхней панели опций в `app.sai_panel` при изменении контролов, поддержка `PressureTargetMode::Density`, `alpha_at` и `sai.curve_type.tension(...)` в `draw_vector_curve_overlay`, а также передача `curveType`, `baseDensity`, `minDensityRatio`, `pressureTarget` в `vector_curve_commit`.
- В [`crates/engine/src/vector_cmds.rs`](crates/engine/src/vector_cmds.rs) обновлены `parse_sai_point`, `build_vector_curve_layer`, `effective_stroke_fg` и `vector_curve_create` для применения плотности и типа кривой при фиксации в слой.

### H3. Обработка горячих клавиш `Tool::VectorCurve` и жизненный цикл буфера точек
- В [`crates/ui-egui/src/shortcuts.rs`](crates/ui-egui/src/shortcuts.rs) вынесена функция `vector_curve_keys`:
  - `edit.undo` (`Ctrl+Z` / `Cmd+Z`) при непустом `points` удаляет последнюю точку (`pop_last_point()`) и поглощает событие;
  - `Backspace` / `Delete` в режиме `Draw` очищают текущую незафиксированную кривую (`clear()`), а в режимах `Edit`/`Weight` удаляют выбранный узел.
- В [`crates/ui-egui/src/vector_ui.rs`](crates/ui-egui/src/vector_ui.rs) и [`crates/ui-egui/src/lib.rs`](crates/ui-egui/src/lib.rs) добавлены `target_layer_id` и `reconcile_vector_curve_layer`, очищающие буфер незафиксированной кривой при удалении/откате целевого векторного слоя; в `draw_vector_curve_overlay` добавлена соответствующая проверка.

### H4. Синхронизация Manga Balloons и геометрия хвоста
- В [`crates/ui-egui/src/comic_balloon_panel.rs`](crates/ui-egui/src/comic_balloon_panel.rs) реализованы:
  - гидратация `Layer -> UI` по активному `(doc_idx, layer_id, balloon.id)`;
  - запись изменений панели в активный слой (`apply_panel_to_active_layer` через `shape.balloon.update` с `"coalesce": "balloon-panel-edit"` и `shape.edit` для fill/stroke);
  - исправлена опечатка расчёта горизонтального центра (`doc_size.width`).
- В [`crates/ui-egui/src/balloon_gizmo.rs`](crates/ui-egui/src/balloon_gizmo.rs) сохраняется `active_balloon_id` при `on_pointer_down` и телеметрия `[Telemetry:Balloon:GizmoDrag]` перемещена после обновления `drag.hit`.
- В [`crates/vector/src/balloon.rs`](crates/vector/src/balloon.rs) уточнена `point_and_normal_on_body` для `Cloud` и `Spiky`, а в `build_single_tail_path` добавлены ограничение минимальной внешней проекции кончика (`min_outward`), сонаправление `chord_perp` с `tangent` и ослабление изгиба при малых углах.

### Переводы i18n
- В `crates/ui-egui/src/i18n/` (`cs.tsv`, `de.tsv`, `el.tsv`, `es.tsv`, `fr.tsv`, `hu.tsv`, `id.tsv`, `it.tsv`, `ko.tsv`, `nl.tsv`, `pl.tsv`, `pt-br.tsv`, `sr.tsv`, `tr.tsv`, `uk.tsv`, `vi.tsv`, `zh-hans.tsv`, `zh-hant.tsv`) добавлены строки локализации для `Manga Balloons`, `Balloons`, `SAI Linework` и `Linework`.

---

## 2. Результаты верификации

Все проверки выполнены после финального редактирования кода:

| Команда | Результат |
|---|---|
| `cargo check --workspace` | **OK** (`0` ошибок) |
| `cargo clippy --workspace -- -D warnings` | **OK** (`0` предупреждений) |
| `cargo test -p photocraft-vector` | **OK**: unit `89 passed, 0 failed, 2 ignored`; integration `vector_curve_tests`: `4 passed, 0 failed` |
| `cargo test -p photocraft-ui-egui --lib -j 2` | **OK**: `3999 passed, 0 failed, 20 ignored` |
| `cargo run -p xtask -- parity` | **OK**: `642 / 642 live (100.0%)` |

> Примечание: для `photocraft-ui-egui` использованы флаги `--lib -j 2`, чтобы при параллельной линковке всех тестовых/example-целей на Windows с `debuginfo` не упираться в лимит файла подкачки (`os error 1455`).
