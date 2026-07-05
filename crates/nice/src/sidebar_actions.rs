//! `SidebarActions` — the create / close / select seam the sidebar UI drives.
//!
//! This is the one nameable surface R13 rewires (dossier G3). Every sidebar
//! control that creates, closes, or selects a tab funnels through this trait
//! rather than reaching into the model directly, so R13's swap from
//! "model-only" to "real sessions" is mechanical: replace the injected
//! [`SidebarActions`] implementation and nothing in the views changes.
//!
//! ## R10 is model-only — nothing spawns
//!
//! [`ModelSidebarActions`] is the R10 implementation: it mutates **only** the R8
//! [`TabModel`] value tree (create the tab shape, remove tabs via the single
//! [`TabModel::remove_tab`] entry point, move the active-tab selection). No pty
//! is spawned, no Claude process starts, and there is **no busy-pane close
//! confirmation** — that is W5/R18. The create paths build the model shape the
//! session layer will later populate:
//!
//!   * [`ModelSidebarActions::create_terminal_tab`] — one terminal-only tab with
//!     a single "Terminal 1" pane, appended to the pinned Terminals project.
//!   * [`ModelSidebarActions::create_claude_tab_in_project`] — the
//!     `[Claude, Terminal 1]` pane shape (a Claude pane focused, plus a companion
//!     terminal), appended to the named project.
//!
//! ## Selection is the caller's concern
//!
//! These methods touch the model only. The sidebar view owns the
//! [`nice_model::SidebarTabSelection`] invariant, so after a create it re-seeds
//! the selection from the new active tab and after a close it prunes the
//! selection against the surviving tab ids. The one model-side selection
//! side-effect the close paths carry is **reselection**: removing the active tab
//! would leave `active_tab_id` dangling, so the close paths promote a surviving
//! neighbour through [`TabModel::select_tab`] (still model-only). R13 replaces
//! this with the real focus/dissolve cascade.

// The seam's in-crate caller is the sidebar shell view (`sidebar_shell`); the
// trait's full method set is the R13 rewiring contract (plan "Exported
// contracts"), so some methods have no live caller until a control that invokes
// them is wired. The model shapes below ARE exercised by this module's tests.
#![allow(dead_code)]

use nice_model::{Pane, PaneKind, Tab, TabModel};

/// The create / close / select actions the sidebar UI invokes. The per-window
/// state owns a boxed instance ([`crate::sidebar_shell::SidebarShellView`]); R13
/// swaps the implementation to spawn/close real sessions. Keeping this a single
/// trait is what makes R13's rewiring mechanical (plan "Exported contracts").
pub(crate) trait SidebarActions {
    /// Create a new terminal-only tab in the pinned Terminals project and select
    /// it. Returns the new tab id (or `None` if the Terminals project is somehow
    /// absent). R13 spawns the pty; R10 only shapes the model.
    fn create_terminal_tab(&mut self, model: &mut TabModel) -> Option<String>;

    /// Create a new tab in `project_id` with the `[Claude, Terminal 1]` pane
    /// shape (the Claude pane focused) and select it. Returns the new tab id (or
    /// `None` if `project_id` is unknown). Model-only — nothing spawns.
    fn create_claude_tab_in_project(
        &mut self,
        model: &mut TabModel,
        project_id: &str,
    ) -> Option<String>;

    /// Select `tab_id` — the [`TabModel::select_tab`] passthrough.
    fn select_tab(&mut self, model: &mut TabModel, tab_id: &str);

    /// Remove `tab_id` via the single [`TabModel::remove_tab`] entry point, then
    /// reselect a surviving neighbour if the active tab was the one removed.
    fn close_tab(&mut self, model: &mut TabModel, tab_id: &str);

    /// Remove every id in `tab_ids` (the multi-select "Close N Tabs" path), each
    /// via [`TabModel::remove_tab`], then reselect once at the end.
    fn close_tabs(&mut self, model: &mut TabModel, tab_ids: &[String]);

    /// Remove every tab in `project_id` and drop the now-empty project (never the
    /// pinned Terminals group), then reselect. "Close Project" is offered only
    /// for non-Terminals groups by the view.
    fn close_project(&mut self, model: &mut TabModel, project_id: &str);
}

/// The R10 model-only [`SidebarActions`] implementation. Holds a monotonic id
/// counter so the tabs/panes it mints get unique ids without a UUID dependency
/// (nothing here is persisted this cycle — R18 owns persistence). R13 replaces
/// this whole type with the session-spawning implementation.
#[derive(Default)]
pub(crate) struct ModelSidebarActions {
    /// Monotonic counter feeding minted tab/pane ids.
    next_id: u64,
}

impl ModelSidebarActions {
    /// A fresh instance.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Mint a unique id with the given prefix.
    fn mint(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}-{}", self.next_id)
    }

    /// Remove the tab with id `tab_id` via the single removal entry point (which
    /// also sweeps sibling `parent_tab_id` references), if present.
    fn remove_by_id(model: &mut TabModel, tab_id: &str) {
        if let Some((pi, ti)) = model.project_tab_index(tab_id) {
            model.remove_tab(pi, ti);
        }
    }

    /// If the active tab no longer exists (it was just removed), promote the
    /// first surviving navigable tab to active. Leaves the dangling id in place
    /// only when the tree is fully drained (a teardown edge the view's selection
    /// prune handles) — the model has no "clear active" writer.
    fn reselect_if_active_missing(model: &mut TabModel) {
        let active_present = model
            .active_tab_id()
            .is_some_and(|a| model.tab_for(a).is_some());
        if active_present {
            return;
        }
        if let Some(first) = model.navigable_sidebar_tab_ids().first().cloned() {
            model.select_tab(&first);
        }
    }
}

impl SidebarActions for ModelSidebarActions {
    fn create_terminal_tab(&mut self, model: &mut TabModel) -> Option<String> {
        let ti = model
            .projects
            .iter()
            .position(|p| p.id == TabModel::TERMINALS_PROJECT_ID)?;
        let tab_id = self.mint("term-tab");
        let pane_id = self.mint("pane");
        let path = model.projects[ti].path.clone();
        let mut tab = Tab::new(tab_id.clone(), "Terminal", path);
        tab.panes = vec![Pane::new(pane_id.clone(), "Terminal 1", PaneKind::Terminal)];
        tab.active_pane_id = Some(pane_id);
        // Match the Main tab's seed: "Terminal 1" already consumed slot 1.
        tab.next_terminal_index = 2;
        model.projects[ti].tabs.push(tab);
        model.select_tab(&tab_id);
        Some(tab_id)
    }

    fn create_claude_tab_in_project(
        &mut self,
        model: &mut TabModel,
        project_id: &str,
    ) -> Option<String> {
        let pi = model.projects.iter().position(|p| p.id == project_id)?;
        let tab_id = self.mint("claude-tab");
        let claude_pane_id = self.mint("pane");
        let terminal_pane_id = self.mint("pane");
        let path = model.projects[pi].path.clone();
        let mut tab = Tab::new(tab_id.clone(), "New tab", path);
        // The [Claude, Terminal 1] shape: the Claude pane focused, with a
        // companion terminal (dossier G3 — model-only, nothing spawns).
        tab.panes = vec![
            Pane::new(claude_pane_id.clone(), "Claude", PaneKind::Claude),
            Pane::new(terminal_pane_id, "Terminal 1", PaneKind::Terminal),
        ];
        tab.active_pane_id = Some(claude_pane_id);
        tab.next_terminal_index = 2;
        model.projects[pi].tabs.push(tab);
        model.select_tab(&tab_id);
        Some(tab_id)
    }

    fn select_tab(&mut self, model: &mut TabModel, tab_id: &str) {
        model.select_tab(tab_id);
    }

    fn close_tab(&mut self, model: &mut TabModel, tab_id: &str) {
        Self::remove_by_id(model, tab_id);
        Self::reselect_if_active_missing(model);
    }

    fn close_tabs(&mut self, model: &mut TabModel, tab_ids: &[String]) {
        for id in tab_ids {
            Self::remove_by_id(model, id);
        }
        Self::reselect_if_active_missing(model);
    }

    fn close_project(&mut self, model: &mut TabModel, project_id: &str) {
        // Never dissolve the pinned Terminals group.
        if project_id == TabModel::TERMINALS_PROJECT_ID {
            return;
        }
        let Some(pi) = model.projects.iter().position(|p| p.id == project_id) else {
            return;
        };
        let ids: Vec<String> = model.projects[pi].tabs.iter().map(|t| t.id.clone()).collect();
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
    use nice_model::{PaneKind, TabModel};

    /// A model seeded with the pinned Terminals group (Main tab) plus one
    /// non-Terminals project "proj" holding a single tab "t-a".
    fn seeded() -> TabModel {
        let mut model = TabModel::new("/home/u");
        model.ensure_project("proj", "Proj", "/home/u/proj");
        // ensure_project returns the index; append a tab directly for the test.
        let pi = model
            .projects
            .iter()
            .position(|p| p.id == "proj")
            .unwrap();
        let mut tab = Tab::new("t-a", "A", "/home/u/proj");
        tab.panes = vec![Pane::new("t-a-p", "Claude", PaneKind::Claude)];
        tab.active_pane_id = Some("t-a-p".into());
        model.projects[pi].tabs.push(tab);
        model
    }

    #[test]
    fn create_terminal_tab_appends_to_terminals_and_selects() {
        let mut model = seeded();
        let mut actions = ModelSidebarActions::new();

        let id = actions.create_terminal_tab(&mut model).unwrap();

        let terminals = &model.projects[0];
        assert_eq!(terminals.id, TabModel::TERMINALS_PROJECT_ID);
        let created = terminals.tabs.iter().find(|t| t.id == id).unwrap();
        assert_eq!(created.panes.len(), 1);
        assert_eq!(created.panes[0].kind, PaneKind::Terminal);
        assert_eq!(created.panes[0].title, "Terminal 1");
        assert_eq!(created.next_terminal_index, 2);
        assert_eq!(model.active_tab_id(), Some(id.as_str()));
    }

    #[test]
    fn create_claude_tab_builds_claude_terminal_shape_and_selects() {
        let mut model = seeded();
        let mut actions = ModelSidebarActions::new();

        let id = actions
            .create_claude_tab_in_project(&mut model, "proj")
            .unwrap();

        let tab = model.tab_for(&id).unwrap();
        assert_eq!(tab.panes.len(), 2, "[Claude, Terminal 1] — two panes");
        assert_eq!(tab.panes[0].kind, PaneKind::Claude);
        assert_eq!(tab.panes[1].kind, PaneKind::Terminal);
        assert_eq!(tab.panes[1].title, "Terminal 1");
        assert_eq!(
            tab.active_pane_id.as_deref(),
            Some(tab.panes[0].id.as_str()),
            "the Claude pane is focused"
        );
        assert!(tab.has_claude(), "the tab reads as a Claude tab for the dot");
        assert_eq!(model.active_tab_id(), Some(id.as_str()));
    }

    #[test]
    fn create_claude_tab_unknown_project_is_none() {
        let mut model = seeded();
        let mut actions = ModelSidebarActions::new();
        assert!(actions
            .create_claude_tab_in_project(&mut model, "nope")
            .is_none());
    }

    #[test]
    fn close_tab_removes_and_reselects_when_active() {
        let mut model = seeded();
        let mut actions = ModelSidebarActions::new();
        // Make "t-a" the active tab, then close it.
        model.select_tab("t-a");
        assert_eq!(model.active_tab_id(), Some("t-a"));

        actions.close_tab(&mut model, "t-a");

        assert!(model.tab_for("t-a").is_none(), "the tab is gone");
        // Active was reselected onto a surviving navigable tab (the Main tab).
        let active = model.active_tab_id().unwrap();
        assert!(model.tab_for(active).is_some(), "active points at a live tab");
    }

    #[test]
    fn close_tab_keeps_active_when_other_removed() {
        let mut model = seeded();
        let mut actions = ModelSidebarActions::new();
        model.select_tab(TabModel::MAIN_TERMINAL_TAB_ID);

        actions.close_tab(&mut model, "t-a");

        assert!(model.tab_for("t-a").is_none());
        assert_eq!(
            model.active_tab_id(),
            Some(TabModel::MAIN_TERMINAL_TAB_ID),
            "closing a non-active tab must not move the active selection"
        );
    }

    #[test]
    fn close_tab_sweeps_dangling_parent_pointers() {
        // A child tab indented under "t-a"; removing "t-a" via the single entry
        // point must clear the child's parent pointer (the sweep can't be
        // skipped).
        let mut model = seeded();
        let pi = model.projects.iter().position(|p| p.id == "proj").unwrap();
        let mut child = Tab::new("t-child", "Child", "/home/u/proj");
        child.parent_tab_id = Some("t-a".into());
        model.projects[pi].tabs.push(child);
        let mut actions = ModelSidebarActions::new();

        actions.close_tab(&mut model, "t-a");

        assert_eq!(
            model.tab_for("t-child").unwrap().parent_tab_id,
            None,
            "the removal entry point swept the dangling parent pointer"
        );
    }

    #[test]
    fn close_tabs_removes_every_id() {
        let mut model = seeded();
        // Add a second tab in proj.
        let pi = model.projects.iter().position(|p| p.id == "proj").unwrap();
        model.projects[pi]
            .tabs
            .push(Tab::new("t-b", "B", "/home/u/proj"));
        let mut actions = ModelSidebarActions::new();

        actions.close_tabs(&mut model, &["t-a".to_string(), "t-b".to_string()]);

        assert!(model.tab_for("t-a").is_none());
        assert!(model.tab_for("t-b").is_none());
    }

    #[test]
    fn close_project_removes_tabs_and_drops_project() {
        let mut model = seeded();
        let mut actions = ModelSidebarActions::new();

        actions.close_project(&mut model, "proj");

        assert!(
            model.projects.iter().all(|p| p.id != "proj"),
            "the emptied non-Terminals project is dropped"
        );
        assert!(model.tab_for("t-a").is_none());
        // Terminals group is untouched.
        assert!(model
            .projects
            .iter()
            .any(|p| p.id == TabModel::TERMINALS_PROJECT_ID));
    }

    #[test]
    fn close_project_refuses_terminals_group() {
        let mut model = seeded();
        let mut actions = ModelSidebarActions::new();

        actions.close_project(&mut model, TabModel::TERMINALS_PROJECT_ID);

        assert!(
            model
                .projects
                .iter()
                .any(|p| p.id == TabModel::TERMINALS_PROJECT_ID),
            "the pinned Terminals group can never be closed"
        );
    }

    #[test]
    fn select_tab_delegates_to_model() {
        let mut model = seeded();
        let mut actions = ModelSidebarActions::new();
        actions.select_tab(&mut model, "t-a");
        assert_eq!(model.active_tab_id(), Some("t-a"));
    }
}
