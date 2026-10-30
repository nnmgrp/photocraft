# ТЗ для DeepSeek: Исследование дублирования стилей текста (TypeR vs Character Styles)

> **Тип задачи:** ИСКЛЮЧИТЕЛЬНО АРХИТЕКТУРНОЕ И ИСТОРИЧЕСКОЕ ИССЛЕДОВАНИЕ (Research Only).  
> **ПРАВКИ В КОД НЕ ВНОСИТЬ! КОММИТЫ НЕ СОЗДАВАТЬ!**  
> **Целевой репозиторий:** `F:\Downloads\Telegram Desktop\FORK\photocraft`  
> **Ветка для анализа:** `community/gpu-screentone-preview` (или любая история коммитов в `origin`/`upstream`/`friend`).

---

## 1. Описание проблемы (по скриншоту)

На экране пользователя одновременно открыты два независимых окна управления стилями текста, которые на 90% дублируют функционал друг друга:

1. **Окно «Font Settings: [Имя стиля]» (Модуль TypeR):**
   * Поля: *Style name*, *Folder*, *Font*, *PostScript*, *Style*, *Bold*, *Italic*, *ALL CAPS*, *Color*, *System (Metrics/Optical)*, *Size (pt)*, *Leading*, *Tracking*, *Horizontal scale*, *Outline Thickness & Color*.
   * Хранилище: плоский JSON `typesetter_state.json` / `TypeR_Export.json` (`TextStylePreset` в `crates/engine/src/snow_typesetter.rs`).
2. **Окно «Character Styles / Paragraph Styles / Style Options» (Нативная подсистема PhotoCraft):**
   * Поля: *Name*, *Font*, *Size*, *Leading*, *Tracking*, *Baseline*, *Color*, *Faux Bold*, *Faux Italic*, *Underline*, *Strikethrough*.
   * Хранилище: нативные структуры Photoshop (`crates/engine/src/type_styles_cmds.rs`, `type_styles.rs`, с интеграцией в PSD `EngineData`).
3. **Дополнительно:** Та же типографика дублируется на нижней панели Canvas Capsule и в классических палитрах Character / Paragraph.

**Следствие для пользователя:** Раздвоение интерфейса и путаница. Пользователь манги не понимает: если он создал стиль в TypeR, почему его нет в Character Styles, и наоборот?

---

## 2. Вопросы исследования для DeepSeek

Используя `git log`, `git show`, `git log -S`, `git blame` и поиск по кодовой базе, проведите детальное расследование и составьте структурированный отчёт:

### Вопрос 1: Генеалогия и хронология появления
* В каком коммите (и кем: апстрим `storytold`, ветка `wilwal2020`, форк SnowE22) в PhotoCraft появились родные **Character Styles** и окно **Style Options**?
* В каком коммите и на какой базе в форк был занесён модуль **TypeR** со своими пресетами `TextStylePreset`?
* Были ли на тот момент в PhotoCraft уже готовы нативные Character Styles, или TypeR создавался в вакууме как автономный инструмент из Photoshop-скрипта (JSX)?

### Вопрос 2: Сравнение моделей данных
* Сопоставьте поля структуры `TextStylePreset` (`snow_typesetter.rs`) и структуры `CharacterStyle` / `ParagraphStyle` в нативном движке PhotoCraft (`type_styles.rs` / `type_styles_cmds.rs`).
* Какие поля уникальны для TypeR (например: папки стилей `Folder`, обводка текста `Outline`, `strip_dash_prefix`, `optical_baseline_factor`)?
* Какие поля есть в Character Styles, но отсутствуют в TypeR?
* Как обе системы сохраняются в файлы проекта (`.pcraft` и `.psd`)?

### Вопрос 3: Причины изолированности
* Почему автор TypeR (SnowE22) реализовал собственную систему хранения стилей в отдельном `typesetter_state.json`, вместо того чтобы расширить родные `Character Styles` движка?
* Связано ли это с наследием оригинального скрипта TypeR для Adobe Photoshop (где пресеты читались из внешнего файла настроек)?

### Вопрос 4: Стратегия объединения на будущее (Рекомендации)
* **Вариант А (Адаптер / Подкапотная синхронизация):** Оставить удобный для верстальщика интерфейс палитры TypeR (с папками и быстрыми репликами), но внутри создавать/использовать нативные `Character Styles` документа.
* **Вариант Б (Специализация ролей):** Оставить TypeR как «быстрый буфер набора для манги» (сессионный инструмент), а Character Styles — как документные глобальные стили.
* **Вариант В (Единое модальное окно):** Объединить модалки «Font Settings» и «Style Options» в одну универсальную.

---

## 3. Требования к результату
* Выдать структурированный аналитический отчёт с указанием точных хешей коммитов, путей к файлам и номеров строк.
* Никаких правок в файлы репозитория вносить НЕЛЬЗЯ.
