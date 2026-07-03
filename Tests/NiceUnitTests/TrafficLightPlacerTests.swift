//
//  TrafficLightPlacerTests.swift
//  NiceUnitTests
//
//  Coverage for `TrafficLightPlacer`, the per-window owner of the three
//  standard window buttons' positions (close / miniaturize / zoom).
//  Replaces `TrafficLightNudgerTests`, reusing its hidden-title-bar
//  window-construction + RunLoop-spin pattern.
//
//  Two layers:
//
//    • PURE MATH — the static `desiredOriginX` / `desiredOriginY` are
//      tested with no window so the geometry contract (absolute y=26 row,
//      uniform +8 inward x preserving the OS pitch) is pinned independent
//      of AppKit's lay-out.
//
//    • INTEGRATION — construct a real hidden-title-bar `NSWindow`, read
//      each button's NATIVE default origin BEFORE placing, run the placer,
//      and assert each button ends at its OWN default + 8 (x) and centered
//      26pt from the top (y). All assertions are RELATIVE to each button's
//      captured default; we never hardcode the live-app measurements
//      (9/32/55), so the suite stays OS- and context-robust.
//
//  The regression pins live in the convergence / no-compound test
//  (BUG B's exact shape: capture-then-pin used to compound on every
//  re-apply) and the idempotence test (our own setFrameOrigin re-fires
//  the frame notification; the >0.5pt guard must hold).
//

import AppKit
import XCTest
@testable import Nice

@MainActor
final class TrafficLightPlacerTests: XCTestCase {

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

    private let kinds: [NSWindow.ButtonType] = [.closeButton, .miniaturizeButton, .zoomButton]

    /// Window-coord leading x of a button (its frame origin mapped from its
    /// superview into the window). Matches what the placer captures /
    /// targets.
    private func windowX(of button: NSView) throws -> CGFloat {
        let superview = try XCTUnwrap(button.superview)
        return superview.convert(button.frame, to: nil).origin.x
    }

    /// Window-coord visual center-from-top of a button.
    private func centerFromTop(of button: NSView, in window: NSWindow) throws -> CGFloat {
        let superview = try XCTUnwrap(button.superview)
        let contentHeight = try XCTUnwrap(window.contentView).bounds.height
        let originYInWindow = superview.convert(button.frame, to: nil).origin.y
        let centerYInWindow = originYInWindow + button.frame.height / 2
        return contentHeight - centerYInWindow
    }

    // MARK: - (a) Pure math

    func testDesiredOriginYAbsoluteTo26() {
        // center-from-top 26, button height 14 → origin 26pt below top of
        // a 600pt window = 600 - 26 - 7.
        XCTAssertEqual(
            TrafficLightPlacer.desiredOriginY(windowHeight: 600, buttonHeight: 14),
            567, accuracy: 0.0001
        )
        // Absolute: a taller window pushes the origin up by the same delta.
        XCTAssertEqual(
            TrafficLightPlacer.desiredOriginY(windowHeight: 800, buttonHeight: 14),
            767, accuracy: 0.0001
        )
    }

    func testDesiredOriginXUniformPlus8() {
        // +8 regardless of the OS default — documents OS-robustness. On
        // macOS 26 the default close x is 9 → 17; a stale-macOS default of
        // 20 → 28. The translation is identical either way.
        XCTAssertEqual(TrafficLightPlacer.desiredOriginX(nativeDefaultX: 9), 17, accuracy: 0.0001)
        XCTAssertEqual(TrafficLightPlacer.desiredOriginX(nativeDefaultX: 20), 28, accuracy: 0.0001)
    }

    // MARK: - (b) Integration: each button at its OWN default + 8, y=26

    func testPlacesEachButtonAtCapturedDefaultPlusEight() throws {
        let window = makeHiddenTitleBarWindow()

        // Capture native defaults BEFORE placing.
        var defaultX: [NSWindow.ButtonType: CGFloat] = [:]
        for kind in kinds {
            let button = try XCTUnwrap(window.standardWindowButton(kind))
            defaultX[kind] = try windowX(of: button)
        }

        let placer = TrafficLightPlacer(window: window)
        placer.start()
        spinRunLoop()

        for kind in kinds {
            let button = try XCTUnwrap(window.standardWindowButton(kind))
            let expectedX = try XCTUnwrap(defaultX[kind]) + WindowChrome.trafficLightNudgeX
            XCTAssertEqual(try windowX(of: button), expectedX, accuracy: 0.5,
                           "\(kind) x should be its own default + 8")
            XCTAssertEqual(try centerFromTop(of: button, in: window),
                           WindowChrome.trafficLightCenterFromTop, accuracy: 0.5,
                           "\(kind) center should sit 26pt from the window top")
        }
        placer.stop()
    }

    // MARK: - (c) Relative order + equal native pitch preserved

    func testPreservesMonotonicOrderAndEqualPitch() throws {
        let window = makeHiddenTitleBarWindow()
        let placer = TrafficLightPlacer(window: window)
        placer.start()
        spinRunLoop()

        let close = try windowX(of: try XCTUnwrap(window.standardWindowButton(.closeButton)))
        let mini = try windowX(of: try XCTUnwrap(window.standardWindowButton(.miniaturizeButton)))
        let zoom = try windowX(of: try XCTUnwrap(window.standardWindowButton(.zoomButton)))

        XCTAssertLessThan(close, mini, "close must lead miniaturize")
        XCTAssertLessThan(mini, zoom, "miniaturize must lead zoom")
        // Uniform +8 translation preserves the OS-native pitch (whatever it
        // is on this host): the two gaps stay equal. This is the graft-9
        // pin — we do NOT bake an absolute 28/48/68.
        XCTAssertEqual(mini - close, zoom - mini, accuracy: 0.5,
                       "uniform native inter-button pitch must be preserved")
        placer.stop()
    }

    // MARK: - (d) Convergence / no-compound (BUG B regression pin)

    func testReapplyConvergesAndDoesNotCompound() throws {
        let window = makeHiddenTitleBarWindow()
        let close = try XCTUnwrap(window.standardWindowButton(.closeButton))
        let defaultXVal = try windowX(of: close)

        let placer = TrafficLightPlacer(window: window)
        placer.start()
        spinRunLoop()
        XCTAssertEqual(try windowX(of: close),
                       defaultXVal + WindowChrome.trafficLightNudgeX, accuracy: 0.5,
                       "precondition: close should be placed at default + 8")

        // Simulate AppKit clobbering the button back to its native default
        // (what a freshly-opened tear-off window does post-finalization).
        let superview = try XCTUnwrap(close.superview)
        close.setFrameOrigin(superview.convert(CGPoint(x: defaultXVal, y: close.frame.origin.y), from: nil))

        // A window move (the tear-off's post-open reposition) re-resolves +
        // re-applies. The target is ABSOLUTE (default + 8), so it lands back
        // at default + 8, NOT default + 16.
        NotificationCenter.default.post(name: NSWindow.didMoveNotification, object: window)
        spinRunLoop()
        XCTAssertEqual(try windowX(of: close),
                       defaultXVal + WindowChrome.trafficLightNudgeX, accuracy: 0.5,
                       "re-apply must restore default + 8, not compound")

        // Repeat the move three times: still default + 8, never default + N·8.
        for _ in 0..<3 {
            NotificationCenter.default.post(name: NSWindow.didMoveNotification, object: window)
        }
        spinRunLoop()
        XCTAssertEqual(try windowX(of: close),
                       defaultXVal + WindowChrome.trafficLightNudgeX, accuracy: 0.5,
                       "repeated re-applies must not compound the offset")
        placer.stop()
    }

    // MARK: - (e) Idempotence: the >0.5pt guard holds

    func testIdempotentOnRepeatedFrameChange() throws {
        let window = makeHiddenTitleBarWindow()
        let close = try XCTUnwrap(window.standardWindowButton(.closeButton))

        let placer = TrafficLightPlacer(window: window)
        placer.start()
        spinRunLoop()
        let settled = close.frame.origin

        // Re-post the button's own frameDidChange after it has settled. The
        // >0.5pt guard means apply() finds cur == desired and does not move
        // it, so the origin is unchanged (and we don't recurse).
        NotificationCenter.default.post(name: NSView.frameDidChangeNotification, object: close)
        spinRunLoop()

        XCTAssertEqual(close.frame.origin.x, settled.x, accuracy: 0.5,
                       "settled x must not drift on a redundant frame event")
        XCTAssertEqual(close.frame.origin.y, settled.y, accuracy: 0.5,
                       "settled y must not drift on a redundant frame event")
        placer.stop()
    }

    // MARK: - (f) Pin toggle: inline placement + state

    func testPinSitsOnePitchRightOfZoomOnSharedRow() throws {
        let window = makeHiddenTitleBarWindow()
        let placer = TrafficLightPlacer(window: window)
        placer.start()
        spinRunLoop()

        let miniButton = try XCTUnwrap(window.standardWindowButton(.miniaturizeButton))
        let zoomButton = try XCTUnwrap(window.standardWindowButton(.zoomButton))
        let mini = try windowX(of: miniButton)
        let zoom = try windowX(of: zoomButton)
        let pitch = zoom - mini

        // Parented into the lights' superview so it shares their space.
        let pin = placer.pinButton
        XCTAssertNotNil(pin.superview, "pin should be parented into the chrome")
        XCTAssertFalse(pin.isHidden, "pin should be visible in windowed mode")

        // One native pitch to zoom's right — the same gap the lights keep.
        XCTAssertEqual(try windowX(of: pin), zoom + pitch, accuracy: 0.5,
                       "pin should sit one native inter-button pitch right of zoom")
        // Shares the absolute 26pt-from-top top-bar row.
        XCTAssertEqual(try centerFromTop(of: pin, in: window),
                       WindowChrome.trafficLightCenterFromTop, accuracy: 0.5,
                       "pin should share the 26pt-from-top row with the lights")
        // Matches zoom's size (a fourth light).
        XCTAssertEqual(pin.frame.width, zoomButton.frame.width, accuracy: 0.5,
                       "pin width should match the zoom button")
        XCTAssertEqual(pin.frame.height, zoomButton.frame.height, accuracy: 0.5,
                       "pin height should match the zoom button")
        placer.stop()
    }

    func testPinHoldsPlacementAcrossResizeAndRefocus() throws {
        let window = makeHiddenTitleBarWindow()
        let placer = TrafficLightPlacer(window: window)
        placer.start()
        spinRunLoop()

        let zoomButton = try XCTUnwrap(window.standardWindowButton(.zoomButton))
        let miniButton = try XCTUnwrap(window.standardWindowButton(.miniaturizeButton))
        let expectedPitch = try windowX(of: zoomButton) - (try windowX(of: miniButton))

        // Resize taller: the absolute y target keeps the pin on the 26pt row.
        window.setContentSize(NSSize(width: 900, height: 720))
        NotificationCenter.default.post(name: NSWindow.didResizeNotification, object: window)
        spinRunLoop()
        XCTAssertEqual(try centerFromTop(of: placer.pinButton, in: window),
                       WindowChrome.trafficLightCenterFromTop, accuracy: 0.5,
                       "pin should stay on the 26pt row after a resize")
        XCTAssertEqual(try windowX(of: placer.pinButton),
                       (try windowX(of: zoomButton)) + expectedPitch, accuracy: 0.5,
                       "pin should stay one pitch right of zoom after a resize")

        // Refocus (didBecomeKey) re-resolves + re-applies; the pin holds.
        NotificationCenter.default.post(name: NSWindow.didBecomeKeyNotification, object: window)
        spinRunLoop()
        XCTAssertEqual(try windowX(of: placer.pinButton),
                       (try windowX(of: zoomButton)) + expectedPitch, accuracy: 0.5,
                       "pin should stay one pitch right of zoom after refocus")
        placer.stop()
    }

    func testPinToggleFlipsActiveState() {
        let pin = ChromePinButton()
        XCTAssertFalse(pin.isActive, "pin starts inactive")
        pin.performClick(nil)
        XCTAssertTrue(pin.isActive, "click activates the pin")
        pin.performClick(nil)
        XCTAssertFalse(pin.isActive, "second click deactivates the pin")
    }

    func testStopRemovesPinFromChrome() throws {
        let window = makeHiddenTitleBarWindow()
        let placer = TrafficLightPlacer(window: window)
        placer.start()
        spinRunLoop()
        XCTAssertNotNil(placer.pinButton.superview, "precondition: pin is parented")

        placer.stop()
        XCTAssertNil(placer.pinButton.superview, "stop() must remove the pin from the chrome")
    }
}
