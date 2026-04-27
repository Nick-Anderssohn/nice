//
//  PaneStripGeometryTests.swift
//  NiceUnitTests
//
//  Tests the pure-Swift overflow / visibility math that drives the
//  toolbar's `InlinePaneStrip` chrome (edge fades, overflow chevron,
//  attention badge). The view itself is exercised by UITests; these
//  tests cover the geometry struct and the `Tab.hasOffscreenAttention`
//  predicate so a regression in the math doesn't have to wait for a
//  full XCUITest run to surface.
//
//  Coordinate convention (see `PaneStripGeometry.swift`): pill frames
//  are reported in the ScrollView's named coordinate space, where the
//  viewport is fixed at `[0, visibleWidth]` regardless of scroll
//  offset. Frames with negative `minX` have scrolled past the leading
//  edge; frames with `maxX > visibleWidth` extend past the trailing
//  edge.
//

import XCTest
@testable import Nice

final class PaneStripGeometryTests: XCTestCase {

    private func rect(x: CGFloat, width: CGFloat) -> CGRect {
        CGRect(x: x, y: 0, width: width, height: 28)
    }

    private func geometry(
        paneFrames: [String: CGRect],
        visibleWidth: CGFloat
    ) -> PaneStripGeometry {
        PaneStripGeometry(paneFrames: paneFrames, visibleWidth: visibleWidth)
    }

    // MARK: - contentWidth

    /// `contentWidth` must be invariant under scroll. The same three
    /// pills, viewed at scroll-zero and after scrolling 80pt right,
    /// report identical `contentWidth`. Drives the chevron's
    /// scroll-stability.
    func test_contentWidth_isInvariantUnderScroll() {
        let atRest = geometry(
            paneFrames: [
                "p1": rect(x: 0,   width: 100),
                "p2": rect(x: 102, width: 100),
                "p3": rect(x: 204, width: 100),
            ],
            visibleWidth: 200
        )

        let scrolled = geometry(
            paneFrames: [
                "p1": rect(x: -80, width: 100),
                "p2": rect(x:  22, width: 100),
                "p3": rect(x: 124, width: 100),
            ],
            visibleWidth: 200
        )

        XCTAssertEqual(atRest.contentWidth, scrolled.contentWidth)
        XCTAssertEqual(atRest.contentWidth, 304)
        XCTAssertTrue(atRest.isOverflowing)
        XCTAssertTrue(scrolled.isOverflowing)
    }

    // MARK: - Overflow detection

    /// Three pills that fit inside the viewport: no overflow, no scroll
    /// affordances, no offscreen panes.
    func test_noOverflow_whenAllPanesFit() {
        let geo = geometry(
            paneFrames: [
                "p1": rect(x: 0,   width: 100),
                "p2": rect(x: 102, width: 100),
                "p3": rect(x: 204, width: 100),
            ],
            visibleWidth: 320
        )

        XCTAssertFalse(geo.isOverflowing)
        XCTAssertFalse(geo.canScrollLeading)
        XCTAssertFalse(geo.canScrollTrailing)
        XCTAssertEqual(geo.offscreenPaneIds, [])
    }

    /// p1 is scrolled past the leading edge; p2/p3 are visible. Only the
    /// leading fade should fire and only p1 is offscreen.
    func test_leadingOnlyOverflow() {
        let geo = geometry(
            paneFrames: [
                "p1": rect(x: -120, width: 100),  // fully past left
                "p2": rect(x: -16,  width: 100),  // partially clipped
                "p3": rect(x: 86,   width: 100),
            ],
            visibleWidth: 200
        )

        XCTAssertTrue(geo.canScrollLeading)
        XCTAssertFalse(geo.canScrollTrailing)
        XCTAssertTrue(geo.isOverflowing)
        XCTAssertEqual(geo.offscreenPaneIds, ["p1"])
    }

    /// p3 extends past the trailing edge; p1/p2 are visible. Trailing
    /// fade only, only p3 is offscreen.
    func test_trailingOnlyOverflow() {
        let geo = geometry(
            paneFrames: [
                "p1": rect(x: 0,   width: 100),
                "p2": rect(x: 102, width: 100),  // partially clipped
                "p3": rect(x: 220, width: 100),  // fully past right
            ],
            visibleWidth: 200
        )

        XCTAssertFalse(geo.canScrollLeading)
        XCTAssertTrue(geo.canScrollTrailing)
        XCTAssertTrue(geo.isOverflowing)
        XCTAssertEqual(geo.offscreenPaneIds, ["p3"])
    }

    /// Active pane in the middle scrolled into view, with hidden panes
    /// on both sides — both fades fire and both offscreen ids surface.
    func test_bothEdgesOverflow() {
        let geo = geometry(
            paneFrames: [
                "p1": rect(x: -130, width: 100),  // off left
                "p2": rect(x: 50,   width: 100),  // visible
                "p3": rect(x: 220,  width: 100),  // off right
            ],
            visibleWidth: 200
        )

        XCTAssertTrue(geo.canScrollLeading)
        XCTAssertTrue(geo.canScrollTrailing)
        XCTAssertEqual(geo.offscreenPaneIds, ["p1", "p3"])
    }

    /// One pane wider than the viewport always overflows; the chevron
    /// should still appear so the user can reach it via the menu.
    func test_singleHugePane_overflows() {
        let geo = geometry(
            paneFrames: ["p1": rect(x: 0, width: 500)],
            visibleWidth: 200
        )

        XCTAssertTrue(geo.isOverflowing)
        XCTAssertTrue(geo.canScrollTrailing)
        XCTAssertEqual(geo.offscreenPaneIds, [])
    }

    // MARK: - Edge cases

    /// Pre-layout the view emits `visibleWidth == 0`. Geometry must stay
    /// quiet in that frame so the chevron / fades don't briefly flash on
    /// initial appearance.
    func test_zeroVisibleWidth_isQuiet() {
        let geo = geometry(
            paneFrames: ["p1": rect(x: 0, width: 100)],
            visibleWidth: 0
        )

        XCTAssertFalse(geo.isOverflowing)
        XCTAssertFalse(geo.canScrollTrailing)
        XCTAssertEqual(geo.offscreenPaneIds, [])
    }

    /// Frames straddling each edge by sub-pixel amounts must not be
    /// classified as offscreen — that's the `edgeTolerance` contract.
    /// Without the tolerance, snapping pills would flicker the chrome
    /// on layout passes.
    func test_subPixelClipping_doesNotCountAsOverflow() {
        let geo = geometry(
            paneFrames: [
                "p1": rect(x: -0.3, width: 100),
                // Content fits the viewport within `edgeTolerance`:
                // contentWidth = 200.1 - (-0.3) = 200.4.
                "p2": rect(x: 99.7, width: 100.4),
            ],
            visibleWidth: 200
        )

        XCTAssertFalse(geo.isOverflowing)
        XCTAssertFalse(geo.canScrollLeading)
        XCTAssertFalse(geo.canScrollTrailing)
        XCTAssertEqual(geo.offscreenPaneIds, [])
    }

    /// No panes at all (e.g. between active-tab swaps) must be safe and
    /// produce no chrome.
    func test_emptyFrames_isQuiet() {
        let geo = geometry(paneFrames: [:], visibleWidth: 400)

        XCTAssertFalse(geo.isOverflowing)
        XCTAssertEqual(geo.contentWidth, 0)
        XCTAssertEqual(geo.offscreenPaneIds, [])
    }

    // MARK: - Tab.hasOffscreenAttention (paired with Pane.needsAttention)

    /// The badge is the union of two facts (geometry × model). Walk
    /// every relevant combination so a regression in either side fails
    /// loudly.
    func test_hasOffscreenAttention_acrossStatusAndVisibility() {
        let panes: [Pane] = [
            // p1: thinking, fully offscreen → triggers
            Pane(id: "p1", title: "A", kind: .claude, status: .thinking),
            // p2: waiting + acknowledged, visible → does not trigger
            Pane(
                id: "p2",
                title: "B",
                kind: .claude,
                status: .waiting,
                waitingAcknowledged: true
            ),
            // p3: waiting + unacknowledged, visible → does not trigger
            //     (visible panes have their own pulsing dot)
            Pane(
                id: "p3",
                title: "C",
                kind: .claude,
                status: .waiting,
                waitingAcknowledged: false
            ),
            // p4: idle, offscreen → does not trigger
            Pane(id: "p4", title: "D", kind: .claude, status: .idle),
        ]
        let tab = Tab(
            id: "t",
            title: "T",
            cwd: "/",
            panes: panes,
            activePaneId: "p1"
        )

        // Only p1 and p4 are offscreen here; p1 needs attention, p4
        // doesn't.
        XCTAssertTrue(
            tab.hasOffscreenAttention(offscreenIds: ["p1", "p4"]),
            "Offscreen .thinking pane should light the badge"
        )

        // Same panes, but offscreen set excludes p1 — now nothing
        // offscreen needs attention.
        XCTAssertFalse(
            tab.hasOffscreenAttention(offscreenIds: ["p4"]),
            "Idle offscreen pane alone should not light the badge"
        )

        // p3 is waiting+unacknowledged but visible: should not trigger.
        XCTAssertFalse(
            tab.hasOffscreenAttention(offscreenIds: []),
            "Visible attention-worthy panes do not light the overflow badge"
        )
    }

    /// Acknowledging a pane's waiting state must clear the badge, even
    /// if the pane is still offscreen — the user already saw it.
    func test_hasOffscreenAttention_clearsOnAcknowledge() {
        var pane = Pane(
            id: "p1",
            title: "A",
            kind: .claude,
            status: .waiting,
            waitingAcknowledged: false
        )
        var tab = Tab(
            id: "t",
            title: "T",
            cwd: "/",
            panes: [pane],
            activePaneId: "p1"
        )

        XCTAssertTrue(tab.hasOffscreenAttention(offscreenIds: ["p1"]))

        pane.markAcknowledgedIfWaiting()
        tab.panes = [pane]

        XCTAssertFalse(
            tab.hasOffscreenAttention(offscreenIds: ["p1"]),
            "An acknowledged waiting pane should stop attracting attention"
        )
    }

    // MARK: - Pane.needsAttention

    func test_paneNeedsAttention_perStatus() {
        let thinking = Pane(id: "a", title: "", kind: .claude, status: .thinking)
        XCTAssertTrue(thinking.needsAttention)

        let waitingFresh = Pane(
            id: "b",
            title: "",
            kind: .claude,
            status: .waiting,
            waitingAcknowledged: false
        )
        XCTAssertTrue(waitingFresh.needsAttention)

        let waitingAck = Pane(
            id: "c",
            title: "",
            kind: .claude,
            status: .waiting,
            waitingAcknowledged: true
        )
        XCTAssertFalse(waitingAck.needsAttention)

        let idle = Pane(id: "d", title: "", kind: .claude, status: .idle)
        XCTAssertFalse(idle.needsAttention)
    }
}
