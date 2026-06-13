//
//  CrossWindowMoveTests.swift
//  NiceUnitTests
//
//  Integration tests for `PaneMigrationCoordinator.commitCrossWindowMove`
//  — the cross-window pane move that the target strip's drop delegate
//  drives. Two windows (each an AppState) are registered in a shared
//  NiceServices.registry; a live-pane handle is published, then the
//  coordinator is invoked directly (the deterministic seam the UITest
//  exercises through the real drag). Covers terminal panes (insert into
//  the target strip) and Claude panes (become a new tab under the
//  matching project), plus the same-window no-op guard.
//

import AppKit
import XCTest
@testable import Nice

@MainActor
final class CrossWindowMoveTests: XCTestCase {

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

    /// Seed a terminal tab into `app` and arm its pty with `panes`'
    /// first id live. Publishes nothing.
    private func seedTerminalTab(into app: AppState, projectId: String,
                                 tabId: String, paneIds: [String]) {
        let panes = paneIds.enumerated().map {
            Pane(id: $0.element, title: "Terminal \($0.offset + 1)", kind: .terminal)
        }
        let tab = Tab(id: tabId, title: "T", cwd: "/tmp/\(projectId)",
                      panes: panes, activePaneId: paneIds.first)
        app.tabs.projects = [app.tabs.projects[0],
                             Project(id: projectId, name: projectId.uppercased(),
                                     path: "/tmp/\(projectId)", tabs: [tab])]
        for id in paneIds {
            _ = app.sessions.makeSession(for: tabId, cwd: "/tmp/\(projectId)",
                                         initialTerminalPaneId: id)
        }
    }

    /// Publish a live-pane handle for `paneId` dragged from `source`.
    /// The claim closure returns a `PaneClaim` tri-state via
    /// `claimPaneForTransfer` (matching production).
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

    private func paneIds(_ app: AppState, _ tabId: String) -> [String] {
        app.tabs.tab(for: tabId)?.panes.map(\.id) ?? []
    }

    // MARK: - Terminal move

    func test_terminalPane_movesIntoTargetStripAtSlot() {
        seedTerminalTab(into: winA, projectId: "a", tabId: "a-tab", paneIds: ["pA", "pB"])
        seedTerminalTab(into: winB, projectId: "b", tabId: "b-tab", paneIds: ["pX"])
        publishDrag(from: winA, tabId: "a-tab", paneId: "pA")

        let moved = PaneMigrationCoordinator(services: services).commitCrossWindowMove(
            into: winB, targetTabId: "b-tab", relativeToPaneId: "pX", placeAfter: true
        )
        XCTAssertTrue(moved)

        // Gone from the source window, present in the target after pX.
        XCTAssertEqual(paneIds(winA, "a-tab"), ["pB"])
        XCTAssertEqual(paneIds(winB, "b-tab"), ["pX", "pA"])
        // Live pty entry migrated.
        XCTAssertEqual(winA.sessions.ptySessions["a-tab"]?.hasPane("pA"), false)
        XCTAssertEqual(winB.sessions.ptySessions["b-tab"]?.hasPane("pA"), true)
        // Target focuses the migrated pane; in-flight handle consumed.
        XCTAssertEqual(winB.tabs.tab(for: "b-tab")?.activePaneId, "pA")
        XCTAssertNil(services.livePaneRegistry.currentDrag)
    }

    func test_sameWindowDrag_isNoOp() {
        // A drag that originated in the target window must NOT be treated
        // as a cross-window move (the local reorder path handles it).
        seedTerminalTab(into: winB, projectId: "b", tabId: "b-tab", paneIds: ["pX", "pY"])
        publishDrag(from: winB, tabId: "b-tab", paneId: "pX")
        let moved = PaneMigrationCoordinator(services: services).commitCrossWindowMove(
            into: winB, targetTabId: "b-tab", relativeToPaneId: "pY", placeAfter: true
        )
        XCTAssertFalse(moved)
        XCTAssertEqual(paneIds(winB, "b-tab"), ["pX", "pY"])
    }

    func test_movingLastPane_dissolvesSourceTab_withoutTerminating() {
        // win-A's project tab has a single pane; moving it empties and
        // dissolves that tab. The Terminals project keeps its Main tab,
        // so the app does not terminate and the source window stays.
        seedTerminalTab(into: winA, projectId: "a", tabId: "a-tab", paneIds: ["pA"])
        seedTerminalTab(into: winB, projectId: "b", tabId: "b-tab", paneIds: ["pX"])
        publishDrag(from: winA, tabId: "a-tab", paneId: "pA")

        _ = PaneMigrationCoordinator(services: services).commitCrossWindowMove(
            into: winB, targetTabId: "b-tab", relativeToPaneId: nil, placeAfter: false
        )
        // Source tab dissolved (and its now-empty project removed by the
        // dissolve cascade); target gained the pane.
        XCTAssertNil(winA.tabs.tab(for: "a-tab"))
        XCTAssertEqual(paneIds(winB, "b-tab"), ["pX", "pA"])
    }

    // MARK: - Claude move

    func test_claudePane_becomesNewTabUnderMatchingProject() {
        // Source Claude tab.
        var claudePane = Pane(id: "cA", title: "Claude", kind: .claude)
        claudePane.isClaudeRunning = true
        let srcTab = Tab(id: "a-claude", title: "Repo", cwd: "/tmp/repo",
                         panes: [claudePane, Pane(id: "cA-t1", title: "Terminal 1", kind: .terminal)],
                         activePaneId: "cA", claudeSessionId: "sess-7")
        winA.tabs.projects = [winA.tabs.projects[0],
                              Project(id: "p-repo", name: "REPO", path: "/tmp/repo", tabs: [srcTab])]
        _ = winA.sessions.makeSession(for: "a-claude", cwd: "/tmp/repo", initialClaudePaneId: "cA")

        seedTerminalTab(into: winB, projectId: "b", tabId: "b-tab", paneIds: ["pX"])
        publishDrag(from: winA, tabId: "a-claude", paneId: "cA")

        let moved = PaneMigrationCoordinator(services: services).commitCrossWindowMove(
            into: winB, targetTabId: "b-tab", relativeToPaneId: "pX", placeAfter: true
        )
        XCTAssertTrue(moved)

        // A NEW tab under a recreated /tmp/repo project in win-B — NOT
        // inserted into b-tab's strip.
        XCTAssertEqual(paneIds(winB, "b-tab"), ["pX"], "Claude must not join the terminal strip.")
        let proj = winB.tabs.projects.first { $0.path == "/tmp/repo" }
        XCTAssertEqual(proj?.id, "p-repo")
        let newTab = proj?.tabs.first
        XCTAssertEqual(newTab?.claudeSessionId, "sess-7")
        XCTAssertEqual(newTab?.panes.first?.id, "cA")
        XCTAssertEqual(newTab?.panes.first?.kind, .claude)
        XCTAssertEqual(winB.sessions.ptySessions[newTab!.id]?.hasPane("cA"), true)
        // Removed from the source.
        XCTAssertFalse(winA.tabs.tab(for: "a-claude")?.panes.contains { $0.id == "cA" } ?? true)
    }
}
