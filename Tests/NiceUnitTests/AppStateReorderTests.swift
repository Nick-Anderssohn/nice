//
//  AppStateReorderTests.swift
//  NiceUnitTests
//
//  Unit tests for AppState.moveTab and AppState.moveProject — the
//  sidebar drag-to-reorder helpers. Tests exercise only the in-memory
//  model; no pty sessions involved.
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
        appState.projects.first(where: { $0.id == projectId })?.tabs.map(\.id) ?? []
    }

    private func projectIds() -> [String] {
        appState.projects.map(\.id)
    }

    // MARK: - moveTab

    func test_moveTab_before_movesSourceIntoTargetSlot() {
        seedTwoProjects()
        // [p1t0, p1t1, p1t2] — drop p1t2 before p1t0.
        appState.moveTab("p1t2", relativeTo: "p1t0", placeAfter: false)
        XCTAssertEqual(tabIds(inProject: "p1"), ["p1t2", "p1t0", "p1t1"])
    }

    func test_moveTab_after_landsJustPastTarget() {
        seedTwoProjects()
        // [p1t0, p1t1, p1t2] — drop p1t0 after p1t1.
        appState.moveTab("p1t0", relativeTo: "p1t1", placeAfter: true)
        XCTAssertEqual(tabIds(inProject: "p1"), ["p1t1", "p1t0", "p1t2"])
    }

    func test_moveTab_after_lastTab_movesToEnd() {
        seedTwoProjects()
        // [p1t0, p1t1, p1t2] — drop p1t0 after last (p1t2).
        appState.moveTab("p1t0", relativeTo: "p1t2", placeAfter: true)
        XCTAssertEqual(tabIds(inProject: "p1"), ["p1t1", "p1t2", "p1t0"])
    }

    func test_moveTab_adjacent_afterPredecessor_isNoOp() {
        seedTwoProjects()
        // p1t1 already sits just after p1t0 → dropping "after p1t0" is a
        // no-op rather than churning the array.
        let before = tabIds(inProject: "p1")
        appState.moveTab("p1t1", relativeTo: "p1t0", placeAfter: true)
        XCTAssertEqual(tabIds(inProject: "p1"), before)
    }

    func test_moveTab_adjacent_beforeSuccessor_isNoOp() {
        seedTwoProjects()
        let before = tabIds(inProject: "p1")
        appState.moveTab("p1t0", relativeTo: "p1t1", placeAfter: false)
        XCTAssertEqual(tabIds(inProject: "p1"), before)
    }

    func test_moveTab_sameId_isNoOp() {
        seedTwoProjects()
        let before = tabIds(inProject: "p1")
        appState.moveTab("p1t0", relativeTo: "p1t0", placeAfter: false)
        XCTAssertEqual(tabIds(inProject: "p1"), before)
    }

    func test_moveTab_acrossProjects_isNoOp() {
        seedTwoProjects()
        // Cross-project drops aren't supported — the spec is "within
        // their project". Both projects should stay untouched.
        let p1Before = tabIds(inProject: "p1")
        let p2Before = tabIds(inProject: "p2")
        appState.moveTab("p1t0", relativeTo: "p2t0", placeAfter: false)
        XCTAssertEqual(tabIds(inProject: "p1"), p1Before)
        XCTAssertEqual(tabIds(inProject: "p2"), p2Before)
    }

    func test_moveTab_unknownSource_isNoOp() {
        seedTwoProjects()
        let before = tabIds(inProject: "p1")
        appState.moveTab("ghost", relativeTo: "p1t0", placeAfter: true)
        XCTAssertEqual(tabIds(inProject: "p1"), before)
    }

    func test_moveTab_unknownTarget_isNoOp() {
        seedTwoProjects()
        let before = tabIds(inProject: "p1")
        appState.moveTab("p1t0", relativeTo: "ghost", placeAfter: false)
        XCTAssertEqual(tabIds(inProject: "p1"), before)
    }

    // MARK: - moveProject

    func test_moveProject_before_movesSourceIntoTargetSlot() {
        seedThreeRegularProjects()
        // [p1, p2, p3] — drop p3 before p1.
        appState.moveProject("p3", relativeTo: "p1", placeAfter: false)
        XCTAssertEqual(projectIds(), ["p3", "p1", "p2"])
    }

    func test_moveProject_after_landsJustPastTarget() {
        seedThreeRegularProjects()
        // [p1, p2, p3] — drop p1 after p2.
        appState.moveProject("p1", relativeTo: "p2", placeAfter: true)
        XCTAssertEqual(projectIds(), ["p2", "p1", "p3"])
    }

    func test_moveProject_after_lastProject_movesToEnd() {
        seedThreeRegularProjects()
        appState.moveProject("p1", relativeTo: "p3", placeAfter: true)
        XCTAssertEqual(projectIds(), ["p2", "p3", "p1"])
    }

    func test_moveProject_adjacent_afterPredecessor_isNoOp() {
        seedThreeRegularProjects()
        let before = projectIds()
        // p2 already sits just after p1 — no-op.
        appState.moveProject("p2", relativeTo: "p1", placeAfter: true)
        XCTAssertEqual(projectIds(), before)
    }

    func test_moveProject_sameId_isNoOp() {
        seedThreeRegularProjects()
        let before = projectIds()
        appState.moveProject("p1", relativeTo: "p1", placeAfter: false)
        XCTAssertEqual(projectIds(), before)
    }

    func test_moveProject_unknownSource_isNoOp() {
        seedThreeRegularProjects()
        let before = projectIds()
        appState.moveProject("ghost", relativeTo: "p1", placeAfter: false)
        XCTAssertEqual(projectIds(), before)
    }

    func test_moveProject_unknownTarget_isNoOp() {
        seedThreeRegularProjects()
        let before = projectIds()
        appState.moveProject("p1", relativeTo: "ghost", placeAfter: false)
        XCTAssertEqual(projectIds(), before)
    }

    func test_moveProject_terminalsAsSource_isNoOp() {
        seedWithTerminals()
        // The pinned Terminals project can't be moved.
        let before = projectIds()
        appState.moveProject(AppState.terminalsProjectId, relativeTo: "p1", placeAfter: true)
        XCTAssertEqual(projectIds(), before)
    }

    func test_moveProject_terminalsAsTarget_isNoOp() {
        seedWithTerminals()
        // Dropping a project onto the Terminals row can't displace it
        // — Terminals must stay at index 0.
        let before = projectIds()
        appState.moveProject("p2", relativeTo: AppState.terminalsProjectId, placeAfter: false)
        XCTAssertEqual(projectIds(), before)
    }

    func test_moveProject_reorderingRegularProjects_leavesTerminalsPinned() {
        seedWithTerminals()
        // Swap p1 and p2 after the Terminals row.
        appState.moveProject("p2", relativeTo: "p1", placeAfter: false)
        XCTAssertEqual(projectIds(), [AppState.terminalsProjectId, "p2", "p1"])
    }

    // MARK: - wouldMoveTab / wouldMoveProject (drop-indicator predicates)

    func test_wouldMoveTab_realMove_isTrue() {
        seedTwoProjects()
        XCTAssertTrue(appState.wouldMoveTab("p1t2", relativeTo: "p1t0", placeAfter: false))
    }

    func test_wouldMoveTab_sameId_isFalse() {
        seedTwoProjects()
        XCTAssertFalse(appState.wouldMoveTab("p1t0", relativeTo: "p1t0", placeAfter: false))
    }

    func test_wouldMoveTab_adjacent_noOp_isFalse() {
        seedTwoProjects()
        // p1t1 sits just after p1t0 → "after p1t0" is a no-op.
        XCTAssertFalse(appState.wouldMoveTab("p1t1", relativeTo: "p1t0", placeAfter: true))
        // And "before p1t2" is also a no-op (same final slot).
        XCTAssertFalse(appState.wouldMoveTab("p1t1", relativeTo: "p1t2", placeAfter: false))
    }

    func test_wouldMoveTab_crossProject_isFalse() {
        seedTwoProjects()
        XCTAssertFalse(appState.wouldMoveTab("p1t0", relativeTo: "p2t0", placeAfter: false))
    }

    func test_wouldMoveProject_realMove_isTrue() {
        seedThreeRegularProjects()
        XCTAssertTrue(appState.wouldMoveProject("p3", relativeTo: "p1", placeAfter: false))
    }

    func test_wouldMoveProject_adjacent_noOp_isFalse() {
        seedThreeRegularProjects()
        XCTAssertFalse(appState.wouldMoveProject("p2", relativeTo: "p1", placeAfter: true))
    }

    func test_wouldMoveProject_terminals_isFalse() {
        seedWithTerminals()
        XCTAssertFalse(appState.wouldMoveProject(AppState.terminalsProjectId, relativeTo: "p1", placeAfter: true))
        XCTAssertFalse(appState.wouldMoveProject("p1", relativeTo: AppState.terminalsProjectId, placeAfter: false))
    }

    // MARK: - Fixtures

    /// Seeds two projects, three tabs each, each tab with one terminal
    /// pane. Mirrors the pattern in `AppStateNavigationTests`.
    private func seedTwoProjects() {
        let p1 = makeProject(id: "p1", name: "P1", tabCount: 3)
        let p2 = makeProject(id: "p2", name: "P2", tabCount: 2)
        appState.projects = [p1, p2]
    }

    /// Seeds three plain projects with no Terminals row — the
    /// simplest fixture for happy-path project moves.
    private func seedThreeRegularProjects() {
        let p1 = makeProject(id: "p1", name: "P1", tabCount: 1)
        let p2 = makeProject(id: "p2", name: "P2", tabCount: 1)
        let p3 = makeProject(id: "p3", name: "P3", tabCount: 1)
        appState.projects = [p1, p2, p3]
    }

    /// Seeds Terminals (pinned at index 0) plus two regular projects,
    /// for tests that assert Terminals' pinning behavior.
    private func seedWithTerminals() {
        let terminals = Project(
            id: AppState.terminalsProjectId,
            name: "Terminals",
            path: "/tmp/terminals",
            tabs: [
                Tab(
                    id: AppState.mainTerminalTabId, title: "Main",
                    cwd: "/tmp/terminals",
                    panes: [Pane(id: "\(AppState.mainTerminalTabId)-p0", title: "zsh", kind: .terminal)],
                    activePaneId: "\(AppState.mainTerminalTabId)-p0"
                )
            ]
        )
        let p1 = makeProject(id: "p1", name: "P1", tabCount: 1)
        let p2 = makeProject(id: "p2", name: "P2", tabCount: 1)
        appState.projects = [terminals, p1, p2]
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
