# ТЗ №2: Доработка и стабилизация Photocraft v0.16 (Community)

> **Для кого:** Модели OpenCode / Antigravity.  
> **Целевая ветка:** `sync/v0.16-community`.  
> **Репозиторий:** `F:\Downloads\Telegram Desktop\FORK\photocraft`

---

## ⚠️ СТРОЖАЙШИЕ ПРАВИЛА ОКРУЖЕНИЯ

1. **ДИСК C: ЗАЩИЩЕН ОТ ЗАПИСИ:** На диске `C:` свободно менее 2.4 GB. Ни в коем случае НЕ вызывайте `cargo` напрямую!
2. **ЕДИНСТВЕННЫЙ СПОСОБ ЗАПУСКА КАРГО:** Все команды `cargo` запускать **СТРОГО** через скрипт-обертку:
   ```powershell
   powershell -ExecutionPolicy Bypass -File "F:\fork_home\run-cargo.ps1" <аргументы>
   ```
3. **КОММИТЫ:** Префикс `fix(v0.16):` или `feat(v0.16):`.

---

## ШАГ 0. ИСХОДНАЯ ВЕТКА

1. Переключитесь на ветку:
   ```bash
   git checkout sync/v0.16-community
   git pull origin sync/v0.16-community
   ```
2. Убедитесь, что база 0.16 и коммит `05035a249` активны.

---

## ЗАДАЧА 1. ИСПРАВЛЕНИЕ РЕГРЕССИЙ ПОРТИРОВАНИЯ 0.16

### 1. GenTonik (Скринтоны) серый/отключен
* **Файл:** `crates/ui-egui/src/menus.rs`, `crates/engine/src/filters.rs`
* **Проблема:** Пункт меню GenTonik отображается серым (disabled), диалог скринтона не открывается.
* **Решение:** Проверить предикат доступности команды `filter.screentone`. Убедиться, что при наличии активного растрового слоя пункт активен (`is_raster_layer && !is_locked`), а при клике корректно поднимается окно настроек генератора полутонов.

### 2. Панель Pages отсутствует в дефолтном докинге
* **Файл:** `crates/ui-egui/src/panel_docking.rs`
* **Проблема:** При первом запуске 0.16 панель Pages не видна в нижнем или боковом доке, хотя доступна через меню `Window -> Pages`.
* **Решение:** Добавить `"pages"` в список вкладок по умолчанию нижнего дока (`Tab::Pages`) рядом с Timeline/History.

### 3. Пропала вертикальная вёрстка (Y) в палитре Paragraph и на холсте
* **Файл:** `crates/ui-egui/src/type_panels_ui.rs`
* **Проблема:** В 0.16 при обновлении интерфейса текста кнопки вертикального выравнивания (Top, Center, Bottom) пропали из палитры Paragraph, а существующие кнопки не двигают текстовый фрейм.
* **Решение:** Восстановить три кнопки вертикального выравнивания по Y в секцию абзаца (`ui_paragraph_panel`), связать их с `frame_alignment_y` в структуре текстового слоя.

### 4. Пропали избранные шрифты (Favorite Fonts)
* **Файл:** `crates/ui-egui/src/type_panels_ui.rs` / `font_picker.rs`
* **Проблема:** В выпадающем списке выбора шрифта пропали звёздочки для добавления в избранное и фильтр «Показать только избранные».
* **Решение:** Восстановить чтение/запись списка `favorite_fonts: HashSet<String>` из настроек пользователя и отображение иконки звёздочки `⭐` слева от названия каждого шрифта.

### 5. Схлопывание вкладок при открытии TypeR
* **Файл:** `crates/ui-egui/src/panel_docking.rs`, `crates/ui-egui/src/typesetter_ui.rs`
* **Проблема:** При открытии панели TypeR соседние вкладки в правом доке (`Character`, `Paragraph`, `Layers`) самопроизвольно закрываются.
* **Решение:** Разрешить одновременное сосуществование панели TypeR и стандартных текстовых панелей. Не форсировать скрытие соседних панелей в методе `show_typesetter_panel()`.

### 6. Конфликт шортката Ctrl+Shift+F (Layer Search vs Fade)
* **Файл:** `crates/ui-egui/src/lib.rs` / `menus.rs`
* **Проблема:** Нажатие `Ctrl+Shift+F` перехватывается командой `edit.fade` («Fade is not available, there is nothing to Fade») вместо открытия быстрого поиска по слоям.
* **Решение:** Переназначить поиск по слоям на шорткат `Ctrl+Alt+F` (или `Ctrl+F` при активной панели слоёв), чтобы устранить коллизию с классическим Fade из Photoshop.

---

## ШАГ 2. ПРОВЕРКА И СБОРКА РЕЛИЗА 0.16

1. **Проверка компиляции:**
   ```powershell
   powershell -ExecutionPolicy Bypass -File "F:\fork_home\run-cargo.ps1" check -p ui-egui
   ```
2. **Запуск тестов:**
   ```powershell
   powershell -ExecutionPolicy Bypass -File "F:\fork_home\run-cargo.ps1" test -p engine -p ui-egui
   ```
3. **Сборка релизного бинарника 0.16:**
   ```powershell
   powershell -ExecutionPolicy Bypass -File "F:\fork_home\run-cargo.ps1" build --release -p photocraft
   ```
4. **Упаковка релиза 0.16:**
   Создать каталог `dist/photocraft-v0.16-community/` и скопировать туда `target/release/photocraft.exe`.
