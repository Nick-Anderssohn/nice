//
//  SidebarDropResolverTests.swift
//  NiceUnitTests
//
//  Unit tests for `SidebarDropResolver` — the pure slot-picking
//  logic behind the sidebar's drag-reorder indicator. These tests
//  cover the rules the user actually cares about visually
//  (before/after placement, header-vs-tabs split, no-op
//  suppression) without needing a live SwiftUI drag session — the
//  UX regressions this file is guarding against were ones the
//  model-only `AppStateReorderTests` couldn't see.
//

import CoreGraphics
import XCTest
@testable import Nice

@MainActor
final class SidebarDropResolverTests: XCTestCase {

    // MARK: - Tab drops

    func test_tabDrop_overFirstTabTopHalf_isBeforeFirstTab() {
        // t0 frame 20..50; top half is y ≤ midpoint (35).
        let outcome = resolveTab(draggedId: "t2", y: 25)
        XCTAssertEqual(outcome, .tab(draggedId: "t2", targetId: "t0", placeAfter: false))
    }

    func test_tabDrop_overFirstTabBottomHalf_isAfterFirstTab() {
        // t0 frame 20..50; bottom half is y > 35.
        let outcome = resolveTab(draggedId: "t2", y: 45)
        XCTAssertEqual(outcome, .tab(draggedId: "t2", targetId: "t0", placeAfter: true))
    }

    func test_tabDrop_aboveFirstTab_headerArea_isBeforeFirstTab() {
        // Cursor above all tab frames (in the project header area)
        // should route to "insert at slot 0 of this project".
        let outcome = resolveTab(draggedId: "t2", y: -10)
        XCTAssertEqual(outcome, .tab(draggedId: "t2", targetId: "t0", placeAfter: false))
    }

    func test_tabDrop_belowLastTab_trailingGap_isAfterLastTab() {
        // Cursor past the last tab's maxY (in the 4pt trailing gap)
        // should route to "append to this project".
        let outcome = resolveTab(draggedId: "t0", y: 200)
        XCTAssertEqual(outcome, .tab(draggedId: "t0", targetId: "t2", placeAfter: true))
    }

    func test_tabDrop_overMiddleTab_splitsOnMidpoint() {
        // t1 frame 50..80; midpoint 65. Top half < 65, bottom half > 65.
        let beforeOutcome = resolveTab(draggedId: "t2", y: 55)
        XCTAssertEqual(beforeOutcome, .tab(draggedId: "t2", targetId: "t1", placeAfter: false))
        let afterOutcome = resolveTab(draggedId: "t0", y: 75)
        XCTAssertEqual(afterOutcome, .tab(draggedId: "t0", targetId: "t1", placeAfter: true))
    }

    func test_tabDrop_adjacentNoOp_returnsNil() {
        // Dragging t1 to "after t0" is a no-op (already there).
        let wouldMoveTab: (String, String, Bool) -> Bool = { src, _, _ in
            // Mimic AppState.wouldMoveTab for [t0, t1, t2] where
            // src=t1 is already at slot 1: all adjacent drops are
            // no-ops.
            if src == "t1" { return false }
            return true
        }
        let outcome = SidebarDropResolver.resolve(
            payload: .tab("t1"),
            location: CGPoint(x: 10, y: 25),
            targetProjectId: "p1",
            isTerminalsGroup: false,
            headerHeight: 20,
            tabOrder: ["t0", "t1", "t2"],
            tabFrames: threeTabFrames,
            wouldMoveTab: wouldMoveTab,
            wouldMoveProject: { _, _, _ in true }
        )
        XCTAssertNil(outcome)
    }

    func test_tabDrop_emptyProject_returnsNil() {
        let outcome = SidebarDropResolver.resolve(
            payload: .tab("x"),
            location: CGPoint(x: 10, y: 10),
            targetProjectId: "p1",
            isTerminalsGroup: false,
            headerHeight: 20,
            tabOrder: [],
            tabFrames: [:],
            wouldMoveTab: { _, _, _ in true },
            wouldMoveProject: { _, _, _ in true }
        )
        XCTAssertNil(outcome)
    }

    // MARK: - Project drops

    func test_projectDrop_cursorInHeader_isBeforeProject() {
        // Bug 1 guard: anywhere in the header region is "before this
        // project", so the indicator line paints above the header
        // rather than collapsing onto the group's tab-heavy midpoint.
        let outcome = resolveProject(draggedId: "pX", y: 10, headerHeight: 30)
        XCTAssertEqual(outcome, .project(draggedId: "pX", targetId: "p1", placeAfter: false))
        XCTAssertEqual(outcome?.indicator, .projectBefore)
    }

    func test_projectDrop_cursorOverTabs_isAfterProject() {
        // Bug 1 core: cursor over any tab of the target project should
        // land "after this project" — below the last tab — not
        // "before" just because the tabs sit in the group's upper half.
        let outcome = resolveProject(draggedId: "pX", y: 80, headerHeight: 30)
        XCTAssertEqual(outcome, .project(draggedId: "pX", targetId: "p1", placeAfter: true))
        XCTAssertEqual(outcome?.indicator, .projectAfter)
    }

    func test_projectDrop_cursorInTrailingGap_isAfterProject() {
        let outcome = resolveProject(draggedId: "pX", y: 300, headerHeight: 30)
        XCTAssertEqual(outcome, .project(draggedId: "pX", targetId: "p1", placeAfter: true))
    }

    func test_projectDrop_ontoTerminalsGroup_returnsNil() {
        let outcome = SidebarDropResolver.resolve(
            payload: .project("pX"),
            location: CGPoint(x: 10, y: 10),
            targetProjectId: AppState.terminalsProjectId,
            isTerminalsGroup: true,
            headerHeight: 30,
            tabOrder: [],
            tabFrames: [:],
            wouldMoveTab: { _, _, _ in true },
            wouldMoveProject: { _, _, _ in true }
        )
        XCTAssertNil(outcome)
    }

    func test_projectDrop_noOp_returnsNil() {
        let outcome = SidebarDropResolver.resolve(
            payload: .project("pX"),
            location: CGPoint(x: 10, y: 80),
            targetProjectId: "p1",
            isTerminalsGroup: false,
            headerHeight: 30,
            tabOrder: ["t0", "t1", "t2"],
            tabFrames: threeTabFrames,
            wouldMoveTab: { _, _, _ in true },
            wouldMoveProject: { _, _, _ in false }
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

    /// Helper: resolve a tab drop against the three-tab fixture.
    private func resolveTab(
        draggedId: String,
        y: CGFloat,
        line: UInt = #line
    ) -> SidebarDropResolver.Outcome? {
        SidebarDropResolver.resolve(
            payload: .tab(draggedId),
            location: CGPoint(x: 10, y: y),
            targetProjectId: "p1",
            isTerminalsGroup: false,
            headerHeight: 20,
            tabOrder: ["t0", "t1", "t2"],
            tabFrames: threeTabFrames,
            wouldMoveTab: { _, _, _ in true },
            wouldMoveProject: { _, _, _ in true }
        )
    }

    private func resolveProject(
        draggedId: String,
        y: CGFloat,
        headerHeight: CGFloat
    ) -> SidebarDropResolver.Outcome? {
        SidebarDropResolver.resolve(
            payload: .project(draggedId),
            location: CGPoint(x: 10, y: y),
            targetProjectId: "p1",
            isTerminalsGroup: false,
            headerHeight: headerHeight,
            tabOrder: ["t0", "t1", "t2"],
            tabFrames: threeTabFrames,
            wouldMoveTab: { _, _, _ in true },
            wouldMoveProject: { _, _, _ in true }
        )
    }
}
