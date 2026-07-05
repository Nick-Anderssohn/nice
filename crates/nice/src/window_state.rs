//! `WindowState` — the per-window composition root, the Rust mirror of Swift's
//! `AppState` (`Sources/Nice/State/AppState.swift:60-75`).
//!
//! Each Nice window owns exactly one `WindowState`, held as a `gpui::Entity`
//! (app-global) and tracked by [`crate::window_registry::WindowRegistry`]. It is
//! handed to the window as a **constructor argument** by the app's window
//! builder (`crate::app::build_window_root`) — the deliberate inversion of
//! Swift's `WindowGroup` token dance (plan DO-NOT-PORT): "which saved slot /
//! which adopted pane does this window own" becomes a plain parameter. R18 will
//! pass restored state and R25 an adopted pane through the same seam.
//!
//! ## Decomposition (mirrors `AppState`)
//!
//! `AppState` holds six sub-models; R12 carries the subset that exists now,
//! per the plan's "Per-window state struct" decision:
//!
//! * [`model`](WindowState::model) — the R8 `TabModel` document (projects / tabs
//!   / panes), the single source of truth for a window's tab tree. Isolation
//!   between windows is exactly that each `WindowState` owns its own `TabModel`.
//! * [`sidebar`](WindowState::sidebar) — the R10 `SidebarModel` (collapse / mode
//!   / peek state).
//! * [`selection`](WindowState::selection) — the R10 `SidebarTabSelection`
//!   (Finder-style multi-select), seeded so the "selection ⊇ {active tab}"
//!   invariant holds from construction.
//! * [`sidebar_actions`](WindowState::sidebar_actions) /
//!   [`pane_strip_actions`](WindowState::pane_strip_actions) — the R10/R11
//!   create/close/select seams. Model-only today; R13 swaps the implementations
//!   for real sessions without touching callers.
//! * [`session`](WindowState::session) — the per-window
//!   [`SessionManager`](crate::session_manager::SessionManager) (R13). Owns the
//!   window's live pane sessions and routes their OSC title/cwd events into
//!   `model`; [`teardown`](WindowState::teardown) is the close hook that tears
//!   them down. R12 carried an empty placeholder here.
//!
//! `AppState`'s remaining sub-models (`sessions`, `closer`,
//! `fileExplorerOrchestrator`, `fileBrowserStore`) are deferred: sessions to R13,
//! the file explorer to R19. They land in later cycles behind the same struct.
//!
//! The fields carry `#![allow(dead_code)]`: R12 slice 1 establishes the state
//! container + window builder + registry; the *keymap* slice (R12 slice 2) is
//! the first live reader of `sidebar` / the action seams (routing ⌘B, ⌘T, the
//! pane-step actions through them), and R13 reads the session slot. The shapes
//! below are exercised by this module's tests.
#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};

use nice_model::{SidebarMode, SidebarModel, SidebarTabSelection, TabModel};

use crate::pane_strip_actions::{ModelPaneStripActions, PaneStripActions};
use crate::session_manager::SessionManager;
use crate::sidebar_actions::{ModelSidebarActions, SidebarActions};

/// Process-wide monotonic source of per-window session ids. Cheap, dependency-
/// free stand-in for Swift's `UUID().uuidString` window-session id — R13 owns
/// the real session identity, but a stable unique id per window exists now so
/// the registry's per-session-id lookup (undo routing, Stage 5) has a real key
/// to match on.
static NEXT_SESSION_SEQ: AtomicU64 = AtomicU64::new(1);

fn mint_session_id() -> String {
    format!("win-{}", NEXT_SESSION_SEQ.fetch_add(1, Ordering::Relaxed))
}

/// The per-window composition root. One per Nice window, owned by a
/// `gpui::Entity` and registered in [`crate::window_registry::WindowRegistry`].
pub(crate) struct WindowState {
    /// The R8 document — this window's projects / tabs / panes tree. Two windows
    /// are isolated precisely because each owns its own `TabModel`.
    pub(crate) model: TabModel,
    /// R10 sidebar collapse / mode / peek state.
    pub(crate) sidebar: SidebarModel,
    /// R10 Finder-style multi-selection (invariant: contains the active tab).
    pub(crate) selection: SidebarTabSelection,
    /// R10 sidebar create/close/select seam (model-only; R13 rewires).
    pub(crate) sidebar_actions: Box<dyn SidebarActions>,
    /// R11 pane-strip select/close/add seam (model-only; R13 rewires).
    pub(crate) pane_strip_actions: Box<dyn PaneStripActions>,
    /// The per-window pty/session manager (R13). Owns this window's live pane
    /// sessions and routes their OSC title/cwd events into `model`. R12 carried
    /// an empty placeholder here; R13 slice 1 fills it with the real
    /// [`SessionManager`] (the action seams that drive it are rewired in a later
    /// R13 slice — this just makes the manager part of the per-window state).
    pub(crate) session: SessionManager,
    /// Stable unique per-window id (the registry's per-session-id lookup key).
    session_id: String,
}

impl WindowState {
    /// A fresh default window: a seeded [`TabModel`] rooted at `initial_cwd`
    /// (pinned Terminals group + Main tab, per `TabModel::new`), an expanded
    /// sidebar in tabs mode, and a selection seeded from the model's active tab —
    /// mirroring `AppState`'s convenience init defaults
    /// (`initialSidebarCollapsed: false`, `initialSidebarMode: .tabs`). Every ⌘N
    /// mints one of these; R18 will add a variant that takes restored state.
    pub(crate) fn new(initial_cwd: impl Into<String>) -> Self {
        let model = TabModel::new(initial_cwd);
        let mut selection = SidebarTabSelection::new();
        selection.sync_active_tab_id(model.active_tab_id());
        Self {
            model,
            sidebar: SidebarModel::new(false, SidebarMode::Tabs),
            selection,
            sidebar_actions: Box::new(ModelSidebarActions::new()),
            pane_strip_actions: Box::new(ModelPaneStripActions::new()),
            session: SessionManager::new(),
            session_id: mint_session_id(),
        }
    }

    /// This window's stable session id — the registry's per-session-id lookup
    /// key (undo routing, Stage 5). R13 reconciles it with the real session
    /// identity.
    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Tear the window's owned resources down on close. R12 has nothing to stop
    /// (the shipped live terminal is owned by the view and dies with the window's
    /// entity, exactly as before this cycle); this is the hook
    /// [`crate::window_registry::WindowRegistry`] calls on window close, which
    /// R13 extends to terminate the window's sessions / ptys. Idempotent.
    pub(crate) fn teardown(&mut self) {
        // Terminate this window's ptys: dropping each cached session handle tears
        // its child process group down (SIGHUP→SIGKILL), so no orphan zsh
        // survives. R14 adds control-socket teardown; R18 flushes the session
        // snapshot before this runs. Idempotent.
        self.session.teardown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nice_model::TabModel;

    #[test]
    fn new_seeds_default_window_shape() {
        let state = WindowState::new("/home/u");
        // Seeded TabModel: the pinned Terminals group + Main tab, Main active.
        assert_eq!(
            state.model.active_tab_id(),
            Some(TabModel::MAIN_TERMINAL_TAB_ID),
            "the Main terminal tab is active on a fresh window"
        );
        assert!(
            state
                .model
                .projects
                .iter()
                .any(|p| p.id == TabModel::TERMINALS_PROJECT_ID),
            "the pinned Terminals group is present"
        );
        // Sidebar defaults: expanded, tabs mode (AppState convenience-init parity).
        assert!(!state.sidebar.collapsed(), "sidebar starts expanded");
        assert_eq!(state.sidebar.mode(), SidebarMode::Tabs);
        // Selection invariant: the active tab is selected from construction.
        assert!(
            state.selection.contains(TabModel::MAIN_TERMINAL_TAB_ID),
            "selection is seeded with the active tab"
        );
    }

    #[test]
    fn each_window_has_a_unique_session_id() {
        let a = WindowState::new("/home/u");
        let b = WindowState::new("/home/u");
        assert!(!a.session_id().is_empty());
        assert_ne!(
            a.session_id(),
            b.session_id(),
            "session ids must be unique per window (the undo-routing lookup key)"
        );
    }

    #[test]
    fn windows_are_isolated_mutating_one_model_leaves_the_other_untouched() {
        // The isolation guarantee at the state level: two windows own
        // independent TabModels, so a mutation to one's tree is invisible to the
        // other. (The live two-window itest — mutate A, B byte-identical — is the
        // scenario slice; this pins the underlying state ownership.)
        let mut a = WindowState::new("/home/u");
        let b = WindowState::new("/home/u");

        let before_b: Vec<usize> = b.model.projects.iter().map(|p| p.tabs.len()).collect();

        // Mutate A's tree through its own seam (the same surface the keymap slice
        // will drive): add a terminal tab.
        let new_id = a
            .sidebar_actions
            .create_terminal_tab(&mut a.model)
            .expect("Terminals group exists");
        assert!(a.model.tab_for(&new_id).is_some(), "A gained the new tab");

        let after_b: Vec<usize> = b.model.projects.iter().map(|p| p.tabs.len()).collect();
        assert_eq!(before_b, after_b, "B's tree is unchanged by A's mutation");
        assert!(
            b.model.tab_for(&new_id).is_none(),
            "A's new tab never appears in B"
        );
    }

    #[test]
    fn teardown_is_idempotent() {
        // R12's teardown is a no-op hook; calling it more than once is safe (R13
        // extends it to real session teardown, which must also be idempotent —
        // the registry calls it exactly once on close, but app-terminate paths
        // may double up).
        let mut state = WindowState::new("/home/u");
        state.teardown();
        state.teardown();
    }
}
