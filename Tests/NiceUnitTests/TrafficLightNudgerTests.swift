//
//  TrafficLightNudgerTests.swift
//  NiceUnitTests
//
//  Regression coverage for the torn-off-window traffic-light
//  misalignment (bug 2). The visible symptom: a window born during a
//  live tear-off drag had its standard window buttons left at the
//  default macOS position instead of nudged into the sidebar card.
//
//  Root cause: AppKit re-lays-out the standard buttons asynchronously
//  AFTER the first synchronous nudge (and, for the tear-off window,
//  after the post-open `setFrameOrigin` reposition), clobbering the
//  offset — and the window opens already-key + is never resized, so
//  none of the pre-existing focus/resize re-applies ever fire to fix
//  it. The fix re-applies on `NSWindow.didMoveNotification` (covering
//  the reposition) and on a short deferred schedule.
//
//  This test pins the `didMove` re-apply: nudge a window, simulate
//  AppKit clobbering a button back to its default origin, post a move
//  notification, and assert the offset is restored. Pre-fix (no
//  `didMove` observer) the button would stay at the clobbered origin.
//

import AppKit
import XCTest
@testable import Nice

@MainActor
final class TrafficLightNudgerTests: XCTestCase {

    private func makeHiddenTitleBarWindow() -> NSWindow {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 800, height: 600),
            styleMask: [.titled, .closable, .miniaturizable, .resizable,
                        .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        return window
    }

    private func spinRunLoop(_ seconds: TimeInterval = 0.35) {
        RunLoop.current.run(until: Date().addingTimeInterval(seconds))
    }

    /// The first nudge offsets every standard button by (dx, dy) from
    /// its captured canonical origin.
    func testNudgeOffsetsButtons() throws {
        let window = makeHiddenTitleBarWindow()
        let close = try XCTUnwrap(window.standardWindowButton(.closeButton))
        let canonical = close.frame.origin

        TrafficLightNudger.nudge(window: window, dx: 8, dy: -10)
        spinRunLoop()

        XCTAssertEqual(close.frame.origin.x, canonical.x + 8, accuracy: 0.5)
        XCTAssertEqual(close.frame.origin.y, canonical.y - 10, accuracy: 0.5)
    }

    /// After AppKit clobbers a button back to default, a window-move
    /// notification re-applies the offset (the tear-off-window fix). A
    /// move fires when the torn-off window is repositioned post-open.
    func testReappliesOffsetOnWindowMove() throws {
        let window = makeHiddenTitleBarWindow()
        let close = try XCTUnwrap(window.standardWindowButton(.closeButton))
        let canonical = close.frame.origin

        TrafficLightNudger.nudge(window: window, dx: 8, dy: -10)
        spinRunLoop()
        XCTAssertEqual(close.frame.origin.x, canonical.x + 8, accuracy: 0.5,
                       "precondition: button should be nudged")

        // Simulate AppKit re-laying-out the button back to its default
        // position (what happens on a freshly-opened tear-off window).
        close.setFrameOrigin(canonical)
        XCTAssertEqual(close.frame.origin.x, canonical.x, accuracy: 0.5,
                       "precondition: button clobbered back to default")

        // A window move (the tear-off window's post-open reposition)
        // must re-assert the offset.
        NotificationCenter.default.post(
            name: NSWindow.didMoveNotification, object: window
        )
        spinRunLoop()

        XCTAssertEqual(close.frame.origin.x, canonical.x + 8, accuracy: 0.5,
                       "window move must re-apply the traffic-light nudge")
        XCTAssertEqual(close.frame.origin.y, canonical.y - 10, accuracy: 0.5)
    }

    /// Re-applying never compounds: repeated nudges/moves keep the
    /// button at canonical + offset, not canonical + N·offset.
    func testReapplyDoesNotCompound() throws {
        let window = makeHiddenTitleBarWindow()
        let close = try XCTUnwrap(window.standardWindowButton(.closeButton))
        let canonical = close.frame.origin

        TrafficLightNudger.nudge(window: window, dx: 8, dy: -10)
        spinRunLoop()
        for _ in 0..<3 {
            NotificationCenter.default.post(
                name: NSWindow.didMoveNotification, object: window
            )
        }
        spinRunLoop()

        XCTAssertEqual(close.frame.origin.x, canonical.x + 8, accuracy: 0.5,
                       "offset must not compound across re-applies")
        XCTAssertEqual(close.frame.origin.y, canonical.y - 10, accuracy: 0.5)
    }
}
