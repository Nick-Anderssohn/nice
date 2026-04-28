//
//  TabModelCwdResolutionTests.swift
//  NiceUnitTests
//
//  Cwd resolution helpers used at restore/spawn time:
//    • `resolvedSpawnCwd(for:pane:)` — falls back to the tab cwd when
//      the pane's last-observed dir was deleted between launches.
//    • `spawnCwdForNewPane(in:callerProvided:)` — caller wins, then
//      active-pane cwd, then tab cwd.
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
final class TabModelCwdResolutionTests: XCTestCase {

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

    // MARK: - resolvedSpawnCwd(for:pane:)

    func test_resolvedSpawnCwd_prefersPaneCwdWhenItExists() throws {
        let dir = try makeTempDir()
        seedTerminalTab(tabId: "t1", paneId: "p1", tabCwd: "/tmp", paneCwd: dir)

        let tab = try XCTUnwrap(appState.tabs.tab(for: "t1"))
        let pane = try XCTUnwrap(tab.panes.first)
        XCTAssertEqual(
            appState.tabs.resolvedSpawnCwd(for: tab, pane: pane), dir,
            "pane cwd that exists on disk must be the spawn dir"
        )
    }

    func test_resolvedSpawnCwd_fallsBackWhenPaneCwdMissing() throws {
        // The user's last cwd was a temp dir; user deleted it before
        // relaunch. Spawning at a non-existent path would fail
        // chdir(2); we must fall back to a real directory.
        let liveDir = try makeTempDir()
        let deadDir = try makeTempDir()
        try FileManager.default.removeItem(atPath: deadDir)

        seedTerminalTab(tabId: "t1", paneId: "p1", tabCwd: liveDir, paneCwd: deadDir)

        let tab = try XCTUnwrap(appState.tabs.tab(for: "t1"))
        let pane = try XCTUnwrap(tab.panes.first)
        XCTAssertEqual(
            appState.tabs.resolvedSpawnCwd(for: tab, pane: pane), liveDir,
            "deleted pane cwd must fall back to the tab's cwd"
        )
    }

    func test_resolvedSpawnCwd_nilPaneCwd_fallsBackToTab() throws {
        let liveDir = try makeTempDir()
        seedTerminalTab(tabId: "t1", paneId: "p1", tabCwd: liveDir)

        let tab = try XCTUnwrap(appState.tabs.tab(for: "t1"))
        let pane = try XCTUnwrap(tab.panes.first)
        XCTAssertNil(pane.cwd, "fixture must start without a pane cwd")
        XCTAssertEqual(
            appState.tabs.resolvedSpawnCwd(for: tab, pane: pane), liveDir
        )
    }

    // MARK: - spawnCwdForNewPane

    func test_spawnCwdForNewPane_callerProvidedWins() throws {
        let liveDir = try makeTempDir()
        seedTerminalTab(tabId: "t1", paneId: "p1", tabCwd: liveDir, paneCwd: liveDir)

        let tab = try XCTUnwrap(appState.tabs.tab(for: "t1"))
        XCTAssertEqual(
            appState.tabs.spawnCwdForNewPane(in: tab, callerProvided: "/explicit"),
            "/explicit",
            "an explicit cwd from the caller must win over inheritance"
        )
    }

    func test_spawnCwdForNewPane_inheritsActivePaneCwd() throws {
        // The user cd'd somewhere in the active pane, then asked for a
        // new pane — the new pane should open at the same dir, not at
        // wherever the tab was first launched.
        let tabDir = try makeTempDir()
        let paneDir = try makeTempDir()
        seedTerminalTab(tabId: "t1", paneId: "p1", tabCwd: tabDir, paneCwd: paneDir)

        let tab = try XCTUnwrap(appState.tabs.tab(for: "t1"))
        XCTAssertEqual(
            appState.tabs.spawnCwdForNewPane(in: tab, callerProvided: nil),
            paneDir
        )
    }

    func test_spawnCwdForNewPane_fallsBackToTabCwdWhenNoActivePane() throws {
        let tabDir = try makeTempDir()
        // Tab with no `activePaneId` — `spawnCwdForNewPane` has no pane
        // to inherit from and must use the tab cwd.
        let tab = Tab(
            id: "t1",
            title: "Terminal",
            cwd: tabDir,
            branch: nil,
            panes: [],
            activePaneId: nil
        )
        XCTAssertEqual(
            appState.tabs.spawnCwdForNewPane(in: tab, callerProvided: nil),
            tabDir
        )
    }

    // MARK: - helpers

    private func seedTerminalTab(
        tabId: String, paneId: String, tabCwd: String, paneCwd: String? = nil
    ) {
        var pane = Pane(id: paneId, title: "zsh", kind: .terminal)
        pane.cwd = paneCwd
        let tab = Tab(
            id: tabId,
            title: "Terminal",
            cwd: tabCwd,
            branch: nil,
            panes: [pane],
            activePaneId: paneId
        )
        let project = Project(
            id: "p", name: "P", path: tabCwd, tabs: [tab]
        )
        appState.tabs.projects.append(project)
    }

    private func makeTempDir() throws -> String {
        let path = NSTemporaryDirectory()
            + "nice-tab-cwd-test-\(UUID().uuidString)"
        try FileManager.default.createDirectory(
            atPath: path, withIntermediateDirectories: true
        )
        addTeardownBlock {
            try? FileManager.default.removeItem(atPath: path)
        }
        return path
    }
}
