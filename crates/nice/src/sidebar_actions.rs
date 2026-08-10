//! `SidebarActions` — the create / close / select seam the sidebar UI drives.
//!
//! This is the one nameable surface R13 rewires (dossier G3). Every sidebar
//! control that creates, closes, or selects a session funnels through this trait
//! rather than reaching into the model directly, so R13's swap from
//! "model-only" to "real sessions" is mechanical: replace the injected
//! [`SidebarActions`] implementation and nothing in the views changes.
//!
//! ## R10 is model-only — nothing spawns
//!
//! [`ModelSidebarActions`] is the R10 implementation: it mutates **only** the R8
//! [`WorkspaceModel`] value tree (create the session shape, remove sessions via the single
//! [`WorkspaceModel::remove_session`] entry point, move the active-session selection). No pty
//! is spawned, no Claude process starts, and there is **no busy-window close
//! confirmation** — that is W5/R18. The create paths build the model shape the
//! session layer will later populate:
//!
//!   * [`ModelSidebarActions::create_terminal_session`] — one terminal-only session with
//!     a single "Terminal 1" window, appended to the pinned Terminals project.
//!
//! ## Selection is the caller's concern
//!
//! These methods touch the model only. The sidebar view owns the
//! [`nice_model::SidebarSessionSelection`] invariant, so after a create it re-seeds
//! the selection from the new active session and after a close it prunes the
//! selection against the surviving session ids. The one model-side selection
//! side-effect the close paths carry is **reselection**: removing the active session
//! would leave `active_session_id` dangling, so the close paths promote a surviving
//! neighbour through [`WorkspaceModel::select_session`] (still model-only). R13 replaces
//! this with the real focus/dissolve cascade.

// The seam's in-crate caller is the sidebar shell view (`sidebar_shell`); the
// trait's full method set is the R13 rewiring contract (plan "Exported
// contracts"), so some methods have no live caller until a control that invokes
// them is wired. The model shapes below ARE exercised by this module's tests.
#![allow(dead_code)]

use nice_model::{TermWindow, TermWindowKind, Session, WorkspaceModel};

use crate::pty_manager::default_mint_id;

/// The create / close / select actions the sidebar UI invokes. The per-window
/// state owns a boxed instance ([`crate::sidebar_shell::SidebarShellView`]); R13
/// swaps the implementation to spawn/close real sessions. Keeping this a single
/// trait is what makes R13's rewiring mechanical (plan "Exported contracts").
pub(crate) trait SidebarActions {
    /// Create a new terminal-only session in the pinned Terminals project and select
    /// it. Returns the new session id (or `None` if the Terminals project is somehow
    /// absent). R13 spawns the pty; R10 only shapes the model.
    fn create_terminal_session(&mut self, model: &mut WorkspaceModel) -> Option<String>;

    /// Select `session_id` — the [`WorkspaceModel::select_session`] passthrough.
    fn select_session(&mut self, model: &mut WorkspaceModel, session_id: &str);

    /// Remove `session_id` via the single [`WorkspaceModel::remove_session`] entry point, then
    /// reselect a surviving neighbour if the active session was the one removed.
    fn close_session(&mut self, model: &mut WorkspaceModel, session_id: &str);

    /// Remove every id in `session_ids` (the multi-select "Close N Tabs" path), each
    /// via [`WorkspaceModel::remove_session`], then reselect once at the end.
    fn close_sessions(&mut self, model: &mut WorkspaceModel, session_ids: &[String]);

    /// Remove every session in `project_id` and drop the now-empty project (never the
    /// pinned Terminals group), then reselect. "Close Project" is offered only
    /// for non-Terminals groups by the view.
    fn close_project(&mut self, model: &mut WorkspaceModel, project_id: &str);
}

/// The R10 model-only [`SidebarActions`] implementation. Session/window ids come from
/// the process-global time+counter minter
/// ([`crate::pty_manager::default_mint_id`]) so a mint can never collide
/// with an id restored from a previous launch, nor with a window the strip's
/// [`crate::window_strip_actions::ModelWindowStripActions`] minted this launch.
/// (The original per-instance counter restarted at 0 every launch and both
/// action structs counted independently under the same `pane-` prefix — either
/// path could re-mint an id already live in the model.)
#[derive(Default)]
pub(crate) struct ModelSidebarActions;

impl ModelSidebarActions {
    /// A fresh instance.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Remove the session with id `session_id` via the single removal entry point (which
    /// also sweeps sibling `parent_session_id` references), if present.
    fn remove_by_id(model: &mut WorkspaceModel, session_id: &str) {
        if let Some((pi, ti)) = model.project_session_index(session_id) {
            model.remove_session(pi, ti);
        }
    }

    /// If the active session no longer exists (it was just removed), promote the
    /// first surviving navigable session to active. Leaves the dangling id in place
    /// only when the tree is fully drained (a teardown edge the view's selection
    /// prune handles) — the model has no "clear active" writer.
    fn reselect_if_active_missing(model: &mut WorkspaceModel) {
        let active_present = model
            .active_session_id()
            .is_some_and(|a| model.session_for(a).is_some());
        if active_present {
            return;
        }
        if let Some(first) = model.navigable_sidebar_session_ids().first().cloned() {
            model.select_session(&first);
        }
    }
}

impl SidebarActions for ModelSidebarActions {
    fn create_terminal_session(&mut self, model: &mut WorkspaceModel) -> Option<String> {
        let ti = model
            .projects
            .iter()
            .position(|p| p.id == WorkspaceModel::TERMINALS_PROJECT_ID)?;
        let session_id = default_mint_id("term-tab-");
        let term_window_id = default_mint_id("pane-");
        let path = model.projects[ti].path.clone();
        let mut session = Session::new(session_id.clone(), "Terminal", path);
        session.windows = vec![TermWindow::new(term_window_id.clone(), "Terminal 1", TermWindowKind::Terminal)];
        session.active_window_id = Some(term_window_id);
        // Match the Main session's seed: "Terminal 1" already consumed slot 1.
        session.next_terminal_index = 2;
        model.projects[ti].sessions.push(session);
        model.select_session(&session_id);
        Some(session_id)
    }

    fn select_session(&mut self, model: &mut WorkspaceModel, session_id: &str) {
        model.select_session(session_id);
    }

    fn close_session(&mut self, model: &mut WorkspaceModel, session_id: &str) {
        Self::remove_by_id(model, session_id);
        Self::reselect_if_active_missing(model);
    }

    fn close_sessions(&mut self, model: &mut WorkspaceModel, session_ids: &[String]) {
        for id in session_ids {
            Self::remove_by_id(model, id);
        }
        Self::reselect_if_active_missing(model);
    }

    fn close_project(&mut self, model: &mut WorkspaceModel, project_id: &str) {
        // Never dissolve the pinned Terminals group.
        if project_id == WorkspaceModel::TERMINALS_PROJECT_ID {
            return;
        }
        let Some(pi) = model.projects.iter().position(|p| p.id == project_id) else {
            return;
        };
        let ids: Vec<String> = model.projects[pi].sessions.iter().map(|t| t.id.clone()).collect();
        for id in &ids {
            Self::remove_by_id(model, id);
        }
        // Drop the now-empty non-Terminals project.
        model.projects.retain(|p| p.id != project_id);
        Self::reselect_if_active_missing(model);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nice_model::{TermWindowKind, WorkspaceModel};

    /// A model seeded with the pinned Terminals group (Main session) plus one
    /// non-Terminals project "proj" holding a single session "t-a".
    fn seeded() -> WorkspaceModel {
        let mut model = WorkspaceModel::new("/home/u");
        model.ensure_project("proj", "Proj", "/home/u/proj");
        // ensure_project returns the index; append a session directly for the test.
        let pi = model
            .projects
            .iter()
            .position(|p| p.id == "proj")
            .unwrap();
        let mut session = Session::new("t-a", "A", "/home/u/proj");
        session.windows = vec![TermWindow::new("t-a-p", "Claude", TermWindowKind::Claude)];
        session.active_window_id = Some("t-a-p".into());
        model.projects[pi].sessions.push(session);
        model
    }

    #[test]
    fn create_terminal_session_appends_to_terminals_and_selects() {
        let mut model = seeded();
        let mut actions = ModelSidebarActions::new();

        let id = actions.create_terminal_session(&mut model).unwrap();

        let terminals = &model.projects[0];
        assert_eq!(terminals.id, WorkspaceModel::TERMINALS_PROJECT_ID);
        let created = terminals.sessions.iter().find(|t| t.id == id).unwrap();
        assert_eq!(created.windows.len(), 1);
        assert_eq!(created.windows[0].kind, TermWindowKind::Terminal);
        assert_eq!(created.windows[0].title, "Terminal 1");
        assert_eq!(created.next_terminal_index, 2);
        assert_eq!(model.active_session_id(), Some(id.as_str()));
    }

    #[test]
    fn close_session_removes_and_reselects_when_active() {
        let mut model = seeded();
        let mut actions = ModelSidebarActions::new();
        // Make "t-a" the active session, then close it.
        model.select_session("t-a");
        assert_eq!(model.active_session_id(), Some("t-a"));

        actions.close_session(&mut model, "t-a");

        assert!(model.session_for("t-a").is_none(), "the session is gone");
        // Active was reselected onto a surviving navigable session (the Main session).
        let active = model.active_session_id().unwrap();
        assert!(model.session_for(active).is_some(), "active points at a live session");
    }

    #[test]
    fn close_session_keeps_active_when_other_removed() {
        let mut model = seeded();
        let mut actions = ModelSidebarActions::new();
        model.select_session(WorkspaceModel::MAIN_TERMINAL_SESSION_ID);

        actions.close_session(&mut model, "t-a");

        assert!(model.session_for("t-a").is_none());
        assert_eq!(
            model.active_session_id(),
            Some(WorkspaceModel::MAIN_TERMINAL_SESSION_ID),
            "closing a non-active session must not move the active selection"
        );
    }

    #[test]
    fn close_session_sweeps_dangling_parent_pointers() {
        // A child session indented under "t-a"; removing "t-a" via the single entry
        // point must clear the child's parent pointer (the sweep can't be
        // skipped).
        let mut model = seeded();
        let pi = model.projects.iter().position(|p| p.id == "proj").unwrap();
        let mut child = Session::new("t-child", "Child", "/home/u/proj");
        child.parent_session_id = Some("t-a".into());
        model.projects[pi].sessions.push(child);
        let mut actions = ModelSidebarActions::new();

        actions.close_session(&mut model, "t-a");

        assert_eq!(
            model.session_for("t-child").unwrap().parent_session_id,
            None,
            "the removal entry point swept the dangling parent pointer"
        );
    }

    #[test]
    fn close_sessions_removes_every_id() {
        let mut model = seeded();
        // Add a second session in proj.
        let pi = model.projects.iter().position(|p| p.id == "proj").unwrap();
        model.projects[pi]
            .sessions
            .push(Session::new("t-b", "B", "/home/u/proj"));
        let mut actions = ModelSidebarActions::new();

        actions.close_sessions(&mut model, &["t-a".to_string(), "t-b".to_string()]);

        assert!(model.session_for("t-a").is_none());
        assert!(model.session_for("t-b").is_none());
    }

    #[test]
    fn close_project_removes_sessions_and_drops_project() {
        let mut model = seeded();
        let mut actions = ModelSidebarActions::new();

        actions.close_project(&mut model, "proj");

        assert!(
            model.projects.iter().all(|p| p.id != "proj"),
            "the emptied non-Terminals project is dropped"
        );
        assert!(model.session_for("t-a").is_none());
        // Terminals group is untouched.
        assert!(model
            .projects
            .iter()
            .any(|p| p.id == WorkspaceModel::TERMINALS_PROJECT_ID));
    }

    #[test]
    fn close_project_refuses_terminals_group() {
        let mut model = seeded();
        let mut actions = ModelSidebarActions::new();

        actions.close_project(&mut model, WorkspaceModel::TERMINALS_PROJECT_ID);

        assert!(
            model
                .projects
                .iter()
                .any(|p| p.id == WorkspaceModel::TERMINALS_PROJECT_ID),
            "the pinned Terminals group can never be closed"
        );
    }

    #[test]
    fn sidebar_and_strip_minters_never_share_a_window_id() {
        // Both action structs mint under the "window-" prefix; with the old
        // independent per-instance counters each minted "window-1" first, so a
        // sidebar-created session's window could collide with a strip-added window in
        // the same launch. The shared process-global minter makes them disjoint.
        use crate::window_strip_actions::{ModelWindowStripActions, WindowStripActions};

        let mut model = seeded();
        let session_id = ModelSidebarActions::new().create_terminal_session(&mut model).unwrap();
        ModelWindowStripActions::new()
            .add_terminal_window(&mut model, &session_id)
            .unwrap();

        let session = model.session_for(&session_id).unwrap();
        assert_eq!(session.windows.len(), 2);
        assert_ne!(session.windows[0].id, session.windows[1].id, "sidebar/strip mints must not collide");
    }

    #[test]
    fn select_session_delegates_to_model() {
        let mut model = seeded();
        let mut actions = ModelSidebarActions::new();
        actions.select_session(&mut model, "t-a");
        assert_eq!(model.active_session_id(), Some("t-a"));
    }
}
