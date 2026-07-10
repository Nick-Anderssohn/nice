//! Ported `TabModel` behavior suites from `Tests/NiceUnitTests/`. Each Swift
//! `TabModel*Tests` / `AppStateBranchTrackingTests` / `PaneNamingTests` case's
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
use crate::{PersistedPane, PersistedTab};

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
fn model_with(cwd: &str, paths: &[&str]) -> TabModel {
    TabModel::with_fs(cwd, fake_fs("/home", paths))
}

/// A model with an empty fake fs (nothing exists), home `/home`.
fn model_empty(cwd: &str) -> TabModel {
    model_with(cwd, &[])
}

// MARK: - Pane / tab builders

fn claude(id: &str) -> Pane {
    Pane::new(id, "Claude", PaneKind::Claude)
}

fn terminal(id: &str, title: &str) -> Pane {
    Pane::new(id, title, PaneKind::Terminal)
}

/// Mirror of `TabModelFixtures.seedClaudeTab`: append a Claude + terminal tab
/// under `project_id` (creating the project). Returns `(claude_pane_id,
/// terminal_pane_id)`. Claude pane id `<tab>-claude`, terminal `<tab>-t1`.
fn seed_claude_tab(
    model: &mut TabModel,
    project_id: &str,
    tab_id: &str,
    session_id: &str,
    path: &str,
    is_claude_running: bool,
) -> (String, String) {
    let claude_pane_id = format!("{}-claude", tab_id);
    let terminal_pane_id = format!("{}-t1", tab_id);
    let mut claude_pane = claude(&claude_pane_id);
    claude_pane.is_claude_running = is_claude_running;
    let mut tab = Tab::new(tab_id, "New tab", path);
    tab.panes = vec![claude_pane, terminal(&terminal_pane_id, "Terminal 1")];
    tab.active_pane_id = Some(claude_pane_id.clone());
    tab.claude_session_id = Some(session_id.to_string());
    model.projects.push(Project {
        id: project_id.into(),
        name: project_id.to_uppercase(),
        path: path.into(),
        tabs: vec![tab],
    });
    (claude_pane_id, terminal_pane_id)
}

/// Mirror of `TabModelFixtures.seedTerminalProject`: a bare project with a
/// single seed terminal tab.
fn seed_terminal_project(model: &mut TabModel, id: &str, name: &str, path: &str) {
    let seed_tab_id = format!("{}-seed", id);
    let seed_pane_id = format!("{}-seed-p0", id);
    let mut tab = Tab::new(&seed_tab_id, "seed", path);
    tab.panes = vec![terminal(&seed_pane_id, "zsh")];
    tab.active_pane_id = Some(seed_pane_id);
    model.projects.push(Project {
        id: id.into(),
        name: name.into(),
        path: path.into(),
        tabs: vec![tab],
    });
}

fn project_by_id<'a>(model: &'a TabModel, id: &str) -> &'a Project {
    model
        .projects
        .iter()
        .find(|p| p.id == id)
        .unwrap_or_else(|| panic!("project '{}' not found", id))
}

fn tab_ids_in(model: &TabModel, project_id: &str) -> Vec<String> {
    project_by_id(model, project_id)
        .tabs
        .iter()
        .map(|t| t.id.clone())
        .collect()
}

fn pane_ids(model: &TabModel, tab_id: &str) -> Vec<String> {
    model
        .tab_for(tab_id)
        .map(|t| t.panes.iter().map(|p| p.id.clone()).collect())
        .unwrap_or_default()
}

/// Install a mutation counter and return the shared cell the callback bumps.
fn mutation_counter(model: &mut TabModel) -> Rc<Cell<u32>> {
    let counter = Rc::new(Cell::new(0u32));
    let c = counter.clone();
    model.set_on_tree_mutation(move || c.set(c.get() + 1));
    counter
}

// =====================================================================
// TabModelCwdResolutionTests
// =====================================================================

/// Seed a terminal tab (one pane) under project `p`, mirroring the Swift
/// `seedTerminalTab` helper.
fn seed_terminal_tab(
    model: &mut TabModel,
    tab_id: &str,
    pane_id: &str,
    tab_cwd: &str,
    pane_cwd: Option<&str>,
) {
    let mut pane = terminal(pane_id, "zsh");
    pane.cwd = pane_cwd.map(|s| s.to_string());
    let mut tab = Tab::new(tab_id, "Terminal", tab_cwd);
    tab.panes = vec![pane];
    tab.active_pane_id = Some(pane_id.to_string());
    model.projects.push(Project {
        id: "p".into(),
        name: "P".into(),
        path: tab_cwd.into(),
        tabs: vec![tab],
    });
}

#[test]
fn resolved_spawn_cwd_prefers_pane_cwd_when_it_exists() {
    let dir = "/tmp/live-dir";
    let mut model = model_with("/tmp/main", &[dir]);
    seed_terminal_tab(&mut model, "t1", "p1", "/tmp", Some(dir));
    let tab = model.tab_for("t1").unwrap().clone();
    let pane = tab.panes[0].clone();
    assert_eq!(model.resolved_spawn_cwd_for_pane(&tab, &pane), dir);
}

#[test]
fn resolved_spawn_cwd_falls_back_when_pane_cwd_missing() {
    let live = "/tmp/live";
    let dead = "/tmp/dead";
    // Only `live` exists on the fake fs; the pane cwd (`dead`) was deleted.
    let mut model = model_with("/tmp/main", &[live]);
    seed_terminal_tab(&mut model, "t1", "p1", live, Some(dead));
    let tab = model.tab_for("t1").unwrap().clone();
    let pane = tab.panes[0].clone();
    assert_eq!(
        model.resolved_spawn_cwd_for_pane(&tab, &pane),
        live,
        "deleted pane cwd must fall back to the tab's cwd"
    );
}

#[test]
fn resolved_spawn_cwd_nil_pane_cwd_falls_back_to_tab() {
    let live = "/tmp/live";
    let mut model = model_with("/tmp/main", &[live]);
    seed_terminal_tab(&mut model, "t1", "p1", live, None);
    let tab = model.tab_for("t1").unwrap().clone();
    let pane = tab.panes[0].clone();
    assert!(pane.cwd.is_none());
    assert_eq!(model.resolved_spawn_cwd_for_pane(&tab, &pane), live);
}

#[test]
fn spawn_cwd_for_new_pane_caller_provided_wins() {
    let live = "/tmp/live";
    let mut model = model_with("/tmp/main", &[live]);
    seed_terminal_tab(&mut model, "t1", "p1", live, Some(live));
    let tab = model.tab_for("t1").unwrap().clone();
    assert_eq!(
        model.spawn_cwd_for_new_pane(&tab, Some("/explicit")),
        "/explicit",
        "an explicit caller cwd must win over inheritance"
    );
}

#[test]
fn spawn_cwd_for_new_pane_inherits_active_pane_cwd() {
    let tab_dir = "/tmp/tab-dir";
    let pane_dir = "/tmp/pane-dir";
    let mut model = model_with("/tmp/main", &[tab_dir, pane_dir]);
    seed_terminal_tab(&mut model, "t1", "p1", tab_dir, Some(pane_dir));
    let tab = model.tab_for("t1").unwrap().clone();
    assert_eq!(model.spawn_cwd_for_new_pane(&tab, None), pane_dir);
}

#[test]
fn spawn_cwd_for_new_pane_falls_back_to_tab_cwd_when_no_active_pane() {
    let tab_dir = "/tmp/tab-dir";
    let model = model_empty("/tmp/main");
    // Tab with no active pane and no panes — nothing to inherit.
    let tab = Tab::new("t1", "Terminal", tab_dir);
    assert_eq!(model.spawn_cwd_for_new_pane(&tab, None), tab_dir);
}

#[test]
fn adopt_tab_cwd_unknown_tab_id_returns_false_no_mutation() {
    let mut model = model_empty("/tmp/main");
    seed_claude_tab(&mut model, "p", "t-known", "s", "/tmp/known", true);
    let pre = model.tab_for("t-known").unwrap().clone();
    let changed = model.adopt_tab_cwd("t-ghost", "/tmp/anywhere");
    assert!(!changed, "unknown tab id must return false");
    assert_eq!(
        model.tab_for("t-known").unwrap(),
        &pre,
        "siblings must not change when an unknown id is passed"
    );
}

#[test]
fn adopt_tab_cwd_same_cwd_returns_false_panes_untouched() {
    let mut model = model_empty("/tmp/main");
    let (_c, term) = seed_claude_tab(&mut model, "p", "t-same", "s", "/tmp/same", true);
    model.mutate_tab("t-same", |tab| {
        if let Some(p) = tab.panes.iter_mut().find(|p| p.id == term) {
            p.cwd = Some("/tmp/same".into());
        }
    });
    let pre = model.tab_for("t-same").unwrap().clone();
    let changed = model.adopt_tab_cwd("t-same", "/tmp/same");
    assert!(!changed, "same cwd must short-circuit to false");
    assert_eq!(
        model.tab_for("t-same").unwrap(),
        &pre,
        "no-op rotation must leave every pane (incl. nil ones) unchanged"
    );
}

#[test]
fn adopt_tab_cwd_different_cwd_returns_true_tab_updated() {
    let mut model = model_empty("/tmp/main");
    seed_claude_tab(&mut model, "p", "t-rotate", "s", "/tmp/before", true);
    let changed = model.adopt_tab_cwd("t-rotate", "/tmp/after");
    assert!(changed, "different cwd must return true");
    assert_eq!(model.tab_for("t-rotate").unwrap().cwd, "/tmp/after");
}

#[test]
fn adopt_tab_cwd_pane_policy_matching_follows() {
    let mut model = model_empty("/tmp/main");
    let (_c, term) = seed_claude_tab(&mut model, "p", "t-match", "s", "/tmp/old", true);
    model.mutate_tab("t-match", |tab| {
        if let Some(p) = tab.panes.iter_mut().find(|p| p.id == term) {
            p.cwd = Some("/tmp/old".into());
        }
    });
    assert!(model.adopt_tab_cwd("t-match", "/tmp/new"));
    let pane_cwd = model
        .tab_for("t-match")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == term)
        .unwrap()
        .cwd
        .clone();
    assert_eq!(
        pane_cwd.as_deref(),
        Some("/tmp/new"),
        "pane that matched the old tab.cwd must follow into new_cwd"
    );
}

#[test]
fn adopt_tab_cwd_pane_policy_nil_follows() {
    let mut model = model_empty("/tmp/main");
    let (claude_pane, _t) = seed_claude_tab(&mut model, "p", "t-nil", "s", "/tmp/old", true);
    assert!(
        model
            .tab_for("t-nil")
            .unwrap()
            .panes
            .iter()
            .find(|p| p.id == claude_pane)
            .unwrap()
            .cwd
            .is_none(),
        "precondition: Claude pane starts with nil cwd"
    );
    assert!(model.adopt_tab_cwd("t-nil", "/tmp/new"));
    let pane_cwd = model
        .tab_for("t-nil")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == claude_pane)
        .unwrap()
        .cwd
        .clone();
    assert_eq!(
        pane_cwd.as_deref(),
        Some("/tmp/new"),
        "nil-cwd pane must follow the tab into new_cwd"
    );
}

#[test]
fn adopt_tab_cwd_pane_policy_diverged_stays() {
    let mut model = model_empty("/tmp/main");
    let (_c, term) = seed_claude_tab(&mut model, "p", "t-div", "s", "/tmp/old", true);
    model.mutate_tab("t-div", |tab| {
        if let Some(p) = tab.panes.iter_mut().find(|p| p.id == term) {
            p.cwd = Some("/tmp/somewhere-else".into());
        }
    });
    assert!(model.adopt_tab_cwd("t-div", "/tmp/new"));
    let pane_cwd = model
        .tab_for("t-div")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == term)
        .unwrap()
        .cwd
        .clone();
    assert_eq!(
        pane_cwd.as_deref(),
        Some("/tmp/somewhere-else"),
        "diverged pane must keep its user-chosen cwd across the rotation"
    );
}

#[test]
fn adopt_tab_cwd_mixed_panes_applies_policy_per_pane() {
    let mut model = model_empty("/tmp/main");
    let (claude_pane, term) = seed_claude_tab(&mut model, "p", "t-mixed", "s", "/tmp/old", true);
    let extra = "t-mixed-t2".to_string();
    model.mutate_tab("t-mixed", |tab| {
        // Claude pane stays nil (nil follows). Terminal pane at /tmp/old
        // (matching-old follows).
        if let Some(p) = tab.panes.iter_mut().find(|p| p.id == term) {
            p.cwd = Some("/tmp/old".into());
        }
        let mut diverged = terminal(&extra, "Terminal 2");
        diverged.cwd = Some("/tmp/diverged".into());
        tab.panes.push(diverged);
    });
    assert!(model.adopt_tab_cwd("t-mixed", "/tmp/new"));
    let panes = model.tab_for("t-mixed").unwrap().panes.clone();
    let cwd_of = |id: &str| {
        panes
            .iter()
            .find(|p| p.id == id)
            .unwrap()
            .cwd
            .clone()
            .unwrap_or_default()
    };
    assert_eq!(cwd_of(&claude_pane), "/tmp/new", "nil pane must follow");
    assert_eq!(cwd_of(&term), "/tmp/new", "matching-old pane must follow");
    assert_eq!(
        cwd_of("t-mixed-t2"),
        "/tmp/diverged",
        "diverged pane must stay put — pane policy is per-pane, not all-or-nothing"
    );
}

// =====================================================================
// TabModelInsertExtractPaneTests
// =====================================================================

/// Seed the [p0, p1, p2] terminal-pane fixture into a fresh project `ie`
/// alongside the pinned Terminals group.
fn ie_model() -> TabModel {
    let mut model = model_empty("/tmp/main");
    let mut tab = Tab::new("ie-tab", "Insert/extract test", "/tmp/ie");
    tab.panes = vec![
        terminal("ie-tab-p0", "Terminal 1"),
        terminal("ie-tab-p1", "Terminal 2"),
        terminal("ie-tab-p2", "Terminal 3"),
    ];
    tab.active_pane_id = Some("ie-tab-p0".into());
    model.projects.push(Project {
        id: "ie".into(),
        name: "IE".into(),
        path: "/tmp/ie".into(),
        tabs: vec![tab],
    });
    model
}

#[test]
fn extract_pane_removes_and_returns_pane() {
    let mut model = ie_model();
    let removed = model.extract_pane("ie-tab-p1", "ie-tab");
    assert_eq!(removed.map(|p| p.id), Some("ie-tab-p1".to_string()));
    assert_eq!(pane_ids(&model, "ie-tab"), ["ie-tab-p0", "ie-tab-p2"]);
}

#[test]
fn extract_pane_non_active_leaves_active_unchanged() {
    let mut model = ie_model();
    model.extract_pane("ie-tab-p1", "ie-tab");
    assert_eq!(
        model.tab_for("ie-tab").unwrap().active_pane_id.as_deref(),
        Some("ie-tab-p0")
    );
}

#[test]
fn extract_pane_active_refocuses_slot_neighbor() {
    let mut model = ie_model();
    model.mutate_tab("ie-tab", |t| t.active_pane_id = Some("ie-tab-p1".into()));
    model.extract_pane("ie-tab-p1", "ie-tab");
    assert_eq!(
        model.tab_for("ie-tab").unwrap().active_pane_id.as_deref(),
        Some("ie-tab-p2"),
        "removing the middle active pane focuses the pane that slid into its slot"
    );
}

#[test]
fn extract_pane_active_last_refocuses_previous() {
    let mut model = ie_model();
    model.mutate_tab("ie-tab", |t| t.active_pane_id = Some("ie-tab-p2".into()));
    model.extract_pane("ie-tab-p2", "ie-tab");
    assert_eq!(
        model.tab_for("ie-tab").unwrap().active_pane_id.as_deref(),
        Some("ie-tab-p1")
    );
}

#[test]
fn extract_pane_last_remaining_clears_active() {
    let mut model = ie_model();
    model.extract_pane("ie-tab-p1", "ie-tab");
    model.extract_pane("ie-tab-p2", "ie-tab");
    model.mutate_tab("ie-tab", |t| t.active_pane_id = Some("ie-tab-p0".into()));
    model.extract_pane("ie-tab-p0", "ie-tab");
    assert!(pane_ids(&model, "ie-tab").is_empty());
    assert!(model.tab_for("ie-tab").unwrap().active_pane_id.is_none());
}

#[test]
fn extract_pane_unknown_pane_returns_nil_no_mutation() {
    let mut model = ie_model();
    let counter = mutation_counter(&mut model);
    let removed = model.extract_pane("ghost", "ie-tab");
    assert!(removed.is_none());
    assert_eq!(pane_ids(&model, "ie-tab"), ["ie-tab-p0", "ie-tab-p1", "ie-tab-p2"]);
    assert_eq!(counter.get(), 0);
}

#[test]
fn extract_pane_real_removal_fires_on_tree_mutation_once() {
    let mut model = ie_model();
    let counter = mutation_counter(&mut model);
    model.extract_pane("ie-tab-p1", "ie-tab");
    assert_eq!(counter.get(), 1);
}

#[test]
fn insert_pane_before_target() {
    let mut model = ie_model();
    model.insert_pane(terminal("fx", "Foreign"), "ie-tab", Some("ie-tab-p1"), false);
    assert_eq!(
        pane_ids(&model, "ie-tab"),
        ["ie-tab-p0", "fx", "ie-tab-p1", "ie-tab-p2"]
    );
}

#[test]
fn insert_pane_after_target() {
    let mut model = ie_model();
    model.insert_pane(terminal("fx", "Foreign"), "ie-tab", Some("ie-tab-p1"), true);
    assert_eq!(
        pane_ids(&model, "ie-tab"),
        ["ie-tab-p0", "ie-tab-p1", "fx", "ie-tab-p2"]
    );
}

#[test]
fn insert_pane_nil_target_appends() {
    let mut model = ie_model();
    model.insert_pane(terminal("fx", "Foreign"), "ie-tab", None, false);
    assert_eq!(
        pane_ids(&model, "ie-tab"),
        ["ie-tab-p0", "ie-tab-p1", "ie-tab-p2", "fx"]
    );
}

#[test]
fn insert_pane_unknown_target_appends() {
    let mut model = ie_model();
    model.insert_pane(terminal("fx", "Foreign"), "ie-tab", Some("ghost"), true);
    assert_eq!(
        pane_ids(&model, "ie-tab"),
        ["ie-tab-p0", "ie-tab-p1", "ie-tab-p2", "fx"]
    );
}

#[test]
fn insert_pane_duplicate_id_is_no_op() {
    let mut model = ie_model();
    let counter = mutation_counter(&mut model);
    model.insert_pane(terminal("ie-tab-p1", "Dup"), "ie-tab", Some("ie-tab-p0"), true);
    assert_eq!(pane_ids(&model, "ie-tab"), ["ie-tab-p0", "ie-tab-p1", "ie-tab-p2"]);
    assert_eq!(counter.get(), 0);
}

#[test]
fn insert_pane_does_not_change_active_pane_id() {
    let mut model = ie_model();
    model.insert_pane(terminal("fx", "Foreign"), "ie-tab", Some("ie-tab-p1"), false);
    assert_eq!(
        model.tab_for("ie-tab").unwrap().active_pane_id.as_deref(),
        Some("ie-tab-p0")
    );
}

#[test]
fn insert_pane_real_insert_fires_on_tree_mutation_once() {
    let mut model = ie_model();
    let counter = mutation_counter(&mut model);
    model.insert_pane(terminal("fx", "Foreign"), "ie-tab", Some("ie-tab-p1"), false);
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
    let idx = model.ensure_project_by_path(TabModel::TERMINALS_PROJECT_ID, "Terminals", "/some/other/path");
    assert_eq!(idx, 0, "reserved Terminals id resolves to the pinned project at index 0");
    assert_eq!(model.projects.len(), before, "must not append a duplicate Terminals project");
    assert_eq!(
        model
            .projects
            .iter()
            .filter(|p| p.id == TabModel::TERMINALS_PROJECT_ID)
            .count(),
        1
    );
}

// =====================================================================
// TabModelMovePaneTests
// =====================================================================

/// Seed the [p0, p1, p2] fixture into project `mp`.
fn mp_model() -> TabModel {
    let mut model = model_empty("/tmp/main");
    let mut tab = Tab::new("mp-tab", "Move-pane test", "/tmp/mp");
    tab.panes = vec![
        terminal("mp-tab-p0", "Terminal 1"),
        terminal("mp-tab-p1", "Terminal 2"),
        terminal("mp-tab-p2", "Terminal 3"),
    ];
    tab.active_pane_id = Some("mp-tab-p0".into());
    model.projects.push(Project {
        id: "mp".into(),
        name: "MP".into(),
        path: "/tmp/mp".into(),
        tabs: vec![tab],
    });
    model
}

fn mp_pane_ids(model: &TabModel) -> Vec<String> {
    pane_ids(model, "mp-tab")
}

#[test]
fn move_pane_before_moves_source_into_target_slot() {
    let mut model = mp_model();
    model.move_pane("mp-tab-p2", "mp-tab", "mp-tab-p0", false);
    assert_eq!(mp_pane_ids(&model), ["mp-tab-p2", "mp-tab-p0", "mp-tab-p1"]);
}

#[test]
fn move_pane_after_lands_just_past_target() {
    let mut model = mp_model();
    model.move_pane("mp-tab-p0", "mp-tab", "mp-tab-p1", true);
    assert_eq!(mp_pane_ids(&model), ["mp-tab-p1", "mp-tab-p0", "mp-tab-p2"]);
}

#[test]
fn move_pane_after_last_pane_moves_to_end() {
    let mut model = mp_model();
    model.move_pane("mp-tab-p0", "mp-tab", "mp-tab-p2", true);
    assert_eq!(mp_pane_ids(&model), ["mp-tab-p1", "mp-tab-p2", "mp-tab-p0"]);
}

#[test]
fn move_pane_remove_shifts_insert_boundary_lands_correctly() {
    let mut model = mp_model();
    // src=0, dst=1, placeAfter → insertIndex 2 before shift; src<insert so 1.
    model.move_pane("mp-tab-p0", "mp-tab", "mp-tab-p1", true);
    assert_eq!(mp_pane_ids(&model), ["mp-tab-p1", "mp-tab-p0", "mp-tab-p2"]);
}

#[test]
fn move_pane_same_id_is_no_op() {
    let mut model = mp_model();
    let before = mp_pane_ids(&model);
    model.move_pane("mp-tab-p0", "mp-tab", "mp-tab-p0", false);
    assert_eq!(mp_pane_ids(&model), before);
}

#[test]
fn move_pane_adjacent_after_predecessor_is_no_op() {
    let mut model = mp_model();
    let before = mp_pane_ids(&model);
    model.move_pane("mp-tab-p1", "mp-tab", "mp-tab-p0", true);
    assert_eq!(mp_pane_ids(&model), before);
}

#[test]
fn move_pane_adjacent_before_successor_is_no_op() {
    let mut model = mp_model();
    let before = mp_pane_ids(&model);
    model.move_pane("mp-tab-p0", "mp-tab", "mp-tab-p1", false);
    assert_eq!(mp_pane_ids(&model), before);
}

#[test]
fn move_pane_unknown_pane_id_is_no_op() {
    let mut model = mp_model();
    let before = mp_pane_ids(&model);
    model.move_pane("ghost", "mp-tab", "mp-tab-p0", true);
    assert_eq!(mp_pane_ids(&model), before);
}

#[test]
fn move_pane_unknown_target_id_is_no_op() {
    let mut model = mp_model();
    let before = mp_pane_ids(&model);
    model.move_pane("mp-tab-p0", "mp-tab", "ghost", false);
    assert_eq!(mp_pane_ids(&model), before);
}

#[test]
fn move_pane_unknown_tab_id_is_no_op() {
    let mut model = mp_model();
    let before = mp_pane_ids(&model);
    model.move_pane("mp-tab-p0", "ghost-tab", "mp-tab-p1", false);
    assert_eq!(mp_pane_ids(&model), before);
}

#[test]
fn move_pane_real_move_fires_on_tree_mutation_once() {
    let mut model = mp_model();
    let counter = mutation_counter(&mut model);
    model.move_pane("mp-tab-p0", "mp-tab", "mp-tab-p2", true);
    assert_eq!(counter.get(), 1, "a real reorder fires exactly once");
}

#[test]
fn move_pane_same_id_does_not_fire() {
    let mut model = mp_model();
    let counter = mutation_counter(&mut model);
    model.move_pane("mp-tab-p0", "mp-tab", "mp-tab-p0", false);
    assert_eq!(counter.get(), 0);
}

#[test]
fn move_pane_adjacent_no_op_does_not_fire() {
    let mut model = mp_model();
    let counter = mutation_counter(&mut model);
    model.move_pane("mp-tab-p1", "mp-tab", "mp-tab-p0", true);
    assert_eq!(counter.get(), 0);
}

#[test]
fn move_pane_unknown_pane_id_does_not_fire() {
    let mut model = mp_model();
    let counter = mutation_counter(&mut model);
    model.move_pane("ghost", "mp-tab", "mp-tab-p0", true);
    assert_eq!(counter.get(), 0);
}

#[test]
fn move_pane_unknown_tab_id_does_not_fire() {
    let mut model = mp_model();
    let counter = mutation_counter(&mut model);
    model.move_pane("mp-tab-p0", "ghost-tab", "mp-tab-p1", false);
    assert_eq!(counter.get(), 0);
}

#[test]
fn move_pane_does_not_change_active_pane_id() {
    let mut model = mp_model();
    let before = model.tab_for("mp-tab").unwrap().active_pane_id.clone();
    model.move_pane("mp-tab-p0", "mp-tab", "mp-tab-p2", true);
    assert_eq!(model.tab_for("mp-tab").unwrap().active_pane_id, before);
}

#[test]
fn would_move_pane_real_move_is_true() {
    let model = mp_model();
    assert!(model.would_move_pane("mp-tab-p2", "mp-tab", "mp-tab-p0", false));
}

#[test]
fn would_move_pane_same_id_is_false() {
    let model = mp_model();
    assert!(!model.would_move_pane("mp-tab-p0", "mp-tab", "mp-tab-p0", false));
}

#[test]
fn would_move_pane_adjacent_no_op_is_false() {
    let model = mp_model();
    assert!(!model.would_move_pane("mp-tab-p1", "mp-tab", "mp-tab-p0", true));
    assert!(!model.would_move_pane("mp-tab-p1", "mp-tab", "mp-tab-p2", false));
}

#[test]
fn would_move_pane_unknown_pane_id_is_false() {
    let model = mp_model();
    assert!(!model.would_move_pane("ghost", "mp-tab", "mp-tab-p0", true));
}

#[test]
fn would_move_pane_unknown_target_id_is_false() {
    let model = mp_model();
    assert!(!model.would_move_pane("mp-tab-p0", "mp-tab", "ghost", false));
}

#[test]
fn would_move_pane_unknown_tab_id_is_false() {
    let model = mp_model();
    assert!(!model.would_move_pane("mp-tab-p0", "ghost-tab", "mp-tab-p1", true));
}

// =====================================================================
// TabModelReorderTests
// =====================================================================

/// Two projects, 3 and 2 tabs, each tab one terminal pane. Replaces the whole
/// projects array (no Terminals), mirroring the Swift `seedTwoProjects`.
fn reorder_two_projects() -> TabModel {
    let mut model = model_empty("/tmp/main");
    model.projects = vec![
        make_project("p1", "P1", 3),
        make_project("p2", "P2", 2),
    ];
    model
}

fn make_project(id: &str, name: &str, tab_count: usize) -> Project {
    let tabs = (0..tab_count)
        .map(|i| {
            let tid = format!("{}t{}", id, i);
            let pid = format!("{}t{}-p0", id, i);
            let mut tab = Tab::new(&tid, format!("{}-T{}", name, i), format!("/tmp/{}", id));
            tab.panes = vec![terminal(&pid, "zsh")];
            tab.active_pane_id = Some(pid);
            tab
        })
        .collect();
    Project {
        id: id.into(),
        name: name.into(),
        path: format!("/tmp/{}", id),
        tabs,
    }
}

#[test]
fn move_tab_before_moves_source_into_target_slot() {
    let mut model = reorder_two_projects();
    model.move_tab("p1t2", "p1t0", false);
    assert_eq!(tab_ids_in(&model, "p1"), ["p1t2", "p1t0", "p1t1"]);
}

#[test]
fn move_tab_after_lands_just_past_target() {
    let mut model = reorder_two_projects();
    model.move_tab("p1t0", "p1t1", true);
    assert_eq!(tab_ids_in(&model, "p1"), ["p1t1", "p1t0", "p1t2"]);
}

#[test]
fn move_tab_after_last_tab_moves_to_end() {
    let mut model = reorder_two_projects();
    model.move_tab("p1t0", "p1t2", true);
    assert_eq!(tab_ids_in(&model, "p1"), ["p1t1", "p1t2", "p1t0"]);
}

#[test]
fn move_tab_adjacent_after_predecessor_is_no_op() {
    let mut model = reorder_two_projects();
    let before = tab_ids_in(&model, "p1");
    model.move_tab("p1t1", "p1t0", true);
    assert_eq!(tab_ids_in(&model, "p1"), before);
}

#[test]
fn move_tab_adjacent_before_successor_is_no_op() {
    let mut model = reorder_two_projects();
    let before = tab_ids_in(&model, "p1");
    model.move_tab("p1t0", "p1t1", false);
    assert_eq!(tab_ids_in(&model, "p1"), before);
}

#[test]
fn move_tab_same_id_is_no_op() {
    let mut model = reorder_two_projects();
    let before = tab_ids_in(&model, "p1");
    model.move_tab("p1t0", "p1t0", false);
    assert_eq!(tab_ids_in(&model, "p1"), before);
}

#[test]
fn move_tab_across_projects_is_no_op() {
    let mut model = reorder_two_projects();
    let p1_before = tab_ids_in(&model, "p1");
    let p2_before = tab_ids_in(&model, "p2");
    model.move_tab("p1t0", "p2t0", false);
    assert_eq!(tab_ids_in(&model, "p1"), p1_before);
    assert_eq!(tab_ids_in(&model, "p2"), p2_before);
}

#[test]
fn move_tab_unknown_source_is_no_op() {
    let mut model = reorder_two_projects();
    let before = tab_ids_in(&model, "p1");
    model.move_tab("ghost", "p1t0", true);
    assert_eq!(tab_ids_in(&model, "p1"), before);
}

#[test]
fn move_tab_unknown_target_is_no_op() {
    let mut model = reorder_two_projects();
    let before = tab_ids_in(&model, "p1");
    model.move_tab("p1t0", "ghost", false);
    assert_eq!(tab_ids_in(&model, "p1"), before);
}

#[test]
fn would_move_tab_real_move_is_true() {
    let model = reorder_two_projects();
    assert!(model.would_move_tab("p1t2", "p1t0", false));
}

#[test]
fn would_move_tab_same_id_is_false() {
    let model = reorder_two_projects();
    assert!(!model.would_move_tab("p1t0", "p1t0", false));
}

#[test]
fn would_move_tab_adjacent_no_op_is_false() {
    let model = reorder_two_projects();
    assert!(!model.would_move_tab("p1t1", "p1t0", true));
    assert!(!model.would_move_tab("p1t1", "p1t2", false));
}

#[test]
fn would_move_tab_cross_project_is_false() {
    let model = reorder_two_projects();
    assert!(!model.would_move_tab("p1t0", "p2t0", false));
}

#[test]
fn move_tab_within_terminals_project_reorders() {
    // Terminals with [Main, term-t1, term-t2] + one user project.
    let mut model = model_empty("/tmp/main");
    let mut terminals = Project {
        id: TabModel::TERMINALS_PROJECT_ID.into(),
        name: "Terminals".into(),
        path: "/tmp/terminals".into(),
        tabs: vec![],
    };
    for (tid, title) in [
        (TabModel::MAIN_TERMINAL_TAB_ID, "Main"),
        ("term-t1", "Term 1"),
        ("term-t2", "Term 2"),
    ] {
        let pid = format!("{}-p0", tid);
        let mut tab = Tab::new(tid, title, "/tmp/terminals");
        tab.panes = vec![terminal(&pid, "zsh")];
        tab.active_pane_id = Some(pid);
        terminals.tabs.push(tab);
    }
    model.projects = vec![terminals, make_project("p1", "P1", 2)];

    model.move_tab("term-t2", TabModel::MAIN_TERMINAL_TAB_ID, false);
    assert_eq!(
        tab_ids_in(&model, TabModel::TERMINALS_PROJECT_ID),
        ["term-t2", TabModel::MAIN_TERMINAL_TAB_ID, "term-t1"]
    );
}

#[test]
fn move_tab_terminals_to_user_project_is_no_op() {
    let mut model = model_empty("/tmp/main");
    let mut terminals = Project {
        id: TabModel::TERMINALS_PROJECT_ID.into(),
        name: "Terminals".into(),
        path: "/tmp/terminals".into(),
        tabs: vec![],
    };
    for (tid, title) in [
        (TabModel::MAIN_TERMINAL_TAB_ID, "Main"),
        ("term-t1", "Term 1"),
    ] {
        let pid = format!("{}-p0", tid);
        let mut tab = Tab::new(tid, title, "/tmp/terminals");
        tab.panes = vec![terminal(&pid, "zsh")];
        tab.active_pane_id = Some(pid);
        terminals.tabs.push(tab);
    }
    model.projects = vec![terminals, make_project("p1", "P1", 2)];

    let term_before = tab_ids_in(&model, TabModel::TERMINALS_PROJECT_ID);
    let p1_before = tab_ids_in(&model, "p1");
    model.move_tab(TabModel::MAIN_TERMINAL_TAB_ID, "p1t0", true);
    assert_eq!(tab_ids_in(&model, TabModel::TERMINALS_PROJECT_ID), term_before);
    assert_eq!(tab_ids_in(&model, "p1"), p1_before);
}

// =====================================================================
// TabModelSubtreeReorderTests (M7.8 round 3 — parent drags move the block)
// =====================================================================

/// One project shaped like the repro tree:
/// `[A, A1*, A2*, B, B1*, C]` where `*` marks a depth-1 child (parent in
/// brackets): A1/A2 under A, B1 under B, C standalone.
fn lineage_project_model() -> TabModel {
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
        let mut tab = Tab::new(tid, tid, "/tmp/p1");
        tab.panes = vec![terminal(&pid, "zsh")];
        tab.active_pane_id = Some(pid);
        tab.parent_tab_id = parent.map(str::to_string);
        project.tabs.push(tab);
    }
    model.projects = vec![project];
    model
}

#[test]
fn move_parent_after_block_carries_whole_subtree() {
    let mut model = lineage_project_model();
    // Drop A after C's block (C is standalone): the whole A block moves,
    // children in order, still nested.
    model.move_tab("A", "C", true);
    assert_eq!(tab_ids_in(&model, "p1"), ["B", "B1", "C", "A", "A1", "A2"]);
    assert_eq!(model.tab_for("A1").unwrap().parent_tab_id.as_deref(), Some("A"));
    assert_eq!(model.tab_for("A2").unwrap().parent_tab_id.as_deref(), Some("A"));
}

#[test]
fn move_parent_before_block_carries_whole_subtree() {
    let mut model = lineage_project_model();
    // Drop B's block before A's block.
    model.move_tab("B", "A", false);
    assert_eq!(tab_ids_in(&model, "p1"), ["B", "B1", "A", "A1", "A2", "C"]);
}

#[test]
fn move_parent_onto_own_child_is_no_op() {
    let mut model = lineage_project_model();
    let before = tab_ids_in(&model, "p1");
    for (target, after) in [("A1", false), ("A1", true), ("A2", false), ("A2", true)] {
        model.move_tab("A", target, after);
        assert_eq!(tab_ids_in(&model, "p1"), before, "A onto {target}/{after}");
        assert!(!model.would_move_tab("A", target, after));
    }
}

#[test]
fn move_parent_just_after_target_root_lands_after_its_block() {
    let mut model = lineage_project_model();
    // "Just after B's row" is an interior slot (between B and B1) —
    // normalizes to after B's whole block, keeping the block contiguous.
    model.move_tab("C", "B", true);
    assert_eq!(tab_ids_in(&model, "p1"), ["A", "A1", "A2", "B", "B1", "C"]);
    // ... which here is exactly where C already is: a no-op.
    assert!(!model.would_move_tab("C", "B", true));
}

#[test]
fn move_root_targeting_foreign_child_lands_after_that_block() {
    let mut model = lineage_project_model();
    // A slot naming A's child row normalizes to after A's whole block —
    // a top-level tab can never interleave into a group.
    model.move_tab("C", "A1", false);
    assert_eq!(tab_ids_in(&model, "p1"), ["A", "A1", "A2", "C", "B", "B1"]);
}

#[test]
fn move_childless_root_between_blocks_still_works() {
    let mut model = lineage_project_model();
    model.move_tab("C", "A", false);
    assert_eq!(tab_ids_in(&model, "p1"), ["C", "A", "A1", "A2", "B", "B1"]);
}

#[test]
fn move_parent_gathers_scattered_children() {
    let mut model = lineage_project_model();
    // Corrupt the order the way the old single-row move could: A stranded at
    // the end, children left at the top.
    let tabs = &mut model.projects[0].tabs;
    let a = tabs.remove(0);
    tabs.push(a); // [A1, A2, B, B1, C, A]
    // Any real move of A re-gathers the block contiguously.
    model.move_tab("A", "B", false);
    assert_eq!(tab_ids_in(&model, "p1"), ["A", "A1", "A2", "B", "B1", "C"]);
}

#[test]
fn move_child_reorders_among_siblings_only() {
    let mut model = lineage_project_model();
    // A2 before A1 — legal sibling reorder.
    model.move_tab("A2", "A1", false);
    assert_eq!(tab_ids_in(&model, "p1"), ["A", "A2", "A1", "B", "B1", "C"]);
    // Parent row with place_after == true is the top-of-run slot.
    model.move_tab("A1", "A", true);
    assert_eq!(tab_ids_in(&model, "p1"), ["A", "A1", "A2", "B", "B1", "C"]);
}

#[test]
fn move_child_outside_its_block_is_rejected() {
    let mut model = lineage_project_model();
    let before = tab_ids_in(&model, "p1");
    for (target, after) in [
        ("A", false),  // before its own parent
        ("B", false),  // another block's root
        ("B", true),   // inside another block
        ("B1", true),  // another block's child
        ("C", true),   // a standalone root
    ] {
        model.move_tab("A1", target, after);
        assert_eq!(tab_ids_in(&model, "p1"), before, "A1 onto {target}/{after}");
        assert!(!model.would_move_tab("A1", target, after));
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
    model.move_tab("A", "C", true);
    assert_eq!(*count.borrow(), 1, "real block move fires exactly once");
    model.move_tab("A", "A1", false); // own-subtree drop: no-op
    model.move_tab("A1", "C", true); // child leaving its block: rejected
    assert_eq!(*count.borrow(), 1, "illegal/no-op drops fire no event");
}

#[test]
fn would_move_parent_matches_move_semantics() {
    let model = lineage_project_model();
    assert!(model.would_move_tab("A", "C", true));
    assert!(model.would_move_tab("B", "A", false));
    // A after B's block == A back in front of C — a real move.
    assert!(model.would_move_tab("A", "B", true));
    // B before its own current successor's block boundary — no order change.
    assert!(!model.would_move_tab("B", "A", true));
    assert!(!model.would_move_tab("B", "A1", false));
}

// =====================================================================
// TabModelNavigationTests (tab half)
// =====================================================================

/// Two projects alongside the pinned Terminals group (kept from the seed),
/// two tabs each. navigable = [Main, p1t0, p1t1, p2t0, p2t1].
fn nav_two_projects() -> TabModel {
    let mut model = model_empty("/tmp/main");
    model.projects.push(make_project("p1", "P1", 2));
    model.projects.push(make_project("p2", "P2", 2));
    model
}

#[test]
fn navigable_sidebar_tab_ids_terminals_always_first() {
    let model = model_empty("/tmp/main");
    assert_eq!(
        model.navigable_sidebar_tab_ids(),
        [TabModel::MAIN_TERMINAL_TAB_ID]
    );
}

#[test]
fn tab_id_owning_resolves_by_pane_across_projects_and_is_scoped() {
    // A pane's owning tab is found by scanning every project's pane lists — even
    // when the owner is not the first project — and pane ids are a distinct
    // namespace from tab ids, so a tab-id-shaped query never matches a pane.
    let mut model = model_empty("/tmp/main");
    seed_claude_tab(&mut model, "p1", "t1", "S1", "/tmp/p1", true);
    seed_claude_tab(&mut model, "p2", "t2", "S2", "/tmp/p2", true);
    seed_claude_tab(&mut model, "p3", "t3", "S3", "/tmp/p3", true);

    // Resolves the middle project's claude pane (reverse scan hits a non-first
    // project) and its companion terminal pane.
    assert_eq!(model.tab_id_owning("t2-claude").as_deref(), Some("t2"));
    assert_eq!(model.tab_id_owning("t2-t1").as_deref(), Some("t2"));
    // The pinned Terminals group is scanned too — its Main pane resolves.
    let main_pane = model
        .tab_for(TabModel::MAIN_TERMINAL_TAB_ID)
        .unwrap()
        .panes[0]
        .id
        .clone();
    assert_eq!(
        model.tab_id_owning(&main_pane).as_deref(),
        Some(TabModel::MAIN_TERMINAL_TAB_ID)
    );
    // A tab id is not a pane id — passing one must not match any pane list.
    assert_eq!(model.tab_id_owning("t1"), None);
    // An entirely unknown pane id (stale / from another window) is None.
    assert_eq!(model.tab_id_owning("definitely-not-a-pane"), None);
}

#[test]
fn next_sidebar_tab_is_no_op_when_only_main_terminal_exists() {
    let mut model = model_empty("/tmp/main");
    model.select_tab(TabModel::MAIN_TERMINAL_TAB_ID);
    model.select_next_sidebar_tab();
    assert_eq!(model.active_tab_id(), Some(TabModel::MAIN_TERMINAL_TAB_ID));
    model.select_prev_sidebar_tab();
    assert_eq!(model.active_tab_id(), Some(TabModel::MAIN_TERMINAL_TAB_ID));
}

#[test]
fn next_sidebar_tab_cycles_through_visible_tabs() {
    let mut model = nav_two_projects();
    let ids = model.navigable_sidebar_tab_ids();
    assert_eq!(ids.len(), 5, "Main + (P1: T0,T1) + (P2: T0,T1)");
    model.select_tab(&ids[0]);
    for expected in ids.iter().skip(1) {
        model.select_next_sidebar_tab();
        assert_eq!(model.active_tab_id(), Some(expected.as_str()));
    }
    model.select_next_sidebar_tab();
    assert_eq!(model.active_tab_id(), Some(ids[0].as_str()));
}

#[test]
fn prev_sidebar_tab_cycles_backward() {
    let mut model = nav_two_projects();
    let ids = model.navigable_sidebar_tab_ids();
    model.select_tab(&ids[0]);
    model.select_prev_sidebar_tab();
    assert_eq!(model.active_tab_id(), Some(ids.last().unwrap().as_str()));
    model.select_prev_sidebar_tab();
    assert_eq!(model.active_tab_id(), Some(ids[ids.len() - 2].as_str()));
}

// =====================================================================
// Selection side effects (the `active_tab_id` didSet — TabModel.swift:43-53)
//
// `select_tab` carries the two ported `didSet` side effects: dismiss the
// waiting pulse on the target tab's active pane, and fire the did-mutate
// signal exactly once — but only when the id actually changes. These pin the
// wiring through `select_tab`; `Pane::mark_acknowledged_if_waiting` itself is
// unit-tested in pane.rs.
// =====================================================================

#[test]
fn select_tab_acknowledges_waiting_on_target_active_pane() {
    // A tab whose active pane (a Claude pane) sits in unacknowledged Waiting:
    // selecting the tab is the user looking at it, so the pulse must be
    // dismissed (the `acknowledge_waiting_on_active_pane` didSet side effect).
    let mut model = model_empty("/tmp/main");
    let (claude_pane_id, _term) =
        seed_claude_tab(&mut model, "p", "ct", "sess", "/tmp/p", true);
    // Drive the Claude pane into unacknowledged Waiting while the user is on
    // the seed's Main tab (i.e. not viewing this pane).
    model.mutate_tab("ct", |tab| {
        let claude = tab
            .panes
            .iter_mut()
            .find(|p| p.id == claude_pane_id)
            .unwrap();
        claude.apply_status_transition(crate::TabStatus::Waiting, false);
    });
    assert!(
        !model.tab_for("ct").unwrap().active_pane().unwrap().waiting_acknowledged,
        "precondition: the target's active pane is waiting and unacknowledged"
    );

    model.select_tab("ct");

    assert!(
        model.tab_for("ct").unwrap().active_pane().unwrap().waiting_acknowledged,
        "selecting a tab must acknowledge the waiting pulse on its active pane"
    );
}

#[test]
fn select_tab_fires_mutation_once_on_change_and_never_on_reselect() {
    let mut model = nav_two_projects();
    let ids = model.navigable_sidebar_tab_ids();
    // Land on the first tab before installing the counter so the seed's own
    // selection isn't counted; ids[0] is already active from the seed, so this
    // is a no-op and nothing is missed.
    model.select_tab(&ids[0]);
    let counter = mutation_counter(&mut model);

    // A real selection change fires exactly once.
    model.select_tab(&ids[1]);
    assert_eq!(
        counter.get(),
        1,
        "a real selection change must fire the did-mutate signal exactly once"
    );

    // Re-selecting the already-active tab changes nothing — no event.
    model.select_tab(&ids[1]);
    assert_eq!(
        counter.get(),
        1,
        "re-selecting the active tab is a no-op and must not fire the signal"
    );
}

// =====================================================================
// TabModelProjectBucketingTests (model half)
//
// The Swift suite drives `SessionsModel.createTabFromMainTerminal` (spawns a
// pty). The model-relevant behavior — which project the tab buckets into — is
// `add_tab_to_projects`, tested directly here. The tab-build + worktree-dir
// string construction (`<cwd>/.claude/worktrees/<name>`, `/`→`+`) and the pty
// spawn belong to `createTabFromMainTerminal` and are R13:
//   R13: createTabFromMainTerminal worktree-dir construction + tab shape
//        (test_claudeFromMainTerminal_withWorktreeFlag_* / _withoutWorktreeFlag_
//        tabCwdMatchesProjectPath) — extract_worktree_name (the parser half) is
//        ported below; the bucketing-by-parent-cwd half is covered here.
//   R13/R18: the addRestoredTabModel restore-heal cases
//        (test_addRestoredTabModel_*).
// =====================================================================

/// A Claude + terminal tab for bucketing assertions.
fn new_claude_tab(id: &str, cwd: &str) -> Tab {
    let mut tab = Tab::new(id, "New tab", cwd);
    tab.panes = vec![
        claude(&format!("{}-claude", id)),
        terminal(&format!("{}-t1", id), "Terminal 1"),
    ];
    tab.active_pane_id = Some(format!("{}-claude", id));
    tab
}

fn non_terminals_projects(model: &TabModel) -> Vec<&Project> {
    model
        .projects
        .iter()
        .filter(|p| p.id != TabModel::TERMINALS_PROJECT_ID)
        .collect()
}

// MARK: - extract_worktree_name

#[test]
fn extract_worktree_name_short_flag() {
    assert_eq!(
        TabModel::extract_worktree_name(&["-w", "foo"]),
        Some("foo".to_string())
    );
}

#[test]
fn extract_worktree_name_long_flag() {
    assert_eq!(
        TabModel::extract_worktree_name(&["--worktree", "foo"]),
        Some("foo".to_string())
    );
}

#[test]
fn extract_worktree_name_trailing_flag_returns_none() {
    assert_eq!(TabModel::extract_worktree_name(&["-w"]), None);
    assert_eq!(TabModel::extract_worktree_name(&["a", "--worktree"]), None);
}

#[test]
fn extract_worktree_name_empty_value_returns_none() {
    assert_eq!(TabModel::extract_worktree_name(&["-w", ""]), None);
}

#[test]
fn extract_worktree_name_scans_past_other_args() {
    assert_eq!(
        TabModel::extract_worktree_name(&["--model", "sonnet", "-w", "foo"]),
        Some("foo".to_string())
    );
}

#[test]
fn extract_worktree_name_equals_form_not_recognized() {
    // Design decision: only space-delimited is supported.
    assert_eq!(TabModel::extract_worktree_name(&["-w=foo"]), None);
    assert_eq!(TabModel::extract_worktree_name(&["--worktree=foo"]), None);
}

#[test]
fn extract_worktree_name_absent_returns_none() {
    let empty: &[&str] = &[];
    assert_eq!(TabModel::extract_worktree_name(empty), None);
    assert_eq!(TabModel::extract_worktree_name(&["--model", "sonnet"]), None);
}

// MARK: - sanitize_worktree_name (`/`→`+`, mirroring Claude's worktree-dir
// derivation; `SessionsModel.swift:677-682`).

#[test]
fn sanitize_worktree_name_replaces_slash_with_plus() {
    assert_eq!(TabModel::sanitize_worktree_name("foo/bar"), "foo+bar");
}

#[test]
fn sanitize_worktree_name_replaces_every_slash() {
    assert_eq!(TabModel::sanitize_worktree_name("a/b/c"), "a+b+c");
}

#[test]
fn sanitize_worktree_name_no_slash_unchanged() {
    assert_eq!(TabModel::sanitize_worktree_name("feature-x"), "feature-x");
}

#[test]
fn sanitize_worktree_name_empty_unchanged() {
    assert_eq!(TabModel::sanitize_worktree_name(""), "");
}

// MARK: - extract_claude_session_id

#[test]
fn extract_claude_session_id_resume_space_delimited() {
    assert_eq!(
        TabModel::extract_claude_session_id(&["--resume", "abc-123"]),
        Some("abc-123".to_string())
    );
}

#[test]
fn extract_claude_session_id_session_id_space_delimited() {
    assert_eq!(
        TabModel::extract_claude_session_id(&["--session-id", "uuid-1"]),
        Some("uuid-1".to_string())
    );
}

#[test]
fn extract_claude_session_id_resume_equals_form() {
    assert_eq!(
        TabModel::extract_claude_session_id(&["--resume=xyz"]),
        Some("xyz".to_string())
    );
}

#[test]
fn extract_claude_session_id_session_id_equals_form() {
    assert_eq!(
        TabModel::extract_claude_session_id(&["--session-id=qwe"]),
        Some("qwe".to_string())
    );
}

#[test]
fn extract_claude_session_id_scans_past_other_args() {
    assert_eq!(
        TabModel::extract_claude_session_id(&["--model", "sonnet", "--resume", "abc"]),
        Some("abc".to_string())
    );
}

#[test]
fn extract_claude_session_id_trailing_resume_returns_none() {
    assert_eq!(TabModel::extract_claude_session_id(&["--resume"]), None);
    assert_eq!(TabModel::extract_claude_session_id(&["a", "--session-id"]), None);
}

#[test]
fn extract_claude_session_id_absent_returns_none() {
    let empty: &[&str] = &[];
    assert_eq!(TabModel::extract_claude_session_id(empty), None);
    assert_eq!(TabModel::extract_claude_session_id(&["--model", "sonnet"]), None);
}

// MARK: - add_tab_to_projects (bucketing)

#[test]
fn add_tab_to_projects_under_main_cwd_creates_new_project_group() {
    let main_cwd = "/tmp/nice-test-home";
    let mut model = model_empty(main_cwd); // Terminals path = main_cwd, no git roots
    let cwd = "/tmp/nice-test-home/Projects/zephyr";
    model.add_tab_to_projects(new_claude_tab("t-z", cwd), cwd);

    assert_eq!(model.projects.len(), 2, "Terminals + one new project group");
    assert_eq!(model.projects[0].id, TabModel::TERMINALS_PROJECT_ID);
    assert_eq!(model.projects[0].tabs.len(), 1, "Terminals must not absorb Claude tabs");
    let new = non_terminals_projects(&model)[0];
    assert_eq!(new.name, "ZEPHYR");
    assert_eq!(new.path, cwd);
    assert_eq!(new.tabs.len(), 1);
    assert!(new.tabs[0].panes.iter().any(|p| p.kind == PaneKind::Claude));
}

#[test]
fn add_tab_to_projects_cwd_equals_main_cwd_still_creates_new_project() {
    let main_cwd = "/tmp/nice-test-home";
    let mut model = model_empty(main_cwd);
    model.add_tab_to_projects(new_claude_tab("t-m", main_cwd), main_cwd);

    assert_eq!(model.projects.len(), 2);
    assert_eq!(model.projects[0].tabs.len(), 1, "Terminals keeps only Main");
    let new = non_terminals_projects(&model)[0];
    assert_eq!(new.path, main_cwd);
    assert_eq!(new.tabs.len(), 1);
}

#[test]
fn add_tab_to_projects_picks_existing_project_when_cwd_matches() {
    let mut model = model_empty("/tmp/nice-test-home");
    seed_terminal_project(&mut model, "p1", "P1", "/tmp/p1");
    model.add_tab_to_projects(new_claude_tab("t-x", "/tmp/p1/sub"), "/tmp/p1/sub");

    assert_eq!(model.projects.len(), 2, "reuse p1, not create a third project");
    let p1 = project_by_id(&model, "p1");
    assert_eq!(p1.tabs.len(), 2);
    assert!(p1.tabs.last().unwrap().panes.iter().any(|p| p.kind == PaneKind::Claude));
    assert_eq!(model.projects[0].tabs.len(), 1);
}

#[test]
fn add_tab_to_projects_longest_prefix_wins_among_projects() {
    let mut model = model_empty("/tmp/nice-test-home");
    seed_terminal_project(&mut model, "p1", "P1", "/tmp/p1");
    seed_terminal_project(&mut model, "p1-nested", "Nested", "/tmp/p1/nested");
    model.add_tab_to_projects(new_claude_tab("t-x", "/tmp/p1/nested/x"), "/tmp/p1/nested/x");

    assert_eq!(project_by_id(&model, "p1").tabs.len(), 1, "shallower must not win");
    assert_eq!(
        project_by_id(&model, "p1-nested").tabs.len(),
        2,
        "deeper project is the longest-prefix match"
    );
}

#[test]
fn add_tab_to_projects_nested_git_repo_creates_separate_project_from_outer() {
    let outer = "/fs/outer";
    let nested = "/fs/outer/nested-1";
    let mut model = model_with(
        "/tmp/main",
        &[outer, "/fs/outer/.git", nested, "/fs/outer/nested-1/.git"],
    );
    seed_terminal_project(&mut model, "outer", "OUTER", outer);
    model.add_tab_to_projects(new_claude_tab("t-n", nested), nested);

    assert_eq!(project_by_id(&model, "outer").tabs.len(), 1, "outer must not absorb the nested tab");
    let nested_p = model
        .projects
        .iter()
        .find(|p| p.id != TabModel::TERMINALS_PROJECT_ID && p.id != "outer")
        .expect("a separate project rooted at the nested repo must exist");
    assert_eq!(nested_p.path, nested);
    assert_eq!(nested_p.name, "NESTED-1");
    assert_eq!(nested_p.tabs.len(), 1);
}

#[test]
fn add_tab_to_projects_subdir_of_existing_repo_buckets_into_existing_project() {
    let repo = "/fs/repo";
    let sub = "/fs/repo/src/deep";
    let mut model = model_with("/tmp/main", &[repo, "/fs/repo/.git", sub]);
    seed_terminal_project(&mut model, "repo", "REPO", repo);
    model.add_tab_to_projects(new_claude_tab("t-s", sub), sub);

    assert_eq!(project_by_id(&model, "repo").tabs.len(), 2, "sub-dir tab buckets into the repo project");
    assert!(
        model
            .projects
            .iter()
            .all(|p| p.id == TabModel::TERMINALS_PROJECT_ID || p.id == "repo"),
        "no spurious project for the sub-dir"
    );
}

#[test]
fn add_tab_to_projects_first_cwd_inside_repo_anchors_project_at_git_root() {
    let repo = "/fs/repo";
    let sub = "/fs/repo/src/deep";
    let mut model = model_with("/tmp/main", &[repo, "/fs/repo/.git", sub]);
    model.add_tab_to_projects(new_claude_tab("t-f", sub), sub);

    let new = non_terminals_projects(&model);
    assert_eq!(new.len(), 1);
    assert_eq!(new[0].path, repo, "project anchored at the git root, not the cwd");
    assert_eq!(new[0].name, "REPO");
    assert_eq!(new[0].tabs.len(), 1);
}

#[test]
fn add_tab_to_projects_cwd_inside_nice_worktree_buckets_into_parent_repo() {
    let repo = "/fs/repo";
    let worktree = "/fs/repo/.claude/worktrees/bug";
    let mut model = model_with(
        "/tmp/main",
        &[repo, "/fs/repo/.git", worktree, "/fs/repo/.claude/worktrees/bug/.git"],
    );
    seed_terminal_project(&mut model, "repo", "REPO", repo);
    model.add_tab_to_projects(new_claude_tab("t-w", worktree), worktree);

    assert_eq!(
        project_by_id(&model, "repo").tabs.len(),
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
fn resolved_spawn_cwd_falls_back_to_project_path_when_tab_cwd_missing() {
    let project_path = "/fs/project";
    let missing = "/fs/project/.claude/worktrees/deleted";
    let mut model = model_with("/tmp/main", &[project_path]); // `missing` not registered
    seed_terminal_project(&mut model, "tmp", "TMP", project_path);
    let mut tab = Tab::new("tmp-worktree-tab", "worktree", missing);
    tab.panes = vec![terminal("tmp-worktree-tab-p0", "zsh")];
    tab.active_pane_id = Some("tmp-worktree-tab-p0".into());
    let idx = model.projects.iter().position(|p| p.id == "tmp").unwrap();
    model.projects[idx].tabs.push(tab.clone());

    assert_eq!(model.resolved_spawn_cwd(&tab), project_path);
}

#[test]
fn resolved_spawn_cwd_returns_tab_cwd_when_it_exists() {
    let existing = "/fs/existing";
    let mut model = model_with("/tmp/main", &[existing]);
    seed_terminal_project(&mut model, "tmp", "TMP", "/does-not-matter");
    let mut tab = Tab::new("tmp-real-tab", "real", existing);
    tab.panes = vec![terminal("tmp-real-tab-p0", "zsh")];
    tab.active_pane_id = Some("tmp-real-tab-p0".into());
    let idx = model.projects.iter().position(|p| p.id == "tmp").unwrap();
    model.projects[idx].tabs.push(tab.clone());

    assert_eq!(model.resolved_spawn_cwd(&tab), existing);
}

// =====================================================================
// TabModelProjectRepairTests
// =====================================================================

/// A repair-fixture tab: a single terminal pane, no pty.
fn repair_tab(id: &str, cwd: &str) -> Tab {
    let mut tab = Tab::new(id, id, cwd);
    tab.panes = vec![terminal(&format!("{}-p0", id), "zsh")];
    tab.active_pane_id = Some(format!("{}-p0", id));
    tab
}

fn project_with(id: &str, name: &str, path: &str, tabs: Vec<Tab>) -> Project {
    Project {
        id: id.into(),
        name: name.into(),
        path: path.into(),
        tabs,
    }
}

#[test]
fn repair_moves_nested_tab_into_own_project() {
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
        vec![repair_tab("outer-seed", outer), repair_tab("stray-nested", nested)],
    ));

    model.repair_project_structure();

    let outer_p = project_by_id(&model, "outer");
    assert_eq!(outer_p.tabs.len(), 1, "only the nested-cwd tab should have moved");
    assert_eq!(outer_p.tabs[0].id, "outer-seed");

    let nested_p = model
        .projects
        .iter()
        .find(|p| p.path == nested)
        .expect("a new project anchored at the nested repo must exist");
    assert_ne!(nested_p.id, TabModel::TERMINALS_PROJECT_ID);
    assert_ne!(nested_p.id, "outer");
    assert!(nested_p.id.starts_with("p-nested-1-"));
    assert_eq!(nested_p.name, "NESTED-1");
    assert_eq!(nested_p.tabs.len(), 1);
    assert_eq!(nested_p.tabs[0].id, "stray-nested");
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
        vec![repair_tab("sub-seed", sub), repair_tab("deep-nested", nested)],
    ));

    model.repair_project_structure();

    let promoted = project_by_id(&model, "p-sub-original");
    assert_eq!(promoted.path, outer, "pass 1 promotes outer/sub to outer");
    assert_eq!(promoted.name, "OUTER");
    assert_eq!(promoted.tabs.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["sub-seed"]);

    let nested_p = model
        .projects
        .iter()
        .find(|p| p.path == nested)
        .expect("pass 2 must create a project for the nested-cwd tab");
    assert_eq!(nested_p.tabs.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["deep-nested"]);
}

#[test]
fn repair_skips_tabs_with_missing_cwd() {
    let repo = "/fs/repo";
    let missing = "/fs/repo/.claude/worktrees/deleted";
    let mut model = model_with("/home", &[repo, "/fs/repo/.git"]); // missing not registered
    model.projects.push(project_with("repo", "REPO", repo, vec![repair_tab("ghost", missing)]));

    model.repair_project_structure();

    let p = project_by_id(&model, "repo");
    assert_eq!(p.tabs.len(), 1);
    assert_eq!(p.tabs[0].id, "ghost");
}

#[test]
fn repair_promotes_subdir_project_to_git_root() {
    let repo = "/fs/repo";
    let deep = "/fs/repo/src/deep";
    let mut model = model_with("/home", &[repo, "/fs/repo/.git", deep]);
    model.projects.push(project_with("p-deep-123", "DEEP", deep, vec![repair_tab("deep-tab", deep)]));

    model.repair_project_structure();

    assert_eq!(non_terminals_projects(&model).len(), 1, "promotion must not create/drop projects");
    let promoted = project_by_id(&model, "p-deep-123");
    assert_eq!(promoted.path, repo);
    assert_eq!(promoted.name, "REPO");
    assert_eq!(promoted.tabs.len(), 1);
    assert_eq!(promoted.tabs[0].id, "deep-tab");
}

#[test]
fn repair_merges_duplicate_projects_at_same_git_root() {
    let repo = "/fs/repo";
    let mut model = model_with("/home", &[repo, "/fs/repo/.git"]);
    model.projects.push(project_with("first", "REPO", repo, vec![repair_tab("first-tab", repo)]));
    model.projects.push(project_with("second", "REPO", repo, vec![repair_tab("second-tab", repo)]));

    model.repair_project_structure();

    assert_eq!(non_terminals_projects(&model).len(), 1, "duplicate at same path merged");
    let canonical = project_by_id(&model, "first");
    assert_eq!(
        canonical.tabs.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        ["first-tab", "second-tab"],
        "canonical's own tabs first, then the merged dupe's"
    );
    assert!(model.projects.iter().all(|p| p.id != "second"), "merged dupe removed");
}

#[test]
fn repair_drops_empty_projects_but_preserves_terminals() {
    let mut model = model_with("/home", &[]);
    model.projects.push(project_with("abandoned", "GHOST", "/tmp/no-tabs-here", vec![]));
    let terminals_before = model.projects[0].id.clone();

    model.repair_project_structure();

    assert_eq!(model.projects[0].id, terminals_before);
    assert_eq!(model.projects[0].id, TabModel::TERMINALS_PROJECT_ID);
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
        after.tabs.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
        before.tabs.iter().map(|t| t.id.clone()).collect::<Vec<_>>()
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
                repair_tab("outer-seed", outer),
                repair_tab("stray-nested", nested),
                repair_tab("deep-sub", deep),
            ],
        ));
        model.projects.push(project_with("p-deep-123", "DEEP", deep, vec![]));
        model
    };
    let snapshot = |model: &TabModel| {
        model
            .projects
            .iter()
            .map(|p| {
                (
                    p.id.clone(),
                    p.name.clone(),
                    p.path.clone(),
                    p.tabs.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
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

/// Inject a single-pane tab under a fresh project keyed by path. Deterministic
/// ids per call via a counter (mirrors `TabModelFixtures.injectTab`).
fn inject_tab(model: &mut TabModel, title: &str, project_path: &str, kind: PaneKind) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let tab_id = format!("t-inject-{}", n);
    let (pane_id, pane_title) = match kind {
        PaneKind::Claude => (format!("{}-claude", tab_id), "Claude"),
        PaneKind::Terminal => (format!("{}-term", tab_id), "Terminal"),
    };
    let mut tab = Tab::new(&tab_id, title, project_path);
    tab.panes = vec![Pane::new(&pane_id, pane_title, kind)];
    tab.active_pane_id = Some(pane_id);
    if let Some(idx) = model.projects.iter().position(|p| p.path == project_path) {
        model.projects[idx].tabs.push(tab);
    } else {
        model.projects.push(Project {
            id: format!("p-inject-{}", n),
            name: last_path_component(project_path),
            path: project_path.into(),
            tabs: vec![tab],
        });
    }
    tab_id
}

#[test]
fn rename_tab_sets_title_and_marks_manually_set() {
    let mut model = model_empty("/tmp/main");
    let id = inject_tab(&mut model, "New tab", "/tmp/rename-test", PaneKind::Terminal);
    model.rename_tab(&id, "My session");
    let after = model.tab_for(&id).unwrap();
    assert_eq!(after.title, "My session");
    assert!(after.title_manually_set);
}

#[test]
fn rename_tab_trims_whitespace() {
    let mut model = model_empty("/tmp/main");
    let id = inject_tab(&mut model, "New tab", "/tmp/rename-test", PaneKind::Terminal);
    model.rename_tab(&id, "   padded   ");
    assert_eq!(model.tab_for(&id).unwrap().title, "padded");
}

#[test]
fn rename_tab_empty_input_is_noop() {
    let mut model = model_empty("/tmp/main");
    let id = inject_tab(&mut model, "Original", "/tmp/rename-test", PaneKind::Terminal);
    model.rename_tab(&id, "   ");
    let after = model.tab_for(&id).unwrap();
    assert_eq!(after.title, "Original");
    assert!(!after.title_manually_set, "empty rename must not mark manually set");
}

#[test]
fn apply_auto_title_skips_after_manual_rename() {
    let mut model = model_empty("/tmp/main");
    let id = inject_tab(&mut model, "New tab", "/tmp/rename-test", PaneKind::Terminal);
    model.rename_tab(&id, "My session");
    model.apply_auto_title(&id, "late-arriving-session");
    assert_eq!(
        model.tab_for(&id).unwrap().title,
        "My session",
        "apply_auto_title must skip a user-renamed tab"
    );
}

#[test]
fn apply_auto_title_on_other_tabs_is_unaffected_by_rename() {
    let mut model = model_empty("/tmp/main");
    let renamed = inject_tab(&mut model, "New tab", "/tmp/rename-test", PaneKind::Terminal);
    let other = inject_tab(&mut model, "New tab", "/tmp/rename-other", PaneKind::Terminal);
    model.rename_tab(&renamed, "Manual name");
    model.apply_auto_title(&other, "fix-some-bug");

    assert_eq!(model.tab_for(&renamed).unwrap().title, "Manual name");
    assert_eq!(model.tab_for(&other).unwrap().title, "Fix some bug");
    assert!(model.tab_for(&other).unwrap().title_auto_generated);
    assert!(!model.tab_for(&other).unwrap().title_manually_set);
}

#[test]
fn apply_auto_title_still_works_on_fresh_tab() {
    let mut model = model_empty("/tmp/main");
    let id = inject_tab(&mut model, "New tab", "/tmp/rename-test", PaneKind::Terminal);
    model.apply_auto_title(&id, "fix-top-bar-height");
    let after = model.tab_for(&id).unwrap();
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
// cases already live in tab.rs (slice 1).
// =====================================================================

fn main_pane_id(model: &TabModel) -> String {
    model.tab_for(TabModel::MAIN_TERMINAL_TAB_ID).unwrap().panes[0]
        .id
        .clone()
}

#[test]
fn seed_main_tab_initial_pane_title_is_terminal_1() {
    let model = model_empty("/tmp/main");
    let main = model.tab_for(TabModel::MAIN_TERMINAL_TAB_ID).unwrap();
    assert_eq!(main.panes[0].title, "Terminal 1");
    assert_eq!(main.next_terminal_index, 2, "seed counter primed at 2");
}

/// Validation §4b: monotonic-after-closing spot-probe.
#[test]
fn add_pane_is_monotonic_after_closing_a_pane() {
    let mut model = model_empty("/tmp/main");
    let tab_id = TabModel::MAIN_TERMINAL_TAB_ID;
    model.add_pane(tab_id, "px2", None);
    model.add_pane(tab_id, "px3", None);
    let tab = model.tab_for(tab_id).unwrap();
    assert_eq!(tab.panes[1].title, "Terminal 2");
    assert_eq!(tab.panes[2].title, "Terminal 3");

    model.mutate_tab(tab_id, |t| t.panes.retain(|p| p.id != "px2"));

    let px4 = model.add_pane(tab_id, "px4", None).unwrap();
    let tab_after = model.tab_for(tab_id).unwrap();
    let new_pane = tab_after.panes.iter().find(|p| p.id == px4).unwrap();
    assert_eq!(new_pane.title, "Terminal 4", "closing T2 must not reuse the number");
    assert_eq!(tab_after.next_terminal_index, 5, "closing a pane must not decrement the counter");
}

#[test]
fn add_pane_explicit_title_still_increments_counter() {
    let mut model = model_empty("/tmp/main");
    let tab_id = TabModel::MAIN_TERMINAL_TAB_ID;
    let counter_before = model.tab_for(tab_id).unwrap().next_terminal_index;
    let panes_before = model.tab_for(tab_id).unwrap().panes.len();

    model.add_pane(tab_id, "pe", Some("vim foo.swift".into()));

    let tab = model.tab_for(tab_id).unwrap();
    assert_eq!(tab.panes.len(), panes_before + 1);
    assert_eq!(tab.panes.last().unwrap().title, "vim foo.swift");
    assert_eq!(tab.next_terminal_index, counter_before + 1, "explicit title still advances the counter");
}

#[test]
fn add_pane_increments_counter() {
    let mut model = model_empty("/tmp/main");
    let tab_id = TabModel::MAIN_TERMINAL_TAB_ID;
    model.add_pane(tab_id, "a", None);
    model.add_pane(tab_id, "b", None);
    model.add_pane(tab_id, "c", None);
    assert_eq!(model.tab_for(tab_id).unwrap().next_terminal_index, 5, "seed 2 + 3 adds → 5");
}

#[test]
fn rename_pane_changes_title() {
    let mut model = model_empty("/tmp/main");
    let tab_id = TabModel::MAIN_TERMINAL_TAB_ID;
    let pane_id = main_pane_id(&model);
    model.rename_pane(tab_id, &pane_id, "logs");
    assert_eq!(model.tab_for(tab_id).unwrap().panes[0].title, "logs");
}

#[test]
fn rename_pane_trims_whitespace() {
    let mut model = model_empty("/tmp/main");
    let tab_id = TabModel::MAIN_TERMINAL_TAB_ID;
    let pane_id = main_pane_id(&model);
    model.rename_pane(tab_id, &pane_id, "  padded  ");
    assert_eq!(model.tab_for(tab_id).unwrap().panes[0].title, "padded");
}

#[test]
fn rename_pane_empty_input_resets_to_auto_default_clears_flag() {
    let mut model = model_empty("/tmp/main");
    let tab_id = TabModel::MAIN_TERMINAL_TAB_ID;
    let pane_id = main_pane_id(&model);
    let counter_before = model.tab_for(tab_id).unwrap().next_terminal_index;

    model.rename_pane(tab_id, &pane_id, "logs");
    assert!(model.tab_for(tab_id).unwrap().panes[0].title_manually_set);

    model.rename_pane(tab_id, &pane_id, "  ");

    let tab = model.tab_for(tab_id).unwrap();
    assert!(!tab.panes[0].title_manually_set, "empty submit clears the lock");
    assert_eq!(
        tab.panes[0].title,
        format!("Terminal {}", counter_before),
        "empty submit resets to the auto-default consuming the next counter slot"
    );
    assert_eq!(tab.next_terminal_index, counter_before + 1, "reset path advances the counter");
}

#[test]
fn rename_pane_sets_title_manually_set() {
    let mut model = model_empty("/tmp/main");
    let tab_id = TabModel::MAIN_TERMINAL_TAB_ID;
    let pane_id = main_pane_id(&model);
    model.rename_pane(tab_id, &pane_id, "build");
    let pane = &model.tab_for(tab_id).unwrap().panes[0];
    assert_eq!(pane.title, "build");
    assert!(pane.title_manually_set);
}

#[test]
fn rename_pane_fires_on_tree_mutation() {
    let mut model = model_empty("/tmp/main");
    let tab_id = TabModel::MAIN_TERMINAL_TAB_ID;
    let pane_id = main_pane_id(&model);
    let counter = mutation_counter(&mut model);
    model.rename_pane(tab_id, &pane_id, "new name");
    assert_eq!(counter.get(), 1);
}

#[test]
fn rename_pane_does_not_fire_on_tree_mutation_when_no_change() {
    let mut model = model_empty("/tmp/main");
    let tab_id = TabModel::MAIN_TERMINAL_TAB_ID;
    let pane_id = main_pane_id(&model);
    model.rename_pane(tab_id, &pane_id, "logs"); // first rename locks
    let counter = mutation_counter(&mut model);
    model.rename_pane(tab_id, &pane_id, "logs"); // identical → no change
    assert_eq!(counter.get(), 0);
}

#[test]
fn rename_pane_does_not_touch_other_panes() {
    let mut model = model_empty("/tmp/main");
    let tab_id = TabModel::MAIN_TERMINAL_TAB_ID;
    let pane1 = main_pane_id(&model);
    model.mutate_tab(tab_id, |t| {
        t.panes.push(terminal("stable-p2", "Terminal 2"));
    });
    let before = model
        .tab_for(tab_id)
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == "stable-p2")
        .unwrap()
        .title
        .clone();
    model.rename_pane(tab_id, &pane1, "renamed");
    let after = model
        .tab_for(tab_id)
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == "stable-p2")
        .unwrap()
        .title
        .clone();
    assert_eq!(after, before);
}

// R18: hydration (test_hydration_*) drives addRestoredTabModel / PersistedTab;
// recover_next_terminal_index itself is pinned in tab.rs. Persistence
// round-trips (test_persistedTab_* / test_persistedPane_* / test_snapshot_*)
// arrive with the R18 schema.

// =====================================================================
// AppStateBranchTrackingTests — depth-1 lineage tree shape (via
// insert_branch_parent + remove_tab). The /branch trigger CLASSIFICATION
// (source=resume/clear/nil, id-change detection, per-window dispatch) is R16;
// the OSC-title-ignore + pty cascade are R13/R15; PersistedTab round-trips R18.
// The caller's post-rotation id update (child adopts NEW) is simulated here
// with mutate_tab so the tree assertions stand alone.
// =====================================================================

/// Seed one Claude tab `t1` (session `S0`, claude pane `t1-claude`) in project
/// `p`, plus the pinned Terminals group. cwd `/tmp/p`.
fn branch_seed(session: &str, title: &str) -> TabModel {
    let mut model = TabModel::with_fs("/tmp", fake_fs("/home", &[]));
    seed_claude_tab(&mut model, "p", "t1", session, "/tmp/p", true);
    model.mutate_tab("t1", |t| t.title = title.to_string());
    model
}

#[test]
fn insert_branch_parent_creates_parent_shape() {
    let mut model = branch_seed("OLD", "wire up the foo");
    let parent = model
        .insert_branch_parent("t1", "parent-1", "parent-1-claude", "parent-1-t1", "OLD")
        .expect("insert_branch_parent must return the inserted parent");
    // Caller (R16) updates the originating tab to the post-rotation id.
    model.mutate_tab("t1", |t| t.claude_session_id = Some("NEW".into()));

    let project = project_by_id(&model, "p");
    assert_eq!(project.tabs.len(), 2, "exactly one sibling parent added");
    let parent_tab = &project.tabs[0];
    let child = &project.tabs[1];

    assert_eq!(parent_tab.id, "parent-1");
    assert_eq!(child.id, "t1", "originating tab keeps its id");
    assert_eq!(child.claude_session_id.as_deref(), Some("NEW"), "child adopts the post-rotation id");
    assert_eq!(child.parent_tab_id.as_deref(), Some("parent-1"), "child points at the new parent");

    assert_eq!(parent_tab.claude_session_id.as_deref(), Some("OLD"), "parent pinned to the pre-rotation id");
    assert!(parent_tab.parent_tab_id.is_none(), "parent stays at root");
    assert_eq!(parent_tab.title, "wire up the foo", "parent inherits the title");
    assert_eq!(parent_tab.cwd, child.cwd, "parent inherits the cwd");

    assert_eq!(parent_tab.panes.len(), 2);
    assert!(parent_tab.panes.iter().any(|p| p.kind == PaneKind::Claude));
    assert!(parent_tab.panes.iter().any(|p| p.kind == PaneKind::Terminal));
    // Deferred-resume: the parent's Claude pane is created NOT running.
    let parent_claude = parent_tab.panes.iter().find(|p| p.kind == PaneKind::Claude).unwrap();
    assert!(!parent_claude.is_claude_running, "branch parent's Claude pane is deferred (not running)");
    assert_eq!(parent, *parent_tab, "returned parent equals the inserted tree node");
}

#[test]
fn first_branch_promotes_parent_to_root_and_originating_becomes_child() {
    let mut model = branch_seed("S0", "New tab");
    model.insert_branch_parent("t1", "P1", "P1-c", "P1-t", "S0");
    model.mutate_tab("t1", |t| t.claude_session_id = Some("S1".into()));

    let project = project_by_id(&model, "p");
    assert_eq!(project.tabs.len(), 2);
    assert!(project.tabs[0].parent_tab_id.is_none(), "first parent becomes the lineage root");
    assert_eq!(
        project.tabs[1].parent_tab_id.as_deref(),
        Some("P1"),
        "originating tab pulled in as a depth-1 child of the new root"
    );
}

#[test]
fn second_branch_adds_sibling_child_under_same_root() {
    let mut model = branch_seed("S0", "New tab");
    model.insert_branch_parent("t1", "P1", "P1-c", "P1-t", "S0");
    model.mutate_tab("t1", |t| t.claude_session_id = Some("S1".into()));
    model.insert_branch_parent("t1", "P2", "P2-c", "P2-t", "S1");
    model.mutate_tab("t1", |t| t.claude_session_id = Some("S2".into()));

    let after = project_by_id(&model, "p");
    assert_eq!(after.tabs.len(), 3);
    let (root, second, originating) = (&after.tabs[0], &after.tabs[1], &after.tabs[2]);

    assert_eq!(root.id, "P1", "root never changes once established");
    assert_eq!(root.claude_session_id.as_deref(), Some("S0"));
    assert!(root.parent_tab_id.is_none());

    assert_eq!(originating.id, "t1");
    assert_eq!(originating.claude_session_id.as_deref(), Some("S2"));
    assert_eq!(originating.parent_tab_id.as_deref(), Some("P1"), "originating keeps pointing at the original root");

    assert_eq!(second.claude_session_id.as_deref(), Some("S1"));
    assert_eq!(second.parent_tab_id.as_deref(), Some("P1"), "second parent is a sibling under the same root");
}

#[test]
fn third_branch_keeps_adding_siblings_under_same_root() {
    let mut model = branch_seed("S0", "New tab");
    for (i, new_session) in ["S1", "S2", "S3"].iter().enumerate() {
        let old = model.tab_for("t1").unwrap().claude_session_id.clone().unwrap();
        let pid = format!("parent-{}", i);
        model.insert_branch_parent("t1", &pid, &format!("{}-c", pid), &format!("{}-t", pid), &old);
        model.mutate_tab("t1", |t| t.claude_session_id = Some(new_session.to_string()));
        assert_eq!(project_by_id(&model, "p").tabs.len(), i + 2);
    }

    let final_p = project_by_id(&model, "p");
    let root = &final_p.tabs[0];
    assert!(root.parent_tab_id.is_none());
    assert_eq!(root.claude_session_id.as_deref(), Some("S0"));
    for tab in final_p.tabs.iter().skip(1) {
        assert_eq!(tab.parent_tab_id.as_deref(), Some(root.id.as_str()), "every non-root tab points at the original root");
    }
    assert_eq!(final_p.tabs.last().unwrap().id, "t1", "originating tab stays at the bottom in display order");
    assert_eq!(final_p.tabs.last().unwrap().claude_session_id.as_deref(), Some("S3"));
}

/// Validation §4c: /branch on a lineage root re-parents its former children.
#[test]
fn branch_on_root_preserves_depth1_by_reparenting_former_children() {
    let mut model = branch_seed("S0", "New tab");
    // First branch: P1(S0) root, t1(S1) child of P1.
    model.insert_branch_parent("t1", "P1", "P1-c", "P1-t", "S0");
    model.mutate_tab("t1", |t| t.claude_session_id = Some("S1".into()));
    // Second branch on t1: P2(S1) sibling under P1.
    model.insert_branch_parent("t1", "P2", "P2-c", "P2-t", "S1");
    model.mutate_tab("t1", |t| t.claude_session_id = Some("S2".into()));

    // Now /branch on the OLD ROOT (P1). old session on P1 is S0.
    model.insert_branch_parent("P1", "P3", "P3-c", "P3-t", "S0");
    model.mutate_tab("P1", |t| t.claude_session_id = Some("S0-PRIME".into()));

    let after = project_by_id(&model, "p");
    let roots: Vec<&Tab> = after.tabs.iter().filter(|t| t.parent_tab_id.is_none()).collect();
    assert_eq!(roots.len(), 1, "exactly one root remains after /branch on the old root");
    let new_root = roots[0];
    assert_eq!(new_root.id, "P3");
    assert_ne!(new_root.id, "P1", "old root must no longer be at depth 0");
    for tab in after.tabs.iter().filter(|t| t.id != new_root.id) {
        assert_eq!(tab.parent_tab_id.as_deref(), Some("P3"), "every former child re-parented to the new root");
    }
    assert_eq!(model.tab_for("t1").unwrap().claude_session_id.as_deref(), Some("S2"));
    assert_eq!(new_root.claude_session_id.as_deref(), Some("S0"), "new root pins the session current on the old root before its branch");
    assert_eq!(model.tab_for("P1").unwrap().claude_session_id.as_deref(), Some("S0-PRIME"));
}

#[test]
fn closing_parent_clears_child_parent_tab_id() {
    let mut model = branch_seed("OLD", "New tab");
    model.insert_branch_parent("t1", "P1", "P1-c", "P1-t", "OLD");
    assert_eq!(project_by_id(&model, "p").tabs[1].parent_tab_id.as_deref(), Some("P1"));

    let (pi, ti) = model.project_tab_index("P1").unwrap();
    model.remove_tab(pi, ti);

    let after = project_by_id(&model, "p");
    assert_eq!(after.tabs.len(), 1);
    assert_eq!(after.tabs[0].id, "t1");
    assert!(after.tabs[0].parent_tab_id.is_none(), "child's parent_tab_id cleared when parent dissolves");
}

#[test]
fn closing_child_does_not_mutate_parent() {
    let mut model = branch_seed("OLD", "New tab");
    model.insert_branch_parent("t1", "P1", "P1-c", "P1-t", "OLD");

    let (pi, ti) = model.project_tab_index("t1").unwrap();
    model.remove_tab(pi, ti);

    let after = project_by_id(&model, "p");
    assert_eq!(after.tabs.len(), 1);
    assert_eq!(after.tabs[0].id, "P1");
    assert!(after.tabs[0].parent_tab_id.is_none(), "parent must NOT be cleared when an unrelated child closes");
}

#[test]
fn branch_signal_on_terminals_tab_is_no_op() {
    let mut model = model_empty("/tmp/main");
    let before = project_by_id(&model, TabModel::TERMINALS_PROJECT_ID).tabs.len();
    let result = model.insert_branch_parent(
        TabModel::MAIN_TERMINAL_TAB_ID,
        "ghost-parent",
        "ghost-c",
        "ghost-t",
        "FRESH",
    );
    assert!(result.is_none(), "Terminals-project tab must refuse a branch parent");
    assert_eq!(project_by_id(&model, TabModel::TERMINALS_PROJECT_ID).tabs.len(), before);
}

#[test]
fn prune_dangling_parent_references_clears_orphans() {
    let mut model = TabModel::with_fs("/tmp", fake_fs("/home", &[]));
    seed_claude_tab(&mut model, "p", "root", "s-root", "/tmp/p", true);
    // A tab pointing at an existing parent (kept) and one at a ghost (cleared).
    seed_claude_tab(&mut model, "p", "child-valid", "s-cv", "/tmp/p", true);
    seed_claude_tab(&mut model, "p", "child-orphan", "s-co", "/tmp/p", true);
    model.mutate_tab("child-valid", |t| t.parent_tab_id = Some("root".into()));
    model.mutate_tab("child-orphan", |t| t.parent_tab_id = Some("does-not-exist".into()));

    model.prune_dangling_parent_references();

    assert_eq!(model.tab_for("child-valid").unwrap().parent_tab_id.as_deref(), Some("root"), "valid parent kept");
    assert!(model.tab_for("child-orphan").unwrap().parent_tab_id.is_none(), "dangling parent cleared");
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

fn make_handoff_tab(id: &str, cwd: &str) -> Tab {
    let claude_pane_id = format!("{}-claude", id);
    let mut tab = Tab::new(id, "[HANDOFF] Some task", cwd);
    tab.panes = vec![
        claude(&claude_pane_id),
        terminal(&format!("{}-t1", id), "Terminal 1"),
    ];
    tab.active_pane_id = Some(claude_pane_id);
    tab.claude_session_id = Some("handoff-session".into());
    tab.title_manually_set = true;
    tab
}

#[test]
fn handoff_root_originating_tab_child_parent_is_originating_id_returns_true() {
    let mut model = TabModel::with_fs("/tmp", fake_fs("/home", &[]));
    seed_claude_tab(&mut model, "p", "t1", "s", "/tmp/p", true);

    let inserted = model.insert_handoff_child(make_handoff_tab("child1", "/tmp/p"), "t1");
    assert!(inserted);

    let project = project_by_id(&model, "p");
    assert_eq!(project.tabs.len(), 2);
    assert_eq!(project.tabs[0].id, "t1", "originating tab stays at index 0");
    assert_eq!(project.tabs[1].id, "child1", "handoff child placed right after originating");
    assert_eq!(project.tabs[1].parent_tab_id.as_deref(), Some("t1"), "child's parent is the originating (root anchor)");
}

#[test]
fn handoff_originating_tab_is_child_child_inherits_grandparent_returns_true() {
    let mut model = TabModel::with_fs("/tmp", fake_fs("/home", &[]));
    seed_claude_tab(&mut model, "p", "root", "s-root", "/tmp/p", true);
    // "originating" already points at root.
    seed_claude_tab(&mut model, "p", "originating", "session-orig", "/tmp/p", true);
    model.mutate_tab("originating", |t| t.parent_tab_id = Some("root".into()));

    let inserted = model.insert_handoff_child(make_handoff_tab("child1", "/tmp/p"), "originating");
    assert!(inserted);

    let handoff = model.tab_for("child1").expect("handoff child must exist");
    assert_eq!(
        handoff.parent_tab_id.as_deref(),
        Some("root"),
        "depth-1: child of a child inherits the root, not the direct parent"
    );
}

#[test]
fn handoff_unknown_under_tab_id_returns_false_no_insertion() {
    let mut model = TabModel::with_fs("/tmp", fake_fs("/home", &[]));
    seed_claude_tab(&mut model, "p", "t1", "s", "/tmp/p", true);
    let before = project_by_id(&model, "p").tabs.len();

    let inserted = model.insert_handoff_child(make_handoff_tab("child1", "/tmp/p"), "does-not-exist");
    assert!(!inserted, "unknown under_tab_id must return false");
    assert_eq!(project_by_id(&model, "p").tabs.len(), before);
}

#[test]
fn handoff_terminals_project_tab_returns_false_no_insertion() {
    let mut model = TabModel::with_fs("/tmp", fake_fs("/home", &[]));
    let before = project_by_id(&model, TabModel::TERMINALS_PROJECT_ID).tabs.len();
    let inserted =
        model.insert_handoff_child(make_handoff_tab("child1", "/tmp"), TabModel::MAIN_TERMINAL_TAB_ID);
    assert!(!inserted, "Terminals-project tab must refuse a handoff child");
    assert_eq!(project_by_id(&model, TabModel::TERMINALS_PROJECT_ID).tabs.len(), before);
}

// =====================================================================
// ensure_terminals_project_seeded (spawn-hook fire-once)
// =====================================================================

#[test]
fn ensure_existing_terminals_does_not_reorder_or_fire_hook() {
    let mut model = model_empty("/tmp/main"); // Terminals already at index 0
    let fired = Rc::new(Cell::new(0u32));
    let f = fired.clone();
    model.ensure_terminals_project_seeded(|_tab| f.set(f.get() + 1));
    assert_eq!(fired.get(), 0, "hook must not fire when Terminals already present");
    assert_eq!(model.projects[0].id, TabModel::TERMINALS_PROJECT_ID);
}

#[test]
fn ensure_moves_terminals_to_index_zero() {
    let mut model = model_empty("/tmp/main");
    // Displace Terminals to index 1 by inserting a project ahead of it.
    model.projects.insert(0, project_with("other", "OTHER", "/tmp/other", vec![]));
    assert_ne!(model.projects[0].id, TabModel::TERMINALS_PROJECT_ID);

    let fired = Rc::new(Cell::new(0u32));
    let f = fired.clone();
    model.ensure_terminals_project_seeded(|_tab| f.set(f.get() + 1));

    assert_eq!(model.projects[0].id, TabModel::TERMINALS_PROJECT_ID, "Terminals moved back to index 0");
    assert_eq!(fired.get(), 0, "a mere reorder must not fire the spawn hook");
}

#[test]
fn ensure_seeds_terminals_from_scratch_fires_hook_once() {
    let mut model = model_with("/tmp/main", &[]); // fake home "/home"
    model.projects.clear();

    let seen = Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let s = seen.clone();
    model.ensure_terminals_project_seeded(|tab| s.borrow_mut().push(tab.id.clone()));

    assert_eq!(model.projects[0].id, TabModel::TERMINALS_PROJECT_ID);
    let main = model.tab_for(TabModel::MAIN_TERMINAL_TAB_ID).unwrap();
    assert_eq!(main.panes[0].title, "Terminal 1");
    assert_eq!(main.next_terminal_index, 2);
    assert_eq!(main.cwd, "/home", "synthesized Main tab uses the FsProbe home");
    assert_eq!(
        seen.borrow().as_slice(),
        [TabModel::MAIN_TERMINAL_TAB_ID.to_string()],
        "spawn hook fires exactly once with the synthesized Main tab"
    );
}

// =====================================================================
// Validation invariant spot-probes (plan §Validation §4)
//   §4a (running + deferred-resume Claude coexist) lives in tab.rs.
//   §4b (add_pane monotonic after close) → add_pane_is_monotonic_after_closing_a_pane.
//   §4c (/branch on a root re-parents children) →
//       branch_on_root_preserves_depth1_by_reparenting_former_children.
// Plus the pure-helper pins the plan calls out by name.
// =====================================================================

#[test]
fn default_pane_title_per_kind() {
    assert_eq!(TabModel::default_pane_title(PaneKind::Claude, 0), "Claude");
    assert_eq!(TabModel::default_pane_title(PaneKind::Terminal, 7), "Terminal 7");
}

#[test]
fn neighbor_active_pane_id_prefers_slot_then_previous_then_none() {
    let panes = vec![terminal("a", "A"), terminal("b", "B")];
    // Removing index 0 (a) from [a,b] leaves [b]; the slot holds b.
    assert_eq!(TabModel::neighbor_active_pane_id(0, &panes), Some("a".into()));
    // Index past the end falls back to the previous (new last).
    assert_eq!(TabModel::neighbor_active_pane_id(2, &panes), Some("b".into()));
    // Empty post-removal array → None.
    assert_eq!(TabModel::neighbor_active_pane_id(0, &[]), None);
}

#[test]
fn apply_auto_title_caps_humanized_title_at_40_chars() {
    // humanize_session_title caps at 40 characters (TabModel.swift:780-783).
    let mut model = model_empty("/tmp/main");
    let id = inject_tab(&mut model, "New tab", "/tmp/cap-test", PaneKind::Terminal);
    // 12 words × "word" split on '-' → "Word word word ..." well over 40 chars.
    model.apply_auto_title(&id, "aaaa-bbbb-cccc-dddd-eeee-ffff-gggg-hhhh-iiii-jjjj");
    let title = model.tab_for(&id).unwrap().title.clone();
    assert!(title.chars().count() <= 40, "humanized title must be capped at 40 chars, got {:?}", title);
    assert_eq!(title, "Aaaa bbbb cccc dddd eeee ffff gggg hhhh", "capped on a word boundary, trailing space trimmed");
}


// MARK: - from_parts (R18 restore constructor)

#[test]
fn from_parts_does_not_seed_terminals_or_main() {
    // Unlike `new`/`with_fs`, restore's constructor trusts the saved grouping:
    // no synthesized Terminals project, no Main tab.
    let project = Project {
        id: "nice".into(),
        name: "Nice".into(),
        path: "/work".into(),
        tabs: vec![Tab::new("t1", "Ship", "/work")],
    };
    let model = TabModel::from_parts(vec![project], Some("t1".into()), fake_fs("/home", &[]));
    assert_eq!(model.projects.len(), 1);
    assert_eq!(model.projects[0].id, "nice");
    assert!(
        model
            .projects
            .iter()
            .all(|p| p.id != TabModel::TERMINALS_PROJECT_ID),
        "from_parts must NOT synthesize a Terminals project"
    );
    assert_eq!(model.active_tab_id(), Some("t1"));
}

#[test]
fn from_parts_preserves_empty_projects_and_no_active() {
    let model = TabModel::from_parts(vec![], None, fake_fs("/home", &[]));
    assert!(model.projects.is_empty());
    assert_eq!(model.active_tab_id(), None);
}

// MARK: - live_pane_counts (W5 quit/close counting)

#[test]
fn live_pane_counts_folds_both_kinds_over_is_alive() {
    let mut tab = Tab::new("t1", "Tab", "/w");
    tab.panes = vec![
        claude("c1"),
        claude("c2"),
        terminal("term1", "Terminal 1"),
    ];
    let project = Project {
        id: "p".into(),
        name: "P".into(),
        path: "/w".into(),
        tabs: vec![tab],
    };
    let model = TabModel::from_parts(vec![project], Some("t1".into()), fake_fs("/home", &[]));
    assert_eq!(model.live_pane_counts(), (2, 1));
}

#[test]
fn live_pane_counts_excludes_held_not_alive_panes() {
    let mut held_claude = claude("c1");
    held_claude.is_alive = false;
    let mut tab = Tab::new("t1", "Tab", "/w");
    tab.panes = vec![held_claude, terminal("term1", "Terminal 1")];
    let project = Project {
        id: "p".into(),
        name: "P".into(),
        path: "/w".into(),
        tabs: vec![tab],
    };
    let model = TabModel::from_parts(vec![project], Some("t1".into()), fake_fs("/home", &[]));
    assert_eq!(
        model.live_pane_counts(),
        (0, 1),
        "a held (not-alive) pane must not be counted"
    );
}

#[test]
fn live_pane_counts_counts_modelled_but_unspawned_panes() {
    // A restored pane hydrates is_alive = true before any pty spawns — it DOES
    // count (the Swift quirk, preserved).
    let persisted = PersistedTab {
        id: "t1".into(),
        title: "Restored".into(),
        cwd: "/w".into(),
        claude_session_id: Some("sid".into()),
        active_pane_id: None,
        panes: vec![PersistedPane {
            id: "c".into(),
            title: "Claude".into(),
            kind: PaneKind::Claude,
            cwd: None,
            title_manually_set: None,
        }],
        title_manually_set: None,
        parent_tab_id: None,
        next_terminal_index: None,
    };
    let project = Project {
        id: "p".into(),
        name: "P".into(),
        path: "/w".into(),
        tabs: vec![persisted.hydrate()],
    };
    let model = TabModel::from_parts(vec![project], Some("t1".into()), fake_fs("/home", &[]));
    assert_eq!(model.live_pane_counts(), (1, 0));
}
