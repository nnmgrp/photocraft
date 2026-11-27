# Upstream Seam Notes (Блок A + Фаза 1A + Фаза 1B)

Ручная карта правок **внутри основной кодовой базы PhotoCraft (upstream)** с объяснением причины и риска конфликта при будущем rebase на `upstream/main`.

Автоматически сгенерированная построчная карта всех швов и hunk-ов находится в [`TODO/UPSTREAM_SEAMS.md`](UPSTREAM_SEAMS.md) (обновляется командой `powershell -ExecutionPolicy Bypass -File scripts/seam_inspector.ps1`). Этот файл вынесен отдельно, чтобы автогенератор не затирал пояснения.

> **Граница «чемодана» манги (не перечислены ниже как швы upstream):**
> `crates/manga-core/*`, `crates/manga-ui/*`, `crates/manga-bridge/*`, `crates/tone/*`, `crates/engine/src/cmd/manga_cmds/*`, `crates/ui-egui/src/manga_seam.rs`, `TODO/*`.

---

## Блок A — Скринтоны: градиенты `Angle`/`Reflected`/`Diamond`, живой `compose`/PSD-рендер и drag-инструмент

| Файл upstream | Суть правки | Причина | Риск конфликта при rebase |
|---|---|---|---|
| `Cargo.lock` | Обновлён граф зависимостей после добавления `photocraft-algo` в `crates/compose/Cargo.toml`. | Интеграция манги (`--locked` сборка CI) | **Высокий** (перегенерируется `cargo update` / `cargo check` после rebase) |
| `crates/compose/Cargo.toml` | Добавлена зависимость `photocraft-algo = { workspace = true }`. | Интеграция манги (on-demand CPU-оценка незапечённых `ScreentoneLayer`) | **Низкий** (1 строка в `[dependencies]`) |
| `crates/compose/src/lib.rs` | Объявлен модуль `pub mod screentone;`. | Интеграция манги | **Низкий** (1 строка) |
| `crates/compose/src/screentone.rs` *(новый)* | On-demand CPU-вычисление незапечённых GPU-скринтонов (`render_unmasked`, `render_masked`, `render_unbaked_for_export`, `scale_spec_for_downsample`). | Интеграция манги + баг пустых слоёв при сохранении/экспорте/слиянии | **Отсутствует** (новый файл) |
| `crates/compose/src/content.rs` | Ветка `LayerContent::Screentone(st) if st.surface.tile_count() == 0` делегирует рендер в `crate::screentone::render_unmasked`. | Интеграция манги | **Низкий** (1 строка в `match`) |
| `crates/compose/src/composite.rs` | Вынесен `render_layer_or_screentone` для незапечённых `Shading`-скринтонов; `pixel_surface` возвращает `None` для незапечённых скринтонов. | Интеграция манги | **Средний** (`composite_layer_plain` и `composite_layer_effects` часто правятся в upstream) |
| `crates/compose/src/proxy.rs` | Масштабирование параметров незапечённого `ScreentoneLayer` через `scale_spec_for_downsample` при даунсемплинге прокси. | Баг (прокси-превью с искажённым шагом точек) | **Низкий** (локальная ветка в `downsample_layer`) |
| `crates/compose/src/viewport.rs` | Аналогичное масштабирование незапечённого `ScreentoneLayer` в уменьшенных страницах вьюпорта. | Баг (вьпорт-редукция незапечённых скринтонов) | **Низкий** (локальная ветка) |
| `crates/algo/src/comic_screentone.rs` | Поддержка 5 типов градиента (`Linear`, `Radial`, `Angle`, `Reflected`, `Diamond`) и перенос дублировавшейся логики из `manga-core`. | Интеграция манги (единый источник правды в `algo`) | **Отсутствует** (файл добавлен форком) |
| `crates/gpu/src/compose.wgsl` | WGSL-поддержка градиентов `Angle`/`Reflected`/`Diamond`, `fill_frame` и `preserve_lines` в проходе скринтона. | Интеграция манги (GPU/CPU паритет скринтона) | **Средний** (крупный общий шейдер, но блок скринтона локализован) |
| `crates/gpu/src/plan/lower.rs` | Передача параметров градиента, `fill_frame` и `preserve_lines` в uniform-структуру `ScreentoneOp`. | Интеграция манги | **Низкий** (ветка `Screentone`) |
| `crates/gpu/src/plan/planner.rs` | Планирование GPU-узла `Screentone` с учётом `fill_frame` и режима `Shading`/`Sheet`. | Интеграция манги | **Низкий** (ветка `Screentone`) |
| `crates/gpu/tests/it/parity.rs` | Интеграционный тест `screentone_preserve_lines_parity` и проверки градиентов GPU/CPU. | Верификация паритета | **Низкий** (добавлен в конец файла) |
| `crates/io/src/psd_export.rs`, `crates/io/src/psd_export/document.rs`, `crates/io/src/psd_export/tests.rs` | Прокинут `&Document` в `Ex` для on-demand рендера незапечённых скринтонов при экспорте слоёв в PSD + регрессионные тесты. | Баг (пустой слой скринтона в PSD) | **Низкий–Средний** (добавлено поле `doc` в `Ex`) |
| `crates/ui-egui/src/canvas/{tool_input.rs, gesture.rs, paint.rs}` | Протяжка drag-жеста для `Tool::Screentone` (предпросмотр линии градиента и делегирование завершения в `manga_seam::screentone_drag_finish`). | Интеграция манги (градиент скринтона драгом по холсту) | **Низкий** (по 1–6 строк в ветках `Tool::Screentone`) |
| `crates/ui-egui/src/state/tool_options.rs`, `crates/ui-egui/src/tools/{stroke_constraint.rs, tool_reset.rs}` | Поля `screentone_clip`, `screentone_drag_gradient`, привязка Shift-45° и сброс опций инструмента `Tool::Screentone`. | Интеграция манги | **Низкий** (локальные ветки `Tool::Screentone`) |

---

## Фаза 1A — Стабилизация GPU-пайплайна SFX (устранение крашей, алиасинга и повреждений текстур)

| Файл upstream | Суть правки | Причина | Риск конфликта при rebase |
|---|---|---|---|
| `crates/gpu/src/sfx.rs` | Фаервол алиасинга слотов текстур (запрет одновременного чтения/записи в один `TexSlot`), выделен `TexSlot::ScratchC`, безопасный `run_jfa_steps`, `SfxFillSource::None`, обёртка `run_scoped` + проверка `DeviceHealth` (защита от каскадных validation errors/device lost), корректный жизненный цикл `Tex2d` (Ukiyo-e / shtack fill / dry brush). | Баг (вылет приложения и повреждение текстур в SFX-обводке) | **Отсутствует** (файл добавлен форком) |
| `crates/ui-egui/src/canvas/sfx_gpu_preview.rs` | Изолированные слоты превью для мультиоконного режима, `clamp_scissor_rect` в границах render target, очистка незавершённого состояния в `InFlightTile::drop`. | Баг (вылеты wgpu validation / артефакты при закрытии, ресайзе и переключении окон) | **Отсутствует** (файл добавлен форком) |
| `crates/gpu/tests/it/sfx.rs` | Регрессионные GPU-тесты на Ukiyo-e, shtack-заливку, dry brush, отсутствие алиасинга слотов и корректную работу `DeviceHealth`. | Верификация фиксов Фазы 1A | **Отсутствует** (файл добавлен форком) |

---

## Фаза 1B — Устранение OOM на `17000×25000`, фикс TDR, единый `Auto`-предикат, отмена потоков, GLES-дедлок

| Файл upstream | Суть правки | Причина | Риск конфликта при rebase |
|---|---|---|---|
| `crates/algo/src/comic_screentone.rs` | Тайловый (`256×256`) перенос в `render_screentone_to_surface` без плоского буфера на весь холст; общий предикат `screentone_prefers_gpu` (`area >= 512²`, `false` для CMYK/Lab); 8-лучевое CPU-зеркало `eval_preserve_lines_8ray` + юнит-тесты. | Баг (OOM 17K×25K + рассинхрон `Auto` + синхронизация с WGSL TDR-фиксом) | **Отсутствует** (файл добавлен форком) |
| `crates/compose/src/screentone.rs` | Исправлен `band_rows_for(frame.width(), 0)` (ранее передавалась полная высота кадра, отключавшая полосное разбиение); добавлены `buffer_has_tonal_ink` и потайловый `has_visible_backdrop_below` для fallback поверх пустого/белого холста. | Баг (OOM при экспорте/слиянии больших скринтонов + пустой `flattenImage` на белом фоне) | **Отсутствует** (файл добавлен форком) |
| `crates/compose/src/composite.rs` | Исправлена индексация clipped `Dissolve` (`usize` вместо переполняемого `i32` при больших координатах) + тест `dissolve_clipped_matches_spatial_coordinates_without_i32_cast_bug`; подключение `buffer_has_tonal_ink` в `render_layer_or_screentone`. | Баг (переполнение/паника на больших координатах + согласованность `Shading` на белом фоне) | **Средний** (общий файл композитора) |
| `crates/engine/src/pixels.rs`, `crates/engine/src/cmd/{cmd.rs, edit_cmds.rs, ops.rs}`, `crates/engine/src/cmd/layer_multi_cmds/structure.rs` | Убран лишний полноразмерный pre-bake `Sheet`-скринтонов перед `merge_down`, `merge_layers`, `merge_group`, `merge_visible`, `flattenImage`; оставлен полосный `ensure_*_shading_screentone*_baked` только для `Shading` при частичных слияниях подстека. | Баг (OOM при слиянии слоёв на `17000×25000`) | **Средний** (команды слияния в `engine` могут меняться в upstream; правки точечные по 1–4 строки) |
| `crates/engine/src/cmd/image_cmds.rs` | Удалён `flatten_whole`: `image.trim` стримит холст полосами через `render_bands`, `image.duplicate(mergedOnly: true)` использует `flatten_to_surface` + тест `banded_trim_and_duplicate_merged_preserve_expected_bounds`. | Баг (OOM в `image.trim` и `image.duplicate` на гигапиксельных страницах) | **Средний** (функция `trim` и `duplicate` в `image_cmds.rs`) |
| `crates/io/src/psd_export.rs` | В `Ex::mask` добавлена полосная ветка `banded_mask_channel` / `banded_mask_pass` при превышении `banded::banded_from()` + тест `banded_mask_matches_dense_bitwise`. | Баг (OOM при экспорте масок на больших страницах в PSD) | **Средний** (модуль PSD-экспорта активно развивается в upstream) |
| `crates/gpu/src/compose.wgsl` | Оптимизированы `st_is_line_edge` (макс. 25 `textureLoad` вместо 41, ранний выход за 1) и 8-лучевой `st_delined_luma` (снижение потолка с ~17 195 до 473 `textureLoad`/px при `line_thickness = 5.0`, на заливках ~73). | Баг (Windows GPU TDR / зависание драйвера при `preserve_lines`) | **Средний** (локализовано в хелперах `st_*`) |
| `crates/gpu/src/plan/planner.rs`, `crates/gpu/src/plan/tests.rs` | Перевод эвристики `Auto` на единый `screentone_prefers_gpu` по `screentone_frame` + тест на узкую полосу `2000×300`. | Баг (рассинхрон выбора GPU/CPU между `planner`, `gizmo` и `engine`) | **Низкий** (ветка `Screentone`) |
| `crates/gpu/src/setup.rs`, `crates/gpu/tests/it/pipelines.rs` | Добавлен `Compositor::try_new_for_adapter` (последовательная сборка на `wgpu::Backend::Gl`, Rayon на Vulkan/DX12/Metal); `try_new_with_format_threads_classified` обходит пул Rayon при `threads <= 1`. | Баг (дедлок при многопоточной сборке пайплайнов на GLES/ANGLE) | **Низкий–Средний** (инициализация `Compositor` в `setup.rs`) |
| `crates/ui-egui/src/dialogs/{camera_raw_ui.rs, camera_raw_ui/proxy.rs, camera_raw_ui/tests.rs}` | Кооперативная отмена (`Arc<AtomicBool>`) фонового потока `camera-raw-proxy` при смене параметров и в `Drop` (`ProxyJob::run_cancellable`) + тест `cancelled_proxy_job_exits_immediately`. | Баг (фоновые CPU-призраки после закрытия/перенастройки Camera Raw) | **Средний** (диалог Camera Raw развивается в upstream) |
| `crates/ui-egui/src/dialogs/export_dialog.rs` | Кооперативная отмена (`Arc<AtomicBool>` + `Interrupt::cancel_only`) потоков `export-preview` и `export-estimate` при смене параметров и закрытии диалога + тест `cancelled_export_workers_exit_immediately`. | Баг (фоновые CPU-призраки при смене настроек/закрытии Export) | **Средний** (диалог Export развивается в upstream) |
| `crates/ui-egui/src/canvas/background_redraw.rs` | `render_block_cancellable` на базе `render_bands(..., RENDER_TILE, ...)` с проверкой `stop` на границе каждой 256px-полосы + тест `render_block_cancellable_exits_immediately_when_stopped`. | Баг (непрерываемый фоновый перерендер тяжёлых полос) | **Низкий–Средний** (локальная функция в `background_redraw.rs`) |

---

## Примечания по локальной «чемоданной» части (для полноты контекста)
* `crates/manga-core/src/comic_screentone.rs` — дедупликация в пользу `crates/algo/src/comic_screentone.rs`.
* `crates/manga-ui/src/comic_screentone_gizmo.rs`, `comic_screentone_panel.rs`, `i18n/ru.tsv` — единый `screentone_prefers_gpu` и UI для 5 типов градиентов.
* `crates/engine/src/cmd/manga_cmds/comic_screentone.rs` — полосный `sample_source_surface`, потайловые `has_source_ink`/`revealed_bounds`, `ensure_*_shading_screentone*_baked`, единый `screentone_prefers_gpu`, регрессионные тесты.
* `crates/ui-egui/src/manga_seam.rs` — локализация `st.last_damage` в `fill_frame` при live GPU-перетаскивании, обработчик `screentone_drag_finish`.
* `TODO/SCREENTONE_AND_MANGANAKAMA_ROADMAP.md` — актуализация статусов Блока A.
