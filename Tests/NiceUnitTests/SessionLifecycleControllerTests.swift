//
//  SessionLifecycleControllerTests.swift
//  NiceUnitTests
//
//  Pin down the controller's reason-routing + ordering on a single
//  object — no `WindowRegistry`, no `NiceServices`. The Phase 1
//  `WindowRegistryTerminationOrderingTests` covers the full
//  notification-center round-trip; this suite covers the controller's
//  contract in isolation so future refactors can assert it directly.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class SessionLifecycleControllerTests: XCTestCase {

    // Each AppState constructed below gets its own fresh
    // `WindowClaimLedger` by default — no process-wide reset needed.

    // MARK: - handleAppWillTerminate

    /// Ordering invariant: `detachObservers` must run BEFORE the
    /// per-state tearDown loop. Reversing it lets a willClose burst
    /// land on an AppState that we're already tearing down with
    /// `.appTerminating`, which would route it through
    /// `.userClosedWindow` and remove the snapshot we just upserted.
    func test_handleAppWillTerminate_detachesObserversBeforeTearDownLoop() {
        let controller = SessionLifecycleController()
        let fake = FakeSessionStore()
        let appA = AppState(
            services: nil, initialSidebarCollapsed: false,
            initialMainCwd: nil, windowSessionId: "win-A", store: fake
        )
        let appB = AppState(
            services: nil, initialSidebarCollapsed: false,
            initialMainCwd: nil, windowSessionId: "win-B", store: fake
        )
        appA.windowSession.markInitializationComplete()
        appB.windowSession.markInitializationComplete()

        var events: [String] = []
        controller.handleAppWillTerminate(
            allAppStates: [appA, appB],
            detachObservers: { events.append("detach") }
        )
        // After-the-fact: upserts arrived in order during the loop.
        // The detach must have been recorded before any of them.
        for call in fake.upsertCalls { events.append("upsert:\(call.id)") }

        XCTAssertEqual(
            events.first, "detach",
            "detachObservers must run before the per-state tearDown loop — see SessionLifecycleController docstring."
        )
        XCTAssertEqual(
            events.dropFirst().sorted(),
            ["upsert:win-A", "upsert:win-B"],
            "Both AppStates must take the .appTerminating tearDown path (upsert + flush)."
        )
        XCTAssertEqual(fake.flushCount, 2,
                       "Each .appTerminating tearDown flushes once.")
    }

    /// Tearing down with `.appTerminating` upserts the latest
    /// snapshot for every window — that's what makes ⌘Q reopen
    /// them on relaunch.
    func test_handleAppWillTerminate_upsertsEveryWindowSnapshot() {
        let controller = SessionLifecycleController()
        let fake = FakeSessionStore()
        let states = ["w1", "w2", "w3"].map {
            AppState(
                services: nil, initialSidebarCollapsed: false,
                initialMainCwd: nil, windowSessionId: $0, store: fake
            )
        }
        for s in states { s.windowSession.markInitializationComplete() }

        controller.handleAppWillTerminate(
            allAppStates: states,
            detachObservers: { /* noop */ }
        )

        XCTAssertEqual(
            fake.state.windows.map(\.id).sorted(),
            ["w1", "w2", "w3"],
            "Every window's snapshot must reach the store via .appTerminating tearDown."
        )
        XCTAssertEqual(fake.removeCalls, [],
                       "appTerminating must never call remove — that would wipe the windows we want to reopen.")
    }

    /// Empty registry — nothing to tear down, detach still runs.
    func test_handleAppWillTerminate_emptyAppStates_stillCallsDetach() {
        let controller = SessionLifecycleController()
        var detachCount = 0
        controller.handleAppWillTerminate(
            allAppStates: [],
            detachObservers: { detachCount += 1 }
        )
        XCTAssertEqual(detachCount, 1,
                       "detachObservers must always fire — leaving stale observers is a regression vector.")
    }

    // MARK: - handleWindowWillClose

    /// `userInitiatedClose == true` (the flag
    /// `CloseConfirmationDelegate` flips on confirm) must route to
    /// `.userClosedWindow` so the entry is dropped from
    /// `sessions.json`.
    func test_handleWindowWillClose_withUserIntent_routesToUserClosed() {
        let controller = SessionLifecycleController()
        let fake = FakeSessionStore()
        let app = AppState(
            services: nil, initialSidebarCollapsed: false,
            initialMainCwd: nil, windowSessionId: "win-closed", store: fake
        )
        app.windowSession.markInitializationComplete()
        fake.upsert(window: app.windowSession.snapshotPersistedWindow())
        app.userInitiatedClose = true

        controller.handleWindowWillClose(appState: app)

        XCTAssertEqual(fake.removeCalls, ["win-closed"],
                       "userInitiatedClose=true must route to .userClosedWindow → store.remove.")
        XCTAssertFalse(
            fake.state.windows.contains(where: { $0.id == "win-closed" }),
            "Snapshot must be gone after a user-initiated close."
        )
    }

    /// Default (flag never flipped) must route to `.appTerminating`
    /// — the safer failure mode. Even if some future close path
    /// forgets to set the flag, the snapshot survives instead of
    /// silently disappearing.
    func test_handleWindowWillClose_withoutUserIntent_routesToAppTerminating() {
        let controller = SessionLifecycleController()
        let fake = FakeSessionStore()
        let app = AppState(
            services: nil, initialSidebarCollapsed: false,
            initialMainCwd: nil, windowSessionId: "win-no-intent", store: fake
        )
        app.windowSession.markInitializationComplete()
        XCTAssertFalse(app.userInitiatedClose,
                       "Pre-condition: userInitiatedClose defaults to false.")

        controller.handleWindowWillClose(appState: app)

        XCTAssertEqual(fake.removeCalls, [],
                       "Without the intent flag, the controller must NOT call store.remove.")
        XCTAssertTrue(
            fake.state.windows.contains(where: { $0.id == "win-no-intent" }),
            "Snapshot must be present (upserted via .appTerminating) — safer failure mode."
        )
    }

    /// `nil` AppState is a registry race the controller must
    /// tolerate without crashing — the entry's weak ref can be
    /// gone by the time `handleClose` resolves the appState param.
    func test_handleWindowWillClose_nilAppState_isNoOp() {
        let controller = SessionLifecycleController()
        // Should not crash; should not throw.
        controller.handleWindowWillClose(appState: nil)
    }
}
