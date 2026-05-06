//
//  SidebarTabSelectionTests.swift
//  NiceUnitTests
//
//  Coverage for the multi-tab sidebar selection model. The model is
//  pure logic — no SwiftUI / TabModel involvement — so tests stand it
//  up directly with hardcoded id strings. Mirrors the shape of
//  `FileBrowserSelectionTests`, plus the active-tab invariant
//  enforcement that this model owns (`FileBrowserSelection` has no
//  analogous "active path" concept, so the divergence is intentional).
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class SidebarTabSelectionTests: XCTestCase {

    // MARK: - replace

    func test_replace_collapsesToSingleId_andSetsAnchorAndActive() {
        let s = SidebarTabSelection()

        s.replace(with: "tab-1")

        XCTAssertEqual(s.selectedTabIds, ["tab-1"])
        XCTAssertEqual(s.lastClickedTabId, "tab-1")
        XCTAssertEqual(s.activeTabId, "tab-1",
                       "replace must mark the new id as active so the "
                       + "view layer can call selectTab without an "
                       + "extra round-trip through the observer")
    }

    func test_replace_overridesPriorSelection() {
        let s = SidebarTabSelection()
        s.replace(with: "tab-1")
        _ = s.toggle("tab-2")

        s.replace(with: "tab-3")

        XCTAssertEqual(s.selectedTabIds, ["tab-3"])
        XCTAssertEqual(s.lastClickedTabId, "tab-3")
        XCTAssertEqual(s.activeTabId, "tab-3")
    }

    // MARK: - toggle

    func test_toggle_addsAbsentId_movesAnchor_returnsAndActivatesId() {
        let s = SidebarTabSelection()
        s.replace(with: "tab-1")

        let next = s.toggle("tab-2")

        XCTAssertEqual(s.selectedTabIds, ["tab-1", "tab-2"])
        XCTAssertEqual(s.lastClickedTabId, "tab-2")
        XCTAssertEqual(s.activeTabId, "tab-2",
                       "toggling in moves active to the toggled id "
                       + "(most-recently-clicked rule)")
        XCTAssertEqual(next, "tab-2",
                       "view layer needs the new active id to mirror "
                       + "to TabModel.selectTab")
    }

    func test_toggle_removesNonActiveId_movesAnchor_returnsNil_keepsActive() {
        let s = SidebarTabSelection()
        s.replace(with: "tab-1")          // active = tab-1
        _ = s.toggle("tab-2")             // active = tab-2
        _ = s.toggle("tab-3")             // active = tab-3, set = {1,2,3}

        let next = s.toggle("tab-1")      // remove non-active

        XCTAssertEqual(s.selectedTabIds, ["tab-2", "tab-3"])
        XCTAssertEqual(s.lastClickedTabId, "tab-1",
                       "anchor moves even when toggling out a non-"
                       + "active row (Finder)")
        XCTAssertEqual(s.activeTabId, "tab-3",
                       "active is unchanged when toggling out a non-"
                       + "active row")
        XCTAssertNil(next,
                     "no active change → return nil so the view layer "
                     + "skips a redundant selectTab write")
    }

    func test_toggle_removesActiveWithOthers_promotesFirst_returnsPromoted() {
        let s = SidebarTabSelection()
        s.replace(with: "tab-1")          // active = tab-1
        _ = s.toggle("tab-2")             // active = tab-2, set = {1,2}

        let next = s.toggle("tab-2")      // remove active, others remain

        XCTAssertEqual(s.selectedTabIds, ["tab-1"])
        XCTAssertEqual(s.lastClickedTabId, "tab-2")
        XCTAssertEqual(s.activeTabId, "tab-1",
                       "toggling out the active tab while others "
                       + "remain promotes one of them to active")
        XCTAssertEqual(next, "tab-1",
                       "view layer needs the promoted id to mirror "
                       + "to TabModel.selectTab")
    }

    func test_toggle_removesOnlyActive_isRefused_returnsNil() {
        // Refusing to empty the set when the user Cmd-clicks the
        // only-and-active selected row is intentional: it preserves
        // the "selection ⊇ {activeTabId}" invariant and matches no
        // useful Finder behavior. The user-visible effect is "Cmd-
        // click on the only-and-active row is a no-op."
        let s = SidebarTabSelection()
        s.replace(with: "tab-1")

        let next = s.toggle("tab-1")

        XCTAssertEqual(s.selectedTabIds, ["tab-1"],
                       "set must NOT empty — invariant survives")
        XCTAssertEqual(s.activeTabId, "tab-1",
                       "active must NOT clear — invariant survives")
        XCTAssertEqual(s.lastClickedTabId, "tab-1",
                       "anchor still moves to the clicked id")
        XCTAssertNil(next,
                     "no-op → return nil so the view layer doesn't "
                     + "fire a redundant selectTab")
    }

    // MARK: - extend

    func test_extend_inclusiveBetweenAnchorAndCurrent_movesActiveToTarget() {
        let order = ["a", "b", "c", "d", "e"]
        let s = SidebarTabSelection()
        s.replace(with: "b")

        s.extend(through: "d", visibleOrder: order)

        XCTAssertEqual(s.selectedTabIds, ["b", "c", "d"])
        XCTAssertEqual(s.lastClickedTabId, "b",
                       "shift-extend must not move the anchor")
        XCTAssertEqual(s.activeTabId, "d",
                       "shift-clicked tab becomes active")
    }

    func test_extend_targetBeforeAnchor_handlesReverseRange() {
        let order = ["a", "b", "c", "d", "e"]
        let s = SidebarTabSelection()
        s.replace(with: "d")

        s.extend(through: "b", visibleOrder: order)

        XCTAssertEqual(s.selectedTabIds, ["b", "c", "d"])
        XCTAssertEqual(s.activeTabId, "b")
    }

    func test_extend_emptyAnchor_treatsAsReplace() {
        let s = SidebarTabSelection()
        // No prior click — anchor is nil.

        s.extend(through: "c", visibleOrder: ["a", "b", "c"])

        XCTAssertEqual(s.selectedTabIds, ["c"])
        XCTAssertEqual(s.lastClickedTabId, "c")
        XCTAssertEqual(s.activeTabId, "c")
    }

    func test_extend_targetMissingFromOrder_fallsBackToReplace() {
        let s = SidebarTabSelection()
        s.replace(with: "a")

        s.extend(through: "z", visibleOrder: ["a", "b"])

        XCTAssertEqual(s.selectedTabIds, ["z"])
        XCTAssertEqual(s.activeTabId, "z")
    }

    /// `navigableSidebarTabIds` is a flat array spanning Terminals + every
    /// project group, so a shift-extend across group boundaries selects
    /// the whole intermediate run uniformly. Confirms that the model
    /// itself has no notion of group separators — the visible order is
    /// the only thing that matters.
    func test_extend_acrossProjectGroups_selectsContiguousRun() {
        let order = [
            "terminals-main",   // Terminals group
            "term-2",
            "claudeA",          // Project A
            "claudeA-2",
            "claudeB",          // Project B
        ]
        let s = SidebarTabSelection()
        s.replace(with: "terminals-main")

        s.extend(through: "claudeA-2", visibleOrder: order)

        XCTAssertEqual(s.selectedTabIds,
                       ["terminals-main", "term-2", "claudeA", "claudeA-2"])
    }

    // MARK: - selectionIds(forRightClickOn:)

    func test_rightClick_insideSelection_returnsAllSelected() {
        let s = SidebarTabSelection()
        s.replace(with: "a")
        _ = s.toggle("b")
        _ = s.toggle("c")

        let ids = s.selectionIds(forRightClickOn: "b")

        XCTAssertEqual(Set(ids), ["a", "b", "c"])
        // selection unchanged
        XCTAssertEqual(s.selectedTabIds, ["a", "b", "c"])
    }

    /// `selectionIds` is read from inside SwiftUI's `.contextMenu` view
    /// builder, which SwiftUI evaluates as part of body. The function
    /// must be a pure read — any mutation here would loop the render.
    func test_rightClick_outsideSelection_returnsClickedOnly_andDoesNotMutate() {
        let s = SidebarTabSelection()
        s.replace(with: "a")
        _ = s.toggle("b")

        let ids = s.selectionIds(forRightClickOn: "c")

        XCTAssertEqual(ids, ["c"])
        XCTAssertEqual(s.selectedTabIds, ["a", "b"],
                       "selectionIds must be a pure read — no mutation during body eval")
    }

    func test_rightClick_emptySelection_returnsClickedOnly_andDoesNotMutate() {
        let s = SidebarTabSelection()

        let ids = s.selectionIds(forRightClickOn: "x")

        XCTAssertEqual(ids, ["x"])
        XCTAssertTrue(s.selectedTabIds.isEmpty,
                      "empty selection must stay empty until a menu action snaps it")
    }

    // MARK: - snapIfRightClickOutside

    func test_snapIfRightClickOutside_outsideSelection_replaces() {
        let s = SidebarTabSelection()
        s.replace(with: "a")
        _ = s.toggle("b")

        s.snapIfRightClickOutside("c")

        XCTAssertEqual(s.selectedTabIds, ["c"])
        XCTAssertEqual(s.lastClickedTabId, "c")
        XCTAssertEqual(s.activeTabId, "c")
    }

    func test_snapIfRightClickOutside_insideSelection_isNoOp() {
        let s = SidebarTabSelection()
        s.replace(with: "a")
        _ = s.toggle("b")
        _ = s.toggle("c")

        s.snapIfRightClickOutside("b")

        XCTAssertEqual(s.selectedTabIds, ["a", "b", "c"],
                       "right-click on a row already in the selection must not collapse it")
    }

    // MARK: - collapse

    func test_collapse_keepsTarget_dropsRest_movesAnchor_andActive() {
        let s = SidebarTabSelection()
        s.replace(with: "a")
        _ = s.toggle("b")
        _ = s.toggle("c")

        s.collapse(to: "b")

        XCTAssertEqual(s.selectedTabIds, ["b"])
        XCTAssertEqual(s.lastClickedTabId, "b")
        XCTAssertEqual(s.activeTabId, "b")
    }

    /// Pin the docstring claim "State-wise this is identical to
    /// `replace(with:)`" — keeps a future drift between the two
    /// methods from going unnoticed.
    func test_collapse_isStateEquivalentToReplace() {
        let r = SidebarTabSelection()
        let c = SidebarTabSelection()

        // Seed both with the same starting state.
        r.replace(with: "x")
        _ = r.toggle("y")
        c.replace(with: "x")
        _ = c.toggle("y")

        r.replace(with: "z")
        c.collapse(to: "z")

        XCTAssertEqual(r.selectedTabIds, c.selectedTabIds)
        XCTAssertEqual(r.lastClickedTabId, c.lastClickedTabId)
        XCTAssertEqual(r.activeTabId, c.activeTabId)
    }

    // MARK: - clear

    func test_clear_resetsEverything() {
        let s = SidebarTabSelection()
        s.replace(with: "a")
        _ = s.toggle("b")

        s.clear()

        XCTAssertTrue(s.selectedTabIds.isEmpty)
        XCTAssertNil(s.lastClickedTabId)
        XCTAssertNil(s.activeTabId)
    }

    // MARK: - syncActiveTabId

    func test_syncActiveTabId_inSelection_isNoOpForSet_butUpdatesActive() {
        let s = SidebarTabSelection()
        s.replace(with: "a")
        _ = s.toggle("b")            // set = {a, b}, active = b

        s.syncActiveTabId("a")

        XCTAssertEqual(s.selectedTabIds, ["a", "b"],
                       "active already in set → set unchanged")
        XCTAssertEqual(s.activeTabId, "a",
                       "active mirror always updates")
    }

    func test_syncActiveTabId_outsideSelection_collapsesToNewActive() {
        // The canonical "external nav resets multi-selection" path:
        // user has multi-selected {a, b}, then keyboard ⌘N or socket
        // newtab moves active to a fresh tab. The observer sees the
        // active id change, calls syncActiveTabId, and the set
        // collapses to it.
        let s = SidebarTabSelection()
        s.replace(with: "a")
        _ = s.toggle("b")            // set = {a, b}, active = b

        s.syncActiveTabId("c")

        XCTAssertEqual(s.selectedTabIds, ["c"])
        XCTAssertEqual(s.lastClickedTabId, "c",
                       "anchor re-seats on the freshly-collapsed point")
        XCTAssertEqual(s.activeTabId, "c")
    }

    func test_syncActiveTabId_nilLeavesSelectionAlone_butClearsActive() {
        // Mid-shutdown / all-projects-empty: TabModel.activeTabId
        // briefly goes to nil. We don't want that to wipe a multi-
        // selection that's about to be pruned by the dissolve
        // cascade anyway — let prune do the shrinking.
        let s = SidebarTabSelection()
        s.replace(with: "a")
        _ = s.toggle("b")

        s.syncActiveTabId(nil)

        XCTAssertEqual(s.selectedTabIds, ["a", "b"])
        XCTAssertNil(s.activeTabId)
    }

    func test_syncActiveTabId_seedsFromEmpty() {
        // Launch case: selection is empty (transient, not persisted),
        // observer fires with the restored active id, and the set
        // gets seeded so the very first Shift-click has an anchor
        // to extend from.
        let s = SidebarTabSelection()
        XCTAssertTrue(s.selectedTabIds.isEmpty)
        XCTAssertNil(s.lastClickedTabId)

        s.syncActiveTabId("restored-tab")

        XCTAssertEqual(s.selectedTabIds, ["restored-tab"])
        XCTAssertEqual(s.lastClickedTabId, "restored-tab")
        XCTAssertEqual(s.activeTabId, "restored-tab")
    }

    // MARK: - prune

    func test_prune_dropsRemovedIds_keepsValidAnchorAndActive() {
        let s = SidebarTabSelection()
        s.replace(with: "a")
        _ = s.toggle("b")
        _ = s.toggle("c")
        // anchor is now "c", active is "c"

        s.prune(validIds: ["a", "c"])

        XCTAssertEqual(s.selectedTabIds, ["a", "c"])
        XCTAssertEqual(s.lastClickedTabId, "c",
                       "valid anchor must not be cleared")
        XCTAssertEqual(s.activeTabId, "c",
                       "valid active must not be cleared")
    }

    func test_prune_clearsAnchorWhenAnchorRemoved() {
        let s = SidebarTabSelection()
        s.replace(with: "a")
        _ = s.toggle("b")
        // anchor is now "b"

        s.prune(validIds: ["a"])

        XCTAssertEqual(s.selectedTabIds, ["a"])
        XCTAssertNil(s.lastClickedTabId,
                     "anchor must clear when its tab is removed so a "
                     + "subsequent shift-click hits the empty-anchor "
                     + "fallback in extend()")
    }

    func test_prune_clearsActiveWhenActiveRemoved() {
        // `finalizeDissolvedTab` reassigns `TabModel.activeTabId`
        // *after* removing the dissolved tab and pruning the
        // selection. Clearing the local mirror here lets the
        // subsequent `syncActiveTabId(_:)` re-seed the invariant
        // cleanly with the newly-promoted active id.
        let s = SidebarTabSelection()
        s.replace(with: "a")
        _ = s.toggle("b")           // active = b

        s.prune(validIds: ["a"])

        XCTAssertEqual(s.selectedTabIds, ["a"])
        XCTAssertNil(s.activeTabId)
    }

    func test_prune_intersectionEmptiesSet_whenAllRemoved() {
        let s = SidebarTabSelection()
        s.replace(with: "a")
        _ = s.toggle("b")

        s.prune(validIds: ["x", "y"])

        XCTAssertTrue(s.selectedTabIds.isEmpty)
        XCTAssertNil(s.lastClickedTabId)
        XCTAssertNil(s.activeTabId)
    }
}
