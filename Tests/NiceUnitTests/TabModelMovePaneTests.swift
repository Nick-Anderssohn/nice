//
//  TabModelMovePaneTests.swift
//  NiceUnitTests
//
//  Tests for TabModel.movePane and TabModel.wouldMovePane — the
//  pane-pill drag-to-reorder helper and its no-op predicate. Mirrors
//  TabModelReorderTests (the direct analog for tabs) in fixture style,
//  assertion pattern, and coverage approach.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class TabModelMovePaneTests: XCTestCase {

    private var appState: AppState!

    // MARK: - Helpers

    /// The project and tab seeded by setUp. Panes are [p0, p1, p2].
    private let projectId = "mp"
    private let tabId     = "mp-tab"
    private let p0        = "mp-tab-p0"
    private let p1        = "mp-tab-p1"
    private let p2        = "mp-tab-p2"

    // Seed inline in setUp (not via a @MainActor helper call): calling an
    // actor-isolated instance method from the `setUp()` override trips
    // Swift 6's "Sending 'self' risks causing data races". Every passing
    // sibling test (TabModelReorderTests, TabModelRenameTests) keeps setUp
    // to inline work for the same reason.
    override func setUp() {
        super.setUp()
        appState = AppState()
        let tab = Tab(
            id: tabId,
            title: "Move-pane test",
            cwd: "/tmp/mp",
            panes: [
                Pane(id: p0, title: "Terminal 1", kind: .terminal),
                Pane(id: p1, title: "Terminal 2", kind: .terminal),
                Pane(id: p2, title: "Terminal 3", kind: .terminal),
            ],
            activePaneId: p0
        )
        let project = Project(id: projectId, name: "MP", path: "/tmp/mp", tabs: [tab])
        appState.tabs.projects = [appState.tabs.projects[0], project]
    }

    override func tearDown() {
        appState = nil
        super.tearDown()
    }

    private func paneIds() -> [String] {
        appState.tabs.tab(for: tabId)?.panes.map(\.id) ?? []
    }

    // MARK: - movePane reorder cases

    func test_movePane_before_movesSourceIntoTargetSlot() {
        // [p0, p1, p2] — drop p2 before p0 → [p2, p0, p1]
        appState.tabs.movePane(p2, inTab: tabId, relativeTo: p0, placeAfter: false)
        XCTAssertEqual(paneIds(), [p2, p0, p1])
    }

    func test_movePane_after_landsJustPastTarget() {
        // [p0, p1, p2] — drop p0 after p1 → [p1, p0, p2]
        appState.tabs.movePane(p0, inTab: tabId, relativeTo: p1, placeAfter: true)
        XCTAssertEqual(paneIds(), [p1, p0, p2])
    }

    func test_movePane_after_lastPane_movesToEnd() {
        // [p0, p1, p2] — drop p0 after p2 → [p1, p2, p0]
        appState.tabs.movePane(p0, inTab: tabId, relativeTo: p2, placeAfter: true)
        XCTAssertEqual(paneIds(), [p1, p2, p0])
    }

    /// Exercises the remove-shifts-insert boundary: when srcIndex < insertIndex
    /// the insert index must be decremented by 1 to land in the right slot.
    func test_movePane_removeShiftsInsertBoundary_landsCorrectly() {
        // [p0, p1, p2] — drop p0 after p1.
        // srcIndex = 0, dstIndex = 1, placeAfter → insertIndex = 2 before shift;
        // srcIndex < insertIndex so insertIndex becomes 1. Result: [p1, p0, p2].
        appState.tabs.movePane(p0, inTab: tabId, relativeTo: p1, placeAfter: true)
        XCTAssertEqual(paneIds(), [p1, p0, p2])
    }

    // MARK: - movePane no-op cases

    func test_movePane_sameId_isNoOp() {
        let before = paneIds()
        appState.tabs.movePane(p0, inTab: tabId, relativeTo: p0, placeAfter: false)
        XCTAssertEqual(paneIds(), before)
    }

    func test_movePane_adjacent_afterPredecessor_isNoOp() {
        // p1 already sits just after p0 → "after p0" is a no-op.
        let before = paneIds()
        appState.tabs.movePane(p1, inTab: tabId, relativeTo: p0, placeAfter: true)
        XCTAssertEqual(paneIds(), before)
    }

    func test_movePane_adjacent_beforeSuccessor_isNoOp() {
        // p0 already sits just before p1 → "before p1" is a no-op.
        let before = paneIds()
        appState.tabs.movePane(p0, inTab: tabId, relativeTo: p1, placeAfter: false)
        XCTAssertEqual(paneIds(), before)
    }

    func test_movePane_unknownPaneId_isNoOp() {
        let before = paneIds()
        appState.tabs.movePane("ghost", inTab: tabId, relativeTo: p0, placeAfter: true)
        XCTAssertEqual(paneIds(), before)
    }

    func test_movePane_unknownTargetId_isNoOp() {
        let before = paneIds()
        appState.tabs.movePane(p0, inTab: tabId, relativeTo: "ghost", placeAfter: false)
        XCTAssertEqual(paneIds(), before)
    }

    func test_movePane_unknownTabId_isNoOp() {
        let before = paneIds()
        appState.tabs.movePane(p0, inTab: "ghost-tab", relativeTo: p1, placeAfter: false)
        XCTAssertEqual(paneIds(), before)
    }

    // MARK: - onTreeMutation callback

    func test_movePane_realMove_firesOnTreeMutationOnce() {
        var count = 0
        appState.tabs.onTreeMutation = { count += 1 }
        appState.tabs.movePane(p0, inTab: tabId, relativeTo: p2, placeAfter: true)
        XCTAssertEqual(count, 1, "A real reorder must fire onTreeMutation exactly once.")
    }

    func test_movePane_sameId_doesNotFireOnTreeMutation() {
        var count = 0
        appState.tabs.onTreeMutation = { count += 1 }
        appState.tabs.movePane(p0, inTab: tabId, relativeTo: p0, placeAfter: false)
        XCTAssertEqual(count, 0)
    }

    func test_movePane_adjacentNoOp_doesNotFireOnTreeMutation() {
        var count = 0
        appState.tabs.onTreeMutation = { count += 1 }
        appState.tabs.movePane(p1, inTab: tabId, relativeTo: p0, placeAfter: true)
        XCTAssertEqual(count, 0)
    }

    func test_movePane_unknownPaneId_doesNotFireOnTreeMutation() {
        var count = 0
        appState.tabs.onTreeMutation = { count += 1 }
        appState.tabs.movePane("ghost", inTab: tabId, relativeTo: p0, placeAfter: true)
        XCTAssertEqual(count, 0)
    }

    func test_movePane_unknownTabId_doesNotFireOnTreeMutation() {
        var count = 0
        appState.tabs.onTreeMutation = { count += 1 }
        appState.tabs.movePane(p0, inTab: "ghost-tab", relativeTo: p1, placeAfter: false)
        XCTAssertEqual(count, 0)
    }

    // MARK: - activePaneId is preserved

    func test_movePane_doesNotChangeActivePaneId() {
        // The active pane is keyed by id, not index — reordering must
        // not silently change which pane is focused.
        let beforeActive = appState.tabs.tab(for: tabId)?.activePaneId
        appState.tabs.movePane(p0, inTab: tabId, relativeTo: p2, placeAfter: true)
        let afterActive = appState.tabs.tab(for: tabId)?.activePaneId
        XCTAssertEqual(afterActive, beforeActive,
                       "movePane must not change activePaneId — it is keyed by id, not index.")
    }

    // MARK: - wouldMovePane

    func test_wouldMovePane_realMove_isTrue() {
        XCTAssertTrue(appState.tabs.wouldMovePane(p2, inTab: tabId, relativeTo: p0, placeAfter: false))
    }

    func test_wouldMovePane_sameId_isFalse() {
        XCTAssertFalse(appState.tabs.wouldMovePane(p0, inTab: tabId, relativeTo: p0, placeAfter: false))
    }

    func test_wouldMovePane_adjacentNoOp_isFalse() {
        // p1 already sits just after p0 — "after p0" is no-op.
        XCTAssertFalse(appState.tabs.wouldMovePane(p1, inTab: tabId, relativeTo: p0, placeAfter: true))
        // p1 already sits just before p2 — "before p2" is also a no-op.
        XCTAssertFalse(appState.tabs.wouldMovePane(p1, inTab: tabId, relativeTo: p2, placeAfter: false))
    }

    func test_wouldMovePane_unknownPaneId_isFalse() {
        XCTAssertFalse(appState.tabs.wouldMovePane("ghost", inTab: tabId, relativeTo: p0, placeAfter: true))
    }

    func test_wouldMovePane_unknownTargetId_isFalse() {
        XCTAssertFalse(appState.tabs.wouldMovePane(p0, inTab: tabId, relativeTo: "ghost", placeAfter: false))
    }

    func test_wouldMovePane_unknownTabId_isFalse() {
        XCTAssertFalse(appState.tabs.wouldMovePane(p0, inTab: "ghost-tab", relativeTo: p1, placeAfter: true))
    }
}
