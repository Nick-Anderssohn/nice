//
//  AppStateClaudeSessionUpdateTests.swift
//  NiceUnitTests
//
//  Locks down `handleClaudeSessionUpdate(paneId:sessionId:source:cwd:)`
//  — the reverse-index path Claude Code's SessionStart hook uses to
//  tell Nice "this pane's session id is now X" (and "Claude is now
//  running in cwd Y"). Important behaviors:
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
            source: nil, cwd: nil
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
            paneId: "t2-claude", sessionId: "S2-NEW", source: nil, cwd: nil
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
            source: nil, cwd: nil
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

        appState.sessions.handleClaudeSessionUpdate(paneId: "t1-claude", sessionId: "S1", source: nil, cwd: nil)
        appState.sessions.handleClaudeSessionUpdate(paneId: "t1-claude", sessionId: "S1", source: nil, cwd: nil)

        XCTAssertEqual(appState.tabs.tab(for: "t1")?.claudeSessionId, "S1")
    }

    func test_newSessionIdReplacesOld() {
        seedClaudeTab(projectId: "p", tabId: "t1", sessionId: "OLD")

        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude", sessionId: "NEW", source: nil, cwd: nil
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
            paneId: "tB-claude", sessionId: "LEAKED", source: nil, cwd: nil
        )

        XCTAssertEqual(appState.tabs.tab(for: "tA")?.claudeSessionId, "A-INIT",
                       "A's tab must be untouched by a B-shaped paneId")
        XCTAssertEqual(stateB.tabs.tab(for: "tB")?.claudeSessionId, "B-INIT",
                       "B's tab must be untouched until B's own handler is invoked")

        // B's own handler does mutate B.
        stateB.sessions.handleClaudeSessionUpdate(
            paneId: "tB-claude", sessionId: "B-NEW", source: nil, cwd: nil
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
            paneId: "t1-claude", sessionId: "S1-LIVE", source: nil, cwd: nil
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
            paneId: "t1-claude", sessionId: "S1-STALE", source: nil, cwd: nil
        )

        XCTAssertEqual(
            appState.tabs.tab(for: "t1")?.claudeSessionId, "S1-LIVE",
            "stale paneId must not mutate the surviving tab's claudeSessionId"
        )
    }

    // MARK: - Cwd update path
    //
    // The SessionStart hook now forwards Claude's current cwd alongside
    // the session id. Production uses that to keep `tab.cwd` aligned
    // when Claude moves into a worktree mid-session — bare `claude -w`
    // (auto-named worktree the args parser can't predict) and
    // `/worktree` slash commands both swap Claude's working directory
    // without restarting the process. The cwd field is the only way
    // Nice learns about those moves.

    func test_cwdUpdate_matchingCurrent_isNoOp() {
        // Steady state: every SessionStart hook emits cwd, even when
        // the rotation didn't move directories (`/clear`, `/compact`).
        // The handler must coalesce same-cwd reports so the save layer
        // doesn't churn on every prompt.
        seedClaudeTab(
            projectId: "p", tabId: "t1", sessionId: "S1",
            projectPath: "/Users/nick/Projects/notes"
        )
        var callbackHits = 0
        appState.sessions.onSessionMutation = { callbackHits += 1 }

        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude",
            sessionId: "S1",
            source: "clear",
            cwd: "/Users/nick/Projects/notes"
        )

        XCTAssertEqual(
            appState.tabs.tab(for: "t1")?.cwd,
            "/Users/nick/Projects/notes",
            "matching cwd must leave tab.cwd untouched"
        )
        XCTAssertEqual(
            callbackHits, 0,
            "matching cwd + matching session id must not fire onSessionMutation — both branches short-circuit"
        )
    }

    func test_cwdUpdate_differing_updatesTabAndClaudePane() {
        // The shape this whole feature was built to fix: bare
        // `claude -w` lands in an auto-named worktree, the
        // SessionStart hook forwards the worktree path, and `tab.cwd`
        // moves to match. Claude pane.cwd was nil (no OSC 7 ever
        // fires on a Claude pane) so it follows the tab.
        seedClaudeTab(
            projectId: "p", tabId: "t1", sessionId: "S1",
            projectPath: "/Users/nick/Projects/notes"
        )
        var callbackHits = 0
        appState.sessions.onSessionMutation = { callbackHits += 1 }

        let worktree = "/Users/nick/Projects/notes/.claude/worktrees/auto-name"
        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude",
            sessionId: "S1",
            source: "startup",
            cwd: worktree
        )

        let tab = appState.tabs.tab(for: "t1")
        XCTAssertEqual(tab?.cwd, worktree, "tab.cwd must move to the worktree path")
        let claudePane = tab?.panes.first(where: { $0.kind == .claude })
        XCTAssertEqual(
            claudePane?.cwd, worktree,
            "Claude pane.cwd (nil before) must follow the tab into the worktree"
        )
        XCTAssertGreaterThan(
            callbackHits, 0,
            "cwd change must fire onSessionMutation so the save flush picks it up"
        )
    }

    func test_cwdUpdate_companionTerminal_followsWhenMatchingOldCwd() {
        // A terminal companion whose `Pane.cwd` still matches the
        // pre-update `tab.cwd` is "still following the tab" — it
        // hasn't been `cd`'d anywhere via OSC 7 yet. Pulling it along
        // keeps a later-spawned shell from landing back in the project
        // root instead of inside the worktree.
        seedClaudeTab(
            projectId: "p", tabId: "t1", sessionId: "S1",
            projectPath: "/Users/nick/Projects/notes"
        )
        // Explicitly stamp the terminal pane's cwd to match the tab's
        // — simulating a freshly-restored pane that hasn't yet had a
        // chance to emit OSC 7.
        appState.tabs.mutateTab(id: "t1") { tab in
            for i in tab.panes.indices where tab.panes[i].kind == .terminal {
                tab.panes[i].cwd = "/Users/nick/Projects/notes"
            }
        }

        let worktree = "/Users/nick/Projects/notes/.claude/worktrees/auto-name"
        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude",
            sessionId: "S1",
            source: "startup",
            cwd: worktree
        )

        let tab = appState.tabs.tab(for: "t1")
        let termPane = tab?.panes.first(where: { $0.kind == .terminal })
        XCTAssertEqual(
            termPane?.cwd, worktree,
            "terminal pane whose cwd matched the old tab.cwd must follow into the worktree"
        )
    }

    func test_cwdUpdate_companionTerminal_diverged_staysPut() {
        // A terminal companion that has *already* tracked the user
        // somewhere else via OSC 7 must NOT be snapped back into the
        // Claude pane's worktree. Preserves the user's terminal
        // context across the rotation.
        seedClaudeTab(
            projectId: "p", tabId: "t1", sessionId: "S1",
            projectPath: "/Users/nick/Projects/notes"
        )
        let userCd = "/Users/nick/Projects/notes/some/subdir"
        appState.tabs.mutateTab(id: "t1") { tab in
            for i in tab.panes.indices where tab.panes[i].kind == .terminal {
                tab.panes[i].cwd = userCd
            }
        }

        let worktree = "/Users/nick/Projects/notes/.claude/worktrees/auto-name"
        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude",
            sessionId: "S1",
            source: "startup",
            cwd: worktree
        )

        let tab = appState.tabs.tab(for: "t1")
        let termPane = tab?.panes.first(where: { $0.kind == .terminal })
        XCTAssertEqual(
            termPane?.cwd, userCd,
            "diverged terminal pane.cwd (OSC-7-tracked) must stay put — only the Claude pane and tab follow"
        )
    }

    func test_cwdUpdate_nilPaneCwd_followsTheTab() {
        // The Claude pane never emits OSC 7 (it's a Claude process,
        // not a shell), so its `pane.cwd` is always nil at this
        // point. The rule "nil pane.cwd follows the tab" is what
        // makes the Claude pane track the worktree. This test
        // exercises that path directly via a terminal pane with
        // nil cwd, which has the same shape.
        seedClaudeTab(
            projectId: "p", tabId: "t1", sessionId: "S1",
            projectPath: "/Users/nick/Projects/notes"
        )
        // Confirm starting state: terminal pane.cwd is nil from
        // seedClaudeTab.
        let preTab = appState.tabs.tab(for: "t1")
        XCTAssertNil(
            preTab?.panes.first(where: { $0.kind == .terminal })?.cwd,
            "precondition: terminal pane.cwd starts nil from the fixture"
        )

        let worktree = "/Users/nick/Projects/notes/.claude/worktrees/auto-name"
        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude",
            sessionId: "S1",
            source: "startup",
            cwd: worktree
        )

        let termPane = appState.tabs.tab(for: "t1")?
            .panes.first(where: { $0.kind == .terminal })
        XCTAssertEqual(
            termPane?.cwd, worktree,
            "nil pane.cwd must be treated as still-following and inherit the new tab.cwd"
        )
    }

    func test_cwdUpdate_nilCwdInPayload_isNoOp() {
        // Older hook scripts on disk during an upgrade emit no cwd
        // field. The socket layer normalizes that to nil, and the
        // handler must short-circuit without touching tab.cwd.
        seedClaudeTab(
            projectId: "p", tabId: "t1", sessionId: "S1",
            projectPath: "/Users/nick/Projects/notes"
        )

        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude",
            sessionId: "S1",
            source: "clear",
            cwd: nil
        )

        XCTAssertEqual(
            appState.tabs.tab(for: "t1")?.cwd,
            "/Users/nick/Projects/notes",
            "nil cwd in payload must leave tab.cwd untouched"
        )
    }

    func test_cwdUpdate_emptyCwdInPayload_isNoOp() {
        // Defense-in-depth: the socket layer should already collapse
        // empty-string cwd to nil, but the handler's emptiness guard
        // makes the same call even if the socket regressed. Useful
        // belt-and-suspenders given the cwd field came from a
        // user-modifiable hook script.
        seedClaudeTab(
            projectId: "p", tabId: "t1", sessionId: "S1",
            projectPath: "/Users/nick/Projects/notes"
        )

        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude",
            sessionId: "S1",
            source: "clear",
            cwd: ""
        )

        XCTAssertEqual(
            appState.tabs.tab(for: "t1")?.cwd,
            "/Users/nick/Projects/notes",
            "empty-string cwd must be treated as nil"
        )
    }

    func test_cwdUpdate_identicalUpdatesFireCallbackExactlyOnce() {
        // Two consecutive same-cwd updates: only the first should
        // mutate state and fire `onSessionMutation`; the second is
        // already at the target value and short-circuits in
        // mutateTab's change-detection.
        seedClaudeTab(
            projectId: "p", tabId: "t1", sessionId: "S1",
            projectPath: "/Users/nick/Projects/notes"
        )
        var hits = 0
        appState.sessions.onSessionMutation = { hits += 1 }

        let worktree = "/Users/nick/Projects/notes/.claude/worktrees/auto-name"
        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude", sessionId: "S1", source: "clear", cwd: worktree
        )
        let afterFirst = hits
        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude", sessionId: "S1", source: "clear", cwd: worktree
        )

        XCTAssertGreaterThan(afterFirst, 0, "first update must fire onSessionMutation")
        XCTAssertEqual(
            hits, afterFirst,
            "redundant identical update must not refire onSessionMutation"
        )
    }

    // MARK: - Branch + cwd ordering
    //
    // `/branch` (source=resume + id-change) spawns a sibling parent
    // tab pinned to the OLD session id. The pre-rotation transcript
    // lives in the old bucket, so the sibling must inherit the OLD
    // cwd. If `updateTabCwd` ran before `materializeBranchParent`,
    // the sibling would pick up the post-rotation worktree cwd and
    // its own resume would point at the wrong bucket — i.e. exactly
    // the bug this whole feature exists to prevent, reintroduced in
    // a sneakier place. Pin the ordering here.

    func test_branchRotation_withCwdMove_siblingInheritsOldCwd() {
        let originalCwd = "/Users/nick/Projects/notes"
        seedClaudeTab(
            projectId: "p", tabId: "t1", sessionId: "OLD-ID",
            projectPath: originalCwd
        )
        // Pin the originating tab's pre-rotation state.
        XCTAssertEqual(appState.tabs.tab(for: "t1")?.cwd, originalCwd)

        let newCwd = "\(originalCwd)/.claude/worktrees/auto-name"
        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude",
            sessionId: "NEW-ID",
            source: "resume",
            cwd: newCwd
        )

        // The originating tab — same id, post-rotation — now sits in
        // the worktree.
        XCTAssertEqual(
            appState.tabs.tab(for: "t1")?.cwd, newCwd,
            "originating tab must reflect the post-rotation cwd"
        )
        XCTAssertEqual(
            appState.tabs.tab(for: "t1")?.claudeSessionId, "NEW-ID",
            "originating tab's session id must be the new id"
        )

        // The sibling parent — newly inserted, pinned to OLD-ID —
        // must hold the pre-rotation cwd. Locate it by session id.
        let project = appState.tabs.projects.first { $0.id == "p" }
        let sibling = project?.tabs.first { $0.claudeSessionId == "OLD-ID" }
        XCTAssertNotNil(sibling, "branch rotation must materialize a sibling parent tab")
        XCTAssertEqual(
            sibling?.cwd, originalCwd,
            "sibling parent must inherit the OLD cwd — its old-id transcript lives in the pre-rotation bucket"
        )
    }

    // MARK: - helpers

    private func seedClaudeTab(
        projectId: String,
        tabId: String,
        sessionId: String,
        projectPath: String? = nil
    ) {
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs,
            projectId: projectId,
            tabId: tabId,
            sessionId: sessionId,
            projectPath: projectPath
        )
    }
}
