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
//  surface (activeTabId, projects) so the behavior contract is what's
//  pinned down, not the internal implementation.
//
//  AppState's initializer brings up a control socket and a terminal pty
//  for the Main terminal tab in the pinned Terminals project. Tests
//  construct the real AppState (we haven't carved out a "no-pty"
//  init), and just don't touch that pty — only the data-model surface
//  needed for navigation.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class AppStateNavigationTests: XCTestCase {

    private var appState: AppState!
    private var homeSandbox: TestHomeSandbox!

    override func setUp() {
        super.setUp()
        homeSandbox = TestHomeSandbox()
        appState = AppState()
    }

    override func tearDown() {
        appState = nil
        homeSandbox.teardown()
        homeSandbox = nil
        super.tearDown()
    }

    // MARK: - Sidebar navigation

    func test_navigableSidebarTabIds_terminalsAlwaysFirst() {
        // Fresh AppState: only the Main terminal tab exists.
        XCTAssertEqual(appState.tabs.navigableSidebarTabIds, [TabModel.mainTerminalTabId])
    }

    func test_nextSidebarTab_isNoOpWhenOnlyMainTerminalExists() {
        appState.tabs.activeTabId = TabModel.mainTerminalTabId
        appState.tabs.selectNextSidebarTab()
        XCTAssertEqual(appState.tabs.activeTabId, TabModel.mainTerminalTabId)

        appState.tabs.selectPrevSidebarTab()
        XCTAssertEqual(appState.tabs.activeTabId, TabModel.mainTerminalTabId)
    }

    func test_nextSidebarTab_cyclesThroughVisibleTabs() {
        seedTwoProjects()
        let ids = appState.tabs.navigableSidebarTabIds
        XCTAssertEqual(ids.count, 5,
                       "Main + (P1: T1, T2) + (P2: T1, T2) = 5 navigable ids")

        appState.tabs.activeTabId = ids[0]
        for expectedIdx in 1..<ids.count {
            appState.tabs.selectNextSidebarTab()
            XCTAssertEqual(appState.tabs.activeTabId, ids[expectedIdx])
        }
        // One more step wraps back to Main.
        appState.tabs.selectNextSidebarTab()
        XCTAssertEqual(appState.tabs.activeTabId, ids[0])
    }

    func test_prevSidebarTab_cyclesBackward() {
        seedTwoProjects()
        let ids = appState.tabs.navigableSidebarTabIds

        appState.tabs.activeTabId = ids[0]
        // From Main, prev wraps to the last id.
        appState.tabs.selectPrevSidebarTab()
        XCTAssertEqual(appState.tabs.activeTabId, ids.last)

        appState.tabs.selectPrevSidebarTab()
        XCTAssertEqual(appState.tabs.activeTabId, ids[ids.count - 2])
    }

    // MARK: - Pane navigation

    func test_nextPane_movesRightWhenNotAtEnd() {
        // Main tab seeds with a single pane; add a second so we have
        // something to navigate.
        addExtraTerminalPaneToMain()
        let tab = mainTab()
        XCTAssertEqual(tab.panes.count, 2)
        let firstId = tab.panes[0].id
        let secondId = tab.panes[1].id

        // Force focus to the first pane.
        appState.sessions.setActivePane(tabId: tab.id, paneId: firstId)
        appState.sessions.selectNextPane()
        XCTAssertEqual(mainTab().activePaneId, secondId)
    }

    func test_nextPane_wrapsToFirstWhenAtLast() {
        addExtraTerminalPaneToMain()
        let tab = mainTab()
        let firstId = tab.panes[0].id
        let lastId = tab.panes.last!.id

        appState.sessions.setActivePane(tabId: tab.id, paneId: lastId)
        appState.sessions.selectNextPane()
        XCTAssertEqual(mainTab().activePaneId, firstId)
    }

    func test_prevPane_wrapsToLastWhenAtFirst() {
        addExtraTerminalPaneToMain()
        let tab = mainTab()
        let firstId = tab.panes[0].id
        let lastId = tab.panes.last!.id

        appState.sessions.setActivePane(tabId: tab.id, paneId: firstId)
        appState.sessions.selectPrevPane()
        XCTAssertEqual(mainTab().activePaneId, lastId)
    }

    func test_nextPane_isNoOpWhenSinglePane() {
        // Main tab starts with a single pane.
        let originalActive = mainTab().activePaneId
        appState.sessions.selectNextPane()
        XCTAssertEqual(mainTab().activePaneId, originalActive)
    }

    // MARK: - Add terminal

    func test_addTerminalToActiveTab_appendsTerminalAndFocuses() {
        appState.tabs.activeTabId = TabModel.mainTerminalTabId
        let originalCount = mainTab().panes.count
        appState.sessions.addTerminalToActiveTab()
        XCTAssertEqual(mainTab().panes.count, originalCount + 1)
        let newPane = mainTab().panes.last!
        XCTAssertEqual(newPane.kind, .terminal)
        XCTAssertEqual(mainTab().activePaneId, newPane.id)
    }

    func test_helpers_areNoOpWhenActiveTabIdIsNil() {
        appState.tabs.activeTabId = nil
        // Should not crash; should not mutate state.
        appState.sessions.selectNextPane()
        appState.sessions.selectPrevPane()
        appState.sessions.addTerminalToActiveTab()
        XCTAssertNil(appState.tabs.activeTabId)
        // navigableSidebarTabIds still includes Main; selectNext
        // resolves currentIdx to 0 and steps from there.
        appState.tabs.selectNextSidebarTab()
        // Single navigable id ⇒ no-op, activeTabId stays nil.
        XCTAssertNil(appState.tabs.activeTabId)
    }

    // MARK: - Helpers

    /// Seed two project groups alongside the pinned Terminals group,
    /// each with two tabs, each tab holding one terminal pane. Doesn't
    /// spin up pty sessions — just mutates the model. Preserves the
    /// Terminals group at index 0 so the "Terminals always first"
    /// invariant still holds.
    private func seedTwoProjects(
        p1Titles: [String] = ["P1-T1", "P1-T2"],
        p2Titles: [String] = ["P2-T1", "P2-T2"]
    ) {
        let p1 = Project(
            id: "p1", name: "P1", path: "/tmp/p1",
            tabs: p1Titles.enumerated().map { i, title in
                Tab(
                    id: "p1t\(i)", title: title,
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
                    id: "p2t\(i)", title: title,
                    cwd: "/tmp/p2", panes: [
                        Pane(id: "p2t\(i)-p0", title: "zsh", kind: .terminal)
                    ],
                    activePaneId: "p2t\(i)-p0"
                )
            }
        )
        let terminals = appState.tabs.projects.first(where: {
            $0.id == TabModel.terminalsProjectId
        }) ?? Project(
            id: TabModel.terminalsProjectId, name: "Terminals",
            path: "/tmp", tabs: []
        )
        appState.tabs.projects = [terminals, p1, p2]
    }

    /// Snapshot of the Main terminal tab. Re-read on each access so
    /// assertions observe the latest mutation — Swift value semantics
    /// mean a captured `tab` would be stale after any model write.
    private func mainTab() -> Tab {
        appState.tabs.tab(for: TabModel.mainTerminalTabId)!
    }

    /// Append a second terminal pane to the Main terminal tab so pane-
    /// navigation tests have something to step through. Goes through
    /// `addPane`, which DOES spawn a pty (see note at top of file).
    /// For these tests we accept the real pty being created; assertions
    /// only read the data model.
    private func addExtraTerminalPaneToMain() {
        _ = appState.sessions.addPane(tabId: TabModel.mainTerminalTabId, kind: .terminal)
    }
}
