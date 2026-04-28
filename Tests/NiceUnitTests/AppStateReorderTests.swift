//
//  AppStateReorderTests.swift
//  NiceUnitTests
//
//  Unit tests for AppState.moveTab and AppState.wouldMoveTab — the
//  sidebar drag-to-reorder helper and its no-op predicate. Tests
//  exercise only the in-memory model; no pty sessions involved.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class AppStateReorderTests: XCTestCase {

    private var appState: AppState!

    override func setUp() {
        super.setUp()
        appState = AppState()
    }

    override func tearDown() {
        appState = nil
        super.tearDown()
    }

    private func tabIds(inProject projectId: String) -> [String] {
        appState.tabs.projects.first(where: { $0.id == projectId })?.tabs.map(\.id) ?? []
    }

    // MARK: - moveTab

    func test_moveTab_before_movesSourceIntoTargetSlot() {
        seedTwoProjects()
        // [p1t0, p1t1, p1t2] — drop p1t2 before p1t0.
        appState.tabs.moveTab("p1t2", relativeTo: "p1t0", placeAfter: false)
        XCTAssertEqual(tabIds(inProject: "p1"), ["p1t2", "p1t0", "p1t1"])
    }

    func test_moveTab_after_landsJustPastTarget() {
        seedTwoProjects()
        // [p1t0, p1t1, p1t2] — drop p1t0 after p1t1.
        appState.tabs.moveTab("p1t0", relativeTo: "p1t1", placeAfter: true)
        XCTAssertEqual(tabIds(inProject: "p1"), ["p1t1", "p1t0", "p1t2"])
    }

    func test_moveTab_after_lastTab_movesToEnd() {
        seedTwoProjects()
        // [p1t0, p1t1, p1t2] — drop p1t0 after last (p1t2).
        appState.tabs.moveTab("p1t0", relativeTo: "p1t2", placeAfter: true)
        XCTAssertEqual(tabIds(inProject: "p1"), ["p1t1", "p1t2", "p1t0"])
    }

    func test_moveTab_adjacent_afterPredecessor_isNoOp() {
        seedTwoProjects()
        // p1t1 already sits just after p1t0 → dropping "after p1t0" is a
        // no-op rather than churning the array.
        let before = tabIds(inProject: "p1")
        appState.tabs.moveTab("p1t1", relativeTo: "p1t0", placeAfter: true)
        XCTAssertEqual(tabIds(inProject: "p1"), before)
    }

    func test_moveTab_adjacent_beforeSuccessor_isNoOp() {
        seedTwoProjects()
        let before = tabIds(inProject: "p1")
        appState.tabs.moveTab("p1t0", relativeTo: "p1t1", placeAfter: false)
        XCTAssertEqual(tabIds(inProject: "p1"), before)
    }

    func test_moveTab_sameId_isNoOp() {
        seedTwoProjects()
        let before = tabIds(inProject: "p1")
        appState.tabs.moveTab("p1t0", relativeTo: "p1t0", placeAfter: false)
        XCTAssertEqual(tabIds(inProject: "p1"), before)
    }

    func test_moveTab_acrossProjects_isNoOp() {
        seedTwoProjects()
        // Cross-project drops aren't supported — the spec is "within
        // their project". Both projects should stay untouched.
        let p1Before = tabIds(inProject: "p1")
        let p2Before = tabIds(inProject: "p2")
        appState.tabs.moveTab("p1t0", relativeTo: "p2t0", placeAfter: false)
        XCTAssertEqual(tabIds(inProject: "p1"), p1Before)
        XCTAssertEqual(tabIds(inProject: "p2"), p2Before)
    }

    func test_moveTab_unknownSource_isNoOp() {
        seedTwoProjects()
        let before = tabIds(inProject: "p1")
        appState.tabs.moveTab("ghost", relativeTo: "p1t0", placeAfter: true)
        XCTAssertEqual(tabIds(inProject: "p1"), before)
    }

    func test_moveTab_unknownTarget_isNoOp() {
        seedTwoProjects()
        let before = tabIds(inProject: "p1")
        appState.tabs.moveTab("p1t0", relativeTo: "ghost", placeAfter: false)
        XCTAssertEqual(tabIds(inProject: "p1"), before)
    }

    // MARK: - wouldMoveTab (drop-indicator predicate)

    func test_wouldMoveTab_realMove_isTrue() {
        seedTwoProjects()
        XCTAssertTrue(appState.tabs.wouldMoveTab("p1t2", relativeTo: "p1t0", placeAfter: false))
    }

    func test_wouldMoveTab_sameId_isFalse() {
        seedTwoProjects()
        XCTAssertFalse(appState.tabs.wouldMoveTab("p1t0", relativeTo: "p1t0", placeAfter: false))
    }

    func test_wouldMoveTab_adjacent_noOp_isFalse() {
        seedTwoProjects()
        // p1t1 sits just after p1t0 → "after p1t0" is a no-op.
        XCTAssertFalse(appState.tabs.wouldMoveTab("p1t1", relativeTo: "p1t0", placeAfter: true))
        // And "before p1t2" is also a no-op (same final slot).
        XCTAssertFalse(appState.tabs.wouldMoveTab("p1t1", relativeTo: "p1t2", placeAfter: false))
    }

    func test_wouldMoveTab_crossProject_isFalse() {
        seedTwoProjects()
        XCTAssertFalse(appState.tabs.wouldMoveTab("p1t0", relativeTo: "p2t0", placeAfter: false))
    }

    // MARK: - Terminals project

    // Tabs inside the pinned Terminals project aren't special-cased
    // — they reorder like any other project's tabs. These tests pin
    // that behavior so it stays explicit and can't quietly regress.

    func test_moveTab_withinTerminalsProject_reorders() {
        seedTerminalsAndOneProject()
        // Terminals starts with [terminals-main, term-t1, term-t2] —
        // move term-t2 before terminals-main.
        appState.tabs.moveTab("term-t2", relativeTo: TabModel.mainTerminalTabId, placeAfter: false)
        XCTAssertEqual(
            tabIds(inProject: TabModel.terminalsProjectId),
            ["term-t2", TabModel.mainTerminalTabId, "term-t1"]
        )
    }

    func test_moveTab_terminalsToUserProject_isNoOp() {
        seedTerminalsAndOneProject()
        // Cross-project drops still no-op even when the source is
        // the Main Terminals tab — sidebar drag is within-project.
        let termBefore = tabIds(inProject: TabModel.terminalsProjectId)
        let p1Before = tabIds(inProject: "p1")
        appState.tabs.moveTab(TabModel.mainTerminalTabId, relativeTo: "p1t0", placeAfter: true)
        XCTAssertEqual(tabIds(inProject: TabModel.terminalsProjectId), termBefore)
        XCTAssertEqual(tabIds(inProject: "p1"), p1Before)
    }

    // MARK: - Fixtures

    /// Seeds two projects, three tabs each, each tab with one terminal
    /// pane. Mirrors the pattern in `AppStateNavigationTests`.
    private func seedTwoProjects() {
        let p1 = makeProject(id: "p1", name: "P1", tabCount: 3)
        let p2 = makeProject(id: "p2", name: "P2", tabCount: 2)
        appState.tabs.projects = [p1, p2]
    }

    /// Seeds the pinned Terminals project (with its Main tab plus
    /// two extra terminal tabs) and one regular project. Used by the
    /// within-Terminals reorder tests.
    private func seedTerminalsAndOneProject() {
        let terminals = Project(
            id: TabModel.terminalsProjectId,
            name: "Terminals",
            path: "/tmp/terminals",
            tabs: [
                Tab(
                    id: TabModel.mainTerminalTabId, title: "Main",
                    cwd: "/tmp/terminals",
                    panes: [Pane(id: "\(TabModel.mainTerminalTabId)-p0", title: "zsh", kind: .terminal)],
                    activePaneId: "\(TabModel.mainTerminalTabId)-p0"
                ),
                Tab(
                    id: "term-t1", title: "Term 1",
                    cwd: "/tmp/terminals",
                    panes: [Pane(id: "term-t1-p0", title: "zsh", kind: .terminal)],
                    activePaneId: "term-t1-p0"
                ),
                Tab(
                    id: "term-t2", title: "Term 2",
                    cwd: "/tmp/terminals",
                    panes: [Pane(id: "term-t2-p0", title: "zsh", kind: .terminal)],
                    activePaneId: "term-t2-p0"
                ),
            ]
        )
        let p1 = makeProject(id: "p1", name: "P1", tabCount: 2)
        appState.tabs.projects = [terminals, p1]
    }

    private func makeProject(id: String, name: String, tabCount: Int) -> Project {
        Project(
            id: id, name: name, path: "/tmp/\(id)",
            tabs: (0..<tabCount).map { i in
                Tab(
                    id: "\(id)t\(i)", title: "\(name)-T\(i)",
                    cwd: "/tmp/\(id)",
                    panes: [Pane(id: "\(id)t\(i)-p0", title: "zsh", kind: .terminal)],
                    activePaneId: "\(id)t\(i)-p0"
                )
            }
        )
    }
}
