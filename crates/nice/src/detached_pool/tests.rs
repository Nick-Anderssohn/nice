//! Slice-1 tests for the detached pool: the entry ops (order, take-by-id,
//! take-head, kill), the snapshot ⇄ hydrate round trip, and the launch-time
//! duplicate-id reconcile (plan §P4, review B3c).
//!
//! Everything here builds STRUCTURAL entries — an empty `DetachedPtys`. That is
//! exactly what launch hydration produces, and the live-payload half
//! (`take_session` / `insert_session`) belongs to the detach slice, which pins
//! "no SIGHUP on take" with a real marker child.
//!
//! No session store is installed in any of these, so the write-through inside
//! `did_mutate` is the documented no-op — the pool's ops are asserted on their
//! own.

use super::*;

use nice_model::{PersistedSession, TermWindow, TermWindowKind};

use crate::session_store::{PersistedWindow, CURRENT_VERSION};

// ---- builders ----------------------------------------------------------

fn project(id: &str) -> DetachedProject {
    DetachedProject {
        id: id.into(),
        name: format!("Project {id}"),
        path: format!("/tmp/{id}"),
    }
}

fn session(id: &str) -> Session {
    let mut s = Session::new(id, format!("Session {id}"), "/tmp/work");
    let mut window = TermWindow::new(format!("{id}-w1"), "Terminal 1", TermWindowKind::Terminal);
    window.cwd = Some("/tmp/work".into());
    s.active_window_id = Some(window.id.clone());
    s.windows = vec![window];
    s
}

fn entry(id: &str) -> DetachedEntry {
    DetachedEntry::structural(session(id), project("proj"))
}

fn persisted_session(id: &str) -> PersistedSession {
    PersistedSession::from_model(&session(id))
}

fn persisted_window(id: &str, session_ids: &[&str]) -> PersistedWindow {
    PersistedWindow {
        id: id.into(),
        active_session_id: session_ids.first().map(|s| (*s).to_string()),
        sidebar_collapsed: false,
        sidebar_mode: None,
        sidebar_width: None,
        projects: vec![nice_model::PersistedProject {
            id: "proj".into(),
            name: "Project proj".into(),
            path: "/tmp/proj".into(),
            sessions: session_ids.iter().map(|s| persisted_session(s)).collect(),
        }],
        frame: None,
    }
}

fn detached_row(id: &str) -> PersistedDetachedSession {
    PersistedDetachedSession {
        project: PersistedDetachedProject {
            id: "proj".into(),
            name: "Project proj".into(),
            path: "/tmp/proj".into(),
        },
        session: persisted_session(id),
    }
}

fn ids(pool: &DetachedPool) -> Vec<String> {
    pool.entries().iter().map(|e| e.session.id.clone()).collect()
}

// MARK: - entry ops

/// `push_front` puts the most recently detached entry at the head, and the
/// snapshot preserves that order — array order IS the pool order (§P4), which
/// is what makes the adopt-latest chord "the last one I closed".
#[gpui::test]
fn push_front_orders_most_recent_first(cx: &mut gpui::TestAppContext) {
    let pool = cx.new(|_| DetachedPool::new());
    pool.update(cx, |pool, cx| {
        pool.push_front(entry("a"), cx);
        pool.push_front(entry("b"), cx);
        pool.push_front(entry("c"), cx);
    });
    pool.read_with(cx, |pool, _| {
        assert_eq!(ids(pool), vec!["c", "b", "a"]);
        assert_eq!(pool.len(), 3);
        assert!(!pool.is_empty());
        let snapshot: Vec<String> = pool
            .snapshot()
            .iter()
            .map(|row| row.session.id.clone())
            .collect();
        assert_eq!(snapshot, vec!["c", "b", "a"], "snapshot keeps pool order");
    });
}

/// `take` removes and returns exactly the named entry, leaves the rest in
/// order, and returns `None` for an id the pool no longer holds — the "same row
/// clicked in two windows across a notify gap" case, which must be a silent
/// no-op, never a double adopt.
#[gpui::test]
fn take_removes_by_id_and_missing_is_none(cx: &mut gpui::TestAppContext) {
    let pool = cx.new(|_| DetachedPool::new());
    pool.update(cx, |pool, cx| {
        pool.push_front(entry("a"), cx);
        pool.push_front(entry("b"), cx);
        pool.push_front(entry("c"), cx);

        let taken = pool.take("b", cx).expect("b is pooled");
        assert_eq!(taken.session.id, "b");
        assert_eq!(ids(pool), vec!["c", "a"]);
        assert!(!pool.contains("b"));

        assert!(pool.take("b", cx).is_none(), "a second take finds nothing");
        assert!(pool.take("nope", cx).is_none(), "an unknown id is None");
        assert_eq!(ids(pool), vec!["c", "a"], "the failed takes changed nothing");
    });
}

/// `take_head` pops the most recently detached entry (the ⌃⌘A chord), and an
/// empty pool answers `None` rather than panicking.
#[gpui::test]
fn take_head_pops_the_most_recent(cx: &mut gpui::TestAppContext) {
    let pool = cx.new(|_| DetachedPool::new());
    pool.update(cx, |pool, cx| {
        assert!(pool.take_head(cx).is_none(), "an empty pool has no head");

        pool.push_front(entry("a"), cx);
        pool.push_front(entry("b"), cx);

        assert_eq!(pool.take_head(cx).unwrap().session.id, "b");
        assert_eq!(pool.take_head(cx).unwrap().session.id, "a");
        assert!(pool.is_empty());
        assert!(pool.take_head(cx).is_none());
    });
}

/// `kill` drops the entry (and with it the payload, which SIGHUPs the child)
/// and reports whether anything was removed.
#[gpui::test]
fn kill_drops_the_entry_and_reports(cx: &mut gpui::TestAppContext) {
    let pool = cx.new(|_| DetachedPool::new());
    pool.update(cx, |pool, cx| {
        pool.push_front(entry("a"), cx);
        pool.push_front(entry("b"), cx);

        assert!(pool.kill("a", cx));
        assert_eq!(ids(pool), vec!["b"]);
        assert!(!pool.kill("a", cx), "killing a gone entry reports false");
        assert_eq!(ids(pool), vec!["b"]);
    });
}

/// A structural entry has no live child, so `has_live` is false — which is what
/// keeps a relaunched, never-adopted pool from holding the app alive
/// window-less.
#[gpui::test]
fn structural_entries_are_not_live(cx: &mut gpui::TestAppContext) {
    let pool = cx.new(|_| DetachedPool::new());
    pool.update(cx, |pool, cx| {
        pool.push_front(entry("a"), cx);
    });
    pool.read_with(cx, |pool, app| {
        assert!(!pool.has_live(app), "an empty payload is not a live process");
        assert!(pool.entries()[0].ptys.is_structural());
    });
}

/// The pool notifies on every mutation — that is what re-renders every window's
/// Detached section without any pool-side event plumbing.
#[gpui::test]
fn every_mutation_notifies(cx: &mut gpui::TestAppContext) {
    let pool = cx.new(|_| DetachedPool::new());
    let observed = std::rc::Rc::new(std::cell::Cell::new(0usize));

    let subscription = cx.update(|cx| {
        let observed = observed.clone();
        cx.observe(&pool, move |_, _| observed.set(observed.get() + 1))
    });

    pool.update(cx, |pool, cx| pool.push_front(entry("a"), cx));
    cx.run_until_parked();
    assert_eq!(observed.get(), 1, "push notified");

    pool.update(cx, |pool, cx| {
        pool.take("a", cx);
    });
    cx.run_until_parked();
    assert_eq!(observed.get(), 2, "take notified");

    drop(subscription);
}

// MARK: - snapshot / hydrate round trip

/// A pooled entry survives snapshot → JSON → decode → hydrate with its session
/// subtree and its project provenance intact. The hydrated entry is structural:
/// processes cannot survive a quit (D2), so a persisted row comes back as a
/// re-attachable shell.
#[gpui::test]
fn round_trips_through_the_persisted_document(cx: &mut gpui::TestAppContext) {
    let pool = cx.new(|_| DetachedPool::new());
    pool.update(cx, |pool, cx| {
        let mut e = entry("a");
        e.session.claude_session_id = Some("e4f1a2b3-c0d4-4e5f-9a0b-1c2d3e4f5a6b".into());
        e.project = project("nice");
        pool.push_front(e, cx);
    });

    let state = PersistedState {
        version: CURRENT_VERSION,
        windows: Vec::new(),
        detached: pool.read_with(cx, |pool, _| pool.snapshot()),
    };
    let bytes = serde_json::to_vec(&state).unwrap();
    let decoded: PersistedState = serde_json::from_slice(&bytes).unwrap();

    let entries = hydrate_entries(&decoded);
    assert_eq!(entries.len(), 1);
    let restored = &entries[0];
    assert_eq!(restored.session.id, "a");
    assert_eq!(restored.session.title, "Session a");
    assert_eq!(restored.session.cwd, "/tmp/work");
    assert_eq!(
        restored.session.claude_session_id.as_deref(),
        Some("e4f1a2b3-c0d4-4e5f-9a0b-1c2d3e4f5a6b")
    );
    assert_eq!(restored.session.windows.len(), 1, "the pill came back");
    assert_eq!(restored.project.id, "nice");
    assert_eq!(restored.project.name, "Project nice");
    assert_eq!(restored.project.path, "/tmp/nice");
    assert!(!restored.project.is_terminals());
    assert!(
        restored.ptys.is_structural(),
        "a hydrated row has no live child"
    );
}

/// The Terminals provenance re-derives from the frozen project id — nothing
/// persists an `is_terminals` flag (§P3).
#[test]
fn terminals_provenance_re_derives_from_the_id() {
    assert!(project(WorkspaceModel::TERMINALS_PROJECT_ID).is_terminals());
    assert!(!project("nice").is_terminals());
}

// MARK: - the launch-time reconcile (review B3c)

/// A session id present in BOTH buckets keeps its attached copy and loses the
/// detached one. Without this, a hard kill inside a close/adopt debounce window
/// mints two live sessions with the same id and every id-keyed cross-window
/// lookup silently resolves to whichever window matched first.
#[test]
fn hydrate_drops_a_detached_row_that_is_also_attached() {
    let state = PersistedState {
        version: CURRENT_VERSION,
        windows: vec![persisted_window("w1", &["dup", "attached-only"])],
        detached: vec![detached_row("dup"), detached_row("pooled-only")],
    };

    let entries = hydrate_entries(&state);
    let ids: Vec<&str> = entries.iter().map(|e| e.session.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["pooled-only"],
        "windows[] wins the duplicate; the pool keeps only its own row"
    );
}

/// The reconcile scans EVERY window and every project, not just the first.
#[test]
fn hydrate_reconcile_scans_all_windows_and_projects() {
    let state = PersistedState {
        version: CURRENT_VERSION,
        windows: vec![persisted_window("w1", &["a"]), persisted_window("w2", &["b"])],
        detached: vec![detached_row("b"), detached_row("c"), detached_row("a")],
    };
    let entries = hydrate_entries(&state);
    let ids: Vec<&str> = entries.iter().map(|e| e.session.id.as_str()).collect();
    assert_eq!(ids, vec!["c"]);
}

/// A duplicate id WITHIN `detached[]` is REPAIRED, not dropped: the later row is
/// re-keyed and kept. Collapsing it would delete a whole detached session's
/// record — its cwd, its resume id, its pane tree — to enforce an invariant that
/// one mint restores, and this bucket exists precisely so sessions are not lost.
#[test]
fn hydrate_rekeys_duplicates_inside_the_bucket() {
    let mut first = detached_row("dup");
    first.session.title = "the newer one".into();
    let mut second = detached_row("dup");
    second.session.title = "the older one".into();

    let state = PersistedState {
        version: CURRENT_VERSION,
        windows: Vec::new(),
        detached: vec![first, second],
    };
    let mut n = 0;
    let entries = hydrate_entries_with(&state, &mut || {
        n += 1;
        format!("minted-{n}")
    });
    assert_eq!(entries.len(), 2, "neither session was thrown away");
    assert_eq!(entries[0].session.title, "the newer one");
    assert_eq!(entries[0].session.id, "dup", "the first row keeps the id");
    assert_eq!(entries[1].session.title, "the older one");
    assert_eq!(entries[1].session.id, "minted-1", "the clash is re-keyed");
}

/// A row carrying `terminals-main` is re-keyed on sight and NEVER dropped, even
/// when a window holds that id too. Every fresh window seeds the constant, so a
/// pooled row keeping it would collide with the Main of the next window to adopt
/// it — and two windows' Mains are DIFFERENT sessions that merely share a name,
/// so the "the attached copy wins" rule would destroy a real one here.
#[test]
fn hydrate_rekeys_a_row_that_carries_the_seeded_main_id() {
    let main = nice_model::WorkspaceModel::MAIN_TERMINAL_SESSION_ID;
    let state = PersistedState {
        version: CURRENT_VERSION,
        windows: vec![persisted_window("w1", &[main])],
        detached: vec![detached_row(main)],
    };
    let entries = hydrate_entries_with(&state, &mut || "minted-1".to_string());
    let ids: Vec<&str> = entries.iter().map(|e| e.session.id.as_str()).collect();
    assert_eq!(ids, vec!["minted-1"], "kept, under an id of its own");
}

/// The repair never mints an id some window already owns — that would resurrect
/// exactly the cross-bucket duplicate the reconcile above exists to prevent.
#[test]
fn hydrate_rekey_skips_ids_that_are_attached() {
    let state = PersistedState {
        version: CURRENT_VERSION,
        windows: vec![persisted_window("w1", &["minted-1"])],
        detached: vec![detached_row("dup"), detached_row("dup")],
    };
    let mut n = 0;
    let entries = hydrate_entries_with(&state, &mut || {
        n += 1;
        format!("minted-{n}")
    });
    let ids: Vec<&str> = entries.iter().map(|e| e.session.id.as_str()).collect();
    assert_eq!(ids, vec!["dup", "minted-2"]);
}

/// A document with no detached rows hydrates to an empty pool — the old-file
/// and never-detached cases.
#[test]
fn hydrate_of_an_empty_bucket_is_an_empty_pool() {
    let state = PersistedState {
        version: CURRENT_VERSION,
        windows: vec![persisted_window("w1", &["a"])],
        detached: Vec::new(),
    };
    assert!(hydrate_entries(&state).is_empty());
}

// MARK: - the detach-eligibility partition (§P2)

/// Build a session whose term windows carry exactly `alive` (one window per
/// entry); an empty slice is a session with no windows at all.
fn session_with_liveness(id: &str, alive: &[bool]) -> Session {
    let mut s = Session::new(id, format!("Session {id}"), "/tmp/work");
    s.windows = alive
        .iter()
        .enumerate()
        .map(|(i, live)| {
            let mut w = TermWindow::new(
                format!("{id}-w{i}"),
                format!("Terminal {i}"),
                TermWindowKind::Terminal,
            );
            w.is_alive = *live;
            w
        })
        .collect();
    s.active_window_id = s.windows.first().map(|w| w.id.clone());
    s
}

fn model_with(sessions: Vec<Session>) -> WorkspaceModel {
    let project = nice_model::Project {
        id: "proj".into(),
        name: "Project".into(),
        path: "/tmp/proj".into(),
        sessions,
    };
    WorkspaceModel::from_parts_std(vec![project], None)
}

/// The partition matrix. The row that decides the whole design is the
/// **restored-but-never-activated** session: hydration marks its windows
/// `is_alive` even though it has no pty at all, so it is ELIGIBLE and detaches
/// as a structural entry. Both behaviours would pass a matrix that omitted it —
/// and the wrong one would silently `Remove` a resumable Claude row under D1's
/// no-confirm close, narrowing today's safety net for exactly the class detach
/// exists to protect.
#[test]
fn detach_eligibility_partitions_live_restored_held_and_dead_sessions() {
    let model = model_with(vec![
        // A live session with a running pty behind it.
        session_with_liveness("live", &[true]),
        // A restored session the user never activated: model-alive, NO pty. The
        // model cannot tell it apart from `live`, which is the point — it
        // detaches structurally rather than being dropped.
        session_with_liveness("restored-never-activated", &[true]),
        // Every window HELD (process exited, view still mounted) ⇒ nothing to
        // preserve, so today's Remove fate stands.
        session_with_liveness("held", &[false, false]),
        // No windows at all.
        session_with_liveness("dead", &[]),
        // Mixed: one held pane's pill plus a live one ⇒ still eligible.
        session_with_liveness("mixed", &[false, true]),
    ]);

    assert_eq!(
        detach_eligible_session_ids(&model),
        vec![
            "live".to_string(),
            "restored-never-activated".to_string(),
            "mixed".to_string(),
        ],
        "eligibility is model liveness, in sidebar order"
    );
}

/// An empty window contributes nothing, and a window whose every session is
/// model-dead partitions to nothing — which is what makes "setting OFF ⇒ today's
/// behaviour" and "nothing to detach ⇒ today's behaviour" the same code path.
#[test]
fn detach_eligibility_is_empty_when_nothing_is_model_alive() {
    assert!(detach_eligible_session_ids(&model_with(vec![])).is_empty());
    assert!(
        detach_eligible_session_ids(&model_with(vec![
            session_with_liveness("held", &[false]),
            session_with_liveness("dead", &[]),
        ]))
        .is_empty()
    );
}
