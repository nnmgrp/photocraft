#![forbid(unsafe_code)]

#[macro_use]
pub mod i18n;
pub mod balloon_gizmo;
pub mod comic_balloon_panel;
pub mod comic_canvas_capsule;
pub mod comic_kakeami_panel;
pub mod comic_pages_panel;
pub mod comic_sai_panel;
pub mod comic_screentone_gizmo;
pub mod comic_screentone_panel;
pub mod layer_search;
pub mod scrub_wrap;
pub mod snow_breaks_ui;
pub mod snow_clean;
pub mod snow_fx_scrub;
pub mod snow_typer_style;
pub mod snow_typesetter_ui;
pub mod theme;
pub mod view;
pub mod widgets;

pub use photocraft_manga_bridge::{BridgeDocId, BridgeLayerId, CanvasSeamContext, DocRef, HostSnapshot, OutCommand, SeamFlags};

/// Combined UI state owned on the desktop host for all suitcase manga panels and gizmos.
pub struct MangaUiState {
    pub typesetter: snow_typesetter_ui::TypesetterUi,
    pub balloon_ui: comic_balloon_panel::BalloonPanelUi,
    pub balloon_gizmo: balloon_gizmo::BalloonGizmoState,
    pub linework_panel: comic_sai_panel::LineworkPanelState,
    pub screentone_ui: comic_screentone_panel::ScreentoneUi,
    pub screentone_gizmo: comic_screentone_gizmo::ScreentoneGizmoState,
    pub kakeami_ui: comic_kakeami_panel::KakeamiPanelUi,
    pub pages_panel: comic_pages_panel::PagesPanelState,
    pub canvas_capsule: comic_canvas_capsule::CanvasCapsuleState,
    pub sfx_panel: sfx_panel::SfxPanelState,
    pub sfx_gizmo: sfx_gizmo::SfxGizmoState,
    pub layer_search_open: bool,
}

pub mod sfx_gizmo;
pub mod sfx_panel;

impl Default for MangaUiState {
    fn default() -> Self {
        Self {
            typesetter: snow_typesetter_ui::TypesetterUi::new(),
            balloon_ui: comic_balloon_panel::BalloonPanelUi::default(),
            balloon_gizmo: balloon_gizmo::BalloonGizmoState::default(),
            linework_panel: comic_sai_panel::LineworkPanelState::default(),
            screentone_ui: comic_screentone_panel::ScreentoneUi::default(),
            screentone_gizmo: comic_screentone_gizmo::ScreentoneGizmoState::default(),
            kakeami_ui: comic_kakeami_panel::KakeamiPanelUi::default(),
            pages_panel: comic_pages_panel::PagesPanelState::default(),
            canvas_capsule: comic_canvas_capsule::CanvasCapsuleState::default(),
            sfx_panel: sfx_panel::SfxPanelState::default(),
            sfx_gizmo: sfx_gizmo::SfxGizmoState::default(),
            layer_search_open: false,
        }
    }
}
