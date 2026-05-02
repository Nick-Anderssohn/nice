//
//  TabModelMovePaneTests.swift
//  NiceUnitTests
//
//  Pure-data tests for `TabModel.movePane` and `wouldMovePane`. Covers:
//    - Same-tab terminal reorder (forward, backward, no-op).
//    - Cross-tab terminal join (active-pane recovery, destination
//      activePaneId update).
//    - Claude rejection (reorder + cross-tab; both forbidden — Claude
//      moves go through `AppState.absorbAsNewTab`).
//    - Terminal-into-Claude-tab clamp (insertions land at index ≥1 so
//      Claude stays at index 0).
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class TabModelMovePaneTests: XCTestCase {

    private var appState: AppState!

    override func setUp() {
        super.setUp()
        appState = AppState()
    }

    override func tearDown() {
        appState = nil
        super.tearDown()
    }

    // MARK: - Same-tab terminal reorder

    func test_movePane_sameTab_terminal_movesForward() {
        let tab = seedTerminalsTab(paneIds: ["t-a", "t-b", "t-c", "t-d"])
        let moved = appState.tabs.movePane(
            paneId: "t-a", fromTabId: tab, toTabId: tab, insertAt: 2
        )
        XCTAssertTrue(moved)
        XCTAssertEqual(paneIds(in: tab), ["t-b", "t-c", "t-a", "t-d"])
    }

    func test_movePane_sameTab_terminal_movesBackward() {
        let tab = seedTerminalsTab(paneIds: ["t-a", "t-b", "t-c", "t-d"])
        let moved = appState.tabs.movePane(
            paneId: "t-d", fromTabId: tab, toTabId: tab, insertAt: 1
        )
        XCTAssertTrue(moved)
        XCTAssertEqual(paneIds(in: tab), ["t-a", "t-d", "t-b", "t-c"])
    }

    func test_movePane_sameTab_appendDefault() {
        let tab = seedTerminalsTab(paneIds: ["t-a", "t-b", "t-c"])
        let moved = appState.tabs.movePane(
            paneId: "t-a", fromTabId: tab, toTabId: tab, insertAt: nil
        )
        XCTAssertTrue(moved)
        // Default destIndex = n-1 (final position = end).
        XCTAssertEqual(paneIds(in: tab), ["t-b", "t-c", "t-a"])
    }

    func test_movePane_sameTab_droppedOnSelf_isNoOp() {
        let tab = seedTerminalsTab(paneIds: ["t-a", "t-b", "t-c"])
        let moved = appState.tabs.movePane(
            paneId: "t-b", fromTabId: tab, toTabId: tab, insertAt: 1
        )
        XCTAssertFalse(moved)
        XCTAssertEqual(paneIds(in: tab), ["t-a", "t-b", "t-c"])
    }

    // MARK: - Cross-tab terminal move

    func test_movePane_crossTab_appendsToDestination() {
        let src = seedTerminalsTab(paneIds: ["t-a", "t-b"], tabId: "src")
        let dst = seedTerminalsTab(paneIds: ["t-x", "t-y"], tabId: "dst")
        let moved = appState.tabs.movePane(
            paneId: "t-a", fromTabId: src, toTabId: dst, insertAt: nil
        )
        XCTAssertTrue(moved)
        XCTAssertEqual(paneIds(in: src), ["t-b"])
        XCTAssertEqual(paneIds(in: dst), ["t-x", "t-y", "t-a"])
    }

    func test_movePane_crossTab_destinationActiveBecomesMovedPane() {
        let src = seedTerminalsTab(paneIds: ["t-a", "t-b"], tabId: "src")
        let dst = seedTerminalsTab(paneIds: ["t-x", "t-y"], tabId: "dst")
        // Set an explicit non-matching active pane on destination so the
        // assertion is meaningful.
        appState.tabs.mutateTab(id: dst) { $0.activePaneId = "t-x" }
        XCTAssertTrue(appState.tabs.movePane(
            paneId: "t-a", fromTabId: src, toTabId: dst, insertAt: nil
        ))
        XCTAssertEqual(activePaneId(in: dst), "t-a")
    }

    func test_movePane_crossTab_sourceActive_recoversToNeighbor() {
        let src = seedTerminalsTab(paneIds: ["t-a", "t-b", "t-c"], tabId: "src")
        let dst = seedTerminalsTab(paneIds: ["t-x"], tabId: "dst")
        appState.tabs.mutateTab(id: src) { $0.activePaneId = "t-b" }
        XCTAssertTrue(appState.tabs.movePane(
            paneId: "t-b", fromTabId: src, toTabId: dst, insertAt: nil
        ))
        // After removing index 1, the next-neighbor (now at index 1
        // before removal, idx 1 after) is t-c. Match the
        // `paneExited` recovery rule.
        XCTAssertEqual(activePaneId(in: src), "t-c")
    }

    func test_movePane_crossTab_sourceActive_isLast_recoversToPrevious() {
        let src = seedTerminalsTab(paneIds: ["t-a", "t-b", "t-c"], tabId: "src")
        let dst = seedTerminalsTab(paneIds: ["t-x"], tabId: "dst")
        appState.tabs.mutateTab(id: src) { $0.activePaneId = "t-c" }
        XCTAssertTrue(appState.tabs.movePane(
            paneId: "t-c", fromTabId: src, toTabId: dst, insertAt: nil
        ))
        XCTAssertEqual(activePaneId(in: src), "t-b")
    }

    func test_movePane_crossTab_sourceActive_isOnly_recoversToNil() {
        let src = seedTerminalsTab(paneIds: ["t-only"], tabId: "src")
        let dst = seedTerminalsTab(paneIds: ["t-x"], tabId: "dst")
        XCTAssertTrue(appState.tabs.movePane(
            paneId: "t-only", fromTabId: src, toTabId: dst, insertAt: nil
        ))
        XCTAssertNil(activePaneId(in: src))
        XCTAssertEqual(paneIds(in: src), [])
    }

    // MARK: - Claude rejection

    func test_movePane_claude_sameTab_isRejected() {
        let seeded = TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )
        let moved = appState.tabs.movePane(
            paneId: seeded.claudePaneId, fromTabId: seeded.tabId,
            toTabId: seeded.tabId, insertAt: 1
        )
        XCTAssertFalse(moved)
        XCTAssertEqual(
            paneIds(in: seeded.tabId),
            [seeded.claudePaneId, seeded.terminalPaneId]
        )
    }

    func test_movePane_claude_crossTab_isRejected() {
        let src = TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )
        let dst = TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p2", tabId: "t2"
        )
        let moved = appState.tabs.movePane(
            paneId: src.claudePaneId, fromTabId: src.tabId,
            toTabId: dst.tabId, insertAt: nil
        )
        XCTAssertFalse(moved)
        XCTAssertEqual(
            paneIds(in: src.tabId),
            [src.claudePaneId, src.terminalPaneId]
        )
    }

    // MARK: - Terminal-into-Claude-tab clamp

    func test_movePane_terminal_intoClaudeTab_clampedToIndex1() {
        let src = seedTerminalsTab(paneIds: ["t-a"], tabId: "src")
        let claude = TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "claude-tab"
        )
        // Try to insert at index 0 — should clamp to 1 so Claude stays
        // at index 0.
        XCTAssertTrue(appState.tabs.movePane(
            paneId: "t-a", fromTabId: src, toTabId: claude.tabId, insertAt: 0
        ))
        XCTAssertEqual(
            paneIds(in: claude.tabId),
            [claude.claudePaneId, "t-a", claude.terminalPaneId]
        )
    }

    func test_movePane_terminal_sameClaudeTab_cannotPassClaude() {
        // Tab: [claude, t-a, t-b]. Try to move t-b to index 0; should
        // clamp to 1.
        let seeded = TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "ct"
        )
        // Append a second terminal.
        appState.tabs.mutateTab(id: seeded.tabId) { tab in
            tab.panes.append(Pane(id: "t-extra", title: "Term 2", kind: .terminal))
        }
        XCTAssertTrue(appState.tabs.movePane(
            paneId: "t-extra", fromTabId: seeded.tabId,
            toTabId: seeded.tabId, insertAt: 0
        ))
        XCTAssertEqual(
            paneIds(in: seeded.tabId),
            [seeded.claudePaneId, "t-extra", seeded.terminalPaneId]
        )
    }

    // MARK: - Lookup failures

    func test_movePane_unknownPane_isNoOp() {
        let tab = seedTerminalsTab(paneIds: ["t-a", "t-b"])
        XCTAssertFalse(appState.tabs.movePane(
            paneId: "ghost", fromTabId: tab, toTabId: tab, insertAt: 0
        ))
        XCTAssertEqual(paneIds(in: tab), ["t-a", "t-b"])
    }

    func test_movePane_unknownDestTab_isNoOp() {
        let tab = seedTerminalsTab(paneIds: ["t-a"])
        XCTAssertFalse(appState.tabs.movePane(
            paneId: "t-a", fromTabId: tab, toTabId: "ghost", insertAt: nil
        ))
        XCTAssertEqual(paneIds(in: tab), ["t-a"])
    }

    // MARK: - wouldMovePane

    func test_wouldMovePane_sameTabRealMove_isTrue() {
        let tab = seedTerminalsTab(paneIds: ["t-a", "t-b", "t-c"])
        XCTAssertTrue(appState.tabs.wouldMovePane(
            paneId: "t-a", fromTabId: tab, toTabId: tab, insertAt: 2
        ))
    }

    func test_wouldMovePane_sameTabSelf_isFalse() {
        let tab = seedTerminalsTab(paneIds: ["t-a", "t-b"])
        XCTAssertFalse(appState.tabs.wouldMovePane(
            paneId: "t-b", fromTabId: tab, toTabId: tab, insertAt: 1
        ))
    }

    func test_wouldMovePane_claude_isFalse() {
        let seeded = TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )
        XCTAssertFalse(appState.tabs.wouldMovePane(
            paneId: seeded.claudePaneId, fromTabId: seeded.tabId,
            toTabId: seeded.tabId, insertAt: 1
        ))
    }

    func test_wouldMovePane_crossTab_isTrue() {
        let src = seedTerminalsTab(paneIds: ["t-a"], tabId: "src")
        let dst = seedTerminalsTab(paneIds: ["t-x"], tabId: "dst")
        XCTAssertTrue(appState.tabs.wouldMovePane(
            paneId: "t-a", fromTabId: src, toTabId: dst, insertAt: nil
        ))
    }

    // MARK: - Fixtures

    /// Append a terminal-only tab into a fresh project. Returns the tab id.
    @discardableResult
    private func seedTerminalsTab(
        paneIds: [String],
        tabId: String? = nil
    ) -> String {
        let id = tabId ?? "tab-\(UUID().uuidString.prefix(6))"
        let panes = paneIds.map {
            Pane(id: $0, title: $0, kind: .terminal)
        }
        let tab = Tab(
            id: id,
            title: "Tab",
            cwd: "/tmp/\(id)",
            branch: nil,
            panes: panes,
            activePaneId: panes.first?.id
        )
        let project = Project(
            id: "proj-\(id)", name: id.uppercased(),
            path: "/tmp/\(id)", tabs: [tab]
        )
        appState.tabs.projects.append(project)
        return id
    }

    private func paneIds(in tabId: String) -> [String] {
        appState.tabs.tab(for: tabId)?.panes.map(\.id) ?? []
    }

    private func activePaneId(in tabId: String) -> String? {
        appState.tabs.tab(for: tabId)?.activePaneId
    }
}
