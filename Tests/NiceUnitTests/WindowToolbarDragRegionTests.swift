//
//  WindowToolbarDragRegionTests.swift
//  NiceUnitTests
//
//  Asserts the contract `ChromeDragStripView` exposes to AppKit:
//  `mouseDownCanMoveWindow` returns `false`. The view is now a PURE
//  MARKER for `ChromeEventRouter`'s per-press hit-test — it opts OUT of
//  AppKit's native title-bar drag (which is off anyway via
//  `isMovable = false`), and the router, not this flag, owns empty-chrome
//  drag and double-click-zoom.
//
//  The actual end-to-end drag and double-click-to-zoom behaviours
//  are covered by `WindowDragUITests`.
//

import AppKit
import SwiftUI
import XCTest
@testable import Nice

@MainActor
final class WindowToolbarDragRegionTests: XCTestCase {

    func testDragStripOptsOutOfNativeDrag() {
        let view = ChromeDragStripView()
        XCTAssertFalse(
            view.mouseDownCanMoveWindow,
            "ChromeDragStripView is a marker for the router's hit-test; native title-bar drag must stay OFF"
        )
    }
}
