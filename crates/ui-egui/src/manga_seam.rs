//! Desktop host bridge to `photocraft-manga-ui` and `photocraft-manga-core`.
//!
//! `photocraft-manga-ui` and `photocraft-manga-core` are desktop-only dependencies in
//! `crates/ui-egui/Cargo.toml`, so this module compiles against `manga-ui` on native targets and
//! provides empty wasm stubs to keep the browser build green.

/// PaintTool SAI Linework Curve parameters bridged from the Linework dock panel.
#[derive(Clone, Copy, Debug)]
pub struct SaiCurveParams {
    pub curve_type_str: &'static str,
    pub effective_tension: f64,
    pub is_density: bool,
    pub base_density: f32,
    pub min_density_ratio: f32,
    pub min_ratio: f64,
}

impl Default for SaiCurveParams {
    fn default() -> Self {
        Self {
            curve_type_str: "normal",
            effective_tension: photocraft_vector::sai_curve::SAI_DEFAULT_TENSION,
            is_density: false,
            base_density: 1.0,
            min_density_ratio: 0.0,
            min_ratio: 0.0,
        }
    }
}

/// Outcome of processing one effect row's scrubby interaction.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScrubOutcome {
    pub trigger_dnd: bool,
    pub is_scrubbing: bool,
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::collections::HashMap;
    use std::sync::Arc;

    use photocraft_doc::{DocId, Document, Layer, LayerContent, LayerId};
    use photocraft_engine::cmd::manga_cmds::typesetter_adapter;
    use photocraft_manga_core::snow_knuth_plass::{bind_orphan_prepositions, knuth_plass_bubble_wrap_ext, measure_token_width};
    use photocraft_manga_core::snow_typesetter::{
        TextStylePreset, TplImportReport, TypesetterState, contour_variants, detect_speech_bubble_bounds_in_surface, text_shape_candidates_with_bubble,
    };
    use photocraft_manga_ui::balloon_gizmo::{self, BalloonView};
    use photocraft_manga_ui::comic_balloon_panel::{self, BalloonHostContext, create_live_balloon_in_rect_commands};
    use photocraft_manga_ui::comic_canvas_capsule::{self, CapsuleCallbacks, CapsuleView};
    use photocraft_manga_ui::comic_pages_panel::{
        PageItemInfo, PagesPanelAction, PagesViewInfo, natural_cmp_str, natural_sort_paths, parse_page_number_from_name,
    };
    use photocraft_manga_ui::comic_sai_panel::{self, LineworkHostContext, PressureTargetMode, SaiCurveSync};
    use photocraft_manga_ui::comic_screentone_gizmo::{self, GizmoView, PreviewInput};
    use photocraft_manga_ui::comic_screentone_panel::{self, ScreentoneTargetInfo};
    use photocraft_manga_ui::layer_search::{self, SearchActivation, SearchDoc};
    use photocraft_manga_ui::snow_clean;
    use photocraft_manga_ui::snow_fx_scrub::{self, FxDragTarget, FxHudState};
    use photocraft_manga_ui::snow_typesetter_ui::{FontSampleSpec, TypesetterAction, TypesetterHost};
    use photocraft_manga_ui::view::{CanvasToolEvent, CanvasViewXform};
    use photocraft_manga_ui::{BridgeDocId, MangaUiState, OutCommand, theme as manga_theme};
    use photocraft_vector::sai_curve::SaiCurveType;
    use serde_json::{Value, json};

    use crate::PhotocraftApp;
    use crate::canvas::{ToolEvent, ViewXform, canvas_display, canvas_texture_options, display_image};
    use crate::dock::Group;
    use crate::font_preview::{NameStyle, name_size, paint_centered, paint_name};
    use crate::jobs_ui;
    use crate::notices;
    use crate::state::{Panels, Tool};

    use super::{SaiCurveParams, ScrubOutcome};

    /// Suitcase manga state held on [`PhotocraftApp`].
    #[derive(Default)]
    pub struct MangaSeamState {
        pub ui: MangaUiState,
        pub pages_thumbnails: HashMap<DocId, (u64, egui::TextureHandle)>,
        pub typesetter_text_shape_source: Option<u64>,
        pub typesetter_paste_fingerprint: Option<u64>,
        pub typesetter_paste_on_click_was_enabled: bool,
    }

    pub const MANGA_PANELS: &[(&str, &str, Group)] = &[
        ("screentone", "Screentones", Group::Color),
        ("balloon", "Balloons", Group::Color),
        ("linework", "Linework", Group::Layers),
        ("timelapse", "Timelapse", Group::History),
        ("pages", "Pages", Group::Character),
        ("typesetter", "TypeR Manga", Group::Character),
    ];

    const MANGA_IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "psd", "tif", "tiff"];

    fn to_manga_xf(xf: &ViewXform) -> CanvasViewXform {
        CanvasViewXform { rect: xf.rect, zoom: xf.zoom, center: xf.center, flip: xf.flip, rotation: xf.rotation, aspect: xf.aspect }
    }

    /// Captures a read-only [`photocraft_manga_bridge::HostSnapshot`] from `app`.
    pub fn host_snapshot(app: &PhotocraftApp) -> photocraft_manga_bridge::HostSnapshot {
        use photocraft_manga_bridge::{BridgeLayerId, DocRef, HostSnapshot};
        let docs: Vec<DocRef> = app
            .session
            .documents()
            .iter()
            .enumerate()
            .map(|(index, d)| DocRef {
                id: photocraft_manga_bridge::BridgeDocId(d.doc.id.0),
                index,
                name: d.doc.name.clone(),
                path: d.path.clone(),
                revision: d.revision,
                is_dirty: d.is_dirty(),
                size: (d.doc.size.width, d.doc.size.height),
            })
            .collect();
        let active_idx = app.session.active_index();
        let active_doc = active_idx.and_then(|i| docs.get(i).cloned());
        let (active_layer, selected_layers) = match app.session.active() {
            Some(st) => (st.active_layer.map(|l| BridgeLayerId(l.0)), st.selected_layers().into_iter().map(|l| BridgeLayerId(l.0)).collect()),
            None => (None, Vec::new()),
        };
        HostSnapshot {
            active_doc,
            active_doc_index: active_idx,
            active_layer,
            selected_layers,
            documents: docs,
            tool_id: app.active_tool().label(),
            locale: crate::i18n::current().code().to_owned(),
            foreground: app.session.tools.foreground,
            background: app.session.tools.background,
            flags: app.session.seam_flags(),
        }
    }

    fn to_manga_ev(ev: ToolEvent) -> CanvasToolEvent {
        match ev {
            ToolEvent::Down { x, y, pressure } => CanvasToolEvent::Down { x, y, pressure },
            ToolEvent::Move { x, y, pressure } => CanvasToolEvent::Move { x, y, pressure },
            ToolEvent::Up { x, y } => CanvasToolEvent::Up { x, y },
        }
    }

    fn sync_i18n() {
        photocraft_manga_ui::i18n::set_locale(crate::i18n::current().code());
    }

    /// Mirrors PhotoCraft's active theme tokens and locale into `photocraft-manga-ui`.
    pub fn sync_tokens(_app: &mut PhotocraftApp, ctx: &egui::Context) {
        sync_i18n();
        let t = crate::theme::Tokens::get(ctx);
        let mapped = manga_theme::Tokens {
            chrome: t.chrome,
            canvas: t.canvas,
            dock: t.dock,
            card: t.card,
            card_border: t.card_border,
            field: t.field,
            field_border: t.field_border,
            hover: t.hover,
            pressed: t.pressed,
            text: t.text,
            text_dim: t.text_dim,
            text_faint: t.text_faint,
            icon: t.icon,
            accent: t.accent,
            accent_soft: t.accent_soft,
            accent_border: t.accent_border,
            accent_text: t.accent_text,
            separator: t.separator,
            shadow: t.shadow,
            primary_bg: t.primary_bg,
            primary_text: t.primary_text,
            danger: t.danger,
            warning: t.warning,
            radius_sm: t.radius_sm,
            radius: t.radius,
            radius_lg: t.radius_lg,
            bevel: t.bevel,
            pro: t.pro,
            tab_strip: t.tab_strip,
            row_selected: t.row_selected,
        };
        manga_theme::Tokens::install(ctx, mapped);
    }

    /// True if `id` is a manga dock/floating panel handled by this seam.
    pub fn panel_owns(id: &str) -> bool {
        MANGA_PANELS.iter().any(|p| p.0 == id)
    }

    /// Display title for manga panel `id`, if owned by this seam.
    pub fn panel_title(id: &str) -> Option<&'static str> {
        MANGA_PANELS.iter().find(|p| p.0 == id).map(|p| p.1)
    }

    /// Dock group associated with manga panel `id`.
    pub fn panel_group(id: &str) -> Option<Group> {
        MANGA_PANELS.iter().find(|p| p.0 == id).map(|p| p.2)
    }

    /// Resolves aliases (localized/legacy labels or short IDs) to canonical manga panel IDs.
    pub fn normalize_panel(raw: &str) -> Option<&'static str> {
        let trimmed = raw.trim();
        for &(id, title, _) in MANGA_PANELS {
            if id.eq_ignore_ascii_case(trimmed) || title.eq_ignore_ascii_case(trimmed) {
                return Some(id);
            }
        }
        if trimmed.eq_ignore_ascii_case("TypeR") || trimmed.eq_ignore_ascii_case("Manga Typesetter") {
            return Some("typesetter");
        }
        if trimmed.eq_ignore_ascii_case("Screentone") {
            return Some("screentone");
        }
        if trimmed.eq_ignore_ascii_case("Balloon") || trimmed.eq_ignore_ascii_case("Manga Balloons") {
            return Some("balloon");
        }
        None
    }

    /// Initial floating geometry `[x, y, w, h]` for manga panel `id`.
    pub fn panel_default_rect(app: &PhotocraftApp, id: &str) -> [f32; 4] {
        let canvas = app.last_canvas_rect;
        let (x, y) = if canvas.is_positive() { (canvas.left() + 32.0, canvas.top() + 48.0) } else { (140.0, 140.0) };
        let (w, h) = match id {
            "typesetter" => (360.0, 560.0),
            "pages" => (280.0, 520.0),
            "screentone" | "balloon" | "linework" => (300.0, 480.0),
            "timelapse" => (290.0, 420.0),
            _ => (300.0, 420.0),
        };
        [x, y, w, h]
    }

    /// Reads the visibility flag for manga panel `id` from legacy [`Panels`].
    pub fn panel_shown(panels: &Panels, id: &str) -> Option<bool> {
        match id {
            "screentone" => Some(panels.screentone),
            "balloon" => Some(panels.balloon),
            "linework" => Some(panels.linework),
            "timelapse" => Some(panels.timelapse),
            "pages" => Some(panels.pages),
            "typesetter" => Some(panels.typesetter),
            _ => None,
        }
    }

    /// Mutable access to the visibility flag for manga panel `id` on legacy [`Panels`].
    pub fn panel_shown_mut<'a>(panels: &'a mut Panels, id: &str) -> Option<&'a mut bool> {
        match id {
            "screentone" => Some(&mut panels.screentone),
            "balloon" => Some(&mut panels.balloon),
            "linework" => Some(&mut panels.linework),
            "timelapse" => Some(&mut panels.timelapse),
            "pages" => Some(&mut panels.pages),
            "typesetter" => Some(&mut panels.typesetter),
            _ => None,
        }
    }

    fn is_panel_shown(app: &PhotocraftApp, id: &str) -> bool {
        if let Some(ws) = &app.ui.docking { ws.layout.contains(&id.to_owned()) } else { panel_shown(&app.ui.panels, id).unwrap_or(false) }
    }

    fn is_panel_active(app: &PhotocraftApp, id: &str) -> bool {
        if let Some(ws) = &app.ui.docking {
            fn active_in_node(node: &craft_ui::docking::Node<String>, target: &str) -> bool {
                match node {
                    craft_ui::docking::Node::Tabs { panels, active } => panels.get(*active).is_some_and(|p| p == target),
                    craft_ui::docking::Node::Dock { columns, .. } => columns.iter().any(|c| {
                        !c.groups.iter().all(|g| g.minimized) && c.groups.iter().any(|g| !g.minimized && g.panels.get(g.active).is_some_and(|p| p == target))
                    }),
                    craft_ui::docking::Node::Split { first, second, .. } => active_in_node(first, target) || active_in_node(second, target),
                    _ => false,
                }
            }
            ws.layout.root.iter().chain(ws.layout.floating.iter().map(|w| &w.root)).any(|n| active_in_node(n, id))
        } else {
            panel_shown(&app.ui.panels, id).unwrap_or(false)
        }
    }

    /// Window-menu checkmark state for manga panel commands (`window.panel.*` / `window.toggle.timelapse`).
    pub fn panel_checked(app: &PhotocraftApp, id: &str) -> Option<bool> {
        normalize_panel(id.strip_prefix("window.panel.").or_else(|| id.strip_prefix("window.toggle.")).unwrap_or(id)).map(|short| is_panel_shown(app, short))
    }

    fn dispatch_commands(app: &mut PhotocraftApp, cmds: Vec<OutCommand>) -> Option<Value> {
        let mut last = None;
        for cmd in cmds {
            match app.run(cmd.id, cmd.params) {
                Ok(v) => last = Some(v),
                Err(e) => {
                    app.ui.status = e;
                    app.ui.status_error = true;
                }
            }
        }
        last
    }

    fn active_screentone_info(app: &PhotocraftApp) -> Option<ScreentoneTargetInfo> {
        let st = app.session.active()?;
        let lid = st.active_layer?;
        let layer = st.doc.layer(lid)?;
        let LayerContent::Screentone(tone) = &layer.content else {
            return None;
        };
        Some(ScreentoneTargetInfo { doc_id: BridgeDocId(st.doc.id.0), layer_id: lid, spec: tone.spec.clone() })
    }

    fn vector_brush_list(app: &PhotocraftApp) -> Vec<(String, String)> {
        app.session.manga.vector_brushes.presets().iter().map(|p| (p.id.clone(), p.name.clone())).collect()
    }

    fn render_screentone(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
        let target = active_screentone_info(app);
        app.manga.ui.screentone_gizmo.sync_active_layer(&mut app.manga.ui.screentone_ui, target.as_ref());
        let out = comic_screentone_panel::panel(ui, &mut app.manga.ui.screentone_ui, target.as_ref());
        if out.detach_gizmo {
            app.manga.ui.screentone_gizmo.detach();
        }
        let doc_id = app.session.active().map(|st| BridgeDocId(st.doc.id.0));
        let last = dispatch_commands(app, out.commands);
        if out.expect_new_layer_bind
            && let (Some(did), Some(lid)) = (doc_id, last.and_then(|v| v.get("layer").and_then(Value::as_u64)))
        {
            app.manga.ui.screentone_gizmo.bind_layer(did, LayerId(lid));
        }
    }

    fn render_balloon(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
        let brushes = vector_brush_list(app);
        let (doc_id, doc_ref, active_layer, doc_dpi, fg, bg) = match app.session.active() {
            Some(st) => (
                Some(BridgeDocId(st.doc.id.0)),
                Some(st.doc.as_ref()),
                st.active_layer,
                st.doc.resolution_dpi.max(1.0),
                app.session.tools.foreground,
                app.session.tools.background,
            ),
            None => (None, None, None, 72.0, app.session.tools.foreground, app.session.tools.background),
        };
        let host = BalloonHostContext {
            doc: doc_ref,
            doc_id,
            active_layer,
            doc_dpi,
            fg_color: fg,
            bg_color: bg,
            vector_brush_presets: &brushes,
            tool_is_balloon: app.active_tool() == Tool::Balloon,
        };
        let out = comic_balloon_panel::panel(ui, &mut app.manga.ui.balloon_ui, &host);
        let last = dispatch_commands(app, out.commands);
        if let Some(bid) = last.as_ref().and_then(|v| v.get("balloonId").and_then(Value::as_u64)).and_then(|n| u32::try_from(n).ok()) {
            app.manga.ui.balloon_gizmo.active_balloon_id = Some(bid);
        }
        if let Some((msg, is_err)) = out.status_message {
            app.ui.status = msg;
            app.ui.status_error = is_err;
        }
        if out.switch_to_balloon_tool {
            app.ui.tool = Tool::Balloon;
            app.ui.remember_group_tool(Tool::Balloon);
        }
        if out.open_linework_panel {
            let _ = app.run("window.panel.activate", json!({ "panel": "linework" }));
        }
    }

    fn render_linework(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
        let brushes = vector_brush_list(app);
        let selected_stroke_width = crate::vector_ui::selected_stroke_width(app).map(|(_, _, w)| w);
        let active_curve_sync =
            app.ui.vector_curve.as_ref().map(|vc| SaiCurveSync { tension: vc.tension, auto_taper_ends: vc.auto_taper_ends, snap_close: vc.snap_close });
        let host = LineworkHostContext { vector_brush_presets: &brushes, selected_stroke_width, active_curve_sync };
        let out = comic_sai_panel::panel(ui, &mut app.manga.ui.linework_panel, &host);
        if out.changed {
            let panel = app.manga.ui.linework_panel.clone();
            app.ui.tool_options.vector_curve_width = panel.base_width;
            app.ui.tool_options.vector_curve_min_ratio = panel.min_size_ratio;
            app.ui.tool_options.vector_curve_tension = panel.tension as f32;
            app.ui.tool_options.vector_curve_tapered = panel.auto_taper_ends || panel.min_size_ratio < 0.999;
            app.ui.tool_options.vector_curve_closed = panel.closed_loop;
            if let Some(vc) = app.ui.vector_curve.as_mut() {
                vc.tension = panel.tension;
                vc.auto_taper_ends = panel.auto_taper_ends;
                vc.snap_close = panel.snap_close;
            }
        }
        let _ = dispatch_commands(app, out.commands);
        if let Some(w) = out.apply_stroke_width {
            crate::vector_ui::apply_stroke_width(app, w);
        }
        if out.switch_to_vector_curve_tool {
            app.ui.tool = Tool::VectorCurve;
            app.ui.remember_group_tool(Tool::VectorCurve);
        }
    }

    fn activate_page_by_index(app: &mut PhotocraftApp, idx: usize) -> bool {
        if !app.session.set_active(idx) {
            return false;
        }
        app.sync_views();
        if app.manga.ui.pages_panel.auto_sync_typer
            && let Some(doc_state) = app.session.documents().get(idx)
        {
            let path = doc_state.path.as_deref().map(std::path::Path::new);
            let title = &doc_state.doc.name;
            let target_page = path
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .and_then(parse_page_number_from_name)
                .or_else(|| parse_page_number_from_name(title))
                .unwrap_or(idx.saturating_add(1));
            let state = &mut app.manga.ui.typesetter.state;
            if state.current_page != target_page {
                state.current_page = target_page;
                if let Some(pos) = state.lines.iter().position(|l| l.page == target_page) {
                    state.current_line_idx = pos;
                }
            }
        }
        true
    }

    fn can_quick_save_page(doc: &Document, path: Option<&str>, is_dirty: bool) -> bool {
        is_dirty && path.is_some_and(|p| photocraft_engine::file_cmds::saves_in_place(p) && photocraft_engine::file_cmds::holds_at(doc, p))
    }

    fn render_pages(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
        let active_doc_idx = app.session.active_index();
        let total_docs = app.session.documents().len();
        let ctx = ui.ctx().clone();
        let save_in_progress = jobs_ui::foreground_save(app).is_some();

        let mut items = Vec::with_capacity(total_docs);
        for idx in 0..total_docs {
            let (title, path, is_dirty, dimensions, doc_id, doc_rev, can_quick_save) = {
                let Some(doc_state) = app.session.documents().get(idx) else {
                    continue;
                };
                let quick = !save_in_progress && can_quick_save_page(&doc_state.doc, doc_state.path.as_deref(), doc_state.is_dirty());
                (
                    doc_state.doc.name.clone(),
                    doc_state.path.as_deref().map(std::path::PathBuf::from),
                    doc_state.is_dirty(),
                    Some((doc_state.doc.size.width, doc_state.doc.size.height)),
                    doc_state.doc.id,
                    doc_state.revision,
                    quick,
                )
            };
            let cached = app.manga.pages_thumbnails.get(&doc_id).filter(|(r, _)| *r == doc_rev).map(|(_, h)| h.id());
            let thumbnail_texture = match cached {
                Some(id) => Some(id),
                None => app.session.documents().get(idx).map(|doc_state| doc_state.doc.clone()).map(|doc| {
                    let buf = photocraft_compose::thumbnail_buffer(&doc, 256);
                    let (display, _) = canvas_display(app, &doc, None);
                    let img = display_image(display.as_deref(), &buf);
                    let handle = ctx.load_texture(format!("pages-thumb-{}", doc_id.0), img, egui::TextureOptions::LINEAR);
                    let tex_id = handle.id();
                    app.manga.pages_thumbnails.insert(doc_id, (doc_rev, handle));
                    tex_id
                }),
            };
            let mut info = PageItemInfo::new(BridgeDocId(doc_id.0), idx, title, path, Some(idx) == active_doc_idx);
            info.is_dirty = is_dirty;
            info.can_quick_save = can_quick_save;
            info.dimensions = dimensions;
            info.thumbnail_texture = thumbnail_texture;
            let page_num = info.detected_page;
            info.total_lines = app.manga.ui.typesetter.state.lines.iter().filter(|l| l.page == page_num).count();
            info.applied_lines = app.manga.ui.typesetter.state.lines.iter().filter(|l| l.page == page_num && l.is_applied).count();
            items.push(info);
        }

        let view_info =
            PagesViewInfo { items, active_doc_index: active_doc_idx, chapter_folder_name: app.ui.chapter_folder_name.clone(), is_left_dock_split: false };
        if let Some(action) = app.manga.ui.pages_panel.render_embedded(ui, &view_info) {
            handle_pages_action(app, &ctx, action);
        }
    }

    fn doc_index_of(app: &PhotocraftApp, id: BridgeDocId) -> Option<usize> {
        app.session.documents().iter().position(|d| d.doc.id.0 == id.0)
    }

    fn handle_pages_action(app: &mut PhotocraftApp, ctx: &egui::Context, action: PagesPanelAction) {
        match action {
            PagesPanelAction::SelectPage(id) => {
                if let Some(idx) = doc_index_of(app, id) {
                    activate_page_by_index(app, idx);
                }
            }
            PagesPanelAction::ClosePage(id) => {
                if let Some(idx) = doc_index_of(app, id) {
                    let _ = crate::menus::invoke(app, ctx, "file.close", json!({ "document": idx }));
                }
            }
            PagesPanelAction::SavePage(id) => save_page(app, ctx, id),
            PagesPanelAction::OpenChapterFolder => open_chapter_folder(app),
            PagesPanelAction::AddPages => add_pages(app),
            PagesPanelAction::ReorderPages { from_id, to_id } => {
                if let (Some(from), Some(to)) = (doc_index_of(app, from_id), doc_index_of(app, to_id)) {
                    let _ = app.session.move_document(from, to);
                }
            }
            PagesPanelAction::ToggleAutoSync(flag) => app.manga.ui.pages_panel.auto_sync_typer = flag,
            PagesPanelAction::RevealInExplorer(path) => reveal_in_os(&path),
            PagesPanelAction::SortPagesNaturally => sort_pages_naturally(app),
            PagesPanelAction::ToggleSplitDock => {
                let _ = app.run("window.panel.group", json!({ "panel": "pages", "target": "typesetter" }));
            }
        }
    }

    fn save_page(app: &mut PhotocraftApp, ctx: &egui::Context, id: BridgeDocId) {
        if jobs_ui::foreground_save(app).is_some() {
            notices::post(app, "Save already in progress", Vec::new(), false, None);
            return;
        }
        let Some(idx) = doc_index_of(app, id) else {
            notices::post(app, "Could not save page", vec![format!("Document #{} no longer exists.", id.0)], true, None);
            return;
        };
        let can_save = app.session.documents().get(idx).is_some_and(|d| can_quick_save_page(&d.doc, d.path.as_deref(), d.is_dirty()));
        if !can_save {
            notices::post(app, "Could not save page", vec!["This page cannot be saved in place. Use Save As.".into()], true, None);
            return;
        }
        if !activate_page_by_index(app, idx) {
            return;
        }
        if let Err(err) = crate::menus::invoke(app, ctx, "file.save", json!({})) {
            notices::post(app, "Could not save page", vec![err], true, None);
        }
    }

    fn open_chapter_folder(app: &mut PhotocraftApp) {
        let r = app.ask_folder(|app, folder| {
            let folder_path = std::path::PathBuf::from(&folder);
            let name = folder_path.file_name().and_then(|n| n.to_str()).unwrap_or("Chapter").to_string();
            app.ui.chapter_folder_name = Some(name);
            if let Ok(entries) = std::fs::read_dir(&folder_path) {
                let mut paths: Vec<std::path::PathBuf> = entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.is_file() && p.extension().and_then(|e| e.to_str()).is_some_and(|e| MANGA_IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
                    })
                    .collect();
                natural_sort_paths(&mut paths);
                let strs: Vec<String> = paths.iter().filter_map(|p| p.to_str().map(str::to_owned)).collect();
                if !strs.is_empty() {
                    app.open_paths(&strs);
                }
            }
            Ok(Value::Null)
        });
        if let Err(e) = r
            && e != crate::file_dialog::CANCELLED
        {
            app.ui.status = e;
            app.ui.status_error = true;
        }
    }

    fn add_pages(app: &mut PhotocraftApp) {
        let initial_dir = crate::file_dialog::last_used_dir(&app.ui.recent_files);
        let request = crate::file_dialog::FileDialogRequest::Open {
            multiple: true,
            initial_dir,
            extensions: Some(MANGA_IMAGE_EXTS.iter().map(|s| (*s).to_owned()).collect()),
        };
        let r = app.ask_file(request, |app, answer| {
            if let crate::file_dialog::FileDialogAnswer::Paths(paths) = answer {
                let mut bufs: Vec<std::path::PathBuf> = paths.into_iter().map(std::path::PathBuf::from).collect();
                natural_sort_paths(&mut bufs);
                let strs: Vec<String> = bufs.iter().filter_map(|p| p.to_str().map(str::to_owned)).collect();
                if !strs.is_empty() {
                    app.open_paths(&strs);
                }
            }
            Ok(Value::Null)
        });
        if let Err(e) = r
            && e != crate::file_dialog::CANCELLED
        {
            app.ui.status = e;
            app.ui.status_error = true;
        }
    }

    fn reveal_in_os(path: &std::path::Path) {
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("explorer").arg("/select,").arg(path).spawn();
        }
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        if let Some(parent) = path.parent() {
            let _ = std::process::Command::new("xdg-open").arg(parent).spawn();
        }
    }

    fn sort_pages_naturally(app: &mut PhotocraftApp) {
        let count = app.session.documents().len();
        if count <= 1 {
            return;
        }
        let mut sorted: Vec<(DocId, String)> = app.session.documents().iter().map(|d| (d.doc.id, d.doc.name.clone())).collect();
        sorted.sort_by(|a, b| natural_cmp_str(&a.1, &b.1));
        for (target_idx, (id, _)) in sorted.into_iter().enumerate() {
            if let Some(cur_idx) = app.session.documents().iter().position(|d| d.doc.id == id)
                && cur_idx != target_idx
            {
                let _ = app.session.move_document(cur_idx, target_idx);
            }
        }
    }

    struct AppTypesetterHost<'a> {
        app: &'a mut PhotocraftApp,
        picked_style: Option<String>,
    }

    impl TypesetterHost for AppTypesetterHost<'_> {
        fn paint_centered(&self, ui: &mut egui::Ui, family: &str, text: &str, size_pt: f32, rect: egui::Rect, color: egui::Color32) {
            paint_centered(ui, family, text, size_pt, rect, color);
        }

        fn name_size(&self, ctx: &egui::Context, spec: FontSampleSpec<'_>) -> Option<[f32; 2]> {
            let ns = NameStyle { family: spec.family, text: spec.text, size: spec.size_pt, bold: spec.bold, italic: spec.italic };
            name_size(ctx, &ns)
        }

        fn paint_name(&self, ui: &mut egui::Ui, spec: FontSampleSpec<'_>, rect: egui::Rect, color: egui::Color32) {
            let ns = NameStyle { family: spec.family, text: spec.text, size: spec.size_pt, bold: spec.bold, italic: spec.italic };
            paint_name(ui, &ns, rect, color);
        }

        fn is_favorite_font(&self, _ctx: &egui::Context, family: &str) -> bool {
            photocraft_engine::font_library_cmds::favorite_fonts(&self.app.session).iter().any(|f| f == family)
        }

        fn toggle_favorite_font(&mut self, _ctx: &egui::Context, family: &str) -> bool {
            self.app.run("type.font.favorite", json!({ "family": family })).is_ok()
        }

        fn font_family_picker(&mut self, ui: &mut egui::Ui, id_salt: &str, family: &mut String, width: f32) -> bool {
            if let Some(style) = crate::type_tool::family_picker(self.app, ui, id_salt, family, width) {
                self.picked_style = style;
                true
            } else {
                false
            }
        }

        fn font_styles(&self, family: &str) -> Vec<(String, String)> {
            crate::type_tool::styles(family)
                .into_iter()
                .map(|s| {
                    let label = crate::type_tool::style_label(&s);
                    (s, label)
                })
                .collect()
        }

        fn import_tpl(&mut self, state: &mut TypesetterState, data: &[u8], folder: &str) -> Result<TplImportReport, String> {
            typesetter_adapter::import_tool_presets_tpl(state, data, folder)
        }
    }

    fn render_typesetter(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
        let mut ts = std::mem::take(&mut app.manga.ui.typesetter);
        ts.is_open = true;
        let action = {
            let mut host = AppTypesetterHost { app, picked_style: None };
            ts.render_embedded(ui, &mut host)
        };
        app.manga.ui.typesetter = ts;
        if let Some(act) = action {
            handle_typesetter_action(app, act);
        }
        refresh_typesetter_contour(app);
    }

    /// Renders the body of manga panel `id` when owned by this seam. Returns `true` if handled.
    pub fn render_panel(app: &mut PhotocraftApp, ui: &mut egui::Ui, id: &str) -> bool {
        let seam = app.session.seam_flags();
        match id {
            "timelapse" => {
                if seam.timelapse {
                    crate::timelapse_ui::panel(app, ui);
                } else {
                    ui.label("Timelapse is disabled by seam.timelapse.");
                }
            }
            "screentone" => render_screentone(app, ui),
            "balloon" => render_balloon(app, ui),
            "linework" => render_linework(app, ui),
            "pages" => {
                if seam.pages {
                    render_pages(app, ui);
                } else {
                    ui.label("Pages is disabled by seam.pages.");
                }
            }
            "typesetter" => {
                if seam.typesetter {
                    render_typesetter(app, ui);
                } else {
                    ui.label("TypeR Manga is disabled by seam.typesetter.");
                }
            }
            _ => return false,
        }
        true
    }

    /// Panel menu entries specific to manga panels, if any.
    pub fn panel_menu(_app: &mut PhotocraftApp, _ui: &mut egui::Ui, _id: &str) {}

    /// Handles legacy dock/panel commands for manga panels when `app.ui.docking` is inactive.
    pub fn panel_command(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
        let short = id.strip_prefix("window.panel.").or_else(|| id.strip_prefix("window.toggle.")).and_then(normalize_panel)?;
        if app.ui.docking.is_some() {
            return None;
        }
        let show = match params.get("show").or_else(|| params.get("on")) {
            None => !panel_shown(&app.ui.panels, short).unwrap_or(false),
            Some(v) => match v.as_bool() {
                Some(b) => b,
                None => return Some(Err("show must be a boolean".into())),
            },
        };
        if let Some(slot) = panel_shown_mut(&mut app.ui.panels, short) {
            *slot = show;
        }
        Some(Ok(json!({ "showing": show })))
    }

    pub fn active_balloon_id(app: &PhotocraftApp) -> Option<u32> {
        app.manga.ui.balloon_gizmo.active_balloon_id
    }

    pub fn create_live_balloon_in_rect(app: &mut PhotocraftApp, rect: [f64; 4]) {
        let doc = app.session.active().map(|st| st.doc.as_ref());
        let cmds = create_live_balloon_in_rect_commands(&app.manga.ui.balloon_ui, doc, rect, app.session.tools.foreground, app.session.tools.background);
        let last = dispatch_commands(app, cmds);
        if let Some(bid) = last.as_ref().and_then(|v| v.get("balloonId").and_then(Value::as_u64)).and_then(|n| u32::try_from(n).ok()) {
            app.manga.ui.balloon_gizmo.active_balloon_id = Some(bid);
        }
    }

    pub fn sai_curve_params(app: &PhotocraftApp, tension: f64, min_ratio: f32) -> SaiCurveParams {
        let sai = &app.manga.ui.linework_panel;
        let is_density = sai.pressure_target == PressureTargetMode::Density;
        let curve_type_str = match sai.curve_type {
            SaiCurveType::Normal => "normal",
            SaiCurveType::Bezier => "bezier",
        };
        SaiCurveParams {
            curve_type_str,
            effective_tension: sai.curve_type.tension(tension),
            is_density,
            base_density: sai.base_density,
            min_density_ratio: sai.min_density_ratio,
            min_ratio: if is_density { 1.0 } else { f64::from(min_ratio) },
        }
    }

    pub fn sai_snap_close(app: &PhotocraftApp) -> bool {
        app.manga.ui.linework_panel.snap_close
    }

    pub fn sync_sai_base_width(app: &mut PhotocraftApp, width: f32) {
        app.manga.ui.linework_panel.base_width = width;
    }

    pub fn sync_sai_auto_taper(app: &mut PhotocraftApp, auto_taper: bool) {
        app.manga.ui.linework_panel.auto_taper_ends = auto_taper;
    }

    pub fn sync_sai_tension(app: &mut PhotocraftApp, tension: f64) {
        app.manga.ui.linework_panel.tension = tension;
    }

    pub fn sync_sai_snap_close(app: &mut PhotocraftApp, snap_close: bool) {
        app.manga.ui.linework_panel.snap_close = snap_close;
    }

    /// Hides the screentone layer being dragged live from the base document composite.
    pub fn screentone_display_doc(app: &PhotocraftApp, idx: usize) -> Option<(Arc<Document>, u64)> {
        let seam = app.session.seam_flags();
        if !seam.canvas_gizmos || app.session.active_index() != Some(idx) {
            return None;
        }
        let st = app.session.documents().get(idx)?;
        let hidden = app.manga.ui.screentone_gizmo.hide_layer_during_drag(BridgeDocId(st.doc.id.0), &st.doc, st.active_layer)?;
        Some((crate::distort_ui::without_layer(&st.doc, hidden), 1 << 60 | hidden.0))
    }

    /// Refreshes the screentone live drag preview texture when handles move.
    pub fn screentone_refresh_preview(app: &mut PhotocraftApp, ctx: &egui::Context, xf: &ViewXform, output: Option<u32>) {
        let seam = app.session.seam_flags();
        if !seam.canvas_gizmos {
            return;
        }
        let Some(st) = app.session.active() else {
            return;
        };
        let doc = st.doc.clone();
        let doc_idx = app.session.active_index().unwrap_or(0);
        let revision = st.revision;
        let active_layer = st.active_layer;
        let (display, display_key) = canvas_display(app, &doc, output);
        let factor = ((1.0 / xf.zoom).ceil() as u32).max(1);
        let panel_showing = is_panel_active(app, "screentone");
        let mxf = to_manga_xf(xf);
        let view = GizmoView { panel_showing, doc: &doc, doc_id: BridgeDocId(doc.id.0), active_layer, xf: &mxf };
        let input = PreviewInput { view: &view, doc_index: doc_idx, revision, display_key, texture_options: canvas_texture_options(factor) };
        let to_image = |buf: &photocraft_compose::Buffer| display_image(display.as_deref(), buf);
        comic_screentone_gizmo::refresh_preview(ctx, &input, &mut app.manga.ui.screentone_ui, &mut app.manga.ui.screentone_gizmo, &to_image);
    }

    /// Cancels any active screentone gizmo drag on Escape.
    pub fn screentone_escape(app: &mut PhotocraftApp, ctx: &egui::Context) {
        if !app.session.seam_flags().canvas_gizmos {
            return;
        }
        comic_screentone_gizmo::escape(ctx, &mut app.manga.ui.screentone_ui, &mut app.manga.ui.screentone_gizmo);
    }

    fn pointer_screentone(app: &mut PhotocraftApp, ev: ToolEvent, mods: egui::Modifiers, xf: &ViewXform) -> bool {
        let mxf = to_manga_xf(xf);
        let panel_showing = is_panel_active(app, "screentone");
        let Some(st) = app.session.active() else {
            return false;
        };
        let doc = st.doc.clone();
        let doc_id = BridgeDocId(doc.id.0);
        let active_layer = st.active_layer;
        let mut out = Vec::new();
        let handled = {
            let view = GizmoView { panel_showing, doc: &doc, doc_id, active_layer, xf: &mxf };
            comic_screentone_gizmo::pointer(to_manga_ev(ev), mods, &view, &mut app.manga.ui.screentone_ui, &mut app.manga.ui.screentone_gizmo, &mut out)
        };
        dispatch_commands(app, out);
        handled
    }

    fn pointer_balloon(app: &mut PhotocraftApp, ev: ToolEvent, mods: egui::Modifiers, xf: &ViewXform) -> bool {
        let Some(st) = app.session.active() else {
            return false;
        };
        let doc = st.doc.clone();
        let view = BalloonView {
            doc_id: Some(BridgeDocId(doc.id.0)),
            doc: Some(&doc),
            active_layer: st.active_layer,
            tool_is_balloon: app.active_tool() == Tool::Balloon,
            view_xform: to_manga_xf(xf),
        };
        let out = balloon_gizmo::pointer(&mut app.manga.ui.balloon_gizmo, &view, to_manga_ev(ev), mods);
        if let Some(updated) = out.updated_balloon {
            comic_balloon_panel::hydrate_geometry_from_balloon(&mut app.manga.ui.balloon_ui, &updated);
        }
        dispatch_commands(app, out.commands);
        out.handled
    }

    /// Processes screentone and balloon gizmo pointer events before canvas tools handle them.
    pub fn canvas_pointer(app: &mut PhotocraftApp, ev: ToolEvent, mods: egui::Modifiers) -> bool {
        let seam = app.session.seam_flags();
        if !seam.canvas_gizmos {
            return false;
        }
        let Some(xf) = ViewXform::active(app) else {
            return false;
        };
        pointer_screentone(app, ev, mods, &xf) || pointer_balloon(app, ev, mods, &xf)
    }

    /// Hover cursor for screentone and balloon gizmo handles over the canvas.
    pub fn canvas_hover_cursor(app: &PhotocraftApp, xf: &ViewXform, cursor_scr: egui::Pos2) -> Option<egui::CursorIcon> {
        if !app.session.seam_flags().canvas_gizmos {
            return None;
        }
        let st = app.session.active()?;
        let mxf = to_manga_xf(xf);
        let panel_showing = is_panel_active(app, "screentone");
        let sview = GizmoView { panel_showing, doc: &st.doc, doc_id: BridgeDocId(st.doc.id.0), active_layer: st.active_layer, xf: &mxf };
        if let Some(c) = comic_screentone_gizmo::hover_cursor(&sview, &app.manga.ui.screentone_ui, &app.manga.ui.screentone_gizmo, xf.to_doc(cursor_scr)) {
            return Some(c);
        }
        let bview = BalloonView {
            doc_id: Some(BridgeDocId(st.doc.id.0)),
            doc: Some(&st.doc),
            active_layer: st.active_layer,
            tool_is_balloon: app.active_tool() == Tool::Balloon,
            view_xform: mxf,
        };
        balloon_gizmo::hover_cursor(&app.manga.ui.balloon_gizmo, &bview, Some(cursor_scr))
    }

    /// Draws screentone and balloon gizmos onto the canvas overlay painter.
    pub fn draw_canvas_overlays(app: &PhotocraftApp, painter: &egui::Painter, xf: &ViewXform) {
        if !app.session.seam_flags().canvas_gizmos {
            return;
        }
        let Some(st) = app.session.active() else {
            return;
        };
        let mxf = to_manga_xf(xf);
        let panel_showing = is_panel_active(app, "screentone");
        let sview = GizmoView { panel_showing, doc: &st.doc, doc_id: BridgeDocId(st.doc.id.0), active_layer: st.active_layer, xf: &mxf };
        comic_screentone_gizmo::draw(painter, &sview, &app.manga.ui.screentone_ui, &app.manga.ui.screentone_gizmo);
        let bview = BalloonView {
            doc_id: Some(BridgeDocId(st.doc.id.0)),
            doc: Some(&st.doc),
            active_layer: st.active_layer,
            tool_is_balloon: app.active_tool() == Tool::Balloon,
            view_xform: mxf,
        };
        balloon_gizmo::draw(painter, &app.manga.ui.balloon_gizmo, &bview);
    }

    /// Removes a hovered balloon tail on Delete/Backspace when the Balloon tool is active.
    pub fn balloon_tail_delete_key(app: &mut PhotocraftApp, ctx: &egui::Context) -> bool {
        if !app.session.seam_flags().canvas_gizmos || app.ui.tool != Tool::Balloon {
            return false;
        }
        let mods = ctx.input(|i| i.modifiers);
        if mods.command || mods.ctrl || mods.shift {
            return false;
        }
        let pressed = ctx.input_mut(|i| i.consume_key(mods, egui::Key::Backspace) || i.consume_key(mods, egui::Key::Delete));
        if !pressed {
            return false;
        }
        if let Some(
            balloon_gizmo::GizmoHit::TailTip { balloon_id, tail_id }
            | balloon_gizmo::GizmoHit::TailBend { balloon_id, tail_id }
            | balloon_gizmo::GizmoHit::TailMouthCenter { balloon_id, tail_id }
            | balloon_gizmo::GizmoHit::TailMouthWidth { balloon_id, tail_id, .. },
        ) = app.manga.ui.balloon_gizmo.hovered
            && let Some(active) = app.session.active()
            && let Some(layer_id) = active.active_layer
        {
            let _ = app.run("shape.balloon.tail.remove", json!({ "layer": layer_id.0, "balloonId": balloon_id, "tailId": tail_id }));
            return true;
        }
        false
    }

    /// Layers panel footer button: creates a PaintTool SAI Linework layer and focuses the panel.
    pub fn layers_footer_linework_button(app: &mut PhotocraftApp, ui: &mut egui::Ui, actions: &mut Vec<(String, Value)>) {
        if crate::icons::button(ui, "spline", 26.0, false, crate::i18n::tr(crate::i18n::current(), "Create a Linework vector layer")).clicked() {
            actions.push(("layer.newLinework".into(), json!({ "width": app.manga.ui.linework_panel.base_width })));
            app.ui.tool = Tool::VectorCurve;
            app.ui.remember_group_tool(Tool::VectorCurve);
            actions.push(("window.panel.activate".into(), json!({ "panel": "linework" })));
        }
    }

    /// Handles scrubby dragging, value readout and Alt+Click HUD trigger on a layer-effect row.
    pub fn effect_row_scrub(
        ui: &egui::Ui,
        l: &Layer,
        fx_idx: Option<usize>,
        rect: egui::Rect,
        resp: &egui::Response,
        actions: &mut Vec<(String, Value)>,
    ) -> ScrubOutcome {
        let Some(idx) = fx_idx else {
            return ScrubOutcome { trigger_dnd: resp.drag_started(), is_scrubbing: false };
        };
        let Some(fx) = l.effects.items.get(idx) else {
            return ScrubOutcome { trigger_dnd: resp.drag_started(), is_scrubbing: false };
        };
        let Some(param) = snow_fx_scrub::primary_param(fx) else {
            return ScrubOutcome { trigger_dnd: resp.drag_started(), is_scrubbing: false };
        };
        let scrub = snow_fx_scrub::handle_effect_drag(
            ui.ctx(),
            resp,
            FxDragTarget { layer_id: l.id, effect_idx: idx, all_effects: &l.effects.items, allow_dnd: true },
            param,
        );
        if let Some(action) = scrub.action {
            actions.push(action);
        }
        if resp.clicked() && ui.input(|inp| inp.modifiers.alt) {
            let anchor = snow_fx_scrub::compute_hud_anchor(ui.ctx(), rect, 190.0);
            ui.ctx().data_mut(|d| {
                d.insert_temp(snow_fx_scrub::hud_state_id(), FxHudState { layer_id: l.id, effect_idx: idx, anchor_pos: anchor });
            });
        }
        ScrubOutcome { trigger_dnd: scrub.trigger_dnd, is_scrubbing: scrub.is_scrubbing }
    }

    /// Formats the primary numeric parameter of an effect row for right-aligned display.
    pub fn effect_row_value_text(l: &Layer, fx_idx: Option<usize>) -> Option<String> {
        let fx = l.effects.items.get(fx_idx?)?;
        let param = snow_fx_scrub::primary_param(fx)?;
        Some(param.format((param.getter)(fx)))
    }

    /// Context menu item to open the effect's Quick Adjust HUD.
    pub fn effect_row_context_hud(ui: &mut egui::Ui, l: &Layer, fx_idx: Option<usize>, rect: egui::Rect) {
        let Some(idx) = fx_idx else {
            return;
        };
        if l.effects.items.get(idx).is_none() {
            return;
        }
        if ui.button(crate::i18n::tr(crate::i18n::current(), "Quick Adjust HUD…")).clicked() {
            let anchor = snow_fx_scrub::compute_hud_anchor(ui.ctx(), rect, 190.0);
            ui.ctx().data_mut(|d| {
                d.insert_temp(snow_fx_scrub::hud_state_id(), FxHudState { layer_id: l.id, effect_idx: idx, anchor_pos: anchor });
            });
            ui.close();
        }
        ui.separator();
    }

    /// Renders the compound effect HUD overlay if open for `doc`.
    pub fn render_fx_hud(ui: &egui::Ui, doc: &Document, actions: &mut Vec<(String, Value)>) {
        snow_fx_scrub::render_fx_hud(ui.ctx(), doc, actions);
    }

    /// Opens the global Layer & Text Search palette.
    pub fn open_layer_search(app: &mut PhotocraftApp) {
        app.manga.ui.layer_search_open = true;
    }

    /// Applies any queued typesetter requests, renders the Canvas Capsule and Layer Search palette.
    pub fn frame_overlays(app: &mut PhotocraftApp, ctx: &egui::Context) {
        sync_tokens(app, ctx);
        typesetter_requests(app, ctx);
        draw_typesetter_aux_windows(app, ctx);
        show_canvas_capsule(app, ctx);
        show_layer_search(app, ctx);
    }

    fn draw_typesetter_aux_windows(app: &mut PhotocraftApp, ctx: &egui::Context) {
        let seam = app.session.seam_flags();
        let open = seam.typesetter && is_panel_shown(app, "typesetter");
        app.manga.ui.typesetter.is_open = open;
        if !open {
            app.manga.ui.typesetter.flush_auto_save();
            return;
        }
        let mut ts = std::mem::take(&mut app.manga.ui.typesetter);
        let action = egui::Area::new(egui::Id::unique("typer-aux-host"))
            .fixed_pos(egui::pos2(0.0, 0.0))
            .interactable(false)
            .show(ctx, |ui| {
                let mut host = AppTypesetterHost { app, picked_style: None };
                ts.show(ui, &mut host)
            })
            .inner;
        app.manga.ui.typesetter = ts;
        if let Some(act) = action {
            handle_typesetter_action(app, act);
        }
    }

    struct AppCapsuleCallbacks<'a> {
        app: &'a mut PhotocraftApp,
    }

    impl CapsuleCallbacks for AppCapsuleCallbacks<'_> {
        fn paint_icon(&mut self, ui: &mut egui::Ui, rect: egui::Rect, name: &str, size: f32, color: egui::Color32) {
            crate::icons::paint(ui, rect, name, size, color);
        }

        fn icon_button(&mut self, ui: &mut egui::Ui, name: &str, size: f32, active: bool, tip: &str) -> egui::Response {
            crate::icons::button(ui, name, size, active, tip)
        }

        fn draw_type_controls(&mut self, ui: &mut egui::Ui) {
            crate::type_tool::capsule_controls(self.app, ui);
        }
    }

    fn tool_capsule_name(tool: Tool) -> &'static str {
        match tool {
            Tool::Move => "move",
            Tool::Type | Tool::VerticalType => "type",
            Tool::RectMarquee | Tool::EllipseMarquee | Tool::SingleRowMarquee | Tool::SingleColumnMarquee => "marquee",
            Tool::Lasso | Tool::PolygonLasso | Tool::MagneticLasso => "lasso",
            Tool::MagicWand => "magic_wand",
            Tool::ObjectSelection => "object_selection",
            Tool::Remove => "remove",
            _ => "other",
        }
    }

    fn show_canvas_capsule(app: &mut PhotocraftApp, ctx: &egui::Context) {
        let seam = app.session.seam_flags();
        if !seam.canvas_capsule
            || !app.ui.panels.canvas_capsule
            || app.ui.view.hides_chrome()
            || !app.ui.dialogs.is_empty()
            || app.ui.canvas_tool_menu.is_some()
            || app.ui.layer_menu.is_some()
            || app.ui.brush_picker.is_some()
            || egui::Popup::is_any_open(ctx)
            || app.session.active().is_none()
        {
            return;
        }
        let Some(xf) = ViewXform::active(app) else {
            return;
        };
        let active_tone = active_screentone_info(app);
        let (doc_id, doc_arc, active_layer, has_selection) = match app.session.active() {
            Some(st) => (
                Some(BridgeDocId(st.doc.id.0)),
                Some(st.doc.clone()),
                st.active_layer,
                st.doc.selection.as_ref().is_some_and(|s| !s.content_bounds().is_empty()),
            ),
            None => return,
        };
        let view = CapsuleView {
            visible: true,
            view_xform: to_manga_xf(&xf),
            doc: doc_arc.as_deref(),
            doc_id,
            active_layer,
            has_selection,
            tool_name: tool_capsule_name(app.active_tool()),
            is_desktop: true,
            screentone_target: active_tone,
        };
        let mut capsule = std::mem::take(&mut app.manga.ui.canvas_capsule);
        let mut tone = app.manga.ui.screentone_ui.clone();
        let out = {
            let mut cb = AppCapsuleCallbacks { app };
            comic_canvas_capsule::show(ctx, &mut capsule, &mut tone, &view, &mut cb)
        };
        app.manga.ui.canvas_capsule = capsule;
        app.manga.ui.screentone_ui = tone;
        if out.detach_screentone_gizmo {
            app.manga.ui.screentone_gizmo.detach();
        }
        dispatch_commands(app, out.commands);
        if out.action_remove_selection {
            let _ = crate::retouch_ui::remove_selection(app);
        }
        if out.open_screentone_panel {
            let _ = app.run("window.panel.activate", json!({ "panel": "screentone" }));
        }
        if let Some(msg) = out.notice {
            app.ui.status = msg;
            app.ui.status_error = false;
        }
    }

    fn show_layer_search(app: &mut PhotocraftApp, ctx: &egui::Context) {
        if !app.manga.ui.layer_search_open {
            return;
        }
        let mut open = app.manga.ui.layer_search_open;
        let activation = {
            let active_idx = app.session.active_index();
            let docs: Vec<SearchDoc<'_>> = app
                .session
                .documents()
                .iter()
                .enumerate()
                .map(|(index, d)| SearchDoc { id: BridgeDocId(d.doc.id.0), index, name: &d.doc.name, active: Some(index) == active_idx, doc: &d.doc })
                .collect();
            layer_search::show(ctx, &mut open, &docs)
        };
        app.manga.ui.layer_search_open = open;
        if let Some(act) = activation {
            apply_search_activation(app, act);
        }
    }

    fn apply_search_activation(app: &mut PhotocraftApp, act: SearchActivation) {
        let Some(doc_idx) = doc_index_of(app, act.doc_id) else {
            return;
        };
        if app.session.active_index() != Some(doc_idx) {
            if !app.session.set_active(doc_idx) {
                return;
            }
            app.sync_views();
        }
        let _ = app.run("layer.select", json!({ "layer": act.layer.0 }));
        for gid in act.groups_to_expand {
            let _ = app.session.execute("layer.setExpanded", json!({ "layer": gid.0, "expanded": true }));
        }
        if let Some(i) = app.session.active_index().filter(|&i| i < app.ui.views.len()) {
            app.ui.views[i].center = act.center;
            app.ui.views[i].fit_pending = false;
        }
    }

    fn refresh_typesetter_contour(app: &mut PhotocraftApp) {
        if !app.session.seam_flags().typesetter {
            return;
        }
        let layer = app.session.active().and_then(|st| st.active_layer);
        app.manga.ui.typesetter.note_contour_layer(layer);
        if !app.manga.ui.typesetter.take_bubble_contour_request() {
            return;
        }
        let variants = match (app.session.active(), app.manga.ui.typesetter.state.active_line()) {
            (Some(state), Some(line)) => {
                let text = line.text.clone();
                let preset = app.manga.ui.typesetter.state.active_preset().cloned().unwrap_or_default();
                let selection = state.doc.selection.as_ref().map(|s| s.content_bounds()).filter(|b| !b.is_empty());
                let bubble = selection.or_else(|| {
                    let anchor =
                        app.hover_doc.map(|[x, y]| (x as f32, y as f32)).unwrap_or((state.doc.size.width as f32 * 0.5, state.doc.size.height as f32 * 0.5));
                    typesetter_adapter::detect_speech_bubble_bounds(&app.manga.ui.typesetter.state, &state.doc, anchor)
                });
                contour_variants(&text, &preset, bubble, state.doc.resolution_dpi, 12)
            }
            _ => Vec::new(),
        };
        app.manga.ui.typesetter.set_contour_variants(variants);
    }

    fn typesetter_requests(app: &mut PhotocraftApp, ctx: &egui::Context) {
        let seam = app.session.seam_flags();
        if ctx.data_mut(|d| d.remove_temp::<bool>(egui::Id::unique("photocraft_toggle_pages_cmd"))).unwrap_or(false) && seam.pages {
            let _ = app.run("window.panel.pages", json!({}));
        }
        if !seam.typesetter {
            return;
        }
        if app.manga.ui.typesetter.take_document_style_scan_request()
            && let Some(st) = app.session.active()
        {
            let doc = st.doc.clone();
            let (added, updated) = app.manga.ui.typesetter.state.scan_document_text_styles(&doc);
            app.manga.ui.typesetter.auto_save();
            app.ui.status = format!("Scanned text styles: +{added}, updated {updated}");
            app.ui.status_error = false;
        }
        if let Some(path) = app.manga.ui.typesetter.take_document_style_file_scan_request() {
            scan_psd_file_text_styles(app, &path);
        }
        if app.manga.ui.typesetter.take_text_shape_request() {
            refresh_typesetter_text_shape(app, true);
        }
        refresh_text_shape_previews_from_page(app);
        if let Some((layer, text)) = app.manga.ui.typesetter.take_text_shape_apply_request() {
            apply_typesetter_text_shape(app, layer, text);
        }
        if app.manga.ui.typesetter.take_align_request() {
            align_typesetter_layer(app);
        }
        if let Some(preset) = app.manga.ui.typesetter.take_apply_style_request() {
            if let Some(st) = app.session.active() {
                app.manga.typesetter_paste_fingerprint = st.doc.selection.as_ref().map(snow_clean::selection_fingerprint);
            }
            apply_typesetter_style_to_active_layer(app, &preset);
        }
        if app.manga.ui.typesetter.take_multi_bubble_capture_request() {
            capture_typesetter_bubble(app);
        }
        if app.manga.ui.typesetter.take_multi_bubble_fill_request() {
            fill_typesetter_bubbles(app);
        }
        maybe_typesetter_paste_on_click(app);
    }

    fn scan_psd_file_text_styles(app: &mut PhotocraftApp, path: &std::path::Path) {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("styles.psd");
        let bytes = match photocraft_format::read_file(path) {
            Ok(b) => b,
            Err(e) => {
                app.ui.status = format!("Scan PSD failed: {e}");
                app.ui.status_error = true;
                return;
            }
        };
        match photocraft_io::import(name, &bytes) {
            Ok(imported) => {
                let (added, updated) = app.manga.ui.typesetter.state.scan_document_text_styles(&imported.document);
                app.manga.ui.typesetter.auto_save();
                app.ui.status = format!("Scanned PSD styles: +{added}, updated {updated}");
                app.ui.status_error = false;
            }
            Err(e) => {
                app.ui.status = format!("Scan PSD failed: {e}");
                app.ui.status_error = true;
            }
        }
    }

    fn active_text_layer_id(st: &photocraft_engine::DocState) -> Option<LayerId> {
        let is_text = |id: LayerId| matches!(st.doc.layer(id).map(|l| &l.content), Some(LayerContent::Text(_)));
        st.active_layer.filter(|&id| is_text(id)).or_else(|| st.selected_layers().into_iter().rev().find(|&id| is_text(id)))
    }

    fn refresh_text_shape_previews_from_page(app: &mut PhotocraftApp) {
        let source = app.session.active().and_then(|st| {
            let id = active_text_layer_id(st)?;
            let layer = st.doc.layer(id)?;
            let LayerContent::Text(text) = &layer.content else {
                return None;
            };
            let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
            for byte in id.0.to_le_bytes().iter().chain(text.text.as_bytes()) {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            if let Some(sel) = st.doc.selection.as_ref() {
                let b = sel.content_bounds();
                for byte in b.x0.to_le_bytes().iter().chain(b.y0.to_le_bytes().iter()).chain(b.x1.to_le_bytes().iter()).chain(b.y1.to_le_bytes().iter()) {
                    hash ^= u64::from(*byte);
                    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
                }
            }
            Some(hash)
        });
        if source == app.manga.typesetter_text_shape_source {
            return;
        }
        app.manga.typesetter_text_shape_source = source;
        if source.is_some() {
            refresh_typesetter_text_shape(app, false);
        } else {
            app.manga.ui.typesetter.clear_text_shape_candidates();
        }
    }

    fn refresh_typesetter_text_shape(app: &mut PhotocraftApp, open_window: bool) {
        let Some(st) = app.session.active() else {
            return;
        };
        let Some(id) = active_text_layer_id(st) else {
            app.manga.ui.typesetter.clear_text_shape_candidates();
            return;
        };
        let Some(layer) = st.doc.layer(id) else {
            return;
        };
        let LayerContent::Text(text) = &layer.content else {
            app.manga.ui.typesetter.clear_text_shape_candidates();
            return;
        };
        let mut preset = TextStylePreset::default();
        if let Some(run) = text.char_runs().first() {
            preset.font_family = run.style.font_family.clone();
            preset.font_postscript = run.style.postscript_name.clone().unwrap_or_default();
            preset.font_size = run.style.size_pt;
            preset.leading = run.style.leading_pt.unwrap_or(run.style.size_pt * 1.2);
            preset.tracking = run.style.tracking as i32;
            preset.horizontal_scale = run.style.horizontal_scale;
        }
        let selection = st.doc.selection.clone();
        let detected_bubble = if selection.is_none() {
            let anchor = [text.transform.m[4] as i32, text.transform.m[5] as i32];
            let from_box = match text.shape {
                photocraft_doc::text::TextShape::Box { width, height, .. } if width > 20.0 && height > 20.0 => {
                    Some(photocraft_geom::Rect::from_xywh(anchor[0], anchor[1], width.round() as u32, height.round() as u32))
                }
                _ => None,
            };
            from_box
                .or_else(|| {
                    st.doc
                        .layers
                        .iter()
                        .rev()
                        .filter(|l| l.visible && l.id != id)
                        .find_map(|l| l.surface().and_then(|s| detect_speech_bubble_bounds_in_surface(s, anchor[0], anchor[1], 240)))
                })
                .or_else(|| {
                    let line_count = text.text.lines().count().max(1) as f32;
                    let max_chars = text.text.lines().map(|l| l.chars().count()).max().unwrap_or(10).max(4) as f32;
                    let font_size = preset.font_size.max(10.0);
                    let est_w = (max_chars.min(14.0) * font_size * 0.55).clamp(80.0, 180.0);
                    let est_h = (line_count * font_size * 1.25).clamp(40.0, 800.0);
                    Some(photocraft_geom::Rect::from_xywh(anchor[0], anchor[1], est_w.round() as u32, est_h.round() as u32))
                })
        } else {
            None
        };
        let opts = &app.manga.ui.typesetter.state.options;
        let candidates = text_shape_candidates_with_bubble(
            &text.text,
            &preset,
            selection.as_ref(),
            detected_bubble,
            st.doc.bounds(),
            st.doc.resolution_dpi,
            opts.line_spacing_sync,
            opts.minimum_fit_scale_percent,
        );
        match candidates {
            Ok(cands) => {
                if open_window {
                    app.manga.ui.typesetter.set_text_shape_candidates(cands, id.0, preset);
                } else {
                    app.manga.ui.typesetter.set_text_shape_candidates_inline(cands, id.0, preset);
                }
            }
            Err(error) => {
                app.manga.ui.typesetter.clear_text_shape_candidates();
                app.ui.status = format!("TextShapeR: {error}");
                app.ui.status_error = true;
            }
        }
    }

    fn apply_typesetter_text_shape(app: &mut PhotocraftApp, layer: u64, text: String) {
        let (new_point, new_hash) = if let Some(st) = app.session.active()
            && let Some(l) = st.doc.layer(LayerId(layer))
            && let LayerContent::Text(t) = &l.content
        {
            let font_size = t.char_runs().first().map_or(24.0, |r| r.style.size_pt);
            let leading = t.char_runs().first().and_then(|r| r.style.leading_pt).unwrap_or(font_size * 1.2);
            let old_lines = t.text.lines().count().max(1) as f64;
            let new_lines = text.lines().count().max(1) as f64;
            let cur_pt = [t.transform.m[4], t.transform.m[5]];
            let pt = match &st.doc.selection {
                Some(sel) if !sel.content_bounds().is_empty() => {
                    let b = sel.content_bounds();
                    let cx = f64::from(b.x0) + f64::from(b.width()) * 0.5;
                    let cy = f64::from(b.y0) + f64::from(b.height()) * 0.5;
                    let baseline_y = cy - (new_lines - 1.0) * f64::from(leading) * 0.5 + f64::from(font_size) * 0.225;
                    Some([cx, baseline_y])
                }
                _ => {
                    let new_y = cur_pt[1] - (new_lines - old_lines) * f64::from(leading) * 0.5;
                    Some([cur_pt[0], new_y])
                }
            };
            let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
            for byte in layer.to_le_bytes().iter().chain(text.as_bytes()) {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            (pt, Some(hash))
        } else {
            (None, None)
        };
        let mut edit_params = json!({ "layer": layer, "text": text });
        if let Some(pt) = new_point {
            edit_params["point"] = json!(pt);
        }
        if let Err(error) = app.run("type.edit", edit_params) {
            app.ui.status = error;
            app.ui.status_error = true;
            return;
        }
        let _ = app.run("type.setStyle", json!({ "layer": layer, "align": "center" }));
        if let Some(h) = new_hash {
            app.manga.typesetter_text_shape_source = Some(h);
        }
        app.manga.ui.typesetter.auto_save();
    }

    fn maybe_typesetter_paste_on_click(app: &mut PhotocraftApp) {
        if app.manga.ui.typesetter.is_blocking_canvas_insertion() {
            return;
        }
        if !app.manga.ui.typesetter.state.options.paste_on_click {
            app.manga.typesetter_paste_on_click_was_enabled = false;
            app.manga.typesetter_paste_fingerprint = None;
            return;
        }
        let Some(st) = app.session.active() else {
            app.manga.typesetter_paste_fingerprint = None;
            return;
        };
        let fingerprint = st.doc.selection.as_ref().map(snow_clean::selection_fingerprint);
        if !app.manga.typesetter_paste_on_click_was_enabled {
            app.manga.typesetter_paste_on_click_was_enabled = true;
            app.manga.typesetter_paste_fingerprint = fingerprint;
            return;
        }
        let Some(fp) = fingerprint else {
            app.manga.typesetter_paste_fingerprint = None;
            return;
        };
        if app.manga.typesetter_paste_fingerprint == Some(fp) {
            return;
        }
        app.manga.typesetter_paste_fingerprint = Some(fp);
        let Some(text) = app.manga.ui.typesetter.state.active_line().map(|l| l.text.clone()) else {
            return;
        };
        let line_idx = app.manga.ui.typesetter.state.current_line_idx;
        let Some(preset) = app.manga.ui.typesetter.state.preset_for_line(line_idx).cloned() else {
            return;
        };
        if app.manga.ui.typesetter.state.options.auto_clean
            && let Err(error) = typesetter_clean_selection(app)
        {
            app.ui.status = error;
            app.ui.status_error = true;
        }
        handle_typesetter_action(app, TypesetterAction { text, preset });
        app.manga.ui.typesetter.state.advance_line();
        app.manga.ui.typesetter.auto_save();
    }

    fn typesetter_clean_selection(app: &mut PhotocraftApp) -> Result<(), String> {
        let Some(st) = app.session.active() else {
            return Ok(());
        };
        let Some(selection) = st.doc.selection.clone() else {
            return Ok(());
        };
        let layer = st.active_layer.and_then(|id| st.doc.layer(id)).and_then(|l| match &l.content {
            LayerContent::Raster(s) if snow_clean::is_sampleable(s) => Some(s.clone()),
            _ => None,
        });
        let color = snow_clean::clean_fill_color(layer.as_ref(), &selection);
        let hex = format!("#{:02X}{:02X}{:02X}", color[0], color[1], color[2]);
        let existing = st.doc.layers.iter().find(|l| l.name == snow_clean::CLEAN_LAYER).map(|l| l.id.0);
        let id = match existing {
            Some(id) => id,
            None => {
                let created = app.run("layer.new.layer", json!({ "name": snow_clean::CLEAN_LAYER }))?;
                created.get("layer").and_then(Value::as_u64).ok_or("the clean layer was not created")?
            }
        };
        app.run("layer.select", json!({ "layer": id }))?;
        app.run("edit.fill", json!({ "color": hex }))?;
        Ok(())
    }

    fn capture_typesetter_bubble(app: &mut PhotocraftApp) {
        let Some(selection) = app.session.active().and_then(|st| st.doc.selection.clone()) else {
            app.ui.status = crate::i18n::tr(crate::i18n::current(), "Capture a selection first").into();
            app.ui.status_error = true;
            return;
        };
        app.manga.ui.typesetter.push_multi_bubble_capture(selection);
        let count = app.manga.ui.typesetter.multi_bubble_count();
        app.ui.status = format!("{}: {count}", crate::i18n::tr(crate::i18n::current(), "Bubble captured"));
        app.ui.status_error = false;
    }

    fn fill_typesetter_bubbles(app: &mut PhotocraftApp) {
        let captures = app.manga.ui.typesetter.drain_multi_bubble_captures();
        if captures.is_empty() {
            return;
        }
        let mut filled = 0usize;
        for selection in captures {
            let Some(text) = app.manga.ui.typesetter.state.active_line().map(|l| l.text.clone()) else {
                break;
            };
            let idx = app.manga.ui.typesetter.state.current_line_idx;
            let Some(preset) = app.manga.ui.typesetter.state.preset_for_line(idx).cloned() else {
                break;
            };
            if !typesetter_adapter::replace_selection_for_typesetter(&mut app.session, selection) {
                break;
            }
            if app.manga.ui.typesetter.state.options.auto_clean {
                let _ = typesetter_clean_selection(app);
            }
            handle_typesetter_action(app, TypesetterAction { text, preset });
            app.manga.ui.typesetter.state.advance_line();
            filled = filled.saturating_add(1);
        }
        app.ui.status = format!("{}: {filled}", crate::i18n::tr(crate::i18n::current(), "Filled bubbles"));
        app.ui.status_error = false;
        app.manga.ui.typesetter.auto_save();
    }

    fn apply_typesetter_style_to_active_layer(app: &mut PhotocraftApp, preset: &TextStylePreset) {
        let Some(target_id) = app.session.active().and_then(active_text_layer_id) else {
            app.ui.status = "Select a text layer on the page".into();
            app.ui.status_error = true;
            return;
        };
        let hex = format!("#{:02x}{:02x}{:02x}{:02x}", preset.text_color[0], preset.text_color[1], preset.text_color[2], preset.text_color[3]);
        let leading = if preset.leading > 0.0 { json!(preset.leading) } else { json!("auto") };
        let params = json!({
            "layer": target_id.0,
            "font": preset.font_family,
            "postscriptName": preset.font_postscript,
            "size": preset.font_size,
            "leading": leading,
            "tracking": preset.tracking,
            "horizontalScale": preset.horizontal_scale,
            "italic": preset.italic,
            "fauxBold": preset.bold,
            "color": hex,
            "align": preset.effective_align(),
            "firstLineIndent": preset.first_line_indent,
            "startIndent": preset.start_indent,
            "endIndent": preset.end_indent,
            "spaceAfter": preset.space_after,
            "hyphenate": preset.hyphenate,
            "hangingRoman": preset.hanging_punctuation,
            "direction": preset.direction
        });
        match app.run("type.setStyle", params) {
            Ok(_) => {
                if preset.stroke_width > 0.0 {
                    let stroke_hex = format!("#{:02x}{:02x}{:02x}", preset.stroke_color[0], preset.stroke_color[1], preset.stroke_color[2]);
                    let _ = app.run(
                        "layer.layerStyle.stroke",
                        json!({
                            "layer": target_id.0,
                            "size": preset.stroke_width,
                            "position": "outside",
                            "color": stroke_hex,
                            "opacity": (f32::from(preset.stroke_color[3]) / 2.55).round() as u32
                        }),
                    );
                }
                app.ui.status = format!("Applied style «{}»", preset.name);
                app.ui.status_error = false;
                app.manga.typesetter_text_shape_source = None;
                refresh_typesetter_text_shape(app, false);
            }
            Err(error) => {
                app.ui.status = format!("Could not apply style: {error}");
                app.ui.status_error = true;
            }
        }
    }

    fn align_typesetter_layer(app: &mut PhotocraftApp) {
        let h = app.run("layer.align.horizontalCenters", json!({}));
        let v = app.run("layer.align.verticalCenters", json!({}));
        match (h, v) {
            (Ok(_), Ok(_)) => {
                app.ui.status = crate::i18n::tr(crate::i18n::current(), "Layer centered in the selection").into();
                app.ui.status_error = false;
            }
            (Err(e), _) | (_, Err(e)) => {
                app.ui.status = format!("{}: {e}", crate::i18n::tr(crate::i18n::current(), "Align failed"));
                app.ui.status_error = true;
            }
        }
    }

    fn resolve_preset_font(preset: &TextStylePreset) -> (String, String, bool, bool, u16) {
        let resolved_family = {
            let mut shared = photocraft_text::shared().lock().unwrap_or_else(|e| e.into_inner());
            if !preset.font_family.is_empty() && shared.fonts.has_family(&preset.font_family) {
                preset.font_family.clone()
            } else if !preset.font_postscript.is_empty() {
                let r = shared.fonts.resolve_postscript(&preset.font_postscript);
                if shared.fonts.has_family(&r.family) {
                    r.family
                } else if !preset.font_family.is_empty() {
                    preset.font_family.clone()
                } else {
                    "Kurai Voice 2".to_string()
                }
            } else if !preset.font_family.is_empty() {
                preset.font_family.clone()
            } else {
                "Kurai Voice 2".to_string()
            }
        };
        let mut style_name = if !preset.font_style.is_empty() && preset.font_style != "Regular" {
            preset.font_style.clone()
        } else if preset.bold && preset.italic {
            "Bold Italic".to_string()
        } else if preset.bold {
            "Bold".to_string()
        } else if preset.italic {
            "Italic".to_string()
        } else {
            "Regular".to_string()
        };
        let (mut faux_bold, mut faux_italic, mut weight) = (false, false, 400u16);
        if preset.bold || preset.italic {
            let mut shared = photocraft_text::shared().lock().unwrap_or_else(|e| e.into_inner());
            let faces = shared.fonts.faces(&resolved_family);
            if preset.bold {
                let native_bold = faces
                    .iter()
                    .find(|f| {
                        f.italic == preset.italic && (f.weight >= 600.0 || f.style.to_lowercase().contains("bold") || f.style.to_lowercase().contains("black"))
                    })
                    .or_else(|| faces.iter().find(|f| f.weight >= 600.0 || f.style.to_lowercase().contains("bold")));
                if let Some(f) = native_bold {
                    style_name = f.style.clone();
                    weight = f.weight as u16;
                } else {
                    weight = 700;
                    faux_bold = true;
                }
            }
            if preset.italic
                && faces.iter().all(|f| !f.italic)
                && !style_name.to_lowercase().contains("italic")
                && !style_name.to_lowercase().contains("oblique")
            {
                faux_italic = true;
            }
        }
        (resolved_family, style_name, faux_bold, faux_italic, weight)
    }

    fn apply_markdown_runs(app: &mut PhotocraftApp, layer_id: u64, runs_text: &str, styled_segments: &[(String, bool, bool)]) {
        if styled_segments.is_empty() {
            return;
        }
        let chars: Vec<char> = runs_text.chars().collect();
        let mut runs = Vec::new();
        let mut at = 0usize;
        for (content, bold, italic) in styled_segments {
            let needle: Vec<char> = content.chars().collect();
            let mut index = at;
            let mut found = None;
            while index + needle.len() <= chars.len() {
                if chars.get(index..index + needle.len()).is_some_and(|s| s == needle.as_slice()) {
                    found = Some(index);
                    break;
                }
                index += 1;
            }
            if let Some(start) = found {
                runs.push(json!({ "start": start, "end": start + needle.len(), "fauxBold": bold, "fauxItalic": italic }));
                at = start + needle.len();
            }
        }
        if !runs.is_empty() {
            let _ = app.run("type.edit", json!({ "layer": layer_id, "runs": runs }));
        }
    }

    pub fn handle_typesetter_action(app: &mut PhotocraftApp, action: TypesetterAction) {
        if app.manga.ui.typesetter.is_blocking_canvas_insertion() {
            return;
        }
        let (selection_box, auto_clean_params, doc_dpi, doc_size) = {
            let Some(st) = app.session.active() else {
                app.ui.status = "Open or create a document to insert text".into();
                app.ui.status_error = true;
                return;
            };
            let sel_box = st.doc.selection.as_ref().and_then(|sel| {
                let b = sel.content_bounds();
                (!b.is_empty()).then_some([b.x0 as f32, b.y0 as f32, b.width() as f32, b.height() as f32])
            });
            let clean = if sel_box.is_some() && app.manga.ui.typesetter.state.options.auto_clean {
                st.doc.selection.as_ref().map(|sel| {
                    let art = st.active_layer.and_then(|id| st.doc.layer(id)).and_then(|l| l.surface());
                    let rgb = snow_clean::clean_fill_color(art, sel);
                    let hex = format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
                    let clean_id = st.doc.layers.iter().find(|l| l.name == snow_clean::CLEAN_LAYER).map(|l| l.id);
                    (clean_id, hex)
                })
            } else {
                None
            };
            (sel_box, clean, st.doc.resolution_dpi, st.doc.size)
        };

        let source_text = if action.preset.all_caps { action.text.to_uppercase() } else { action.text };
        let (mut final_text, styled_segments) = strip_typer_markdown(&source_text);
        let mut font_size = action.preset.font_size;
        let mut leading = if action.preset.leading > 0.0 { action.preset.leading } else { font_size * 1.2 };
        if action.preset.punctuation_only_scale > 1.0 && is_punctuation_only(&final_text) {
            font_size *= action.preset.punctuation_only_scale;
            leading *= action.preset.punctuation_only_scale;
        }
        if let Some([_bx, _by, bw, bh]) = selection_box {
            final_text =
                knuth_plass_bubble_wrap_ext(&final_text, bw, bh, font_size, doc_dpi, action.preset.horizontal_scale, action.preset.hanging_punctuation);
            let h_scale = if action.preset.horizontal_scale > 1.5 { action.preset.horizontal_scale / 100.0 } else { 1.0 };
            for _ in 0..6 {
                let lines = final_text.split('\n').count().max(1) as f32;
                let widest = final_text.split('\n').map(|l| measure_token_width(l, font_size, h_scale)).fold(0.0f32, f32::max);
                let factor = ((bw * 0.72) / widest.max(1.0)).min((bh * 0.72) / (lines * leading).max(1.0)).min(1.0);
                if factor > 0.97 {
                    break;
                }
                font_size = (font_size * factor).max(6.0);
                leading = (leading * factor).max(font_size * 1.05);
                final_text =
                    knuth_plass_bubble_wrap_ext(&final_text, bw, bh, font_size, doc_dpi, action.preset.horizontal_scale, action.preset.hanging_punctuation);
            }
        } else {
            final_text = bind_orphan_prepositions(&final_text);
        }

        let (resolved_family, style_name, faux_bold, faux_italic, weight) = resolve_preset_font(&action.preset);
        let hex_color = format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            action.preset.text_color[0], action.preset.text_color[1], action.preset.text_color[2], action.preset.text_color[3]
        );
        let is_optical = action.preset.measurement_system.is_optical();
        let caps_mode = if action.preset.all_caps {
            "all"
        } else if action.preset.small_caps {
            "small"
        } else {
            "normal"
        };
        let baseline_mode = if action.preset.superscript {
            "superscript"
        } else if action.preset.subscript {
            "subscript"
        } else {
            "normal"
        };
        let (click_x, click_y) = app.hover_doc.map_or((doc_size.width as f32 * 0.5, doc_size.height as f32 * 0.5), |[x, y]| (x as f32, y as f32));
        let runs_text = final_text.clone();
        let mut type_params = json!({
            "text": final_text,
            "size": font_size,
            "font": resolved_family,
            "fontStyle": style_name,
            "weight": weight,
            "italic": action.preset.italic,
            "fauxBold": faux_bold,
            "fauxItalic": faux_italic,
            "align": action.preset.effective_align(),
            "caps": caps_mode,
            "baseline": baseline_mode,
            "underline": action.preset.underline,
            "strikethrough": action.preset.strikethrough,
            "tracking": action.preset.tracking,
            "leading": leading,
            "horizontalScale": action.preset.horizontal_scale,
            "color": hex_color,
            "kerning": if is_optical { "optical" } else { "metrics" },
            "autoKern": if is_optical { "opticalKern" } else { "metricsKern" },
            "hangingRoman": action.preset.hanging_punctuation,
            "antialias": action.preset.anti_alias,
            "firstLineIndent": action.preset.first_line_indent,
            "startIndent": action.preset.start_indent,
            "endIndent": action.preset.end_indent,
            "spaceAfter": action.preset.space_after,
            "hyphenate": action.preset.hyphenate,
            "direction": action.preset.direction
        });
        if let Some([bx, by, bw, bh]) = selection_box {
            type_params["box"] = json!([bx, by, bw, bh]);
            type_params["verticalAlign"] = json!("center");
            let optical = font_size * app.manga.ui.typesetter.state.options.optical_baseline_factor;
            if optical != 0.0 {
                type_params["baselineShift"] = json!(-optical);
            }
        } else {
            let px_per_pt = if doc_dpi.is_finite() && doc_dpi > 0.0 { doc_dpi / 72.0 } else { 1.0 };
            let line_count = runs_text.lines().count().max(1);
            let baseline_y = click_y - ((line_count - 1) as f32 * (leading * px_per_pt)) * 0.5 + (font_size * px_per_pt) * 0.35;
            type_params["x"] = json!(click_x);
            type_params["y"] = json!(baseline_y);
        }
        if !action.preset.font_postscript.is_empty() {
            type_params["postscriptName"] = Value::String(action.preset.font_postscript.clone());
        }
        if let Some((clean_layer_id, hex_fill)) = auto_clean_params {
            if let Some(clean_id) = clean_layer_id {
                let _ = app.run("layer.select", json!({ "layer": clean_id.0, "mode": "replace" }));
            } else {
                let _ = app.run("layer.new.layer", json!({ "name": snow_clean::CLEAN_LAYER }));
            }
            let _ = app.run("edit.fill", json!({ "color": hex_fill }));
        }
        if let Ok(v) = app.run("type.create", type_params)
            && let Some(layer_id) = v.get("layer").and_then(Value::as_u64)
        {
            apply_markdown_runs(app, layer_id, &runs_text, &styled_segments);
            if action.preset.stroke_width > 0.0 {
                let stroke_hex = format!("#{:02x}{:02x}{:02x}", action.preset.stroke_color[0], action.preset.stroke_color[1], action.preset.stroke_color[2]);
                let _ = app.run(
                    "layer.layerStyle.stroke",
                    json!({
                        "layer": layer_id,
                        "size": action.preset.stroke_width,
                        "position": "outside",
                        "color": stroke_hex,
                        "opacity": (f32::from(action.preset.stroke_color[3]) / 2.55).round() as u32
                    }),
                );
            }
            let _ = app.run("select.deselect", json!({}));
        }
    }

    fn strip_typer_markdown(text: &str) -> (String, Vec<(String, bool, bool)>) {
        let chars: Vec<char> = text.chars().collect();
        let (mut plain, mut segment, mut segments) = (String::new(), String::new(), Vec::new());
        let (mut bold, mut italic) = (false, false);
        let flush = |segment: &mut String, bold: bool, italic: bool, segments: &mut Vec<(String, bool, bool)>| {
            if !segment.is_empty() && (bold || italic) {
                segments.push((std::mem::take(segment), bold, italic));
            } else {
                segment.clear();
            }
        };
        let mut index = 0usize;
        while index < chars.len() {
            if chars[index] == '*' {
                if chars.get(index + 1) == Some(&'*') {
                    flush(&mut segment, bold, italic, &mut segments);
                    bold = !bold;
                    index += 2;
                    continue;
                }
                flush(&mut segment, bold, italic, &mut segments);
                italic = !italic;
                index += 1;
                continue;
            }
            plain.push(chars[index]);
            if bold || italic {
                segment.push(chars[index]);
            } else if !segment.is_empty() {
                flush(&mut segment, bold, italic, &mut segments);
            }
            index += 1;
        }
        flush(&mut segment, bold, italic, &mut segments);
        (plain, segments)
    }

    fn is_punctuation_only(text: &str) -> bool {
        let trimmed = text.trim();
        !trimmed.is_empty()
            && trimmed.chars().all(|c| c.is_ascii_punctuation() || matches!(c, '…' | '—' | '–' | '«' | '»' | '“' | '”' | '‘' | '’' | '！' | '？'))
    }

    #[cfg(test)]
    mod tests {
        use serde_json::json;

        use super::*;
        use crate::{PhotocraftApp, Services};
        use photocraft_manga_ui::comic_pages_panel::PagesPanelAction;

        #[test]
        fn host_snapshot_carries_seam_flags_and_doc_refs() {
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
            app.run("file.new", json!({"name": "P1", "width": 16, "height": 16})).unwrap();
            app.run("prefs.set", json!({"path": "seam.canvas_gizmos", "value": false})).unwrap();
            let snap = host_snapshot(&app);
            assert!(!snap.flags.canvas_gizmos);
            assert!(snap.flags.pages);
            assert_eq!(snap.documents.len(), 1);
            assert_eq!(snap.active_doc.as_ref().map(|d| d.name.as_str()), Some("P1"));
        }

        #[test]
        fn pages_actions_use_stable_doc_id_across_reorders_and_closes() {
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
            for name in ["A", "B", "C"] {
                app.run("file.new", json!({"name": name, "width": 16, "height": 16})).unwrap();
            }
            let id_a = photocraft_manga_bridge::BridgeDocId(app.session.documents()[0].doc.id.0);
            let id_c = photocraft_manga_bridge::BridgeDocId(app.session.documents()[2].doc.id.0);

            let ctx = egui::Context::default();
            // Swap C before A, then close B; id_a still selects A even though its index changed.
            handle_pages_action(&mut app, &ctx, PagesPanelAction::ReorderPages { from_id: id_c, to_id: id_a });
            let idx_b = app.session.documents().iter().position(|d| d.doc.name == "B").unwrap();
            app.session.close(idx_b);
            handle_pages_action(&mut app, &ctx, PagesPanelAction::SelectPage(id_a));
            assert_eq!(app.session.active().map(|s| s.doc.name.as_str()), Some("A"));

            // Closed doc id is safely ignored by search activation instead of panicking or selecting wrong tab.
            let act = SearchActivation {
                doc_id: photocraft_manga_bridge::BridgeDocId(999_999),
                doc_index: 0,
                layer: LayerId(1),
                groups_to_expand: Vec::new(),
                center: [0.0, 0.0],
            };
            apply_search_activation(&mut app, act);
            assert_eq!(app.session.active().map(|s| s.doc.name.as_str()), Some("A"));
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::sync::Arc;

    use photocraft_doc::{Document, Layer};
    use serde_json::Value;

    use crate::PhotocraftApp;
    use crate::canvas::{ToolEvent, ViewXform};
    use crate::dock::Group;
    use crate::state::Panels;

    use super::{SaiCurveParams, ScrubOutcome};

    /// Empty wasm placeholder for manga panel/gizmo runtime state.
    #[derive(Default)]
    pub struct MangaSeamState;

    pub const MANGA_PANELS: &[(&str, &str, Group)] = &[];

    pub fn sync_tokens(_app: &mut PhotocraftApp, _ctx: &egui::Context) {}
    pub fn panel_owns(_id: &str) -> bool {
        false
    }
    pub fn panel_title(_id: &str) -> Option<&'static str> {
        None
    }
    pub fn panel_group(_id: &str) -> Option<Group> {
        None
    }
    pub fn normalize_panel(_raw: &str) -> Option<&'static str> {
        None
    }
    pub fn panel_default_rect(_app: &PhotocraftApp, _id: &str) -> [f32; 4] {
        [140.0, 140.0, 300.0, 420.0]
    }
    pub fn panel_shown(_panels: &Panels, _id: &str) -> Option<bool> {
        None
    }
    pub fn panel_shown_mut<'a>(_panels: &'a mut Panels, _id: &str) -> Option<&'a mut bool> {
        None
    }
    pub fn panel_checked(_app: &PhotocraftApp, _id: &str) -> Option<bool> {
        None
    }
    pub fn render_panel(_app: &mut PhotocraftApp, _ui: &mut egui::Ui, _id: &str) -> bool {
        false
    }
    pub fn panel_menu(_app: &mut PhotocraftApp, _ui: &mut egui::Ui, _id: &str) {}
    pub fn panel_command(_app: &mut PhotocraftApp, _id: &str, _params: &Value) -> Option<Result<Value, String>> {
        None
    }
    pub fn active_balloon_id(_app: &PhotocraftApp) -> Option<u32> {
        None
    }
    pub fn create_live_balloon_in_rect(_app: &mut PhotocraftApp, _rect: [f64; 4]) {}
    pub fn sai_curve_params(_app: &PhotocraftApp, tension: f64, min_ratio: f32) -> SaiCurveParams {
        SaiCurveParams { min_ratio: f64::from(min_ratio), effective_tension: tension, ..SaiCurveParams::default() }
    }
    pub fn sai_snap_close(_app: &PhotocraftApp) -> bool {
        true
    }
    pub fn sync_sai_base_width(_app: &mut PhotocraftApp, _width: f32) {}
    pub fn sync_sai_auto_taper(_app: &mut PhotocraftApp, _auto_taper: bool) {}
    pub fn sync_sai_tension(_app: &mut PhotocraftApp, _tension: f64) {}
    pub fn sync_sai_snap_close(_app: &mut PhotocraftApp, _snap_close: bool) {}
    pub fn screentone_display_doc(_app: &PhotocraftApp, _idx: usize) -> Option<(Arc<Document>, u64)> {
        None
    }
    pub fn screentone_refresh_preview(_app: &mut PhotocraftApp, _ctx: &egui::Context, _xf: &ViewXform, _output: Option<u32>) {}
    pub fn screentone_escape(_app: &mut PhotocraftApp, _ctx: &egui::Context) {}
    pub fn canvas_pointer(_app: &mut PhotocraftApp, _ev: ToolEvent, _mods: egui::Modifiers) -> bool {
        false
    }
    pub fn canvas_hover_cursor(_app: &PhotocraftApp, _xf: &ViewXform, _cursor_scr: egui::Pos2) -> Option<egui::CursorIcon> {
        None
    }
    pub fn draw_canvas_overlays(_app: &PhotocraftApp, _painter: &egui::Painter, _xf: &ViewXform) {}
    pub fn layers_footer_linework_button(_app: &mut PhotocraftApp, _ui: &mut egui::Ui, _actions: &mut Vec<(String, Value)>) {}
    pub fn effect_row_scrub(
        _ui: &egui::Ui,
        _l: &Layer,
        _fx_idx: Option<usize>,
        _rect: egui::Rect,
        resp: &egui::Response,
        _actions: &mut Vec<(String, Value)>,
    ) -> ScrubOutcome {
        ScrubOutcome { trigger_dnd: resp.drag_started(), is_scrubbing: false }
    }
    pub fn effect_row_value_text(_l: &Layer, _fx_idx: Option<usize>) -> Option<String> {
        None
    }
    pub fn effect_row_context_hud(_ui: &mut egui::Ui, _l: &Layer, _fx_idx: Option<usize>, _rect: egui::Rect) {}
    pub fn render_fx_hud(_ui: &egui::Ui, _doc: &Document, _actions: &mut Vec<(String, Value)>) {}
    pub fn open_layer_search(_app: &mut PhotocraftApp) {}
    pub fn balloon_tail_delete_key(_app: &mut PhotocraftApp, _ctx: &egui::Context) -> bool {
        false
    }
    pub fn frame_overlays(_app: &mut PhotocraftApp, _ctx: &egui::Context) {}
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;
