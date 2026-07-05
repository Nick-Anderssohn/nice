//! `SidebarTabSelection` — the sidebar's multi-tab selection model — ported
//! from `Sources/Nice/State/SidebarTabSelection.swift`. Pure logic with no
//! `gpui` / view dependency, exactly like the Swift class carved out for
//! unit-testability.
//!
//! Owns four pieces of state plus the **"selection ⊇ {active_tab_id}"
//! invariant**:
//!
//! * `selected_tab_ids` — the multi-selection set the right-click menu acts on
//!   when the right-clicked tab is in the set. Always contains `active_tab_id`
//!   while `active_tab_id` is non-`None`; every mutator either re-establishes
//!   that on the way out or refuses the operation (⌘-click on the
//!   only-and-active row is a no-op).
//! * `last_clicked_tab_id` — the anchor for ⇧-range selection. Set by plain
//!   click and ⌘-click; **not** moved by ⇧-click (Finder keeps the original
//!   anchor across multiple range extensions). Cleared by [`SidebarTabSelection::prune`]
//!   if the anchor tab was removed from the tree.
//! * `active_tab_id` — a local mirror of `TabModel`'s active tab. The R8 model
//!   is the source of truth; the view layer feeds external active-tab changes
//!   in via [`SidebarTabSelection::sync_active_tab_id`], and the internal
//!   mutators set it eagerly so they can self-check the invariant without a
//!   round-trip through `TabModel`.
//! * the right-click snap policy ([`SidebarTabSelection::selection_ids_for_right_click_on`]
//!   / [`SidebarTabSelection::snap_if_right_click_outside`]).
//!
//! Selection is a transient UI concept — mutations deliberately do **not**
//! route through `TabModel`'s did-mutate signal, so they never reach the
//! session save. Restart wipes it; only the active-tab id (held on `TabModel`)
//! is persisted.

use std::collections::HashSet;

/// The sidebar's multi-tab selection (see the module docs for the invariant it
/// maintains). Construct with [`SidebarTabSelection::new`]; read state through
/// the getters; mutate only through the methods so the
/// "selection ⊇ {active_tab_id}" invariant can't be skipped by a stray field
/// write (the Swift `private(set)` analog).
#[derive(Debug, Clone, Default)]
pub struct SidebarTabSelection {
    /// Tab ids currently selected. Empty by default; otherwise always a
    /// superset of `{active_tab_id}` while `active_tab_id` is non-`None`.
    selected_tab_ids: HashSet<String>,
    /// Anchor for ⇧-range. Set on plain click and ⌘-click; not moved by
    /// ⇧-click.
    last_clicked_tab_id: Option<String>,
    /// Local mirror of `TabModel`'s active tab, used to maintain the
    /// "selection ⊇ {active_tab_id}" invariant during model mutations.
    active_tab_id: Option<String>,
}

impl SidebarTabSelection {
    /// A fresh, empty selection (`SidebarTabSelection.swift:65`).
    pub fn new() -> Self {
        Self::default()
    }

    // MARK: - Mutation

    /// Plain click. Collapse to `{id}`, move the anchor to `id`, and mark `id`
    /// as the new active tab. The view layer follows this with a
    /// `TabModel::select_tab(id)` to mirror the active-tab change
    /// (`SidebarTabSelection.swift:73-77`).
    pub fn replace(&mut self, id: &str) {
        self.selected_tab_ids = once(id);
        self.last_clicked_tab_id = Some(id.to_string());
        self.active_tab_id = Some(id.to_string());
    }

    /// ⌘-click. Toggle `id` in/out, moving the anchor to `id` either direction
    /// (Finder), and re-establish the active-superset invariant.
    ///
    /// Returns the tab id that should now be active, or `None` when the active
    /// tab is unchanged. The view layer calls `TabModel::select_tab` only on a
    /// `Some` — a `None` for a ⌘-click on the only-and-active selected row keeps
    /// the model and `TabModel` in sync without a redundant `select_tab` write
    /// (`SidebarTabSelection.swift:89-127`).
    pub fn toggle(&mut self, id: &str) -> Option<String> {
        if self.selected_tab_ids.contains(id) {
            self.last_clicked_tab_id = Some(id.to_string());
            if self.active_tab_id.as_deref() == Some(id) {
                if self.selected_tab_ids.len() > 1 {
                    // Toggling out the active tab while others are selected:
                    // drop it, promote any remaining member to active. `first`
                    // is non-deterministic but acceptable — the user just
                    // dropped the previously-active row and any of the others
                    // is a defensible "next active." (count > 1 before the
                    // remove, so at least one remains — the unwrap is safe.)
                    self.selected_tab_ids.remove(id);
                    let next = self.selected_tab_ids.iter().next().cloned().unwrap();
                    self.active_tab_id = Some(next.clone());
                    Some(next)
                } else {
                    // Toggling out the only-and-active selected row would empty
                    // the set — that violates the invariant and matches no
                    // useful Finder behavior. Keep the row selected and active.
                    // (`last_clicked_tab_id = id` above still fired, idempotent,
                    // so a subsequent ⇧-click anchors here.)
                    None
                }
            } else {
                // Toggling out a non-active selected row: just drop it. Active
                // tab unchanged.
                self.selected_tab_ids.remove(id);
                None
            }
        } else {
            self.selected_tab_ids.insert(id.to_string());
            self.last_clicked_tab_id = Some(id.to_string());
            self.active_tab_id = Some(id.to_string());
            Some(id.to_string())
        }
    }

    /// ⇧-click. Extend the selection to span from `last_clicked_tab_id` to `id`,
    /// inclusive, using the given visible row order. With no anchor yet, behaves
    /// like [`SidebarTabSelection::replace`]. If either the anchor or the target
    /// is missing from `visible_order`, falls back to a plain replace so the
    /// selection still moves. The anchor is **not** updated — Finder keeps the
    /// original anchor across multiple range extensions.
    ///
    /// `id` always becomes the new active tab (`SidebarTabSelection.swift:139-155`).
    pub fn extend(&mut self, id: &str, visible_order: &[String]) {
        // The Swift `defer { activeTabId = id }` sets active in every branch.
        let anchor = self.last_clicked_tab_id.clone();
        match anchor {
            None => {
                self.selected_tab_ids = once(id);
                self.last_clicked_tab_id = Some(id.to_string());
            }
            Some(anchor) => {
                let a_idx = visible_order.iter().position(|x| x == &anchor);
                let b_idx = visible_order.iter().position(|x| x == id);
                if let (Some(a), Some(b)) = (a_idx, b_idx) {
                    let (lo, hi) = (a.min(b), a.max(b));
                    self.selected_tab_ids = visible_order[lo..=hi].iter().cloned().collect();
                    // Don't move `last_clicked_tab_id` — keep the anchor.
                } else {
                    self.selected_tab_ids = once(id);
                }
            }
        }
        self.active_tab_id = Some(id.to_string());
    }

    /// Collapse the selection to just `id` and re-anchor to it. Used by Esc and
    /// empty-sidebar-click. State-wise identical to [`SidebarTabSelection::replace`];
    /// it lives separately so call sites read for what they mean (plain click
    /// vs. selection-clear) (`SidebarTabSelection.swift:169-173`).
    pub fn collapse(&mut self, id: &str) {
        self.selected_tab_ids = once(id);
        self.last_clicked_tab_id = Some(id.to_string());
        self.active_tab_id = Some(id.to_string());
    }

    /// Drop everything. The only call site is the no-active-tab branch of the
    /// collapse-to-active path, which fires when an Esc / empty-area click
    /// happens while the tree is mid-shutdown (every project drained). Not a
    /// general-purpose "user cleared selection" entry point
    /// (`SidebarTabSelection.swift:182-186`).
    pub fn clear(&mut self) {
        self.selected_tab_ids = HashSet::new();
        self.last_clicked_tab_id = None;
        self.active_tab_id = None;
    }

    /// Mirror the active tab id from `TabModel`. Called by the view layer on
    /// every external active-tab change — session restore at launch, keyboard
    /// tab-cycling, socket-driven `claude newtab`, programmatic activation from
    /// `+` buttons. If the new active tab isn't in the selection set, collapses
    /// the set to it (the "external nav resets multi-selection" rule).
    ///
    /// The internal mutators set `active_tab_id` themselves, so by the time this
    /// fires from a tap path the new active id is already in the set and the
    /// contains-guard short-circuits (`SidebarTabSelection.swift:200-207`).
    pub fn sync_active_tab_id(&mut self, id: Option<&str>) {
        self.active_tab_id = id.map(|s| s.to_string());
        let Some(id) = id else { return };
        if !self.selected_tab_ids.contains(id) {
            self.selected_tab_ids = once(id);
            self.last_clicked_tab_id = Some(id.to_string());
        }
    }

    // MARK: - Query

    /// The selected tab ids.
    pub fn selected_tab_ids(&self) -> &HashSet<String> {
        &self.selected_tab_ids
    }

    /// The ⇧-range anchor, if any.
    pub fn last_clicked_tab_id(&self) -> Option<&str> {
        self.last_clicked_tab_id.as_deref()
    }

    /// The locally-mirrored active tab id, if any.
    pub fn active_tab_id(&self) -> Option<&str> {
        self.active_tab_id.as_deref()
    }

    /// Whether `id` is currently selected (`SidebarTabSelection.swift:211-213`).
    pub fn contains(&self, id: &str) -> bool {
        self.selected_tab_ids.contains(id)
    }

    /// Pure resolution of "which tabs should the menu act on" for a right-click
    /// on `clicked_id`. If the clicked tab is in the set, return the whole
    /// selection; otherwise return just the clicked tab. **No mutation** — this
    /// is read while building the context menu, so any state change here would
    /// loop the render. The visible "snap to clicked row" side effect happens
    /// via [`SidebarTabSelection::snap_if_right_click_outside`], which the menu's
    /// action handlers call before they run (`SidebarTabSelection.swift:226-231`).
    pub fn selection_ids_for_right_click_on(&self, clicked_id: &str) -> Vec<String> {
        if self.selected_tab_ids.contains(clicked_id) {
            self.selected_tab_ids.iter().cloned().collect()
        } else {
            vec![clicked_id.to_string()]
        }
    }

    /// Apply the Finder-style "right-click outside selection replaces it" rule.
    /// Called by menu action handlers right before they fire, so the selection
    /// visibly snaps to the clicked row when the user picked a menu item that
    /// wasn't already part of the selection (`SidebarTabSelection.swift:237-240`).
    pub fn snap_if_right_click_outside(&mut self, clicked_id: &str) {
        if self.selected_tab_ids.contains(clicked_id) {
            return;
        }
        self.replace(clicked_id);
    }

    // MARK: - Tree-change reconciliation

    /// Drop any selected ids no longer in `valid_ids`. Called after a tab is
    /// removed from the tree so the selection set never dangles. Also clears the
    /// anchor if it pointed at a removed tab — a subsequent ⇧-click then falls
    /// back to the empty-anchor branch in [`SidebarTabSelection::extend`] instead
    /// of silently no-op'ing on a stale id. Same treatment for `active_tab_id`:
    /// if the active tab itself was just removed, clear the local mirror so
    /// [`SidebarTabSelection::sync_active_tab_id`] (driven by the same dissolve
    /// cascade reassigning the model's active tab) can re-seed the invariant
    /// cleanly (`SidebarTabSelection.swift:254-262`).
    pub fn prune(&mut self, valid_ids: &HashSet<String>) {
        self.selected_tab_ids.retain(|id| valid_ids.contains(id));
        if let Some(anchor) = &self.last_clicked_tab_id {
            if !valid_ids.contains(anchor) {
                self.last_clicked_tab_id = None;
            }
        }
        if let Some(active) = &self.active_tab_id {
            if !valid_ids.contains(active) {
                self.active_tab_id = None;
            }
        }
    }
}

/// A single-element id set — the `selectedTabIds = [id]` Swift idiom.
fn once(id: &str) -> HashSet<String> {
    std::iter::once(id.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an id set from a slice, for terse assertions
    /// (`XCTAssertEqual(s.selectedTabIds, ["a", "b"])`).
    fn ids(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // MARK: - replace

    #[test]
    fn replace_collapses_to_single_id_and_sets_anchor_and_active() {
        let mut s = SidebarTabSelection::new();

        s.replace("tab-1");

        assert_eq!(s.selected_tab_ids(), &ids(&["tab-1"]));
        assert_eq!(s.last_clicked_tab_id(), Some("tab-1"));
        assert_eq!(
            s.active_tab_id(),
            Some("tab-1"),
            "replace must mark the new id as active so the view layer can call \
             select_tab without an extra round-trip through the observer"
        );
    }

    #[test]
    fn replace_overrides_prior_selection() {
        let mut s = SidebarTabSelection::new();
        s.replace("tab-1");
        s.toggle("tab-2");

        s.replace("tab-3");

        assert_eq!(s.selected_tab_ids(), &ids(&["tab-3"]));
        assert_eq!(s.last_clicked_tab_id(), Some("tab-3"));
        assert_eq!(s.active_tab_id(), Some("tab-3"));
    }

    // MARK: - toggle

    #[test]
    fn toggle_adds_absent_id_moves_anchor_returns_and_activates_id() {
        let mut s = SidebarTabSelection::new();
        s.replace("tab-1");

        let next = s.toggle("tab-2");

        assert_eq!(s.selected_tab_ids(), &ids(&["tab-1", "tab-2"]));
        assert_eq!(s.last_clicked_tab_id(), Some("tab-2"));
        assert_eq!(
            s.active_tab_id(),
            Some("tab-2"),
            "toggling in moves active to the toggled id (most-recently-clicked rule)"
        );
        assert_eq!(
            next.as_deref(),
            Some("tab-2"),
            "view layer needs the new active id to mirror to TabModel::select_tab"
        );
    }

    #[test]
    fn toggle_removes_non_active_id_moves_anchor_returns_none_keeps_active() {
        let mut s = SidebarTabSelection::new();
        s.replace("tab-1"); // active = tab-1
        s.toggle("tab-2"); // active = tab-2
        s.toggle("tab-3"); // active = tab-3, set = {1,2,3}

        let next = s.toggle("tab-1"); // remove non-active

        assert_eq!(s.selected_tab_ids(), &ids(&["tab-2", "tab-3"]));
        assert_eq!(
            s.last_clicked_tab_id(),
            Some("tab-1"),
            "anchor moves even when toggling out a non-active row (Finder)"
        );
        assert_eq!(
            s.active_tab_id(),
            Some("tab-3"),
            "active is unchanged when toggling out a non-active row"
        );
        assert_eq!(
            next, None,
            "no active change → return None so the view layer skips a redundant select_tab write"
        );
    }

    #[test]
    fn toggle_removes_active_with_others_promotes_first_returns_promoted() {
        let mut s = SidebarTabSelection::new();
        s.replace("tab-1"); // active = tab-1
        s.toggle("tab-2"); // active = tab-2, set = {1,2}

        let next = s.toggle("tab-2"); // remove active, others remain

        assert_eq!(s.selected_tab_ids(), &ids(&["tab-1"]));
        assert_eq!(s.last_clicked_tab_id(), Some("tab-2"));
        assert_eq!(
            s.active_tab_id(),
            Some("tab-1"),
            "toggling out the active tab while others remain promotes one of them to active"
        );
        assert_eq!(
            next.as_deref(),
            Some("tab-1"),
            "view layer needs the promoted id to mirror to TabModel::select_tab"
        );
    }

    #[test]
    fn toggle_removes_only_active_is_refused_returns_none() {
        // Refusing to empty the set when the user ⌘-clicks the only-and-active
        // selected row is intentional: it preserves the
        // "selection ⊇ {active_tab_id}" invariant and matches no useful Finder
        // behavior. User-visible effect: "⌘-click on the only-and-active row is
        // a no-op."
        let mut s = SidebarTabSelection::new();
        s.replace("tab-1");

        let next = s.toggle("tab-1");

        assert_eq!(
            s.selected_tab_ids(),
            &ids(&["tab-1"]),
            "set must NOT empty — invariant survives"
        );
        assert_eq!(
            s.active_tab_id(),
            Some("tab-1"),
            "active must NOT clear — invariant survives"
        );
        assert_eq!(
            s.last_clicked_tab_id(),
            Some("tab-1"),
            "anchor still moves to the clicked id"
        );
        assert_eq!(
            next, None,
            "no-op → return None so the view layer doesn't fire a redundant select_tab"
        );
    }

    // MARK: - extend

    #[test]
    fn extend_inclusive_between_anchor_and_current_moves_active_to_target() {
        let order = order(&["a", "b", "c", "d", "e"]);
        let mut s = SidebarTabSelection::new();
        s.replace("b");

        s.extend("d", &order);

        assert_eq!(s.selected_tab_ids(), &ids(&["b", "c", "d"]));
        assert_eq!(
            s.last_clicked_tab_id(),
            Some("b"),
            "shift-extend must not move the anchor"
        );
        assert_eq!(
            s.active_tab_id(),
            Some("d"),
            "shift-clicked tab becomes active"
        );
    }

    #[test]
    fn extend_target_before_anchor_handles_reverse_range() {
        let order = order(&["a", "b", "c", "d", "e"]);
        let mut s = SidebarTabSelection::new();
        s.replace("d");

        s.extend("b", &order);

        assert_eq!(s.selected_tab_ids(), &ids(&["b", "c", "d"]));
        assert_eq!(s.active_tab_id(), Some("b"));
    }

    #[test]
    fn extend_empty_anchor_treats_as_replace() {
        let mut s = SidebarTabSelection::new();
        // No prior click — anchor is None.

        s.extend("c", &order(&["a", "b", "c"]));

        assert_eq!(s.selected_tab_ids(), &ids(&["c"]));
        assert_eq!(s.last_clicked_tab_id(), Some("c"));
        assert_eq!(s.active_tab_id(), Some("c"));
    }

    #[test]
    fn extend_target_missing_from_order_falls_back_to_replace() {
        let mut s = SidebarTabSelection::new();
        s.replace("a");

        s.extend("z", &order(&["a", "b"]));

        assert_eq!(s.selected_tab_ids(), &ids(&["z"]));
        assert_eq!(s.active_tab_id(), Some("z"));
    }

    /// `navigable_sidebar_tab_ids` is a flat array spanning Terminals + every
    /// project group, so a ⇧-extend across group boundaries selects the whole
    /// intermediate run uniformly — the model itself has no notion of group
    /// separators.
    #[test]
    fn extend_across_project_groups_selects_contiguous_run() {
        let order = order(&[
            "terminals-main", // Terminals group
            "term-2",
            "claudeA", // Project A
            "claudeA-2",
            "claudeB", // Project B
        ]);
        let mut s = SidebarTabSelection::new();
        s.replace("terminals-main");

        s.extend("claudeA-2", &order);

        assert_eq!(
            s.selected_tab_ids(),
            &ids(&["terminals-main", "term-2", "claudeA", "claudeA-2"])
        );
    }

    // MARK: - selection_ids_for_right_click_on

    #[test]
    fn right_click_inside_selection_returns_all_selected() {
        let mut s = SidebarTabSelection::new();
        s.replace("a");
        s.toggle("b");
        s.toggle("c");

        let picked = s.selection_ids_for_right_click_on("b");

        assert_eq!(
            picked.into_iter().collect::<HashSet<_>>(),
            ids(&["a", "b", "c"])
        );
        // selection unchanged
        assert_eq!(s.selected_tab_ids(), &ids(&["a", "b", "c"]));
    }

    /// `selection_ids_for_right_click_on` is read while building the context
    /// menu — it must be a pure read; any mutation would loop the render.
    #[test]
    fn right_click_outside_selection_returns_clicked_only_and_does_not_mutate() {
        let mut s = SidebarTabSelection::new();
        s.replace("a");
        s.toggle("b");

        let picked = s.selection_ids_for_right_click_on("c");

        assert_eq!(picked, vec!["c".to_string()]);
        assert_eq!(
            s.selected_tab_ids(),
            &ids(&["a", "b"]),
            "selection_ids must be a pure read — no mutation during menu build"
        );
    }

    #[test]
    fn right_click_empty_selection_returns_clicked_only_and_does_not_mutate() {
        let s = SidebarTabSelection::new();

        let picked = s.selection_ids_for_right_click_on("x");

        assert_eq!(picked, vec!["x".to_string()]);
        assert!(
            s.selected_tab_ids().is_empty(),
            "empty selection must stay empty until a menu action snaps it"
        );
    }

    // MARK: - snap_if_right_click_outside

    #[test]
    fn snap_if_right_click_outside_outside_selection_replaces() {
        let mut s = SidebarTabSelection::new();
        s.replace("a");
        s.toggle("b");

        s.snap_if_right_click_outside("c");

        assert_eq!(s.selected_tab_ids(), &ids(&["c"]));
        assert_eq!(s.last_clicked_tab_id(), Some("c"));
        assert_eq!(s.active_tab_id(), Some("c"));
    }

    #[test]
    fn snap_if_right_click_outside_inside_selection_is_no_op() {
        let mut s = SidebarTabSelection::new();
        s.replace("a");
        s.toggle("b");
        s.toggle("c");

        s.snap_if_right_click_outside("b");

        assert_eq!(
            s.selected_tab_ids(),
            &ids(&["a", "b", "c"]),
            "right-click on a row already in the selection must not collapse it"
        );
    }

    // MARK: - collapse

    #[test]
    fn collapse_keeps_target_drops_rest_moves_anchor_and_active() {
        let mut s = SidebarTabSelection::new();
        s.replace("a");
        s.toggle("b");
        s.toggle("c");

        s.collapse("b");

        assert_eq!(s.selected_tab_ids(), &ids(&["b"]));
        assert_eq!(s.last_clicked_tab_id(), Some("b"));
        assert_eq!(s.active_tab_id(), Some("b"));
    }

    /// Pin the docstring claim "State-wise identical to `replace`" — keeps a
    /// future drift between the two methods from going unnoticed.
    #[test]
    fn collapse_is_state_equivalent_to_replace() {
        let mut r = SidebarTabSelection::new();
        let mut c = SidebarTabSelection::new();

        // Seed both with the same starting state.
        r.replace("x");
        r.toggle("y");
        c.replace("x");
        c.toggle("y");

        r.replace("z");
        c.collapse("z");

        assert_eq!(r.selected_tab_ids(), c.selected_tab_ids());
        assert_eq!(r.last_clicked_tab_id(), c.last_clicked_tab_id());
        assert_eq!(r.active_tab_id(), c.active_tab_id());
    }

    // MARK: - clear

    #[test]
    fn clear_resets_everything() {
        let mut s = SidebarTabSelection::new();
        s.replace("a");
        s.toggle("b");

        s.clear();

        assert!(s.selected_tab_ids().is_empty());
        assert_eq!(s.last_clicked_tab_id(), None);
        assert_eq!(s.active_tab_id(), None);
    }

    // MARK: - sync_active_tab_id

    #[test]
    fn sync_active_tab_id_in_selection_is_no_op_for_set_but_updates_active() {
        let mut s = SidebarTabSelection::new();
        s.replace("a");
        s.toggle("b"); // set = {a, b}, active = b

        s.sync_active_tab_id(Some("a"));

        assert_eq!(
            s.selected_tab_ids(),
            &ids(&["a", "b"]),
            "active already in set → set unchanged"
        );
        assert_eq!(
            s.active_tab_id(),
            Some("a"),
            "active mirror always updates"
        );
    }

    #[test]
    fn sync_active_tab_id_outside_selection_collapses_to_new_active() {
        // The canonical "external nav resets multi-selection" path: user has
        // multi-selected {a, b}, then keyboard ⌘N or socket newtab moves active
        // to a fresh tab. The observer sees the active id change, calls
        // sync_active_tab_id, and the set collapses to it.
        let mut s = SidebarTabSelection::new();
        s.replace("a");
        s.toggle("b"); // set = {a, b}, active = b

        s.sync_active_tab_id(Some("c"));

        assert_eq!(s.selected_tab_ids(), &ids(&["c"]));
        assert_eq!(
            s.last_clicked_tab_id(),
            Some("c"),
            "anchor re-seats on the freshly-collapsed point"
        );
        assert_eq!(s.active_tab_id(), Some("c"));
    }

    #[test]
    fn sync_active_tab_id_none_leaves_selection_alone_but_clears_active() {
        // Mid-shutdown / all-projects-empty: TabModel's active tab briefly goes
        // to None. We don't want that to wipe a multi-selection that's about to
        // be pruned by the dissolve cascade anyway — let prune do the shrinking.
        let mut s = SidebarTabSelection::new();
        s.replace("a");
        s.toggle("b");

        s.sync_active_tab_id(None);

        assert_eq!(s.selected_tab_ids(), &ids(&["a", "b"]));
        assert_eq!(s.active_tab_id(), None);
    }

    #[test]
    fn sync_active_tab_id_seeds_from_empty() {
        // Launch case: selection is empty (transient, not persisted), observer
        // fires with the restored active id, and the set gets seeded so the very
        // first ⇧-click has an anchor to extend from.
        let mut s = SidebarTabSelection::new();
        assert!(s.selected_tab_ids().is_empty());
        assert_eq!(s.last_clicked_tab_id(), None);

        s.sync_active_tab_id(Some("restored-tab"));

        assert_eq!(s.selected_tab_ids(), &ids(&["restored-tab"]));
        assert_eq!(s.last_clicked_tab_id(), Some("restored-tab"));
        assert_eq!(s.active_tab_id(), Some("restored-tab"));
    }

    // MARK: - prune

    #[test]
    fn prune_drops_removed_ids_keeps_valid_anchor_and_active() {
        let mut s = SidebarTabSelection::new();
        s.replace("a");
        s.toggle("b");
        s.toggle("c");
        // anchor is now "c", active is "c"

        s.prune(&ids(&["a", "c"]));

        assert_eq!(s.selected_tab_ids(), &ids(&["a", "c"]));
        assert_eq!(
            s.last_clicked_tab_id(),
            Some("c"),
            "valid anchor must not be cleared"
        );
        assert_eq!(
            s.active_tab_id(),
            Some("c"),
            "valid active must not be cleared"
        );
    }

    #[test]
    fn prune_clears_anchor_when_anchor_removed() {
        let mut s = SidebarTabSelection::new();
        s.replace("a");
        s.toggle("b");
        // anchor is now "b"

        s.prune(&ids(&["a"]));

        assert_eq!(s.selected_tab_ids(), &ids(&["a"]));
        assert_eq!(
            s.last_clicked_tab_id(),
            None,
            "anchor must clear when its tab is removed so a subsequent shift-click \
             hits the empty-anchor fallback in extend()"
        );
    }

    #[test]
    fn prune_clears_active_when_active_removed() {
        // The dissolve cascade reassigns the model's active tab *after* removing
        // the dissolved tab and pruning the selection. Clearing the local mirror
        // here lets the subsequent sync_active_tab_id re-seed the invariant
        // cleanly with the newly-promoted active id.
        let mut s = SidebarTabSelection::new();
        s.replace("a");
        s.toggle("b"); // active = b

        s.prune(&ids(&["a"]));

        assert_eq!(s.selected_tab_ids(), &ids(&["a"]));
        assert_eq!(s.active_tab_id(), None);
    }

    #[test]
    fn prune_intersection_empties_set_when_all_removed() {
        let mut s = SidebarTabSelection::new();
        s.replace("a");
        s.toggle("b");

        s.prune(&ids(&["x", "y"]));

        assert!(s.selected_tab_ids().is_empty());
        assert_eq!(s.last_clicked_tab_id(), None);
        assert_eq!(s.active_tab_id(), None);
    }

    /// Build an ordered id list for `extend`'s `visible_order` argument.
    fn order(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }
}
