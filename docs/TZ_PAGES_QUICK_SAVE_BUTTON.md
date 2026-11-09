    > **Статус: устарело (superseded).** Актуальная версия — `docs/TZ_PAGES_QUICK_SAVE_BUTTON_V2.md`. Этот файл оставлен для истории; код писать по нему нельзя.

# ТЗ для Argon (Gemini Web): Кнопка быстрого сохранения «💾» (Quick In-Place Save) в панели страниц

    **Проект:** Photocraft Community Fork (`nnmgrp/photocraft`)  
    **Целевой исполнитель:** Argon (работает через веб-интерфейс Gemini, без доступа к диску)  
    **Репозиторий:** `F:\Downloads\Telegram Desktop\FORK\photocraft`  
    **Затрагиваемые файлы:**
    1. `crates/ui-egui/src/pages_panel.rs`
    2. `crates/ui-egui/src/lib.rs`

    ---

    ## 1. Контекст задачи и диалог FGSFDS & DeepSeek

    При закрытии страницы на крестик `✖` в панели «Страницы» теперь открывается модальный диалог (`discard_ui.rs`), защищающий от потери правок.
    Для удобства работы добавляется кнопка **быстрого сохранения на месте** `💾` рядом с крестиком `✖`.

    ### Ключевые договорённости:
    1. **Только для измененных файлов («грязных»):** Если страница чистая, кнопка `💾` не отображается (чтобы не создавать визуальный шум на десятках страниц).
    2. **Только для форматов, сохраняемых на месте без окон (PSD, PSB, .pcraft, ORA):**
    * Кнопка обещает мгновенную запись в исходный файл в один клик.
    * Плоские форматы (`.png`, `.jpg`, `.jpeg`, `.webp`, `.tga` и безымянные файлы `None`) **НЕ** должны показывать кнопку `💾`, так как сохранение для них требует вызова «Сохранить как» (Save As). Для них сохранение происходит штатно через диалог закрытия или `Ctrl+S`.
    * Формат `ORA` показывает кнопку только если в документе нет неподдерживаемых элементов (стилей, корректирующих слоев): `holds_at(&doc, path) == true`.
    3. **Расположение в UI:**
    * **Список (List View):** Слева от кнопки `✖` (в раскладке `Layout::right_to_left` добавляется после `✖`).
    * **Сетка (Grid View):** Слева от кнопки `✖` (после `✖`).
    * **Контекстное меню:** Пункт «💾 Сохранить страницу» перед «✖ Закрыть страницу».
    4. **Поведение при клике:**
    * Если страница не активна — сначала переключается фокус на неё (`set_active(idx)`), затем вызывается штатная команда `"file.save"`.
    * Диалог «файл изменен сторонней программой» продолжает работать штатно.

    ---

    ## 2. Конкретные изменения в коде с точными строками

    ### Файл 1: `crates/ui-egui/src/pages_panel.rs`

    #### Место 1: Добавление поля `can_quick_save` в `PageItemInfo`
    **Строка ~48-58:**
    ```rust
    // ДО:
        /// Флаг наличия несохраненных изменений в документе.
        pub is_dirty: bool,
        /// Общее количество текстовых реплик (баблов) в `TypeR` для этой страницы.
        pub total_lines: usize,
        /// Количество уже сверстанных (вставленных на холст) реплик.
        pub applied_lines: usize,
        /// Опциональный идентификатор текстуры миниатюры страницы в `egui`.
        pub thumbnail_texture: Option<TextureId>,
        /// Разрешение исходного холста страницы (ширина, высота) для подсказки.
        pub dimensions: Option<(u32, u32)>,
    }

    // ПОСЛЕ:
        /// Флаг наличия несохраненных изменений в документе.
        pub is_dirty: bool,
        /// Флаг доступности быстрого сохранения на месте (PSD/PSB/.pcraft/ORA без потерь).
        pub can_quick_save: bool,
        /// Общее количество текстовых реплик (баблов) в `TypeR` для этой страницы.
        pub total_lines: usize,
        /// Количество уже сверстанных (вставленных на холст) реплик.
        pub applied_lines: usize,
        /// Опциональный идентификатор текстуры миниатюры страницы в `egui`.
        pub thumbnail_texture: Option<TextureId>,
        /// Разрешение исходного холста страницы (ширина, высота) для подсказки.
        pub dimensions: Option<(u32, u32)>,
    }
    ```

    В конструкторе `PageItemInfo::new` (**строка ~71-82**):
    ```rust
            Self {
                doc_index,
                title,
                path,
                detected_page,
                is_active,
                is_dirty: false,
                can_quick_save: false,
                total_lines: 0,
                applied_lines: 0,
                thumbnail_texture: None,
                dimensions: None,
            }
    ```

    ---

    #### Место 2: Добавление `SavePage(usize)` в `PagesPanelAction`
    **Строка ~116-125:**
    ```rust
    // ДО:
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PagesPanelAction {
        /// Переключить активный документ на холсте (передается `doc_index`).
        SelectPage(usize),
        /// Закрыть указанный документ (передается `doc_index`).
        ClosePage(usize),
        /// Открыть системный диалог выбора папки с главой манги.
        OpenChapterFolder,

    // ПОСЛЕ:
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PagesPanelAction {
        /// Переключить активный документ на холсте (передается `doc_index`).
        SelectPage(usize),
        /// Закрыть указанный документ (передается `doc_index`).
        ClosePage(usize),
        /// Быстро сохранить страницу в её существующий файл на диске без диалогов (передается `doc_index`).
        SavePage(usize),
        /// Открыть системный диалог выбора папки с главой манги.
        OpenChapterFolder,
    ```

    ---

    #### Место 3: Кнопка `💾` в режиме списка (`render_list_view`)
    **Строка ~875-890:**
    В `Layout::right_to_left` виджеты добавляются справа налево. Добавление `💾` сразу после `✖` визуально расположит дискету **слева** от крестика `[↑] [↓] [💾] [✖]`:

    ```rust
    // ДО:
                // Кнопки управления справа (вверх, вниз, закрыть)
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;

                    if ui
                        .small_button("✖")
                        .on_hover_text("Закрыть страницу")
                        .clicked()
                    {
                        action = Some(PagesPanelAction::ClosePage(item.doc_index));
                    }

                    if item.doc_index + 1 < total_session_docs

    // ПОСЛЕ:
                // Кнопки управления справа (вверх, вниз, сохранить, закрыть)
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;

                    if ui
                        .small_button("✖")
                        .on_hover_text("Закрыть страницу")
                        .clicked()
                    {
                        action = Some(PagesPanelAction::ClosePage(item.doc_index));
                    }

                    if item.can_quick_save {
                        if ui
                            .small_button("💾")
                            .on_hover_text("Сохранить страницу в её файл (Ctrl+S)")
                            .clicked()
                        {
                            action = Some(PagesPanelAction::SavePage(item.doc_index));
                        }
                    }

                    if item.doc_index + 1 < total_session_docs
    ```

    ---

    #### Место 4: Кнопка `💾` в режиме сетки (`render_grid_view`)
    **Строка ~1071-1080:**
    Аналогично, в `Layout::right_to_left` помещаем `💾` после `✖`:

    ```rust
    // ДО:
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .small_button("✖")
                        .on_hover_text("Закрыть страницу")
                        .clicked()
                    {
                        action = Some(PagesPanelAction::ClosePage(item.doc_index));
                    }
                });

    // ПОСЛЕ:
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .small_button("✖")
                        .on_hover_text("Закрыть страницу")
                        .clicked()
                    {
                        action = Some(PagesPanelAction::ClosePage(item.doc_index));
                    }

                    if item.can_quick_save {
                        if ui
                            .small_button("💾")
                            .on_hover_text("Сохранить страницу в её файл (Ctrl+S)")
                            .clicked()
                        {
                            action = Some(PagesPanelAction::SavePage(item.doc_index));
                        }
                    }
                });
    ```

    ---

    #### Место 5: Пункт в контекстном меню (`render_card_context_menu`)
    **Строка ~1455-1468:**
    ```rust
    // ДО:
            ui.separator();

            if ui
                .button(
                    RichText::new("✖ Закрыть страницу").color(Color32::from_rgb(240, 90, 90)),
                )
                .clicked()
            {
                action = Some(PagesPanelAction::ClosePage(item.doc_index));
                ui.close();
            }

    // ПОСЛЕ:
            ui.separator();

            if item.can_quick_save {
                if ui
                    .button(RichText::new("💾 Сохранить страницу").color(Color32::from_rgb(100, 200, 100)))
                    .clicked()
                {
                    action = Some(PagesPanelAction::SavePage(item.doc_index));
                    ui.close();
                }
            }

            if ui
                .button(
                    RichText::new("✖ Закрыть страницу").color(Color32::from_rgb(240, 90, 90)),
                )
                .clicked()
            {
                action = Some(PagesPanelAction::ClosePage(item.doc_index));
                ui.close();
            }
    ```

    ---

    ### Файл 2: `crates/ui-egui/src/lib.rs`

    #### Место 1: Расчёт `can_quick_save` при сборке `PageItemInfo`
    **Строка ~1595-1650:**
    Используем существующие движковые проверки:
    `photocraft_engine::file_cmds::saves_in_place(p)` и `photocraft_engine::file_cmds::holds_at(&doc_state.doc, p)`.

    ```rust
    // ДО (строки 1595-1608):
                let (title, path, is_dirty, dimensions, doc_id, doc_rev) = {
                    let doc_state = match self.session.documents().get(idx) {
                        Some(s) => s,
                        None => continue,
                    };
                    (
                        doc_state.doc.name.clone(),
                        doc_state.path.as_deref().map(std::path::PathBuf::from),
                        doc_state.is_dirty(),
                        Some((doc_state.doc.size.width, doc_state.doc.size.height)),
                        doc_state.doc.id,
                        doc_state.revision,
                    )
                };

    // ПОСЛЕ:
                let (title, path, is_dirty, dimensions, doc_id, doc_rev, can_quick_save) = {
                    let doc_state = match self.session.documents().get(idx) {
                        Some(s) => s,
                        None => continue,
                    };
                    let can_quick_save = doc_state.is_dirty()
                        && doc_state.path.as_deref().is_some_and(|p| {
                            photocraft_engine::file_cmds::saves_in_place(p)
                                && photocraft_engine::file_cmds::holds_at(&doc_state.doc, p)
                        });
                    (
                        doc_state.doc.name.clone(),
                        doc_state.path.as_deref().map(std::path::PathBuf::from),
                        doc_state.is_dirty(),
                        Some((doc_state.doc.size.width, doc_state.doc.size.height)),
                        doc_state.doc.id,
                        doc_state.revision,
                        can_quick_save,
                    )
                };
    ```

    И при присвоении в `info` (**строка ~1643-1647**):
    ```rust
    // ДО:
                let is_active = Some(idx) == active_doc_idx;
                let mut info = pages_panel::PageItemInfo::new(idx, title, path, is_active);
                info.is_dirty = is_dirty;
                info.dimensions = dimensions;
                info.thumbnail_texture = thumbnail_texture;

    // ПОСЛЕ:
                let is_active = Some(idx) == active_doc_idx;
                let mut info = pages_panel::PageItemInfo::new(idx, title, path, is_active);
                info.is_dirty = is_dirty;
                info.can_quick_save = can_quick_save;
                info.dimensions = dimensions;
                info.thumbnail_texture = thumbnail_texture;
    ```

    ---

    #### Место 2: Обработка `SavePage(idx)` в `handle_pages_action`
    **Строка ~1697-1700:**
    ```rust
    // ДО:
                pages_panel::PagesPanelAction::ClosePage(idx) => {
                    let _ = crate::menus::invoke(self, ctx, "file.close", serde_json::json!({"document": idx}));
                }
                pages_panel::PagesPanelAction::OpenChapterFolder => {

    // ПОСЛЕ:
                pages_panel::PagesPanelAction::ClosePage(idx) => {
                    let _ = crate::menus::invoke(self, ctx, "file.close", serde_json::json!({"document": idx}));
                }
                pages_panel::PagesPanelAction::SavePage(idx) => {
                    // Если кликнули на неактивной странице — активируем её, чтобы file.save применился именно к ней
                    if self.session.active_index() != Some(idx) {
                        self.session.set_active(idx);
                    }
                    let _ = crate::menus::invoke(self, ctx, "file.save", serde_json::json!({}));
                }
                pages_panel::PagesPanelAction::OpenChapterFolder => {
    ```

    ---

    ## 3. Юнит-тест в конце `crates/ui-egui/src/pages_panel.rs`
    **Строка ~1729:**
    Добавить тест:

    ```rust
        #[test]
        fn test_quick_save_page_rules_and_action() {
            use crate::{PhotocraftApp, Services};
            use photocraft_engine::Session;
            use serde_json::json;

            let mut app = PhotocraftApp::new(Session::new(), Services::default());
            // 1. Создаем безымянный PSD без пути на диске
            app.run("file.new", json!({"width": 10, "height": 10, "name": "untitled.psd"})).unwrap();
            app.run("layer.new.layer", json!({})).unwrap();

            let doc_state = app.session.active().unwrap();
            assert!(doc_state.is_dirty());
            assert!(doc_state.path.is_none());

            // Безымянный файл не должен иметь can_quick_save
            let can_save_unnamed = doc_state.path.as_deref().is_some_and(|p| {
                photocraft_engine::file_cmds::saves_in_place(p)
                    && photocraft_engine::file_cmds::holds_at(&doc_state.doc, p)
            });
            assert!(!can_save_unnamed, "Безымянный документ не должен сохраняться по месту без Save As");

            // 2. Проверяем форматы через движковые предикаты:
            // Многослойные форматы
            assert!(photocraft_engine::file_cmds::saves_in_place("chapter/page01.psd"));
            assert!(photocraft_engine::file_cmds::saves_in_place("chapter/page01.psb"));
            assert!(photocraft_engine::file_cmds::saves_in_place("chapter/page01.pcraft"));

            // Плоские форматы НЕ поддерживают quick save на месте
            assert!(!photocraft_engine::file_cmds::saves_in_place("chapter/page01.png"));
            assert!(!photocraft_engine::file_cmds::saves_in_place("chapter/page01.jpg"));
            assert!(!photocraft_engine::file_cmds::saves_in_place("chapter/page01.webp"));

            // 3. Проверяем переключение активного документа при действии SavePage
            app.run("file.new", json!({"width": 10, "height": 10, "name": "page02.psd"})).unwrap();
            assert_eq!(app.session.active_index(), Some(1));

            app.handle_pages_action(&egui::Context::default(), PagesPanelAction::SavePage(0));
            assert_eq!(app.session.active_index(), Some(0), "SavePage должен активировать целевой документ");
        }
    ```

    ---

    ## 4. Команды проверки сборки и тестов (PowerShell, строго без записи на диск C:)
    ```powershell
    powershell -ExecutionPolicy Bypass -Command "& 'F:\fork_home\run-cargo.ps1' check"
    powershell -ExecutionPolicy Bypass -Command "& 'F:\fork_home\run-cargo.ps1' test -p photocraft-ui-egui pages_panel"
    ```
