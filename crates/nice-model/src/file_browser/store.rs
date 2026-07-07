//! `FileBrowserStore` — the per-window `Tab.id → FileBrowserState` catalog.
//! Ported from `Sources/Nice/State/FileBrowserStore.swift`. Owns the
//! lifecycle of the browser's per-tab state: lazy creation on first access,
//! removal on tab close (wired into the landed dissolve hook by a later
//! slice), and the "toggle hidden iff a state already exists" semantics the
//! ⌘⇧. shortcut needs.
//!
//! In-memory only — see [`crate::file_browser::state::FileBrowserState`] for
//! why expansion / scroll state isn't worth persisting across launches, and for
//! the hidden-files default (dotfiles hidden everywhere by default — the
//! 2026-07-07 deviation from Swift's cwd heuristic).

use crate::file_browser::state::FileBrowserState;
use std::collections::HashMap;

/// Per-window map of tab id → file-browser state (see the module docs).
#[derive(Debug, Default)]
pub struct FileBrowserStore {
    states: HashMap<String, FileBrowserState>,
}

impl FileBrowserStore {
    /// A fresh, empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fetch the state for `tab_id`, creating one rooted at `cwd` if none
    /// exists. `cwd` is a **seed** used only on first creation — a subsequent
    /// call with a different cwd returns the existing state unchanged, so the
    /// user's in-state navigation (`root_path`) is never reset
    /// (`FileBrowserStore.swift:34-39`).
    pub fn ensure_state(&mut self, tab_id: &str, cwd: &str) -> &mut FileBrowserState {
        self.states
            .entry(tab_id.to_string())
            .or_insert_with(|| FileBrowserState::new(cwd))
    }

    /// The state for `tab_id`, if one exists — a pure read that never
    /// allocates.
    pub fn state_for(&self, tab_id: &str) -> Option<&FileBrowserState> {
        self.states.get(tab_id)
    }

    /// Whether a state exists for `tab_id` (the ⌘⇧. gate's "if exists" check,
    /// read-only).
    pub fn has_state(&self, tab_id: &str) -> bool {
        self.states.contains_key(tab_id)
    }

    /// Remove the state for a tab. Called when a tab is dissolved so the map
    /// doesn't grow over a long session. Unknown tabs are a no-op
    /// (`FileBrowserStore.swift:43-45`).
    pub fn remove_state(&mut self, tab_id: &str) {
        self.states.remove(tab_id);
    }

    /// Toggle hidden-file visibility for `tab_id` **iff** a state already
    /// exists; returns `true` if a toggle happened. The "if exists" guard is
    /// what makes the ⌘⇧. shortcut a true no-op — no allocation, no change —
    /// when the user has never opened the file browser for the active tab
    /// (`FileBrowserStore.swift:53-58`).
    pub fn toggle_hidden_files_if_exists(&mut self, tab_id: &str) -> bool {
        match self.states.get_mut(tab_id) {
            Some(state) => {
                state.toggle_show_hidden();
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> FileBrowserStore {
        FileBrowserStore::new()
    }

    // MARK: - ensure_state

    /// `FileBrowserStoreTests.test_ensureState_createsFreshStateOnFirstCall`
    #[test]
    fn ensure_state_creates_fresh_state_on_first_call() {
        let mut store = store();
        assert!(store.state_for("t1").is_none());
        let state = store.ensure_state("t1", "/tmp/proj");
        assert_eq!(state.root_path(), "/tmp/proj");
        assert!(store.state_for("t1").is_some());
    }

    /// `FileBrowserStoreTests.test_ensureState_returnsSameInstanceOnSecondCall`
    /// — in Rust "same instance" means in-place mutations survive a re-fetch.
    #[test]
    fn ensure_state_returns_same_state_on_second_call() {
        let mut store = store();
        store
            .ensure_state("t1", "/tmp/proj")
            .insert_expanded("/tmp/proj/Sources");
        let again = store.ensure_state("t1", "/tmp/proj");
        assert!(
            again.expanded_paths().contains("/tmp/proj/Sources"),
            "repeated ensure_state must return the same state so views see in-place mutations"
        );
    }

    /// `FileBrowserStoreTests.test_ensureState_secondCall_ignoresNewCwdArg`
    #[test]
    fn ensure_state_second_call_ignores_new_cwd_arg() {
        let mut store = store();
        store.ensure_state("t1", "/tmp/original");
        let again = store.ensure_state("t1", "/tmp/different");
        assert_eq!(
            again.root_path(),
            "/tmp/original",
            "cwd is seed-only; a later call must not reset the user's navigation"
        );
    }

    /// `FileBrowserStoreTests.test_ensureState_distinctTabs_getDistinctStates`
    #[test]
    fn ensure_state_distinct_tabs_get_distinct_states() {
        let mut store = store();
        assert_eq!(store.ensure_state("t1", "/tmp/a").root_path(), "/tmp/a");
        assert_eq!(store.ensure_state("t2", "/tmp/b").root_path(), "/tmp/b");
        // Confirm t1 wasn't clobbered by seeding t2.
        assert_eq!(store.ensure_state("t1", "/ignored").root_path(), "/tmp/a");
    }

    // MARK: - remove_state

    /// `FileBrowserStoreTests.test_removeState_dropsExistingEntry`
    #[test]
    fn remove_state_drops_existing_entry() {
        let mut store = store();
        store.ensure_state("t1", "/tmp/proj");
        store.remove_state("t1");
        assert!(store.state_for("t1").is_none());
    }

    /// `FileBrowserStoreTests.test_removeState_unknownTab_isNoop`
    #[test]
    fn remove_state_unknown_tab_is_noop() {
        let mut store = store();
        store.remove_state("nope");
        assert!(!store.has_state("nope"));
    }

    /// `FileBrowserStoreTests.test_removeState_thenEnsureState_seedsFreshFromNewCwd`
    #[test]
    fn remove_state_then_ensure_state_seeds_fresh_from_new_cwd() {
        let mut store = store();
        store.ensure_state("t1", "/tmp/old");
        store.remove_state("t1");
        let next = store.ensure_state("t1", "/tmp/new");
        assert_eq!(next.root_path(), "/tmp/new");
    }

    // MARK: - toggle_hidden_files_if_exists

    /// `FileBrowserStoreTests.test_toggleHiddenFilesIfExists_noState_returnsFalseAndAllocatesNothing`
    #[test]
    fn toggle_hidden_files_if_exists_no_state_returns_false_and_allocates_nothing() {
        let mut store = store();
        let did_toggle = store.toggle_hidden_files_if_exists("t1");
        assert!(!did_toggle, "without a state the call must report no-op");
        assert!(
            !store.has_state("t1"),
            "must NOT lazy-create a state — the shortcut is silent until the browser is opened"
        );
    }

    /// `FileBrowserStoreTests.test_toggleHiddenFilesIfExists_withState_flipsAndReturnsTrue`
    #[test]
    fn toggle_hidden_files_if_exists_with_state_flips_and_returns_true() {
        let mut store = store();
        let before = store.ensure_state("t1", "/tmp/proj").show_hidden();
        let did_toggle = store.toggle_hidden_files_if_exists("t1");
        assert!(did_toggle);
        assert_eq!(store.state_for("t1").unwrap().show_hidden(), !before);
    }

    /// `FileBrowserStoreTests.test_toggleHiddenFilesIfExists_twice_restoresOriginalValue`
    #[test]
    fn toggle_hidden_files_if_exists_twice_restores_original_value() {
        let mut store = store();
        let original = store.ensure_state("t1", "/tmp/proj").show_hidden();
        store.toggle_hidden_files_if_exists("t1");
        store.toggle_hidden_files_if_exists("t1");
        assert_eq!(store.state_for("t1").unwrap().show_hidden(), original);
    }
}
