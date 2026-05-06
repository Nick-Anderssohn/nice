//
//  SidebarTabSelectionTests.swift
//  NiceUnitTests
//
//  Coverage for the multi-tab sidebar selection model. The model is
//  pure logic — no SwiftUI / TabModel involvement — so tests stand it
//  up directly with hardcoded id strings. Mirrors the shape of
//  `FileBrowserSelectionTests` so the two stories stay in lock-step.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class SidebarTabSelectionTests: XCTestCase {

    // MARK: - replace

    func test_replace_collapsesToSingleId_andSetsAnchor() {
        let s = SidebarTabSelection()
        s.replace(with: "tab-1")

        XCTAssertEqual(s.selectedTabIds, ["tab-1"])
        XCTAssertEqual(s.lastClickedTabId, "tab-1")
    }

    func test_replace_overridesPriorSelection() {
        let s = SidebarTabSelection()
        s.replace(with: "tab-1")
        s.toggle("tab-2")

        s.replace(with: "tab-3")

        XCTAssertEqual(s.selectedTabIds, ["tab-3"])
        XCTAssertEqual(s.lastClickedTabId, "tab-3")
    }

    // MARK: - toggle

    func test_toggle_addsAbsentId_andMovesAnchor() {
        let s = SidebarTabSelection()
        s.replace(with: "tab-1")

        s.toggle("tab-2")

        XCTAssertEqual(s.selectedTabIds, ["tab-1", "tab-2"])
        XCTAssertEqual(s.lastClickedTabId, "tab-2")
    }

    func test_toggle_removesPresentId_butStillMovesAnchor() {
        let s = SidebarTabSelection()
        s.replace(with: "tab-1")
        s.toggle("tab-2")

        s.toggle("tab-2")

        XCTAssertEqual(s.selectedTabIds, ["tab-1"])
        XCTAssertEqual(s.lastClickedTabId, "tab-2",
                       "anchor still moves to the toggled id, even on remove (Finder)")
    }

    // MARK: - extend

    func test_extend_inclusiveBetweenAnchorAndCurrent() {
        let order = ["a", "b", "c", "d", "e"]
        let s = SidebarTabSelection()
        s.replace(with: "b")

        s.extend(through: "d", visibleOrder: order)

        XCTAssertEqual(s.selectedTabIds, ["b", "c", "d"])
        XCTAssertEqual(s.lastClickedTabId, "b",
                       "shift-extend must not move the anchor")
    }

    func test_extend_targetBeforeAnchor_handlesReverseRange() {
        let order = ["a", "b", "c", "d", "e"]
        let s = SidebarTabSelection()
        s.replace(with: "d")

        s.extend(through: "b", visibleOrder: order)

        XCTAssertEqual(s.selectedTabIds, ["b", "c", "d"])
    }

    func test_extend_emptyAnchor_treatsAsReplace() {
        let s = SidebarTabSelection()
        // No prior click — anchor is nil.

        s.extend(through: "c", visibleOrder: ["a", "b", "c"])

        XCTAssertEqual(s.selectedTabIds, ["c"])
        XCTAssertEqual(s.lastClickedTabId, "c")
    }

    func test_extend_targetMissingFromOrder_fallsBackToReplace() {
        let s = SidebarTabSelection()
        s.replace(with: "a")

        s.extend(through: "z", visibleOrder: ["a", "b"])

        XCTAssertEqual(s.selectedTabIds, ["z"])
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
        s.toggle("b")
        s.toggle("c")

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
        s.toggle("b")

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
        s.toggle("b")

        s.snapIfRightClickOutside("c")

        XCTAssertEqual(s.selectedTabIds, ["c"])
        XCTAssertEqual(s.lastClickedTabId, "c")
    }

    func test_snapIfRightClickOutside_insideSelection_isNoOp() {
        let s = SidebarTabSelection()
        s.replace(with: "a")
        s.toggle("b")
        s.toggle("c")

        s.snapIfRightClickOutside("b")

        XCTAssertEqual(s.selectedTabIds, ["a", "b", "c"],
                       "right-click on a row already in the selection must not collapse it")
    }

    // MARK: - collapse

    func test_collapse_keepsTarget_dropsRest_movesAnchor() {
        let s = SidebarTabSelection()
        s.replace(with: "a")
        s.toggle("b")
        s.toggle("c")

        s.collapse(to: "b")

        XCTAssertEqual(s.selectedTabIds, ["b"])
        XCTAssertEqual(s.lastClickedTabId, "b")
    }

    // MARK: - clear

    func test_clear_resetsBoth() {
        let s = SidebarTabSelection()
        s.replace(with: "a")
        s.toggle("b")

        s.clear()

        XCTAssertTrue(s.selectedTabIds.isEmpty)
        XCTAssertNil(s.lastClickedTabId)
    }

    // MARK: - prune

    func test_prune_dropsRemovedIds_keepsValidAnchor() {
        let s = SidebarTabSelection()
        s.replace(with: "a")
        s.toggle("b")
        s.toggle("c")
        // anchor is now "c"

        s.prune(validIds: ["a", "c"])

        XCTAssertEqual(s.selectedTabIds, ["a", "c"])
        XCTAssertEqual(s.lastClickedTabId, "c",
                       "valid anchor must not be cleared")
    }

    func test_prune_clearsAnchorWhenAnchorRemoved() {
        let s = SidebarTabSelection()
        s.replace(with: "a")
        s.toggle("b")
        // anchor is now "b"

        s.prune(validIds: ["a"])

        XCTAssertEqual(s.selectedTabIds, ["a"])
        XCTAssertNil(s.lastClickedTabId,
                     "anchor must clear when its tab is removed so a "
                     + "subsequent shift-click hits the empty-anchor "
                     + "fallback in extend()")
    }

    func test_prune_intersectionEmptiesSet_whenAllRemoved() {
        let s = SidebarTabSelection()
        s.replace(with: "a")
        s.toggle("b")

        s.prune(validIds: ["x", "y"])

        XCTAssertTrue(s.selectedTabIds.isEmpty)
        XCTAssertNil(s.lastClickedTabId)
    }
}
