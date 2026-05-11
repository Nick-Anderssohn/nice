//
//  WindowRegistryTerminationOrderingTests.swift
//  NiceUnitTests
//
//  Pins down the AppKit lifecycle interaction that broke prod once:
//  SwiftUI's `WindowGroup` posts `NSWindow.willCloseNotification`
//  for every still-live window during `app.terminate(_:)`. The
//  registry's `willCloseNotification` observer therefore must NOT
//  unconditionally route to `tearDown(.userClosedWindow)` —
//  otherwise the per-window `willClose` cascade fires AFTER the
//  `willTerminate` observer has already upserted each window's
//  snapshot, and the resulting `store.remove(...)` wipes those
//  same snapshots from disk. Next launch reads empty
//  `sessions.json` and the user's tabs are gone.
//
//  These tests fire BOTH notifications in sequence (mimicking the
//  real ⌘Q dance) and assert what the next launch's
//  `SessionStore.load()` would observe.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class WindowRegistryTerminationOrderingTests: XCTestCase {

    // Each AppState constructed below gets its own fresh
    // `WindowClaimLedger` by default — no process-wide reset needed.

    /// THE regression test. Drives the full ⌘Q sequence: post
    /// `willTerminate` (which is what `NiceServices` listens for),
    /// then post `willCloseNotification` for every registered
    /// window (which is what SwiftUI's `WindowGroup` does during
    /// scene teardown). Assert the store still has both windows'
    /// snapshots so a relaunch reopens them.
    func test_willTerminate_thenWillClose_preservesEverySnapshot() {
        let fake = FakeSessionStore()
        let registry = WindowRegistry()

        let appA = AppState(
            services: nil,
            initialSidebarCollapsed: false,
            initialMainCwd: nil,
            windowSessionId: "win-A",
            store: fake
        )
        appA.windowSession.markInitializationComplete()
        let appB = AppState(
            services: nil,
            initialSidebarCollapsed: false,
            initialMainCwd: nil,
            windowSessionId: "win-B",
            store: fake
        )
        appB.windowSession.markInitializationComplete()

        let windowA = makeWindow()
        let windowB = makeWindow()
        registry.register(appState: appA, window: windowA)
        registry.register(appState: appB, window: windowB)

        // Simulate NiceServices's willTerminate handler: detach
        // close observers first, then run .appTerminating tearDown
        // for every still-registered window.
        registry.detachAllCloseObservers()
        for state in registry.allAppStates {
            state.tearDown(reason: .appTerminating)
        }

        // Now AppKit/SwiftUI's per-window willClose cascade during
        // `app.terminate(_:)`. With the observers detached, this
        // must be a no-op — neither window's entry can be wiped.
        NotificationCenter.default.post(
            name: NSWindow.willCloseNotification, object: windowA
        )
        NotificationCenter.default.post(
            name: NSWindow.willCloseNotification, object: windowB
        )
        // The observers were registered with `queue: .main`. Spin
        // the runloop so any in-flight delivery completes before
        // we assert.
        let drained = XCTestExpectation(description: "main queue drained")
        DispatchQueue.main.async { drained.fulfill() }
        wait(for: [drained], timeout: 1.0)

        XCTAssertEqual(
            fake.state.windows.map(\.id).sorted(),
            ["win-A", "win-B"],
            "Both windows must survive the ⌘Q dance — next launch must see both snapshots."
        )

        windowA.close()
        windowB.close()
    }

    /// Defense-in-depth: even if `detachAllCloseObservers` was
    /// somehow missed, the `userInitiatedClose` flag (still false
    /// for an app-terminate path) keeps the registry on the
    /// preserve branch. Both safeguards must pass independently.
    func test_willClose_withoutUserIntent_preservesSnapshot() {
        let fake = FakeSessionStore()
        let registry = WindowRegistry()

        let app = AppState(
            services: nil,
            initialSidebarCollapsed: false,
            initialMainCwd: nil,
            windowSessionId: "win-no-intent",
            store: fake
        )
        app.windowSession.markInitializationComplete()
        // Seed the store as if a prior upsert had landed (mimics
        // what `willTerminate`'s tearDown loop does).
        fake.upsert(window: app.windowSession.snapshotPersistedWindow())

        let window = makeWindow()
        registry.register(appState: app, window: window)
        XCTAssertFalse(app.userInitiatedClose,
                       "Pre-condition: userInitiatedClose must default to false.")

        // Fire willClose WITHOUT setting the user-intent flag —
        // mimics SwiftUI's scene-teardown burst during terminate
        // when (hypothetically) the detach belt-and-suspenders is
        // bypassed.
        NotificationCenter.default.post(
            name: NSWindow.willCloseNotification, object: window
        )
        let drained = XCTestExpectation(description: "main queue drained")
        DispatchQueue.main.async { drained.fulfill() }
        wait(for: [drained], timeout: 1.0)

        XCTAssertEqual(fake.removeCalls, [],
                       "willClose without userInitiatedClose must NOT call remove — it routes via .appTerminating.")
        XCTAssertTrue(
            fake.state.windows.contains(where: { $0.id == "win-no-intent" }),
            "Snapshot must survive willClose when the user didn't initiate the close."
        )

        window.close()
    }

    /// The genuine user-close path still works: when
    /// `userInitiatedClose` is true (as `CloseConfirmationDelegate`
    /// sets it after `windowShouldClose` returns true), the
    /// registry routes to `.userClosedWindow` and the entry is
    /// dropped from the store.
    func test_willClose_withUserIntent_removesEntry() {
        let fake = FakeSessionStore()
        let registry = WindowRegistry()

        let app = AppState(
            services: nil,
            initialSidebarCollapsed: false,
            initialMainCwd: nil,
            windowSessionId: "win-user-closed",
            store: fake
        )
        app.windowSession.markInitializationComplete()
        fake.upsert(window: app.windowSession.snapshotPersistedWindow())

        let window = makeWindow()
        registry.register(appState: app, window: window)
        // The flag is what `CloseConfirmationDelegate.windowShouldClose`
        // flips when the user confirms (or the unconditional
        // no-live-panes branch).
        app.userInitiatedClose = true

        NotificationCenter.default.post(
            name: NSWindow.willCloseNotification, object: window
        )
        let drained = XCTestExpectation(description: "main queue drained")
        DispatchQueue.main.async { drained.fulfill() }
        wait(for: [drained], timeout: 1.0)

        XCTAssertEqual(fake.removeCalls, ["win-user-closed"],
                       "User-initiated close must call store.remove.")
        XCTAssertFalse(
            fake.state.windows.contains(where: { $0.id == "win-user-closed" }),
            "Entry must be gone from the store after a user-initiated close."
        )

        window.close()
    }

    /// Mixed scenario: user closes one window via traffic light
    /// (intent flag set), then ⌘Qs with another window still open.
    /// Result: the user-closed window is gone from the store; the
    /// remaining window's snapshot survives.
    func test_userCloseThenAppQuit_keepsOnlyTheSurvivor() {
        let fake = FakeSessionStore()
        let registry = WindowRegistry()

        let closed = AppState(
            services: nil,
            initialSidebarCollapsed: false,
            initialMainCwd: nil,
            windowSessionId: "win-closed",
            store: fake
        )
        closed.windowSession.markInitializationComplete()
        let surviving = AppState(
            services: nil,
            initialSidebarCollapsed: false,
            initialMainCwd: nil,
            windowSessionId: "win-surviving",
            store: fake
        )
        surviving.windowSession.markInitializationComplete()

        let windowClosed = makeWindow()
        let windowSurviving = makeWindow()
        registry.register(appState: closed, window: windowClosed)
        registry.register(appState: surviving, window: windowSurviving)

        // Pre-seed both snapshots on disk as if a tab mutation
        // had pushed them via scheduleSessionSave.
        fake.upsert(window: closed.windowSession.snapshotPersistedWindow())
        fake.upsert(window: surviving.windowSession.snapshotPersistedWindow())

        // User clicks the red traffic light on `closed`.
        closed.userInitiatedClose = true
        NotificationCenter.default.post(
            name: NSWindow.willCloseNotification, object: windowClosed
        )
        let userCloseDrained = XCTestExpectation(description: "user-close drained")
        DispatchQueue.main.async { userCloseDrained.fulfill() }
        wait(for: [userCloseDrained], timeout: 1.0)

        XCTAssertEqual(fake.state.windows.map(\.id), ["win-surviving"],
                       "After the user close, only the survivor remains on disk.")

        // User ⌘Qs with the survivor still open. Mimics
        // NiceServices's willTerminate observer.
        registry.detachAllCloseObservers()
        for state in registry.allAppStates {
            state.tearDown(reason: .appTerminating)
        }
        NotificationCenter.default.post(
            name: NSWindow.willCloseNotification, object: windowSurviving
        )
        let terminateDrained = XCTestExpectation(description: "terminate drained")
        DispatchQueue.main.async { terminateDrained.fulfill() }
        wait(for: [terminateDrained], timeout: 1.0)

        XCTAssertEqual(
            fake.state.windows.map(\.id), ["win-surviving"],
            "Survivor's snapshot must remain on disk after the terminate cascade — this is the regression that hit prod."
        )

        windowClosed.close()
        windowSurviving.close()
    }

    // MARK: - Helpers

    private func makeWindow() -> NSWindow {
        // `isReleasedWhenClosed = false` is load-bearing in unit tests:
        // the default (`true`) makes AppKit autorelease the window on
        // `close()`, which then dangles when XCTest's post-test
        // autorelease-pool pop sweeps and trips the memory checker.
        // Match the convention used elsewhere in the test target
        // (e.g. WindowRegistryTests.makeWindow).
        let w = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 400, height: 300),
            styleMask: [.borderless, .resizable],
            backing: .buffered,
            defer: true
        )
        w.isReleasedWhenClosed = false
        return w
    }
}
