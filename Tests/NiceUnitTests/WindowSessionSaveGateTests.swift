//
//  WindowSessionSaveGateTests.swift
//  NiceUnitTests
//
//  Pins down `WindowSession.scheduleSessionSave`'s save-gate. The
//  gate has two independent reasons to short-circuit: persistence is
//  disabled (preview / `services == nil`), or AppState's `init` is
//  still running (`isInitializing == true`). Either reason silences
//  the upsert; only when both clear does a save reach the store.
//
//  Without this coverage, a regression that flipped the gate from
//  AND to OR would still pass the rest of the suite and silently
//  start writing ghost-empty windows during init.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class WindowSessionSaveGateTests: XCTestCase {

    private var fake: FakeSessionStore!
    private var tabs: TabModel!
    private var sessions: SessionsModel!
    private var sidebar: SidebarModel!

    override func setUp() {
        super.setUp()
        WindowSession._testing_resetClaimedWindowIds()
        fake = FakeSessionStore()
        tabs = TabModel(initialMainCwd: "/tmp/nice-save-gate-tests")
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

    func test_scheduleSessionSave_blockedDuringInit() {
        // Pre-`markInitializationComplete()`: a save attempt must be
        // a no-op. AppState relies on this to absorb didSet-driven
        // saves during seed assignment.
        let ws = makeWindowSession(persistenceEnabled: true)
        ws.scheduleSessionSave()
        XCTAssertTrue(fake.upsertCalls.isEmpty,
                      "scheduleSessionSave must short-circuit while isInitializing == true.")
    }

    func test_scheduleSessionSave_blockedWhenPersistenceDisabled() {
        // persistenceEnabled == false (test/preview path). Even after
        // `markInitializationComplete()` releases the init gate, no
        // save reaches the store.
        let ws = makeWindowSession(persistenceEnabled: false)
        ws.markInitializationComplete()
        ws.scheduleSessionSave()
        XCTAssertTrue(fake.upsertCalls.isEmpty,
                      "persistenceEnabled == false must always silence the upsert path.")
    }

    func test_scheduleSessionSave_releasedAfterMarkInitializationComplete() {
        // Both gates clear: a save lands on the store.
        let ws = makeWindowSession(persistenceEnabled: true)
        ws.markInitializationComplete()
        ws.scheduleSessionSave()
        XCTAssertEqual(fake.upsertCalls.count, 1,
                       "Save-gate release must allow scheduleSessionSave to reach the store.")
        XCTAssertEqual(fake.upsertCalls.first?.id, ws.windowSessionId,
                       "Upsert payload must target this window's id.")
    }

    func test_scheduleSessionSave_canCoalesce_acrossMutations() {
        // After release, multiple mutations each trigger an upsert
        // — the real `SessionStore` debounces internally on a 500 ms
        // timer; the fake captures every call so a regression that
        // accidentally throttled at the WindowSession layer would
        // surface here.
        let ws = makeWindowSession(persistenceEnabled: true)
        ws.markInitializationComplete()
        ws.scheduleSessionSave()
        ws.scheduleSessionSave()
        ws.scheduleSessionSave()
        XCTAssertEqual(fake.upsertCalls.count, 3,
                       "WindowSession must not coalesce — that's the SessionStore's job, gated on the same window id.")
    }

    private func makeWindowSession(persistenceEnabled: Bool) -> WindowSession {
        WindowSession(
            tabs: tabs,
            sessions: sessions,
            sidebar: sidebar,
            windowSessionId: "win-save-gate",
            persistenceEnabled: persistenceEnabled,
            store: fake
        )
    }
}
