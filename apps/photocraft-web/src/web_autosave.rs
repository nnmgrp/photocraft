//! Isolated IndexedDB crash-recovery adapter for `Services::autosave` on `wasm32`.
//!
//! Architecture & safety guarantees:
//! - **Startup OOM protection**: `load()` enumerates `meta:*` and `blob:*` once, keeping raw
//!   snapshot bytes strictly in JS heap `Uint8Array` handles (`JS_SNAPSHOTS` thread-local) rather
//!   than copying them into 32-bit WASM linear memory. Bytes are copied into a short-lived Rust
//!   `Vec<u8>` ONLY when `Recoverable::load` runs (one document at a time), and dropped immediately
//!   after `photocraft_format::load_from_bytes` finishes.
//! - **Orphan & rejected-entry cleanup**: unfinished generation blobs (`gen != active_gen`),
//!   dangling metadata, corrupt entries, and cancelled/rejected `Recoverable` items are pruned
//!   promptly from IndexedDB.
//! - **Two-phase atomic writes**: Phase 1 durably writes `blob:<key>:<new_gen>` (releasing the
//!   WASM `Vec<u8>` right after copying to `Uint8Array`); Phase 2 atomically updates `meta:<key>`
//!   to point at `new_gen` and deletes `blob:<key>:<old_gen>` in one `Readwrite` transaction.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use js_sys::{Array, ArrayBuffer, Function, Promise, Uint8Array};
use photocraft_ui_egui::Recoverable;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Event, EventTarget, IdbDatabase, IdbRequest, IdbTransaction, IdbTransactionMode};

use crate::autosave_bridge::{
    AutosaveBridge, BLOB_PREFIX, DeletePlan, META_PREFIX, RecoveryMeta, WritePlan, blob_key, find_orphan_blob_keys, meta_key,
};

const DATABASE: &str = "photocraft.recovery";
const STORE: &str = "entries";
const STARTUP_TIMEOUT_MS: f64 = 15_000.0;
const WRITE_TIMEOUT_MS: f64 = 30_000.0;
const MAX_RECORDS: usize = 512;

thread_local! {
    static NEXT_SLOT_ID: Cell<u64> = const { Cell::new(1) };
    /// JS-heap snapshot buffers held outside 32-bit WASM linear memory until `Recoverable::load`.
    static JS_SNAPSHOTS: RefCell<HashMap<u64, Uint8Array>> = RefCell::new(HashMap::new());
}

fn store_js_snapshot(bytes: Uint8Array) -> u64 {
    let id = NEXT_SLOT_ID.with(|next| {
        let cur = next.get();
        next.set(cur.wrapping_add(1).max(1));
        cur
    });
    JS_SNAPSHOTS.with(|map| {
        map.borrow_mut().insert(id, bytes);
    });
    id
}

/// `Send + 'static` handle referencing a single JS-side `Uint8Array` in `JS_SNAPSHOTS`.
///
/// Guarantees:
/// - Always frees the JS `Uint8Array` when consumed or dropped.
/// - If dropped unconsumed after `Services::recover` handed it out (user cancelled/rejected
///   recovery), queues deletion of the entry in IndexedDB via `bridge.discard_key`.
struct JsSnapshotSlot {
    id: u64,
    bridge: AutosaveBridge,
    entry_key: String,
    handed_out: Arc<AtomicBool>,
    consumed: bool,
}

impl JsSnapshotSlot {
    fn take_bytes(&mut self) -> Option<Vec<u8>> {
        self.consumed = true;
        let js_arr = JS_SNAPSHOTS.with(|map| map.borrow_mut().remove(&self.id))?;
        Some(js_arr.to_vec())
    }
}

impl Drop for JsSnapshotSlot {
    fn drop(&mut self) {
        JS_SNAPSHOTS.with(|map| {
            map.borrow_mut().remove(&self.id);
        });
        if !self.consumed && self.handed_out.load(Ordering::Relaxed) {
            self.bridge.discard_key(&self.entry_key);
        }
    }
}

/// Preloaded startup state handed from `load()` to `web::services()` and `WebShell`.
pub struct StartupRecovery {
    pub db: IdbDatabase,
    pub bridge: AutosaveBridge,
    pub entries: Vec<Recoverable>,
    pub handed_out: Arc<AtomicBool>,
}

fn js_err(e: JsValue) -> String {
    if let Some(exception) = e.dyn_ref::<web_sys::DomException>() {
        format!("{}: {}", exception.name(), exception.message())
    } else {
        e.as_string().unwrap_or_else(|| format!("{e:?}"))
    }
}

type Listener = (EventTarget, &'static str, Closure<dyn FnMut(Event)>);

struct Listeners(Vec<Listener>);

impl Listeners {
    fn new() -> Self {
        Self(Vec::new())
    }

    fn add(&mut self, target: &EventTarget, name: &'static str, f: impl FnMut(Event) + 'static) -> Result<(), JsValue> {
        let cb = Closure::<dyn FnMut(Event)>::new(f);
        target.add_event_listener_with_callback(name, cb.as_ref().unchecked_ref())?;
        self.0.push((target.clone(), name, cb));
        Ok(())
    }
}

impl Drop for Listeners {
    fn drop(&mut self) {
        for (target, name, cb) in &self.0 {
            let _ = target.remove_event_listener_with_callback(name, cb.as_ref().unchecked_ref());
        }
    }
}

struct Pending {
    promise: Promise,
    _listeners: Listeners,
}

fn request(request: &IdbRequest) -> Pending {
    let mut listeners = Listeners::new();
    let promise = Promise::new(&mut |resolve, reject| {
        let req = request.clone();
        let failed = reject.clone();
        if let Err(e) = listeners.add(request, "success", move |_| match req.result() {
            Ok(v) => {
                let _ = resolve.call1(&JsValue::NULL, &v);
            }
            Err(e) => {
                let _ = failed.call1(&JsValue::NULL, &e);
            }
        }) {
            let _ = reject.call1(&JsValue::NULL, &e);
        }
        let req = request.clone();
        let failed = reject.clone();
        if let Err(e) = listeners.add(request, "error", move |_| {
            let e = req.error().ok().flatten().map(JsValue::from).unwrap_or_else(|| JsValue::from_str("IndexedDB recovery request failed"));
            let _ = failed.call1(&JsValue::NULL, &e);
        }) {
            let _ = reject.call1(&JsValue::NULL, &e);
        }
    });
    Pending { promise, _listeners: listeners }
}

async fn before(promise: &Promise, deadline: f64, max_ms: f64) -> Result<JsValue, String> {
    let window = web_sys::window().ok_or("no window")?;
    let mut timer = None;
    let mut callback = None;
    let timeout = Promise::new(&mut |_, reject| {
        let cb = Closure::<dyn FnMut()>::new(move || {
            let _ = reject.call1(&JsValue::NULL, &JsValue::from_str("browser recovery storage timed out"));
        });
        timer = Some(window.set_timeout_with_callback_and_timeout_and_arguments_0(
            cb.as_ref().unchecked_ref(),
            (deadline - js_sys::Date::now()).clamp(0.0, max_ms) as i32,
        ));
        callback = Some(cb);
    });
    let id = timer.ok_or("could not start recovery storage timeout")?.map_err(js_err)?;
    let result = JsFuture::from(Promise::race(&Array::of2(promise, &timeout))).await.map_err(js_err);
    window.clear_timeout_with_handle(id);
    drop(callback);
    result
}

async fn await_transaction(tx: &IdbTransaction, timeout_ms: f64) -> Result<(), String> {
    let mut listeners = Listeners::new();
    let promise = Promise::new(&mut |resolve, reject| {
        if let Err(e) = listeners.add(tx, "complete", move |_| {
            let _ = resolve.call0(&JsValue::NULL);
        }) {
            let _ = reject.call1(&JsValue::NULL, &e);
        }
        for event in ["abort", "error"] {
            let tx_clone = tx.clone();
            let failed: Function = reject.clone();
            let target: EventTarget = tx.clone().into();
            if let Err(e) = listeners.add(&target, event, move |_| {
                let e = tx_clone.error().map(JsValue::from).unwrap_or_else(|| JsValue::from_str("recovery transaction aborted"));
                let _ = failed.call1(&JsValue::NULL, &e);
            }) {
                let _ = reject.call1(&JsValue::NULL, &e);
            }
        }
    });
    let result = before(&promise, js_sys::Date::now() + timeout_ms, timeout_ms).await.map(|_| ());
    if result.is_err() {
        let _ = tx.abort();
    }
    result
}

async fn open(deadline: f64) -> Result<IdbDatabase, String> {
    let factory = web_sys::window().ok_or("no window")?.indexed_db().map_err(js_err)?.ok_or("IndexedDB unavailable")?;
    let req = factory.open_with_u32(DATABASE, 1).map_err(js_err)?;
    let mut upgrade = Listeners::new();
    let upgrade_req = req.clone();
    upgrade
        .add(&req, "upgradeneeded", move |_| {
            let result = upgrade_req.result().and_then(|v| v.dyn_into::<IdbDatabase>()).and_then(|db| db.create_object_store(STORE));
            if result.is_err()
                && let Some(tx) = upgrade_req.transaction()
            {
                let _ = tx.abort();
            }
        })
        .map_err(js_err)?;
    let pending = request(&req);
    match before(&pending.promise, deadline, STARTUP_TIMEOUT_MS).await {
        Ok(value) => value.dyn_into().map_err(|_| "invalid recovery database".into()),
        Err(e) => {
            wasm_bindgen_futures::spawn_local(async move {
                if let Ok(value) = JsFuture::from(pending.promise.clone()).await
                    && let Ok(db) = value.dyn_into::<IdbDatabase>()
                {
                    db.close();
                }
                drop(pending);
                drop(upgrade);
            });
            Err(e)
        }
    }
}

/// Opens the IndexedDB recovery store, reaps any crashed/orphan records, and indexes valid
/// snapshots as JS-side `Uint8Array` handles (zero bytes copied into WASM heap at startup).
pub async fn load() -> Result<StartupRecovery, String> {
    let deadline = js_sys::Date::now() + STARTUP_TIMEOUT_MS;
    let db = open(deadline).await?;
    match read_and_reap(&db, deadline).await {
        Ok((bridge, entries, handed_out)) => Ok(StartupRecovery { db, bridge, entries, handed_out }),
        Err(e) => {
            db.close();
            Err(e)
        }
    }
}

async fn read_and_reap(db: &IdbDatabase, deadline: f64) -> Result<(AutosaveBridge, Vec<Recoverable>, Arc<AtomicBool>), String> {
    let tx = db.transaction_with_str_and_mode(STORE, IdbTransactionMode::Readonly).map_err(js_err)?;
    let req = tx.object_store(STORE).map_err(js_err)?.open_cursor().map_err(js_err)?;

    let mut metas: HashMap<String, RecoveryMeta> = HashMap::new();
    let mut blobs: HashMap<String, Uint8Array> = HashMap::new();
    let mut raw_blob_keys: Vec<String> = Vec::new();
    let mut stale_keys: Vec<String> = Vec::new();
    let mut seen = 0usize;

    let mut next = request(&req);
    loop {
        let value = before(&next.promise, deadline, STARTUP_TIMEOUT_MS).await?;
        if value.is_null() {
            break;
        }
        let cursor = value.dyn_into::<web_sys::IdbCursorWithValue>().map_err(|_| "invalid recovery cursor")?;
        seen += 1;
        if seen > MAX_RECORDS {
            return Err(format!("browser recovery record limit ({MAX_RECORDS}) exceeded"));
        }
        let raw_key = cursor.key().map_err(js_err)?.as_string().unwrap_or_default();
        let val = cursor.value().map_err(js_err)?;

        if raw_key.starts_with(META_PREFIX) {
            let parsed = val.as_string().and_then(|s| serde_json::from_str::<RecoveryMeta>(&s).ok());
            match parsed {
                Some(meta) if meta_key(&meta.key) == raw_key => {
                    metas.insert(meta.key.clone(), meta);
                }
                _ => stale_keys.push(raw_key),
            }
        } else if raw_key.starts_with(BLOB_PREFIX) {
            let bytes = if let Ok(arr) = val.clone().dyn_into::<Uint8Array>() {
                Some(arr)
            } else if let Ok(buf) = val.dyn_into::<ArrayBuffer>() {
                Some(Uint8Array::new(&buf))
            } else {
                None
            };
            if let Some(arr) = bytes {
                raw_blob_keys.push(raw_key.clone());
                blobs.insert(raw_key, arr);
            } else {
                stale_keys.push(raw_key);
            }
        } else if !raw_key.is_empty() {
            stale_keys.push(raw_key);
        }

        next = request(&req);
        cursor.continue_().map_err(js_err)?;
    }

    // Keep only metadata entries whose active generation blob actually exists.
    let mut valid_metas: HashMap<String, RecoveryMeta> = HashMap::new();
    for (entry_key, meta) in metas {
        let active_blob = blob_key(&meta.key, meta.active_gen);
        if blobs.contains_key(&active_blob) {
            valid_metas.insert(entry_key, meta);
        } else {
            stale_keys.push(meta_key(&meta.key));
        }
    }

    // Reap orphan blobs from crashed Phase-1 writes or stale previous generations.
    stale_keys.extend(find_orphan_blob_keys(&valid_metas, &raw_blob_keys));
    if !stale_keys.is_empty() {
        if let Err(e) = delete_raw_keys(db, &stale_keys).await {
            log::warn!("web_autosave: failed to reap {} stale recovery keys: {e}", stale_keys.len());
        } else {
            log::info!("web_autosave: reaped {} stale/orphan recovery keys", stale_keys.len());
        }
    }

    let salt = (js_sys::Date::now() as u64).max(1);
    let bridge = AutosaveBridge::with_salt(salt);
    let handed_out = Arc::new(AtomicBool::new(false));

    let mut ordered: Vec<RecoveryMeta> = valid_metas.into_values().collect();
    ordered.sort_by(|a, b| a.key.cmp(&b.key));

    let mut entries = Vec::with_capacity(ordered.len());
    for meta in ordered {
        let active_blob = blob_key(&meta.key, meta.active_gen);
        let Some(js_arr) = blobs.remove(&active_blob) else { continue };
        bridge.register_recovered_meta(&meta);
        let slot_id = store_js_snapshot(js_arr);
        let mut slot = JsSnapshotSlot {
            id: slot_id,
            bridge: bridge.clone(),
            entry_key: meta.key.clone(),
            handed_out: handed_out.clone(),
            consumed: false,
        };
        entries.push(Recoverable {
            key: meta.key,
            name: meta.name,
            path: meta.path,
            load: Box::new(move || {
                let Some(bytes) = slot.take_bytes() else {
                    slot.bridge.discard_key(&slot.entry_key);
                    return Err("recovery snapshot is no longer available".into());
                };
                let result = photocraft_format::load_from_bytes(&bytes).map_err(|e| e.to_string());
                drop(bytes);
                if result.is_err() {
                    slot.bridge.discard_key(&slot.entry_key);
                }
                result
            }),
        });
    }

    Ok((bridge, entries, handed_out))
}

async fn delete_raw_keys(db: &IdbDatabase, keys: &[String]) -> Result<(), String> {
    if keys.is_empty() {
        return Ok(());
    }
    let tx = db.transaction_with_str_and_mode(STORE, IdbTransactionMode::Readwrite).map_err(js_err)?;
    let store = tx.object_store(STORE).map_err(js_err)?;
    for key in keys {
        if let Err(e) = store.delete(&JsValue::from_str(key)).map_err(js_err) {
            let _ = tx.abort();
            return Err(e);
        }
    }
    await_transaction(&tx, STARTUP_TIMEOUT_MS).await
}

/// Two-phase atomic snapshot write:
/// 1. Serialize `.pcraft` -> copy to JS `Uint8Array` -> immediately drop WASM `Vec<u8>` ->
///    durably commit `blob:<key>:<new_gen>`.
/// 2. Atomically switch `meta:<key>.active_gen = new_gen` and delete `blob:<key>:<old_gen>`
///    in a single IndexedDB `Readwrite` transaction.
pub async fn write_atomic(db: &IdbDatabase, plan: &WritePlan) -> Result<(), String> {
    let bytes = photocraft_format::save_to_bytes(&plan.doc, &photocraft_format::SaveOptions::default()).map_err(|e| e.to_string())?;
    let js_bytes = Uint8Array::from(bytes.as_slice());
    // Free 32-bit WASM heap immediately before any async IndexedDB await point.
    drop(bytes);

    // Phase 1: durably write the new generation blob while `meta:<key>` still points to `old_gen`.
    let new_blob_key = blob_key(&plan.entry_key, plan.new_gen);
    {
        let tx = db.transaction_with_str_and_mode(STORE, IdbTransactionMode::Readwrite).map_err(js_err)?;
        let store = tx.object_store(STORE).map_err(js_err)?;
        if let Err(e) = store.put_with_key(&js_bytes, &JsValue::from_str(&new_blob_key)).map_err(js_err) {
            let _ = tx.abort();
            return Err(e);
        }
        await_transaction(&tx, WRITE_TIMEOUT_MS).await?;
    }
    drop(js_bytes);

    // Phase 2: atomically point `meta:<key>` to `new_gen` and prune `old_gen`.
    let meta = RecoveryMeta {
        key: plan.entry_key.clone(),
        name: plan.name.clone(),
        path: plan.path.clone(),
        active_gen: plan.new_gen,
        revision: plan.revision,
    };
    let meta_json = serde_json::to_string(&meta).map_err(|e| format!("failed to serialize recovery metadata: {e}"))?;
    let tx = db.transaction_with_str_and_mode(STORE, IdbTransactionMode::Readwrite).map_err(js_err)?;
    let store = tx.object_store(STORE).map_err(js_err)?;
    let queued = (|| -> Result<(), String> {
        store.put_with_key(&JsValue::from_str(&meta_json), &JsValue::from_str(&meta_key(&plan.entry_key))).map_err(js_err)?;
        if let Some(old) = plan.old_gen
            && old != plan.new_gen
        {
            store.delete(&JsValue::from_str(&blob_key(&plan.entry_key, old))).map_err(js_err)?;
        }
        Ok(())
    })();
    if let Err(e) = queued {
        let _ = tx.abort();
        return Err(e);
    }
    await_transaction(&tx, STARTUP_TIMEOUT_MS).await
}

/// Deletes a recovery entry's metadata and all tracked generation blobs in one transaction.
pub async fn delete_entry(db: &IdbDatabase, plan: &DeletePlan) -> Result<(), String> {
    let tx = db.transaction_with_str_and_mode(STORE, IdbTransactionMode::Readwrite).map_err(js_err)?;
    let store = tx.object_store(STORE).map_err(js_err)?;
    let queued = (|| -> Result<(), String> {
        store.delete(&JsValue::from_str(&plan.meta_key)).map_err(js_err)?;
        for bk in &plan.blob_keys {
            store.delete(&JsValue::from_str(bk)).map_err(js_err)?;
        }
        Ok(())
    })();
    if let Err(e) = queued {
        let _ = tx.abort();
        return Err(e);
    }
    await_transaction(&tx, STARTUP_TIMEOUT_MS).await
}
