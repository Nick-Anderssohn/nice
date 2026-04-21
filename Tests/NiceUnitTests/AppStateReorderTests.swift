//
//  AppStateReorderTests.swift
//  NiceUnitTests
//
//  Unit tests for AppState.moveTab — the sidebar drag-to-reorder helper.
//  Tests exercise only the in-memory model; no pty sessions involved.
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

    func test_moveTab_terminalsTabId_isNoOp() {
        seedTwoProjects()
        // The built-in Terminals tab isn't in a project and isn't
        // reorderable. Both directions should be no-ops.
        let before = tabIds(inProject: "p1")
        appState.moveTab(AppState.terminalsTabId, relativeTo: "p1t0", placeAfter: false)
        XCTAssertEqual(tabIds(inProject: "p1"), before)

        appState.moveTab("p1t0", relativeTo: AppState.terminalsTabId, placeAfter: false)
        XCTAssertEqual(tabIds(inProject: "p1"), before)
    }

    // MARK: - Fixtures

    /// Seeds two projects, three tabs each, each tab with one terminal
    /// pane. Mirrors the pattern in `AppStateNavigationTests`.
    private func seedTwoProjects() {
        let p1 = Project(
            id: "p1", name: "P1", path: "/tmp/p1",
            tabs: (0..<3).map { i in
                Tab(
                    id: "p1t\(i)", title: "P1-T\(i)",
                    cwd: "/tmp/p1",
                    panes: [Pane(id: "p1t\(i)-p0", title: "zsh", kind: .terminal)],
                    activePaneId: "p1t\(i)-p0"
                )
            }
        )
        let p2 = Project(
            id: "p2", name: "P2", path: "/tmp/p2",
            tabs: (0..<2).map { i in
                Tab(
                    id: "p2t\(i)", title: "P2-T\(i)",
                    cwd: "/tmp/p2",
                    panes: [Pane(id: "p2t\(i)-p0", title: "zsh", kind: .terminal)],
                    activePaneId: "p2t\(i)-p0"
                )
            }
        )
        appState.projects = [p1, p2]
    }
}
