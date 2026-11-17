//! Pure Layer-0 seam contracts shared between upstream PhotoCraft crates and the manga UI suitcase.
//!
//! Deliberately avoids all workspace crate dependencies (`photocraft-doc`, `photocraft-ui-egui`,
//! `photocraft-engine`) so that `manga-bridge` stays on `Class::Layer(0)` without cycles.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use serde::{Deserialize, Serialize};

/// Opaque document identifier mirroring `photocraft_doc::DocId.0` without depending on L1 `doc`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BridgeDocId(pub u64);

/// Opaque layer identifier mirroring `photocraft_doc::LayerId.0` without depending on L1 `doc`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BridgeLayerId(pub u64);

/// Lightweight metadata for an open document inside a [`HostSnapshot`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocRef {
    pub id: BridgeDocId,
    /// Current tab index at snapshot capture time (re-resolve via `id` before dispatch).
    pub index: usize,
    pub name: String,
    pub path: Option<String>,
    /// Mirrors `DocState.revision` for cache invalidation.
    pub revision: u64,
    pub is_dirty: bool,
    pub size: (u32, u32),
}

/// Runtime seam toggles (`seam.*` in config / `PHOTOCRAFT_SEAM_MANGA` env override).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SeamFlags {
    pub canvas_capsule: bool,
    pub canvas_gizmos: bool,
    pub toolbar_tools: bool,
    pub firefly_bar: bool,
    pub pages: bool,
    pub timelapse: bool,
    pub typesetter: bool,
    pub gpu_screentone: bool,
    pub second_mask: bool,
}

impl SeamFlags {
    /// Returns a configuration with every manga seam enabled.
    pub const fn all_enabled() -> Self {
        Self {
            canvas_capsule: true,
            canvas_gizmos: true,
            toolbar_tools: true,
            firefly_bar: true,
            pages: true,
            timelapse: true,
            typesetter: true,
            gpu_screentone: true,
            second_mask: true,
        }
    }

    /// Returns a configuration with every manga seam disabled (safe fallback).
    pub const fn all_disabled() -> Self {
        Self {
            canvas_capsule: false,
            canvas_gizmos: false,
            toolbar_tools: false,
            firefly_bar: false,
            pages: false,
            timelapse: false,
            typesetter: false,
            gpu_screentone: false,
            second_mask: false,
        }
    }

    /// Applies an optional global override string such as `PHOTOCRAFT_SEAM_MANGA`.
    pub fn apply_global_override(&mut self, value: &str) {
        match value.trim().to_ascii_lowercase().as_str() {
            "0" | "false" | "off" | "no" => *self = Self::all_disabled(),
            "1" | "true" | "on" | "yes" => *self = Self::all_enabled(),
            _ => {}
        }
    }
}

impl Default for SeamFlags {
    fn default() -> Self {
        Self::all_enabled()
    }
}

/// Read-only frame snapshot captured once from `&PhotocraftApp` before rendering manga UI.
#[derive(Clone, Debug, PartialEq)]
pub struct HostSnapshot {
    pub active_doc: Option<DocRef>,
    pub active_doc_index: Option<usize>,
    pub active_layer: Option<BridgeLayerId>,
    pub selected_layers: Vec<BridgeLayerId>,
    pub documents: Vec<DocRef>,
    /// Stable string identifier mapped from L6 `Tool` by the host adapter.
    pub tool_id: &'static str,
    /// Active UI language code (e.g. `"en"`, `"ru"`, `"ja"`) from upstream `i18n::current().code()`.
    pub locale: String,
    pub foreground: [f32; 4],
    pub background: [f32; 4],
    pub flags: SeamFlags,
}

impl HostSnapshot {
    /// Resolves a stable [`BridgeDocId`] to its current tab index at capture time.
    pub fn doc_index_by_id(&self, id: BridgeDocId) -> Option<usize> {
        self.documents.iter().position(|doc| doc.id == id)
    }

    /// Finds the [`DocRef`] matching a stable [`BridgeDocId`].
    pub fn doc_by_id(&self, id: BridgeDocId) -> Option<&DocRef> {
        self.documents.iter().find(|doc| doc.id == id)
    }
}

/// Command emitted by a panel or canvas gizmo for dispatch via `PhotocraftApp::run`.
#[derive(Clone, Debug, PartialEq)]
pub struct OutCommand {
    pub id: &'static str,
    pub params: serde_json::Value,
}

impl OutCommand {
    /// Builds a new command with static ID and JSON parameters.
    pub fn new(id: &'static str, params: serde_json::Value) -> Self {
        Self { id, params }
    }
}

/// Narrow host bridge abstraction for call sites that must query snapshots and dispatch commands.
pub trait MangaHost {
    fn snapshot(&self) -> HostSnapshot;
    fn dispatch(&mut self, id: &str, params: serde_json::Value) -> Result<serde_json::Value, String>;
}

/// Borrowed context passed to canvas gizmos (`out` buffers commands instead of mutating documents).
pub struct CanvasSeamContext<'a, S> {
    pub snap: &'a HostSnapshot,
    pub state: &'a mut S,
    pub out: &'a mut Vec<OutCommand>,
}

impl<'a, S> CanvasSeamContext<'a, S> {
    /// Appends a command to the deferred dispatch buffer.
    pub fn push(&mut self, id: &'static str, params: serde_json::Value) {
        self.out.push(OutCommand::new(id, params));
    }

    /// Returns the active document's stable ID, if any document is open.
    pub fn active_doc_id(&self) -> Option<BridgeDocId> {
        self.snap.active_doc.as_ref().map(|doc| doc.id)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{BridgeDocId, BridgeLayerId, CanvasSeamContext, DocRef, HostSnapshot, OutCommand, SeamFlags};

    fn sample_doc(id: u64, index: usize, name: &str) -> DocRef {
        DocRef { id: BridgeDocId(id), index, name: name.to_owned(), path: None, revision: 1, is_dirty: false, size: (1200, 1800) }
    }

    fn sample_snapshot() -> HostSnapshot {
        let doc0 = sample_doc(101, 0, "Page 1");
        let doc1 = sample_doc(202, 1, "Page 2");
        HostSnapshot {
            active_doc: Some(doc1.clone()),
            active_doc_index: Some(1),
            active_layer: Some(BridgeLayerId(7)),
            selected_layers: vec![BridgeLayerId(7), BridgeLayerId(8)],
            documents: vec![doc0, doc1],
            tool_id: "move",
            locale: "en".to_owned(),
            foreground: [0.0, 0.0, 0.0, 1.0],
            background: [1.0, 1.0, 1.0, 1.0],
            flags: SeamFlags::all_enabled(),
        }
    }

    #[test]
    fn seam_flags_deserialize_from_empty_object_with_defaults() {
        let parsed: SeamFlags = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, SeamFlags::all_enabled());

        let partial: SeamFlags = serde_json::from_str(r#"{"gpu_screentone": false}"#).unwrap();
        assert!(!partial.gpu_screentone);
        assert!(partial.second_mask);
    }

    #[test]
    fn seam_flags_global_override_parses_bool_strings() {
        let mut flags = SeamFlags::all_enabled();
        flags.apply_global_override("off");
        assert_eq!(flags, SeamFlags::all_disabled());
        flags.apply_global_override("YES");
        assert_eq!(flags, SeamFlags::all_enabled());
        flags.apply_global_override("unknown");
        assert_eq!(flags, SeamFlags::all_enabled());
    }

    #[test]
    fn doc_ref_and_ids_roundtrip_json() {
        let doc = sample_doc(42, 2, "Chapter 3");
        let raw = serde_json::to_string(&doc).unwrap();
        let decoded: DocRef = serde_json::from_str(&raw).unwrap();
        assert_eq!(decoded, doc);
        assert_eq!(decoded.id, BridgeDocId(42));
    }

    #[test]
    fn snapshot_resolves_doc_index_and_context_buffers_commands() {
        let snap = sample_snapshot();
        assert_eq!(snap.doc_index_by_id(BridgeDocId(101)), Some(0));
        assert_eq!(snap.doc_index_by_id(BridgeDocId(202)), Some(1));
        assert_eq!(snap.doc_index_by_id(BridgeDocId(999)), None);
        assert_eq!(snap.doc_by_id(BridgeDocId(202)).map(|d| d.name.as_str()), Some("Page 2"));

        let mut dummy_state = 0_u32;
        let mut out = Vec::new();
        {
            let mut ctx = CanvasSeamContext { snap: &snap, state: &mut dummy_state, out: &mut out };
            *ctx.state += 1;
            assert_eq!(ctx.active_doc_id(), Some(BridgeDocId(202)));
            ctx.push("shape.balloon.update", json!({"coalesce": "drag-1"}));
        }
        assert_eq!(dummy_state, 1);
        assert_eq!(out, vec![OutCommand::new("shape.balloon.update", json!({"coalesce": "drag-1"}))]);
    }
}
