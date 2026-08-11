//! Ported `WorkspaceModel` behavior suites from `Tests/NiceUnitTests/`. Each Swift
//! `WorkspaceModel*Tests` / `AppStateBranchTrackingTests` / `PaneNamingTests` case's
//! *semantics* is reproduced here as a Rust unit test, one behavior per test,
//! including the callback-count assertions. Where a Swift case exercises
//! `SessionsModel`/`AppState`/persistence wiring (spawn, pty, socket,
//! restore-heal, OSC routing, /branch trigger classification), only the model
//! half is ported and the deferred case is left as an `R13:`/`R15:`/`R16:`/
//! `R18:` breadcrumb.

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;

use super::*;
use crate::{PersistedTermWindow, PersistedSession};

// MARK: - Test filesystem seam

/// An in-memory [`FsProbe`]: `exists` is set-membership, `home` is fixed. Lets
/// the git-root / repair / bucketing ports be hermetic where the Swift tests
/// planted real temp dirs.
struct FakeFs {
    home: String,
    existing: HashSet<String>,
}

impl FsProbe for FakeFs {
    fn exists(&self, path: &str) -> bool {
        self.existing.contains(path)
    }
    fn home(&self) -> String {
        self.home.clone()
    }
}

fn fake_fs(home: &str, paths: &[&str]) -> Box<dyn FsProbe> {
    Box::new(FakeFs {
        home: home.to_string(),
        existing: paths.iter().map(|s| s.to_string()).collect(),
    })
}

/// A model seeded at `cwd` with a fake fs (home `/home`, the given existing
/// paths). Registers a git repo as both `<dir>` and `<dir>/.git`; a plain dir
/// as `<dir>`.
fn model_with(cwd: &str, paths: &[&str]) -> WorkspaceModel {
    WorkspaceModel::with_fs(cwd, fake_fs("/home", paths))
}

/// A model with an empty fake fs (nothing exists), home `/home`.
fn model_empty(cwd: &str) -> WorkspaceModel {
    model_with(cwd, &[])
}

// MARK: - TermWindow / session builders

fn claude(id: &str) -> TermWindow {
    TermWindow::new(id, "Claude", TermWindowKind::Claude)
}

fn terminal(id: &str, title: &str) -> TermWindow {
    TermWindow::new(id, title, TermWindowKind::Terminal)
}

/// Mirror of `TabModelFixtures.seedClaudeTab`: append a Claude + terminal session
/// under `project_id` (creating the project). Returns `(claude_window_id,
/// terminal_window_id)`. Claude window id `<session>-claude`, terminal `<session>-t1`.
fn seed_claude_session(
    model: &mut WorkspaceModel,
    project_id: &str,
    session_id: &str,
    claude_session_id: &str,
    path: &str,
    is_claude_running: bool,
) -> (String, String) {
    let claude_window_id = format!("{}-claude", session_id);
    let terminal_window_id = format!("{}-t1", session_id);
    let mut claude_window = claude(&claude_window_id);
    claude_window.is_claude_running = is_claude_running;
    let mut session = Session::new(session_id, "New session", path);
    session.windows = vec![claude_window, terminal(&terminal_window_id, "Terminal 1")];
    session.active_window_id = Some(claude_window_id.clone());
    session.claude_session_id = Some(claude_session_id.to_string());
    model.projects.push(Project {
        id: project_id.into(),
        name: project_id.to_uppercase(),
        path: path.into(),
        sessions: vec![session],
    });
    (claude_window_id, terminal_window_id)
}

/// Mirror of `TabModelFixtures.seedTerminalProject`: a bare project with a
/// single seed terminal session.
fn seed_terminal_project(model: &mut WorkspaceModel, id: &str, name: &str, path: &str) {
    let seed_session_id = format!("{}-seed", id);
    let seed_window_id = format!("{}-seed-p0", id);
    let mut session = Session::new(&seed_session_id, "seed", path);
    session.windows = vec![terminal(&seed_window_id, "zsh")];
    session.active_window_id = Some(seed_window_id);
    model.projects.push(Project {
        id: id.into(),
        name: name.into(),
        path: path.into(),
        sessions: vec![session],
    });
}

fn project_by_id<'a>(model: &'a WorkspaceModel, id: &str) -> &'a Project {
    model
        .projects
        .iter()
        .find(|p| p.id == id)
        .unwrap_or_else(|| panic!("project '{}' not found", id))
}

fn session_ids_in(model: &WorkspaceModel, project_id: &str) -> Vec<String> {
    project_by_id(model, project_id)
        .sessions
        .iter()
        .map(|s| s.id.clone())
        .collect()
}

fn window_ids(model: &WorkspaceModel, session_id: &str) -> Vec<String> {
    model
        .session_for(session_id)
        .map(|s| s.windows.iter().map(|w| w.id.clone()).collect())
        .unwrap_or_default()
}

/// Install a mutation counter and return the shared cell the callback bumps.
fn mutation_counter(model: &mut WorkspaceModel) -> Rc<Cell<u32>> {
    let counter = Rc::new(Cell::new(0u32));
    let c = counter.clone();
    model.set_on_tree_mutation(move || c.set(c.get() + 1));
    counter
}

// =====================================================================
// TabModelCwdResolutionTests
// =====================================================================

/// Seed a terminal session (one window) under project `p`, mirroring the Swift
/// `seedTerminalTab` helper.
fn seed_terminal_session(
    model: &mut WorkspaceModel,
    session_id: &str,
    window_id: &str,
    session_cwd: &str,
    window_cwd: Option<&str>,
) {
    let mut window = terminal(window_id, "zsh");
    window.cwd = window_cwd.map(|s| s.to_string());
    let mut session = Session::new(session_id, "Terminal", session_cwd);
    session.windows = vec![window];
    session.active_window_id = Some(window_id.to_string());
    model.projects.push(Project {
        id: "p".into(),
        name: "P".into(),
        path: session_cwd.into(),
        sessions: vec![session],
    });
}

#[test]
fn resolved_spawn_cwd_prefers_window_cwd_when_it_exists() {
    let dir = "/tmp/live-dir";
    let mut model = model_with("/tmp/main", &[dir]);
    seed_terminal_session(&mut model, "t1", "p1", "/tmp", Some(dir));
    let session = model.session_for("t1").unwrap().clone();
    let window = session.windows[0].clone();
    assert_eq!(model.resolved_spawn_cwd_for_window(&session, &window), dir);
}

#[test]
fn resolved_spawn_cwd_falls_back_when_window_cwd_missing() {
    let live = "/tmp/live";
    let dead = "/tmp/dead";
    // Only `live` exists on the fake fs; the window cwd (`dead`) was deleted.
    let mut model = model_with("/tmp/main", &[live]);
    seed_terminal_session(&mut model, "t1", "p1", live, Some(dead));
    let session = model.session_for("t1").unwrap().clone();
    let window = session.windows[0].clone();
    assert_eq!(
        model.resolved_spawn_cwd_for_window(&session, &window),
        live,
        "deleted window cwd must fall back to the session's cwd"
    );
}

#[test]
fn resolved_spawn_cwd_nil_window_cwd_falls_back_to_session() {
    let live = "/tmp/live";
    let mut model = model_with("/tmp/main", &[live]);
    seed_terminal_session(&mut model, "t1", "p1", live, None);
    let session = model.session_for("t1").unwrap().clone();
    let window = session.windows[0].clone();
    assert!(window.cwd.is_none());
    assert_eq!(model.resolved_spawn_cwd_for_window(&session, &window), live);
}

#[test]
fn spawn_cwd_for_new_window_caller_provided_wins() {
    let live = "/tmp/live";
    let mut model = model_with("/tmp/main", &[live]);
    seed_terminal_session(&mut model, "t1", "p1", live, Some(live));
    let session = model.session_for("t1").unwrap().clone();
    assert_eq!(
        model.spawn_cwd_for_new_window(&session, Some("/explicit")),
        "/explicit",
        "an explicit caller cwd must win over inheritance"
    );
}

#[test]
fn spawn_cwd_for_new_window_inherits_active_window_cwd() {
    let session_dir = "/tmp/session-dir";
    let window_dir = "/tmp/window-dir";
    let mut model = model_with("/tmp/main", &[session_dir, window_dir]);
    seed_terminal_session(&mut model, "t1", "p1", session_dir, Some(window_dir));
    let session = model.session_for("t1").unwrap().clone();
    assert_eq!(model.spawn_cwd_for_new_window(&session, None), window_dir);
}

#[test]
fn spawn_cwd_for_new_window_falls_back_to_session_cwd_when_no_active_window() {
    let session_dir = "/tmp/session-dir";
    let model = model_empty("/tmp/main");
    // Session with no active window and no windows — nothing to inherit.
    let session = Session::new("t1", "Terminal", session_dir);
    assert_eq!(model.spawn_cwd_for_new_window(&session, None), session_dir);
}

#[test]
fn adopt_session_cwd_unknown_session_id_returns_false_no_mutation() {
    let mut model = model_empty("/tmp/main");
    seed_claude_session(&mut model, "p", "t-known", "s", "/tmp/known", true);
    let pre = model.session_for("t-known").unwrap().clone();
    let changed = model.adopt_session_cwd("t-ghost", "/tmp/anywhere");
    assert!(!changed, "unknown session id must return false");
    assert_eq!(
        model.session_for("t-known").unwrap(),
        &pre,
        "siblings must not change when an unknown id is passed"
    );
}

#[test]
fn adopt_session_cwd_same_cwd_returns_false_windows_untouched() {
    let mut model = model_empty("/tmp/main");
    let (_c, term) = seed_claude_session(&mut model, "p", "t-same", "s", "/tmp/same", true);
    model.mutate_session("t-same", |session| {
        if let Some(w) = session.windows.iter_mut().find(|w| w.id == term) {
            w.cwd = Some("/tmp/same".into());
        }
    });
    let pre = model.session_for("t-same").unwrap().clone();
    let changed = model.adopt_session_cwd("t-same", "/tmp/same");
    assert!(!changed, "same cwd must short-circuit to false");
    assert_eq!(
        model.session_for("t-same").unwrap(),
        &pre,
        "no-op rotation must leave every window (incl. nil ones) unchanged"
    );
}

#[test]
fn adopt_session_cwd_different_cwd_returns_true_session_updated() {
    let mut model = model_empty("/tmp/main");
    seed_claude_session(&mut model, "p", "t-rotate", "s", "/tmp/before", true);
    let changed = model.adopt_session_cwd("t-rotate", "/tmp/after");
    assert!(changed, "different cwd must return true");
    assert_eq!(model.session_for("t-rotate").unwrap().cwd, "/tmp/after");
}

#[test]
fn adopt_session_cwd_window_policy_matching_follows() {
    let mut model = model_empty("/tmp/main");
    let (_c, term) = seed_claude_session(&mut model, "p", "t-match", "s", "/tmp/old", true);
    model.mutate_session("t-match", |session| {
        if let Some(w) = session.windows.iter_mut().find(|w| w.id == term) {
            w.cwd = Some("/tmp/old".into());
        }
    });
    assert!(model.adopt_session_cwd("t-match", "/tmp/new"));
    let window_cwd = model
        .session_for("t-match")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == term)
        .unwrap()
        .cwd
        .clone();
    assert_eq!(
        window_cwd.as_deref(),
        Some("/tmp/new"),
        "window that matched the old session.cwd must follow into new_cwd"
    );
}

#[test]
fn adopt_session_cwd_window_policy_nil_follows() {
    let mut model = model_empty("/tmp/main");
    let (claude_window, _t) = seed_claude_session(&mut model, "p", "t-nil", "s", "/tmp/old", true);
    assert!(
        model
            .session_for("t-nil")
            .unwrap()
            .windows
            .iter()
            .find(|w| w.id == claude_window)
            .unwrap()
            .cwd
            .is_none(),
        "precondition: Claude window starts with nil cwd"
    );
    assert!(model.adopt_session_cwd("t-nil", "/tmp/new"));
    let window_cwd = model
        .session_for("t-nil")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == claude_window)
        .unwrap()
        .cwd
        .clone();
    assert_eq!(
        window_cwd.as_deref(),
        Some("/tmp/new"),
        "nil-cwd window must follow the session into new_cwd"
    );
}

#[test]
fn adopt_session_cwd_window_policy_diverged_stays() {
    let mut model = model_empty("/tmp/main");
    let (_c, term) = seed_claude_session(&mut model, "p", "t-div", "s", "/tmp/old", true);
    model.mutate_session("t-div", |session| {
        if let Some(w) = session.windows.iter_mut().find(|w| w.id == term) {
            w.cwd = Some("/tmp/somewhere-else".into());
        }
    });
    assert!(model.adopt_session_cwd("t-div", "/tmp/new"));
    let window_cwd = model
        .session_for("t-div")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == term)
        .unwrap()
        .cwd
        .clone();
    assert_eq!(
        window_cwd.as_deref(),
        Some("/tmp/somewhere-else"),
        "diverged window must keep its user-chosen cwd across the rotation"
    );
}

#[test]
fn adopt_session_cwd_mixed_windows_applies_policy_per_window() {
    let mut model = model_empty("/tmp/main");
    let (claude_window, term) = seed_claude_session(&mut model, "p", "t-mixed", "s", "/tmp/old", true);
    let extra = "t-mixed-t2".to_string();
    model.mutate_session("t-mixed", |session| {
        // Claude window stays nil (nil follows). Terminal window at /tmp/old
        // (matching-old follows).
        if let Some(w) = session.windows.iter_mut().find(|w| w.id == term) {
            w.cwd = Some("/tmp/old".into());
        }
        let mut diverged = terminal(&extra, "Terminal 2");
        diverged.cwd = Some("/tmp/diverged".into());
        session.windows.push(diverged);
    });
    assert!(model.adopt_session_cwd("t-mixed", "/tmp/new"));
    let windows = model.session_for("t-mixed").unwrap().windows.clone();
    let cwd_of = |id: &str| {
        windows
            .iter()
            .find(|w| w.id == id)
            .unwrap()
            .cwd
            .clone()
            .unwrap_or_default()
    };
    assert_eq!(cwd_of(&claude_window), "/tmp/new", "nil window must follow");
    assert_eq!(cwd_of(&term), "/tmp/new", "matching-old window must follow");
    assert_eq!(
        cwd_of("t-mixed-t2"),
        "/tmp/diverged",
        "diverged window must stay put — window policy is per-window, not all-or-nothing"
    );
}

// =====================================================================
// TabModelInsertExtractPaneTests
// =====================================================================

/// Seed the [p0, p1, p2] terminal-window fixture into a fresh project `ie`
/// alongside the pinned Terminals group.
fn ie_model() -> WorkspaceModel {
    let mut model = model_empty("/tmp/main");
    let mut session = Session::new("ie-session", "Insert/extract test", "/tmp/ie");
    session.windows = vec![
        terminal("ie-session-p0", "Terminal 1"),
        terminal("ie-session-p1", "Terminal 2"),
        terminal("ie-session-p2", "Terminal 3"),
    ];
    session.active_window_id = Some("ie-session-p0".into());
    model.projects.push(Project {
        id: "ie".into(),
        name: "IE".into(),
        path: "/tmp/ie".into(),
        sessions: vec![session],
    });
    model
}

#[test]
fn extract_window_removes_and_returns_window() {
    let mut model = ie_model();
    let removed = model.extract_window("ie-session-p1", "ie-session");
    assert_eq!(removed.map(|w| w.id), Some("ie-session-p1".to_string()));
    assert_eq!(window_ids(&model, "ie-session"), ["ie-session-p0", "ie-session-p2"]);
}

#[test]
fn extract_window_non_active_leaves_active_unchanged() {
    let mut model = ie_model();
    model.extract_window("ie-session-p1", "ie-session");
    assert_eq!(
        model.session_for("ie-session").unwrap().active_window_id.as_deref(),
        Some("ie-session-p0")
    );
}

#[test]
fn extract_window_active_refocuses_slot_neighbor() {
    let mut model = ie_model();
    model.mutate_session("ie-session", |s| s.active_window_id = Some("ie-session-p1".into()));
    model.extract_window("ie-session-p1", "ie-session");
    assert_eq!(
        model.session_for("ie-session").unwrap().active_window_id.as_deref(),
        Some("ie-session-p2"),
        "removing the middle active window focuses the window that slid into its slot"
    );
}

#[test]
fn extract_window_active_last_refocuses_previous() {
    let mut model = ie_model();
    model.mutate_session("ie-session", |s| s.active_window_id = Some("ie-session-p2".into()));
    model.extract_window("ie-session-p2", "ie-session");
    assert_eq!(
        model.session_for("ie-session").unwrap().active_window_id.as_deref(),
        Some("ie-session-p1")
    );
}

#[test]
fn extract_window_last_remaining_clears_active() {
    let mut model = ie_model();
    model.extract_window("ie-session-p1", "ie-session");
    model.extract_window("ie-session-p2", "ie-session");
    model.mutate_session("ie-session", |s| s.active_window_id = Some("ie-session-p0".into()));
    model.extract_window("ie-session-p0", "ie-session");
    assert!(window_ids(&model, "ie-session").is_empty());
    assert!(model.session_for("ie-session").unwrap().active_window_id.is_none());
}

#[test]
fn extract_window_unknown_window_returns_nil_no_mutation() {
    let mut model = ie_model();
    let counter = mutation_counter(&mut model);
    let removed = model.extract_window("ghost", "ie-session");
    assert!(removed.is_none());
    assert_eq!(window_ids(&model, "ie-session"), ["ie-session-p0", "ie-session-p1", "ie-session-p2"]);
    assert_eq!(counter.get(), 0);
}

#[test]
fn extract_window_real_removal_fires_on_tree_mutation_once() {
    let mut model = ie_model();
    let counter = mutation_counter(&mut model);
    model.extract_window("ie-session-p1", "ie-session");
    assert_eq!(counter.get(), 1);
}

#[test]
fn insert_window_before_target() {
    let mut model = ie_model();
    model.insert_window(terminal("fx", "Foreign"), "ie-session", Some("ie-session-p1"), false);
    assert_eq!(
        window_ids(&model, "ie-session"),
        ["ie-session-p0", "fx", "ie-session-p1", "ie-session-p2"]
    );
}

#[test]
fn insert_window_after_target() {
    let mut model = ie_model();
    model.insert_window(terminal("fx", "Foreign"), "ie-session", Some("ie-session-p1"), true);
    assert_eq!(
        window_ids(&model, "ie-session"),
        ["ie-session-p0", "ie-session-p1", "fx", "ie-session-p2"]
    );
}

#[test]
fn insert_window_nil_target_appends() {
    let mut model = ie_model();
    model.insert_window(terminal("fx", "Foreign"), "ie-session", None, false);
    assert_eq!(
        window_ids(&model, "ie-session"),
        ["ie-session-p0", "ie-session-p1", "ie-session-p2", "fx"]
    );
}

#[test]
fn insert_window_unknown_target_appends() {
    let mut model = ie_model();
    model.insert_window(terminal("fx", "Foreign"), "ie-session", Some("ghost"), true);
    assert_eq!(
        window_ids(&model, "ie-session"),
        ["ie-session-p0", "ie-session-p1", "ie-session-p2", "fx"]
    );
}

#[test]
fn insert_window_duplicate_id_is_no_op() {
    let mut model = ie_model();
    let counter = mutation_counter(&mut model);
    model.insert_window(terminal("ie-session-p1", "Dup"), "ie-session", Some("ie-session-p0"), true);
    assert_eq!(window_ids(&model, "ie-session"), ["ie-session-p0", "ie-session-p1", "ie-session-p2"]);
    assert_eq!(counter.get(), 0);
}

#[test]
fn insert_window_does_not_change_active_window_id() {
    let mut model = ie_model();
    model.insert_window(terminal("fx", "Foreign"), "ie-session", Some("ie-session-p1"), false);
    assert_eq!(
        model.session_for("ie-session").unwrap().active_window_id.as_deref(),
        Some("ie-session-p0")
    );
}

#[test]
fn insert_window_real_insert_fires_on_tree_mutation_once() {
    let mut model = ie_model();
    let counter = mutation_counter(&mut model);
    model.insert_window(terminal("fx", "Foreign"), "ie-session", Some("ie-session-p1"), false);
    assert_eq!(counter.get(), 1);
}

#[test]
fn ensure_project_by_path_matches_existing_by_path() {
    let mut model = ie_model();
    let idx = model.ensure_project_by_path("different-id", "Different", "/tmp/ie");
    assert_eq!(idx, 1, "matched the seeded project at index 1, not appended");
    assert_eq!(model.projects[idx].id, "ie");
    assert_eq!(model.projects.len(), 2);
}

#[test]
fn ensure_project_by_path_recreates_when_absent_copying_identity() {
    let mut model = ie_model();
    let before = model.projects.len();
    let idx = model.ensure_project_by_path("p-new", "NEW", "/tmp/brand-new");
    assert_eq!(model.projects.len(), before + 1);
    assert_eq!(model.projects[idx].id, "p-new");
    assert_eq!(model.projects[idx].name, "NEW");
    assert_eq!(model.projects[idx].path, "/tmp/brand-new");
}

#[test]
fn ensure_project_by_path_ignores_terminals_project() {
    let mut model = ie_model();
    let terminals_path = model.projects[0].path.clone();
    let idx = model.ensure_project_by_path("p-x", "X", &terminals_path);
    assert_ne!(idx, 0, "must never match the pinned Terminals project by path");
    assert_eq!(model.projects[idx].id, "p-x");
}

#[test]
fn ensure_project_by_path_never_duplicates_terminals_project() {
    let mut model = ie_model();
    let before = model.projects.len();
    let idx = model.ensure_project_by_path(WorkspaceModel::TERMINALS_PROJECT_ID, "Terminals", "/some/other/path");
    assert_eq!(idx, 0, "reserved Terminals id resolves to the pinned project at index 0");
    assert_eq!(model.projects.len(), before, "must not append a duplicate Terminals project");
    assert_eq!(
        model
            .projects
            .iter()
            .filter(|p| p.id == WorkspaceModel::TERMINALS_PROJECT_ID)
            .count(),
        1
    );
}

// =====================================================================
// TabModelMovePaneTests
// =====================================================================

/// Seed the [p0, p1, p2] fixture into project `mp`.
fn mp_model() -> WorkspaceModel {
    let mut model = model_empty("/tmp/main");
    let mut session = Session::new("mp-session", "Move-window test", "/tmp/mp");
    session.windows = vec![
        terminal("mp-session-p0", "Terminal 1"),
        terminal("mp-session-p1", "Terminal 2"),
        terminal("mp-session-p2", "Terminal 3"),
    ];
    session.active_window_id = Some("mp-session-p0".into());
    model.projects.push(Project {
        id: "mp".into(),
        name: "MP".into(),
        path: "/tmp/mp".into(),
        sessions: vec![session],
    });
    model
}

fn mp_window_ids(model: &WorkspaceModel) -> Vec<String> {
    window_ids(model, "mp-session")
}

#[test]
fn move_window_before_moves_source_into_target_slot() {
    let mut model = mp_model();
    model.move_window("mp-session-p2", "mp-session", "mp-session-p0", false);
    assert_eq!(mp_window_ids(&model), ["mp-session-p2", "mp-session-p0", "mp-session-p1"]);
}

#[test]
fn move_window_after_lands_just_past_target() {
    let mut model = mp_model();
    model.move_window("mp-session-p0", "mp-session", "mp-session-p1", true);
    assert_eq!(mp_window_ids(&model), ["mp-session-p1", "mp-session-p0", "mp-session-p2"]);
}

#[test]
fn move_window_after_last_window_moves_to_end() {
    let mut model = mp_model();
    model.move_window("mp-session-p0", "mp-session", "mp-session-p2", true);
    assert_eq!(mp_window_ids(&model), ["mp-session-p1", "mp-session-p2", "mp-session-p0"]);
}

#[test]
fn move_window_remove_shifts_insert_boundary_lands_correctly() {
    let mut model = mp_model();
    // src=0, dst=1, placeAfter → insertIndex 2 before shift; src<insert so 1.
    model.move_window("mp-session-p0", "mp-session", "mp-session-p1", true);
    assert_eq!(mp_window_ids(&model), ["mp-session-p1", "mp-session-p0", "mp-session-p2"]);
}

#[test]
fn move_window_same_id_is_no_op() {
    let mut model = mp_model();
    let before = mp_window_ids(&model);
    model.move_window("mp-session-p0", "mp-session", "mp-session-p0", false);
    assert_eq!(mp_window_ids(&model), before);
}

#[test]
fn move_window_adjacent_after_predecessor_is_no_op() {
    let mut model = mp_model();
    let before = mp_window_ids(&model);
    model.move_window("mp-session-p1", "mp-session", "mp-session-p0", true);
    assert_eq!(mp_window_ids(&model), before);
}

#[test]
fn move_window_adjacent_before_successor_is_no_op() {
    let mut model = mp_model();
    let before = mp_window_ids(&model);
    model.move_window("mp-session-p0", "mp-session", "mp-session-p1", false);
    assert_eq!(mp_window_ids(&model), before);
}

#[test]
fn move_window_unknown_window_id_is_no_op() {
    let mut model = mp_model();
    let before = mp_window_ids(&model);
    model.move_window("ghost", "mp-session", "mp-session-p0", true);
    assert_eq!(mp_window_ids(&model), before);
}

#[test]
fn move_window_unknown_target_id_is_no_op() {
    let mut model = mp_model();
    let before = mp_window_ids(&model);
    model.move_window("mp-session-p0", "mp-session", "ghost", false);
    assert_eq!(mp_window_ids(&model), before);
}

#[test]
fn move_window_unknown_session_id_is_no_op() {
    let mut model = mp_model();
    let before = mp_window_ids(&model);
    model.move_window("mp-session-p0", "ghost-session", "mp-session-p1", false);
    assert_eq!(mp_window_ids(&model), before);
}

#[test]
fn move_window_real_move_fires_on_tree_mutation_once() {
    let mut model = mp_model();
    let counter = mutation_counter(&mut model);
    model.move_window("mp-session-p0", "mp-session", "mp-session-p2", true);
    assert_eq!(counter.get(), 1, "a real reorder fires exactly once");
}

#[test]
fn move_window_same_id_does_not_fire() {
    let mut model = mp_model();
    let counter = mutation_counter(&mut model);
    model.move_window("mp-session-p0", "mp-session", "mp-session-p0", false);
    assert_eq!(counter.get(), 0);
}

#[test]
fn move_window_adjacent_no_op_does_not_fire() {
    let mut model = mp_model();
    let counter = mutation_counter(&mut model);
    model.move_window("mp-session-p1", "mp-session", "mp-session-p0", true);
    assert_eq!(counter.get(), 0);
}

#[test]
fn move_window_unknown_window_id_does_not_fire() {
    let mut model = mp_model();
    let counter = mutation_counter(&mut model);
    model.move_window("ghost", "mp-session", "mp-session-p0", true);
    assert_eq!(counter.get(), 0);
}

#[test]
fn move_window_unknown_session_id_does_not_fire() {
    let mut model = mp_model();
    let counter = mutation_counter(&mut model);
    model.move_window("mp-session-p0", "ghost-session", "mp-session-p1", false);
    assert_eq!(counter.get(), 0);
}

#[test]
fn move_window_does_not_change_active_window_id() {
    let mut model = mp_model();
    let before = model.session_for("mp-session").unwrap().active_window_id.clone();
    model.move_window("mp-session-p0", "mp-session", "mp-session-p2", true);
    assert_eq!(model.session_for("mp-session").unwrap().active_window_id, before);
}

#[test]
fn would_move_window_real_move_is_true() {
    let model = mp_model();
    assert!(model.would_move_window("mp-session-p2", "mp-session", "mp-session-p0", false));
}

#[test]
fn would_move_window_same_id_is_false() {
    let model = mp_model();
    assert!(!model.would_move_window("mp-session-p0", "mp-session", "mp-session-p0", false));
}

#[test]
fn would_move_window_adjacent_no_op_is_false() {
    let model = mp_model();
    assert!(!model.would_move_window("mp-session-p1", "mp-session", "mp-session-p0", true));
    assert!(!model.would_move_window("mp-session-p1", "mp-session", "mp-session-p2", false));
}

#[test]
fn would_move_window_unknown_window_id_is_false() {
    let model = mp_model();
    assert!(!model.would_move_window("ghost", "mp-session", "mp-session-p0", true));
}

#[test]
fn would_move_window_unknown_target_id_is_false() {
    let model = mp_model();
    assert!(!model.would_move_window("mp-session-p0", "mp-session", "ghost", false));
}

#[test]
fn would_move_window_unknown_session_id_is_false() {
    let model = mp_model();
    assert!(!model.would_move_window("mp-session-p0", "ghost-session", "mp-session-p1", true));
}

// =====================================================================
// TabModelReorderTests
// =====================================================================

/// Two projects, 3 and 2 sessions, each session one terminal window. Replaces the whole
/// projects array (no Terminals), mirroring the Swift `seedTwoProjects`.
fn reorder_two_projects() -> WorkspaceModel {
    let mut model = model_empty("/tmp/main");
    model.projects = vec![
        make_project("p1", "P1", 3),
        make_project("p2", "P2", 2),
    ];
    model
}

fn make_project(id: &str, name: &str, session_count: usize) -> Project {
    let sessions = (0..session_count)
        .map(|i| {
            let tid = format!("{}t{}", id, i);
            let pid = format!("{}t{}-p0", id, i);
            let mut session = Session::new(&tid, format!("{}-T{}", name, i), format!("/tmp/{}", id));
            session.windows = vec![terminal(&pid, "zsh")];
            session.active_window_id = Some(pid);
            session
        })
        .collect();
    Project {
        id: id.into(),
        name: name.into(),
        path: format!("/tmp/{}", id),
        sessions,
    }
}

#[test]
fn move_session_before_moves_source_into_target_slot() {
    let mut model = reorder_two_projects();
    model.move_session("p1t2", "p1t0", false);
    assert_eq!(session_ids_in(&model, "p1"), ["p1t2", "p1t0", "p1t1"]);
}

#[test]
fn move_session_after_lands_just_past_target() {
    let mut model = reorder_two_projects();
    model.move_session("p1t0", "p1t1", true);
    assert_eq!(session_ids_in(&model, "p1"), ["p1t1", "p1t0", "p1t2"]);
}

#[test]
fn move_session_after_last_session_moves_to_end() {
    let mut model = reorder_two_projects();
    model.move_session("p1t0", "p1t2", true);
    assert_eq!(session_ids_in(&model, "p1"), ["p1t1", "p1t2", "p1t0"]);
}

#[test]
fn move_session_adjacent_after_predecessor_is_no_op() {
    let mut model = reorder_two_projects();
    let before = session_ids_in(&model, "p1");
    model.move_session("p1t1", "p1t0", true);
    assert_eq!(session_ids_in(&model, "p1"), before);
}

#[test]
fn move_session_adjacent_before_successor_is_no_op() {
    let mut model = reorder_two_projects();
    let before = session_ids_in(&model, "p1");
    model.move_session("p1t0", "p1t1", false);
    assert_eq!(session_ids_in(&model, "p1"), before);
}

#[test]
fn move_session_same_id_is_no_op() {
    let mut model = reorder_two_projects();
    let before = session_ids_in(&model, "p1");
    model.move_session("p1t0", "p1t0", false);
    assert_eq!(session_ids_in(&model, "p1"), before);
}

#[test]
fn move_session_across_projects_is_no_op() {
    let mut model = reorder_two_projects();
    let p1_before = session_ids_in(&model, "p1");
    let p2_before = session_ids_in(&model, "p2");
    model.move_session("p1t0", "p2t0", false);
    assert_eq!(session_ids_in(&model, "p1"), p1_before);
    assert_eq!(session_ids_in(&model, "p2"), p2_before);
}

#[test]
fn move_session_unknown_source_is_no_op() {
    let mut model = reorder_two_projects();
    let before = session_ids_in(&model, "p1");
    model.move_session("ghost", "p1t0", true);
    assert_eq!(session_ids_in(&model, "p1"), before);
}

#[test]
fn move_session_unknown_target_is_no_op() {
    let mut model = reorder_two_projects();
    let before = session_ids_in(&model, "p1");
    model.move_session("p1t0", "ghost", false);
    assert_eq!(session_ids_in(&model, "p1"), before);
}

#[test]
fn would_move_session_real_move_is_true() {
    let model = reorder_two_projects();
    assert!(model.would_move_session("p1t2", "p1t0", false));
}

#[test]
fn would_move_session_same_id_is_false() {
    let model = reorder_two_projects();
    assert!(!model.would_move_session("p1t0", "p1t0", false));
}

#[test]
fn would_move_session_adjacent_no_op_is_false() {
    let model = reorder_two_projects();
    assert!(!model.would_move_session("p1t1", "p1t0", true));
    assert!(!model.would_move_session("p1t1", "p1t2", false));
}

#[test]
fn would_move_session_cross_project_is_false() {
    let model = reorder_two_projects();
    assert!(!model.would_move_session("p1t0", "p2t0", false));
}

#[test]
fn move_session_within_terminals_project_reorders() {
    // Terminals with [Main, term-t1, term-t2] + one user project.
    let mut model = model_empty("/tmp/main");
    let mut terminals = Project {
        id: WorkspaceModel::TERMINALS_PROJECT_ID.into(),
        name: "Terminals".into(),
        path: "/tmp/terminals".into(),
        sessions: vec![],
    };
    for (tid, title) in [
        (WorkspaceModel::MAIN_TERMINAL_SESSION_ID, "Main"),
        ("term-t1", "Term 1"),
        ("term-t2", "Term 2"),
    ] {
        let pid = format!("{}-p0", tid);
        let mut session = Session::new(tid, title, "/tmp/terminals");
        session.windows = vec![terminal(&pid, "zsh")];
        session.active_window_id = Some(pid);
        terminals.sessions.push(session);
    }
    model.projects = vec![terminals, make_project("p1", "P1", 2)];

    model.move_session("term-t2", WorkspaceModel::MAIN_TERMINAL_SESSION_ID, false);
    assert_eq!(
        session_ids_in(&model, WorkspaceModel::TERMINALS_PROJECT_ID),
        ["term-t2", WorkspaceModel::MAIN_TERMINAL_SESSION_ID, "term-t1"]
    );
}

#[test]
fn move_session_terminals_to_user_project_is_no_op() {
    let mut model = model_empty("/tmp/main");
    let mut terminals = Project {
        id: WorkspaceModel::TERMINALS_PROJECT_ID.into(),
        name: "Terminals".into(),
        path: "/tmp/terminals".into(),
        sessions: vec![],
    };
    for (tid, title) in [
        (WorkspaceModel::MAIN_TERMINAL_SESSION_ID, "Main"),
        ("term-t1", "Term 1"),
    ] {
        let pid = format!("{}-p0", tid);
        let mut session = Session::new(tid, title, "/tmp/terminals");
        session.windows = vec![terminal(&pid, "zsh")];
        session.active_window_id = Some(pid);
        terminals.sessions.push(session);
    }
    model.projects = vec![terminals, make_project("p1", "P1", 2)];

    let term_before = session_ids_in(&model, WorkspaceModel::TERMINALS_PROJECT_ID);
    let p1_before = session_ids_in(&model, "p1");
    model.move_session(WorkspaceModel::MAIN_TERMINAL_SESSION_ID, "p1t0", true);
    assert_eq!(session_ids_in(&model, WorkspaceModel::TERMINALS_PROJECT_ID), term_before);
    assert_eq!(session_ids_in(&model, "p1"), p1_before);
}

// =====================================================================
// TabModelSubtreeReorderTests (M7.8 round 3 — parent drags move the block)
// =====================================================================

/// One project shaped like the repro tree:
/// `[A, A1*, A2*, B, B1*, C]` where `*` marks a depth-1 child (parent in
/// brackets): A1/A2 under A, B1 under B, C standalone.
fn lineage_project_model() -> WorkspaceModel {
    let mut model = model_empty("/tmp/main");
    let mut project = make_project("p1", "P1", 0);
    for (tid, parent) in [
        ("A", None),
        ("A1", Some("A")),
        ("A2", Some("A")),
        ("B", None),
        ("B1", Some("B")),
        ("C", None),
    ] {
        let pid = format!("{}-p0", tid);
        let mut session = Session::new(tid, tid, "/tmp/p1");
        session.windows = vec![terminal(&pid, "zsh")];
        session.active_window_id = Some(pid);
        session.parent_session_id = parent.map(str::to_string);
        project.sessions.push(session);
    }
    model.projects = vec![project];
    model
}

#[test]
fn move_parent_after_block_carries_whole_subtree() {
    let mut model = lineage_project_model();
    // Drop A after C's block (C is standalone): the whole A block moves,
    // children in order, still nested.
    model.move_session("A", "C", true);
    assert_eq!(session_ids_in(&model, "p1"), ["B", "B1", "C", "A", "A1", "A2"]);
    assert_eq!(model.session_for("A1").unwrap().parent_session_id.as_deref(), Some("A"));
    assert_eq!(model.session_for("A2").unwrap().parent_session_id.as_deref(), Some("A"));
}

#[test]
fn move_parent_before_block_carries_whole_subtree() {
    let mut model = lineage_project_model();
    // Drop B's block before A's block.
    model.move_session("B", "A", false);
    assert_eq!(session_ids_in(&model, "p1"), ["B", "B1", "A", "A1", "A2", "C"]);
}

#[test]
fn move_parent_onto_own_child_is_no_op() {
    let mut model = lineage_project_model();
    let before = session_ids_in(&model, "p1");
    for (target, after) in [("A1", false), ("A1", true), ("A2", false), ("A2", true)] {
        model.move_session("A", target, after);
        assert_eq!(session_ids_in(&model, "p1"), before, "A onto {target}/{after}");
        assert!(!model.would_move_session("A", target, after));
    }
}

#[test]
fn move_parent_just_after_target_root_lands_after_its_block() {
    let mut model = lineage_project_model();
    // "Just after B's row" is an interior slot (between B and B1) —
    // normalizes to after B's whole block, keeping the block contiguous.
    model.move_session("C", "B", true);
    assert_eq!(session_ids_in(&model, "p1"), ["A", "A1", "A2", "B", "B1", "C"]);
    // ... which here is exactly where C already is: a no-op.
    assert!(!model.would_move_session("C", "B", true));
}

#[test]
fn move_root_targeting_foreign_child_lands_after_that_block() {
    let mut model = lineage_project_model();
    // A slot naming A's child row normalizes to after A's whole block —
    // a top-level session can never interleave into a group.
    model.move_session("C", "A1", false);
    assert_eq!(session_ids_in(&model, "p1"), ["A", "A1", "A2", "C", "B", "B1"]);
}

#[test]
fn move_childless_root_between_blocks_still_works() {
    let mut model = lineage_project_model();
    model.move_session("C", "A", false);
    assert_eq!(session_ids_in(&model, "p1"), ["C", "A", "A1", "A2", "B", "B1"]);
}

#[test]
fn move_parent_gathers_scattered_children() {
    let mut model = lineage_project_model();
    // Corrupt the order the way the old single-row move could: A stranded at
    // the end, children left at the top.
    let sessions = &mut model.projects[0].sessions;
    let a = sessions.remove(0);
    sessions.push(a); // [A1, A2, B, B1, C, A]
    // Any real move of A re-gathers the block contiguously.
    model.move_session("A", "B", false);
    assert_eq!(session_ids_in(&model, "p1"), ["A", "A1", "A2", "B", "B1", "C"]);
}

#[test]
fn move_child_reorders_among_siblings_only() {
    let mut model = lineage_project_model();
    // A2 before A1 — legal sibling reorder.
    model.move_session("A2", "A1", false);
    assert_eq!(session_ids_in(&model, "p1"), ["A", "A2", "A1", "B", "B1", "C"]);
    // Parent row with place_after == true is the top-of-run slot.
    model.move_session("A1", "A", true);
    assert_eq!(session_ids_in(&model, "p1"), ["A", "A1", "A2", "B", "B1", "C"]);
}

#[test]
fn move_child_outside_its_block_is_rejected() {
    let mut model = lineage_project_model();
    let before = session_ids_in(&model, "p1");
    for (target, after) in [
        ("A", false),  // before its own parent
        ("B", false),  // another block's root
        ("B", true),   // inside another block
        ("B1", true),  // another block's child
        ("C", true),   // a standalone root
    ] {
        model.move_session("A1", target, after);
        assert_eq!(session_ids_in(&model, "p1"), before, "A1 onto {target}/{after}");
        assert!(!model.would_move_session("A1", target, after));
    }
}

#[test]
fn subtree_move_fires_one_mutation_and_no_op_fires_none() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let mut model = lineage_project_model();
    let count = Rc::new(RefCell::new(0));
    let c = Rc::clone(&count);
    model.set_on_tree_mutation(move || *c.borrow_mut() += 1);
    model.move_session("A", "C", true);
    assert_eq!(*count.borrow(), 1, "real block move fires exactly once");
    model.move_session("A", "A1", false); // own-subtree drop: no-op
    model.move_session("A1", "C", true); // child leaving its block: rejected
    assert_eq!(*count.borrow(), 1, "illegal/no-op drops fire no event");
}

#[test]
fn would_move_parent_matches_move_semantics() {
    let model = lineage_project_model();
    assert!(model.would_move_session("A", "C", true));
    assert!(model.would_move_session("B", "A", false));
    // A after B's block == A back in front of C — a real move.
    assert!(model.would_move_session("A", "B", true));
    // B before its own current successor's block boundary — no order change.
    assert!(!model.would_move_session("B", "A", true));
    assert!(!model.would_move_session("B", "A1", false));
}

// =====================================================================
// TabModelNavigationTests (session half)
// =====================================================================

/// Two projects alongside the pinned Terminals group (kept from the seed),
/// two sessions each. navigable = [Main, p1t0, p1t1, p2t0, p2t1].
fn nav_two_projects() -> WorkspaceModel {
    let mut model = model_empty("/tmp/main");
    model.projects.push(make_project("p1", "P1", 2));
    model.projects.push(make_project("p2", "P2", 2));
    model
}

#[test]
fn navigable_sidebar_session_ids_terminals_always_first() {
    let model = model_empty("/tmp/main");
    assert_eq!(
        model.navigable_sidebar_session_ids(),
        [WorkspaceModel::MAIN_TERMINAL_SESSION_ID]
    );
}

#[test]
fn session_id_owning_resolves_by_window_across_projects_and_is_scoped() {
    // A window's owning session is found by scanning every project's window lists — even
    // when the owner is not the first project — and window ids are a distinct
    // namespace from session ids, so a session-id-shaped query never matches a window.
    let mut model = model_empty("/tmp/main");
    seed_claude_session(&mut model, "p1", "t1", "S1", "/tmp/p1", true);
    seed_claude_session(&mut model, "p2", "t2", "S2", "/tmp/p2", true);
    seed_claude_session(&mut model, "p3", "t3", "S3", "/tmp/p3", true);

    // Resolves the middle project's claude window (reverse scan hits a non-first
    // project) and its companion terminal window.
    assert_eq!(model.session_id_owning("t2-claude").as_deref(), Some("t2"));
    assert_eq!(model.session_id_owning("t2-t1").as_deref(), Some("t2"));
    // The pinned Terminals group is scanned too — its Main window resolves.
    let main_window = model
        .session_for(WorkspaceModel::MAIN_TERMINAL_SESSION_ID)
        .unwrap()
        .windows[0]
        .id
        .clone();
    assert_eq!(
        model.session_id_owning(&main_window).as_deref(),
        Some(WorkspaceModel::MAIN_TERMINAL_SESSION_ID)
    );
    // A session id is not a window id — passing one must not match any window list.
    assert_eq!(model.session_id_owning("t1"), None);
    // An entirely unknown window id (stale / from another window) is None.
    assert_eq!(model.session_id_owning("definitely-not-a-window"), None);
}

#[test]
fn next_sidebar_session_is_no_op_when_only_main_terminal_exists() {
    let mut model = model_empty("/tmp/main");
    model.select_session(WorkspaceModel::MAIN_TERMINAL_SESSION_ID);
    model.select_next_sidebar_session();
    assert_eq!(model.active_session_id(), Some(WorkspaceModel::MAIN_TERMINAL_SESSION_ID));
    model.select_prev_sidebar_session();
    assert_eq!(model.active_session_id(), Some(WorkspaceModel::MAIN_TERMINAL_SESSION_ID));
}

#[test]
fn next_sidebar_session_cycles_through_visible_sessions() {
    let mut model = nav_two_projects();
    let ids = model.navigable_sidebar_session_ids();
    assert_eq!(ids.len(), 5, "Main + (P1: T0,T1) + (P2: T0,T1)");
    model.select_session(&ids[0]);
    for expected in ids.iter().skip(1) {
        model.select_next_sidebar_session();
        assert_eq!(model.active_session_id(), Some(expected.as_str()));
    }
    model.select_next_sidebar_session();
    assert_eq!(model.active_session_id(), Some(ids[0].as_str()));
}

#[test]
fn prev_sidebar_session_cycles_backward() {
    let mut model = nav_two_projects();
    let ids = model.navigable_sidebar_session_ids();
    model.select_session(&ids[0]);
    model.select_prev_sidebar_session();
    assert_eq!(model.active_session_id(), Some(ids.last().unwrap().as_str()));
    model.select_prev_sidebar_session();
    assert_eq!(model.active_session_id(), Some(ids[ids.len() - 2].as_str()));
}

// =====================================================================
// Selection side effects (the `active_session_id` didSet — TabModel.swift:43-53)
//
// `select_session` carries the two ported `didSet` side effects: dismiss the
// waiting pulse on the target session's active window, and fire the did-mutate
// signal exactly once — but only when the id actually changes. These pin the
// wiring through `select_session`; `TermWindow::mark_acknowledged_if_waiting` itself is
// unit-tested in window.rs.
// =====================================================================

#[test]
fn select_session_acknowledges_waiting_on_target_active_window() {
    // A session whose active window (a Claude window) sits in unacknowledged Waiting:
    // selecting the session is the user looking at it, so the pulse must be
    // dismissed (the `acknowledge_waiting_on_active_window` didSet side effect).
    let mut model = model_empty("/tmp/main");
    let (claude_window_id, _term) =
        seed_claude_session(&mut model, "p", "ct", "sess", "/tmp/p", true);
    // Drive the Claude window into unacknowledged Waiting while the user is on
    // the seed's Main session (i.e. not viewing this window).
    model.mutate_session("ct", |session| {
        let claude = session
            .windows
            .iter_mut()
            .find(|w| w.id == claude_window_id)
            .unwrap();
        claude.apply_status_transition(crate::SessionStatus::Waiting, false);
    });
    assert!(
        !model.session_for("ct").unwrap().active_window().unwrap().waiting_acknowledged,
        "precondition: the target's active window is waiting and unacknowledged"
    );

    model.select_session("ct");

    assert!(
        model.session_for("ct").unwrap().active_window().unwrap().waiting_acknowledged,
        "selecting a session must acknowledge the waiting pulse on its active window"
    );
}

#[test]
fn select_session_fires_mutation_once_on_change_and_never_on_reselect() {
    let mut model = nav_two_projects();
    let ids = model.navigable_sidebar_session_ids();
    // Land on the first session before installing the counter so the seed's own
    // selection isn't counted; ids[0] is already active from the seed, so this
    // is a no-op and nothing is missed.
    model.select_session(&ids[0]);
    let counter = mutation_counter(&mut model);

    // A real selection change fires exactly once.
    model.select_session(&ids[1]);
    assert_eq!(
        counter.get(),
        1,
        "a real selection change must fire the did-mutate signal exactly once"
    );

    // Re-selecting the already-active session changes nothing — no event.
    model.select_session(&ids[1]);
    assert_eq!(
        counter.get(),
        1,
        "re-selecting the active session is a no-op and must not fire the signal"
    );
}

// =====================================================================
// TabModelProjectBucketingTests (model half)
//
// The Swift suite drives `SessionsModel.createTabFromMainTerminal` (spawns a
// pty). The model-relevant behavior — which project the session buckets into — is
// `add_session_to_projects`, tested directly here. The session-build + worktree-dir
// string construction (`<cwd>/.claude/worktrees/<name>`, `/`→`+`) and the pty
// spawn belong to `createTabFromMainTerminal` and are R13:
//   R13: createTabFromMainTerminal worktree-dir construction + session shape
//        (test_claudeFromMainTerminal_withWorktreeFlag_* / _withoutWorktreeFlag_
//        tabCwdMatchesProjectPath) — extract_worktree_name (the parser half) is
//        ported below; the bucketing-by-parent-cwd half is covered here.
//   R13/R18: the addRestoredTabModel restore-heal cases
//        (test_addRestoredTabModel_*).
// =====================================================================

/// A Claude + terminal session for bucketing assertions.
fn new_claude_session(id: &str, cwd: &str) -> Session {
    let mut session = Session::new(id, "New session", cwd);
    session.windows = vec![
        claude(&format!("{}-claude", id)),
        terminal(&format!("{}-t1", id), "Terminal 1"),
    ];
    session.active_window_id = Some(format!("{}-claude", id));
    session
}

fn non_terminals_projects(model: &WorkspaceModel) -> Vec<&Project> {
    model
        .projects
        .iter()
        .filter(|p| p.id != WorkspaceModel::TERMINALS_PROJECT_ID)
        .collect()
}

// MARK: - extract_worktree_name

#[test]
fn extract_worktree_name_short_flag() {
    assert_eq!(
        WorkspaceModel::extract_worktree_name(&["-w", "foo"]),
        Some("foo".to_string())
    );
}

#[test]
fn extract_worktree_name_long_flag() {
    assert_eq!(
        WorkspaceModel::extract_worktree_name(&["--worktree", "foo"]),
        Some("foo".to_string())
    );
}

#[test]
fn extract_worktree_name_trailing_flag_returns_none() {
    assert_eq!(WorkspaceModel::extract_worktree_name(&["-w"]), None);
    assert_eq!(WorkspaceModel::extract_worktree_name(&["a", "--worktree"]), None);
}

#[test]
fn extract_worktree_name_empty_value_returns_none() {
    assert_eq!(WorkspaceModel::extract_worktree_name(&["-w", ""]), None);
}

#[test]
fn extract_worktree_name_scans_past_other_args() {
    assert_eq!(
        WorkspaceModel::extract_worktree_name(&["--model", "sonnet", "-w", "foo"]),
        Some("foo".to_string())
    );
}

#[test]
fn extract_worktree_name_equals_form_not_recognized() {
    // Design decision: only space-delimited is supported.
    assert_eq!(WorkspaceModel::extract_worktree_name(&["-w=foo"]), None);
    assert_eq!(WorkspaceModel::extract_worktree_name(&["--worktree=foo"]), None);
}

#[test]
fn extract_worktree_name_absent_returns_none() {
    let empty: &[&str] = &[];
    assert_eq!(WorkspaceModel::extract_worktree_name(empty), None);
    assert_eq!(WorkspaceModel::extract_worktree_name(&["--model", "sonnet"]), None);
}

// MARK: - sanitize_worktree_name (`/`→`+`, mirroring Claude's worktree-dir
// derivation; `SessionsModel.swift:677-682`).

#[test]
fn sanitize_worktree_name_replaces_slash_with_plus() {
    assert_eq!(WorkspaceModel::sanitize_worktree_name("foo/bar"), "foo+bar");
}

#[test]
fn sanitize_worktree_name_replaces_every_slash() {
    assert_eq!(WorkspaceModel::sanitize_worktree_name("a/b/c"), "a+b+c");
}

#[test]
fn sanitize_worktree_name_no_slash_unchanged() {
    assert_eq!(WorkspaceModel::sanitize_worktree_name("feature-x"), "feature-x");
}

#[test]
fn sanitize_worktree_name_empty_unchanged() {
    assert_eq!(WorkspaceModel::sanitize_worktree_name(""), "");
}

// MARK: - extract_claude_session_id

#[test]
fn extract_claude_session_id_resume_space_delimited() {
    assert_eq!(
        WorkspaceModel::extract_claude_session_id(&["--resume", "abc-123"]),
        Some("abc-123".to_string())
    );
}

#[test]
fn extract_claude_session_id_session_id_space_delimited() {
    assert_eq!(
        WorkspaceModel::extract_claude_session_id(&["--session-id", "uuid-1"]),
        Some("uuid-1".to_string())
    );
}

#[test]
fn extract_claude_session_id_resume_equals_form() {
    assert_eq!(
        WorkspaceModel::extract_claude_session_id(&["--resume=xyz"]),
        Some("xyz".to_string())
    );
}

#[test]
fn extract_claude_session_id_session_id_equals_form() {
    assert_eq!(
        WorkspaceModel::extract_claude_session_id(&["--session-id=qwe"]),
        Some("qwe".to_string())
    );
}

#[test]
fn extract_claude_session_id_scans_past_other_args() {
    assert_eq!(
        WorkspaceModel::extract_claude_session_id(&["--model", "sonnet", "--resume", "abc"]),
        Some("abc".to_string())
    );
}

#[test]
fn extract_claude_session_id_trailing_resume_returns_none() {
    assert_eq!(WorkspaceModel::extract_claude_session_id(&["--resume"]), None);
    assert_eq!(WorkspaceModel::extract_claude_session_id(&["a", "--session-id"]), None);
}

#[test]
fn extract_claude_session_id_absent_returns_none() {
    let empty: &[&str] = &[];
    assert_eq!(WorkspaceModel::extract_claude_session_id(empty), None);
    assert_eq!(WorkspaceModel::extract_claude_session_id(&["--model", "sonnet"]), None);
}

// MARK: - add_session_to_projects (bucketing)

#[test]
fn add_session_to_projects_under_main_cwd_creates_new_project_group() {
    let main_cwd = "/tmp/nice-test-home";
    let mut model = model_empty(main_cwd); // Terminals path = main_cwd, no git roots
    let cwd = "/tmp/nice-test-home/Projects/zephyr";
    model.add_session_to_projects(new_claude_session("t-z", cwd), cwd);

    assert_eq!(model.projects.len(), 2, "Terminals + one new project group");
    assert_eq!(model.projects[0].id, WorkspaceModel::TERMINALS_PROJECT_ID);
    assert_eq!(model.projects[0].sessions.len(), 1, "Terminals must not absorb Claude sessions");
    let new = non_terminals_projects(&model)[0];
    assert_eq!(new.name, "ZEPHYR");
    assert_eq!(new.path, cwd);
    assert_eq!(new.sessions.len(), 1);
    assert!(new.sessions[0].windows.iter().any(|w| w.kind == TermWindowKind::Claude));
}

#[test]
fn add_session_to_projects_cwd_equals_main_cwd_still_creates_new_project() {
    let main_cwd = "/tmp/nice-test-home";
    let mut model = model_empty(main_cwd);
    model.add_session_to_projects(new_claude_session("t-m", main_cwd), main_cwd);

    assert_eq!(model.projects.len(), 2);
    assert_eq!(model.projects[0].sessions.len(), 1, "Terminals keeps only Main");
    let new = non_terminals_projects(&model)[0];
    assert_eq!(new.path, main_cwd);
    assert_eq!(new.sessions.len(), 1);
}

#[test]
fn add_session_to_projects_picks_existing_project_when_cwd_matches() {
    let mut model = model_empty("/tmp/nice-test-home");
    seed_terminal_project(&mut model, "p1", "P1", "/tmp/p1");
    model.add_session_to_projects(new_claude_session("t-x", "/tmp/p1/sub"), "/tmp/p1/sub");

    assert_eq!(model.projects.len(), 2, "reuse p1, not create a third project");
    let p1 = project_by_id(&model, "p1");
    assert_eq!(p1.sessions.len(), 2);
    assert!(p1.sessions.last().unwrap().windows.iter().any(|w| w.kind == TermWindowKind::Claude));
    assert_eq!(model.projects[0].sessions.len(), 1);
}

#[test]
fn add_session_to_projects_longest_prefix_wins_among_projects() {
    let mut model = model_empty("/tmp/nice-test-home");
    seed_terminal_project(&mut model, "p1", "P1", "/tmp/p1");
    seed_terminal_project(&mut model, "p1-nested", "Nested", "/tmp/p1/nested");
    model.add_session_to_projects(new_claude_session("t-x", "/tmp/p1/nested/x"), "/tmp/p1/nested/x");

    assert_eq!(project_by_id(&model, "p1").sessions.len(), 1, "shallower must not win");
    assert_eq!(
        project_by_id(&model, "p1-nested").sessions.len(),
        2,
        "deeper project is the longest-prefix match"
    );
}

#[test]
fn add_session_to_projects_nested_git_repo_creates_separate_project_from_outer() {
    let outer = "/fs/outer";
    let nested = "/fs/outer/nested-1";
    let mut model = model_with(
        "/tmp/main",
        &[outer, "/fs/outer/.git", nested, "/fs/outer/nested-1/.git"],
    );
    seed_terminal_project(&mut model, "outer", "OUTER", outer);
    model.add_session_to_projects(new_claude_session("t-n", nested), nested);

    assert_eq!(project_by_id(&model, "outer").sessions.len(), 1, "outer must not absorb the nested session");
    let nested_p = model
        .projects
        .iter()
        .find(|p| p.id != WorkspaceModel::TERMINALS_PROJECT_ID && p.id != "outer")
        .expect("a separate project rooted at the nested repo must exist");
    assert_eq!(nested_p.path, nested);
    assert_eq!(nested_p.name, "NESTED-1");
    assert_eq!(nested_p.sessions.len(), 1);
}

#[test]
fn add_session_to_projects_subdir_of_existing_repo_buckets_into_existing_project() {
    let repo = "/fs/repo";
    let sub = "/fs/repo/src/deep";
    let mut model = model_with("/tmp/main", &[repo, "/fs/repo/.git", sub]);
    seed_terminal_project(&mut model, "repo", "REPO", repo);
    model.add_session_to_projects(new_claude_session("t-s", sub), sub);

    assert_eq!(project_by_id(&model, "repo").sessions.len(), 2, "sub-dir session buckets into the repo project");
    assert!(
        model
            .projects
            .iter()
            .all(|p| p.id == WorkspaceModel::TERMINALS_PROJECT_ID || p.id == "repo"),
        "no spurious project for the sub-dir"
    );
}

#[test]
fn add_session_to_projects_first_cwd_inside_repo_anchors_project_at_git_root() {
    let repo = "/fs/repo";
    let sub = "/fs/repo/src/deep";
    let mut model = model_with("/tmp/main", &[repo, "/fs/repo/.git", sub]);
    model.add_session_to_projects(new_claude_session("t-f", sub), sub);

    let new = non_terminals_projects(&model);
    assert_eq!(new.len(), 1);
    assert_eq!(new[0].path, repo, "project anchored at the git root, not the cwd");
    assert_eq!(new[0].name, "REPO");
    assert_eq!(new[0].sessions.len(), 1);
}

#[test]
fn add_session_to_projects_cwd_inside_nice_worktree_buckets_into_parent_repo() {
    let repo = "/fs/repo";
    let worktree = "/fs/repo/.claude/worktrees/bug";
    let mut model = model_with(
        "/tmp/main",
        &[repo, "/fs/repo/.git", worktree, "/fs/repo/.claude/worktrees/bug/.git"],
    );
    seed_terminal_project(&mut model, "repo", "REPO", repo);
    model.add_session_to_projects(new_claude_session("t-w", worktree), worktree);

    assert_eq!(
        project_by_id(&model, "repo").sessions.len(),
        2,
        "a cwd inside a Nice worktree buckets into the parent repo"
    );
    assert!(
        model.projects.iter().all(|p| p.name != "BUG"),
        "no worktree-named project should have been created"
    );
}

// MARK: - resolved_spawn_cwd (bucketing suite)

#[test]
fn resolved_spawn_cwd_falls_back_to_project_path_when_session_cwd_missing() {
    let project_path = "/fs/project";
    let missing = "/fs/project/.claude/worktrees/deleted";
    let mut model = model_with("/tmp/main", &[project_path]); // `missing` not registered
    seed_terminal_project(&mut model, "tmp", "TMP", project_path);
    let mut session = Session::new("tmp-worktree-session", "worktree", missing);
    session.windows = vec![terminal("tmp-worktree-session-p0", "zsh")];
    session.active_window_id = Some("tmp-worktree-session-p0".into());
    let idx = model.projects.iter().position(|p| p.id == "tmp").unwrap();
    model.projects[idx].sessions.push(session.clone());

    assert_eq!(model.resolved_spawn_cwd(&session), project_path);
}

#[test]
fn resolved_spawn_cwd_returns_session_cwd_when_it_exists() {
    let existing = "/fs/existing";
    let mut model = model_with("/tmp/main", &[existing]);
    seed_terminal_project(&mut model, "tmp", "TMP", "/does-not-matter");
    let mut session = Session::new("tmp-real-session", "real", existing);
    session.windows = vec![terminal("tmp-real-session-p0", "zsh")];
    session.active_window_id = Some("tmp-real-session-p0".into());
    let idx = model.projects.iter().position(|p| p.id == "tmp").unwrap();
    model.projects[idx].sessions.push(session.clone());

    assert_eq!(model.resolved_spawn_cwd(&session), existing);
}

// =====================================================================
// TabModelProjectRepairTests
// =====================================================================

/// A repair-fixture session: a single terminal window, no pty.
fn repair_session(id: &str, cwd: &str) -> Session {
    let mut session = Session::new(id, id, cwd);
    session.windows = vec![terminal(&format!("{}-p0", id), "zsh")];
    session.active_window_id = Some(format!("{}-p0", id));
    session
}

fn project_with(id: &str, name: &str, path: &str, sessions: Vec<Session>) -> Project {
    Project {
        id: id.into(),
        name: name.into(),
        path: path.into(),
        sessions,
    }
}

#[test]
fn repair_moves_nested_session_into_own_project() {
    let outer = "/fs/outer";
    let nested = "/fs/outer/nested-1";
    let mut model = model_with(
        "/home",
        &[outer, "/fs/outer/.git", nested, "/fs/outer/nested-1/.git"],
    );
    model.projects.push(project_with(
        "outer",
        "OUTER",
        outer,
        vec![repair_session("outer-seed", outer), repair_session("stray-nested", nested)],
    ));

    model.repair_project_structure();

    let outer_p = project_by_id(&model, "outer");
    assert_eq!(outer_p.sessions.len(), 1, "only the nested-cwd session should have moved");
    assert_eq!(outer_p.sessions[0].id, "outer-seed");

    let nested_p = model
        .projects
        .iter()
        .find(|p| p.path == nested)
        .expect("a new project anchored at the nested repo must exist");
    assert_ne!(nested_p.id, WorkspaceModel::TERMINALS_PROJECT_ID);
    assert_ne!(nested_p.id, "outer");
    assert!(nested_p.id.starts_with("p-nested-1-"));
    assert_eq!(nested_p.name, "NESTED-1");
    assert_eq!(nested_p.sessions.len(), 1);
    assert_eq!(nested_p.sessions[0].id, "stray-nested");
}

#[test]
fn repair_promotion_then_move_compose() {
    let outer = "/fs/outer";
    let sub = "/fs/outer/sub";
    let nested = "/fs/outer/sub/nested";
    let mut model = model_with(
        "/home",
        &[
            outer,
            "/fs/outer/.git",
            sub,
            nested,
            "/fs/outer/sub/nested/.git",
        ],
    );
    model.projects.push(project_with(
        "p-sub-original",
        "SUB",
        sub,
        vec![repair_session("sub-seed", sub), repair_session("deep-nested", nested)],
    ));

    model.repair_project_structure();

    let promoted = project_by_id(&model, "p-sub-original");
    assert_eq!(promoted.path, outer, "pass 1 promotes outer/sub to outer");
    assert_eq!(promoted.name, "OUTER");
    assert_eq!(promoted.sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["sub-seed"]);

    let nested_p = model
        .projects
        .iter()
        .find(|p| p.path == nested)
        .expect("pass 2 must create a project for the nested-cwd session");
    assert_eq!(nested_p.sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["deep-nested"]);
}

#[test]
fn repair_skips_sessions_with_missing_cwd() {
    let repo = "/fs/repo";
    let missing = "/fs/repo/.claude/worktrees/deleted";
    let mut model = model_with("/home", &[repo, "/fs/repo/.git"]); // missing not registered
    model.projects.push(project_with("repo", "REPO", repo, vec![repair_session("ghost", missing)]));

    model.repair_project_structure();

    let p = project_by_id(&model, "repo");
    assert_eq!(p.sessions.len(), 1);
    assert_eq!(p.sessions[0].id, "ghost");
}

#[test]
fn repair_promotes_subdir_project_to_git_root() {
    let repo = "/fs/repo";
    let deep = "/fs/repo/src/deep";
    let mut model = model_with("/home", &[repo, "/fs/repo/.git", deep]);
    model.projects.push(project_with("p-deep-123", "DEEP", deep, vec![repair_session("deep-session", deep)]));

    model.repair_project_structure();

    assert_eq!(non_terminals_projects(&model).len(), 1, "promotion must not create/drop projects");
    let promoted = project_by_id(&model, "p-deep-123");
    assert_eq!(promoted.path, repo);
    assert_eq!(promoted.name, "REPO");
    assert_eq!(promoted.sessions.len(), 1);
    assert_eq!(promoted.sessions[0].id, "deep-session");
}

#[test]
fn repair_merges_duplicate_projects_at_same_git_root() {
    let repo = "/fs/repo";
    let mut model = model_with("/home", &[repo, "/fs/repo/.git"]);
    model.projects.push(project_with("first", "REPO", repo, vec![repair_session("first-session", repo)]));
    model.projects.push(project_with("second", "REPO", repo, vec![repair_session("second-session", repo)]));

    model.repair_project_structure();

    assert_eq!(non_terminals_projects(&model).len(), 1, "duplicate at same path merged");
    let canonical = project_by_id(&model, "first");
    assert_eq!(
        canonical.sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        ["first-session", "second-session"],
        "canonical's own sessions first, then the merged dupe's"
    );
    assert!(model.projects.iter().all(|p| p.id != "second"), "merged dupe removed");
}

#[test]
fn repair_drops_empty_projects_but_preserves_terminals() {
    let mut model = model_with("/home", &[]);
    model.projects.push(project_with("abandoned", "GHOST", "/tmp/no-sessions-here", vec![]));
    let terminals_before = model.projects[0].id.clone();

    model.repair_project_structure();

    assert_eq!(model.projects[0].id, terminals_before);
    assert_eq!(model.projects[0].id, WorkspaceModel::TERMINALS_PROJECT_ID);
    assert!(model.projects.iter().all(|p| p.id != "abandoned"), "empty non-Terminals dropped");
}

#[test]
fn repair_leaves_terminals_project_alone() {
    let mut model = model_with("/home", &[]);
    let before = model.projects[0].clone();

    model.repair_project_structure();

    let after = &model.projects[0];
    assert_eq!(after.path, before.path);
    assert_eq!(after.name, before.name);
    assert_eq!(
        after.sessions.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
        before.sessions.iter().map(|s| s.id.clone()).collect::<Vec<_>>()
    );
}

#[test]
fn repair_is_idempotent() {
    let outer = "/fs/outer";
    let nested = "/fs/outer/nested-1";
    let deep = "/fs/outer/src/deep";
    let build = || {
        let mut model = model_with(
            "/home",
            &[
                outer,
                "/fs/outer/.git",
                nested,
                "/fs/outer/nested-1/.git",
                deep,
            ],
        );
        model.projects.push(project_with(
            "outer",
            "OUTER",
            outer,
            vec![
                repair_session("outer-seed", outer),
                repair_session("stray-nested", nested),
                repair_session("deep-sub", deep),
            ],
        ));
        model.projects.push(project_with("p-deep-123", "DEEP", deep, vec![]));
        model
    };
    let snapshot = |model: &WorkspaceModel| {
        model
            .projects
            .iter()
            .map(|p| {
                (
                    p.id.clone(),
                    p.name.clone(),
                    p.path.clone(),
                    p.sessions.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };

    let mut model = build();
    model.repair_project_structure();
    let after_first = snapshot(&model);
    model.repair_project_structure();
    let after_second = snapshot(&model);
    assert_eq!(after_second, after_first, "second repair pass must not mutate a repaired structure");
}

// =====================================================================
// TabModelRenameTests
// =====================================================================

/// Inject a single-window session under a fresh project keyed by path. Deterministic
/// ids per call via a counter (mirrors `TabModelFixtures.injectTab`).
fn inject_session(model: &mut WorkspaceModel, title: &str, project_path: &str, kind: TermWindowKind) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let session_id = format!("t-inject-{}", n);
    let (window_id, window_title) = match kind {
        TermWindowKind::Claude => (format!("{}-claude", session_id), "Claude"),
        TermWindowKind::Terminal => (format!("{}-term", session_id), "Terminal"),
    };
    let mut session = Session::new(&session_id, title, project_path);
    session.windows = vec![TermWindow::new(&window_id, window_title, kind)];
    session.active_window_id = Some(window_id);
    if let Some(idx) = model.projects.iter().position(|p| p.path == project_path) {
        model.projects[idx].sessions.push(session);
    } else {
        model.projects.push(Project {
            id: format!("p-inject-{}", n),
            name: last_path_component(project_path),
            path: project_path.into(),
            sessions: vec![session],
        });
    }
    session_id
}

#[test]
fn rename_session_sets_title_and_marks_manually_set() {
    let mut model = model_empty("/tmp/main");
    let id = inject_session(&mut model, "New session", "/tmp/rename-test", TermWindowKind::Terminal);
    model.rename_session(&id, "My session");
    let after = model.session_for(&id).unwrap();
    assert_eq!(after.title, "My session");
    assert!(after.title_manually_set);
}

#[test]
fn rename_session_trims_whitespace() {
    let mut model = model_empty("/tmp/main");
    let id = inject_session(&mut model, "New session", "/tmp/rename-test", TermWindowKind::Terminal);
    model.rename_session(&id, "   padded   ");
    assert_eq!(model.session_for(&id).unwrap().title, "padded");
}

#[test]
fn rename_session_empty_input_is_noop() {
    let mut model = model_empty("/tmp/main");
    let id = inject_session(&mut model, "Original", "/tmp/rename-test", TermWindowKind::Terminal);
    model.rename_session(&id, "   ");
    let after = model.session_for(&id).unwrap();
    assert_eq!(after.title, "Original");
    assert!(!after.title_manually_set, "empty rename must not mark manually set");
}

#[test]
fn apply_auto_title_skips_after_manual_rename() {
    let mut model = model_empty("/tmp/main");
    let id = inject_session(&mut model, "New session", "/tmp/rename-test", TermWindowKind::Terminal);
    model.rename_session(&id, "My session");
    model.apply_auto_title(&id, "late-arriving-session");
    assert_eq!(
        model.session_for(&id).unwrap().title,
        "My session",
        "apply_auto_title must skip a user-renamed session"
    );
}

#[test]
fn apply_auto_title_on_other_sessions_is_unaffected_by_rename() {
    let mut model = model_empty("/tmp/main");
    let renamed = inject_session(&mut model, "New session", "/tmp/rename-test", TermWindowKind::Terminal);
    let other = inject_session(&mut model, "New session", "/tmp/rename-other", TermWindowKind::Terminal);
    model.rename_session(&renamed, "Manual name");
    model.apply_auto_title(&other, "fix-some-bug");

    assert_eq!(model.session_for(&renamed).unwrap().title, "Manual name");
    assert_eq!(model.session_for(&other).unwrap().title, "Fix some bug");
    assert!(model.session_for(&other).unwrap().title_auto_generated);
    assert!(!model.session_for(&other).unwrap().title_manually_set);
}

#[test]
fn apply_auto_title_still_works_on_fresh_session() {
    let mut model = model_empty("/tmp/main");
    let id = inject_session(&mut model, "New session", "/tmp/rename-test", TermWindowKind::Terminal);
    model.apply_auto_title(&id, "fix-top-bar-height");
    let after = model.session_for(&id).unwrap();
    assert_eq!(after.title, "Fix top bar height");
    assert!(after.title_auto_generated);
    assert!(!after.title_manually_set);
}

// R13/R15: test_paneTitleChanged_afterRename_doesNotClobber drives the OSC
// entry point (SessionsModel.paneTitleChanged); the underlying guard is pinned
// by apply_auto_title_skips_after_manual_rename above.
// R18: test_manualRename_roundTripsThroughPersistedWindow (persistence schema).

// =====================================================================
// PaneNamingTests (model / rename / addPane) — recover_next_terminal_index
// cases already live in session.rs (slice 1).
// =====================================================================

fn main_window_id(model: &WorkspaceModel) -> String {
    model.session_for(WorkspaceModel::MAIN_TERMINAL_SESSION_ID).unwrap().windows[0]
        .id
        .clone()
}

#[test]
fn seed_main_session_initial_window_title_is_terminal_1() {
    let model = model_empty("/tmp/main");
    let main = model.session_for(WorkspaceModel::MAIN_TERMINAL_SESSION_ID).unwrap();
    assert_eq!(main.windows[0].title, "Terminal 1");
    assert_eq!(main.next_terminal_index, 2, "seed counter primed at 2");
}

/// Validation §4b: monotonic-after-closing spot-probe.
#[test]
fn add_window_is_monotonic_after_closing_a_window() {
    let mut model = model_empty("/tmp/main");
    let session_id = WorkspaceModel::MAIN_TERMINAL_SESSION_ID;
    model.add_window(session_id, "px2", None);
    model.add_window(session_id, "px3", None);
    let session = model.session_for(session_id).unwrap();
    assert_eq!(session.windows[1].title, "Terminal 2");
    assert_eq!(session.windows[2].title, "Terminal 3");

    model.mutate_session(session_id, |s| s.windows.retain(|w| w.id != "px2"));

    let px4 = model.add_window(session_id, "px4", None).unwrap();
    let session_after = model.session_for(session_id).unwrap();
    let new_window = session_after.windows.iter().find(|w| w.id == px4).unwrap();
    assert_eq!(new_window.title, "Terminal 4", "closing T2 must not reuse the number");
    assert_eq!(session_after.next_terminal_index, 5, "closing a window must not decrement the counter");
}

#[test]
fn add_window_explicit_title_still_increments_counter() {
    let mut model = model_empty("/tmp/main");
    let session_id = WorkspaceModel::MAIN_TERMINAL_SESSION_ID;
    let counter_before = model.session_for(session_id).unwrap().next_terminal_index;
    let windows_before = model.session_for(session_id).unwrap().windows.len();

    model.add_window(session_id, "pe", Some("vim foo.swift".into()));

    let session = model.session_for(session_id).unwrap();
    assert_eq!(session.windows.len(), windows_before + 1);
    assert_eq!(session.windows.last().unwrap().title, "vim foo.swift");
    assert_eq!(session.next_terminal_index, counter_before + 1, "explicit title still advances the counter");
}

#[test]
fn add_window_increments_counter() {
    let mut model = model_empty("/tmp/main");
    let session_id = WorkspaceModel::MAIN_TERMINAL_SESSION_ID;
    model.add_window(session_id, "a", None);
    model.add_window(session_id, "b", None);
    model.add_window(session_id, "c", None);
    assert_eq!(model.session_for(session_id).unwrap().next_terminal_index, 5, "seed 2 + 3 adds → 5");
}

#[test]
fn rename_window_changes_title() {
    let mut model = model_empty("/tmp/main");
    let session_id = WorkspaceModel::MAIN_TERMINAL_SESSION_ID;
    let window_id = main_window_id(&model);
    model.rename_window(session_id, &window_id, "logs");
    assert_eq!(model.session_for(session_id).unwrap().windows[0].title, "logs");
}

#[test]
fn rename_window_trims_whitespace() {
    let mut model = model_empty("/tmp/main");
    let session_id = WorkspaceModel::MAIN_TERMINAL_SESSION_ID;
    let window_id = main_window_id(&model);
    model.rename_window(session_id, &window_id, "  padded  ");
    assert_eq!(model.session_for(session_id).unwrap().windows[0].title, "padded");
}

#[test]
fn rename_window_empty_input_resets_to_auto_default_clears_flag() {
    let mut model = model_empty("/tmp/main");
    let session_id = WorkspaceModel::MAIN_TERMINAL_SESSION_ID;
    let window_id = main_window_id(&model);
    let counter_before = model.session_for(session_id).unwrap().next_terminal_index;

    model.rename_window(session_id, &window_id, "logs");
    assert!(model.session_for(session_id).unwrap().windows[0].title_manually_set);

    model.rename_window(session_id, &window_id, "  ");

    let session = model.session_for(session_id).unwrap();
    assert!(!session.windows[0].title_manually_set, "empty submit clears the lock");
    assert_eq!(
        session.windows[0].title,
        format!("Terminal {}", counter_before),
        "empty submit resets to the auto-default consuming the next counter slot"
    );
    assert_eq!(session.next_terminal_index, counter_before + 1, "reset path advances the counter");
}

#[test]
fn rename_window_sets_title_manually_set() {
    let mut model = model_empty("/tmp/main");
    let session_id = WorkspaceModel::MAIN_TERMINAL_SESSION_ID;
    let window_id = main_window_id(&model);
    model.rename_window(session_id, &window_id, "build");
    let window = &model.session_for(session_id).unwrap().windows[0];
    assert_eq!(window.title, "build");
    assert!(window.title_manually_set);
}

#[test]
fn rename_window_fires_on_tree_mutation() {
    let mut model = model_empty("/tmp/main");
    let session_id = WorkspaceModel::MAIN_TERMINAL_SESSION_ID;
    let window_id = main_window_id(&model);
    let counter = mutation_counter(&mut model);
    model.rename_window(session_id, &window_id, "new name");
    assert_eq!(counter.get(), 1);
}

#[test]
fn rename_window_does_not_fire_on_tree_mutation_when_no_change() {
    let mut model = model_empty("/tmp/main");
    let session_id = WorkspaceModel::MAIN_TERMINAL_SESSION_ID;
    let window_id = main_window_id(&model);
    model.rename_window(session_id, &window_id, "logs"); // first rename locks
    let counter = mutation_counter(&mut model);
    model.rename_window(session_id, &window_id, "logs"); // identical → no change
    assert_eq!(counter.get(), 0);
}

#[test]
fn rename_window_does_not_touch_other_windows() {
    let mut model = model_empty("/tmp/main");
    let session_id = WorkspaceModel::MAIN_TERMINAL_SESSION_ID;
    let window1 = main_window_id(&model);
    model.mutate_session(session_id, |s| {
        s.windows.push(terminal("stable-p2", "Terminal 2"));
    });
    let before = model
        .session_for(session_id)
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == "stable-p2")
        .unwrap()
        .title
        .clone();
    model.rename_window(session_id, &window1, "renamed");
    let after = model
        .session_for(session_id)
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == "stable-p2")
        .unwrap()
        .title
        .clone();
    assert_eq!(after, before);
}

// R18: hydration (test_hydration_*) drives addRestoredTabModel / PersistedSession;
// recover_next_terminal_index itself is pinned in session.rs. Persistence
// round-trips (test_persistedTab_* / test_persistedPane_* / test_snapshot_*)
// arrive with the R18 schema.

// =====================================================================
// AppStateBranchTrackingTests — depth-1 lineage tree shape (via
// insert_branch_parent + remove_session). The /branch trigger CLASSIFICATION
// (source=resume/clear/nil, id-change detection, per-window dispatch) is R16;
// the OSC-title-ignore + pty cascade are R13/R15; PersistedSession round-trips R18.
// The caller's post-rotation id update (child adopts NEW) is simulated here
// with mutate_session so the tree assertions stand alone.
// =====================================================================

/// Seed one Claude session `t1` (session `S0`, claude window `t1-claude`) in project
/// `p`, plus the pinned Terminals group. cwd `/tmp/p`.
fn branch_seed(session: &str, title: &str) -> WorkspaceModel {
    let mut model = WorkspaceModel::with_fs("/tmp", fake_fs("/home", &[]));
    seed_claude_session(&mut model, "p", "t1", session, "/tmp/p", true);
    model.mutate_session("t1", |s| s.title = title.to_string());
    model
}

#[test]
fn insert_branch_parent_creates_parent_shape() {
    let mut model = branch_seed("OLD", "wire up the foo");
    let parent = model
        .insert_branch_parent("t1", "parent-1", "parent-1-claude", "parent-1-t1", "OLD")
        .expect("insert_branch_parent must return the inserted parent");
    // Caller (R16) updates the originating session to the post-rotation id.
    model.mutate_session("t1", |s| s.claude_session_id = Some("NEW".into()));

    let project = project_by_id(&model, "p");
    assert_eq!(project.sessions.len(), 2, "exactly one sibling parent added");
    let parent_session = &project.sessions[0];
    let child = &project.sessions[1];

    assert_eq!(parent_session.id, "parent-1");
    assert_eq!(child.id, "t1", "originating session keeps its id");
    assert_eq!(child.claude_session_id.as_deref(), Some("NEW"), "child adopts the post-rotation id");
    assert_eq!(child.parent_session_id.as_deref(), Some("parent-1"), "child points at the new parent");

    assert_eq!(parent_session.claude_session_id.as_deref(), Some("OLD"), "parent pinned to the pre-rotation id");
    assert!(parent_session.parent_session_id.is_none(), "parent stays at root");
    assert_eq!(parent_session.title, "wire up the foo", "parent inherits the title");
    assert_eq!(parent_session.cwd, child.cwd, "parent inherits the cwd");

    assert_eq!(parent_session.windows.len(), 2);
    assert!(parent_session.windows.iter().any(|w| w.kind == TermWindowKind::Claude));
    assert!(parent_session.windows.iter().any(|w| w.kind == TermWindowKind::Terminal));
    // Deferred-resume: the parent's Claude window is created NOT running.
    let parent_claude = parent_session.windows.iter().find(|w| w.kind == TermWindowKind::Claude).unwrap();
    assert!(!parent_claude.is_claude_running, "branch parent's Claude window is deferred (not running)");
    assert_eq!(parent, *parent_session, "returned parent equals the inserted tree node");
}

#[test]
fn first_branch_promotes_parent_to_root_and_originating_becomes_child() {
    let mut model = branch_seed("S0", "New session");
    model.insert_branch_parent("t1", "P1", "P1-c", "P1-t", "S0");
    model.mutate_session("t1", |s| s.claude_session_id = Some("S1".into()));

    let project = project_by_id(&model, "p");
    assert_eq!(project.sessions.len(), 2);
    assert!(project.sessions[0].parent_session_id.is_none(), "first parent becomes the lineage root");
    assert_eq!(
        project.sessions[1].parent_session_id.as_deref(),
        Some("P1"),
        "originating session pulled in as a depth-1 child of the new root"
    );
}

#[test]
fn second_branch_adds_sibling_child_under_same_root() {
    let mut model = branch_seed("S0", "New session");
    model.insert_branch_parent("t1", "P1", "P1-c", "P1-t", "S0");
    model.mutate_session("t1", |s| s.claude_session_id = Some("S1".into()));
    model.insert_branch_parent("t1", "P2", "P2-c", "P2-t", "S1");
    model.mutate_session("t1", |s| s.claude_session_id = Some("S2".into()));

    let after = project_by_id(&model, "p");
    assert_eq!(after.sessions.len(), 3);
    let (root, second, originating) = (&after.sessions[0], &after.sessions[1], &after.sessions[2]);

    assert_eq!(root.id, "P1", "root never changes once established");
    assert_eq!(root.claude_session_id.as_deref(), Some("S0"));
    assert!(root.parent_session_id.is_none());

    assert_eq!(originating.id, "t1");
    assert_eq!(originating.claude_session_id.as_deref(), Some("S2"));
    assert_eq!(originating.parent_session_id.as_deref(), Some("P1"), "originating keeps pointing at the original root");

    assert_eq!(second.claude_session_id.as_deref(), Some("S1"));
    assert_eq!(second.parent_session_id.as_deref(), Some("P1"), "second parent is a sibling under the same root");
}

#[test]
fn third_branch_keeps_adding_siblings_under_same_root() {
    let mut model = branch_seed("S0", "New session");
    for (i, new_session) in ["S1", "S2", "S3"].iter().enumerate() {
        let old = model.session_for("t1").unwrap().claude_session_id.clone().unwrap();
        let pid = format!("parent-{}", i);
        model.insert_branch_parent("t1", &pid, &format!("{}-c", pid), &format!("{}-t", pid), &old);
        model.mutate_session("t1", |s| s.claude_session_id = Some(new_session.to_string()));
        assert_eq!(project_by_id(&model, "p").sessions.len(), i + 2);
    }

    let final_p = project_by_id(&model, "p");
    let root = &final_p.sessions[0];
    assert!(root.parent_session_id.is_none());
    assert_eq!(root.claude_session_id.as_deref(), Some("S0"));
    for session in final_p.sessions.iter().skip(1) {
        assert_eq!(session.parent_session_id.as_deref(), Some(root.id.as_str()), "every non-root session points at the original root");
    }
    assert_eq!(final_p.sessions.last().unwrap().id, "t1", "originating session stays at the bottom in display order");
    assert_eq!(final_p.sessions.last().unwrap().claude_session_id.as_deref(), Some("S3"));
}

/// Validation §4c: /branch on a lineage root re-parents its former children.
#[test]
fn branch_on_root_preserves_depth1_by_reparenting_former_children() {
    let mut model = branch_seed("S0", "New session");
    // First branch: P1(S0) root, t1(S1) child of P1.
    model.insert_branch_parent("t1", "P1", "P1-c", "P1-t", "S0");
    model.mutate_session("t1", |s| s.claude_session_id = Some("S1".into()));
    // Second branch on t1: P2(S1) sibling under P1.
    model.insert_branch_parent("t1", "P2", "P2-c", "P2-t", "S1");
    model.mutate_session("t1", |s| s.claude_session_id = Some("S2".into()));

    // Now /branch on the OLD ROOT (P1). old session on P1 is S0.
    model.insert_branch_parent("P1", "P3", "P3-c", "P3-t", "S0");
    model.mutate_session("P1", |s| s.claude_session_id = Some("S0-PRIME".into()));

    let after = project_by_id(&model, "p");
    let roots: Vec<&Session> = after.sessions.iter().filter(|s| s.parent_session_id.is_none()).collect();
    assert_eq!(roots.len(), 1, "exactly one root remains after /branch on the old root");
    let new_root = roots[0];
    assert_eq!(new_root.id, "P3");
    assert_ne!(new_root.id, "P1", "old root must no longer be at depth 0");
    for session in after.sessions.iter().filter(|s| s.id != new_root.id) {
        assert_eq!(session.parent_session_id.as_deref(), Some("P3"), "every former child re-parented to the new root");
    }
    assert_eq!(model.session_for("t1").unwrap().claude_session_id.as_deref(), Some("S2"));
    assert_eq!(new_root.claude_session_id.as_deref(), Some("S0"), "new root pins the session current on the old root before its branch");
    assert_eq!(model.session_for("P1").unwrap().claude_session_id.as_deref(), Some("S0-PRIME"));
}

#[test]
fn closing_parent_clears_child_parent_session_id() {
    let mut model = branch_seed("OLD", "New session");
    model.insert_branch_parent("t1", "P1", "P1-c", "P1-t", "OLD");
    assert_eq!(project_by_id(&model, "p").sessions[1].parent_session_id.as_deref(), Some("P1"));

    let (pi, ti) = model.project_session_index("P1").unwrap();
    model.remove_session(pi, ti);

    let after = project_by_id(&model, "p");
    assert_eq!(after.sessions.len(), 1);
    assert_eq!(after.sessions[0].id, "t1");
    assert!(after.sessions[0].parent_session_id.is_none(), "child's parent_session_id cleared when parent dissolves");
}

#[test]
fn closing_child_does_not_mutate_parent() {
    let mut model = branch_seed("OLD", "New session");
    model.insert_branch_parent("t1", "P1", "P1-c", "P1-t", "OLD");

    let (pi, ti) = model.project_session_index("t1").unwrap();
    model.remove_session(pi, ti);

    let after = project_by_id(&model, "p");
    assert_eq!(after.sessions.len(), 1);
    assert_eq!(after.sessions[0].id, "P1");
    assert!(after.sessions[0].parent_session_id.is_none(), "parent must NOT be cleared when an unrelated child closes");
}

#[test]
fn branch_signal_on_terminals_session_is_no_op() {
    let mut model = model_empty("/tmp/main");
    let before = project_by_id(&model, WorkspaceModel::TERMINALS_PROJECT_ID).sessions.len();
    let result = model.insert_branch_parent(
        WorkspaceModel::MAIN_TERMINAL_SESSION_ID,
        "ghost-parent",
        "ghost-c",
        "ghost-t",
        "FRESH",
    );
    assert!(result.is_none(), "Terminals-project session must refuse a branch parent");
    assert_eq!(project_by_id(&model, WorkspaceModel::TERMINALS_PROJECT_ID).sessions.len(), before);
}

#[test]
fn prune_dangling_parent_references_clears_orphans() {
    let mut model = WorkspaceModel::with_fs("/tmp", fake_fs("/home", &[]));
    seed_claude_session(&mut model, "p", "root", "s-root", "/tmp/p", true);
    // A session pointing at an existing parent (kept) and one at a ghost (cleared).
    seed_claude_session(&mut model, "p", "child-valid", "s-cv", "/tmp/p", true);
    seed_claude_session(&mut model, "p", "child-orphan", "s-co", "/tmp/p", true);
    model.mutate_session("child-valid", |s| s.parent_session_id = Some("root".into()));
    model.mutate_session("child-orphan", |s| s.parent_session_id = Some("does-not-exist".into()));

    model.prune_dangling_parent_references();

    assert_eq!(model.session_for("child-valid").unwrap().parent_session_id.as_deref(), Some("root"), "valid parent kept");
    assert!(model.session_for("child-orphan").unwrap().parent_session_id.is_none(), "dangling parent cleared");
}

// =====================================================================
// dedupe_window_ids — the restore-time heal for pre-fix saves where the
// reset-at-launch minter persisted two windows sharing one id (the
// double-selected / rename-edits-both strip glitch).
// =====================================================================

#[test]
fn dedupe_window_ids_renames_later_duplicate_and_keeps_active_on_first() {
    let mut model = model_empty("/tmp/main");
    // The shape of the real corrupt save: "Moldavite" (window-1) restored from a
    // previous launch, "Terminal 15" (window-1) minted by the fresh counter.
    let mut session = Session::new("t-dup", "Main", "/tmp");
    session.windows = vec![
        terminal("window-1", "Moldavite"),
        terminal("keep-2", "Vault"),
        terminal("window-1", "Terminal 15"),
    ];
    session.active_window_id = Some("window-1".into());
    model.projects[0].sessions.push(session);

    model.dedupe_window_ids();

    let session = model.session_for("t-dup").unwrap();
    assert_eq!(session.windows[0].id, "window-1", "first occurrence keeps its id");
    assert_eq!(session.windows[1].id, "keep-2", "unique ids untouched");
    assert_eq!(session.windows[2].id, "window-1-dup2", "later duplicate is re-minted");
    assert_eq!(
        session.active_window_id.as_deref(),
        Some("window-1"),
        "active resolves unambiguously to the kept first window"
    );
}

#[test]
fn dedupe_window_ids_repoints_active_when_cross_session_duplicate_renamed() {
    let mut model = model_empty("/tmp/main");
    let mut a = Session::new("t-a", "A", "/tmp");
    a.windows = vec![terminal("window-1", "First")];
    a.active_window_id = Some("window-1".into());
    let mut b = Session::new("t-b", "B", "/tmp");
    b.windows = vec![terminal("window-1", "Second")];
    b.active_window_id = Some("window-1".into());
    model.projects[0].sessions.push(a);
    model.projects[0].sessions.push(b);

    model.dedupe_window_ids();

    assert_eq!(model.session_for("t-a").unwrap().windows[0].id, "window-1");
    let b = model.session_for("t-b").unwrap();
    assert_eq!(b.windows[0].id, "window-1-dup2");
    assert_eq!(
        b.active_window_id.as_deref(),
        Some("window-1-dup2"),
        "the orphaned active pointer follows the renamed window"
    );
}

#[test]
fn dedupe_window_ids_suffix_skips_an_id_already_in_the_tree() {
    let mut model = model_empty("/tmp/main");
    // A window literally named "window-1-dup2" already exists, so the rename must
    // step past it to the next unused suffix.
    let mut session = Session::new("t-dup", "Main", "/tmp");
    session.windows = vec![
        terminal("window-1-dup2", "Squatter"),
        terminal("window-1", "First"),
        terminal("window-1", "Second"),
    ];
    model.projects[0].sessions.push(session);

    model.dedupe_window_ids();

    let ids: Vec<&str> = model
        .session_for("t-dup")
        .unwrap()
        .windows
        .iter()
        .map(|w| w.id.as_str())
        .collect();
    assert_eq!(ids, vec!["window-1-dup2", "window-1", "window-1-dup3"]);
}

#[test]
fn dedupe_window_ids_is_a_noop_on_unique_ids() {
    let mut model = model_empty("/tmp/main");
    let mut session = Session::new("t-ok", "OK", "/tmp");
    session.windows = vec![terminal("a", "A"), terminal("b", "B")];
    session.active_window_id = Some("b".into());
    model.projects[0].sessions.push(session);
    let before = model.session_for("t-ok").unwrap().clone();

    model.dedupe_window_ids();
    // Idempotent: a second pass over the healed tree changes nothing either.
    model.dedupe_window_ids();

    assert_eq!(*model.session_for("t-ok").unwrap(), before, "unique ids are untouched");
}

// R16: test_branch_resumeWithIdChange_createsParentTab (classification →
// materializeBranchParent) — the parent SHAPE it produces is pinned by
// insert_branch_parent_creates_parent_shape above.
// R16: test_clear_withIdChange / test_missingSource / test_resumeWithSameId /
// test_branchOn_nilClaudeSessionId — all are source/id-change classification
// decisions (whether to call insert_branch_parent), not model behavior.
// R16: test_branchMaterialization_isScopedToOwningWindow (per-window dispatch).
// R13/R15: test_branchParentPane_isClaudeRunningFalse_ignoresShellOscTitle —
// the deferred-resume flag is pinned by insert_branch_parent_creates_parent_shape;
// the paneTitleChanged gate that reads it is R13/R15.
// R18: test_persistedTab_parentTabId_roundTrips / _legacyJsonWithoutParentTabId.

// =====================================================================
// TabModelInsertHandoffChildTests
// =====================================================================

fn make_handoff_session(id: &str, cwd: &str) -> Session {
    let claude_window_id = format!("{}-claude", id);
    let mut session = Session::new(id, "[HANDOFF] Some task", cwd);
    session.windows = vec![
        claude(&claude_window_id),
        terminal(&format!("{}-t1", id), "Terminal 1"),
    ];
    session.active_window_id = Some(claude_window_id);
    session.claude_session_id = Some("handoff-session".into());
    session.title_manually_set = true;
    session
}

#[test]
fn handoff_root_originating_session_child_parent_is_originating_id_returns_true() {
    let mut model = WorkspaceModel::with_fs("/tmp", fake_fs("/home", &[]));
    seed_claude_session(&mut model, "p", "t1", "s", "/tmp/p", true);

    let inserted = model.insert_handoff_child(make_handoff_session("child1", "/tmp/p"), "t1");
    assert!(inserted);

    let project = project_by_id(&model, "p");
    assert_eq!(project.sessions.len(), 2);
    assert_eq!(project.sessions[0].id, "t1", "originating session stays at index 0");
    assert_eq!(project.sessions[1].id, "child1", "handoff child placed right after originating");
    assert_eq!(project.sessions[1].parent_session_id.as_deref(), Some("t1"), "child's parent is the originating (root anchor)");
}

#[test]
fn handoff_originating_session_is_child_child_inherits_grandparent_returns_true() {
    let mut model = WorkspaceModel::with_fs("/tmp", fake_fs("/home", &[]));
    seed_claude_session(&mut model, "p", "root", "s-root", "/tmp/p", true);
    // "originating" already points at root.
    seed_claude_session(&mut model, "p", "originating", "session-orig", "/tmp/p", true);
    model.mutate_session("originating", |s| s.parent_session_id = Some("root".into()));

    let inserted = model.insert_handoff_child(make_handoff_session("child1", "/tmp/p"), "originating");
    assert!(inserted);

    let handoff = model.session_for("child1").expect("handoff child must exist");
    assert_eq!(
        handoff.parent_session_id.as_deref(),
        Some("root"),
        "depth-1: child of a child inherits the root, not the direct parent"
    );
}

#[test]
fn handoff_unknown_under_session_id_returns_false_no_insertion() {
    let mut model = WorkspaceModel::with_fs("/tmp", fake_fs("/home", &[]));
    seed_claude_session(&mut model, "p", "t1", "s", "/tmp/p", true);
    let before = project_by_id(&model, "p").sessions.len();

    let inserted = model.insert_handoff_child(make_handoff_session("child1", "/tmp/p"), "does-not-exist");
    assert!(!inserted, "unknown under_session_id must return false");
    assert_eq!(project_by_id(&model, "p").sessions.len(), before);
}

#[test]
fn handoff_terminals_project_session_returns_false_no_insertion() {
    let mut model = WorkspaceModel::with_fs("/tmp", fake_fs("/home", &[]));
    let before = project_by_id(&model, WorkspaceModel::TERMINALS_PROJECT_ID).sessions.len();
    let inserted =
        model.insert_handoff_child(make_handoff_session("child1", "/tmp"), WorkspaceModel::MAIN_TERMINAL_SESSION_ID);
    assert!(!inserted, "Terminals-project session must refuse a handoff child");
    assert_eq!(project_by_id(&model, WorkspaceModel::TERMINALS_PROJECT_ID).sessions.len(), before);
}

// -- the pinned-claude-session-id child (background /fork) --------------
//
// A background `/fork` nests under the forked-from session and must resume the
// FORK's conversation, not a fresh one. `insert_handoff_child` already
// supports that: it rewrites `parent_session_id` and nothing else, so a caller
// pins `claude_session_id` on the session it hands over. These pin that the
// pinned id survives insertion, through both lineage shapes and the refusal.

/// A fork-shaped child: a Claude session pinned to `claude_session_id`,
/// unselected and deferred (its Claude window is not running until the user
/// opens it).
fn make_pinned_fork_session(id: &str, cwd: &str, claude_session_id: &str) -> Session {
    let mut session = make_handoff_session(id, cwd);
    session.title = "⑂ fix the thing".into();
    session.claude_session_id = Some(claude_session_id.into());
    session
}

#[test]
fn handoff_child_keeps_its_pinned_claude_session_id() {
    let mut model = WorkspaceModel::with_fs("/tmp", fake_fs("/home", &[]));
    seed_claude_session(&mut model, "p", "t1", "parent-session", "/tmp/p", true);

    let inserted = model.insert_handoff_child(
        make_pinned_fork_session("fork1", "/tmp/p/worktree", "fork-session-id"),
        "t1",
    );
    assert!(inserted);

    let child = model.session_for("fork1").expect("fork child must exist");
    assert_eq!(
        child.claude_session_id.as_deref(),
        Some("fork-session-id"),
        "the child's pinned claude session id must survive the insert verbatim"
    );
    assert_eq!(child.parent_session_id.as_deref(), Some("t1"), "nested under the forked-from session");
    assert_eq!(child.cwd, "/tmp/p/worktree", "the fork's own worktree cwd is kept");
    assert_eq!(child.title, "⑂ fix the thing");
    assert_eq!(
        model.session_for("t1").unwrap().claude_session_id.as_deref(),
        Some("parent-session"),
        "the anchor session is not rotated onto the fork's id"
    );
}

#[test]
fn handoff_child_of_a_child_pins_its_id_at_depth_1() {
    // Forking from a session that is already a depth-1 child: the fork becomes
    // a sibling under the same root (never depth 2) and still carries its id.
    let mut model = WorkspaceModel::with_fs("/tmp", fake_fs("/home", &[]));
    seed_claude_session(&mut model, "p", "root", "s-root", "/tmp/p", true);
    seed_claude_session(&mut model, "p", "originating", "s-orig", "/tmp/p", true);
    model.mutate_session("originating", |s| s.parent_session_id = Some("root".into()));

    assert!(model.insert_handoff_child(
        make_pinned_fork_session("fork1", "/tmp/p", "fork-session-id"),
        "originating"
    ));

    let child = model.session_for("fork1").expect("fork child must exist");
    assert_eq!(child.parent_session_id.as_deref(), Some("root"), "depth-1: sibling under the root");
    assert_eq!(child.claude_session_id.as_deref(), Some("fork-session-id"));
}

#[test]
fn handoff_child_refused_under_terminals_is_not_inserted_anywhere() {
    // The pinned id changes nothing about the refusal: the Terminals group never
    // hosts Claude, so a fork anchored there is dropped whole rather than
    // leaking a session into another project.
    let mut model = WorkspaceModel::with_fs("/tmp", fake_fs("/home", &[]));
    let before: usize = model.projects.iter().map(|p| p.sessions.len()).sum();

    let inserted = model.insert_handoff_child(
        make_pinned_fork_session("fork1", "/tmp", "fork-session-id"),
        WorkspaceModel::MAIN_TERMINAL_SESSION_ID,
    );

    assert!(!inserted, "Terminals-project anchor must refuse the child");
    assert!(model.session_for("fork1").is_none(), "the refused child must not land in ANY project");
    assert_eq!(model.projects.iter().map(|p| p.sessions.len()).sum::<usize>(), before);
}

// =====================================================================
// ensure_terminals_project_seeded (spawn-hook fire-once)
// =====================================================================

#[test]
fn ensure_existing_terminals_does_not_reorder_or_fire_hook() {
    let mut model = model_empty("/tmp/main"); // Terminals already at index 0
    let fired = Rc::new(Cell::new(0u32));
    let f = fired.clone();
    model.ensure_terminals_project_seeded(|_session| f.set(f.get() + 1));
    assert_eq!(fired.get(), 0, "hook must not fire when Terminals already present");
    assert_eq!(model.projects[0].id, WorkspaceModel::TERMINALS_PROJECT_ID);
}

#[test]
fn ensure_moves_terminals_to_index_zero() {
    let mut model = model_empty("/tmp/main");
    // Displace Terminals to index 1 by inserting a project ahead of it.
    model.projects.insert(0, project_with("other", "OTHER", "/tmp/other", vec![]));
    assert_ne!(model.projects[0].id, WorkspaceModel::TERMINALS_PROJECT_ID);

    let fired = Rc::new(Cell::new(0u32));
    let f = fired.clone();
    model.ensure_terminals_project_seeded(|_session| f.set(f.get() + 1));

    assert_eq!(model.projects[0].id, WorkspaceModel::TERMINALS_PROJECT_ID, "Terminals moved back to index 0");
    assert_eq!(fired.get(), 0, "a mere reorder must not fire the spawn hook");
}

#[test]
fn ensure_seeds_terminals_from_scratch_fires_hook_once() {
    let mut model = model_with("/tmp/main", &[]); // fake home "/home"
    model.projects.clear();

    let seen = Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let s = seen.clone();
    model.ensure_terminals_project_seeded(|session| s.borrow_mut().push(session.id.clone()));

    assert_eq!(model.projects[0].id, WorkspaceModel::TERMINALS_PROJECT_ID);
    let main = model.session_for(WorkspaceModel::MAIN_TERMINAL_SESSION_ID).unwrap();
    assert_eq!(main.windows[0].title, "Terminal 1");
    assert_eq!(main.next_terminal_index, 2);
    assert_eq!(main.cwd, "/home", "synthesized Main session uses the FsProbe home");
    assert_eq!(
        seen.borrow().as_slice(),
        [WorkspaceModel::MAIN_TERMINAL_SESSION_ID.to_string()],
        "spawn hook fires exactly once with the synthesized Main session"
    );
}

// =====================================================================
// Validation invariant spot-probes (plan §Validation §4)
//   §4a (running + deferred-resume Claude coexist) lives in session.rs.
//   §4b (add_window monotonic after close) → add_window_is_monotonic_after_closing_a_window.
//   §4c (/branch on a root re-parents children) →
//       branch_on_root_preserves_depth1_by_reparenting_former_children.
// Plus the pure-helper pins the plan calls out by name.
// =====================================================================

#[test]
fn default_window_title_per_kind() {
    assert_eq!(WorkspaceModel::default_window_title(TermWindowKind::Claude, 0), "Claude");
    assert_eq!(WorkspaceModel::default_window_title(TermWindowKind::Terminal, 7), "Terminal 7");
}

#[test]
fn neighbor_active_window_id_prefers_slot_then_previous_then_none() {
    let windows = vec![terminal("a", "A"), terminal("b", "B")];
    // Removing index 0 (a) from [a,b] leaves [b]; the slot holds b.
    assert_eq!(WorkspaceModel::neighbor_active_window_id(0, &windows), Some("a".into()));
    // Index past the end falls back to the previous (new last).
    assert_eq!(WorkspaceModel::neighbor_active_window_id(2, &windows), Some("b".into()));
    // Empty post-removal array → None.
    assert_eq!(WorkspaceModel::neighbor_active_window_id(0, &[]), None);
}

#[test]
fn apply_auto_title_caps_humanized_title_at_40_chars() {
    // humanize_session_title caps at 40 characters (TabModel.swift:780-783).
    let mut model = model_empty("/tmp/main");
    let id = inject_session(&mut model, "New session", "/tmp/cap-test", TermWindowKind::Terminal);
    // 12 words × "word" split on '-' → "Word word word ..." well over 40 chars.
    model.apply_auto_title(&id, "aaaa-bbbb-cccc-dddd-eeee-ffff-gggg-hhhh-iiii-jjjj");
    let title = model.session_for(&id).unwrap().title.clone();
    assert!(title.chars().count() <= 40, "humanized title must be capped at 40 chars, got {:?}", title);
    assert_eq!(title, "Aaaa bbbb cccc dddd eeee ffff gggg hhhh", "capped on a word boundary, trailing space trimmed");
}


// MARK: - from_parts (R18 restore constructor)

#[test]
fn from_parts_does_not_seed_terminals_or_main() {
    // Unlike `new`/`with_fs`, restore's constructor trusts the saved grouping:
    // no synthesized Terminals project, no Main session.
    let project = Project {
        id: "nice".into(),
        name: "Nice".into(),
        path: "/work".into(),
        sessions: vec![Session::new("t1", "Ship", "/work")],
    };
    let model = WorkspaceModel::from_parts(vec![project], Some("t1".into()), fake_fs("/home", &[]));
    assert_eq!(model.projects.len(), 1);
    assert_eq!(model.projects[0].id, "nice");
    assert!(
        model
            .projects
            .iter()
            .all(|p| p.id != WorkspaceModel::TERMINALS_PROJECT_ID),
        "from_parts must NOT synthesize a Terminals project"
    );
    assert_eq!(model.active_session_id(), Some("t1"));
}

#[test]
fn from_parts_preserves_empty_projects_and_no_active() {
    let model = WorkspaceModel::from_parts(vec![], None, fake_fs("/home", &[]));
    assert!(model.projects.is_empty());
    assert_eq!(model.active_session_id(), None);
}

// MARK: - live_window_counts (W5 quit/close counting)

#[test]
fn live_window_counts_folds_both_kinds_over_is_alive() {
    let mut session = Session::new("t1", "Session", "/w");
    session.windows = vec![
        claude("c1"),
        claude("c2"),
        terminal("term1", "Terminal 1"),
    ];
    let project = Project {
        id: "p".into(),
        name: "P".into(),
        path: "/w".into(),
        sessions: vec![session],
    };
    let model = WorkspaceModel::from_parts(vec![project], Some("t1".into()), fake_fs("/home", &[]));
    assert_eq!(model.live_window_counts(), (2, 1));
}

#[test]
fn live_window_counts_excludes_held_not_alive_windows() {
    let mut held_claude = claude("c1");
    held_claude.is_alive = false;
    let mut session = Session::new("t1", "Session", "/w");
    session.windows = vec![held_claude, terminal("term1", "Terminal 1")];
    let project = Project {
        id: "p".into(),
        name: "P".into(),
        path: "/w".into(),
        sessions: vec![session],
    };
    let model = WorkspaceModel::from_parts(vec![project], Some("t1".into()), fake_fs("/home", &[]));
    assert_eq!(
        model.live_window_counts(),
        (0, 1),
        "a held (not-alive) window must not be counted"
    );
}

#[test]
fn live_window_counts_counts_modelled_but_unspawned_windows() {
    // A restored window hydrates is_alive = true before any pty spawns — it DOES
    // count (the Swift quirk, preserved).
    let persisted = PersistedSession {
        id: "t1".into(),
        title: "Restored".into(),
        cwd: "/w".into(),
        claude_session_id: Some("sid".into()),
        active_window_id: None,
        windows: vec![PersistedTermWindow {
            id: "c".into(),
            title: "Claude".into(),
            kind: TermWindowKind::Claude,
            cwd: None,
            title_manually_set: None,
        }],
        title_manually_set: None,
        parent_session_id: None,
        next_terminal_index: None,
    };
    let project = Project {
        id: "p".into(),
        name: "P".into(),
        path: "/w".into(),
        sessions: vec![persisted.hydrate()],
    };
    let model = WorkspaceModel::from_parts(vec![project], Some("t1".into()), fake_fs("/home", &[]));
    assert_eq!(model.live_window_counts(), (1, 0));
}

// =====================================================================
// fire_mutation coverage matrix (BUGHUNT1-D)
//
// The once-per-window observer subsumes the enumerated per-site session saves
// only if every `&mut self` mutator that changes persisted state fires the
// did-mutate signal on a real change. This matrix pins that: one fresh counting
// observer per mutator, installed AFTER fixture setup so only the mutator under
// test is counted. Read-only / pure helpers, and change-guarded no-ops, must
// never fire.
// =====================================================================

/// Install a fresh mutation counter on `model`, run `act`, and return how many
/// times the observer fired. The counter is installed after the caller has
/// finished seeding the fixture, so fixture setup is never counted.
fn fires_for(mut model: WorkspaceModel, act: impl FnOnce(&mut WorkspaceModel)) -> u32 {
    let counter = mutation_counter(&mut model);
    act(&mut model);
    counter.get()
}

/// Terminals(Main) + a non-terminals project `p` holding one claude session `t1`
/// (windows `t1-claude`, `t1-t1`). `active_session_id` is the seed's Main session, so
/// selecting `t1` is a real change.
fn matrix_fixture() -> WorkspaceModel {
    let mut model = model_empty("/tmp/main");
    seed_claude_session(&mut model, "p", "t1", "S1", "/tmp/p", true);
    model
}

#[test]
fn fire_mutation_matrix_every_persisting_mutator_fires() {
    // --- Selection ---
    assert!(
        fires_for(matrix_fixture(), |m| m.select_session("t1")) >= 1,
        "select_session (real change) must fire"
    );

    // --- Reorder ---
    let two_roots = {
        let mut m = matrix_fixture();
        m.projects[1].sessions.push(new_claude_session("t2", "/tmp/p"));
        m
    };
    assert!(
        fires_for(two_roots, |m| m.move_session("t2", "t1", false)) >= 1,
        "move_session (real reorder) must fire"
    );
    assert!(
        fires_for(matrix_fixture(), |m| m.move_window("t1-t1", "t1", "t1-claude", false)) >= 1,
        "move_window (real reorder) must fire"
    );

    // --- TermWindows: insert / extract / add ---
    assert!(
        fires_for(matrix_fixture(), |m| {
            m.insert_window(terminal("np", "X"), "t1", None, true);
        }) >= 1,
        "insert_window (real insert) must fire"
    );
    assert!(
        fires_for(matrix_fixture(), |m| {
            m.extract_window("t1-t1", "t1");
        }) >= 1,
        "extract_window (real removal) must fire"
    );
    assert!(
        fires_for(matrix_fixture(), |m| {
            m.add_window("t1", "np", None);
        }) >= 1,
        "add_window (real append) must fire"
    );

    // --- Titles / generic mutate ---
    assert!(
        fires_for(matrix_fixture(), |m| m.rename_window("t1", "t1-claude", "Renamed")) >= 1,
        "rename_window (real change) must fire"
    );
    assert!(
        fires_for(matrix_fixture(), |m| m.rename_session("t1", "Renamed")) >= 1,
        "rename_session (real change) must fire"
    );
    assert!(
        fires_for(matrix_fixture(), |m| m.apply_auto_title("t1", "some-generated-title")) >= 1,
        "apply_auto_title (real change) must fire"
    );
    assert!(
        fires_for(matrix_fixture(), |m| {
            m.mutate_session("t1", |s| s.cwd = "/tmp/elsewhere".into());
        }) >= 1,
        "mutate_session (session found) must fire"
    );

    // --- Cwd adoption ---
    assert!(
        fires_for(matrix_fixture(), |m| {
            m.adopt_session_cwd("t1", "/tmp/moved");
        }) >= 1,
        "adopt_session_cwd (real change) must fire"
    );

    // --- Lineage ---
    assert!(
        fires_for(matrix_fixture(), |m| {
            m.insert_branch_parent("t1", "P1", "P1-c", "P1-t", "OLD");
        }) >= 1,
        "insert_branch_parent (real insert) must fire"
    );
    assert!(
        fires_for(matrix_fixture(), |m| {
            m.insert_handoff_child(make_handoff_session("c1", "/tmp/p"), "t1");
        }) >= 1,
        "insert_handoff_child (real insert) must fire"
    );

    // --- Removal + dangling sweeps ---
    assert!(
        fires_for(matrix_fixture(), |m| {
            let (pi, ti) = m.project_session_index("t1").unwrap();
            m.remove_session(pi, ti);
        }) >= 1,
        "remove_session (always removes) must fire"
    );
    let child_of_t1 = {
        let mut m = matrix_fixture();
        let mut t2 = new_claude_session("t2", "/tmp/p");
        t2.parent_session_id = Some("t1".into());
        m.projects[1].sessions.push(t2);
        m
    };
    assert!(
        fires_for(child_of_t1, |m| m.clear_dangling_parent_references("t1")) >= 1,
        "clear_dangling_parent_references (real clear) must fire"
    );
    let orphan = {
        let mut m = matrix_fixture();
        let mut t2 = new_claude_session("t2", "/tmp/p");
        t2.parent_session_id = Some("ghost".into());
        m.projects[1].sessions.push(t2);
        m
    };
    assert!(
        fires_for(orphan, |m| m.prune_dangling_parent_references()) >= 1,
        "prune_dangling_parent_references (real clear) must fire"
    );
    let dup_window_ids = {
        let mut m = matrix_fixture();
        let mut t2 = new_claude_session("t2", "/tmp/p");
        t2.windows = vec![terminal("d", "A"), terminal("d", "B")];
        m.projects[1].sessions.push(t2);
        m
    };
    assert!(
        fires_for(dup_window_ids, |m| m.dedupe_window_ids()) >= 1,
        "dedupe_window_ids (real rename) must fire"
    );

    // --- Project structure ---
    assert!(
        fires_for(matrix_fixture(), |m| {
            m.ensure_project("p-new", "NEW", "/tmp/new");
        }) >= 1,
        "ensure_project (real append) must fire"
    );
    assert!(
        fires_for(matrix_fixture(), |m| {
            m.ensure_project_by_path("p-new2", "NEW2", "/tmp/new2");
        }) >= 1,
        "ensure_project_by_path (real append) must fire"
    );
    let no_terminals = {
        let proj = Project {
            id: "p".into(),
            name: "P".into(),
            path: "/tmp/p".into(),
            sessions: vec![new_claude_session("t1", "/tmp/p")],
        };
        WorkspaceModel::from_parts(vec![proj], Some("t1".into()), fake_fs("/home", &[]))
    };
    assert!(
        fires_for(no_terminals, |m| m.ensure_terminals_project_seeded(|_| {})) >= 1,
        "ensure_terminals_project_seeded (synthesize) must fire"
    );
    let repo_fs = model_with("/tmp/main", &["/tmp/repo", "/tmp/repo/.git"]);
    assert!(
        fires_for(repo_fs, |m| {
            m.add_session_to_projects(new_claude_session("tz", "/tmp/repo"), "/tmp/repo");
        }) >= 1,
        "add_session_to_projects (always adds) must fire"
    );
    assert!(
        fires_for(matrix_fixture(), |m| m.repair_project_structure()) >= 1,
        "repair_project_structure fires unconditionally (D4/D5)"
    );
}

#[test]
fn fire_mutation_matrix_reads_and_no_ops_never_fire() {
    // Pure reads never fire.
    assert_eq!(
        fires_for(matrix_fixture(), |m| {
            let _ = m.session_for("t1");
            let _ = m.project_session_index("t1");
            let _ = m.navigable_sidebar_session_ids();
            let _ = m.live_window_counts();
            let _ = m.would_move_session("t1", "t1", false);
            let _ = m.would_move_window("t1-t1", "t1", "t1-claude", false);
            let _ = m.is_terminals_project_session("t1");
            let _ = m.session_id_owning("t1-claude");
        }),
        0,
        "pure reads must never fire the did-mutate signal"
    );

    // Change-guarded mutators that changed nothing do not fire.
    assert_eq!(
        fires_for(matrix_fixture(), |m| {
            m.mutate_session("ghost", |_| {});
        }),
        0,
        "mutate_session on an unknown session (not found) must not fire"
    );
    assert_eq!(
        fires_for(matrix_fixture(), |m| {
            m.adopt_session_cwd("t1", "/tmp/p");
        }),
        0,
        "adopt_session_cwd to the same cwd is a no-op and must not fire"
    );
    assert_eq!(
        fires_for(matrix_fixture(), |m| m.rename_session("t1", "   ")),
        0,
        "rename_session with empty input is a no-op and must not fire"
    );
    assert_eq!(
        fires_for(matrix_fixture(), |m| m.select_session(WorkspaceModel::MAIN_TERMINAL_SESSION_ID)),
        0,
        "re-selecting the already-active session must not fire"
    );
    assert_eq!(
        fires_for(matrix_fixture(), |m| m.clear_dangling_parent_references("t1")),
        0,
        "clear_dangling_parent_references with nothing pointing at the id must not fire"
    );
    assert_eq!(
        fires_for(matrix_fixture(), |m| m.dedupe_window_ids()),
        0,
        "dedupe_window_ids with all-unique ids is a no-op and must not fire"
    );
}
