//
//  AppStateNavigationTests.swift
//  NiceUnitTests
//
//  Unit tests for the keyboard-navigation helpers in
//  Sources/Nice/State/AppState.swift (selectNextSidebarTab,
//  selectNextPane, addTerminalToActiveTab, etc.).
//
//  These tests exercise the in-memory model only — they don't spawn pty
//  sessions. Each helper is verified end-to-end via the public AppState
//  surface (activeTabId, projects, terminalsTab) so the behavior contract
//  is what's pinned down, not the internal implementation.
//
//  AppState's initializer brings up a control socket and a terminal pty
//  for the built-in Terminals tab. Tests construct the real AppState (we
//  haven't carved out a "no-pty" init), and just don't touch that pty —
//  only the data-model surface needed for navigation.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class AppStateNavigationTests: XCTestCase {

    private var appState: AppState!

    override func setUp() {
        super.setUp()
        appState = AppState()
    }

    override func tearDown() {
        appState = nil
        super.tearDown()
    }

    // MARK: - Sidebar navigation

    func test_navigableSidebarTabIds_terminalsAlwaysFirst() {
        // Fresh AppState: only the Terminals tab exists.
        XCTAssertEqual(appState.navigableSidebarTabIds, [AppState.terminalsTabId])
    }

    func test_nextSidebarTab_isNoOpWhenOnlyTerminalsExists() {
        appState.activeTabId = AppState.terminalsTabId
        appState.selectNextSidebarTab()
        XCTAssertEqual(appState.activeTabId, AppState.terminalsTabId)

        appState.selectPrevSidebarTab()
        XCTAssertEqual(appState.activeTabId, AppState.terminalsTabId)
    }

    func test_nextSidebarTab_cyclesThroughVisibleTabs() {
        seedTwoProjects()
        let ids = appState.navigableSidebarTabIds
        XCTAssertEqual(ids.count, 5,
                       "Terminals + (P1: T1, T2) + (P2: T1, T2) = 5 navigable ids")

        appState.activeTabId = ids[0]
        for expectedIdx in 1..<ids.count {
            appState.selectNextSidebarTab()
            XCTAssertEqual(appState.activeTabId, ids[expectedIdx])
        }
        // One more step wraps back to Terminals.
        appState.selectNextSidebarTab()
        XCTAssertEqual(appState.activeTabId, ids[0])
    }

    func test_prevSidebarTab_cyclesBackward() {
        seedTwoProjects()
        let ids = appState.navigableSidebarTabIds

        appState.activeTabId = ids[0]
        // From Terminals, prev wraps to the last id.
        appState.selectPrevSidebarTab()
        XCTAssertEqual(appState.activeTabId, ids.last)

        appState.selectPrevSidebarTab()
        XCTAssertEqual(appState.activeTabId, ids[ids.count - 2])
    }

    func test_nextSidebarTab_respectsSidebarQueryFilter() {
        seedTwoProjects(p1Titles: ["needle", "first"], p2Titles: ["second", "third"])
        // Filter to titles containing "needle" — only the first project's
        // first tab matches. Terminals tab is always present (search
        // filters projects, not built-ins).
        appState.sidebarQuery = "needle"

        let ids = appState.navigableSidebarTabIds
        XCTAssertEqual(ids.first, AppState.terminalsTabId)
        XCTAssertEqual(ids.count, 2, "Terminals + 1 matching project tab")

        appState.activeTabId = ids[0]
        appState.selectNextSidebarTab()
        XCTAssertEqual(appState.activeTabId, ids[1])
        appState.selectNextSidebarTab()
        XCTAssertEqual(appState.activeTabId, ids[0], "wraps back to Terminals")
    }

    // MARK: - Pane navigation

    func test_nextPane_movesRightWhenNotAtEnd() {
        // Terminals tab seeds with a single pane; add a second so we
        // have something to navigate.
        addExtraTerminalPaneToTerminals()
        let tab = appState.terminalsTab
        XCTAssertEqual(tab.panes.count, 2)
        let firstId = tab.panes[0].id
        let secondId = tab.panes[1].id

        // Force focus to the first pane.
        appState.setActivePane(tabId: tab.id, paneId: firstId)
        appState.selectNextPane()
        XCTAssertEqual(appState.terminalsTab.activePaneId, secondId)
    }

    func test_nextPane_wrapsToFirstWhenAtLast() {
        addExtraTerminalPaneToTerminals()
        let tab = appState.terminalsTab
        let firstId = tab.panes[0].id
        let lastId = tab.panes.last!.id

        appState.setActivePane(tabId: tab.id, paneId: lastId)
        appState.selectNextPane()
        XCTAssertEqual(appState.terminalsTab.activePaneId, firstId)
    }

    func test_prevPane_wrapsToLastWhenAtFirst() {
        addExtraTerminalPaneToTerminals()
        let tab = appState.terminalsTab
        let firstId = tab.panes[0].id
        let lastId = tab.panes.last!.id

        appState.setActivePane(tabId: tab.id, paneId: firstId)
        appState.selectPrevPane()
        XCTAssertEqual(appState.terminalsTab.activePaneId, lastId)
    }

    func test_nextPane_isNoOpWhenSinglePane() {
        // Terminals tab starts with a single pane.
        let originalActive = appState.terminalsTab.activePaneId
        appState.selectNextPane()
        XCTAssertEqual(appState.terminalsTab.activePaneId, originalActive)
    }

    // MARK: - Add terminal

    func test_addTerminalToActiveTab_appendsTerminalAndFocuses() {
        appState.activeTabId = AppState.terminalsTabId
        let originalCount = appState.terminalsTab.panes.count
        appState.addTerminalToActiveTab()
        XCTAssertEqual(appState.terminalsTab.panes.count, originalCount + 1)
        let newPane = appState.terminalsTab.panes.last!
        XCTAssertEqual(newPane.kind, .terminal)
        XCTAssertEqual(appState.terminalsTab.activePaneId, newPane.id)
    }

    func test_helpers_areNoOpWhenActiveTabIdIsNil() {
        appState.activeTabId = nil
        // Should not crash; should not mutate state.
        appState.selectNextPane()
        appState.selectPrevPane()
        appState.addTerminalToActiveTab()
        XCTAssertNil(appState.activeTabId)
        // navigableSidebarTabIds still includes Terminals; selectNext
        // resolves currentIdx to 0 and steps from there.
        appState.selectNextSidebarTab()
        // Single navigable id ⇒ no-op, activeTabId stays nil.
        XCTAssertNil(appState.activeTabId)
    }

    // MARK: - Helpers

    /// Seed two projects with two tabs each, each tab having one terminal
    /// pane. Doesn't spin up pty sessions — just mutates the model.
    private func seedTwoProjects(
        p1Titles: [String] = ["P1-T1", "P1-T2"],
        p2Titles: [String] = ["P2-T1", "P2-T2"]
    ) {
        let p1 = Project(
            id: "p1", name: "P1", path: "/tmp/p1",
            tabs: p1Titles.enumerated().map { i, title in
                Tab(
                    id: "p1t\(i)", title: title, status: .idle,
                    cwd: "/tmp/p1", panes: [
                        Pane(id: "p1t\(i)-p0", title: "zsh", kind: .terminal)
                    ],
                    activePaneId: "p1t\(i)-p0"
                )
            }
        )
        let p2 = Project(
            id: "p2", name: "P2", path: "/tmp/p2",
            tabs: p2Titles.enumerated().map { i, title in
                Tab(
                    id: "p2t\(i)", title: title, status: .idle,
                    cwd: "/tmp/p2", panes: [
                        Pane(id: "p2t\(i)-p0", title: "zsh", kind: .terminal)
                    ],
                    activePaneId: "p2t\(i)-p0"
                )
            }
        )
        appState.projects = [p1, p2]
    }

    /// Append a second terminal pane to the built-in Terminals tab so
    /// pane-navigation tests have something to step through. Skips the
    /// pty side (uses direct model mutation through addPane, which DOES
    /// spawn a pty — see note at top of file). For these tests we accept
    /// the real pty being created; tests only assert on the data model.
    private func addExtraTerminalPaneToTerminals() {
        _ = appState.addPane(tabId: AppState.terminalsTabId, kind: .terminal)
    }
}
