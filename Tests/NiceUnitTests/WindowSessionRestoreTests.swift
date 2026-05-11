//
//  WindowSessionRestoreTests.swift
//  NiceUnitTests
//
//  Direct coverage for `WindowSession.restoreSavedWindow`'s three
//  branches: matched-non-empty adopts that window, matched-but-empty
//  falls back to the first non-empty unclaimed slot, and unmatched
//  adopts an unclaimed slot (or stays fresh when every non-empty slot
//  is already claimed by a sibling window in this process). The
//  deferred Claude-spawn double-`DispatchQueue.main.async` is left
//  untested per the spec — the synchronous code path is the more
//  important branch and the deferred path requires a real pty.
//
//  Each test reseeds `FakeSessionStore.state`, resets the static
//  `claimedWindowIds` claim set, and drives `restoreSavedWindow`
//  directly. Snapshots use Claude-only tabs so `addRestoredTabModel`'s
//  Claude branch returns spawn info instead of calling
//  `sessions.makeSession` — no real pty work happens during the
//  assertion window.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class WindowSessionRestoreTests: XCTestCase {

    private var fake: FakeSessionStore!
    private var tabs: TabModel!
    private var sessions: SessionsModel!
    private var sidebar: SidebarModel!

    override func setUp() {
        super.setUp()
        WindowSession._testing_resetClaimedWindowIds()
        fake = FakeSessionStore()
        tabs = TabModel(initialMainCwd: "/tmp/nice-restore-tests")
        sessions = SessionsModel(tabs: tabs)
        sidebar = SidebarModel(initialCollapsed: false, initialMode: .tabs)
    }

    override func tearDown() {
        sessions?.tearDown()
        sessions = nil
        tabs = nil
        sidebar = nil
        fake = nil
        WindowSession._testing_resetClaimedWindowIds()
        super.tearDown()
    }

    // MARK: - Matched adoption

    func test_restore_matchedNonEmpty_adoptsThatWindow() {
        let claudeTab = makePersistedClaudeTab(id: "t-alpha", sessionId: "sid-alpha")
        let project = makePersistedProject(id: "proj-alpha", tabs: [claudeTab])
        let window = makePersistedWindow(
            id: "win-1",
            activeTabId: "t-alpha",
            projects: [makeEmptyTerminalsProject(), project]
        )
        fake.state = PersistedState(version: PersistedState.currentVersion, windows: [window])

        let ws = makeWindowSession(windowSessionId: "win-1")
        ws.restoreSavedWindow()

        XCTAssertEqual(ws.windowSessionId, "win-1",
                       "Matched adoption should not rotate the window id.")
        XCTAssertTrue(WindowSession._testing_isClaimed("win-1"),
                      "Adopted slot must land in claimedWindowIds so siblings won't poach it.")
        XCTAssertEqual(tabs.projects.map(\.id),
                       [TabModel.terminalsProjectId, "proj-alpha"],
                       "Snapshot project order must round-trip with Terminals at index 0.")
        XCTAssertEqual(tabs.tab(for: "t-alpha")?.claudeSessionId, "sid-alpha",
                       "Restored Claude tab must carry its session id for --resume.")
        XCTAssertEqual(tabs.activeTabId, "t-alpha",
                       "Snapshot activeTabId should be honoured when the tab survives restore.")
    }

    // MARK: - Matched-but-empty fallback

    func test_restore_matchedButEmpty_fallsBackToFirstNonEmpty() {
        // Matched slot exists but carries no projects (a prior crash
        // mid-restore wrote an empty entry). Adoption should fall
        // through to the first non-empty unclaimed slot.
        let emptyMatched = makePersistedWindow(id: "win-1", projects: [])
        let claudeTab = makePersistedClaudeTab(id: "t-recover", sessionId: "sid-recover")
        let recoveryWindow = makePersistedWindow(
            id: "win-recovery",
            projects: [
                makeEmptyTerminalsProject(),
                makePersistedProject(id: "proj-recover", tabs: [claudeTab]),
            ]
        )
        fake.state = PersistedState(
            version: PersistedState.currentVersion,
            windows: [emptyMatched, recoveryWindow]
        )

        let ws = makeWindowSession(windowSessionId: "win-1")
        ws.restoreSavedWindow()

        XCTAssertEqual(ws.windowSessionId, "win-recovery",
                       "Falling back to a different slot must rotate windowSessionId so subsequent saves target it.")
        XCTAssertTrue(WindowSession._testing_isClaimed("win-recovery"))
        XCTAssertNotNil(tabs.tab(for: "t-recover"),
                        "Recovered tab from the non-empty slot must be in the rebuilt tree.")
    }

    // MARK: - Unmatched adoption

    func test_restore_unmatched_adoptsUnclaimedSlot() {
        // No saved entry has windowSessionId == "win-fresh". The
        // first non-empty unclaimed slot ("orphan") should be adopted
        // as a first-launch migration.
        let claudeTab = makePersistedClaudeTab(id: "t-orphan", sessionId: "sid-orphan")
        let orphan = makePersistedWindow(
            id: "orphan",
            projects: [
                makeEmptyTerminalsProject(),
                makePersistedProject(id: "proj-orphan", tabs: [claudeTab]),
            ]
        )
        fake.state = PersistedState(
            version: PersistedState.currentVersion, windows: [orphan]
        )

        let ws = makeWindowSession(windowSessionId: "win-fresh")
        ws.restoreSavedWindow()

        XCTAssertEqual(ws.windowSessionId, "orphan",
                       "Unmatched first-launch adoption must rotate to the orphan id.")
        XCTAssertTrue(WindowSession._testing_isClaimed("orphan"))
        XCTAssertNotNil(tabs.tab(for: "t-orphan"))
    }

    func test_restore_unmatched_secondAppStateStaysFresh() {
        // First WindowSession adopts the only non-empty saved slot.
        // Second WindowSession in the same process sees no matched id
        // and no unclaimed non-empty slot, so it stays fresh — its
        // tree shows only the seed Terminals project.
        let claudeTab = makePersistedClaudeTab(id: "t-only", sessionId: "sid-only")
        let onlyWindow = makePersistedWindow(
            id: "shared-orphan",
            projects: [
                makeEmptyTerminalsProject(),
                makePersistedProject(id: "proj-only", tabs: [claudeTab]),
            ]
        )
        fake.state = PersistedState(
            version: PersistedState.currentVersion, windows: [onlyWindow]
        )

        // Window 1 — adopts the orphan.
        let ws1 = makeWindowSession(windowSessionId: "win-A")
        ws1.restoreSavedWindow()
        XCTAssertEqual(ws1.windowSessionId, "shared-orphan")

        // Window 2 — separate models, separate id. With the orphan
        // already claimed, ws2 finds no eligible adoption candidate
        // and stays fresh.
        let tabs2 = TabModel(initialMainCwd: "/tmp/nice-restore-tests")
        let sessions2 = SessionsModel(tabs: tabs2)
        let sidebar2 = SidebarModel(initialCollapsed: false, initialMode: .tabs)
        let ws2 = WindowSession(
            tabs: tabs2, sessions: sessions2, sidebar: sidebar2,
            windowSessionId: "win-B",
            persistenceEnabled: true,
            store: fake
        )
        ws2.restoreSavedWindow()

        XCTAssertEqual(ws2.windowSessionId, "win-B",
                       "Second window must not adopt an already-claimed slot.")
        XCTAssertTrue(WindowSession._testing_isClaimed("win-B"))
        XCTAssertEqual(tabs2.projects.map(\.id), [TabModel.terminalsProjectId],
                       "Stay-fresh window must keep the seed-only tree shape.")

        // Clean up the second window's pty subsystem; setUp's tearDown
        // only handles `sessions`, not `sessions2`.
        sessions2.tearDown()
    }

    // MARK: - Empty state

    func test_restore_emptyState_isNoOp() {
        // No saved state at all: restoreSavedWindow walks the early
        // exit (adopted == nil). The seed Terminals project from
        // TabModel(initialMainCwd:) is preserved unchanged, and the
        // window claims its minted id without rotating.
        fake.state = .empty

        let ws = makeWindowSession(windowSessionId: "win-empty")
        let beforeMainTabId = tabs.projects.first?.tabs.first?.id
        ws.restoreSavedWindow()

        XCTAssertEqual(ws.windowSessionId, "win-empty",
                       "Empty state must not rotate the minted window id.")
        XCTAssertTrue(WindowSession._testing_isClaimed("win-empty"),
                      "Even with no adoption, defer must still claim the window's own id.")
        XCTAssertEqual(tabs.projects.map(\.id), [TabModel.terminalsProjectId],
                       "Seed tree must survive restore unchanged.")
        XCTAssertEqual(tabs.projects.first?.tabs.first?.id, beforeMainTabId,
                       "Seed Main tab id must be preserved — restore is a no-op when state is empty.")
        XCTAssertTrue(fake.upsertCalls.isEmpty,
                      "scheduleSessionSave is gated by isInitializing; restore should not upsert mid-init.")
    }

    func test_restore_prunesEmptyGhosts() {
        // When the window is adopted, restoreSavedWindow garbage-
        // collects empty ghost entries from prior failed restores.
        // Pin that down: the prune call lands on the fake with the
        // adopted id as the keep target.
        let claudeTab = makePersistedClaudeTab(id: "t-g", sessionId: "sid-g")
        let ghostA = makePersistedWindow(id: "ghost-a", projects: [])
        let ghostB = makePersistedWindow(id: "ghost-b", projects: [])
        let live = makePersistedWindow(
            id: "live",
            projects: [
                makeEmptyTerminalsProject(),
                makePersistedProject(id: "proj-live", tabs: [claudeTab]),
            ]
        )
        fake.state = PersistedState(
            version: PersistedState.currentVersion,
            windows: [ghostA, live, ghostB]
        )

        let ws = makeWindowSession(windowSessionId: "win-anything")
        ws.restoreSavedWindow()

        XCTAssertEqual(fake.pruneKeepingCalls, ["live"],
                       "Prune must run exactly once with the adopted id as the keep target.")
        // The fake's pruneEmptyWindows mirrors production semantics —
        // ghost entries with totalTabCount == 0 are dropped.
        XCTAssertEqual(fake.state.windows.map(\.id), ["live"],
                       "Empty ghost windows must be pruned from the persisted state.")
    }

    // MARK: - /branch lineage round-trip

    func test_restore_preserves_branchLineage_parentTabIds() {
        // A /branch lineage on disk: root tab plus two depth-1
        // children pointing at the root. restoreSavedWindow must
        // hydrate the parentTabId pointers verbatim so the sidebar
        // re-renders the indent, and so a future /branch on the
        // restored child correctly sees its existing root and adds
        // another sibling rather than a new lineage.
        let root = makePersistedClaudeTab(id: "tRoot", sessionId: "S0")
        let parent2 = makePersistedClaudeTab(
            id: "tP2", sessionId: "S1", parentTabId: "tRoot"
        )
        let originating = makePersistedClaudeTab(
            id: "tCurrent", sessionId: "S2", parentTabId: "tRoot"
        )
        let project = makePersistedProject(
            id: "p", tabs: [root, parent2, originating]
        )
        let window = makePersistedWindow(
            id: "win-1", projects: [makeEmptyTerminalsProject(), project]
        )
        fake.upsert(window: window)

        let ws = makeWindowSession(windowSessionId: "win-1")
        ws.restoreSavedWindow()

        XCTAssertNil(
            tabs.tab(for: "tRoot")?.parentTabId,
            "root must hydrate without a parent"
        )
        XCTAssertEqual(
            tabs.tab(for: "tP2")?.parentTabId, "tRoot",
            "second-parent's lineage pointer must survive restore"
        )
        XCTAssertEqual(
            tabs.tab(for: "tCurrent")?.parentTabId, "tRoot",
            "originating tab's lineage pointer must survive restore"
        )
    }

    func test_restore_clearsDangling_parentTabId_references() {
        // Defensive: a hand-edited or partially corrupted sessions.json
        // can hold a child tab whose parentTabId points at a tab the
        // snapshot does not contain (the parent was removed by hand,
        // or the user's prior launch crashed mid-/branch after the
        // child was persisted but before the parent was). The renderer
        // tolerates the dangling pointer (still draws the indent), but
        // the depth-1 invariant survives only when stale references
        // get swept on the way in. Restore must clear them so the
        // child renders at root and a future /branch on it starts a
        // fresh lineage instead of inheriting the ghost.
        let orphaned = makePersistedClaudeTab(
            id: "tChild", sessionId: "S1", parentTabId: "tGhostParent"
        )
        let project = makePersistedProject(id: "p", tabs: [orphaned])
        let window = makePersistedWindow(
            id: "win-1", projects: [makeEmptyTerminalsProject(), project]
        )
        fake.upsert(window: window)

        let ws = makeWindowSession(windowSessionId: "win-1")
        ws.restoreSavedWindow()

        XCTAssertNotNil(
            tabs.tab(for: "tChild"),
            "child tab itself must still be restored"
        )
        XCTAssertNil(
            tabs.tab(for: "tChild")?.parentTabId,
            "dangling parentTabId reference must be cleared on restore"
        )
        XCTAssertNil(
            tabs.tab(for: "tGhostParent"),
            "the ghost parent must not be conjured into existence"
        )
    }

    // MARK: - Heal-on-restore by transcript lookup
    //
    // Pre-fix builds — and any code path that wrote a stale `tab.cwd`
    // (notably bare `claude -w` with no name, where Claude
    // auto-generates the worktree directory and the args parser
    // can't predict it) — leave a Claude tab pointing at the project
    // root while the real transcript lives in a sibling
    // `~/.claude/projects/<encoded-worktree>` bucket. The heal scan
    // locates the transcript by session id, recovers the real cwd
    // from its content, and adopts it for both the deferred-shell
    // spawn and the persisted `tab.cwd` (via mutateTab + a deferred
    // scheduleSessionSave the post-init flush picks up).
    //
    // Tests sandbox `$HOME` via `TestHomeSandbox`, plant a
    // `~/.claude/projects/<bucket>/<sid>.jsonl` of the desired
    // shape, and pre-create the recovered cwd on disk where the
    // heal's existence-check requires it.

    func test_heal_bucketMatch_noChange() throws {
        // Steady state: the persisted `tab.cwd` is already correct.
        // The expected transcript path exists, so the heal scan
        // returns nil without ever enumerating sibling buckets.
        let sandbox = TestHomeSandbox()
        defer { sandbox.teardown() }

        let cwd = "/tmp/nice-heal-tests-match-\(UUID().uuidString)"
        try plantDirectory(at: cwd)
        try plantTranscript(
            sessionId: "sid-match", bucketCwd: cwd, withMessageCwd: cwd
        )

        let tab = makePersistedClaudeTab(
            id: "t-match", sessionId: "sid-match", cwd: cwd
        )
        fake.state = makeState(tab: tab)

        let ws = makeWindowSession(windowSessionId: "win-match")
        ws.restoreSavedWindow()

        XCTAssertEqual(
            tabs.tab(for: "t-match")?.cwd, cwd,
            "matching bucket must leave tab.cwd untouched"
        )
    }

    func test_heal_mismatchedBucket_recoversFromCwdField() throws {
        // The classic bug shape: persisted `tab.cwd` is the project
        // root, but Claude bucketed under the worktree path. The
        // first regular-message record in the transcript carries a
        // top-level `cwd` field pointing at the worktree — heal
        // adopts it.
        let sandbox = TestHomeSandbox()
        defer { sandbox.teardown() }

        let projectCwd = "/tmp/nice-heal-tests-mismatch-\(UUID().uuidString)"
        let worktreeCwd = "\(projectCwd)/.claude/worktrees/auto-name"
        try plantDirectory(at: projectCwd)
        try plantDirectory(at: worktreeCwd)
        try plantTranscript(
            sessionId: "sid-mismatch",
            bucketCwd: worktreeCwd,
            withMessageCwd: worktreeCwd
        )

        let tab = makePersistedClaudeTab(
            id: "t-mismatch", sessionId: "sid-mismatch", cwd: projectCwd
        )
        fake.state = makeState(tab: tab)

        let ws = makeWindowSession(windowSessionId: "win-mismatch")
        ws.restoreSavedWindow()

        XCTAssertEqual(
            tabs.tab(for: "t-mismatch")?.cwd, worktreeCwd,
            "heal must overwrite tab.cwd with the worktree path it recovered from the transcript"
        )
        let claudePane = tabs.tab(for: "t-mismatch")?
            .panes.first(where: { $0.kind == .claude })
        XCTAssertEqual(
            claudePane?.cwd, worktreeCwd,
            "Claude pane.cwd (nil from PersistedPane) must follow the corrected tab.cwd"
        )
    }

    func test_heal_mismatchedBucket_recoversFromWorktreeStateRecord() throws {
        // First few messages don't carry a top-level `cwd` — only a
        // `worktree-state` record describing the session's
        // `worktreePath`. The fallback in `readCwdFromTranscript`
        // must pick that up.
        let sandbox = TestHomeSandbox()
        defer { sandbox.teardown() }

        let projectCwd = "/tmp/nice-heal-tests-wts-\(UUID().uuidString)"
        let worktreeCwd = "\(projectCwd)/.claude/worktrees/auto-name"
        try plantDirectory(at: projectCwd)
        try plantDirectory(at: worktreeCwd)
        try plantTranscript(
            sessionId: "sid-wts",
            bucketCwd: worktreeCwd,
            // Transcript head: permission-mode + worktree-state +
            // file-history-snapshot, none with a top-level cwd.
            // worktreePath is the only signal.
            lines: [
                #"{"type":"permission-mode","permissionMode":"auto","sessionId":"sid-wts"}"#,
                #"""
                {"type":"worktree-state","worktreeSession":{"originalCwd":"\#(projectCwd)","worktreePath":"\#(worktreeCwd)","worktreeName":"auto-name","sessionId":"sid-wts"},"sessionId":"sid-wts"}
                """#,
                #"{"type":"file-history-snapshot","isSnapshotUpdate":false}"#,
            ]
        )

        let tab = makePersistedClaudeTab(
            id: "t-wts", sessionId: "sid-wts", cwd: projectCwd
        )
        fake.state = makeState(tab: tab)

        let ws = makeWindowSession(windowSessionId: "win-wts")
        ws.restoreSavedWindow()

        XCTAssertEqual(
            tabs.tab(for: "t-wts")?.cwd, worktreeCwd,
            "worktree-state record's worktreePath must serve as the heal fallback when no per-message cwd is present yet"
        )
    }

    func test_heal_noMatchingBucket_fallsBack() throws {
        // No transcript anywhere under `~/.claude/projects/` carries
        // the session id. Heal returns nil; tab.cwd stays as
        // persisted; the existing `resolvedSpawnCwd` fallback path
        // remains in effect.
        let sandbox = TestHomeSandbox()
        defer { sandbox.teardown() }

        let cwd = "/tmp/nice-heal-tests-orphan-\(UUID().uuidString)"
        try plantDirectory(at: cwd)
        // Deliberately don't plant a transcript file. Even the
        // expected bucket directory is missing.

        let tab = makePersistedClaudeTab(
            id: "t-orphan", sessionId: "sid-orphan", cwd: cwd
        )
        fake.state = makeState(tab: tab)

        let ws = makeWindowSession(windowSessionId: "win-orphan")
        ws.restoreSavedWindow()

        XCTAssertEqual(
            tabs.tab(for: "t-orphan")?.cwd, cwd,
            "no recoverable bucket must leave tab.cwd untouched"
        )
    }

    func test_heal_multipleMatches_picksMostRecent() throws {
        // Defensive tie-break: if two buckets somehow carry the same
        // session id (UUIDs make this nominally impossible, but
        // hand-edited or test-corrupted state can produce it), the
        // heal picks the most-recently-modified file.
        let sandbox = TestHomeSandbox()
        defer { sandbox.teardown() }

        let projectCwd = "/tmp/nice-heal-tests-multi-\(UUID().uuidString)"
        let staleCwd = "\(projectCwd)/.claude/worktrees/stale"
        let freshCwd = "\(projectCwd)/.claude/worktrees/fresh"
        try plantDirectory(at: projectCwd)
        try plantDirectory(at: staleCwd)
        try plantDirectory(at: freshCwd)

        try plantTranscript(
            sessionId: "sid-multi",
            bucketCwd: staleCwd,
            withMessageCwd: staleCwd
        )
        // Force-stale the first file's mtime so the second is
        // unambiguously newer when the heal picks.
        let staleTranscript = transcriptPath(
            sessionId: "sid-multi", bucketCwd: staleCwd
        )
        let oldDate = Date(timeIntervalSince1970: 1_000_000)
        try FileManager.default.setAttributes(
            [.modificationDate: oldDate], ofItemAtPath: staleTranscript
        )

        try plantTranscript(
            sessionId: "sid-multi",
            bucketCwd: freshCwd,
            withMessageCwd: freshCwd
        )

        let tab = makePersistedClaudeTab(
            id: "t-multi", sessionId: "sid-multi", cwd: projectCwd
        )
        fake.state = makeState(tab: tab)

        let ws = makeWindowSession(windowSessionId: "win-multi")
        ws.restoreSavedWindow()

        XCTAssertEqual(
            tabs.tab(for: "t-multi")?.cwd, freshCwd,
            "tie-break must pick the most-recently-modified transcript"
        )
    }

    func test_heal_unreadableTranscript_fallsBack() throws {
        // First 30 lines are non-JSON garbage: every parse attempt
        // fails, neither `cwd` nor `worktreePath` is recovered, and
        // the heal returns nil. tab.cwd stays as persisted.
        let sandbox = TestHomeSandbox()
        defer { sandbox.teardown() }

        let projectCwd = "/tmp/nice-heal-tests-garbage-\(UUID().uuidString)"
        let worktreeCwd = "\(projectCwd)/.claude/worktrees/garbage"
        try plantDirectory(at: projectCwd)
        try plantDirectory(at: worktreeCwd)
        try plantTranscript(
            sessionId: "sid-garbage",
            bucketCwd: worktreeCwd,
            // 30+ lines of non-JSON so the parser exhausts its
            // head-scan budget without finding a cwd anywhere.
            lines: Array(repeating: "not json at all", count: 35)
        )

        let tab = makePersistedClaudeTab(
            id: "t-garbage", sessionId: "sid-garbage", cwd: projectCwd
        )
        fake.state = makeState(tab: tab)

        let ws = makeWindowSession(windowSessionId: "win-garbage")
        ws.restoreSavedWindow()

        XCTAssertEqual(
            tabs.tab(for: "t-garbage")?.cwd, projectCwd,
            "unparseable transcript must surface as no heal — tab.cwd unchanged"
        )
    }

    func test_heal_recoveredCwdMissingOnDisk_skipsHeal() throws {
        // The transcript references a worktree that has since been
        // deleted from disk. The heal abandons the rewrite — there's
        // no point pointing `tab.cwd` at a phantom path; the resume
        // is unrecoverable either way, and `resolvedSpawnCwd`'s
        // existing fallback drops the user back into the project
        // root via the same code path it always has.
        let sandbox = TestHomeSandbox()
        defer { sandbox.teardown() }

        let projectCwd = "/tmp/nice-heal-tests-missing-\(UUID().uuidString)"
        let deletedWorktreeCwd = "\(projectCwd)/.claude/worktrees/deleted"
        try plantDirectory(at: projectCwd)
        // Deliberately don't plant the worktree directory itself.
        try plantTranscript(
            sessionId: "sid-missing",
            bucketCwd: deletedWorktreeCwd,
            withMessageCwd: deletedWorktreeCwd
        )

        let tab = makePersistedClaudeTab(
            id: "t-missing", sessionId: "sid-missing", cwd: projectCwd
        )
        fake.state = makeState(tab: tab)

        let ws = makeWindowSession(windowSessionId: "win-missing")
        ws.restoreSavedWindow()

        XCTAssertEqual(
            tabs.tab(for: "t-missing")?.cwd, projectCwd,
            "phantom recovered path must NOT overwrite tab.cwd — leave the existing fallback to handle it"
        )
    }

    func test_heal_terminalOnlyTab_skipsScan() throws {
        // Terminal-only tabs (no claudeSessionId) carry no session id
        // to look up. The early-out check skips the projects-dir
        // enumeration entirely. Verifying via the public behavior:
        // restoring such a tab leaves it untouched even when a
        // transcript on disk happens to share the same path.
        let sandbox = TestHomeSandbox()
        defer { sandbox.teardown() }

        let cwd = "/tmp/nice-heal-tests-terminal-\(UUID().uuidString)"
        try plantDirectory(at: cwd)
        // Plant something the heal *could* pick up if it ran — but
        // it shouldn't, because the tab has no session id.
        try plantTranscript(
            sessionId: "should-not-be-looked-up",
            bucketCwd: cwd,
            withMessageCwd: "/elsewhere"
        )

        let terminalTab = PersistedTab(
            id: "t-terminal",
            title: "Terminal tab",
            cwd: cwd,
            branch: nil,
            claudeSessionId: nil,   // <-- no Claude session
            activePaneId: "t-terminal-t1",
            panes: [
                PersistedPane(
                    id: "t-terminal-t1", title: "Terminal 1", kind: .terminal
                ),
            ],
            titleManuallySet: nil
        )
        fake.state = makeState(tab: terminalTab)

        let ws = makeWindowSession(windowSessionId: "win-terminal")
        ws.restoreSavedWindow()

        XCTAssertEqual(
            tabs.tab(for: "t-terminal")?.cwd, cwd,
            "terminal-only tab must not consult the heal path at all"
        )
    }

    func test_heal_snapshotRoundTrip_locksCorrectedCwd() throws {
        // After heal mutates `tab.cwd` in-place, `snapshotPersistedWindow()`
        // must serialize the corrected value — that's what the
        // post-init save flush picks up.
        let sandbox = TestHomeSandbox()
        defer { sandbox.teardown() }

        let projectCwd = "/tmp/nice-heal-tests-roundtrip-\(UUID().uuidString)"
        let worktreeCwd = "\(projectCwd)/.claude/worktrees/auto-name"
        try plantDirectory(at: projectCwd)
        try plantDirectory(at: worktreeCwd)
        try plantTranscript(
            sessionId: "sid-roundtrip",
            bucketCwd: worktreeCwd,
            withMessageCwd: worktreeCwd
        )

        let tab = makePersistedClaudeTab(
            id: "t-roundtrip", sessionId: "sid-roundtrip", cwd: projectCwd
        )
        fake.state = makeState(tab: tab)

        let ws = makeWindowSession(windowSessionId: "win-roundtrip")
        ws.restoreSavedWindow()

        let snapshot = ws.snapshotPersistedWindow()
        let snapshotTab = snapshot.projects
            .flatMap(\.tabs)
            .first(where: { $0.id == "t-roundtrip" })
        XCTAssertEqual(
            snapshotTab?.cwd, worktreeCwd,
            "snapshot must serialize the post-heal cwd so the next save persists the correction"
        )
    }

    // MARK: - Heal test helpers

    /// Mirror of Claude's bucket-encoding convention (every `/` and
    /// `.` becomes `-`). Production lives in
    /// `WindowSession.encodeClaudeBucket`; duplicated here so the test
    /// is independent of the SUT's implementation.
    private func encodeBucket(_ path: String) -> String {
        var out = ""
        out.reserveCapacity(path.count)
        for ch in path {
            out.append(ch == "/" || ch == "." ? "-" : ch)
        }
        return out
    }

    private func transcriptPath(sessionId: String, bucketCwd: String) -> String {
        let projectsRoot = NSHomeDirectory() + "/.claude/projects"
        return "\(projectsRoot)/\(encodeBucket(bucketCwd))/\(sessionId).jsonl"
    }

    /// Create an empty directory at `path`, making intermediate
    /// directories. Used to plant both the worktree (so heal's
    /// existence check passes) and the project root (so the
    /// `resolvedSpawnCwd` fallback wouldn't accidentally pass before
    /// heal even ran).
    private func plantDirectory(at path: String) throws {
        try FileManager.default.createDirectory(
            atPath: path, withIntermediateDirectories: true
        )
    }

    /// Plant a Claude transcript at the bucket implied by
    /// `bucketCwd`. The default content is a single message-shape
    /// line carrying `withMessageCwd` as its `cwd` field —
    /// `readCwdFromTranscript`'s per-message branch. Pass `lines`
    /// for custom transcript bodies (worktree-state-only, garbage,
    /// etc.).
    private func plantTranscript(
        sessionId: String,
        bucketCwd: String,
        withMessageCwd: String? = nil,
        lines: [String]? = nil
    ) throws {
        let path = transcriptPath(sessionId: sessionId, bucketCwd: bucketCwd)
        let bucketDir = (path as NSString).deletingLastPathComponent
        try FileManager.default.createDirectory(
            atPath: bucketDir, withIntermediateDirectories: true
        )
        let body: String
        if let lines {
            body = lines.joined(separator: "\n") + "\n"
        } else if let withMessageCwd {
            body = #"""
            {"type":"user","cwd":"\#(withMessageCwd)","sessionId":"\#(sessionId)"}
            """# + "\n"
        } else {
            body = ""
        }
        try body.write(toFile: path, atomically: true, encoding: .utf8)
    }

    /// Wrap a single PersistedTab in a fresh PersistedState so the
    /// fake store has something to hand back from `load()`. The
    /// persisted window id is generic — `restoreSavedWindow`'s
    /// unmatched-adoption path picks it up because
    /// `claimedWindowIds` is reset in `setUp`. Any non-matching
    /// `makeWindowSession` id will route through the unmatched
    /// fallback and adopt it.
    private func makeState(tab: PersistedTab) -> PersistedState {
        let project = makePersistedProject(id: "proj-heal", tabs: [tab])
        let window = makePersistedWindow(
            id: "heal-test-window",
            projects: [makeEmptyTerminalsProject(), project]
        )
        return PersistedState(
            version: PersistedState.currentVersion, windows: [window]
        )
    }

    // MARK: - Helpers

    private func makeWindowSession(windowSessionId: String) -> WindowSession {
        WindowSession(
            tabs: tabs,
            sessions: sessions,
            sidebar: sidebar,
            windowSessionId: windowSessionId,
            persistenceEnabled: true,
            store: fake
        )
    }

    /// Empty Terminals project — restoreSavedWindow's `defer
    /// ensureTerminalsProjectSeededAndSpawn()` returns `.existed` for
    /// it, so no real pty spawn is triggered during the test.
    private func makeEmptyTerminalsProject() -> PersistedProject {
        PersistedProject(
            id: TabModel.terminalsProjectId,
            name: "Terminals",
            path: "/tmp/nice-restore-tests",
            tabs: []
        )
    }

    private func makePersistedClaudeTab(
        id: String,
        sessionId: String,
        parentTabId: String? = nil,
        cwd: String = "/tmp/nice-restore-tests"
    ) -> PersistedTab {
        let claudePaneId = "\(id)-claude"
        return PersistedTab(
            id: id,
            title: "Claude tab",
            cwd: cwd,
            branch: nil,
            claudeSessionId: sessionId,
            activePaneId: claudePaneId,
            panes: [
                PersistedPane(id: claudePaneId, title: "Claude", kind: .claude),
            ],
            titleManuallySet: nil,
            parentTabId: parentTabId
        )
    }

    private func makePersistedProject(
        id: String, tabs: [PersistedTab]
    ) -> PersistedProject {
        PersistedProject(
            id: id, name: id.uppercased(),
            path: "/tmp/nice-restore-tests/\(id)",
            tabs: tabs
        )
    }

    private func makePersistedWindow(
        id: String,
        activeTabId: String? = nil,
        projects: [PersistedProject]
    ) -> PersistedWindow {
        PersistedWindow(
            id: id,
            activeTabId: activeTabId,
            sidebarCollapsed: false,
            projects: projects
        )
    }
}
