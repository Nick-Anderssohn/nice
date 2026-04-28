//
//  AppStateCloseProjectTests.swift
//  NiceUnitTests
//
//  Covers the right-click → "Close Project" flow on the sidebar:
//  requestCloseProject, confirmPendingClose for the `.project` scope,
//  and the projectsPendingRemoval hand-off through paneExited. Also
//  asserts that requestCloseTab dissolves a tab whose companion
//  terminal is lazy (unspawned) — a regression caught manually when
//  the first cut of Close Project left Claude tabs as zombie terminal
//  tabs because terminatePane is a no-op on unspawned panes.
//
//  Tests seed panes via plain `Project` / `Tab` / `Pane` values and
//  never drive a pty, so every pane is "unspawned" from the
//  perspective of `ptySessions` — which is exactly the state that
//  originally exposed the lazy-companion bug.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class AppStateCloseProjectTests: XCTestCase {

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

    // MARK: - requestCloseProject

    func test_requestCloseProject_idleProject_removesProjectAndAllTabs() {
        seedProjectWithClaudeTab(projectId: "p1", tabId: "t1")
        seedProjectWithClaudeTab(projectId: "p1", tabId: "t2",
                                 appendToExistingProject: true)

        appState.closer.requestCloseProject(projectId: "p1")

        XCTAssertNil(appState.tabs.projects.first { $0.id == "p1" },
                     "Project must be removed once all its tabs dissolve.")
        XCTAssertNil(appState.tabs.tab(for: "t1"))
        XCTAssertNil(appState.tabs.tab(for: "t2"))
        XCTAssertNil(appState.closer.pendingCloseRequest,
                     "No busy panes → no confirmation prompt.")
    }

    func test_requestCloseProject_busyClaudePane_stagesPendingRequest() {
        seedProjectWithClaudeTab(projectId: "p1", tabId: "t1")
        setClaudeStatusOnEveryTab(in: "p1", status: .thinking)

        appState.closer.requestCloseProject(projectId: "p1")

        XCTAssertNotNil(appState.tabs.projects.first { $0.id == "p1" },
                        "Busy panes must block synchronous removal.")
        guard case let .project(projectId)? = appState.closer.pendingCloseRequest?.scope else {
            return XCTFail("Expected .project scope; got \(String(describing: appState.closer.pendingCloseRequest?.scope))")
        }
        XCTAssertEqual(projectId, "p1")
        XCTAssertFalse(appState.closer.pendingCloseRequest!.busyPanes.isEmpty,
                       "busyPanes must list the blocker(s) for the alert body.")
    }

    func test_requestCloseProject_terminalsGroup_isNoOp() {
        // Keep a second project around so the bare app isn't down to
        // just Terminals (the test exercises the guard, not the
        // NSApp.terminate path).
        seedProjectWithClaudeTab(projectId: "p1", tabId: "t1")
        let before = appState.tabs.projects.map(\.id)

        appState.closer.requestCloseProject(projectId: TabModel.terminalsProjectId)

        XCTAssertEqual(appState.tabs.projects.map(\.id), before,
                       "The pinned Terminals project must never be removable via right-click.")
        XCTAssertNil(appState.closer.pendingCloseRequest)
    }

    func test_requestCloseProject_unknownProjectId_isNoOp() {
        seedProjectWithClaudeTab(projectId: "p1", tabId: "t1")
        let before = appState.tabs.projects.map(\.id)

        appState.closer.requestCloseProject(projectId: "does-not-exist")

        XCTAssertEqual(appState.tabs.projects.map(\.id), before)
        XCTAssertNil(appState.closer.pendingCloseRequest)
    }

    func test_requestCloseProject_emptyProject_removesSynchronously() {
        // Keep a seeded project so the termination guard stays happy.
        seedProjectWithClaudeTab(projectId: "p1", tabId: "t1")
        appState.tabs.projects.append(
            Project(id: "p2", name: "P2", path: "/tmp/p2", tabs: [])
        )

        appState.closer.requestCloseProject(projectId: "p2")

        XCTAssertNil(appState.tabs.projects.first { $0.id == "p2" },
                     "An empty project has no async pane-exit to wait on — it must be removed synchronously.")
    }

    func test_requestCloseProject_reassignsActiveTabOffClosedProject() {
        seedProjectWithClaudeTab(projectId: "p1", tabId: "t1")
        seedProjectWithClaudeTab(projectId: "p2", tabId: "t2")
        appState.tabs.activeTabId = "t1"

        appState.closer.requestCloseProject(projectId: "p1")

        XCTAssertNotEqual(appState.tabs.activeTabId, "t1",
                          "activeTabId must move off the closed project's tabs.")
        XCTAssertNotNil(appState.tabs.activeTabId.flatMap { appState.tabs.tab(for: $0) },
                        "activeTabId must point at a real, still-existing tab.")
    }

    // MARK: - confirm / cancel on .project scope

    func test_confirmPendingClose_projectScope_tearsEverythingDown() {
        seedProjectWithClaudeTab(projectId: "p1", tabId: "t1")
        setClaudeStatusOnEveryTab(in: "p1", status: .thinking)

        appState.closer.requestCloseProject(projectId: "p1")
        XCTAssertNotNil(appState.closer.pendingCloseRequest)

        appState.closer.confirmPendingClose()

        XCTAssertNil(appState.closer.pendingCloseRequest,
                     "Confirming must clear the pending request.")
        XCTAssertNil(appState.tabs.projects.first { $0.id == "p1" },
                     "Force-quit from a .project-scoped pending close must remove the project.")
        XCTAssertNil(appState.tabs.tab(for: "t1"))
    }

    func test_cancelPendingClose_projectScope_leavesEverythingIntact() {
        seedProjectWithClaudeTab(projectId: "p1", tabId: "t1")
        setClaudeStatusOnEveryTab(in: "p1", status: .thinking)

        appState.closer.requestCloseProject(projectId: "p1")
        appState.closer.cancelPendingClose()

        XCTAssertNil(appState.closer.pendingCloseRequest)
        XCTAssertNotNil(appState.tabs.projects.first { $0.id == "p1" },
                        "Cancel must leave the project in place.")
        XCTAssertNotNil(appState.tabs.tab(for: "t1"))
    }

    // MARK: - requestCloseTab with lazy companion (regression)

    func test_requestCloseTab_claudeTabWithUnspawnedCompanion_dissolves() {
        // terminatePane is a no-op for unspawned panes, so an earlier
        // cut of Close Tab killed only the claude pane and left the
        // tab alive with its unfocused companion terminal. Seeded
        // tabs here have no pty session, so every pane is "unspawned"
        // — the exact shape that reproduced the manual bug.
        seedProjectWithClaudeTab(projectId: "p1", tabId: "t1")
        // Extra project keeps us off the all-empty NSApp.terminate path.
        seedProjectWithClaudeTab(projectId: "p2", tabId: "t2")

        appState.closer.requestCloseTab(tabId: "t1")

        XCTAssertNil(appState.tabs.tab(for: "t1"),
                     "Close Tab must dissolve the tab even when the companion terminal was never spawned.")
        XCTAssertNotNil(appState.tabs.projects.first { $0.id == "p1" },
                        "Close Tab must leave the containing project in place — only Close Project removes it.")
    }

    // MARK: - Helpers

    /// Seed a claude + terminal tab into a new or existing project
    /// without driving pty creation. Matches the shape of tabs built
    /// by `createTabFromMainTerminal` but stays in the model layer.
    private func seedProjectWithClaudeTab(
        projectId: String,
        tabId: String,
        appendToExistingProject: Bool = false
    ) {
        let claudePaneId = "\(tabId)-claude"
        let terminalPaneId = "\(tabId)-t1"
        let tab = Tab(
            id: tabId,
            title: "New tab",
            cwd: "/tmp/\(projectId)",
            branch: nil,
            panes: [
                Pane(id: claudePaneId, title: "Claude", kind: .claude),
                Pane(id: terminalPaneId, title: "Terminal 1", kind: .terminal),
            ],
            activePaneId: claudePaneId,
            claudeSessionId: "session-\(tabId)"
        )
        if appendToExistingProject,
           let pi = appState.tabs.projects.firstIndex(where: { $0.id == projectId }) {
            appState.tabs.projects[pi].tabs.append(tab)
        } else {
            let project = Project(id: projectId, name: projectId.uppercased(),
                                  path: "/tmp/\(projectId)", tabs: [tab])
            appState.tabs.projects.append(project)
        }
    }

    /// Flip every claude pane inside `projectId` to `status`. Used to
    /// force the `isBusy` path for tests that want a pending-close
    /// alert instead of an immediate tear-down.
    private func setClaudeStatusOnEveryTab(in projectId: String, status: TabStatus) {
        var projects = appState.tabs.projects
        guard let pi = projects.firstIndex(where: { $0.id == projectId }) else { return }
        for ti in projects[pi].tabs.indices {
            for pxi in projects[pi].tabs[ti].panes.indices
            where projects[pi].tabs[ti].panes[pxi].kind == .claude {
                projects[pi].tabs[ti].panes[pxi].status = status
            }
        }
        appState.tabs.projects = projects
    }
}
