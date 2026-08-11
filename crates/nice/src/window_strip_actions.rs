//! `WindowStripActions` — the select / close / add-terminal seam the toolbar window
//! strip drives.
//!
//! The window-level sibling of [`crate::sidebar_actions::SidebarActions`]: every
//! pill control that selects, closes, or adds a window funnels through this trait
//! rather than reaching into the [`WorkspaceModel`] directly, so R13's swap from
//! "model-only" to "real sessions" is mechanical — replace the injected
//! implementation and nothing in the strip views changes (plan "Actions ride the
//! R10 seam" / "R13 rewires the seam without touching strip internals").
//!
//! ## R11 is model-only — nothing spawns
//!
//! [`ModelWindowStripActions`] mutates **only** the R8 value tree. Selecting a window
//! moves the session's `active_window_id` (real focus / session activation is R13);
//! closing a window routes through [`WorkspaceModel::extract_window`], which re-points the
//! active window to a neighbor and has **no** busy-window close confirmation (that is
//! R18); adding a window routes through [`WorkspaceModel::add_window`], the R8 auto-naming
//! ("Terminal N", monotonic counter) — R13 spawns the pty behind it.

// The seam's in-crate caller is the toolbar view (`toolbar`); the trait's full
// method set is the R13 rewiring contract, so a method may have no live caller
// until a control that invokes it is wired. The model behavior below IS exercised
// by this module's tests.
#![allow(dead_code)]

use nice_model::WorkspaceModel;

use crate::pty_manager::default_mint_id;

/// The select / close / add-terminal actions the toolbar pill strip invokes. The
/// toolbar view owns a boxed instance; R13 swaps the implementation to drive real
/// sessions. Keeping it a single trait is what makes R13's rewiring mechanical.
pub(crate) trait WindowStripActions {
    /// Make `term_window_id` the active window of `session_id`. Model-only: it moves
    /// `active_window_id` (a no-op if the window isn't on the session, so selection can
    /// never dangle). R13 replaces this with real focus / session activation.
    fn select_window(&mut self, model: &mut WorkspaceModel, session_id: &str, term_window_id: &str);

    /// Close `term_window_id` on `session_id` via the single [`WorkspaceModel::extract_window`] entry
    /// point, which re-points `active_window_id` to a neighbor when the closed window
    /// was active. No busy-window confirmation — that is R18.
    fn close_term_window(&mut self, model: &mut WorkspaceModel, session_id: &str, term_window_id: &str);

    /// Append an auto-named terminal window to `session_id` and focus it via the R8
    /// [`WorkspaceModel::add_window`] (the "Terminal N" monotonic counter). Returns the
    /// new window id, or `None` if the session is unknown. R13 spawns the pty.
    fn add_terminal_window(&mut self, model: &mut WorkspaceModel, session_id: &str) -> Option<String>;

    /// Move focus to the next window within the active session, **wrapping**. A no-op
    /// when the active session has fewer than two windows (or has no active window). The
    /// R12 ⌘⌥→ shortcut drives this; model-only — it moves the active session's
    /// `active_window_id`. R13 re-implements it behind this same name so real focus
    /// activation + the deferred-spawn acknowledgment ride along without touching
    /// callers. Ported from `SessionsModel.stepActivePane(by: +1)`.
    fn select_next_window(&mut self, model: &mut WorkspaceModel);

    /// Move focus to the previous window within the active session, **wrapping**. The
    /// ⌘⌥← counterpart of [`select_next_window`](Self::select_next_window); same
    /// <2-windows no-op and same R13 rewiring contract. Ported from
    /// `SessionsModel.stepActivePane(by: -1)`.
    fn select_prev_window(&mut self, model: &mut WorkspaceModel);
}

/// The R11 model-only [`WindowStripActions`] implementation. Window ids come from
/// the process-global time+counter minter
/// ([`crate::pty_manager::default_mint_id`]) so a mint can never collide
/// with a window id restored from a previous launch. (The original per-instance
/// counter restarted at 0 every launch while restored windows kept their persisted
/// ids verbatim — the first `+` press after a relaunch re-minted an existing
/// `pane-N` id, and every id-keyed strip affordance then matched both windows:
/// double-selected pills, click-inert select, rename editing both.)
#[derive(Default)]
pub(crate) struct ModelWindowStripActions;

impl ModelWindowStripActions {
    /// A fresh instance.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Wrapping step of the active session's `active_window_id` by `offset` windows. A
    /// no-op when there is no active session, the active session has fewer than two windows,
    /// or its `active_window_id` is unset / not on the session (selection can never
    /// dangle). Mirrors Swift's `SessionsModel.stepActivePane`
    /// (`((i + off) % n + n) % n`, expressed here with `rem_euclid`).
    fn step_active_window(model: &mut WorkspaceModel, offset: isize) {
        let Some(session_id) = model.active_session_id().map(str::to_owned) else {
            return;
        };
        let Some((pi, ti)) = model.project_session_index(&session_id) else {
            return;
        };
        let session = &model.projects[pi].sessions[ti];
        let count = session.windows.len();
        if count < 2 {
            return;
        }
        let Some(active) = session.active_window_id.clone() else {
            return;
        };
        let Some(cur) = session.windows.iter().position(|w| w.id == active) else {
            return;
        };
        let next = (cur as isize + offset).rem_euclid(count as isize) as usize;
        let next_id = session.windows[next].id.clone();
        model.projects[pi].sessions[ti].active_window_id = Some(next_id);
    }
}

impl WindowStripActions for ModelWindowStripActions {
    fn select_window(&mut self, model: &mut WorkspaceModel, session_id: &str, term_window_id: &str) {
        let Some((pi, ti)) = model.project_session_index(session_id) else {
            return;
        };
        let session = &mut model.projects[pi].sessions[ti];
        // Guard against selecting a window that isn't on the session — never leave a
        // dangling active_window_id.
        if session.windows.iter().any(|w| w.id == term_window_id) {
            session.active_window_id = Some(term_window_id.to_string());
        }
    }

    fn close_term_window(&mut self, model: &mut WorkspaceModel, session_id: &str, term_window_id: &str) {
        model.extract_window(term_window_id, session_id);
    }

    fn add_terminal_window(&mut self, model: &mut WorkspaceModel, session_id: &str) -> Option<String> {
        let term_window_id = default_mint_id("pane-");
        model.add_window(session_id, term_window_id, None)
    }

    fn select_next_window(&mut self, model: &mut WorkspaceModel) {
        Self::step_active_window(model, 1);
    }

    fn select_prev_window(&mut self, model: &mut WorkspaceModel) {
        Self::step_active_window(model, -1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nice_model::{TermWindow, TermWindowKind, Session, WorkspaceModel};

    /// The freshly-seeded model's Main terminal session (one "Terminal 1" window,
    /// `next_terminal_index = 2`, that window active).
    fn seeded() -> WorkspaceModel {
        WorkspaceModel::new("/home/u")
    }

    fn main_session_id() -> &'static str {
        WorkspaceModel::MAIN_TERMINAL_SESSION_ID
    }

    #[test]
    fn add_terminal_window_appends_auto_named_and_focuses_it() {
        let mut model = seeded();
        let mut actions = ModelWindowStripActions::new();

        let new_id = actions.add_terminal_window(&mut model, main_session_id()).unwrap();

        let session = model.session_for(main_session_id()).unwrap();
        assert_eq!(session.windows.len(), 2, "the new terminal window was appended");
        let created = session.windows.last().unwrap();
        assert_eq!(created.id, new_id);
        assert_eq!(created.kind, TermWindowKind::Terminal);
        // Seed consumed slot 1 ("Terminal 1"); the add takes slot 2.
        assert_eq!(created.title, "Terminal 2");
        assert_eq!(
            session.active_window_id.as_deref(),
            Some(new_id.as_str()),
            "the new window is focused"
        );
        assert_eq!(session.next_terminal_index, 3, "the monotonic counter advanced");
    }

    #[test]
    fn add_terminal_window_never_collides_with_a_restored_window_id() {
        // The relaunch bug: a previous launch minted "window-1", persisted it, and
        // a fresh instance's first mint re-issued "window-1" — two windows sharing
        // one id (double-selected pills, rename editing both). Simulate the
        // restored save, then mint from fresh instances (a new instance per
        // launch) and require every id on the session to stay unique.
        let mut model = seeded();
        let (pi, ti) = model.project_session_index(main_session_id()).unwrap();
        model.projects[pi].sessions[ti]
            .windows
            .push(TermWindow::new("pane-1", "Moldavite", TermWindowKind::Terminal));

        ModelWindowStripActions::new()
            .add_terminal_window(&mut model, main_session_id())
            .unwrap();
        ModelWindowStripActions::new()
            .add_terminal_window(&mut model, main_session_id())
            .unwrap();

        let session = model.session_for(main_session_id()).unwrap();
        let mut ids: Vec<&str> = session.windows.iter().map(|w| w.id.as_str()).collect();
        ids.sort();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before, "no two windows may share an id: {ids:?}");
    }

    #[test]
    fn add_terminal_window_unknown_session_is_none() {
        let mut model = seeded();
        let mut actions = ModelWindowStripActions::new();
        assert!(actions.add_terminal_window(&mut model, "nope").is_none());
    }

    #[test]
    fn select_window_moves_active_when_the_window_exists() {
        let mut model = seeded();
        let mut actions = ModelWindowStripActions::new();
        // Add a second window so there's something else to select.
        let other = actions.add_terminal_window(&mut model, main_session_id()).unwrap();
        let first = model.session_for(main_session_id()).unwrap().windows[0].id.clone();

        // add_terminal_window focused `other`; select the first window back.
        actions.select_window(&mut model, main_session_id(), &first);
        assert_eq!(
            model.session_for(main_session_id()).unwrap().active_window_id.as_deref(),
            Some(first.as_str())
        );

        actions.select_window(&mut model, main_session_id(), &other);
        assert_eq!(
            model.session_for(main_session_id()).unwrap().active_window_id.as_deref(),
            Some(other.as_str())
        );
    }

    #[test]
    fn select_window_unknown_window_never_dangles_active() {
        let mut model = seeded();
        let mut actions = ModelWindowStripActions::new();
        let before = model.session_for(main_session_id()).unwrap().active_window_id.clone();

        actions.select_window(&mut model, main_session_id(), "ghost-pane");

        assert_eq!(
            model.session_for(main_session_id()).unwrap().active_window_id,
            before,
            "selecting a window not on the session must leave active_window_id untouched"
        );
    }

    #[test]
    fn close_term_window_removes_and_refocuses_a_neighbor() {
        // A session with [a, b, c], b active; closing b re-points active to the window
        // that slid into b's slot (c), per WorkspaceModel::neighbor_active_window_id.
        let mut model = seeded();
        let pi = 0;
        let ti = 0;
        let mut session = Session::new("t2", "Two", "/home/u");
        session.windows = vec![
            TermWindow::new("a", "A", TermWindowKind::Terminal),
            TermWindow::new("b", "B", TermWindowKind::Terminal),
            TermWindow::new("c", "C", TermWindowKind::Terminal),
        ];
        session.active_window_id = Some("b".into());
        model.projects[pi].sessions.insert(ti, session);
        let mut actions = ModelWindowStripActions::new();

        actions.close_term_window(&mut model, "t2", "b");

        let session = model.session_for("t2").unwrap();
        assert_eq!(session.windows.len(), 2);
        assert!(session.windows.iter().all(|w| w.id != "b"), "b is gone");
        assert_eq!(
            session.active_window_id.as_deref(),
            Some("c"),
            "active re-points to the window that slid into the freed slot"
        );
    }

    /// Seed a `[a, b, c]` three-window session (a active) as the active session of the
    /// model, so window-stepping has something to wrap over.
    fn seeded_three_window() -> WorkspaceModel {
        let mut model = seeded();
        let mut session = Session::new("t3", "Three", "/home/u");
        session.windows = vec![
            TermWindow::new("a", "A", TermWindowKind::Terminal),
            TermWindow::new("b", "B", TermWindowKind::Terminal),
            TermWindow::new("c", "C", TermWindowKind::Terminal),
        ];
        session.active_window_id = Some("a".into());
        model.projects[0].sessions.insert(0, session);
        model.select_session("t3");
        model
    }

    fn active_window(model: &WorkspaceModel) -> Option<String> {
        model
            .session_for("t3")
            .and_then(|s| s.active_window_id.clone())
    }

    #[test]
    fn select_next_window_wraps_forward_over_the_active_session() {
        let mut model = seeded_three_window();
        let mut actions = ModelWindowStripActions::new();

        actions.select_next_window(&mut model);
        assert_eq!(active_window(&model).as_deref(), Some("b"));
        actions.select_next_window(&mut model);
        assert_eq!(active_window(&model).as_deref(), Some("c"));
        // Wrap: c → a.
        actions.select_next_window(&mut model);
        assert_eq!(active_window(&model).as_deref(), Some("a"), "next wraps c→a");
    }

    #[test]
    fn select_prev_window_wraps_backward_over_the_active_session() {
        let mut model = seeded_three_window();
        let mut actions = ModelWindowStripActions::new();

        // Wrap immediately: a → c.
        actions.select_prev_window(&mut model);
        assert_eq!(active_window(&model).as_deref(), Some("c"), "prev wraps a→c");
        actions.select_prev_window(&mut model);
        assert_eq!(active_window(&model).as_deref(), Some("b"));
    }

    #[test]
    fn select_next_window_is_a_noop_with_fewer_than_two_windows() {
        // The seeded Main session has a single window; stepping must not move (or panic).
        let mut model = seeded();
        model.select_session(main_session_id());
        let before = model.session_for(main_session_id()).unwrap().active_window_id.clone();
        let mut actions = ModelWindowStripActions::new();

        actions.select_next_window(&mut model);
        actions.select_prev_window(&mut model);

        assert_eq!(
            model.session_for(main_session_id()).unwrap().active_window_id,
            before,
            "a single-window session has nowhere to step — active_window_id is untouched"
        );
    }

    #[test]
    fn close_term_window_of_inactive_window_keeps_active() {
        let mut model = seeded();
        let mut session = Session::new("t2", "Two", "/home/u");
        session.windows = vec![
            TermWindow::new("a", "A", TermWindowKind::Terminal),
            TermWindow::new("b", "B", TermWindowKind::Terminal),
        ];
        session.active_window_id = Some("a".into());
        model.projects[0].sessions.insert(0, session);
        let mut actions = ModelWindowStripActions::new();

        actions.close_term_window(&mut model, "t2", "b");

        let session = model.session_for("t2").unwrap();
        assert_eq!(
            session.active_window_id.as_deref(),
            Some("a"),
            "closing a non-active window must not move the active window"
        );
    }
}
