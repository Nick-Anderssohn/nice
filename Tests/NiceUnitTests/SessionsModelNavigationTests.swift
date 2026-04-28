//
//  SessionsModelNavigationTests.swift
//  NiceUnitTests
//
//  Pane navigation within a tab: selectNextPane / selectPrevPane and
//  addTerminalToActiveTab. Driven through the SessionsModel surface.
//  These tests touch the real pty-spawn path inside `addPane`;
//  assertions only read the data model.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class SessionsModelNavigationTests: XCTestCase {

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

    /// Snapshot of the Main terminal tab. Re-read on each access so
    /// assertions observe the latest mutation — Swift value semantics
    /// mean a captured `tab` would be stale after any model write.
    private func mainTab() -> Tab {
        appState.tabs.tab(for: TabModel.mainTerminalTabId)!
    }

    /// Append a second terminal pane to the Main terminal tab so pane-
    /// navigation tests have something to step through. Goes through
    /// `addPane`, which DOES spawn a pty.
    private func addExtraTerminalPaneToMain() {
        _ = appState.sessions.addPane(tabId: TabModel.mainTerminalTabId, kind: .terminal)
    }
}
