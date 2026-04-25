//
//  AppStatePaneCwdTests.swift
//  NiceUnitTests
//
//  Locks down the per-pane cwd plumbing:
//    • `paneCwdChanged(tabId:paneId:cwd:)` — the OSC 7 callback path,
//      called from `TabPtySession.onPaneCwdChange`.
//    • `resolvedSpawnCwd(for:pane:)` — used at restore/spawn to pick
//      the right working directory. Falls back to the tab/project
//      cwd when the pane's last-observed dir was deleted between
//      launches.
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
final class AppStatePaneCwdTests: XCTestCase {

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

    // MARK: - paneCwdChanged

    func test_paneCwdChanged_storesOnPane() {
        seedTerminalTab(tabId: "t1", paneId: "p1", tabCwd: "/tmp")

        appState.paneCwdChanged(
            tabId: "t1", paneId: "p1", cwd: "/Users/nick/Downloads"
        )

        XCTAssertEqual(
            appState.tab(for: "t1")?.panes.first?.cwd,
            "/Users/nick/Downloads",
            "OSC 7 update must land on Pane.cwd"
        )
    }

    func test_paneCwdChanged_doesNotMutateTabCwd() {
        // Tab.cwd is load-bearing for `claude --resume` on Claude
        // tabs — overwriting it from a companion terminal's cd would
        // silently relocate the session on restore.
        seedTerminalTab(tabId: "t1", paneId: "p1", tabCwd: "/tmp/anchor")

        appState.paneCwdChanged(
            tabId: "t1", paneId: "p1", cwd: "/Users/nick/Downloads"
        )

        XCTAssertEqual(
            appState.tab(for: "t1")?.cwd, "/tmp/anchor",
            "Tab.cwd must stay anchored even when a pane cd's elsewhere"
        )
    }

    func test_paneCwdChanged_unknownPane_isNoOp() {
        seedTerminalTab(tabId: "t1", paneId: "p1", tabCwd: "/tmp")

        appState.paneCwdChanged(
            tabId: "t1", paneId: "ghost", cwd: "/Users/nick"
        )

        XCTAssertNil(
            appState.tab(for: "t1")?.panes.first?.cwd,
            "stale paneId must not invent a cwd on the wrong pane"
        )
    }

    func test_paneCwdChanged_unknownTab_isNoOp() {
        seedTerminalTab(tabId: "t1", paneId: "p1", tabCwd: "/tmp")

        // Just shouldn't crash or mutate anything.
        appState.paneCwdChanged(
            tabId: "ghost-tab", paneId: "p1", cwd: "/Users/nick"
        )

        XCTAssertNil(appState.tab(for: "t1")?.panes.first?.cwd)
    }

    // MARK: - resolvedSpawnCwd(for:pane:)

    func test_resolvedSpawnCwd_prefersPaneCwdWhenItExists() throws {
        let dir = try makeTempDir()
        seedTerminalTab(tabId: "t1", paneId: "p1", tabCwd: "/tmp")
        appState.paneCwdChanged(tabId: "t1", paneId: "p1", cwd: dir)

        let tab = try XCTUnwrap(appState.tab(for: "t1"))
        let pane = try XCTUnwrap(tab.panes.first)
        XCTAssertEqual(
            appState.resolvedSpawnCwd(for: tab, pane: pane), dir,
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

        seedTerminalTab(tabId: "t1", paneId: "p1", tabCwd: liveDir)
        appState.paneCwdChanged(tabId: "t1", paneId: "p1", cwd: deadDir)

        let tab = try XCTUnwrap(appState.tab(for: "t1"))
        let pane = try XCTUnwrap(tab.panes.first)
        XCTAssertEqual(
            appState.resolvedSpawnCwd(for: tab, pane: pane), liveDir,
            "deleted pane cwd must fall back to the tab's cwd"
        )
    }

    func test_resolvedSpawnCwd_nilPaneCwd_fallsBackToTab() throws {
        let liveDir = try makeTempDir()
        seedTerminalTab(tabId: "t1", paneId: "p1", tabCwd: liveDir)

        let tab = try XCTUnwrap(appState.tab(for: "t1"))
        let pane = try XCTUnwrap(tab.panes.first)
        XCTAssertNil(pane.cwd, "fixture must start without a pane cwd")
        XCTAssertEqual(
            appState.resolvedSpawnCwd(for: tab, pane: pane), liveDir
        )
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
        appState.projects.append(project)
    }

    private func makeTempDir() throws -> String {
        let path = NSTemporaryDirectory()
            + "nice-pane-cwd-test-\(UUID().uuidString)"
        try FileManager.default.createDirectory(
            atPath: path, withIntermediateDirectories: true
        )
        addTeardownBlock {
            try? FileManager.default.removeItem(atPath: path)
        }
        return path
    }
}
