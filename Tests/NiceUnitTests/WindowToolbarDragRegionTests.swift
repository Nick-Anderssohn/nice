//
//  WindowToolbarDragRegionTests.swift
//  NiceUnitTests
//
//  Asserts the contract `WindowDragRegion.DragView` exposes to
//  AppKit's title-bar drag tracker: `mouseDownCanMoveWindow` returns
//  `true`, opting the empty-chrome regions into the cooperative
//  drag path. Without this flag, AppKit's tracker doesn't fire and
//  the user can't drag the window from the toolbar.
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

    func testDragViewOptsIntoCooperativeDrag() {
        let view = WindowDragRegion.DragView()
        XCTAssertTrue(
            view.mouseDownCanMoveWindow,
            "DragView must opt in to AppKit's cooperative title-bar drag tracker"
        )
    }
}
