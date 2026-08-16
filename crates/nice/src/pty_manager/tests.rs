//! Ported `SessionsModel` unit tests (R13 slice 1) — the pure model-routing
//! half. Each case drives the [`PtyManager`] surface and asserts on the
//! [`WorkspaceModel`] document, exactly as the Swift `SessionsModelNavigationTests` /
//! `SessionsModelPaneCwdTests` / the `AppStatePaneLifecycleTests` title-policy
//! cases assert on `appState.sessions`. The Swift originals also spawn real ptys as a
//! side effect (`AppState` is live); the observable assertions are purely the
//! model mutations, which these reproduce without a gpui context — the spawn /
//! focus side effects are exercised by the slice-3 `session-lifecycle` scenario.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use nice_model::{TermWindow, TermWindowKind, Project, SidebarSessionSelection, Session, WorkspaceModel, SessionStatus};
use nice_term_core::SpawnSpec;
use nice_term_view::TerminalEvent;

use super::{
    build_claude_exec_command, build_claude_extra_env, build_claude_prefill_command,
    claude_launch_display_command, claude_session_title_from_args, claude_worktree_cwd, clip_title,
    compose_claude_reply, default_mint_id, dispatch_extra_args, dispatch_prompt, dispatch_title,
    handoff_extra_args, handoff_prompt, handoff_title,
    merge_env_spec_wins, mint_session_uuid, parse_claude_title, pending_prefill_for,
    ClaudeReplyDecision,
    ClaudeSessionMode, DissolveTerminus, WindowLaunchStatus, PtyManager, WindowShellEnv,
    WINDOW_TITLE_MAX,
};

/// A fresh empty selection for cascade tests that don't seed a multi-selection.
fn selection() -> SidebarSessionSelection {
    SidebarSessionSelection::new()
}

/// Seed a `[Claude, Terminal 1]` session (Claude focused) into project `project_id`
/// (created or appended-to) — the Rust twin of `TabModelFixtures.seedClaudeTab`.
/// Returns `(claude_window_id, terminal_window_id)`. `is_claude_running` is explicit
/// so the paneHeld case can seed a running Claude and observe the flag clearing.
fn seed_claude_session_in(
    model: &mut WorkspaceModel,
    project_id: &str,
    session_id: &str,
    is_claude_running: bool,
) -> (String, String) {
    let claude_id = format!("{session_id}-claude");
    let terminal_id = format!("{session_id}-t1");
    let path = format!("/tmp/{project_id}");
    let mut claude = TermWindow::new(&claude_id, "Claude", TermWindowKind::Claude);
    claude.is_claude_running = is_claude_running;
    let mut session = Session::new(session_id, "New session", &path);
    session.windows = vec![
        claude,
        TermWindow::new(&terminal_id, "Terminal 1", TermWindowKind::Terminal),
    ];
    session.active_window_id = Some(claude_id.clone());
    session.next_terminal_index = 2;
    if let Some(p) = model.projects.iter_mut().find(|p| p.id == project_id) {
        p.sessions.push(session);
    } else {
        model.projects.push(Project {
            id: project_id.into(),
            name: project_id.to_uppercase(),
            path: path.into(),
            sessions: vec![session],
        });
    }
    (claude_id, terminal_id)
}

/// A manager with a deterministic, collision-free id minter (`<prefix>N`) so
/// ported tests that add windows can reason about ids if they need to.
fn counting_manager() -> PtyManager {
    let counter = AtomicU64::new(0);
    PtyManager::with_mint_id(move |prefix| {
        format!("{prefix}{}", counter.fetch_add(1, Ordering::Relaxed))
    })
}

/// The freshly-seeded window model: pinned Terminals group + Main session (one
/// "Terminal 1" window, `next_terminal_index = 2`, that window active).
fn seeded() -> WorkspaceModel {
    WorkspaceModel::new("/home/u")
}

fn main_session_id() -> &'static str {
    WorkspaceModel::MAIN_TERMINAL_SESSION_ID
}

/// Snapshot of the Main terminal session (re-read on each access so assertions
/// observe the latest mutation).
fn main_session(model: &WorkspaceModel) -> &Session {
    model.session_for(WorkspaceModel::MAIN_TERMINAL_SESSION_ID).unwrap()
}

/// Seed a bare terminal session (`session_id` with a single terminal window `term_window_id`,
/// `Session.cwd == session_cwd`) into a fresh non-Terminals project — the Rust twin of
/// `SessionsModelPaneCwdTests.seedTerminalTab`.
fn seed_terminal_session(model: &mut WorkspaceModel, session_id: &str, term_window_id: &str, session_cwd: &str) {
    let mut session = Session::new(session_id, "Terminal", session_cwd);
    session.windows = vec![TermWindow::new(term_window_id, "zsh", TermWindowKind::Terminal)];
    session.active_window_id = Some(term_window_id.to_string());
    model.projects.push(Project {
        id: "p".into(),
        name: "P".into(),
        path: session_cwd.into(),
        sessions: vec![session],
    });
}

/// Seed a `[Claude, Terminal 1]` session (Claude focused) into a non-Terminals
/// project — the Rust twin of `AppStatePaneLifecycleTests.seedProjectWithClaudeTab`.
/// Returns `(claude_window_id, terminal_window_id)`. `is_claude_running` stays
/// `false` (its default), matching R13's invariant.
fn seed_claude_session(model: &mut WorkspaceModel, session_id: &str) -> (String, String) {
    let claude_id = format!("{session_id}-claude");
    let terminal_id = format!("{session_id}-t1");
    let mut session = Session::new(session_id, "New session", "/home/u/proj");
    session.windows = vec![
        TermWindow::new(&claude_id, "Claude", TermWindowKind::Claude),
        TermWindow::new(&terminal_id, "Terminal 1", TermWindowKind::Terminal),
    ];
    session.active_window_id = Some(claude_id.clone());
    session.next_terminal_index = 2;
    model.projects.push(Project {
        id: "p".into(),
        name: "P".into(),
        path: "/home/u/proj".into(),
        sessions: vec![session],
    });
    (claude_id, terminal_id)
}

// ===========================================================================
// SessionsModelNavigationTests (ported)
// ===========================================================================

/// Add a second terminal window to Main so window-navigation has something to step
/// through — the Rust twin of `addExtraTerminalPaneToMain` (goes through
/// `add_window`, which in the live app spawns; here the model half).
fn add_extra_terminal_window_to_main(mgr: &mut PtyManager, model: &mut WorkspaceModel) -> String {
    mgr.add_window(model, main_session_id(), None).unwrap()
}

#[test]
fn next_window_moves_right_when_not_at_end() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    add_extra_terminal_window_to_main(&mut mgr, &mut model);

    let session = main_session(&model);
    assert_eq!(session.windows.len(), 2);
    let first_id = session.windows[0].id.clone();
    let second_id = session.windows[1].id.clone();

    mgr.set_active_window(&mut model, main_session_id(), &first_id);
    mgr.select_next_window(&mut model);
    assert_eq!(main_session(&model).active_window_id.as_ref(), Some(&second_id));
}

#[test]
fn next_window_wraps_to_first_when_at_last() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    add_extra_terminal_window_to_main(&mut mgr, &mut model);

    let session = main_session(&model);
    let first_id = session.windows[0].id.clone();
    let last_id = session.windows.last().unwrap().id.clone();

    mgr.set_active_window(&mut model, main_session_id(), &last_id);
    mgr.select_next_window(&mut model);
    assert_eq!(main_session(&model).active_window_id.as_ref(), Some(&first_id));
}

#[test]
fn prev_window_wraps_to_last_when_at_first() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    add_extra_terminal_window_to_main(&mut mgr, &mut model);

    let session = main_session(&model);
    let first_id = session.windows[0].id.clone();
    let last_id = session.windows.last().unwrap().id.clone();

    mgr.set_active_window(&mut model, main_session_id(), &first_id);
    mgr.select_prev_window(&mut model);
    assert_eq!(main_session(&model).active_window_id.as_ref(), Some(&last_id));
}

#[test]
fn next_window_is_noop_when_single_window() {
    // The seeded Main session starts with a single window; stepping must not move.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let original_active = main_session(&model).active_window_id.clone();

    mgr.select_next_window(&mut model);
    assert_eq!(main_session(&model).active_window_id, original_active);
}

#[test]
fn add_terminal_to_active_session_appends_terminal_and_focuses() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    model.select_session(main_session_id());
    let original_count = main_session(&model).windows.len();

    mgr.add_terminal_to_active_session(&mut model);

    let session = main_session(&model);
    assert_eq!(session.windows.len(), original_count + 1);
    let new_term_window = session.windows.last().unwrap();
    assert_eq!(new_term_window.kind, TermWindowKind::Terminal);
    // Seed consumed slot 1 ("Terminal 1"); the add is auto-named "Terminal 2".
    assert_eq!(new_term_window.title, "Terminal 2");
    assert_eq!(session.active_window_id.as_ref(), Some(&new_term_window.id));
}

/// Rust twin of `test_helpers_areNoOpWhenActiveTabIdIsNil`, adapted to the
/// Rust model's invariant. Swift set `activeTabId = nil` directly; the Rust
/// `WorkspaceModel` has **no `None` writer** for `active_session_id` post-construction
/// (the sole writer, `set_active_session_id`, is private and only ever sets `Some`),
/// so the literal nil case is unreachable. This ports the reachable half of the
/// Swift intent: the window-navigation helpers are safe no-ops with nothing to
/// step through, and the sidebar step is a no-op with a single navigable session
/// (the "single navigable id ⇒ no-op" tail the Swift case also asserts).
#[test]
fn helpers_are_safe_noops_when_nothing_to_navigate() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    // Fresh window: one navigable session (Main), one window.
    let before_active_session = model.active_session_id().map(str::to_owned);
    let before_active_window = main_session(&model).active_window_id.clone();

    // Single-window session: window stepping is a no-op (must not crash or move).
    mgr.select_next_window(&mut model);
    mgr.select_prev_window(&mut model);
    assert_eq!(main_session(&model).active_window_id, before_active_window);

    // Single navigable sidebar session: stepping the sidebar is a no-op too.
    model.select_next_sidebar_session();
    assert_eq!(model.active_session_id().map(str::to_owned), before_active_session);
}

// ===========================================================================
// SessionsModelPaneCwdTests (ported)
// ===========================================================================

#[test]
fn window_cwd_changed_stores_on_window() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_terminal_session(&mut model, "t1", "p1", "/tmp");

    let changed = mgr.window_cwd_changed(&mut model, "t1", "p1", "/Users/nick/Downloads");

    assert!(changed, "a real cwd change reports changed");
    assert_eq!(
        model.session_for("t1").unwrap().windows[0].cwd.as_deref(),
        Some("/Users/nick/Downloads"),
        "OSC 7 update must land on TermWindow.cwd"
    );
}

#[test]
fn window_cwd_changed_does_not_mutate_session_cwd() {
    // Session.cwd is load-bearing for `claude --resume` — a companion terminal's cd
    // must never relocate the session's anchor.
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_terminal_session(&mut model, "t1", "p1", "/tmp/anchor");

    mgr.window_cwd_changed(&mut model, "t1", "p1", "/Users/nick/Downloads");

    assert_eq!(
        model.session_for("t1").unwrap().cwd,
        "/tmp/anchor",
        "Session.cwd must stay anchored even when a window cd's elsewhere"
    );
}

#[test]
fn window_cwd_changed_unknown_window_is_noop() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_terminal_session(&mut model, "t1", "p1", "/tmp");

    let changed = mgr.window_cwd_changed(&mut model, "t1", "ghost", "/Users/nick");

    assert!(!changed);
    assert_eq!(
        model.session_for("t1").unwrap().windows[0].cwd, None,
        "stale paneId must not invent a cwd on the wrong window"
    );
}

#[test]
fn window_cwd_changed_unknown_session_is_noop() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_terminal_session(&mut model, "t1", "p1", "/tmp");

    let changed = mgr.window_cwd_changed(&mut model, "ghost-tab", "p1", "/Users/nick");

    assert!(!changed);
    assert_eq!(model.session_for("t1").unwrap().windows[0].cwd, None);
}

// ===========================================================================
// Terminal-branch title policy (ported from AppStatePaneLifecycleTests)
// ===========================================================================

#[test]
fn window_title_changed_terminal_window_updates_window_title() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (_claude, terminal_id) = seed_claude_session(&mut model, "t1");

    mgr.window_title_changed(&mut model, "t1", &terminal_id, "nvim foo.rb");

    let term_window = model
        .session_for("t1")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == terminal_id)
        .unwrap();
    assert_eq!(term_window.title, "nvim foo.rb");
}

#[test]
fn window_title_changed_terminal_window_empty_title_ignored() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (_claude, terminal_id) = seed_claude_session(&mut model, "t1");
    let before = model
        .session_for("t1")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == terminal_id)
        .unwrap()
        .title
        .clone();

    mgr.window_title_changed(&mut model, "t1", &terminal_id, "   \n");

    let after = model
        .session_for("t1")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == terminal_id)
        .unwrap()
        .title
        .clone();
    assert_eq!(
        after, before,
        "Whitespace-only titles must not overwrite the current title."
    );
}

#[test]
fn window_title_changed_terminal_window_manually_set_ignores_osc_title() {
    // Once the user renames a terminal window, OSC titles from the running program
    // must not overwrite their custom label.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (_claude, terminal_id) = seed_claude_session(&mut model, "t1");

    model.rename_window("t1", &terminal_id, "build watcher");
    assert!(
        model
            .session_for("t1")
            .unwrap()
            .windows
            .iter()
            .find(|w| w.id == terminal_id)
            .unwrap()
            .title_manually_set,
        "Pre-condition: rename must flip the lock."
    );

    mgr.window_title_changed(&mut model, "t1", &terminal_id, "nvim foo.rb");

    let term_window = model
        .session_for("t1")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == terminal_id)
        .unwrap();
    assert_eq!(
        term_window.title, "build watcher",
        "OSC titles must not overwrite a manually-renamed terminal window."
    );
}

#[test]
fn window_title_changed_terminal_empty_submit_releases_lock_then_accepts_osc() {
    // Empty-submit in the pill editor releases the lock; the next OSC flows in.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (_claude, terminal_id) = seed_claude_session(&mut model, "t1");

    model.rename_window("t1", &terminal_id, "logs");
    model.rename_window("t1", &terminal_id, "");
    assert!(
        !model
            .session_for("t1")
            .unwrap()
            .windows
            .iter()
            .find(|w| w.id == terminal_id)
            .unwrap()
            .title_manually_set,
        "Pre-condition: empty submit must clear the lock."
    );

    mgr.window_title_changed(&mut model, "t1", &terminal_id, "vim x.swift");

    let term_window = model
        .session_for("t1")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == terminal_id)
        .unwrap();
    assert_eq!(
        term_window.title, "vim x.swift",
        "After releasing the lock, OSC titles must flow into the pill again."
    );
}

#[test]
fn window_title_changed_terminal_window_clips_at_40_chars() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (_claude, terminal_id) = seed_claude_session(&mut model, "t1");
    let long: String = "x".repeat(80);

    mgr.window_title_changed(&mut model, "t1", &terminal_id, &long);

    let term_window = model
        .session_for("t1")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == terminal_id)
        .unwrap();
    assert_eq!(
        term_window.title.chars().count(),
        40,
        "Terminal titles must cap at 40 chars so the toolbar pill doesn't overflow."
    );
}

// ===========================================================================
// Claude-branch is_claude_running gate (ported deferred-resume cases)
// ===========================================================================

#[test]
fn window_title_changed_claude_deferred_resume_ignores_shell_title() {
    // A deferred-resume Claude window is a plain zsh (is_claude_running == false);
    // its theme OSC titles ("user@host:cwd") must not clobber the persisted
    // session label. The whole Claude branch drops on the gate this cycle.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal) = seed_claude_session(&mut model, "t1");
    model.apply_auto_title("t1", "fix-top-bar-height");
    assert_eq!(
        model.session_for("t1").unwrap().title,
        "Fix top bar height",
        "Precondition: session has a real auto-titled label."
    );

    mgr.window_title_changed(
        &mut model,
        "t1",
        &claude_id,
        "Nick@Nicks MacBook Air:~/Projects/nice",
    );

    assert_eq!(
        model.session_for("t1").unwrap().title,
        "Fix top bar height",
        "OSC titles from a deferred-resume Claude window (zsh, not claude) \
         must not overwrite the persisted session title."
    );
    // The Claude pill label is likewise untouched.
    let term_window = model
        .session_for("t1")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == claude_id)
        .unwrap();
    assert_eq!(term_window.title, "Claude");
}

#[test]
fn window_title_changed_claude_deferred_resume_ignores_status_prefix() {
    // Defensive: a braille/sparkle status prefix from a non-claude process must
    // not flip the window status while is_claude_running is false — the
    // spinner/sparkle vocabulary belongs to claude (R15).
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal) = seed_claude_session(&mut model, "t1");
    let title_before = model.session_for("t1").unwrap().title.clone();

    // U+2840 is inside the braille spinner range Claude uses for "thinking".
    mgr.window_title_changed(&mut model, "t1", &claude_id, "\u{2840} fix-bug");

    let term_window = model
        .session_for("t1")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == claude_id)
        .unwrap();
    assert_eq!(
        term_window.status,
        nice_model::SessionStatus::Idle,
        "Status transitions are gated on is_claude_running."
    );
    assert_eq!(
        model.session_for("t1").unwrap().title,
        title_before,
        "Session title must not change while is_claude_running is false."
    );
}

// ===========================================================================
// Claude-branch T5 status parsing (ported AppStatePaneLifecycleTests, running)
// ===========================================================================

/// Seed a `[Claude, Terminal 1]` session whose Claude window is **running** (the
/// post-promotion / creation state that opens the T5 OSC gate). Selects the session
/// so `apply_status_transition`'s viewed-window ack fires like the shipped window.
fn seed_running_claude_session(model: &mut WorkspaceModel, session_id: &str) -> (String, String) {
    let (claude_id, terminal_id) = seed_claude_session(model, session_id);
    model.mutate_session(session_id, |session| {
        if let Some(w) = session.windows.iter_mut().find(|w| w.id == claude_id) {
            w.is_claude_running = true;
        }
    });
    model.select_session(session_id);
    (claude_id, terminal_id)
}

#[test]
fn window_title_changed_claude_braille_spinner_sets_thinking_and_humanizes_title() {
    // U+2840 is inside the braille spinner range (0x2800..=0x28FF) Claude uses
    // for "thinking"; the trailing label humanizes onto the session title.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _t) = seed_running_claude_session(&mut model, "t1");

    mgr.window_title_changed(&mut model, "t1", &claude_id, "\u{2840} fix-top-bar-height");

    let term_window = model
        .session_for("t1")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == claude_id)
        .unwrap();
    assert_eq!(term_window.status, nice_model::SessionStatus::Thinking);
    assert_eq!(model.session_for("t1").unwrap().title, "Fix top bar height");
}

#[test]
fn window_title_changed_claude_sparkle_sets_waiting() {
    // U+2733 (✳) is the sparkle Claude uses for "waiting for input."
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _t) = seed_running_claude_session(&mut model, "t1");

    mgr.window_title_changed(&mut model, "t1", &claude_id, "\u{2733} needs-input");

    let term_window = model
        .session_for("t1")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == claude_id)
        .unwrap();
    assert_eq!(term_window.status, nice_model::SessionStatus::Waiting);
}

#[test]
fn window_title_changed_claude_placeholder_label_ignored() {
    // "Claude Code" is the generic placeholder Claude emits before a session has
    // a real name — it must not clobber an existing session title.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _t) = seed_running_claude_session(&mut model, "t1");

    mgr.window_title_changed(&mut model, "t1", &claude_id, "\u{2840} fix-bug");
    assert_eq!(model.session_for("t1").unwrap().title, "Fix bug");

    mgr.window_title_changed(&mut model, "t1", &claude_id, "\u{2840} Claude Code");
    assert_eq!(
        model.session_for("t1").unwrap().title,
        "Fix bug",
        "Placeholder 'Claude Code' must not overwrite a real session title."
    );
}

#[test]
fn window_title_changed_claude_unknown_prefix_treated_as_label() {
    // A non-braille, non-sparkle first char means no status update — the whole
    // string is the label.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _t) = seed_running_claude_session(&mut model, "t1");

    mgr.window_title_changed(&mut model, "t1", &claude_id, "refactor-auth-layer");

    let term_window = model
        .session_for("t1")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == claude_id)
        .unwrap();
    assert_eq!(term_window.status, nice_model::SessionStatus::Idle, "no prefix ⇒ no status change");
    assert_eq!(model.session_for("t1").unwrap().title, "Refactor auth layer");
}

#[test]
fn window_title_changed_claude_manually_set_window_still_flips_status() {
    // The window-level title lock is a *title* lock, not a *status* lock: a renamed
    // Claude window must still flip status when claude emits a braille prefix, and
    // its custom pill name must survive (the OSC gate lives in the terminal
    // branch, never blocking status extraction).
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _t) = seed_running_claude_session(&mut model, "t1");
    model.mutate_session("t1", |session| {
        if let Some(w) = session.windows.iter_mut().find(|w| w.id == claude_id) {
            w.title = "deploy session".to_string();
            w.title_manually_set = true;
        }
    });

    mgr.window_title_changed(&mut model, "t1", &claude_id, "\u{2840} fix-top-bar-height");

    let term_window = model
        .session_for("t1")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == claude_id)
        .unwrap();
    assert_eq!(term_window.status, nice_model::SessionStatus::Thinking, "status still flips");
    assert_eq!(term_window.title, "deploy session", "the user's custom pill name survives");
}

#[test]
fn window_title_changed_claude_accepts_title_after_promotion() {
    // The full deferred-resume → live-claude story: the gate holds against zsh
    // OSC while is_claude_running is false, and RELEASES after the promotion
    // flips it true (`AppStatePaneLifecycleTests.acceptsTitleAfterPromotion`).
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _t) = seed_claude_session(&mut model, "t1"); // is_claude_running == false
    model.select_session("t1");
    let title_before = model.session_for("t1").unwrap().title.clone();

    // Pre-promotion: zsh OSC ignored.
    mgr.window_title_changed(&mut model, "t1", &claude_id, "Nick@host:~/repo");
    assert_eq!(
        model.session_for("t1").unwrap().title,
        title_before,
        "gate must hold before is_claude_running flips true"
    );

    // Simulate the socket-handshake promotion that flips the flag.
    model.mutate_session("t1", |session| {
        if let Some(w) = session.windows.iter_mut().find(|w| w.id == claude_id) {
            w.is_claude_running = true;
        }
    });

    // Post-promotion: real claude OSC accepted; status flips, label humanizes.
    mgr.window_title_changed(&mut model, "t1", &claude_id, "\u{2840} fix-bug");
    let term_window = model
        .session_for("t1")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == claude_id)
        .unwrap();
    assert_eq!(term_window.status, nice_model::SessionStatus::Thinking, "status fires once the gate releases");
    assert_eq!(model.session_for("t1").unwrap().title, "Fix bug", "auto-title applies once the gate releases");
}

// ===========================================================================
// set_active_window model-half: ack-when-viewed (SessionsModel.swift:534-545)
// ===========================================================================

#[test]
fn set_active_window_acknowledges_waiting_window_when_session_is_viewed() {
    // A waiting window that becomes active while its session is the viewed session lands
    // acknowledged (no lingering pulse) — the `markAcknowledgedIfWaiting` side
    // effect of setActivePane.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, terminal_id) = seed_claude_session(&mut model, "t1");
    model.select_session("t1"); // t1 is the viewed session

    // Claude window enters waiting while the companion terminal is active.
    model.mutate_session("t1", |session| {
        session.active_window_id = Some(terminal_id.clone());
        let term_window = session.windows.iter_mut().find(|w| w.id == claude_id).unwrap();
        term_window.apply_status_transition(nice_model::SessionStatus::Waiting, false);
    });
    assert!(
        !model
            .session_for("t1")
            .unwrap()
            .windows
            .iter()
            .find(|w| w.id == claude_id)
            .unwrap()
            .waiting_acknowledged
    );

    // Focusing the waiting Claude window while viewing t1 acknowledges it.
    mgr.set_active_window(&mut model, "t1", &claude_id);

    let term_window = model
        .session_for("t1")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == claude_id)
        .unwrap();
    assert_eq!(term_window.status, nice_model::SessionStatus::Waiting);
    assert!(
        term_window.waiting_acknowledged,
        "activating a waiting window on the viewed session must acknowledge it"
    );
}

#[test]
fn set_active_window_does_not_acknowledge_when_session_not_viewed() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal) = seed_claude_session(&mut model, "t1");
    // Main is the viewed session, not t1.
    model.select_session(WorkspaceModel::MAIN_TERMINAL_SESSION_ID);
    model.mutate_session("t1", |session| {
        let term_window = session.windows.iter_mut().find(|w| w.id == claude_id).unwrap();
        term_window.apply_status_transition(nice_model::SessionStatus::Waiting, false);
    });

    mgr.set_active_window(&mut model, "t1", &claude_id);

    let term_window = model
        .session_for("t1")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == claude_id)
        .unwrap();
    assert!(
        !term_window.waiting_acknowledged,
        "activating a window on an unviewed session must not acknowledge its pulse"
    );
}

#[test]
fn set_active_window_unknown_window_never_dangles_active() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let before = main_session(&model).active_window_id.clone();

    mgr.set_active_window(&mut model, main_session_id(), "ghost-pane");

    assert_eq!(
        main_session(&model).active_window_id,
        before,
        "selecting a window not on the session must leave active_window_id untouched"
    );
}

// ===========================================================================
// route_terminal_event: mapped OSC events reach the model (title/cwd routing)
// ===========================================================================

#[test]
fn route_title_changed_updates_terminal_window_pill() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (_claude, terminal_id) = seed_claude_session(&mut model, "t1");

    mgr.route_terminal_event(
        &mut model,
        &mut selection(),
        "t1",
        &terminal_id,
        // A never-split window's sole pane carries the window's own id.
        &terminal_id,
        &TerminalEvent::TitleChanged("nvim foo.rb".to_string()),
    );

    let term_window = model
        .session_for("t1")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == terminal_id)
        .unwrap();
    assert_eq!(term_window.title, "nvim foo.rb");
}

#[test]
fn route_cwd_changed_writes_window_cwd_only() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_terminal_session(&mut model, "t1", "p1", "/tmp/anchor");

    mgr.route_terminal_event(
        &mut model,
        &mut selection(),
        "t1",
        "p1",
        "p1",
        &TerminalEvent::CwdChanged(std::path::PathBuf::from("/Users/nick/Downloads")),
    );

    let session = model.session_for("t1").unwrap();
    assert_eq!(session.windows[0].cwd.as_deref(), Some("/Users/nick/Downloads"));
    assert_eq!(session.cwd, "/tmp/anchor", "Session.cwd stays anchored");
}

#[test]
fn route_title_reset_and_output_started_leave_the_pill() {
    // TitleReset carries no new label (terminal title-policy only accepts a
    // non-empty set); OutputStarted only clears the launch overlay. Neither may
    // panic or mutate the pill. (Exited routes to window_exited — covered by the
    // paneExited / route-exit cases below.)
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (_claude, terminal_id) = seed_claude_session(&mut model, "t1");
    let before = model
        .session_for("t1")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == terminal_id)
        .unwrap()
        .title
        .clone();

    mgr.route_terminal_event(
        &mut model,
        &mut selection(),
        "t1",
        &terminal_id,
        &terminal_id,
        &TerminalEvent::TitleReset,
    );
    mgr.route_terminal_event(
        &mut model,
        &mut selection(),
        "t1",
        &terminal_id,
        &terminal_id,
        &TerminalEvent::OutputStarted,
    );

    let after = model
        .session_for("t1")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == terminal_id)
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
    assert_eq!(clip_title(&long, WINDOW_TITLE_MAX).chars().count(), 40);
    // A short title passes through untouched.
    assert_eq!(clip_title("nvim foo.rb", WINDOW_TITLE_MAX), "nvim foo.rb");
    // Multi-byte chars are counted by char, not byte (10 CJK chars < 40).
    let cjk = "工作".repeat(5); // 10 chars, 30 bytes
    assert_eq!(clip_title(&cjk, WINDOW_TITLE_MAX), cjk);
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
fn window_exited_removes_window_and_shifts_active_to_neighbor() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, terminal_id) = seed_claude_session_in(&mut model, "p", "t1", false);
    model.select_session("t1");

    // Focus the claude window, then exit it — focus must shift to the neighbor
    // (the terminal window at index 1).
    mgr.set_active_window(&mut model, "t1", &claude_id);
    let res = mgr.window_exited(&mut model, &mut selection(), "t1", &claude_id);

    let session = model.session_for("t1").unwrap();
    assert_eq!(session.windows.len(), 1);
    assert_eq!(session.windows[0].id, terminal_id);
    assert_eq!(
        session.active_window_id.as_deref(),
        Some(terminal_id.as_str()),
        "focus must shift to the surviving window; a dangling activePaneId breaks the toolbar"
    );
    assert_eq!(
        res.refocus_session.as_deref(),
        Some("t1"),
        "the session survived → the live caller spawns the refocused companion (step 4)"
    );
    assert_eq!(res.terminus, DissolveTerminus::None);
}

#[test]
fn window_exited_last_window_dissolves_session() {
    // Seed two extra projects so dissolving one session doesn't empty everything
    // (which would fire the window terminus).
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (c1, term1) = seed_claude_session_in(&mut model, "p1", "t1", false);
    seed_claude_session_in(&mut model, "p2", "t2", false);

    mgr.window_exited(&mut model, &mut selection(), "t1", &c1);
    mgr.window_exited(&mut model, &mut selection(), "t1", &term1);

    assert!(
        model.session_for("t1").is_none(),
        "session must dissolve once every window exits"
    );
    assert!(
        model.session_for("t2").is_some(),
        "other sessions must not be touched by one session's dissolve"
    );
}

#[test]
fn window_exited_dissolved_active_session_falls_back_to_first_available() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (c1, term1) = seed_claude_session_in(&mut model, "p1", "t1", false);
    seed_claude_session_in(&mut model, "p2", "t2", false);
    model.select_session("t1");

    mgr.window_exited(&mut model, &mut selection(), "t1", &c1);
    mgr.window_exited(&mut model, &mut selection(), "t1", &term1);

    // Dissolving the active session leaves active_session_id at the first session in
    // navigable order — the Terminals Main session.
    assert_eq!(model.active_session_id(), Some(WorkspaceModel::MAIN_TERMINAL_SESSION_ID));
}

#[test]
fn window_exited_unknown_window_is_noop() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let before = main_session(&model).windows.len();

    let res = mgr.window_exited(&mut model, &mut selection(), main_session_id(), "does-not-exist");

    assert_eq!(
        main_session(&model).windows.len(),
        before,
        "unknown paneId must not corrupt state"
    );
    assert_eq!(
        res.refocus_session.as_deref(),
        Some(main_session_id()),
        "the session survived untouched"
    );
    assert_eq!(res.terminus, DissolveTerminus::None);
}

#[test]
fn window_exited_last_session_of_last_project_reports_window_emptied() {
    // Dissolving the only session in the window (the seeded Terminals Main session, its
    // single window) leaves every project empty — the terminus the live caller
    // turns into close-window-or-quit. (The Swift lifecycle tests deliberately
    // seed extra projects to AVOID this; here we pin the signal itself.)
    let mut mgr = counting_manager();
    let mut model = seeded();
    let main = main_session_id();
    let term_window_id = main_session(&model).windows[0].id.clone();

    let res = mgr.window_exited(&mut model, &mut selection(), main, &term_window_id);

    assert!(model.session_for(main).is_none(), "the last session dissolved");
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
fn window_held_flips_is_alive_and_idles_status() {
    // Seed a running Claude window mid-think, then hold it: is_alive → false, the
    // pulsing status idles out, the ack clears, and is_claude_running clears.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal) = seed_claude_session_in(&mut model, "p", "t1", true);
    model.mutate_session("t1", |session| {
        let term_window = session.windows.iter_mut().find(|w| w.id == claude_id).unwrap();
        term_window.status = SessionStatus::Thinking;
        term_window.waiting_acknowledged = false;
    });

    mgr.window_held(&mut model, "t1", &claude_id);

    let term_window = model
        .session_for("t1")
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == claude_id)
        .unwrap();
    assert!(!term_window.is_alive, "paneHeld flips is_alive to false");
    assert_eq!(term_window.status, SessionStatus::Idle, "paneHeld idles the status out");
    assert!(
        !term_window.waiting_acknowledged,
        "paneHeld clears waiting_acknowledged so a future waiting window can pulse"
    );
    assert!(
        !term_window.is_claude_running,
        "paneHeld clears is_claude_running (a held pty is a corpse, not a live shell)"
    );
}

#[test]
fn window_held_keeps_window_in_session_windows_array() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal) = seed_claude_session_in(&mut model, "p", "t1", false);
    let before = model.session_for("t1").unwrap().windows.len();

    mgr.window_held(&mut model, "t1", &claude_id);

    let session = model.session_for("t1").unwrap();
    assert_eq!(
        session.windows.len(),
        before,
        "paneHeld must not remove the window — that's paneExited's job"
    );
    assert!(
        session.windows.iter().any(|w| w.id == claude_id),
        "the held window must still be findable by id"
    );
}

#[test]
fn window_held_clears_launch_overlay() {
    // Exit-before-first-byte: the overlay was still up when the process died;
    // paneHeld must clear it so the placeholder doesn't sit on the held footer.
    let mut mgr = counting_manager();
    let mut model = seeded();
    mgr.set_launch_overlay_grace(Duration::ZERO);
    let (claude_id, _terminal) = seed_claude_session_in(&mut model, "p", "t1", false);
    mgr.register_window_launch(&claude_id, "claude");
    assert!(
        mgr.window_launch_state(&claude_id).is_some(),
        "pre-condition: overlay entry exists before paneHeld"
    );

    mgr.window_held(&mut model, "t1", &claude_id);

    assert!(
        mgr.window_launch_state(&claude_id).is_none(),
        "paneHeld must clear the launch overlay"
    );
}

#[test]
fn window_held_unknown_window_is_noop() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let before = main_session(&model).windows.len();

    mgr.window_held(&mut model, main_session_id(), "does-not-exist");

    assert_eq!(main_session(&model).windows.len(), before);
}

// ===========================================================================
// AppStateLaunchOverlayTests (ported)
// ===========================================================================

#[test]
fn register_window_launch_zero_grace_immediately_visible() {
    let mut mgr = counting_manager();
    mgr.set_launch_overlay_grace(Duration::ZERO);

    let armed = mgr.register_window_launch("p1", "claude -w foo");

    assert!(!armed, "zero grace promotes synchronously — no deadline to arm");
    assert_eq!(
        mgr.window_launch_state("p1"),
        Some(&WindowLaunchStatus::Visible {
            command: "claude -w foo".into()
        }),
        "with a zero-second grace the overlay is promoted immediately"
    );
}

#[test]
fn clear_window_launch_removes_visible_entry() {
    let mut mgr = counting_manager();
    mgr.set_launch_overlay_grace(Duration::ZERO);
    mgr.register_window_launch("p1", "claude");
    assert_eq!(
        mgr.window_launch_state("p1"),
        Some(&WindowLaunchStatus::Visible {
            command: "claude".into()
        })
    );

    mgr.clear_window_launch("p1");

    assert!(
        mgr.window_launch_state("p1").is_none(),
        "first-byte clear must remove the entry entirely"
    );
}

#[test]
fn clear_window_launch_before_deadline_fires_suppresses_overlay() {
    // Non-zero grace → registration leaves the entry Pending (the live caller
    // arms the deadline). Clear before the deadline fires, then simulate the
    // deadline firing via promote_window_launch: the Pending-guard early-exits.
    let mut mgr = counting_manager();
    mgr.set_launch_overlay_grace(Duration::from_millis(200));
    let armed = mgr.register_window_launch("p1", "claude");
    assert!(armed, "non-zero grace defers to the injected deadline");
    assert_eq!(
        mgr.window_launch_state("p1"),
        Some(&WindowLaunchStatus::Pending {
            command: "claude".into()
        })
    );

    mgr.clear_window_launch("p1");
    // Deadline fires after the clear — must not resurrect the overlay.
    mgr.promote_window_launch("p1");

    assert!(
        mgr.window_launch_state("p1").is_none(),
        "a cleared window must stay cleared even after the grace deadline fires"
    );
}

#[test]
fn register_window_launch_async_path_promotes_on_deadline() {
    let mut mgr = counting_manager();
    mgr.set_launch_overlay_grace(Duration::from_millis(150));
    mgr.register_window_launch("p1", "claude -w slow");
    assert_eq!(
        mgr.window_launch_state("p1"),
        Some(&WindowLaunchStatus::Pending {
            command: "claude -w slow".into()
        }),
        "before the deadline the state is Pending — overlay stays hidden"
    );

    // The injected deadline fires (App-Nap-safe in production, direct here).
    mgr.promote_window_launch("p1");

    assert_eq!(
        mgr.window_launch_state("p1"),
        Some(&WindowLaunchStatus::Visible {
            command: "claude -w slow".into()
        }),
        "after the deadline the entry is promoted to Visible"
    );
}

#[test]
fn register_window_launch_replaces_prior_entry() {
    // A second register for the same paneId replaces the first (defends against
    // in-place window promotion re-using an id that already had state).
    let mut mgr = counting_manager();
    mgr.set_launch_overlay_grace(Duration::ZERO);
    mgr.register_window_launch("p1", "claude");
    assert_eq!(
        mgr.window_launch_state("p1"),
        Some(&WindowLaunchStatus::Visible {
            command: "claude".into()
        })
    );

    mgr.register_window_launch("p1", "claude --resume");

    assert_eq!(
        mgr.window_launch_state("p1"),
        Some(&WindowLaunchStatus::Visible {
            command: "claude --resume".into()
        }),
        "re-registering must overwrite the command, not stack entries"
    );
}

#[test]
fn window_exited_clears_launch_state() {
    // A window that exits — even silently, before emitting any byte — must not
    // leave a stale overlay entry behind.
    let mut mgr = counting_manager();
    let mut model = seeded();
    mgr.set_launch_overlay_grace(Duration::ZERO);
    let term_window_id = "p-exit";
    let mut session = Session::new("t1", "t", "/tmp");
    session.windows = vec![TermWindow::new(term_window_id, "Claude", TermWindowKind::Claude)];
    session.active_window_id = Some(term_window_id.to_string());
    model.projects.push(Project {
        id: "p".into(),
        name: "P".into(),
        path: "/tmp".into(),
        sessions: vec![session],
    });
    mgr.register_window_launch(term_window_id, "claude");
    assert!(mgr.window_launch_state(term_window_id).is_some());

    mgr.window_exited(&mut model, &mut selection(), "t1", term_window_id);

    assert!(
        mgr.window_launch_state(term_window_id).is_none(),
        "an exited window must leave no stale overlay entry"
    );
}

// ===========================================================================
// AppStateTabSelectionTests — prune wiring through the dissolve cascade (ported)
// ===========================================================================

#[test]
fn closing_session_prunes_from_multi_selection() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_claude_session_in(&mut model, "pa", "a", false);
    seed_claude_session_in(&mut model, "pb", "b", false);
    let mut sel = SidebarSessionSelection::new();
    sel.replace("a");
    let _ = sel.toggle("b");
    assert_eq!(
        sel.selected_session_ids(),
        &HashSet::from(["a".to_string(), "b".to_string()])
    );

    mgr.close_session(&mut model, &mut sel, "a");

    assert_eq!(
        sel.selected_session_ids(),
        &HashSet::from(["b".to_string()]),
        "finalize_dissolved_session must prune so closed sessions don't linger in the selection"
    );
}

#[test]
fn closing_session_clears_anchor_when_anchor_was_the_closed_session() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_claude_session_in(&mut model, "pa", "a", false);
    seed_claude_session_in(&mut model, "pb", "b", false);
    let mut sel = SidebarSessionSelection::new();
    sel.replace("b");
    let _ = sel.toggle("a"); // toggle moves the anchor to the toggled id
    assert_eq!(sel.last_clicked_session_id(), Some("a"));

    mgr.close_session(&mut model, &mut sel, "a");

    assert_eq!(
        sel.last_clicked_session_id(),
        None,
        "the anchor must clear when its session dissolves"
    );
}

#[test]
fn closing_session_keeps_anchor_when_anchor_survives() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_claude_session_in(&mut model, "pa", "a", false);
    seed_claude_session_in(&mut model, "pb", "b", false);
    let mut sel = SidebarSessionSelection::new();
    sel.replace("b");
    let _ = sel.toggle("a"); // anchor is now `a`; we close `b` instead

    mgr.close_session(&mut model, &mut sel, "b");

    assert_eq!(
        sel.last_clicked_session_id(),
        Some("a"),
        "the anchor must survive when a different session dissolves"
    );
    assert_eq!(sel.selected_session_ids(), &HashSet::from(["a".to_string()]));
}

// ===========================================================================
// Tri-state close shapes — held / spawning / model-only all reach the cascade
// (AppStateCloseProjectTests's three no-live-child shapes + the
// NiceTerminalViewDeferredSpawnTests distinctions).
// ===========================================================================

#[test]
fn close_session_claude_session_with_unspawned_companion_dissolves() {
    // Model-only shape: neither window has a session. Close must still dissolve
    // the row — an earlier cut left the session alive with its unfocused companion.
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_claude_session_in(&mut model, "p1", "t1", false);
    seed_claude_session_in(&mut model, "p2", "t2", false); // keep off the window terminus

    mgr.close_session(&mut model, &mut selection(), "t1");

    assert!(
        model.session_for("t1").is_none(),
        "close must dissolve the session even when the companion terminal was never spawned"
    );
    assert!(
        model.projects.iter().any(|p| p.id == "p1"),
        "close session must leave the containing project in place (only close-project removes it)"
    );
}

#[test]
fn close_session_armed_deferred_claude_window_with_unspawned_companion_dissolves() {
    // Spawning shape: the Claude window captured a deferred spawn that never fired
    // (paneIsSpawned true), the companion is model-only. Close routes the Claude
    // window through terminate_window's armed fast path → synthesized nil exit.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal) = seed_claude_session_in(&mut model, "p1", "t1", false);
    seed_claude_session_in(&mut model, "p2", "t2", false);
    mgr.mark_synthetic_armed_deferred_window("t1", &claude_id);

    mgr.close_session(&mut model, &mut selection(), "t1");

    assert!(
        model.session_for("t1").is_none(),
        "close on a never-focused resume-deferred Claude session must dissolve the sidebar row"
    );
    assert!(model.projects.iter().any(|p| p.id == "p1"));
}

#[test]
fn close_session_held_claude_window_with_unspawned_companion_dissolves() {
    // Held shape: the Claude window's process already died (view mounted), the
    // companion is model-only. Close routes the held window through terminate_window's
    // held fast path → synchronous window_exited → cascade.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal) = seed_claude_session_in(&mut model, "p1", "t1", false);
    seed_claude_session_in(&mut model, "p2", "t2", false);
    model.mutate_session("t1", |session| {
        let term_window = session.windows.iter_mut().find(|w| w.id == claude_id).unwrap();
        term_window.is_alive = false;
        term_window.is_claude_running = false;
    });
    mgr.mark_synthetic_held_window("t1", &claude_id);

    mgr.close_session(&mut model, &mut selection(), "t1");

    assert!(
        model.session_for("t1").is_none(),
        "close on a held-window session must dissolve the row, not just remove the windows"
    );
    assert!(model.projects.iter().any(|p| p.id == "p1"));
}

// ===========================================================================
// Validation ordering probes (a)–(d)
// ===========================================================================

#[test]
fn probe_a_exit_refocuses_neighbor_and_flags_companion_spawn_before_dissolve() {
    // (a) Exiting the active window refocuses the slot neighbor AND signals the
    // deferred-companion spawn (step 4), and the dissolve check runs AFTER — a
    // surviving session with a refocused companion must NOT dissolve. window_exited
    // returns refocus_session=Some (→ the live caller spawns the companion) with
    // terminus=None, proving the exit handled the refocus-onto-companion case
    // instead of dissolving.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, terminal_id) = seed_claude_session_in(&mut model, "p", "t1", false);
    model.select_session("t1");
    mgr.set_active_window(&mut model, "t1", &claude_id);

    let res = mgr.window_exited(&mut model, &mut selection(), "t1", &claude_id);

    let session = model.session_for("t1").expect("session must survive — a companion remains");
    assert_eq!(
        session.active_window_id.as_deref(),
        Some(terminal_id.as_str()),
        "focus refocuses onto the slot neighbor (the deferred companion)"
    );
    assert_eq!(
        res.refocus_session.as_deref(),
        Some("t1"),
        "the surviving session is flagged for the step-4 companion spawn"
    );
    assert_eq!(
        res.terminus,
        DissolveTerminus::None,
        "the dissolve check ran after the refocus and saw a non-empty session"
    );
}

#[test]
fn probe_b_noop_title_re_report_reports_no_change() {
    // (b) A no-op title re-report fires no mutation event: window_title_changed
    // returns did-change (the caller's R18 save gate). A real change returns
    // true; re-reporting the same title returns false.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (_claude, terminal_id) = seed_claude_session_in(&mut model, "p", "t1", false);

    assert!(
        mgr.window_title_changed(&mut model, "t1", &terminal_id, "nvim foo.rb"),
        "a real title change reports changed"
    );
    assert!(
        !mgr.window_title_changed(&mut model, "t1", &terminal_id, "nvim foo.rb"),
        "re-reporting the current title must report no change (no mutation event)"
    );
}

#[test]
fn probe_c_terminate_all_two_held_windows_visits_each_once() {
    // (c) terminate_all with two held windows completes without skipping or
    // double-visiting an entry: both windows removed, session dissolved, both synthetic
    // markers consumed. The snapshot-first iteration is what makes this safe (the
    // first held window_exited mutates the model + cache mid-loop).
    let mut mgr = counting_manager();
    let mut model = seeded();
    // A session with two windows, both marked held (kind is irrelevant to terminate).
    let mut session = Session::new("t1", "t", "/tmp/p1");
    session.windows = vec![
        TermWindow::new("t1-a", "A", TermWindowKind::Terminal),
        TermWindow::new("t1-b", "B", TermWindowKind::Terminal),
    ];
    session.active_window_id = Some("t1-a".to_string());
    session.next_terminal_index = 3;
    model.projects.push(Project {
        id: "p1".into(),
        name: "P1".into(),
        path: "/tmp/p1".into(),
        sessions: vec![session],
    });
    seed_claude_session_in(&mut model, "p2", "t2", false); // keep off the window terminus
    mgr.mark_synthetic_held_window("t1", "t1-a");
    mgr.mark_synthetic_held_window("t1", "t1-b");

    mgr.terminate_all(&mut model, &mut selection(), "t1");

    assert!(
        model.session_for("t1").is_none(),
        "both held windows exit and the session dissolves — no entry skipped"
    );
    // Both one-shot markers consumed exactly once (a double-visit would have
    // found the marker already gone and mis-routed as model-only).
    assert!(!mgr.window_is_spawned("t1", "t1-a"));
    assert!(!mgr.window_is_spawned("t1", "t1-b"));
}

// ---- R20.5 terminal foreground-child busy seam ------------------------------

#[test]
fn synthetic_foreground_child_marker_reports_busy() {
    // The seam the busy-close → confirmation-modal wiring is unit-tested on:
    // a marker forces a "shell has a foreground child" answer with no real pty
    // running a real command (the live `tcgetpgrp` is covered by the scenario).
    let mut mgr = counting_manager();
    mgr.mark_synthetic_foreground_child("t1", "t1-a");
    assert_eq!(
        mgr.synthetic_or_absent_foreground_child("t1", "t1-a"),
        Some(true),
        "a synthetic-foreground-child marker must report busy without a real pty"
    );
}

#[test]
fn model_only_window_has_no_foreground_child() {
    // A model-only / absent window — no cached session and no synthetic marker —
    // is NOT busy (`shell_has_foreground_child` ⇒ false), mirroring Swift's
    // `guard let entry = entries[id] else { return false }`: a lazy companion
    // terminal never focused is idle, not a foreground-child holder.
    let mgr = counting_manager();
    assert_eq!(
        mgr.synthetic_or_absent_foreground_child("t1", "t1-a"),
        Some(false),
        "an absent/model-only window must not be classified as having a foreground child"
    );
}

// ---- W5 (R18) project-pending-removal (Close Project) -----------------------

#[test]
fn pending_removal_drops_project_row_when_its_last_session_dissolves() {
    // Close Project marks the project pending; closing its (model-only) session
    // dissolves it, and finalize drops the now-empty non-Terminals row.
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_claude_session_in(&mut model, "proj", "t1", false);
    // Another project keeps the window non-empty (so no terminus fires).
    seed_claude_session_in(&mut model, "other", "t2", false);
    mgr.mark_project_pending_removal("proj");

    let terminus = mgr.close_session(&mut model, &mut selection(), "t1");

    assert!(
        model.projects.iter().all(|p| p.id != "proj"),
        "the pending non-Terminals project row drops when its last session dissolves"
    );
    assert_eq!(terminus, DissolveTerminus::None, "the other project keeps the window alive");
}

#[test]
fn pending_removal_keeps_row_until_the_last_session_goes() {
    // A multi-session pending project keeps its row (and the flag) across the first
    // dissolve; only the final session drops the row.
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_claude_session_in(&mut model, "proj", "t1", false);
    seed_claude_session_in(&mut model, "proj", "t2", false); // same project, 2nd session
    mgr.mark_project_pending_removal("proj");

    mgr.close_session(&mut model, &mut selection(), "t1");
    assert!(
        model.projects.iter().any(|p| p.id == "proj"),
        "an earlier-session dissolve keeps the pending flag + the project row"
    );

    mgr.close_session(&mut model, &mut selection(), "t2");
    assert!(
        model.projects.iter().all(|p| p.id != "proj"),
        "the last-session dissolve finally drops the pending project row"
    );
}

#[test]
fn unmarked_project_row_survives_an_empty_session_dissolve() {
    // Without the pending flag, a dissolved session leaves its (now-empty) project
    // row in place — the default (non-Close-Project) close behavior.
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_claude_session_in(&mut model, "proj", "t1", false);
    seed_claude_session_in(&mut model, "other", "t2", false);

    mgr.close_session(&mut model, &mut selection(), "t1");

    assert!(
        model.projects.iter().any(|p| p.id == "proj"),
        "an unmarked project's empty row survives (only Close Project drops it)"
    );
}

#[test]
fn probe_d_close_model_only_session_reaches_cascade_synchronously() {
    // (d) Closing a session whose windows are all model-only reaches the cascade
    // synchronously — no async window-exit to wait on, the session is gone on return.
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_claude_session_in(&mut model, "p1", "t1", false);
    seed_claude_session_in(&mut model, "p2", "t2", false);

    let terminus = mgr.close_session(&mut model, &mut selection(), "t1");

    assert!(
        model.session_for("t1").is_none(),
        "a model-only session dissolves synchronously on close_session's return"
    );
    assert_eq!(terminus, DissolveTerminus::None, "other projects remain non-empty");
}

// ---- R14 env injection: the spec-wins merge + the per-window matrix -----------
//
// The manager's `spawn_window` merges these pairs into the caller-built spec's env
// before forking the pty. `spawn_window` itself needs a gpui `App`, so the pure
// merge + matrix (`session_window_env_pairs`, exercised here through a
// spawn_window-shaped merge) are unit-tested directly (Validation §3 a/b/c); the
// full live spawn path is the `shell-socket` scenario.

/// Helper: the injection pairs a window gets under the zsh profile, composed
/// exactly as production does (`crate::shell::window_inject_pairs` over a
/// `ShellRuntime`) so these tests keep pinning the real pairs rather than a
/// hand-rolled copy of them — including the degraded `zdotdir: None` shape.
fn zsh_inject_pairs(zdotdir: Option<&str>, user_zdotdir: Option<&str>) -> Vec<(String, String)> {
    let runtime = crate::shell::ShellRuntime {
        profile: Box::new(crate::shell::zsh::ZshProfile::new("/bin/zsh")),
        inject: zdotdir.map(|d| crate::shell::InjectPaths {
            dir: d.into(),
            rcfile: None,
        }),
        user_env: crate::shell::UserShellEnv {
            user_zdotdir: user_zdotdir.map(str::to_string),
        },
    };
    crate::shell::window_inject_pairs(&runtime)
}

/// Helper: a manager with a fully-populated window shell env (socket + the zsh
/// profile's injection pairs for a given zdotdir / inherited user zdotdir).
fn manager_with_shell_env(
    socket: Option<&str>,
    zdotdir: Option<&str>,
    user_zdotdir: Option<&str>,
) -> PtyManager {
    let mut m = PtyManager::new();
    m.set_window_shell_env(WindowShellEnv {
        socket_path: socket.map(str::to_string),
        inject_pairs: zsh_inject_pairs(zdotdir, user_zdotdir),
        compose_conf: Some("/conf/compose.json".to_string()),
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
    merge_env_spec_wins(&mut spec.env, mgr.session_window_env_pairs("t1", "p1"));

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

/// Validation §3(b): a window spawned through the manager carries
/// `NICE_SOCKET` + `NICE_TAB_ID` + `NICE_PANE_ID` (the exact ids handed to
/// `spawn_window` — the same ids `ensure_active_window_spawned` passes through).
#[test]
fn injected_window_env_carries_socket_and_window_identity() {
    let mgr = manager_with_shell_env(Some("/tmp/win.sock"), Some("/z"), Some("/user/z"));
    // A default shell spec (what `ensure_active_window_spawned` builds), then the
    // exact merge `spawn_window` performs.
    let mut spec = SpawnSpec::shell("/work");
    merge_env_spec_wins(&mut spec.env, mgr.session_window_env_pairs("tabX", "paneY"));

    assert_eq!(value_of(&spec.env, "NICE_SOCKET"), Some("/tmp/win.sock"));
    assert_eq!(value_of(&spec.env, "NICE_TAB_ID"), Some("tabX"));
    assert_eq!(value_of(&spec.env, "NICE_PANE_ID"), Some("paneY"));
    assert_eq!(value_of(&spec.env, "ZDOTDIR"), Some("/z"));
    // Command Compose: the conf path rides the same injection (the ZLE widget
    // reads it per compose); absent field ⇒ var not injected (next test).
    assert_eq!(
        value_of(&spec.env, "NICE_COMPOSE_CONF"),
        Some("/conf/compose.json")
    );
}

/// Command Compose: a `WindowShellEnv` without a conf path injects NO
/// `NICE_COMPOSE_CONF` var (the widget falls back to its defaults).
#[test]
fn absent_compose_conf_is_not_injected() {
    let mut m = PtyManager::new();
    m.set_window_shell_env(WindowShellEnv {
        socket_path: Some("/tmp/s".to_string()),
        inject_pairs: zsh_inject_pairs(None, None),
        compose_conf: None,
    });
    let pairs = m.session_window_env_pairs("t", "p");
    assert!(
        !pairs.iter().any(|(k, _)| k == "NICE_COMPOSE_CONF"),
        "no compose_conf field ⇒ no NICE_COMPOSE_CONF injection"
    );
}

/// Validation §3(c): `NICE_USER_ZDOTDIR` is present-but-EMPTY when Nice inherited
/// no `ZDOTDIR` (the empty/absent distinction the `.zshenv` stub keys off).
#[test]
fn user_zdotdir_is_present_but_empty_when_none_inherited() {
    let mgr = manager_with_shell_env(Some("/tmp/sock"), Some("/z"), None);
    let pairs = mgr.session_window_env_pairs("t", "p");
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
    let mgr = PtyManager::new();
    assert!(
        mgr.session_window_env_pairs("t", "p").is_empty(),
        "a manager with no window shell env must inject nothing"
    );
}

// ---- the per-pane shell snapshot (design §2's PaneShell) ---------------------

/// A spawned pane captures the ACTIVE profile's kind + compose capability, and a
/// manager with no runtime installed (the `run_selftest` shape) captures the
/// historical zsh snapshot. `compose_route` gates the trigger bytes on this, so a
/// wrong snapshot is exactly finding F6 coming back.
#[gpui::test]
fn spawned_pane_snapshots_the_active_profile(cx: &mut gpui::TestAppContext) {
    // A cheap hermetic child: no shell rc, no user config, just a process that
    // sits on its pty until the manager drops it.
    fn spec() -> SpawnSpec {
        SpawnSpec::shell(std::env::temp_dir().to_string_lossy().to_string()).with_argv(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 30".to_string(),
        ])
    }

    let zsh = crate::shell::PaneShell {
        kind: crate::shell::ShellKind::Zsh,
        compose: crate::shell::ComposeSupport::Trigger,
    };

    cx.update(|cx| {
        // 1. No ShellRuntime (scenarios never bootstrap one) ⇒ the zsh default.
        let mut mgr = PtyManager::new();
        mgr.set_event_wakes_enabled_for_test(false);
        mgr.spawn_window("t1", "p1", spec(), cx).unwrap();
        assert_eq!(mgr.pane_shell("t1", "p1", "p1"), Some(zsh));

        // 2. A zsh runtime ⇒ the same snapshot, read off the profile this time.
        crate::app::set_scenario_shell_inject_config(cx, None, None);
        mgr.spawn_window("t1", "p2", spec(), cx).unwrap();
        assert_eq!(mgr.pane_shell("t1", "p2", "p2"), Some(zsh));

        // 3. A fallback runtime ⇒ a pane that must never receive trigger bytes.
        cx.set_global(crate::shell::ShellRuntime {
            profile: Box::new(crate::shell::fallback::FallbackProfile::new(
                "/usr/local/bin/fish",
            )),
            inject: None,
            user_env: crate::shell::UserShellEnv::default(),
        });
        mgr.spawn_window("t1", "p3", spec(), cx).unwrap();
        assert_eq!(
            mgr.pane_shell("t1", "p3", "p3"),
            Some(crate::shell::PaneShell {
                kind: crate::shell::ShellKind::Other,
                compose: crate::shell::ComposeSupport::None,
            })
        );

        // 4. The earlier panes keep THEIR snapshot — a mid-run profile swap never
        //    reaches back into a running pane (design §2: runtime-only, per pane).
        assert_eq!(mgr.pane_shell("t1", "p1", "p1"), Some(zsh));
        assert_eq!(mgr.pane_shell("t1", "p2", "p2"), Some(zsh));

        // 5. A window with no live pty has no snapshot.
        assert_eq!(mgr.pane_shell("t1", "nope", "nope"), None);
        assert_eq!(mgr.pane_shell("nope", "p1", "p1"), None);

        mgr.teardown();
    });
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
            &zsh_inject_pairs(Some("/z"), Some("/user/z")),
            crate::shell::PrefillStrategy::ShellSide,
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
    let env = build_claude_extra_env(
        &ClaudeSessionMode::None,
        "t",
        "p",
        None,
        &[],
        crate::shell::PrefillStrategy::ShellSide,
        None,
    );
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
        &zsh_inject_pairs(Some("/managed/z"), Some("/user/z")),
        crate::shell::PrefillStrategy::ShellSide,
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
        &zsh_inject_pairs(Some("/z"), None),
        crate::shell::PrefillStrategy::ShellSide,
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
        &zsh_inject_pairs(Some("/z"), Some("/user/z")),
        crate::shell::PrefillStrategy::ShellSide,
        Some("/Users/nick/Library/Application Support/settings.json".to_string()),
    );
    assert_eq!(
        value_of(&env, "NICE_PREFILL_COMMAND"),
        Some("claude --settings '/Users/nick/Library/Application Support/settings.json' --resume SID"),
        "--settings must precede --resume and be single-quoted"
    );
}

/// A profile with no prefill mechanism (design §5's fallback) gets NO
/// `NICE_PREFILL_COMMAND` — its shell has no rc tail that would consume the var,
/// so the pane opens at a bare prompt. Everything else about the mode is
/// unchanged, and the profile's (empty) inject pairs still splice.
#[test]
fn claude_extra_env_prefill_off_sets_no_prefill_command() {
    let env = build_claude_extra_env(
        &ClaudeSessionMode::ResumeDeferred("SID-123".into()),
        "t1",
        "p1",
        Some("/tmp/s.sock"),
        // What `window_inject_pairs` yields for a fallback profile: nothing.
        &[],
        crate::shell::PrefillStrategy::Off,
        Some("/settings.json".to_string()),
    );
    assert_eq!(
        value_of(&env, "NICE_PREFILL_COMMAND"),
        None,
        "a shell with no prefill mechanism must not be handed the prefill var"
    );
    assert_eq!(value_of(&env, "ZDOTDIR"), None, "no zsh var leaks into a non-zsh pane");
    assert_eq!(value_of(&env, "NICE_USER_ZDOTDIR"), None);
    // The common columns are untouched by the prefill axis.
    assert_eq!(value_of(&env, "TERM_PROGRAM"), Some("ghostty"));
    assert_eq!(value_of(&env, "NICE_TAB_ID"), Some("t1"));
    assert_eq!(value_of(&env, "NICE_PANE_ID"), Some("p1"));
    assert_eq!(value_of(&env, "NICE_SOCKET"), Some("/tmp/s.sock"));
}

/// The app-typed strategy (step 4's bash prefill) also sets no env var — it
/// records a pending prefill and types it after the pane's first OSC 7. Nothing
/// returns it yet; this pins that the env composer stays out of its way.
#[test]
fn claude_extra_env_prefill_app_typed_sets_no_prefill_command() {
    let env = build_claude_extra_env(
        &ClaudeSessionMode::ResumeDeferred("SID".into()),
        "t",
        "p",
        Some("/s"),
        &[("NICE_BASH_RC".to_string(), "/rc".to_string())],
        crate::shell::PrefillStrategy::AppTyped,
        None,
    );
    assert_eq!(value_of(&env, "NICE_PREFILL_COMMAND"), None);
    assert_eq!(
        value_of(&env, "NICE_BASH_RC"),
        Some("/rc"),
        "the profile's own inject pairs still splice, whatever the prefill strategy"
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
// App-typed prefill plumbing (design §6.4) — the delivery half of the same
// FROZEN line for a shell with no `print -z`. `build_claude_extra_env` (above)
// covers the env half: `ShellSide` gets `NICE_PREFILL_COMMAND`, `AppTyped` and
// `Off` do not. Here: who gets a PENDING line, and that the pane's first OSC 7
// hands it out exactly once.
// =====================================================================

/// `pending_prefill_for` is the exact complement of the `NICE_PREFILL_COMMAND`
/// env row: only a `ResumeDeferred` spawn under an app-typed profile holds a
/// line, and the line is the frozen composer's, verbatim.
#[test]
fn pending_prefill_only_for_resume_deferred_under_an_app_typed_profile() {
    use crate::shell::PrefillStrategy;

    let deferred = ClaudeSessionMode::ResumeDeferred("SID".into());

    assert_eq!(
        pending_prefill_for(&deferred, PrefillStrategy::AppTyped, None).as_deref(),
        Some("claude --resume SID"),
        "bash's pane holds the frozen line pending its first OSC 7"
    );
    assert_eq!(
        pending_prefill_for(&deferred, PrefillStrategy::AppTyped, Some("/s.json")).as_deref(),
        Some("claude --settings '/s.json' --resume SID"),
        "the settings path splices through the same frozen composer"
    );
    assert_eq!(
        pending_prefill_for(&deferred, PrefillStrategy::ShellSide, None),
        None,
        "zsh already got the line via NICE_PREFILL_COMMAND — typing it too would double it"
    );
    assert_eq!(
        pending_prefill_for(&deferred, PrefillStrategy::Off, None),
        None,
        "a shell with no prefill mechanism opens at a bare prompt"
    );

    // No other mode prefills anything — they exec claude directly.
    for mode in [
        ClaudeSessionMode::None,
        ClaudeSessionMode::New("SID".into()),
        ClaudeSessionMode::Resume("SID".into()),
    ] {
        assert_eq!(
            pending_prefill_for(&mode, PrefillStrategy::AppTyped, Some("/s.json")),
            None,
            "{mode:?} runs claude itself; there is nothing to pre-type"
        );
    }
}

/// The spawn side arms the pane and the routing side disarms it: a deferred-resume
/// spawn under a bash profile records the line, the pane's FIRST `CwdChanged`
/// takes it, and every later one is a plain cwd update. The zsh profile records
/// nothing at all (its rc tail does the pre-typing).
///
/// Every cwd here is a path that does not exist, so each forked child `_exit`s at
/// its `chdir` and no shell is ever really sourced — the assertions are the
/// manager's own bookkeeping. The real-bash end of this lives in
/// `window_state`'s `prefill_is_typed_into_a_real_bash_pane_...` test.
#[gpui::test]
fn deferred_resume_arms_the_pending_prefill_and_the_first_osc7_takes_it(
    cx: &mut gpui::TestAppContext,
) {
    use crate::shell::ShellProfile;
    const NO_SPAWN_CWD: &str = "/nice-unit-test-no-such-dir";

    let rc = crate::shell::bash::hermetic::scratch("nice-prefill-rc");
    cx.update(|cx| {
        cx.set_global(crate::pty_manager::ResolvedClaudePath(None));

        // A real BashProfile runtime: `prefill()` is `AppTyped` and `spawn_argv`
        // needs genuine `InjectPaths` (its `--rcfile` comes from them).
        let profile = crate::shell::bash::BashProfile::new("/bin/bash");
        let inject = profile.write_rc_files(&rc.0).expect("write nice.bashrc");
        cx.set_global(crate::shell::ShellRuntime {
            profile: Box::new(profile),
            inject: Some(inject),
            user_env: crate::shell::UserShellEnv::default(),
        });

        let mut model = WorkspaceModel::new("/home/u");
        let (claude_window, _) = seed_claude_session_in(&mut model, "p", "t1", false);
        let mut mgr = PtyManager::new();
        mgr.set_event_wakes_enabled_for_test(false);

        mgr.spawn_claude_window(
            "t1",
            &claude_window,
            NO_SPAWN_CWD,
            &ClaudeSessionMode::ResumeDeferred("SID".into()),
            &[],
            Some("/s.json"),
            cx,
        )
        .expect("the deferred-resume spawn returns Ok even when the child dies at chdir");

        assert_eq!(
            mgr.pending_prefill("t1", &claude_window, &claude_window),
            Some("claude --settings '/s.json' --resume SID"),
            "an app-typed pane is armed at spawn"
        );

        // First OSC 7: the line comes back to the subscription, AND the cwd
        // routing that has always run still runs.
        let mut selection = selection();
        let routed = mgr.route_terminal_event(
            &mut model,
            &mut selection,
            "t1",
            &claude_window,
            // A never-split window's sole pane carries the window's own id.
            &claude_window,
            &TerminalEvent::CwdChanged("/tmp/first".into()),
        );
        let line = routed.prefill.expect("the first OSC 7 hands the line out");
        assert_eq!(line, "claude --settings '/s.json' --resume SID");
        assert!(
            !line.ends_with('\n') && !line.ends_with('\r'),
            "the line must sit EDITABLE at the prompt — a trailing newline would run it: <{line}>"
        );
        assert_eq!(
            window_cwd(&model, "t1", &claude_window),
            Some("/tmp/first".to_string()),
            "taking the prefill must not displace the OSC 7's cwd update"
        );

        // Second OSC 7: a plain cwd update. `take` makes this once-only by
        // construction, which is what keeps the line from being typed twice.
        let routed = mgr.route_terminal_event(
            &mut model,
            &mut selection,
            "t1",
            &claude_window,
            // A never-split window's sole pane carries the window's own id.
            &claude_window,
            &TerminalEvent::CwdChanged("/tmp/second".into()),
        );
        assert_eq!(routed.prefill, None, "the slot is emptied by the first take");
        assert_eq!(
            mgr.pending_prefill("t1", &claude_window, &claude_window),
            None,
            "and stays empty"
        );
        assert_eq!(
            window_cwd(&model, "t1", &claude_window),
            Some("/tmp/second".to_string()),
            "later OSC 7s are plain cwd updates"
        );

        // A zsh runtime over the same spawn: nothing pending, because the rc
        // tail's `print -z` reads NICE_PREFILL_COMMAND out of the env instead.
        crate::app::set_scenario_shell_inject_config(cx, None, None);
        let (zsh_window, _) = seed_claude_session_in(&mut model, "p", "t2", false);
        mgr.spawn_claude_window(
            "t2",
            &zsh_window,
            NO_SPAWN_CWD,
            &ClaudeSessionMode::ResumeDeferred("SID".into()),
            &[],
            Some("/s.json"),
            cx,
        )
        .expect("spawn");
        assert_eq!(
            mgr.pending_prefill("t2", &zsh_window, &zsh_window),
            None,
            "a shell-side profile is never armed"
        );
        let routed = mgr.route_terminal_event(
            &mut model,
            &mut selection,
            "t2",
            &zsh_window,
            &zsh_window,
            &TerminalEvent::CwdChanged("/tmp/zsh".into()),
        );
        assert_eq!(routed.prefill, None);

        mgr.teardown();
    });
}

/// A window's routed cwd, as `window_cwd_changed` stored it.
fn window_cwd(model: &WorkspaceModel, session_id: &str, term_window_id: &str) -> Option<String> {
    model
        .session_for(session_id)?
        .windows
        .iter()
        .find(|w| w.id == term_window_id)?
        .cwd
        .clone()
}

// =====================================================================
// compose_claude_reply — the FROZEN socket reply grammar (≤3 positional fields;
// reply tail of `handleClaudeSocketRequest`, `SessionsModel.swift:897-910`).
// =====================================================================

/// The newtab path replies `newtab` regardless of any settings path.
#[test]
fn reply_newtab() {
    assert_eq!(compose_claude_reply(&ClaudeReplyDecision::NewSession, None), "newtab");
    assert_eq!(
        compose_claude_reply(&ClaudeReplyDecision::NewSession, Some("/s.json")),
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
        claude_session_id: "OLD".into(),
    };
    assert_eq!(compose_claude_reply(&decision, None), "inplace");
}

/// In-place, minted id, sync off → `inplace <uuid>`
/// (`test_inplaceWithoutSessionId_mintsFreshIdAndRepliesWithIt`).
#[test]
fn reply_inplace_minted_id_sync_off_appends_uuid() {
    let decision = ClaudeReplyDecision::InPlace {
        parsed_from_args: false,
        claude_session_id: "minted-uuid".into(),
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
        claude_session_id: "unused".into(),
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
        claude_session_id: "minted-uuid".into(),
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
            claude_session_id: "x".into(),
        };
        let reply = compose_claude_reply(&decision, None);
        assert!(
            reply.split(' ').count() <= 2,
            "sync-off reply {reply:?} must be ≤2 fields"
        );
    }
}

/// Fix D's two normalizing verbs keep the same three positional fields, so the
/// wrapper's frozen `read -r mode sid settings` still parses them. Their id
/// field is ALWAYS the full uuid — never the `-` placeholder — because the
/// whole point is handing the wrapper an id its own args did not have.
#[test]
fn reply_attach_and_resume_carry_the_uuid_then_the_pointer() {
    let uuid = "b8c8244b-e94e-4c38-95fb-31be9a28187e";
    for (decision, verb) in [
        (
            ClaudeReplyDecision::Attach {
                claude_session_id: uuid.into(),
            },
            "attach",
        ),
        (
            ClaudeReplyDecision::Resume {
                claude_session_id: uuid.into(),
            },
            "resume",
        ),
    ] {
        assert_eq!(compose_claude_reply(&decision, None), format!("{verb} {uuid}"));
        assert_eq!(
            compose_claude_reply(&decision, Some("/ptr.json")),
            format!("{verb} {uuid} /ptr.json"),
            "the settings pointer stays the 3rd field"
        );
    }
}

// =====================================================================
// parse_claude_title — the T5 status/label split (`SessionsModel.swift:439-453`).
// The pure split; the trim / empty-skip / "Claude Code" placeholder / auto-title
// pipeline is R15 slice-3's window_title_changed branch.
// =====================================================================

/// A braille-spinner first scalar (U+2800..=U+28FF) ⇒ Thinking; the label is the
/// remainder after the prefix scalar.
#[test]
fn parse_title_braille_spinner_sets_thinking() {
    let (status, label) = parse_claude_title("\u{2840} fix-top-bar-height");
    assert_eq!(status, Some(SessionStatus::Thinking));
    assert_eq!(label, " fix-top-bar-height");
}

/// The braille range is inclusive at both ends.
#[test]
fn parse_title_braille_range_boundaries_set_thinking() {
    assert_eq!(parse_claude_title("\u{2800}x").0, Some(SessionStatus::Thinking));
    assert_eq!(parse_claude_title("\u{28FF}x").0, Some(SessionStatus::Thinking));
}

/// Claude Code 2.1.228 swapped the title busy spinner from braille to the
/// half-shaded circles ◐◑ ("Updated terminal title busy-spinner glyphs to
/// reduce tab-bar jitter"); the whole ◐◓◑◒ quad (U+25D0..=U+25D3) ⇒ Thinking.
#[test]
fn parse_title_half_shaded_circle_spinner_sets_thinking() {
    // The observed 2.1.228 title shape: "◐ <label>".
    let (status, label) = parse_claude_title("\u{25D0} count-to-three");
    assert_eq!(status, Some(SessionStatus::Thinking));
    assert_eq!(label, " count-to-three");
    // Every phase of the quad, inclusive at both ends.
    for c in ['\u{25D0}', '\u{25D1}', '\u{25D2}', '\u{25D3}'] {
        assert_eq!(
            parse_claude_title(&format!("{c}x")).0,
            Some(SessionStatus::Thinking),
            "{c:?} must map to Thinking"
        );
    }
    // Neighbours just outside the quad stay plain labels.
    assert_eq!(parse_claude_title("\u{25CF}x").0, None); // ● black circle
    assert_eq!(parse_claude_title("\u{25D4}x").0, None); // ◔ quadrant circle
}

/// The sparkle ✳ (U+2733) ⇒ Waiting.
#[test]
fn parse_title_sparkle_sets_waiting() {
    let (status, label) = parse_claude_title("\u{2733} needs-input");
    assert_eq!(status, Some(SessionStatus::Waiting));
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
        (Some(SessionStatus::Thinking), " Claude Code")
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
        (Some(SessionStatus::Waiting), "")
    );
}

// =====================================================================
// mint_session_uuid — real lowercased UUIDv4 (getentropy-backed), a separate
// mint from the ms+counter session/window id minter.
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

// ---- R15 Claude-session constructor pure helpers --------------------------------
//
// The constructor itself (`create_claude_session`) spawns a pty, so its end-to-end
// shape is the slice-3 `claude-lifecycle` scenario; these pin the pure pieces it
// composes (Swift `createTabFromMainTerminal`'s title + sessionCwd closures and
// `TabPtySession.launchDisplayCommand`).

// claude_session_title_from_args — join, 40-char cap, trim, empty ⇒ "New session".

#[test]
fn claude_title_empty_args_is_new_session() {
    assert_eq!(claude_session_title_from_args(&[]), "New session");
}

#[test]
fn claude_title_joins_args_with_spaces() {
    let args = vec!["--resume".to_string(), "abc-123".to_string()];
    assert_eq!(claude_session_title_from_args(&args), "--resume abc-123");
}

#[test]
fn claude_title_caps_at_40_chars() {
    // A single 50-char arg: the cut lands mid-content, no trailing space to trim.
    let args = vec!["x".repeat(50)];
    let title = claude_session_title_from_args(&args);
    assert_eq!(title, "x".repeat(40));
    assert_eq!(title.chars().count(), 40);
}

#[test]
fn claude_title_trims_trailing_space_exposed_by_the_cut() {
    // 39 x's + " tail" joins to 44 chars; the 40-char cut lands on the space after
    // the x-run, which is then trimmed off (→ 39 x's).
    let args = vec!["x".repeat(39), "tail".to_string()];
    let title = claude_session_title_from_args(&args);
    assert_eq!(title, "x".repeat(39));
}

#[test]
fn claude_title_all_whitespace_falls_back_to_new_session() {
    let args = vec!["   ".to_string()];
    assert_eq!(claude_session_title_from_args(&args), "New session");
}

// claude_worktree_cwd — Session.cwd worktree split (`-w` space form only, `/`→`+`).

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

// handoff_title — the locked "[H] …" label (R26). Strip a single existing
// prefix (no stacking), trim, blank → "Session".

#[test]
fn handoff_title_prefixes_a_plain_title() {
    assert_eq!(handoff_title(Some("Foo")), "[H] Foo");
}

#[test]
fn handoff_title_does_not_stack_an_existing_prefix() {
    // A handoff fired FROM a handoff session reads "[H] Foo", not doubled.
    assert_eq!(handoff_title(Some("[H] Foo")), "[H] Foo");
}

#[test]
fn handoff_title_none_falls_back_to_session() {
    assert_eq!(handoff_title(None), "[H] Session");
}

#[test]
fn handoff_title_whitespace_only_falls_back_to_session() {
    // A whitespace-only title would otherwise yield a ragged "[H]    ".
    assert_eq!(handoff_title(Some("   ")), "[H] Session");
}

#[test]
fn handoff_title_trims_surrounding_whitespace() {
    assert_eq!(handoff_title(Some("  Bar  ")), "[H] Bar");
}

// handoff_prompt — always points at the notes file; blank instructions get the
// default read-and-wait directive, custom instructions override it.

#[test]
fn handoff_prompt_empty_instructions_uses_default_directive() {
    assert_eq!(
        handoff_prompt("/x/y.md", ""),
        "Read the handoff notes at /x/y.md. Do not start working yet — once you have \
         read it, wait for the user to tell you how to proceed."
    );
}

#[test]
fn handoff_prompt_custom_instructions_override_the_default() {
    assert_eq!(
        handoff_prompt("/x/y.md", "keep going"),
        "Read the handoff notes at /x/y.md. keep going"
    );
}

#[test]
fn handoff_prompt_whitespace_only_instructions_fall_back_to_default() {
    assert_eq!(
        handoff_prompt("/x/y.md", "   \n\t "),
        "Read the handoff notes at /x/y.md. Do not start working yet — once you have \
         read it, wait for the user to tell you how to proceed."
    );
}

// handoff_extra_args — optional --model/--effort flags, prompt ALWAYS last.

#[test]
fn handoff_extra_args_empty_model_and_effort_is_just_the_prompt() {
    assert_eq!(handoff_extra_args("", "", "P"), vec!["P".to_string()]);
}

#[test]
fn handoff_extra_args_model_only() {
    assert_eq!(
        handoff_extra_args("claude-opus-4-8", "", "P"),
        vec!["--model", "claude-opus-4-8", "P"]
    );
}

#[test]
fn handoff_extra_args_effort_only() {
    assert_eq!(
        handoff_extra_args("", "xhigh", "P"),
        vec!["--effort", "xhigh", "P"]
    );
}

#[test]
fn handoff_extra_args_model_and_effort_then_prompt_last() {
    assert_eq!(
        handoff_extra_args("m", "xhigh", "P"),
        vec!["--model", "m", "--effort", "xhigh", "P"]
    );
}

// dispatch_title — the locked "[D] <worktree-name>" label.

#[test]
fn dispatch_title_prefixes_the_worktree_name() {
    assert_eq!(dispatch_title("fix-tabs"), "[D] fix-tabs");
}

#[test]
fn dispatch_title_trims_surrounding_whitespace() {
    assert_eq!(dispatch_title("  fix-tabs  "), "[D] fix-tabs");
}

#[test]
fn dispatch_title_blank_name_falls_back_to_session() {
    // The socket parser rejects an EMPTY worktreeName, but " " gets through and
    // would otherwise render a ragged "[D]  ".
    assert_eq!(dispatch_title("   "), "[D] Session");
}

// dispatch_prompt — always points at the task file and tells the child to START
// (the opposite of handoff's read-and-wait); instructions append when non-blank.

#[test]
fn dispatch_prompt_empty_instructions_is_the_read_and_start_directive() {
    assert_eq!(
        dispatch_prompt("/repo/.claude/dispatch/w-1.md", ""),
        "Read the dispatch task file at /repo/.claude/dispatch/w-1.md, then start \
         working on the task it describes."
    );
}

#[test]
fn dispatch_prompt_appends_non_empty_instructions() {
    assert_eq!(
        dispatch_prompt("/t/w.md", "Only touch the parser."),
        "Read the dispatch task file at /t/w.md, then start working on the task it \
         describes. Only touch the parser."
    );
}

#[test]
fn dispatch_prompt_whitespace_only_instructions_are_dropped() {
    assert_eq!(
        dispatch_prompt("/t/w.md", "  \n\t "),
        "Read the dispatch task file at /t/w.md, then start working on the task it \
         describes."
    );
}

// dispatch_extra_args — the load-bearing order: --add-dir, then --worktree
// (which terminates --add-dir's VARIADIC list), then optional --model/--effort,
// then the prompt LAST.

#[test]
fn dispatch_extra_args_default_order_is_add_dir_worktree_then_prompt() {
    // The default dispatch (no model, no effort) is exactly the case where a
    // prompt placed right after `--add-dir` would be swallowed as a second
    // directory — `--worktree` must sit between them.
    assert_eq!(
        dispatch_extra_args("wt", "/repo/.claude/dispatch/wt-1.md", "", "", "P"),
        vec![
            "--add-dir",
            "/repo/.claude/dispatch",
            "--worktree",
            "wt",
            "P"
        ]
    );
}

#[test]
fn dispatch_extra_args_model_only() {
    assert_eq!(
        dispatch_extra_args("wt", "/d/t.md", "opus", "", "P"),
        vec!["--add-dir", "/d", "--worktree", "wt", "--model", "opus", "P"]
    );
}

#[test]
fn dispatch_extra_args_effort_only() {
    assert_eq!(
        dispatch_extra_args("wt", "/d/t.md", "", "xhigh", "P"),
        vec![
            "--add-dir", "/d", "--worktree", "wt", "--effort", "xhigh", "P"
        ]
    );
}

#[test]
fn dispatch_extra_args_model_and_effort_then_prompt_last() {
    let args = dispatch_extra_args("wt", "/d/t.md", "opus", "xhigh", "P");
    assert_eq!(
        args,
        vec![
            "--add-dir",
            "/d",
            "--worktree",
            "wt",
            "--model",
            "opus",
            "--effort",
            "xhigh",
            "P"
        ]
    );
    assert_eq!(args.last().map(String::as_str), Some("P"), "prompt stays last");
}

#[test]
fn dispatch_extra_args_bare_task_file_name_omits_add_dir() {
    // No parent directory component ⇒ nothing to add; `--worktree` leads, so the
    // variadic hazard cannot arise.
    assert_eq!(
        dispatch_extra_args("wt", "t.md", "", "", "P"),
        vec!["--worktree", "wt", "P"]
    );
}

// ===========================================================================
// tmux-port Phase 2 — panes: exit / hold / status / break-pane
//
// The pty CACHE half of these paths needs a gpui `App` to spawn a real handle,
// which the `nice` binary crate cannot link (see this module's header), so what
// is pinned here is the model half — which is where every user-visible
// consequence lives. The handle moves and subscription re-keying are covered by
// the `splits` live scenario.
// ===========================================================================

/// Split `window_id`'s pill by adding a shell leaf `new_pane_id` beside the
/// existing one, focused — what the `^⌘\` action does to the model.
fn split_window(model: &mut WorkspaceModel, session_id: &str, window_id: &str, new_pane_id: &str) {
    model.mutate_session(session_id, |session| {
        let window = session
            .windows
            .iter_mut()
            .find(|w| w.id == window_id)
            .expect("window to split");
        let target = window.active_pane_id.clone();
        assert!(window.layout.split(
            &target,
            nice_model::SplitOrient::Beside,
            nice_model::Pane::new(new_pane_id, TermWindowKind::Terminal),
        ));
        window.active_pane_id = new_pane_id.to_string();
    });
}

fn window_in<'a>(model: &'a WorkspaceModel, session_id: &str, window_id: &str) -> &'a TermWindow {
    model
        .session_for(session_id)
        .unwrap()
        .windows
        .iter()
        .find(|w| w.id == window_id)
        .expect("window still on the session")
}

#[test]
fn pane_exit_in_a_split_pill_collapses_the_tree_and_keeps_the_pill() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal_id) = seed_claude_session(&mut model, "t1");
    split_window(&mut model, "t1", &claude_id, "shell");

    let res = mgr.pane_exited(&mut model, &mut selection(), "t1", &claude_id, "shell");

    let window = window_in(&model, "t1", &claude_id);
    assert_eq!(window.layout.leaf_count(), 1, "the split collapsed");
    assert_eq!(
        window.active_pane_id, claude_id,
        "focus moved spatially to the surviving pane"
    );
    assert_eq!(
        model.session_for("t1").unwrap().windows.len(),
        2,
        "a pane close is not a pill close"
    );
    assert_eq!(
        res,
        super::WindowExitResolution::default(),
        "no session can dissolve while the pill still has panes"
    );
}

#[test]
fn last_pane_exit_delegates_to_the_pill_close_flow() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, terminal_id) = seed_claude_session(&mut model, "t1");

    // The pill's only pane — this is a pill close, index-neighbor refocus and all.
    mgr.pane_exited(&mut model, &mut selection(), "t1", &claude_id, &claude_id);

    let session = model.session_for("t1").unwrap();
    assert_eq!(
        session.windows.iter().map(|w| w.id.clone()).collect::<Vec<_>>(),
        vec![terminal_id.clone()],
        "the pill left the session"
    );
    assert_eq!(session.active_window_id.as_deref(), Some(terminal_id.as_str()));
}

#[test]
fn a_claude_leaf_exiting_cleanly_flips_the_pill_to_terminal() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal_id) = seed_claude_session(&mut model, "t1");
    model.mutate_session("t1", |session| {
        session
            .windows
            .iter_mut()
            .find(|w| w.id == claude_id)
            .unwrap()
            .is_claude_running = true;
    });
    split_window(&mut model, "t1", &claude_id, "shell");

    mgr.pane_exited(&mut model, &mut selection(), "t1", &claude_id, &claude_id);

    let window = window_in(&model, "t1", &claude_id);
    assert_eq!(
        window.kind,
        TermWindowKind::Terminal,
        "no Claude leaf, no Claude pill — the pill is just its shells now"
    );
    assert!(!window.is_claude_running);
    assert!(
        window.layout_is_valid(),
        "the kind flip is what keeps the invariant true"
    );
    assert!(
        !model.session_for("t1").unwrap().has_claude(),
        "the sidebar dot must stop tracking a Claude that is gone"
    );
}

#[test]
fn a_held_claude_leaf_keeps_the_pill_claude_and_alive() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal_id) = seed_claude_session(&mut model, "t1");
    model.mutate_session("t1", |session| {
        let window = session.windows.iter_mut().find(|w| w.id == claude_id).unwrap();
        window.is_claude_running = true;
        window.status = SessionStatus::Thinking;
    });
    split_window(&mut model, "t1", &claude_id, "shell");

    mgr.pane_held(&mut model, "t1", &claude_id, &claude_id);

    let window = window_in(&model, "t1", &claude_id);
    assert_eq!(window.kind, TermWindowKind::Claude, "a held leaf is not removed");
    assert!(
        window.is_alive,
        "the shell pane beside it keeps the pill alive — this is the whole point of per-leaf holds"
    );
    assert!(!window.layout.pane(&claude_id).unwrap().is_alive);
    assert!(!window.is_claude_running, "a held pty is a corpse, not a live Claude");
    assert_eq!(window.status, SessionStatus::Idle);
    assert!(
        !model.session_for("t1").unwrap().has_claude(),
        "a dead Claude stops counting the moment it dies, not when the pill closes"
    );
}

#[test]
fn holding_the_last_pane_still_kills_the_pill() {
    // The pre-splits behavior, unchanged for every never-split pill.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal_id) = seed_claude_session(&mut model, "t1");

    mgr.pane_held(&mut model, "t1", &claude_id, &claude_id);

    let window = window_in(&model, "t1", &claude_id);
    assert!(!window.is_alive);
    assert!(!window.any_pane_alive());
}

#[test]
fn a_shell_pane_holding_leaves_a_running_claude_alone() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal_id) = seed_claude_session(&mut model, "t1");
    model.mutate_session("t1", |session| {
        let window = session.windows.iter_mut().find(|w| w.id == claude_id).unwrap();
        window.is_claude_running = true;
    });
    split_window(&mut model, "t1", &claude_id, "shell");

    mgr.pane_held(&mut model, "t1", &claude_id, "shell");

    let window = window_in(&model, "t1", &claude_id);
    assert!(window.is_alive);
    assert!(
        window.is_claude_running,
        "the shell died, not Claude — the promotion guard must not be cleared"
    );
    assert!(model.session_for("t1").unwrap().has_claude());
}

#[test]
fn pill_status_is_the_or_across_its_panes() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal_id) = seed_claude_session(&mut model, "t1");
    model.mutate_session("t1", |session| {
        session
            .windows
            .iter_mut()
            .find(|w| w.id == claude_id)
            .unwrap()
            .is_claude_running = true;
    });
    split_window(&mut model, "t1", &claude_id, "shell");

    // Claude thinking, shell silent ⇒ the pill is thinking.
    mgr.pane_title_changed(&mut model, "t1", &claude_id, &claude_id, "\u{2840} fix-bug");
    assert_eq!(window_in(&model, "t1", &claude_id).status, SessionStatus::Thinking);

    // A `claude` the user started by hand in the SHELL pane goes waiting; Claude
    // stays thinking, so the OR keeps the pill thinking.
    mgr.pane_title_changed(&mut model, "t1", &claude_id, "shell", "\u{2733} needs-input");
    assert_eq!(
        window_in(&model, "t1", &claude_id).status,
        SessionStatus::Thinking,
        "thinking beats waiting"
    );

    // Claude's pane dies. Its contribution drops out, but the shell's hand-run
    // claude is still waiting, so the pill must stay lit — this is what the OR
    // buys over a single window-level status.
    mgr.pane_held(&mut model, "t1", &claude_id, &claude_id);
    assert_eq!(
        window_in(&model, "t1", &claude_id).status,
        SessionStatus::Waiting,
        "one pane falling silent can't un-light a pill whose other pane still wants attention"
    );

    // The last lit pane goes quiet ⇒ the pill goes dark.
    mgr.pane_held(&mut model, "t1", &claude_id, "shell");
    assert_eq!(window_in(&model, "t1", &claude_id).status, SessionStatus::Idle);
}

#[test]
fn a_shell_panes_hand_run_claude_un_lights_the_pill_when_it_exits() {
    // The clearing half of P9's OR. A shell pane has no exit signal of its own —
    // the hand-run `claude` quits and the shell just goes back to printing its
    // own title — so a title that parses to NO status has to mean "idle", or the
    // pill stays lit over an idle prompt forever.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal_id) = seed_claude_session(&mut model, "t1");
    split_window(&mut model, "t1", &claude_id, "shell");

    mgr.pane_title_changed(&mut model, "t1", &claude_id, "shell", "\u{2733} needs-input");
    assert_eq!(
        window_in(&model, "t1", &claude_id).status,
        SessionStatus::Waiting,
        "a hand-run claude in a shell pane lights the pill (P9's OR)"
    );

    // `claude` exits; the shell's next OSC title is its own again.
    mgr.pane_title_changed(&mut model, "t1", &claude_id, "shell", "zsh");
    assert_eq!(
        window_in(&model, "t1", &claude_id).status,
        SessionStatus::Idle,
        "a spinner-less title must retire the pane's recorded status"
    );

    // And it can light up again on the next hand-run claude — the entry was
    // cleared, not poisoned.
    mgr.pane_title_changed(&mut model, "t1", &claude_id, "shell", "\u{2840} fix-bug");
    assert_eq!(
        window_in(&model, "t1", &claude_id).status,
        SessionStatus::Thinking
    );
}

#[test]
fn a_shell_panes_plain_title_leaves_the_claude_leaf_lit() {
    // The clearing rule is per-PANE: a shell pane going quiet must not reach
    // across and idle the Claude leaf, whose status the control socket owns.
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal_id) = seed_claude_session(&mut model, "t1");
    model.mutate_session("t1", |session| {
        session
            .windows
            .iter_mut()
            .find(|w| w.id == claude_id)
            .unwrap()
            .is_claude_running = true;
    });
    split_window(&mut model, "t1", &claude_id, "shell");

    mgr.pane_title_changed(&mut model, "t1", &claude_id, &claude_id, "\u{2840} fix-bug");
    mgr.pane_title_changed(&mut model, "t1", &claude_id, "shell", "zsh");

    assert_eq!(
        window_in(&model, "t1", &claude_id).status,
        SessionStatus::Thinking,
        "the shell pane's silence is its own, not the Claude pane's"
    );
}

#[test]
fn a_never_split_terminal_pill_still_has_no_status() {
    // The spinner parse is a SPLIT-pill affordance; a lone terminal window's
    // status stays as meaningless as it has always been.
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_terminal_session(&mut model, "t1", "p1", "/tmp/anchor");

    mgr.pane_title_changed(&mut model, "t1", "p1", "p1", "\u{2840} fix-bug");

    assert_eq!(window_in(&model, "t1", "p1").status, SessionStatus::Idle);
}

#[test]
fn a_shell_pane_title_never_reaches_the_claude_branch() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal_id) = seed_claude_session(&mut model, "t1");
    model.mutate_session("t1", |session| {
        session
            .windows
            .iter_mut()
            .find(|w| w.id == claude_id)
            .unwrap()
            .is_claude_running = true;
    });
    split_window(&mut model, "t1", &claude_id, "shell");
    let title_before = model.session_for("t1").unwrap().title.clone();

    mgr.pane_title_changed(&mut model, "t1", &claude_id, "shell", "\u{2840} nvim foo.rb");

    assert_eq!(
        model.session_for("t1").unwrap().title,
        title_before,
        "only the Claude leaf's label feeds the session auto-title"
    );
}

#[test]
fn only_the_focused_pane_writes_the_pill_label() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_terminal_session(&mut model, "t1", "p1", "/tmp/anchor");
    split_window(&mut model, "t1", "p1", "shell"); // focus follows the new pane

    let changed = mgr.pane_title_changed(&mut model, "t1", "p1", "p1", "nvim foo.rb");
    assert!(!changed, "a background pane must not rename what the user is reading");
    assert_eq!(window_in(&model, "t1", "p1").title, "zsh");

    assert!(mgr.pane_title_changed(&mut model, "t1", "p1", "shell", "htop"));
    assert_eq!(window_in(&model, "t1", "p1").title, "htop");
}

#[test]
fn osc_cwd_lands_on_the_emitting_pane_and_the_pill_follows_focus() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    seed_terminal_session(&mut model, "t1", "p1", "/tmp/anchor");
    split_window(&mut model, "t1", "p1", "shell");

    mgr.pane_cwd_changed(&mut model, "t1", "p1", "p1", "/tmp/background");
    mgr.pane_cwd_changed(&mut model, "t1", "p1", "shell", "/tmp/focused");

    let window = window_in(&model, "t1", "p1");
    assert_eq!(window.layout.pane("p1").unwrap().cwd.as_deref(), Some("/tmp/background"));
    assert_eq!(window.layout.pane("shell").unwrap().cwd.as_deref(), Some("/tmp/focused"));
    assert_eq!(
        window.cwd.as_deref(),
        Some("/tmp/focused"),
        "the pill's cwd tracks the pane the user is looking at"
    );
    assert_eq!(model.session_for("t1").unwrap().cwd, "/tmp/anchor", "Session.cwd stays anchored");
}

#[test]
fn break_pane_inserts_the_moved_pane_as_the_next_pill() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, terminal_id) = seed_claude_session(&mut model, "t1");
    split_window(&mut model, "t1", &claude_id, "shell");
    mgr.pane_cwd_changed(&mut model, "t1", &claude_id, "shell", "/tmp/work");

    let new_id = mgr
        .move_pane_to_new_window(&mut model, "t1", &claude_id, "shell")
        .expect("a shell pane in a split pill can break out");

    let session = model.session_for("t1").unwrap();
    assert_eq!(
        session.windows.iter().map(|w| w.id.clone()).collect::<Vec<_>>(),
        vec![claude_id.clone(), new_id.clone(), terminal_id],
        "the new pill lands right after the one it came from"
    );
    let source = window_in(&model, "t1", &claude_id);
    assert_eq!(source.layout.leaf_count(), 1);
    assert_eq!(source.active_pane_id, claude_id, "focus fell back into the source pill");

    let moved = window_in(&model, "t1", &new_id);
    assert_eq!(moved.kind, TermWindowKind::Terminal);
    assert_eq!(moved.title, "Terminal 2", "the new pill takes the next auto-name slot");
    assert_eq!(moved.cwd.as_deref(), Some("/tmp/work"));
    assert_eq!(
        moved.active_pane_id, "shell",
        "the pane keeps its id, which is what lets its live pty re-key without respawning"
    );
    assert!(moved.layout_is_valid());
}

#[test]
fn break_pane_refuses_the_claude_pane_and_an_unsplit_pill() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal_id) = seed_claude_session(&mut model, "t1");

    assert!(
        mgr.move_pane_to_new_window(&mut model, "t1", &claude_id, &claude_id)
            .is_none(),
        "breaking out a pill's only pane is just a rename"
    );

    split_window(&mut model, "t1", &claude_id, "shell");
    assert!(
        mgr.move_pane_to_new_window(&mut model, "t1", &claude_id, &claude_id)
            .is_none(),
        "a Claude leaf must never become a pill through this path (P3)"
    );
    assert_eq!(model.session_for("t1").unwrap().windows.len(), 2);
}

// The pane-keyed cores guard against a stale pane id (a direct caller passing a
// pane the tree no longer holds). The live subscription path resolves the owning
// window from the pane first, so these guards are only reachable by such callers
// — pin them as no-ops the way the window-level twins are pinned.

#[test]
fn pane_title_changed_unknown_pane_is_noop() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal_id) = seed_claude_session(&mut model, "t1");
    split_window(&mut model, "t1", &claude_id, "shell");

    let before = window_in(&model, "t1", &claude_id).clone();
    let changed = mgr.pane_title_changed(&mut model, "t1", &claude_id, "ghost-pane", "\u{2840} fix-bug");
    assert!(!changed, "a stale pane id must not report a change");
    assert_eq!(
        window_in(&model, "t1", &claude_id),
        &before,
        "neither the pill title/status nor any pane may move"
    );
}

#[test]
fn pane_cwd_changed_unknown_pane_is_noop() {
    let mut mgr = counting_manager();
    let mut model = seeded();
    let (claude_id, _terminal_id) = seed_claude_session(&mut model, "t1");
    split_window(&mut model, "t1", &claude_id, "shell");

    let before = window_in(&model, "t1", &claude_id).clone();
    let changed = mgr.pane_cwd_changed(&mut model, "t1", &claude_id, "ghost-pane", "/tmp/ghost");
    assert!(!changed, "a stale pane id must not report a change");
    assert_eq!(
        window_in(&model, "t1", &claude_id),
        &before,
        "neither the pill cwd nor any pane may move"
    );
}
