//
//  SidebarTabSelection.swift
//  Nice
//
//  Sidebar multi-tab selection. Owned by `AppState`, injected into the
//  SwiftUI environment so `TabRow` and the SidebarView body can both
//  read and mutate it. Three pieces of state, mirroring the pattern in
//  `FileBrowserSelection`:
//
//    • `selectedTabIds` — the multi-selection set the right-click menu
//      acts on when the right-clicked tab is in the set.
//    • `lastClickedTabId` — anchor for Shift-range selection. Set by
//      plain click and Cmd-click; not moved by Shift-click (Finder
//      keeps the original anchor across multiple range extensions).
//    • `selectionIds(forRightClickOn:)` — the "snap to clicked tab if
//      it's outside the selection" policy, encapsulated here so the
//      view layer doesn't have to repeat the rule.
//
//  Selection is a transient UI concept — we deliberately do NOT route
//  mutations through `TabModel.onTreeMutation`, so it never reaches
//  `WindowSession.scheduleSessionSave`. Restart wipes it; only the
//  active-tab id (held on `TabModel`) is persisted.
//
//  No SwiftUI / view dependency — fully unit-testable.
//

import Foundation

@MainActor
@Observable
final class SidebarTabSelection {
    /// Tab ids currently selected. Empty by default. The view layer is
    /// responsible for keeping this set a superset of `{activeTabId}`
    /// whenever a tab is active — the selection model itself is
    /// agnostic of `TabModel` (same separation of concerns as
    /// `FileBrowserSelection`).
    private(set) var selectedTabIds: Set<String> = []

    /// Anchor for Shift-range. Set on plain click and Cmd-click; not
    /// moved by Shift-click. Cleared by `prune` if the anchor tab was
    /// removed from the tree.
    private(set) var lastClickedTabId: String?

    init() {}

    // MARK: - Mutation

    /// Plain click. Collapse the selection to exactly `{id}` and move
    /// the anchor to it.
    func replace(with id: String) {
        selectedTabIds = [id]
        lastClickedTabId = id
    }

    /// Cmd-click. Toggle `id` in/out of the set; move the anchor to
    /// `id` either direction (matches Finder).
    func toggle(_ id: String) {
        if selectedTabIds.contains(id) {
            selectedTabIds.remove(id)
        } else {
            selectedTabIds.insert(id)
        }
        lastClickedTabId = id
    }

    /// Shift-click. Extend the selection to span from `lastClickedTabId`
    /// to `id`, inclusive, using the given visible row order. If there
    /// is no anchor yet, behaves like `replace(with: id)`. If either the
    /// anchor or the target is missing from `visibleOrder`, falls back
    /// to a plain replace so the selection still moves. The anchor is
    /// *not* updated — Finder keeps the original anchor across multiple
    /// range extensions.
    func extend(through id: String, visibleOrder: [String]) {
        guard let anchor = lastClickedTabId else {
            selectedTabIds = [id]
            lastClickedTabId = id
            return
        }
        guard let aIdx = visibleOrder.firstIndex(of: anchor),
              let bIdx = visibleOrder.firstIndex(of: id) else {
            selectedTabIds = [id]
            return
        }
        let lo = min(aIdx, bIdx)
        let hi = max(aIdx, bIdx)
        selectedTabIds = Set(visibleOrder[lo...hi])
        // Don't move `lastClickedTabId` — keep the anchor.
    }

    /// Collapse the selection to just `id` and re-anchor to it. Used by
    /// Esc and empty-sidebar-click — the active tab stays whatever it
    /// already was (so we don't call `selectTab` from here), we just
    /// drop the rest of the multi-selection. Anchor moves to `id` so a
    /// subsequent Shift-click extends from the freshly-collapsed point
    /// rather than from a tab that's no longer in the set.
    ///
    /// State-wise this is identical to `replace(with:)`; it lives
    /// separately so the call sites read for what they mean (plain
    /// click vs. selection-clear) without making readers cross-check
    /// the active-tab side effect.
    func collapse(to id: String) {
        selectedTabIds = [id]
        lastClickedTabId = id
    }

    /// Drop everything. Used only when the tree itself empties — e.g.
    /// the all-projects-empty path on shutdown. Normal user actions
    /// either replace, toggle, or collapse.
    func clear() {
        selectedTabIds = []
        lastClickedTabId = nil
    }

    // MARK: - Query

    func contains(_ id: String) -> Bool {
        selectedTabIds.contains(id)
    }

    /// Pure resolution of "which tabs should the menu act on" for a
    /// right-click on `clickedId`. If the clicked tab is in the set,
    /// return the whole selection; otherwise return just the clicked
    /// tab. No state mutation — this is called from inside SwiftUI's
    /// `.contextMenu` view builder, which SwiftUI evaluates as part of
    /// body. Any `objectWillChange` fired here would loop the render.
    ///
    /// The visible "snap to clicked row" side effect happens via
    /// `snapIfRightClickOutside(_:)`, which the menu's action buttons
    /// call before they run.
    func selectionIds(forRightClickOn clickedId: String) -> [String] {
        if selectedTabIds.contains(clickedId) {
            return Array(selectedTabIds)
        }
        return [clickedId]
    }

    /// Apply the Finder-style "right-click outside selection replaces
    /// it" rule. Called by menu action handlers right before they fire,
    /// so the selection visibly snaps to the clicked row when the user
    /// picked a menu item that wasn't already part of the selection.
    func snapIfRightClickOutside(_ clickedId: String) {
        guard !selectedTabIds.contains(clickedId) else { return }
        replace(with: clickedId)
    }

    // MARK: - Tree-change reconciliation

    /// Drop any selected ids no longer in `validIds`. Called by
    /// `AppState.finalizeDissolvedTab` after a tab is removed from the
    /// tree, so the selection set never dangles. Also clears the
    /// anchor if it pointed at a removed tab — a subsequent Shift-click
    /// then falls back to the empty-anchor branch in `extend` instead
    /// of silently no-op'ing on a stale id.
    func prune(validIds: Set<String>) {
        selectedTabIds = selectedTabIds.intersection(validIds)
        if let anchor = lastClickedTabId, !validIds.contains(anchor) {
            lastClickedTabId = nil
        }
    }
}
