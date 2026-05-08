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
//  tabs because terminatePane is a no-op on unspawned panes — and
//  the through-line for the right-click → Close bug on a never-
//  focused resume-deferred Claude tab (entry exists, pty never
//  forked, terminatePane silently returned without firing paneExited
//  before the armed-but-not-fired fast path was added).
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

    func test_requestCloseTab_armedDeferredClaudePaneWithUnspawnedCompanion_dissolves() {
        // Repro for the right-click → Close bug on a never-focused
        // resume-deferred Claude tab. After window restore the
        // Claude pane's NiceTerminalView captures a deferred zsh
        // spawn, but if the user never clicks the tab the gate
        // never fires (no non-zero frame in a window). The pane's
        // entry exists in the session's `entries` dict, so
        // `paneIsSpawned` returns true and `hardKillTab` routes it
        // through `terminatePane`. Before the fix, `terminatePane`
        // hit `guard pid > 0 else { return }` and silently no-op'd
        // without firing `paneExited`. The companion terminal pane
        // (also unspawned, but with no entry at all) was dropped
        // by hardKillTab's unspawned branch — but the never-fired
        // Claude pane stayed in `tab.panes`, so the tab welded to
        // the sidebar.
        //
        // The synthetic-armed-deferred seam mirrors the production
        // post-cancel state at the model layer: `paneIsSpawned`
        // true, `terminatePane` fires `paneExited(_, _, nil)`
        // synchronously. Drives the same control-flow path the
        // user hits in production without standing up a real
        // SwiftTerm view that AppKit would resize away from .zero.
        let seed = TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )
        // Extra project keeps us off the all-empty NSApp.terminate path.
        seedProjectWithClaudeTab(projectId: "p2", tabId: "t2")
        // The restored Claude pane is rendered as Claude but no
        // claude process is running yet — match the production
        // resume-deferred pane state at the model layer.
        appState.tabs.mutateTab(id: "t1") { tab in
            guard let pi = tab.panes.firstIndex(where: { $0.id == seed.claudePaneId })
            else { return }
            tab.panes[pi].isClaudeRunning = false
        }
        appState.sessions.markSyntheticArmedDeferredPaneForTesting(
            tabId: "t1", paneId: seed.claudePaneId
        )

        appState.closer.requestCloseTab(tabId: "t1")

        XCTAssertNil(
            appState.tabs.tab(for: "t1"),
            "Close Tab on a never-focused resume-deferred Claude tab must dissolve the sidebar row — that's the bug."
        )
        XCTAssertNotNil(
            appState.tabs.projects.first { $0.id == "p1" },
            "Close Tab must leave the containing project in place."
        )
    }

    func test_requestCloseTab_heldClaudePaneWithUnspawnedCompanion_dissolves() {
        // Repro for the held-pane close bug: `claude -w foo` outside
        // a git repo exits non-zero, TabPtySession holds the pane open
        // so the user can read the error, then the user right-clicks
        // the sidebar tab and picks Close. Before the hardKillTab
        // reorder, terminatePane on the held pane fired `paneExited`
        // synchronously while the unspawned companion was still in
        // tab.panes — `onTabBecameEmpty` saw a non-empty list and
        // skipped the dissolve, so the panes vanished but the sidebar
        // row stayed.
        let seed = TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )
        // Extra project keeps us off the all-empty NSApp.terminate path.
        seedProjectWithClaudeTab(projectId: "p2", tabId: "t2")
        // Mirror the production held-pane state at the model layer:
        // `Pane.isAlive = false` (the bookkeeping `paneHeld` does on
        // the SessionsModel side) plus the synthetic seam that makes
        // `paneIsSpawned` true and `terminatePane` fire synchronously.
        appState.tabs.mutateTab(id: "t1") { tab in
            guard let pi = tab.panes.firstIndex(where: { $0.id == seed.claudePaneId })
            else { return }
            tab.panes[pi].isAlive = false
            tab.panes[pi].isClaudeRunning = false
        }
        appState.sessions.markSyntheticHeldPaneForTesting(
            tabId: "t1", paneId: seed.claudePaneId
        )

        appState.closer.requestCloseTab(tabId: "t1")

        XCTAssertNil(
            appState.tabs.tab(for: "t1"),
            "Close Tab on a held-pane tab must dissolve the sidebar row, not just remove the panes."
        )
        XCTAssertNotNil(
            appState.tabs.projects.first { $0.id == "p1" },
            "Close Tab on the held-pane tab must leave the containing project in place."
        )
    }

    // MARK: - Helpers

    private func seedProjectWithClaudeTab(
        projectId: String,
        tabId: String,
        appendToExistingProject: Bool = false
    ) {
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs,
            projectId: projectId,
            tabId: tabId,
            appendToExisting: appendToExistingProject
        )
    }

    private func setClaudeStatusOnEveryTab(in projectId: String, status: TabStatus) {
        TabModelFixtures.setClaudeStatusOnEveryTab(
            in: appState.tabs, projectId: projectId, status: status
        )
    }
}
