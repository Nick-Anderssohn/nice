//
//  SidebarDropResolverTests.swift
//  NiceUnitTests
//
//  Unit tests for `SidebarDropResolver` — the pure slot-picking
//  logic behind the sidebar's tab-drag indicator. These tests cover
//  the rules the user cares about visually (per-tab midpoint split,
//  header-area → slot 0, trailing-gap → append, no-op suppression)
//  without needing a live SwiftUI drag session.
//

import CoreGraphics
import XCTest
@testable import Nice

@MainActor
final class SidebarDropResolverTests: XCTestCase {

    func test_overFirstTabTopHalf_isBeforeFirstTab() {
        // t0 frame 20..50; top half is y ≤ midpoint (35).
        let outcome = resolve(draggedId: "t2", y: 25)
        XCTAssertEqual(outcome, .init(draggedId: "t2", targetId: "t0", placeAfter: false))
    }

    func test_overFirstTabBottomHalf_isAfterFirstTab() {
        // t0 frame 20..50; bottom half is y > 35.
        let outcome = resolve(draggedId: "t2", y: 45)
        XCTAssertEqual(outcome, .init(draggedId: "t2", targetId: "t0", placeAfter: true))
    }

    func test_aboveFirstTab_headerArea_isBeforeFirstTab() {
        // Cursor above all tab frames (in the project header area)
        // should route to "insert at slot 0 of this project".
        let outcome = resolve(draggedId: "t2", y: -10)
        XCTAssertEqual(outcome, .init(draggedId: "t2", targetId: "t0", placeAfter: false))
    }

    func test_belowLastTab_trailingGap_isAfterLastTab() {
        // Cursor past the last tab's maxY (in the 4pt trailing gap)
        // should route to "append to this project".
        let outcome = resolve(draggedId: "t0", y: 200)
        XCTAssertEqual(outcome, .init(draggedId: "t0", targetId: "t2", placeAfter: true))
    }

    func test_overMiddleTab_splitsOnMidpoint() {
        // t1 frame 50..80; midpoint 65. Top half < 65, bottom half > 65.
        let beforeOutcome = resolve(draggedId: "t2", y: 55)
        XCTAssertEqual(beforeOutcome, .init(draggedId: "t2", targetId: "t1", placeAfter: false))
        let afterOutcome = resolve(draggedId: "t0", y: 75)
        XCTAssertEqual(afterOutcome, .init(draggedId: "t0", targetId: "t1", placeAfter: true))
    }

    func test_adjacentNoOp_returnsNil() {
        // Dragging t1 to "after t0" is a no-op (already there).
        let wouldMoveTab: (String, String, Bool) -> Bool = { src, _, _ in
            // Mimic AppState.wouldMoveTab for [t0, t1, t2] where
            // src=t1 is already at slot 1: all adjacent drops are
            // no-ops.
            if src == "t1" { return false }
            return true
        }
        let outcome = SidebarDropResolver.resolve(
            draggedTabId: "t1",
            location: CGPoint(x: 10, y: 25),
            tabOrder: ["t0", "t1", "t2"],
            tabFrames: threeTabFrames,
            wouldMoveTab: wouldMoveTab
        )
        XCTAssertNil(outcome)
    }

    func test_emptyProject_returnsNil() {
        let outcome = SidebarDropResolver.resolve(
            draggedTabId: "x",
            location: CGPoint(x: 10, y: 10),
            tabOrder: [],
            tabFrames: [:],
            wouldMoveTab: { _, _, _ in true }
        )
        XCTAssertNil(outcome)
    }

    // MARK: - Fixtures

    /// Three 30pt-tall tabs stacked starting at y=20 (i.e., below a
    /// 20pt header). Frames: t0=20…50, t1=50…80, t2=80…110.
    private let threeTabFrames: [String: CGRect] = [
        "t0": CGRect(x: 0, y: 20, width: 200, height: 30),
        "t1": CGRect(x: 0, y: 50, width: 200, height: 30),
        "t2": CGRect(x: 0, y: 80, width: 200, height: 30),
    ]

    /// Helper: resolve a tab drop against the three-tab fixture with
    /// a permissive `wouldMoveTab` (so slot-picking is exercised in
    /// isolation from the no-op predicate).
    private func resolve(
        draggedId: String,
        y: CGFloat
    ) -> SidebarDropResolver.Outcome? {
        SidebarDropResolver.resolve(
            draggedTabId: draggedId,
            location: CGPoint(x: 10, y: y),
            tabOrder: ["t0", "t1", "t2"],
            tabFrames: threeTabFrames,
            wouldMoveTab: { _, _, _ in true }
        )
    }
}
