//
//  PaneStripDropResolverTests.swift
//  NiceUnitTests
//
//  Unit tests for `PaneStripDropResolver` — the pure slot-picking
//  logic behind the top-bar pane-strip drag indicator. These tests
//  cover the rules the user cares about visually (per-pill midpoint
//  split, left-of-first → slot 0, right-of-last → append, no-op
//  suppression) without needing a live SwiftUI drag session.
//

import CoreGraphics
import XCTest
@testable import Nice

@MainActor
final class PaneStripDropResolverTests: XCTestCase {

    func test_overFirstPaneLeftHalf_isBeforeFirstPane() {
        // p0 frame x:0…100; left half is x ≤ midX (50).
        let outcome = resolve(draggedId: "p2", x: 25)
        XCTAssertEqual(outcome, .init(draggedId: "p2", destination: .slot(targetId: "p0", placeAfter: false)))
    }

    func test_overFirstPaneRightHalf_isAfterFirstPane() {
        // p0 frame x:0…100; right half is x > 50.
        let outcome = resolve(draggedId: "p2", x: 75)
        XCTAssertEqual(outcome, .init(draggedId: "p2", destination: .slot(targetId: "p0", placeAfter: true)))
    }

    func test_leftOfFirstPane_isBeforeFirstPane() {
        // Cursor left of all pill frames should route to "insert at
        // slot 0 of this strip".
        let outcome = resolve(draggedId: "p2", x: -10)
        XCTAssertEqual(outcome, .init(draggedId: "p2", destination: .slot(targetId: "p0", placeAfter: false)))
    }

    func test_rightOfLastPane_isAfterLastPane() {
        // Cursor past the last pill's maxX should route to "append to
        // this strip".
        let outcome = resolve(draggedId: "p0", x: 350)
        XCTAssertEqual(outcome, .init(draggedId: "p0", destination: .slot(targetId: "p2", placeAfter: true)))
    }

    func test_overMiddlePane_splitsOnMidpoint() {
        // p1 frame x:100…200; midX = 150.
        // Left half (x < 150) → before p1; right half (x > 150) → after p1.
        let beforeOutcome = resolve(draggedId: "p2", x: 120)
        XCTAssertEqual(beforeOutcome, .init(draggedId: "p2", destination: .slot(targetId: "p1", placeAfter: false)))

        let afterOutcome = resolve(draggedId: "p0", x: 180)
        XCTAssertEqual(afterOutcome, .init(draggedId: "p0", destination: .slot(targetId: "p1", placeAfter: true)))
    }

    func test_adjacentNoOp_returnsNil() {
        // Dragging p1 to "after p0" is a no-op (already there).
        let wouldMovePane: (String, String, Bool) -> Bool = { src, _, _ in
            // Mimic TabModel.wouldMovePane for [p0, p1, p2] where
            // src=p1 is already at slot 1: all adjacent drops are
            // no-ops.
            if src == "p1" { return false }
            return true
        }
        let outcome = PaneStripDropResolver.resolve(
            draggedPaneId: "p1",
            location: CGPoint(x: 75, y: 14),
            paneOrder: paneOrder,
            paneFrames: threePaneFrames,
            wouldMovePane: wouldMovePane
        )
        XCTAssertNil(outcome)
    }

    func test_emptyPaneOrder_returnsNil() {
        let outcome = PaneStripDropResolver.resolve(
            draggedPaneId: "x",
            location: CGPoint(x: 50, y: 14),
            paneOrder: [],
            paneFrames: [:],
            wouldMovePane: { _, _, _ in true }
        )
        XCTAssertNil(outcome)
    }

    func test_singlePane_selfDrop_returnsNil() {
        // Only one pane in the strip; dropping it "before itself" is a
        // no-op. wouldMovePane returns false to model the real logic.
        let outcome = PaneStripDropResolver.resolve(
            draggedPaneId: "p0",
            location: CGPoint(x: 25, y: 14),
            paneOrder: ["p0"],
            paneFrames: ["p0": CGRect(x: 0, y: 0, width: 100, height: 28)],
            wouldMovePane: { _, _, _ in false }
        )
        XCTAssertNil(outcome)
    }

    func test_indicator_paneBefore() {
        // When placeAfter == false the indicator should be .paneBefore.
        let outcome = resolve(draggedId: "p2", x: 25)  // left half of p0
        XCTAssertEqual(outcome?.indicator, .paneBefore("p0"))
    }

    func test_indicator_paneAfter() {
        // When placeAfter == true the indicator should be .paneAfter.
        let outcome = resolve(draggedId: "p0", x: 350)  // right of last
        XCTAssertEqual(outcome?.indicator, .paneAfter("p2"))
    }

    // MARK: - Fixtures

    /// Three 100pt-wide pills side by side starting at x=0.
    /// Frames: p0=0…100, p1=100…200, p2=200…300.
    private let threePaneFrames: [String: CGRect] = [
        "p0": CGRect(x: 0,   y: 0, width: 100, height: 28),
        "p1": CGRect(x: 100, y: 0, width: 100, height: 28),
        "p2": CGRect(x: 200, y: 0, width: 100, height: 28),
    ]
    private let paneOrder = ["p0", "p1", "p2"]

    /// Helper: resolve a pane drop against the three-pane fixture with
    /// a permissive `wouldMovePane` (so slot-picking is exercised in
    /// isolation from the no-op predicate).
    private func resolve(
        draggedId: String,
        x: CGFloat
    ) -> PaneStripDropResolver.Outcome? {
        PaneStripDropResolver.resolve(
            draggedPaneId: draggedId,
            location: CGPoint(x: x, y: 14),
            paneOrder: paneOrder,
            paneFrames: threePaneFrames,
            wouldMovePane: { _, _, _ in true }
        )
    }
}
