# ТЗ: Защита от потери данных при закрытии страниц и Редизайн модального окна TypeR

**Проект:** Photocraft Community Fork (`nnmgrp/photocraft`)  
**Дата:** 28 октября 2026  
**Целевые исполнители:** DeepSeek-V3 / OpenCode / SnowE22 / NanoNik  
**Приоритет:** **CRITICAL (Data Loss) + HIGH (UI/UX)**  

---

## 1. Сводка задач

| № | Блок | Файлы | Приоритет | Суть задачи |
|---|---|---|---|---|
| **1** | **Pages: Защита от потери данных** | `crates/ui-egui/src/lib.rs`<br>`crates/ui-egui/src/panel_docking.rs`<br>`crates/ui-egui/src/pages_panel.rs` | **CRITICAL** | Крестик «✖ Закрыть страницу» в панели страниц стирает документ из RAM без сохранения. Заменить вызов `self.run` на `crate::menus::invoke` с защитой `discard_ui`. |
| **2** | **TypeR: Редизайн Font Settings** | `crates/ui-egui/src/snow_typesetter_ui.rs`<br>`crates/engine/src/snow_typesetter.rs`<br>`crates/ui-egui/src/lib.rs` | **HIGH** | Модернизация окна по макету Photoshop: полоска символов `[T][T][TT]...`, выбор выравнивания `[≡L][≡C][≡R]`, убрать дубли полей, починить заблокированное перемещение (`.anchor`) и забытые поля экспорта. |

---

## 2. Задача 1: Защита от потери данных при закрытии страниц (CRITICAL)

### 2.1. Описание проблемы
Нажатие на кнопку `✖` (*«Закрыть страницу»*) на миниатюре страницы в панели Pages закрывает документ немедленно, даже если он был отредактирован (`is_dirty == true`), без всплывающего окна с вопросом о сохранении.

### 2.2. Причина в коде
В файле [`crates/ui-egui/src/lib.rs:1679-1681`](file:///F:/Downloads/Telegram%20Desktop/FORK/photocraft/crates/ui-egui/src/lib.rs#L1679-L1681):
```rust
pages_panel::PagesPanelAction::ClosePage(idx) => {
    let _ = self.run("file.close", serde_json::json!({"document": idx}));
}
```
* `self.run(...)` отправляет команду `"file.close"` напрямую в низкоуровневый движок (`photocraft_engine`).
* Движок мгновенно выгружает страницу из памяти.
* Защитный механизм PhotoCraft ([`crate::discard_ui::intercept`](file:///F:/Downloads/Telegram%20Desktop/FORK/photocraft/crates/ui-egui/src/menus.rs#L81)) вызывается **только** через [`crate::menus::invoke()`](file:///F:/Downloads/Telegram%20Desktop/FORK/photocraft/crates/ui-egui/src/menus.rs#L75).

### 2.3. Требуемое решение
1. В сигнатуру метода `handle_pages_action` в [`crates/ui-egui/src/lib.rs`](file:///F:/Downloads/Telegram%20Desktop/FORK/photocraft/crates/ui-egui/src/lib.rs#L1652) передать контекст egui:
   ```rust
   pub(crate) fn handle_pages_action(&mut self, ctx: &egui::Context, action: pages_panel::PagesPanelAction)
   ```
2. В [`crates/ui-egui/src/panel_docking.rs:1299`](file:///F:/Downloads/Telegram%20Desktop/FORK/photocraft/crates/ui-egui/src/panel_docking.rs#L1299) и `:1713` передавать `ui.ctx()`:
   ```rust
   if let Some(action) = app.render_pages_panel(ui) {
       app.handle_pages_action(ui.ctx(), action);
   }
   ```
3. В обработчике `ClosePage` вызывать безопасную функцию с перехватом:
   ```rust
   pages_panel::PagesPanelAction::ClosePage(idx) => {
       let _ = crate::menus::invoke(self, ctx, "file.close", serde_json::json!({"document": idx}));
   }
   ```
4. Добавить юнит-тест в `crates/ui-egui/src/pages_panel.rs`:
   * Создать документ, внести изменение (`layer.new.layer`), убедиться, что `is_dirty() == true`.
   * Вызвать `handle_pages_action` с `ClosePage(0)`.
   * Убедиться, что вызов перехвачен модальным окном `discard_ui` и документ не удалён из `app.session.documents()`.

---

## 3. Задача 2: Редизайн и исправление модалки «Font Settings» (HIGH)

### 3.1. Визуальная схема интерфейса (ASCII UI Mockup)

```text
┌────────────────────────────────────────────────────────────────────────┐
│ Font Settings: Крик / SFX                                          [✖] │
├────────────────────────────────────────────────────────────────────────┤
│ Style name:  [ Крик / SFX                                            ] │
│ Folder:      [ Звуки                                               ▾ ] │
├────────────────────────────────────────────────────────────────────────┤
│ Typography and Font                                                    │
│                                                                        │
│ Font:        [★] [ CC Wild Words                                   ▾ ] │
│ PostScript:  [ CCWildWords                ]  Style: [ Regular      ▾ ] │
│                                                                        │
│ Оформление:  [ B ] [ I ] [ TT ] [ Tт ] [ T¹ ] [ T₁ ] [ U ] [ S ] [ «» ]│
│                                                                        │
│ Выравнивание:[ ≡ По левому        ] [ ≡ По центру ] [ ≡ По правому   ] │
│                                                                        │
│ Anti-alias:  [ Чёткий (crisp)                                      ▾ ] │
│ Color:       [■] [ #C70000FF   ]                                       │
├────────────────────────────────────────────────────────────────────────┤
│ Typography Size (Line Spacing Sync)                                    │
│                                                                        │
│ Система:     [ Metrics ]  [ Optical ]                                  │
│ Size (pt):   [ 24.0 ] [A-] [A+]     [🔗]  Leading:   [ 26.0          ] │
│ Tracking:    [ 20   ]                     H-Scale (%): [ 100         ] │
├────────────────────────────────────────────────────────────────────────┤
│ Text Outline                                                           │
│                                                                        │
│ Thickness:   [ 4.0 px ]             Color: [□] [ #FFFFFFFF           ] │
├────────────────────────────────────────────────────────────────────────┤
│ Предпросмотр:                                                          │
│ ┌────────────────────────────────────────────────────────────────────┐ │
│ │  Крик / SFX: Ах ты ж!.. Неужели это сработало?! 123                │ │
│ └────────────────────────────────────────────────────────────────────┘ │
├────────────────────────────────────────────────────────────────────────┤
│ [ Duplicate ]  [ Delete ]                                     [ Done ] │
└────────────────────────────────────────────────────────────────────────┘
```

---

### 3.2. Пошаговые технические требования

#### Шаг 1: Разблокировка перемещения окна
В [`crates/ui-egui/src/snow_typesetter_ui.rs:364-371`](file:///F:/Downloads/Telegram%20Desktop/FORK/photocraft/crates/ui-egui/src/snow_typesetter_ui.rs#L364-L371):
* **Удалить:** `.anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])` (блокировало перемещение окна мышью).
* **Использовать:**
  ```rust
  Window::new(title)
      .collapsible(false)
      .resizable(true)
      .default_width(480.0)
      .default_height(620.0)
      .min_width(430.0)
      .pivot(egui::Align2::CENTER_CENTER)
      .default_pos(ctx.screen_rect().center())
      .show(&ctx, |ui| {
          egui::ScrollArea::vertical()
              .auto_shrink([false, false])
              .show(ui, |ui| {
                  self.render_font_settings_modal(ui, edit_idx, &mut close_modal);
              });
      });
  ```

#### Шаг 2: Устранение дубликатов контролов ввода
В методе `render_font_settings_modal`:
1. **Строка «Папка» (Folder):**
   * Оставить один `egui::ComboBox`.
   * Внутри списка: пункт `Без папки`, существующие папки и пункт `+ Новая папка...` (открывающий компактный ввод названия новой папки).
   * **Удалить дублирующий `ui.text_edit_singleline(&mut folder_str)` из той же строки!**
2. **Строка «Шрифт» (Font):**
   * Оставить кнопку `★` (избранное) и поисковый пикер `family_picker_in`.
   * **Удалить дублирующий `ui.text_edit_singleline(&mut preset.font_family)` из той же строки!**
3. **Строка «Style» и «PostScript»:**
   * Поле `Style` оформить как выпадающий список доступных начертаний выбранного шрифта (`Regular`, `Bold`, `Italic` и т.д.).
   * Поле `PostScript` оставить компактным read-only / редактируемым полем рядом, либо спрятать вниз под `CollapsingHeader`.

#### Шаг 3: Полоска модификаторов символов в стиле Photoshop
Ряд компактных кнопок-тогглов (без громоздких чекбоксов):
* `[ B ]` — `preset.bold` (Псевдожирный / Bold)
* `[ I ]` — `preset.italic` (Псевдокурсив / Italic)
* `[ TT ]` — `preset.all_caps` (Все прописные / All Caps)
* `[ Tт ]` — `preset.small_caps` (Капитель / Small Caps)
* `[ T¹ ]` — `preset.superscript` (Верхний индекс)
* `[ T₁ ]` — `preset.subscript` (Нижний индекс)
* `[ U ]` — `preset.underline` (Подчёркивание)
* `[ S ]` — `preset.strikethrough` (Зачёркивание)
* `[ «» ]` — `preset.hanging_punctuation` (Оптическая компенсация знаков в строках)

Каждая кнопка должна иметь подсказку (hover tooltip).

#### Шаг 4: Выравнивание текста (Left / Center / Right)
1. **В структуре `TextStylePreset` ([`crates/engine/src/snow_typesetter.rs:40`](file:///F:/Downloads/Telegram%20Desktop/FORK/photocraft/crates/engine/src/snow_typesetter.rs#L40)):**
   ```rust
   #[serde(default = "default_align_center")]
   pub align: String, // "left", "center", "right"
   ```
   С функцией `fn default_align_center() -> String { "center".to_string() }`.
2. **В UI модального окна:**
   Сегментированный переключатель:
   `[ ≡ По левому ] [ ≡ По центру ] [ ≡ По правому ]`
3. **В рантайме создания текстового слоя ([`crates/ui-egui/src/lib.rs:1509`](file:///F:/Downloads/Telegram%20Desktop/FORK/photocraft/crates/ui-egui/src/lib.rs#L1509)):**
   Заменить жесткое `"align": "center"` на `action.preset.align.as_str()`.

#### Шаг 5: Исправление функции экспорта `export_styles_json`
В [`crates/engine/src/snow_typesetter.rs:210-270`](file:///F:/Downloads/Telegram%20Desktop/FORK/photocraft/crates/engine/src/snow_typesetter.rs#L210-L270):
* В сериализацию `paragraphStyle` пробросить выравнивание:
  ```rust
  "paragraphStyle": {
      "alignment": preset.align,
      "autoLeadingPercentage": 1.2
  }
  ```
* В объект стиля добавить сохранение сглаживания и висячей пунктуации:
  ```rust
  "antiAlias": preset.anti_alias,
  "hangingPunctuation": preset.hanging_punctuation,
  ```
* В `import_styles_json` поддержать считывание этих полей обратно.

#### Шаг 6: Компактная строка обводки текста (Text Outline)
В одну горизонтальную строку:
`Thickness: [ 4.0 px ]             Color: [■] [ #FFFFFFFF ]`
Если толщина > 0, обводка активна.

#### Шаг 7: Панель предпросмотра (Live Preview)
Внизу модального окна перед кнопками действий разместить компактный фрейм с живым рендерингом текста пресета:
`Крик / SFX: Ах ты ж!.. Неужели это сработало?! 123`
(используя выбранный цвет текста, кегль с ограничением высоты и цвет обводки).

---

## 4. Требования к верификации и сборке

1. **Правило диска C:** Все команды Cargo запускать строго через `run-cargo.ps1`:
   ```powershell
   powershell -ExecutionPolicy Bypass -Command "& 'F:\fork_home\run-cargo.ps1' check"
   ```
2. **Юнит-тесты:**
   ```powershell
   powershell -ExecutionPolicy Bypass -Command "& 'F:\fork_home\run-cargo.ps1' test -p photocraft-engine snow_typesetter"
   powershell -ExecutionPolicy Bypass -Command "& 'F:\fork_home\run-cargo.ps1' test -p photocraft-ui-egui pages_panel"
   ```
3. **Ручная проверка (Checklist):**
   - [ ] Открыть документ, изменить слой (`is_dirty`), нажать `✖` в панели Pages — появляется окно подтверждения сохранения, документ не удаляется молча.
   - [ ] Открыть окно Font Settings — окно можно перетаскивать мышью за заголовок в любое место экрана.
   - [ ] В окне отсутствуют дубли полей Folder и Font.
   - [ ] Полоска кнопок `[ B ] [ I ] [ TT ] [ Tт ] ...` переключает соответствующие параметры стиля.
   - [ ] Переключение выравнивания на `≡ По левому` создает текстовый слой с левым выравниванием.
   - [ ] Экспорт в `TypeR_Export.json` и повторный импорт сохраняют `anti_alias` и `hanging_punctuation`.
