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
    /// Seed a state rooted at `root_path`. `show_hidden` is derived from the cwd
    /// via [`FileBrowserState::default_show_hidden`] (hidden off in `$HOME`, on
    /// elsewhere); the root is seeded into `expanded_paths` so files render on
    /// first draw (`FileBrowserState.swift:58-64`). `home` is injected (the
    /// Swift `NSHomeDirectory()` seam) so the cwd heuristic is testable.
    pub fn new(root_path: impl Into<String>, home: &str) -> Self {
        let root_path = root_path.into();
        let show_hidden = Self::default_show_hidden(&root_path, home);
        let mut expanded_paths = BTreeSet::new();
        expanded_paths.insert(root_path.clone());
        Self {
            root_path,
            expanded_paths,
            show_hidden,
            selection: FileBrowserSelection::new(),
        }
    }

    /// Seed value for `show_hidden` from the spawning cwd: hidden **off** iff
    /// the cwd is exactly `home` (the dotfile flood there isn't useful default
    /// content), **on** everywhere else (a project root's `.gitignore`, `.env`
    /// etc. are content the developer expects to see). Comparison standardizes
    /// both sides (tilde expansion + trailing-slash / `.`-`..` normalization)
    /// so `~`, `~/`, and the literal home path agree
    /// (`FileBrowserState.swift:74-79`).
    pub fn default_show_hidden(cwd: &str, home: &str) -> bool {
        standardize(cwd, home) != standardize(home, home)
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

/// Standardize a path for the home-vs-elsewhere comparison: expand a leading
/// `~` to `home`, then lexically normalize (drop empty / `.` components,
/// resolve `..`, strip trailing slashes). Symlinks are NOT resolved — matching
/// `URL.standardizedFileURL`, which the Swift original relies on.
fn standardize(path: &str, home: &str) -> String {
    let expanded = if path == "~" {
        home.to_string()
    } else if let Some(rest) = path.strip_prefix("~/") {
        format!("{}/{}", home.trim_end_matches('/'), rest)
    } else {
        path.to_string()
    };
    normalize_lexical(&expanded)
}

fn normalize_lexical(p: &str) -> String {
    let absolute = p.starts_with('/');
    let mut comps: Vec<&str> = Vec::new();
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                comps.pop();
            }
            x => comps.push(x),
        }
    }
    let joined = comps.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed injected home so the cwd heuristic tests don't depend on the
    /// test runner's real `$HOME` (the Swift suite uses `NSHomeDirectory()`;
    /// this port injects it, which the plan requires).
    const HOME: &str = "/Users/tester";

    // MARK: - default_show_hidden

    /// `FileBrowserStateTests.test_defaultShowHidden_inHomeDir_isFalse`
    #[test]
    fn default_show_hidden_in_home_dir_is_false() {
        assert!(!FileBrowserState::default_show_hidden(HOME, HOME));
    }

    /// `FileBrowserStateTests.test_defaultShowHidden_homeDirWithTrailingSlash_isFalse`
    #[test]
    fn default_show_hidden_home_dir_with_trailing_slash_is_false() {
        let with_slash = format!("{HOME}/");
        assert!(!FileBrowserState::default_show_hidden(&with_slash, HOME));
    }

    /// `FileBrowserStateTests.test_defaultShowHidden_tildePath_isFalse`
    #[test]
    fn default_show_hidden_tilde_path_is_false() {
        assert!(!FileBrowserState::default_show_hidden("~", HOME));
    }

    /// `FileBrowserStateTests.test_defaultShowHidden_subdirOfHome_isTrue`
    #[test]
    fn default_show_hidden_subdir_of_home_is_true() {
        let sub = format!("{HOME}/Projects");
        assert!(FileBrowserState::default_show_hidden(&sub, HOME));
    }

    /// `FileBrowserStateTests.test_defaultShowHidden_unrelatedPath_isTrue`
    #[test]
    fn default_show_hidden_unrelated_path_is_true() {
        assert!(FileBrowserState::default_show_hidden("/tmp/some-project", HOME));
    }

    /// `FileBrowserStateTests.test_defaultShowHidden_filesystemRoot_isTrue`
    #[test]
    fn default_show_hidden_filesystem_root_is_true() {
        assert!(FileBrowserState::default_show_hidden("/", HOME));
    }

    // MARK: - init

    /// `FileBrowserStateTests.test_init_seedsExpandedPathsWithRoot`
    #[test]
    fn init_seeds_expanded_paths_with_root() {
        let state = FileBrowserState::new("/tmp/proj", HOME);
        assert!(state.expanded_paths().contains("/tmp/proj"));
    }

    /// `FileBrowserStateTests.test_init_seedsShowHiddenFromCwd`
    #[test]
    fn init_seeds_show_hidden_from_cwd() {
        let home_state = FileBrowserState::new(HOME, HOME);
        assert!(!home_state.show_hidden(), "home tabs default to hidden-off");

        let project_state = FileBrowserState::new("/tmp/proj", HOME);
        assert!(project_state.show_hidden(), "non-home tabs default to hidden-on");
    }

    // MARK: - set_root_path (the didSet)

    /// `FileBrowserStateTests.test_rootPath_didSet_addsNewRootToExpandedPaths`
    #[test]
    fn set_root_path_adds_new_root_to_expanded_paths() {
        let mut state = FileBrowserState::new("/tmp/proj", HOME);
        state.set_root_path("/tmp/proj/Sources");
        assert!(state.expanded_paths().contains("/tmp/proj/Sources"));
    }

    /// `FileBrowserStateTests.test_rootPath_didSet_preservesPriorExpansionInOtherSubtrees`
    #[test]
    fn set_root_path_preserves_prior_expansion_in_other_subtrees() {
        let mut state = FileBrowserState::new("/tmp/proj", HOME);
        state.insert_expanded("/tmp/proj/Sources");
        state.set_root_path("/elsewhere");
        assert!(state.expanded_paths().contains("/tmp/proj/Sources"));
        assert!(state.expanded_paths().contains("/elsewhere"));
    }

    // MARK: - toggle_expansion

    /// `FileBrowserStateTests.test_toggleExpansion_addsThenRemoves`
    #[test]
    fn toggle_expansion_adds_then_removes() {
        let mut state = FileBrowserState::new("/tmp/proj", HOME);
        assert!(!state.expanded_paths().contains("/tmp/proj/Sources"));
        state.toggle_expansion("/tmp/proj/Sources");
        assert!(state.expanded_paths().contains("/tmp/proj/Sources"));
        state.toggle_expansion("/tmp/proj/Sources");
        assert!(!state.expanded_paths().contains("/tmp/proj/Sources"));
    }

    /// `FileBrowserStateTests.test_toggleExpansion_doesNotAffectOtherEntries`
    #[test]
    fn toggle_expansion_does_not_affect_other_entries() {
        let mut state = FileBrowserState::new("/tmp/proj", HOME);
        state.insert_expanded("/tmp/proj/Sources");
        state.toggle_expansion("/tmp/proj/Tests");
        assert!(state.expanded_paths().contains("/tmp/proj/Sources"));
        assert!(state.expanded_paths().contains("/tmp/proj/Tests"));
    }
}
