//! `FileBrowserState` — per-tab state for the sidebar's file-browser mode.
//! Ported from `Sources/Nice/State/FileBrowserState.swift`. In-memory only
//! (deliberately not persisted — expansion sets don't round-trip well across
//! launches when directories churn, and the tab's cwd is already persisted on
//! [`crate::Tab`]).
//!
//! Owns: the tree `root_path`, the set of expanded directory paths, the
//! dotfile-visibility flag (seeded cwd-aware, sticky afterwards), and the
//! multi-row [`FileBrowserSelection`]. The tagged listing cache described in
//! the R19 staleness-healing decision is a **view-layer** concern and lands
//! with the browser view (a later slice), not here.
//!
//! Swift's `rootPath` `didSet` — every re-root adds the new root to
//! `expanded_paths` so the tree shows its contents immediately — is spelled
//! [`FileBrowserState::set_root_path`] here (Rust has no `didSet`); the
//! constructor seeds the same way explicitly.

use crate::file_browser::selection::FileBrowserSelection;
use std::collections::BTreeSet;

/// Per-tab file-browser state (see the module docs). `expanded_paths` is a
/// [`BTreeSet`] of **absolute** paths — stable across rebuilds because it keys
/// on paths, not identity.
#[derive(Debug, Default)]
pub struct FileBrowserState {
    root_path: String,
    expanded_paths: BTreeSet<String>,
    show_hidden: bool,
    selection: FileBrowserSelection,
}

impl FileBrowserState {
    /// Seed a state rooted at `root_path`, with dotfiles **hidden** by default;
    /// the root is seeded into `expanded_paths` so files render on first draw
    /// (`FileBrowserState.swift:58-64`).
    ///
    /// INTENTIONAL DEVIATION from the Swift app (user decision 2026-07-07): a new
    /// tab always defaults to hidden-off (dotfiles hidden), regardless of cwd.
    /// The Swift original seeded `show_hidden` from a cwd heuristic — hidden off
    /// only in `$HOME`, on in every project root so `.gitignore` / `.env` showed
    /// — but the user prefers a clean listing everywhere by default. A per-tab
    /// value, once the user toggles it (⌘⇧. / the eye control), keeps winning:
    /// the [`FileBrowserStore`](crate::file_browser::FileBrowserStore) creates a
    /// state on first access and never re-seeds it, so only brand-new tabs take
    /// this default.
    pub fn new(root_path: impl Into<String>) -> Self {
        let root_path = root_path.into();
        let mut expanded_paths = BTreeSet::new();
        expanded_paths.insert(root_path.clone());
        Self {
            root_path,
            expanded_paths,
            show_hidden: false,
            selection: FileBrowserSelection::new(),
        }
    }

    /// The directory currently shown as the tree root.
    pub fn root_path(&self) -> &str {
        &self.root_path
    }

    /// Re-root the tree. Adds the new root to `expanded_paths` so its children
    /// render immediately — the Swift `rootPath` `didSet`. Prior expansion
    /// entries in other subtrees are preserved, not cleared
    /// (`FileBrowserState.swift:28-32`).
    pub fn set_root_path(&mut self, path: impl Into<String>) {
        self.root_path = path.into();
        self.expanded_paths.insert(self.root_path.clone());
    }

    /// The set of expanded directory paths.
    pub fn expanded_paths(&self) -> &BTreeSet<String> {
        &self.expanded_paths
    }

    /// Insert `path` into the expanded set (the Swift
    /// `expandedPaths.insert(...)` direct write).
    pub fn insert_expanded(&mut self, path: impl Into<String>) {
        self.expanded_paths.insert(path.into());
    }

    /// Symmetric add/remove of `path` in the expanded set — the disclosure
    /// triangle toggle (`FileBrowserState.swift:81-87`).
    pub fn toggle_expansion(&mut self, path: &str) {
        if self.expanded_paths.contains(path) {
            self.expanded_paths.remove(path);
        } else {
            self.expanded_paths.insert(path.to_string());
        }
    }

    /// Whether dotfiles are visible.
    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// Set dotfile visibility (breadcrumb eye toggle).
    pub fn set_show_hidden(&mut self, value: bool) {
        self.show_hidden = value;
    }

    /// Flip dotfile visibility — the ⌘⇧. shortcut's per-state action.
    pub fn toggle_show_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
    }

    /// The multi-row selection.
    pub fn selection(&self) -> &FileBrowserSelection {
        &self.selection
    }

    /// The multi-row selection, mutable.
    pub fn selection_mut(&mut self) -> &mut FileBrowserSelection {
        &mut self.selection
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // MARK: - init

    /// `FileBrowserStateTests.test_init_seedsExpandedPathsWithRoot`
    #[test]
    fn init_seeds_expanded_paths_with_root() {
        let state = FileBrowserState::new("/tmp/proj");
        assert!(state.expanded_paths().contains("/tmp/proj"));
    }

    /// New tabs default to hidden-off (dotfiles hidden) everywhere — the
    /// INTENTIONAL deviation from the Swift cwd heuristic (user decision
    /// 2026-07-07). A project root no longer shows `.gitignore` / `.env` by
    /// default; the user opts in with ⌘⇧. or the eye control.
    #[test]
    fn init_defaults_show_hidden_off_everywhere() {
        assert!(!FileBrowserState::new("/tmp/proj").show_hidden());
        assert!(!FileBrowserState::new("/Users/tester").show_hidden());
        assert!(!FileBrowserState::new("/").show_hidden());
    }

    // MARK: - set_root_path (the didSet)

    /// `FileBrowserStateTests.test_rootPath_didSet_addsNewRootToExpandedPaths`
    #[test]
    fn set_root_path_adds_new_root_to_expanded_paths() {
        let mut state = FileBrowserState::new("/tmp/proj");
        state.set_root_path("/tmp/proj/Sources");
        assert!(state.expanded_paths().contains("/tmp/proj/Sources"));
    }

    /// `FileBrowserStateTests.test_rootPath_didSet_preservesPriorExpansionInOtherSubtrees`
    #[test]
    fn set_root_path_preserves_prior_expansion_in_other_subtrees() {
        let mut state = FileBrowserState::new("/tmp/proj");
        state.insert_expanded("/tmp/proj/Sources");
        state.set_root_path("/elsewhere");
        assert!(state.expanded_paths().contains("/tmp/proj/Sources"));
        assert!(state.expanded_paths().contains("/elsewhere"));
    }

    // MARK: - toggle_expansion

    /// `FileBrowserStateTests.test_toggleExpansion_addsThenRemoves`
    #[test]
    fn toggle_expansion_adds_then_removes() {
        let mut state = FileBrowserState::new("/tmp/proj");
        assert!(!state.expanded_paths().contains("/tmp/proj/Sources"));
        state.toggle_expansion("/tmp/proj/Sources");
        assert!(state.expanded_paths().contains("/tmp/proj/Sources"));
        state.toggle_expansion("/tmp/proj/Sources");
        assert!(!state.expanded_paths().contains("/tmp/proj/Sources"));
    }

    /// `FileBrowserStateTests.test_toggleExpansion_doesNotAffectOtherEntries`
    #[test]
    fn toggle_expansion_does_not_affect_other_entries() {
        let mut state = FileBrowserState::new("/tmp/proj");
        state.insert_expanded("/tmp/proj/Sources");
        state.toggle_expansion("/tmp/proj/Tests");
        assert!(state.expanded_paths().contains("/tmp/proj/Sources"));
        assert!(state.expanded_paths().contains("/tmp/proj/Tests"));
    }
}
