# ТЗ №1 для OpenCode: Проверка коммитов SnowE22 и стабилизация Photocraft v0.14 (Comic-alpha)

> **Для кого:** Модели OpenCode (DeepSeek-V3 / GLM-4 / Claude).  
> **Целевая ветка:** `community/gpu-screentone-preview` (тег `Comic-alpha`).  
> **Репозиторий:** `F:\Downloads\Telegram Desktop\FORK\photocraft`

---

## ⚠️ СТРОЖАЙШИЕ ПРАВИЛА ОКРУЖЕНИЯ (НЕ НАРУШАТЬ)

1. **ДИСК C: ЗАЩИЩЕН ОТ ЗАПИСИ:** На диске `C:` свободно менее 2.4 GB. Ни в коем случае НЕ вызывайте `cargo` напрямую!
2. **ЕДИНСТВЕННЫЙ СПОСОБ ЗАПУСКА КАРГО:** Все команды `cargo` запускать **СТРОГО** через скрипт-обертку:
   ```powershell
   powershell -ExecutionPolicy Bypass -File "F:\fork_home\run-cargo.ps1" <аргументы>
   ```
   Этот скрипт перенаправляет `CARGO_HOME`, `RUSTUP_HOME` и кэш компилятора на диск `F:`.
3. **КОММИТЫ:** Писать атомарные понятные коммиты с префиксом `fix(v0.14):` или `feat(v0.14):`.

---

## ШАГ 0. ПРОВЕРКА СВЕЖИХ КОММИТОВ SNOWE22

SnowE22 уже запушил в репозиторий ветку `community/gpu-screentone-preview` и тег `Comic-alpha`.

1. Обновите локальную ветку:
   ```bash
   git checkout community/gpu-screentone-preview
   git pull origin community/gpu-screentone-preview
   ```
2. Убедитесь, что присутствуют 3 ключевых коммита SnowE22:
   * `f96494bfd` — `feat(layer-effects): интерактивное управление параметрами эффектов жестом мыши и всплывающий HUD`
   * `37cdd44b2` — `feat(typesetter): поддержка цвета шрифта и метрической/оптической системы в стилях TypeR`
   * `ca9df02c8` — `feat(typesetter,export): запоминание папки экспорта PNG (Ctrl+Shift+S), автоперенос слов в баббл и inline-редактирование реплик`

---

## ЗАДАЧА 1. АУДИТ И ПРОВЕРКА НАРАБОТОК SNOWE22

1. **Проверка стилей TypeR (`crates/engine/src/typesetter.rs`, `crates/ui-egui/src/typesetter_ui.rs`):**
   * Убедиться, что при сохранении стиля сохраняются цвет шрифта и система кернинга (`metrics`/`optical`).
   * Проверить, что при клике на стиль в палитре TypeR эти свойства корректно применяются к активному тексту.
2. **Проверка экспорта PNG и диалогов (`crates/ui-egui/src/export_dialog.rs`, `file_dialog.rs`):**
   * Убедиться, что последняя выбранная папка экспорта сохраняется в настройках (`prefs`) и подставляется при следующем нажатии `Ctrl+Shift+S`.
3. **Проверка inline-редактирования реплик:**
   * Проверить работу редактирования текста реплик прямо в списке TypeR (Singleline TextEdit) и отсутствие сбоев автосохранения (`self.auto_save()`).

---

## ЗАДАЧА 2. ИСПРАВЛЕНИЕ ОСТАВШИХСЯ UI-БАГОВ (ПО СКРИНШОТАМ)

### Баг 2.1 (Скриншот 1): Обрезка текста в Options внутри TypeR
* **Файл:** `crates/ui-egui/src/typesetter_ui.rs`
* **Симптом:** При открытии вкладки Options и сжатии панели текст «Baseline shift» и длинный путь к папке сценариев/стилей наезжают друг на друга и обрезаются.
* **Решение:**
  1. Включить перенос текста: `ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);`.
  2. Заменить фиксированную ширину слайдера Baseline Shift на динамическую:
     `let slider_w = (ui.available_width() * 0.45).clamp(60.0, 130.0);`
  3. Путь к папке отображать с сокращением (`truncate`) и подсказкой `.on_hover_text(&folder_path)`.

### Баг 2.2 (Скриншоты 2 и 3): Исчезновение кнопок тулбара Pages при сжатии дока
* **Файл:** `crates/ui-egui/src/pages_panel.rs`
* **Симптом:** Когда граница дока сдвигается влево и панель Pages сужается (<200px), кнопки тулбара («Open Chapter», «Add Page», «Grid», «Sort») обрезаются и исчезают за экраном из-за жёсткого `ui.horizontal`.
* **Решение:**
  1. Заменить жёсткий `ui.horizontal(|ui| ...)` на адаптивный `ui.horizontal_wrapped(|ui| ...)`, чтобы кнопки переносились на вторую строку при сужении панели.
  2. Если `ui.available_width() < 180.0`, скрывать длинный текст кнопок и отображать только компактные иконки:
     * `[📂]` (вместо «Open Chapter»)
     * `[➕]` (вместо «Add Page»)
     * `[⊞]` (вместо «Thumbnails»)
     * `[⇅]` (вместо «Natural Sort»)
     * С полным описанием действия в `.on_hover_text(...)`.

### Баг 2.3 (Скриншот 4): Слипание заголовков вкладок дока
* **Файл:** `crates/ui-egui/src/panel_docking.rs` (отрисовка табов дока)
* **Симптом:** При большом количестве открытых панелей (`Properties`, `Character`, `Paragraph`, `Layers`, `History`) заголовки наезжают друг на друга и сливаются в нечитаемый текст (`PropertieCharacteParagraph`).
* **Решение:**
  1. Для плашки вкладки задать максимальную ширину и `clip_rect`.
  2. У правого края заголовка вкладки применить мягкое затухание прозрачности (fade out на 8-12px) вместо наложения символов на соседний таб.
  3. Добавить всплывающий тултип с полным названием вкладки при наведении курсора: `.on_hover_text(tab.title())`.

---

## ШАГ 3. ПРОВЕРКА, ТЕСТЫ И СБОРКА РЕЛИЗА 0.14

1. **Проверка компиляции:**
   ```powershell
   powershell -ExecutionPolicy Bypass -File "F:\fork_home\run-cargo.ps1" check -p ui-egui
   ```
2. **Запуск тестов:**
   ```powershell
   powershell -ExecutionPolicy Bypass -File "F:\fork_home\run-cargo.ps1" test -p engine -p ui-egui
   ```
3. **Сборка релизного бинарника 0.14:**
   ```powershell
   powershell -ExecutionPolicy Bypass -File "F:\fork_home\run-cargo.ps1" build --release -p photocraft
   ```
4. **Упаковка релиза 0.14:**
   Создать каталог `dist/photocraft-v0.14-community/` и скопировать туда `target/release/photocraft.exe`.
