//
//  SessionsModelPaneCwdTests.swift
//  NiceUnitTests
//
//  `paneCwdChanged(tabId:paneId:cwd:)` — the OSC 7 callback path,
//  invoked from `TabPtySession.onPaneCwdChange`. Asserts the update
//  lands on `Pane.cwd`, leaves `Tab.cwd` untouched (load-bearing for
//  Claude `--resume`), and silently drops stale tab/pane ids.
//
//  Tests use the convenience `AppState()` init (services == nil),
//  which disables SessionStore persistence — same pattern the other
//  AppState tests use.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class SessionsModelPaneCwdTests: XCTestCase {

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

    func test_paneCwdChanged_storesOnPane() {
        seedTerminalTab(tabId: "t1", paneId: "p1", tabCwd: "/tmp")

        appState.sessions.paneCwdChanged(
            tabId: "t1", paneId: "p1", cwd: "/Users/nick/Downloads"
        )

        XCTAssertEqual(
            appState.tabs.tab(for: "t1")?.panes.first?.cwd,
            "/Users/nick/Downloads",
            "OSC 7 update must land on Pane.cwd"
        )
    }

    func test_paneCwdChanged_doesNotMutateTabCwd() {
        // Tab.cwd is load-bearing for `claude --resume` on Claude
        // tabs — overwriting it from a companion terminal's cd would
        // silently relocate the session on restore.
        seedTerminalTab(tabId: "t1", paneId: "p1", tabCwd: "/tmp/anchor")

        appState.sessions.paneCwdChanged(
            tabId: "t1", paneId: "p1", cwd: "/Users/nick/Downloads"
        )

        XCTAssertEqual(
            appState.tabs.tab(for: "t1")?.cwd, "/tmp/anchor",
            "Tab.cwd must stay anchored even when a pane cd's elsewhere"
        )
    }

    func test_paneCwdChanged_unknownPane_isNoOp() {
        seedTerminalTab(tabId: "t1", paneId: "p1", tabCwd: "/tmp")

        appState.sessions.paneCwdChanged(
            tabId: "t1", paneId: "ghost", cwd: "/Users/nick"
        )

        XCTAssertNil(
            appState.tabs.tab(for: "t1")?.panes.first?.cwd,
            "stale paneId must not invent a cwd on the wrong pane"
        )
    }

    func test_paneCwdChanged_unknownTab_isNoOp() {
        seedTerminalTab(tabId: "t1", paneId: "p1", tabCwd: "/tmp")

        // Just shouldn't crash or mutate anything.
        appState.sessions.paneCwdChanged(
            tabId: "ghost-tab", paneId: "p1", cwd: "/Users/nick"
        )

        XCTAssertNil(appState.tabs.tab(for: "t1")?.panes.first?.cwd)
    }

    // MARK: - helpers

    private func seedTerminalTab(
        tabId: String, paneId: String, tabCwd: String
    ) {
        let tab = Tab(
            id: tabId,
            title: "Terminal",
            cwd: tabCwd,
            branch: nil,
            panes: [Pane(id: paneId, title: "zsh", kind: .terminal)],
            activePaneId: paneId
        )
        let project = Project(
            id: "p", name: "P", path: tabCwd, tabs: [tab]
        )
        appState.tabs.projects.append(project)
    }
}
