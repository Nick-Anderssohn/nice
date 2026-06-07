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
//  controller is called with a stub `openWindow: {}` closure, mirroring
//  the test pattern in `CrossWindowMoveTests`.
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
    private func publishDrag(from source: AppState, tabId: String, paneId: String) {
        services.livePaneRegistry.publish(.init(
            paneId: paneId,
            sourceWindowSessionId: source.windowSession.windowSessionId,
            sourceTabId: tabId,
            claim: { [weak source] in
                source?.sessions.detachLivePane(tabId: tabId, paneId: paneId)
            }
        ))
    }

    // MARK: - Terminal pane tear-off

    func test_terminalPane_tearOff_seedEnqueuedWithCorrectFields() {
        seedTerminalTab(into: winA, projectId: "a", tabId: "a-tab", paneIds: ["pA"])
        publishDrag(from: winA, tabId: "a-tab", paneId: "pA")

        var openWindowCalled = false
        let releasePoint = NSPoint(x: 200, y: 300)
        PaneTearOffController(services: services).tearOff(
            paneId: "pA",
            sourceWindowSessionId: "win-A",
            at: releasePoint,
            openWindow: { openWindowCalled = true }
        )

        // The openWindow closure must have been called to trigger the
        // new window.
        XCTAssertTrue(openWindowCalled)

        // The in-flight handle must be consumed (drag is no longer live).
        XCTAssertNil(services.livePaneRegistry.currentDrag)

        // The pane must be gone from the source tab.
        XCTAssertNil(winA.tabs.tab(for: "a-tab")?.panes.first { $0.id == "pA" })

        // The live pty entry must have left the source session. The
        // tab was the only pane so it dissolves entirely — either the
        // session entry is gone (nil) or it no longer hosts the pane.
        XCTAssertTrue(winA.sessions.ptySessions["a-tab"]?.hasPane("pA") != true,
                      "Live entry should no longer be in the source session")

        // A seed must be enqueued and consumable.
        guard let seed = services.consumeTearOffSeed() else {
            XCTFail("Expected a PendingTearOff seed to be enqueued")
            return
        }
        XCTAssertEqual(seed.paneId, "pA")
        XCTAssertEqual(seed.kind, .terminal)
        XCTAssertNil(seed.claudeSessionId)
        XCTAssertEqual(seed.projectId, "a")
        XCTAssertEqual(seed.projectName, "A")
        XCTAssertEqual(seed.projectPath, "/tmp/a")
        XCTAssertEqual(seed.screenPoint, releasePoint)

        // No second seed should be lurking.
        XCTAssertNil(services.consumeTearOffSeed())
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

        var openWindowCalled = false
        let releasePoint = NSPoint(x: 500, y: 400)
        PaneTearOffController(services: services).tearOff(
            paneId: "cA",
            sourceWindowSessionId: "win-A",
            at: releasePoint,
            openWindow: { openWindowCalled = true }
        )

        XCTAssertTrue(openWindowCalled)
        XCTAssertNil(services.livePaneRegistry.currentDrag)

        // Pane removed from source tab (still has companion terminal).
        let remaining = winA.tabs.tab(for: "a-claude")?.panes.map(\.id) ?? []
        XCTAssertFalse(remaining.contains("cA"), "Claude pane should be gone from source tab")
        XCTAssertEqual(winA.sessions.ptySessions["a-claude"]?.hasPane("cA"), false)

        // Seed fields.
        guard let seed = services.consumeTearOffSeed() else {
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

        XCTAssertNil(services.consumeTearOffSeed())
    }

    // MARK: - Last-pane dissolve

    func test_tearingOffLastPane_dissolvesSourceTab_withoutTerminating() {
        // winA has a project tab with a single terminal pane.
        // winB is registered so the app has two windows (won't terminate
        // when winA's project tab empties).
        seedTerminalTab(into: winA, projectId: "a", tabId: "a-tab", paneIds: ["pA"])
        // winB already registered; nothing extra to seed there.
        publishDrag(from: winA, tabId: "a-tab", paneId: "pA")

        PaneTearOffController(services: services).tearOff(
            paneId: "pA",
            sourceWindowSessionId: "win-A",
            at: NSPoint(x: 0, y: 0),
            openWindow: {}
        )

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

        // Consume the seed so the queue is clean.
        _ = services.consumeTearOffSeed()
    }
}
