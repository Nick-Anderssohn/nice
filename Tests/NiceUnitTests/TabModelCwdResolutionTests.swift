//
//  TabModelCwdResolutionTests.swift
//  NiceUnitTests
//
//  Cwd resolution helpers used at restore/spawn time:
//    • `resolvedSpawnCwd(for:pane:)` — falls back to the tab cwd when
//      the pane's last-observed dir was deleted between launches.
//    • `spawnCwdForNewPane(in:callerProvided:)` — caller wins, then
//      active-pane cwd, then tab cwd.
//    • `adoptTabCwd(forTabId:newCwd:)` — single owner of the
//      "pane.cwd follows tab.cwd when nil or matching old" policy
//      shared by the SessionStart-hook rotation handler and the
//      restore-time heal pass. Direct unit tests below pin the
//      return-bool contract and the per-pane policy without going
//      through either indirect caller.
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

    // MARK: - adoptTabCwd(forTabId:newCwd:)

    func test_adoptTabCwd_unknownTabId_returnsFalse_noMutation() {
        // Tab id that's not in the projects tree. The function must
        // return false (no work done) and must not touch any other
        // tab as a side effect.
        let seeded = TabModelFixtures.seedClaudeTab(
            into: appState.tabs,
            projectId: "p", tabId: "t-known",
            projectPath: "/tmp/known"
        )
        let pre = appState.tabs.tab(for: seeded.tabId)

        let changed = appState.tabs.adoptTabCwd(
            forTabId: "t-ghost", newCwd: "/tmp/anywhere"
        )

        XCTAssertFalse(
            changed, "unknown tab id must return false"
        )
        XCTAssertEqual(
            appState.tabs.tab(for: seeded.tabId), pre,
            "siblings must not change when an unknown id is passed"
        )
    }

    func test_adoptTabCwd_sameCwd_returnsFalse_panesUntouched() {
        // newCwd matches the existing tab.cwd: nothing to do. The
        // pane-policy loop must NOT run — pane.cwd values (including
        // nil panes) stay exactly as they were so a no-op rotation
        // doesn't burn a save round or stomp a pane the user `cd`'d
        // away from to that same path.
        let seeded = TabModelFixtures.seedClaudeTab(
            into: appState.tabs,
            projectId: "p", tabId: "t-same",
            projectPath: "/tmp/same"
        )
        // Pre-state: one nil pane (the Claude pane from the fixture
        // doesn't carry a cwd), one pane already at the matching cwd.
        appState.tabs.mutateTab(id: seeded.tabId) { tab in
            // Claude pane stays nil; terminal pane sits at tab.cwd.
            if let i = tab.panes.firstIndex(where: { $0.id == seeded.terminalPaneId }) {
                tab.panes[i].cwd = "/tmp/same"
            }
        }
        let pre = appState.tabs.tab(for: seeded.tabId)

        let changed = appState.tabs.adoptTabCwd(
            forTabId: seeded.tabId, newCwd: "/tmp/same"
        )

        XCTAssertFalse(
            changed, "same cwd must short-circuit to false"
        )
        XCTAssertEqual(
            appState.tabs.tab(for: seeded.tabId), pre,
            "no-op rotation must leave every pane (including nil ones) unchanged"
        )
    }

    func test_adoptTabCwd_differentCwd_returnsTrue_tabUpdated() {
        // Happy path: caller hands us a new cwd that differs from
        // tab.cwd. Function returns true and tab.cwd is rewritten.
        let seeded = TabModelFixtures.seedClaudeTab(
            into: appState.tabs,
            projectId: "p", tabId: "t-rotate",
            projectPath: "/tmp/before"
        )

        let changed = appState.tabs.adoptTabCwd(
            forTabId: seeded.tabId, newCwd: "/tmp/after"
        )

        XCTAssertTrue(
            changed, "different cwd must return true"
        )
        XCTAssertEqual(
            appState.tabs.tab(for: seeded.tabId)?.cwd, "/tmp/after",
            "tab.cwd must be rewritten to newCwd"
        )
    }

    func test_adoptTabCwd_panePolicy_matchingFollows() {
        // A pane whose cwd matches the pre-rotation tab.cwd must
        // adopt the new cwd. This is the steady-state shape: panes
        // that were sitting at the project root follow the tab into
        // the worktree when Claude rotates.
        let seeded = TabModelFixtures.seedClaudeTab(
            into: appState.tabs,
            projectId: "p", tabId: "t-match",
            projectPath: "/tmp/old"
        )
        appState.tabs.mutateTab(id: seeded.tabId) { tab in
            if let i = tab.panes.firstIndex(where: { $0.id == seeded.terminalPaneId }) {
                tab.panes[i].cwd = "/tmp/old"
            }
        }

        XCTAssertTrue(appState.tabs.adoptTabCwd(
            forTabId: seeded.tabId, newCwd: "/tmp/new"
        ))

        let pane = appState.tabs.tab(for: seeded.tabId)?
            .panes.first(where: { $0.id == seeded.terminalPaneId })
        XCTAssertEqual(
            pane?.cwd, "/tmp/new",
            "pane that matched the old tab.cwd must follow into newCwd"
        )
    }

    func test_adoptTabCwd_panePolicy_nilFollows() {
        // A pane with no observed cwd (OSC 7 never fired — restored
        // deferred-resume pane, or a brand-new spawn that hasn't
        // emitted its first chpwd yet) follows the tab into newCwd.
        // Without this branch the pane would stay nil and the next
        // `resolvedSpawnCwd` lookup would fall back to the (still
        // unchanged) pane cwd — silently breaking the rotation.
        let seeded = TabModelFixtures.seedClaudeTab(
            into: appState.tabs,
            projectId: "p", tabId: "t-nil",
            projectPath: "/tmp/old"
        )
        // Sanity-check the fixture: Claude pane comes in with cwd nil.
        XCTAssertNil(
            appState.tabs.tab(for: seeded.tabId)?
                .panes.first(where: { $0.id == seeded.claudePaneId })?.cwd,
            "fixture precondition: Claude pane starts with nil cwd"
        )

        XCTAssertTrue(appState.tabs.adoptTabCwd(
            forTabId: seeded.tabId, newCwd: "/tmp/new"
        ))

        let pane = appState.tabs.tab(for: seeded.tabId)?
            .panes.first(where: { $0.id == seeded.claudePaneId })
        XCTAssertEqual(
            pane?.cwd, "/tmp/new",
            "nil-cwd pane must follow the tab into newCwd"
        )
    }

    func test_adoptTabCwd_panePolicy_divergedStays() {
        // A pane the user `cd`'d into a third path — neither nil nor
        // matching the old tab.cwd — keeps its own cwd across the
        // rotation. The user's manual navigation wins over an
        // auto-follow.
        let seeded = TabModelFixtures.seedClaudeTab(
            into: appState.tabs,
            projectId: "p", tabId: "t-diverged",
            projectPath: "/tmp/old"
        )
        appState.tabs.mutateTab(id: seeded.tabId) { tab in
            if let i = tab.panes.firstIndex(where: { $0.id == seeded.terminalPaneId }) {
                tab.panes[i].cwd = "/tmp/somewhere-else"
            }
        }

        XCTAssertTrue(appState.tabs.adoptTabCwd(
            forTabId: seeded.tabId, newCwd: "/tmp/new"
        ))

        let pane = appState.tabs.tab(for: seeded.tabId)?
            .panes.first(where: { $0.id == seeded.terminalPaneId })
        XCTAssertEqual(
            pane?.cwd, "/tmp/somewhere-else",
            "diverged pane must keep its user-chosen cwd across the rotation"
        )
    }

    func test_adoptTabCwd_mixedPanes_appliesPolicyPerPane() {
        // Lock the per-pane independence of the policy: a single tab
        // carries three panes — one matching old, one nil, one
        // diverged. The first two follow newCwd; the third stays at
        // its diverged path. A future refactor that batched the
        // mutation (e.g. "if any pane diverged, leave them all") would
        // light this up.
        let seeded = TabModelFixtures.seedClaudeTab(
            into: appState.tabs,
            projectId: "p", tabId: "t-mixed",
            projectPath: "/tmp/old"
        )
        let extraPaneId = "\(seeded.tabId)-t2"
        appState.tabs.mutateTab(id: seeded.tabId) { tab in
            // Claude pane stays nil (matches policy branch: nil follows).
            // Terminal pane already at /tmp/old (matches policy branch:
            // matching old follows).
            if let i = tab.panes.firstIndex(where: { $0.id == seeded.terminalPaneId }) {
                tab.panes[i].cwd = "/tmp/old"
            }
            // Third pane sits at a diverged path the user `cd`'d to.
            var diverged = Pane(id: extraPaneId, title: "Terminal 2", kind: .terminal)
            diverged.cwd = "/tmp/diverged"
            tab.panes.append(diverged)
        }

        XCTAssertTrue(appState.tabs.adoptTabCwd(
            forTabId: seeded.tabId, newCwd: "/tmp/new"
        ))

        let panes = appState.tabs.tab(for: seeded.tabId)?.panes ?? []
        XCTAssertEqual(
            panes.first(where: { $0.id == seeded.claudePaneId })?.cwd,
            "/tmp/new",
            "nil pane must follow"
        )
        XCTAssertEqual(
            panes.first(where: { $0.id == seeded.terminalPaneId })?.cwd,
            "/tmp/new",
            "matching-old pane must follow"
        )
        XCTAssertEqual(
            panes.first(where: { $0.id == extraPaneId })?.cwd,
            "/tmp/diverged",
            "diverged pane must stay put — pane policy is per-pane, not all-or-nothing"
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
