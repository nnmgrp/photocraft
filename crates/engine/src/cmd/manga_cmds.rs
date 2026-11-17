//! Manga-specific engine commands and session state ported for 0.23.

use photocraft_manga_core::snow_typesetter::TypesetterState;

use crate::commands::CommandSpec;

pub mod actions_set;
pub mod balloon;
pub mod brush_preset_items;
pub mod comic_screentone;
pub mod fill_tool;
pub mod gigapixel;
pub mod layer_extras;
pub mod mask_transfer;
pub mod second_mask;
pub mod timelapse;
pub mod typesetter_adapter;
pub mod vector_brush;
pub mod vector_curve;

/// Session-scoped state owned by manga commands and panels.
#[derive(Default)]
pub struct MangaSessionState {
    pub typesetter: TypesetterState,
    pub(crate) fill_reference: std::sync::Mutex<Option<fill_tool::ReferenceCache>>,
    pub(crate) fill_gesture: std::sync::Mutex<Option<fill_tool::Gesture>>,
    pub timelapse: timelapse::recorder::Recorder,
    pub vector_brushes: vector_brush::VectorBrushLibrary,
}

/// All manga command specifications registered into the engine command table.
pub fn specs() -> Vec<CommandSpec> {
    let mut v = Vec::new();
    v.extend(mask_transfer::specs());
    v.extend(second_mask::specs());
    v.extend(actions_set::specs());
    v.extend(layer_extras::specs());
    v.extend(brush_preset_items::specs());
    v.extend(comic_screentone::specs());
    v.extend(fill_tool::specs());
    v.extend(vector_brush::specs());
    v.extend(balloon::specs());
    v.extend(vector_curve::specs());
    v.extend(timelapse::specs());
    v.extend(gigapixel::presets::specs());
    v.extend(gigapixel::specs());
    v
}
