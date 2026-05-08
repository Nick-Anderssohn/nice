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
        let counts = appState.tabs.livePaneCounts
        XCTAssertEqual(counts.claude, 0)
        XCTAssertEqual(counts.terminal, 1,
                       "Fresh AppState seeds the Terminals project with one Main tab, one terminal pane.")
    }

    func test_livePaneCounts_mixedClaudeAndTerminal() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")

        let counts = appState.tabs.livePaneCounts
        XCTAssertEqual(counts.claude, 1)
        XCTAssertEqual(counts.terminal, 2,
                       "Main + the seeded claude tab's companion terminal = 2 terminals.")
    }

    func test_livePaneCounts_deadPanesExcluded() {
        // Flip the Main terminal pane to dead; counts should drop to zero.
        let mainId = TabModel.mainTerminalTabId
        let paneId = appState.tabs.tab(for: mainId)!.panes[0].id
        mutateTabTestOnly(id: mainId) { tab in
            guard let pi = tab.panes.firstIndex(where: { $0.id == paneId }) else { return }
            tab.panes[pi].isAlive = false
        }

        XCTAssertEqual(appState.tabs.livePaneCounts.terminal, 0,
                       "isAlive == false panes must not be counted.")
    }

    // MARK: - paneExited

    func test_paneExited_removesPaneAndShiftsActivePaneToNeighbor() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        appState.tabs.activeTabId = "t1"
        let tab = appState.tabs.tab(for: "t1")!
        XCTAssertEqual(tab.panes.count, 2)
        let claudePaneId = tab.panes[0].id
        let terminalPaneId = tab.panes[1].id

        // Focus claude pane, then simulate its exit — focus should
        // shift to the neighbor (terminal pane at index 1).
        appState.sessions.setActivePane(tabId: "t1", paneId: claudePaneId)
        appState.sessions.paneExited(tabId: "t1", paneId: claudePaneId, exitCode: 0)

        let after = appState.tabs.tab(for: "t1")!
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
        XCTAssertNotNil(appState.tabs.tab(for: "t1"))

        let tab = appState.tabs.tab(for: "t1")!
        for pane in tab.panes {
            appState.sessions.paneExited(tabId: "t1", paneId: pane.id, exitCode: 0)
        }

        XCTAssertNil(appState.tabs.tab(for: "t1"),
                     "Tab must dissolve once every pane exits.")
        XCTAssertNotNil(appState.tabs.tab(for: "t2"),
                        "Other tabs must not be touched by one tab's dissolve.")
    }

    func test_paneExited_dissolvedActiveTab_fallsBackToFirstAvailable() {
        seedProjectWithClaudeTab(projectId: "p1", tabId: "t1")
        seedProjectWithClaudeTab(projectId: "p2", tabId: "t2")
        appState.tabs.activeTabId = "t1"

        let tab = appState.tabs.tab(for: "t1")!
        for pane in tab.panes {
            appState.sessions.paneExited(tabId: "t1", paneId: pane.id, exitCode: 0)
        }

        // Dissolving the active tab must leave activeTabId pointing at
        // something valid (the first tab in sidebar order — the
        // Terminals Main tab).
        XCTAssertEqual(appState.tabs.activeTabId, TabModel.mainTerminalTabId)
    }

    func test_paneExited_unknownPane_isNoOp() {
        let before = appState.tabs.livePaneCounts
        appState.sessions.paneExited(tabId: TabModel.mainTerminalTabId,
                            paneId: "does-not-exist",
                            exitCode: 0)
        XCTAssertEqual(appState.tabs.livePaneCounts.terminal, before.terminal,
                       "Unknown paneId must not corrupt state.")
    }

    // MARK: - paneHeld

    func test_paneHeld_flipsIsAliveAndIdlesStatus() {
        // The held-on-exit feature: when claude (or any process) exits
        // non-cleanly, the pane is held open with its scrollback
        // visible. This handler is what flips the model state to "dead
        // but still on screen" so the rest of the app (sidebar dot,
        // hasClaude, livePaneCounts, isBusy) stops treating the pane
        // as live. Pane sits in `.thinking` before the exit (typical:
        // claude was mid-response when it crashed); hold must idle
        // it out.
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let claudePaneId = appState.tabs.tab(for: "t1")!.panes[0].id
        mutateTabTestOnly(id: "t1") { tab in
            guard let pi = tab.panes.firstIndex(where: { $0.id == claudePaneId })
            else { return }
            tab.panes[pi].status = .thinking
            tab.panes[pi].waitingAcknowledged = false
        }

        appState.sessions.paneHeld(tabId: "t1", paneId: claudePaneId, exitCode: 1)

        let pane = appState.tabs.tab(for: "t1")!.panes
            .first(where: { $0.id == claudePaneId })!
        XCTAssertFalse(pane.isAlive,
                       "paneHeld must flip isAlive to false so the rest of the model treats the pane as dead.")
        XCTAssertEqual(pane.status, .idle,
                       "paneHeld must idle out the status — a held-dead pane is not thinking or waiting.")
        XCTAssertFalse(pane.waitingAcknowledged,
                       "paneHeld must clear waitingAcknowledged so a future fresh waiting pane can pulse again.")
        XCTAssertFalse(pane.isClaudeRunning,
                       "paneHeld must clear isClaudeRunning so a fresh `claude` invocation in this tab is routed correctly (no stale promotion target).")
    }

    func test_paneHeld_keepsPaneInTabPanesArray() {
        // The whole point of the hold is that the pane (and therefore
        // its toolbar pill + SwiftTerm view) stays mounted; only
        // `paneExited` removes it. Distinct contract from `paneExited`
        // — a regression that confused the two would make held panes
        // vanish from the toolbar.
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let claudePaneId = appState.tabs.tab(for: "t1")!.panes[0].id
        let paneCountBefore = appState.tabs.tab(for: "t1")!.panes.count

        appState.sessions.paneHeld(tabId: "t1", paneId: claudePaneId, exitCode: 1)

        let after = appState.tabs.tab(for: "t1")!
        XCTAssertEqual(after.panes.count, paneCountBefore,
                       "paneHeld must not remove the pane from tab.panes — that's paneExited's job.")
        XCTAssertNotNil(after.panes.first(where: { $0.id == claudePaneId }),
                        "The held pane must still be findable by id.")
    }

    func test_paneHeld_clearsLaunchOverlay() {
        // Exit-before-first-byte case: the "Launching…" overlay was
        // still visible when the process died (e.g. claude
        // mis-resolves and exits in <0.75s). Without clearing the
        // overlay, the launch placeholder would sit on top of the
        // dead-pane footer until something else cleared it. Set the
        // grace to zero so `registerPaneLaunch` promotes synchronously
        // — no DispatchQueue dance in tests.
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        appState.sessions.launchOverlayGraceSeconds = 0
        let claudePaneId = appState.tabs.tab(for: "t1")!.panes[0].id
        appState.sessions.registerPaneLaunch(paneId: claudePaneId, command: "claude")
        XCTAssertNotNil(appState.sessions.paneLaunchStates[claudePaneId],
                        "Pre-condition: launch overlay entry must exist before paneHeld fires.")

        appState.sessions.paneHeld(tabId: "t1", paneId: claudePaneId, exitCode: 1)

        XCTAssertNil(appState.sessions.paneLaunchStates[claudePaneId],
                     "paneHeld must clear the launch overlay; otherwise an exit-before-first-byte leaves the placeholder stuck on top of the held pane's footer.")
    }

    func test_paneHeld_unknownPane_isNoOp() {
        // Defensive: a callback fires for a tab that's already been
        // dissolved or for a pane id that's not in the model.
        let before = appState.tabs.livePaneCounts
        appState.sessions.paneHeld(
            tabId: TabModel.mainTerminalTabId,
            paneId: "does-not-exist",
            exitCode: 1
        )
        XCTAssertEqual(appState.tabs.livePaneCounts.terminal, before.terminal,
                       "Unknown paneId must not corrupt state.")
    }

    // MARK: - paneTitleChanged

    func test_paneTitleChanged_terminalPane_updatesPaneTitle() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let terminalId = appState.tabs.tab(for: "t1")!.panes[1].id

        appState.sessions.paneTitleChanged(tabId: "t1", paneId: terminalId, title: "nvim foo.rb")

        let pane = appState.tabs.tab(for: "t1")!.panes.first { $0.id == terminalId }!
        XCTAssertEqual(pane.title, "nvim foo.rb")
    }

    func test_paneTitleChanged_terminalPane_emptyTitleIgnored() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let terminalId = appState.tabs.tab(for: "t1")!.panes[1].id
        let before = appState.tabs.tab(for: "t1")!.panes.first { $0.id == terminalId }!.title

        appState.sessions.paneTitleChanged(tabId: "t1", paneId: terminalId, title: "   \n")

        let pane = appState.tabs.tab(for: "t1")!.panes.first { $0.id == terminalId }!
        XCTAssertEqual(pane.title, before, "Whitespace-only titles must not overwrite the current title.")
    }

    func test_paneTitleChanged_terminalPane_clipsAt40Chars() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let terminalId = appState.tabs.tab(for: "t1")!.panes[1].id
        let long = String(repeating: "x", count: 80)

        appState.sessions.paneTitleChanged(tabId: "t1", paneId: terminalId, title: long)

        let pane = appState.tabs.tab(for: "t1")!.panes.first { $0.id == terminalId }!
        XCTAssertEqual(pane.title.count, 40, "Terminal titles must cap at 40 chars so the toolbar pill doesn't overflow.")
    }

    func test_paneTitleChanged_claudePane_brailleSpinner_setsThinking() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let claudeId = appState.tabs.tab(for: "t1")!.panes[0].id
        // U+2840 is inside the braille spinner range 0x2800..0x28FF
        // that Claude Code uses for its "thinking" indicator.
        let title = "\u{2840} fix-top-bar-height"

        appState.sessions.paneTitleChanged(tabId: "t1", paneId: claudeId, title: title)

        let pane = appState.tabs.tab(for: "t1")!.panes.first { $0.id == claudeId }!
        XCTAssertEqual(pane.status, .thinking)
        // And the trailing label should humanize into the tab title.
        let tab = appState.tabs.tab(for: "t1")!
        XCTAssertEqual(tab.title, "Fix top bar height")
    }

    func test_paneTitleChanged_claudePane_sparkle_setsWaiting() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let claudeId = appState.tabs.tab(for: "t1")!.panes[0].id
        // U+2733 (✳) is the sparkle Claude uses for "waiting for input."
        let title = "\u{2733} needs-input"

        appState.sessions.paneTitleChanged(tabId: "t1", paneId: claudeId, title: title)

        let pane = appState.tabs.tab(for: "t1")!.panes.first { $0.id == claudeId }!
        XCTAssertEqual(pane.status, .waiting)
    }

    func test_paneTitleChanged_claudePane_placeholderLabelIgnored() {
        // "Claude Code" is the generic placeholder Claude emits before
        // a session has a real name — must not clobber an existing tab
        // title with it.
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let claudeId = appState.tabs.tab(for: "t1")!.panes[0].id

        // Set a real title first, then send the placeholder. The real
        // title must survive.
        appState.sessions.paneTitleChanged(tabId: "t1", paneId: claudeId,
                                  title: "\u{2840} fix-bug")
        XCTAssertEqual(appState.tabs.tab(for: "t1")!.title, "Fix bug")

        appState.sessions.paneTitleChanged(tabId: "t1", paneId: claudeId,
                                  title: "\u{2840} Claude Code")
        XCTAssertEqual(appState.tabs.tab(for: "t1")!.title, "Fix bug",
                       "Placeholder 'Claude Code' must not overwrite a real session title.")
    }

    func test_paneTitleChanged_claudePane_unknownPrefix_treatedAsLabel() {
        // A non-braille, non-sparkle first char means no status update —
        // the whole string is the label.
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let claudeId = appState.tabs.tab(for: "t1")!.panes[0].id

        appState.sessions.paneTitleChanged(tabId: "t1", paneId: claudeId,
                                  title: "refactor-auth-layer")

        XCTAssertEqual(appState.tabs.tab(for: "t1")!.title, "Refactor auth layer")
    }

    func test_paneTitleChanged_claudePane_deferredResume_ignoresShellTitle() {
        // Restored Claude tabs (and freshly-materialized /branch parent
        // tabs) spawn `/bin/zsh -il` in `.resumeDeferred` mode — the
        // pty is plain zsh until the user hits Enter on the pre-typed
        // `claude --resume <uuid>`. zsh themes (oh-my-zsh, p10k, …)
        // emit OSC window titles like "user@host:cwd" on every prompt;
        // those must NOT clobber the persisted Claude session label.
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p", tabId: "t1",
            isClaudeRunning: false
        )
        let claudeId = appState.tabs.tab(for: "t1")!.panes[0].id
        appState.tabs.applyAutoTitle(tabId: "t1", rawTitle: "fix-top-bar-height")
        XCTAssertEqual(appState.tabs.tab(for: "t1")!.title, "Fix top bar height",
                       "Precondition: tab has a real auto-titled label.")

        appState.sessions.paneTitleChanged(tabId: "t1", paneId: claudeId,
                                  title: "Nick@Nicks MacBook Air:~/Projects/nice")

        XCTAssertEqual(appState.tabs.tab(for: "t1")!.title, "Fix top bar height",
                       "OSC titles from a deferred-resume Claude pane (zsh, not claude) " +
                       "must not overwrite the persisted session title.")
    }

    func test_paneTitleChanged_claudePane_deferredResume_ignoresStatusPrefix() {
        // Defensive: braille/sparkle prefixes from a non-claude process
        // (extremely unlikely from zsh themes, but cheap to pin) must
        // not flip the pane status either while `isClaudeRunning` is
        // false — the spinner/sparkle vocabulary belongs to claude.
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p", tabId: "t1",
            isClaudeRunning: false
        )
        let claudeId = appState.tabs.tab(for: "t1")!.panes[0].id
        let titleBefore = appState.tabs.tab(for: "t1")!.title

        appState.sessions.paneTitleChanged(tabId: "t1", paneId: claudeId,
                                  title: "\u{2840} fix-bug")

        let pane = appState.tabs.tab(for: "t1")!.panes.first { $0.id == claudeId }!
        XCTAssertEqual(pane.status, .idle,
                       "Status transitions are gated on isClaudeRunning.")
        XCTAssertEqual(appState.tabs.tab(for: "t1")!.title, titleBefore,
                       "Tab title must not change while isClaudeRunning is false.")
    }

    func test_paneTitleChanged_claudePane_acceptsTitleAfterPromotion() {
        // The full deferred-resume → live-claude story. A regression
        // that breaks promotion (e.g. `handleClaudeSocketRequest`
        // forgets to flip `isClaudeRunning`) would leave a restored
        // tab forever silent — the gate would hold against zsh, but
        // also hold against the real claude OSC stream that arrives
        // after promotion. Pin both halves: gate holds before the
        // flip, and releases after.
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p", tabId: "t1",
            isClaudeRunning: false
        )
        let claudeId = appState.tabs.tab(for: "t1")!.panes[0].id
        let titleBefore = appState.tabs.tab(for: "t1")!.title

        // Pre-promotion: zsh OSC ignored.
        appState.sessions.paneTitleChanged(tabId: "t1", paneId: claudeId,
                                  title: "Nick@host:~/repo")
        XCTAssertEqual(appState.tabs.tab(for: "t1")!.title, titleBefore,
                       "Gate must hold before isClaudeRunning flips true.")

        // Simulate the socket-handshake promotion that flips the flag.
        // We don't drive `handleClaudeSocketRequest` here (that path is
        // covered separately in SessionsModelClaudeSocketRequestTests);
        // poking the flag directly keeps this test focused on the gate.
        mutateTabTestOnly(id: "t1") { tab in
            guard let pi = tab.panes.firstIndex(where: { $0.id == claudeId }) else { return }
            tab.panes[pi].isClaudeRunning = true
        }

        // Post-promotion: real claude OSC accepted, status flips to
        // thinking, label humanizes onto the tab title.
        appState.sessions.paneTitleChanged(tabId: "t1", paneId: claudeId,
                                  title: "\u{2840} fix-bug")
        let pane = appState.tabs.tab(for: "t1")!.panes.first { $0.id == claudeId }!
        XCTAssertEqual(pane.status, .thinking,
                       "Status transition must fire once isClaudeRunning flips true.")
        XCTAssertEqual(appState.tabs.tab(for: "t1")!.title, "Fix bug",
                       "Auto-title must apply once the gate releases.")
    }

    // MARK: - applyAutoTitle

    func test_applyAutoTitle_humanizesKebabCase() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        appState.tabs.applyAutoTitle(tabId: "t1", rawTitle: "fix-top-bar-height")
        XCTAssertEqual(appState.tabs.tab(for: "t1")!.title, "Fix top bar height")
    }

    func test_applyAutoTitle_humanizesSnakeCase() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        appState.tabs.applyAutoTitle(tabId: "t1", rawTitle: "fix_top_bar_height")
        XCTAssertEqual(appState.tabs.tab(for: "t1")!.title, "Fix top bar height")
    }

    func test_applyAutoTitle_capsAt40Chars() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let long = String(repeating: "x", count: 60)
        appState.tabs.applyAutoTitle(tabId: "t1", rawTitle: long)

        let title = appState.tabs.tab(for: "t1")!.title
        XCTAssertLessThanOrEqual(title.count, 40)
    }

    func test_applyAutoTitle_whitespaceOnly_isNoop() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        let before = appState.tabs.tab(for: "t1")!.title

        appState.tabs.applyAutoTitle(tabId: "t1", rawTitle: "   \n  ")
        XCTAssertEqual(appState.tabs.tab(for: "t1")!.title, before)
    }

    func test_applyAutoTitle_setsTitleAutoGeneratedFlag() {
        seedProjectWithClaudeTab(projectId: "p", tabId: "t1")
        XCTAssertFalse(appState.tabs.tab(for: "t1")!.titleAutoGenerated,
                       "Seeded tab starts with manual title.")

        appState.tabs.applyAutoTitle(tabId: "t1", rawTitle: "fix-bug")
        XCTAssertTrue(appState.tabs.tab(for: "t1")!.titleAutoGenerated,
                      "applyAutoTitle must mark the title as auto-generated so a future manual rename can opt out.")
    }

    // MARK: - Helpers

    private func seedProjectWithClaudeTab(projectId: String, tabId: String) {
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: projectId, tabId: tabId
        )
    }

    /// Mutate a tab in-place from the test by rewriting the projects
    /// array. `mutateTab` in AppState is private; poking via
    /// `projects = ...` is the cleanest way to set up specific pane
    /// state without adding a test-only API.
    private func mutateTabTestOnly(id: String, _ transform: (inout Tab) -> Void) {
        var projects = appState.tabs.projects
        for pi in projects.indices {
            guard let ti = projects[pi].tabs.firstIndex(where: { $0.id == id }) else {
                continue
            }
            transform(&projects[pi].tabs[ti])
            appState.tabs.projects = projects
            return
        }
    }
}
