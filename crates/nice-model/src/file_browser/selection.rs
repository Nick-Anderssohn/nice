//! `FileBrowserSelection` — the file browser's multi-row selection model, one
//! per [`crate::file_browser::state::FileBrowserState`]. Ported from
//! `Sources/Nice/State/FileBrowserSelection.swift`.
//!
//! Mirrors the landed sidebar idiom ([`crate::selection::SidebarTabSelection`])
//! but keyed by **absolute path** and with **no active-tab invariant** and no
//! toggle-out refusal — Finder semantics throughout:
//!
//! * `selected_paths` — current selection; the right-click menu acts on these
//!   when the right-clicked path is in the set.
//! * `last_clicked_path` — anchor for ⇧-range. Set by plain click and
//!   ⌘-click; **not** moved by ⇧-click (Finder keeps the original anchor
//!   across multiple range extensions).
//! * the right-click snap policy: [`selection_paths_for_right_click_on`] is a
//!   **pure read** used to build the menu;
//!   [`snap_if_right_click_outside`] mutates and is called from each menu
//!   item's action (snap on ACTION, not on right-click).
//!
//! [`selection_paths_for_right_click_on`]: FileBrowserSelection::selection_paths_for_right_click_on
//! [`snap_if_right_click_outside`]: FileBrowserSelection::snap_if_right_click_outside

use std::collections::HashSet;

/// Per-file-browser multi-row selection (see the module docs). Mutate only
/// through the methods so the anchor rules can't be skipped (the Swift
/// `private(set)` analog).
#[derive(Debug, Clone, Default)]
pub struct FileBrowserSelection {
    selected_paths: HashSet<String>,
    last_clicked_path: Option<String>,
}

impl FileBrowserSelection {
    /// A fresh, empty selection (`FileBrowserSelection.swift:31`).
    pub fn new() -> Self {
        Self::default()
    }

    // MARK: - Mutation

    /// Replace the selection with exactly `paths`. Called on plain click
    /// (single path) and right-click outside selection. The anchor moves to
    /// `anchor` when given, else to the last of `paths`
    /// (`FileBrowserSelection.swift:37-44`).
    pub fn replace(&mut self, paths: &[String], anchor: Option<&str>) {
        self.selected_paths = paths.iter().cloned().collect();
        self.last_clicked_path = match anchor {
            Some(a) => Some(a.to_string()),
            None => paths.last().cloned(),
        };
    }

    /// Toggle `path` in/out of the selection (⌘-click). The anchor moves to
    /// `path` in either direction, Finder-style — even on a remove
    /// (`FileBrowserSelection.swift:47-54`).
    pub fn toggle(&mut self, path: &str) {
        if self.selected_paths.contains(path) {
            self.selected_paths.remove(path);
        } else {
            self.selected_paths.insert(path.to_string());
        }
        self.last_clicked_path = Some(path.to_string());
    }

    /// Extend the selection to span from `last_clicked_path` to `path`,
    /// inclusive, using `visible_order`. With no anchor yet, behaves like a
    /// single-path replace. If the anchor OR target is missing from
    /// `visible_order`, falls back to a single-path replace so the selection
    /// still moves. The anchor is **not** updated — Finder keeps the original
    /// anchor across multiple range extensions
    /// (`FileBrowserSelection.swift:61-78`).
    pub fn extend(&mut self, path: &str, visible_order: &[String]) {
        let Some(anchor) = self.last_clicked_path.clone() else {
            self.selected_paths = once(path);
            self.last_clicked_path = Some(path.to_string());
            return;
        };
        let a_idx = visible_order.iter().position(|x| x == &anchor);
        let b_idx = visible_order.iter().position(|x| x == path);
        match (a_idx, b_idx) {
            (Some(a), Some(b)) => {
                let (lo, hi) = (a.min(b), a.max(b));
                self.selected_paths = visible_order[lo..=hi].iter().cloned().collect();
                // Don't move the anchor.
            }
            _ => {
                self.selected_paths = once(path);
            }
        }
    }

    /// Clear the selection entirely (`FileBrowserSelection.swift:81-84`).
    pub fn clear(&mut self) {
        self.selected_paths = HashSet::new();
        self.last_clicked_path = None;
    }

    // MARK: - Query

    /// The currently-selected paths.
    pub fn selected_paths(&self) -> &HashSet<String> {
        &self.selected_paths
    }

    /// The ⇧-range anchor, if any.
    pub fn last_clicked_path(&self) -> Option<&str> {
        self.last_clicked_path.as_deref()
    }

    /// Whether `path` is selected (`FileBrowserSelection.swift:88-90`).
    pub fn contains(&self, path: &str) -> bool {
        self.selected_paths.contains(path)
    }

    /// Pure resolution of "which paths should the menu act on" for a right-click
    /// on `clicked_path`. If the clicked path is in the selection, return the
    /// whole selection; otherwise just the clicked path. **No mutation** — this
    /// is read while building the context menu, so any state change would loop
    /// the render. The visible "snap to clicked row" side effect happens via
    /// [`FileBrowserSelection::snap_if_right_click_outside`]
    /// (`FileBrowserSelection.swift:103-108`).
    pub fn selection_paths_for_right_click_on(&self, clicked_path: &str) -> Vec<String> {
        if self.selected_paths.contains(clicked_path) {
            self.selected_paths.iter().cloned().collect()
        } else {
            vec![clicked_path.to_string()]
        }
    }

    /// Apply the Finder-style "right-click outside selection replaces it" rule.
    /// Called by menu action handlers right before they fire, so the selection
    /// visibly snaps to the clicked row when the user picked a menu item that
    /// wasn't already part of the selection
    /// (`FileBrowserSelection.swift:115-118`).
    pub fn snap_if_right_click_outside(&mut self, clicked_path: &str) {
        if self.selected_paths.contains(clicked_path) {
            return;
        }
        self.replace(&[clicked_path.to_string()], None);
    }
}

/// A single-element path set — the `selectedPaths = [path]` Swift idiom.
fn once(path: &str) -> HashSet<String> {
    std::iter::once(path.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn paths(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // MARK: - replace

    /// `FileBrowserSelectionTests.test_replace_setsExactly`
    #[test]
    fn replace_sets_exactly() {
        let mut s = FileBrowserSelection::new();
        s.replace(&paths(&["/a", "/b"]), None);
        assert_eq!(s.selected_paths(), &set(&["/a", "/b"]));
        assert_eq!(s.last_clicked_path(), Some("/b"));
    }

    /// `FileBrowserSelectionTests.test_replace_explicitAnchor_overridesDefault`
    #[test]
    fn replace_explicit_anchor_overrides_default() {
        let mut s = FileBrowserSelection::new();
        s.replace(&paths(&["/a", "/b"]), Some("/a"));
        assert_eq!(s.last_clicked_path(), Some("/a"));
    }

    // MARK: - toggle

    /// `FileBrowserSelectionTests.test_toggle_addsAbsentPath`
    #[test]
    fn toggle_adds_absent_path() {
        let mut s = FileBrowserSelection::new();
        s.replace(&paths(&["/a"]), None);
        s.toggle("/b");
        assert_eq!(s.selected_paths(), &set(&["/a", "/b"]));
        assert_eq!(s.last_clicked_path(), Some("/b"));
    }

    /// `FileBrowserSelectionTests.test_toggle_removesPresentPath`
    #[test]
    fn toggle_removes_present_path() {
        let mut s = FileBrowserSelection::new();
        s.replace(&paths(&["/a", "/b"]), None);
        s.toggle("/b");
        assert_eq!(s.selected_paths(), &set(&["/a"]));
        assert_eq!(
            s.last_clicked_path(),
            Some("/b"),
            "anchor still moves to the toggled row, even on remove"
        );
    }

    // MARK: - extend

    /// `FileBrowserSelectionTests.test_extend_inclusiveBetweenLastAndCurrent`
    #[test]
    fn extend_inclusive_between_last_and_current() {
        let order = paths(&["/a", "/b", "/c", "/d", "/e"]);
        let mut s = FileBrowserSelection::new();
        s.replace(&paths(&["/b"]), None);
        s.extend("/d", &order);
        assert_eq!(s.selected_paths(), &set(&["/b", "/c", "/d"]));
        assert_eq!(
            s.last_clicked_path(),
            Some("/b"),
            "shift-extend must not move the anchor"
        );
    }

    /// `FileBrowserSelectionTests.test_extend_currentBeforeLastReversesRange`
    #[test]
    fn extend_current_before_last_reverses_range() {
        let order = paths(&["/a", "/b", "/c", "/d", "/e"]);
        let mut s = FileBrowserSelection::new();
        s.replace(&paths(&["/d"]), None);
        s.extend("/b", &order);
        assert_eq!(s.selected_paths(), &set(&["/b", "/c", "/d"]));
    }

    /// `FileBrowserSelectionTests.test_extend_emptyAnchor_treatsAsReplace`
    #[test]
    fn extend_empty_anchor_treats_as_replace() {
        let mut s = FileBrowserSelection::new();
        s.extend("/c", &paths(&["/a", "/b", "/c"]));
        assert_eq!(s.selected_paths(), &set(&["/c"]));
        assert_eq!(s.last_clicked_path(), Some("/c"));
    }

    /// `FileBrowserSelectionTests.test_extend_targetMissingFromOrder_fallsBackToReplace`
    #[test]
    fn extend_target_missing_from_order_falls_back_to_replace() {
        let order = paths(&["/a", "/b"]);
        let mut s = FileBrowserSelection::new();
        s.replace(&paths(&["/a"]), None);
        s.extend("/c", &order);
        assert_eq!(s.selected_paths(), &set(&["/c"]));
    }

    // MARK: - selection_paths_for_right_click_on

    /// `FileBrowserSelectionTests.test_rightClick_insideSelection_returnsAll`
    #[test]
    fn right_click_inside_selection_returns_all() {
        let mut s = FileBrowserSelection::new();
        s.replace(&paths(&["/a", "/b", "/c"]), None);
        let picked = s.selection_paths_for_right_click_on("/b");
        assert_eq!(picked.into_iter().collect::<HashSet<_>>(), set(&["/a", "/b", "/c"]));
        assert_eq!(s.selected_paths(), &set(&["/a", "/b", "/c"]));
    }

    /// `FileBrowserSelectionTests.test_rightClick_outsideSelection_returnsOnePath_doesNotMutate`
    #[test]
    fn right_click_outside_selection_returns_one_path_does_not_mutate() {
        let mut s = FileBrowserSelection::new();
        s.replace(&paths(&["/a", "/b"]), None);
        let picked = s.selection_paths_for_right_click_on("/c");
        assert_eq!(picked, vec!["/c".to_string()]);
        assert_eq!(
            s.selected_paths(),
            &set(&["/a", "/b"]),
            "selection_paths must be a pure read — no mutation during menu build"
        );
    }

    /// `FileBrowserSelectionTests.test_rightClick_emptySelection_returnsOnePath_doesNotMutate`
    #[test]
    fn right_click_empty_selection_returns_one_path_does_not_mutate() {
        let s = FileBrowserSelection::new();
        let picked = s.selection_paths_for_right_click_on("/x");
        assert_eq!(picked, vec!["/x".to_string()]);
        assert!(s.selected_paths().is_empty());
    }

    // MARK: - snap_if_right_click_outside

    /// `FileBrowserSelectionTests.test_snapIfRightClickOutside_outsideSelection_replaces`
    #[test]
    fn snap_if_right_click_outside_outside_selection_replaces() {
        let mut s = FileBrowserSelection::new();
        s.replace(&paths(&["/a", "/b"]), None);
        s.snap_if_right_click_outside("/c");
        assert_eq!(s.selected_paths(), &set(&["/c"]));
    }

    /// `FileBrowserSelectionTests.test_snapIfRightClickOutside_insideSelection_isNoOp`
    #[test]
    fn snap_if_right_click_outside_inside_selection_is_no_op() {
        let mut s = FileBrowserSelection::new();
        s.replace(&paths(&["/a", "/b", "/c"]), None);
        s.snap_if_right_click_outside("/b");
        assert_eq!(
            s.selected_paths(),
            &set(&["/a", "/b", "/c"]),
            "right-click on a row already in the selection must not collapse it"
        );
    }

    // MARK: - clear

    /// `FileBrowserSelectionTests.test_clear_resetsBoth`
    #[test]
    fn clear_resets_both() {
        let mut s = FileBrowserSelection::new();
        s.replace(&paths(&["/a", "/b"]), None);
        s.clear();
        assert!(s.selected_paths().is_empty());
        assert_eq!(s.last_clicked_path(), None);
    }
}
