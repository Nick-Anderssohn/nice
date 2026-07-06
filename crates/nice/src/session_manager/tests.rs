//! Ported `SessionsModel` unit tests (R13 slice 1) — the pure model-routing
//! half. Each case drives the [`SessionManager`] surface and asserts on the
//! [`TabModel`] document, exactly as the Swift `SessionsModelNavigationTests` /
//! `SessionsModelPaneCwdTests` / the `AppStatePaneLifecycleTests` title-policy
//! cases assert on `appState.tabs`. The Swift originals also spawn real ptys as a
//! side effect (`AppState` is live); the observable assertions are purely the
//! model mutations, which these reproduce without a gpui context — the spawn /
//! focus side effects are exercised by the slice-3 `session-lifecycle` scenario.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use nice_model::{Pane, PaneKind, Project, SidebarTabSelection, Tab, TabModel, TabStatus};
use nice_term_core::SpawnSpec;
use nice_term_view::TerminalEvent;

use super::{
    build_claude_exec_command, build_claude_extra_env, build_claude_prefill_command,
    claude_launch_display_command, claude_tab_title_from_args, claude_worktree_cwd, clip_title,
    compose_claude_reply, default_mint_id, merge_env_spec_wins, mint_session_uuid, parse_claude_title,
    ClaudeReplyDecision, ClaudeSessionMode, DissolveTerminus, PaneLaunchStatus, SessionManager,
    WindowShellEnv, PANE_TITLE_MAX,
};

/// A fresh empty selection for cascade tests that don't seed a multi-selection.
fn selection() -> SidebarTabSelection {
    SidebarTabSelection::new()
}

/// Seed a `[Claude, Terminal 1]` tab (Claude focused) into project `project_id`
/// (created or appended-to) — the Rust twin of `TabModelFixtures.seedClaudeTab`.
/// Returns `(claude_pane_id, terminal_pane_id)`. `is_claude_running` is explicit
/// so the paneHeld case can seed a running Claude and observe the flag clearing.
fn seed_claude_tab_in(
    model: &mut TabModel,
    project_id: &str,
    tab_id: &str,
    is_claude_running: bool,
) -> (String, String) {
    let claude_id = format!("{tab_id}-claude");
    let terminal_id = format!("{tab_id}-t1");
    let path = format!("/tmp/{project_id}");
    let mut claude = Pane::new(&claude_id, "Claude", PaneKind::Claude);
    claude.is_claude_running = is_claude_running;
    let mut tab = Tab::new(tab_id, "New tab", &path);
    tab.panes = vec![
        claude,
        Pane::new(&terminal_id, "Terminal 1", PaneKind::Terminal),
    ];
    tab.active_pane_id = Some(claude_id.clone());
    tab.next_terminal_index = 2;
    if let Some(p) = model.projects.iter_mut().find(|p| p.id == project_id) {
        p.tabs.push(tab);
    } else {
        model.projects.push(Project {
            id: project_id.into(),
            name: project_id.to_uppercase(),
            path: path.into(),
            tabs: vec![tab],
        });
    }
    (claude_id, terminal_id)
}

/// A manager with a deterministic, collision-free id minter (`<prefix>N`) so
/// ported tests that add panes can reason about ids if they need to.
fn counting_manager() -> SessionManager {
    let counter = AtomicU64::new(0);
    SessionManager::with_mint_id(move |prefix| {
        format!("{prefix}{}", counter.fetch_add(1, Ordering::Relaxed))
    })
}

/// The freshly-seeded window model: pinned Terminals group + Main tab (one
/// "Terminal 1" pane, `next_terminal_index = 2`, that pane active).
fn seeded() -> TabModel {
    TabModel::new("/home/u")
}

fn main_tab_id() -> &'static str {
    TabModel::MAIN_TERMINAL_TAB_ID
}

/// Snapshot of the Main terminal tab (re-read on each access so assertions
/// observe the latest mutation).
fn main_tab(model: &TabModel) -> &Tab {
    model.tab_for(TabModel::MAIN_TERMINAL_TAB_ID).unwrap()
}

/// Seed a bare terminal tab (`tab_id` with a single terminal pane `pane_id`,
/// `Tab.cwd == tab_cwd`) into a fresh non-Terminals project — the Rust twin of
/// `SessionsModelPaneCwdTests.seedTerminalTab`.
fn seed_terminal_tab(model: &mut TabModel, tab_id: &str, pane_id: &str, tab_cwd: &str) {
    let mut tab = Tab::new(tab_id, "Terminal", tab_cwd);
    tab.panes = vec![Pane::new(pane_id, "zsh", PaneKind::Terminal)];
    tab.active_pane_id = Some(pane_id.to_string());
    model.projects.push(Project {
        id: "p".into(),
        name: "P".into(),
        path: tab_cwd.into(),
        tabs: vec![tab],
    });
}

/// Seed a `[Claude, Terminal 1]` tab (Claude focused) into a non-Terminals
/// project — the Rust twin of `AppStatePaneLifecycleTests.seedProjectWithClaudeTab`.
/// Returns `(claude_pane_id, terminal_pane_id)`. `is_claude_running` stays
/// `false` (its default), matching R13's invariant.
fn seed_claude_tab(model: &mut TabModel, tab_id: &str) -> (String, String) {
    let claude_id = format!("{tab_id}-claude");
    let terminal_id = format!("{tab_id}-t1");
    let mut tab = Tab::new(tab_id, "New tab", "/home/u/proj");
    tab.panes = vec![
        Pane::new(&claude_id, "Claude", PaneKind::Claude),
        Pane::new(&terminal_id, "Terminal 1", PaneKind::Terminal),
    ];
    tab.active_pane_id = Some(claude_id.clone());
    tab.next_terminal_index = 2;
    model.projects.push(Project {
        id: "p".into(),
        name: "P".into(),
        path: "/home/u/proj".into(),
        tabs: vec![tab],
    });
    (claude_id, terminal_id)
}

// ===========================================================================
// SessionsModelNavigationTests (ported)
// ===========================================================================

/// Add a second terminal pane to Main so pane-navigation has something to step
/// through — the Rust twin of `addExtraTerminalPaneToMain` (goes through
/// `add_pane`, which in the live app spawns; here the model half).
fn add_extra_terminal_pane_to_main(mgr: &mut SessionManager, model: &mut TabModel) -> String {
    mgr.add_pane(model, main_tab_id(), None).unwrap()
}

#[test]
fn next_pane_moves_right_when_not_at_end() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    add_extra_terminal_pane_to_main(&mut mgr, &mut model);

    let tab = main_tab(&model);
    assert_eq!(tab.panes.len(), 2);
    let first_id = tab.panes[0].id.clone();
    let second_id = tab.panes[1].id.clone();

    mgr.set_active_pane(&mut model, main_tab_id(), &first_id);
    mgr.select_next_pane(&mut model);
    assert_eq!(main_tab(&model).active_pane_id.as_ref(), Some(&second_id));
}

#[test]
fn next_pane_wraps_to_first_when_at_last() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    add_extra_terminal_pane_to_main(&mut mgr, &mut model);

    let tab = main_tab(&model);
    let first_id = tab.panes[0].id.clone();
    let last_id = tab.panes.last().unwrap().id.clone();

    mgr.set_active_pane(&mut model, main_tab_id(), &last_id);
    mgr.select_next_pane(&mut model);
    assert_eq!(main_tab(&model).active_pane_id.as_ref(), Some(&first_id));
}

#[test]
fn prev_pane_wraps_to_last_when_at_first() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    add_extra_terminal_pane_to_main(&mut mgr, &mut model);

    let tab = main_tab(&model);
    let first_id = tab.panes[0].id.clone();
    let last_id = tab.panes.last().unwrap().id.clone();

    mgr.set_active_pane(&mut model, main_tab_id(), &first_id);
    mgr.select_prev_pane(&mut model);
    assert_eq!(main_tab(&model).active_pane_id.as_ref(), Some(&last_id));
}

#[test]
fn next_pane_is_noop_when_single_pane() {
    // The seeded Main tab starts with a single pane; stepping must not move.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let original_active = main_tab(&model).active_pane_id.clone();

    mgr.select_next_pane(&mut model);
    assert_eq!(main_tab(&model).active_pane_id, original_active);
}

#[test]
fn add_terminal_to_active_tab_appends_terminal_and_focuses() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    model.select_tab(main_tab_id());
    let original_count = main_tab(&model).panes.len();

    mgr.add_terminal_to_active_tab(&mut model);

    let tab = main_tab(&model);
    assert_eq!(tab.panes.len(), original_count + 1);
    let new_pane = tab.panes.last().unwrap();
    assert_eq!(new_pane.kind, PaneKind::Terminal);
    // Seed consumed slot 1 ("Terminal 1"); the add is auto-named "Terminal 2".
    assert_eq!(new_pane.title, "Terminal 2");
    assert_eq!(tab.active_pane_id.as_ref(), Some(&new_pane.id));
}

/// Rust twin of `test_helpers_areNoOpWhenActiveTabIdIsNil`, adapted to the
/// Rust model's invariant. Swift set `activeTabId = nil` directly; the Rust
/// `TabModel` has **no `None` writer** for `active_tab_id` post-construction
/// (the sole writer, `set_active_tab_id`, is private and only ever sets `Some`),
/// so the literal nil case is unreachable. This ports the reachable half of the
/// Swift intent: the pane-navigation helpers are safe no-ops with nothing to
/// step through, and the sidebar step is a no-op with a single navigable tab
/// (the "single navigable id ⇒ no-op" tail the Swift case also asserts).
#[test]
fn helpers_are_safe_noops_when_nothing_to_navigate() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    // Fresh window: one navigable tab (Main), one pane.
    let before_active_tab = model.active_tab_id().map(str::to_owned);
    let before_active_pane = main_tab(&model).active_pane_id.clone();

    // Single-pane tab: pane stepping is a no-op (must not crash or move).
    mgr.select_next_pane(&mut model);
    mgr.select_prev_pane(&mut model);
    assert_eq!(main_tab(&model).active_pane_id, before_active_pane);

    // Single navigable sidebar tab: stepping the sidebar is a no-op too.
    model.select_next_sidebar_tab();
    assert_eq!(model.active_tab_id().map(str::to_owned), before_active_tab);
}

// ===========================================================================
// SessionsModelPaneCwdTests (ported)
// ===========================================================================

#[test]
fn pane_cwd_changed_stores_on_pane() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_terminal_tab(&mut model, "t1", "p1", "/tmp");

    let changed = mgr.pane_cwd_changed(&mut model, "t1", "p1", "/Users/nick/Downloads");

    assert!(changed, "a real cwd change reports changed");
    assert_eq!(
        model.tab_for("t1").unwrap().panes[0].cwd.as_deref(),
        Some("/Users/nick/Downloads"),
        "OSC 7 update must land on Pane.cwd"
    );
}

#[test]
fn pane_cwd_changed_does_not_mutate_tab_cwd() {
    // Tab.cwd is load-bearing for `claude --resume` — a companion terminal's cd
    // must never relocate the session's anchor.
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_terminal_tab(&mut model, "t1", "p1", "/tmp/anchor");

    mgr.pane_cwd_changed(&mut model, "t1", "p1", "/Users/nick/Downloads");

    assert_eq!(
        model.tab_for("t1").unwrap().cwd,
        "/tmp/anchor",
        "Tab.cwd must stay anchored even when a pane cd's elsewhere"
    );
}

#[test]
fn pane_cwd_changed_unknown_pane_is_noop() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_terminal_tab(&mut model, "t1", "p1", "/tmp");

    let changed = mgr.pane_cwd_changed(&mut model, "t1", "ghost", "/Users/nick");

    assert!(!changed);
    assert_eq!(
        model.tab_for("t1").unwrap().panes[0].cwd, None,
        "stale paneId must not invent a cwd on the wrong pane"
    );
}

#[test]
fn pane_cwd_changed_unknown_tab_is_noop() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_terminal_tab(&mut model, "t1", "p1", "/tmp");

    let changed = mgr.pane_cwd_changed(&mut model, "ghost-tab", "p1", "/Users/nick");

    assert!(!changed);
    assert_eq!(model.tab_for("t1").unwrap().panes[0].cwd, None);
}

// ===========================================================================
// Terminal-branch title policy (ported from AppStatePaneLifecycleTests)
// ===========================================================================

#[test]
fn pane_title_changed_terminal_pane_updates_pane_title() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (_claude, terminal_id) = seed_claude_tab(&mut model, "t1");

    mgr.pane_title_changed(&mut model, "t1", &terminal_id, "nvim foo.rb");

    let pane = model
        .tab_for("t1")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == terminal_id)
        .unwrap();
    assert_eq!(pane.title, "nvim foo.rb");
}

#[test]
fn pane_title_changed_terminal_pane_empty_title_ignored() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (_claude, terminal_id) = seed_claude_tab(&mut model, "t1");
    let before = model
        .tab_for("t1")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == terminal_id)
        .unwrap()
        .title
        .clone();

    mgr.pane_title_changed(&mut model, "t1", &terminal_id, "   \n");

    let after = model
        .tab_for("t1")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == terminal_id)
        .unwrap()
        .title
        .clone();
    assert_eq!(
        after, before,
        "Whitespace-only titles must not overwrite the current title."
    );
}

#[test]
fn pane_title_changed_terminal_pane_manually_set_ignores_osc_title() {
    // Once the user renames a terminal pane, OSC titles from the running program
    // must not overwrite their custom label.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (_claude, terminal_id) = seed_claude_tab(&mut model, "t1");

    model.rename_pane("t1", &terminal_id, "build watcher");
    assert!(
        model
            .tab_for("t1")
            .unwrap()
            .panes
            .iter()
            .find(|p| p.id == terminal_id)
            .unwrap()
            .title_manually_set,
        "Pre-condition: rename must flip the lock."
    );

    mgr.pane_title_changed(&mut model, "t1", &terminal_id, "nvim foo.rb");

    let pane = model
        .tab_for("t1")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == terminal_id)
        .unwrap();
    assert_eq!(
        pane.title, "build watcher",
        "OSC titles must not overwrite a manually-renamed terminal pane."
    );
}

#[test]
fn pane_title_changed_terminal_empty_submit_releases_lock_then_accepts_osc() {
    // Empty-submit in the pill editor releases the lock; the next OSC flows in.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (_claude, terminal_id) = seed_claude_tab(&mut model, "t1");

    model.rename_pane("t1", &terminal_id, "logs");
    model.rename_pane("t1", &terminal_id, "");
    assert!(
        !model
            .tab_for("t1")
            .unwrap()
            .panes
            .iter()
            .find(|p| p.id == terminal_id)
            .unwrap()
            .title_manually_set,
        "Pre-condition: empty submit must clear the lock."
    );

    mgr.pane_title_changed(&mut model, "t1", &terminal_id, "vim x.swift");

    let pane = model
        .tab_for("t1")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == terminal_id)
        .unwrap();
    assert_eq!(
        pane.title, "vim x.swift",
        "After releasing the lock, OSC titles must flow into the pill again."
    );
}

#[test]
fn pane_title_changed_terminal_pane_clips_at_40_chars() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (_claude, terminal_id) = seed_claude_tab(&mut model, "t1");
    let long: String = "x".repeat(80);

    mgr.pane_title_changed(&mut model, "t1", &terminal_id, &long);

    let pane = model
        .tab_for("t1")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == terminal_id)
        .unwrap();
    assert_eq!(
        pane.title.chars().count(),
        40,
        "Terminal titles must cap at 40 chars so the toolbar pill doesn't overflow."
    );
}

// ===========================================================================
// Claude-branch is_claude_running gate (ported deferred-resume cases)
// ===========================================================================

#[test]
fn pane_title_changed_claude_deferred_resume_ignores_shell_title() {
    // A deferred-resume Claude pane is a plain zsh (is_claude_running == false);
    // its theme OSC titles ("user@host:cwd") must not clobber the persisted
    // session label. The whole Claude branch drops on the gate this cycle.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal) = seed_claude_tab(&mut model, "t1");
    model.apply_auto_title("t1", "fix-top-bar-height");
    assert_eq!(
        model.tab_for("t1").unwrap().title,
        "Fix top bar height",
        "Precondition: tab has a real auto-titled label."
    );

    mgr.pane_title_changed(
        &mut model,
        "t1",
        &claude_id,
        "Nick@Nicks MacBook Air:~/Projects/nice",
    );

    assert_eq!(
        model.tab_for("t1").unwrap().title,
        "Fix top bar height",
        "OSC titles from a deferred-resume Claude pane (zsh, not claude) \
         must not overwrite the persisted session title."
    );
    // The Claude pill label is likewise untouched.
    let pane = model
        .tab_for("t1")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == claude_id)
        .unwrap();
    assert_eq!(pane.title, "Claude");
}

#[test]
fn pane_title_changed_claude_deferred_resume_ignores_status_prefix() {
    // Defensive: a braille/sparkle status prefix from a non-claude process must
    // not flip the pane status while is_claude_running is false — the
    // spinner/sparkle vocabulary belongs to claude (R15).
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal) = seed_claude_tab(&mut model, "t1");
    let title_before = model.tab_for("t1").unwrap().title.clone();

    // U+2840 is inside the braille spinner range Claude uses for "thinking".
    mgr.pane_title_changed(&mut model, "t1", &claude_id, "\u{2840} fix-bug");

    let pane = model
        .tab_for("t1")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == claude_id)
        .unwrap();
    assert_eq!(
        pane.status,
        nice_model::TabStatus::Idle,
        "Status transitions are gated on is_claude_running."
    );
    assert_eq!(
        model.tab_for("t1").unwrap().title,
        title_before,
        "Tab title must not change while is_claude_running is false."
    );
}

// ===========================================================================
// Claude-branch T5 status parsing (ported AppStatePaneLifecycleTests, running)
// ===========================================================================

/// Seed a `[Claude, Terminal 1]` tab whose Claude pane is **running** (the
/// post-promotion / creation state that opens the T5 OSC gate). Selects the tab
/// so `apply_status_transition`'s viewed-pane ack fires like the shipped window.
fn seed_running_claude_tab(model: &mut TabModel, tab_id: &str) -> (String, String) {
    let (claude_id, terminal_id) = seed_claude_tab(model, tab_id);
    model.mutate_tab(tab_id, |tab| {
        if let Some(p) = tab.panes.iter_mut().find(|p| p.id == claude_id) {
            p.is_claude_running = true;
        }
    });
    model.select_tab(tab_id);
    (claude_id, terminal_id)
}

#[test]
fn pane_title_changed_claude_braille_spinner_sets_thinking_and_humanizes_title() {
    // U+2840 is inside the braille spinner range (0x2800..=0x28FF) Claude uses
    // for "thinking"; the trailing label humanizes onto the tab title.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _t) = seed_running_claude_tab(&mut model, "t1");

    mgr.pane_title_changed(&mut model, "t1", &claude_id, "\u{2840} fix-top-bar-height");

    let pane = model
        .tab_for("t1")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == claude_id)
        .unwrap();
    assert_eq!(pane.status, nice_model::TabStatus::Thinking);
    assert_eq!(model.tab_for("t1").unwrap().title, "Fix top bar height");
}

#[test]
fn pane_title_changed_claude_sparkle_sets_waiting() {
    // U+2733 (✳) is the sparkle Claude uses for "waiting for input."
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _t) = seed_running_claude_tab(&mut model, "t1");

    mgr.pane_title_changed(&mut model, "t1", &claude_id, "\u{2733} needs-input");

    let pane = model
        .tab_for("t1")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == claude_id)
        .unwrap();
    assert_eq!(pane.status, nice_model::TabStatus::Waiting);
}

#[test]
fn pane_title_changed_claude_placeholder_label_ignored() {
    // "Claude Code" is the generic placeholder Claude emits before a session has
    // a real name — it must not clobber an existing tab title.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _t) = seed_running_claude_tab(&mut model, "t1");

    mgr.pane_title_changed(&mut model, "t1", &claude_id, "\u{2840} fix-bug");
    assert_eq!(model.tab_for("t1").unwrap().title, "Fix bug");

    mgr.pane_title_changed(&mut model, "t1", &claude_id, "\u{2840} Claude Code");
    assert_eq!(
        model.tab_for("t1").unwrap().title,
        "Fix bug",
        "Placeholder 'Claude Code' must not overwrite a real session title."
    );
}

#[test]
fn pane_title_changed_claude_unknown_prefix_treated_as_label() {
    // A non-braille, non-sparkle first char means no status update — the whole
    // string is the label.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _t) = seed_running_claude_tab(&mut model, "t1");

    mgr.pane_title_changed(&mut model, "t1", &claude_id, "refactor-auth-layer");

    let pane = model
        .tab_for("t1")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == claude_id)
        .unwrap();
    assert_eq!(pane.status, nice_model::TabStatus::Idle, "no prefix ⇒ no status change");
    assert_eq!(model.tab_for("t1").unwrap().title, "Refactor auth layer");
}

#[test]
fn pane_title_changed_claude_manually_set_pane_still_flips_status() {
    // The pane-level title lock is a *title* lock, not a *status* lock: a renamed
    // Claude pane must still flip status when claude emits a braille prefix, and
    // its custom pill name must survive (the OSC gate lives in the terminal
    // branch, never blocking status extraction).
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _t) = seed_running_claude_tab(&mut model, "t1");
    model.mutate_tab("t1", |tab| {
        if let Some(p) = tab.panes.iter_mut().find(|p| p.id == claude_id) {
            p.title = "deploy session".to_string();
            p.title_manually_set = true;
        }
    });

    mgr.pane_title_changed(&mut model, "t1", &claude_id, "\u{2840} fix-top-bar-height");

    let pane = model
        .tab_for("t1")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == claude_id)
        .unwrap();
    assert_eq!(pane.status, nice_model::TabStatus::Thinking, "status still flips");
    assert_eq!(pane.title, "deploy session", "the user's custom pill name survives");
}

#[test]
fn pane_title_changed_claude_accepts_title_after_promotion() {
    // The full deferred-resume → live-claude story: the gate holds against zsh
    // OSC while is_claude_running is false, and RELEASES after the promotion
    // flips it true (`AppStatePaneLifecycleTests.acceptsTitleAfterPromotion`).
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _t) = seed_claude_tab(&mut model, "t1"); // is_claude_running == false
    model.select_tab("t1");
    let title_before = model.tab_for("t1").unwrap().title.clone();

    // Pre-promotion: zsh OSC ignored.
    mgr.pane_title_changed(&mut model, "t1", &claude_id, "Nick@host:~/repo");
    assert_eq!(
        model.tab_for("t1").unwrap().title,
        title_before,
        "gate must hold before is_claude_running flips true"
    );

    // Simulate the socket-handshake promotion that flips the flag.
    model.mutate_tab("t1", |tab| {
        if let Some(p) = tab.panes.iter_mut().find(|p| p.id == claude_id) {
            p.is_claude_running = true;
        }
    });

    // Post-promotion: real claude OSC accepted; status flips, label humanizes.
    mgr.pane_title_changed(&mut model, "t1", &claude_id, "\u{2840} fix-bug");
    let pane = model
        .tab_for("t1")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == claude_id)
        .unwrap();
    assert_eq!(pane.status, nice_model::TabStatus::Thinking, "status fires once the gate releases");
    assert_eq!(model.tab_for("t1").unwrap().title, "Fix bug", "auto-title applies once the gate releases");
}

// ===========================================================================
// set_active_pane model-half: ack-when-viewed (SessionsModel.swift:534-545)
// ===========================================================================

#[test]
fn set_active_pane_acknowledges_waiting_pane_when_tab_is_viewed() {
    // A waiting pane that becomes active while its tab is the viewed tab lands
    // acknowledged (no lingering pulse) — the `markAcknowledgedIfWaiting` side
    // effect of setActivePane.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, terminal_id) = seed_claude_tab(&mut model, "t1");
    model.select_tab("t1"); // t1 is the viewed tab

    // Claude pane enters waiting while the companion terminal is active.
    model.mutate_tab("t1", |tab| {
        tab.active_pane_id = Some(terminal_id.clone());
        let pane = tab.panes.iter_mut().find(|p| p.id == claude_id).unwrap();
        pane.apply_status_transition(nice_model::TabStatus::Waiting, false);
    });
    assert!(
        !model
            .tab_for("t1")
            .unwrap()
            .panes
            .iter()
            .find(|p| p.id == claude_id)
            .unwrap()
            .waiting_acknowledged
    );

    // Focusing the waiting Claude pane while viewing t1 acknowledges it.
    mgr.set_active_pane(&mut model, "t1", &claude_id);

    let pane = model
        .tab_for("t1")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == claude_id)
        .unwrap();
    assert_eq!(pane.status, nice_model::TabStatus::Waiting);
    assert!(
        pane.waiting_acknowledged,
        "activating a waiting pane on the viewed tab must acknowledge it"
    );
}

#[test]
fn set_active_pane_does_not_acknowledge_when_tab_not_viewed() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal) = seed_claude_tab(&mut model, "t1");
    // Main is the viewed tab, not t1.
    model.select_tab(TabModel::MAIN_TERMINAL_TAB_ID);
    model.mutate_tab("t1", |tab| {
        let pane = tab.panes.iter_mut().find(|p| p.id == claude_id).unwrap();
        pane.apply_status_transition(nice_model::TabStatus::Waiting, false);
    });

    mgr.set_active_pane(&mut model, "t1", &claude_id);

    let pane = model
        .tab_for("t1")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == claude_id)
        .unwrap();
    assert!(
        !pane.waiting_acknowledged,
        "activating a pane on an unviewed tab must not acknowledge its pulse"
    );
}

#[test]
fn set_active_pane_unknown_pane_never_dangles_active() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let before = main_tab(&model).active_pane_id.clone();

    mgr.set_active_pane(&mut model, main_tab_id(), "ghost-pane");

    assert_eq!(
        main_tab(&model).active_pane_id,
        before,
        "selecting a pane not on the tab must leave active_pane_id untouched"
    );
}

// ===========================================================================
// route_terminal_event: mapped OSC events reach the model (title/cwd routing)
// ===========================================================================

#[test]
fn route_title_changed_updates_terminal_pane_pill() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (_claude, terminal_id) = seed_claude_tab(&mut model, "t1");

    mgr.route_terminal_event(
        &mut model,
        &mut selection(),
        "t1",
        &terminal_id,
        &TerminalEvent::TitleChanged("nvim foo.rb".to_string()),
    );

    let pane = model
        .tab_for("t1")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == terminal_id)
        .unwrap();
    assert_eq!(pane.title, "nvim foo.rb");
}

#[test]
fn route_cwd_changed_writes_pane_cwd_only() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_terminal_tab(&mut model, "t1", "p1", "/tmp/anchor");

    mgr.route_terminal_event(
        &mut model,
        &mut selection(),
        "t1",
        "p1",
        &TerminalEvent::CwdChanged(std::path::PathBuf::from("/Users/nick/Downloads")),
    );

    let tab = model.tab_for("t1").unwrap();
    assert_eq!(tab.panes[0].cwd.as_deref(), Some("/Users/nick/Downloads"));
    assert_eq!(tab.cwd, "/tmp/anchor", "Tab.cwd stays anchored");
}

#[test]
fn route_title_reset_and_output_started_leave_the_pill() {
    // TitleReset carries no new label (terminal title-policy only accepts a
    // non-empty set); OutputStarted only clears the launch overlay. Neither may
    // panic or mutate the pill. (Exited routes to pane_exited — covered by the
    // paneExited / route-exit cases below.)
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (_claude, terminal_id) = seed_claude_tab(&mut model, "t1");
    let before = model
        .tab_for("t1")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == terminal_id)
        .unwrap()
        .title
        .clone();

    mgr.route_terminal_event(
        &mut model,
        &mut selection(),
        "t1",
        &terminal_id,
        &TerminalEvent::TitleReset,
    );
    mgr.route_terminal_event(
        &mut model,
        &mut selection(),
        "t1",
        &terminal_id,
        &TerminalEvent::OutputStarted,
    );

    let after = model
        .tab_for("t1")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == terminal_id)
        .unwrap()
        .title
        .clone();
    assert_eq!(after, before, "reset + first-output must not touch the pill");
}

// ===========================================================================
// Pure helpers
// ===========================================================================

#[test]
fn clip_title_caps_at_char_boundary_not_bytes() {
    let long: String = "x".repeat(80);
    assert_eq!(clip_title(&long, PANE_TITLE_MAX).chars().count(), 40);
    // A short title passes through untouched.
    assert_eq!(clip_title("nvim foo.rb", PANE_TITLE_MAX), "nvim foo.rb");
    // Multi-byte chars are counted by char, not byte (10 CJK chars < 40).
    let cjk = "工作".repeat(5); // 10 chars, 30 bytes
    assert_eq!(clip_title(&cjk, PANE_TITLE_MAX), cjk);
}

#[test]
fn default_mint_id_is_prefixed_and_unique() {
    // The monotonic counter in the suffix makes uniqueness exact, not
    // probabilistic: a batch of back-to-back mints carries no duplicates.
    let ids: Vec<String> = (0..64).map(|_| default_mint_id("t1-p")).collect();
    assert!(ids.iter().all(|id| id.starts_with("t1-p")));
    let mut dedup = ids.clone();
    dedup.sort();
    dedup.dedup();
    assert_eq!(dedup.len(), ids.len(), "back-to-back mints must not collide");
}

// ===========================================================================
// AppStatePaneLifecycleTests — paneExited (ported)
// ===========================================================================

#[test]
fn pane_exited_removes_pane_and_shifts_active_to_neighbor() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, terminal_id) = seed_claude_tab_in(&mut model, "p", "t1", false);
    model.select_tab("t1");

    // Focus the claude pane, then exit it — focus must shift to the neighbor
    // (the terminal pane at index 1).
    mgr.set_active_pane(&mut model, "t1", &claude_id);
    let res = mgr.pane_exited(&mut model, &mut selection(), "t1", &claude_id);

    let tab = model.tab_for("t1").unwrap();
    assert_eq!(tab.panes.len(), 1);
    assert_eq!(tab.panes[0].id, terminal_id);
    assert_eq!(
        tab.active_pane_id.as_deref(),
        Some(terminal_id.as_str()),
        "focus must shift to the surviving pane; a dangling activePaneId breaks the toolbar"
    );
    assert_eq!(
        res.refocus_tab.as_deref(),
        Some("t1"),
        "the tab survived → the live caller spawns the refocused companion (step 4)"
    );
    assert_eq!(res.terminus, DissolveTerminus::None);
}

#[test]
fn pane_exited_last_pane_dissolves_tab() {
    // Seed two extra projects so dissolving one tab doesn't empty everything
    // (which would fire the window terminus).
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (c1, term1) = seed_claude_tab_in(&mut model, "p1", "t1", false);
    seed_claude_tab_in(&mut model, "p2", "t2", false);

    mgr.pane_exited(&mut model, &mut selection(), "t1", &c1);
    mgr.pane_exited(&mut model, &mut selection(), "t1", &term1);

    assert!(
        model.tab_for("t1").is_none(),
        "tab must dissolve once every pane exits"
    );
    assert!(
        model.tab_for("t2").is_some(),
        "other tabs must not be touched by one tab's dissolve"
    );
}

#[test]
fn pane_exited_dissolved_active_tab_falls_back_to_first_available() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (c1, term1) = seed_claude_tab_in(&mut model, "p1", "t1", false);
    seed_claude_tab_in(&mut model, "p2", "t2", false);
    model.select_tab("t1");

    mgr.pane_exited(&mut model, &mut selection(), "t1", &c1);
    mgr.pane_exited(&mut model, &mut selection(), "t1", &term1);

    // Dissolving the active tab leaves active_tab_id at the first tab in
    // navigable order — the Terminals Main tab.
    assert_eq!(model.active_tab_id(), Some(TabModel::MAIN_TERMINAL_TAB_ID));
}

#[test]
fn pane_exited_unknown_pane_is_noop() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let before = main_tab(&model).panes.len();

    let res = mgr.pane_exited(&mut model, &mut selection(), main_tab_id(), "does-not-exist");

    assert_eq!(
        main_tab(&model).panes.len(),
        before,
        "unknown paneId must not corrupt state"
    );
    assert_eq!(
        res.refocus_tab.as_deref(),
        Some(main_tab_id()),
        "the tab survived untouched"
    );
    assert_eq!(res.terminus, DissolveTerminus::None);
}

#[test]
fn pane_exited_last_tab_of_last_project_reports_window_emptied() {
    // Dissolving the only tab in the window (the seeded Terminals Main tab, its
    // single pane) leaves every project empty — the terminus the live caller
    // turns into close-window-or-quit. (The Swift lifecycle tests deliberately
    // seed extra projects to AVOID this; here we pin the signal itself.)
    let mut mgr = counting_manager();
    let mut model = seeded();
    let main = main_tab_id();
    let pane_id = main_tab(&model).panes[0].id.clone();

    let res = mgr.pane_exited(&mut model, &mut selection(), main, &pane_id);

    assert!(model.tab_for(main).is_none(), "the last tab dissolved");
    assert_eq!(
        res.terminus,
        DissolveTerminus::WindowEmptied,
        "every project empty → the window-emptied terminus fires"
    );
}

// ===========================================================================
// AppStatePaneLifecycleTests — paneHeld (ported)
// ===========================================================================

#[test]
fn pane_held_flips_is_alive_and_idles_status() {
    // Seed a running Claude pane mid-think, then hold it: is_alive → false, the
    // pulsing status idles out, the ack clears, and is_claude_running clears.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal) = seed_claude_tab_in(&mut model, "p", "t1", true);
    model.mutate_tab("t1", |tab| {
        let pane = tab.panes.iter_mut().find(|p| p.id == claude_id).unwrap();
        pane.status = TabStatus::Thinking;
        pane.waiting_acknowledged = false;
    });

    mgr.pane_held(&mut model, "t1", &claude_id);

    let pane = model
        .tab_for("t1")
        .unwrap()
        .panes
        .iter()
        .find(|p| p.id == claude_id)
        .unwrap();
    assert!(!pane.is_alive, "paneHeld flips is_alive to false");
    assert_eq!(pane.status, TabStatus::Idle, "paneHeld idles the status out");
    assert!(
        !pane.waiting_acknowledged,
        "paneHeld clears waiting_acknowledged so a future waiting pane can pulse"
    );
    assert!(
        !pane.is_claude_running,
        "paneHeld clears is_claude_running (a held pty is a corpse, not a live shell)"
    );
}

#[test]
fn pane_held_keeps_pane_in_tab_panes_array() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal) = seed_claude_tab_in(&mut model, "p", "t1", false);
    let before = model.tab_for("t1").unwrap().panes.len();

    mgr.pane_held(&mut model, "t1", &claude_id);

    let tab = model.tab_for("t1").unwrap();
    assert_eq!(
        tab.panes.len(),
        before,
        "paneHeld must not remove the pane — that's paneExited's job"
    );
    assert!(
        tab.panes.iter().any(|p| p.id == claude_id),
        "the held pane must still be findable by id"
    );
}

#[test]
fn pane_held_clears_launch_overlay() {
    // Exit-before-first-byte: the overlay was still up when the process died;
    // paneHeld must clear it so the placeholder doesn't sit on the held footer.
    let mut mgr = counting_manager();
    let mut model = seeded();
    mgr.set_launch_overlay_grace(Duration::ZERO);
    let (claude_id, _terminal) = seed_claude_tab_in(&mut model, "p", "t1", false);
    mgr.register_pane_launch(&claude_id, "claude");
    assert!(
        mgr.pane_launch_state(&claude_id).is_some(),
        "pre-condition: overlay entry exists before paneHeld"
    );

    mgr.pane_held(&mut model, "t1", &claude_id);

    assert!(
        mgr.pane_launch_state(&claude_id).is_none(),
        "paneHeld must clear the launch overlay"
    );
}

#[test]
fn pane_held_unknown_pane_is_noop() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let before = main_tab(&model).panes.len();

    mgr.pane_held(&mut model, main_tab_id(), "does-not-exist");

    assert_eq!(main_tab(&model).panes.len(), before);
}

// ===========================================================================
// AppStateLaunchOverlayTests (ported)
// ===========================================================================

#[test]
fn register_pane_launch_zero_grace_immediately_visible() {
    let mut mgr = counting_manager();
    mgr.set_launch_overlay_grace(Duration::ZERO);

    let armed = mgr.register_pane_launch("p1", "claude -w foo");

    assert!(!armed, "zero grace promotes synchronously — no deadline to arm");
    assert_eq!(
        mgr.pane_launch_state("p1"),
        Some(&PaneLaunchStatus::Visible {
            command: "claude -w foo".into()
        }),
        "with a zero-second grace the overlay is promoted immediately"
    );
}

#[test]
fn clear_pane_launch_removes_visible_entry() {
    let mut mgr = counting_manager();
    mgr.set_launch_overlay_grace(Duration::ZERO);
    mgr.register_pane_launch("p1", "claude");
    assert_eq!(
        mgr.pane_launch_state("p1"),
        Some(&PaneLaunchStatus::Visible {
            command: "claude".into()
        })
    );

    mgr.clear_pane_launch("p1");

    assert!(
        mgr.pane_launch_state("p1").is_none(),
        "first-byte clear must remove the entry entirely"
    );
}

#[test]
fn clear_pane_launch_before_deadline_fires_suppresses_overlay() {
    // Non-zero grace → registration leaves the entry Pending (the live caller
    // arms the deadline). Clear before the deadline fires, then simulate the
    // deadline firing via promote_pane_launch: the Pending-guard early-exits.
    let mut mgr = counting_manager();
    mgr.set_launch_overlay_grace(Duration::from_millis(200));
    let armed = mgr.register_pane_launch("p1", "claude");
    assert!(armed, "non-zero grace defers to the injected deadline");
    assert_eq!(
        mgr.pane_launch_state("p1"),
        Some(&PaneLaunchStatus::Pending {
            command: "claude".into()
        })
    );

    mgr.clear_pane_launch("p1");
    // Deadline fires after the clear — must not resurrect the overlay.
    mgr.promote_pane_launch("p1");

    assert!(
        mgr.pane_launch_state("p1").is_none(),
        "a cleared pane must stay cleared even after the grace deadline fires"
    );
}

#[test]
fn register_pane_launch_async_path_promotes_on_deadline() {
    let mut mgr = counting_manager();
    mgr.set_launch_overlay_grace(Duration::from_millis(150));
    mgr.register_pane_launch("p1", "claude -w slow");
    assert_eq!(
        mgr.pane_launch_state("p1"),
        Some(&PaneLaunchStatus::Pending {
            command: "claude -w slow".into()
        }),
        "before the deadline the state is Pending — overlay stays hidden"
    );

    // The injected deadline fires (App-Nap-safe in production, direct here).
    mgr.promote_pane_launch("p1");

    assert_eq!(
        mgr.pane_launch_state("p1"),
        Some(&PaneLaunchStatus::Visible {
            command: "claude -w slow".into()
        }),
        "after the deadline the entry is promoted to Visible"
    );
}

#[test]
fn register_pane_launch_replaces_prior_entry() {
    // A second register for the same paneId replaces the first (defends against
    // in-place pane promotion re-using an id that already had state).
    let mut mgr = counting_manager();
    mgr.set_launch_overlay_grace(Duration::ZERO);
    mgr.register_pane_launch("p1", "claude");
    assert_eq!(
        mgr.pane_launch_state("p1"),
        Some(&PaneLaunchStatus::Visible {
            command: "claude".into()
        })
    );

    mgr.register_pane_launch("p1", "claude --resume");

    assert_eq!(
        mgr.pane_launch_state("p1"),
        Some(&PaneLaunchStatus::Visible {
            command: "claude --resume".into()
        }),
        "re-registering must overwrite the command, not stack entries"
    );
}

#[test]
fn pane_exited_clears_launch_state() {
    // A pane that exits — even silently, before emitting any byte — must not
    // leave a stale overlay entry behind.
    let mut mgr = counting_manager();
    let mut model = seeded();
    mgr.set_launch_overlay_grace(Duration::ZERO);
    let pane_id = "p-exit";
    let mut tab = Tab::new("t1", "t", "/tmp");
    tab.panes = vec![Pane::new(pane_id, "Claude", PaneKind::Claude)];
    tab.active_pane_id = Some(pane_id.to_string());
    model.projects.push(Project {
        id: "p".into(),
        name: "P".into(),
        path: "/tmp".into(),
        tabs: vec![tab],
    });
    mgr.register_pane_launch(pane_id, "claude");
    assert!(mgr.pane_launch_state(pane_id).is_some());

    mgr.pane_exited(&mut model, &mut selection(), "t1", pane_id);

    assert!(
        mgr.pane_launch_state(pane_id).is_none(),
        "an exited pane must leave no stale overlay entry"
    );
}

// ===========================================================================
// AppStateTabSelectionTests — prune wiring through the dissolve cascade (ported)
// ===========================================================================

#[test]
fn closing_tab_prunes_from_multi_selection() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_claude_tab_in(&mut model, "pa", "a", false);
    seed_claude_tab_in(&mut model, "pb", "b", false);
    let mut sel = SidebarTabSelection::new();
    sel.replace("a");
    let _ = sel.toggle("b");
    assert_eq!(
        sel.selected_tab_ids(),
        &HashSet::from(["a".to_string(), "b".to_string()])
    );

    mgr.close_tab(&mut model, &mut sel, "a");

    assert_eq!(
        sel.selected_tab_ids(),
        &HashSet::from(["b".to_string()]),
        "finalize_dissolved_tab must prune so closed tabs don't linger in the selection"
    );
}

#[test]
fn closing_tab_clears_anchor_when_anchor_was_the_closed_tab() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_claude_tab_in(&mut model, "pa", "a", false);
    seed_claude_tab_in(&mut model, "pb", "b", false);
    let mut sel = SidebarTabSelection::new();
    sel.replace("b");
    let _ = sel.toggle("a"); // toggle moves the anchor to the toggled id
    assert_eq!(sel.last_clicked_tab_id(), Some("a"));

    mgr.close_tab(&mut model, &mut sel, "a");

    assert_eq!(
        sel.last_clicked_tab_id(),
        None,
        "the anchor must clear when its tab dissolves"
    );
}

#[test]
fn closing_tab_keeps_anchor_when_anchor_survives() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_claude_tab_in(&mut model, "pa", "a", false);
    seed_claude_tab_in(&mut model, "pb", "b", false);
    let mut sel = SidebarTabSelection::new();
    sel.replace("b");
    let _ = sel.toggle("a"); // anchor is now `a`; we close `b` instead

    mgr.close_tab(&mut model, &mut sel, "b");

    assert_eq!(
        sel.last_clicked_tab_id(),
        Some("a"),
        "the anchor must survive when a different tab dissolves"
    );
    assert_eq!(sel.selected_tab_ids(), &HashSet::from(["a".to_string()]));
}

// ===========================================================================
// Tri-state close shapes — held / spawning / model-only all reach the cascade
// (AppStateCloseProjectTests's three no-live-child shapes + the
// NiceTerminalViewDeferredSpawnTests distinctions).
// ===========================================================================

#[test]
fn close_tab_claude_tab_with_unspawned_companion_dissolves() {
    // Model-only shape: neither pane has a session. Close must still dissolve
    // the row — an earlier cut left the tab alive with its unfocused companion.
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_claude_tab_in(&mut model, "p1", "t1", false);
    seed_claude_tab_in(&mut model, "p2", "t2", false); // keep off the window terminus

    mgr.close_tab(&mut model, &mut selection(), "t1");

    assert!(
        model.tab_for("t1").is_none(),
        "close must dissolve the tab even when the companion terminal was never spawned"
    );
    assert!(
        model.projects.iter().any(|p| p.id == "p1"),
        "close tab must leave the containing project in place (only close-project removes it)"
    );
}

#[test]
fn close_tab_armed_deferred_claude_pane_with_unspawned_companion_dissolves() {
    // Spawning shape: the Claude pane captured a deferred spawn that never fired
    // (paneIsSpawned true), the companion is model-only. Close routes the Claude
    // pane through terminate_pane's armed fast path → synthesized nil exit.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal) = seed_claude_tab_in(&mut model, "p1", "t1", false);
    seed_claude_tab_in(&mut model, "p2", "t2", false);
    mgr.mark_synthetic_armed_deferred_pane("t1", &claude_id);

    mgr.close_tab(&mut model, &mut selection(), "t1");

    assert!(
        model.tab_for("t1").is_none(),
        "close on a never-focused resume-deferred Claude tab must dissolve the sidebar row"
    );
    assert!(model.projects.iter().any(|p| p.id == "p1"));
}

#[test]
fn close_tab_held_claude_pane_with_unspawned_companion_dissolves() {
    // Held shape: the Claude pane's process already died (view mounted), the
    // companion is model-only. Close routes the held pane through terminate_pane's
    // held fast path → synchronous pane_exited → cascade.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal) = seed_claude_tab_in(&mut model, "p1", "t1", false);
    seed_claude_tab_in(&mut model, "p2", "t2", false);
    model.mutate_tab("t1", |tab| {
        let pane = tab.panes.iter_mut().find(|p| p.id == claude_id).unwrap();
        pane.is_alive = false;
        pane.is_claude_running = false;
    });
    mgr.mark_synthetic_held_pane("t1", &claude_id);

    mgr.close_tab(&mut model, &mut selection(), "t1");

    assert!(
        model.tab_for("t1").is_none(),
        "close on a held-pane tab must dissolve the row, not just remove the panes"
    );
    assert!(model.projects.iter().any(|p| p.id == "p1"));
}

// ===========================================================================
// Validation ordering probes (a)–(d)
// ===========================================================================

#[test]
fn probe_a_exit_refocuses_neighbor_and_flags_companion_spawn_before_dissolve() {
    // (a) Exiting the active pane refocuses the slot neighbor AND signals the
    // deferred-companion spawn (step 4), and the dissolve check runs AFTER — a
    // surviving tab with a refocused companion must NOT dissolve. pane_exited
    // returns refocus_tab=Some (→ the live caller spawns the companion) with
    // terminus=None, proving the exit handled the refocus-onto-companion case
    // instead of dissolving.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, terminal_id) = seed_claude_tab_in(&mut model, "p", "t1", false);
    model.select_tab("t1");
    mgr.set_active_pane(&mut model, "t1", &claude_id);

    let res = mgr.pane_exited(&mut model, &mut selection(), "t1", &claude_id);

    let tab = model.tab_for("t1").expect("tab must survive — a companion remains");
    assert_eq!(
        tab.active_pane_id.as_deref(),
        Some(terminal_id.as_str()),
        "focus refocuses onto the slot neighbor (the deferred companion)"
    );
    assert_eq!(
        res.refocus_tab.as_deref(),
        Some("t1"),
        "the surviving tab is flagged for the step-4 companion spawn"
    );
    assert_eq!(
        res.terminus,
        DissolveTerminus::None,
        "the dissolve check ran after the refocus and saw a non-empty tab"
    );
}

#[test]
fn probe_b_noop_title_re_report_reports_no_change() {
    // (b) A no-op title re-report fires no mutation event: pane_title_changed
    // returns did-change (the caller's R18 save gate). A real change returns
    // true; re-reporting the same title returns false.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (_claude, terminal_id) = seed_claude_tab_in(&mut model, "p", "t1", false);

    assert!(
        mgr.pane_title_changed(&mut model, "t1", &terminal_id, "nvim foo.rb"),
        "a real title change reports changed"
    );
    assert!(
        !mgr.pane_title_changed(&mut model, "t1", &terminal_id, "nvim foo.rb"),
        "re-reporting the current title must report no change (no mutation event)"
    );
}

#[test]
fn probe_c_terminate_all_two_held_panes_visits_each_once() {
    // (c) terminate_all with two held panes completes without skipping or
    // double-visiting an entry: both panes removed, tab dissolved, both synthetic
    // markers consumed. The snapshot-first iteration is what makes this safe (the
    // first held pane_exited mutates the model + cache mid-loop).
    let mut mgr = counting_manager();
    let mut model = seeded();
    // A tab with two panes, both marked held (kind is irrelevant to terminate).
    let mut tab = Tab::new("t1", "t", "/tmp/p1");
    tab.panes = vec![
        Pane::new("t1-a", "A", PaneKind::Terminal),
        Pane::new("t1-b", "B", PaneKind::Terminal),
    ];
    tab.active_pane_id = Some("t1-a".to_string());
    tab.next_terminal_index = 3;
    model.projects.push(Project {
        id: "p1".into(),
        name: "P1".into(),
        path: "/tmp/p1".into(),
        tabs: vec![tab],
    });
    seed_claude_tab_in(&mut model, "p2", "t2", false); // keep off the window terminus
    mgr.mark_synthetic_held_pane("t1", "t1-a");
    mgr.mark_synthetic_held_pane("t1", "t1-b");

    mgr.terminate_all(&mut model, &mut selection(), "t1");

    assert!(
        model.tab_for("t1").is_none(),
        "both held panes exit and the tab dissolves — no entry skipped"
    );
    // Both one-shot markers consumed exactly once (a double-visit would have
    // found the marker already gone and mis-routed as model-only).
    assert!(!mgr.pane_is_spawned("t1", "t1-a"));
    assert!(!mgr.pane_is_spawned("t1", "t1-b"));
}

#[test]
fn probe_d_close_model_only_tab_reaches_cascade_synchronously() {
    // (d) Closing a tab whose panes are all model-only reaches the cascade
    // synchronously — no async pane-exit to wait on, the tab is gone on return.
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_claude_tab_in(&mut model, "p1", "t1", false);
    seed_claude_tab_in(&mut model, "p2", "t2", false);

    let terminus = mgr.close_tab(&mut model, &mut selection(), "t1");

    assert!(
        model.tab_for("t1").is_none(),
        "a model-only tab dissolves synchronously on close_tab's return"
    );
    assert_eq!(terminus, DissolveTerminus::None, "other projects remain non-empty");
}

// ---- R14 env injection: the spec-wins merge + the per-pane matrix -----------
//
// The manager's `spawn_pane` merges these pairs into the caller-built spec's env
// before forking the pty. `spawn_pane` itself needs a gpui `App`, so the pure
// merge + matrix (`window_pane_env_pairs`, exercised here through a
// spawn_pane-shaped merge) are unit-tested directly (Validation §3 a/b/c); the
// full live spawn path is the `shell-socket` scenario.

/// Helper: a manager with a fully-populated window shell env (socket + zdotdir +
/// an inherited user zdotdir).
fn manager_with_shell_env(
    socket: Option<&str>,
    zdotdir: Option<&str>,
    user_zdotdir: Option<&str>,
) -> SessionManager {
    let mut m = SessionManager::new();
    m.set_window_shell_env(WindowShellEnv {
        socket_path: socket.map(str::to_string),
        zdotdir: zdotdir.map(str::to_string),
        user_zdotdir: user_zdotdir.map(str::to_string),
    });
    m
}

fn value_of<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// Validation §3(a): a `ZDOTDIR` the caller already set on the spec (the
/// deliberately-blanked shells) SURVIVES the manager's injection — spec wins.
#[test]
fn spec_provided_zdotdir_survives_manager_injection() {
    let mgr = manager_with_shell_env(Some("/tmp/sock"), Some("/managed/zdotdir"), Some("/user/z"));
    // A spec that blanks ZDOTDIR to its own cwd, exactly like the ~10 landed
    // scenarios (`SpawnSpec::with_env(vec![("ZDOTDIR", cwd)])`).
    let mut spec = SpawnSpec::shell("/work").with_env(vec![("ZDOTDIR".to_string(), "/work".to_string())]);
    merge_env_spec_wins(&mut spec.env, mgr.window_pane_env_pairs("t1", "p1"));

    assert_eq!(
        value_of(&spec.env, "ZDOTDIR"),
        Some("/work"),
        "the spec's blanked ZDOTDIR must win over the manager's injected value"
    );
    // Exactly one ZDOTDIR entry — the merge never duplicates a key.
    assert_eq!(
        spec.env.iter().filter(|(k, _)| k == "ZDOTDIR").count(),
        1,
        "no duplicate ZDOTDIR key"
    );
    // The keys the spec did NOT set are still injected.
    assert_eq!(value_of(&spec.env, "NICE_SOCKET"), Some("/tmp/sock"));
    assert_eq!(value_of(&spec.env, "NICE_TAB_ID"), Some("t1"));
}

/// Validation §3(b): a pane spawned through the manager carries
/// `NICE_SOCKET` + `NICE_TAB_ID` + `NICE_PANE_ID` (the exact ids handed to
/// `spawn_pane` — the same ids `ensure_active_pane_spawned` passes through).
#[test]
fn injected_pane_env_carries_socket_and_pane_identity() {
    let mgr = manager_with_shell_env(Some("/tmp/win.sock"), Some("/z"), Some("/user/z"));
    // A default shell spec (what `ensure_active_pane_spawned` builds), then the
    // exact merge `spawn_pane` performs.
    let mut spec = SpawnSpec::shell("/work");
    merge_env_spec_wins(&mut spec.env, mgr.window_pane_env_pairs("tabX", "paneY"));

    assert_eq!(value_of(&spec.env, "NICE_SOCKET"), Some("/tmp/win.sock"));
    assert_eq!(value_of(&spec.env, "NICE_TAB_ID"), Some("tabX"));
    assert_eq!(value_of(&spec.env, "NICE_PANE_ID"), Some("paneY"));
    assert_eq!(value_of(&spec.env, "ZDOTDIR"), Some("/z"));
}

/// Validation §3(c): `NICE_USER_ZDOTDIR` is present-but-EMPTY when Nice inherited
/// no `ZDOTDIR` (the empty/absent distinction the `.zshenv` stub keys off).
#[test]
fn user_zdotdir_is_present_but_empty_when_none_inherited() {
    let mgr = manager_with_shell_env(Some("/tmp/sock"), Some("/z"), None);
    let pairs = mgr.window_pane_env_pairs("t", "p");
    assert_eq!(
        value_of(&pairs, "NICE_USER_ZDOTDIR"),
        Some(""),
        "NICE_USER_ZDOTDIR must be SET (empty string), never absent"
    );
    assert!(
        pairs.iter().any(|(k, _)| k == "NICE_USER_ZDOTDIR"),
        "the key must be present"
    );
}

/// A manager with no bootstrapped socket injects NOTHING — the scenarios/itests
/// that build a `WindowState` directly keep their env untouched.
#[test]
fn unbootstrapped_manager_injects_no_env() {
    let mgr = SessionManager::new();
    assert!(
        mgr.window_pane_env_pairs("t", "p").is_empty(),
        "a manager with no window shell env must inject nothing"
    );
}

// ---- R14 build_claude_extra_env: the FROZEN per-mode matrix (R15 wires it) ---

/// EVERY mode sets TERM_PROGRAM + the ids + NICE_SOCKET, and a non-deferred mode
/// adds NONE of the ZDOTDIR / prefill trio (that is ResumeDeferred's alone).
#[test]
fn claude_extra_env_common_columns_for_every_mode() {
    for mode in [
        ClaudeSessionMode::None,
        ClaudeSessionMode::New("id".into()),
        ClaudeSessionMode::Resume("id".into()),
    ] {
        let env = build_claude_extra_env(
            &mode,
            "tab1",
            "pane1",
            Some("/tmp/s.sock"),
            Some("/z"),
            Some("/user/z"),
            None,
        );
        assert_eq!(value_of(&env, "TERM_PROGRAM"), Some("ghostty"));
        assert_eq!(value_of(&env, "NICE_TAB_ID"), Some("tab1"));
        assert_eq!(value_of(&env, "NICE_PANE_ID"), Some("pane1"));
        assert_eq!(value_of(&env, "NICE_SOCKET"), Some("/tmp/s.sock"));
        // The deferred-only trio is absent for non-deferred modes.
        assert_eq!(value_of(&env, "ZDOTDIR"), None, "{mode:?} must not set ZDOTDIR");
        assert_eq!(value_of(&env, "NICE_USER_ZDOTDIR"), None);
        assert_eq!(value_of(&env, "NICE_PREFILL_COMMAND"), None);
    }
}

/// No socket ⇒ no NICE_SOCKET (the only conditional common column).
#[test]
fn claude_extra_env_omits_socket_when_absent() {
    let env = build_claude_extra_env(&ClaudeSessionMode::None, "t", "p", None, None, None, None);
    assert_eq!(value_of(&env, "NICE_SOCKET"), None);
    assert_eq!(value_of(&env, "TERM_PROGRAM"), Some("ghostty"));
}

/// ResumeDeferred adds ZDOTDIR + the always-present NICE_USER_ZDOTDIR + the
/// pinned NICE_PREFILL_COMMAND format (`claude --resume <uuid>`, no settings).
#[test]
fn claude_extra_env_resume_deferred_sets_prefill_and_zdotdir() {
    let env = build_claude_extra_env(
        &ClaudeSessionMode::ResumeDeferred("SID-123".into()),
        "t1",
        "p1",
        Some("/tmp/s.sock"),
        Some("/managed/z"),
        Some("/user/z"),
        None,
    );
    assert_eq!(value_of(&env, "ZDOTDIR"), Some("/managed/z"));
    assert_eq!(value_of(&env, "NICE_USER_ZDOTDIR"), Some("/user/z"));
    assert_eq!(
        value_of(&env, "NICE_PREFILL_COMMAND"),
        Some("claude --resume SID-123"),
        "the frozen prefill format is `claude --resume <uuid>`"
    );
}

/// ResumeDeferred with no inherited user zdotdir still sets NICE_USER_ZDOTDIR to
/// the empty string (the .zshenv stub's absent/empty distinction).
#[test]
fn claude_extra_env_resume_deferred_user_zdotdir_empty_when_none() {
    let env = build_claude_extra_env(
        &ClaudeSessionMode::ResumeDeferred("S".into()),
        "t",
        "p",
        Some("/s"),
        Some("/z"),
        None,
        None,
    );
    assert_eq!(value_of(&env, "NICE_USER_ZDOTDIR"), Some(""));
}

/// A `settings_path` splices a single-quoted `--settings <path>` BEFORE
/// `--resume` in the prefill line (theme parity), matching the Swift byte-for-byte.
#[test]
fn claude_extra_env_settings_path_splices_into_prefill() {
    let env = build_claude_extra_env(
        &ClaudeSessionMode::ResumeDeferred("SID".into()),
        "t",
        "p",
        Some("/s"),
        Some("/z"),
        Some("/user/z"),
        Some("/Users/nick/Library/Application Support/settings.json".to_string()),
    );
    assert_eq!(
        value_of(&env, "NICE_PREFILL_COMMAND"),
        Some("claude --settings '/Users/nick/Library/Application Support/settings.json' --resume SID"),
        "--settings must precede --resume and be single-quoted"
    );
}

// =====================================================================
// build_claude_exec_command — the exec-command flag matrix
// (TabPtySessionClaudeArgsTests, `TabPtySession.swift:938-970`). Regressions
// silently break resume (wrong flag order eats the UUID), fresh sessions
// (missing --session-id ⇒ CLI picks its own id and Nice can't resume), and the
// override branch (NICE_CLAUDE_OVERRIDE must suppress every injected flag).
// =====================================================================

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// `.none` with no extra args → bare `exec '<claude>'`.
#[test]
fn exec_command_none_no_session_flag_no_extra_args() {
    let cmd = build_claude_exec_command(
        "/usr/local/bin/claude",
        &ClaudeSessionMode::None,
        &[],
        false,
        None,
    );
    assert_eq!(cmd, "exec '/usr/local/bin/claude'");
}

/// `.none` appends extra args, each single-quoted.
#[test]
fn exec_command_none_with_extra_args_appended_quoted() {
    let cmd = build_claude_exec_command(
        "/usr/local/bin/claude",
        &ClaudeSessionMode::None,
        &args(&["--foo", "bar baz"]),
        false,
        None,
    );
    assert_eq!(cmd, "exec '/usr/local/bin/claude' '--foo' 'bar baz'");
}

/// `.new` emits `--session-id <uuid>` BEFORE the user's extra args (load-bearing
/// order — else the UUID is parsed as the trailing flag's value).
#[test]
fn exec_command_new_emits_session_id_before_extra_args() {
    let cmd = build_claude_exec_command(
        "/usr/local/bin/claude",
        &ClaudeSessionMode::New("abc-123".into()),
        &args(&["--model", "opus"]),
        false,
        None,
    );
    assert_eq!(
        cmd,
        "exec '/usr/local/bin/claude' --session-id 'abc-123' '--model' 'opus'"
    );
}

/// `.resume` emits `--resume <uuid>` and DROPS extra args (the transcript
/// already carries the session's flags).
#[test]
fn exec_command_resume_emits_resume_flag_drops_extra_args() {
    let cmd = build_claude_exec_command(
        "/usr/local/bin/claude",
        &ClaudeSessionMode::Resume("abc-123".into()),
        &args(&["--model", "opus"]),
        false,
        None,
    );
    assert_eq!(cmd, "exec '/usr/local/bin/claude' --resume 'abc-123'");
}

/// `.resumeDeferred` doesn't `exec claude` at all — the helper returns just the
/// exec prefix defensively (the caller uses the plain-shell branch instead).
#[test]
fn exec_command_resume_deferred_emits_only_exec_prefix() {
    let cmd = build_claude_exec_command(
        "/usr/local/bin/claude",
        &ClaudeSessionMode::ResumeDeferred("abc-123".into()),
        &[],
        false,
        None,
    );
    assert_eq!(cmd, "exec '/usr/local/bin/claude'");
}

/// NICE_CLAUDE_OVERRIDE (`is_override`) suppresses `--session-id`.
#[test]
fn exec_command_override_suppresses_session_flag() {
    let cmd = build_claude_exec_command(
        "/usr/local/bin/claude",
        &ClaudeSessionMode::New("abc-123".into()),
        &args(&["--model", "opus"]),
        true,
        None,
    );
    assert_eq!(cmd, "exec '/usr/local/bin/claude'");
}

/// Override suppresses `--resume` too.
#[test]
fn exec_command_override_suppresses_resume_flag() {
    let cmd = build_claude_exec_command(
        "/usr/local/bin/claude",
        &ClaudeSessionMode::Resume("abc-123".into()),
        &[],
        true,
        None,
    );
    assert_eq!(cmd, "exec '/usr/local/bin/claude'");
}

/// A path with spaces is single-quoted as one token.
#[test]
fn exec_command_path_with_spaces_quoted() {
    let cmd = build_claude_exec_command(
        "/Users/dev user/bin/claude",
        &ClaudeSessionMode::None,
        &[],
        false,
        None,
    );
    assert_eq!(cmd, "exec '/Users/dev user/bin/claude'");
}

/// A path with an embedded single quote uses the `'\''` escape.
#[test]
fn exec_command_path_with_single_quote_uses_escape_sequence() {
    let cmd = build_claude_exec_command(
        "/Users/dev's/claude",
        &ClaudeSessionMode::None,
        &[],
        false,
        None,
    );
    assert_eq!(cmd, r#"exec '/Users/dev'\''s/claude'"#);
}

/// Shell metacharacters in extra args pass through literally inside single
/// quotes — the shell must receive `$HOME` / backtick verbatim.
#[test]
fn exec_command_extra_arg_shell_metacharacters_pass_through_literally() {
    let cmd = build_claude_exec_command(
        "/claude",
        &ClaudeSessionMode::None,
        &args(&["$HOME", "`whoami`"]),
        false,
        None,
    );
    assert_eq!(cmd, "exec '/claude' '$HOME' '`whoami`'");
}

/// A stale/deleted-transcript UUID is emitted anyway — no arg-build-time
/// validation (the user sees claude's own "session not found" in the pty).
#[test]
fn exec_command_resume_stale_uuid_emits_resume_flag_anyway() {
    let cmd = build_claude_exec_command(
        "/usr/local/bin/claude",
        &ClaudeSessionMode::Resume("00000000-deleted-transcript-0000".into()),
        &[],
        false,
        None,
    );
    assert_eq!(
        cmd,
        "exec '/usr/local/bin/claude' --resume '00000000-deleted-transcript-0000'"
    );
}

/// `--settings <path>` is emitted BEFORE `--session-id` (a global flag with its
/// own value must never sit between `--session-id`/`--resume` and their UUID).
#[test]
fn exec_command_settings_path_emitted_before_session_id() {
    let cmd = build_claude_exec_command(
        "/c",
        &ClaudeSessionMode::New("abc-123".into()),
        &args(&["--model", "opus"]),
        false,
        Some("/Users/x/.nice/claude-theme-settings.json"),
    );
    assert_eq!(
        cmd,
        "exec '/c' --settings '/Users/x/.nice/claude-theme-settings.json' --session-id 'abc-123' '--model' 'opus'"
    );
}

/// `--settings <path>` is emitted before `--resume`.
#[test]
fn exec_command_settings_path_emitted_before_resume() {
    let cmd = build_claude_exec_command(
        "/c",
        &ClaudeSessionMode::Resume("abc-123".into()),
        &[],
        false,
        Some("/s.json"),
    );
    assert_eq!(cmd, "exec '/c' --settings '/s.json' --resume 'abc-123'");
}

/// `settings_path == None` omits the flag entirely.
#[test]
fn exec_command_settings_path_none_omits_flag() {
    let cmd = build_claude_exec_command(
        "/c",
        &ClaudeSessionMode::New("abc-123".into()),
        &[],
        false,
        None,
    );
    assert_eq!(cmd, "exec '/c' --session-id 'abc-123'");
}

/// Override suppresses `--settings` like every other injected flag.
#[test]
fn exec_command_settings_path_suppressed_by_override() {
    let cmd = build_claude_exec_command(
        "/c",
        &ClaudeSessionMode::New("abc-123".into()),
        &[],
        true,
        Some("/s.json"),
    );
    assert_eq!(cmd, "exec '/c'");
}

/// A settings path with a space is single-quoted.
#[test]
fn exec_command_settings_path_quoted_when_contains_space() {
    let cmd = build_claude_exec_command(
        "/c",
        &ClaudeSessionMode::None,
        &[],
        false,
        Some("/Users/dev user/.nice/s.json"),
    );
    assert_eq!(cmd, "exec '/c' --settings '/Users/dev user/.nice/s.json'");
}

// =====================================================================
// build_claude_prefill_command — the FROZEN deferred-resume prefill string
// (`claude[ --settings '<path>'] --resume <sid>`, `TabPtySession.swift:898-899`).
// =====================================================================

#[test]
fn prefill_command_omits_settings_when_none() {
    assert_eq!(
        build_claude_prefill_command(None, "abc-123"),
        "claude --resume abc-123"
    );
}

#[test]
fn prefill_command_splices_single_quoted_settings_before_resume() {
    assert_eq!(
        build_claude_prefill_command(Some("/s.json"), "abc-123"),
        "claude --settings '/s.json' --resume abc-123"
    );
}

#[test]
fn prefill_command_settings_path_with_space_single_quoted() {
    assert_eq!(
        build_claude_prefill_command(Some("/Users/dev user/s.json"), "SID"),
        "claude --settings '/Users/dev user/s.json' --resume SID"
    );
}

// =====================================================================
// compose_claude_reply — the FROZEN socket reply grammar (≤3 positional fields;
// reply tail of `handleClaudeSocketRequest`, `SessionsModel.swift:897-910`).
// =====================================================================

/// The newtab path replies `newtab` regardless of any settings path.
#[test]
fn reply_newtab() {
    assert_eq!(compose_claude_reply(&ClaudeReplyDecision::NewTab, None), "newtab");
    assert_eq!(
        compose_claude_reply(&ClaudeReplyDecision::NewTab, Some("/s.json")),
        "newtab",
        "a settings path never changes the newtab reply"
    );
}

/// In-place, args already carried the id, sync off → bare `inplace`
/// (`test_inplaceWithSessionId_flipsIsClaudeRunningTrue_andRepliesInplace`).
#[test]
fn reply_inplace_parsed_id_sync_off_is_bare_inplace() {
    let decision = ClaudeReplyDecision::InPlace {
        parsed_from_args: true,
        session_id: "OLD".into(),
    };
    assert_eq!(compose_claude_reply(&decision, None), "inplace");
}

/// In-place, minted id, sync off → `inplace <uuid>`
/// (`test_inplaceWithoutSessionId_mintsFreshIdAndRepliesWithIt`).
#[test]
fn reply_inplace_minted_id_sync_off_appends_uuid() {
    let decision = ClaudeReplyDecision::InPlace {
        parsed_from_args: false,
        session_id: "minted-uuid".into(),
    };
    assert_eq!(compose_claude_reply(&decision, None), "inplace minted-uuid");
}

/// Sync on + user-supplied session id → `inplace - <path>` (the `-` sid
/// placeholder, then the settings pointer as the 3rd field;
/// `test_inplaceWithSessionId_syncOn_appendsSettingsPointer`).
#[test]
fn reply_inplace_parsed_id_sync_on_uses_dash_placeholder() {
    let decision = ClaudeReplyDecision::InPlace {
        parsed_from_args: true,
        session_id: "unused".into(),
    };
    assert_eq!(
        compose_claude_reply(&decision, Some("/ptr.json")),
        "inplace - /ptr.json"
    );
}

/// Sync on + minted id → `inplace <uuid> <path>` (real minted id, not `-`;
/// `test_inplaceWithoutSessionId_syncOn_appendsSettingsPointerAfterMintedId`).
#[test]
fn reply_inplace_minted_id_sync_on_appends_uuid_then_path() {
    let decision = ClaudeReplyDecision::InPlace {
        parsed_from_args: false,
        session_id: "minted-uuid".into(),
    };
    assert_eq!(
        compose_claude_reply(&decision, Some("/ptr.json")),
        "inplace minted-uuid /ptr.json"
    );
}

/// Sync off replies are byte-identical to the pre-theming protocol — no third
/// field ever appears (`test_inplace_syncOff_repliesByteIdentical`).
#[test]
fn reply_inplace_sync_off_never_has_third_field() {
    for parsed in [true, false] {
        let decision = ClaudeReplyDecision::InPlace {
            parsed_from_args: parsed,
            session_id: "x".into(),
        };
        let reply = compose_claude_reply(&decision, None);
        assert!(
            reply.split(' ').count() <= 2,
            "sync-off reply {reply:?} must be ≤2 fields"
        );
    }
}

// =====================================================================
// parse_claude_title — the T5 status/label split (`SessionsModel.swift:439-453`).
// The pure split; the trim / empty-skip / "Claude Code" placeholder / auto-title
// pipeline is R15 slice-3's pane_title_changed branch.
// =====================================================================

/// A braille-spinner first scalar (U+2800..=U+28FF) ⇒ Thinking; the label is the
/// remainder after the prefix scalar.
#[test]
fn parse_title_braille_spinner_sets_thinking() {
    let (status, label) = parse_claude_title("\u{2840} fix-top-bar-height");
    assert_eq!(status, Some(TabStatus::Thinking));
    assert_eq!(label, " fix-top-bar-height");
}

/// The braille range is inclusive at both ends.
#[test]
fn parse_title_braille_range_boundaries_set_thinking() {
    assert_eq!(parse_claude_title("\u{2800}x").0, Some(TabStatus::Thinking));
    assert_eq!(parse_claude_title("\u{28FF}x").0, Some(TabStatus::Thinking));
}

/// The sparkle ✳ (U+2733) ⇒ Waiting.
#[test]
fn parse_title_sparkle_sets_waiting() {
    let (status, label) = parse_claude_title("\u{2733} needs-input");
    assert_eq!(status, Some(TabStatus::Waiting));
    assert_eq!(label, " needs-input");
}

/// A non-braille, non-sparkle first char ⇒ no status change; the whole string is
/// the label.
#[test]
fn parse_title_unknown_prefix_treated_as_label() {
    let (status, label) = parse_claude_title("refactor-auth-layer");
    assert_eq!(status, None);
    assert_eq!(label, "refactor-auth-layer");
}

/// The generic "Claude Code" placeholder is not special-cased HERE — it parses
/// as a plain label (slice-3's branch drops it after trimming).
#[test]
fn parse_title_placeholder_is_a_plain_label() {
    assert_eq!(parse_claude_title("Claude Code"), (None, "Claude Code"));
    // With a braille prefix, the status still flips and the label carries the
    // placeholder (which the caller trims and drops).
    assert_eq!(
        parse_claude_title("\u{2840} Claude Code"),
        (Some(TabStatus::Thinking), " Claude Code")
    );
}

/// An empty title yields no status and an empty label (the caller skips it).
#[test]
fn parse_title_empty_is_none_empty_label() {
    assert_eq!(parse_claude_title(""), (None, ""));
}

/// A bare status glyph with no trailing label still flips the status; the label
/// is empty.
#[test]
fn parse_title_bare_status_glyph_empty_label() {
    assert_eq!(
        parse_claude_title("\u{2733}"),
        (Some(TabStatus::Waiting), "")
    );
}

// =====================================================================
// mint_session_uuid — real lowercased UUIDv4 (getentropy-backed), a separate
// mint from the ms+counter tab/pane id minter.
// =====================================================================

/// Canonical `8-4-4-4-12` lowercase hex shape, 36 chars, hyphens at the fixed
/// offsets.
#[test]
fn session_uuid_canonical_format() {
    let id = mint_session_uuid();
    assert_eq!(id.len(), 36, "{id}");
    let bytes = id.as_bytes();
    assert_eq!(bytes[8], b'-');
    assert_eq!(bytes[13], b'-');
    assert_eq!(bytes[18], b'-');
    assert_eq!(bytes[23], b'-');
    for (i, c) in id.char_indices() {
        if [8, 13, 18, 23].contains(&i) {
            continue;
        }
        assert!(
            c.is_ascii_hexdigit() && !c.is_ascii_uppercase(),
            "char {i} = {c:?} must be lowercase hex in {id}"
        );
    }
}

/// Version nibble is `4` and the variant nibble is one of `8/9/a/b` (RFC 4122
/// version 4, variant 1).
#[test]
fn session_uuid_version_and_variant_bits() {
    for _ in 0..256 {
        let id = mint_session_uuid();
        let bytes = id.as_bytes();
        // Version nibble: first char of the third group (index 14).
        assert_eq!(bytes[14], b'4', "version nibble must be 4 in {id}");
        // Variant nibble: first char of the fourth group (index 19).
        assert!(
            matches!(bytes[19], b'8' | b'9' | b'a' | b'b'),
            "variant nibble must be 8/9/a/b in {id}"
        );
    }
}

/// Reparsed bytes carry the exact version (4) and variant (0b10) bits.
#[test]
fn session_uuid_bits_survive_reparse() {
    let id = mint_session_uuid();
    let hex: String = id.chars().filter(|c| *c != '-').collect();
    let byte6 = u8::from_str_radix(&hex[12..14], 16).unwrap();
    let byte8 = u8::from_str_radix(&hex[16..18], 16).unwrap();
    assert_eq!(byte6 >> 4, 4, "byte 6 high nibble = version 4");
    assert_eq!(byte8 >> 6, 0b10, "byte 8 top two bits = variant 1");
}

/// A batch of mints are all distinct (122 bits of entropy ⇒ no collision at
/// human creation rates).
#[test]
fn session_uuid_uniqueness() {
    let mut seen = HashSet::new();
    for _ in 0..1000 {
        assert!(seen.insert(mint_session_uuid()), "duplicate session uuid minted");
    }
}

// ---- R15 Claude-tab constructor pure helpers --------------------------------
//
// The constructor itself (`create_claude_tab`) spawns a pty, so its end-to-end
// shape is the slice-3 `claude-lifecycle` scenario; these pin the pure pieces it
// composes (Swift `createTabFromMainTerminal`'s title + sessionCwd closures and
// `TabPtySession.launchDisplayCommand`).

// claude_tab_title_from_args — join, 40-char cap, trim, empty ⇒ "New tab".

#[test]
fn claude_title_empty_args_is_new_tab() {
    assert_eq!(claude_tab_title_from_args(&[]), "New tab");
}

#[test]
fn claude_title_joins_args_with_spaces() {
    let args = vec!["--resume".to_string(), "abc-123".to_string()];
    assert_eq!(claude_tab_title_from_args(&args), "--resume abc-123");
}

#[test]
fn claude_title_caps_at_40_chars() {
    // A single 50-char arg: the cut lands mid-content, no trailing space to trim.
    let args = vec!["x".repeat(50)];
    let title = claude_tab_title_from_args(&args);
    assert_eq!(title, "x".repeat(40));
    assert_eq!(title.chars().count(), 40);
}

#[test]
fn claude_title_trims_trailing_space_exposed_by_the_cut() {
    // 39 x's + " tail" joins to 44 chars; the 40-char cut lands on the space after
    // the x-run, which is then trimmed off (→ 39 x's).
    let args = vec!["x".repeat(39), "tail".to_string()];
    let title = claude_tab_title_from_args(&args);
    assert_eq!(title, "x".repeat(39));
}

#[test]
fn claude_title_all_whitespace_falls_back_to_new_tab() {
    let args = vec!["   ".to_string()];
    assert_eq!(claude_tab_title_from_args(&args), "New tab");
}

// claude_worktree_cwd — Tab.cwd worktree split (`-w` space form only, `/`→`+`).

#[test]
fn claude_worktree_cwd_no_worktree_is_plain_cwd() {
    assert_eq!(claude_worktree_cwd("/tmp/p", &[]), "/tmp/p");
    // The `=` form is deliberately NOT a worktree (session-id takes both; worktree
    // is space-form only — the landed extractor enforces it).
    let eq = vec!["--worktree=foo".to_string()];
    assert_eq!(claude_worktree_cwd("/tmp/p", &eq), "/tmp/p");
}

#[test]
fn claude_worktree_cwd_space_form_builds_worktree_path() {
    let args = vec!["-w".to_string(), "feature".to_string()];
    assert_eq!(
        claude_worktree_cwd("/tmp/p", &args),
        "/tmp/p/.claude/worktrees/feature"
    );
}

#[test]
fn claude_worktree_cwd_sanitizes_slash_to_plus() {
    // Claude sanitizes `/` → `+` when deriving the on-disk worktree dir name.
    let args = vec!["--worktree".to_string(), "foo/bar".to_string()];
    assert_eq!(
        claude_worktree_cwd("/tmp/p", &args),
        "/tmp/p/.claude/worktrees/foo+bar"
    );
}

#[test]
fn claude_worktree_cwd_trims_trailing_slash_on_anchor() {
    let args = vec!["-w".to_string(), "wt".to_string()];
    assert_eq!(
        claude_worktree_cwd("/tmp/p/", &args),
        "/tmp/p/.claude/worktrees/wt"
    );
}

// claude_launch_display_command — the user-facing overlay string.

#[test]
fn claude_display_command_plain() {
    assert_eq!(
        claude_launch_display_command(&ClaudeSessionMode::New("uuid".into()), &[]),
        "claude"
    );
}

#[test]
fn claude_display_command_with_user_args_hides_session_plumbing() {
    let args = vec!["--dangerously-skip-permissions".to_string()];
    // The overlay shows the user's args, never `--session-id <uuid>` / the zsh wrap.
    assert_eq!(
        claude_launch_display_command(&ClaudeSessionMode::New("uuid".into()), &args),
        "claude --dangerously-skip-permissions"
    );
}

#[test]
fn claude_display_command_resume_hides_uuid() {
    assert_eq!(
        claude_launch_display_command(&ClaudeSessionMode::Resume("uuid".into()), &[]),
        "claude --resume"
    );
}
