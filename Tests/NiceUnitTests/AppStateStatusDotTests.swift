//
//  AppStateStatusDotTests.swift
//  NiceUnitTests
//
//  End-to-end tests for the sidebar/toolbar status-dot sync invariant.
//  The sidebar reads `Tab.status` / `Tab.waitingAcknowledged`; the
//  toolbar pill reads `Pane.status` / `Pane.waitingAcknowledged`. These
//  tests drive `AppState.paneTitleChanged` (the real bug entry point)
//  and assert the two surfaces never disagree.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class AppStateStatusDotTests: XCTestCase {

    private var appState: AppState!

    override func setUp() {
        super.setUp()
        appState = AppState()
    }

    override func tearDown() {
        appState = nil
        super.tearDown()
    }

    // MARK: - Regression: the reported bug
    //
    // Repro:
    //   1. New Claude tab (claude pane + companion terminal).
    //   2. Claude starts thinking — sidebar + toolbar both orange-pulse.
    //   3. User keyboard-shortcuts to the companion terminal.
    //   4. Claude transitions to waiting.
    //   5. Before the fix: sidebar stuck on orange pulse; toolbar shows
    //      blue pulse on the Claude pill. After the fix: both show blue
    //      pulse, because tab.status is a pure function of pane.status.

    func test_paneTitleChanged_onInactiveClaudePane_updatesTabStatus() {
        let tabId = createClaudeTab(cwd: "/tmp/nice-status-dot-test")
        guard let tab = appState.tab(for: tabId),
              let claude = tab.panes.first(where: { $0.kind == .claude }),
              let term = tab.panes.first(where: { $0.kind == .terminal })
        else {
            XCTFail("Expected claude + terminal panes on fresh Claude tab")
            return
        }

        // User focuses the terminal pane — the setup for the bug.
        appState.setActivePane(tabId: tabId, paneId: term.id)

        // Claude emits a braille-prefixed title: thinking.
        appState.paneTitleChanged(
            tabId: tabId, paneId: claude.id, title: "\u{2800} fix-bug"
        )
        XCTAssertEqual(appState.tab(for: tabId)?.status, .thinking,
                       "tab.status must track the Claude pane even when the companion terminal is active.")
        XCTAssertEqual(appState.tab(for: tabId)?.panes.first(where: { $0.id == claude.id })?.status,
                       .thinking)

        // Claude transitions to waiting — the exact moment the sidebar
        // used to freeze on thinking.
        appState.paneTitleChanged(
            tabId: tabId, paneId: claude.id, title: "\u{2733} fix-bug"
        )
        let after = appState.tab(for: tabId)
        XCTAssertEqual(after?.status, .waiting,
                       "Sidebar dot MUST match toolbar dot: both were supposed to flip blue.")
        XCTAssertEqual(after?.panes.first(where: { $0.id == claude.id })?.status,
                       .waiting)
        XCTAssertFalse(after?.waitingAcknowledged ?? true,
                       "User is not on the Claude pane, so the pulse must not be suppressed.")
    }

    func test_paneTitleChanged_onActiveClaudePane_acksWaiting() {
        let tabId = createClaudeTab(cwd: "/tmp/nice-status-dot-test")
        guard let tab = appState.tab(for: tabId),
              let claude = tab.panes.first(where: { $0.kind == .claude })
        else {
            XCTFail("Expected claude pane")
            return
        }
        // Claude pane is already active after tab creation. Ensure the
        // user is also viewing the tab.
        appState.activeTabId = tabId
        XCTAssertEqual(appState.tab(for: tabId)?.activePaneId, claude.id)

        appState.paneTitleChanged(
            tabId: tabId, paneId: claude.id, title: "\u{2733} hello"
        )
        let t = appState.tab(for: tabId)
        XCTAssertEqual(t?.status, .waiting)
        XCTAssertTrue(t?.waitingAcknowledged ?? false,
                      "Waiting that arrives while the user is on the Claude pane must land already-acked — no pulse.")
    }

    func test_sidebarAndToolbar_agreeAfterArbitraryTransitions() {
        let tabId = createClaudeTab(cwd: "/tmp/nice-status-dot-test")
        guard let tab = appState.tab(for: tabId),
              let claude = tab.panes.first(where: { $0.kind == .claude }),
              let term = tab.panes.first(where: { $0.kind == .terminal })
        else {
            XCTFail("Expected claude + terminal panes")
            return
        }

        let transitions: [(activePane: String, title: String, expected: TabStatus)] = [
            (claude.id, "\u{2800} step1", .thinking),
            (term.id,   "\u{2733} step2", .waiting),
            (term.id,   "\u{2800} step3", .thinking),
            (claude.id, "\u{2733} step4", .waiting),
        ]

        for step in transitions {
            appState.setActivePane(tabId: tabId, paneId: step.activePane)
            appState.paneTitleChanged(
                tabId: tabId, paneId: claude.id, title: step.title
            )
            let t = appState.tab(for: tabId)
            let claudePane = t?.panes.first(where: { $0.id == claude.id })
            XCTAssertEqual(t?.status, step.expected,
                           "tab.status (sidebar source) drifted at step '\(step.title)'")
            XCTAssertEqual(t?.status, claudePane?.status,
                           "tab.status must equal the Claude pane's status at step '\(step.title)' — sidebar and toolbar must read the same state.")
        }
    }

    // MARK: - Invariant: one Claude pane per tab

    func test_createTabFromMainTerminal_hasExactlyOneClaudePane() {
        let tabId = createClaudeTab(cwd: "/tmp/nice-status-dot-test")
        let count = appState.tab(for: tabId)?.panes
            .filter { $0.kind == .claude }.count
        XCTAssertEqual(count, 1)
    }

    func test_addPane_cannotCreateClaudePane() {
        let tabId = createClaudeTab(cwd: "/tmp/nice-status-dot-test")
        let result = appState.addPane(tabId: tabId, kind: .claude)
        XCTAssertNil(result,
                     "addPane must refuse to create a second Claude pane — that would violate the one-Claude-per-tab invariant.")

        let claudeCount = appState.tab(for: tabId)?.panes
            .filter { $0.kind == .claude }.count
        XCTAssertEqual(claudeCount, 1)
    }

    // MARK: - Helpers

    /// Create a Claude tab via the real `createTabFromMainTerminal`
    /// path. This spawns a pty (zsh) in the background — tests only
    /// assert on the data-model surface, not on the process.
    @discardableResult
    private func createClaudeTab(cwd: String) -> String {
        let before = allTabIds()
        appState.createTabFromMainTerminal(cwd: cwd, args: [])
        let after = allTabIds()
        let newIds = after.subtracting(before)
        guard let id = newIds.first else {
            XCTFail("createTabFromMainTerminal did not produce a new tab")
            return ""
        }
        return id
    }

    private func allTabIds() -> Set<String> {
        var ids: Set<String> = []
        for project in appState.projects {
            for tab in project.tabs {
                ids.insert(tab.id)
            }
        }
        return ids
    }
}
