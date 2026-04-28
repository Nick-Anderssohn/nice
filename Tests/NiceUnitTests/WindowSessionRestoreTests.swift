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

    private func makePersistedClaudeTab(id: String, sessionId: String) -> PersistedTab {
        let claudePaneId = "\(id)-claude"
        return PersistedTab(
            id: id,
            title: "Claude tab",
            cwd: "/tmp/nice-restore-tests",
            branch: nil,
            claudeSessionId: sessionId,
            activePaneId: claudePaneId,
            panes: [
                PersistedPane(id: claudePaneId, title: "Claude", kind: .claude),
            ]
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
