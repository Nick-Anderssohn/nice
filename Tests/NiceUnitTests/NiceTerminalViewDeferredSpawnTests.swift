//
//  NiceTerminalViewDeferredSpawnTests.swift
//  NiceUnitTests
//
//  Pins the gating behaviour of `NiceTerminalView.armDeferredSpawn(...)`
//  and `setFrameSize(_:)` — the architectural fix that defers shell
//  spawn until AppKit has assigned the view a real frame, so the pty's
//  first `TIOCSWINSZ` reflects real geometry rather than SwiftTerm's
//  80×25 zero-frame fallback.
//
//  These tests exercise the gate state machine directly via
//  `@testable` access to `pendingSpawn` / `hasFiredPendingSpawn`. They
//  do *not* attempt to verify the synchronous-resize ordering claim
//  (that `terminal.cols` is updated before `startProcess` runs); that
//  guarantee is structural in `setFrameSize`'s body and is covered by
//  manual smoke-testing the row-0 startup invariant.
//
//  Note on `window.contentView = view`: AppKit auto-sizes a window's
//  contentView to the window's content rect, which fires
//  `setFrameSize` with a non-zero size. Tests that need to keep the
//  view's frame zero while still being window-attached use the
//  `addSubview` pattern via a sized host view instead.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class NiceTerminalViewDeferredSpawnTests: XCTestCase {

    // MARK: - Gate stays armed until both window + non-zero frame

    func test_armDeferredSpawn_withZeroFrame_noWindow_doesNotSpawn() {
        let view = NiceTerminalView(frame: .zero)

        view.armDeferredSpawn(
            executable: "/usr/bin/true",
            args: [],
            environment: nil,
            execName: nil,
            currentDirectory: nil
        )

        XCTAssertFalse(
            view.hasFiredPendingSpawn,
            "gate must stay closed while frame is .zero and view is detached"
        )
        XCTAssertNotNil(
            view.pendingSpawn,
            "spawn args must be retained for the eventual layout-pass fire"
        )
        XCTAssertFalse(
            view.process.running,
            "no child should be forked before the gate fires"
        )
    }

    func test_armDeferredSpawn_withWindowButZeroFrame_doesNotFire() {
        let view = NiceTerminalView(frame: .zero)
        // Use a sized host so `view.window` becomes non-nil without
        // AppKit auto-resizing `view` away from .zero (which would
        // happen if we made `view` the contentView directly).
        let window = makeWindow()
        let host = NSView(frame: NSRect(x: 0, y: 0, width: 100, height: 100))
        window.contentView = host
        host.addSubview(view)

        XCTAssertNotNil(view.window, "host setup must give view a window")
        XCTAssertEqual(view.frame.size, .zero, "view frame must still be zero")

        view.armDeferredSpawn(
            executable: "/usr/bin/true",
            args: [],
            environment: nil,
            execName: nil,
            currentDirectory: nil
        )

        XCTAssertFalse(
            view.hasFiredPendingSpawn,
            "frame > 0 guard must keep the gate closed even when the view is in a window"
        )
        XCTAssertNotNil(view.pendingSpawn)
    }

    // MARK: - First non-zero frame fires the gate via the immediate-apply path

    func test_attachToWindow_afterArm_firesGate_andRestoresDebounce() {
        let view = NiceTerminalView(frame: .zero)
        view.resizeDebounceMs = 200      // ambient runtime value

        // Arm BEFORE attaching to the window. The contentView
        // assignment below is what triggers the FIRST non-zero
        // setFrameSize, which is the path under test (the
        // immediate-apply trick that synchronously applies the
        // resize through SwiftTerm so terminal.cols × rows reflects
        // real geometry before startProcess runs).
        view.armDeferredSpawn(
            executable: "/usr/bin/true",
            args: [],
            environment: nil,
            execName: nil,
            currentDirectory: nil
        )
        XCTAssertFalse(view.hasFiredPendingSpawn, "no fire before attach")

        let window = makeWindow()
        window.contentView = view   // fires setFrameSize(realSize) + viewDidMoveToWindow

        XCTAssertTrue(
            view.hasFiredPendingSpawn,
            "first non-zero layout pass with window attached must fire the gate"
        )
        XCTAssertNil(view.pendingSpawn, "args are consumed once the gate fires")
        XCTAssertEqual(
            view.resizeDebounceMs, 200,
            "the immediate-apply trick zeros resizeDebounceMs through "
            + "super.setFrameSize; the original value MUST be restored "
            + "afterward so runtime fast-drag bursts still coalesce"
        )
    }

    func test_setFrameSize_afterFirstFire_doesNotZeroDebounce() {
        let view = NiceTerminalView(frame: .zero)
        view.resizeDebounceMs = 200

        view.armDeferredSpawn(
            executable: "/usr/bin/true",
            args: [],
            environment: nil,
            execName: nil,
            currentDirectory: nil
        )
        let window = makeWindow()
        window.contentView = view   // first fire
        XCTAssertTrue(view.hasFiredPendingSpawn)

        // Subsequent layout pass — should take the normal coalesced
        // path. `needsImmediateApply` must evaluate false because
        // `hasFiredPendingSpawn == true`, so the debounce is not
        // touched.
        view.setFrameSize(NSSize(width: 1000, height: 500))
        XCTAssertEqual(
            view.resizeDebounceMs, 200,
            "post-fire setFrameSize must leave resizeDebounceMs at its "
            + "ambient value — runtime drags depend on the coalescer"
        )
        XCTAssertNil(view.pendingSpawn)
    }

    // MARK: - helpers

    /// Off-screen NSWindow used to flip `view.window` from nil to
    /// non-nil. Pattern lifted from `WindowRegistryTests.makeWindow`.
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
