# Аудит переноса фич из `0.14` (`opencode/v0.14-fixes` и `snowe22/latest`) в `manga/0.23-sfx`

Сверено по коду `HEAD` (`manga/0.23-sfx`) против веток `opencode/v0.14-fixes` и `snowe22/latest`.

---

## ❌ Не переехало / потеряно при переходе на `0.23`

1. **Скрывание курсора при тяге числовых полей вбок (`scrub_wrap`):**
   - **Статус:** работает **только** в `crates/manga-ui/src/comic_sai_panel.rs:422` (через `crates/manga-ui/src/scrub_wrap.rs:186` с `CursorIcon::None`).
   - **Где потеряно:**
     - общие числовые поля PhotoCraft (`crates/ui-egui/src/widgets/scrub.rs:82` ставит `CursorIcon::ResizeHorizontal`, а `crates/ui-egui/src/input/scrub_wrap.rs:12` включает wrap **только в fullscreen** и вообще не скрывает курсор);
     - остальные манга-панели (`sfx_panel.rs`, `comic_screentone_panel.rs`, `balloon_panel.rs`) используют обычные `egui::DragValue` без `scrub_wrap`.
2. **Хоткей `Ctrl+Shift+S` → быстрый экспорт PNG (`f63582810` + `b691e1982`):**
   - **Статус:** откатилось к апстриму.
   - **Где сейчас:** в `crates/ui-egui/src/menu_catalog.rs:18` `Cmd+Shift+S` снова привязан к `Save As…` (`file.saveAs`), а `Quick Export as PNG` (`menu_catalog.rs:22`) остался без хоткея (`None`).
3. **Блокировка случайного изменения чисел колесом мыши в скроллируемых панелях (`pc_in_scroll_panel`, `700d6cb21` от SnowE22):**
   - **Статус:** полностью отсутствует в `0.23`.
   - **Где сейчас:** `crates/ui-egui/src/widgets/field_wheel.rs` использует штатную логику апстрима — если начать скролл прямо над числовым полем, оно меняет значение вместо прокрутки панели.
4. **Двойной клик и мелкие UX-жесты из `0.14` (нужна ручная проверка на живом UI):**
   - Проверить поведение двойного клика по строке эффекта слоя, заголовкам/табам доков (`15dba8da3`), репликам TypeR (inline-редактирование из `ca9df02c8`) и открытию/сбросу параметров.

---

## ✅ Переехало и подключено в `manga/0.23-sfx`

- **Интерактивный жест по эффектам слоя + Quick Adjust HUD (`f96494bfd`, `snow_fx_scrub`):**
  - `crates/manga-ui/src/snow_fx_scrub.rs` → `crates/ui-egui/src/manga_seam.rs:1831-1893` → `crates/ui-egui/src/panels/layer_drag.rs:350,382,404,410`.
- **TypeR: Кнут–Пласс, висячая пунктуация, `faux_bold`, сохранение `anti_alias` (`3f4442951`, `e9cbd8cee`, `700d6cb21`):**
  - `crates/manga-core/src/snow_knuth_plass.rs`, `crates/manga-core/src/snow_typesetter.rs`.
- **TypeR: единый многострочный редактор сценария, Font Settings modal (редизайн Photoshop) (`0ad685ebe`, `feb73bfe0`):**
  - `crates/manga-ui/src/snow_typesetter_ui.rs:918,977,1794,2282,2312`.
- **PSD Point Text с вертикальной центровкой базовой линии (`ad1301288`):**
  - `crates/ui-egui/src/manga_seam.rs:2837,3497`.
- **Панель Pages (естественная сортировка `natural_sort_paths`):**
  - `crates/manga-ui/src/comic_pages_panel.rs:384`.
- **Canvas Capsule (плавающая контекстная панель):**
  - `crates/manga-ui/src/comic_canvas_capsule.rs`.
- **Вторая маска слоя (`SecondMask`, `ea20f0e5f`):**
  - `crates/engine/src/cmd/manga_cmds/second_mask.rs`, `crates/engine/src/cmd/channel_cmds/refs.rs:20`.
- **Запоминание папки экспорта (`quick_export_png`, `remember_export_folder`, `ca9df02c8`):**
  - `crates/ui-egui/src/file_dialog.rs:203,217`, `crates/ui-egui/src/dialogs/export_dialog.rs:1148`.
- **Блокировки слоёв (`/` — lock transparency и др.):**
  - `crates/ui-egui/src/lock_layers_shortcut_tests.rs`.
