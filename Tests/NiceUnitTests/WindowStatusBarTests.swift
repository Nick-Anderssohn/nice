//
//  WindowStatusBarTests.swift
//  NiceUnitTests
//
//  Contracts of the bottom status bar's non-visual pieces:
//
//    • `ChromeWidgetHostingView` — the AppKit host every status-bar
//      widget renders inside. It must claim its whole bounds in
//      `hitTest(_:)` (so `ChromeEventRouter` finds the
//      `ChromeWidgetHosting` marker and passes the press through) and
//      report `mouseDownCanMoveWindow == false` (so the router's
//      attribute-walk fallback can't classify a widget as empty chrome).
//      Together these are what make "a widget press never moves the
//      window" structural rather than a flag.
//
//    • `StatusBarText` — the pure display helpers (clock format, home
//      abbreviation).
//
//  The live drag / double-click behaviour of the bar's EMPTY pixels runs
//  through the same `ChromeEventRouter` path as the top bar; its pure
//  decision table (including the `.widget` rows) is locked down in
//  `ChromeEventRouterTests`.
//

import AppKit
import SwiftUI
import XCTest
@testable import Nice

@MainActor
final class WindowStatusBarTests: XCTestCase {

    // MARK: - Widget host contract

    func testWidgetHostOptsOutOfNativeDrag() {
        let host = ChromeWidgetHostingView(rootView: AnyView(Text("x")))
        XCTAssertFalse(
            host.mouseDownCanMoveWindow,
            "A status-bar widget press must never ride the native window drag"
        )
    }

    func testWidgetHostIsRouterMarker() {
        let host = ChromeWidgetHostingView(rootView: AnyView(Text("x")))
        XCTAssertTrue(
            (host as AnyObject) is ChromeWidgetHosting,
            "ChromeEventRouter classifies widget presses by this marker conformance"
        )
    }

    func testWidgetHostClaimsItsWholeBounds() {
        let parent = NSView(frame: NSRect(x: 0, y: 0, width: 300, height: 28))
        let host = ChromeWidgetHostingView(rootView: AnyView(Color.clear))
        host.frame = NSRect(x: 0, y: 0, width: 100, height: 28)
        parent.addSubview(host)

        XCTAssertTrue(
            host.hitTest(NSPoint(x: 50, y: 14)) === host,
            "Every in-bounds point must resolve to the widget host itself"
        )
        XCTAssertNil(
            host.hitTest(NSPoint(x: 150, y: 14)),
            "Out-of-bounds points must fall through to the bar (empty chrome)"
        )
    }

    // MARK: - Clock format

    func testClockFormatsHHmm() {
        var components = DateComponents()
        components.year = 2026
        components.month = 7
        components.day = 2
        components.hour = 9
        components.minute = 5
        let calendar = Calendar(identifier: .gregorian)
        let date = calendar.date(from: components)!
        XCTAssertEqual(
            StatusBarText.clock(date, timeZone: calendar.timeZone),
            "09:05"
        )
    }

    func testClockUses24HourClock() {
        var components = DateComponents()
        components.year = 2026
        components.month = 7
        components.day = 2
        components.hour = 21
        components.minute = 47
        let calendar = Calendar(identifier: .gregorian)
        let date = calendar.date(from: components)!
        XCTAssertEqual(
            StatusBarText.clock(date, timeZone: calendar.timeZone),
            "21:47"
        )
    }

    // MARK: - Home abbreviation

    func testAbbreviateHomeExactMatch() {
        XCTAssertEqual(
            StatusBarText.abbreviateHome("/Users/x", home: "/Users/x"),
            "~"
        )
    }

    func testAbbreviateHomePrefix() {
        XCTAssertEqual(
            StatusBarText.abbreviateHome("/Users/x/Projects/nice", home: "/Users/x"),
            "~/Projects/nice"
        )
    }

    func testAbbreviateHomeRequiresComponentBoundary() {
        // "/Users/xylophone" must NOT abbreviate for home "/Users/x".
        XCTAssertEqual(
            StatusBarText.abbreviateHome("/Users/xylophone", home: "/Users/x"),
            "/Users/xylophone"
        )
    }

    func testAbbreviateHomeLeavesForeignPathsAlone() {
        XCTAssertEqual(
            StatusBarText.abbreviateHome("/tmp/scratch", home: "/Users/x"),
            "/tmp/scratch"
        )
    }

    func testAbbreviateHomeDegenerateHomes() {
        XCTAssertEqual(StatusBarText.abbreviateHome("/tmp", home: ""), "/tmp")
        XCTAssertEqual(StatusBarText.abbreviateHome("/tmp", home: "/"), "/tmp")
    }
}
