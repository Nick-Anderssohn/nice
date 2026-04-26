//
//  WindowRegistryTests.swift
//  NiceUnitTests
//
//  Exercises the NSWindow → AppState mapping that KeyboardShortcutMonitor
//  and app-wide termination route through. The important invariants:
//  register stores an entry reachable via activeAppState, unregistered
//  windows (Settings) are correctly identified, and willClose cleanup
//  removes the entry without leaking notification observers.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class WindowRegistryTests: XCTestCase {

    private var homeSandbox: TestHomeSandbox!

    override func setUp() {
        super.setUp()
        homeSandbox = TestHomeSandbox()
    }

    override func tearDown() {
        homeSandbox.teardown()
        homeSandbox = nil
        super.tearDown()
    }

    // MARK: - register / isSettingsWindow

    func test_register_makesWindowNonSettings() {
        let registry = WindowRegistry()
        let appState = AppState()
        let window = makeWindow()
        defer {
            appState.tearDown()
            window.close()
        }

        XCTAssertTrue(registry.isSettingsWindow(window),
                      "Before register, an unknown window must be treated as Settings-like (i.e. shortcuts ignored).")
        registry.register(appState: appState, window: window)
        XCTAssertFalse(registry.isSettingsWindow(window),
                       "After register, the window must be recognised as a main window.")
    }

    func test_register_storesAppStateReachableViaActiveAppState() {
        let registry = WindowRegistry()
        let appState = AppState()
        let window = makeWindow()
        defer {
            appState.tearDown()
            window.close()
        }

        registry.register(appState: appState, window: window)
        // `activeAppState()` without preferring key falls back to
        // lastActiveAppState — which `register` seeds on first call.
        XCTAssertIdentical(registry.activeAppState(), appState)
    }

    func test_register_isIdempotentForSameWindow() {
        let registry = WindowRegistry()
        let first = AppState()
        let second = AppState()
        let window = makeWindow()
        defer {
            first.tearDown()
            second.tearDown()
            window.close()
        }

        registry.register(appState: first, window: window)
        // A second register with a different AppState for the same
        // window must be a no-op — the original mapping is preserved.
        registry.register(appState: second, window: window)
        XCTAssertEqual(registry.allAppStates.count, 1,
                       "Double-register must not duplicate entries.")
        XCTAssertIdentical(registry.allAppStates.first, first,
                           "Original appState mapping must win — re-register is a no-op.")
    }

    func test_register_installsCloseConfirmationDelegate() {
        let registry = WindowRegistry()
        let appState = AppState()
        let window = makeWindow()
        defer {
            appState.tearDown()
            window.close()
        }

        XCTAssertNil(window.delegate, "Fresh NSWindow has no delegate.")
        registry.register(appState: appState, window: window)
        XCTAssertNotNil(window.delegate,
                        "register must install the close-confirmation proxy as the window's delegate.")
    }

    // MARK: - multiple windows

    func test_multipleWindows_storedByObjectIdentifier() {
        let registry = WindowRegistry()
        let a = AppState()
        let b = AppState()
        let windowA = makeWindow()
        let windowB = makeWindow()
        defer {
            a.tearDown()
            b.tearDown()
            windowA.close()
            windowB.close()
        }

        registry.register(appState: a, window: windowA)
        registry.register(appState: b, window: windowB)
        XCTAssertEqual(registry.allAppStates.count, 2)
    }

    // MARK: - handleClose via willCloseNotification

    func test_willCloseNotification_removesEntryAndTearsDownAppState() {
        let registry = WindowRegistry()
        let appState = AppState()
        let window = makeWindow()
        registry.register(appState: appState, window: window)
        XCTAssertFalse(registry.isSettingsWindow(window))

        // Fire the same notification AppKit posts on close. The
        // registry's observer should run synchronously on the main
        // queue and call handleClose, which removes the entry.
        NotificationCenter.default.post(
            name: NSWindow.willCloseNotification, object: window
        )

        // The observer is registered with `queue: .main`, so the post
        // may be delivered asynchronously on a later run loop cycle.
        // Spin the run loop briefly to let it fire.
        let expectation = XCTestExpectation(description: "willClose delivered")
        DispatchQueue.main.async { expectation.fulfill() }
        wait(for: [expectation], timeout: 1.0)

        XCTAssertTrue(registry.isSettingsWindow(window),
                      "After close, window should no longer be in the registry.")
        XCTAssertTrue(registry.allAppStates.isEmpty,
                      "Entry must be removed — leaving it in leaks the AppState + notification observers.")

        window.close()
    }

    // MARK: - activeAppState fallback

    func test_activeAppState_fallsBackToFirstRegisteredWhenNoLastActive() {
        let registry = WindowRegistry()
        let appState = AppState()
        let window = makeWindow()
        defer {
            appState.tearDown()
            window.close()
        }

        // Initially nothing registered — activeAppState returns nil.
        XCTAssertNil(registry.activeAppState())

        registry.register(appState: appState, window: window)
        // First register seeds lastActiveAppState, so activeAppState
        // returns that AppState without needing a didBecomeKey notif.
        XCTAssertIdentical(registry.activeAppState(), appState)
    }

    func test_activeAppState_preferKey_returnsNilWhenKeyWindowUnregistered() {
        // preferKey=true with no registered windows should fall through
        // to lastActiveAppState (nil) and ultimately return nil.
        let registry = WindowRegistry()
        XCTAssertNil(registry.activeAppState(preferKey: true))
    }

    // MARK: - Session-id lookups (cross-window undo focus follow)

    func test_appStateForSessionId_returnsRegisteredAppState() {
        let registry = WindowRegistry()
        let appState = AppState(
            services: nil,
            initialSidebarCollapsed: false,
            initialMainCwd: nil,
            windowSessionId: "win-A"
        )
        let window = makeWindow()
        defer {
            appState.tearDown()
            window.close()
        }
        registry.register(appState: appState, window: window)

        XCTAssertIdentical(registry.appState(forSessionId: "win-A"), appState)
    }

    func test_appStateForSessionId_returnsNilForUnknownId() {
        let registry = WindowRegistry()
        let appState = AppState(
            services: nil,
            initialSidebarCollapsed: false,
            initialMainCwd: nil,
            windowSessionId: "win-A"
        )
        let window = makeWindow()
        defer {
            appState.tearDown()
            window.close()
        }
        registry.register(appState: appState, window: window)

        XCTAssertNil(registry.appState(forSessionId: "win-other"))
    }

    func test_windowForSessionId_returnsRegisteredWindow() {
        let registry = WindowRegistry()
        let appState = AppState(
            services: nil,
            initialSidebarCollapsed: false,
            initialMainCwd: nil,
            windowSessionId: "win-B"
        )
        let window = makeWindow()
        defer {
            appState.tearDown()
            window.close()
        }
        registry.register(appState: appState, window: window)

        XCTAssertIdentical(registry.window(forSessionId: "win-B"), window)
    }

    func test_bringToFront_callsMakeKeyAndOrderFront_onRegisteredWindow() {
        let registry = WindowRegistry()
        let appState = AppState(
            services: nil,
            initialSidebarCollapsed: false,
            initialMainCwd: nil,
            windowSessionId: "win-C"
        )
        let window = RecordingWindow(
            contentRect: NSRect(x: 0, y: 0, width: 100, height: 100),
            styleMask: [.titled],
            backing: .buffered,
            defer: false
        )
        window.isReleasedWhenClosed = false
        defer {
            appState.tearDown()
            window.close()
        }
        registry.register(appState: appState, window: window)

        registry.bringToFront(sessionId: "win-C")

        XCTAssertEqual(window.makeKeyAndOrderFrontCallCount, 1)
    }

    func test_bringToFront_unknownSessionId_isNoOp() {
        let registry = WindowRegistry()
        // Should not crash; nothing observable.
        registry.bringToFront(sessionId: "unknown")
    }

    // MARK: - helpers

    /// A minimally-viable NSWindow for unit tests. Off-screen, no style,
    /// not brought to front. Safe to close in teardown.
    private func makeWindow() -> NSWindow {
        let w = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 100, height: 100),
            styleMask: [.titled],
            backing: .buffered,
            defer: false
        )
        w.isReleasedWhenClosed = false
        return w
    }
}

/// `NSWindow` subclass that counts `makeKeyAndOrderFront` calls so
/// tests can assert focus-routing without poking AppKit globals.
final class RecordingWindow: NSWindow {
    var makeKeyAndOrderFrontCallCount: Int = 0

    override func makeKeyAndOrderFront(_ sender: Any?) {
        makeKeyAndOrderFrontCallCount += 1
        // Don't call super — we don't want the test to actually
        // bring the window forward and steal focus from the runner.
    }
}
