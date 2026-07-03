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

    /// The status-bar widget host is the router's `.widget` marker: it must
    /// conform to `ChromeWidgetHosting` and report `mouseDownCanMoveWindow ==
    /// false` so the router's attribute-walk fallback never reclassifies a
    /// widget press as the draggable empty-chrome strip.
    func testWidgetHostOptsOutOfNativeDragAndIsMarker() {
        let host = ChromeWidgetGuard<AnyView>.WidgetHostView(rootView: AnyView(Text("x")))
        XCTAssertFalse(
            host.mouseDownCanMoveWindow,
            "widget host must opt out of native drag so the router treats it as a widget, not chrome"
        )
        // Compile-time proof that the host is the router's widget marker.
        let _: ChromeWidgetHosting = host
    }
}
