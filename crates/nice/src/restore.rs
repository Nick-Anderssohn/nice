//! L2/L3 window-restore glue — the launch-time fan-out's building blocks
//! (Swift `WindowSession.restoreSavedWindow` + the `SessionStore` adoption loop).
//!
//! `app::run` owns the composed fan-out (it needs a live gpui `App` to open
//! windows); this module holds the gpui-free pieces so they stay unit-testable:
//!
//! * [`WindowSeed`] — the `(window_id, hydrated model parts, sidebar_collapsed,
//!   frame)` a restored window is built around
//!   ([`crate::window_state::WindowState::with_seed`]);
//! * [`is_restorable`] — the ghost filter (`!projects.is_empty()`), applied both
//!   as the crashed-mid-restore pre-pass and per-window in the fan-out;
//! * [`hydrate_seed`] — a persisted window → a [`WindowSeed`] (its projects
//!   hydrate through `nice_model`);
//! * [`heal_model_cwds`] — the restore-time cwd-heal pass over a built model
//!   (Claude tabs only), returning whether anything was adopted so the caller can
//!   fire restore's single save.

use std::path::Path;

use nice_model::{Project, TabModel};

use crate::cwd_heal::heal_spawn_cwd;
use crate::session_store::{PersistedFrame, PersistedWindow};

/// The parts a restored window is rebuilt from (Swift's restore inputs). Threaded
/// into [`crate::window_state::WindowState::with_seed`]; the fan-out also reads
/// [`frame`](Self::frame) for the W6 bounds override.
pub(crate) struct WindowSeed {
    /// The persisted window id — becomes `WindowState.session_id` verbatim (the
    /// L2 identity rule: restored windows keep their saved id).
    pub(crate) window_id: String,
    /// The hydrated project/tab/pane tree (trust the saved grouping).
    pub(crate) projects: Vec<Project>,
    /// The saved active tab (re-applied iff it survives the repair pass).
    pub(crate) active_tab_id: Option<String>,
    /// Whether the sidebar was collapsed (restored FROM THE STORE — the
    /// deliberate divergence from Swift's SceneStorage restore).
    pub(crate) sidebar_collapsed: bool,
    /// R19: the saved sidebar mode (tabs / files), or `None` ⇒ Tabs (a pre-R19
    /// save, or a window last in tabs mode). Seeded into the rebuilt
    /// [`SidebarModel`](nice_model::SidebarModel) by
    /// [`WindowState::with_seed`](crate::window_state::WindowState::with_seed).
    pub(crate) sidebar_mode: Option<nice_model::SidebarMode>,
    /// The saved on-screen frame (Cocoa points), or `None` ⇒ default placement.
    pub(crate) frame: Option<PersistedFrame>,
}

/// A saved window is restorable iff it has at least one project (Swift's
/// adopt-iff-`!projects.isEmpty` ghost signature, `WindowSession.swift:262-276`).
/// A Terminals-only window whose Terminals project is empty is NOT restorable
/// (its projects list is empty after the snapshot drop rules), but a
/// Terminals-only window that still carries the (always-persisted) Terminals
/// project IS — the empty-Terminals project keeps the window alive.
pub(crate) fn is_restorable(window: &PersistedWindow) -> bool {
    !window.projects.is_empty()
}

/// Hydrate a persisted window into a [`WindowSeed`] — each persisted project
/// hydrates through `nice_model` (which applies the per-tab/-pane restore
/// defaults). Pure; the fan-out builds the `WindowState` from it.
pub(crate) fn hydrate_seed(window: &PersistedWindow) -> WindowSeed {
    WindowSeed {
        window_id: window.id.clone(),
        projects: window.projects.iter().map(|p| p.hydrate()).collect(),
        active_tab_id: window.active_tab_id.clone(),
        sidebar_collapsed: window.sidebar_collapsed,
        sidebar_mode: window.sidebar_mode,
        frame: window.frame.clone(),
    }
}

/// The restore-time cwd-heal pass (L3/C5): for every **Claude** tab (one carrying
/// a `claude_session_id`), check whether Claude actually bucketed the transcript
/// under the saved `tab.cwd`; if not, recover the real cwd from the transcript
/// head and adopt it onto the tab via [`TabModel::adopt_tab_cwd`] (which
/// deliberately does NOT fire the mutation signal — the caller runs restore's one
/// save). Terminal tabs never heal. Returns whether any tab's cwd was adopted.
///
/// `projects_root` is injectable (`~/.claude/projects` in production, resolved in
/// `app::run`) so tests/scenarios drive it against a planted temp bucket tree.
pub(crate) fn heal_model_cwds(model: &mut TabModel, projects_root: &Path) -> bool {
    // Collect the (tab_id, session_id, cwd) triples up front so the scan doesn't
    // borrow `model` across the `adopt_tab_cwd` mutation.
    let claude_tabs: Vec<(String, String, String)> = model
        .projects
        .iter()
        .flat_map(|p| p.tabs.iter())
        .filter_map(|t| {
            t.claude_session_id
                .as_ref()
                .map(|sid| (t.id.clone(), sid.clone(), t.cwd.clone()))
        })
        .collect();

    let mut adopted = false;
    for (tab_id, sid, cwd) in claude_tabs {
        if let Some(recovered) = heal_spawn_cwd(&sid, &cwd, projects_root) {
            if model.adopt_tab_cwd(&tab_id, &recovered) {
                adopted = true;
            }
        }
    }
    adopted
}

#[cfg(test)]
mod tests {
    use super::*;
    use nice_model::{PersistedProject, PersistedTab};

    fn window_with(projects: Vec<PersistedProject>) -> PersistedWindow {
        PersistedWindow {
            id: "w1".into(),
            active_tab_id: None,
            sidebar_collapsed: false,
            sidebar_mode: None,
            projects,
            frame: None,
        }
    }

    #[test]
    fn is_restorable_rejects_a_projectless_ghost() {
        // A crashed-mid-restore ghost saved with an empty projects list.
        assert!(!is_restorable(&window_with(vec![])));
    }

    #[test]
    fn is_restorable_accepts_an_empty_terminals_only_window() {
        // Terminals is always persisted even when empty, so a Terminals-only
        // window still has one project and IS restorable (its saved cwd survives).
        let terminals = PersistedProject {
            id: TabModel::TERMINALS_PROJECT_ID.into(),
            name: "Terminals".into(),
            path: "/home".into(),
            tabs: vec![],
        };
        assert!(is_restorable(&window_with(vec![terminals])));
    }

    #[test]
    fn hydrate_seed_carries_id_collapse_and_projects() {
        let mut w = window_with(vec![PersistedProject {
            id: "nice".into(),
            name: "Nice".into(),
            path: "/work".into(),
            tabs: vec![PersistedTab {
                id: "t1".into(),
                title: "A".into(),
                cwd: "/work".into(),
                claude_session_id: None,
                active_pane_id: None,
                panes: vec![],
                title_manually_set: None,
                parent_tab_id: None,
                next_terminal_index: None,
            }],
        }]);
        w.id = "win-abc".into();
        w.sidebar_collapsed = true;
        w.active_tab_id = Some("t1".into());
        let seed = hydrate_seed(&w);
        assert_eq!(seed.window_id, "win-abc");
        assert!(seed.sidebar_collapsed);
        assert_eq!(seed.active_tab_id.as_deref(), Some("t1"));
        assert_eq!(seed.projects.len(), 1);
        assert_eq!(seed.projects[0].tabs[0].id, "t1");
    }
}
