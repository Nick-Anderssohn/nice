//
//  AppStatePaneLifecycleTests.swift
//  NiceUnitTests
//
//  Covers the pane lifecycle / title / auto-title helpers on AppState
//  that sit between "the pty said something" and "the sidebar updates."
//  These used to be tested only via manual runs; regressions (wrong
//  active-pane shift on exit, Claude status prefix not parsed, title
//  humanization dropping words) would only surface once the app was
//  launched and clicked through.
//
//  Tests use the convenience `AppState()` init (services == nil), which
//  disables SessionStore persistence so nothing here touches the user's
//  real sessions.json. Multiple projects are seeded before calling
//  paneExited — otherwise dissolving the Terminals tab's last pane
//  leaves every project empty and `NSApp.terminate(nil)` fires.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class AppStatePaneLifecycleTests: XCTestCase {

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

    // MARK: - livePaneCounts

    func test_livePaneCounts_initialMainTabSingleTerminal() {
        let counts = appState.livePaneCounts
        XCTAssertEqual(counts.claude, 0)
        XCTAssertEqual(counts.terminal, 1,
                       "Fresh AppState seeds the Terminals project with one Main tab, one terminal pane.")
    }

    func test_livePaneCounts_mixedClaudeAndTerminal() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")

        let counts = appState.livePaneCounts
        XCTAssertEqual(counts.claude, 1)
        XCTAssertEqual(counts.terminal, 2,
                       "Main + the seeded claude tab's companion terminal = 2 terminals.")
    }

    func test_livePaneCounts_deadPanesExcluded() {
        // Flip the Main terminal pane to dead; counts should drop to zero.
        let mainId = AppState.mainTerminalTabId
        let paneId = appState.tab(for: mainId)!.panes[0].id
        mutateTabTestOnly(id: mainId) { tab in
            guard let pi = tab.panes.firstIndex(where: { $0.id == paneId }) else { return }
            tab.panes[pi].isAlive = false
        }

        XCTAssertEqual(appState.livePaneCounts.terminal, 0,
                       "isAlive == false panes must not be counted.")
    }

    // MARK: - paneExited

    func test_paneExited_removesPaneAndShiftsActivePaneToNeighbor() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        appState.activeTabId = "t1"
        let tab = appState.tab(for: "t1")!
        XCTAssertEqual(tab.panes.count, 2)
        let claudePaneId = tab.panes[0].id
        let terminalPaneId = tab.panes[1].id

        // Focus claude pane, then simulate its exit — focus should
        // shift to the neighbor (terminal pane at index 1).
        appState.setActivePane(tabId: "t1", paneId: claudePaneId)
        appState.paneExited(tabId: "t1", paneId: claudePaneId, exitCode: 0)

        let after = appState.tab(for: "t1")!
        XCTAssertEqual(after.panes.count, 1)
        XCTAssertEqual(after.panes[0].id, terminalPaneId)
        XCTAssertEqual(after.activePaneId, terminalPaneId,
                       "Focus must shift to the surviving pane; leaving activePaneId pointing at a removed pane would break the toolbar.")
    }

    func test_paneExited_lastPaneDissolvesTab() {
        // Seed two projects so dissolving one tab doesn't empty
        // everything (which would fire NSApp.terminate).
        seedProjectWithClaudeTab(projectId: "p1", tabId: "t1")
        seedProjectWithClaudeTab(projectId: "p2", tabId: "t2")
        XCTAssertNotNil(appState.tab(for: "t1"))

        let tab = appState.tab(for: "t1")!
        for pane in tab.panes {
            appState.paneExited(tabId: "t1", paneId: pane.id, exitCode: 0)
        }

        XCTAssertNil(appState.tab(for: "t1"),
                     "Tab must dissolve once every pane exits.")
        XCTAssertNotNil(appState.tab(for: "t2"),
                        "Other tabs must not be touched by one tab's dissolve.")
    }

    func test_paneExited_dissolvedActiveTab_fallsBackToFirstAvailable() {
        seedProjectWithClaudeTab(projectId: "p1", tabId: "t1")
        seedProjectWithClaudeTab(projectId: "p2", tabId: "t2")
        appState.activeTabId = "t1"

        let tab = appState.tab(for: "t1")!
        for pane in tab.panes {
            appState.paneExited(tabId: "t1", paneId: pane.id, exitCode: 0)
        }

        // Dissolving the active tab must leave activeTabId pointing at
        // something valid (the first tab in sidebar order — the
        // Terminals Main tab).
        XCTAssertEqual(appState.activeTabId, AppState.mainTerminalTabId)
    }

    func test_paneExited_unknownPane_isNoOp() {
        let before = appState.livePaneCounts
        appState.paneExited(tabId: AppState.mainTerminalTabId,
                            paneId: "does-not-exist",
                            exitCode: 0)
        XCTAssertEqual(appState.livePaneCounts.terminal, before.terminal,
                       "Unknown paneId must not corrupt state.")
    }

    // MARK: - paneTitleChanged

    func test_paneTitleChanged_terminalPane_updatesPaneTitle() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let terminalId = appState.tab(for: "t1")!.panes[1].id

        appState.paneTitleChanged(tabId: "t1", paneId: terminalId, title: "nvim foo.rb")

        let pane = appState.tab(for: "t1")!.panes.first { $0.id == terminalId }!
        XCTAssertEqual(pane.title, "nvim foo.rb")
    }

    func test_paneTitleChanged_terminalPane_emptyTitleIgnored() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let terminalId = appState.tab(for: "t1")!.panes[1].id
        let before = appState.tab(for: "t1")!.panes.first { $0.id == terminalId }!.title

        appState.paneTitleChanged(tabId: "t1", paneId: terminalId, title: "   \n")

        let pane = appState.tab(for: "t1")!.panes.first { $0.id == terminalId }!
        XCTAssertEqual(pane.title, before, "Whitespace-only titles must not overwrite the current title.")
    }

    func test_paneTitleChanged_terminalPane_clipsAt40Chars() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let terminalId = appState.tab(for: "t1")!.panes[1].id
        let long = String(repeating: "x", count: 80)

        appState.paneTitleChanged(tabId: "t1", paneId: terminalId, title: long)

        let pane = appState.tab(for: "t1")!.panes.first { $0.id == terminalId }!
        XCTAssertEqual(pane.title.count, 40, "Terminal titles must cap at 40 chars so the toolbar pill doesn't overflow.")
    }

    func test_paneTitleChanged_claudePane_brailleSpinner_setsThinking() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let claudeId = appState.tab(for: "t1")!.panes[0].id
        // U+2840 is inside the braille spinner range 0x2800..0x28FF
        // that Claude Code uses for its "thinking" indicator.
        let title = "\u{2840} fix-top-bar-height"

        appState.paneTitleChanged(tabId: "t1", paneId: claudeId, title: title)

        let pane = appState.tab(for: "t1")!.panes.first { $0.id == claudeId }!
        XCTAssertEqual(pane.status, .thinking)
        // And the trailing label should humanize into the tab title.
        let tab = appState.tab(for: "t1")!
        XCTAssertEqual(tab.title, "Fix top bar height")
    }

    func test_paneTitleChanged_claudePane_sparkle_setsWaiting() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let claudeId = appState.tab(for: "t1")!.panes[0].id
        // U+2733 (✳) is the sparkle Claude uses for "waiting for input."
        let title = "\u{2733} needs-input"

        appState.paneTitleChanged(tabId: "t1", paneId: claudeId, title: title)

        let pane = appState.tab(for: "t1")!.panes.first { $0.id == claudeId }!
        XCTAssertEqual(pane.status, .waiting)
    }

    func test_paneTitleChanged_claudePane_placeholderLabelIgnored() {
        // "Claude Code" is the generic placeholder Claude emits before
        // a session has a real name — must not clobber an existing tab
        // title with it.
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let claudeId = appState.tab(for: "t1")!.panes[0].id

        // Set a real title first, then send the placeholder. The real
        // title must survive.
        appState.paneTitleChanged(tabId: "t1", paneId: claudeId,
                                  title: "\u{2840} fix-bug")
        XCTAssertEqual(appState.tab(for: "t1")!.title, "Fix bug")

        appState.paneTitleChanged(tabId: "t1", paneId: claudeId,
                                  title: "\u{2840} Claude Code")
        XCTAssertEqual(appState.tab(for: "t1")!.title, "Fix bug",
                       "Placeholder 'Claude Code' must not overwrite a real session title.")
    }

    func test_paneTitleChanged_claudePane_unknownPrefix_treatedAsLabel() {
        // A non-braille, non-sparkle first char means no status update —
        // the whole string is the label.
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let claudeId = appState.tab(for: "t1")!.panes[0].id

        appState.paneTitleChanged(tabId: "t1", paneId: claudeId,
                                  title: "refactor-auth-layer")

        XCTAssertEqual(appState.tab(for: "t1")!.title, "Refactor auth layer")
    }

    // MARK: - applyAutoTitle

    func test_applyAutoTitle_humanizesKebabCase() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        appState.applyAutoTitle(tabId: "t1", rawTitle: "fix-top-bar-height")
        XCTAssertEqual(appState.tab(for: "t1")!.title, "Fix top bar height")
    }

    func test_applyAutoTitle_humanizesSnakeCase() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        appState.applyAutoTitle(tabId: "t1", rawTitle: "fix_top_bar_height")
        XCTAssertEqual(appState.tab(for: "t1")!.title, "Fix top bar height")
    }

    func test_applyAutoTitle_capsAt40Chars() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let long = String(repeating: "x", count: 60)
        appState.applyAutoTitle(tabId: "t1", rawTitle: long)

        let title = appState.tab(for: "t1")!.title
        XCTAssertLessThanOrEqual(title.count, 40)
    }

    func test_applyAutoTitle_whitespaceOnly_isNoop() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let before = appState.tab(for: "t1")!.title

        appState.applyAutoTitle(tabId: "t1", rawTitle: "   \n  ")
        XCTAssertEqual(appState.tab(for: "t1")!.title, before)
    }

    func test_applyAutoTitle_setsTitleAutoGeneratedFlag() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        XCTAssertFalse(appState.tab(for: "t1")!.titleAutoGenerated,
                       "Seeded tab starts with manual title.")

        appState.applyAutoTitle(tabId: "t1", rawTitle: "fix-bug")
        XCTAssertTrue(appState.tab(for: "t1")!.titleAutoGenerated,
                      "applyAutoTitle must mark the title as auto-generated so a future manual rename can opt out.")
    }

    // MARK: - Helpers

    /// Seed a new project with a claude + terminal tab without going
    /// through createTabFromMainTerminal (which depends on the control
    /// socket + claude binary). The pane objects exist in the data
    /// model only — no pty — which is fine for logic-layer tests.
    private func seedProjectWithClaudeTab(projectId: String, tabId: String) {
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
        let project = Project(id: projectId, name: projectId.uppercased(),
                              path: "/tmp/\(projectId)", tabs: [tab])
        appState.projects.append(project)
    }

    /// Mutate a tab in-place from the test by rewriting the projects
    /// array. `mutateTab` in AppState is private; poking via
    /// `projects = ...` is the cleanest way to set up specific pane
    /// state without adding a test-only API.
    private func mutateTabTestOnly(id: String, _ transform: (inout Tab) -> Void) {
        var projects = appState.projects
        for pi in projects.indices {
            guard let ti = projects[pi].tabs.firstIndex(where: { $0.id == id }) else {
                continue
            }
            transform(&projects[pi].tabs[ti])
            appState.projects = projects
            return
        }
    }
}
