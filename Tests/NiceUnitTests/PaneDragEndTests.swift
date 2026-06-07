//
//  PaneDragEndTests.swift
//  NiceUnitTests
//
//  Unit tests for `PaneDragEnd.outcome(...)` — the pure classification
//  factored out of `PaneDragSource.Coordinator.draggingSession(_:endedAt:
//  operation:)`. The real `NSDraggingSource` callback can only be reached
//  headlessly with a live drag session + real screen point, so the
//  load-bearing decision (tear-off vs snap-back vs already-handled) lives
//  in this pure function instead, where all three arms + the geometry
//  edges are cheap to pin.
//
//  Coordinate space matches production: global Cocoa screen coordinates
//  (origin bottom-left, y up). `NSRect.contains` treats the min edges as
//  inside and the max edges as outside.
//

import AppKit
import XCTest
@testable import Nice

final class PaneDragEndTests: XCTestCase {

    private let windowA = NSRect(x: 0, y: 0, width: 800, height: 600)
    private let windowB = NSRect(x: 1000, y: 0, width: 800, height: 600)
    private let screen = NSRect(x: 0, y: 0, width: 2560, height: 1440)

    // MARK: - .ignore (a drop target accepted the drag)

    func testAcceptedElsewhereIgnores() {
        // operation == .move means a `.onDrop` strip (reorder or
        // cross-window) already claimed/withdrew the handle. Even if the
        // release point is over empty desktop, we must NOT tear off.
        let outcome = PaneDragEnd.outcome(
            operation: .move,
            screenPoint: NSPoint(x: 2400, y: 1200), // empty desktop
            contentWindowFrames: [windowA, windowB],
            screenFrames: [screen]
        )
        XCTAssertEqual(outcome, .ignore)
    }

    func testCopyOperationAlsoIgnores() {
        // Any non-empty operation mask is "accepted somewhere".
        let outcome = PaneDragEnd.outcome(
            operation: .copy,
            screenPoint: NSPoint(x: 400, y: 300),
            contentWindowFrames: [windowA],
            screenFrames: [screen]
        )
        XCTAssertEqual(outcome, .ignore)
    }

    // MARK: - .tearOff (operation == [] over empty desktop)

    func testReleaseOnEmptyDesktopTearsOff() {
        let outcome = PaneDragEnd.outcome(
            operation: [],
            screenPoint: NSPoint(x: 900, y: 300), // gap between A and B, on screen
            contentWindowFrames: [windowA, windowB],
            screenFrames: [screen]
        )
        XCTAssertEqual(outcome, .tearOff)
    }

    func testReleaseOutsideTheOnlyWindowTearsOff() {
        let outcome = PaneDragEnd.outcome(
            operation: [],
            screenPoint: NSPoint(x: 1500, y: 700),
            contentWindowFrames: [windowA],
            screenFrames: [screen]
        )
        XCTAssertEqual(outcome, .tearOff)
    }

    func testNoContentWindowsTearsOff() {
        // Degenerate: nothing to land on but the desktop.
        let outcome = PaneDragEnd.outcome(
            operation: [],
            screenPoint: NSPoint(x: 100, y: 100),
            contentWindowFrames: [],
            screenFrames: [screen]
        )
        XCTAssertEqual(outcome, .tearOff)
    }

    // MARK: - .withdraw (operation == [] but not on empty desktop)

    func testReleaseInsideAContentWindowWithdraws() {
        // Released over our own / another window's non-target chrome (the
        // sidebar, terminal body, etc.) — snap the pane back, don't tear.
        let outcome = PaneDragEnd.outcome(
            operation: [],
            screenPoint: NSPoint(x: 400, y: 300), // inside windowA
            contentWindowFrames: [windowA, windowB],
            screenFrames: [screen]
        )
        XCTAssertEqual(outcome, .withdraw)
    }

    func testReleaseInsideSecondWindowWithdraws() {
        let outcome = PaneDragEnd.outcome(
            operation: [],
            screenPoint: NSPoint(x: 1400, y: 300), // inside windowB
            contentWindowFrames: [windowA, windowB],
            screenFrames: [screen]
        )
        XCTAssertEqual(outcome, .withdraw)
    }

    func testReleaseOffAllScreensWithdraws() {
        // A multi-display dead zone (no NSScreen covers the point): never
        // tear off into a place the new window can't be seen.
        let outcome = PaneDragEnd.outcome(
            operation: [],
            screenPoint: NSPoint(x: 5000, y: 5000),
            contentWindowFrames: [windowA],
            screenFrames: [screen]
        )
        XCTAssertEqual(outcome, .withdraw)
    }

    // MARK: - Geometry edges

    func testPointExactlyOnWindowMinEdgeIsInsideSoWithdraws() {
        // NSRect.contains includes the min (bottom-left) edges.
        let outcome = PaneDragEnd.outcome(
            operation: [],
            screenPoint: NSPoint(x: 0, y: 0), // windowA.origin
            contentWindowFrames: [windowA],
            screenFrames: [screen]
        )
        XCTAssertEqual(outcome, .withdraw)
    }

    func testPointExactlyOnWindowMaxEdgeIsOutsideSoTearsOff() {
        // NSRect.contains excludes the max (top-right) edges, so a release
        // exactly on the right/top edge is "outside" the window.
        let outcome = PaneDragEnd.outcome(
            operation: [],
            screenPoint: NSPoint(x: 800, y: 600), // windowA max corner
            contentWindowFrames: [windowA],
            screenFrames: [screen]
        )
        XCTAssertEqual(outcome, .tearOff)
    }

    func testEmptyScreenFramesSkipsTheOnScreenGuard() {
        // With no screen frames supplied (test convenience), the on-screen
        // guard is skipped: a point outside all windows still tears off.
        let outcome = PaneDragEnd.outcome(
            operation: [],
            screenPoint: NSPoint(x: 9999, y: 9999),
            contentWindowFrames: [windowA],
            screenFrames: []
        )
        XCTAssertEqual(outcome, .tearOff)
    }
}
