//
//  TabModelNavigationTests.swift
//  NiceUnitTests
//
//  Sidebar tab navigation: navigableSidebarTabIds and the
//  selectNext/PrevSidebarTab cycle. Pure data-model assertions; no pty
//  sessions involved.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class TabModelNavigationTests: XCTestCase {

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
}
