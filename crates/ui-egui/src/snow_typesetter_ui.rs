use std::hash::{Hash, Hasher};

use egui::{
    Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, ScrollArea, Sense, Stroke,
    Window, vec2,
};
use photocraft_doc::text::{
    AntiAlias, Caps, CharStyle, FontBaseline, ParagraphRun, ParagraphStyle, TextAlign, TextRun,
    TextShape,
};
use photocraft_doc::{Color, TextLayer};
use photocraft_engine::typesetter::{ScriptLine, TextStylePreset, TypesetterState};
use serde::{Deserialize, Serialize};

use crate::theme::Tokens;
use crate::widgets;

#[derive(PartialEq, Clone, Copy, Debug, Serialize, Deserialize)]
pub enum TypesetterDockMode {
    Floating,
    DockRight,
    DockLeft,
    DockBottom,
}

impl Default for TypesetterDockMode {
    fn default() -> Self {
        Self::Floating
    }
}

#[derive(PartialEq, Clone, Copy, Debug, Serialize, Deserialize)]
pub enum TypesetterTab {
    Queue,
    FullText,
    Styles,
    Settings,
}

pub struct TypesetterAction {
    pub text: String,
    pub preset: TextStylePreset,
}

fn default_true() -> bool {
    true
}

fn default_script_list_height() -> f32 {
    160.0
}

/// Point size for style names drawn in their own face on the style cards.
const STYLE_NAME_PT: f32 = 13.0;

/// Extra leading between script rows in the unified editor (logical pixels). egui's multiline
/// `TextEdit` packs rows at the font's own line height, which is too tight for reading a script.
const SCRIPT_LINE_SPACING: f32 = 4.0;

#[derive(Serialize, Deserialize)]
struct TypesetterPersistedState {
    pub dock_mode: TypesetterDockMode,
    pub state: TypesetterState,
    pub selected_folder: Option<String>,
    #[serde(default)]
    pub custom_storage_dir: Option<std::path::PathBuf>,
    #[serde(default)]
    pub favorite_fonts: Vec<String>,
    #[serde(default = "default_true")]
    pub is_open: bool,
    #[serde(default = "default_script_list_height")]
    pub script_list_height: f32,
}

pub fn typesetter_storage_path() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("PHOTOCRAFT_CONFIG_DIR") {
        return Some(std::path::PathBuf::from(dir).join("typesetter_state.json"));
    }
    // Portable mode: check next to executable or portable.txt
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let local = parent.join("typesetter_state.json");
            if local.exists() || parent.join("portable.txt").exists() {
                return Some(local);
            }
        }
    }
    let cwd_local = std::path::PathBuf::from("typesetter_state.json");
    if cwd_local.exists() {
        return Some(cwd_local);
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let p = std::path::PathBuf::from(appdata).join("Photocraft");
            let _ = std::fs::create_dir_all(&p);
            return Some(p.join("typesetter_state.json"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            let p = std::path::PathBuf::from(home).join("Library/Application Support/Photocraft");
            let _ = std::fs::create_dir_all(&p);
            return Some(p.join("typesetter_state.json"));
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            let p = std::path::PathBuf::from(xdg).join("photocraft");
            let _ = std::fs::create_dir_all(&p);
            return Some(p.join("typesetter_state.json"));
        } else if let Some(home) = std::env::var_os("HOME") {
            let p = std::path::PathBuf::from(home).join(".config/photocraft");
            let _ = std::fs::create_dir_all(&p);
            return Some(p.join("typesetter_state.json"));
        }
    }
    None
}

pub struct TypesetterUi {
    pub is_open: bool,
    pub dock_mode: TypesetterDockMode,
    pub current_tab: TypesetterTab,
    pub state: TypesetterState,
    pub script_input_buffer: String,
    pub style_filter: String,
    pub line_filter: String,
    pub full_text_filter: String,
    pub selected_folder: Option<String>,
    pub custom_storage_dir: Option<std::path::PathBuf>,
    pub status_message: Option<String>,
    pub status_error: bool,
    pub filter_current_page_only: bool,
    pub editing_preset_idx: Option<usize>,
    pub duplicating_folder: Option<String>,
    pub duplicate_folder_name_buffer: String,
    pub new_preset_name_buffer: String,
    pub show_full_text_window: bool,
    pub creating_new_folder: bool,
    pub new_folder_name_buffer: String,
    pub show_script_editor_modal: bool,
    pub script_editor_buffer: String,
    pub favorite_fonts: Vec<String>,
    pub rendered_this_frame: bool,
    pub scroll_to_active_line: bool,
    pub script_buffer: String,
    pub script_list_height: f32,
    /// Pass number in which the auxiliary windows were last drawn, so a frame that renders both
    /// the docked body and the floating one does not open them twice.
    aux_pass: u64,
}

impl Default for TypesetterUi {
    fn default() -> Self {
        let mut ui = Self {
            is_open: true,
            dock_mode: TypesetterDockMode::Floating,
            current_tab: TypesetterTab::Queue,
            state: TypesetterState::default(),
            script_input_buffer: String::new(),
            style_filter: String::new(),
            line_filter: String::new(),
            full_text_filter: String::new(),
            selected_folder: None,
            custom_storage_dir: None,
            status_message: None,
            status_error: false,
            filter_current_page_only: true,
            editing_preset_idx: None,
            duplicating_folder: None,
            duplicate_folder_name_buffer: String::new(),
            new_preset_name_buffer: String::new(),
            show_full_text_window: false,
            creating_new_folder: false,
            new_folder_name_buffer: String::new(),
            show_script_editor_modal: false,
            script_editor_buffer: String::new(),
            favorite_fonts: Vec::new(),
            rendered_this_frame: false,
            scroll_to_active_line: false,
            script_buffer: String::new(),
            script_list_height: 160.0,
            aux_pass: u64::MAX,
        };
        ui.auto_load();
        ui
    }
}

fn draw_dashed_hline(painter: &egui::Painter, x_start: f32, x_end: f32, y: f32, stroke: Stroke) {
    let mut x = x_start;
    let dash_len = 3.0;
    let gap_len = 3.0;
    while x < x_end {
        let x2 = (x + dash_len).min(x_end);
        painter.line_segment([egui::pos2(x, y), egui::pos2(x2, y)], stroke);
        x += dash_len + gap_len;
    }
}

impl TypesetterUi {
    /// Синхронизирует буфер единого текстового редактора из массива строк реплик.
    pub fn sync_script_buffer_from_state(&mut self) {
        if !self.state.lines.is_empty() {
            self.script_buffer = self.state.lines.iter().map(|l| l.raw.as_str()).collect::<Vec<_>>().join("\n");
        }
    }

    /// Синхронизирует массив строк реплик и метаданных при редактировании единого текстового буфера.
    pub fn sync_state_from_script_buffer(&mut self) {
        let lines: Vec<&str> = self.script_buffer.split('\n').collect();
        let mut new_lines = Vec::with_capacity(lines.len());
        let mut page = 1;
        for (i, line_str) in lines.iter().enumerate() {
            let trimmed = line_str.trim();
            if let Some(p) = photocraft_engine::typesetter::detect_page_header(trimmed) {
                page = p;
            }
            let cleaned = photocraft_engine::typesetter::clean_script_line_with_options(trimmed, &self.state.options);
            let was_applied = self.state.lines.get(i).map(|l| l.is_applied).unwrap_or(false);
            new_lines.push(ScriptLine {
                raw: line_str.to_string(),
                text: if cleaned.is_empty() { line_str.to_string() } else { cleaned },
                is_applied: was_applied,
                page,
            });
        }
        self.state.lines = new_lines;
        if self.state.current_line_idx >= self.state.lines.len() {
            self.state.current_line_idx = self.state.lines.len().saturating_sub(1);
        }
        self.auto_save();
    }
    pub fn new() -> Self {
        Self::default()
    }

    /// Вставить сценарий напрямую из буфера обмена.
    pub fn paste_script_from_clipboard(&mut self) {
        let text_opt = arboard::Clipboard::new().ok().and_then(|mut cb| cb.get_text().ok());
        if let Some(text) = text_opt {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                self.script_input_buffer = text.clone();
                self.script_editor_buffer = text.clone();
                self.state.load_script(&text);
                self.sync_script_buffer_from_state();
                self.auto_save();
                self.status_message = Some(format!("Загружено реплик из буфера: {}", self.state.lines.len()));
                self.status_error = false;
                return;
            }
        }
        // Если буфер пуст или недоступен — открываем окно редактора для ручного ввода
        self.open_script_editor();
        self.status_message = Some("Буфер пуст. Вставьте текст вручную в окно редактора".into());
        self.status_error = false;
    }

    /// Открыть модальное окно редактора сценария.
    pub fn open_script_editor(&mut self) {
        if self.script_editor_buffer.trim().is_empty() && !self.state.lines.is_empty() {
            // Если буфер редактора пуст, подставляем текущие реплики
            self.script_editor_buffer = self.state.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n");
        }
        self.show_script_editor_modal = true;
    }

    /// Диалог выбора .txt файла сценария.
    pub fn open_script_file_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Текстовые файлы (*.txt)", &["txt"])
            .set_title("Выберите файл сценария")
            .pick_file()
        {
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    self.script_input_buffer = content.clone();
                    self.script_editor_buffer = content.clone();
                    self.state.load_script(&content);
                    self.sync_script_buffer_from_state();
                    self.auto_save();
                    self.status_message = Some(format!("Загружено реплик из файла: {}", self.state.lines.len()));
                    self.status_error = false;
                }
                Err(err) => {
                    self.status_message = Some(format!("Ошибка чтения файла: {}", err));
                    self.status_error = true;
                }
            }
        }
    }

    /// Текущий эффективный путь к файлу пресетов (пользовательская папка или системная).
    pub fn storage_path(&self) -> Option<std::path::PathBuf> {
        if let Some(custom_dir) = &self.custom_storage_dir {
            return Some(custom_dir.join("typesetter_state.json"));
        }
        typesetter_storage_path()
    }

    /// Автоматическое сохранение состояния и пресетов в файл настроек.
    pub fn auto_save(&self) {
        let persisted = TypesetterPersistedState {
            dock_mode: self.dock_mode,
            state: self.state.clone(),
            selected_folder: self.selected_folder.clone(),
            custom_storage_dir: self.custom_storage_dir.clone(),
            favorite_fonts: self.favorite_fonts.clone(),
            is_open: self.is_open,
            script_list_height: self.script_list_height,
        };
        if let Ok(json_str) = serde_json::to_string_pretty(&persisted) {
            if let Some(custom_dir) = &self.custom_storage_dir {
                let _ = std::fs::create_dir_all(custom_dir);
                let _ = std::fs::write(custom_dir.join("typesetter_state.json"), &json_str);
            }
            if let Some(default_path) = typesetter_storage_path() {
                let _ = std::fs::write(default_path, &json_str);
            }
        }
    }

    /// Автоматическая загрузка сохраненных пресетов и состояния.
    pub fn auto_load(&mut self) {
        if let Some(path) = typesetter_storage_path() {
            if path.exists() {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(data) = serde_json::from_str::<TypesetterPersistedState>(&content) {
                        self.dock_mode = data.dock_mode;
                        self.state = data.state;
                        self.selected_folder = data.selected_folder;
                        if !data.favorite_fonts.is_empty() {
                            self.favorite_fonts = data.favorite_fonts;
                        }
                        self.custom_storage_dir = data.custom_storage_dir;
                        self.is_open = data.is_open;
                        self.script_list_height = data.script_list_height;
                        self.sync_script_buffer_from_state();
                    } else if let Ok(count) = self.state.import_styles_json(&content) {
                        if count > 0 {
                            return;
                        }
                    }
                }
            }
        }

        if let Some(custom_dir) = &self.custom_storage_dir {
            let custom_path = custom_dir.join("typesetter_state.json");
            if custom_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&custom_path) {
                    if let Ok(data) = serde_json::from_str::<TypesetterPersistedState>(&content) {
                        self.dock_mode = data.dock_mode;
                        self.state = data.state;
                        self.selected_folder = data.selected_folder;
                        if !data.favorite_fonts.is_empty() {
                            self.favorite_fonts = data.favorite_fonts;
                        }
                        self.is_open = data.is_open;
                        if !self.state.presets.is_empty() {
                            return;
                        }
                    }
                }
            }
        }

        // Если файла еще нет, проверяем TypeR_Export.json рядом с exe или в текущей папке
        let export_candidates = [
            std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("TypeR_Export.json"))),
            Some(std::path::PathBuf::from("TypeR_Export.json")),
        ];
        for cand in export_candidates.into_iter().flatten() {
            if cand.exists() {
                if let Ok(content) = std::fs::read_to_string(&cand) {
                    if let Ok(count) = self.state.import_styles_json(&content) {
                        if count > 0 {
                            self.auto_save();
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Отрисовка TypeR при встраивании в контейнер дока (левый, правый или нижний).
    pub fn render_embedded(&mut self, ui: &mut egui::Ui) -> Option<TypesetterAction> {
        self.rendered_this_frame = true;
        let mut action = None;
        egui::Frame::NONE
            .inner_margin(egui::Margin { left: 6, right: 6, top: 4, bottom: 4 })
            .show(ui, |ui| {
                self.render_header_toolbar(ui);
                self.render_body(ui, &mut action);
            });
        // Придоканная панель тоже должна открывать окна стиля/сценария; в плавающем режиме их
        // рисует `show`, а счётчик pass не даст открыть их дважды за кадр.
        let ctx = ui.ctx().clone();
        self.render_aux_windows(&ctx, &mut action);
        action
    }

    /// Основная точка отрисовки UI (поддержка свободного окна и плавающего режима).
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<TypesetterAction> {
        if !self.is_open {
            self.rendered_this_frame = false;
            return None;
        }

        let mut action = None;
        let ctx = ui.ctx().clone();
        self.rendered_this_frame = false;

        self.render_aux_windows(&ctx, &mut action);

        action
    }

    /// Окна-спутники TypeR: полный текст, настройки стиля и редактор сценария.
    ///
    /// Рисуются и в плавающем (`show`), и в придоканном (`render_embedded`) режиме: окна egui
    /// живут на контексте, поэтому вызов один на кадр — иначе панель, которую в доке рисуют и
    /// `show`, и док, создала бы каждое окно дважды.
    fn render_aux_windows(&mut self, ctx: &egui::Context, action: &mut Option<TypesetterAction>) {
        let pass = ctx.cumulative_pass_nr();
        if self.aux_pass == pass {
            return;
        }
        self.aux_pass = pass;
        if self.show_full_text_window {
            let mut show_win = self.show_full_text_window;
            Window::new(tl!("Manga Script (Full Text)"))
                .open(&mut show_win)
                .default_width(450.0)
                .default_height(550.0)
                .min_width(320.0)
                .min_height(300.0)
                .resizable(true)
                .show(ctx, |ui| {
                    self.render_full_text_content(ui, &mut *action);
                });
            self.show_full_text_window = show_win;
        }

        // Модальное окно настроек конкретного шрифта / стиля
        if let Some(edit_idx) = self.editing_preset_idx {
            let mut close_modal = false;
            let title = if let Some(preset) = self.state.presets.get(edit_idx) {
                format!("{}: {}", tl!("Font Settings"), preset.name)
            } else {
                tl!("Font Settings").to_string()
            };

            // Окно должно свободно перетаскиваться: `.anchor()` прибивал его к центру каждый
            // кадр, из-за чего заголовок не тянулся. Вместо этого центрируем только стартовую
            // позицию, а дальнейшие — запоминает egui по стабильному id (иначе id зависел от
            // имени стиля и позиция сбрасывалась при смене стиля).
            let mut window_open = true;
            Window::new(title)
                .id(egui::Id::new("typer_font_settings_window"))
                .open(&mut window_open)
                .collapsible(false)
                // Photoshop's panel keeps a fixed width; only the height follows the content.
                .resizable([false, true])
                .default_width(585.0)
                .min_width(585.0)
                .max_width(585.0)
                .default_height(685.0)
                .min_height(685.0)
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ctx.content_rect().center())
                .show(ctx, |ui| {
                    // The mockup's window keeps a fixed 585-point width, scrollbar or not. egui
                    // adds a floating scrollbar's allocated width to the scroll area's outer size
                    // as soon as the content fills the width, so any reservation here would make
                    // the window 2 points wider than the mockup. A floating bar needs no space:
                    // it is drawn over the panel's edge, like in Photoshop.
                    ui.spacing_mut().scroll.floating_allocated_width = 0.0;
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            self.render_font_settings_modal(ui, edit_idx, &mut close_modal);
                        });
                });

            if !window_open {
                let pending =
                    ctx.data_mut(|d| d.remove_temp::<PreviewEdit>(egui::Id::new("typesetter-preview-edit")));
                if let Some(pending) = pending {
                    if pending.idx == edit_idx {
                        if let Some(preset) = self.state.presets.get_mut(edit_idx) {
                            commit_preview_edit(preset, &pending.buffer, &pending.original);
                        }
                    }
                }
                close_modal = true;
            }
            if close_modal {
                self.editing_preset_idx = None;
                self.auto_save();
            }
        }

        // Модальное окно ввода и редактирования текста сценария
        if self.show_script_editor_modal {
            let mut close_modal = false;
            Window::new(tl!("Script Editor"))
                .id(egui::Id::new("typer_script_editor_window"))
                .collapsible(false)
                .resizable(true)
                .default_width(520.0)
                .default_height(420.0)
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ctx.content_rect().center())
                .show(ctx, |ui| {
                    self.render_script_editor_modal(ui, &mut close_modal);
                });

            if close_modal {
                self.show_script_editor_modal = false;
            }
        }
    }

    /// Верхняя панель вкладок и меню опций/импорта/экспорта в едином стиле Photoshop.
    fn render_header_toolbar(&mut self, ui: &mut egui::Ui) {
        let t = Tokens::get(ui.ctx());

        ui.horizontal(|ui| {
            for (tab_val, label_key) in [
                (TypesetterTab::Queue, "Typesetter"),
                (TypesetterTab::FullText, "Script"),
                (TypesetterTab::Settings, "Options"),
            ] {
                let label = tl!(label_key);
                let is_sel = self.current_tab == tab_val;
                if widgets::pill_tab(ui, label, is_sel).clicked() {
                    self.current_tab = tab_val;
                }
            }

            // Меню панели и переключатель страниц главы
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.menu_button("···", |ui| {
                    widgets::style_spectrum_popup_menu(ui);

                    ui.label(RichText::new(tl!("Manga Workflow")).size(11.0).color(t.text_dim).strong());
                    if ui.button(tl!("Chapter Pages…")).clicked() {
                        ui.close();
                        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("photocraft_toggle_pages_cmd"), true));
                    }
                    if ui.button(tl!("Split Left Dock (Pages + TypeR)…")).clicked() {
                        ui.close();
                        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("photocraft_toggle_split_dock_cmd"), true));
                    }

                    ui.separator();
                    ui.label(RichText::new(tl!("Script")).size(11.0).color(t.text_dim).strong());

                    if ui.button(tl!("Paste / Edit script…")).clicked() {
                        ui.close();
                        self.open_script_editor();
                    }

                    if ui.button(tl!("Load script file (.txt)…")).clicked() {
                        ui.close();
                        self.open_script_file_dialog();
                    }

                    if !self.state.lines.is_empty() {
                        if ui.button(tl!("Clear script")).clicked() {
                            ui.close();
                            self.state.lines.clear();
                            self.state.current_line_idx = 0;
                            self.auto_save();
                        }
                    }

                    ui.separator();
                    ui.label(RichText::new(tl!("TypeR Styles")).size(11.0).color(t.text_dim).strong());

                    if ui.button(tl!("Import TypeR JSON…")).clicked() {
                        ui.close();
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("TypeR JSON (*.json)", &["json"])
                            .set_title(tl!("Choose TypeR styles file"))
                            .pick_file()
                        {
                            match std::fs::read_to_string(&path) {
                                Ok(content) => match self.state.import_styles_json(&content) {
                                    Ok(count) => {
                                        self.status_message = Some(format!("{}: {}", tl!("Loaded styles"), count));
                                        self.status_error = false;
                                        self.auto_save();
                                    }
                                    Err(err) => {
                                        self.status_message = Some(format!("{}: {}", tl!("Import error"), err));
                                        self.status_error = true;
                                    }
                                },
                                Err(err) => {
                                    self.status_message = Some(format!("{}: {}", tl!("Read error"), err));
                                    self.status_error = true;
                                }
                            }
                        }
                    }

                    if ui.button(tl!("Export TypeR JSON…")).clicked() {
                        ui.close();
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("TypeR JSON (*.json)", &["json"])
                            .set_title(tl!("Export TypeR styles"))
                            .save_file()
                        {
                            match self.state.export_styles_json(self.selected_folder.as_deref()) {
                                Ok(json_content) => match std::fs::write(&path, json_content) {
                                    Ok(_) => {
                                        self.status_message = Some(tl!("Styles successfully exported").into());
                                        self.status_error = false;
                                    }
                                    Err(err) => {
                                        self.status_message = Some(format!("{}: {}", tl!("Write error"), err));
                                        self.status_error = true;
                                    }
                                },
                                Err(err) => {
                                    self.status_message = Some(format!("{}: {}", tl!("Export error"), err));
                                    self.status_error = true;
                                }
                            }
                        }
                    }
                });

                if ui.button(RichText::new("📄").size(11.0))
                    .on_hover_text(tl!("Toggle Chapter Pages panel"))
                    .clicked()
                {
                    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("photocraft_toggle_pages_cmd"), true));
                }
            });
        });

        // Статусное сообщение
        if let Some(msg) = &self.status_message {
            let color = if self.status_error {
                t.danger
            } else {
                t.accent
            };
            ui.add_space(2.0);
            ui.label(RichText::new(msg).color(color).small());
        }

        ui.add_space(2.0);
    }

    /// Тело активной вкладки тайпсеттера.
    fn render_body(&mut self, ui: &mut egui::Ui, action: &mut Option<TypesetterAction>) {
        match self.current_tab {
            TypesetterTab::Queue | TypesetterTab::Styles => self.render_unified_typer_workspace(ui, action),
            TypesetterTab::FullText => self.render_full_text_content(ui, action),
            TypesetterTab::Settings => self.render_settings_tab(ui),
        }
    }

    /// Единая рабочая область TypeR: адаптивный выбор между горизонтальной
    /// раскладкой (для Нижнего дока) и вертикальной раскладкой (для Левого/Правого доков).
    fn render_unified_typer_workspace(&mut self, ui: &mut egui::Ui, action: &mut Option<TypesetterAction>) {
        if ui.available_height() <= 280.0 && ui.available_width() >= 450.0 {
            self.render_horizontal_typer_workspace(ui, action);
        } else {
            self.render_vertical_typer_workspace(ui, action);
        }
    }

    /// Вертикальная раскладка TypeR (для боковых доков и плавающего окна).
    fn render_vertical_typer_workspace(&mut self, ui: &mut egui::Ui, action: &mut Option<TypesetterAction>) {
        let t = Tokens::get(ui.ctx());
        let total_lines = self.state.lines.len();
        let cur_idx = self.state.current_line_idx;

        // --- 1. ВЕРХНИЙ БЛОК: СТРЕЛКИ ▲/▼, СТАТУС, ВСТАВКА (➔), ТЕКУЩАЯ СТРОКА ---
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                let up_btn = egui::Button::new("▲").min_size(vec2(22.0, 16.0));
                if ui.add(up_btn).on_hover_text(tl!("Previous line (↑)")).clicked() {
                    self.state.prev_line();
                    self.scroll_to_active_line = true;
                }
                let down_btn = egui::Button::new("▼").min_size(vec2(22.0, 16.0));
                if ui.add(down_btn).on_hover_text(tl!("Next line (↓)")).clicked() {
                    self.state.advance_line();
                    self.scroll_to_active_line = true;
                }
            });

            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;

                let line_num_str = if total_lines == 0 { 0 } else { cur_idx + 1 };
                let style_name = self.state.active_preset().map(|p| p.name.clone()).unwrap_or_else(|| tl!("Default").into());
                let font_size = self.state.active_preset().map(|p| p.font_size.round() as i32).unwrap_or(12);

                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "{}: {}, {}: {}, {}: {}pt",
                            tl!("line"), line_num_str, tl!("style"), style_name, tl!("size"), font_size
                        ))
                        .color(t.text_dim)
                        .small(),
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let insert_btn = egui::Button::new(
                            RichText::new("▶")
                                .color(t.accent_text)
                                .strong(),
                        )
                        .fill(t.accent)
                        .min_size(vec2(26.0, 18.0));

                        if ui.add(insert_btn).on_hover_text(tl!("Insert current line into selection/bubble (Enter)")).clicked() {
                            if let Some(line) = self.state.active_line() {
                                if let Some(preset) = self.state.active_preset().cloned() {
                                    *action = Some(TypesetterAction {
                                        text: line.text.clone(),
                                        preset,
                                    });
                                    self.state.advance_line();
                                    self.scroll_to_active_line = true;
                                    self.auto_save();
                                }
                            }
                        }
                    });
                });

                let current_text = self.state.active_line().map(|l| l.text.as_str()).unwrap_or("");
                Frame::NONE
                    .fill(Color32::WHITE)
                    .stroke(Stroke::new(1.0, Color32::from_rgb(170, 175, 185)))
                    .corner_radius(CornerRadius::same(3))
                    .inner_margin(Margin::symmetric(6, 4))
                    .show(ui, |ui| {
                        let display = if current_text.is_empty() {
                            if total_lines == 0 {
                                tl!("(no script loaded — see 'Script' tab)")
                            } else {
                                tl!("(end of script)")
                            }
                        } else {
                            current_text
                        };

                        let text_color = if current_text.is_empty() {
                            Color32::from_rgb(140, 145, 155)
                        } else {
                            Color32::from_rgb(20, 25, 35)
                        };

                        ui.add(
                            egui::Label::new(
                                RichText::new(display)
                                    .color(text_color)
                                    .size(12.0)
                                    .strong(),
                            )
                            .wrap_mode(egui::TextWrapMode::Wrap),
                        );
                    });
            });
        });

        widgets::hairline(ui);
        ui.add_space(2.0);

        ui.horizontal(|ui| {
            let is_narrow = ui.available_width() < 300.0;
            ui.label(RichText::new(format!("{}:", tl!("Script"))).color(t.text_dim).small().strong());

            let paste_text = if is_narrow { "📋".to_string() } else { format!("📋 {}", tl!("Paste")) };
            if ui.small_button(paste_text).on_hover_text(tl!("Paste script text from clipboard (Ctrl+V)")).clicked() {
                self.paste_script_from_clipboard();
            }

            let edit_text = if is_narrow { "📝".to_string() } else { format!("📝 {}", tl!("Editor")) };
            if ui.small_button(edit_text).on_hover_text(tl!("Open script text editor")).clicked() {
                self.open_script_editor();
            }

            if ui.small_button("📥 .txt").on_hover_text(tl!("Load script file (.txt)")).clicked() {
                self.open_script_file_dialog();
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if total_lines > 0 {
                    ui.label(
                        RichText::new(format!("{}/{}", if total_lines == 0 { 0 } else { cur_idx + 1 }, total_lines))
                            .color(t.text_dim)
                            .small(),
                    );
                }
            });
        });
        ui.add_space(2.0);

        let script_list_height = self.script_list_height;
        self.render_script_list(ui, action, script_list_height);

        // Splitter / ползунок изменения высоты окна сценария
        let (res_rect, res_resp) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), Sense::drag());
        let is_res_hover = res_resp.hovered() || res_resp.dragged();
        if is_res_hover {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if res_resp.dragged() {
            self.script_list_height = (self.script_list_height + res_resp.drag_delta().y).clamp(80.0, 600.0);
            self.auto_save();
        }
        let splitter_line_col = if is_res_hover {
            t.accent
        } else {
            Color32::from_rgb(205, 210, 220)
        };
        ui.painter().hline(res_rect.x_range(), res_rect.center().y, Stroke::new(1.0, splitter_line_col));
        let cx = res_rect.center().x;
        let cy = res_rect.center().y;
        for offset in [-12.0, -6.0, 0.0, 6.0, 12.0] {
            ui.painter().circle_filled(egui::pos2(cx + offset, cy), 1.2, splitter_line_col);
        }
        ui.add_space(2.0);

        self.render_preset_toolbar(ui);

        let num_cols = if ui.available_width() < 300.0 { 1 } else { 2 };
        self.render_style_presets_grid(ui, action, num_cols);
    }

    /// Горизонтальная раскладка TypeR (специально для Нижнего дока).
    fn render_horizontal_typer_workspace(&mut self, ui: &mut egui::Ui, action: &mut Option<TypesetterAction>) {
        let t = Tokens::get(ui.ctx());
        let total_lines = self.state.lines.len();
        let cur_idx = self.state.current_line_idx;

        ui.horizontal(|ui| {
            // --- 1. Левая колонка: Сценарий (список реплик) ---
            let left_w = 250.0f32.min(ui.available_width() * 0.32);
            ui.allocate_ui_with_layout(vec2(left_w, ui.available_height()), Layout::top_down(Align::Min), |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{}:", tl!("Script"))).color(t.text_dim).small().strong());
                    if ui.small_button("📋").on_hover_text(tl!("Paste script from clipboard (Ctrl+V)")).clicked() {
                        self.paste_script_from_clipboard();
                    }
                    if ui.small_button("📝").on_hover_text(tl!("Open script text editor")).clicked() {
                        self.open_script_editor();
                    }
                    if ui.small_button("📥").on_hover_text(tl!("Load script file (.txt)")).clicked() {
                        self.open_script_file_dialog();
                    }
                    if total_lines > 0 {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(RichText::new(format!("{}/{}", cur_idx + 1, total_lines)).color(t.text_dim).small());
                        });
                    }
                });

                let list_h = (ui.available_height() - 4.0).max(60.0);
                self.render_script_list(ui, action, list_h);
            });

            ui.separator();

            // --- 2. Средняя колонка: Текущая реплика и быстрая вставка ---
            let mid_w = 230.0f32.min(ui.available_width() * 0.35);
            ui.allocate_ui_with_layout(vec2(mid_w, ui.available_height()), Layout::top_down(Align::Min), |ui| {
                ui.horizontal(|ui| {
                    let up_btn = egui::Button::new("▲").min_size(vec2(22.0, 16.0));
                    if ui.add(up_btn).on_hover_text(tl!("Previous line (↑)")).clicked() {
                        self.state.prev_line();
                        self.scroll_to_active_line = true;
                    }
                    let down_btn = egui::Button::new("▼").min_size(vec2(22.0, 16.0));
                    if ui.add(down_btn).on_hover_text(tl!("Next line (↓)")).clicked() {
                        self.state.advance_line();
                        self.scroll_to_active_line = true;
                    }

                    let line_num_str = if total_lines == 0 { 0 } else { cur_idx + 1 };
                    let style_name = self.state.active_preset().map(|p| p.name.clone()).unwrap_or_else(|| tl!("Default").into());
                    let font_size = self.state.active_preset().map(|p| p.font_size.round() as i32).unwrap_or(12);

                    ui.label(
                        RichText::new(format!("{}: {} ({}pt)", line_num_str, style_name, font_size))
                            .color(t.text_dim)
                            .small(),
                    );
                });

                let current_text = self.state.active_line().map(|l| l.text.as_str()).unwrap_or("");
                let box_h = (ui.available_height() - 32.0).max(40.0);
                Frame::NONE
                    .fill(Color32::WHITE)
                    .stroke(Stroke::new(1.0, Color32::from_rgb(170, 175, 185)))
                    .corner_radius(CornerRadius::same(3))
                    .inner_margin(Margin::symmetric(6, 4))
                    .show(ui, |ui| {
                        let display = if current_text.is_empty() {
                            if total_lines == 0 { "(сценарий не загружен)" } else { tl!("(end of script)") }
                        } else {
                            current_text
                        };
                        let text_color = if current_text.is_empty() {
                            Color32::from_rgb(140, 145, 155)
                        } else {
                            Color32::from_rgb(20, 25, 35)
                        };
                        ScrollArea::vertical()
                            .id_salt("horizontal_current_text_scroll")
                            .max_height(box_h)
                            .show(ui, |ui| {
                                ui.add(egui::Label::new(RichText::new(display).color(text_color).size(12.0).strong()).wrap_mode(egui::TextWrapMode::Wrap));
                            });
                    });

                let insert_btn = egui::Button::new(RichText::new("▶ Вставить (Enter)").color(t.accent_text).strong())
                    .fill(t.accent)
                    .min_size(vec2(ui.available_width(), 24.0));
                if ui.add(insert_btn).clicked() {
                    if let Some(line) = self.state.active_line() {
                        if let Some(preset) = self.state.active_preset().cloned() {
                            *action = Some(TypesetterAction {
                                text: line.text.clone(),
                                preset,
                            });
                            self.state.advance_line();
                            self.scroll_to_active_line = true;
                            self.auto_save();
                        }
                    }
                }
            });

            ui.separator();

            // --- 3. Правая колонка: Стили и пресеты ---
            ui.allocate_ui_with_layout(vec2(ui.available_width(), ui.available_height()), Layout::top_down(Align::Min), |ui| {
                self.render_preset_toolbar(ui);
                let num_cols = ((ui.available_width() / 150.0).floor() as usize).max(1);
                self.render_style_presets_grid(ui, action, num_cols);
            });
        });
    }

    /// Тулбар управления стилями (+ Стиль, + Папка, Поиск).
    fn render_preset_toolbar(&mut self, ui: &mut egui::Ui) {
        let t = Tokens::get(ui.ctx());
        ui.horizontal(|ui| {
            if ui.small_button(tl!("+ Style")).on_hover_text("Создать новый стиль").clicked() {
                let new_preset = TextStylePreset {
                    name: tl!("New Preset").to_string(),
                    ..TextStylePreset::default()
                };
                self.state.presets.push(new_preset);
                self.editing_preset_idx = Some(self.state.presets.len() - 1);
                self.auto_save();
            }

            if ui.small_button(tl!("+ Folder")).on_hover_text("Создать новую папку стилей").clicked() {
                self.creating_new_folder = true;
                self.new_folder_name_buffer = tl!("New Folder").into();
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.style_filter).hint_text("🔍").desired_width(70.0));
            });
        });

        if self.creating_new_folder {
            Frame::NONE
                .fill(t.field)
                .stroke(Stroke::new(1.0, t.field_border))
                .corner_radius(CornerRadius::same(t.radius_sm as u8))
                .inner_margin(Margin::same(6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{}:", tl!("Folder name"))).color(t.text).small());
                        ui.text_edit_singleline(&mut self.new_folder_name_buffer);
                        if ui.button("OK").clicked() {
                            let name = self.new_folder_name_buffer.trim().to_string();
                            if !name.is_empty() {
                                self.selected_folder = Some(name);
                            }
                            self.creating_new_folder = false;
                        }
                        if ui.button(tl!("Cancel")).clicked() {
                            self.creating_new_folder = false;
                        }
                    });
                });
        }

        if let Some(src_folder) = self.duplicating_folder.clone() {
            Frame::NONE
                .fill(t.field)
                .stroke(Stroke::new(1.0, t.field_border))
                .corner_radius(CornerRadius::same(t.radius_sm as u8))
                .inner_margin(Margin::same(6))
                .show(ui, |ui| {
                    ui.label(RichText::new(format!("{}: '{}'", tl!("Duplicate folder"), src_folder)).color(t.text).strong().small());
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.duplicate_folder_name_buffer);
                        if ui.button("OK").clicked() {
                            let count = self.state.duplicate_folder(&src_folder, &self.duplicate_folder_name_buffer);
                            self.status_message = Some(format!("{}: {}", tl!("Folder duplicated"), count));
                            self.duplicating_folder = None;
                            self.auto_save();
                        }
                        if ui.button(tl!("Cancel")).clicked() {
                            self.duplicating_folder = None;
                        }
                    });
                });
        }
    }

    /// Полноценный единый текстовый редактор сценария TypeR с гуттерами строк,
    /// сквозным выделением текста мышью, свободным слиянием (Backspace) и переносом (Enter).
    fn render_script_list(&mut self, ui: &mut egui::Ui, action: &mut Option<TypesetterAction>, max_height: f32) {
        if self.script_buffer.is_empty() && !self.state.lines.is_empty() {
            self.sync_script_buffer_from_state();
        }

        let gutter_w = 46.0f32;
        let action_w = 24.0f32;
        let edit_id = egui::Id::new("typer_unified_script_text_edit");
        let active_line_idx = self.state.current_line_idx;
        let mut do_scroll_active = self.scroll_to_active_line;

        // Если буфер пуст и реплик нет — показываем плашку быстрого старта с кнопками
        if self.script_buffer.trim().is_empty() && self.state.lines.is_empty() {
            Frame::NONE
                .fill(Color32::WHITE)
                .stroke(Stroke::new(1.0, Color32::from_rgb(180, 185, 195)))
                .corner_radius(CornerRadius::same(2))
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(8.0);
                        ui.label(RichText::new(tl!("No script loaded")).color(Color32::from_rgb(120, 125, 135)).small());
                        ui.add_space(6.0);
                        ui.horizontal_wrapped(|ui| {
                            if ui.button(RichText::new(format!("📋 {}", tl!("Paste"))).strong()).on_hover_text(tl!("Paste script from clipboard (Ctrl+V)")).clicked() {
                                self.paste_script_from_clipboard();
                            }
                            if ui.button(format!("📥 {}", tl!("Load .txt"))).on_hover_text(tl!("Load script file (.txt)")).clicked() {
                                self.open_script_file_dialog();
                            }
                            if ui.button(format!("📝 {}", tl!("Editor"))).on_hover_text(tl!("Enter script manually")).clicked() {
                                self.open_script_editor();
                            }
                        });
                        ui.add_space(8.0);
                    });
                });
            return;
        }

        ScrollArea::vertical()
            .id_salt("typer_script_unified_scroll")
            .max_height(max_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let avail_w = ui.available_width().max(180.0);
                // Строки сценария читаются слишком плотно при стандартном line-height egui:
                // добавляем межстрочный интервал, который TextEdit учитывает при раскладке.
                ui.spacing_mut().extra_text_line_spacing = SCRIPT_LINE_SPACING;
                // Высота редактора должна следовать за ползунком-сплиттером: без desired_rows
                // TextEdit всегда занимает 4 строки, и изменение script_list_height не видно.
                let row_h = (ui.text_style_height(&egui::TextStyle::Body) + SCRIPT_LINE_SPACING).max(1.0);
                let desired_rows = (((max_height - 8.0) / row_h).floor() as usize).max(1);
                let edit = egui::TextEdit::multiline(&mut self.script_buffer)
                    .id(edit_id)
                    // Рабочий текст сценария читается пропорциональным шрифтом, а не моноширинным.
                    .font(egui::TextStyle::Body)
                    .text_color(Color32::from_rgb(20, 25, 35))
                    // Frame::NONE заменяет margin целиком, поэтому отступы задаём во фрейме:
                    // иначе текст начинается под левым гуттером и первые символы строки скрыты.
                    .frame(egui::Frame::NONE.inner_margin(egui::Margin {
                        left: gutter_w as i8,
                        right: action_w as i8,
                        top: 4,
                        bottom: 4,
                    }))
                    .desired_rows(desired_rows)
                    .desired_width(avail_w);

                // Резервируем форму подложки до отрисовки TextEdit: иначе белая заливка,
                // добавленная после текста, перекрыла бы его в том же слое painter.
                let background_idx = ui.painter().add(egui::Shape::Noop);
                let output = edit.show(ui);
                let rect = output.response.rect;

                // 1. Отрисовка подложки и вертикальных границ колонок
                ui.painter().set(background_idx, egui::Shape::rect_filled(rect, CornerRadius::same(2), Color32::WHITE));

                // Левый гуттер (#e4e6eb)
                let left_gutter_rect = egui::Rect::from_min_max(rect.min, egui::pos2(rect.left() + gutter_w, rect.bottom()));
                ui.painter().rect_filled(left_gutter_rect, CornerRadius { nw: 2, sw: 2, ne: 0, se: 0 }, Color32::from_rgb(228, 230, 235));
                ui.painter().vline(rect.left() + gutter_w, rect.top()..=rect.bottom(), Stroke::new(1.0, Color32::from_rgb(205, 208, 215)));

                // Правый гуттер (#fafafc)
                let right_gutter_rect = egui::Rect::from_min_max(egui::pos2(rect.right() - action_w, rect.top()), rect.max);
                ui.painter().rect_filled(right_gutter_rect, CornerRadius { ne: 2, se: 2, nw: 0, sw: 0 }, Color32::from_rgb(250, 250, 252));
                ui.painter().vline(rect.right() - action_w, rect.top()..=rect.bottom(), Stroke::new(1.0, Color32::from_rgb(225, 228, 235)));

                // Внешняя аккуратная рамка
                ui.painter().rect_stroke(rect, CornerRadius::same(2), Stroke::new(1.0, Color32::from_rgb(180, 185, 195)), egui::StrokeKind::Inside);

                // 2. Расчет геометрии логических строк по строкам галлеи (PlacedRow)
                let mut logical_line_idx = 0;
                let mut line_start_y = 0.0f32;
                let mut line_first_row_y = 0.0f32;
                let mut line_first_row_h = 16.0f32;
                let mut line_has_start = false;

                let rows_count = output.galley.rows.len();
                let mut logical_lines_geom = Vec::new();

                for (r_idx, placed_row) in output.galley.rows.iter().enumerate() {
                    let row_y = placed_row.pos.y;
                    let row_h = placed_row.row.size.y;
                    if !line_has_start {
                        line_start_y = row_y;
                        line_first_row_y = row_y;
                        line_first_row_h = row_h;
                        line_has_start = true;
                    }

                    if placed_row.ends_with_newline || r_idx + 1 == rows_count {
                        let line_end_y = row_y + row_h;
                        logical_lines_geom.push((
                            logical_line_idx,
                            line_start_y,
                            line_end_y,
                            line_first_row_y,
                            line_first_row_h,
                        ));
                        logical_line_idx += 1;
                        line_has_start = false;
                    }
                }

                let mouse_pos = ui.input(|i| i.pointer.hover_pos());
                let primary_clicked = ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary));
                let mut clicked_target_line = None;
                let mut clicked_action_line = None;

                for (line_idx, start_y, end_y, first_row_y, first_row_h) in logical_lines_geom {
                    let screen_start_y = output.galley_pos.y + start_y;
                    let screen_end_y = output.galley_pos.y + end_y;
                    let screen_center_y = output.galley_pos.y + first_row_y + first_row_h * 0.5;

                    let is_active = line_idx == active_line_idx;
                    let is_applied = self.state.lines.get(line_idx).map(|l| l.is_applied).unwrap_or(false);

                    // Скролл к активной строке при программной смене (стрелки ▲/▼, Enter)
                    if is_active && do_scroll_active {
                        let active_rect = egui::Rect::from_min_max(
                            egui::pos2(rect.left(), screen_start_y),
                            egui::pos2(rect.right(), screen_end_y),
                        );
                        ui.scroll_to_rect(active_rect, Some(Align::Center));
                        do_scroll_active = false;
                    }

                    // А. Левый гуттер: номер строки
                    let num_color = if is_active {
                        Color32::from_rgb(0, 100, 200)
                    } else if is_applied {
                        Color32::from_rgb(160, 165, 175)
                    } else {
                        Color32::from_rgb(100, 105, 115)
                    };
                    ui.painter().text(
                        egui::pos2(rect.left() + 22.0, screen_center_y),
                        egui::Align2::RIGHT_CENTER,
                        format!("{}", line_idx + 1),
                        egui::FontId::monospace(10.5),
                        num_color,
                    );

                    // Мишень / иконка статуса
                    let target_center = egui::pos2(rect.left() + 34.0, screen_center_y);
                    let target_rect = egui::Rect::from_center_size(target_center, vec2(16.0, 16.0));
                    let is_target_hovered = mouse_pos.is_some_and(|p| target_rect.contains(p));

                    if is_active {
                        ui.painter().circle_stroke(target_center, 4.5, Stroke::new(1.3, Color32::from_rgb(0, 120, 215)));
                        ui.painter().circle_filled(target_center, 2.3, Color32::from_rgb(0, 120, 215));
                    } else {
                        let stroke_color = if is_target_hovered {
                            Color32::from_rgb(0, 120, 215)
                        } else if is_applied {
                            Color32::from_rgb(175, 180, 190)
                        } else {
                            Color32::from_rgb(130, 135, 145)
                        };
                        ui.painter().circle_stroke(target_center, 4.5, Stroke::new(1.2, stroke_color));
                        ui.painter().circle_stroke(target_center, 1.5, Stroke::new(1.0, stroke_color));
                    }

                    if is_target_hovered && primary_clicked {
                        clicked_target_line = Some(line_idx);
                    }

                    // Б. Правый гуттер: кнопка быстрой отправки строки на холст (->)
                    let action_center = egui::pos2(rect.right() - 12.0, screen_center_y);
                    let action_rect = egui::Rect::from_center_size(action_center, vec2(18.0, 18.0));
                    let is_action_hovered = mouse_pos.is_some_and(|p| action_rect.contains(p));

                    let btn_bg = if is_action_hovered {
                        Color32::from_rgb(0, 120, 215)
                    } else {
                        Color32::from_rgb(240, 243, 248)
                    };
                    let icon_color = if is_action_hovered {
                        Color32::WHITE
                    } else {
                        Color32::from_rgb(120, 125, 135)
                    };
                    let border_color = if is_action_hovered {
                        Color32::from_rgb(0, 100, 190)
                    } else {
                        Color32::from_rgb(205, 210, 220)
                    };

                    ui.painter().circle_filled(action_center, 7.5, btn_bg);
                    ui.painter().circle_stroke(action_center, 7.5, Stroke::new(1.0, border_color));

                    let arrow_stroke = Stroke::new(1.4, icon_color);
                    let ax = action_center.x;
                    let ay = action_center.y;
                    ui.painter().line_segment([egui::pos2(ax - 3.5, ay), egui::pos2(ax + 2.5, ay)], arrow_stroke);
                    ui.painter().line_segment([egui::pos2(ax + 0.5, ay - 2.5), egui::pos2(ax + 3.0, ay)], arrow_stroke);
                    ui.painter().line_segment([egui::pos2(ax + 0.5, ay + 2.5), egui::pos2(ax + 3.0, ay)], arrow_stroke);

                    if is_action_hovered && primary_clicked {
                        clicked_action_line = Some(line_idx);
                    }

                    // В. Горизонтальная пунктирная линия под строкой
                    let dash_stroke = Stroke::new(1.0, Color32::from_rgb(181, 181, 181)); // #b5b5b5
                    draw_dashed_hline(ui.painter(), rect.left(), rect.right(), screen_end_y, dash_stroke);
                }

                if let Some(target_idx) = clicked_target_line {
                    self.state.set_current_line(target_idx);
                    self.scroll_to_active_line = true;
                }

                if let Some(act_idx) = clicked_action_line {
                    if let Some(line) = self.state.lines.get(act_idx) {
                        if let Some(preset) = self.state.active_preset().cloned() {
                            *action = Some(TypesetterAction {
                                text: line.text.clone(),
                                preset,
                            });
                            self.state.set_current_line(act_idx);
                            self.state.advance_line();
                            self.auto_save();
                        }
                    }
                }

                // 3. Отслеживание курсора: при клике или переходе по стрелкам обновляем активную строку
                if let Some(cr) = output.cursor_range {
                    let c_idx = cr.primary.index;
                    let char_count = self.script_buffer.chars().take(c_idx.into()).filter(|&c| c == '\n').count();
                    if char_count != self.state.current_line_idx && char_count < self.state.lines.len() {
                        self.state.set_current_line(char_count);
                    }
                }

                // 4. Синхронизация состояния при правках в текстовом поле
                if output.response.response.changed() {
                    self.sync_state_from_script_buffer();
                }
            });

        self.scroll_to_active_line = false;
    }

    /// Сетка карточек стилей шрифтов по папкам (с адаптивным числом колонок).
    fn render_style_presets_grid(&mut self, ui: &mut egui::Ui, action: &mut Option<TypesetterAction>, num_cols: usize) {
        let t = Tokens::get(ui.ctx());
        let active_preset_idx = self.state.selected_preset_idx;

        let mut style_to_select = None;
        let mut style_to_edit = None;
        let mut style_to_duplicate = None;
        let mut style_to_apply = None;
        let mut style_to_delete = None;

        let mut folder_names = self.state.folders();
        folder_names.push(tl!("No folder").to_string());

        let filter_lower = self.style_filter.trim().to_lowercase();

        ScrollArea::vertical()
            .id_salt("typer_folders_scroll")
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                for folder_name in &folder_names {
                    let is_unassigned = folder_name == tl!("No folder");

                    let matching_indices: Vec<usize> = self.state.presets
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| {
                            let in_folder = if is_unassigned {
                                p.folder.is_none() || p.folder.as_deref() == Some("")
                            } else {
                                p.folder.as_deref() == Some(folder_name.as_str())
                            };
                            let matches_filter = filter_lower.is_empty() || p.name.to_lowercase().contains(&filter_lower);
                            in_folder && matches_filter
                        })
                        .map(|(idx, _)| idx)
                        .collect();

                    if matching_indices.is_empty() && (is_unassigned || !filter_lower.is_empty()) {
                        continue;
                    }

                    let header_title = format!("{} ({})", folder_name, matching_indices.len());

                    egui::CollapsingHeader::new(RichText::new(&header_title).strong())
                        .id_salt(format!("folder_collapse_{}", folder_name))
                        .default_open(true)
                        .show(ui, |ui| {
                            let mut draw_card = |col: &mut egui::Ui, p_idx: usize| {
                                let preset = &self.state.presets[p_idx];
                                let is_active = p_idx == active_preset_idx;

                                let (card_bg, card_border, text_color) = if is_active {
                                    (
                                        Color32::from_rgb(0, 120, 215),
                                        Color32::from_rgb(100, 180, 255),
                                        Color32::WHITE,
                                    )
                                } else {
                                    (
                                        t.field,
                                        t.field_border,
                                        t.text,
                                    )
                                };

                                Frame::NONE
                                    .fill(card_bg)
                                    .stroke(Stroke::new(1.0, card_border))
                                    .corner_radius(CornerRadius::same(t.radius_sm as u8))
                                    .inner_margin(Margin::symmetric(4, 3))
                                    .show(col, |ui| {
                                        ui.horizontal(|ui| {
                                            let sel_icon = if is_active { "●" } else { "○" };
                                            let ind_color = if is_active { Color32::WHITE } else { t.text_dim };
                                            if ui.add(egui::Label::new(RichText::new(sel_icon).color(ind_color).small()).sense(Sense::click())).clicked() {
                                                style_to_select = Some(p_idx);
                                            }

                                            let swatch_color = egui::Color32::from_rgba_unmultiplied(
                                                preset.text_color[0],
                                                preset.text_color[1],
                                                preset.text_color[2],
                                                preset.text_color[3],
                                            );
                                            let (swatch_rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                                            ui.painter().rect_filled(swatch_rect, 2.0, swatch_color);
                                            if preset.stroke_width > 0.0 {
                                                let stroke_c = egui::Color32::from_rgba_unmultiplied(
                                                    preset.stroke_color[0],
                                                    preset.stroke_color[1],
                                                    preset.stroke_color[2],
                                                    preset.stroke_color[3],
                                                );
                                                ui.painter().rect_stroke(swatch_rect, 2.0, egui::Stroke::new(1.0, stroke_c), egui::StrokeKind::Outside);
                                            }

                                            let mut open_edit = false;
                                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                                let edit_btn = egui::Button::new(
                                                    RichText::new("✏")
                                                        .color(if is_active { Color32::WHITE } else { t.text_dim })
                                                        .small(),
                                                )
                                                .frame(false);
                                                if ui.add(edit_btn).on_hover_text("Настройки шрифта и стиля").clicked() {
                                                    open_edit = true;
                                                }

                                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                                    let mut title = RichText::new(&preset.name).color(text_color).small();
                                                    if preset.bold {
                                                        title = title.strong();
                                                    }
                                                    if preset.italic {
                                                        title = title.italics();
                                                    }

                                                    // Имя стиля показываем его собственным шрифтом,
                                                    // если семейство загружено и имя влезает в карточку.
                                                    let avail_w = ui.available_width().max(16.0);
                                                    let name_label = if let Some([w, h]) = crate::font_preview::name_size(ui.ctx(), &preset.font_family, &preset.name, STYLE_NAME_PT)
                                                        .filter(|[w, _]| *w <= avail_w)
                                                    {
                                                        let (rect, resp) = ui.allocate_exact_size(
                                                            vec2(w, h.max(ui.text_style_height(&egui::TextStyle::Body))),
                                                            Sense::click(),
                                                        );
                                                        crate::font_preview::paint_name(ui, &preset.font_family, &preset.name, STYLE_NAME_PT, rect, text_color);
                                                        resp
                                                    } else {
                                                        ui.add(egui::Label::new(title).truncate().sense(Sense::click()))
                                                    };

                                                    let sys_label = if preset.measurement_system.is_optical() { "Оптическая" } else { "Метрическая" };
                                                    let hex_c = format!("#{:02X}{:02X}{:02X}{:02X}", preset.text_color[0], preset.text_color[1], preset.text_color[2], preset.text_color[3]);
                                                    let tip = format!(
                                                        "{}\nШрифт: {}\nКегль: {}pt\nИнтерлиньяж: {}pt\nТрекинг: {}\nЦвет шрифта: {}\nСистема: {}\n\nЛКМ: выбрать стиль\n2xЛКМ: применить и вставить\nПКМ: меню действий",
                                                        preset.name, preset.font_family, preset.font_size, preset.leading, preset.tracking, hex_c, sys_label
                                                    );

                                                    let name_label = name_label.on_hover_text(tip);

                                                    if name_label.clicked() {
                                                        style_to_select = Some(p_idx);
                                                    }
                                                    if name_label.double_clicked() {
                                                        style_to_apply = Some(p_idx);
                                                    }

                                                    name_label.context_menu(|ui| {
                                                        if ui.button("✏ Редактировать стиль...").clicked() {
                                                            open_edit = true;
                                                            ui.close();
                                                        }
                                                        if ui.button("+ Дублировать стиль").clicked() {
                                                            style_to_duplicate = Some(p_idx);
                                                            ui.close();
                                                        }
                                                        if ui.button("▶ Применить и вставить").clicked() {
                                                            style_to_apply = Some(p_idx);
                                                            ui.close();
                                                        }
                                                        ui.separator();
                                                        if ui.button(RichText::new("🗑 Удалить стиль").color(t.danger)).clicked() {
                                                            style_to_delete = Some(p_idx);
                                                            ui.close();
                                                        }
                                                    });
                                                });
                                            });

                                            if open_edit {
                                                style_to_edit = Some(p_idx);
                                            }
                                        });
                                    });
                                col.add_space(2.0);
                            };

                            if num_cols <= 1 {
                                for &p_idx in &matching_indices {
                                    draw_card(ui, p_idx);
                                }
                            } else {
                                ui.columns(num_cols, |cols| {
                                    for (i, &p_idx) in matching_indices.iter().enumerate() {
                                        let col = &mut cols[i % num_cols];
                                        draw_card(col, p_idx);
                                    }
                                });
                            }
                        });
                }
            });

        if let Some(idx) = style_to_select {
            self.state.selected_preset_idx = idx;
            self.auto_save();
        }

        if let Some(idx) = style_to_edit {
            self.editing_preset_idx = Some(idx);
        }

        if let Some(idx) = style_to_duplicate {
            self.state.duplicate_preset(idx);
            self.auto_save();
        }

        if let Some(idx) = style_to_delete {
            if idx < self.state.presets.len() {
                self.state.presets.remove(idx);
                if self.state.selected_preset_idx >= self.state.presets.len() {
                    self.state.selected_preset_idx = self.state.presets.len().saturating_sub(1);
                }
                self.auto_save();
            }
        }

        if let Some(idx) = style_to_apply {
            self.state.selected_preset_idx = idx;
            if let Some(line) = self.state.active_line() {
                if let Some(preset) = self.state.presets.get(idx).cloned() {
                    *action = Some(TypesetterAction {
                        text: line.text.clone(),
                        preset,
                    });
                    self.state.advance_line();
                    self.scroll_to_active_line = true;
                }
            }
            self.auto_save();
        }
    }
    /// Вкладка "Текст": окно всего текста с листанием наперёд и полным просмотром.
    fn render_full_text_content(&mut self, ui: &mut egui::Ui, action: &mut Option<TypesetterAction>) {
        let t = Tokens::get(ui.ctx());
        let pages = self.state.pages();
        let current_page = self.state.current_page;
        let total_lines = self.state.lines.len();
        let applied_count = self.state.lines.iter().filter(|l| l.is_applied).count();
        let pending_count = total_lines.saturating_sub(applied_count);

        // Верхняя панель действий со сценарием (адаптивная к ширине)
        ui.horizontal(|ui| {
            let avail_w = ui.available_width();
            let is_narrow = avail_w < 300.0;

            let paste_btn_text = if is_narrow { "📋".to_string() } else { format!("📋 {}", tl!("Paste")) };
            if ui.button(RichText::new(paste_btn_text).strong()).on_hover_text(tl!("Paste script text from clipboard (Ctrl+V)")).clicked() {
                self.paste_script_from_clipboard();
            }

            let editor_btn_text = if is_narrow { "📝".to_string() } else { format!("📝 {}", tl!("Editor")) };
            if ui.button(RichText::new(editor_btn_text).strong()).on_hover_text(tl!("Open script text editor")).clicked() {
                self.open_script_editor();
            }

            let file_btn_text = if is_narrow { "📥 .txt".to_string() } else { format!("📥 {}", tl!("Open .txt")) };
            if ui.button(file_btn_text).on_hover_text(tl!("Load script file (.txt)")).clicked() {
                self.open_script_file_dialog();
            }

            if total_lines > 0 {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("🗑").on_hover_text(tl!("Clear entire script")).clicked() {
                        self.state.lines.clear();
                        self.state.current_line_idx = 0;
                        self.auto_save();
                    }
                });
            }
        });
        widgets::hairline(ui);
        ui.add_space(2.0);

        // Панель управления текстом
        ui.horizontal(|ui| {
            if ui.small_button("◀").on_hover_text(tl!("Previous page")).clicked() {
                if let Some(pos) = pages.iter().position(|&p| p == current_page) {
                    if pos > 0 {
                        self.state.current_page = pages[pos - 1];
                    }
                }
            }

            ui.label(RichText::new(format!("{} {}", tl!("Page"), current_page)).strong());

            if ui.small_button("▶").on_hover_text(tl!("Next page")).clicked() {
                if let Some(pos) = pages.iter().position(|&p| p == current_page) {
                    if pos + 1 < pages.len() {
                        self.state.current_page = pages[pos + 1];
                    }
                }
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let detach_icon = if self.show_full_text_window { "🗖" } else { "🗗" };
                if ui.small_button(detach_icon).on_hover_text(tl!("Open script in separate window")).clicked() {
                    self.show_full_text_window = !self.show_full_text_window;
                }

                if ui.small_button("🎯").on_hover_text(tl!("Go to current line/page")).clicked() {
                    if let Some(cur) = self.state.lines.get(self.state.current_line_idx) {
                        self.state.current_page = cur.page;
                    }
                }

                ui.checkbox(&mut self.filter_current_page_only, tl!("Page"))
                    .on_hover_text(tl!("Show current page dialogues only"));
            });
        });

        // Поиск и статистика
        ui.horizontal(|ui| {
            let search_w = (ui.available_width() - 95.0).max(80.0);
            ui.add(egui::TextEdit::singleline(&mut self.full_text_filter).hint_text(format!("🔍 {}", tl!("Search…"))).desired_width(search_w));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{}/{} ({}: {})", applied_count, total_lines, tl!("left"), pending_count))
                        .weak()
                        .small(),
                );
            });
        });

        ui.separator();

        if total_lines == 0 {
            ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(16.0);
                    ui.label(RichText::new(tl!("No script loaded")).strong().color(t.text_dim));
                    ui.add_space(10.0);
                    let avail_w = ui.available_width();
                    if avail_w >= 360.0 {
                        ui.horizontal(|ui| {
                            if ui.button(RichText::new(format!("📋 {}", tl!("Paste from Clipboard"))).strong()).on_hover_text(tl!("Paste script from clipboard (Ctrl+V)")).clicked() {
                                self.paste_script_from_clipboard();
                            }
                            if ui.button(format!("📥 {}", tl!("Load .txt"))).on_hover_text(tl!("Load script file (.txt)")).clicked() {
                                self.open_script_file_dialog();
                            }
                            if ui.button(format!("📝 {}", tl!("Enter in Editor"))).on_hover_text(tl!("Enter script manually")).clicked() {
                                self.open_script_editor();
                            }
                        });
                    } else {
                        // Адаптивный вертикальный стек кнопок во всю ширину
                        ui.spacing_mut().item_spacing.y = 6.0;
                        let btn_w = (avail_w - 20.0).max(180.0);
                        if ui.add_sized([btn_w, 28.0], egui::Button::new(RichText::new(format!("📋 {}", tl!("Paste from Clipboard"))).strong())).on_hover_text(tl!("Paste script from clipboard (Ctrl+V)")).clicked() {
                            self.paste_script_from_clipboard();
                        }
                        if ui.add_sized([btn_w, 28.0], egui::Button::new(format!("📥 {}", tl!("Load .txt")))).on_hover_text(tl!("Load script file (.txt)")).clicked() {
                            self.open_script_file_dialog();
                        }
                        if ui.add_sized([btn_w, 28.0], egui::Button::new(format!("📝 {}", tl!("Enter in Editor")))).on_hover_text(tl!("Enter script manually")).clicked() {
                            self.open_script_editor();
                        }
                    }
                    ui.add_space(16.0);
                });
            });
            return;
        }

        // Список всего текста скроллируемый
        ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                let filter = self.full_text_filter.to_lowercase();
                let mut line_to_set = None;
                let mut line_to_insert = None;
                let mut text_changed = false;
                let mut should_clear_scroll = false;
                let do_scroll = self.scroll_to_active_line;
                let cur_line_idx = self.state.current_line_idx;
                let mut last_rendered_page = 0;

                for (idx, line) in self.state.lines.iter_mut().enumerate() {
                    if self.filter_current_page_only && line.page != self.state.current_page {
                        continue;
                    }
                    if !filter.is_empty() && !line.text.to_lowercase().contains(&filter) {
                        continue;
                    }

                    // Разделитель страниц
                    if line.page != last_rendered_page {
                        last_rendered_page = line.page;
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("─── {} {} ───", tl!("Page"), line.page)).color(t.text_dim).strong().small());
                        });
                        ui.add_space(2.0);
                    }

                    let is_active = idx == cur_line_idx;
                    let (card_bg, card_border, text_color, num_color) = if is_active {
                        (
                            Color32::from_rgb(204, 232, 255),
                            Color32::from_rgb(0, 120, 215),
                            Color32::from_rgb(10, 25, 50),
                            Color32::from_rgb(0, 90, 180),
                        )
                    } else if line.is_applied {
                        (
                            Color32::from_rgb(250, 250, 252),
                            Color32::TRANSPARENT,
                            Color32::from_rgb(150, 155, 165),
                            Color32::from_rgb(170, 175, 185),
                        )
                    } else {
                        (
                            Color32::WHITE,
                            Color32::TRANSPARENT,
                            Color32::from_rgb(20, 25, 35),
                            Color32::from_rgb(110, 115, 125),
                        )
                    };

                    let card_frame = Frame::NONE
                        .fill(card_bg)
                        .stroke(Stroke::new(if is_active { 1.5 } else { 0.0 }, card_border))
                        .corner_radius(CornerRadius::same(3))
                        .inner_margin(Margin::symmetric(6, 4));

                    let row_resp = card_frame.show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{:2}", idx + 1)).color(num_color).monospace().small());

                            let target_icon = if is_active { "●" } else { "◎" };
                            if ui.small_button(target_icon).on_hover_text(tl!("Make this line current")).clicked() {
                                line_to_set = Some((idx, true));
                            }

                            let mut do_insert = false;
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let (btn_text, btn_fill) = if line.is_applied {
                                    ("✓", Color32::from_rgb(46, 125, 50))
                                } else {
                                    ("▶", Color32::from_rgb(0, 120, 215))
                                };
                                let insert_btn = egui::Button::new(
                                    RichText::new(btn_text).color(Color32::WHITE).strong()
                                )
                                .fill(btn_fill)
                                .min_size(vec2(22.0, 16.0));

                                if ui.add(insert_btn).on_hover_text(tl!("Insert this line into bubble (Enter)")).clicked() {
                                    do_insert = true;
                                }

                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    let edit = egui::TextEdit::singleline(&mut line.text)
                                        .font(egui::TextStyle::Body)
                                        .text_color(text_color)
                                        .frame(egui::Frame::NONE)
                                        .id_salt(egui::Id::new("typer_fulltext_line").with(idx));
                                    let edit_resp = ui.add(edit);
                                    if edit_resp.gained_focus() || edit_resp.clicked() {
                                        line_to_set = Some((idx, false));
                                    }
                                    if edit_resp.changed() {
                                        text_changed = true;
                                    }
                                });
                            });

                            if do_insert {
                                line_to_insert = Some((idx, line.clone()));
                            }
                        });
                    });

                    if is_active && do_scroll {
                        row_resp.response.scroll_to_me(Some(Align::Center));
                        should_clear_scroll = true;
                    }

                    ui.add_space(2.0);
                }

                if should_clear_scroll {
                    self.scroll_to_active_line = false;
                }

                if let Some((idx, req_scroll)) = line_to_set {
                    self.state.set_current_line(idx);
                    if req_scroll {
                        self.scroll_to_active_line = true;
                    }
                }

                if let Some((idx, line)) = line_to_insert {
                    if let Some(preset) = self.state.active_preset().cloned() {
                        *action = Some(TypesetterAction {
                            text: line.text.clone(),
                            preset,
                        });
                        self.state.set_current_line(idx + 1);
                        self.scroll_to_active_line = true;
                        self.auto_save();
                    }
                }

                if text_changed {
                    self.auto_save();
                }
            });
    }

    /// Модальное окно для прямого ввода, вставки (Ctrl+V) и редактирования текста сценария.
    fn render_script_editor_modal(&mut self, ui: &mut egui::Ui, close_modal: &mut bool) {
        let t = Tokens::get(ui.ctx());

        ui.horizontal(|ui| {
            if ui.button(format!("📋 {}", tl!("Paste from Clipboard (Ctrl+V)"))).on_hover_text(tl!("Paste copied text from clipboard")).clicked() {
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    if let Ok(text) = cb.get_text() {
                        if !text.trim().is_empty() {
                            self.script_editor_buffer = text;
                        }
                    }
                }
            }

            if ui.button(format!("📥 {}", tl!("Load file (.txt)…"))).clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Текстовые файлы (*.txt)", &["txt"])
                    .set_title(tl!("Choose script file"))
                    .pick_file()
                {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        self.script_editor_buffer = content;
                    }
                }
            }

            if ui.button(tl!("Clear")).clicked() {
                self.script_editor_buffer.clear();
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let line_count = self.script_editor_buffer.lines().filter(|l| !l.trim().is_empty()).count();
                ui.label(RichText::new(format!("{}: {}", tl!("Lines"), line_count)).color(t.text_dim).small());
            });
        });

        ui.add_space(4.0);
        ui.label(
            RichText::new(tl!("Each line becomes a separate dialogue entry. [Page 1] tags split pages."))
                .color(t.text_dim)
                .small(),
        );
        ui.add_space(4.0);

        let available_h = ui.available_height() - 42.0;
        egui::ScrollArea::vertical()
            .max_height(available_h.max(160.0))
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.script_editor_buffer)
                        .hint_text(tl!("Paste script text here (Ctrl+V) or write dialogues…"))
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace)
                        .lock_focus(true),
                );
            });

        ui.add_space(6.0);
        widgets::hairline(ui);
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            let has_text = !self.script_editor_buffer.trim().is_empty();
            let apply_btn = egui::Button::new(
                RichText::new(format!("✓ {}", tl!("Load into typesetter"))).color(t.accent_text).strong()
            )
            .fill(t.accent);

            if ui.add_enabled(has_text, apply_btn).clicked() {
                self.script_input_buffer = self.script_editor_buffer.clone();
                self.state.load_script(&self.script_editor_buffer);
                self.sync_script_buffer_from_state();
                self.auto_save();
                self.status_message = Some(format!("{}: {}", tl!("Loaded dialogues"), self.state.lines.len()));
                self.status_error = false;
                *close_modal = true;
            }

            if ui.button(tl!("Cancel")).clicked() {
                *close_modal = true;
            }
        });
    }

    /// Модальное окно редактирования настроек конкретного шрифта и стиля.
    fn render_font_settings_modal(&mut self, ui: &mut egui::Ui, idx: usize, close_modal: &mut bool) {
        let available_folders = self.state.folders();
        let mut to_delete = false;
        let mut to_duplicate = false;
        let mut changed = false;
        let mut toggle_fav_font: Option<String> = None;
        let mut toggle_sync = false;
        let sync_leading = self.state.options.line_spacing_sync;

        // The inline "new folder" field below the Folder combo, kept across frames in egui's
        // memory: the preset borrow below must not also borrow `self` for it.
        let new_folder_open_id = egui::Id::new("typesetter-modal-new-folder-open");
        let new_folder_name_id = egui::Id::new("typesetter-modal-new-folder-name");
        let preview_edit_id = egui::Id::new("typesetter-preview-edit");
        let mut new_folder_open = ui.data(|d| d.get_temp::<bool>(new_folder_open_id)).unwrap_or(false);
        let mut new_folder_name = ui.data_mut(|d| d.get_temp::<String>(new_folder_name_id)).unwrap_or_default();

        if let Some(preset) = self.state.presets.get_mut(idx) {
            Frame::NONE
                .inner_margin(Margin { left: FORM_LEFT_INSET as i8, right: 0, top: 11, bottom: 4 })
                .show(ui, |ui| {
                    // The mockup has one caption column and one control axis. Vertical spacing is
                    // explicit: the section rules and headers sit on fixed offsets from the first
                    // row, not on the default item spacing.
                    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                    ui.spacing_mut().interact_size.y = ROW_H;
                    // The mockup's grid: 94 + 6 + 445 = 545 inside the form's 6-point inset.
                    ui.set_width(LABEL_W + COL_GAP + RIGHT_W);

                    // 1. Style name
                    form_row(ui, "Style name", |ui| {
                        if ui
                            .add_sized(
                                [RIGHT_W, ROW_H],
                                egui::TextEdit::singleline(&mut preset.name).vertical_align(Align::Center),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                    });

                    ui.add_space(5.0);

                    // 2. Folder
                    form_row(ui, "Folder", |ui| {
                        let folder_str = preset.folder.clone().unwrap_or_default();
                        egui::ComboBox::from_id_salt("edit_preset_folder_combo")
                            .selected_text(if folder_str.is_empty() {
                                tl!("No folder").to_string()
                            } else {
                                folder_str.clone()
                            })
                            .width(RIGHT_W)
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(folder_str.is_empty(), tl!("No folder")).clicked() {
                                    preset.folder = None;
                                    changed = true;
                                }
                                for f in &available_folders {
                                    if ui.selectable_label(folder_str == *f, f).clicked() {
                                        preset.folder = Some(f.clone());
                                        changed = true;
                                    }
                                }
                                ui.separator();
                                if ui.selectable_label(false, tl!("+ New folder...")).clicked() {
                                    new_folder_open = true;
                                    new_folder_name.clear();
                                }
                            });
                    });

                    if new_folder_open {
                        ui.add_space(5.0);
                        form_row(ui, "Folder name", |ui| {
                            let response = ui.add_sized(
                                [240.0, ROW_H],
                                egui::TextEdit::singleline(&mut new_folder_name)
                                    .hint_text(tl!("New folder name"))
                                    .vertical_align(Align::Center),
                            );
                            ui.add_space(6.0);
                            let submit = ui.add_sized([54.0, ROW_H], egui::Button::new(tl!("OK"))).clicked()
                                || (response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                            ui.add_space(6.0);
                            if submit {
                                let name = new_folder_name.trim();
                                if !name.is_empty() {
                                    preset.folder = Some(name.to_string());
                                    changed = true;
                                }
                                new_folder_open = false;
                                new_folder_name.clear();
                            }
                            if ui.add_sized([72.0, ROW_H], egui::Button::new(tl!("Cancel"))).clicked() {
                                new_folder_open = false;
                                new_folder_name.clear();
                            }
                        });
                        ui.add_space(5.0);
                    } else {
                        ui.add_space(10.0);
                    }

                    section_header(ui, "Typography and Font", 6.0);
                    ui.add_space(7.0);

                    // 3. Font
                    form_row(ui, "Font", |ui| {
                        let is_fav = crate::type_tool::is_favorite_font(ui.ctx(), &preset.font_family);
                        let star_text = if is_fav { "★" } else { "☆" };
                        let star_color = if is_fav {
                            egui::Color32::from_rgb(240, 190, 50)
                        } else {
                            ui.visuals().weak_text_color()
                        };
                        let star_btn = egui::Button::new(RichText::new(star_text).color(star_color).size(14.0))
                            .min_size(vec2(43.0, ROW_H));
                        if ui.add(star_btn).on_hover_text(tl!("Favorite font")).clicked() {
                            toggle_fav_font = Some(preset.font_family.clone());
                        }
                        ui.add_space(6.0);

                        let prev_fam = preset.font_family.clone();
                        let families = crate::type_tool::families();
                        let font_combo_w = RIGHT_W - 43.0 - 6.0;
                        if crate::type_tool::family_picker_in(
                            ui,
                            "typesetter-preset-font",
                            &mut preset.font_family,
                            font_combo_w,
                            &families,
                            photocraft_engine::prefs::FontPreview::Medium,
                            &[],
                        ) {
                            changed = true;
                            if preset.font_family != prev_fam {
                                if let Some(engine) = photocraft_text::try_shared() {
                                    let faces = engine
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                                        .fonts
                                        .faces(&preset.font_family);
                                    if let Some(first_face) = faces.first() {
                                        if let Some(ps) = &first_face.postscript_name {
                                            preset.font_postscript = ps.clone();
                                        }
                                        preset.font_style = first_face.style.clone();
                                    }
                                }
                            }
                        }
                    });

                    ui.add_space(7.0);

                    // 4. PostScript + Style
                    form_row(ui, "PostScript", |ui| {
                        if ui
                            .add_sized(
                                [295.0, ROW_H],
                                egui::TextEdit::singleline(&mut preset.font_postscript).vertical_align(Align::Center),
                            )
                            .changed()
                        {
                            changed = true;
                        }

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            let mut styles = crate::type_tool::styles(&preset.font_family);
                            if !preset.font_style.is_empty() && !styles.iter().any(|s| *s == preset.font_style) {
                                styles.insert(0, preset.font_style.clone());
                            }
                            let options: Vec<(String, String)> = styles
                                .iter()
                                .map(|s| (s.clone(), crate::type_tool::style_label(s)))
                                .collect();
                            let options_ref: Vec<(String, &str)> = options
                                .iter()
                                .map(|(value, label)| (value.clone(), label.as_str()))
                                .collect();
                            if crate::widgets::dropdown_literal(
                                ui,
                                "typesetter-preset-style",
                                &mut preset.font_style,
                                &options_ref,
                                91.0,
                            ) {
                                changed = true;
                            }
                            ui.add_space(6.0);
                            ui.label(format!("{}:", tl!("Style")));
                        });
                    });

                    ui.add_space(7.0);

                    // 5. Formatting (9 buttons evenly spanning RIGHT_W: 9 * 45 + 8 * 5 = 445)
                    form_row(ui, "Formatting", |ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;

                        if character_toggle(ui, &mut preset.bold, "B", tl!("Faux bold")) {
                            if preset.bold && !preset.font_style.to_lowercase().contains("bold") {
                                preset.font_style = if preset.italic { "Bold Italic".into() } else { "Bold".into() };
                            } else if !preset.bold && preset.font_style.to_lowercase().contains("bold") {
                                preset.font_style = if preset.italic { "Italic".into() } else { "Regular".into() };
                            }
                            changed = true;
                        }
                        if character_toggle(ui, &mut preset.italic, "I", tl!("Faux italic")) {
                            if preset.italic && !preset.font_style.to_lowercase().contains("italic") {
                                preset.font_style = if preset.bold { "Bold Italic".into() } else { "Italic".into() };
                            } else if !preset.italic && preset.font_style.to_lowercase().contains("italic") {
                                preset.font_style = if preset.bold { "Bold".into() } else { "Regular".into() };
                            }
                            changed = true;
                        }
                        if character_toggle(ui, &mut preset.all_caps, "TT", tl!("All Caps")) {
                            if preset.all_caps {
                                preset.small_caps = false;
                            }
                            changed = true;
                        }
                        if character_toggle(ui, &mut preset.small_caps, "Tт", tl!("Small Caps")) {
                            if preset.small_caps {
                                preset.all_caps = false;
                            }
                            changed = true;
                        }
                        if character_toggle(ui, &mut preset.superscript, "T¹", tl!("Superscript")) {
                            if preset.superscript {
                                preset.subscript = false;
                            }
                            changed = true;
                        }
                        if character_toggle(ui, &mut preset.subscript, "T₁", tl!("Subscript")) {
                            if preset.subscript {
                                preset.superscript = false;
                            }
                            changed = true;
                        }
                        if character_toggle(ui, &mut preset.underline, "U", tl!("Underline")) {
                            changed = true;
                        }
                        if character_toggle(ui, &mut preset.strikethrough, "S", tl!("Strikethrough")) {
                            changed = true;
                        }
                        if character_toggle(ui, &mut preset.hanging_punctuation, "«»", tl!("Hanging Punctuation")) {
                            changed = true;
                        }
                    });

                    ui.add_space(8.0);

                    // 6. Alignment (3 buttons evenly spanning RIGHT_W: 3 * 145 + 2 * 5 = 445)
                    form_row(ui, "Alignment", |ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;
                        for value in ["left", "center", "right"] {
                            let key = match value {
                                "left" => "Align Left",
                                "right" => "Align Right",
                                _ => "Align Center",
                            };
                            let label = crate::i18n::t(key);
                            if alignment_button(ui, value, label, label, preset.align.as_str() == value) {
                                preset.align = value.to_string();
                                changed = true;
                            }
                        }
                    });

                    ui.add_space(8.0);

                    // 7. Anti-alias (217 wide, matching the Color swatch + hex row)
                    form_row(ui, "Anti-alias", |ui| {
                        let cur_aa = preset.anti_alias.clone();
                        let display_label = anti_alias_label(&cur_aa);
                        egui::ComboBox::from_id_salt("style_modal_aa")
                            .selected_text(display_label)
                            .width(217.0)
                            .show_ui(ui, |ui| {
                                for value in ["crisp", "sharp", "smooth", "strong", "none"] {
                                    if ui.selectable_label(cur_aa == value, anti_alias_label(value)).clicked() {
                                        preset.anti_alias = value.to_string();
                                        changed = true;
                                    }
                                }
                            });
                    });

                    ui.add_space(6.0);

                    // 8. Color (43 swatch + 7 gap + 167 hex = 217)
                    form_row(ui, "Color", |ui| {
                        if color_swatch_with_hex_sized(
                            ui,
                            &mut preset.text_color,
                            "typesetter-text-color",
                            vec2(43.0, 24.0),
                            7.0,
                            167.0,
                        ) {
                            changed = true;
                        }
                    });

                    ui.add_space(11.0);
                    section_header(ui, "Typography Size (Line Spacing Sync)", 8.0);
                    ui.add_space(8.0);

                    // 9. System type (joined segmented control: 2 * 95 = 190, 0 gap)
                    form_row(ui, "System type", |ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        let is_optical = preset.measurement_system.is_optical();
                        // One joined 190-point control: only the outer corners are rounded.
                        if ui
                            .add(
                                egui::Button::new(RichText::new(tl!("Metrics")).size(12.5))
                                    .selected(!is_optical)
                                    .corner_radius(CornerRadius { nw: 4, sw: 4, ne: 0, se: 0 })
                                    .min_size(vec2(95.0, ROW_H)),
                            )
                            .on_hover_text(tl!("Use standard font metrics and the baseline"))
                            .clicked()
                        {
                            preset.measurement_system = photocraft_engine::typesetter::MeasurementSystem::Metric;
                            changed = true;
                        }
                        if ui
                            .add(
                                egui::Button::new(RichText::new(tl!("Optical")).size(12.5))
                                    .selected(is_optical)
                                    .corner_radius(CornerRadius { nw: 0, sw: 0, ne: 4, se: 4 })
                                    .min_size(vec2(95.0, ROW_H)),
                            )
                            .on_hover_text(tl!("Use visual kerning and bubble centering"))
                            .clicked()
                        {
                            preset.measurement_system = photocraft_engine::typesetter::MeasurementSystem::Optical;
                            changed = true;
                        }
                    });

                    ui.add_space(6.0);

                    // 10. Size (pt) + A- / A+ / Link + Leading (right-aligned)
                    form_row(ui, "Size (pt)", |ui| {
                        let mut size = preset.font_size;
                        if ui
                            .add_sized(
                                [86.0, ROW_H],
                                egui::DragValue::new(&mut size).speed(0.5).range(4.0..=200.0).fixed_decimals(1),
                            )
                            .changed()
                        {
                            if sync_leading {
                                preset.sync_leading_with_size(size);
                            } else {
                                preset.font_size = size;
                            }
                            changed = true;
                        }

                        ui.add_space(5.0);
                        if ui.add_sized([45.0, ROW_H], egui::Button::new("A-")).clicked() {
                            let new_sz = (preset.font_size - 1.0).max(4.0);
                            if sync_leading {
                                preset.sync_leading_with_size(new_sz);
                            } else {
                                preset.font_size = new_sz;
                            }
                            changed = true;
                        }
                        ui.add_space(5.0);
                        if ui.add_sized([45.0, ROW_H], egui::Button::new("A+")).clicked() {
                            let new_sz = preset.font_size + 1.0;
                            if sync_leading {
                                preset.sync_leading_with_size(new_sz);
                            } else {
                                preset.font_size = new_sz;
                            }
                            changed = true;
                        }
                        ui.add_space(5.0);

                        let link_icon = if sync_leading { "🔗" } else { "⛓" };
                        let link_tip = if sync_leading {
                            tl!("Leading is linked to size")
                        } else {
                            tl!("Leading is not linked to size")
                        };
                        let link_btn = egui::Button::new(RichText::new(link_icon).size(12.5))
                            .selected(sync_leading)
                            .min_size(vec2(46.0, ROW_H));
                        if ui.add(link_btn).on_hover_text(link_tip).clicked() {
                            toggle_sync = true;
                        }

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            let leading_value = ui.add_sized(
                                [86.0, ROW_H],
                                egui::DragValue::new(&mut preset.leading)
                                    .speed(0.5)
                                    .range(4.0..=300.0)
                                    .fixed_decimals(1),
                            );
                            remember_rect(ui, "leading-value", leading_value.rect);
                            if leading_value.changed() {
                                changed = true;
                            }
                            ui.add_space(6.0);
                            ui.allocate_ui_with_layout(vec2(85.0, ROW_H), Layout::left_to_right(Align::Center), |ui| {
                                // The fixed cell width keeps both captions on one axis, so the
                                // Leading and H-Scale labels share their left edge.
                                ui.set_min_size(vec2(85.0, ROW_H));
                                ui.label(format!("{}:", tl!("Leading")));
                            });
                        });
                    });

                    ui.add_space(7.0);

                    // 11. Tracking + H-Scale (%) (aligned with Size & Leading above)
                    form_row(ui, "Tracking", |ui| {
                        if ui
                            .add_sized(
                                [86.0, ROW_H],
                                egui::DragValue::new(&mut preset.tracking).speed(1.0).range(-200..=1000),
                            )
                            .changed()
                        {
                            changed = true;
                        }

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            let hscale_value = ui.add_sized(
                                [86.0, ROW_H],
                                egui::DragValue::new(&mut preset.horizontal_scale)
                                    .speed(1.0)
                                    .range(20.0..=300.0)
                                    .fixed_decimals(0),
                            );
                            remember_rect(ui, "hscale-value", hscale_value.rect);
                            if hscale_value.changed() {
                                changed = true;
                            }
                            ui.add_space(6.0);
                            ui.allocate_ui_with_layout(vec2(85.0, ROW_H), Layout::left_to_right(Align::Center), |ui| {
                                // The fixed cell width keeps both captions on one axis, so the
                                // Leading and H-Scale labels share their left edge.
                                ui.set_min_size(vec2(85.0, ROW_H));
                                ui.label(format!("{}:", tl!("H-Scale (%)")));
                            });
                        });
                    });

                    ui.add_space(11.0);
                    section_header(ui, "Text Outline", 8.0);
                    ui.add_space(5.0);

                    // 12. Thickness (px) + Color swatch and HEX (variant B: the Color label is a step)
                    form_row(ui, "Thickness (px)", |ui| {
                        if ui
                            .add_sized(
                                [116.0, ROW_H],
                                egui::DragValue::new(&mut preset.stroke_width)
                                    .speed(0.2)
                                    .range(0.0..=30.0)
                                    .fixed_decimals(1),
                            )
                            .changed()
                        {
                            changed = true;
                        }

                        ui.add_space(48.0);
                        ui.allocate_ui_with_layout(vec2(43.0, ROW_H), Layout::left_to_right(Align::Center), |ui| {
                            ui.label(format!("{}:", tl!("Color")));
                        });
                        ui.add_space(8.0);
                        if color_swatch_with_hex_sized(
                            ui,
                            &mut preset.stroke_color,
                            "typesetter-stroke-color",
                            vec2(28.0, 24.0),
                            8.0,
                            194.0,
                        ) {
                            changed = true;
                        }
                    });

                    ui.add_space(11.0);
                    section_header(ui, "Preview sample", 8.0);
                    ui.add_space(3.0);

                    let t = Tokens::get(ui.ctx());
                    let mut edit = ui
                        .data(|d| d.get_temp::<PreviewEdit>(preview_edit_id))
                        .filter(|e| e.idx == idx);
                    let mut begin_edit = false;
                    let frame = Frame::NONE
                        .fill(t.field)
                        .stroke(Stroke::new(1.0, t.field_border))
                        .corner_radius(CornerRadius::same(6))
                        // The mockup's frame is 545×40 with a 521×28 image inside: the 1-point
                        // stroke counts on both sides, so the margins are 5 (vertical) and 11
                        // (horizontal) around the 28-point content.
                        .inner_margin(Margin { left: 11, right: 11, top: 5, bottom: 5 })
                        .show(ui, |ui| {
                            ui.set_min_height(PREVIEW_H);
                            ui.set_max_height(PREVIEW_H);
                            let mut cancel_edit = false;
                            let mut commit_edit = false;
                            if let Some(field) = edit.as_mut() {
                                // The field is the frame's inner width and 26 high, vertically
                                // centered in the 28-point band, like the mockup.
                                let width = ui.available_width();
                                let response = ui
                                    .allocate_ui_with_layout(
                                        vec2(width, PREVIEW_H),
                                        Layout::left_to_right(Align::Center),
                                        |ui| {
                                            ui.add_sized(
                                                [width, 26.0],
                                                egui::TextEdit::singleline(&mut field.buffer)
                                                    .hint_text(tl!("Leave empty for the automatic sample"))
                                                    .vertical_align(Align::Center),
                                            )
                                        },
                                    )
                                    .inner;
                                if field.request_focus {
                                    response.request_focus();
                                    if let Some(mut state) =
                                        egui::TextEdit::load_state(ui.ctx(), response.id)
                                    {
                                        let all = egui::text::CCursorRange::two(
                                            egui::text::CCursor::new(0),
                                            egui::text::CCursor::new(field.buffer.chars().count()),
                                        );
                                        state.cursor.set_char_range(Some(all));
                                        state.store(ui.ctx(), response.id);
                                    }
                                    field.request_focus = false;
                                }
                                let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                                cancel_edit = escape;
                                commit_edit = !escape && (response.lost_focus() || enter);
                            } else {
                                render_style_preview(ui, preset);
                            }
                            if cancel_edit {
                                edit = None;
                            } else if commit_edit {
                                if let Some(pending) = edit.take() {
                                    if commit_preview_edit(
                                        preset,
                                        &pending.buffer,
                                        &pending.original,
                                    ) {
                                        changed = true;
                                    }
                                }
                            }
                        });

                    let frame_rect = frame.response.rect;
                    // Geometry probe for the kittest suite: the mockup's preview frame is
                    // 545×40 with a 521×28 image inside.
                    ui.data_mut(|d| {
                        d.insert_temp(egui::Id::new("typesetter-preview-frame-rect"), frame_rect)
                    });
                    if edit.is_none() {
                        let frame_click = ui.interact(
                            frame_rect,
                            egui::Id::new(("typesetter-preview-frame", idx)),
                            Sense::click(),
                        );
                        if frame_click.double_clicked() {
                            begin_edit = true;
                        }
                        if ui.rect_contains_pointer(frame_rect) {
                            let btn_rect = egui::Rect::from_min_size(
                                egui::pos2(frame_rect.max.x - 22.0, frame_rect.min.y + 4.0),
                                vec2(18.0, 18.0),
                            );
                            let btn = ui.interact(
                                btn_rect,
                                egui::Id::new(("typesetter-preview-edit", idx)),
                                Sense::click(),
                            );
                            ui.painter().rect_filled(
                                btn_rect,
                                CornerRadius::same(4),
                                if btn.hovered() { t.hover } else { t.field },
                            );
                            ui.painter().text(
                                btn_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "✎",
                                egui::FontId::proportional(12.0),
                                t.text_dim,
                            );
                            if btn.on_hover_text(tl!("Edit preview text")).clicked() {
                                begin_edit = true;
                            }
                        }
                    }
                    if begin_edit {
                        let effective = style_preview_text(preset);
                        edit = Some(PreviewEdit {
                            idx,
                            buffer: effective.clone(),
                            original: effective,
                            request_focus: true,
                        });
                    }
                    ui.data_mut(|d| {
                        if let Some(edit) = &edit {
                            d.insert_temp(preview_edit_id, edit.clone());
                        } else {
                            d.remove_temp::<PreviewEdit>(preview_edit_id);
                        }
                    });

                    ui.add_space(25.0);
                    widgets::hairline(ui);
                    ui.add_space(6.0);

                    // 13. Footer buttons (Duplicate, Delete on left; Done on right)
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if ui
                            .add_sized([95.0, FOOTER_H], egui::Button::new(tl!("Duplicate")))
                            .on_hover_text(tl!("Duplicate this style"))
                            .clicked()
                        {
                            to_duplicate = true;
                        }
                        if ui
                            .add_sized([85.0, FOOTER_H], egui::Button::new(tl!("Delete")))
                            .on_hover_text(tl!("Delete this style"))
                            .clicked()
                        {
                            to_delete = true;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let done_btn = egui::Button::new(
                                RichText::new(tl!("Done")).color(t.accent_text).strong(),
                            )
                            .fill(t.accent)
                            .min_size(vec2(77.0, FOOTER_H));
                            if ui.add(done_btn).clicked() {
                                *close_modal = true;
                            }
                        });
                    });
                });
        }

        // Done and the window's own close button must persist the preview field before
        // closing, even when the click did not move focus out of it first.
        if *close_modal {
            let pending = ui.data_mut(|d| d.remove_temp::<PreviewEdit>(preview_edit_id));
            if let Some(pending) = pending {
                if pending.idx == idx {
                    if let Some(preset) = self.state.presets.get_mut(idx) {
                        if commit_preview_edit(preset, &pending.buffer, &pending.original) {
                            changed = true;
                        }
                    }
                }
            }
        }

        ui.data_mut(|d| {
            d.insert_temp(new_folder_open_id, new_folder_open);
            d.insert_temp(new_folder_name_id, new_folder_name.clone());
        });

        if toggle_sync {
            self.state.options.line_spacing_sync = !self.state.options.line_spacing_sync;
            self.auto_save();
        }

        if let Some(font) = toggle_fav_font {
            let now_fav = crate::type_tool::toggle_favorite_font(ui.ctx(), &font);
            if now_fav {
                if !self.favorite_fonts.contains(&font) {
                    self.favorite_fonts.push(font);
                }
            } else {
                self.favorite_fonts.retain(|f| f != &font);
            }
            changed = true;
        }

        if to_duplicate {
            self.state.duplicate_preset(idx);
            *close_modal = true;
        } else if to_delete {
            self.state.delete_preset(idx);
            *close_modal = true;
        }

        if changed {
            self.auto_save();
        }
    }

    /// Вкладка "Опции": общие настройки тайпсеттера, привязки и сохранение.
    fn render_settings_tab(&mut self, ui: &mut egui::Ui) {
        let t = Tokens::get(ui.ctx());

        ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            ui.label(RichText::new(tl!("TYPESETTER BEHAVIOR")).size(11.0).color(t.text_dim).strong());

            if ui.checkbox(
                &mut self.state.options.auto_page_switch,
                tl!("Auto-advance to next page"),
            ).on_hover_text(tl!("Automatically advance script to the next page")).changed() {
                self.auto_save();
            }

            if ui.checkbox(
                &mut self.state.options.line_spacing_sync,
                tl!("Sync line spacing"),
            ).on_hover_text(tl!("Sync line spacing with font size")).changed() {
                self.auto_save();
            }

            if ui.checkbox(
                &mut self.state.options.strip_character_names,
                tl!("Strip character names"),
            ).on_hover_text(tl!("Strip character names at start of line ('HERO: Text')")).changed() {
                self.auto_save();
            }

            if ui.checkbox(
                &mut self.state.options.strip_dash_prefix,
                tl!("Strip leading dashes"),
            ).on_hover_text(tl!("Strip leading dashes ('- Text', '— Text')")).changed() {
                self.auto_save();
            }

            if ui.checkbox(
                &mut self.state.options.hanging_punctuation,
                tl!("Компенсация знаков в строках"),
            ).on_hover_text(tl!("Оптическая компенсация и вынос знаков препинания за границы строк (висячая пунктуация)")).changed() {
                self.auto_save();
            }

            widgets::hairline(ui);
            ui.add_space(4.0);

            ui.label(RichText::new(tl!("GEOMETRY & CENTERING")).size(11.0).color(t.text_dim).strong());

            let avail_w = ui.available_width();
            if avail_w < 340.0 {
                ui.label(RichText::new(tl!("Baseline shift:")).color(t.text));
                let slider_w = (ui.available_width() - 8.0).max(120.0);
                if ui.add_sized(
                    [slider_w, 20.0],
                    egui::Slider::new(&mut self.state.options.optical_baseline_factor, 0.0..=0.6)
                        .show_value(true)
                        .suffix(" ×")
                        .max_decimals(3),
                ).changed() {
                    self.auto_save();
                }
            } else {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tl!("Baseline shift:")).color(t.text));
                    let slider_w = (ui.available_width() - 8.0).max(120.0);
                    if ui.add_sized(
                        [slider_w, 20.0],
                        egui::Slider::new(&mut self.state.options.optical_baseline_factor, 0.0..=0.6)
                            .show_value(true)
                            .suffix(" ×")
                            .max_decimals(3),
                    ).changed() {
                        self.auto_save();
                    }
                });
            }
            ui.label(
                RichText::new(tl!("Shifting the baseline down ensures precise visual vertical centering inside bubbles."))
                    .color(t.text_faint)
                    .small(),
            );

            widgets::hairline(ui);
            ui.add_space(4.0);

            ui.label(RichText::new(tl!("PRESET STORAGE")).size(11.0).color(t.text_dim).strong());
            if let Some(path) = self.storage_path() {
                let dir_str = path.parent().map(|p| p.display().to_string()).unwrap_or_else(|| ".".into());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{}:", tl!("Folder"))).color(t.text_dim).small());
                    ui.add(
                        egui::Label::new(
                            RichText::new(&dir_str)
                                .color(t.text_faint)
                                .small(),
                        ).truncate(),
                    ).on_hover_text(&dir_str);
                });
                let file_str = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "typesetter_state.json".into());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{}:", tl!("File"))).color(t.text_dim).small());
                    ui.add(
                        egui::Label::new(
                            RichText::new(&file_str)
                                .color(t.text_faint)
                                .small(),
                        ).truncate(),
                    ).on_hover_text(path.display().to_string());
                });
            }

            ui.horizontal_wrapped(|ui| {
                if ui.button(tl!("Choose folder…")).on_hover_text(tl!("Choose a custom directory to store typesetter presets and state")).clicked() {
                    let mut dlg = rfd::FileDialog::new().set_title(tl!("Choose presets folder"));
                    if let Some(curr) = self.storage_path().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
                        dlg = dlg.set_directory(&curr);
                    }
                    if let Some(folder) = dlg.pick_folder() {
                        self.custom_storage_dir = Some(folder.clone());
                        self.auto_save();
                        self.status_message = Some(format!("{}: {}", tl!("Storage folder set"), folder.display()));
                        self.status_error = false;
                    }
                }
                if ui.button(tl!("Open folder")).on_hover_text(tl!("Open presets folder in file manager")).clicked() {
                    if let Some(path) = self.storage_path() {
                        if let Some(dir) = path.parent() {
                            #[cfg(windows)]
                            {
                                let _ = std::process::Command::new("explorer").arg(dir).spawn();
                            }
                            #[cfg(target_os = "macos")]
                            {
                                let _ = std::process::Command::new("open").arg(dir).spawn();
                            }
                            #[cfg(not(any(windows, target_os = "macos")))]
                            {
                                let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
                            }
                        }
                    }
                }
                if ui.button(tl!("Save now")).clicked() {
                    self.auto_save();
                    self.status_message = Some(tl!("Presets saved").into());
                    self.status_error = false;
                }
                if ui.button(tl!("Load from file")).clicked() {
                    self.auto_load();
                    self.status_message = Some(tl!("Presets loaded from file").into());
                    self.status_error = false;
                }
                if self.custom_storage_dir.is_some() {
                    if ui.button(tl!("Reset folder")).on_hover_text(tl!("Reset presets storage to default AppData directory")).clicked() {
                        self.custom_storage_dir = None;
                        self.auto_save();
                        self.status_message = Some(tl!("Storage folder reset to default").into());
                        self.status_error = false;
                    }
                }
            });
        });
    }
}

// The Font Settings grid, measured from the mockup: a 545-point form, one 94-point caption
// column, a 6-point gap and one 445-point control column. A row is 26 high.
const FORM_LEFT_INSET: f32 = 6.0;
const LABEL_W: f32 = 94.0;
const COL_GAP: f32 = 6.0;
const RIGHT_W: f32 = 445.0;
const ROW_H: f32 = 26.0;
const FOOTER_H: f32 = 30.0;
const PREVIEW_H: f32 = 28.0;

/// The inline editor of the style preview's sample text, kept in egui's temporary memory so the
/// preset borrow that draws the form cannot own it.
#[derive(Clone, Debug, Default)]
struct PreviewEdit {
    idx: usize,
    buffer: String,
    original: String,
    request_focus: bool,
}

/// Cuts `s` to at most 200 characters on a `char` boundary, marking the cut with an ellipsis.
fn truncate_chars(s: &mut String, max: usize) {
    if s.chars().count() <= max {
        return;
    }
    let mut cut = s.char_indices().nth(max).map_or(s.len(), |(i, _)| i);
    // Count the ellipsis towards the limit, like the sample line in the mockup.
    cut = s[..cut].char_indices().nth(max.saturating_sub(1)).map_or(0, |(i, _)| i);
    s.truncate(cut);
    s.push('…');
}

/// What the preview's inline editor commits: one line, trimmed, at most 200 characters. `None`
/// means the user left the effective sample untouched, so an empty `preview_text` stays empty and
/// keeps following the style name.
fn preview_text_after_commit(buffer: &str, original: &str, current: &str) -> Option<String> {
    // A CRLF pair is one line break, not two: collapse `\r\n` into a single space first, then
    // turn any lone `\r`/`\n` into a space as well.
    let mut normalized = buffer.trim().replace("\r\n", " ").replace(['\r', '\n'], " ");
    truncate_chars(&mut normalized, 200);
    if normalized == original || normalized == current {
        None
    } else {
        Some(normalized)
    }
}

fn commit_preview_edit(preset: &mut TextStylePreset, buffer: &str, original: &str) -> bool {
    let Some(value) = preview_text_after_commit(buffer, original, &preset.preview_text) else {
        return false;
    };
    preset.preview_text = value;
    true
}

/// One caption cell of the form: fixed 94-point column, vertically centred, `tl!`-translated.
fn form_label(ui: &mut egui::Ui, key: &str) {
    ui.allocate_ui_with_layout(
        vec2(LABEL_W, ROW_H),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_size(vec2(LABEL_W, ROW_H));
            ui.label(format!("{}:", tl!(key)));
        },
    );
}

/// Records a widget rect in egui's temporary memory so the kittest suite can assert the mockup
/// geometry without reaching into private layout state.
fn remember_rect(ui: &egui::Ui, name: &str, rect: egui::Rect) {
    ui.data_mut(|d| d.insert_temp(egui::Id::new(("typesetter-ui-rect", name)), rect));
}

/// One form row: the shared caption column, the 6-point gutter and the caller's 445-point
/// controls. Uses the parent's zero item spacing so the geometry does not drift.
fn form_row(ui: &mut egui::Ui, key: &str, controls: impl FnOnce(&mut egui::Ui)) {
    let row = ui.horizontal(|ui| {
        form_label(ui, key);
        ui.add_space(COL_GAP);
        // The mockup's control *column* is the 445-point cell every row shares, not the widget
        // that happens to sit in it: the 217-point Anti-alias combo still lives in the same
        // column as a full-width field. Record the allocated cell so the rows line up.
        let column = std::cell::Cell::new(None);
        ui.allocate_ui_with_layout(
            vec2(RIGHT_W, ROW_H),
            Layout::left_to_right(Align::Center),
            |ui| {
                controls(ui);
                column.set(Some(ui.max_rect()));
            },
        );
        if let Some(rect) = column.get() {
            remember_rect(ui, &format!("controls:{key}"), rect);
        }
    });
    remember_rect(ui, &format!("row:{key}"), row.response.rect);
}

/// A section rule and its title. The caller controls the space before the rule and after the
/// title, because the mockup pins each band to fixed offsets from the first row.
fn section_header(ui: &mut egui::Ui, key: &str, line_gap: f32) {
    widgets::hairline(ui);
    ui.add_space(line_gap);
    let title = ui.label(RichText::new(tl!(key)).strong().size(13.5));
    remember_rect(ui, &format!("header:{key}"), title.rect);
}

/// The three-line alignment icon and caption inside a 145×26 button. Drawn by hand so the icon
/// can sit flush with the text, while the accessibility label stays the translated caption.
fn alignment_button(
    ui: &mut egui::Ui,
    value: &str,
    label: &str,
    tooltip: &str,
    selected: bool,
) -> bool {
    let t = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(145.0, ROW_H), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), selected, label)
    });
    let response = response.on_hover_text(tooltip);
    let hovered = response.hovered();
    let (fill, border) = if selected {
        (t.accent_soft, t.accent)
    } else if hovered {
        (t.hover, t.field_border)
    } else {
        (t.field, t.field_border)
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(4),
        fill,
        Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );

    let icon = egui::Rect::from_min_size(
        egui::pos2(rect.min.x + 9.0, rect.center().y - 6.0),
        vec2(12.0, 12.0),
    );
    let stroke = Stroke::new(1.5, t.text);
    for row in 0..3 {
        let y = icon.min.y + 2.0 + row as f32 * 4.0;
        let (x0, x1) = match value {
            "left" => (icon.min.x, icon.min.x + 7.0),
            "right" => (icon.max.x - 7.0, icon.max.x),
            _ => (icon.center().x - 3.5, icon.center().x + 3.5),
        };
        ui.painter().line_segment([egui::pos2(x0, y), egui::pos2(x1, y)], stroke);
    }
    ui.painter().text(
        egui::pos2(icon.max.x + 8.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(12.5),
        t.text,
    );
    response.clicked()
}

/// `Чёткий (crisp)` in Russian, `Crisp` in English: the key is shown in brackets only when the
/// catalogue actually translated it.
fn anti_alias_label(value: &str) -> String {
    let key = match value {
        "crisp" => "Crisp",
        "sharp" => "Sharp",
        "smooth" => "Smooth",
        "strong" => "Strong",
        _ => "None",
    };
    let translated = crate::i18n::t(key);
    if translated == key {
        translated.to_string()
    } else {
        format!("{translated} ({value})")
    }
}

/// A compact Photoshop-style character toggle with a tooltip.
fn character_toggle(ui: &mut egui::Ui, value: &mut bool, label: &str, tooltip: &str) -> bool {
    let response = ui
        .add(
            egui::Button::new(RichText::new(label).strong())
                .selected(*value)
                .min_size(vec2(45.0, ROW_H)),
        )
        .on_hover_text(tooltip);
    if response.clicked() {
        *value = !*value;
        true
    } else {
        false
    }
}

/// The colour swatch and the hex field of the mockup. The field shows the colour as
/// `#RRGGBBAA` and applies whatever hex value is typed into it.
fn color_swatch_with_hex_sized(
    ui: &mut egui::Ui,
    color: &mut [u8; 4],
    id: &str,
    swatch_size: egui::Vec2,
    hex_gap: f32,
    hex_width: f32,
) -> bool {
    let uid = egui::Id::new(id);
    let hex_of = |c: &[u8; 4]| format!("#{:02X}{:02X}{:02X}{:02X}", c[0], c[1], c[2], c[3]);
    let mut hex: String = ui.data_mut(|d| d.get_temp::<String>(uid.with("hex"))).unwrap_or_default();
    let mut changed = false;

    let mut swatch = egui::Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]);
    // `color_edit_button_srgba` takes its footprint from `interact_size`; scaling it around the
    // call is the supported way to get the mockup's 43×24 / 28×24 swatches.
    let previous_interact_size = ui.spacing().interact_size;
    ui.spacing_mut().interact_size = swatch_size;
    let response = egui::color_picker::color_edit_button_srgba(
        ui,
        &mut swatch,
        egui::color_picker::Alpha::BlendOrAdditive,
    );
    ui.spacing_mut().interact_size = previous_interact_size;
    ui.add_space(hex_gap);
    if response.changed() {
        *color = [swatch.r(), swatch.g(), swatch.b(), swatch.a()];
        hex = hex_of(color);
        ui.data_mut(|d| d.insert_temp(uid.with("hex"), hex.clone()));
        changed = true;
    }

    if hex.is_empty() {
        hex = hex_of(color);
        ui.data_mut(|d| d.insert_temp(uid.with("hex"), hex.clone()));
    }

    let text_edit = egui::TextEdit::singleline(&mut hex)
        .char_limit(9)
        .font(egui::TextStyle::Monospace)
        .vertical_align(Align::Center);
    let hex_resp = ui.add_sized([hex_width, 26.0], text_edit);
    if hex_resp.changed() {
        ui.data_mut(|d| d.insert_temp(uid.with("hex"), hex.clone()));
        let trimmed = hex.trim();
        let stripped = trimmed.strip_prefix('#').unwrap_or(trimmed);
        if let Ok(val) = u32::from_str_radix(stripped, 16) {
            match stripped.len() {
                6 => {
                    *color = [
                        ((val >> 16) & 0xFF) as u8,
                        ((val >> 8) & 0xFF) as u8,
                        (val & 0xFF) as u8,
                        color[3],
                    ];
                    changed = true;
                }
                8 => {
                    *color = [
                        ((val >> 24) & 0xFF) as u8,
                        ((val >> 16) & 0xFF) as u8,
                        ((val >> 8) & 0xFF) as u8,
                        (val & 0xFF) as u8,
                    ];
                    changed = true;
                }
                _ => {}
            }
        }
    } else if hex_resp.lost_focus() {
        hex = hex_of(color);
        ui.data_mut(|d| d.insert_temp(uid.with("hex"), hex));
    }

    changed
}

/// The demonstration phrase: sample glyphs, not a UI string, so it stays Russian in every
/// language and is deliberately absent from the catalogues.
const PREVIEW_SAMPLE: &str = "Ах ты ж!.. Неужели это сработало?! 123";

/// The automatic sample: the style name and the demonstration phrase.
fn style_preview_auto_text(preset: &TextStylePreset) -> String {
    let name = preset.name.trim();
    let mut sample = if name.is_empty() {
        PREVIEW_SAMPLE.to_string()
    } else {
        format!("{name}: {PREVIEW_SAMPLE}")
    };
    truncate_chars(&mut sample, 200);
    sample
}

/// The sample shown in the style preview: the preset's own [`TextStylePreset::preview_text`] when
/// it has one, otherwise the automatic name + phrase sample.
fn style_preview_text(preset: &TextStylePreset) -> String {
    let custom = preset.preview_text.trim().replace(['\r', '\n'], " ");
    if custom.is_empty() {
        style_preview_auto_text(preset)
    } else {
        let mut sample = custom;
        truncate_chars(&mut sample, 200);
        sample
    }
}

fn style_preview_weight(style: &str) -> u16 {
    let s = style.to_ascii_lowercase();
    if s.contains("thin") {
        100
    } else if s.contains("extra light") || s.contains("extralight") || s.contains("ultra light") || s.contains("ultralight") {
        200
    } else if s.contains("light") {
        300
    } else if s.contains("medium") {
        500
    } else if s.contains("semi bold") || s.contains("semibold") || s.contains("demi bold") || s.contains("demibold") {
        600
    } else if s.contains("extra bold") || s.contains("extrabold") || s.contains("ultra bold") || s.contains("ultrabold") {
        800
    } else if s.contains("black") || s.contains("heavy") {
        900
    } else if s.contains("bold") {
        700
    } else {
        400
    }
}

fn style_preview_antialias(value: &str) -> AntiAlias {
    match value.to_ascii_lowercase().as_str() {
        "none" => AntiAlias::None,
        "sharp" => AntiAlias::Sharp,
        "smooth" => AntiAlias::Smooth,
        "strong" => AntiAlias::Strong,
        _ => AntiAlias::Crisp,
    }
}

/// Everything that changes the rendered preview, plus its pixel size. The texture is only
/// rebuilt when this key changes.
fn style_preview_key(preset: &TextStylePreset, width: f32, height: f32, scale: u32) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    style_preview_text(preset).hash(&mut h);
    preset.font_family.hash(&mut h);
    preset.font_postscript.hash(&mut h);
    preset.font_style.hash(&mut h);
    preset.font_size.to_bits().hash(&mut h);
    preset.leading.to_bits().hash(&mut h);
    preset.tracking.hash(&mut h);
    preset.horizontal_scale.to_bits().hash(&mut h);
    preset.bold.hash(&mut h);
    preset.italic.hash(&mut h);
    preset.all_caps.hash(&mut h);
    preset.small_caps.hash(&mut h);
    preset.superscript.hash(&mut h);
    preset.subscript.hash(&mut h);
    preset.underline.hash(&mut h);
    preset.strikethrough.hash(&mut h);
    preset.stroke_width.to_bits().hash(&mut h);
    preset.stroke_color.hash(&mut h);
    preset.text_color.hash(&mut h);
    preset.align.hash(&mut h);
    preset.anti_alias.hash(&mut h);
    (width.round() as u32).hash(&mut h);
    (height.round() as u32).hash(&mut h);
    scale.hash(&mut h);
    h.finish()
}

/// The point-text layer of the sample: one line by construction, at the preset's own size
/// clamped to the preview's 10–28 point range.
fn style_preview_layer(preset: &TextStylePreset, scale: u32) -> TextLayer {
    let sample = style_preview_text(preset).replace(['\r', '\n'], " ");
    let size = preset.font_size.clamp(10.0, 28.0) * scale as f32;
    let style_name = preset.font_style.to_ascii_lowercase();
    let native_bold = style_preview_weight(&preset.font_style) >= 600;
    let native_italic = style_name.contains("italic") || style_name.contains("oblique");
    let char_style = CharStyle {
        font_family: preset.font_family.clone(),
        font_style: preset.font_style.clone(),
        postscript_name: (!preset.font_postscript.is_empty()).then(|| preset.font_postscript.clone()),
        weight: style_preview_weight(&preset.font_style),
        italic: preset.italic || native_italic,
        size_pt: size,
        color: Color::WHITE,
        tracking: preset.tracking as f32,
        leading_pt: Some(preset.leading.clamp(4.0, 300.0) * scale as f32),
        horizontal_scale: (preset.horizontal_scale / 100.0).clamp(0.2, 3.0),
        underline: preset.underline,
        strikethrough: preset.strikethrough,
        faux_bold: preset.bold && !native_bold,
        faux_italic: preset.italic && !native_italic,
        caps: if preset.all_caps {
            Caps::AllCaps
        } else if preset.small_caps {
            Caps::SmallCaps
        } else {
            Caps::Normal
        },
        baseline: if preset.superscript {
            FontBaseline::Superscript
        } else if preset.subscript {
            FontBaseline::Subscript
        } else {
            FontBaseline::Normal
        },
        ..Default::default()
    };
    let paragraph = ParagraphStyle {
        align: match preset.align.as_str() {
            "left" => TextAlign::Left,
            "right" => TextAlign::Right,
            _ => TextAlign::Center,
        },
        ..Default::default()
    };
    TextLayer {
        text: sample.clone(),
        font_family: preset.font_family.clone(),
        size_pt: size,
        color: Color::WHITE,
        runs: vec![TextRun { len: sample.len(), style: char_style }],
        paragraphs: vec![ParagraphRun { len: sample.len(), style: paragraph }],
        // Point text is one line by construction; a box would wrap a long sample onto a second
        // line that the 28-point preview cannot show.
        shape: TextShape::Point,
        antialias: style_preview_antialias(&preset.anti_alias),
        ..Default::default()
    }
}

/// Rasterizes the style sample into a straight-alpha RGBA image: the faux outline in
/// `stroke_color` under the glyphs tinted with `text_color`.
fn style_preview_image(preset: &TextStylePreset, width: f32, height: f32, scale: u32) -> Option<egui::ColorImage> {
    let w = (width * scale as f32).round().max(1.0) as usize;
    let h = (height * scale as f32).round().max(1.0) as usize;
    let layer = style_preview_layer(preset, scale);
    let engine = photocraft_text::try_shared()?;
    let mut engine = engine.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let layout = engine.layout(&layer, 72.0);
    let bounds = layout.bounds()?;
    let text_w = bounds[2] - bounds[0];
    let text_h = bounds[3] - bounds[1];
    if !text_w.is_finite() || !text_h.is_finite() || text_w <= 0.0 || text_h <= 0.0 {
        return None;
    }
    let pad = preset.stroke_width.clamp(0.0, 30.0) * scale as f32;
    // Keep a small fixed safety margin for antialiased glyph edges. Fit only shrinks: a small
    // point size is never blown up to the frame. The outline is clipped by the preview bounds
    // instead of shrinking the text to make room for it.
    let fit = ((w as f32 - 6.0) / text_w).min((h as f32 - 6.0) / text_h).min(1.0);
    if !fit.is_finite() || fit <= 0.0 {
        return None;
    }
    let cx = (bounds[0] + bounds[2]) / 2.0;
    let cy = (bounds[1] + bounds[3]) / 2.0;
    let centered_x = w as f32 / 2.0;
    let edge_x = match preset.align.as_str() {
        "left" => 1.0 - (bounds[0] - cx) * fit,
        "right" => w as f32 - 1.0 - (bounds[2] - cx) * fit,
        _ => centered_x,
    };
    // Keep left/right preview samples near the center instead of sending short labels across
    // the whole field. This only positions the preview image; document text alignment is untouched.
    let max_alignment_shift = w as f32 * 0.12;
    let x = centered_x + (edge_x - centered_x).clamp(-max_alignment_shift, max_alignment_shift);
    let y = (h as f32 - (bounds[1] + bounds[3]) * fit) / 2.0;
    let transform = photocraft_geom::Affine::translate(f64::from(-cx), f64::from(-cy))
        .then(&photocraft_geom::Affine::scale(f64::from(fit)))
        .then(&photocraft_geom::Affine::translate(f64::from(x), f64::from(y)));
    // Reject oversized ink before the rasterizer allocates a surface (malformed fonts).
    let ink = photocraft_text::render::ink_rect(&layout, &transform);
    if ink.is_empty() || ink.width() > w as u32 * 4 || ink.height() > h as u32 * 4 {
        return None;
    }
    let rendered = photocraft_text::render::rasterize(
        &layout,
        &transform,
        photocraft_color::PixelFormat::new(photocraft_color::ColorMode::Rgb, photocraft_color::SampleType::U8, true),
        layer.antialias,
    );
    let mut pixels = vec![[0u8; 4]; w * h];
    rendered.surface.read_rgba8_into(photocraft_geom::Rect::new(0, 0, w as i32, h as i32), &mut pixels);

    // Keep the outline proportional when a long sample is fitted into the preview. The fit is
    // intentionally independent of outline width, so this cannot shrink text to make room for it.
    let radius = pad * fit;
    // Dilate the glyphs with a filled circular structuring element. Sampling only a few points
    // on the perimeter circle skipped the pixels between the rays and shredded the stroke into
    // needle-shaped copies; a filled disk reaches every pixel the stroke must cover, and the
    // coverage weight fades smoothly over the boundary pixel for anti-aliased edges.
    // Entries are (dx, dy, coverage_weight), one per device pixel of the disk.
    let mut offsets: Vec<(i32, i32, f32)> = Vec::new();
    if radius >= 0.5 && preset.stroke_color[3] > 0 {
        let r_ceil = radius.ceil() as i32;
        for dy in -r_ceil..=r_ceil {
            for dx in -r_ceil..=r_ceil {
                let dist = ((dx * dx + dy * dy) as f32).sqrt();
                if dist <= radius + 0.5 {
                    let coverage = (radius + 0.5 - dist).clamp(0.0, 1.0);
                    if coverage > 0.0 {
                        offsets.push((dx, dy, coverage));
                    }
                }
            }
        }
    }
    let stroke_a = preset.stroke_color[3] as f32 / 255.0;
    let text_a = preset.text_color[3] as f32 / 255.0;
    let stroke_rgb = [preset.stroke_color[0] as f32 / 255.0, preset.stroke_color[1] as f32 / 255.0, preset.stroke_color[2] as f32 / 255.0];
    let text_rgb = [preset.text_color[0] as f32 / 255.0, preset.text_color[1] as f32 / 255.0, preset.text_color[2] as f32 / 255.0];
    let mut out = vec![0u8; w * h * 4];
    // Only ink produces text coverage and the disk reaches at most radius + 0.5 px beyond it,
    // so scanning just the padded ink rect gives the same image while skipping empty margins.
    let pad = if offsets.is_empty() { 0 } else { (radius + 0.5).ceil() as i32 };
    let x_lo = (ink.x0 - pad).clamp(0, w as i32) as usize;
    let x_hi = (ink.x1 + pad).clamp(0, w as i32) as usize;
    let y_lo = (ink.y0 - pad).clamp(0, h as i32) as usize;
    let y_hi = (ink.y1 + pad).clamp(0, h as i32) as usize;
    for y in y_lo..y_hi {
        for x in x_lo..x_hi {
            let idx = y * w + x;
            let ta = pixels[idx][3] as f32 / 255.0 * text_a;
            let mut sa = 0.0f32;
            for (dx, dy, weight) in &offsets {
                let (sx, sy) = (x as i32 + dx, y as i32 + dy);
                if sx >= 0 && sy >= 0 && sx < w as i32 && sy < h as i32 {
                    let sample_a = pixels[sy as usize * w + sx as usize][3] as f32 / 255.0;
                    sa = sa.max(sample_a * weight);
                }
            }
            sa *= stroke_a;
            let stroke_weight = sa * (1.0 - ta);
            let out_a = ta + stroke_weight;
            if out_a > 0.0 {
                let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                let channel = |i: usize| q((text_rgb[i] * ta + stroke_rgb[i] * stroke_weight) / out_a);
                let o = idx * 4;
                out[o] = channel(0);
                out[o + 1] = channel(1);
                out[o + 2] = channel(2);
                out[o + 3] = q(out_a);
            }
        }
    }
    Some(egui::ColorImage::from_rgba_unmultiplied([w, h], &out))
}

fn style_preview_texture(ctx: &egui::Context, preset: &TextStylePreset, width: f32, height: f32, scale: u32) -> Option<egui::TextureHandle> {
    let key = style_preview_key(preset, width, height, scale);
    let id = egui::Id::new("typesetter-style-preview");
    if let Some((cached_key, texture)) = ctx.data(|d| d.get_temp::<(u64, egui::TextureHandle)>(id))
        && cached_key == key
    {
        return Some(texture);
    }
    let image = style_preview_image(preset, width, height, scale)?;
    let texture = ctx.load_texture("typesetter-style-preview", image, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, (key, texture.clone())));
    Some(texture)
}

/// Live preview of the preset's face and character settings, rendered by the same text engine
/// as document text.
fn render_style_preview(ui: &mut egui::Ui, preset: &TextStylePreset) {
    // The frame's 12-point inner margins already provide the mockup's padding; the image fills
    // the remaining 521×28 points.
    let width = ui.available_width().max(40.0);
    let height = PREVIEW_H;
    let scale = ui.ctx().pixels_per_point().ceil().clamp(1.0, 3.0) as u32;
    match style_preview_texture(ui.ctx(), preset, width, height, scale) {
        Some(texture) => {
            let response = ui.add(egui::Image::new((texture.id(), vec2(width, height))));
            // Geometry probe for the kittest suite: the 28-point image inside the 40-point frame.
            ui.data_mut(|d| {
                d.insert_temp(egui::Id::new("typesetter-preview-image-rect"), response.rect)
            });
        }
        None => {
            let t = Tokens::get(ui.ctx());
            ui.label(RichText::new(tl!("Preview unavailable")).color(t.text_faint).small());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::LayerContent;
    use photocraft_doc::text::{TextShape, VerticalAlign};
    use photocraft_engine::typesetter::MeasurementSystem;
    use serde_json::json;

    #[test]
    fn typesetter_action_applies_font_color_and_measurement_system() {
        let mut app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 400, "height": 400})).unwrap();

        // 1. Action with Optical measurement system and custom RGBA text color
        let action_optical = TypesetterAction {
            text: "Optical Text".into(),
            preset: TextStylePreset {
                name: "Test Optical".into(),
                font_size: 24.0,
                text_color: [255, 100, 50, 200],
                measurement_system: MeasurementSystem::Optical,
                ..TextStylePreset::default()
            },
        };

        app.handle_typesetter_action(action_optical);

        let st = app.session.active().unwrap();
        let layer_id = st.active_layer.unwrap();
        let layer = st.doc.layer(layer_id).unwrap();
        if let LayerContent::Text(t) = &layer.content {
            assert_eq!(t.text, "OPTICAL TEXT"); // all_caps default is true
            let style = &t.runs[0].style;
            assert_eq!(style.kerning, photocraft_doc::text::Kerning::Optical);
            let rgb = style.color.to_rgb();
            assert!((rgb[0] - (255.0 / 255.0)).abs() < 0.02);
            assert!((rgb[1] - (100.0 / 255.0)).abs() < 0.02);
            assert!((rgb[2] - (50.0 / 255.0)).abs() < 0.02);
            assert!((style.color.alpha - (200.0 / 255.0)).abs() < 0.02);
        } else {
            panic!("Expected text layer");
        }

        // 2. Action with Metric measurement system
        let action_metric = TypesetterAction {
            text: "Metric Text".into(),
            preset: TextStylePreset {
                name: "Test Metric".into(),
                font_size: 20.0,
                text_color: [0, 128, 255, 255],
                measurement_system: MeasurementSystem::Metric,
                all_caps: false,
                ..TextStylePreset::default()
            },
        };

        app.handle_typesetter_action(action_metric);

        let st = app.session.active().unwrap();
        let layer_id = st.active_layer.unwrap();
        let layer = st.doc.layer(layer_id).unwrap();
        if let LayerContent::Text(t) = &layer.content {
            assert_eq!(t.text, "Metric Text");
            let style = &t.runs[0].style;
            assert_eq!(style.kerning, photocraft_doc::text::Kerning::Metrics);
            let rgb = style.color.to_rgb();
            assert!((rgb[0] - 0.0).abs() < 0.02);
            assert!((rgb[1] - (128.0 / 255.0)).abs() < 0.02);
            assert!((rgb[2] - 1.0).abs() < 0.02);
        } else {
            panic!("Expected text layer");
        }
    }

#[test]
    fn typesetter_action_uses_preset_alignment_and_character_options() {
        let mut app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 400, "height": 400})).unwrap();

        app.handle_typesetter_action(TypesetterAction {
            text: "Left aligned".into(),
            preset: TextStylePreset {
                name: "Left aligned".into(),
                align: "left".into(),
                all_caps: false,
                small_caps: true,
                superscript: true,
                underline: true,
                strikethrough: true,
                ..TextStylePreset::default()
            },
        });

        let st = app.session.active().unwrap();
        let layer = st.doc.layer(st.active_layer.unwrap()).unwrap();
        let LayerContent::Text(t) = &layer.content else { panic!("Expected text layer") };
        assert_eq!(t.paragraphs[0].style.align, TextAlign::Left);
        assert_eq!(t.runs[0].style.caps, Caps::SmallCaps);
        assert_eq!(t.runs[0].style.baseline, FontBaseline::Superscript);
        assert!(t.runs[0].style.underline);
        assert!(t.runs[0].style.strikethrough);
    }

    #[test]
    fn typesetter_without_selection_inserts_point_text_centered_at_high_dpi() {
        let mut app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 800, "height": 600, "resolution": 300})).unwrap();
        app.hover_doc = Some([400.0, 300.0]);

        let (font_size, leading) = (24.0, 30.0);
        app.handle_typesetter_action(TypesetterAction {
            text: "One two\nThree four\nFive six".into(),
            preset: TextStylePreset {
                name: "DPI".into(),
                font_size,
                leading,
                all_caps: false,
                ..TextStylePreset::default()
            },
        });

        let st = app.session.active().unwrap();
        let layer_id = st.active_layer.unwrap();
        let layer = st.doc.layer(layer_id).unwrap();
        let LayerContent::Text(t) = &layer.content else { panic!("Expected text layer") };
        assert_eq!(t.shape, TextShape::Point, "no selection must not create an artificial box");

        let dpi = st.doc.resolution_dpi;
        let px_per_pt = dpi / 72.0;
        let leading_px = leading * px_per_pt;
        let expected_baseline = 300.0 - leading_px + font_size * px_per_pt * 0.7 * 0.5;

        // The whole block is centered on the click, not hung from its first baseline.
        assert!((t.transform.m[4] - 400.0).abs() < 0.01, "{:?}", t.transform.m);
        assert!((t.transform.m[5] - f64::from(expected_baseline)).abs() < 0.01, "{:?}", t.transform.m);

        // At 300 DPI all three lines survive: the old `leading * 10` box kept only two.
        let layout = photocraft_text::shared().lock().unwrap().layout(t, dpi);
        assert_eq!(layout.lines.len(), 3);
        assert!((layout.lines[0].baseline - 0.0).abs() < 0.01);
        assert!((layout.lines[2].baseline - 2.0 * leading_px).abs() < 0.01);
    }

    #[test]
    fn typesetter_with_selection_keeps_box_text_and_uses_baseline_shift() {
        let mut app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 400, "height": 300, "resolution": 300})).unwrap();
        app.run("select.all", json!({})).unwrap();

        let font_size = 24.0;
        let factor = app.typesetter.state.options.optical_baseline_factor;
        app.handle_typesetter_action(TypesetterAction {
            text: "Bubble".into(),
            preset: TextStylePreset {
                name: "Bubble".into(),
                font_size,
                leading: 30.0,
                all_caps: false,
                ..TextStylePreset::default()
            },
        });

        let st = app.session.active().unwrap();
        let layer_id = st.active_layer.unwrap();
        let layer = st.doc.layer(layer_id).unwrap();
        let LayerContent::Text(t) = &layer.content else { panic!("Expected text layer") };
        assert_eq!(
            t.shape,
            TextShape::Box { x: 0.0, y: 0.0, width: 400.0, height: 300.0 },
            "a selection keeps native paragraph text"
        );
        assert_eq!(t.paragraphs[0].style.vertical_align, VerticalAlign::Center);
        // Positive Baseline Shift raises glyphs; the slider's value is "shift down".
        assert!((t.runs[0].style.baseline_shift_pt + font_size * factor).abs() < 1e-4);
    }

    /// The style-settings window must be draggable. It used to be `.anchor`ed to the screen
    /// centre, which re-pinned it there every frame, so its title bar could not be dragged.
    #[test]
    fn font_settings_window_can_be_dragged() {
        use egui_kittest::Harness;

        let mut typer = TypesetterUi::default();
        if typer.state.presets.is_empty() {
            typer.state.presets.push(TextStylePreset::default());
        }
        typer.is_open = true;
        typer.editing_preset_idx = Some(0);
        let mut frames = 0u32;
        // The window is 585×685, so a 700-point-tall harness leaves it pinned against the top
        // edge and the drag cannot move it up. A 900×900 harness gives it room.
        let mut h = Harness::builder().with_size(egui::vec2(900.0, 900.0)).build_ui_state(
            move |ui, typer: &mut TypesetterUi| {
                crate::PhotocraftApp::setup_context(ui.ctx(), crate::theme::ThemeKind::ALL[0]);
                frames += 1;
                if frames < 2 {
                    // The named font families reach the context one pass after installation.
                    return;
                }
                typer.show(ui);
            },
            typer,
        );
        h.run_steps(3);
        let id = egui::Id::new("typer_font_settings_window");
        let before = h.ctx.memory(|m| m.area_rect(id)).expect("the style window is shown");
        let grab = egui::pos2(before.center().x, before.top() + 10.0);
        let drop = grab + egui::vec2(-80.0, -60.0);
        h.hover_at(grab);
        h.drag_at(grab);
        h.run_steps(1);
        h.hover_at(drop);
        h.run_steps(1);
        h.drop_at(drop);
        h.run_steps(2);
        let after = h.ctx.memory(|m| m.area_rect(id)).expect("the style window is still shown");
        assert!(after.top() < before.top() - 20.0, "the title drag must move the window up: {before:?} -> {after:?}");
        assert!(after.left() < before.left() - 20.0, "and sideways: {before:?} -> {after:?}");
    }

    /// The docked panel body is drawn by `render_embedded`; it must still open the style dialog,
    /// which used to be reachable only through the floating `show` path.
    #[test]
    fn docked_panel_opens_the_style_dialog() {
        use egui_kittest::Harness;

        let mut typer = TypesetterUi::default();
        if typer.state.presets.is_empty() {
            typer.state.presets.push(TextStylePreset::default());
        }
        typer.is_open = true;
        typer.editing_preset_idx = Some(0);
        let mut frames = 0u32;
        let mut h = Harness::builder().with_size(egui::vec2(900.0, 700.0)).build_ui_state(
            move |ui, typer: &mut TypesetterUi| {
                crate::PhotocraftApp::setup_context(ui.ctx(), crate::theme::ThemeKind::ALL[0]);
                frames += 1;
                if frames < 2 {
                    return;
                }
                let _ = typer.render_embedded(ui);
            },
            typer,
        );
        h.run_steps(3);
        let id = egui::Id::new("typer_font_settings_window");
        assert!(h.ctx.memory(|m| m.area_rect(id)).is_some(), "the docked panel opens the style dialog");
    }

    #[test]
    fn test_unified_script_editor_sync_and_operations() {
        let mut ui = TypesetterUi::default();
        // 1. Initial script load
        ui.state.load_script("Линия 1: Привет!\nЛиния 2: Мир!\nЛиния 3: Тайпер!");
        ui.sync_script_buffer_from_state();
        assert_eq!(ui.state.lines.len(), 3);
        assert_eq!(ui.script_buffer, "Линия 1: Привет!\nЛиния 2: Мир!\nЛиния 3: Тайпер!");

        // 2. User presses Backspace at start of line 2 (merging line 1 and line 2)
        ui.script_buffer = "Линия 1: Привет! Линия 2: Мир!\nЛиния 3: Тайпер!".to_string();
        ui.sync_state_from_script_buffer();
        assert_eq!(ui.state.lines.len(), 2);
        assert_eq!(ui.state.lines[0].raw, "Линия 1: Привет! Линия 2: Мир!");
        assert_eq!(ui.state.lines[0].text, "Привет! Линия 2: Мир!");
        assert_eq!(ui.state.lines[1].raw, "Линия 3: Тайпер!");
        assert_eq!(ui.state.lines[1].text, "Тайпер!");

        // 3. User presses Enter in the middle of line 1 (splitting into new line)
        ui.script_buffer = "Линия 1: Привет!\nНовая строка!\nЛиния 3: Тайпер!".to_string();
        ui.sync_state_from_script_buffer();
        assert_eq!(ui.state.lines.len(), 3);
        assert_eq!(ui.state.lines[1].text, "Новая строка!");
        assert_eq!(ui.state.lines[2].text, "Тайпер!");

        // 4. Height persistence
        ui.script_list_height = 240.0;
        let persisted = TypesetterPersistedState {
            dock_mode: ui.dock_mode,
            state: ui.state.clone(),
            selected_folder: ui.selected_folder.clone(),
            custom_storage_dir: ui.custom_storage_dir.clone(),
            favorite_fonts: ui.favorite_fonts.clone(),
            is_open: ui.is_open,
            script_list_height: ui.script_list_height,
        };
        let json_str = serde_json::to_string(&persisted).unwrap();
        let loaded: TypesetterPersistedState = serde_json::from_str(&json_str).unwrap();
        assert_eq!(loaded.script_list_height, 240.0);
    }

    /// The character toggles must all fit on a single row, like the mockup: a wrapped row used
    /// to push the last buttons onto a second line, and the panel then grew a scrollbar.
    #[test]
    fn font_settings_character_toggles_stay_on_one_row() {
        use egui_kittest::Harness;
        use egui_kittest::kittest::Queryable;

        const TOGGLES: [&str; 9] = ["B", "I", "TT", "Tт", "T¹", "T₁", "U", "S", "«»"];

        let mut typer = TypesetterUi::default();
        if typer.state.presets.is_empty() {
            typer.state.presets.push(TextStylePreset::default());
        }
        typer.is_open = true;
        typer.editing_preset_idx = Some(0);
        let mut frames = 0u32;
        let mut h = Harness::builder().with_size(egui::vec2(900.0, 700.0)).build_ui_state(
            move |ui, typer: &mut TypesetterUi| {
                crate::PhotocraftApp::setup_context(ui.ctx(), crate::theme::ThemeKind::ALL[0]);
                frames += 1;
                if frames < 2 {
                    // The named font families reach the context one pass after installation.
                    return;
                }
                typer.show(ui);
            },
            typer,
        );
        h.run_steps(3);
        let id = egui::Id::new("typer_font_settings_window");
        let area = h.ctx.memory(|m| m.area_rect(id)).expect("the style window is shown");

        let mut rects = Vec::new();
        for label in TOGGLES {
            rects.push(h.get_by_label(label).rect());
        }
        let top = rects[0].top();
        for (label, rect) in TOGGLES.iter().zip(&rects) {
            assert!((rect.top() - top).abs() < 1.0, "the {label} toggle left the first row: {rect:?}");
            assert!((rect.width() - 45.0).abs() <= 1.0, "the {label} toggle must be 45 points wide: {rect:?}");
            assert!((rect.height() - 26.0).abs() <= 1.0, "the {label} toggle must be 26 points high: {rect:?}");
            assert!(rect.left() >= area.left() && rect.right() <= area.right(), "the {label} toggle is outside the window: {rect:?} not in {area:?}");
        }
        // The panel keeps a fixed width, like Photoshop.
        assert!((area.width() - 585.0).abs() < 2.0, "the style window must keep its fixed 585-point width: {area:?}");
        assert!((area.height() - 685.0).abs() < 4.0, "the style window opens 685 points high: {area:?}");
    }

    /// The window is fixed in X and resizable in Y; the content grid is 545 wide.
    #[test]
    fn font_settings_window_is_585_wide() {
        use egui_kittest::Harness;

        let mut typer = TypesetterUi::default();
        if typer.state.presets.is_empty() {
            typer.state.presets.push(TextStylePreset::default());
        }
        typer.is_open = true;
        typer.editing_preset_idx = Some(0);
        let mut frames = 0u32;
        let mut h = Harness::builder().with_size(egui::vec2(900.0, 900.0)).build_ui_state(
            move |ui, typer: &mut TypesetterUi| {
                crate::PhotocraftApp::setup_context(ui.ctx(), crate::theme::ThemeKind::ALL[0]);
                frames += 1;
                if frames < 2 {
                    return;
                }
                typer.show(ui);
            },
            typer,
        );
        h.run_steps(3);
        let id = egui::Id::new("typer_font_settings_window");
        let area = h.ctx.memory(|m| m.area_rect(id)).expect("the style window is shown");
        assert!((area.width() - 585.0).abs() < 2.0, "fixed mockup width: {area:?}");
        assert!((area.height() - 685.0).abs() < 4.0, "default mockup height: {area:?}");
    }

    /// The preview frame is 545×40 and the image inside is 521×28, exactly the mockup sizes.
    #[test]
    fn font_settings_preview_frame_is_40_high() {
        use egui_kittest::Harness;

        let mut typer = TypesetterUi::default();
        if typer.state.presets.is_empty() {
            typer.state.presets.push(TextStylePreset::default());
        }
        typer.is_open = true;
        typer.editing_preset_idx = Some(0);
        let mut frames = 0u32;
        let mut h = Harness::builder().with_size(egui::vec2(900.0, 900.0)).build_ui_state(
            move |ui, typer: &mut TypesetterUi| {
                crate::PhotocraftApp::setup_context(ui.ctx(), crate::theme::ThemeKind::ALL[0]);
                frames += 1;
                if frames < 2 {
                    return;
                }
                typer.show(ui);
            },
            typer,
        );
        h.run_steps(3);

        let frame = h
            .ctx
            .data(|d| d.get_temp::<egui::Rect>(egui::Id::new("typesetter-preview-frame-rect")))
            .expect("the preview frame records its rect");
        assert!((frame.width() - 545.0).abs() <= 2.0, "the preview frame spans the form: {frame:?}");
        assert!((frame.height() - 40.0).abs() <= 1.0, "the preview frame is 40 high: {frame:?}");
        let image = h
            .ctx
            .data(|d| d.get_temp::<egui::Rect>(egui::Id::new("typesetter-preview-image-rect")))
            .expect("the preview image records its rect");
        assert!((image.width() - 521.0).abs() <= 2.0, "the image fills the frame's inner width: {image:?}");
        assert!((image.height() - 28.0).abs() <= 1.0, "the image is 28 high: {image:?}");
        assert!(frame.contains_rect(image), "the image sits inside the frame: {image:?} in {frame:?}");
    }

    /// Duplicate/Delete are 95/85 wide, Done is 77 and flush with the form's right edge; all are
    /// 30 high, as in the mockup.
    #[test]
    fn font_settings_footer_button_sizes() {
        use egui_kittest::Harness;
        use egui_kittest::kittest::Queryable;

        let mut typer = TypesetterUi::default();
        if typer.state.presets.is_empty() {
            typer.state.presets.push(TextStylePreset::default());
        }
        typer.is_open = true;
        typer.editing_preset_idx = Some(0);
        let mut frames = 0u32;
        let mut h = Harness::builder().with_size(egui::vec2(900.0, 900.0)).build_ui_state(
            move |ui, typer: &mut TypesetterUi| {
                crate::PhotocraftApp::setup_context(ui.ctx(), crate::theme::ThemeKind::ALL[0]);
                frames += 1;
                if frames < 2 {
                    return;
                }
                typer.show(ui);
            },
            typer,
        );
        h.run_steps(3);

        let duplicate = h.get_by_label("Duplicate").rect();
        let delete = h.get_by_label("Delete").rect();
        let done = h.get_by_label("Done").rect();
        assert!((duplicate.width() - 95.0).abs() <= 1.0, "Duplicate is 95 wide: {duplicate:?}");
        assert!((delete.width() - 85.0).abs() <= 1.0, "Delete is 85 wide: {delete:?}");
        assert!((done.width() - 77.0).abs() <= 1.0, "Done is 77 wide: {done:?}");
        for (name, rect) in [("Duplicate", duplicate), ("Delete", delete), ("Done", done)] {
            assert!((rect.height() - 30.0).abs() <= 1.0, "{name} is 30 high: {rect:?}");
        }
        let frame = h
            .ctx
            .data(|d| d.get_temp::<egui::Rect>(egui::Id::new("typesetter-preview-frame-rect")))
            .expect("the preview frame records its rect");
        assert!((done.right() - frame.right()).abs() <= 2.0, "Done is flush with the form's right edge: {done:?} vs {frame:?}");
        assert!((duplicate.left() - frame.left()).abs() <= 2.0, "Duplicate starts at the form's left edge: {duplicate:?} vs {frame:?}");
    }

    /// Font Settings is fully routed through `tl!`/`i18n::t`: no raw Cyrillic remains in the modal
    /// or its inline helpers. `Tт` is the small-caps glyph, not a UI string.
    #[test]
    fn font_settings_modal_has_no_raw_cyrillic() {
        let source = include_str!("snow_typesetter_ui.rs");
        let start = source.find("fn render_font_settings_modal").expect("the modal exists");
        let rest = &source[start..];
        let end = rest.find("\n    ///").unwrap_or(rest.len());
        let modal = rest[..end].replace("\"Tт\"", "\"\"");
        let offenders: Vec<(usize, &str)> = modal
            .lines()
            .enumerate()
            .filter(|(_, line)| line.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c)))
            .collect();
        assert!(offenders.is_empty(), "raw Cyrillic UI strings in Font Settings: {offenders:?}");
    }

    /// `Чёткий (crisp)` in Russian, no redundant `Crisp (crisp)` in English, and the key in
    /// brackets for translations that actually differ.
    #[test]
    fn anti_alias_labels_are_localized() {
        use crate::i18n::{self, Lang};

        let en = Lang::from_code("en").unwrap();
        let ru = Lang::from_code("ru").unwrap();
        let ja = Lang::from_code("ja").unwrap();
        i18n::with_language(en, || {
            assert_eq!(anti_alias_label("crisp"), "Crisp");
            assert_eq!(anti_alias_label("none"), "None");
        });
        i18n::with_language(ru, || {
            assert_eq!(anti_alias_label("crisp"), "Чёткий (crisp)");
            assert_eq!(anti_alias_label("sharp"), "Резкий (sharp)");
        });
        i18n::with_language(ja, || {
            assert_eq!(anti_alias_label("crisp"), "鮮明 (crisp)");
        });
    }

    /// Every caption starts on one 94-point column, and the stroke `Color:` keeps the mockup's
    /// stepped position left of the `Leading:`/`H-Scale (%)` axis.
    #[test]
    fn font_settings_rows_share_control_axis() {
        use egui_kittest::Harness;
        use egui_kittest::kittest::Queryable;

        let mut typer = TypesetterUi::default();
        if typer.state.presets.is_empty() {
            typer.state.presets.push(TextStylePreset::default());
        }
        typer.is_open = true;
        typer.editing_preset_idx = Some(0);
        let mut frames = 0u32;
        let mut h = Harness::builder().with_size(egui::vec2(900.0, 900.0)).build_ui_state(
            move |ui, typer: &mut TypesetterUi| {
                crate::PhotocraftApp::setup_context(ui.ctx(), crate::theme::ThemeKind::ALL[0]);
                frames += 1;
                if frames < 2 {
                    return;
                }
                typer.show(ui);
            },
            typer,
        );
        h.run_steps(3);

        let labels = [
            "Style name:", "Folder:", "Font:", "PostScript:", "Formatting:", "Alignment:",
            "Anti-alias:", "Size (pt):", "Tracking:", "Thickness (px):",
        ];
        let x = h.get_by_label(labels[0]).rect().left();
        for label in &labels[1..] {
            let left = h.get_by_label(label).rect().left();
            assert!((left - x).abs() <= 1.0, "the {label} caption must start at the shared axis: {left} vs {x}");
        }

        // Every control block is the same 445-point column with one left and one right edge.
        let block = |key: &str| {
            h.ctx
                .data(|d| d.get_temp::<egui::Rect>(egui::Id::new(("typesetter-ui-rect", format!("controls:{key}")))))
                .unwrap_or_else(|| panic!("the {key} row records its control block"))
        };
        let first = block("Style name");
        assert!((first.width() - 445.0).abs() <= 1.0, "the control column is 445 wide: {first:?}");
        for key in [
            "Style name", "Folder", "Font", "PostScript", "Formatting", "Alignment",
            "Anti-alias", "Color", "System type", "Size (pt)", "Tracking", "Thickness (px)",
        ] {
            let rect = block(key);
            assert!((rect.left() - first.left()).abs() <= 1.0, "the {key} controls share the left axis: {rect:?} vs {first:?}");
            assert!((rect.right() - first.right()).abs() <= 1.0, "the {key} controls share the right edge: {rect:?} vs {first:?}");
        }

        let colors: Vec<_> = h.get_all_by_label("Color:").map(|node| node.rect()).collect();
        assert_eq!(colors.len(), 2, "text and stroke colour have separate captions");
        assert!((colors[0].left() - x).abs() <= 1.0, "the text Color: caption sits on the shared axis: {:?}", colors[0]);
        let leading = h.get_by_label("Leading:").rect().left();
        // The mockup's stroke `Color:` caption stands 164 points into the control column: 290 in
        // the mockup's own coordinates (126 + 164), right behind the 116-point thickness field and
        // its 48-point gap. The step is measured against the column, because the mockup's window
        // starts at x = 7 while the harness centres the window, so the absolute 290 of the
        // mockup sheet is not a screen coordinate here.
        let step = first.left() + 164.0;
        assert!((colors[1].left() - step).abs() <= 3.0, "the stroke Color: caption keeps the mockup's step: {:?} vs {step}", colors[1]);
        assert!((colors[1].left() - leading).abs() > 50.0, "the stroke Color: caption must not align with Leading: {:?} vs {leading}", colors[1]);
    }

    /// The form bands sit on the mockup's fixed offsets from the first row (±3 points).
    #[test]
    fn font_settings_vertical_offsets_match_mockup() {
        use egui_kittest::Harness;
        use egui_kittest::kittest::Queryable;

        let mut typer = TypesetterUi::default();
        if typer.state.presets.is_empty() {
            typer.state.presets.push(TextStylePreset::default());
        }
        typer.is_open = true;
        typer.editing_preset_idx = Some(0);
        let mut frames = 0u32;
        let mut h = Harness::builder().with_size(egui::vec2(900.0, 1000.0)).build_ui_state(
            move |ui, typer: &mut TypesetterUi| {
                crate::PhotocraftApp::setup_context(ui.ctx(), crate::theme::ThemeKind::ALL[0]);
                frames += 1;
                if frames < 2 {
                    return;
                }
                typer.show(ui);
            },
            typer,
        );
        h.run_steps(3);

        let rect = |name: String| {
            let id = egui::Id::new(("typesetter-ui-rect", name.clone()));
            h.ctx
                .data(|d| d.get_temp::<egui::Rect>(id))
                .unwrap_or_else(|| panic!("the form records {name}"))
        };
        let base = rect("row:Style name".into()).top();
        let targets = [
            ("row:Folder".to_string(), 31.0),
            ("header:Typography and Font".to_string(), 74.0),
            ("row:Font".to_string(), 97.0),
            ("row:PostScript".to_string(), 130.0),
            ("row:Formatting".to_string(), 163.0),
            ("row:Alignment".to_string(), 197.0),
            ("row:Anti-alias".to_string(), 231.0),
            ("row:Color".to_string(), 263.0),
            ("header:Typography Size (Line Spacing Sync)".to_string(), 309.0),
            ("row:System type".to_string(), 333.0),
            ("row:Size (pt)".to_string(), 365.0),
            ("row:Tracking".to_string(), 398.0),
            ("header:Text Outline".to_string(), 444.0),
            ("row:Thickness (px)".to_string(), 465.0),
            ("header:Preview sample".to_string(), 511.0),
        ];
        for (name, y) in targets {
            let actual = rect(name.clone()).top() - base;
            assert!((actual - y).abs() <= 3.0, "{name} is at {actual} from the first row, expected {y}");
        }

        let frame = h
            .ctx
            .data(|d| d.get_temp::<egui::Rect>(egui::Id::new("typesetter-preview-frame-rect")))
            .expect("the preview frame records its rect");
        assert!((frame.top() - base - 530.0).abs() <= 3.0, "the preview frame starts at 530: {frame:?}");
        let done = h.get_by_label("Done").rect();
        assert!((done.top() - base - 602.0).abs() <= 3.0, "the footer starts at 602: {done:?}");
    }

    #[test]
    fn style_preview_fits_a_long_sample_on_one_line() {
        let mut preset = TextStylePreset::default();
        preset.name = "Long style ".repeat(20);
        let engine = photocraft_text::shared();

        let layer = style_preview_layer(&preset, 1);
        assert!(matches!(layer.shape, TextShape::Point), "the preview must be point text, not a wrapping box");
        let layout = engine.lock().unwrap().layout(&layer, 72.0);
        assert_eq!(layout.lines.len(), 1, "point text keeps a long sample on one line");

        let image = style_preview_image(&preset, 180.0, 28.0, 1).expect("the long preview fits after shrinking");

        assert_eq!(image.size, [180, 28]);
        let [w, h] = image.size;
        let mut ink = (w, h, 0usize, 0usize, 0usize);
        for y in 0..h {
            for x in 0..w {
                if image.pixels[y * w + x].a() > 0 {
                    ink.0 = ink.0.min(x);
                    ink.1 = ink.1.min(y);
                    ink.2 = ink.2.max(x);
                    ink.3 = ink.3.max(y);
                    ink.4 += 1;
                }
            }
        }
        assert!(ink.4 > 0, "the preview should contain rendered text");
        assert!(ink.0 >= 1 && ink.1 >= 1, "the ink keeps a 1 px safety margin: {ink:?}");
        assert!(ink.2 <= w - 2 && ink.3 <= h - 2, "the ink stays inside the image: {ink:?} for {w}×{h}");
        assert!(ink.2 - ink.0 > w * 3 / 4, "a long sample is fitted across the preview width: {ink:?}");
    }

    #[test]
    fn style_preview_alignment_moves_the_sample_without_wrapping() {
        let _ = photocraft_text::shared();
        let base = TextStylePreset {
            preview_text: "Aa".into(),
            font_size: 20.0,
            stroke_width: 0.0,
            ..TextStylePreset::default()
        };
        let bounds = |align: &str| {
            let preset = TextStylePreset { align: align.into(), ..base.clone() };
            let image = style_preview_image(&preset, 220.0, 48.0, 1).expect("preview image");
            let [w, h] = image.size;
            let mut left = w;
            let mut right = 0;
            for y in 0..h {
                for x in 0..w {
                    if image.pixels[y * w + x].a() > 0 {
                        left = left.min(x);
                        right = right.max(x);
                    }
                }
            }
            (left, right)
        };
        let left = bounds("left");
        let center = bounds("center");
        let right = bounds("right");
        assert!(left.0 > 0, "left-aligned preview ink stays inside its image: {left:?}");
        assert!(right.1 < 220, "right-aligned preview ink stays inside its image: {right:?}");
        assert!(left.0 < center.0, "left alignment starts before center: {left:?} vs {center:?}");
        assert!(right.1 > center.1, "right alignment ends after center: {right:?} vs {center:?}");
        let left_center = (left.0 + left.1) / 2;
        let right_center = (right.0 + right.1) / 2;
        assert!(
            right_center - left_center <= 220 * 25 / 100,
            "alignment should nudge the sample without sending it across the whole preview: {left:?} vs {right:?}"
        );
    }

    /// `preview_text` is the rendered sample and is part of the texture key, so editing it must
    /// rebuild the preview rather than reuse the cached texture.
    #[test]
    fn style_preview_uses_preview_text_and_cache_key() {
        let _ = photocraft_text::shared();
        let base = TextStylePreset {
            name: "Summary".into(),
            font_size: 20.0,
            ..TextStylePreset::default()
        };
        let custom = TextStylePreset {
            preview_text: "A much longer custom sample that the style shows".into(),
            ..base.clone()
        };
        assert_eq!(style_preview_text(&base), format!("Summary: {PREVIEW_SAMPLE}"), "an empty preview_text uses the automatic sample");
        assert_eq!(style_preview_text(&custom), "A much longer custom sample that the style shows");

        assert_ne!(
            style_preview_key(&base, 180.0, 28.0, 1),
            style_preview_key(&custom, 180.0, 28.0, 1),
            "preview_text must enter the texture key"
        );
        let base_image = style_preview_image(&base, 180.0, 28.0, 1).expect("base preview");
        let custom_image = style_preview_image(&custom, 180.0, 28.0, 1).expect("custom preview");
        assert_ne!(base_image.pixels, custom_image.pixels, "the two samples render differently");
    }

    /// An untouched automatic sample stays automatic; edits are trimmed to one line and capped.
    #[test]
    fn preview_text_commits_on_enter_and_esc_restores() {
        let mut preset = TextStylePreset {
            name: "Summary".into(),
            ..TextStylePreset::default()
        };
        let automatic = style_preview_text(&preset);

        assert!(!commit_preview_edit(&mut preset, &automatic, &automatic), "leaving the automatic sample untouched keeps preview_text empty");
        assert_eq!(preset.preview_text, "");

        assert!(commit_preview_edit(&mut preset, "  Custom  ", &automatic));
        assert_eq!(preset.preview_text, "Custom", "the commit trims the value");

        // Esc never reaches `commit_preview_edit`; the buffer is simply dropped, so the preset and
        // its original value stay as they were.
        let original = preset.preview_text.clone();
        let _cancel = preview_text_after_commit("Discarded", "Custom", &preset.preview_text);
        assert_eq!(preset.preview_text, original);

        // Clearing the field returns to the automatic sample.
        assert!(commit_preview_edit(&mut preset, "  ", &automatic));
        assert_eq!(preset.preview_text, "");

        let long = "x".repeat(250);
        assert!(commit_preview_edit(&mut preset, &long, &automatic));
        assert_eq!(preset.preview_text.chars().count(), 200);
        assert!(preset.preview_text.ends_with('…'));

        let multiline = "One\r\nTwo";
        assert!(commit_preview_edit(&mut preset, multiline, &automatic));
        assert_eq!(preset.preview_text, "One Two");
    }

    #[test]
    fn style_preview_porter_duff_text_over_stroke() {
        let _ = photocraft_text::shared();
        let preset = TextStylePreset {
            name: "Aa".into(),
            // A short explicit sample keeps `fit == 1.0`, so this test checks the stroke disk
            // rather than the downscaling of a long automatic sample.
            preview_text: "Aa".into(),
            font_size: 20.0,
            text_color: [0, 0, 0, 255], // Black text
            stroke_width: 4.0,
            stroke_color: [255, 255, 255, 255], // White stroke
            ..TextStylePreset::default()
        };

        let image = style_preview_image(&preset, 120.0, 40.0, 1).expect("preview image");
        let has_black_ink = image.pixels.iter().any(|p| p.r() < 30 && p.g() < 30 && p.b() < 30 && p.a() > 200);
        assert!(has_black_ink, "black text with white stroke must render dark inner pixels, not pure white");
        let has_white_stroke = image.pixels.iter().any(|p| p.r() > 220 && p.g() > 220 && p.b() > 220 && p.a() > 200);
        assert!(has_white_stroke, "white stroke must render white outer pixels");
    }

    #[test]
    fn style_preview_outline_does_not_scale_down_the_text() {
        let _ = photocraft_text::shared();
        let base = TextStylePreset {
            preview_text: "Outline".into(),
            font_size: 28.0,
            text_color: [0, 0, 0, 255],
            stroke_color: [255, 255, 255, 255],
            ..TextStylePreset::default()
        };
        let without_outline = style_preview_image(&TextStylePreset { stroke_width: 0.0, ..base.clone() }, 220.0, 48.0, 1).expect("plain preview");
        let with_outline = style_preview_image(&TextStylePreset { stroke_width: 30.0, ..base }, 220.0, 48.0, 1).expect("outlined preview");
        let dark_pixel_bounds = |image: &egui::ColorImage| {
            let [w, h] = image.size;
            let mut bounds = (w, h, 0, 0, 0);
            for y in 0..h {
                for x in 0..w {
                    let p = image.pixels[y * w + x];
                    if p.a() == 255 && p.r() == 0 && p.g() == 0 && p.b() == 0 {
                        bounds.0 = bounds.0.min(x);
                        bounds.1 = bounds.1.min(y);
                        bounds.2 = bounds.2.max(x);
                        bounds.3 = bounds.3.max(y);
                        bounds.4 += 1;
                    }
                }
            }
            bounds
        };
        assert_eq!(
            dark_pixel_bounds(&with_outline),
            dark_pixel_bounds(&without_outline),
            "the outline may be clipped, but it must not change the text scale or glyph pixels"
        );
    }

    #[test]
    fn style_preview_outline_stays_proportional_when_the_sample_is_fitted() {
        let _ = photocraft_text::shared();
        let preset = TextStylePreset {
            preview_text: "H".into(),
            font_size: 28.0,
            text_color: [0, 0, 0, 255],
            stroke_width: 4.0,
            stroke_color: [255, 255, 255, 255],
            ..TextStylePreset::default()
        };
        let stroke_to_text_ratio = |height| {
            let image = style_preview_image(&preset, 64.0, height, 1).expect("preview image");
            let mut stroke_pixels = 0usize;
            let mut text_pixels = 0usize;
            for pixel in image.pixels {
                if pixel.a() < 200 {
                    continue;
                }
                if pixel.r() > 220 && pixel.g() > 220 && pixel.b() > 220 {
                    stroke_pixels += 1;
                } else if pixel.r() < 30 && pixel.g() < 30 && pixel.b() < 30 {
                    text_pixels += 1;
                }
            }
            stroke_pixels as f32 / text_pixels.max(1) as f32
        };

        let full_size_ratio = stroke_to_text_ratio(48.0);
        let fitted_ratio = stroke_to_text_ratio(24.0);
        assert!(
            fitted_ratio <= full_size_ratio * 1.6,
            "fitting the sample must scale its outline too, rather than turning it into a blob: {fitted_ratio:.2} vs {full_size_ratio:.2}"
        );
    }

    /// Regression: the stroke must be a filled circular dilation of the glyphs. The previous
    /// implementation sampled only eight points on the perimeter circle, so pixels between the
    /// rays stayed transparent and the stroke looked like needle-shaped copies of the glyphs.
    /// Anchors are only full-coverage pixels (pure black, alpha 255): for those the disk
    /// guarantees every pixel within radius - 0.5 px to come out nearly opaque.
    #[test]
    fn style_preview_stroke_band_has_no_holes() {
        let _ = photocraft_text::shared();
        let preset = TextStylePreset {
            name: "Aa".into(),
            // See `style_preview_porter_duff_text_over_stroke`: the stroke band is measured at
            // the preset's own size, so the sample must not force `fit < 1.0`.
            preview_text: "Aa".into(),
            font_size: 28.0, // large enough for full-coverage cores inside the glyph stems
            text_color: [0, 0, 0, 255], // Black text
            stroke_width: 4.0,
            stroke_color: [255, 255, 255, 255], // White stroke
            ..TextStylePreset::default()
        };

        let image = style_preview_image(&preset, 140.0, 48.0, 1).expect("preview image");
        let [w, h] = image.size;
        let at = |x: i32, y: i32| image.pixels[y as usize * w + x as usize];
        let is_solid_ink = |p: egui::Color32| p.a() == 255 && p.r() == 0 && p.g() == 0 && p.b() == 0;

        // Every pixel within radius - 0.5 of solid ink sits deep inside the stroke band: the
        // disk centred on such a pixel covers that solid ink with weight 1, so the stroke must
        // fill it. Eight-point perimeter sampling leaves exactly such pixels transparent.
        let radius = preset.stroke_width;
        let mut band = 0usize;
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let p = at(x, y);
                if is_solid_ink(p) {
                    continue;
                }
                let mut near_ink = false;
                'near: for dy in -4i32..=4 {
                    for dx in -4i32..=4 {
                        if ((dx * dx + dy * dy) as f32).sqrt() > radius - 0.5 {
                            continue;
                        }
                        let (nx, ny) = (x + dx, y + dy);
                        if nx >= 0 && ny >= 0 && nx < w as i32 && ny < h as i32 && is_solid_ink(at(nx, ny)) {
                            near_ink = true;
                            break 'near;
                        }
                    }
                }
                if near_ink {
                    band += 1;
                    assert!(p.a() >= 240, "hole in the stroke band at ({x},{y}): {p:?} must be covered by the stroke");
                }
            }
        }
        assert!(band >= 40, "expected a solid stroke band around the glyphs, found only {band} covered pixels");
    }

    /// The right column of the Typography Size rows must share one grid line: the Leading and
    /// H-Scale (%) labels sit in equal 85 px cells in front of equal 86 px drag values that hug
    /// the panel edge, so their left edges must coincide.
    #[test]
    fn font_settings_leading_and_hscale_columns_align() {
        use egui_kittest::Harness;
        use egui_kittest::kittest::Queryable;

        let mut typer = TypesetterUi::default();
        if typer.state.presets.is_empty() {
            typer.state.presets.push(TextStylePreset::default());
        }
        typer.is_open = true;
        typer.editing_preset_idx = Some(0);
        let mut frames = 0u32;
        let mut h = Harness::builder().with_size(egui::vec2(900.0, 1600.0)).build_ui_state(
            move |ui, typer: &mut TypesetterUi| {
                crate::PhotocraftApp::setup_context(ui.ctx(), crate::theme::ThemeKind::ALL[0]);
                frames += 1;
                if frames < 2 {
                    // The named font families reach the context one pass after installation.
                    return;
                }
                typer.show(ui);
            },
            typer,
        );
        h.run_steps(3);

        let leading = h.get_by_label(format!("{}:", tl!("Leading")).as_str()).rect();
        let hscale = h.get_by_label("H-Scale (%):").rect();
        assert!(
            (leading.top() - hscale.top()).abs() > 4.0,
            "expected the two labels on different rows: {leading:?} vs {hscale:?}"
        );
        assert!(
            (leading.left() - hscale.left()).abs() < 1.0,
            "the Leading label {leading:?} and the H-Scale label {hscale:?} must share one left edge"
        );

        let value = |name: &str| {
            h.ctx
                .data(|d| d.get_temp::<egui::Rect>(egui::Id::new(("typesetter-ui-rect", name))))
                .unwrap_or_else(|| panic!("the {name} value records its rect"))
        };
        let leading_value = value("leading-value");
        let hscale_value = value("hscale-value");
        assert!((leading_value.width() - 86.0).abs() <= 1.0, "the Leading value is 86 wide: {leading_value:?}");
        assert!((hscale_value.width() - 86.0).abs() <= 1.0, "the H-Scale value is 86 wide: {hscale_value:?}");
        assert!((leading_value.right() - hscale_value.right()).abs() < 1.0, "both values hug the same right edge: {leading_value:?} vs {hscale_value:?}");
        let leading_block = h
            .ctx
            .data(|d| d.get_temp::<egui::Rect>(egui::Id::new(("typesetter-ui-rect", "controls:Size (pt)".to_string()))))
            .expect("the Size row records its block");
        assert!((leading_value.right() - leading_block.right()).abs() <= 1.0, "the Leading value ends at the 445-point column's right edge: {leading_value:?} vs {leading_block:?}");
    }
}
