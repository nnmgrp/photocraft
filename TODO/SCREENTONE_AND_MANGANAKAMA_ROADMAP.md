# Roadmap: Апгрейд Screentone и заимствования из `MangaNakama`

Сводный план доработки скринтонов (по аудиту кода на `HEAD` + ревизии `AISTUDIO Revision.md`) и список модулей/алгоритмов из [bluescreenoff/MangaNakama](https://github.com/bluescreenoff/MangaNakama) (`MIT OR Apache-2.0`, стек `Rust + wgpu + WGSL`), которые стоит адаптировать в **PhotoCraft: Manga Suite**.

---

## Блок A — Зачистка багов и расхождений в нашем Screentone (ВЫПОЛНЕНО)

1. **[x] Убрать старый дубль и баг знака `sdf_polygon` в `crates/manga-core/src/comic_screentone.rs`:**
   - Модуль сведён к `pub use photocraft_algo::comic_screentone::*;` (единый источник правды без старого дубля и бага знака `sdf_polygon`).
2. **[x] Активировать 3 уже написанных градиента (`Angle`, `Reflected`, `Diamond`):**
   - Расширено поле до 3 бит (`& 7u`) в `crates/gpu/src/plan/lower.rs` и `crates/gpu/src/compose.wgsl`, добавлены ветки в `ScreentoneGradientType`, CPU `compute_grad_factor`, валидацию `engine` и UI-список `GRADIENTS`; исправлен `screentone_frame` в `planner.rs` для `fill_frame`; добавлен GPU/CPU-паритет тест.
3. **[x] Устранить расхождение GPU vs CPU в `preserve_lines`:**
   - Перенесены 4-осевой детектор линий, `line_thickness`-дилатация (`lut_at(27)`) и 2px де-лайнинг `best_delined_luma` в `crates/gpu/src/compose.wgsl`, добавлен паритет-тест `screentone_preserve_lines_parity`.
4. **[x] Синхронизировать `panel_for_sfx` с `eval_screentone_sheet` в `crates/gpu/src/sfx.wgsl`:**
   - `panel_for_sfx` в `crates/manga-ui/src/comic_screentone_panel.rs` ограничен параметрами, реально поддерживаемыми `sfx.wgsl` (`Dots/Lines/Crosshatch/Noise`, `dotShape`, `dotSize`, `spacingX/Y`, `angle`, `density`, `lineWidth`), без правок в активных SFX-файлах.
5. **[x] Открытые баги `0.23.1` и базовый UX:**
   - Починена пропажа нерастрированных `LayerContent::Screentone` при `Merge Layers`, `Merge Group`, сохранении/превью и CPU-композитинге (`crates/compose/src/screentone.rs`, `content.rs`, `composite.rs`, `proxy.rs`, `viewport.rs`, `engine`);
   - Починен экспорт `LayerContent::Screentone` в PSD/PNG (`crates/io/src/psd_export.rs`) с учётом `use_source_luminance` подложки и `fill_frame`/маски;
   - Выведен выбор цвета **`Foreground`** (+ `Black` / `White` и автовыбор `Normal` для светлых/цветных тонов) в `comic_screentone_panel.rs`;
   - Реализован интерактивный **`Tool::Screentone`**: клик = заливка выделения/холста, зажим + тяга (с `Shift`-привязкой по 45° и предпросмотром вектора) = живой градиент скринтона выбранного стиля + опция **`Clip`**.

---

## Блок B — Живые линии и манга-штриховка (CPU + WGSL паритет)

Новые поля в `ScreentoneSpec` (все с `#[serde(default)]` и нейтральными дефолтами для 100% обратной совместимости):
- `line_wobble: f32` — живое дрожание руки / волна;
- `line_wave_freq: f32` — частота волны/дрожания;
- `line_dash: f32` — разбиение линий на манга-штрихи с заострением концов (`dash + taper`);
- `line_progressive: bool` — **прогрессивное 3-уровневое какеами** (1-й слой `ink > 0.08`, 2-й `ink > 0.38`, 3-й по биссектрисе `ink > 0.68`);
- Подключить существующие `jitter_pos` и `jitter_size` к `Lines` и `Crosshatch` (сейчас там игнорируются);
- **`PatternType::FocusLines` (сютюсэн)** с `focus_inner_radius` и привязкой центра к гизмо `origin`;
- Вынести **`Scrape / Kezuri`** на общий уровень после `eval()`, чтобы скребок работал на всех паттернах (включая линии и шум), добавить `scrape_angle` и плавный 1D-шум ножа.

---

## Блок C — Паритет с CSP и Krita

- **`DotShape::Euclidean`** — честный полиграфический евклидов растр (`0..50%` чёрные кружки → `50%` шахматка → `50..100%` белые кружки на чёрном, O(1) SDF с субпиксельным AA);
- **`PatternType::SandNoise`** — быстрый 2-октавный пиксельный/волокнистый шум CSP с вытягиванием через `aspect_x / aspect_y` (без 3×3 цикла ячеек);
- **`PatternType::CustomTexture`** — наложение любого тайла из библиотеки паттернов `Session::patterns` через `angle`, `spacing`, гизмо `origin`, `Shading`-порог и `Kezuri`;
- **UI-удобства в `comic_screentone_panel.rs`:**
  - замок **`Link X=Y`** рядом со `Spacing X / Y`;
  - связанное поле **`LPI`** (`spacing_px = doc_dpi / lpi`);
  - **`Reflect Layer Opacity`** (прозрачность слоя уменьшает размер/плотность точек, оставляя их 100% чёрными);
  - **`Show Tone Area`** (полупрозрачная цветная подсветка области скринтона);
  - 6 новых пресетов: `euclid-60`, `sand-fiber`, `kakeami-3`, `hand-hatch`, `wavy-lines`, `focus-burst`.

---

## Блок D — Что берём и адаптируем из `bluescreenoff/MangaNakama` (`MIT OR Apache-2.0`)

При заимствовании кода обязательно сохранять attribution (`MIT OR Apache-2.0`). Стек идентичен нашему (`Rust + wgpu + WGSL`).

| Модуль в `MangaNakama` | Что именно берём | Куда ложится в PhotoCraft |
|---|---|---|
| `crates/core/src/convert_lt.rs` | **`Layer › Convert to Lines and Tones (LT)`**: 4-направленный Sobel (`[up, right, down, left]` с `WEAK_DIRECTION = 0.25`), пред-постеризация, дилатация `width`, морфологическое замыкание `join` (CLOSE), отдельный слой Бета (`black_fill`) + **1–4 отдельных живых слоя скринтона с масками** в одной папке за один шаг Undo | Новая команда в `crates/engine/src/cmd/manga_cmds/` + диалог в `manga-ui`, создающий наши живые `LayerContent::Screentone` |
| `crates/core/src/tone.rs` | Математически точное сохранение площади (**area conservation**) и **инверсия фигур после 50%**; единый `TonePattern::on(u, v, ink, cell)` для скринтон-заливки бабблов (`BalloonTone`) | `crates/algo/src/comic_screentone.rs`, `crates/gpu/src/compose.wgsl`, `crates/vector/src/balloon.rs` |
| `crates/core/src/fill.rs` | Автоопределение толщины лайнарта (auto gap-closing + подкрашивание под линию) и **One-gesture Tone** (клик по замкнутой области → сразу живой слой скринтона с маской) | `crates/algo/src/fill.rs` + `Tool::Screentone` / `Tool::Fill` |
| `crates/core/src/genlines.rs` (+ `genlines/`) | Живые процедурные **speed lines**, **focus lines** и **beta-flash (ベタフラ)** как редактируемые объекты | Отдельный генератор эффектных линий в `manga-core` / `manga-ui` |
| `crates/core/src/dust.rs` | Инструмент **Remove Dust** (автоочистка мелкого мусора/точек на 600-DPI сканах и лайнарте) | Фильтр/инструмент в `manga-core` / `algo` |
| `crates/core/src/ruler.rs` | Перспективные линейки (1 / 2 / 3 точки схода, редактируемые после создания) | Оверлей + привязка штриха в `manga-ui` / `ui-egui` |
| `crates/core/src/frame.rs` + `frame_order.rs` | Фреймы манги с автоматической нумерацией порядка чтения, путём чтения на холсте и **выходом арта за рамку (panel burst)** | Будущий модуль панелей манги |
| `crates/core/src/preflight.rs` | Предпечатная проверка манга-страниц (600 DPI, обрез, поля безопасности, ч/б-контроль) | Инструмент проверки перед экспортом главы |
| `crates/core/src/stabilize.rs`, `taper.rs`, `correction.rs` | Стабилизатор штриха, taper кончиков и коррекция кривых | Вместе с парсерами `.kpp`/`.sut`/`.abr`/`.gbr` и `dab.wgsl` — см. `TODO/hokusai-sai-brush-engine.md` (без C/FFI `libmypaint`) |

### Бонус (алгоритмические фильтры без нейросетей)
- **[owoevan-dev/refinish-skill](https://github.com/owoevan-dev/refinish-skill)** (`MIT`, Python NumPy/OpenCV): референс чистых алгоритмов постобработки для будущих фильтров — cel-shading, ксилография/укиё-э и снятие «AI-мыла» (de-AI) с фонов.
