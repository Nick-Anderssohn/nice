//
//  WindowSessionTearDownTests.swift
//  NiceUnitTests
//
//  Coverage for `WindowSession.tearDown(reason:)`. The two reasons
//  diverge on disk — `.appTerminating` upserts the latest snapshot
//  so relaunch reopens the window, while `.userClosedWindow`
//  removes the entry so a window the user explicitly closed is
//  gone for good. Persistence-disabled callers (preview/test mode)
//  go silent on either reason. Claim release on the shared
//  `WindowClaimLedger` is unconditional so a future window can
//  reuse the slot regardless of close path.
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
    private var ledger: WindowClaimLedger!

    override func setUp() {
        super.setUp()
        fake = FakeSessionStore()
        tabs = TabModel(initialMainCwd: "/tmp/nice-teardown-tests")
        sessions = SessionsModel(tabs: tabs)
        sidebar = SidebarModel(initialCollapsed: false, initialMode: .tabs)
        ledger = WindowClaimLedger()
    }

    override func tearDown() {
        sessions?.tearDown()
        sessions = nil
        tabs = nil
        sidebar = nil
        fake = nil
        ledger = nil
        super.tearDown()
    }

    // MARK: - Claim release (both reasons)

    func test_tearDown_appTerminating_releasesClaimedWindowId() {
        let ws = makeWindowSession(persistenceEnabled: false, id: "win-release-app")
        // Simulate a successful prior restore by claiming the id.
        ws.restoreSavedWindow()
        XCTAssertTrue(ledger.contains("win-release-app"),
                      "Pre-condition: restore must have claimed the id.")

        ws.tearDown(reason: .appTerminating)

        XCTAssertFalse(ledger.contains("win-release-app"),
                       "tearDown must release the id so a future window can adopt the slot.")
    }

    func test_tearDown_userClosedWindow_releasesClaimedWindowId() {
        let ws = makeWindowSession(persistenceEnabled: false, id: "win-release-user")
        ws.restoreSavedWindow()
        XCTAssertTrue(ledger.contains("win-release-user"))

        ws.tearDown(reason: .userClosedWindow)

        XCTAssertFalse(ledger.contains("win-release-user"),
                       "Claim release must run unconditionally — applies to both reasons.")
    }

    // MARK: - .appTerminating: upsert + flush, preserve for relaunch

    func test_tearDown_appTerminating_writesAndFlushes() {
        let ws = makeWindowSession(persistenceEnabled: true, id: "win-persist")
        ws.markInitializationComplete()
        let upsertsBefore = fake.upsertCalls.count

        ws.tearDown(reason: .appTerminating)

        XCTAssertEqual(fake.upsertCalls.count, upsertsBefore + 1,
                       "appTerminating tearDown must write the final snapshot.")
        XCTAssertEqual(fake.upsertCalls.last?.id, "win-persist",
                       "Final upsert must target this window's id.")
        XCTAssertTrue(fake.removeCalls.isEmpty,
                      "appTerminating must NOT call remove — that's the user-close path.")
        XCTAssertEqual(fake.flushCount, 1,
                       "appTerminating tearDown must flush so willTerminate doesn't lose the last write.")
    }

    // MARK: - .userClosedWindow: remove + flush, gone for good

    func test_tearDown_userClosedWindow_removesAndFlushes() {
        let ws = makeWindowSession(persistenceEnabled: true, id: "win-removed")
        ws.markInitializationComplete()

        ws.tearDown(reason: .userClosedWindow)

        XCTAssertEqual(fake.removeCalls, ["win-removed"],
                       "userClosedWindow must drop the entry from the store by id.")
        XCTAssertTrue(fake.upsertCalls.isEmpty,
                      "userClosedWindow must NOT upsert — would resurrect the window on next launch.")
        XCTAssertEqual(fake.flushCount, 1,
                       "userClosedWindow must flush so the removal is on disk before the user can quit.")
    }

    func test_tearDown_userClosedWindow_dropsEntryFromState() {
        // End-to-end through the fake: pre-seed the store with the
        // window's saved entry, tear down with userClosedWindow, and
        // assert the entry is no longer in the store's state. This
        // is the contract the bug fix actually delivers — a relaunch
        // load() after this would see no trace of the window.
        let ws = makeWindowSession(persistenceEnabled: true, id: "win-doomed")
        let snapshot = PersistedWindow(
            id: "win-doomed",
            activeTabId: nil,
            sidebarCollapsed: false,
            projects: [
                PersistedProject(
                    id: TabModel.terminalsProjectId,
                    name: "Terminals",
                    path: "/tmp/nice-teardown-tests",
                    tabs: []
                ),
            ]
        )
        let neighbor = PersistedWindow(
            id: "win-survivor",
            activeTabId: nil,
            sidebarCollapsed: false,
            projects: snapshot.projects
        )
        fake.state = PersistedState(
            version: PersistedState.currentVersion,
            windows: [snapshot, neighbor]
        )
        ws.markInitializationComplete()

        ws.tearDown(reason: .userClosedWindow)

        XCTAssertEqual(fake.state.windows.map(\.id), ["win-survivor"],
                       "Doomed entry must be gone; sibling windows untouched.")
    }

    // MARK: - Persistence disabled: silent on both reasons

    func test_tearDown_persistenceDisabled_doesNothing_appTerminating() {
        let ws = makeWindowSession(persistenceEnabled: false, id: "win-no-persist-app")
        ws.restoreSavedWindow()

        ws.tearDown(reason: .appTerminating)

        XCTAssertTrue(fake.upsertCalls.isEmpty)
        XCTAssertTrue(fake.removeCalls.isEmpty)
        XCTAssertEqual(fake.flushCount, 0)
        XCTAssertFalse(ledger.contains("win-no-persist-app"))
    }

    func test_tearDown_persistenceDisabled_doesNothing_userClosedWindow() {
        let ws = makeWindowSession(persistenceEnabled: false, id: "win-no-persist-user")
        ws.restoreSavedWindow()

        ws.tearDown(reason: .userClosedWindow)

        XCTAssertTrue(fake.upsertCalls.isEmpty)
        XCTAssertTrue(fake.removeCalls.isEmpty,
                      "persistenceEnabled == false must skip remove too — the test/preview path is silent.")
        XCTAssertEqual(fake.flushCount, 0)
        XCTAssertFalse(ledger.contains("win-no-persist-user"))
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
        // Use .appTerminating so the slot survives on disk and the
        // second window can re-adopt it. (.userClosedWindow would
        // delete the slot — that's what its dedicated test pins.)
        ws1.tearDown(reason: .appTerminating)
        XCTAssertFalse(ledger.contains("slot"),
                       "tearDown must release the slot regardless of reason.")

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
            store: fake,
            claimLedger: ledger
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
            store: fake,
            claimLedger: ledger
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
