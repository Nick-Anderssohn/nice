//! Ported `SessionsModel` unit tests (R13 slice 1) — the pure model-routing
//! half. Each case drives the [`SessionManager`] surface and asserts on the
//! [`TabModel`] document, exactly as the Swift `SessionsModelNavigationTests` /
//! `SessionsModelPaneCwdTests` / the `AppStatePaneLifecycleTests` title-policy
//! cases assert on `appState.tabs`. The Swift originals also spawn real ptys as a
//! side effect (`AppState` is live); the observable assertions are purely the
//! model mutations, which these reproduce without a gpui context — the spawn /
//! focus side effects are exercised by the slice-3 `session-lifecycle` scenario.

use std::sync::atomic::{AtomicU64, Ordering};

use nice_model::{Pane, PaneKind, Project, Tab, TabModel};
use nice_term_view::TerminalEvent;

use super::{clip_title, default_mint_id, SessionManager, PANE_TITLE_MAX};

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
        "t1",
        "p1",
        &TerminalEvent::CwdChanged(std::path::PathBuf::from("/Users/nick/Downloads")),
    );

    let tab = model.tab_for("t1").unwrap();
    assert_eq!(tab.panes[0].cwd.as_deref(), Some("/Users/nick/Downloads"));
    assert_eq!(tab.cwd, "/tmp/anchor", "Tab.cwd stays anchored");
}

#[test]
fn route_title_reset_and_lifecycle_events_are_noops() {
    // TitleReset carries no new label (terminal title-policy only accepts a
    // non-empty set); OutputStarted / Exited are slice-2 concerns. None may
    // panic or mutate the pill here.
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

    mgr.route_terminal_event(&mut model, "t1", &terminal_id, &TerminalEvent::TitleReset);
    mgr.route_terminal_event(&mut model, "t1", &terminal_id, &TerminalEvent::OutputStarted);
    mgr.route_terminal_event(
        &mut model,
        "t1",
        &terminal_id,
        &TerminalEvent::Exited {
            status: nice_term_core::ExitStatus::Exited(0),
            held: false,
        },
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
    assert_eq!(after, before, "reset + lifecycle events must not touch the pill");
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
