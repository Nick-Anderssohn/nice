//
//  ChromeEventRouterTests.swift
//  NiceUnitTests
//
//  Exhaustive table over `ChromeEventRouter.decision(hitChain:clickCount:
//  inBand:isFullScreen:)` — the PURE arbitration the process-wide local
//  monitor runs on every chrome press. The live `performDrag` / `performZoom`
//  / `isMovable` side of the router needs a real window + event, so it is
//  covered by `WindowDragUITests` / `PaneReorderUITests` / `TearOffHookUITests`
//  instead; this locks down the routing logic itself.
//
//  Invariants asserted:
//    • out-of-band OR full screen ⇒ pass through (no drag, no zoom).
//    • empty-chrome strip: single click arms a drag, double click runs the
//      double-click action.
//    • a pill press always passes through (never drags, never zooms), and
//      WINS over the strip when both are in the chain (pill precedence).
//

import XCTest
@testable import Nice

@MainActor
final class ChromeEventRouterTests: XCTestCase {

    private typealias HitKind = ChromeEventRouter.HitKind
    private typealias Routing = ChromeEventRouter.Routing

    // MARK: - Pill: always pass through, never zooms

    func test_pill_singleClick_passesThrough() {
        XCTAssertEqual(
            ChromeEventRouter.decision(
                hitChain: [.pill], clickCount: 1, inBand: true, isFullScreen: false
            ),
            .passThrough
        )
    }

    func test_pill_doubleClick_passesThrough_noZoom() {
        // A double-click on a pill must NOT zoom — the pill owns its press.
        XCTAssertEqual(
            ChromeEventRouter.decision(
                hitChain: [.pill], clickCount: 2, inBand: true, isFullScreen: false
            ),
            .passThrough
        )
    }

    // MARK: - Strip: single click arms drag, double click acts

    func test_strip_singleClick_armsDrag() {
        XCTAssertEqual(
            ChromeEventRouter.decision(
                hitChain: [.strip], clickCount: 1, inBand: true, isFullScreen: false
            ),
            .armDrag
        )
    }

    func test_strip_doubleClick_runsDoubleClickAction() {
        XCTAssertEqual(
            ChromeEventRouter.decision(
                hitChain: [.strip], clickCount: 2, inBand: true, isFullScreen: false
            ),
            .doubleClickAction
        )
    }

    // MARK: - Band / full-screen gating

    func test_strip_outOfBand_passesThrough() {
        XCTAssertEqual(
            ChromeEventRouter.decision(
                hitChain: [.strip], clickCount: 1, inBand: false, isFullScreen: false
            ),
            .passThrough
        )
    }

    func test_strip_fullScreen_passesThrough() {
        XCTAssertEqual(
            ChromeEventRouter.decision(
                hitChain: [.strip], clickCount: 2, inBand: true, isFullScreen: true
            ),
            .passThrough
        )
    }

    // MARK: - Non-chrome content

    func test_emptyChain_passesThrough() {
        XCTAssertEqual(
            ChromeEventRouter.decision(
                hitChain: [], clickCount: 1, inBand: true, isFullScreen: false
            ),
            .passThrough
        )
    }

    // MARK: - Widget: always pass through, never drags or zooms

    func test_widget_singleClick_passesThrough() {
        // A press/drag on a status-bar widget (cwd / clock) must never arm a
        // window drag.
        XCTAssertEqual(
            ChromeEventRouter.decision(
                hitChain: [.widget], clickCount: 1, inBand: true, isFullScreen: false
            ),
            .passThrough
        )
    }

    func test_widget_doubleClick_passesThrough_noZoom() {
        // A double-click on a widget must NOT zoom — the widget owns its press.
        XCTAssertEqual(
            ChromeEventRouter.decision(
                hitChain: [.widget], clickCount: 2, inBand: true, isFullScreen: false
            ),
            .passThrough
        )
    }

    func test_widgetOverStrip_singleClick_passesThrough() {
        // The widget sits on top of the status bar's empty-chrome strip; the
        // widget must win so a press/drag on it never moves the window.
        XCTAssertEqual(
            ChromeEventRouter.decision(
                hitChain: [.widget, .strip], clickCount: 1, inBand: true, isFullScreen: false
            ),
            .passThrough
        )
    }

    func test_widgetOverStrip_doubleClick_passesThrough() {
        // The widget wins even on a double-click, so a press on it never zooms.
        XCTAssertEqual(
            ChromeEventRouter.decision(
                hitChain: [.widget, .strip], clickCount: 2, inBand: true, isFullScreen: false
            ),
            .passThrough
        )
    }

    // MARK: - Pill precedence over strip

    func test_pillOverStrip_singleClick_passesThrough() {
        // A pill drawn on top of the strip background: on a single click the
        // pill must win, so the press never arms a window drag.
        XCTAssertEqual(
            ChromeEventRouter.decision(
                hitChain: [.pill, .strip], clickCount: 1, inBand: true, isFullScreen: false
            ),
            .passThrough
        )
    }

    func test_pillOverStrip_doubleClick_passesThrough() {
        // A pill drawn on top of the strip background: the pill must win even
        // on a double-click, so a press on the pill never zooms the window.
        XCTAssertEqual(
            ChromeEventRouter.decision(
                hitChain: [.pill, .strip], clickCount: 2, inBand: true, isFullScreen: false
            ),
            .passThrough
        )
    }
}
