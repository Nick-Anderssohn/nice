//
//  DoubleClickTitleBarActionTests.swift
//  NiceUnitTests
//
//  Covers the `AppleActionOnDoubleClick` → action mapping that drives
//  what a double-click on the custom title-bar band does. The default
//  (zoom) path is exercised end-to-end by
//  `WindowDragUITests.testEmptyToolbarDoubleClickZoomsWindow`; this
//  locks down the Minimize / None / unknown / absent branches, which
//  that UI test can't reach without mutating the host's global prefs.
//

import XCTest
@testable import Nice

final class DoubleClickTitleBarActionTests: XCTestCase {

    func test_minimize_mapsToMinimize() {
        XCTAssertEqual(DoubleClickTitleBarAction(rawSetting: "Minimize"), .minimize)
    }

    func test_none_mapsToNone() {
        XCTAssertEqual(DoubleClickTitleBarAction(rawSetting: "None"), DoubleClickTitleBarAction.none)
    }

    func test_maximize_mapsToZoom() {
        XCTAssertEqual(DoubleClickTitleBarAction(rawSetting: "Maximize"), .zoom)
    }

    func test_absent_defaultsToZoom() {
        // No value set in NSGlobalDomain ⇒ macOS default of Maximize/zoom.
        XCTAssertEqual(DoubleClickTitleBarAction(rawSetting: nil), .zoom)
    }

    func test_unknownValue_defaultsToZoom() {
        // A value we don't recognize must not no-op the gesture; fall
        // back to the system default so the band still behaves.
        XCTAssertEqual(DoubleClickTitleBarAction(rawSetting: "Frobnicate"), .zoom)
    }
}
