//
//  WindowSessionTearDownTests.swift
//  NiceUnitTests
//
//  Coverage for `WindowSession.tearDown` — the persistence-enabled
//  case writes-then-flushes, the persistence-disabled case is
//  silent, and *both* paths release this window's claim on the
//  process-wide `claimedWindowIds` set so a future window can adopt
//  the slot. The "second window can adopt the slot after first
//  closes" invariant is the one the original spec called out as
//  unverified — pinning it down here.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class WindowSessionTearDownTests: XCTestCase {

    private var fake: FakeSessionStore!
    private var tabs: TabModel!
    private var sessions: SessionsModel!
    private var sidebar: SidebarModel!

    override func setUp() {
        super.setUp()
        WindowSession._testing_resetClaimedWindowIds()
        fake = FakeSessionStore()
        tabs = TabModel(initialMainCwd: "/tmp/nice-teardown-tests")
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

    func test_tearDown_releasesClaimedWindowId() {
        let ws = makeWindowSession(persistenceEnabled: false, id: "win-release")
        // Simulate a successful prior restore by claiming the id.
        ws.restoreSavedWindow()
        XCTAssertTrue(WindowSession._testing_isClaimed("win-release"),
                      "Pre-condition: restore must have claimed the id.")

        ws.tearDown()

        XCTAssertFalse(WindowSession._testing_isClaimed("win-release"),
                       "tearDown must release the id so a future window can adopt the slot.")
    }

    func test_tearDown_persistenceEnabled_writesAndFlushes() {
        let ws = makeWindowSession(persistenceEnabled: true, id: "win-persist")
        ws.markInitializationComplete()
        let upsertsBefore = fake.upsertCalls.count

        ws.tearDown()

        XCTAssertEqual(fake.upsertCalls.count, upsertsBefore + 1,
                       "tearDown with persistence must write the final snapshot.")
        XCTAssertEqual(fake.upsertCalls.last?.id, "win-persist",
                       "Final upsert must target this window's id.")
        XCTAssertEqual(fake.flushCount, 1,
                       "Persistence-enabled tearDown must flush so willTerminate doesn't lose the last write.")
    }

    func test_tearDown_persistenceDisabled_doesNothing() {
        let ws = makeWindowSession(persistenceEnabled: false, id: "win-no-persist")
        // Claim the id directly via restoreSavedWindow so we can
        // assert that release still happens even with persistence off.
        ws.restoreSavedWindow()

        ws.tearDown()

        XCTAssertTrue(fake.upsertCalls.isEmpty,
                      "persistenceEnabled == false must skip the final upsert.")
        XCTAssertEqual(fake.flushCount, 0,
                       "persistenceEnabled == false must skip the flush.")
        XCTAssertFalse(WindowSession._testing_isClaimed("win-no-persist"),
                       "Claim release must run unconditionally — the slot must be reusable.")
    }

    func test_secondWindow_canAdoptSlotAfterFirstTearsDown() {
        // First window adopts the only saved slot, then tears down.
        // A subsequent window with no matched id must now be free to
        // adopt the (un-claimed) slot.
        let claudeTab = makePersistedClaudeTab(id: "t-survives", sessionId: "sid-survives")
        let saved = PersistedWindow(
            id: "slot",
            activeTabId: nil,
            sidebarCollapsed: false,
            projects: [
                PersistedProject(
                    id: TabModel.terminalsProjectId,
                    name: "Terminals",
                    path: "/tmp/nice-teardown-tests",
                    tabs: []
                ),
                PersistedProject(
                    id: "proj-survives",
                    name: "PROJ-SURVIVES",
                    path: "/tmp/nice-teardown-tests/survives",
                    tabs: [claudeTab]
                ),
            ]
        )
        fake.state = PersistedState(
            version: PersistedState.currentVersion, windows: [saved]
        )

        let ws1 = makeWindowSession(persistenceEnabled: true, id: "win-first")
        ws1.restoreSavedWindow()
        XCTAssertEqual(ws1.windowSessionId, "slot",
                       "First window must adopt the orphan slot.")
        ws1.tearDown()
        XCTAssertFalse(WindowSession._testing_isClaimed("slot"),
                       "tearDown must release the slot.")

        // Second window — fresh models, fresh id. Without the slot
        // release, this window would stay fresh; with it, the slot
        // is reusable.
        let tabs2 = TabModel(initialMainCwd: "/tmp/nice-teardown-tests")
        let sessions2 = SessionsModel(tabs: tabs2)
        let sidebar2 = SidebarModel(initialCollapsed: false, initialMode: .tabs)
        let ws2 = WindowSession(
            tabs: tabs2, sessions: sessions2, sidebar: sidebar2,
            windowSessionId: "win-second",
            persistenceEnabled: true,
            store: fake
        )
        ws2.restoreSavedWindow()

        XCTAssertEqual(ws2.windowSessionId, "slot",
                       "Second window must be able to adopt the released slot.")
        XCTAssertNotNil(tabs2.tab(for: "t-survives"),
                        "Restored tab must materialize in the second window's tree.")

        sessions2.tearDown()
    }

    // MARK: - Helpers

    private func makeWindowSession(persistenceEnabled: Bool, id: String) -> WindowSession {
        WindowSession(
            tabs: tabs,
            sessions: sessions,
            sidebar: sidebar,
            windowSessionId: id,
            persistenceEnabled: persistenceEnabled,
            store: fake
        )
    }

    private func makePersistedClaudeTab(id: String, sessionId: String) -> PersistedTab {
        let claudePaneId = "\(id)-claude"
        return PersistedTab(
            id: id,
            title: "Survives",
            cwd: "/tmp/nice-teardown-tests",
            branch: nil,
            claudeSessionId: sessionId,
            activePaneId: claudePaneId,
            panes: [
                PersistedPane(id: claudePaneId, title: "Claude", kind: .claude),
            ]
        )
    }
}
