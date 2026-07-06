//! Ported from `Tests/NiceUnitTests/SessionStoreTests.swift` (24),
//! `SessionStoreFlushOrderingTests.swift` (2), and
//! `SessionStoreWriteFailureTests.swift` (1), plus NEW cases with no Swift twin:
//! the one-time Swift migration read, the store-absent no-op rule, and
//! `PersistedState`/`PersistedFrame` round-trip + decode-tolerance (the window
//! envelope; the model-leaf round-trips live in `nice-model`'s `persisted.rs`).
//!
//! Swift's "ioQueue" assertions become "writer thread, not the caller": the
//! recording IO captures the thread each write ran on and the test asserts it
//! differs from the calling (test) thread — the Rust analog of "not main".
//!
//! Tests use per-test temp dirs and never read/write the developer's real
//! `~/Library/Application Support`. Cadence assertions here run on wall-clock
//! `Instant`s bounded generously (these are libtest logic tests, not scenario
//! perf gates) — the same shape as the Swift `XCTestExpectation` spins.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use nice_model::{PaneKind, PersistedPane, PersistedProject, PersistedTab};

use super::*;

// ---- temp-dir plumbing -------------------------------------------------

/// A throwaway directory removed on drop.
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn unique(prefix: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{prefix}-{}-{n}", std::process::id()))
}
fn scratch(prefix: &str) -> Scratch {
    let dir = unique(prefix);
    fs::create_dir_all(&dir).expect("create scratch dir");
    Scratch(dir)
}

// ---- recording IO ------------------------------------------------------

struct WriteRecord {
    snapshot: PersistedState,
    thread: ThreadId,
    fired_at: Instant,
    completed_at: Instant,
}

/// Test double: records each write (snapshot + thread + timestamps) and never
/// touches disk. An optional injected `delay` pins "flush blocks until the
/// writer completes"; a channel wakes tests waiting for a write.
struct RecordingIo {
    writes: Arc<Mutex<Vec<WriteRecord>>>,
    delay: Duration,
    // `mpsc::Sender` is `Send` but not `Sync`; wrap it so `RecordingIo`
    // satisfies the `SessionStoreIo: Send + Sync` bound.
    notify: Mutex<mpsc::Sender<()>>,
}
impl SessionStoreIo for RecordingIo {
    fn write(&self, state: &PersistedState, _path: &Path) {
        let fired_at = Instant::now();
        let thread = std::thread::current().id();
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        let completed_at = Instant::now();
        self.writes.lock().unwrap().push(WriteRecord {
            snapshot: state.clone(),
            thread,
            fired_at,
            completed_at,
        });
        let _ = self.notify.lock().unwrap().send(());
    }
}

/// A store backed by a recording IO. The returned path is a placeholder (the
/// recorder never writes disk); the channel receiver fires once per write.
fn recorder_store(delay: Duration) -> (SessionStore, Arc<Mutex<Vec<WriteRecord>>>, Receiver<()>) {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = mpsc::channel();
    let io = RecordingIo {
        writes: Arc::clone(&writes),
        delay,
        notify: Mutex::new(tx),
    };
    let dir = scratch("nice-store-rec");
    let path = dir.0.join("sessions.json");
    // Keep the scratch dir alive for the store's lifetime by leaking it into
    // the store's path only; the OS temp dir is reaped on process exit even if
    // Drop doesn't run for a leaked handle.
    std::mem::forget(dir);
    let store = SessionStore::open_with(path, None, Box::new(io), DEBOUNCE);
    (store, writes, rx)
}

/// A production-shaped store (DiskIo) at `path`.
fn disk_store(path: &Path) -> SessionStore {
    SessionStore::open_with(path.to_path_buf(), None, Box::new(DiskIo), DEBOUNCE)
}

// ---- window/tab builders (mirror the Swift helpers) --------------------

fn make_persisted_tab(id: &str) -> PersistedTab {
    PersistedTab {
        id: id.into(),
        title: id.into(),
        cwd: "/tmp".into(),
        claude_session_id: None,
        active_pane_id: None,
        panes: vec![],
        title_manually_set: None,
        parent_tab_id: None,
        next_terminal_index: None,
    }
}

fn make_window(id: &str, tabs: Vec<PersistedTab>) -> PersistedWindow {
    let active_tab_id = tabs.first().map(|t| t.id.clone());
    let projects = if tabs.is_empty() {
        vec![]
    } else {
        vec![PersistedProject {
            id: "p".into(),
            name: "Project".into(),
            path: "/tmp".into(),
            tabs,
        }]
    };
    PersistedWindow {
        id: id.into(),
        active_tab_id,
        sidebar_collapsed: false,
        projects,
        frame: None,
    }
}

/// A window whose single tab carries `session_id` (a Claude tab) — for the
/// flush-ordering rotations.
fn make_session_window(id: &str, session_id: &str) -> PersistedWindow {
    let tab = PersistedTab {
        id: format!("tab-{id}"),
        title: "Tab".into(),
        cwd: "/tmp".into(),
        claude_session_id: Some(session_id.into()),
        active_pane_id: Some(format!("pane-{id}")),
        panes: vec![PersistedPane {
            id: format!("pane-{id}"),
            title: "Claude".into(),
            kind: PaneKind::Claude,
            cwd: None,
            title_manually_set: None,
        }],
        title_manually_set: None,
        parent_tab_id: None,
        next_terminal_index: None,
    };
    PersistedWindow {
        id: id.into(),
        active_tab_id: Some(tab.id.clone()),
        sidebar_collapsed: false,
        projects: vec![PersistedProject {
            id: format!("project-{id}"),
            name: format!("Project {id}"),
            path: "/tmp".into(),
            tabs: vec![tab],
        }],
        frame: None,
    }
}

fn first_tab_session_id(window: &PersistedWindow) -> Option<String> {
    window.projects.first()?.tabs.first()?.claude_session_id.clone()
}

fn sorted_ids(state: &PersistedState) -> Vec<String> {
    let mut ids: Vec<String> = state.windows.iter().map(|w| w.id.clone()).collect();
    ids.sort();
    ids
}

// MARK: - init / load

#[test]
fn init_on_missing_file_load_returns_empty() {
    let dir = scratch("nice-store-init");
    let store = disk_store(&dir.0.join("sessions.json"));
    assert!(store.load().windows.is_empty());
    assert_eq!(store.load().version, CURRENT_VERSION);
}

#[test]
fn init_on_corrupt_file_load_returns_empty_without_crash() {
    let dir = scratch("nice-store-corrupt");
    let path = dir.0.join("sessions.json");
    fs::write(&path, b"not json {{").unwrap();
    let store = disk_store(&path);
    assert!(
        store.load().windows.is_empty(),
        "a corrupt sessions.json must decode to empty so the app still launches"
    );
}

#[test]
fn init_on_shape_mismatch_discards_payload() {
    let dir = scratch("nice-store-shape");
    let path = dir.0.join("sessions.json");
    fs::write(&path, br#"{"windows":"not-an-array"}"#).unwrap();
    let store = disk_store(&path);
    assert!(
        store.load().windows.is_empty(),
        "a structurally invalid payload must decode to empty rather than crash"
    );
}

// MARK: - upsert

#[test]
fn upsert_appends_new_window() {
    let dir = scratch("nice-store-upsert-append");
    let store = disk_store(&dir.0.join("sessions.json"));
    store.upsert(make_window("w1", vec![]));
    store.flush();
    assert_eq!(store.load().windows.len(), 1);
    assert_eq!(store.load().windows[0].id, "w1");
}

#[test]
fn upsert_replaces_existing_window_by_id() {
    let dir = scratch("nice-store-upsert-replace");
    let store = disk_store(&dir.0.join("sessions.json"));
    store.upsert(make_window("w1", vec![make_persisted_tab("t1")]));
    store.flush();
    store.upsert(make_window(
        "w1",
        vec![make_persisted_tab("t1"), make_persisted_tab("t2")],
    ));
    store.flush();
    let windows = store.load().windows;
    assert_eq!(windows.len(), 1, "upsert must replace by id, not append");
    assert_eq!(windows[0].total_tab_count(), 2);
}

#[test]
fn upsert_multiple_windows_coexist_by_distinct_id() {
    let dir = scratch("nice-store-upsert-multi");
    let store = disk_store(&dir.0.join("sessions.json"));
    store.upsert(make_window("w1", vec![make_persisted_tab("t1")]));
    store.upsert(make_window("w2", vec![make_persisted_tab("t2")]));
    store.flush();
    assert_eq!(sorted_ids(&store.load()), vec!["w1", "w2"]);
}

// MARK: - flush

#[test]
fn flush_writes_synchronously() {
    let dir = scratch("nice-store-flush-sync");
    let path = dir.0.join("sessions.json");
    {
        let store = disk_store(&path);
        store.upsert(make_window("w1", vec![]));
        store.flush();
        assert!(path.exists(), "flush must write the file synchronously");
    }
    let fresh = disk_store(&path);
    assert_eq!(fresh.load().windows[0].id, "w1");
}

#[test]
fn flush_after_upsert_persists_latest_state_for_same_window() {
    let dir = scratch("nice-store-flush-latest");
    let path = dir.0.join("sessions.json");
    {
        let store = disk_store(&path);
        store.upsert(make_window("w1", vec![make_persisted_tab("t1")]));
        store.flush();
        store.upsert(make_window(
            "w1",
            vec![make_persisted_tab("t1"), make_persisted_tab("t2")],
        ));
        store.flush();
        // Spin past the debounce so any stale cancelled work would fire.
        std::thread::sleep(Duration::from_millis(600));
    }
    let fresh = disk_store(&path);
    assert_eq!(fresh.load().windows.len(), 1);
    assert_eq!(
        fresh.load().windows[0].total_tab_count(),
        2,
        "the latest state must survive — a cancelled debounce must not resurrect the first upsert"
    );
}

// MARK: - threading + timing (Swift's queue-routing cases)

#[test]
fn debounced_write_runs_on_writer_thread_after_debounce_window() {
    let (store, writes, rx) = recorder_store(Duration::ZERO);
    let upserted_at = Instant::now();
    store.upsert(make_window("w1", vec![]));
    rx.recv_timeout(Duration::from_secs(2))
        .expect("debounced writer must fire");
    let w = writes.lock().unwrap();
    assert_eq!(w.len(), 1);
    assert_ne!(
        w[0].thread,
        std::thread::current().id(),
        "debounced write must run on the writer thread, not the caller"
    );
    assert!(
        w[0].fired_at.duration_since(upserted_at) >= Duration::from_millis(400),
        "debounced write must respect the 500ms window (≥0.4s lower bound)"
    );
}

#[test]
fn flush_runs_write_on_writer_thread_and_blocks_until_complete() {
    let writer_sleep = Duration::from_millis(200);
    let (store, writes, _rx) = recorder_store(writer_sleep);
    store.upsert(make_window("w1", vec![]));
    let before = Instant::now();
    store.flush();
    let after = Instant::now();
    let w = writes.lock().unwrap();
    assert!(!w.is_empty());
    let last = w.last().unwrap();
    assert_ne!(
        last.thread,
        std::thread::current().id(),
        "flush must dispatch the write to the writer thread, not run it on the caller"
    );
    assert!(
        after.duration_since(before) >= writer_sleep - Duration::from_millis(50),
        "flush must block until the off-thread write completes"
    );
    assert!(after >= last.completed_at, "flush returned before the write completed");
}

#[test]
fn upsert_then_flush_writes_exactly_once() {
    let (store, writes, _rx) = recorder_store(Duration::ZERO);
    store.upsert(make_window("w1", vec![]));
    store.flush();
    // Spin past the debounce: a broken cancellation would fire a 2nd write.
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(
        writes.lock().unwrap().len(),
        1,
        "upsert + flush must write exactly once"
    );
}

#[test]
fn flush_after_in_flight_debounce_writes_in_order_latest_snapshot_wins() {
    let (store, writes, rx) = recorder_store(Duration::from_millis(50));
    store.upsert(make_session_window("w1", "A"));
    // Wait for the debounced write(A) to fire and complete.
    rx.recv_timeout(Duration::from_secs(2))
        .expect("debounce timer must fire");
    store.upsert(make_session_window("w1", "B"));
    store.flush();
    let w = writes.lock().unwrap();
    assert_eq!(w.len(), 2, "debounced write(A) plus flush write(B) must both land");
    assert_eq!(
        first_tab_session_id(&w[0].snapshot.windows[0]).as_deref(),
        Some("A"),
        "serial writer: write(A) completes first"
    );
    assert_eq!(
        first_tab_session_id(&w[1].snapshot.windows[0]).as_deref(),
        Some("B"),
        "then write(B) from flush"
    );
}

#[test]
fn rapid_upserts_coalesce_to_single_write() {
    let (store, writes, _rx) = recorder_store(Duration::ZERO);
    for i in 0..100 {
        store.upsert(make_window(&format!("w{}", i % 5), vec![]));
    }
    store.flush();
    assert_eq!(
        writes.lock().unwrap().len(),
        1,
        "100 rapid upserts + flush must coalesce to exactly one write"
    );
}

// MARK: - prune_empty_windows

#[test]
fn prune_empty_windows_drops_zero_tab_windows_except_keep() {
    let dir = scratch("nice-store-prune");
    let store = disk_store(&dir.0.join("sessions.json"));
    store.upsert(make_window("keep", vec![]));
    store.upsert(make_window("empty", vec![]));
    store.upsert(make_window("full", vec![make_persisted_tab("t1")]));
    store.flush();
    store.prune_empty_windows("keep");
    store.flush();
    assert_eq!(
        sorted_ids(&store.load()),
        vec!["full", "keep"],
        "prune keeps the caller's slot and anything with tabs; drops the rest"
    );
}

#[test]
fn prune_empty_windows_is_noop_when_nothing_to_prune() {
    let dir = scratch("nice-store-prune-noop");
    let store = disk_store(&dir.0.join("sessions.json"));
    store.upsert(make_window("w1", vec![make_persisted_tab("t1")]));
    store.flush();
    let before = store.load();
    store.prune_empty_windows("w1");
    store.flush();
    assert_eq!(store.load(), before, "pruning when nothing is empty must not change state");
}

// MARK: - remove

#[test]
fn remove_drops_entry_by_id_and_persists() {
    let dir = scratch("nice-store-remove");
    let path = dir.0.join("sessions.json");
    {
        let store = disk_store(&path);
        store.upsert(make_window("w1", vec![make_persisted_tab("t1")]));
        store.upsert(make_window("w2", vec![make_persisted_tab("t2")]));
        store.flush();
        store.remove("w1");
        store.flush();
        assert_eq!(sorted_ids(&store.load()), vec!["w2"]);
    }
    let fresh = disk_store(&path);
    assert_eq!(
        sorted_ids(&fresh.load()),
        vec!["w2"],
        "remove must persist to disk so a relaunch sees the survivors"
    );
}

#[test]
fn remove_is_noop_when_id_missing() {
    let (store, writes, _rx) = recorder_store(Duration::ZERO);
    store.upsert(make_window("w1", vec![make_persisted_tab("t1")]));
    store.flush();
    let before = writes.lock().unwrap().len();
    store.remove("ghost");
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(
        writes.lock().unwrap().len(),
        before,
        "remove for a missing id must not trigger an extra write"
    );
}

#[test]
fn remove_schedules_debounced_write() {
    let dir = scratch("nice-store-remove-sched");
    let path = dir.0.join("sessions.json");
    {
        let store = disk_store(&path);
        store.upsert(make_window("w1", vec![make_persisted_tab("t1")]));
        store.upsert(make_window("w2", vec![make_persisted_tab("t2")]));
        store.flush();
        store.remove("w1");
        // No flush — wait for the debounce + write to land.
        std::thread::sleep(Duration::from_millis(700));
    }
    let fresh = disk_store(&path);
    assert_eq!(
        sorted_ids(&fresh.load()),
        vec!["w2"],
        "remove must schedule a debounced write so the change reaches disk without a flush"
    );
}

// MARK: - round-trip (window envelope)

#[test]
fn round_trip_preserves_every_field() {
    let dir = scratch("nice-store-rt-full");
    let path = dir.0.join("sessions.json");
    let window = PersistedWindow {
        id: "w1".into(),
        active_tab_id: Some("t1".into()),
        sidebar_collapsed: true,
        projects: vec![PersistedProject {
            id: "nice".into(),
            name: "Nice".into(),
            path: "/Users/nick/Projects/nice".into(),
            tabs: vec![PersistedTab {
                id: "t1".into(),
                title: "Fix top bar height".into(),
                cwd: "/Users/nick/Projects/nice".into(),
                claude_session_id: Some("e4f1a2b3-c0d4-4e5f-9a0b-1c2d3e4f5a6b".into()),
                active_pane_id: Some("p1".into()),
                panes: vec![
                    PersistedPane {
                        id: "p1".into(),
                        title: "Claude".into(),
                        kind: PaneKind::Claude,
                        cwd: None,
                        title_manually_set: None,
                    },
                    PersistedPane {
                        id: "p2".into(),
                        title: "zsh".into(),
                        kind: PaneKind::Terminal,
                        cwd: None,
                        title_manually_set: None,
                    },
                ],
                title_manually_set: None,
                parent_tab_id: None,
                next_terminal_index: None,
            }],
        }],
        frame: None,
    };
    {
        let store = disk_store(&path);
        store.upsert(window.clone());
        store.flush();
    }
    let restored = disk_store(&path).load().windows.into_iter().next();
    assert_eq!(restored.as_ref(), Some(&window), "encode + decode must preserve every field");
}

#[test]
fn round_trip_preserves_nil_optionals() {
    let dir = scratch("nice-store-rt-nil");
    let path = dir.0.join("sessions.json");
    let window = PersistedWindow {
        id: "w1".into(),
        active_tab_id: None,
        sidebar_collapsed: false,
        projects: vec![PersistedProject {
            id: "terminals".into(),
            name: "Terminals".into(),
            path: "/tmp".into(),
            tabs: vec![PersistedTab {
                id: "t1".into(),
                title: "Main".into(),
                cwd: "/tmp".into(),
                claude_session_id: None,
                active_pane_id: None,
                panes: vec![],
                title_manually_set: None,
                parent_tab_id: None,
                next_terminal_index: None,
            }],
        }],
        frame: None,
    };
    {
        let store = disk_store(&path);
        store.upsert(window.clone());
        store.flush();
    }
    let restored = disk_store(&path).load().windows.into_iter().next();
    assert_eq!(restored.as_ref(), Some(&window));
}

#[test]
fn persisted_window_round_trip_with_frame() {
    let dir = scratch("nice-store-rt-frame");
    let path = dir.0.join("sessions.json");
    let window = PersistedWindow {
        id: "w1".into(),
        active_tab_id: Some("t1".into()),
        sidebar_collapsed: false,
        projects: vec![PersistedProject {
            id: "p".into(),
            name: "P".into(),
            path: "/tmp".into(),
            tabs: vec![make_persisted_tab("t1")],
        }],
        frame: Some(PersistedFrame {
            x: 17.5,
            y: 33.25,
            width: 1280.5,
            height: 800.75,
        }),
    };
    {
        let store = disk_store(&path);
        store.upsert(window.clone());
        store.flush();
    }
    let restored = disk_store(&path).load().windows.into_iter().next();
    assert_eq!(
        restored.as_ref(),
        Some(&window),
        "PersistedFrame must round-trip through encode/decode unchanged (sub-pixel doubles)"
    );
}

// MARK: - decode tolerance (window envelope)

#[test]
fn decodes_future_version_with_unknown_fields_forward_compat() {
    let json = r#"{
        "version": 4,
        "futureRoot": "ignore me",
        "windows": [{
            "id": "w1",
            "activeTabId": "t1",
            "sidebarCollapsed": false,
            "futureWindow": 42,
            "projects": [{
                "id": "p1", "name": "Project", "path": "/tmp", "futureProject": ["a","b"],
                "tabs": [{
                    "id": "t1", "title": "Main", "cwd": "/tmp", "branch": null,
                    "claudeSessionId": "session-uuid", "activePaneId": "pane-1",
                    "futureTab": {"nested": true},
                    "panes": [{"id": "pane-1", "title": "Claude", "kind": "claude", "cwd": "/tmp", "futurePane": "ignored"}]
                }]
            }]
        }]
    }"#;
    let decoded: PersistedState = serde_json::from_str(json).unwrap();
    assert_eq!(decoded.windows.len(), 1);
    assert_eq!(decoded.windows[0].id, "w1");
    let tab = &decoded.windows[0].projects[0].tabs[0];
    assert_eq!(
        tab.claude_session_id.as_deref(),
        Some("session-uuid"),
        "forward-compat must preserve v3 fields verbatim"
    );
    assert_eq!(tab.panes[0].kind, PaneKind::Claude);
}

#[test]
fn persisted_window_decodes_without_frame_field_backwards_compat() {
    let json = r#"{
        "version": 3,
        "windows": [{
            "id": "w1", "activeTabId": "t1", "sidebarCollapsed": false,
            "projects": [{
                "id": "terminals", "name": "Terminals", "path": "/tmp",
                "tabs": [{"id": "t1", "title": "Main", "cwd": "/Users/nick", "branch": null,
                          "claudeSessionId": null, "activePaneId": "p1",
                          "panes": [{"id": "p1", "title": "zsh", "kind": "terminal"}]}]
            }]
        }]
    }"#;
    let decoded: PersistedState = serde_json::from_str(json).unwrap();
    assert!(
        decoded.windows[0].frame.is_none(),
        "a missing frame field must decode as None so older sessions load"
    );
}

#[test]
fn read_state_on_missing_file_is_empty() {
    let dir = scratch("nice-store-readstate");
    assert!(read_state(&dir.0.join("nope.json")).windows.is_empty());
}

// MARK: - write-failure recovery (SessionStoreWriteFailureTests)

#[test]
#[cfg(unix)]
fn flush_failure_leaves_prior_file_untouched_and_does_not_panic() {
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch("nice-store-writefail");
    // The store's own dir — the atomic-write temp file is created here, so
    // locking it blocks the write.
    let nice_dir = dir.0.join(STORE_FOLDER);
    fs::create_dir_all(&nice_dir).unwrap();
    let path = nice_dir.join("sessions.json");

    // Step 1: baseline state A on disk.
    {
        let store = disk_store(&path);
        store.upsert(make_session_window("w1", "STATE-A"));
        store.flush();
    }
    let prior_bytes = fs::read(&path).unwrap();
    assert!(!prior_bytes.is_empty(), "precondition: state A must be on disk");

    // Step 2: lock the parent dir so the temp file can't be created.
    fs::set_permissions(&nice_dir, fs::Permissions::from_mode(0o500)).unwrap();

    // Step 3: a flush of state B must NOT panic; the failed atomic write leaves
    // the prior file intact.
    {
        let store = disk_store(&path);
        store.upsert(make_session_window("w1", "STATE-B"));
        store.flush();
    }

    // Step 4: restore writability + inspect.
    fs::set_permissions(&nice_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let post_bytes = fs::read(&path).unwrap();
    assert_eq!(
        post_bytes, prior_bytes,
        "a failed atomic write must leave the prior sessions.json byte-identical"
    );
    let fresh = disk_store(&path);
    assert_eq!(
        first_tab_session_id(&fresh.load().windows[0]).as_deref(),
        Some("STATE-A"),
        "after a failed flush, a fresh store must read the prior state"
    );
}

// MARK: - flush ordering (SessionStoreFlushOrderingTests)

#[test]
fn interleaved_upserts_across_windows_flush_persists_latest_for_each() {
    let dir = scratch("nice-store-interleave");
    let path = dir.0.join("sessions.json");
    let restored = {
        let store = disk_store(&path);
        // Round 1.
        store.upsert(make_session_window("w1", "S1-INIT"));
        store.upsert(make_session_window("w2", "S2-INIT"));
        store.upsert(make_session_window("w3", "S3-INIT"));
        // Round 2 — interleaved rotations inside the debounce window.
        store.upsert(make_session_window("w2", "S2-NEW"));
        store.upsert(make_session_window("w1", "S1-NEW"));
        store.upsert(make_session_window("w3", "S3-NEW"));
        store.flush();
        std::thread::sleep(Duration::from_millis(600));
        store.load()
    };
    let by_id = |id: &str| -> Option<String> {
        restored
            .windows
            .iter()
            .find(|w| w.id == id)
            .and_then(first_tab_session_id)
    };
    assert_eq!(by_id("w1").as_deref(), Some("S1-NEW"));
    assert_eq!(by_id("w2").as_deref(), Some("S2-NEW"));
    assert_eq!(by_id("w3").as_deref(), Some("S3-NEW"));
    assert_eq!(restored.windows.len(), 3);
}

#[test]
fn late_upsert_after_flush_does_not_resurrect_stale_state() {
    let dir = scratch("nice-store-late");
    let path = dir.0.join("sessions.json");
    {
        let store = disk_store(&path);
        store.upsert(make_session_window("w1", "A"));
        store.flush();
        store.upsert(make_session_window("w1", "B"));
        store.flush();
        std::thread::sleep(Duration::from_millis(600));
    }
    let fresh = disk_store(&path);
    assert_eq!(
        first_tab_session_id(&fresh.load().windows[0]).as_deref(),
        Some("B"),
        "latest flushed state must survive — no late debounce can revert it"
    );
}

// MARK: - migration (NEW — no Swift twin)

#[test]
fn migration_adopts_swift_file_drops_branch_writes_own_leaves_source_untouched() {
    let dir = scratch("nice-store-migrate");
    let swift_dir = dir.0.join(SWIFT_STORE_FOLDER);
    fs::create_dir_all(&swift_dir).unwrap();
    let swift_path = swift_dir.join("sessions.json");
    // A Swift-shaped v3 file, WITH `branch` fields (which our schema drops).
    let swift_json = r#"{
        "version": 3,
        "windows": [{
            "id": "swift-w1",
            "activeTabId": "t1",
            "sidebarCollapsed": true,
            "projects": [{
                "id": "nice", "name": "Nice", "path": "/Users/nick/Projects/nice",
                "tabs": [{
                    "id": "t1", "title": "Ship", "cwd": "/Users/nick/Projects/nice",
                    "branch": "feature/x", "claudeSessionId": "sid-42",
                    "activePaneId": "p1", "titleManuallySet": true,
                    "panes": [{"id": "p1", "title": "Claude", "kind": "claude", "cwd": "/Users/nick/Projects/nice"}]
                }]
            }]
        }]
    }"#;
    fs::write(&swift_path, swift_json).unwrap();
    let swift_before = fs::read(&swift_path).unwrap();

    let own_path = dir.0.join(STORE_FOLDER).join("sessions.json");
    assert!(!own_path.exists(), "precondition: own store absent → migration runs");

    let migrated_windows = {
        let store = SessionStore::open(own_path.clone(), Some(swift_path.clone()));
        let windows = store.load().windows;
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].id, "swift-w1");
        assert!(windows[0].sidebar_collapsed);
        let tab = &windows[0].projects[0].tabs[0];
        assert_eq!(tab.claude_session_id.as_deref(), Some("sid-42"), "lossless adopt");
        assert_eq!(tab.title_manually_set, Some(true));
        assert!(own_path.exists(), "migration must write the OWN store");
        windows
    };

    // A fresh store (own present, no source) reads its own file — same content.
    {
        let fresh = SessionStore::open(own_path.clone(), None);
        assert_eq!(fresh.load().windows, migrated_windows);
    }

    // The Swift source bytes are untouched.
    assert_eq!(
        fs::read(&swift_path).unwrap(),
        swift_before,
        "migration must NEVER write the Swift source path"
    );
}

#[test]
fn migration_absent_source_and_own_yields_empty_no_file() {
    let dir = scratch("nice-store-migrate-absent");
    let own_path = dir.0.join(STORE_FOLDER).join("sessions.json");
    let swift_path = dir.0.join(SWIFT_STORE_FOLDER).join("sessions.json");
    let store = SessionStore::open(own_path.clone(), Some(swift_path));
    assert!(store.load().windows.is_empty());
    assert!(
        !own_path.exists(),
        "no source + no own ⇒ empty state, and no own file is written"
    );
}

// MARK: - process Global + absent ⇒ no-op (NEW)

#[test]
fn global_routing_then_absent_is_noop() {
    // Single test owns the process Global (no other test installs it), so this
    // is race-free under libtest's parallelism.
    clear_global();
    assert!(global().is_none());
    // Absent ⇒ every hook is a no-op and load() is empty.
    assert!(load().windows.is_empty());
    upsert(make_window("ghost", vec![]));
    remove("ghost");
    prune_empty_windows("ghost");
    flush();
    assert!(load().windows.is_empty(), "hooks must be no-ops with no store installed");

    // Install, then the free-function hooks route to the store.
    let dir = scratch("nice-store-global");
    let store = disk_store(&dir.0.join("sessions.json"));
    let _handle = install_global(store);
    assert!(global().is_some());
    upsert(make_window("w1", vec![make_persisted_tab("t1")]));
    flush();
    assert_eq!(load().windows.iter().map(|w| w.id.clone()).collect::<Vec<_>>(), vec!["w1"]);

    // Clear ⇒ back to no-op.
    drop(_handle);
    clear_global();
    assert!(global().is_none());
    assert!(load().windows.is_empty());
}
