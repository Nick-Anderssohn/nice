//! Slice-3 tests for adoption: the round trip that moves a LIVE pty back into a
//! window without respawning it, the project re-homing rules, and the guards
//! (duplicate id, an id no longer pooled) that must each be a silent no-op.
//!
//! These drive the real primitives — `WindowState::detach_session` out,
//! `WindowState::adopt_entry` in, with the pool between them — rather than
//! hand-built entries, so a regression in either half fails here.

use super::*;

use gpui::{AppContext, Entity, TestAppContext};
use nice_model::{Session, TermWindow, TermWindowKind, WorkspaceModel};
use nice_term_core::SpawnSpec;

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

/// A window whose model carries the seeded Terminals/Main plus one session in a
/// project at `project_path` (id `project_id`).
fn window_with_session(
    cx: &mut TestAppContext,
    project_id: &str,
    project_path: &str,
    session_id: &str,
) -> Entity<WindowState> {
    let mut model = WorkspaceModel::new("/home/u");
    let pi = model.ensure_project(project_id, "Work", project_path);
    let mut session = Session::new(session_id, "Work", project_path);
    let window = TermWindow::new(
        format!("{session_id}-w"),
        "Terminal 1",
        TermWindowKind::Terminal,
    );
    session.active_window_id = Some(window.id.clone());
    session.windows = vec![window];
    model.projects[pi].sessions.push(session);
    model.select_session(session_id);
    cx.new(|_cx| WindowState::with_model(model))
}

/// A window with nothing but the seeded Terminals/Main tree.
fn plain_window(cx: &mut TestAppContext) -> Entity<WindowState> {
    cx.new(|_cx| WindowState::with_model(WorkspaceModel::new("/home/u")))
}

fn install_pool(cx: &mut TestAppContext) -> Entity<DetachedPool> {
    cx.update(|app| {
        let pool = app.new(|_cx| DetachedPool::new());
        app.set_global(DetachedPoolGlobal(pool.clone()));
        pool
    })
}

/// Spawn the fixture child into `session_id`'s sole pill and hand back its pane
/// handle — the identity every "no respawn" assertion is made against.
fn spawn_live_pane(
    cx: &mut TestAppContext,
    state: &Entity<WindowState>,
    session_id: &str,
) -> Entity<nice_term_view::TerminalSessionHandle> {
    let term_window_id = format!("{session_id}-w");
    state.update(cx, |ws, wcx| {
        ws.ptys.set_event_wakes_enabled_for_test(false);
        ws.ptys
            .spawn_window(session_id, &term_window_id, fixture_spec(), wcx)
            .expect("the fixture child spawns");
        ws.ptys
            .pane_handle(session_id, &term_window_id, &term_window_id)
            .expect("the spawned pane has a handle")
    })
}

fn pooled_ids(cx: &mut TestAppContext, pool: &Entity<DetachedPool>) -> Vec<String> {
    pool.read_with(cx, |pool, _| {
        pool.entries().iter().map(|e| e.session.id.clone()).collect()
    })
}

/// Detach `session_id` out of `state` and hand back the id its pool row carries.
/// A session is RE-KEYED as it leaves a window (`WindowState::detach_session`):
/// the pool is app-global while a `WorkspaceModel` only keeps ids unique inside
/// one window, so no caller may keep using the in-window id afterwards.
fn detach_and_pooled_id(
    cx: &mut TestAppContext,
    state: &Entity<WindowState>,
    session_id: &str,
) -> String {
    cx.update(|app| {
        detach_into_pool(app, state, session_id).expect("the window owns that session");
        pool(app)
            .expect("a pool is installed")
            .read(app)
            .entries()
            .first()
            .expect("the detached row is on the head")
            .session
            .id
            .clone()
    })
}

// ---- the round trip --------------------------------------------------------

/// The load-bearing property of adoption: the SAME
/// `Entity<TerminalSessionHandle>` lands in the adopting window, so the child is
/// never respawned and its scrollback is the scrollback the user left. Pinned
/// across two different windows, which is the shipped shape (detach here, adopt
/// there).
#[gpui::test]
fn adopt_moves_the_live_handle_into_another_window(cx: &mut TestAppContext) {
    let pool = install_pool(cx);
    let source = window_with_session(cx, "p", "/home/u/proj", "t1");
    let target = plain_window(cx);
    let handle = spawn_live_pane(cx, &source, "t1");

    let detached = detach_and_pooled_id(cx, &source, "t1");
    assert_eq!(pooled_ids(cx, &pool), vec![detached.clone()]);
    cx.update(|app| {
        assert!(
            handle.read(app).session().try_status().is_none(),
            "the child is still running while pooled"
        );
        assert!(adopt_into(app, &target, &detached), "the target adopts it");
    });

    assert!(pooled_ids(cx, &pool).is_empty(), "the pool gave the row up");
    target.update(cx, |ws, _cx| {
        assert!(
            ws.workspace.session_for(&detached).is_some(),
            "the model gained it"
        );
        assert_eq!(
            ws.workspace.active_session_id(),
            Some(detached.as_str()),
            "adoption selects what it just adopted"
        );
        // The pill and pane ids are untouched by the re-key — only the session's
        // own id changes, so the payload keys straight back in.
        let adopted = ws
            .ptys
            .pane_handle(&detached, "t1-w", "t1-w")
            .expect("the adopting window owns the pane now");
        assert_eq!(
            adopted.entity_id(),
            handle.entity_id(),
            "the SAME handle — nothing respawned"
        );
        ws.ptys.teardown();
    });
    source.update(cx, |ws, _cx| ws.ptys.teardown());
}

/// A structural entry (an empty payload — the post-relaunch rows and §P2's
/// never-activated sessions) adopts as a model row with no pty, which is exactly
/// the state the lazy spawn on activation exists for. Its `claude_session_id`
/// survives, so the resume machinery has what it needs when that spawn happens.
#[gpui::test]
fn structural_adopt_lands_ptyless_with_its_resume_id(cx: &mut TestAppContext) {
    let pool = install_pool(cx);
    let target = plain_window(cx);

    let mut session = Session::new("t1", "Resumable", "/home/u/proj");
    session.claude_session_id = Some("e4f1a2b3-c0d4-4e5f-9a0b-1c2d3e4f5a6b".into());
    let window = TermWindow::new("t1-w", "Claude", TermWindowKind::Claude);
    session.active_window_id = Some(window.id.clone());
    session.windows = vec![window];
    let entry = DetachedEntry::structural(
        session,
        DetachedProject {
            id: "p".into(),
            name: "Work".into(),
            path: "/home/u/proj".into(),
        },
    );
    pool.update(cx, |pool, pcx| pool.push_front(entry, pcx));

    cx.update(|app| assert!(adopt_into(app, &target, "t1")));
    target.update(cx, |ws, _cx| {
        let session = ws.workspace.session_for("t1").expect("adopted");
        assert_eq!(
            session.claude_session_id.as_deref(),
            Some("e4f1a2b3-c0d4-4e5f-9a0b-1c2d3e4f5a6b"),
            "the resume id rides along"
        );
        assert!(
            session.windows.iter().all(|w| w.is_alive),
            "the pill is model-alive, so activation will spawn it"
        );
        assert!(
            !ws.ptys.has_window("t1", "t1-w"),
            "nothing spawned at adopt time — the respawn is the activation path's"
        );
        assert!(
            ws.ptys.session_has_pty("t1"),
            "the container IS registered, or the lazy spawn would refuse"
        );
    });
}

// ---- project re-homing -----------------------------------------------------

/// An existing group at the SAME path is reused rather than duplicated, even
/// though the adopting window minted its own project id for it.
#[gpui::test]
fn adopt_re_homes_into_an_existing_project_at_the_same_path(cx: &mut TestAppContext) {
    let _pool = install_pool(cx);
    let source = window_with_session(cx, "source-p", "/home/u/proj", "t1");
    let target = window_with_session(cx, "target-p", "/home/u/proj", "t2");
    let before = target.read_with(cx, |ws, _| ws.workspace.projects.len());

    let detached = detach_and_pooled_id(cx, &source, "t1");
    cx.update(|app| assert!(adopt_into(app, &target, &detached)));

    target.update(cx, |ws, _cx| {
        assert_eq!(
            ws.workspace.projects.len(),
            before,
            "the path matched, so no second project row appeared"
        );
        let (pi, _) = ws.workspace.project_session_index(&detached).unwrap();
        assert_eq!(
            ws.workspace.projects[pi].id, "target-p",
            "it joined the target's OWN group for that path"
        );
    });
}

/// No group at that path ⇒ one is created, carrying the provenance verbatim.
#[gpui::test]
fn adopt_creates_the_project_when_the_target_has_none(cx: &mut TestAppContext) {
    let _pool = install_pool(cx);
    let source = window_with_session(cx, "p", "/home/u/proj", "t1");
    let target = plain_window(cx);

    let detached = detach_and_pooled_id(cx, &source, "t1");
    cx.update(|app| assert!(adopt_into(app, &target, &detached)));

    target.update(cx, |ws, _cx| {
        let (pi, _) = ws.workspace.project_session_index(&detached).unwrap();
        let project = &ws.workspace.projects[pi];
        assert_eq!(project.id, "p");
        assert_eq!(project.name, "Work");
        assert_eq!(project.path, "/home/u/proj");
    });
}

/// A Terminals-provenance session lands in the ADOPTING window's own pinned
/// Terminals group — never a second group carrying the reserved id.
#[gpui::test]
fn adopt_puts_a_terminals_session_in_the_targets_terminals_group(cx: &mut TestAppContext) {
    let _pool = install_pool(cx);
    let source = plain_window(cx);
    let target = plain_window(cx);
    // Mint a second Terminals session in the source and detach THAT (Main stays
    // put, so the source keeps a session and the ids cannot collide).
    source.update(cx, |ws, _cx| {
        let mut session = Session::new("term-2", "Terminal 2", "/home/u");
        let window = TermWindow::new("term-2-w", "Terminal 1", TermWindowKind::Terminal);
        session.active_window_id = Some(window.id.clone());
        session.windows = vec![window];
        ws.workspace.projects[0].sessions.push(session);
    });

    let detached = detach_and_pooled_id(cx, &source, "term-2");
    cx.update(|app| assert!(adopt_into(app, &target, &detached)));

    target.update(cx, |ws, _cx| {
        let terminals = ws
            .workspace
            .projects
            .iter()
            .filter(|p| p.id == WorkspaceModel::TERMINALS_PROJECT_ID)
            .count();
        assert_eq!(terminals, 1, "exactly one Terminals group, as always");
        assert!(
            ws.workspace.is_terminals_project_session(&detached),
            "the adopted row joined it"
        );
    });
}

/// The shape the live gate found broken: every fresh window seeds the SAME
/// `terminals-main`, so a pooled Main used to collide with the seeded Main of
/// whatever window adopted it — `adopt_entry`'s duplicate-id guard refused, the
/// sidebar click did nothing at all, and the row sat in the pool forever. Detach
/// re-keys, so both sessions coexist.
#[gpui::test]
fn a_pooled_main_adopts_into_a_window_that_seeded_its_own(cx: &mut TestAppContext) {
    let pool = install_pool(cx);
    let source = plain_window(cx);
    let target = plain_window(cx);
    let main = WorkspaceModel::MAIN_TERMINAL_SESSION_ID;

    let detached = detach_and_pooled_id(cx, &source, main);
    assert_ne!(
        detached, main,
        "the pooled row must not still carry the constant every window seeds"
    );
    cx.update(|app| {
        assert!(
            adopt_into(app, &target, &detached),
            "the adopt must not be refused by the duplicate-id guard"
        );
    });

    assert!(pooled_ids(cx, &pool).is_empty());
    target.update(cx, |ws, _cx| {
        assert!(
            ws.workspace.session_for(main).is_some(),
            "the window's OWN Main is untouched"
        );
        assert!(
            ws.workspace.session_for(&detached).is_some(),
            "and the adopted one sits beside it"
        );
        assert_eq!(
            ws.workspace.projects[0].sessions.len(),
            2,
            "both live in the one pinned Terminals group"
        );
    });
}

/// Two windows detaching their seeded Main put TWO rows in the pool, not one
/// shadowing the other — the shape whose collision made the launch reconcile
/// collapse a real detached session out of `sessions.json`.
#[gpui::test]
fn two_windows_detaching_their_main_make_two_distinct_rows(cx: &mut TestAppContext) {
    let pool = install_pool(cx);
    let first = plain_window(cx);
    let second = plain_window(cx);
    let main = WorkspaceModel::MAIN_TERMINAL_SESSION_ID;

    let a = detach_and_pooled_id(cx, &first, main);
    let b = detach_and_pooled_id(cx, &second, main);

    assert_ne!(a, b, "each detached row carries its own id");
    let ids = pooled_ids(cx, &pool);
    assert_eq!(ids.len(), 2, "both rows are on the pool: {ids:?}");
    assert_eq!(ids[0], b, "most recently detached first");
}

/// A `/branch` child whose parent did not come along loses its parent pointer
/// (review N7) — otherwise the sidebar indents it under a session that does not
/// exist in this window.
#[gpui::test]
fn adopt_clears_a_parent_reference_the_target_cannot_resolve(cx: &mut TestAppContext) {
    let pool = install_pool(cx);
    let target = plain_window(cx);

    let mut child = Session::new("child", "Branch", "/home/u/proj");
    child.parent_session_id = Some("absent-parent".into());
    let entry = DetachedEntry::structural(
        child,
        DetachedProject {
            id: "p".into(),
            name: "Work".into(),
            path: "/home/u/proj".into(),
        },
    );
    pool.update(cx, |pool, pcx| pool.push_front(entry, pcx));

    cx.update(|app| assert!(adopt_into(app, &target, "child")));
    target.update(cx, |ws, _cx| {
        assert_eq!(
            ws.workspace.session_for("child").unwrap().parent_session_id,
            None,
            "the dangling lineage link is dropped"
        );
    });
}

/// The parent link SURVIVES when the parent is in the adopting window — the
/// clearing is targeted, not a blanket flatten.
#[gpui::test]
fn adopt_keeps_a_parent_reference_the_target_can_resolve(cx: &mut TestAppContext) {
    let pool = install_pool(cx);
    let target = window_with_session(cx, "p", "/home/u/proj", "parent");

    let mut child = Session::new("child", "Branch", "/home/u/proj");
    child.parent_session_id = Some("parent".into());
    let entry = DetachedEntry::structural(
        child,
        DetachedProject {
            id: "p".into(),
            name: "Work".into(),
            path: "/home/u/proj".into(),
        },
    );
    pool.update(cx, |pool, pcx| pool.push_front(entry, pcx));

    cx.update(|app| assert!(adopt_into(app, &target, "child")));
    target.update(cx, |ws, _cx| {
        assert_eq!(
            ws.workspace
                .session_for("child")
                .unwrap()
                .parent_session_id
                .as_deref(),
            Some("parent")
        );
    });
}

// ---- the guards ------------------------------------------------------------

/// The same row clicked in two windows across a notify gap: the second take
/// finds nothing, and that must be a silent no-op rather than a double adopt
/// (review N-r2-6c).
#[gpui::test]
fn adopting_an_id_that_is_no_longer_pooled_is_a_silent_no_op(cx: &mut TestAppContext) {
    let pool = install_pool(cx);
    let source = window_with_session(cx, "p", "/home/u/proj", "t1");
    let first = plain_window(cx);
    let second = plain_window(cx);

    let detached = detach_and_pooled_id(cx, &source, "t1");
    cx.update(|app| {
        assert!(adopt_into(app, &first, &detached), "the first click wins");
        assert!(
            !adopt_into(app, &second, &detached),
            "the second finds nothing to take"
        );
    });

    assert!(pooled_ids(cx, &pool).is_empty());
    second.update(cx, |ws, _cx| {
        assert!(
            ws.workspace.session_for(&detached).is_none(),
            "the losing window gained nothing"
        );
    });
}

/// B3c: a window that ALREADY owns that session id refuses, and the refusal
/// leaves the row in the pool — never dropped, which would SIGHUP a live child
/// over a belt-and-braces check.
#[gpui::test]
fn adopting_a_duplicate_id_refuses_and_keeps_the_row_pooled(cx: &mut TestAppContext) {
    let pool = install_pool(cx);
    let target = window_with_session(cx, "p", "/home/u/proj", "t1");

    // A pooled entry sharing the target's session id (the crash-window shape the
    // launch reconcile also guards).
    let entry = DetachedEntry::structural(
        Session::new("t1", "Impostor", "/home/u/proj"),
        DetachedProject {
            id: "p".into(),
            name: "Work".into(),
            path: "/home/u/proj".into(),
        },
    );
    pool.update(cx, |pool, pcx| pool.push_front(entry, pcx));

    cx.update(|app| assert!(!adopt_into(app, &target, "t1"), "refused"));
    assert_eq!(
        pooled_ids(cx, &pool),
        vec!["t1".to_string()],
        "the entry stayed in the pool"
    );
    target.update(cx, |ws, _cx| {
        assert_eq!(
            ws.workspace.session_for("t1").unwrap().title,
            "Work",
            "the window's own session is untouched"
        );
    });
}

/// The direct `adopt_entry` refusal hands the entry BACK, so a caller that is not
/// the pool driver can also avoid dropping a live child.
#[gpui::test]
fn adopt_entry_hands_a_refused_entry_back(cx: &mut TestAppContext) {
    let target = window_with_session(cx, "p", "/home/u/proj", "t1");
    let entry = DetachedEntry::structural(
        Session::new("t1", "Impostor", "/home/u/proj"),
        DetachedProject {
            id: "p".into(),
            name: "Work".into(),
            path: "/home/u/proj".into(),
        },
    );
    target.update(cx, |ws, wcx| {
        let back = ws.adopt_entry(entry, wcx).expect_err("duplicate id refused");
        assert_eq!(back.session.title, "Impostor", "the entry came back whole");
    });
}

/// ⌃⌘A's driver pops the HEAD — the most recently detached row.
#[gpui::test]
fn adopt_head_takes_the_most_recent_row(cx: &mut TestAppContext) {
    let pool = install_pool(cx);
    let target = plain_window(cx);
    pool.update(cx, |pool, pcx| {
        pool.push_front(
            DetachedEntry::structural(
                Session::new("older", "Older", "/home/u/proj"),
                DetachedProject {
                    id: "p".into(),
                    name: "Work".into(),
                    path: "/home/u/proj".into(),
                },
            ),
            pcx,
        );
        pool.push_front(
            DetachedEntry::structural(
                Session::new("newer", "Newer", "/home/u/proj"),
                DetachedProject {
                    id: "p".into(),
                    name: "Work".into(),
                    path: "/home/u/proj".into(),
                },
            ),
            pcx,
        );
    });

    cx.update(|app| assert!(adopt_head_into(app, &target)));
    assert_eq!(pooled_ids(cx, &pool), vec!["older".to_string()]);
    target.update(cx, |ws, _cx| {
        assert!(ws.workspace.session_for("newer").is_some());
        assert_eq!(ws.workspace.active_session_id(), Some("newer"));
    });
}

/// Every driver is inert with no pool installed — the hermeticity rule the
/// scenarios and `run_selftest` run under.
#[gpui::test]
fn every_driver_is_inert_without_a_pool(cx: &mut TestAppContext) {
    let target = plain_window(cx);
    cx.update(|app| {
        assert!(!adopt_into(app, &target, "whatever"));
        assert!(!adopt_head_into(app, &target));
        assert!(!kill_pooled(app, "whatever"));
        assert!(!adopt_into_new_window(app, "whatever"));
        assert!(pool_rows(app).is_empty());
    });
}

/// Kill drops the row (and its payload, which SIGHUPs the child) and reports
/// whether it found one.
#[gpui::test]
fn kill_pooled_drops_the_row(cx: &mut TestAppContext) {
    let pool = install_pool(cx);
    pool.update(cx, |pool, pcx| {
        pool.push_front(
            DetachedEntry::structural(
                Session::new("t1", "Work", "/home/u/proj"),
                DetachedProject {
                    id: "p".into(),
                    name: "Work".into(),
                    path: "/home/u/proj".into(),
                },
            ),
            pcx,
        );
    });

    cx.update(|app| {
        assert!(kill_pooled(app, "t1"));
        assert!(!kill_pooled(app, "t1"), "a second kill finds nothing");
    });
    assert!(pooled_ids(cx, &pool).is_empty());
}

/// `pool_rows` is what the sidebar paints: pool order, and the Claude flag the
/// row's glyph keys off.
#[gpui::test]
fn pool_rows_render_in_pool_order_with_the_claude_flag(cx: &mut TestAppContext) {
    let pool = install_pool(cx);
    pool.update(cx, |pool, pcx| {
        let shell = DetachedEntry::structural(
            Session::new("shell", "Shell", "/home/u"),
            DetachedProject {
                id: "p".into(),
                name: "Work".into(),
                path: "/home/u".into(),
            },
        );
        let mut claude_session = Session::new("claude", "Claude", "/home/u");
        claude_session.windows = vec![TermWindow::new("cw", "Claude", TermWindowKind::Claude)];
        let claude = DetachedEntry::structural(
            claude_session,
            DetachedProject {
                id: "p".into(),
                name: "Work".into(),
                path: "/home/u".into(),
            },
        );
        pool.push_front(shell, pcx);
        pool.push_front(claude, pcx);
    });

    cx.update(|app| {
        let rows = pool_rows(app);
        assert_eq!(
            rows,
            vec![
                ("claude".to_string(), "Claude".to_string(), true),
                ("shell".to_string(), "Shell".to_string(), false),
            ]
        );
    });
}
