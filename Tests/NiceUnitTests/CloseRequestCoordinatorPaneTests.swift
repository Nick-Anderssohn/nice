//
//  CloseRequestCoordinatorPaneTests.swift
//  NiceUnitTests
//
//  Direct coverage for `requestClosePane` and the `.pane` scope
//  through `confirmPendingClose`. The repository-wide test suite
//  exercises tab- and project-scoped closes thoroughly; the
//  pane-scope path was previously only hit transitively.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class CloseRequestCoordinatorPaneTests: XCTestCase {

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

    // MARK: - Idle pane → immediate kill

    func test_requestClosePane_idleClaude_doesNotPromptAlert() {
        // Seeded panes are unspawned, so `terminatePane` is a no-op —
        // we can't observe the kill itself, but we can pin "no alert
        // raised" since the idle path skips the busy branch.
        let seed = TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )

        appState.closer.requestClosePane(tabId: "t1", paneId: seed.claudePaneId)

        XCTAssertNil(
            appState.closer.pendingCloseRequest,
            "Idle Claude pane must not stage a confirmation alert."
        )
    }

    func test_requestClosePane_terminalPane_doesNotPromptAlert() {
        // Unspawned terminals have no foreground child to query, so
        // `shellHasForegroundChild` returns false and the pane is idle.
        let seed = TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )

        appState.closer.requestClosePane(tabId: "t1", paneId: seed.terminalPaneId)

        XCTAssertNil(appState.closer.pendingCloseRequest)
    }

    // MARK: - Busy pane → alert

    func test_requestClosePane_thinkingClaude_promptsAlert() {
        let seed = TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )
        TabModelFixtures.setClaudeStatusOnEveryTab(
            in: appState.tabs, projectId: "p1", status: .thinking
        )

        appState.closer.requestClosePane(tabId: "t1", paneId: seed.claudePaneId)

        guard case let .pane(tabId, paneId)
            = appState.closer.pendingCloseRequest?.scope
        else {
            XCTFail("Expected a pending .pane close request")
            return
        }
        XCTAssertEqual(tabId, "t1")
        XCTAssertEqual(paneId, seed.claudePaneId)
        XCTAssertEqual(
            appState.closer.pendingCloseRequest?.busyPanes.count, 1,
            "Pane-scope alert must list exactly the targeted pane."
        )
    }

    func test_requestClosePane_waitingClaude_promptsAlert() {
        let seed = TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )
        TabModelFixtures.setClaudeStatusOnEveryTab(
            in: appState.tabs, projectId: "p1", status: .waiting
        )

        appState.closer.requestClosePane(tabId: "t1", paneId: seed.claudePaneId)

        XCTAssertNotNil(appState.closer.pendingCloseRequest,
                        "Waiting Claude pane must trigger the busy alert.")
    }

    // MARK: - Unknown ids → no-op

    func test_requestClosePane_unknownTab_isNoOp() {
        let seed = TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )

        appState.closer.requestClosePane(
            tabId: "ghost-tab", paneId: seed.claudePaneId
        )

        XCTAssertNil(appState.closer.pendingCloseRequest)
        XCTAssertNotNil(
            appState.tabs.tab(for: "t1"),
            "Unknown tab must not mutate any other state."
        )
    }

    func test_requestClosePane_unknownPane_isNoOp() {
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )

        appState.closer.requestClosePane(tabId: "t1", paneId: "ghost-pane")

        XCTAssertNil(appState.closer.pendingCloseRequest)
    }

    // MARK: - confirmPendingClose with .pane scope

    func test_confirmPendingClose_paneScope_clearsRequest() {
        let seed = TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )
        TabModelFixtures.setClaudeStatusOnEveryTab(
            in: appState.tabs, projectId: "p1", status: .thinking
        )

        appState.closer.requestClosePane(tabId: "t1", paneId: seed.claudePaneId)
        XCTAssertNotNil(appState.closer.pendingCloseRequest)

        appState.closer.confirmPendingClose()

        XCTAssertNil(
            appState.closer.pendingCloseRequest,
            "Confirm must clear the pending request before dispatching the kill."
        )
    }

    func test_cancelPendingClose_paneScope_leavesPaneAlone() {
        let seed = TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )
        TabModelFixtures.setClaudeStatusOnEveryTab(
            in: appState.tabs, projectId: "p1", status: .thinking
        )

        appState.closer.requestClosePane(tabId: "t1", paneId: seed.claudePaneId)
        appState.closer.cancelPendingClose()

        XCTAssertNil(appState.closer.pendingCloseRequest)
        XCTAssertNotNil(
            appState.tabs.tab(for: "t1")?.panes.first { $0.id == seed.claudePaneId },
            "Cancel must leave the targeted pane untouched."
        )
    }
}
