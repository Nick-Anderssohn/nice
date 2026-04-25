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
        seedClaudeTab(projectId: "p", tabId: "t1", initialSessionId: "S1")

        appState.handleClaudeSessionUpdate(
            paneId: "definitely-not-a-real-pane-id",
            sessionId: "should-be-ignored"
        )

        XCTAssertEqual(
            appState.tab(for: "t1")?.claudeSessionId, "S1",
            "unknown paneId must not mutate any tab"
        )
    }

    func test_updatesTargetTab_whenMultipleProjectsExist() {
        seedClaudeTab(projectId: "p1", tabId: "t1", initialSessionId: "S1")
        seedClaudeTab(projectId: "p2", tabId: "t2", initialSessionId: "S2")
        seedClaudeTab(projectId: "p3", tabId: "t3", initialSessionId: "S3")

        // Update the middle tab. Other tabs must stay untouched —
        // tabIdOwning's reverse scan must hit the right project even
        // when it's not first.
        appState.handleClaudeSessionUpdate(
            paneId: "t2-claude", sessionId: "S2-NEW"
        )

        XCTAssertEqual(appState.tab(for: "t1")?.claudeSessionId, "S1")
        XCTAssertEqual(appState.tab(for: "t2")?.claudeSessionId, "S2-NEW")
        XCTAssertEqual(appState.tab(for: "t3")?.claudeSessionId, "S3")
    }

    func test_resolvesByPaneId_notTabId() {
        // Pane ids and tab ids are different namespaces. The reverse
        // scan keys off the pane list, not the tab id, so passing a
        // tab id (even an existing one) must not match a tab.
        seedClaudeTab(projectId: "p", tabId: "t1", initialSessionId: "S1")

        appState.handleClaudeSessionUpdate(
            paneId: "t1", // tab id, not a pane id
            sessionId: "should-not-apply"
        )

        XCTAssertEqual(
            appState.tab(for: "t1")?.claudeSessionId, "S1",
            "tabId-shaped paneId must not match pane list"
        )
    }

    // MARK: - Idempotency

    func test_redundantUpdateLeavesValueUnchanged() {
        // Same id twice — the second call has nothing to do. We can't
        // observe scheduleSessionSave from the test (services == nil
        // disables persistence), but the public state must round-trip
        // cleanly.
        seedClaudeTab(projectId: "p", tabId: "t1", initialSessionId: "S1")

        appState.handleClaudeSessionUpdate(paneId: "t1-claude", sessionId: "S1")
        appState.handleClaudeSessionUpdate(paneId: "t1-claude", sessionId: "S1")

        XCTAssertEqual(appState.tab(for: "t1")?.claudeSessionId, "S1")
    }

    func test_newSessionIdReplacesOld() {
        seedClaudeTab(projectId: "p", tabId: "t1", initialSessionId: "OLD")

        appState.handleClaudeSessionUpdate(
            paneId: "t1-claude", sessionId: "NEW"
        )

        XCTAssertEqual(appState.tab(for: "t1")?.claudeSessionId, "NEW")
    }

    // MARK: - helpers

    private func seedClaudeTab(
        projectId: String, tabId: String, initialSessionId: String
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
            claudeSessionId: initialSessionId
        )
        let project = Project(
            id: projectId, name: projectId.uppercased(),
            path: "/tmp/\(projectId)", tabs: [tab]
        )
        appState.projects.append(project)
    }
}
