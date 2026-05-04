//
//  AppStateClaudeSessionUpdateTests.swift
//  NiceUnitTests
//
//  Locks down `handleClaudeSessionUpdate(paneId:sessionId:)` — the
//  reverse-index path the Claude UserPromptSubmit hook uses to tell
//  Nice "this pane's session id is now X." Important behaviors:
//    • unknown paneId is a silent no-op (stale pane, or hook fired
//      from a non-Nice claude that happens to share the socket path)
//    • the right tab is updated when multiple projects each have
//      claude tabs
//    • a redundant update with the same id leaves observable state
//      unchanged
//
//  Tests use the convenience `AppState()` init (services == nil), which
//  disables SessionStore persistence — same pattern the other AppState
//  tests use.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class AppStateClaudeSessionUpdateTests: XCTestCase {

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

    // MARK: - Lookup

    func test_unknownPaneId_isNoOp() {
        seedClaudeTab(projectId: "p", tabId: "t1", sessionId: "S1")

        appState.sessions.handleClaudeSessionUpdate(
            paneId: "definitely-not-a-real-pane-id",
            sessionId: "should-be-ignored",
            source: nil
        )

        XCTAssertEqual(
            appState.tabs.tab(for: "t1")?.claudeSessionId, "S1",
            "unknown paneId must not mutate any tab"
        )
    }

    func test_updatesTargetTab_whenMultipleProjectsExist() {
        seedClaudeTab(projectId: "p1", tabId: "t1", sessionId: "S1")
        seedClaudeTab(projectId: "p2", tabId: "t2", sessionId: "S2")
        seedClaudeTab(projectId: "p3", tabId: "t3", sessionId: "S3")

        // Update the middle tab. Other tabs must stay untouched —
        // tabIdOwning's reverse scan must hit the right project even
        // when it's not first.
        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t2-claude", sessionId: "S2-NEW", source: nil
        )

        XCTAssertEqual(appState.tabs.tab(for: "t1")?.claudeSessionId, "S1")
        XCTAssertEqual(appState.tabs.tab(for: "t2")?.claudeSessionId, "S2-NEW")
        XCTAssertEqual(appState.tabs.tab(for: "t3")?.claudeSessionId, "S3")
    }

    func test_resolvesByPaneId_notTabId() {
        // Pane ids and tab ids are different namespaces. The reverse
        // scan keys off the pane list, not the tab id, so passing a
        // tab id (even an existing one) must not match a tab.
        seedClaudeTab(projectId: "p", tabId: "t1", sessionId: "S1")

        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1", // tab id, not a pane id
            sessionId: "should-not-apply",
            source: nil
        )

        XCTAssertEqual(
            appState.tabs.tab(for: "t1")?.claudeSessionId, "S1",
            "tabId-shaped paneId must not match pane list"
        )
    }

    // MARK: - Idempotency

    func test_redundantUpdateLeavesValueUnchanged() {
        // Same id twice — the second call has nothing to do. We can't
        // observe scheduleSessionSave from the test (services == nil
        // disables persistence), but the public state must round-trip
        // cleanly.
        seedClaudeTab(projectId: "p", tabId: "t1", sessionId: "S1")

        appState.sessions.handleClaudeSessionUpdate(paneId: "t1-claude", sessionId: "S1", source: nil)
        appState.sessions.handleClaudeSessionUpdate(paneId: "t1-claude", sessionId: "S1", source: nil)

        XCTAssertEqual(appState.tabs.tab(for: "t1")?.claudeSessionId, "S1")
    }

    func test_newSessionIdReplacesOld() {
        seedClaudeTab(projectId: "p", tabId: "t1", sessionId: "OLD")

        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude", sessionId: "NEW", source: nil
        )

        XCTAssertEqual(appState.tabs.tab(for: "t1")?.claudeSessionId, "NEW")
    }

    // MARK: - Per-window scoping
    //
    // `tabIdOwning(paneId:)` is a method on `TabModel`, not a global
    // index — each AppState scopes the lookup to its own projects.
    // This pins the per-window scoping so a future "centralize the
    // index" refactor doesn't accidentally cross-route session updates
    // between windows.

    func test_handleSessionUpdate_isScopedToOwningWindow() {
        // Window A owns paneId "tA-claude". Window B owns "tB-claude".
        seedClaudeTab(projectId: "pA", tabId: "tA", sessionId: "A-INIT")

        let stateB = AppState()
        defer { _ = stateB } // suppress "never read" if the compiler gets clever
        TabModelFixtures.seedClaudeTab(
            into: stateB.tabs,
            projectId: "pB", tabId: "tB", sessionId: "B-INIT"
        )

        // Cross-window send: A's socket receives a paneId belonging to
        // B. A's `tabIdOwning` returns nil (B's pane isn't in A's
        // projects), so the call is a no-op on A. B is also untouched
        // because nothing dispatched to B's handler.
        appState.sessions.handleClaudeSessionUpdate(
            paneId: "tB-claude", sessionId: "LEAKED", source: nil
        )

        XCTAssertEqual(appState.tabs.tab(for: "tA")?.claudeSessionId, "A-INIT",
                       "A's tab must be untouched by a B-shaped paneId")
        XCTAssertEqual(stateB.tabs.tab(for: "tB")?.claudeSessionId, "B-INIT",
                       "B's tab must be untouched until B's own handler is invoked")

        // B's own handler does mutate B.
        stateB.sessions.handleClaudeSessionUpdate(
            paneId: "tB-claude", sessionId: "B-NEW", source: nil
        )
        XCTAssertEqual(stateB.tabs.tab(for: "tB")?.claudeSessionId, "B-NEW")
        XCTAssertEqual(appState.tabs.tab(for: "tA")?.claudeSessionId, "A-INIT",
                       "B's mutation must not bleed into A")
    }

    // MARK: - Stale-pane race
    //
    // The hook fires asynchronously: a `session_update` over the socket
    // can land after the pane it refers to has already exited. This is
    // distinct from the "unknown paneId" case above — here the paneId
    // *was* valid moments earlier. The handler must short-circuit cleanly
    // (the live tab's `claudeSessionId` must not be mutated, and nothing
    // must crash).

    func test_stalePaneId_afterPaneExited_isNoOp() {
        seedClaudeTab(projectId: "p", tabId: "t1", sessionId: "S1")

        // First update lands while the pane is alive — proves baseline.
        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude", sessionId: "S1-LIVE", source: nil
        )
        XCTAssertEqual(appState.tabs.tab(for: "t1")?.claudeSessionId, "S1-LIVE")

        // Pane exits (production path: pty closes, paneExited fires).
        appState.sessions.paneExited(
            tabId: "t1", paneId: "t1-claude", exitCode: 0
        )
        XCTAssertNil(
            appState.tabs.tab(for: "t1")?.panes.first(where: { $0.id == "t1-claude" }),
            "precondition: claude pane must be gone after paneExited"
        )

        // A late `session_update` for the now-defunct pane arrives. The
        // tab still exists (its terminal pane is alive), but the paneId
        // no longer maps to it.
        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude", sessionId: "S1-STALE", source: nil
        )

        XCTAssertEqual(
            appState.tabs.tab(for: "t1")?.claudeSessionId, "S1-LIVE",
            "stale paneId must not mutate the surviving tab's claudeSessionId"
        )
    }

    // MARK: - helpers

    private func seedClaudeTab(
        projectId: String, tabId: String, sessionId: String
    ) {
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs,
            projectId: projectId,
            tabId: tabId,
            sessionId: sessionId
        )
    }
}
