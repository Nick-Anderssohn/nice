//
//  PaneTearOffControllerTests.swift
//  NiceUnitTests
//
//  Integration tests for `PaneTearOffController.tearOff(...)` — the
//  model/lifecycle entry point for tearing a live pane off its source
//  window and queuing it for a new window.
//
//  Two windows (each an AppState) are registered in a shared
//  NiceServices.registry. A live-pane handle is published and the
//  controller is called with a stub `openWindow: { _ in }` closure that
//  captures the pairing token the controller minted, mirroring the test
//  pattern in `CrossWindowMoveTests`.
//
//  NOTE: `tearOff` now DEFERS its `openWindow` call one runloop turn (so
//  a new window is never born mid-`NSDraggingSession`). The seed enqueue
//  + source-tab dissolve + respawn epilogue are still SYNCHRONOUS, but
//  the `openWindow` token capture is not — tests that read the token (to
//  consume the seed by token) pump the main runloop first.
//
//  Covers:
//    • Terminal pane tear-off — pane leaves source, seed is enqueued
//      with correct fields, currentDrag cleared.
//    • Claude pane tear-off — same structure, claudeSessionId
//      threaded through.
//    • Tearing off the last pane of a project tab dissolves the source
//      tab while the Terminals project keeps its Main tab (no app
//      termination).
//

import AppKit
import XCTest
@testable import Nice

@MainActor
final class PaneTearOffControllerTests: XCTestCase {

    private var services: NiceServices!
    private var winA: AppState!
    private var winB: AppState!
    private var windowA: NSWindow!
    private var windowB: NSWindow!

    override func setUp() {
        super.setUp()
        services = NiceServices()
        winA = AppState(services: services, initialSidebarCollapsed: false,
                        initialMainCwd: nil, windowSessionId: "win-A",
                        store: FakeSessionStore())
        winB = AppState(services: services, initialSidebarCollapsed: false,
                        initialMainCwd: nil, windowSessionId: "win-B",
                        store: FakeSessionStore())
        windowA = NSWindow()
        windowB = NSWindow()
        services.registry.register(appState: winA, window: windowA)
        services.registry.register(appState: winB, window: windowB)
    }

    override func tearDown() {
        windowA = nil; windowB = nil
        winA = nil; winB = nil; services = nil
        super.tearDown()
    }

    // MARK: - Helpers

    /// Seed a terminal pane into `app` under its own project group.
    /// Returns the pane id (first id in `paneIds`).
    @discardableResult
    private func seedTerminalTab(
        into app: AppState,
        projectId: String,
        tabId: String,
        paneIds: [String]
    ) -> String {
        let panes = paneIds.enumerated().map {
            Pane(id: $0.element, title: "Terminal \($0.offset + 1)", kind: .terminal)
        }
        let tab = Tab(id: tabId, title: "T", cwd: "/tmp/\(projectId)",
                      panes: panes, activePaneId: paneIds.first)
        app.tabs.projects = [
            app.tabs.projects[0],
            Project(id: projectId, name: projectId.uppercased(),
                    path: "/tmp/\(projectId)", tabs: [tab])
        ]
        for id in paneIds {
            _ = app.sessions.makeSession(for: tabId, cwd: "/tmp/\(projectId)",
                                         initialTerminalPaneId: id)
        }
        return paneIds[0]
    }

    /// Publish a live-pane handle for `paneId` dragged from `source`.
    /// The claim closure resolves the pane to a `PaneClaim` tri-state via
    /// `claimPaneForTransfer` (matching production), so a deferred pane
    /// reports `.notSpawned` rather than a swallowed nil.
    private func publishDrag(from source: AppState, tabId: String, paneId: String) {
        services.livePaneRegistry.publish(.init(
            paneId: paneId,
            sourceWindowSessionId: source.windowSession.windowSessionId,
            sourceTabId: tabId,
            claim: { [weak source] in
                source?.sessions.claimPaneForTransfer(tabId: tabId, paneId: paneId) ?? .gone
            }
        ))
    }

    /// Run a tear-off and return the pairing token the controller minted,
    /// captured from the (deferred) `openWindow` closure. Pumps the main
    /// runloop so the `DispatchQueue.main.async` open fires. nil if the
    /// controller aborted before opening (the closure never ran).
    @discardableResult
    private func tearOffCapturingToken(
        paneId: String,
        from sourceSessionId: String,
        at point: NSPoint = NSPoint(x: 0, y: 0)
    ) -> String? {
        var capturedToken: String?
        let opened = expectation(description: "openWindow called")
        PaneTearOffController(services: services).tearOff(
            paneId: paneId,
            sourceWindowSessionId: sourceSessionId,
            at: point,
            openWindow: { token in
                capturedToken = token
                opened.fulfill()
            }
        )
        wait(for: [opened], timeout: 1.0)
        return capturedToken
    }

    // MARK: - Terminal pane tear-off

    func test_terminalPane_tearOff_seedEnqueuedWithCorrectFields() {
        seedTerminalTab(into: winA, projectId: "a", tabId: "a-tab", paneIds: ["pA"])
        publishDrag(from: winA, tabId: "a-tab", paneId: "pA")

        let releasePoint = NSPoint(x: 200, y: 300)
        let token = tearOffCapturingToken(
            paneId: "pA", from: "win-A", at: releasePoint
        )

        // The openWindow closure must have been called (deferred) with a
        // non-nil pairing token to trigger the new window.
        guard let token else {
            XCTFail("Expected openWindow to fire with a pairing token")
            return
        }

        // The in-flight handle must be consumed (drag is no longer live).
        XCTAssertNil(services.livePaneRegistry.currentDrag)

        // The pane must be gone from the source tab.
        XCTAssertNil(winA.tabs.tab(for: "a-tab")?.panes.first { $0.id == "pA" })

        // The live pty entry must have left the source session. The
        // tab was the only pane so it dissolves entirely — either the
        // session entry is gone (nil) or it no longer hosts the pane.
        XCTAssertTrue(winA.sessions.ptySessions["a-tab"]?.hasPane("pA") != true,
                      "Live entry should no longer be in the source session")

        // A seed must be enqueued under the minted token and consumable.
        guard let seed = services.consumeTearOffSeed(token: token) else {
            XCTFail("Expected a PendingTearOff seed enqueued under the token")
            return
        }
        XCTAssertEqual(seed.paneId, "pA")
        XCTAssertEqual(seed.kind, .terminal)
        XCTAssertNil(seed.claudeSessionId)
        XCTAssertEqual(seed.projectId, "a")
        XCTAssertEqual(seed.projectName, "A")
        XCTAssertEqual(seed.projectPath, "/tmp/a")
        XCTAssertEqual(seed.screenPoint, releasePoint)

        // One-shot: the seed is gone after the first consume.
        XCTAssertNil(services.consumeTearOffSeed(token: token))
    }

    // MARK: - Claude pane tear-off

    func test_claudePane_tearOff_seedEnqueuedWithClaudeSessionId() {
        // Build a Claude tab in winA.
        var claudePane = Pane(id: "cA", title: "Claude", kind: .claude)
        claudePane.isClaudeRunning = true
        let srcTab = Tab(id: "a-claude", title: "Repo", cwd: "/tmp/repo",
                         panes: [claudePane,
                                 Pane(id: "cA-t1", title: "Terminal 1", kind: .terminal)],
                         activePaneId: "cA", claudeSessionId: "sess-42")
        winA.tabs.projects = [
            winA.tabs.projects[0],
            Project(id: "p-repo", name: "REPO", path: "/tmp/repo", tabs: [srcTab])
        ]
        _ = winA.sessions.makeSession(for: "a-claude", cwd: "/tmp/repo",
                                       initialClaudePaneId: "cA")
        publishDrag(from: winA, tabId: "a-claude", paneId: "cA")

        let releasePoint = NSPoint(x: 500, y: 400)
        let token = tearOffCapturingToken(
            paneId: "cA", from: "win-A", at: releasePoint
        )
        guard let token else {
            XCTFail("Expected openWindow to fire with a pairing token")
            return
        }
        XCTAssertNil(services.livePaneRegistry.currentDrag)

        // Pane removed from source tab (still has companion terminal).
        let remaining = winA.tabs.tab(for: "a-claude")?.panes.map(\.id) ?? []
        XCTAssertFalse(remaining.contains("cA"), "Claude pane should be gone from source tab")
        XCTAssertEqual(winA.sessions.ptySessions["a-claude"]?.hasPane("cA"), false)

        // Seed fields.
        guard let seed = services.consumeTearOffSeed(token: token) else {
            XCTFail("Expected a PendingTearOff seed for the Claude pane")
            return
        }
        XCTAssertEqual(seed.paneId, "cA")
        XCTAssertEqual(seed.kind, .claude)
        XCTAssertEqual(seed.claudeSessionId, "sess-42")
        XCTAssertEqual(seed.projectId, "p-repo")
        XCTAssertEqual(seed.projectName, "REPO")
        XCTAssertEqual(seed.projectPath, "/tmp/repo")
        XCTAssertEqual(seed.screenPoint, releasePoint)

        XCTAssertNil(services.consumeTearOffSeed(token: token))
    }

    // MARK: - Last-pane dissolve

    func test_tearingOffLastPane_dissolvesSourceTab_withoutTerminating() {
        // winA has a project tab with a single terminal pane.
        // winB is registered so the app has two windows (won't terminate
        // when winA's project tab empties).
        seedTerminalTab(into: winA, projectId: "a", tabId: "a-tab", paneIds: ["pA"])
        // winB already registered; nothing extra to seed there.
        publishDrag(from: winA, tabId: "a-tab", paneId: "pA")

        // The dissolve epilogue is SYNCHRONOUS (runs before the deferred
        // openWindow), so it's asserted immediately after the call. The
        // token capture pumps the runloop and cleans up the seed.
        let token = tearOffCapturingToken(paneId: "pA", from: "win-A")

        // The project tab must have dissolved (pane was the last one).
        XCTAssertNil(winA.tabs.tab(for: "a-tab"),
                     "Source tab should dissolve after last pane torn off")

        // The Terminals project's Main tab must still exist so the app
        // doesn't terminate.
        let terminalsProject = winA.tabs.projects.first {
            $0.id == TabModel.terminalsProjectId
        }
        XCTAssertFalse(terminalsProject?.tabs.isEmpty ?? true,
                       "Terminals project Main tab must survive the dissolve")

        // Consume the seed so the map is clean.
        if let token { _ = services.consumeTearOffSeed(token: token) }
    }

    // MARK: - Bug 3: source's new active pane is spawned after tear-off

    func test_tearOff_spawnsSourceTabsNewActiveTerminal() {
        // winA has a Claude tab: [Claude (active), companion terminal].
        // The companion terminal is modelled but its pty is DEFERRED
        // (not spawned). Tearing off the Claude pane shifts focus to the
        // companion — which must now be spawned so it doesn't render blank.
        var claudePane = Pane(id: "cA", title: "Claude", kind: .claude)
        claudePane.isClaudeRunning = true
        let companionId = "cA-t1"
        let srcTab = Tab(id: "a-claude", title: "Repo", cwd: "/tmp/repo",
                         panes: [claudePane,
                                 Pane(id: companionId, title: "Terminal 1", kind: .terminal)],
                         activePaneId: "cA", claudeSessionId: "sess-1")
        winA.tabs.projects = [
            winA.tabs.projects[0],
            Project(id: "p-repo", name: "REPO", path: "/tmp/repo", tabs: [srcTab])
        ]
        // The Claude tab is the active tab when its Claude pane is torn
        // off (matches production: you tear off the pane you're looking at).
        winA.tabs.activeTabId = "a-claude"
        // Spawn only the Claude pane — companion stays deferred (mirrors
        // production: companion terminals spawn on first focus).
        _ = winA.sessions.makeSession(for: "a-claude", cwd: "/tmp/repo",
                                       initialClaudePaneId: "cA")
        XCTAssertEqual(winA.sessions.ptySessions["a-claude"]?.hasPane(companionId), false,
                       "Precondition: companion terminal is not yet spawned")
        publishDrag(from: winA, tabId: "a-claude", paneId: "cA")

        let token = tearOffCapturingToken(paneId: "cA", from: "win-A")

        // Focus shifted to the companion terminal AND it was spawned.
        // (The respawn epilogue is synchronous — it runs before the
        // deferred openWindow the helper waits on.)
        XCTAssertEqual(winA.tabs.tab(for: "a-claude")?.activePaneId, companionId)
        XCTAssertEqual(winA.sessions.ptySessions["a-claude"]?.hasPane(companionId), true,
                       "Source tab's new active terminal must be spawned (bug 3)")

        if let token { _ = services.consumeTearOffSeed(token: token) }
    }
}
