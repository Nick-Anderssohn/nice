//! Slice-4 tests for tear-off: the window-level extraction that turns a focused
//! pane into a synthetic [`DetachedEntry`], both §P9 branches, and the hand-off
//! into a receiving window.
//!
//! They live beside the adoption tests because tear-off is adoption's other door:
//! the entry these mint is the entry [`WindowState::adopt_entry`] lands, so the
//! round trip here is the same one the real construction path
//! ([`crate::app::open_managed_window_adopting`]) performs — minus `open_window`
//! itself, which needs a real platform window and is covered by the live
//! scenario.

use super::*;

use gpui::{AppContext, Entity, TestAppContext};
use nice_model::{Pane, Project, Session, SplitOrient, TermWindow, TermWindowKind, WorkspaceModel};
use nice_term_core::SpawnSpec;

use crate::pty_manager::DissolveTerminus;

// ---- fixtures --------------------------------------------------------------

/// A cheap hermetic child: no shell rc, no user config, just a process that sits
/// on its pty until its `PaneState` is dropped.
fn fixture_spec() -> SpawnSpec {
    SpawnSpec::shell(std::env::temp_dir().to_string_lossy().to_string()).with_argv(vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "sleep 30".to_string(),
    ])
}

/// One session (`t1`, pill `t1-w`) in project `p`, in a SEEDED window — so the
/// window holds no Main session and tearing `t1`'s last pill out really does empty
/// it (the `WindowEmptied` terminus).
fn window_with_one_session(cx: &mut TestAppContext) -> Entity<WindowState> {
    let mut session = Session::new("t1", "Work", "/home/u/proj");
    let window = TermWindow::new("t1-w", "Terminal 1", TermWindowKind::Terminal);
    session.active_window_id = Some(window.id.clone());
    session.windows = vec![window];
    session.next_terminal_index = 2;
    let seed = crate::restore::WindowSeed {
        window_id: "window-1".to_string(),
        projects: vec![Project {
            id: "p".to_string(),
            name: "Work".to_string(),
            path: "/home/u/proj".to_string(),
            sessions: vec![session],
        }],
        active_session_id: Some("t1".to_string()),
        sidebar_collapsed: false,
        sidebar_mode: None,
        sidebar_width: None,
        frame: None,
    };
    cx.new(|_cx| WindowState::with_seed(seed))
}

/// A window with nothing but the seeded Terminals/Main tree — the receiving side.
fn plain_window(cx: &mut TestAppContext) -> Entity<WindowState> {
    cx.new(|_cx| WindowState::with_model(WorkspaceModel::new("/home/u")))
}

/// Split `t1-w` so it holds a second leaf `shell`, focused — the multi-leaf shape.
fn split_the_pill(cx: &mut TestAppContext, state: &Entity<WindowState>) {
    state.update(cx, |ws, _cx| {
        ws.workspace.mutate_session("t1", |session| {
            let window = session.windows.iter_mut().find(|w| w.id == "t1-w").unwrap();
            let target = window.active_pane_id.clone();
            assert!(window.layout.split(
                &target,
                SplitOrient::Beside,
                Pane::new("shell", TermWindowKind::Terminal),
            ));
            window.active_pane_id = "shell".to_string();
        });
    });
}

// ---- the multi-leaf branch -------------------------------------------------

/// A split pill's pane leaves the WINDOW, not just the pill: the source pill
/// survives healed, and the pane comes back as a synthetic session carrying its
/// LIVE handle — which the receiving window then adopts without a respawn.
#[gpui::test]
fn tearing_off_a_split_pane_hands_the_live_handle_to_another_window(cx: &mut TestAppContext) {
    let source = window_with_one_session(cx);
    let target = plain_window(cx);
    split_the_pill(cx, &source);
    let handle = source.update(cx, |ws, wcx| {
        ws.ptys.set_event_wakes_enabled_for_test(false);
        ws.ptys
            .spawn_pane("t1", "t1-w", "shell", fixture_spec(), wcx)
            .expect("the fixture child spawns");
        ws.ptys
            .pane_handle("t1", "t1-w", "shell")
            .expect("the spawned pane has a handle")
    });

    let (entry, terminus) = source
        .update(cx, |ws, wcx| ws.tear_off_pane("t1", "t1-w", "shell", wcx))
        .expect("a shell pane in a split pill tears off");
    assert_eq!(
        terminus,
        DissolveTerminus::None,
        "the source session still has its pill, so the window is not empty"
    );

    source.update(cx, |ws, _cx| {
        let session = ws.workspace.session_for("t1").expect("the source session survives");
        assert_eq!(session.windows.len(), 1, "no new pill in the source window");
        assert_eq!(session.windows[0].layout.leaf_count(), 1, "the pill healed");
        assert!(
            ws.ptys.pane_handle("t1", "t1-w", "shell").is_none(),
            "the source manager gave the pane's pty up"
        );
    });
    assert_ne!(entry.session.id, "t1", "the pane became a session of its own");
    assert_eq!(
        entry.project.id, "p",
        "the provenance is the source project, so the new window re-homes it there"
    );

    // The receiving side — the same primitive the construction path calls.
    let adopted_id = entry.session.id.clone();
    let pill_id = entry.session.windows[0].id.clone();
    target.update(cx, |ws, wcx| {
        assert!(
            ws.adopt_entry(entry, wcx).is_ok(),
            "a fresh window accepts it"
        );
        let moved = ws
            .ptys
            .pane_handle(&adopted_id, &pill_id, "shell")
            .expect("the new window owns the pane now");
        assert_eq!(
            moved.entity_id(),
            handle.entity_id(),
            "the SAME handle — the child never noticed the move"
        );
        assert_eq!(
            ws.workspace.active_session_id(),
            Some(adopted_id.as_str()),
            "and the new window opens on it"
        );
        ws.ptys.teardown();
    });
    source.update(cx, |ws, _cx| ws.ptys.teardown());
}

// ---- the single-leaf branch ------------------------------------------------

/// An unsplit pill cannot lose its last leaf (`PaneLayout::remove` refuses), so
/// the WHOLE pill moves — and when it was the session's last, the source session
/// dissolves and the now-empty window reports `WindowEmptied` for the caller to
/// close.
#[gpui::test]
fn tearing_off_an_unsplit_pill_dissolves_the_emptied_source_session(cx: &mut TestAppContext) {
    let source = window_with_one_session(cx);
    let handle = source.update(cx, |ws, wcx| {
        ws.ptys.set_event_wakes_enabled_for_test(false);
        ws.ptys
            .spawn_window("t1", "t1-w", fixture_spec(), wcx)
            .expect("the fixture child spawns");
        ws.ptys
            .pane_handle("t1", "t1-w", "t1-w")
            .expect("the spawned pane has a handle")
    });

    let (entry, terminus) = source
        .update(cx, |ws, wcx| ws.tear_off_pane("t1", "t1-w", "t1-w", wcx))
        .expect("the only pill of the only session tears off");

    assert_eq!(
        terminus,
        DissolveTerminus::WindowEmptied,
        "its last session dissolved, so the source window has nothing left"
    );
    source.update(cx, |ws, _cx| {
        assert!(
            ws.workspace.session_for("t1").is_none(),
            "the pill-less source session dissolved"
        );
        assert!(
            ws.user_initiated_close(),
            "and the emptied window's disk slot is marked for removal"
        );
    });
    assert_eq!(
        entry.session.windows[0].id, "t1-w",
        "the pill travelled whole, id and all"
    );
    cx.update(|app| {
        assert!(
            handle.read(app).session().try_status().is_none(),
            "the child is still running — a tear-off kills nothing"
        );
        assert!(entry.has_live(app), "and the entry knows it");
    });
    drop(entry);
}

// ---- refusals --------------------------------------------------------------

/// The scope guard: a Claude pane never becomes its own window through this door
/// (a Claude session moves whole through Detach ▸ Open in New Window). Refused
/// silently — exactly what break-pane does — and with nothing mutated.
#[gpui::test]
fn tearing_off_a_claude_pane_is_refused(cx: &mut TestAppContext) {
    let source = window_with_one_session(cx);
    source.update(cx, |ws, _cx| {
        ws.workspace.mutate_session("t1", |session| {
            let window = session.windows.iter_mut().find(|w| w.id == "t1-w").unwrap();
            window.kind = TermWindowKind::Claude;
            window.layout = nice_model::PaneLayout::single(Pane::new(
                "t1-w",
                TermWindowKind::Claude,
            ));
        });
    });

    source.update(cx, |ws, wcx| {
        assert!(
            ws.tear_off_pane("t1", "t1-w", "t1-w", wcx).is_none(),
            "the Claude pane is refused"
        );
        assert!(
            ws.tear_off_pane("nope", "nope", "nope", wcx).is_none(),
            "so is a session this window does not own"
        );
        assert!(
            ws.workspace.session_for("t1").is_some(),
            "a refusal mutates nothing"
        );
    });
}

// ---- what the post-open save persists (review N3) --------------------------

/// The seeded window's snapshot already carries the adopted session BEFORE any
/// live mutation — which is exactly why the construction path fires the
/// restore-style explicit save after `open_window`: the mutation observer only
/// catches what happens AFTER construction, so without that save a window that
/// crashed before its first mutation would persist nothing.
#[gpui::test]
fn an_adopted_window_snapshots_its_session_before_any_mutation(cx: &mut TestAppContext) {
    let source = window_with_one_session(cx);
    let (entry, _terminus) = source
        .update(cx, |ws, wcx| ws.tear_off_pane("t1", "t1-w", "t1-w", wcx))
        .expect("the pill tears off");
    let adopted_id = entry.session.id.clone();

    // The receiving window as the construction path builds it: seeded (so no eager
    // Main), entry landed before it opens.
    let target = cx.new(|_cx| {
        WindowState::with_seed(crate::restore::WindowSeed {
            window_id: "window-2".to_string(),
            projects: vec![Project {
                id: WorkspaceModel::TERMINALS_PROJECT_ID.to_string(),
                name: "Terminals".to_string(),
                path: "/home/u".to_string(),
                sessions: Vec::new(),
            }],
            active_session_id: None,
            sidebar_collapsed: false,
            sidebar_mode: None,
            sidebar_width: None,
            frame: None,
        })
    });
    target.update(cx, |ws, wcx| {
        assert!(
            ws.adopt_entry(entry, wcx).is_ok(),
            "a fresh window accepts it"
        );
        let snapshot = ws.persisted_snapshot();
        let ids: Vec<String> = snapshot
            .projects
            .iter()
            .flat_map(|p| p.sessions.iter())
            .map(|s| s.id.clone())
            .collect();
        assert_eq!(
            ids,
            vec![adopted_id.clone()],
            "the torn-off session is the window's whole content — and no eager Main beside it"
        );
        ws.ptys.teardown();
    });
}
