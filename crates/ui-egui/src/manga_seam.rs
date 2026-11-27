//! Host bridge to `photocraft-manga-ui` and `photocraft-manga-core`.
//!
//! The real Manga panels, gizmos and typesetter UI run on both native and wasm targets. Desktop-only
//! filesystem and blocking-dialog paths are gated with `#[cfg(not(target_arch = "wasm32"))]` and
//! fall back to a clean no-op in the browser. This keeps native behaviour untouched.

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

/// In-progress `Tool::Sfx` drag-to-create rectangle in document coordinates.
#[derive(Clone, Copy, Debug)]
pub struct SfxCreateDrag {
    pub start_doc: [f64; 2],
    pub cur_doc: [f64; 2],
    pub shift: bool,
}

mod native {
    use std::collections::HashMap;
    use std::sync::Arc;

    use photocraft_color::PixelFormat;
    use photocraft_doc::sfx::{
        MAX_SFX_SIZE_PT, SfxData, SfxFillSource, SfxWarpMode, reset_sfx_warps_for_new_layer, scale_new_sfx_for_document,
    };
    use photocraft_doc::vector::{BalloonLineStyle, BalloonTextFitMode, LiveBalloon, LiveBalloonBody, LiveBalloonGroup};
    use photocraft_doc::{DocId, Document, Fill, Layer, LayerContent, LayerId, LiveShape, ShapeLayer};
    use photocraft_engine::DocState;
    use photocraft_engine::cmd::manga_cmds::typesetter_adapter;
    use photocraft_manga_core::sfx_geom::build_sfx_geometry;
    use photocraft_manga_core::snow_knuth_plass::{bind_orphan_prepositions, knuth_plass_bubble_wrap_ext, knuth_plass_split_overflow, measure_token_width};
    use photocraft_manga_core::snow_typesetter::{
        TextStylePreset, TplImportReport, TypesetterState, contour_variants, detect_bubble_chambers, detect_speech_bubble_bounds_in_surface,
        fit_script_text_to_selection, parse_script_markup, script_markup_run_params, sort_chamber_indices_reading_order, text_shape_candidates_with_bubble,
    };
    use photocraft_manga_ui::balloon_gizmo::{self, BalloonView};
    use photocraft_manga_ui::comic_balloon_panel::{self, BalloonHostContext, create_live_balloon_in_rect_commands};
    use photocraft_manga_ui::comic_canvas_capsule::{self, CapsuleCallbacks, CapsuleView};
    use photocraft_manga_ui::comic_pages_panel::{
        PageItemInfo, PagesPanelAction, PagesViewInfo, natural_cmp_str, natural_sort_paths, parse_page_number_from_name,
    };
    use photocraft_manga_ui::comic_sai_panel::{self, LineworkHostContext, PressureTargetMode, SaiCurveSync};
    use photocraft_manga_ui::comic_screentone_gizmo::{self, GizmoView, PreviewInput};
    use photocraft_manga_ui::comic_screentone_panel::{self, ScreentoneTargetInfo, ScreentoneUi, SfxScreentoneBanner};
    use photocraft_manga_ui::layer_search::{self, SearchActivation, SearchDoc};
    use photocraft_manga_ui::snow_clean;
    use photocraft_manga_ui::snow_fx_scrub::{self, FxDragTarget, FxHudState};
    use photocraft_manga_ui::snow_typesetter_ui::{FontSampleSpec, TypesetterAction, TypesetterHost};
    use photocraft_manga_ui::view::{CanvasToolEvent, CanvasViewXform};
    use photocraft_manga_ui::{BridgeDocId, MangaUiState, OutCommand, theme as manga_theme};
    use photocraft_raster::Surface;
    use photocraft_vector::balloon::LiveBalloonGeom;
    use photocraft_vector::sai_curve::SaiCurveType;
    use serde_json::{Value, json};

    use crate::PhotocraftApp;
    use crate::canvas::{ToolEvent, ViewXform, canvas_display, display_image};
    use crate::dock::Group;
    // The cached, face-specific font-name previews use native font libraries; the browser uses a
    // proportional-font fallback in the `TypesetterHost` impl below.
    #[cfg(not(target_arch = "wasm32"))]
    use crate::font_preview::{NameStyle, name_size, paint_centered, paint_name};
    use crate::jobs_ui;
    use crate::notices;
    use crate::state::{Panels, Tool};

    use super::{SaiCurveParams, ScrubOutcome, SfxCreateDrag};

    /// Suitcase manga state held on [`PhotocraftApp`].
    #[derive(Default)]
    pub struct MangaSeamState {
        pub ui: MangaUiState,
        pub pages_thumbnails: HashMap<DocId, (u64, egui::TextureHandle)>,
        pub typesetter_text_shape_source: Option<u64>,
        pub typesetter_paste_fingerprint: Option<u64>,
        pub typesetter_paste_on_click_was_enabled: bool,
        /// Active `Tool::Sfx` drag-to-create box (document coordinates).
        pub sfx_create_drag: Option<SfxCreateDrag>,
        /// Isolated Gentonik (Screentone) panel state used ONLY when live-editing an SFX layer's
        /// screentone fill, so global `ui.screentone_ui` settings are never overwritten.
        pub sfx_screentone_ui: ScreentoneUi,
        pub sfx_screentone_bound: Option<(BridgeDocId, LayerId, bool)>,
        pub sfx_screentone_pending_full: bool,
        /// Live zero-readback GPU SFX draft `(doc_id, layer_id, data)` drawn straight onto the
        /// canvas during interactive drags or while an async WASM GPU tiled commit is in flight.
        pub sfx_gpu_live_draft: Option<(DocId, LayerId, SfxData)>,
        /// Target layer whose raster pixels were already cleared once for the active GPU preview
        /// session, so subsequent drag frames skip `s.edit` (0 doc revision bumps, 0 12K recomposites).
        pub sfx_gpu_layer_cleared: Option<(DocId, LayerId)>,
        /// Non-blocking tiled GPU commit runner (zero CPU freezes on WASM button clicks & slider release).
        pub sfx_gpu_async_bake: crate::canvas::sfx_gpu_preview::SfxGpuAsyncBake,
    }

    /// Canonical order matches [`Group::Manga::tabs`].
    pub const MANGA_PANELS: &[(&str, &str, Group)] = &[
        ("balloon", "Balloons", Group::Manga),
        ("screentone", "Screentones", Group::Manga),
        ("linework", "Linework", Group::Manga),
        ("typesetter", "TypeR Manga", Group::Manga),
        ("pages", "Pages", Group::Manga),
        ("timelapse", "Timelapse", Group::Manga),
        ("sfx", "SFX", Group::Manga),
    ];

    fn manga_tab_index(id: &str) -> Option<usize> {
        MANGA_PANELS.iter().position(|p| p.0 == id)
    }

    const MANGA_IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "psd", "tif", "tiff"];

    fn to_manga_xf(xf: &ViewXform) -> CanvasViewXform {
        CanvasViewXform { rect: xf.rect, zoom: xf.zoom, center: xf.center, flip: xf.flip, rotation: xf.rotation, aspect: xf.aspect }
    }

    /// Canvas sampling used by the screentone live-drag preview. The native helper is gated out on
    /// wasm, so the browser reproduces its two-line policy locally (identical values).
    #[cfg(not(target_arch = "wasm32"))]
    fn host_texture_options(factor: u32) -> egui::TextureOptions {
        crate::canvas::canvas_texture_options(factor)
    }

    #[cfg(target_arch = "wasm32")]
    fn host_texture_options(factor: u32) -> egui::TextureOptions {
        if factor > 1 {
            egui::TextureOptions::LINEAR
        } else {
            egui::TextureOptions { magnification: egui::TextureFilter::Nearest, ..egui::TextureOptions::LINEAR }
        }
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
        if trimmed.eq_ignore_ascii_case("SFX") || trimmed.eq_ignore_ascii_case("Manga SFX") || trimmed.eq_ignore_ascii_case("SFX Lettering") {
            return Some("sfx");
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
            "screentone" | "balloon" | "linework" | "sfx" => (300.0, 480.0),
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
            "sfx" => Some(panels.sfx),
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
            "sfx" => Some(&mut panels.sfx),
            _ => None,
        }
    }

    fn is_legacy_manga_tab_active(app: &PhotocraftApp, id: &str) -> bool {
        !MANGA_PANELS.is_empty()
            && app.ui.panels.manga
            && !app.ui.dock.is_collapsed(Group::Manga)
            && manga_tab_index(id).is_some_and(|idx| app.ui.dock_tabs.manga.min(MANGA_PANELS.len().saturating_sub(1)) == idx)
    }

    fn is_panel_shown(app: &PhotocraftApp, id: &str) -> bool {
        if let Some(ws) = &app.ui.docking { ws.layout.contains(&id.to_owned()) } else { is_legacy_manga_tab_active(app, id) }
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
            is_legacy_manga_tab_active(app, id)
        }
    }

    /// Window-menu checkmark state for manga panel commands (`window.panel.*` / `window.toggle.timelapse`).
    pub fn panel_checked(app: &PhotocraftApp, id: &str) -> Option<bool> {
        normalize_panel(id.strip_prefix("window.panel.").or_else(|| id.strip_prefix("window.toggle.")).unwrap_or(id)).map(|short| is_panel_active(app, short))
    }

    fn sfx_gpu_preview_ready(app: &PhotocraftApp) -> bool {
        app.gpu.is_some()
            && app.gpu_render_state.as_ref().is_some_and(crate::canvas::sfx_gpu_preview::is_supported)
    }

    fn clear_sfx_layer_once_for_gpu(app: &mut PhotocraftApp, doc_id: DocId, lid: LayerId, data: &SfxData, coalesce: Option<&str>) {
        if app.manga.sfx_gpu_layer_cleared == Some((doc_id, lid)) {
            return;
        }
        let mut params = json!({
            "layer": lid.0,
            "sfx": data,
            "preview": true,
            "gpuPreview": true,
        });
        if let (Some(c), Some(obj)) = (coalesce, params.as_object_mut()) {
            obj.insert("coalesce".to_owned(), Value::String(c.to_owned()));
        }
        if app.run("sfx.update", params).is_ok() {
            app.manga.sfx_gpu_layer_cleared = Some((doc_id, lid));
        }
    }

    fn dispatch_sfx_update(app: &mut PhotocraftApp, cmd: OutCommand) -> Option<Value> {
        let is_preview = cmd.params.get("preview").and_then(Value::as_bool).unwrap_or(false);
        let parsed_sfx = cmd.params.get("sfx").and_then(|v| serde_json::from_value::<SfxData>(v.clone()).ok());
        let coalesce = cmd.params.get("coalesce").and_then(Value::as_str).map(str::to_owned);
        let target_info = app.session.active().and_then(|st| {
            let lid = cmd.params.get("layer").and_then(Value::as_u64).map(LayerId).or(st.active_layer)?;
            Some((st.doc.id, lid, st.doc.pixel_format(), st.doc.resolution_dpi.max(1.0)))
        });

        let gpu_ok = sfx_gpu_preview_ready(app)
            && parsed_sfx.as_ref().is_some_and(photocraft_engine::cmd::manga_cmds::sfx::sfx_gpu_compatible);

        if let (true, Some(sfx), Some((doc_id, lid, fmt, dpi))) = (gpu_ok, parsed_sfx, target_info) {
            if is_preview {
                // Zero-CPU, zero-doc-recomposite 60 FPS GPU preview: clear old pixels ONCE, then
                // stream live SfxData solely into `sfx_gpu_live_draft` without bumping `doc.revision`.
                app.manga.sfx_gpu_async_bake.cancel();
                clear_sfx_layer_once_for_gpu(app, doc_id, lid, &sfx, coalesce.as_deref());
                app.manga.sfx_gpu_live_draft = Some((doc_id, lid, sfx));
                return Some(json!({ "ok": true, "layer": lid.0 }));
            }

            // Full commit (`preview: false`): on WASM, keep the live GPU overlay visible and bake
            // full-res tiles asynchronously on GPU via `map_async` (zero CPU freeze on 12K×17K!).
            #[cfg(target_arch = "wasm32")]
            if let Some(rs) = app.gpu_render_state.clone() {
                clear_sfx_layer_once_for_gpu(app, doc_id, lid, &sfx, coalesce.as_deref());
                app.manga.sfx_gpu_live_draft = Some((doc_id, lid, sfx.clone()));
                if app.manga.sfx_gpu_async_bake.start(&rs, doc_id, lid, sfx, fmt, dpi, coalesce) {
                    return Some(json!({ "ok": true, "layer": lid.0 }));
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            let _ = (fmt, dpi);
        }

        // Native full commit (uses the installed native GPU tiled hook!) or CPU fallback.
        app.manga.sfx_gpu_async_bake.cancel();
        app.manga.sfx_gpu_live_draft = None;
        app.manga.sfx_gpu_layer_cleared = None;
        match app.run(cmd.id, cmd.params) {
            Ok(v) => Some(v),
            Err(e) => {
                app.ui.status = e;
                app.ui.status_error = true;
                None
            }
        }
    }

    fn dispatch_commands(app: &mut PhotocraftApp, cmds: Vec<OutCommand>) -> Option<Value> {
        let mut last = None;
        for cmd in cmds {
            if cmd.id == "sfx.update" {
                last = dispatch_sfx_update(app, cmd);
                continue;
            }
            if matches!(cmd.id, "sfx.bakeToRaster" | "sfx.create" | "sfx.presets.apply") {
                app.manga.sfx_gpu_async_bake.cancel();
                app.manga.sfx_gpu_live_draft = None;
                app.manga.sfx_gpu_layer_cleared = None;
            }
            let screentone_restyle_target = (cmd.id == "layer.screentone")
                .then(|| cmd.params.get("layer").and_then(Value::as_u64).map(LayerId))
                .flatten();
            match app.run(cmd.id, cmd.params) {
                Ok(v) => {
                    // Live GPU screentone drags only invalidate the layer's `fill_frame` instead
                    // of forcing a full-canvas recomposite on every pointer move.
                    if let Some(lid) = screentone_restyle_target
                        && let Some(st) = app.session.active_mut()
                        && st.last_damage.is_none()
                        && let Some(layer) = st.doc.layer(lid)
                        && matches!(layer.content, LayerContent::Screentone(_))
                        && !photocraft_compose::effects::has_effects(layer)
                    {
                        let frame = photocraft_compose::fill_frame(layer, st.doc.bounds());
                        if !frame.is_empty() {
                            st.last_damage = Some(frame);
                        }
                    }
                    last = Some(v);
                }
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

    /// Active SFX layer whose primary fill, secondary fill, or a Blending effect uses `Screentone`.
    fn active_sfx_screentone(app: &PhotocraftApp) -> Option<(BridgeDocId, LayerId, String, SfxData, photocraft_algo::ScreentoneSpec, bool)> {
        let st = app.session.active()?;
        let lid = st.active_layer?;
        let layer = st.doc.layer(lid)?;
        let sfx = layer.sfx.as_deref()?;
        if let SfxFillSource::Screentone { spec, .. } = &sfx.fill {
            return Some((BridgeDocId(st.doc.id.0), lid, layer.name.clone(), sfx.clone(), spec.clone(), false));
        }
        if let Some(SfxFillSource::Screentone { spec, .. }) = &sfx.blending.secondary_fill {
            return Some((BridgeDocId(st.doc.id.0), lid, layer.name.clone(), sfx.clone(), spec.clone(), true));
        }
        for fx in &sfx.blending.effects {
            match fx {
                photocraft_doc::sfx::SfxBlendEffect::FillOverlay { enabled: true, fill: SfxFillSource::Screentone { spec, .. }, .. }
                | photocraft_doc::sfx::SfxBlendEffect::Stroke { enabled: true, fill: SfxFillSource::Screentone { spec, .. }, .. } => {
                    return Some((BridgeDocId(st.doc.id.0), lid, layer.name.clone(), sfx.clone(), spec.clone(), true));
                }
                _ => {}
            }
        }
        None
    }

    fn apply_sfx_screentone_spec(sfx: &mut SfxData, spec: photocraft_algo::ScreentoneSpec, is_secondary: bool) {
        if !is_secondary {
            if let SfxFillSource::Screentone { spec: dst, .. } = &mut sfx.fill {
                *dst = spec;
            }
        } else if let Some(SfxFillSource::Screentone { spec: dst, .. }) = sfx.blending.secondary_fill.as_mut() {
            *dst = spec;
        } else {
            for fx in &mut sfx.blending.effects {
                match fx {
                    photocraft_doc::sfx::SfxBlendEffect::FillOverlay { enabled: true, fill: SfxFillSource::Screentone { spec: dst, .. }, .. }
                    | photocraft_doc::sfx::SfxBlendEffect::Stroke { enabled: true, fill: SfxFillSource::Screentone { spec: dst, .. }, .. } => {
                        *dst = spec;
                        break;
                    }
                    _ => {}
                }
            }
        }
        sfx.validate();
    }

    fn render_screentone(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
        if let Some((doc_id, lid, layer_name, mut sfx, live_spec, is_secondary)) = active_sfx_screentone(app) {
            let key = (doc_id, lid, is_secondary);
            if app.manga.sfx_screentone_bound != Some(key) || app.manga.sfx_screentone_ui.spec != live_spec {
                app.manga.sfx_screentone_ui.spec = live_spec;
                app.manga.sfx_screentone_ui.spec.mode = photocraft_algo::ScreentoneMode::Sheet;
                app.manga.sfx_screentone_ui.spec.use_source_luminance = false;
                app.manga.sfx_screentone_bound = Some(key);
            }
            let banner = SfxScreentoneBanner { layer_name: &layer_name, is_secondary };
            let changed = comic_screentone_panel::panel_for_sfx(ui, &mut app.manga.sfx_screentone_ui, &banner);
            let pointer_down = ui.ctx().input(|i| i.pointer.primary_down());
            let coalesce = format!("sfx-gentonik:{}:{}", doc_id.0, lid.0);
            if changed {
                apply_sfx_screentone_spec(&mut sfx, app.manga.sfx_screentone_ui.spec.clone(), is_secondary);
                app.manga.ui.sfx_panel.draft = sfx.clone();
                app.manga.ui.sfx_panel.synced_data = Some(sfx.clone());
                app.manga.sfx_screentone_pending_full = pointer_down;
                let _ = dispatch_sfx_update(
                    app,
                    OutCommand::new("sfx.update", json!({ "layer": lid.0, "sfx": sfx, "coalesce": coalesce, "preview": pointer_down })),
                );
            } else if app.manga.sfx_screentone_pending_full && !pointer_down {
                app.manga.sfx_screentone_pending_full = false;
                let _ = dispatch_sfx_update(
                    app,
                    OutCommand::new("sfx.update", json!({ "layer": lid.0, "sfx": sfx, "coalesce": coalesce, "preview": false })),
                );
            }
            return;
        }
        app.manga.sfx_screentone_bound = None;
        app.manga.sfx_screentone_pending_full = false;
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
        let (doc_id, doc_ref, active_layer, selected_layers, doc_dpi, fg, bg) = match app.session.active() {
            Some(st) => (
                Some(BridgeDocId(st.doc.id.0)),
                Some(st.doc.as_ref()),
                st.active_layer,
                st.selected_layers(),
                st.doc.resolution_dpi.max(1.0),
                app.session.tools.foreground,
                app.session.tools.background,
            ),
            None => (None, None, None, Vec::new(), 72.0, app.session.tools.foreground, app.session.tools.background),
        };
        let host = BalloonHostContext {
            doc: doc_ref,
            doc_id,
            active_layer,
            selected_layers: &selected_layers,
            active_balloon_id: app.manga.ui.balloon_gizmo.active_balloon_id,
            doc_dpi,
            fg_color: fg,
            bg_color: bg,
            vector_brush_presets: &brushes,
            tool_is_balloon: app.active_tool() == Tool::Balloon,
        };
        // The panel owns the Simplified/Manual mode. Both states default to Manual (true);
        // keep them in step on startup too, including when panel UI state was restored.
        if app.manga.ui.balloon_ui.show_vertex_handles != app.manga.ui.balloon_gizmo.show_vertex_handles {
            app.manga.ui.balloon_gizmo.show_vertex_handles = app.manga.ui.balloon_ui.show_vertex_handles;
        }
        let out = comic_balloon_panel::panel(ui, &mut app.manga.ui.balloon_ui, &host);
        // A fill edit is the only panel edit that changes lettering contrast. Detect it before
        // the commands are consumed so recolouring runs exactly once, on this frame only.
        let fill_changed = out.commands.iter().any(|c| c.id == "shape.edit" && c.params.get("fill").is_some());
        if let Some(bid) = out.select_balloon_id {
            app.manga.ui.balloon_gizmo.active_balloon_id = Some(bid);
        }
        let last = dispatch_commands(app, out.commands);
        if fill_changed && app.manga.ui.balloon_ui.auto_contrast && let Some(id) = active_layer {
            recolor_linked_balloon_text(app, id);
        }
        if out.auto_fit_linked_text {
            fit_active_balloon_linked_text(app);
        }
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
        if let Some(show) = out.show_vertex_handles {
            app.manga.ui.balloon_gizmo.show_vertex_handles = show;
        }
        if out.open_typer_panel {
            prepare_typer_for_active_balloon(app);
            let _ = app.run("window.panel.activate", json!({ "panel": "typesetter" }));
        }
        if out.open_linework_panel {
            let _ = app.run("window.panel.activate", json!({ "panel": "linework" }));
            // The baked SAI strokes are now the editable geometry, so switch to the Linework tool
            // exactly as the Layers' "Create Linework" action does.
            app.ui.tool = Tool::VectorCurve;
            app.ui.remember_group_tool(Tool::VectorCurve);
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
            #[cfg(not(target_arch = "wasm32"))]
            paint_centered(ui, family, text, size_pt, rect, color);
            #[cfg(target_arch = "wasm32")]
            {
                let _ = family;
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(size_pt), color);
            }
        }

        fn name_size(&self, ctx: &egui::Context, spec: FontSampleSpec<'_>) -> Option<[f32; 2]> {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let ns = NameStyle { family: spec.family, text: spec.text, size: spec.size_pt, bold: spec.bold, italic: spec.italic };
                name_size(ctx, &ns)
            }
            #[cfg(target_arch = "wasm32")]
            {
                let _ = (ctx, spec.family, spec.bold, spec.italic);
                Some([spec.text.chars().count() as f32 * spec.size_pt * 0.6, spec.size_pt * 1.2])
            }
        }

        fn paint_name(&self, ui: &mut egui::Ui, spec: FontSampleSpec<'_>, rect: egui::Rect, color: egui::Color32) {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let ns = NameStyle { family: spec.family, text: spec.text, size: spec.size_pt, bold: spec.bold, italic: spec.italic };
                paint_name(ui, &ns, rect, color);
            }
            #[cfg(target_arch = "wasm32")]
            {
                let _ = (spec.family, spec.bold, spec.italic);
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, spec.text, egui::FontId::proportional(spec.size_pt), color);
            }
        }

        fn is_favorite_font(&self, _ctx: &egui::Context, family: &str) -> bool {
            photocraft_engine::font_library_cmds::favorite_fonts(&self.app.session).iter().any(|f| f == family)
        }

        fn toggle_favorite_font(&mut self, _ctx: &egui::Context, family: &str) -> bool {
            self.app.run("type.font.favorite", json!({ "family": family })).is_ok()
        }

        fn font_family_picker(&mut self, ui: &mut egui::Ui, id_salt: &str, family: &mut String, width: f32) -> bool {
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(style) = crate::type_tool::family_picker(self.app, ui, id_salt, family, width) {
                self.picked_style = style;
                true
            } else {
                false
            }
            #[cfg(target_arch = "wasm32")]
            {
                // The full picker walks native font libraries; offer a plain family field so the
                // TypeR Manga dock stays usable in the browser.
                let _ = (id_salt, &self.picked_style);
                let before = family.clone();
                ui.horizontal(|ui| {
                    ui.label("Font:");
                    ui.add_sized([width, 0.0], egui::TextEdit::singleline(family));
                });
                *family != before
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
        let create_sfx = ts.take_create_sfx_request();
        app.manga.ui.typesetter = ts;
        if let Some(act) = action {
            handle_typesetter_action(app, act);
        }
        if create_sfx {
            handle_typesetter_create_sfx(app);
        }
        refresh_typesetter_contour(app);
    }

    fn handle_typesetter_create_sfx(app: &mut PhotocraftApp) {
        if let Some(line) = app.manga.ui.typesetter.state.active_line().cloned() {
            let line_idx = app.manga.ui.typesetter.state.current_line_idx;
            let preset = app.manga.ui.typesetter.state.preset_for_line(line_idx).cloned().unwrap_or_default();
            let mut sfx = app.manga.ui.sfx_panel.draft.clone();
            // Each typesetter line becomes a brand-new SFX layer: drop warps inherited from the
            // panel draft so the new lettering is never mirrored or collapsed on creation.
            reset_sfx_warps_for_new_layer(&mut sfx);
            sfx.text = line.text;
            if !preset.font_family.trim().is_empty() {
                sfx.font_family = preset.font_family;
            }
            sfx.postscript_name = (!preset.font_postscript.trim().is_empty()).then_some(preset.font_postscript);
            sfx.size_pt = preset.font_size.max(8.0);
            sfx.weight = if preset.bold { 800 } else { 700 };
            sfx.italic = preset.italic;
            sfx.tracking = preset.tracking as f32;
            if let Some(st) = app.session.active() {
                let origin = st
                    .doc
                    .selection
                    .as_ref()
                    .map(|s| s.content_bounds())
                    .filter(|b| !b.is_empty())
                    .map(|b| [f64::from(b.x0), f64::from(b.y0)])
                    .unwrap_or([f64::from(st.doc.size.width) * 0.35, f64::from(st.doc.size.height) * 0.4]);
                sfx.origin = origin;
            }
            sfx.validate();
            if app.run("sfx.create", json!({ "sfx": sfx })).is_ok() {
                app.manga.ui.typesetter.state.advance_line();
                app.manga.ui.typesetter.auto_save();
                let _ = app.run("window.panel.sfx", json!({ "show": true }));
            }
            return;
        }
        if let Some(st) = app.session.active()
            && let Some(tid) = active_text_layer_id(st)
            && app.run("sfx.convertFromText", json!({ "layer": tid.0 })).is_ok()
        {
            let _ = app.run("window.panel.sfx", json!({ "show": true }));
        }
    }

    fn render_sfx(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
        let presets = app.session.manga.sfx_presets.presets().to_vec();
        let gpu_ready = sfx_gpu_preview_ready(app);
        let target = if let Some(st) = app.session.active() {
            let active_layer = st.active_layer.and_then(|id| st.doc.layer(id));
            let active_is_text = active_text_layer_id(st).is_some()
                && active_layer.is_some_and(|l| matches!(l.content, LayerContent::Text(_)));
            let active_is_smart = active_layer.is_some_and(|l| matches!(l.content, LayerContent::Smart(_)));
            let live_override = app
                .manga
                .sfx_gpu_live_draft
                .as_ref()
                .filter(|(did, lid, _)| *did == st.doc.id && Some(*lid) == st.active_layer)
                .map(|(_, _, d)| d.clone());
            let sfx_data = live_override.or_else(|| active_layer.and_then(|l| l.sfx.as_deref().cloned()));
            let gpu_active = gpu_ready
                && sfx_data
                    .as_ref()
                    .is_none_or(photocraft_engine::cmd::manga_cmds::sfx::sfx_gpu_compatible);
            photocraft_manga_ui::sfx_panel::SfxTargetInfo {
                doc_id: Some(BridgeDocId(st.doc.id.0)),
                layer_id: st.active_layer,
                data: sfx_data,
                active_is_text,
                active_is_smart,
                doc_dpi: st.doc.resolution_dpi.max(1.0),
                gpu_active,
                presets: &presets,
            }
        } else {
            photocraft_manga_ui::sfx_panel::SfxTargetInfo {
                doc_id: None,
                layer_id: None,
                data: None,
                active_is_text: false,
                active_is_smart: false,
                doc_dpi: 72.0,
                gpu_active: gpu_ready,
                presets: &presets,
            }
        };
        let out = photocraft_manga_ui::sfx_panel::panel(ui, &mut app.manga.ui.sfx_panel, &mut app.manga.ui.sfx_gizmo, &target);
        dispatch_commands(app, out.commands);
    }

    /// Renders the body of manga panel `id` when owned by this seam. Returns `true` if handled.
    pub fn render_panel(app: &mut PhotocraftApp, ui: &mut egui::Ui, id: &str) -> bool {
        // Manga panels are controls, not prose: their headings must never turn into browser-style
        // blue text selections. egui's TextEdit keeps its own selectable caret/selection.
        ui.style_mut().interaction.selectable_labels = false;
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
            "sfx" => render_sfx(app, ui),
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

    /// Renders the `tab` index of the unified [`Group::Manga`] dock card.
    pub fn render_dock_tab(app: &mut PhotocraftApp, ui: &mut egui::Ui, tab: usize) {
        if let Some(&(id, _, _)) = MANGA_PANELS.get(tab) {
            let _ = render_panel(app, ui, id);
        }
    }

    /// Panel menu entries specific to manga panels, if any.
    pub fn panel_menu(_app: &mut PhotocraftApp, _ui: &mut egui::Ui, _id: &str) {}

    /// Handles legacy dock/panel commands for manga panels when `app.ui.docking` is inactive:
    /// opens the unified `Group::Manga` dock on the requested tab (or toggles it closed when that
    /// tab is already open, expanded and selected).
    pub fn panel_command(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
        let short = id.strip_prefix("window.panel.").or_else(|| id.strip_prefix("window.toggle.")).and_then(normalize_panel)?;
        if app.ui.docking.is_some() {
            return None;
        }
        let tab_idx = manga_tab_index(short).unwrap_or(0);
        let currently_active = app.ui.panels.manga && !app.ui.dock.is_collapsed(Group::Manga) && app.ui.dock_tabs.manga == tab_idx;
        let show = match params.get("show").or_else(|| params.get("on")) {
            None => !currently_active,
            Some(v) => match v.as_bool() {
                Some(b) => b,
                None => return Some(Err("show must be a boolean".into())),
            },
        };
        if show {
            app.ui.panels.manga = true;
            app.ui.dock_tabs.manga = tab_idx;
            crate::dock::reveal(app, Group::Manga);
        } else {
            app.ui.panels.manga = false;
        }
        for &(pid, _, _) in MANGA_PANELS {
            if let Some(slot) = panel_shown_mut(&mut app.ui.panels, pid) {
                *slot = show && pid == short;
            }
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
        let input = PreviewInput { view: &view, doc_index: doc_idx, revision, display_key, texture_options: host_texture_options(factor) };
        let to_image = |buf: &photocraft_compose::Buffer| display_image(display.as_deref(), buf);
        comic_screentone_gizmo::refresh_preview(ctx, &input, &mut app.manga.ui.screentone_ui, &mut app.manga.ui.screentone_gizmo, &to_image);
    }

    /// Cancels any active screentone gizmo drag on Escape.
    pub fn screentone_escape(app: &mut PhotocraftApp, ctx: &egui::Context) {
        if !app.session.seam_flags().canvas_gizmos {
            return;
        }
        let Some(xf) = ViewXform::active(app) else { return };
        let mxf = to_manga_xf(&xf);
        let panel_showing = is_panel_active(app, "screentone") || app.active_tool() == Tool::Screentone;
        let Some(st) = app.session.active() else { return };
        let doc = st.doc.clone();
        let doc_id = BridgeDocId(doc.id.0);
        let active_layer = st.active_layer;
        let mut out = Vec::new();
        {
            let view = GizmoView { panel_showing, doc: &doc, doc_id, active_layer, xf: &mxf };
            comic_screentone_gizmo::escape(ctx, &view, &mut app.manga.ui.screentone_ui, &mut app.manga.ui.screentone_gizmo, &mut out);
        }
        // A cancelled GPU drag sends the pre-drag spec back through the same coalesce key.
        dispatch_commands(app, out);
    }

    fn pointer_screentone(app: &mut PhotocraftApp, ev: ToolEvent, mods: egui::Modifiers, xf: &ViewXform) -> bool {
        let mxf = to_manga_xf(xf);
        let panel_showing = is_panel_active(app, "screentone") || app.active_tool() == Tool::Screentone;
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

    fn balloon_story_links(doc: &Document, active_layer: Option<LayerId>) -> Vec<([f64; 2], [f64; 2])> {
        let Some(lid) = active_layer else { return Vec::new() };
        let Some(layer) = doc.layer(lid) else { return Vec::new() };
        let LayerContent::Shape(sh) = &layer.content else { return Vec::new() };
        let Some(LiveShape::BalloonGroup { group }) = &sh.live else { return Vec::new() };
        let mut center_by_text: std::collections::HashMap<LayerId, [f64; 2]> = std::collections::HashMap::new();
        for (_, _, l) in doc.walk() {
            if let LayerContent::Shape(other_sh) = &l.content
                && let Some(LiveShape::BalloonGroup { group: g }) = &other_sh.live
            {
                for b in &g.items {
                    if let Some(tid) = b.linked_text_layer {
                        let r = b.body.rect();
                        center_by_text.insert(tid, [r[0] + r[2] * 0.5, r[1] + r[3] * 0.5]);
                    }
                }
            }
        }
        let mut links = Vec::new();
        for b in &group.items {
            let Some(tid) = b.linked_text_layer else { continue };
            let Some(from) = center_by_text.get(&tid).copied() else { continue };
            let Some(tl) = doc.layer(tid).and_then(|l| match &l.content {
                LayerContent::Text(t) => Some(t),
                _ => None,
            }) else {
                continue;
            };
            if tl.story_threaded
                && let Some(next_id) = tl.next_story_layer
                && let Some(to) = center_by_text.get(&next_id).copied()
            {
                links.push((from, to));
            }
        }
        links
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
            story_links: Vec::new(),
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
        pointer_screentone(app, ev, mods, &xf) || pointer_balloon(app, ev, mods, &xf) || pointer_sfx(app, ev, mods, &xf)
    }

    fn live_sfx_for(app: &PhotocraftApp, doc_id: DocId, active_layer: Option<LayerId>) -> Option<&SfxData> {
        let active_lid = active_layer?;
        app.manga
            .sfx_gpu_live_draft
            .as_ref()
            .filter(|(d, l, _)| *d == doc_id && *l == active_lid)
            .map(|(_, _, sfx)| sfx)
    }

    fn sfx_view_from<'a>(
        doc: &'a Document,
        active_layer: Option<LayerId>,
        live_sfx: Option<&'a SfxData>,
        tool_is_sfx: bool,
        panel_open: bool,
        xf: &ViewXform,
    ) -> photocraft_manga_ui::sfx_gizmo::SfxView<'a> {
        photocraft_manga_ui::sfx_gizmo::SfxView {
            doc_id: Some(BridgeDocId(doc.id.0)),
            doc: Some(doc),
            active_layer,
            live_sfx,
            tool_is_sfx,
            panel_open,
            view_xform: to_manga_xf(xf),
        }
    }

    fn pointer_sfx(app: &mut PhotocraftApp, ev: ToolEvent, mods: egui::Modifiers, xf: &ViewXform) -> bool {
        let Some(st) = app.session.active() else { return false };
        let doc = st.doc.clone();
        let active_layer = st.active_layer;
        let tool_is_sfx = app.active_tool() == Tool::Sfx;
        if !tool_is_sfx {
            app.manga.sfx_create_drag = None;
        }
        let panel_open = is_panel_active(app, "sfx");
        let press_pt = match ev {
            ToolEvent::Down { x, y, .. } => Some([x, y]),
            _ => None,
        };
        let live_sfx_owned = live_sfx_for(app, doc.id, active_layer).cloned();
        let out = {
            let view = sfx_view_from(&doc, active_layer, live_sfx_owned.as_ref(), tool_is_sfx, panel_open, xf);
            app.manga.ui.sfx_gizmo.sync_target(&view);
            photocraft_manga_ui::sfx_gizmo::pointer(&mut app.manga.ui.sfx_gizmo, &view, to_manga_ev(ev), mods)
        };
        if let Some(updated) = out.updated_sfx {
            app.manga.ui.sfx_panel.draft = updated.clone();
            app.manga.ui.sfx_panel.synced_data = Some(updated);
        }
        dispatch_commands(app, out.commands);
        if out.handled {
            return true;
        }
        // A press that missed every SFX gizmo handle arms a drag-to-create box instead of
        // instantly creating a layer on pointer-down (the layer is created on release).
        if let Some(pt) = press_pt.filter(|_| tool_is_sfx) {
            return sfx_canvas_down(app, pt, mods);
        }
        false
    }

    /// Cancels an active SFX canvas drag on Escape, returning true when consumed.
    pub fn sfx_escape_key(app: &mut PhotocraftApp, ctx: &egui::Context) -> bool {
        let has_create_drag = app.manga.sfx_create_drag.is_some();
        if !app.manga.ui.sfx_gizmo.has_active_drag() && !has_create_drag {
            return false;
        }
        if !ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            return false;
        }
        app.manga.sfx_create_drag = None;
        if !app.manga.ui.sfx_gizmo.has_active_drag() {
            return true;
        }
        sfx_escape(app, ctx);
        true
    }

    /// Cancels any active SFX gizmo drag if Escape was pressed this frame.
    pub fn sfx_escape(app: &mut PhotocraftApp, ctx: &egui::Context) {
        if !app.session.seam_flags().canvas_gizmos {
            return;
        }
        if !ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            return;
        }
        app.manga.sfx_create_drag = None;
        if !app.manga.ui.sfx_gizmo.has_active_drag() {
            return;
        }
        let Some(xf) = ViewXform::active(app) else { return };
        let Some(st) = app.session.active() else { return };
        let doc = st.doc.clone();
        let active_layer = st.active_layer;
        let tool_is_sfx = app.active_tool() == Tool::Sfx;
        let panel_open = is_panel_active(app, "sfx");
        let live_sfx_owned = live_sfx_for(app, doc.id, active_layer).cloned();
        let out = {
            let view = sfx_view_from(&doc, active_layer, live_sfx_owned.as_ref(), tool_is_sfx, panel_open, &xf);
            photocraft_manga_ui::sfx_gizmo::cancel_drag(&mut app.manga.ui.sfx_gizmo, &view)
        };
        if let Some(updated) = out.updated_sfx {
            app.manga.ui.sfx_panel.draft = updated.clone();
            app.manga.ui.sfx_panel.synced_data = Some(updated);
        }
        dispatch_commands(app, out.commands);
    }

    fn layer_contains_pt(layer: &Layer, px: i32, py: i32) -> bool {
        match &layer.content {
            LayerContent::Raster(s) => {
                let b = s.content_bounds();
                !b.is_empty() && b.contains(px, py)
            }
            LayerContent::Group(g) => g.children.iter().any(|c| {
                c.surface().is_some_and(|s| {
                    let b = s.content_bounds();
                    !b.is_empty() && b.contains(px, py)
                })
            }),
            _ => false,
        }
    }

    fn sfx_hit_layer(app: &PhotocraftApp, doc_pt: [f64; 2]) -> Option<LayerId> {
        let px = doc_pt[0].round() as i32;
        let py = doc_pt[1].round() as i32;
        let st = app.session.active()?;
        st.doc
            .layers
            .iter()
            .rev()
            .find(|l| l.visible && l.sfx.is_some() && layer_contains_pt(l, px, py))
            .map(|l| l.id)
    }

    /// Captures a fresh SFX draft from the panel, clearing stale per-instance warps and scaling
    /// unbound tool defaults to the document (already-synced drafts carry scaled values).
    fn new_sfx_from_draft(app: &PhotocraftApp) -> Option<SfxData> {
        let mut sfx = app.manga.ui.sfx_panel.draft.clone();
        reset_sfx_warps_for_new_layer(&mut sfx);
        sfx.warp_mode = SfxWarpMode::Resize;
        if app.manga.ui.sfx_panel.bound_target.is_none()
            && let Some(st) = app.session.active()
        {
            scale_new_sfx_for_document(&mut sfx, st.doc.size.width, st.doc.size.height);
        }
        Some(sfx)
    }

    fn sfx_doc_dpi(app: &PhotocraftApp) -> f32 {
        app.session.active().map_or(72.0, |st| st.doc.resolution_dpi.max(1.0))
    }

    /// Sizes and positions a fresh SFX so its natural layout fills `rect` (`[x0,y0,x1,y1]`).
    ///
    /// Without `stretch_to_box` the word keeps its aspect ratio and is centered inside the box;
    /// with it, `free_transform.scale_x/scale_y` stretch the word to the exact box aspect.
    pub fn fit_sfx_to_rect(sfx: &mut SfxData, rect: [f64; 4], dpi: f32, stretch_to_box: bool) {
        let bw = (rect[2] - rect[0]).max(1.0);
        let bh = (rect[3] - rect[1]).max(1.0);
        reset_sfx_warps_for_new_layer(sfx);
        sfx.warp_mode = SfxWarpMode::Resize;
        sfx.free_transform.scale_x = 1.0;
        sfx.free_transform.scale_y = 1.0;
        sfx.size_pt = 100.0;
        sfx.origin = [0.0, 0.0];
        sfx.validate();

        let natural = build_sfx_geometry(sfx, &mut text_engine(), dpi);
        let nat_w = (natural.rest_box[2] - natural.rest_box[0]).max(1.0);
        let nat_h = (natural.rest_box[3] - natural.rest_box[1]).max(1.0);
        let ratio = (bw / nat_w).min(bh / nat_h);
        sfx.size_pt = (100.0 * ratio).clamp(8.0, f64::from(MAX_SFX_SIZE_PT)) as f32;
        sfx.validate();

        // Re-measure at the fitted size: font metrics are not perfectly linear in size_pt.
        let fitted = build_sfx_geometry(sfx, &mut text_engine(), dpi);
        let fit_w = (fitted.rest_box[2] - fitted.rest_box[0]).max(1.0);
        let fit_h = (fitted.rest_box[3] - fitted.rest_box[1]).max(1.0);
        if stretch_to_box {
            sfx.free_transform.scale_x = (bw / fit_w).clamp(0.05, 32.0);
            sfx.free_transform.scale_y = (bh / fit_h).clamp(0.05, 32.0);
        }
        let fit_cx = (fitted.rest_box[0] + fitted.rest_box[2]) * 0.5;
        let fit_cy = (fitted.rest_box[1] + fitted.rest_box[3]) * 0.5;
        sfx.origin = [rect[0] + bw * 0.5 - fit_cx, rect[1] + bh * 0.5 - fit_cy];
        sfx.validate();
    }

    fn text_engine() -> std::sync::MutexGuard<'static, photocraft_text::TextEngine> {
        match photocraft_text::shared().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// `Tool::Sfx` press: select a hit layer, otherwise arm a drag-to-create box. Returns whether
    /// the press was consumed by an existing layer.
    pub fn sfx_canvas_down(app: &mut PhotocraftApp, doc_pt: [f64; 2], mods: egui::Modifiers) -> bool {
        if let Some(hit) = sfx_hit_layer(app, doc_pt) {
            if app.session.active().is_some_and(|st| st.active_layer != Some(hit)) {
                let _ = app.run("layer.select", json!({ "layer": hit.0 }));
            }
            app.manga.sfx_create_drag = None;
            return true;
        }
        app.manga.sfx_create_drag = Some(SfxCreateDrag { start_doc: doc_pt, cur_doc: doc_pt, shift: mods.shift });
        false
    }

    pub fn sfx_create_move(app: &mut PhotocraftApp, doc_pt: [f64; 2], mods: egui::Modifiers) {
        if let Some(drag) = app.manga.sfx_create_drag.as_mut() {
            drag.cur_doc = doc_pt;
            drag.shift = mods.shift;
        }
    }

    fn sfx_create_from_rect(app: &mut PhotocraftApp, rect: [f64; 4], stretch: bool) {
        let Some(mut sfx) = new_sfx_from_draft(app) else { return };
        fit_sfx_to_rect(&mut sfx, rect, sfx_doc_dpi(app), stretch);
        let _ = app.run("sfx.create", json!({ "sfx": sfx }));
    }

    /// Finishes a `Tool::Sfx` drag: a plain click fits to the active selection (or creates at the
    /// press point), a dragged box fits the SFX to the rectangle (Shift stretches to its aspect).
    pub fn sfx_create_finish(app: &mut PhotocraftApp, doc_pt: [f64; 2], mods: egui::Modifiers) {
        let Some(drag) = app.manga.sfx_create_drag.take() else { return };
        let stretch = drag.shift || mods.shift;
        let x0 = drag.start_doc[0].min(doc_pt[0]);
        let x1 = drag.start_doc[0].max(doc_pt[0]);
        let y0 = drag.start_doc[1].min(doc_pt[1]);
        let y1 = drag.start_doc[1].max(doc_pt[1]);
        let (bw, bh) = (x1 - x0, y1 - y0);
        let zoom = f64::from(ViewXform::active(app).map_or(1.0, |xf| xf.zoom.max(0.05)));
        let screen_span = bw.max(bh) * zoom;

        if screen_span < 8.0 || bw < 8.0 || bh < 8.0 {
            let sel_bounds = app
                .session
                .active()
                .and_then(|st| st.doc.selection.as_ref())
                .map(|s| s.content_bounds())
                .filter(|b| !b.is_empty());
            if let Some(b) = sel_bounds {
                sfx_create_from_rect(app, [b.x0 as f64, b.y0 as f64, b.x1 as f64, b.y1 as f64], false);
            } else {
                sfx_canvas_press(app, drag.start_doc);
            }
            return;
        }
        sfx_create_from_rect(app, [x0, y0, x1, y1], stretch);
    }

    /// Creates a new SFX layer (or switches selection) when pressing on canvas with `Tool::Sfx`.
    pub fn sfx_canvas_press(app: &mut PhotocraftApp, doc_pt: [f64; 2]) {
        if let Some(hit) = sfx_hit_layer(app, doc_pt) {
            if app.session.active().is_some_and(|st| st.active_layer != Some(hit)) {
                let _ = app.run("layer.select", json!({ "layer": hit.0 }));
            }
            return;
        }
        let Some(mut sfx) = new_sfx_from_draft(app) else { return };
        sfx.origin = doc_pt;
        sfx.validate();
        let _ = app.run("sfx.create", json!({ "sfx": sfx }));
    }

    /// Creates a new Screentone layer from `app.manga.ui.screentone_ui`, honoring `screentone_clip`,
    /// and binds the on-canvas gizmo to it.
    fn create_screentone_from_ui(app: &mut PhotocraftApp) {
        let mut p = comic_screentone_panel::params(&app.manga.ui.screentone_ui);
        if app.ui.tool_options.screentone_clip
            && let Some(map) = p.as_object_mut()
        {
            map.insert("clipped".into(), json!(true));
        }
        if let Ok(v) = app.run(comic_screentone_panel::CMD_LAYER, p)
            && let Some(lid) = v.get("layer").and_then(Value::as_u64)
            && let Some(doc_id) = app.session.active().map(|st| BridgeDocId(st.doc.id.0))
        {
            app.manga.ui.screentone_gizmo.bind_layer(doc_id, LayerId(lid));
        }
    }

    /// Finishes a `Tool::Screentone` press/drag:
    /// - A plain click (`< 4` screen px) creates a Screentone layer over the selection/canvas.
    /// - A drag sets gradient type & angle from `(start -> end)` before creating the Screentone layer.
    pub fn screentone_drag_finish(app: &mut PhotocraftApp, start: [f64; 2], end: [f64; 2]) {
        use photocraft_algo::ScreentoneGradientType;
        let zoom = f64::from(app.point_zoom().max(0.05));
        let dist_px = (end[0] - start[0]).hypot(end[1] - start[1]) * zoom;
        if dist_px >= 4.0 {
            let gt = match app.ui.tool_options.screentone_drag_gradient.as_str() {
                "radial" => ScreentoneGradientType::Radial,
                "angle" => ScreentoneGradientType::Angle,
                "reflected" => ScreentoneGradientType::Reflected,
                "diamond" => ScreentoneGradientType::Diamond,
                _ => ScreentoneGradientType::Linear,
            };
            let angle = (end[1] - start[1]).atan2(end[0] - start[0]).to_degrees() as f32;
            let spec = &mut app.manga.ui.screentone_ui.spec;
            spec.grad_type = gt;
            spec.grad_angle = angle;
            if (spec.grad_size_start - spec.grad_size_end).abs() < 1e-4 {
                spec.grad_size_start = 1.0;
                spec.grad_size_end = 0.0;
            }
        }
        create_screentone_from_ui(app);
    }

    /// Top Options Bar controls when `Tool::Screentone` is active.
    pub fn screentone_options_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (style, icon, tip) in [
            ("linear", "gradient", tl!("Linear Gradient")),
            ("radial", "circle", tl!("Radial Gradient")),
            ("angle", "rotate-cw", tl!("Angle Gradient")),
            ("reflected", "arrow-left-right", tl!("Reflected Gradient")),
            ("diamond", "diamond", tl!("Diamond Gradient")),
        ] {
            if crate::icons::button(ui, icon, 24.0, app.ui.tool_options.screentone_drag_gradient == style, tip).clicked() {
                app.ui.tool_options.screentone_drag_gradient = style.into();
            }
        }
        ui.spacing_mut().item_spacing.x = 8.0;
        crate::widgets::checkbox(ui, &mut app.ui.tool_options.screentone_clip, tl!("Clip"));
        crate::widgets::vline(ui, 22.0);
        if ui.button(tl!("New Screentone Layer")).clicked() {
            create_screentone_from_ui(app);
        }
        if ui.button(tl!("Apply Filter to Layer")).clicked() {
            let p = comic_screentone_panel::params(&app.manga.ui.screentone_ui);
            let _ = app.run(comic_screentone_panel::CMD_FILTER, p);
        }
        crate::widgets::vline(ui, 22.0);
        if ui.button(tl!("Open Screentones Panel")).clicked() {
            let _ = app.run("window.panel.screentone", json!({ "show": true }));
        }
    }

    /// Top Options Bar controls when `Tool::Sfx` is active.
    pub fn sfx_options_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
        use photocraft_doc::sfx::SfxWarpMode;
        let mut active_mode = app
            .session
            .active()
            .and_then(|st| st.active_layer.and_then(|id| st.doc.layer(id)))
            .and_then(|l| l.sfx.as_deref())
            .map(|s| s.warp_mode)
            .unwrap_or(app.manga.ui.sfx_panel.draft.warp_mode);
        let prev = active_mode;
        ui.label("SFX Mode:");
        for (mode, label) in [
            (SfxWarpMode::Resize, "Resize"),
            (SfxWarpMode::FreeTransform, "FT 4-Corner + Дуга"),
            (SfxWarpMode::PerGlyph, "По точкам (Alt)"),
            (SfxWarpMode::MeshLattice, "Mesh Grid"),
            (SfxWarpMode::LiquifyField, "Liquify"),
        ] {
            ui.selectable_value(&mut active_mode, mode, label);
        }
        if active_mode != prev {
            app.manga.ui.sfx_panel.draft.warp_mode = active_mode;
            if let Some(st) = app.session.active()
                && let Some(id) = st.active_layer
                && let Some(cur) = st.doc.layer(id).and_then(|l| l.sfx.as_deref())
            {
                let mut next = cur.clone();
                next.warp_mode = active_mode;
                let _ = app.run("sfx.update", json!({ "layer": id.0, "sfx": next, "coalesce": "sfx-mode" }));
            }
        }
        crate::widgets::vline(ui, 22.0);
        if ui.button("Open SFX Panel").clicked() {
            let _ = app.run("window.panel.sfx", json!({ "show": true }));
        }
        if let Some(st) = app.session.active()
            && let Some(id) = active_text_layer_id(st)
            && ui.button("Convert Text -> SFX").clicked()
        {
            let _ = app.run("sfx.convertFromText", json!({ "layer": id.0 }));
        }
    }

    /// Hover cursor for screentone and balloon gizmo handles over the canvas.
    pub fn canvas_hover_cursor(app: &PhotocraftApp, xf: &ViewXform, cursor_scr: egui::Pos2) -> Option<egui::CursorIcon> {
        if !app.session.seam_flags().canvas_gizmos {
            return None;
        }
        let st = app.session.active()?;
        let mxf = to_manga_xf(xf);
        let panel_showing = is_panel_active(app, "screentone") || app.active_tool() == Tool::Screentone;
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
            story_links: Vec::new(),
        };
        if let Some(c) = balloon_gizmo::hover_cursor(&app.manga.ui.balloon_gizmo, &bview, Some(cursor_scr)) {
            return Some(c);
        }
        let live_sfx = live_sfx_for(app, st.doc.id, st.active_layer);
        let sfx_v = sfx_view_from(&st.doc, st.active_layer, live_sfx, app.active_tool() == Tool::Sfx, is_panel_active(app, "sfx"), xf);
        photocraft_manga_ui::sfx_gizmo::hover_cursor(&app.manga.ui.sfx_gizmo, &sfx_v, cursor_scr)
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
        let panel_showing = is_panel_active(app, "screentone") || app.active_tool() == Tool::Screentone;
        let sview = GizmoView { panel_showing, doc: &st.doc, doc_id: BridgeDocId(st.doc.id.0), active_layer: st.active_layer, xf: &mxf };
        comic_screentone_gizmo::draw(painter, &sview, &app.manga.ui.screentone_ui, &app.manga.ui.screentone_gizmo);
        let bview = BalloonView {
            doc_id: Some(BridgeDocId(st.doc.id.0)),
            doc: Some(&st.doc),
            active_layer: st.active_layer,
            tool_is_balloon: app.active_tool() == Tool::Balloon,
            view_xform: mxf,
            story_links: balloon_story_links(&st.doc, st.active_layer),
        };
        balloon_gizmo::draw(painter, &app.manga.ui.balloon_gizmo, &bview);
        if let Some((prev_doc, prev_lid, ref live_sfx)) = app.manga.sfx_gpu_live_draft
            && prev_doc == st.doc.id
            && sfx_gpu_preview_ready(app)
            && let Some(layer) = st.doc.layer(prev_lid)
            && layer.visible
            && photocraft_engine::cmd::manga_cmds::sfx::sfx_gpu_compatible(live_sfx)
        {
            crate::canvas::sfx_gpu_preview::paint(
                painter,
                xf.rect,
                *xf,
                live_sfx.clone(),
                st.doc.resolution_dpi.max(1.0),
                layer.opacity,
            );
        }
        let live_sfx = live_sfx_for(app, st.doc.id, st.active_layer);
        let sfx_v = sfx_view_from(&st.doc, st.active_layer, live_sfx, app.active_tool() == Tool::Sfx, is_panel_active(app, "sfx"), xf);
        photocraft_manga_ui::sfx_gizmo::draw(painter, &app.manga.ui.sfx_gizmo, &sfx_v);
        if let Some(drag) = app.manga.sfx_create_drag {
            draw_sfx_create_marquee(painter, xf, drag);
        }
    }

    /// Crisp dashed selection box + `W × H px` badge for an in-progress `Tool::Sfx` drag.
    fn draw_sfx_create_marquee(painter: &egui::Painter, xf: &ViewXform, drag: SfxCreateDrag) {
        let a = xf.to_screen(drag.start_doc[0] as f32, drag.start_doc[1] as f32);
        let b = xf.to_screen(drag.cur_doc[0] as f32, drag.cur_doc[1] as f32);
        let r = egui::Rect::from_two_pos(a, b);
        if r.width() < 4.0 || r.height() < 4.0 {
            return;
        }
        let t = manga_theme::Tokens::get(painter.ctx());
        let stroke = egui::Stroke::new(1.0, t.accent);
        let dash = |p0: egui::Pos2, p1: egui::Pos2| {
            let len = p0.distance(p1);
            let steps = (len / 10.0).floor() as usize;
            for i in 0..steps {
                let s = p0 + (p1 - p0) * ((i as f32 * 10.0) / len.max(1.0));
                let e = p0 + (p1 - p0) * ((i as f32 * 10.0 + 6.0) / len.max(1.0));
                painter.line_segment([s, e], stroke);
            }
        };
        dash(r.left_top(), r.right_top());
        dash(r.right_top(), r.right_bottom());
        dash(r.right_bottom(), r.left_bottom());
        dash(r.left_bottom(), r.left_top());
        let zoom = f64::from(xf.zoom.max(0.05));
        let label = format!("{} × {} px", ((r.width() as f64) / zoom).round() as i32, ((r.height() as f64) / zoom).round() as i32);
        painter.text(r.left_top() - egui::vec2(0.0, 4.0), egui::Align2::LEFT_BOTTOM, label, egui::FontId::proportional(11.0), t.text);
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

    /// Commits a full-resolution SFX render once the user releases the mouse after dragging a
    /// control in the SFX dock, standard Character dock, or Canvas Bar.
    fn flush_pending_sfx_full_render(app: &mut PhotocraftApp, ctx: &egui::Context) {
        // 1. Poll any in-flight non-blocking WASM GPU tiled commit first.
        if let Some(rs) = app.gpu_render_state.clone()
            && let Some(done) = app.manga.sfx_gpu_async_bake.poll(&rs, ctx)
        {
            let _ = photocraft_engine::cmd::manga_cmds::sfx::commit_sfx_gpu_surface(
                &mut app.session,
                done.doc_id,
                done.layer_id,
                done.data,
                done.surface,
                done.coalesce,
            );
            if app.manga.sfx_gpu_live_draft.as_ref().is_some_and(|(d, l, _)| *d == done.doc_id && *l == done.layer_id) {
                app.manga.sfx_gpu_live_draft = None;
                app.manga.sfx_gpu_layer_cleared = None;
            }
            ctx.request_repaint();
            return;
        }

        let pointer_down = ctx.input(|i| i.pointer.primary_down());
        if app.manga.ui.sfx_panel.pending_full_render && !pointer_down {
            app.manga.ui.sfx_panel.pending_full_render = false;
            if let Some((doc_id, layer_id)) = app.manga.ui.sfx_panel.pending_full_target.take() {
                let live = app
                    .manga
                    .sfx_gpu_live_draft
                    .as_ref()
                    .filter(|(d, l, _)| d.0 == doc_id.0 && *l == layer_id)
                    .map(|(_, _, sfx)| sfx.clone());
                let sfx = live.or_else(|| {
                    app.session
                        .active()
                        .filter(|st| st.doc.id.0 == doc_id.0)
                        .and_then(|st| st.doc.layer(layer_id))
                        .and_then(|l| l.sfx.as_deref().cloned())
                });
                if let Some(sfx) = sfx {
                    let coalesce = format!("sfx-panel:{}:{}", doc_id.0, layer_id.0);
                    let _ = dispatch_sfx_update(
                        app,
                        OutCommand::new(
                            "sfx.update",
                            json!({
                                "layer": layer_id.0,
                                "sfx": sfx,
                                "coalesce": coalesce,
                                "preview": false,
                            }),
                        ),
                    );
                }
            }
            return;
        }
        // Safety net: if a live GPU preview drag ended abnormally (focus loss / alt-tab / widget
        // blur without a release event) and no async bake is running, flush full-res commit.
        if !pointer_down
            && !app.manga.sfx_gpu_async_bake.is_active()
            && !app.manga.ui.sfx_gizmo.has_active_drag()
            && !app.manga.ui.sfx_panel.pending_full_render
            && !app.manga.sfx_screentone_pending_full
            && let Some((doc_id, layer_id, sfx)) = app.manga.sfx_gpu_live_draft.clone()
        {
            let coalesce = format!("sfx-panel:{}:{}", doc_id.0, layer_id.0);
            let _ = dispatch_sfx_update(
                app,
                OutCommand::new(
                    "sfx.update",
                    json!({
                        "layer": layer_id.0,
                        "sfx": sfx,
                        "coalesce": coalesce,
                        "preview": false,
                    }),
                ),
            );
        }
    }

    /// Applies any queued typesetter requests, renders the Canvas Capsule and Layer Search palette.
    pub fn frame_overlays(app: &mut PhotocraftApp, ctx: &egui::Context) {
        sync_tokens(app, ctx);
        typesetter_requests(app, ctx);
        draw_typesetter_aux_windows(app, ctx);
        show_canvas_capsule(app, ctx);
        flush_pending_sfx_full_render(app, ctx);
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
            #[cfg(not(target_arch = "wasm32"))]
            crate::type_tool::capsule_controls(self.app, ui);
            // The desktop Type capsule is driven by native font/options panels; the browser keeps
            // the other capsule controls and skips this one.
            #[cfg(target_arch = "wasm32")]
            let _ = (&self.app, ui);
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
        let balloon_fit_mode = active_balloon_fit_context(app);
        let sfx_target = doc_id.zip(active_layer).and_then(|(did, lid)| {
            doc_arc
                .as_deref()
                .and_then(|d| d.layer(lid))
                .and_then(|l| l.sfx.as_deref().cloned())
                .map(|sfx| (did, lid, sfx))
        });
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
            balloon_fit_mode,
            sfx_target,
        };
        let mut capsule = std::mem::take(&mut app.manga.ui.canvas_capsule);
        let mut tone = app.manga.ui.screentone_ui.clone();
        let out = {
            let mut cb = AppCapsuleCallbacks { app };
            comic_canvas_capsule::show(ctx, &mut capsule, &mut tone, &view, &mut cb)
        };
        app.manga.ui.canvas_capsule = capsule;
        app.manga.ui.screentone_ui = tone;
        if let Some(pending) = out.sfx_preview_pending {
            app.manga.ui.sfx_panel.pending_full_render = true;
            app.manga.ui.sfx_panel.pending_full_target = Some(pending);
        }
        if out.detach_screentone_gizmo {
            app.manga.ui.screentone_gizmo.detach();
        }
        dispatch_commands(app, out.commands);
        if let Some((shape_id, balloon_id, mode)) = out.set_balloon_fit_mode {
            apply_balloon_fit_mode(app, shape_id, balloon_id, mode);
        }
        if out.action_remove_selection {
            #[cfg(not(target_arch = "wasm32"))]
            let _ = crate::retouch_ui::remove_selection(app);
            // The native retouch helper is desktop-only; the browser ignores this capsule action.
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

    /// Visible vector balloons: prefer the active layer, then the one under the click.
    fn balloon_group_at(st: &DocState, anchor: (f32, f32)) -> Option<(LayerId, LiveBalloonGroup)> {
        let mut candidates: Vec<&Layer> = st.doc.layers.iter().rev().filter(|l| l.visible).collect();
        if let Some(active) = st.active_layer
            && let Some(pos) = candidates.iter().position(|l| l.id == active)
        {
            let layer = candidates.remove(pos);
            candidates.insert(0, layer);
        }
        for layer in candidates {
            let LayerContent::Shape(sh) = &layer.content else { continue };
            let Some(LiveShape::BalloonGroup { group }) = &sh.live else { continue };
            if group.items.is_empty() {
                continue;
            }
            let b = sh.path.control_bounds().unwrap_or((0.0, 0.0, 0.0, 0.0));
            let (ax, ay) = (f64::from(anchor.0), f64::from(anchor.1));
            let under_click = ax >= b.0 && ax <= b.2 && ay >= b.1 && ay <= b.3;
            if under_click || Some(layer.id) == st.active_layer {
                return Some((layer.id, group.clone()));
            }
        }
        None
    }

    /// A visible vector balloon hit by a document point, with everything a Type-tool click needs.
    /// The hit tests the balloon *body* (including UniFlash's white centre), not its decorative
    /// outer path with tail and spikes.
    #[derive(Clone, Copy, Debug)]
    pub(crate) struct BalloonHit {
        pub layer_id: LayerId,
        pub balloon_id: u32,
        /// `[x, y, width, height]` of the balloon's safe text rectangle (kept for tests).
        #[allow(dead_code)]
        pub safe_rect: [f64; 4],
        /// The chamber's chosen insertion mode; `Contour` falls back to `safe_rect` for the box
        /// creation step and applies its area path immediately afterwards.
        pub text_fit_mode: BalloonTextFitMode,
        /// The rectangle a new type layer is created in: safe for `Safe`/`Contour`, tail-free
        /// outer body bounds for `Outer`.
        pub target_rect: [f64; 4],
        pub is_dark: bool,
        /// Kept on the hit result for callers that distinguish rectangular balloons (e.g. tests).
        #[allow(dead_code)]
        pub is_rect: bool,
        /// The chamber's existing link, if any (used by callers/tests; a fresh click relinks).
        #[allow(dead_code)]
        pub linked_text_layer: Option<LayerId>,
    }

    /// Topmost visible balloon under `(x, y)`: prefer the active balloon layer, then walk visible
    /// layers from top to bottom, and within a multi-chamber group test chambers from top down.
    pub(crate) fn balloon_at_point(app: &PhotocraftApp, x: f64, y: f64) -> Option<BalloonHit> {
        let st = app.session.active()?;
        let mut candidates: Vec<&Layer> = st.doc.layers.iter().rev().filter(|l| l.visible).collect();
        if let Some(active) = st.active_layer
            && let Some(pos) = candidates.iter().position(|l| l.id == active)
        {
            let layer = candidates.remove(pos);
            candidates.insert(0, layer);
        }
        for layer in candidates {
            let LayerContent::Shape(sh) = &layer.content else { continue };
            let Some(LiveShape::BalloonGroup { group }) = &sh.live else { continue };
            for b in group.items.iter().rev() {
                if b.contains_body_point([x, y]) {
                    let safe_rect = b.safe_text_rect();
                    let target_rect = match b.text_fit_mode {
                        BalloonTextFitMode::Safe | BalloonTextFitMode::Contour => safe_rect,
                        BalloonTextFitMode::Outer => b.outer_text_rect(),
                    };
                    return Some(BalloonHit {
                        layer_id: layer.id,
                        balloon_id: b.id,
                        safe_rect,
                        text_fit_mode: b.text_fit_mode,
                        target_rect,
                        is_dark: vector_balloon_is_dark(sh),
                        is_rect: matches!(b.body, LiveBalloonBody::Rect { .. } | LiveBalloonBody::ChamferRect { .. }),
                        linked_text_layer: b.linked_text_layer,
                    });
                }
            }
        }
        None
    }

    /// The balloon whose fit mode the Canvas Capsule should display: the active balloon layer, or
    /// the balloon linked to the active lettering layer.
    fn active_balloon_fit_context(app: &PhotocraftApp) -> Option<(LayerId, u32, BalloonTextFitMode)> {
        let st = app.session.active()?;
        let active = st.active_layer?;
        if let Some(layer) = st.doc.layer(active)
            && let LayerContent::Shape(sh) = &layer.content
            && let Some(LiveShape::BalloonGroup { group }) = &sh.live
        {
            let b = group
                .items
                .iter()
                .find(|b| Some(b.id) == app.manga.ui.balloon_gizmo.active_balloon_id)
                .or_else(|| group.items.first())?;
            return Some((active, b.id, b.text_fit_mode));
        }
        st.doc.layers.iter().rev().filter(|l| l.visible).find_map(|layer| {
            let LayerContent::Shape(sh) = &layer.content else { return None };
            let Some(LiveShape::BalloonGroup { group }) = &sh.live else { return None };
            let b = group.items.iter().find(|b| b.linked_text_layer == Some(active))?;
            Some((layer.id, b.id, b.text_fit_mode))
        })
    }

    /// Link one chamber of a vector balloon to a text layer (immediately, so the TypeR panel shows
    /// it without waiting for the next TypeR insertion).
    pub(crate) fn link_balloon_to_text_layer(app: &mut PhotocraftApp, shape_id: LayerId, balloon_id: u32, text_id: u64) {
        link_chambers_to_text_layers(app, &[(shape_id, balloon_id, text_id)]);
    }

    /// Apply a Capsule Bar / panel fit-mode choice. The `shape.balloon.update` command rebuilds
    /// the balloon and resynchronises its linked text in the same undo step (`Box` <-> `Area`).
    fn apply_balloon_fit_mode(app: &mut PhotocraftApp, shape_id: LayerId, balloon_id: u32, mode: BalloonTextFitMode) {
        let Some(st) = app.session.active() else { return };
        let Some(layer) = st.doc.layer(shape_id) else { return };
        let LayerContent::Shape(sh) = &layer.content else { return };
        let Some(LiveShape::BalloonGroup { group }) = &sh.live else { return };
        let Some(mut b) = group.items.iter().find(|it| it.id == balloon_id).cloned() else { return };
        if b.text_fit_mode == mode {
            return;
        }
        b.text_fit_mode = mode;
        let _ = app.run("shape.balloon.update", json!({ "layer": shape_id.0, "balloon": b }));
    }

    /// A chamber's pure body contour in document space, for `type.fillPath` in `Contour` mode.
    fn balloon_text_area_path(app: &PhotocraftApp, shape_id: LayerId, balloon_id: u32) -> Option<photocraft_doc::vector::Path> {
        let st = app.session.active()?;
        let layer = st.doc.layer(shape_id)?;
        let LayerContent::Shape(sh) = &layer.content else { return None };
        let Some(LiveShape::BalloonGroup { group }) = &sh.live else { return None };
        group.items.iter().find(|it| it.id == balloon_id).map(|b| b.text_area_path())
    }

    /// Make an existing linked text layer wrap along the balloon's contour.
    pub(crate) fn apply_balloon_contour_to_text(app: &mut PhotocraftApp, shape_id: LayerId, balloon_id: u32, text_id: u64) {
        let Some(path) = balloon_text_area_path(app, shape_id, balloon_id) else { return };
        if app.run("type.fillPath", json!({ "layer": text_id, "path": path })).is_ok() {
            let _ = app.run("type.setStyle", json!({ "layer": text_id, "align": "center", "verticalAlign": "center" }));
        }
    }

    fn color_is_near_white(color: [u8; 4]) -> bool {
        (0.299 * f32::from(color[0]) + 0.587 * f32::from(color[1]) + 0.114 * f32::from(color[2])) >= 230.0
    }

    fn foreground_is_near_white(app: &PhotocraftApp) -> bool {
        let fg = app.session.tools.foreground;
        let rgb = [fg[0], fg[1], fg[2]].map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        color_is_near_white([rgb[0], rgb[1], rgb[2], 255])
    }

    /// Colour a freshly created balloon text layer black or white when auto contrast is on.
    /// Returns the colour applied, if any, so callers can put it straight into `type.create`.
    pub(crate) fn balloon_text_contrast_color(app: &PhotocraftApp, hit: BalloonHit) -> Option<&'static str> {
        if !app.manga.ui.balloon_ui.auto_contrast {
            return None;
        }
        if hit.is_dark {
            Some("#ffffffff")
        } else if foreground_is_near_white(app) {
            Some("#000000ff")
        } else {
            None
        }
    }

    /// Shrink a newly created box text layer until its laid-out lines fit the balloon's safe
    /// rectangle. Bounded (at most ten steps, never below 1 pt) and a no-op for empty text.
    pub(crate) fn auto_fit_text_layer_to_rect(app: &mut PhotocraftApp, text_id: u64, rect: [f64; 4]) {
        let Some(st) = app.session.active() else { return };
        let Some(layer) = st.doc.layer(LayerId(text_id)) else { return };
        let LayerContent::Text(t) = &layer.content else { return };
        if t.text.trim().is_empty() {
            return;
        }
        let Some(mut size) = t.runs.first().map(|r| r.style.size_pt).filter(|s| s.is_finite() && *s > 0.0) else {
            return;
        };
        let (max_w, max_h) = (rect[2].max(1.0) as f32, rect[3].max(1.0) as f32);
        let dpi = st.doc.resolution_dpi.max(1.0);
        let fits = |candidate: f32| {
            let mut probe = t.clone();
            for run in &mut probe.runs {
                run.style.size_pt = candidate;
            }
            let layout = photocraft_text::shared().lock().unwrap_or_else(|e| e.into_inner()).layout(&probe, dpi);
            let top = layout.lines.iter().map(|l| l.baseline - l.ascent).fold(f32::INFINITY, f32::min);
            let bottom = layout.lines.iter().map(|l| l.baseline + l.descent).fold(f32::NEG_INFINITY, f32::max);
            let width = layout.lines.iter().map(|l| l.x1 - l.x0).fold(0.0f32, f32::max);
            (bottom - top) <= max_h && width <= max_w
        };
        if fits(size) {
            return;
        }
        for _ in 0..10 {
            size = (size * 0.9).max(1.0);
            if fits(size) || size <= 1.0 {
                break;
            }
        }
        let _ = app.run("type.setStyle", json!({ "layer": text_id, "size": size }));
    }

    /// Recolour the linked text of every chamber of a balloon layer after its fill changed. Only
    /// called on a fill-change frame so undo history is not spammed every repaint.
    fn recolor_linked_balloon_text(app: &mut PhotocraftApp, layer_id: LayerId) {
        let Some(st) = app.session.active() else { return };
        let Some(layer) = st.doc.layer(layer_id) else { return };
        let LayerContent::Shape(sh) = &layer.content else { return };
        let Some(LiveShape::BalloonGroup { group }) = &sh.live else { return };
        let dark = vector_balloon_is_dark(sh);
        let edits: Vec<(u64, &'static str)> = group
            .items
            .iter()
            .filter_map(|b| {
                let tid = b.linked_text_layer?;
                let current_white = st.doc.layer(tid).and_then(|l| match &l.content {
                    LayerContent::Text(t) => t.runs.first().map(|r| color_is_near_white(r.style.color.to_rgba8())),
                    _ => None,
                });
                let color = if dark {
                    "#ffffffff"
                } else if current_white == Some(true) {
                    "#000000ff"
                } else {
                    return None;
                };
                Some((tid.0, color))
            })
            .collect();
        for (tid, color) in edits {
            let _ = app.run("type.setStyle", json!({ "layer": tid, "color": color }));
        }
    }

    /// Fit the linked text of the active balloon into its safe text rectangle (Auto-fit button).
    fn fit_active_balloon_linked_text(app: &mut PhotocraftApp) {
        let Some(st) = app.session.active() else { return };
        let Some(layer_id) = st.active_layer else { return };
        let Some(LayerContent::Shape(sh)) = st.doc.layer(layer_id).map(|l| &l.content) else { return };
        let Some(LiveShape::BalloonGroup { group }) = &sh.live else { return };
        let active_bid = app.manga.ui.balloon_gizmo.active_balloon_id;
        let Some(b) = active_bid
            .and_then(|id| group.items.iter().find(|it| it.id == id && it.linked_text_layer.is_some()))
            .or_else(|| group.items.iter().find(|it| it.linked_text_layer.is_some()))
        else {
            return;
        };
        if let Some(tid) = b.linked_text_layer {
            auto_fit_text_layer_to_rect(app, tid.0, b.safe_text_rect());
        }
    }

    fn text_layer_center(layer: &Layer) -> Option<[f64; 2]> {
        if let Some(bounds) = layer.surface().map(|s| s.content_bounds()).filter(|b| !b.is_empty()) {
            return Some([f64::from(bounds.x0) + f64::from(bounds.width()) * 0.5, f64::from(bounds.y0) + f64::from(bounds.height()) * 0.5]);
        }
        let LayerContent::Text(t) = &layer.content else { return None };
        let (ox, oy) = (t.transform.m[4], t.transform.m[5]);
        match &t.shape {
            photocraft_doc::text::TextShape::Box { x, y, width, height } => Some([ox + f64::from(*x) + f64::from(*width) * 0.5, oy + f64::from(*y) + f64::from(*height) * 0.5]),
            _ => Some([ox, oy]),
        }
    }

    fn preset_orientation_str(raw: &str) -> &'static str {
        let trimmed = raw.trim();
        if trimmed.eq_ignore_ascii_case("vertical") || trimmed.eq_ignore_ascii_case("vrtc") {
            "vertical"
        } else {
            "horizontal"
        }
    }

    /// "Open / Send to TypeR" on a selected balloon with no linked text: bind an existing text
    /// layer already sitting inside the balloon, or point the TypeR insertion at the balloon's
    /// centre so the next line is created there and linked by `handle_typesetter_action`.
    fn prepare_typer_for_active_balloon(app: &mut PhotocraftApp) {
        let Some(st) = app.session.active() else { return };
        let Some(layer_id) = st.active_layer else { return };
        let Some(LayerContent::Shape(sh)) = st.doc.layer(layer_id).map(|l| &l.content) else { return };
        let Some(LiveShape::BalloonGroup { group }) = &sh.live else { return };
        let active_bid = app.manga.ui.balloon_gizmo.active_balloon_id;
        let Some(b) = active_bid.and_then(|id| group.items.iter().find(|it| it.id == id)).or_else(|| group.items.first()) else {
            return;
        };
        if b.linked_text_layer.is_some() {
            return;
        }
        let inside = st.doc.layers.iter().find(|l| {
            if !l.visible || !matches!(l.content, LayerContent::Text(_)) {
                return false;
            }
            text_layer_center(l).is_some_and(|c| b.contains_body_point(c))
        });
        if let Some(layer) = inside {
            link_balloon_to_text_layer(app, layer_id, b.id, layer.id.0);
        } else {
            let r = b.body.rect();
            app.hover_doc = Some([r[0] + r[2] * 0.5, r[1] + r[3] * 0.5]);
        }
    }

    /// Is text layer `text_id` linked to a visible vector balloon, and is that balloon dark?
    /// Used by TypeR's "Apply to layer" auto contrast.
    fn linked_balloon_is_dark(app: &PhotocraftApp, text_id: LayerId) -> bool {
        let Some(st) = app.session.active() else { return false };
        st.doc.layers.iter().rev().filter(|l| l.visible).any(|layer| {
            let LayerContent::Shape(sh) = &layer.content else { return false };
            let Some(LiveShape::BalloonGroup { group }) = &sh.live else { return false };
            group.items.iter().any(|b| b.linked_text_layer == Some(text_id)) && vector_balloon_is_dark(sh)
        })
    }

    /// Is this text layer explicitly linked to any visible vector balloon?
    fn text_is_balloon_linked(app: &PhotocraftApp, text_id: LayerId) -> bool {
        let Some(st) = app.session.active() else { return false };
        st.doc.layers.iter().rev().filter(|l| l.visible).any(|layer| {
            let LayerContent::Shape(sh) = &layer.content else { return false };
            let Some(LiveShape::BalloonGroup { group }) = &sh.live else { return false };
            group.items.iter().any(|b| b.linked_text_layer == Some(text_id))
        })
    }

    /// Is this text layer sitting inside a visible vector balloon (even if not explicitly linked)?
    fn text_inside_balloon_dark(app: &PhotocraftApp, text_id: LayerId) -> bool {
        let Some(st) = app.session.active() else { return false };
        let Some(layer) = st.doc.layer(text_id) else { return false };
        let Some([cx, cy]) = text_layer_center(layer) else { return false };
        balloon_at_point(app, cx, cy).is_some_and(|h| h.is_dark)
    }

    #[derive(Clone, Copy)]
    struct OrderedChamber {
        rect: photocraft_geom::Rect,
        balloon_id: Option<u32>,
        balloon_layer: Option<LayerId>,
        is_rect: bool,
        linked_text_layer: Option<LayerId>,
    }

    fn live_balloon_safe_rect(b: &LiveBalloon) -> photocraft_geom::Rect {
        let r = b.safe_text_rect();
        photocraft_geom::Rect::from_xywh(r[0].round() as i32, r[1].round() as i32, r[2].max(1.0).round() as u32, r[3].max(1.0).round() as u32)
    }

    fn sort_rects_reading_order(rects: &[photocraft_geom::Rect], rtl: bool) -> Vec<usize> {
        let boxes: Vec<[f64; 4]> = rects.iter().map(|r| [f64::from(r.x0), f64::from(r.y0), f64::from(r.width()), f64::from(r.height())]).collect();
        sort_chamber_indices_reading_order(&boxes, rtl)
    }

    fn balloon_group_ordered_chambers(layer_id: LayerId, group: &LiveBalloonGroup, rtl: bool) -> Vec<OrderedChamber> {
        let raw_rects: Vec<photocraft_geom::Rect> = group.items.iter().map(live_balloon_safe_rect).collect();
        sort_rects_reading_order(&raw_rects, rtl)
            .into_iter()
            .filter_map(|idx| {
                let b = group.items.get(idx)?;
                Some(OrderedChamber {
                    rect: raw_rects[idx],
                    balloon_id: Some(b.id),
                    balloon_layer: Some(layer_id),
                    is_rect: matches!(b.body, LiveBalloonBody::Rect { .. } | LiveBalloonBody::ChamferRect { .. }),
                    linked_text_layer: b.linked_text_layer,
                })
            })
            .collect()
    }

    /// Chamber rectangles of a vector balloon group, straight from each balloon's safe text area.
    fn balloon_group_chambers(group: &LiveBalloonGroup) -> Vec<photocraft_geom::Rect> {
        balloon_group_ordered_chambers(LayerId(0), group, true).into_iter().map(|c| c.rect).collect()
    }

    /// A white mask standing in for a bubble's interior, for the 64-line contour fitter.
    /// Oval/round balloons use an elliptical silhouette (`rx`, `ry`) so top and bottom rows are
    /// narrower than the middle; rectangular balloons keep the full rectangle.
    fn synthetic_bubble_selection(rect: photocraft_geom::Rect, rectangular: bool) -> Surface {
        let mut surface = Surface::new(PixelFormat::RGBA32F);
        if rectangular || rect.width() < 8 || rect.height() < 8 {
            surface.fill_rect(rect, &[1.0, 1.0, 1.0, 1.0]);
            return surface;
        }
        let cx = f64::from(rect.x0) + f64::from(rect.width()) * 0.5;
        let cy = f64::from(rect.y0) + f64::from(rect.height()) * 0.5;
        let rx = (f64::from(rect.width()) * 0.5).max(1.0);
        let ry = (f64::from(rect.height()) * 0.5).max(1.0);
        for y in rect.y0..rect.y1 {
            let ny = ((f64::from(y) + 0.5 - cy) / ry).clamp(-1.0, 1.0);
            let span = (1.0 - ny * ny).max(0.0).sqrt() * rx;
            let x0 = (cx - span).floor() as i32;
            let x1 = (cx + span).ceil() as i32;
            let row = photocraft_geom::Rect::new(x0.max(rect.x0), y, x1.min(rect.x1), y + 1);
            if !row.is_empty() {
                surface.fill_rect(row, &[1.0, 1.0, 1.0, 1.0]);
            }
        }
        surface
    }

    /// A black vector balloon needs inverted (white) lettering. `Uniflash + Solid` keeps its black
    /// needles on a white centre, so it is explicitly excluded.
    fn vector_balloon_is_dark(sh: &ShapeLayer) -> bool {
        let Some(LiveShape::BalloonGroup { group }) = &sh.live else {
            return false;
        };
        let dark_fill = match &sh.fill {
            Some(Fill::Solid(c)) => {
                let rgb = c.to_rgb();
                rgb[0] < 0.2 && rgb[1] < 0.2 && rgb[2] < 0.2
            }
            _ => false,
        };
        dark_fill && !group.items.iter().all(|b| matches!(b.body, LiveBalloonBody::Uniflash { .. }) && b.line_style == BalloonLineStyle::Solid)
    }

    fn typesetter_text_is_dark(color: [u8; 4]) -> bool {
        (0.299 * f32::from(color[0]) + 0.587 * f32::from(color[1]) + 0.114 * f32::from(color[2])) < 118.0
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
                let anchor =
                    app.hover_doc.map(|[x, y]| (x as f32, y as f32)).unwrap_or((state.doc.size.width as f32 * 0.5, state.doc.size.height as f32 * 0.5));
                let bubble = selection.or_else(|| {
                    balloon_group_at(state, anchor)
                        .and_then(|(_, group)| balloon_group_chambers(&group).first().copied())
                        .or_else(|| typesetter_adapter::detect_speech_bubble_bounds(&app.manga.ui.typesetter.state, &state.doc, anchor))
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
                .or_else(|| balloon_group_at(st, (anchor[0] as f32, anchor[1] as f32)).and_then(|(_, group)| balloon_group_chambers(&group).first().copied()))
                .or_else(|| {
                    st.doc
                        .layers
                        .iter()
                        .rev()
                        .filter(|l| l.visible && l.id != id)
                        .find_map(|l| l.surface().and_then(|s| detect_speech_bubble_bounds_in_surface(s, anchor[0], anchor[1], 240)))
                })
                .or_else(|| typesetter_adapter::detect_speech_bubble_bounds(&app.manga.ui.typesetter.state, &st.doc, (anchor[0] as f32, anchor[1] as f32)))
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
        let (new_point, new_hash, keep_frame) = if let Some(st) = app.session.active()
            && let Some(l) = st.doc.layer(LayerId(layer))
            && let LayerContent::Text(t) = &l.content
        {
            // Box/area type (including every balloon-linked layer) keeps its frame: `type.edit`'s
            // `point` parameter would collapse it into a point layer at the box's top-left.
            let keep_frame = matches!(t.shape, photocraft_doc::text::TextShape::Box { .. } | photocraft_doc::text::TextShape::Area { .. })
                || text_is_balloon_linked(app, LayerId(layer));
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
            (if keep_frame { None } else { pt }, Some(hash), keep_frame)
        } else {
            (None, None, false)
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
        if keep_frame {
            // Centring inside the preserved box/area, not just horizontally on a point layer.
            let _ = app.run("type.setStyle", json!({ "layer": layer, "align": "center", "verticalAlign": "center" }));
        } else {
            let _ = app.run("type.setStyle", json!({ "layer": layer, "align": "center" }));
        }
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
        let mut hex = format!("#{:02x}{:02x}{:02x}{:02x}", preset.text_color[0], preset.text_color[1], preset.text_color[2], preset.text_color[3]);
        let orientation = preset_orientation_str(&preset.orientation);
        let (on_dark_balloon, linked, box_or_area) = app.session.active().map_or((false, false, false), |st| {
            let shape = st.doc.layer(target_id).and_then(|l| match &l.content {
                LayerContent::Text(t) => Some(t.shape.clone()),
                _ => None,
            });
            let box_or_area = matches!(shape, Some(photocraft_doc::text::TextShape::Box { .. } | photocraft_doc::text::TextShape::Area { .. }));
            (
                linked_balloon_is_dark(app, target_id) || text_inside_balloon_dark(app, target_id),
                text_is_balloon_linked(app, target_id),
                box_or_area,
            )
        });
        if app.manga.ui.balloon_ui.auto_contrast && on_dark_balloon && typesetter_text_is_dark(preset.text_color) {
            hex = "#ffffffff".into();
        }
        let leading = if preset.leading > 0.0 { json!(preset.leading) } else { json!("auto") };
        let mut params = json!({
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
            "direction": preset.direction,
            "orientation": orientation
        });
        // Box and area type (including balloon-linked layers) should be centred vertically by
        // "Apply to layer" instead of being left at the top of their frame.
        if box_or_area || linked {
            params["verticalAlign"] = json!("center");
        }
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
        // A lettering layer paired with a balloon centres inside that chamber, not in the
        // marquee selection or the whole canvas.
        let linked = app.session.active().and_then(|st| {
            let id = st.active_layer?;
            st.doc.layers.iter().rev().filter(|l| l.visible).find_map(|layer| {
                let LayerContent::Shape(sh) = &layer.content else { return None };
                let Some(LiveShape::BalloonGroup { group }) = &sh.live else { return None };
                let b = group.items.iter().find(|b| b.linked_text_layer == Some(id))?;
                Some((layer.id, b.clone(), id.0))
            })
        });
        if let Some((shape_id, b, text_id)) = linked {
            let fitted = if b.text_fit_mode == BalloonTextFitMode::Contour {
                apply_balloon_contour_to_text(app, shape_id, b.id, text_id);
                true
            } else {
                let r = if b.text_fit_mode == BalloonTextFitMode::Outer { b.outer_text_rect() } else { b.safe_text_rect() };
                app.run("type.edit", json!({ "layer": text_id, "box": r })).is_ok()
            };
            let style = app.run("type.setStyle", json!({ "layer": text_id, "align": "center", "verticalAlign": "center" }));
            if fitted && style.is_ok() {
                app.ui.status = crate::i18n::tr(crate::i18n::current(), "Layer centered in its balloon").into();
                app.ui.status_error = false;
            } else {
                app.ui.status = crate::i18n::tr(crate::i18n::current(), "Align failed").into();
                app.ui.status_error = true;
            }
            return;
        }
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

    /// Text for each chamber of a double/triple bubble. Two chambers use the overflow splitter
    /// (`//` and blank lines are explicit separators); extra chambers take successive script lines.
    fn chamber_text_parts(text: &str, chambers: &[photocraft_geom::Rect], font_size: f32, dpi: f32, h_scale: f32) -> Vec<String> {
        if chambers.len() == 2 {
            let a = (chambers[0].width() as f32, chambers[0].height() as f32);
            let b = (chambers[1].width() as f32, chambers[1].height() as f32);
            let (first, second) = knuth_plass_split_overflow(text, a, b, font_size, dpi, h_scale);
            let mut parts = vec![first];
            if let Some(second) = second {
                parts.push(second);
            }
            return parts;
        }
        let lines: Vec<String> = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(ToOwned::to_owned).collect();
        (0..chambers.len()).map(|i| lines.get(i).cloned().unwrap_or_default()).collect()
    }

    struct ChamberFitInput<'a> {
        raw_text: &'a str,
        preset: &'a TextStylePreset,
        fit_surface: Option<&'a photocraft_raster::Surface>,
        selection_box: Option<[f32; 4]>,
        doc_bounds: photocraft_geom::Rect,
        doc_dpi: f32,
        line_spacing_sync: bool,
        minimum_fit_scale_percent: f32,
    }

    fn fit_text_for_chamber(input: &ChamberFitInput<'_>) -> (String, f32, f32, Option<String>) {
        let mut final_text = input.raw_text.to_string();
        let mut font_size = input.preset.font_size;
        let mut leading = if input.preset.leading > 0.0 { input.preset.leading } else { font_size * 1.2 };
        if input.preset.punctuation_only_scale > 1.0 && is_punctuation_only(&final_text) {
            font_size *= input.preset.punctuation_only_scale;
            leading *= input.preset.punctuation_only_scale;
        }
        let mut contour_fitted = false;
        let mut err_msg = None;
        if let Some(surface) = input.fit_surface {
            let mut fitted_preset = input.preset.clone();
            match fit_script_text_to_selection(
                &final_text,
                &mut fitted_preset,
                surface,
                input.doc_bounds,
                input.doc_dpi,
                input.line_spacing_sync,
                input.minimum_fit_scale_percent,
            ) {
                Ok(Some(fitted)) => {
                    final_text = fitted;
                    font_size = fitted_preset.font_size;
                    leading = if fitted_preset.leading > 0.0 { fitted_preset.leading } else { font_size * 1.2 };
                    contour_fitted = true;
                }
                Ok(None) => {}
                Err(error) => {
                    err_msg = Some(format!("TypeR contour fit: {error}"));
                }
            }
        }
        if !contour_fitted && let Some([_bx, _by, bw, bh]) = input.selection_box {
            final_text =
                knuth_plass_bubble_wrap_ext(&final_text, bw, bh, font_size, input.doc_dpi, input.preset.horizontal_scale, input.preset.hanging_punctuation);
            let h_scale = if input.preset.horizontal_scale > 1.5 { input.preset.horizontal_scale / 100.0 } else { 1.0 };
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
                    knuth_plass_bubble_wrap_ext(&final_text, bw, bh, font_size, input.doc_dpi, input.preset.horizontal_scale, input.preset.hanging_punctuation);
            }
        } else if !contour_fitted {
            final_text = bind_orphan_prepositions(&final_text);
        }
        (final_text, font_size, leading, err_msg)
    }

    fn apply_chamber_runs_and_stroke(app: &mut PhotocraftApp, layer_id: u64, markup_runs: &[Value], preset: &TextStylePreset, invert_for_dark_bubble: bool) {
        if !markup_runs.is_empty() {
            let _ = app.run("type.edit", json!({ "layer": layer_id, "runs": markup_runs }));
        }
        if preset.stroke_width > 0.0 && !invert_for_dark_bubble {
            let stroke_hex = format!("#{:02x}{:02x}{:02x}", preset.stroke_color[0], preset.stroke_color[1], preset.stroke_color[2]);
            let _ = app.run(
                "layer.layerStyle.stroke",
                json!({
                    "layer": layer_id,
                    "size": preset.stroke_width,
                    "position": "outside",
                    "color": stroke_hex,
                    "opacity": (f32::from(preset.stroke_color[3]) / 2.55).round() as u32
                }),
            );
        }
    }

    fn upsert_chamber_text(
        app: &mut PhotocraftApp,
        existing: Option<LayerId>,
        style_base: &Value,
        create_params: Value,
        text: &str,
        story_threaded: bool,
    ) -> Option<u64> {
        let can_reuse = existing.and_then(|tid| {
            let st = app.session.active()?;
            let l = st.doc.layer(tid)?;
            if matches!(l.content, LayerContent::Text(_)) && !st.doc.effective_locks(tid).all { Some(tid.0) } else { None }
        });
        if let Some(tid) = can_reuse {
            let mut edit = json!({
                "layer": tid,
                "replace": { "start": 0, "end": usize::MAX, "text": text },
                "storyThreaded": story_threaded,
            });
            if let Some(b) = create_params.get("box") {
                edit["box"] = b.clone();
            }
            if app.run("type.edit", edit).is_ok() {
                let mut st_params = style_base.clone();
                st_params["layer"] = json!(tid);
                let _ = app.run("type.setStyle", st_params);
                return Some(tid);
            }
        }
        let mut p = create_params;
        if story_threaded {
            p["storyThreaded"] = json!(true);
        }
        app.run("type.create", p).ok().and_then(|v| v.get("layer").and_then(Value::as_u64))
    }

    fn link_chambers_to_text_layers(app: &mut PhotocraftApp, links: &[(LayerId, u32, u64)]) {
        for &(blid, bid, tid) in links {
            let updated_balloon = app.session.active().and_then(|st| {
                let l = st.doc.layer(blid)?;
                let LayerContent::Shape(sh) = &l.content else { return None };
                let Some(LiveShape::BalloonGroup { group }) = &sh.live else { return None };
                let mut b = group.items.iter().find(|it| it.id == bid)?.clone();
                if b.linked_text_layer == Some(LayerId(tid)) {
                    return None;
                }
                b.linked_text_layer = Some(LayerId(tid));
                Some(b)
            });
            if let Some(b) = updated_balloon {
                let contour = b.text_fit_mode == BalloonTextFitMode::Contour;
                let _ = app.run("shape.balloon.update", json!({ "layer": blid.0, "balloon": b }));
                if contour {
                    // The engine already made it an Area, but reassert the inline path so a
                    // freshly linked layer is centred inside the body immediately.
                    apply_balloon_contour_to_text(app, blid, bid, tid);
                }
            }
        }
    }

    pub fn handle_typesetter_action(app: &mut PhotocraftApp, action: TypesetterAction) {
        if app.manga.ui.typesetter.is_blocking_canvas_insertion() {
            return;
        }
        let rtl = app.manga.ui.typesetter.state.options.manga_rtl_reading_order;
        let (selection_box, auto_clean_params, doc_dpi, doc_size, doc_bounds, fit_surface, chambers, dark_bubble) = {
            let Some(st) = app.session.active() else {
                app.ui.status = "Open or create a document to insert text".into();
                app.ui.status_error = true;
                return;
            };
            let anchor = app.hover_doc.map_or((st.doc.size.width as f32 * 0.5, st.doc.size.height as f32 * 0.5), |[x, y]| (x as f32, y as f32));
            let vector = balloon_group_at(st, anchor);
            let detected_raster = if st.doc.selection.is_none() && vector.is_none() {
                typesetter_adapter::detect_speech_bubble_bounds(&app.manga.ui.typesetter.state, &st.doc, anchor)
            } else {
                None
            };
            let ordered = match &st.doc.selection {
                Some(sel) => {
                    let raw = detect_bubble_chambers(sel, sel.content_bounds());
                    sort_rects_reading_order(&raw, rtl)
                        .into_iter()
                        .map(|i| OrderedChamber { rect: raw[i], balloon_id: None, balloon_layer: None, is_rect: false, linked_text_layer: None })
                        .collect()
                }
                None => match &vector {
                    Some((blid, g)) => balloon_group_ordered_chambers(*blid, g, rtl),
                    None => detected_raster
                        .into_iter()
                        .map(|r| OrderedChamber { rect: r, balloon_id: None, balloon_layer: None, is_rect: false, linked_text_layer: None })
                        .collect(),
                },
            };
            let rect_box = |r: photocraft_geom::Rect| [r.x0 as f32, r.y0 as f32, r.width() as f32, r.height() as f32];
            let sel_box = st
                .doc
                .selection
                .as_ref()
                .and_then(|sel| {
                    let b = sel.content_bounds();
                    (!b.is_empty()).then(|| rect_box(b))
                })
                .or_else(|| ordered.first().map(|c| rect_box(c.rect)));
            let fit_surface = st.doc.selection.clone().or_else(|| ordered.first().map(|c| synthetic_bubble_selection(c.rect, c.is_rect)));
            let dark = match &st.doc.selection {
                Some(sel) => {
                    let art = st.active_layer.and_then(|id| st.doc.layer(id)).and_then(|l| l.surface());
                    snow_clean::clean_fill_color(art, sel) == [0, 0, 0]
                }
                None => vector
                    .as_ref()
                    .and_then(|(id, _)| st.doc.layer(*id))
                    .and_then(|l| match &l.content {
                        LayerContent::Shape(sh) => Some(vector_balloon_is_dark(sh)),
                        _ => None,
                    })
                    .unwrap_or(false),
            };
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
            (sel_box, clean, st.doc.resolution_dpi, st.doc.size, st.doc.bounds(), fit_surface, ordered, dark)
        };

        let source_text = if action.preset.all_caps { action.text.to_uppercase() } else { action.text };
        // Inline `**bold**` / `*italic*` markers become editable character runs after fitting.
        let (plain_text, markup_words) = parse_script_markup(&source_text);
        let (line_spacing_sync, minimum_fit_scale_percent, optical_factor) = {
            let opts = &app.manga.ui.typesetter.state.options;
            (opts.line_spacing_sync, opts.minimum_fit_scale_percent, opts.optical_baseline_factor)
        };

        let (resolved_family, style_name, faux_bold, faux_italic, weight) = resolve_preset_font(&action.preset);
        // Lettering contrast is opt-out: an explicit dark fill only inverts TypeR's text when
        // the Balloons panel's auto-contrast preference is on.
        let invert_for_dark_bubble = app.manga.ui.balloon_ui.auto_contrast && dark_bubble && typesetter_text_is_dark(action.preset.text_color);
        let hex_color = if invert_for_dark_bubble {
            "#ffffffff".to_string()
        } else {
            format!(
                "#{:02x}{:02x}{:02x}{:02x}",
                action.preset.text_color[0], action.preset.text_color[1], action.preset.text_color[2], action.preset.text_color[3]
            )
        };
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
        let style_template = json!({
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
            "horizontalScale": action.preset.horizontal_scale,
            "color": hex_color,
            "kerning": if is_optical { "optical" } else { "metrics" },
            "hangingRoman": action.preset.hanging_punctuation,
            "firstLineIndent": action.preset.first_line_indent,
            "startIndent": action.preset.start_indent,
            "endIndent": action.preset.end_indent,
            "spaceAfter": action.preset.space_after,
            "hyphenate": action.preset.hyphenate,
            "direction": action.preset.direction,
            "orientation": preset_orientation_str(&action.preset.orientation),
        });

        if let Some((clean_layer_id, hex_fill)) = auto_clean_params {
            if let Some(clean_id) = clean_layer_id {
                let _ = app.run("layer.select", json!({ "layer": clean_id.0, "mode": "replace" }));
            } else {
                let _ = app.run("layer.new.layer", json!({ "name": snow_clean::CLEAN_LAYER }));
            }
            let _ = app.run("edit.fill", json!({ "color": hex_fill }));
        }

        let hover_doc = app.hover_doc;
        let make_create_params = |text: &str, size: f32, leading: f32, box_rect: Option<[f32; 4]>| {
            let mut p = style_template.clone();
            p["text"] = json!(text);
            p["size"] = json!(size);
            p["leading"] = json!(leading);
            p["antialias"] = json!(action.preset.anti_alias);
            p["autoKern"] = json!(if is_optical { "opticalKern" } else { "metricsKern" });
            if !action.preset.font_postscript.is_empty() {
                p["postscriptName"] = Value::String(action.preset.font_postscript.clone());
            }
            if let Some([bx, by, bw, bh]) = box_rect {
                p["box"] = json!([bx, by, bw, bh]);
                p["verticalAlign"] = json!("center");
                let optical = size * optical_factor;
                if optical != 0.0 {
                    p["baselineShift"] = json!(-optical);
                }
            } else {
                let (click_x, click_y) = hover_doc.map_or((doc_size.width as f32 * 0.5, doc_size.height as f32 * 0.5), |[x, y]| (x as f32, y as f32));
                let px_per_pt = if doc_dpi.is_finite() && doc_dpi > 0.0 { doc_dpi / 72.0 } else { 1.0 };
                let line_count = text.lines().count().max(1);
                let baseline_y = click_y - ((line_count - 1) as f32 * (leading * px_per_pt)) * 0.5 + (size * px_per_pt) * 0.35;
                p["x"] = json!(click_x);
                p["y"] = json!(baseline_y);
            }
            p
        };

        if chambers.len() > 1 {
            let rects: Vec<photocraft_geom::Rect> = chambers.iter().map(|c| c.rect).collect();
            let parts = chamber_text_parts(&plain_text, &rects, action.preset.font_size, doc_dpi, action.preset.horizontal_scale);
            let mut created: Vec<u64> = Vec::new();
            let mut pending_links: Vec<(LayerId, u32, u64)> = Vec::new();
            for (idx, chamber) in chambers.iter().enumerate() {
                let raw_part = parts.get(idx).cloned().unwrap_or_default();
                let part_surface = synthetic_bubble_selection(chamber.rect, chamber.is_rect);
                let part_box = [chamber.rect.x0 as f32, chamber.rect.y0 as f32, chamber.rect.width() as f32, chamber.rect.height() as f32];
                let (part_text, part_size, part_leading, fit_err) = fit_text_for_chamber(&ChamberFitInput {
                    raw_text: &raw_part,
                    preset: &action.preset,
                    fit_surface: Some(&part_surface),
                    selection_box: Some(part_box),
                    doc_bounds,
                    doc_dpi,
                    line_spacing_sync,
                    minimum_fit_scale_percent,
                });
                if let Some(err) = fit_err {
                    app.ui.status = err;
                    app.ui.status_error = true;
                }
                let mut part_style = style_template.clone();
                part_style["size"] = json!(part_size);
                part_style["leading"] = json!(part_leading);
                part_style["verticalAlign"] = json!("center");
                let params = make_create_params(&part_text, part_size, part_leading, Some(part_box));
                if let Some(tid) = upsert_chamber_text(app, chamber.linked_text_layer, &part_style, params, &part_text, true) {
                    let part_runs = script_markup_run_params(&part_text, &markup_words);
                    apply_chamber_runs_and_stroke(app, tid, &part_runs, &action.preset, invert_for_dark_bubble);
                    created.push(tid);
                    if let (Some(blid), Some(bid)) = (chamber.balloon_layer, chamber.balloon_id) {
                        pending_links.push((blid, bid, tid));
                    }
                }
            }
            for (idx, id) in created.iter().enumerate() {
                let mut edit = json!({ "layer": id, "storyThreaded": true });
                edit["nextStoryLayer"] = created.get(idx + 1).map_or(Value::Null, |n| json!(n));
                let _ = app.run("type.edit", edit);
            }
            link_chambers_to_text_layers(app, &pending_links);
            let _ = app.run("select.deselect", json!({}));
            return;
        }

        let (final_text, font_size, leading, fit_err) = fit_text_for_chamber(&ChamberFitInput {
            raw_text: &plain_text,
            preset: &action.preset,
            fit_surface: fit_surface.as_ref(),
            selection_box,
            doc_bounds,
            doc_dpi,
            line_spacing_sync,
            minimum_fit_scale_percent,
        });
        if let Some(err) = fit_err {
            app.ui.status = err;
            app.ui.status_error = true;
        }
        let markup_runs = script_markup_run_params(&final_text, &markup_words);
        let mut single_style = style_template.clone();
        single_style["size"] = json!(font_size);
        single_style["leading"] = json!(leading);
        if selection_box.is_some() {
            single_style["verticalAlign"] = json!("center");
        }
        let params = make_create_params(&final_text, font_size, leading, selection_box);
        let existing_single = chambers.first().and_then(|c| c.linked_text_layer);
        if let Some(layer_id) = upsert_chamber_text(app, existing_single, &single_style, params, &final_text, false) {
            apply_chamber_runs_and_stroke(app, layer_id, &markup_runs, &action.preset, invert_for_dark_bubble);
            if let Some(ch) = chambers.first()
                && let (Some(blid), Some(bid)) = (ch.balloon_layer, ch.balloon_id)
            {
                link_chambers_to_text_layers(app, &[(blid, bid, layer_id)]);
            }
            let _ = app.run("select.deselect", json!({}));
        }
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

        /// The capsule's own font list must be allowed to stay open: hiding the capsule whenever
        /// any egui popup is open closed the very list the user had just opened.
        #[test]
        fn the_capsule_font_popup_stays_open_and_can_pick_a_family() {
            use crate::state::Tool;
            use egui::accesskit::Role;
            use egui_kittest::Harness;
            use egui_kittest::kittest::Queryable;

            let _ = photocraft_text::shared();
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
            app.run("file.new", json!({"width": 800, "height": 600})).unwrap();
            app.run("type.create", json!({"text": "Sample", "font": "Ubuntu-Light", "x": 200, "y": 200})).unwrap();
            app.ui.tool_options.type_font = "Ubuntu-Light".into();
            app.session.edit_prefs(|p| p.seam.canvas_capsule = true);
            app.ui.panels.canvas_capsule = true;
            app.ui.tool = Tool::Type;
            app.sync_views();

            let mut h = Harness::builder().with_size(egui::vec2(1000.0, 700.0)).with_max_steps(16).build_ui_state(
                |ui, app: &mut PhotocraftApp| {
                    if !ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                        return;
                    }
                    show_canvas_capsule(app, ui.ctx());
                },
                app,
            );
            PhotocraftApp::setup_context(&h.ctx, Default::default());
            h.run_steps(6);

            h.get_by_role_and_label(Role::Button, "Font").click();
            h.run_steps(4);
            assert!(egui::Popup::is_any_open(&h.ctx), "the family popup opened");

            // These extra frames used to hide the capsule (and with it the popup) entirely.
            h.run_steps(6);
            assert!(egui::Popup::is_any_open(&h.ctx), "the popup is still open after several frames");
            assert!(h.query_by_role_and_label(Role::Button, "Font").is_some(), "the capsule is still drawn");

            // The list itself is live: arrow to the next family and accept it with Enter.
            h.key_press(egui::Key::ArrowDown);
            h.run_steps(2);
            h.key_press(egui::Key::Enter);
            h.run_steps(2);
            assert_ne!(h.state().ui.tool_options.type_font, "Ubuntu-Light", "a family can be picked from the kept-open list");
        }

        #[test]
        fn typesetter_dual_chamber_manga_rtl_splits_fits_styles_and_links_stories() {
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
            app.run("file.new", json!({"name": "Dual", "width": 900, "height": 600})).unwrap();

            let right_b =
                LiveBalloon { id: 1, body: LiveBalloonBody::Ellipse { rect: [460.0, 120.0, 220.0, 140.0], squircle: 0.25 }, ..LiveBalloon::default() };
            let created = app.run("shape.balloon.create", json!({ "balloon": right_b })).unwrap();
            let balloon_lid = created["layer"].as_u64().unwrap();
            let left_b = LiveBalloon { id: 2, body: LiveBalloonBody::Ellipse { rect: [140.0, 140.0, 220.0, 140.0], squircle: 0.25 }, ..LiveBalloon::default() };
            app.run("shape.balloon.add", json!({ "layer": balloon_lid, "balloon": left_b })).unwrap();
            app.hover_doc = Some([560.0, 190.0]);
            app.manga.ui.typesetter.state.options.manga_rtl_reading_order = true;

            let preset = TextStylePreset { all_caps: false, font_size: 14.0, stroke_width: 2.0, ..TextStylePreset::default() };
            handle_typesetter_action(&mut app, TypesetterAction { text: "**Right** first // Left second".to_string(), preset });

            let st = app.session.active().unwrap();
            let LayerContent::Shape(sh) = &st.doc.layer(LayerId(balloon_lid)).unwrap().content else {
                panic!("expected shape");
            };
            let Some(LiveShape::BalloonGroup { group }) = &sh.live else {
                panic!("expected group");
            };
            let tid_right = group.items[0].linked_text_layer.expect("right chamber linked");
            let tid_left = group.items[1].linked_text_layer.expect("left chamber linked");
            assert_ne!(tid_right, tid_left);

            let l_right = st.doc.layer(tid_right).unwrap();
            let LayerContent::Text(tl_right) = &l_right.content else { panic!("expected text") };
            assert!(tl_right.text.contains("Right"), "RTL puts first part into right chamber: {:?}", tl_right.text);
            assert!(tl_right.story_threaded);
            assert_eq!(tl_right.next_story_layer, Some(tid_left));
            assert!(!l_right.effects.items.is_empty(), "stroke effect applied per chamber");

            let l_left = st.doc.layer(tid_left).unwrap();
            let LayerContent::Text(tl_left) = &l_left.content else { panic!("expected text") };
            assert!(tl_left.text.contains("Left"), "RTL puts second part into left chamber: {:?}", tl_left.text);
            assert!(!l_left.effects.items.is_empty(), "stroke effect applied to second chamber too");
        }

        /// The Manga dock exposes SFX, Balloons and Linework (plus the other manga panels) as
        /// first-class tabs, and the legacy panel command opens the unified dock on the right tab.
        #[test]
        fn manga_dock_tabs_expose_sfx_balloons_linework_and_open_on_command() {
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
            app.run("file.new", json!({ "width": 256, "height": 256 })).unwrap();

            for (id, title) in [
                ("sfx", "SFX"),
                ("balloon", "Balloons"),
                ("linework", "Linework"),
                ("screentone", "Screentones"),
                ("typesetter", "TypeR Manga"),
            ] {
                assert!(MANGA_PANELS.iter().any(|p| p.0 == id && p.1 == title), "{id}/{title} must be a manga dock tab");
                assert!(panel_owns(id), "{id} must be owned by the manga seam");
                assert_eq!(panel_group(id), Some(Group::Manga));
                assert_eq!(panel_title(id), Some(title));
                assert!(manga_tab_index(id).is_some(), "{id} needs a canonical tab index");
                let rect = panel_default_rect(&app, id);
                assert_eq!(rect.len(), 4);
                assert!(rect[2] > 0.0 && rect[3] > 0.0, "{id} needs a positive default rect");
            }

            assert_eq!(normalize_panel("SFX"), Some("sfx"));
            assert_eq!(normalize_panel("Balloons"), Some("balloon"));
            assert_eq!(normalize_panel("Linework"), Some("linework"));
            assert_eq!(normalize_panel("TypeR"), Some("typesetter"));

            // Opening the SFX tab creates the unified Manga dock and marks it active/checked.
            let opened = panel_command(&mut app, "window.panel.sfx", &json!({ "show": true })).expect("dock command").expect("ok");
            assert_eq!(opened["showing"], true);
            assert!(app.ui.panels.manga);
            assert_eq!(app.ui.dock_tabs.manga, manga_tab_index("sfx").unwrap());
            assert_eq!(panel_checked(&app, "window.panel.sfx"), Some(true));
            assert_eq!(panel_checked(&app, "window.panel.balloon"), Some(false));
        }

        /// Pressing the canvas with the SFX tool creates an SFX layer at the press point; the
        /// draft's text and origin travel onto that layer through the `sfx.create` command.
        #[test]
        fn manga_sfx_canvas_press_creates_layer_at_point() {
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
            app.run("file.new", json!({ "width": 256, "height": 256 })).unwrap();
            assert!(app.session.active().unwrap().doc.layers.iter().all(|l| l.sfx.is_none()));

            app.manga.ui.sfx_panel.draft.text = "BAM!".into();
            app.manga.ui.sfx_panel.draft.validate();
            sfx_canvas_press(&mut app, [48.0, 64.0]);

            let st = app.session.active().unwrap();
            let sfx_layers: Vec<_> = st.doc.layers.iter().filter(|l| l.sfx.is_some()).collect();
            assert_eq!(sfx_layers.len(), 1, "one canvas press creates exactly one SFX layer");
            let data = sfx_layers[0].sfx.as_ref().unwrap();
            assert_eq!(data.text, "BAM!");
            assert_eq!(data.origin, [48.0, 64.0]);
            assert_eq!(st.active_layer, Some(sfx_layers[0].id), "the new SFX layer becomes active");
        }

        /// A press on a 600-DPI / 12303×17499 page must scale fresh SFX defaults up, and a new
        /// layer must not inherit the distorted warps still held in the panel draft.
        #[test]
        fn sfx_canvas_press_scales_defaults_and_resets_stale_warps_on_large_canvas() {
            use photocraft_doc::sfx::SfxGlyphTransform;

            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
            app.run("file.new", json!({ "width": 12303, "height": 17499 })).unwrap();

            let mut draft = app.manga.ui.sfx_panel.draft.clone();
            draft.text = "BAM".into();
            draft.free_transform.corners = [[120.0, -40.0]; 4];
            draft.free_transform.perspective_x = 0.4;
            draft.spline.points = vec![[10.0, 10.0], [-10.0, -10.0]];
            draft.per_glyph = vec![SfxGlyphTransform { glyph_index: 0, dx: 30.0, corners: [[9.0, 9.0]; 4], ..SfxGlyphTransform::default() }];
            draft.liquify_strokes.push(Default::default());
            draft.mesh.points[0] = [-2.0, -2.0];
            app.manga.ui.sfx_panel.draft = draft;

            sfx_canvas_press(&mut app, [100.0, 100.0]);

            let st = app.session.active().unwrap();
            let data = st
                .doc
                .layers
                .iter()
                .filter_map(|l| l.sfx.as_deref())
                .next()
                .expect("one SFX layer on the large canvas");
            assert!(data.size_pt > 54.0 * 5.0, "size_pt must scale with the document: {}", data.size_pt);
            assert_eq!(data.free_transform.corners, [[0.0; 2]; 4]);
            assert_eq!(data.free_transform.perspective_x, 0.0);
            assert!(data.spline.points.is_empty(), "arc points must be reset");
            assert!(data.per_glyph.is_empty(), "per-glyph deltas must be reset");
            assert!(data.liquify_strokes.is_empty(), "liquify strokes must be reset");
            assert!(data.mesh.is_identity(), "mesh offsets must be reset");
        }

        /// A dragged rectangle fits the SFX size to the box, keeps the default warp mode and
        /// centers the word inside the drawn box.
        #[test]
        fn manga_sfx_drag_to_create_fits_size_pt_to_box() {
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
            app.run("file.new", json!({ "width": 256, "height": 256 })).unwrap();
            app.manga.ui.sfx_panel.draft.text = "BAM".into();
            app.manga.ui.sfx_panel.draft.validate();

            let start = [20.0_f64, 24.0];
            let end = [180.0_f64, 124.0];
            assert!(!sfx_canvas_down(&mut app, start, egui::Modifiers::NONE));
            sfx_create_move(&mut app, end, egui::Modifiers::NONE);
            sfx_create_finish(&mut app, end, egui::Modifiers::NONE);
            assert!(app.manga.sfx_create_drag.is_none(), "finishing clears the drag state");

            let st = app.session.active().unwrap();
            let sfx = st.doc.layers.iter().filter_map(|l| l.sfx.as_deref()).next().expect("dragged SFX layer");
            assert_eq!(sfx.warp_mode, SfxWarpMode::Resize);
            assert!(sfx.origin[0] >= start[0] - 1.0 && sfx.origin[1] >= start[1] - 1.0, "word must sit in the drawn box");
            assert!(sfx.size_pt > 10.0 && sfx.size_pt < 400.0, "size_pt must be fitted, got {}", sfx.size_pt);

            let geom = build_sfx_geometry(sfx, &mut text_engine(), st.doc.resolution_dpi.max(1.0));
            let cx = (geom.rest_box[0] + geom.rest_box[2]) * 0.5;
            let cy = (geom.rest_box[1] + geom.rest_box[3]) * 0.5;
            assert!((cx - (start[0] + end[0]) * 0.5).abs() < 4.0, "word must be centered in the box: {cx}");
            assert!((cy - (start[1] + end[1]) * 0.5).abs() < 4.0, "word must be centered in the box: {cy}");
            assert!((geom.rest_box[2] - geom.rest_box[0]) <= end[0] - start[0] + 4.0);
            assert!((geom.rest_box[3] - geom.rest_box[1]) <= end[1] - start[1] + 4.0);
        }

        /// With Shift the fitted SFX is stretched to the exact box aspect through `free_transform`.
        #[test]
        fn manga_sfx_drag_to_create_shift_stretches_to_box_aspect() {
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
            app.run("file.new", json!({ "width": 256, "height": 256 })).unwrap();
            app.manga.ui.sfx_panel.draft.text = "BAM".into();
            let size_before = app.manga.ui.sfx_panel.draft.size_pt;

            // Very wide, shallow box (~8.6:1) so Shift stretching must widen scale_x/scale_y well past 1.5x.
            let start = [8.0_f64, 16.0];
            let end = [248.0_f64, 44.0];
            assert!(!sfx_canvas_down(&mut app, start, egui::Modifiers::SHIFT));
            sfx_create_move(&mut app, end, egui::Modifiers::SHIFT);
            sfx_create_finish(&mut app, end, egui::Modifiers::SHIFT);

            let st = app.session.active().unwrap();
            let sfx = st.doc.layers.iter().filter_map(|l| l.sfx.as_deref()).next().expect("stretched SFX layer");
            assert!(
                (sfx.free_transform.scale_x - 1.0).abs() > 0.05 || (sfx.free_transform.scale_y - 1.0).abs() > 0.05,
                "expected non-unit scale, got ({}, {})",
                sfx.free_transform.scale_x,
                sfx.free_transform.scale_y
            );
            assert!(
                sfx.free_transform.scale_x / sfx.free_transform.scale_y > 1.5,
                "box is much wider than tall: ({}, {})",
                sfx.free_transform.scale_x,
                sfx.free_transform.scale_y
            );
            assert!(sfx.size_pt != size_before, "size_pt is still fitted to the box");
            assert_eq!(sfx.warp_mode, SfxWarpMode::Resize);
        }

        /// A plain click with a non-empty selection fits the new SFX to that selection rectangle.
        #[test]
        fn manga_sfx_plain_click_with_selection_fits_selection_bounds() {
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
            app.run("file.new", json!({ "width": 256, "height": 256 })).unwrap();
            let sel = [40.0_f64, 60.0, 200.0, 140.0];
            app.run(
                "select.rect",
                json!({ "x": sel[0], "y": sel[1], "width": sel[2] - sel[0], "height": sel[3] - sel[1] }),
            )
            .unwrap();

            let click = [10.0_f64, 10.0];
            assert!(!sfx_canvas_down(&mut app, click, egui::Modifiers::NONE));
            sfx_create_finish(&mut app, click, egui::Modifiers::NONE);

            let st = app.session.active().unwrap();
            let sfx = st.doc.layers.iter().filter_map(|l| l.sfx.as_deref()).next().expect("selection-fitted SFX layer");
            let geom = build_sfx_geometry(sfx, &mut text_engine(), st.doc.resolution_dpi.max(1.0));
            let cx = (geom.rest_box[0] + geom.rest_box[2]) * 0.5;
            let cy = (geom.rest_box[1] + geom.rest_box[3]) * 0.5;
            assert!((cx - (sel[0] + sel[2]) * 0.5).abs() < 4.0, "SFX must center in the selection: {cx}");
            assert!((cy - (sel[1] + sel[3]) * 0.5).abs() < 4.0, "SFX must center in the selection: {cy}");
            assert!(geom.rest_box[2] - geom.rest_box[0] <= sel[2] - sel[0] + 4.0);
            assert!(geom.rest_box[3] - geom.rest_box[1] <= sel[3] - sel[1] + 4.0);
        }

        /// The top Options Bar renders its mode selector and switching a mode updates the SFX draft.
        #[test]
        fn manga_sfx_options_bar_renders_and_switches_mode() {
            use egui_kittest::kittest::Queryable;
            use egui_kittest::Harness;
            use photocraft_doc::sfx::SfxWarpMode;

            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
            app.run("file.new", json!({ "width": 256, "height": 256 })).unwrap();
            app.manga.ui.sfx_panel.draft.warp_mode = SfxWarpMode::FreeTransform;

            let mut h = Harness::builder().with_size(egui::vec2(900.0, 110.0)).with_max_steps(6).build_ui_state(
                |ui, app: &mut PhotocraftApp| sfx_options_bar(app, ui),
                app,
            );
            PhotocraftApp::setup_context(&h.ctx, Default::default());
            h.run_steps(2);
            assert!(h.query_by_label("SFX Mode:").is_some(), "the mode selector is labelled");
            h.get_by_label("По точкам (Alt)").click();
            h.run_steps(2);
            assert_eq!(h.state().manga.ui.sfx_panel.draft.warp_mode, SfxWarpMode::PerGlyph);
        }

        /// Live-editing an SFX layer's screentone fill through the Gentonik dock keeps the global
        /// `screentone_ui` settings untouched and supports both primary and secondary fills.
        #[test]
        fn manga_sfx_gentonik_link_isolates_global_screentone_settings() {
            use photocraft_algo::{PatternType, ScreentoneSpec};

            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
            app.run("file.new", json!({ "width": 256, "height": 256 })).unwrap();
            app.manga.ui.screentone_ui.spec.dot_size = 11.5;
            app.manga.ui.screentone_ui.spec.pattern_type = PatternType::Crosshatch;

            let sfx = SfxData {
                text: "TONE".into(),
                fill: SfxFillSource::Screentone {
                    spec: ScreentoneSpec { pattern_type: PatternType::Dots, dot_size: 4.0, spacing_x: 7.0, spacing_y: 7.0, ..Default::default() },
                    opacity: 1.0,
                },
                ..SfxData::default()
            };
            app.run("sfx.create", json!({ "sfx": sfx })).unwrap();

            let (_, _, _, mut live_sfx, spec, is_sec) = active_sfx_screentone(&app).expect("active SFX screentone detected");
            assert!(!is_sec);
            assert_eq!(spec.pattern_type, PatternType::Dots);

            // Editing the SFX spec must leave global Gentonik settings completely untouched.
            let mut next_spec = spec;
            next_spec.pattern_type = PatternType::Lines;
            next_spec.dot_size = 6.0;
            apply_sfx_screentone_spec(&mut live_sfx, next_spec, false);
            assert!(matches!(live_sfx.fill, SfxFillSource::Screentone { ref spec, .. } if spec.pattern_type == PatternType::Lines));
            assert_eq!(app.manga.ui.screentone_ui.spec.dot_size, 11.5);
            assert_eq!(app.manga.ui.screentone_ui.spec.pattern_type, PatternType::Crosshatch);

            // Secondary fill Screentone is also detected when primary fill is Solid.
            let mut sec_sfx = SfxData { fill: SfxFillSource::Solid { color: photocraft_doc::Color::WHITE }, ..SfxData::default() };
            sec_sfx.blending.secondary_fill = Some(SfxFillSource::Screentone { spec: ScreentoneSpec::default(), opacity: 0.8 });
            app.run("sfx.create", json!({ "sfx": sec_sfx })).unwrap();
            let (_, _, _, _, _, is_sec2) = active_sfx_screentone(&app).expect("secondary SFX screentone detected");
            assert!(is_sec2);
        }

        /// Every `Group::Manga` tab (and the `photocraft-manga-ui` panels behind them) renders for
        /// several frames without panicking when hosted by PhotoCraft's shell.
        #[test]
        fn manga_panels_render_without_panic() {
            use egui_kittest::Harness;

            let _ = photocraft_text::shared();
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
            app.run("file.new", json!({ "width": 320, "height": 240 })).unwrap();
            app.run("type.create", json!({ "text": "BAM!", "x": 40.0, "y": 60.0 })).unwrap();
            app.ui.panels.manga = true;

            // `build_ui_state` lays out frame 0 before `setup_context` installs PhotoCraft's
            // bundled fonts. Skip that frame (the project's standard guard) so glyphs such as "●"
            // are only laid out once the real font families are present.
            let steps = MANGA_PANELS.len() * 2;
            let mut h = Harness::builder().with_size(egui::vec2(1100.0, 820.0)).with_max_steps(steps as u64).allow_missing_glyphs().build_ui_state(
                move |ui, app: &mut PhotocraftApp| {
                    if !ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                        return;
                    }
                    let idx = app.ui.dock_tabs.manga.min(MANGA_PANELS.len() - 1);
                    let id = MANGA_PANELS[idx].0;
                    assert!(render_panel(app, ui, id), "{id} must render through the manga seam");
                    app.ui.dock_tabs.manga = (idx + 1) % MANGA_PANELS.len();
                },
                app,
            );
            PhotocraftApp::setup_context(&h.ctx, Default::default());
            h.run_steps(steps);
        }

        /// Switching the unified Manga dock's tabs renders real egui controls for each panel.
        #[test]
        fn manga_dock_tab_switching_renders_controls_without_panic() {
            use egui::accesskit::Role;
            use egui_kittest::kittest::Queryable as _;
            use egui_kittest::Harness;

            let _ = photocraft_text::shared();
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
            app.run("file.new", json!({ "width": 320, "height": 240 })).unwrap();
            app.ui.panels.manga = true;
            app.ui.dock_tabs.manga = 0;

            let mut h = Harness::builder().with_size(egui::vec2(1100.0, 820.0)).with_max_steps(4).allow_missing_glyphs().build_ui_state(
                |ui, app: &mut PhotocraftApp| {
                    if !ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                        return;
                    }
                    let idx = app.ui.dock_tabs.manga.min(MANGA_PANELS.len() - 1);
                    render_dock_tab(app, ui, idx);
                },
                app,
            );
            PhotocraftApp::setup_context(&h.ctx, Default::default());

            let mut total = 0usize;
            for tab in 0..MANGA_PANELS.len() {
                h.state_mut().ui.dock_tabs.manga = tab;
                h.run_steps(2);
                let controls = h.query_all_by_role(Role::Button).count()
                    + h.query_all_by_role(Role::SpinButton).count()
                    + h.query_all_by_role(Role::Slider).count()
                    + h.query_all_by_role(Role::TextInput).count();
                assert!(controls > 0, "{} tab must render real controls", MANGA_PANELS[tab].0);
                total += controls;
            }
            assert!(total > 0);
        }

        fn new_balloon_app() -> PhotocraftApp {
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
            app.run("file.new", json!({ "width": 800, "height": 600 })).unwrap();
            app
        }

        fn ellipse_balloon(balloon_id: u32, rect: [f64; 4]) -> LiveBalloon {
            LiveBalloon { id: balloon_id, body: LiveBalloonBody::Ellipse { rect, squircle: 0.25 }, ..LiveBalloon::default() }
        }

        fn newest_text_layer(app: &PhotocraftApp) -> (u64, photocraft_doc::TextLayer) {
            let st = app.session.active().unwrap();
            st.doc
                .layers
                .iter()
                .rev()
                .find_map(|l| match &l.content {
                    LayerContent::Text(t) => Some((l.id.0, t.clone())),
                    _ => None,
                })
                .unwrap()
        }

        fn balloon_link(app: &PhotocraftApp, layer_id: u64, balloon_id: u32) -> Option<LayerId> {
            let st = app.session.active().unwrap();
            let LayerContent::Shape(sh) = &st.doc.layer(LayerId(layer_id)).unwrap().content else { return None };
            let Some(LiveShape::BalloonGroup { group }) = &sh.live else { return None };
            group.items.iter().find(|b| b.id == balloon_id)?.linked_text_layer
        }

        /// A Type-tool click in UniFlash's white centre hits the balloon body (not its spikes),
        /// creates a centred box on the safe rect, and links it immediately.
        #[test]
        fn type_click_in_uniflash_white_center_creates_centered_linked_box() {
            let mut app = new_balloon_app();
            let b = LiveBalloon {
                id: 1,
                body: LiveBalloonBody::Uniflash {
                    rect: [200.0, 100.0, 240.0, 160.0],
                    needles: 160,
                    inner_ratio: 0.62,
                    bundle_group: 0.4,
                    length_jitter: 0.5,
                    angle_jitter: 0.25,
                    needle_width: 2.2,
                    outer_sharpness: 1.0,
                    inner_sharpness: 1.0,
                    mode: Default::default(),
                    seed: 7,
                },
                line_style: BalloonLineStyle::Solid,
                ..LiveBalloon::default()
            };
            let created = app.run("shape.balloon.create", json!({ "balloon": b })).unwrap();
            let shape_id = created["layer"].as_u64().unwrap();

            let hit = balloon_at_point(&app, 320.0, 180.0).expect("the white centre must hit the balloon");
            assert_eq!(hit.balloon_id, 1);
            assert!(!hit.is_rect);
            assert!(!hit.is_dark, "Uniflash + Solid has a white text centre");
            let sr = hit.safe_rect;

            crate::tools::type_tool::pointer_up(&mut app, [320.0, 180.0], [320.0, 180.0]);

            let (text_id, t) = newest_text_layer(&app);
            let (w, h) = match t.shape {
                photocraft_doc::text::TextShape::Box { width, height, .. } => (width, height),
                _ => panic!("a balloon click must create paragraph text, got {:?}", t.shape),
            };
            assert!((f64::from(w) - sr[2].max(1.0)).abs() < 1.0, "box width follows the safe rect: {w} vs {}", sr[2]);
            assert!((f64::from(h) - sr[3].max(1.0)).abs() < 1.0, "box height follows the safe rect: {h} vs {}", sr[3]);
            assert!((t.transform.m[4] - sr[0]).abs() < 1.0, "box origin x follows the safe rect");
            assert!((t.transform.m[5] - sr[1]).abs() < 1.0, "box origin y follows the safe rect");
            assert_eq!(t.paragraphs[0].style.align, photocraft_doc::text::TextAlign::Center);
            assert_eq!(t.vertical_align(), photocraft_doc::text::VerticalAlign::Center);
            assert_eq!(balloon_link(&app, shape_id, 1), Some(LayerId(text_id)), "TypeR link shows the text at once");
        }

        /// A dark balloon with auto contrast on gives new lettering white; an already linked
        /// text layer is recoloured when the fill is toggled white, and back when it goes dark.
        #[test]
        fn dark_balloon_click_white_text_and_fill_toggle_recolor_linked_text() {
            let mut app = new_balloon_app();
            let created = app.run("shape.balloon.create", json!({ "balloon": ellipse_balloon(5, [100.0, 100.0, 200.0, 140.0]) })).unwrap();
            let shape_id = created["layer"].as_u64().unwrap();
            app.run("shape.edit", json!({ "layer": shape_id, "fill": "#000000" })).unwrap();
            assert!(balloon_at_point(&app, 200.0, 170.0).unwrap().is_dark);
            app.manga.ui.balloon_ui.auto_contrast = true;

            crate::tools::type_tool::pointer_up(&mut app, [200.0, 170.0], [200.0, 170.0]);
            let (text_id, t) = newest_text_layer(&app);
            assert_eq!(t.runs[0].style.color.to_rgba8(), [255, 255, 255, 255], "dark fill inverts TypeR text to white");
            assert_eq!(balloon_link(&app, shape_id, 5), Some(LayerId(text_id)));

            // Fill toggled to white: the linked text must go black on the very next fill-change
            // frame (the seam calls `recolor_linked_balloon_text` only on those frames).
            app.run("shape.edit", json!({ "layer": shape_id, "fill": "#ffffff" })).unwrap();
            recolor_linked_balloon_text(&mut app, LayerId(shape_id));
            assert_eq!(newest_text_layer(&app).1.runs[0].style.color.to_rgba8(), [0, 0, 0, 255]);

            app.run("shape.edit", json!({ "layer": shape_id, "fill": "#000000" })).unwrap();
            recolor_linked_balloon_text(&mut app, LayerId(shape_id));
            assert_eq!(newest_text_layer(&app).1.runs[0].style.color.to_rgba8(), [255, 255, 255, 255]);
        }

        /// "Apply to layer" in TypeR propagates vertical orientation and centred vertical align,
        /// and inverts dark lettering on a dark linked balloon when auto contrast is on.
        #[test]
        fn typesetter_apply_style_propagates_orientation_vertical_align_and_contrast() {
            let mut app = new_balloon_app();
            let created = app.run("shape.balloon.create", json!({ "balloon": ellipse_balloon(9, [100.0, 100.0, 200.0, 140.0]) })).unwrap();
            let shape_id = created["layer"].as_u64().unwrap();
            app.run("shape.edit", json!({ "layer": shape_id, "fill": "#000000" })).unwrap();
            app.run(
                "type.create",
                json!({ "text": "Hello", "font": "Inter", "size": 14.0, "box": [110.0, 110.0, 180.0, 120.0], "color": "#000000ff" }),
            )
            .unwrap();
            let (text_id, _) = newest_text_layer(&app);
            link_balloon_to_text_layer(&mut app, LayerId(shape_id), 9, text_id);
            app.run("layer.select", json!({ "layer": text_id })).unwrap();

            let preset = TextStylePreset {
                font_family: "Inter".into(),
                font_size: 14.0,
                orientation: "vertical".into(),
                text_color: [0, 0, 0, 255],
                ..TextStylePreset::default()
            };
            apply_typesetter_style_to_active_layer(&mut app, &preset);

            let (_, t) = newest_text_layer(&app);
            assert_eq!(t.orientation, photocraft_doc::text::Orientation::Vertical);
            assert_eq!(t.vertical_align(), photocraft_doc::text::VerticalAlign::Center);
            assert_eq!(t.runs[0].style.color.to_rgba8(), [255, 255, 255, 255], "dark preset on a dark balloon becomes white");
        }

        /// TextShapeR applied to a balloon-linked Box or Area text layer updates only the text
        /// string, keeping its Box/Area frame and centred vertical alignment intact.
        #[test]
        fn text_shape_r_preserves_balloon_box_and_area_frames() {
            let mut app = new_balloon_app();
            let created = app.run("shape.balloon.create", json!({ "balloon": ellipse_balloon(11, [120.0, 90.0, 220.0, 160.0]) })).unwrap();
            let shape_id = created["layer"].as_u64().unwrap();
            crate::tools::type_tool::pointer_up(&mut app, [230.0, 170.0], [230.0, 170.0]);
            let (text_id, before_box) = newest_text_layer(&app);
            assert!(matches!(before_box.shape, photocraft_doc::text::TextShape::Box { .. }));

            apply_typesetter_text_shape(&mut app, text_id, "Lorem\nIpsum".into());
            let (_, after_box) = newest_text_layer(&app);
            assert_eq!(after_box.text, "Lorem\nIpsum");
            assert_eq!(after_box.shape, before_box.shape, "TextShapeR must not turn a Box into Point");
            assert!((after_box.transform.m[4] - before_box.transform.m[4]).abs() < 1e-3);
            assert!((after_box.transform.m[5] - before_box.transform.m[5]).abs() < 1e-3);
            assert_eq!(after_box.paragraphs[0].style.align, photocraft_doc::text::TextAlign::Center);
            assert_eq!(after_box.vertical_align(), photocraft_doc::text::VerticalAlign::Center);

            // Switch to Contour (Area) and verify TextShapeR keeps the Area contour too.
            apply_balloon_fit_mode(&mut app, LayerId(shape_id), 11, BalloonTextFitMode::Contour);
            let (_, before_area) = newest_text_layer(&app);
            assert!(matches!(before_area.shape, photocraft_doc::text::TextShape::Area { .. }));

            apply_typesetter_text_shape(&mut app, text_id, "One\nTwo\nThree".into());
            let (_, after_area) = newest_text_layer(&app);
            assert_eq!(after_area.text, "One\nTwo\nThree");
            assert!(matches!(after_area.shape, photocraft_doc::text::TextShape::Area { .. }), "Area shape must survive TextShapeR");
            assert_eq!(after_area.vertical_align(), photocraft_doc::text::VerticalAlign::Center);
        }

        /// Aligning a linked text layer in TypeR centres it inside its balloon chamber according
        /// to the balloon's fit mode, not at the centre of the 800x600 canvas.
        #[test]
        fn align_typesetter_layer_centers_in_linked_balloon_not_canvas() {
            let mut app = new_balloon_app();
            let b = ellipse_balloon(12, [40.0, 40.0, 200.0, 140.0]);
            let safe = b.safe_text_rect();
            let created = app.run("shape.balloon.create", json!({ "balloon": b })).unwrap();
            let shape_id = created["layer"].as_u64().unwrap();
            crate::tools::type_tool::pointer_up(&mut app, [140.0, 110.0], [140.0, 110.0]);
            let (text_id, _) = newest_text_layer(&app);
            // Push the text box far away toward the canvas bottom-right.
            app.run("type.edit", json!({ "layer": text_id, "box": [500.0, 400.0, 100.0, 60.0] })).unwrap();
            app.run("layer.select", json!({ "layer": text_id })).unwrap();

            align_typesetter_layer(&mut app);

            let (_, t) = newest_text_layer(&app);
            let (w, h) = match t.shape {
                photocraft_doc::text::TextShape::Box { width, height, .. } => (f64::from(width), f64::from(height)),
                _ => panic!("expected Box shape in Safe mode, got {:?}", t.shape),
            };
            assert!((t.transform.m[4] - safe[0]).abs() < 1.0, "aligned x matches balloon safe rect, not canvas centre");
            assert!((t.transform.m[5] - safe[1]).abs() < 1.0, "aligned y matches balloon safe rect, not canvas centre");
            assert!((w - safe[2]).abs() < 1.0 && (h - safe[3]).abs() < 1.0);
            assert_eq!(t.paragraphs[0].style.align, photocraft_doc::text::TextAlign::Center);
            assert_eq!(t.vertical_align(), photocraft_doc::text::VerticalAlign::Center);
            assert_eq!(balloon_link(&app, shape_id, 12), Some(LayerId(text_id)));
        }

        /// Capsule Bar fit-mode switching exposes the balloon context on both the balloon layer
        /// and its linked text layer, and immediately switches Safe -> Outer -> Contour -> Safe.
        #[test]
        fn capsule_bar_fit_mode_switch_refits_linked_text_between_safe_outer_and_contour() {
            let mut app = new_balloon_app();
            let b = ellipse_balloon(13, [100.0, 100.0, 220.0, 160.0]);
            let created = app.run("shape.balloon.create", json!({ "balloon": b })).unwrap();
            let shape_id = created["layer"].as_u64().unwrap();
            crate::tools::type_tool::pointer_up(&mut app, [210.0, 180.0], [210.0, 180.0]);
            let (text_id, safe_t) = newest_text_layer(&app);
            let safe_w = match safe_t.shape {
                photocraft_doc::text::TextShape::Box { width, .. } => width,
                _ => panic!("expected Safe Box"),
            };

            // Context resolves when the linked text layer is active.
            app.run("layer.select", json!({ "layer": text_id })).unwrap();
            assert_eq!(active_balloon_fit_context(&app), Some((LayerId(shape_id), 13, BalloonTextFitMode::Safe)));

            // Switch to Outer: same text layer, wider box covering the tail-free body bounds.
            apply_balloon_fit_mode(&mut app, LayerId(shape_id), 13, BalloonTextFitMode::Outer);
            assert_eq!(active_balloon_fit_context(&app), Some((LayerId(shape_id), 13, BalloonTextFitMode::Outer)));
            let (id_after_outer, outer_t) = newest_text_layer(&app);
            assert_eq!(id_after_outer, text_id, "switching mode must not recreate the text layer");
            let outer_w = match outer_t.shape {
                photocraft_doc::text::TextShape::Box { width, .. } => width,
                _ => panic!("expected Outer Box"),
            };
            assert!(outer_w > safe_w + 10.0, "Outer box ({outer_w}) must be wider than Safe box ({safe_w})");

            // Switch to Contour: same text layer, becomes TextShape::Area.
            apply_balloon_fit_mode(&mut app, LayerId(shape_id), 13, BalloonTextFitMode::Contour);
            let (id_after_contour, contour_t) = newest_text_layer(&app);
            assert_eq!(id_after_contour, text_id);
            assert!(matches!(contour_t.shape, photocraft_doc::text::TextShape::Area { .. }));

            // Switch back to Safe: restores TextShape::Box on the same layer.
            apply_balloon_fit_mode(&mut app, LayerId(shape_id), 13, BalloonTextFitMode::Safe);
            let (id_after_safe, back_t) = newest_text_layer(&app);
            assert_eq!(id_after_safe, text_id);
            assert!(matches!(back_t.shape, photocraft_doc::text::TextShape::Box { .. }));
        }

        #[test]
        fn screentone_tool_click_fills_and_drag_creates_angled_gradient() {
            use photocraft_algo::ScreentoneGradientType;
            let mut app = new_balloon_app();
            app.ui.tool = Tool::Screentone;
            app.ui.tool_options.screentone_clip = true;
            // Click (< 4px move) creates a screentone layer preserving existing gradient_type and clipped flag.
            screentone_drag_finish(&mut app, [10.0, 10.0], [10.5, 10.5]);
            let st1 = app.session.active().unwrap();
            let id1 = st1.active_layer.unwrap();
            let layer1 = st1.doc.layer(id1).unwrap();
            assert!(layer1.is_screentone());
            assert!(layer1.clipped, "click honors screentone_clip");

            // Drag (>= 4px) configures Linear gradient at the drag angle and binds gizmo.
            app.ui.tool_options.screentone_clip = false;
            app.ui.tool_options.screentone_drag_gradient = "diamond".into();
            screentone_drag_finish(&mut app, [20.0, 20.0], [60.0, 60.0]);
            let st2 = app.session.active().unwrap();
            let id2 = st2.active_layer.unwrap();
            assert_ne!(id1, id2);
            let LayerContent::Screentone(tone2) = &st2.doc.layer(id2).unwrap().content else {
                panic!("expected ScreentoneLayer");
            };
            assert_eq!(tone2.spec.grad_type, ScreentoneGradientType::Diamond);
            assert!((tone2.spec.grad_angle - 45.0).abs() < 1e-3, "got {}", tone2.spec.grad_angle);
            assert!(!st2.doc.layer(id2).unwrap().clipped);
        }
    }
}

pub use native::*;
