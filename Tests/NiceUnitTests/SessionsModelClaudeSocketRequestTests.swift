//
//  SessionsModelClaudeSocketRequestTests.swift
//  NiceUnitTests
//
//  Pins `SessionsModel.handleClaudeSocketRequest` — the dispatch the
//  zsh `claude()` shadow function calls into via the per-window
//  control socket whenever the user types `claude` (or hits Enter on
//  a pre-typed `claude --resume <uuid>` in a deferred-resume tab).
//
//  Why this is its own file, separate from
//  `AppStatePaneLifecycleTests`: this dispatch is the ONLY production
//  writer that flips a Claude pane's `isClaudeRunning` from false to
//  true. `paneTitleChanged`'s OSC-title gate (added to keep zsh OSC
//  noise from clobbering the saved tab title on restored deferred-
//  resume tabs) is keyed on that exact transition. If the handshake
//  ever forgot to flip the flag — or flipped it for the wrong pane —
//  every restored tab in the sidebar would silently never accept its
//  real Claude title. Pin the three reply branches and the flag write
//  here so a regression in this specific transition is loud.
//
//  Branches covered:
//    • newtab + tabId is empty
//    • newtab + tabId belongs to the pinned Terminals project
//    • newtab + paneId not in tab
//    • newtab + tab already has a running claude pane
//    • inplace + args contain --resume <uuid>      (parsedId path)
//    • inplace <uuid> + args have no session id     (mint-new path)
//
//  All tests use `AppState()` (services == nil), so `makeSession`
//  doesn't spawn real ptys; the dispatch's model side effects are
//  fully observable via `appState.tabs`.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class SessionsModelClaudeSocketRequestTests: XCTestCase {

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

    // MARK: - newtab paths

    func test_emptyTabId_repliesNewtab() {
        let reply = captureReply { reply in
            appState.sessions.handleClaudeSocketRequest(
                cwd: "/tmp/x",
                args: [],
                tabId: "",
                paneId: "p",
                reply: reply
            )
        }
        XCTAssertEqual(reply, "newtab",
                       "Empty tabId means the request didn't carry a sidebar tab — fall through to a fresh tab.")
    }

    func test_terminalsProjectTab_repliesNewtab() {
        // The pinned Terminals project never hosts Claude sessions —
        // running `claude` from a Main-tab terminal opens a fresh
        // Claude tab in the matching project, never promotes the
        // Main pane in place.
        let mainTabId = TabModel.mainTerminalTabId
        let mainPaneId = appState.tabs.tab(for: mainTabId)!.panes[0].id

        let reply = captureReply { reply in
            appState.sessions.handleClaudeSocketRequest(
                cwd: "/tmp/x",
                args: [],
                tabId: mainTabId,
                paneId: mainPaneId,
                reply: reply
            )
        }

        XCTAssertEqual(reply, "newtab")
        let mainTab = appState.tabs.tab(for: mainTabId)!
        let pane = mainTab.panes.first { $0.id == mainPaneId }!
        XCTAssertEqual(pane.kind, .terminal,
                       "Main pane must NOT be promoted to .claude — Terminals never hosts Claude.")
        XCTAssertFalse(pane.isClaudeRunning,
                       "Main pane must NOT be flipped to claude-running.")
    }

    func test_paneIdNotInTab_repliesNewtab() {
        let seed = TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p", tabId: "t1",
            isClaudeRunning: false
        )

        let reply = captureReply { reply in
            appState.sessions.handleClaudeSocketRequest(
                cwd: "/tmp/p",
                args: [],
                tabId: seed.tabId,
                paneId: "does-not-exist",
                reply: reply
            )
        }

        XCTAssertEqual(reply, "newtab",
                       "Stale paneId (pane exited while the wrapper's nc was in flight) must fall through to a new tab.")
    }

    func test_existingClaudeRunning_repliesNewtab() {
        // The "at most one Claude pane per tab" invariant is enforced
        // here: if the target tab already has a live claude pane,
        // promoting the requesting pane in place would create a
        // second claude pane in the tab. Open a fresh tab instead.
        let seed = TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p", tabId: "t1",
            isClaudeRunning: true
        )

        let reply = captureReply { reply in
            appState.sessions.handleClaudeSocketRequest(
                cwd: "/tmp/p",
                args: [],
                tabId: seed.tabId,
                paneId: seed.terminalPaneId,
                reply: reply
            )
        }

        XCTAssertEqual(reply, "newtab")
        let terminalPane = appState.tabs.tab(for: seed.tabId)!
            .panes.first { $0.id == seed.terminalPaneId }!
        XCTAssertEqual(terminalPane.kind, .terminal,
                       "Terminal pane must NOT be promoted when the tab already has a running claude.")
        XCTAssertFalse(terminalPane.isClaudeRunning)
    }

    // MARK: - inplace promotion paths

    func test_inplaceWithSessionId_flipsIsClaudeRunningTrue_andRepliesInplace() {
        // The deferred-resume case: a restored tab spawned a plain
        // zsh with `claude --resume <uuid>` pre-typed; the user hit
        // Enter, the wrapper extracted the args, and the socket
        // request lands here with the session id already in `args`.
        // The flag flip from false→true is what the
        // `paneTitleChanged` gate releases on; this is the load-
        // bearing transition the test pins.
        let seed = TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p", tabId: "t1",
            sessionId: "OLD",
            isClaudeRunning: false
        )

        let reply = captureReply { reply in
            appState.sessions.handleClaudeSocketRequest(
                cwd: "/tmp/p",
                args: ["--resume", "abc-123"],
                tabId: seed.tabId,
                paneId: seed.claudePaneId,
                reply: reply
            )
        }

        XCTAssertEqual(reply, "inplace",
                       "When the wrapper already had a session id (--resume <uuid>), reply is plain 'inplace' — wrapper passes args through unchanged.")

        let tab = appState.tabs.tab(for: seed.tabId)!
        let pane = tab.panes.first { $0.id == seed.claudePaneId }!
        XCTAssertTrue(pane.isClaudeRunning,
                      "Deferred-resume promotion must flip isClaudeRunning to true — this is the gate-release signal `paneTitleChanged` keys on.")
        XCTAssertEqual(pane.kind, .claude)
        XCTAssertEqual(pane.title, "Claude",
                       "Pane title is reset to 'Claude' so the pill doesn't show stale text until the OSC arrives.")
        XCTAssertEqual(tab.activePaneId, seed.claudePaneId)
        XCTAssertEqual(tab.claudeSessionId, "abc-123",
                       "The id parsed from --resume must overwrite the seeded session id so persistence survives a relaunch.")
    }

    func test_inplaceWithoutSessionId_mintsFreshIdAndRepliesWithIt() {
        // The plain `claude` (no --resume / --session-id) case: user
        // typed `claude` in a terminal pane inside a Claude tab. We
        // mint a fresh session id and ship it back so the wrapper
        // can prepend `--session-id <uuid>` before exec'ing claude.
        let seed = TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p", tabId: "t1",
            sessionId: "OLD",
            isClaudeRunning: false
        )

        let reply = captureReply { reply in
            appState.sessions.handleClaudeSocketRequest(
                cwd: "/tmp/p",
                args: [],
                tabId: seed.tabId,
                paneId: seed.terminalPaneId,
                reply: reply
            )
        }

        XCTAssertTrue(reply.hasPrefix("inplace "),
                      "Reply must be 'inplace <uuid>' when args carry no session id.")
        let mintedId = String(reply.dropFirst("inplace ".count))
        XCTAssertFalse(mintedId.isEmpty,
                       "Reply must include the freshly minted uuid.")

        let tab = appState.tabs.tab(for: seed.tabId)!
        XCTAssertEqual(tab.claudeSessionId, mintedId,
                       "The minted id must be the new tab claudeSessionId — wrapper and model must agree on what to persist.")

        // Promotion of a terminal pane: kind flips, isClaudeRunning
        // flips, pane title resets to "Claude".
        let pane = tab.panes.first { $0.id == seed.terminalPaneId }!
        XCTAssertEqual(pane.kind, .claude,
                       "Terminal pane promotes to .claude kind in place.")
        XCTAssertTrue(pane.isClaudeRunning)
        XCTAssertEqual(pane.title, "Claude")
    }

    // MARK: - Helpers

    /// Drive `handleClaudeSocketRequest` and return the single string
    /// it passed to `reply`. The production reply closure is `@Sendable`
    /// (called from the socket queue), but in unit tests the dispatch
    /// runs on the test's MainActor and the closure fires synchronously
    /// before the caller returns — capturing into a local is safe.
    private func captureReply(
        _ drive: (@escaping @Sendable (String) -> Void) -> Void
    ) -> String {
        nonisolated(unsafe) var captured: String?
        drive { reply in captured = reply }
        return captured ?? ""
    }
}
