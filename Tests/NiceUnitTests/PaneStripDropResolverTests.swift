//
//  PaneStripDropResolverTests.swift
//  NiceUnitTests
//
//  Pure tests for the pane-strip x-axis slot resolver. Builds a fake
//  pane strip (each pill is a 100pt-wide rect at 28pt tall, laid out
//  end-to-end with no spacing for arithmetic ease) and exercises the
//  resolver against various cursor positions and Claude rules.
//

import CoreGraphics
import Foundation
import XCTest
@testable import Nice

final class PaneStripDropResolverTests: XCTestCase {

    // MARK: - Geometry helpers

    /// Build pane frames for `ids` placed end-to-end at width `w`,
    /// starting at x=0. midX of pill i is `w * i + w/2`.
    private func frames(_ ids: [String], width w: CGFloat = 100) -> [String: CGRect] {
        var out: [String: CGRect] = [:]
        for (i, id) in ids.enumerated() {
            out[id] = CGRect(x: CGFloat(i) * w, y: 0, width: w, height: 28)
        }
        return out
    }

    private func payload(
        kind: PaneKind = .terminal,
        sourceTab: String = "src-tab",
        paneId: String = "src-pane"
    ) -> PaneDragPayload {
        PaneDragPayload(
            windowSessionId: "win",
            tabId: sourceTab, paneId: paneId, kind: kind
        )
    }

    // MARK: - Basic slot computation (cross-tab — no source adjustments)

    func test_resolve_cursorBeforeFirstPill_returnsSlot0() {
        let order = ["a", "b", "c"]
        let outcome = PaneStripDropResolver.resolve(
            payload: payload(),
            destTabId: "dst",
            destPaneOrder: order,
            destHasClaudeAtZero: false,
            cursorX: -10,
            paneFrames: frames(order)
        )
        XCTAssertEqual(outcome?.visualSlot, 0)
        XCTAssertEqual(outcome?.finalIndex, 0)
    }

    func test_resolve_cursorAfterLastPill_returnsSlotN() {
        let order = ["a", "b", "c"]
        let outcome = PaneStripDropResolver.resolve(
            payload: payload(),
            destTabId: "dst",
            destPaneOrder: order,
            destHasClaudeAtZero: false,
            cursorX: 999,
            paneFrames: frames(order)
        )
        XCTAssertEqual(outcome?.visualSlot, 3)
        XCTAssertEqual(outcome?.finalIndex, 3)
    }

    func test_resolve_cursorOnLeftHalfOfPill_returnsSlotBeforeIt() {
        let order = ["a", "b", "c"]
        // Pill b: x=[100,200], midX=150.
        let outcome = PaneStripDropResolver.resolve(
            payload: payload(),
            destTabId: "dst",
            destPaneOrder: order,
            destHasClaudeAtZero: false,
            cursorX: 120,
            paneFrames: frames(order)
        )
        XCTAssertEqual(outcome?.visualSlot, 1)
    }

    func test_resolve_cursorOnRightHalfOfPill_returnsSlotAfterIt() {
        let order = ["a", "b", "c"]
        let outcome = PaneStripDropResolver.resolve(
            payload: payload(),
            destTabId: "dst",
            destPaneOrder: order,
            destHasClaudeAtZero: false,
            cursorX: 180,
            paneFrames: frames(order)
        )
        XCTAssertEqual(outcome?.visualSlot, 2)
    }

    func test_resolve_emptyDestination_returnsSlot0() {
        let outcome = PaneStripDropResolver.resolve(
            payload: payload(),
            destTabId: "dst",
            destPaneOrder: [],
            destHasClaudeAtZero: false,
            cursorX: 50,
            paneFrames: [:]
        )
        XCTAssertEqual(outcome?.visualSlot, 0)
        XCTAssertEqual(outcome?.finalIndex, 0)
    }

    // MARK: - Claude clamp (terminal into Claude tab)

    func test_resolve_terminal_intoClaudeTab_cursorAtStart_clampedTo1() {
        let order = ["claude", "t1", "t2"]
        let outcome = PaneStripDropResolver.resolve(
            payload: payload(kind: .terminal),
            destTabId: "dst",
            destPaneOrder: order,
            destHasClaudeAtZero: true,
            cursorX: 10,           // would be slot 0
            paneFrames: frames(order)
        )
        XCTAssertEqual(outcome?.visualSlot, 1)
        XCTAssertEqual(outcome?.finalIndex, 1)
    }

    func test_resolve_terminal_intoNonClaudeTab_noClamp() {
        let order = ["t0", "t1"]
        let outcome = PaneStripDropResolver.resolve(
            payload: payload(kind: .terminal),
            destTabId: "dst",
            destPaneOrder: order,
            destHasClaudeAtZero: false,
            cursorX: 10,
            paneFrames: frames(order)
        )
        XCTAssertEqual(outcome?.visualSlot, 0)
    }

    // MARK: - Claude reorder rejection

    func test_resolve_claudePayload_inOwnTab_isRejected() {
        let order = ["claude", "t1", "t2"]
        let outcome = PaneStripDropResolver.resolve(
            payload: payload(kind: .claude, sourceTab: "ct", paneId: "claude"),
            destTabId: "ct",       // SAME tab as source
            destPaneOrder: order,
            destHasClaudeAtZero: true,
            cursorX: 250,
            paneFrames: frames(order)
        )
        XCTAssertNil(outcome)
    }

    func test_resolve_claudePayload_inOtherTab_returnsValidSlot() {
        let order = ["t-x", "t-y"]
        // Claude going to a non-own tab — resolver returns a valid
        // slot; the drop delegate routes to absorbAsNewTab regardless.
        let outcome = PaneStripDropResolver.resolve(
            payload: payload(kind: .claude, sourceTab: "src-ct", paneId: "claude"),
            destTabId: "dst",      // DIFFERENT tab
            destPaneOrder: order,
            destHasClaudeAtZero: false,
            cursorX: 10,
            paneFrames: frames(order)
        )
        XCTAssertNotNil(outcome)
    }

    // MARK: - Same-tab no-op detection (source removal adjustment)

    func test_resolve_sameTab_dropOnSelfMidpoint_isNoOp() {
        // [a, b, c]. Drag b. Drop on left half of b → would be slot 1
        // → final position 1 (after removing b at idx 1 in raw, then
        // adjusting). srcIdx == finalIdx → no-op.
        let order = ["a", "b", "c"]
        let outcome = PaneStripDropResolver.resolve(
            payload: payload(sourceTab: "t", paneId: "b"),
            destTabId: "t",
            destPaneOrder: order,
            destHasClaudeAtZero: false,
            cursorX: 120,                  // over b, left half → slot 1
            paneFrames: frames(order)
        )
        XCTAssertNil(outcome)
    }

    func test_resolve_sameTab_dropOnSelfRightHalf_isNoOp() {
        // Drag b, cursor on right half of b → raw slot 2 → adjust for
        // srcIdx (1<2) → final 1 → == srcIdx → no-op.
        let order = ["a", "b", "c"]
        let outcome = PaneStripDropResolver.resolve(
            payload: payload(sourceTab: "t", paneId: "b"),
            destTabId: "t",
            destPaneOrder: order,
            destHasClaudeAtZero: false,
            cursorX: 180,                  // over b, right half
            paneFrames: frames(order)
        )
        XCTAssertNil(outcome)
    }

    func test_resolve_sameTab_moveBackward_finalIndexIsCorrect() {
        // [a, b, c]. Drag c (srcIdx=2). Drop on left half of a (slot
        // 0) → final 0.
        let order = ["a", "b", "c"]
        let outcome = PaneStripDropResolver.resolve(
            payload: payload(sourceTab: "t", paneId: "c"),
            destTabId: "t",
            destPaneOrder: order,
            destHasClaudeAtZero: false,
            cursorX: 10,
            paneFrames: frames(order)
        )
        XCTAssertEqual(outcome?.finalIndex, 0)
        XCTAssertEqual(outcome?.visualSlot, 0)
    }

    func test_resolve_sameTab_moveForward_finalIndexAccountsForRemoval() {
        // [a, b, c, d]. Drag a (srcIdx=0). Drop on right half of c
        // (slot 3) → adjust: srcIdx(0)<3 → final 2.
        let order = ["a", "b", "c", "d"]
        let outcome = PaneStripDropResolver.resolve(
            payload: payload(sourceTab: "t", paneId: "a"),
            destTabId: "t",
            destPaneOrder: order,
            destHasClaudeAtZero: false,
            cursorX: 280,                  // over c, right half → slot 3
            paneFrames: frames(order)
        )
        XCTAssertEqual(outcome?.visualSlot, 3)
        XCTAssertEqual(outcome?.finalIndex, 2)
    }

    func test_resolve_sameTab_appendToEnd_finalIsLastIndex() {
        // [a, b, c]. Drag a. Cursor after last → raw slot 3 → adjust
        // → final 2 (last index).
        let order = ["a", "b", "c"]
        let outcome = PaneStripDropResolver.resolve(
            payload: payload(sourceTab: "t", paneId: "a"),
            destTabId: "t",
            destPaneOrder: order,
            destHasClaudeAtZero: false,
            cursorX: 999,
            paneFrames: frames(order)
        )
        XCTAssertEqual(outcome?.finalIndex, 2)
        XCTAssertEqual(outcome?.visualSlot, 3)
    }
}
