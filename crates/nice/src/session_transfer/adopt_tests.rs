//! Pool-free coverage of the kept transfer primitives, ported forward from the
//! deleted `detached_pool/adopt_tests.rs` (§I5). The pool is gone, so these drive
//! the round trip the same way the Move and tear-off verbs now do: extract out of
//! a source window with [`WindowState::detach_session`], land into a destination
//! with [`WindowState::adopt_entry`] — no app-global pool between them.
//!
//! Two things are pinned here: the project re-homing matrix (an existing group at
//! the same path is reused, a missing one is created, a Terminals-provenance
//! session lands in the destination's own pinned Terminals group) and the
//! `/branch` parent-ref rules (a link the destination cannot resolve is cleared;
//! one it can is kept). The duplicate-id refusal that hands a live entry BACK is
//! covered too — never dropped over a belt-and-braces check.

use gpui::{AppContext, Entity, TestAppContext};
use nice_model::{Session, TermWindow, TermWindowKind, WorkspaceModel};

use crate::session_transfer::{DetachedEntry, DetachedProject};
use crate::window_state::WindowState;

// ---- fixtures --------------------------------------------------------------

/// A window whose model is the seeded Terminals/Main tree plus one session
/// `session_id` in a project at `project_path` (id `project_id`).
fn window_with_session(
    cx: &mut TestAppContext,
    project_id: &str,
    project_path: &str,
    session_id: &str,
) -> Entity<WindowState> {
    let mut model = WorkspaceModel::new("/home/u");
    let pi = model.ensure_project(project_id, "Work", project_path);
    let mut session = Session::new(session_id, "Work", project_path);
    let pill = format!("{session_id}-w");
    session.windows = vec![TermWindow::new(&pill, "Terminal 1", TermWindowKind::Terminal)];
    session.active_window_id = Some(pill);
    model.projects[pi].sessions.push(session);
    model.select_session(session_id);
    cx.new(|_cx| WindowState::with_model(model))
}

/// A window with nothing but the seeded Terminals/Main tree — the receiving side.
fn plain_window(cx: &mut TestAppContext) -> Entity<WindowState> {
    cx.new(|_cx| WindowState::with_model(WorkspaceModel::new("/home/u")))
}

/// Detach `session_id` out of `source` and hand back the entry it minted — the
/// value a destination window adopts. A session is RE-KEYED as it leaves a window
/// (`detach_session`), so the returned entry's id is not `session_id`.
fn detach(
    cx: &mut TestAppContext,
    source: &Entity<WindowState>,
    session_id: &str,
) -> DetachedEntry {
    source
        .update(cx, |ws, wcx| ws.detach_session(session_id, wcx))
        .expect("the window owns that session")
        .0
}

/// A structural (ptyless) entry for provenance `project` carrying `session` — the
/// shape a never-activated / resumable session moves as. Built by hand since the
/// pool's `DetachedEntry::structural` constructor died with the pool.
fn structural_entry(session: Session, project: DetachedProject) -> DetachedEntry {
    DetachedEntry {
        session,
        ptys: crate::pty_manager::DetachedPtys::empty(),
        project,
    }
}

// ---- project re-homing -----------------------------------------------------

/// An existing group at the SAME path is reused rather than duplicated, even
/// though the adopting window minted its own project id for it.
#[gpui::test]
fn adopt_re_homes_into_an_existing_project_at_the_same_path(cx: &mut TestAppContext) {
    let source = window_with_session(cx, "source-p", "/home/u/proj", "t1");
    let target = window_with_session(cx, "target-p", "/home/u/proj", "t2");
    let before = target.read_with(cx, |ws, _| ws.workspace.projects.len());

    let entry = detach(cx, &source, "t1");
    let moved_id = entry.session.id.clone();
    target.update(cx, |ws, wcx| {
        assert!(ws.adopt_entry(entry, wcx).is_ok());
    });

    target.update(cx, |ws, _cx| {
        assert_eq!(
            ws.workspace.projects.len(),
            before,
            "the path matched, so no second project row appeared"
        );
        let (pi, _) = ws.workspace.project_session_index(&moved_id).unwrap();
        assert_eq!(
            ws.workspace.projects[pi].id, "target-p",
            "it joined the target's OWN group for that path"
        );
    });
}

/// No group at that path ⇒ one is created, carrying the provenance verbatim.
#[gpui::test]
fn adopt_creates_the_project_when_the_target_has_none(cx: &mut TestAppContext) {
    let source = window_with_session(cx, "p", "/home/u/proj", "t1");
    let target = plain_window(cx);

    let entry = detach(cx, &source, "t1");
    let moved_id = entry.session.id.clone();
    target.update(cx, |ws, wcx| {
        assert!(ws.adopt_entry(entry, wcx).is_ok());
    });

    target.update(cx, |ws, _cx| {
        let (pi, _) = ws.workspace.project_session_index(&moved_id).unwrap();
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
    let source = plain_window(cx);
    let target = plain_window(cx);
    // Mint a second Terminals session in the source and detach THAT (Main stays
    // put, so the source keeps a session and the ids cannot collide).
    source.update(cx, |ws, _cx| {
        let mut session = Session::new("term-2", "Terminal 2", "/home/u");
        session.windows = vec![TermWindow::new("term-2-w", "Terminal 1", TermWindowKind::Terminal)];
        session.active_window_id = Some("term-2-w".to_string());
        ws.workspace.projects[0].sessions.push(session);
    });

    let entry = detach(cx, &source, "term-2");
    let moved_id = entry.session.id.clone();
    target.update(cx, |ws, wcx| {
        assert!(ws.adopt_entry(entry, wcx).is_ok());
    });

    target.update(cx, |ws, _cx| {
        let terminals = ws
            .workspace
            .projects
            .iter()
            .filter(|p| p.id == WorkspaceModel::TERMINALS_PROJECT_ID)
            .count();
        assert_eq!(terminals, 1, "exactly one Terminals group, as always");
        assert!(
            ws.workspace.is_terminals_project_session(&moved_id),
            "the adopted row joined it"
        );
    });
}

/// Every fresh window seeds the SAME `terminals-main`, so a moved Main used to
/// collide with the seeded Main of whatever window adopted it — the duplicate-id
/// guard would refuse. Detach RE-KEYS on the way out, so both Mains coexist.
#[gpui::test]
fn a_moved_main_adopts_into_a_window_that_seeded_its_own(cx: &mut TestAppContext) {
    let source = plain_window(cx);
    let target = plain_window(cx);
    let main = WorkspaceModel::MAIN_TERMINAL_SESSION_ID;

    let entry = detach(cx, &source, main);
    let moved_id = entry.session.id.clone();
    assert_ne!(
        moved_id, main,
        "the moved session must not still carry the constant every window seeds"
    );
    target.update(cx, |ws, wcx| {
        assert!(
            ws.adopt_entry(entry, wcx).is_ok(),
            "the adopt must not be refused by the duplicate-id guard"
        );
    });

    target.update(cx, |ws, _cx| {
        assert!(
            ws.workspace.session_for(main).is_some(),
            "the window's OWN Main is untouched"
        );
        assert!(
            ws.workspace.session_for(&moved_id).is_some(),
            "and the adopted one sits beside it"
        );
        assert_eq!(
            ws.workspace.projects[0].sessions.len(),
            2,
            "both live in the one pinned Terminals group"
        );
    });
}

// ---- parent-ref rules ------------------------------------------------------

/// A `/branch` child whose parent did not come along loses its parent pointer
/// (review N7) — otherwise the sidebar indents it under a session that does not
/// exist in this window.
#[gpui::test]
fn adopt_clears_a_parent_reference_the_target_cannot_resolve(cx: &mut TestAppContext) {
    let target = plain_window(cx);

    let mut child = Session::new("child", "Branch", "/home/u/proj");
    child.parent_session_id = Some("absent-parent".into());
    let entry = structural_entry(
        child,
        DetachedProject {
            id: "p".into(),
            name: "Work".into(),
            path: "/home/u/proj".into(),
        },
    );

    target.update(cx, |ws, wcx| {
        assert!(ws.adopt_entry(entry, wcx).is_ok());
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
    let target = window_with_session(cx, "p", "/home/u/proj", "parent");

    let mut child = Session::new("child", "Branch", "/home/u/proj");
    child.parent_session_id = Some("parent".into());
    let entry = structural_entry(
        child,
        DetachedProject {
            id: "p".into(),
            name: "Work".into(),
            path: "/home/u/proj".into(),
        },
    );

    target.update(cx, |ws, wcx| {
        assert!(ws.adopt_entry(entry, wcx).is_ok());
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

// ---- the duplicate-id guard ------------------------------------------------

/// B3c: a window that ALREADY owns that session id refuses, and the refusal hands
/// the entry BACK rather than dropping it — dropping would SIGHUP a live child
/// over a belt-and-braces check.
#[gpui::test]
fn adopt_entry_refuses_a_duplicate_id_and_hands_the_entry_back(cx: &mut TestAppContext) {
    let target = window_with_session(cx, "p", "/home/u/proj", "t1");
    let entry = structural_entry(
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
        assert_eq!(
            ws.workspace.session_for("t1").unwrap().title,
            "Work",
            "the window's own session is untouched"
        );
    });
}
