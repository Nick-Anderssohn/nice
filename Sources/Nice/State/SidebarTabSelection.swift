//
//  SidebarTabSelection.swift
//  Nice
//
//  Sidebar multi-tab selection. Owned by `AppState`, injected into the
//  SwiftUI environment so `TabRow` and the `SidebarView` body can both
//  read and mutate it.
//
//  Owns four pieces of state, plus the "selection ⊇ {activeTabId}"
//  invariant:
//
//    • `selectedTabIds` — the multi-selection set the right-click
//      menu acts on when the right-clicked tab is in the set. Always
//      contains `activeTabId` when `activeTabId` is non-nil.
//    • `lastClickedTabId` — anchor for Shift-range selection. Set by
//      plain click and Cmd-click; not moved by Shift-click (Finder
//      keeps the original anchor across multiple range extensions).
//    • `activeTabId` — local mirror of `TabModel.activeTabId`. The
//      view layer's `.onChange(of: tabs.activeTabId, initial: true)`
//      observer feeds this via `syncActiveTabId(_:)` on every
//      external active-tab change (session restore at launch,
//      keyboard ⌘1..⌘9, socket-driven `claude newtab`); internal
//      mutators set it eagerly so they can self-check the invariant
//      without a round-trip through TabModel.
//    • `selectionIds(forRightClickOn:)` — encapsulates the "snap to
//      clicked tab if it's outside the selection" policy so the view
//      layer doesn't have to repeat the rule.
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
    /// Tab ids currently selected. Empty by default; otherwise always
    /// a superset of `{activeTabId}` when `activeTabId` is non-nil.
    /// The model maintains the invariant itself — every mutator
    /// either re-establishes it on the way out or refuses the
    /// operation (Cmd-click on the only-and-active row is a no-op).
    private(set) var selectedTabIds: Set<String> = []

    /// Anchor for Shift-range. Set on plain click and Cmd-click; not
    /// moved by Shift-click. Cleared by `prune` if the anchor tab was
    /// removed from the tree.
    private(set) var lastClickedTabId: String?

    /// Local mirror of `TabModel.activeTabId`, used to maintain the
    /// "selectedTabIds ⊇ {activeTabId}" invariant during model
    /// mutations. Synced from the source of truth (`TabModel`) by
    /// the view layer's `.onChange(of: tabs.activeTabId,
    /// initial: true)` observer via `syncActiveTabId(_:)`. Internal
    /// mutators also update it eagerly; the resulting double-write
    /// from a plain/cmd/shift click is idempotent (same value), and
    /// keeps the model self-consistent if a test exercises it
    /// without a TabModel attached.
    private(set) var activeTabId: String?

    init() {}

    // MARK: - Mutation

    /// Plain click. Collapse to `{id}`, move the anchor to `id`, and
    /// mark `id` as the new active tab. The view layer follows this
    /// with `tabs.selectTab(id)` to mirror the active-tab change to
    /// `TabModel`.
    func replace(with id: String) {
        selectedTabIds = [id]
        lastClickedTabId = id
        activeTabId = id
    }

    /// Cmd-click. Toggle `id` in/out, moving the anchor to `id`
    /// either direction (Finder), and re-establish the active-
    /// superset invariant.
    ///
    /// Returns the tab id that should now be active, or `nil` if the
    /// active tab is unchanged. The view layer calls
    /// `tabs.selectTab(returnedId)` only when this is non-nil — a
    /// `nil` return for a Cmd-click on the only-and-active selected
    /// row keeps the model and `TabModel` in sync without a
    /// redundant `selectTab` write that would still fire didSet.
    @discardableResult
    func toggle(_ id: String) -> String? {
        if selectedTabIds.contains(id) {
            lastClickedTabId = id
            if activeTabId == id {
                if selectedTabIds.count > 1 {
                    // Toggling out the active tab while others are
                    // selected: drop it, promote any remaining
                    // member to active. `Set.first` is non-
                    // deterministic but acceptable — the user just
                    // dropped the previously-active row and any of
                    // the others is a defensible "next active."
                    selectedTabIds.remove(id)
                    let next = selectedTabIds.first!  // count > 1, safe
                    activeTabId = next
                    return next
                } else {
                    // Toggling out the only-and-active selected row
                    // would empty the set — that violates the
                    // invariant and matches no useful Finder
                    // behavior. Keep the row selected and active.
                    // The `lastClickedTabId = id` above still fires
                    // (idempotent), so subsequent shift-click
                    // anchors here.
                    return nil
                }
            } else {
                // Toggling out a non-active selected row: just drop
                // it. Active tab unchanged.
                selectedTabIds.remove(id)
                return nil
            }
        } else {
            selectedTabIds.insert(id)
            lastClickedTabId = id
            activeTabId = id
            return id
        }
    }

    /// Shift-click. Extend the selection to span from `lastClickedTabId`
    /// to `id`, inclusive, using the given visible row order. If
    /// there is no anchor yet, behaves like `replace(with: id)`. If
    /// either the anchor or the target is missing from `visibleOrder`,
    /// falls back to a plain replace so the selection still moves.
    /// The anchor is *not* updated — Finder keeps the original anchor
    /// across multiple range extensions.
    ///
    /// `id` always becomes the new active tab; the view layer mirrors
    /// that to `TabModel.selectTab`.
    func extend(through id: String, visibleOrder: [String]) {
        defer { activeTabId = id }
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

    /// Collapse the selection to just `id` and re-anchor to it. Used
    /// by Esc and empty-sidebar-click. The active tab is also set to
    /// `id` so the model stays self-consistent — but the view layer
    /// is responsible for calling `tabs.selectTab(id)` when it wants
    /// the change reflected in `TabModel` (the existing call sites
    /// pass the already-active id, so the additional `selectTab`
    /// would be redundant).
    ///
    /// State-wise this is identical to `replace(with:)`; it lives
    /// separately so the call sites read for what they mean (plain
    /// click vs. selection-clear) without making readers cross-check
    /// the active-tab side effect.
    func collapse(to id: String) {
        selectedTabIds = [id]
        lastClickedTabId = id
        activeTabId = id
    }

    /// Drop everything. The only call site is
    /// `SidebarView.collapseSelectionToActive()`'s no-active-tab
    /// branch, which fires when an Esc / empty-area click happens
    /// while the tree is mid-shutdown (every project drained). Not a
    /// general-purpose "user cleared selection" entry point —
    /// regular UI paths use `replace(with:)`, `toggle(_:)`, or
    /// `collapse(to:)`.
    func clear() {
        selectedTabIds = []
        lastClickedTabId = nil
        activeTabId = nil
    }

    /// Mirror the active tab id from `TabModel.activeTabId`. Called
    /// by the view layer's `.onChange(of:initial:)` observer on
    /// every external active-tab change — session restore at launch,
    /// keyboard tab-cycling, socket-driven `claude newtab`,
    /// programmatic activation from `+` buttons. If the new active
    /// tab isn't in the selection set, collapses the set to it (the
    /// "external nav resets multi-selection" rule).
    ///
    /// Internal mutators (`replace` / `toggle` / `extend` /
    /// `collapse`) set `activeTabId` themselves, so by the time this
    /// callback fires from a tap path the new active id is already
    /// in the set and the contains-guard short-circuits.
    func syncActiveTabId(_ id: String?) {
        activeTabId = id
        guard let id else { return }
        if !selectedTabIds.contains(id) {
            selectedTabIds = [id]
            lastClickedTabId = id
        }
    }

    // MARK: - Query

    func contains(_ id: String) -> Bool {
        selectedTabIds.contains(id)
    }

    /// Pure resolution of "which tabs should the menu act on" for a
    /// right-click on `clickedId`. If the clicked tab is in the set,
    /// return the whole selection; otherwise return just the clicked
    /// tab. No state mutation — this is called from inside SwiftUI's
    /// `.contextMenu` view builder, which SwiftUI evaluates as part
    /// of body. Any `objectWillChange` fired here would loop the
    /// render.
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
    /// of silently no-op'ing on a stale id. Same treatment for
    /// `activeTabId`: if the active tab itself was just removed,
    /// clear the local mirror so `syncActiveTabId(_:)` (driven by
    /// the same dissolve cascade reassigning `TabModel.activeTabId`)
    /// can re-seed the invariant cleanly.
    func prune(validIds: Set<String>) {
        selectedTabIds = selectedTabIds.intersection(validIds)
        if let anchor = lastClickedTabId, !validIds.contains(anchor) {
            lastClickedTabId = nil
        }
        if let active = activeTabId, !validIds.contains(active) {
            activeTabId = nil
        }
    }
}
