//! Pure-Rust state machine for isolated browser crash-recovery autosave (`Services::autosave`).
//!
//! Guarantees:
//! - **Single-flight coalescing**: at most ONE document is serialized / written to IndexedDB at
//!   a time. While a write is in flight, newer autosave ticks for the same document overwrite its
//!   lightweight `Arc<Document>` slot instead of holding multiple serialized copies in WASM RAM.
//! - **Two-phase atomic generations**: each snapshot writes `blob:<key>:<new_gen>` first, then
//!   atomically switches `meta:<key>.active_gen = new_gen` and deletes `blob:<key>:<old_gen>`.
//!   If the tab crashes mid-write, the previous valid snapshot stays intact and any unfinished
//!   `blob:<key>:<new_gen>` is reaped as an orphan on next launch.
//! - **Clean `adopt` / `discard` lifecycle**: recovered documents inherit their existing `key`
//!   and `active_gen`; saving or closing a document (even mid-flight) queues full deletion of
//!   both metadata and blob keys.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use photocraft_doc::Document;
use serde::{Deserialize, Serialize};

pub const META_PREFIX: &str = "meta:";
pub const BLOB_PREFIX: &str = "blob:";

/// Metadata stored under `meta:<key>` pointing atomically to `blob:<key>:<active_gen>`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryMeta {
    pub key: String,
    pub name: String,
    pub path: Option<String>,
    pub active_gen: u64,
    pub revision: u64,
}

pub fn meta_key(entry_key: &str) -> String {
    format!("{META_PREFIX}{entry_key}")
}

pub fn blob_key(entry_key: &str, generation: u64) -> String {
    format!("{BLOB_PREFIX}{entry_key}:{generation}")
}

/// Parses `blob:<entry_key>:<generation>` into `(entry_key, generation)`.
pub fn parse_blob_key(raw: &str) -> Option<(&str, u64)> {
    let rest = raw.strip_prefix(BLOB_PREFIX)?;
    let (entry_key, gen_str) = rest.rsplit_once(':')?;
    if entry_key.is_empty() {
        return None;
    }
    let generation = gen_str.parse::<u64>().ok()?;
    Some((entry_key, generation))
}

/// Returns all raw `blob:*` keys that do not match any valid `meta:<key>.active_gen`.
pub fn find_orphan_blob_keys(metas: &HashMap<String, RecoveryMeta>, raw_blob_keys: &[String]) -> Vec<String> {
    let mut orphans = Vec::new();
    for raw in raw_blob_keys {
        let keep = parse_blob_key(raw)
            .and_then(|(k, g)| metas.get(k).map(|m| m.active_gen == g))
            .unwrap_or(false);
        if !keep {
            orphans.push(raw.clone());
        }
    }
    orphans
}

/// One serialized snapshot write scheduled for the async IndexedDB runner.
pub struct WritePlan {
    pub doc_id: u64,
    pub entry_key: String,
    pub doc: Arc<Document>,
    pub revision: u64,
    pub name: String,
    pub path: Option<String>,
    pub new_gen: u64,
    pub old_gen: Option<u64>,
}

/// Full cleanup of one recovery entry (its metadata key and any blob generations).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeletePlan {
    pub entry_key: String,
    pub meta_key: String,
    pub blob_keys: Vec<String>,
}

/// Next unit of async IndexedDB work; at most one is in flight at any moment.
pub enum WorkItem {
    Write(WritePlan),
    Delete(DeletePlan),
}

struct PendingDoc {
    doc: Arc<Document>,
    revision: u64,
    path: Option<String>,
}

struct InFlightWrite {
    doc_id: u64,
    revision: u64,
    entry_key: String,
    new_gen: u64,
    old_gen: Option<u64>,
    discard_after: bool,
}

struct Inner {
    session_salt: u64,
    unavailable: Option<String>,
    /// Live `doc_id -> entry_key` mapping (assigned on first autosave or via `adopt`).
    doc_keys: HashMap<u64, String>,
    /// Last committed `active_gen` per `entry_key` (populated on startup and after writes).
    active_gens: HashMap<String, u64>,
    /// Latest `revision` already durably persisted in IndexedDB for `doc_id`.
    persisted_revisions: HashMap<u64, u64>,
    /// Coalesced latest unsaved snapshot per `doc_id` (only `Arc<Document>`, zero serialized copies).
    queued: HashMap<u64, PendingDoc>,
    queue_order: VecDeque<u64>,
    pending_deletes: VecDeque<DeletePlan>,
    in_flight: bool,
    in_flight_write: Option<InFlightWrite>,
    completed: Vec<(u64, u64, Result<(), String>)>,
}

impl Inner {
    fn new(session_salt: u64) -> Self {
        Self {
            session_salt,
            unavailable: None,
            doc_keys: HashMap::new(),
            active_gens: HashMap::new(),
            persisted_revisions: HashMap::new(),
            queued: HashMap::new(),
            queue_order: VecDeque::new(),
            pending_deletes: VecDeque::new(),
            in_flight: false,
            in_flight_write: None,
            completed: Vec::new(),
        }
    }
}

/// Thread-safe handle shared between `Services` closures and `WebShell`'s async IndexedDB pump.
#[derive(Clone)]
pub struct AutosaveBridge(Arc<Mutex<Inner>>);

impl Default for AutosaveBridge {
    fn default() -> Self {
        Self::with_salt(1)
    }
}

impl AutosaveBridge {
    pub fn with_salt(session_salt: u64) -> Self {
        Self(Arc::new(Mutex::new(Inner::new(session_salt))))
    }

    pub fn mark_unavailable(&self, reason: impl Into<String>) {
        let mut g = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        g.unavailable = Some(reason.into());
    }

    /// Registers metadata discovered at startup so `adopt` knows its current `active_gen`.
    pub fn register_recovered_meta(&self, meta: &RecoveryMeta) {
        let mut g = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        g.active_gens.insert(meta.key.clone(), meta.active_gen);
    }

    /// Binds a recovered document's runtime `doc_id` to its existing IndexedDB `key`.
    pub fn adopt(&self, doc_id: u64, key: &str) {
        let mut g = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        g.doc_keys.insert(doc_id, key.to_owned());
    }

    /// Enqueues or coalesces an autosave snapshot for `doc` without copying or serializing it yet.
    pub fn enqueue(&self, doc: Arc<Document>, revision: u64, path: Option<String>) -> Result<(), String> {
        let mut g = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(reason) = g.unavailable.as_ref() {
            return Err(reason.clone());
        }
        let doc_id = doc.id.0;
        let salt = g.session_salt;
        g.doc_keys.entry(doc_id).or_insert_with(|| format!("doc-{salt}-{doc_id}"));
        if let Some(in_flight) = g.in_flight_write.as_mut()
            && in_flight.doc_id == doc_id
        {
            in_flight.discard_after = false;
            if in_flight.revision == revision {
                return Ok(());
            }
        }
        if g.persisted_revisions.get(&doc_id) == Some(&revision) && !g.queued.contains_key(&doc_id) {
            g.completed.push((doc_id, revision, Ok(())));
            return Ok(());
        }
        let is_new = !g.queued.contains_key(&doc_id);
        g.queued.insert(doc_id, PendingDoc { doc, revision, path });
        if is_new {
            g.queue_order.push_back(doc_id);
        }
        Ok(())
    }

    /// Drops queued work for `doc_id` and schedules full IndexedDB cleanup for its recovery entry.
    pub fn discard(&self, doc_id: u64) {
        let mut g = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        g.queued.remove(&doc_id);
        g.queue_order.retain(|id| *id != doc_id);
        g.persisted_revisions.remove(&doc_id);
        if let Some(in_flight) = g.in_flight_write.as_mut()
            && in_flight.doc_id == doc_id
        {
            in_flight.discard_after = true;
            g.doc_keys.remove(&doc_id);
            return;
        }
        if let Some(entry_key) = g.doc_keys.remove(&doc_id) {
            let mut blob_keys = Vec::new();
            if let Some(generation) = g.active_gens.remove(&entry_key) {
                blob_keys.push(blob_key(&entry_key, generation));
            }
            g.pending_deletes.push_back(DeletePlan {
                meta_key: meta_key(&entry_key),
                entry_key,
                blob_keys,
            });
        }
    }

    /// Queues an unconditional delete for a raw recovery entry key (e.g. a rejected recovery entry).
    pub fn discard_key(&self, entry_key: &str) {
        let mut g = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let mut blob_keys = Vec::new();
        if let Some(generation) = g.active_gens.remove(entry_key) {
            blob_keys.push(blob_key(entry_key, generation));
        }
        g.pending_deletes.push_back(DeletePlan {
            meta_key: meta_key(entry_key),
            entry_key: entry_key.to_owned(),
            blob_keys,
        });
    }

    /// Claims the next single-flight unit of IndexedDB work, or `None` if busy or idle.
    pub fn next_work(&self) -> Option<WorkItem> {
        let mut g = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if g.in_flight {
            return None;
        }
        if let Some(del) = g.pending_deletes.pop_front() {
            g.in_flight = true;
            return Some(WorkItem::Delete(del));
        }
        while let Some(doc_id) = g.queue_order.pop_front() {
            let Some(pending) = g.queued.remove(&doc_id) else { continue };
            let Some(entry_key) = g.doc_keys.get(&doc_id).cloned() else { continue };
            let old_gen = g.active_gens.get(&entry_key).copied();
            let new_gen = old_gen.unwrap_or(0).saturating_add(1);
            g.in_flight = true;
            g.in_flight_write = Some(InFlightWrite {
                doc_id,
                revision: pending.revision,
                entry_key: entry_key.clone(),
                new_gen,
                old_gen,
                discard_after: false,
            });
            let name = pending.doc.name.clone();
            return Some(WorkItem::Write(WritePlan {
                doc_id,
                entry_key,
                doc: pending.doc,
                revision: pending.revision,
                name,
                path: pending.path,
                new_gen,
                old_gen,
            }));
        }
        None
    }

    /// Completes the active `WorkItem::Write`, updating `active_gen` and emitting completion status.
    pub fn finish_write(&self, doc_id: u64, revision: u64, result: Result<(), String>) {
        let mut g = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        g.in_flight = false;
        let flight = g.in_flight_write.take();
        if let Some(fl) = flight {
            if result.is_ok() {
                g.active_gens.insert(fl.entry_key.clone(), fl.new_gen);
                g.persisted_revisions.insert(doc_id, revision);
            }
            if fl.discard_after {
                let mut blob_keys = vec![blob_key(&fl.entry_key, fl.new_gen)];
                if let Some(old) = fl.old_gen
                    && old != fl.new_gen
                {
                    blob_keys.push(blob_key(&fl.entry_key, old));
                }
                g.active_gens.remove(&fl.entry_key);
                g.persisted_revisions.remove(&doc_id);
                g.pending_deletes.push_back(DeletePlan {
                    meta_key: meta_key(&fl.entry_key),
                    entry_key: fl.entry_key,
                    blob_keys,
                });
                return;
            }
        }
        g.completed.push((doc_id, revision, result));
    }

    /// Completes the active `WorkItem::Delete`.
    pub fn finish_delete(&self) {
        let mut g = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        g.in_flight = false;
    }

    /// Drains completed write outcomes for `Services::autosave_results` / `flush_autosaves`.
    pub fn take_completed(&self) -> Vec<(u64, u64, Result<(), String>)> {
        let mut g = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        std::mem::take(&mut g.completed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::{ColorMode, DocId, Layer, LayerContent, SampleType, Size};

    fn test_doc(id: u64, name: &str) -> Arc<Document> {
        let mut doc = Document::new(name, Size { width: 256, height: 256 }, ColorMode::Rgb, SampleType::U8);
        doc.id = DocId(id);
        Arc::new(doc)
    }

    #[test]
    fn single_flight_coalescing_never_runs_parallel_writes() {
        let bridge = AutosaveBridge::with_salt(99);
        let d1 = test_doc(7, "Page01");
        bridge.enqueue(d1.clone(), 1, Some("page01.pcraft".into())).unwrap();

        let Some(WorkItem::Write(w1)) = bridge.next_work() else {
            panic!("expected first write plan");
        };
        assert_eq!((w1.doc_id, w1.revision, w1.new_gen, w1.old_gen), (7, 1, 1, None));
        assert_eq!(w1.name, "Page01");
        assert_eq!(w1.path.as_deref(), Some("page01.pcraft"));
        assert!(Arc::ptr_eq(&w1.doc, &d1));

        // While write #1 is in flight, enqueue rev 2 and rev 3: they coalesce into one pending slot
        // and `next_work` MUST return `None` (no parallel writes / duplicate RAM buffers).
        bridge.enqueue(d1.clone(), 2, Some("page01.pcraft".into())).unwrap();
        bridge.enqueue(d1, 3, Some("page01.pcraft".into())).unwrap();
        assert!(bridge.next_work().is_none(), "must enforce single-flight while a write is active");

        bridge.finish_write(7, 1, Ok(()));
        assert_eq!(bridge.take_completed(), vec![(7, 1, Ok(()))]);

        // Next write picks up only the latest coalesced rev 3 and bumps generation 1 -> 2.
        let Some(WorkItem::Write(w2)) = bridge.next_work() else {
            panic!("expected coalesced write plan");
        };
        assert_eq!((w2.doc_id, w2.revision, w2.new_gen, w2.old_gen), (7, 3, 2, Some(1)));
        bridge.finish_write(7, 3, Ok(()));
        assert!(bridge.next_work().is_none());
    }

    #[test]
    fn adopt_and_atomic_generation_bump_and_discard() {
        let bridge = AutosaveBridge::with_salt(5);
        let recovered = RecoveryMeta {
            key: "doc-1-42".into(),
            name: "Ch04_p12".into(),
            path: None,
            active_gen: 3,
            revision: 11,
        };
        bridge.register_recovered_meta(&recovered);
        bridge.adopt(100, "doc-1-42");

        let d = test_doc(100, "Ch04_p12");
        bridge.enqueue(d, 12, None).unwrap();
        let Some(WorkItem::Write(w)) = bridge.next_work() else {
            panic!("expected write for adopted doc");
        };
        assert_eq!(w.entry_key, "doc-1-42");
        assert_eq!((w.new_gen, w.old_gen), (4, Some(3)));
        bridge.finish_write(100, 12, Ok(()));

        bridge.discard(100);
        let Some(WorkItem::Delete(del)) = bridge.next_work() else {
            panic!("expected delete plan after discard");
        };
        assert_eq!(del.meta_key, "meta:doc-1-42");
        assert_eq!(del.blob_keys, vec!["blob:doc-1-42:4".to_owned()]);
        bridge.finish_delete();
        assert!(bridge.next_work().is_none());
    }

    #[test]
    fn discard_while_in_flight_queues_cleanup_after_completion() {
        let bridge = AutosaveBridge::with_salt(12);
        let d = test_doc(3, "Temp");
        bridge.enqueue(d, 1, None).unwrap();
        let Some(WorkItem::Write(w)) = bridge.next_work() else {
            panic!("expected write");
        };
        assert_eq!(w.new_gen, 1);

        // User closes/saves document while IndexedDB write is still running.
        bridge.discard(3);
        bridge.finish_write(3, 1, Ok(()));
        // Discarded document does not emit a stale completion status, and immediately schedules cleanup.
        assert!(bridge.take_completed().is_empty());
        let Some(WorkItem::Delete(del)) = bridge.next_work() else {
            panic!("expected deferred cleanup delete");
        };
        assert_eq!(del.meta_key, meta_key(&w.entry_key));
        assert!(del.blob_keys.contains(&blob_key(&w.entry_key, 1)));
    }

    #[test]
    fn find_orphan_blob_keys_identifies_stale_and_unreferenced_blobs() {
        let mut metas = HashMap::new();
        metas.insert(
            "doc-1-1".into(),
            RecoveryMeta { key: "doc-1-1".into(), name: "A".into(), path: None, active_gen: 5, revision: 9 },
        );
        let blobs = vec![
            "blob:doc-1-1:4".into(), // stale previous gen
            "blob:doc-1-1:5".into(), // active gen (MUST KEEP)
            "blob:doc-1-1:6".into(), // crashed phase-1 write (ORPHAN)
            "blob:doc-9-9:1".into(), // no metadata entry (ORPHAN)
        ];
        let orphans = find_orphan_blob_keys(&metas, &blobs);
        assert_eq!(orphans, vec!["blob:doc-1-1:4", "blob:doc-1-1:6", "blob:doc-9-9:1"]);
    }

    #[test]
    fn pcraft_roundtrip_preserves_sfx_document_for_autosave() {
        let mut doc = Document::new("MangaSFX", Size { width: 1024, height: 1024 }, ColorMode::Rgb, SampleType::U8);
        let mut layer = Layer::new("SFX: BAM!", LayerContent::Raster(photocraft_raster::Surface::new(doc.pixel_format())));
        layer.sfx = Some(Arc::new(photocraft_doc::sfx::SfxData { text: "BAM!".into(), ..Default::default() }));
        let lid = doc.insert_above(None, layer);

        let bytes = photocraft_format::save_to_bytes(&doc, &photocraft_format::SaveOptions::default()).unwrap();
        let restored = photocraft_format::load_from_bytes(&bytes).unwrap();
        assert_eq!(restored.name, "MangaSFX");
        assert_eq!(restored.size, doc.size);
        let sfx = restored.layer(lid).and_then(|l| l.sfx.as_deref()).expect("SFX metadata must survive autosave roundtrip");
        assert_eq!(sfx.text, "BAM!");
    }

    #[test]
    fn duplicate_revision_retry_dedups_and_confirms_persisted() {
        let bridge = AutosaveBridge::with_salt(7);
        let d = test_doc(11, "Chapter01");
        bridge.enqueue(d.clone(), 4, None).unwrap();
        let Some(WorkItem::Write(_)) = bridge.next_work() else {
            panic!("expected write");
        };
        // Retry of the exact same revision while in flight must NOT enqueue a duplicate write.
        bridge.enqueue(d.clone(), 4, None).unwrap();
        bridge.finish_write(11, 4, Ok(()));
        assert_eq!(bridge.take_completed(), vec![(11, 4, Ok(()))]);
        assert!(bridge.next_work().is_none());

        // Subsequent flush retry for the already-persisted revision confirms immediately with zero I/O.
        bridge.enqueue(d, 4, None).unwrap();
        assert!(bridge.next_work().is_none());
        assert_eq!(bridge.take_completed(), vec![(11, 4, Ok(()))]);
    }

    #[test]
    fn unavailable_storage_and_rejected_entry_discard_key() {
        let bridge = AutosaveBridge::with_salt(3);
        bridge.register_recovered_meta(&RecoveryMeta {
            key: "doc-old-9".into(),
            name: "Rejected".into(),
            path: None,
            active_gen: 2,
            revision: 5,
        });
        bridge.discard_key("doc-old-9");
        let Some(WorkItem::Delete(del)) = bridge.next_work() else {
            panic!("expected delete plan for rejected entry");
        };
        assert_eq!(del.entry_key, "doc-old-9");
        assert_eq!(del.meta_key, "meta:doc-old-9");
        assert_eq!(del.blob_keys, vec!["blob:doc-old-9:2".to_owned()]);
        bridge.finish_delete();

        bridge.mark_unavailable("IndexedDB denied");
        let err = bridge.enqueue(test_doc(2, "X"), 1, None).unwrap_err();
        assert_eq!(err, "IndexedDB denied");
    }
}
