//! Session store (R18) — ports Swift `SessionStore` (`SessionStore.swift`).
//!
//! Persists the per-window tab tree to
//! `<app-support>/Nice RS Dev/sessions.json` so a relaunch restores
//! windows/tabs/panes. Claude tabs resume via `claude --resume <uuid>`;
//! terminal-only tabs restore with a fresh shell in their saved cwd.
//!
//! ## Schema
//!
//! The window envelope (`PersistedState`/`PersistedWindow`/`PersistedFrame`)
//! lives here; the model-shaped leaves (`PersistedPane`/`PersistedTab`/
//! `PersistedProject`) live gpui-free in `nice-model`. The schema is Swift's v3
//! **minus `branch`** (M5), tolerant by SHAPE: no version gate on read, unknown
//! fields ignored (NO `deny_unknown_fields`), nil-omitted optionals. A
//! missing/corrupt/shape-mismatched file decodes to `{version:3, windows:[]}` —
//! never an error, so the app always launches. Writes are `version: 3`.
//!
//! ## Store machinery (the observable contract — dossier §3.2)
//!
//! * (a) mutations never block — `upsert`/`remove`/`prune_empty_windows` update
//!   the in-memory cache and schedule a debounced write;
//! * (b) 500 ms debounce coalescing (each mutation pushes the deadline out);
//! * (c) `flush()` is synchronous — cancels the pending timer, forces a write,
//!   and blocks until it lands;
//! * (d) the writer never runs concurrently with itself — ONE dedicated OS
//!   thread owns every write (the `control_socket` thread precedent; gpui
//!   timers are App-Nap-deferred, so a scheduler-level thread event, not a
//!   parked timer, drives the cadence);
//! * (e) atomic write (temp + rename via the shared [`crate::atomic_file`]
//!   helper) — a failed write leaves the prior file intact;
//! * (f) `remove(id)` / `prune_empty_windows(keeping)` filter the cache.
//!
//! ## Global + absent ⇒ no-op
//!
//! The store lives in a process [`GLOBAL`] installed by `app::run` only. Every
//! persistence hook goes through the [`upsert`]/[`remove`]/[`prune`]/[`flush`]/
//! [`load`] free functions, which are a **no-op when the Global is absent** —
//! scenarios/tests opt in via an injected temp store path
//! ([`install_global`]). Nothing here reads or writes the developer's real
//! `~/Library/Application Support`; the base dir is injectable
//! (`NICE_APPLICATION_SUPPORT_ROOT`, resolved only in `app::run`).
//!
//! ## Migration
//!
//! One-time: iff the OWN file is ABSENT, [`open`] reads the Swift app's
//! `…/Nice/sessions.json` (source path injectable) with the same tolerance,
//! drops `branch` (the leaf structs have no such field), and writes to the OWN
//! store only — writing the Swift path would fight a still-installed Swift
//! Nice's flushes.
//!
//! The launch-bootstrap + restore-fan-out consumers land with a later slice,
//! hence the module-wide `dead_code` allow (the established
//! later-slice-consumer pattern).

#![allow(dead_code)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use nice_model::PersistedProject;

/// The schema version R18 writes. v1/v2 files fail to decode and start fresh
/// (the same one-off migration Swift accepted at the v2→v3 bump).
pub const CURRENT_VERSION: i64 = 3;

/// Debounce window for `upsert`/`remove`/`prune`. Short enough that a quick ⌘W
/// + ⌘Q still catches the final state (the subsequent synchronous `flush`
/// cancels the pending timer).
pub const DEBOUNCE: Duration = Duration::from_millis(500);

/// Application-support subfolder for the Rust build — deliberately distinct
/// from the Swift `Nice` / `Nice Dev` folders so it can't clobber the user's
/// real sessions.
pub const STORE_FOLDER: &str = "Nice RS Dev";

/// The Swift app's application-support subfolder — the one-time migration
/// SOURCE. Read only when the own store is absent; NEVER written.
pub const SWIFT_STORE_FOLDER: &str = "Nice";

/// On-screen frame captured at last save (Cocoa window coordinates, origin at
/// the bottom-left of the primary screen — matches Swift `PersistedFrame`, so
/// migration needs no value conversion). Raw `f64`s keep the JSON shape stable
/// and human-readable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedFrame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// One persisted window. `sidebar_collapsed` is REQUIRED (Swift's decode
/// requires it); `frame` is optional so pre-frame-persistence files decode and
/// fall back to default placement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedWindow {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_tab_id: Option<String>,
    pub sidebar_collapsed: bool,
    pub projects: Vec<PersistedProject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<PersistedFrame>,
}

impl PersistedWindow {
    /// Total saved tabs across all projects — "does this window have restorable
    /// state" without caring which project owns what.
    pub fn total_tab_count(&self) -> usize {
        self.projects.iter().map(|p| p.tabs.len()).sum()
    }
}

/// The whole persisted document. `version` is written `3`; unknown future
/// versions still decode (forward-compat) since there is no version gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedState {
    pub version: i64,
    pub windows: Vec<PersistedWindow>,
}

impl PersistedState {
    /// The empty state a missing/corrupt/shape-mismatched file decodes to.
    pub fn empty() -> Self {
        PersistedState {
            version: CURRENT_VERSION,
            windows: Vec::new(),
        }
    }
}

// MARK: - Disk I/O seam

/// Disk side-effects performed by [`SessionStore`]. A trait so tests inject a
/// recorder that captures writes (with thread context) and skips touching disk.
/// Production uses [`DiskIo`]. Always invoked on the store's dedicated writer
/// thread — implementations must be safe off the calling thread.
pub trait SessionStoreIo: Send + Sync + 'static {
    fn write(&self, state: &PersistedState, path: &Path);
}

/// Production `SessionStoreIo`: serialize pretty + sorted-keys → atomic write.
/// A failed atomic write leaves the prior file intact (the write-failure
/// recovery contract).
pub struct DiskIo;

impl SessionStoreIo for DiskIo {
    fn write(&self, state: &PersistedState, path: &Path) {
        if let Ok(bytes) = serialize_state(state) {
            // `try?` semantics: a failed write is swallowed; the atomic rename
            // never ran, so the prior file is untouched.
            let _ = crate::atomic_file::write_atomic(path, &bytes, None);
        }
    }
}

/// Serialize `state` pretty-printed with recursively sorted keys (Swift's
/// `.sortedKeys`), so the output is byte-stable regardless of serde_json's
/// `preserve_order` feature — the property the write-if-changed compare needs.
fn serialize_state(state: &PersistedState) -> serde_json::Result<Vec<u8>> {
    let value = serde_json::to_value(state)?;
    serde_json::to_vec_pretty(&sort_value(value))
}

/// Recursively rebuild `v` with every object's keys in sorted order (arrays
/// keep their element order).
fn sort_value(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort();
            let mut sorted = Map::new();
            for k in keys {
                let child = map.get(&k).cloned().unwrap_or(Value::Null);
                sorted.insert(k, sort_value(child));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sort_value).collect()),
        other => other,
    }
}

/// Read + tolerantly decode the state at `path`. Missing / empty / corrupt /
/// shape-mismatched ⇒ [`PersistedState::empty`], never an error (Swift's
/// `read(from:)` contract).
pub fn read_state(path: &Path) -> PersistedState {
    let Ok(bytes) = fs::read(path) else {
        return PersistedState::empty();
    };
    if bytes.is_empty() {
        return PersistedState::empty();
    }
    serde_json::from_slice(&bytes).unwrap_or_else(|_| PersistedState::empty())
}

// MARK: - The store

/// Writer-thread coordination state, behind one mutex + condvar.
struct Inner {
    /// Last-read / last-written state — the in-memory cache.
    cached: PersistedState,
    /// Bumped on every cache mutation. `flush` blocks until the writer reports
    /// having written this revision.
    revision: u64,
    /// The last revision the writer has written (attempted). `== revision`
    /// means the cache is on disk.
    written_revision: u64,
    /// When the pending debounced write should fire, if one is scheduled.
    deadline: Option<Instant>,
    /// Force an immediate write of the current revision (set by `flush`).
    flush_now: bool,
    /// Writer-thread exit signal.
    shutdown: bool,
}

struct Shared {
    path: PathBuf,
    io: Box<dyn SessionStoreIo>,
    debounce: Duration,
    inner: Mutex<Inner>,
    cv: Condvar,
}

/// A live session store: an in-memory cache plus one dedicated writer thread.
/// Dropping the store flushes anything pending and joins the writer, so no
/// thread leaks.
pub struct SessionStore {
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl SessionStore {
    /// Open the store at `own_path`, running the one-time Swift migration when
    /// the own file is absent and `swift_source` is given. Production
    /// (`DiskIo`, 500 ms debounce).
    pub fn open(own_path: PathBuf, swift_source: Option<PathBuf>) -> Self {
        Self::open_with(own_path, swift_source, Box::new(DiskIo), DEBOUNCE)
    }

    /// [`SessionStore::open`] with an injected I/O seam + debounce (tests).
    pub fn open_with(
        own_path: PathBuf,
        swift_source: Option<PathBuf>,
        io: Box<dyn SessionStoreIo>,
        debounce: Duration,
    ) -> Self {
        // Own file present ⇒ read it. Absent ⇒ migrate from the Swift source
        // (branch auto-dropped: our leaf structs have no `branch` field). The
        // Swift file is only ever READ.
        let (cached, migrated) = if own_path.exists() {
            (read_state(&own_path), false)
        } else if let Some(src) = &swift_source {
            (read_state(src), true)
        } else {
            (PersistedState::empty(), false)
        };

        let store = Self::build(own_path, cached, io, debounce);

        // One-time migration write: persist the adopted state to the OWN store
        // so a subsequent launch reads its own file (and never re-reads a
        // since-changed Swift file). Never touches `swift_source`.
        if migrated && !store.shared.inner.lock().unwrap().cached.windows.is_empty() {
            {
                let mut inner = store.shared.inner.lock().unwrap();
                inner.revision += 1;
            }
            store.flush();
        }

        store
    }

    /// Construct the store + spawn its writer thread. Creates the parent dir of
    /// `path` best-effort so the first write lands.
    fn build(
        path: PathBuf,
        cached: PersistedState,
        io: Box<dyn SessionStoreIo>,
        debounce: Duration,
    ) -> Self {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let shared = Arc::new(Shared {
            path,
            io,
            debounce,
            inner: Mutex::new(Inner {
                cached,
                revision: 0,
                written_revision: 0,
                deadline: None,
                flush_now: false,
                shutdown: false,
            }),
            cv: Condvar::new(),
        });
        let handle = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("nice-session-store-writer".into())
                .spawn(move || writer_loop(shared))
                .expect("spawn session-store writer thread")
        };
        SessionStore {
            shared,
            handle: Some(handle),
        }
    }

    /// Return the current persisted state (the cache; no disk hit).
    pub fn load(&self) -> PersistedState {
        self.shared.inner.lock().unwrap().cached.clone()
    }

    /// Merge `window` into the cache, replacing any entry with the same id, and
    /// schedule a debounced write. Never blocks.
    pub fn upsert(&self, window: PersistedWindow) {
        let mut inner = self.shared.inner.lock().unwrap();
        let mut windows = std::mem::take(&mut inner.cached.windows);
        match windows.iter().position(|w| w.id == window.id) {
            Some(idx) => windows[idx] = window,
            None => windows.push(window),
        }
        inner.cached = PersistedState {
            version: CURRENT_VERSION,
            windows,
        };
        self.mark_dirty_and_schedule(&mut inner);
    }

    /// Drop the entry whose id matches. No-op (no write) if the id isn't
    /// present — a quit right after a `remove` must not resurrect the slot from
    /// a stale debounce, and a no-op must not schedule spurious I/O.
    pub fn remove(&self, window_id: &str) {
        let mut inner = self.shared.inner.lock().unwrap();
        let before = inner.cached.windows.len();
        let windows: Vec<PersistedWindow> = std::mem::take(&mut inner.cached.windows)
            .into_iter()
            .filter(|w| w.id != window_id)
            .collect();
        if windows.len() == before {
            inner.cached.windows = windows;
            return;
        }
        inner.cached = PersistedState {
            version: CURRENT_VERSION,
            windows,
        };
        self.mark_dirty_and_schedule(&mut inner);
    }

    /// Drop every window with zero tabs except `keeping` (the caller's own slot
    /// so it can still save into it). No-op (no write) when nothing changes.
    pub fn prune_empty_windows(&self, keeping: &str) {
        self.prune_empty_windows_keeping(&[keeping.to_string()]);
    }

    /// [`prune_empty_windows`](Self::prune_empty_windows) keeping a SET of ids —
    /// the R18 restore fan-out's post-restore GC (Swift `pruneEmptyWindows(keeping:)`
    /// run after adoption), keeping every just-restored window id so a legitimately
    /// empty Terminals-only restored window survives the prune. No-op (no write)
    /// when nothing changes.
    pub fn prune_empty_windows_keeping(&self, keeping: &[String]) {
        let mut inner = self.shared.inner.lock().unwrap();
        let before = inner.cached.windows.len();
        let windows: Vec<PersistedWindow> = std::mem::take(&mut inner.cached.windows)
            .into_iter()
            .filter(|w| keeping.iter().any(|k| k == &w.id) || w.total_tab_count() > 0)
            .collect();
        if windows.len() == before {
            inner.cached.windows = windows;
            return;
        }
        inner.cached = PersistedState {
            version: CURRENT_VERSION,
            windows,
        };
        self.mark_dirty_and_schedule(&mut inner);
    }

    fn mark_dirty_and_schedule(&self, inner: &mut Inner) {
        inner.revision += 1;
        inner.deadline = Some(Instant::now() + self.shared.debounce);
        self.shared.cv.notify_all();
    }

    /// Cancel any pending debounced write and flush the cache to disk
    /// synchronously — blocks until the write completes, preserving the
    /// "flushed before terminate returns" guarantee. Writes only when the cache
    /// is ahead of disk (nothing to flush ⇒ returns immediately).
    pub fn flush(&self) {
        let mut inner = self.shared.inner.lock().unwrap();
        let target = inner.revision;
        if inner.written_revision >= target {
            return;
        }
        inner.flush_now = true;
        inner.deadline = None; // flush cancels/coalesces the pending debounce
        self.shared.cv.notify_all();
        while inner.written_revision < target {
            inner = self.shared.cv.wait(inner).unwrap();
        }
    }
}

impl Drop for SessionStore {
    fn drop(&mut self) {
        // Persist anything pending, then stop + join the writer (no thread
        // leak, no lost write).
        self.flush();
        {
            let mut inner = self.shared.inner.lock().unwrap();
            inner.shutdown = true;
        }
        self.shared.cv.notify_all();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The dedicated writer thread: sleeps until a debounced deadline elapses, a
/// flush forces a write, or shutdown. Serial by construction — the ONLY writer.
fn writer_loop(shared: Arc<Shared>) {
    loop {
        let mut inner = shared.inner.lock().unwrap();
        // Wait for work.
        loop {
            if inner.shutdown && inner.deadline.is_none() && !inner.flush_now {
                return;
            }
            if inner.flush_now {
                break;
            }
            match inner.deadline {
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        break;
                    }
                    let (guard, _) = shared.cv.wait_timeout(inner, deadline - now).unwrap();
                    inner = guard;
                }
                None => {
                    if inner.shutdown {
                        return;
                    }
                    inner = shared.cv.wait(inner).unwrap();
                }
            }
        }

        let target = inner.revision;
        let force = inner.flush_now;
        inner.flush_now = false;
        inner.deadline = None;
        if !force && inner.written_revision >= target {
            // Debounced timer fired with nothing new to write.
            continue;
        }
        let snapshot = inner.cached.clone();
        drop(inner);

        shared.io.write(&snapshot, &shared.path);

        let mut inner = shared.inner.lock().unwrap();
        inner.written_revision = target;
        shared.cv.notify_all();
    }
}

// MARK: - Path resolution (called from app::run ONLY)

/// Application-support root: `NICE_APPLICATION_SUPPORT_ROOT` when set (tests /
/// scenarios redirect state into a sandbox), else `~/Library/Application
/// Support`. Resolved only in `app::run` (the `shell_inject.rs` convention) —
/// never in a test or `run_selftest`.
fn support_root() -> PathBuf {
    match env::var("NICE_APPLICATION_SUPPORT_ROOT") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => {
            let home = env::var("HOME").unwrap_or_else(|_| "/".to_string());
            PathBuf::from(home).join("Library/Application Support")
        }
    }
}

/// The OWN store path: `<support-root>/Nice RS Dev/sessions.json`.
pub fn default_store_path() -> PathBuf {
    support_root().join(STORE_FOLDER).join("sessions.json")
}

/// The Swift migration SOURCE path: `<support-root>/Nice/sessions.json`. Read
/// only when the own store is absent; never written.
pub fn swift_migration_source() -> PathBuf {
    support_root().join(SWIFT_STORE_FOLDER).join("sessions.json")
}

// MARK: - Process Global (absent ⇒ every persistence hook is a no-op)

fn global_cell() -> &'static Mutex<Option<Arc<SessionStore>>> {
    static GLOBAL: OnceLock<Mutex<Option<Arc<SessionStore>>>> = OnceLock::new();
    GLOBAL.get_or_init(|| Mutex::new(None))
}

/// Install `store` as the process store, returning the shared handle. Called by
/// `app::run` only.
pub fn install_global(store: SessionStore) -> Arc<SessionStore> {
    let arc = Arc::new(store);
    *global_cell().lock().unwrap() = Some(Arc::clone(&arc));
    arc
}

/// The installed store, if any.
pub fn global() -> Option<Arc<SessionStore>> {
    global_cell().lock().unwrap().clone()
}

/// Uninstall the process store (test teardown / shutdown). Dropping the last
/// `Arc` flushes + joins the writer.
pub fn clear_global() {
    *global_cell().lock().unwrap() = None;
}

/// Persistence hooks — each a NO-OP when no store is installed.
pub fn upsert(window: PersistedWindow) {
    if let Some(store) = global() {
        store.upsert(window);
    }
}

pub fn remove(window_id: &str) {
    if let Some(store) = global() {
        store.remove(window_id);
    }
}

pub fn prune_empty_windows(keeping: &str) {
    if let Some(store) = global() {
        store.prune_empty_windows(keeping);
    }
}

pub fn prune_empty_windows_keeping(keeping: &[String]) {
    if let Some(store) = global() {
        store.prune_empty_windows_keeping(keeping);
    }
}

pub fn flush() {
    if let Some(store) = global() {
        store.flush();
    }
}

/// The current persisted state — empty when no store is installed.
pub fn load() -> PersistedState {
    global()
        .map(|store| store.load())
        .unwrap_or_else(PersistedState::empty)
}

#[cfg(test)]
mod tests;
