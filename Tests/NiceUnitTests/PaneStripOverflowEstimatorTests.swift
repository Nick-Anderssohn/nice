//
//  PaneStripOverflowEstimatorTests.swift
//  NiceUnitTests
//
//  Tests the direct width-based decision that drives the toolbar's
//  overflow chevron. We can't easily unit-test the in-view SwiftUI
//  preference plumbing, but the predicate itself — "does the
//  estimated content width exceed the available width minus reserved
//  chevron / new-tab / spacing slots?" — is pure and deterministic.
//

import XCTest
@testable import Nice

final class PaneStripOverflowEstimatorTests: XCTestCase {

    // MARK: - Pill width estimation

    /// Single pill with no title still has the fixed chrome width
    /// (icon, padding, reserved close-button slot) — never zero.
    func test_estimatedPillWidth_emptyTitle_equalsChrome() {
        let p = Pane(id: "p", title: "", kind: .terminal)
        XCTAssertEqual(
            PaneStripOverflowEstimator.estimatedPillWidth(for: p),
            PaneStripOverflowEstimator.pillChromeWidth
        )
    }

    /// Long titles cap at the SwiftUI `frame(maxWidth: 220)` clamp so
    /// the estimator agrees with what the pill actually renders.
    func test_estimatedPillWidth_longTitle_capsAt220() {
        let p = Pane(
            id: "p",
            title: String(repeating: "M", count: 100),
            kind: .claude
        )
        XCTAssertEqual(
            PaneStripOverflowEstimator.estimatedPillWidth(for: p),
            PaneStripOverflowEstimator.pillMaxWidth
        )
    }

    /// A typical short title ("hi" ≈ 12pt of text) lands somewhere
    /// between the bare chrome and the 220pt cap.
    func test_estimatedPillWidth_shortTitle_isBetweenChromeAndMax() {
        let p = Pane(id: "p", title: "hi", kind: .claude)
        let w = PaneStripOverflowEstimator.estimatedPillWidth(for: p)
        XCTAssertGreaterThan(w, PaneStripOverflowEstimator.pillChromeWidth)
        XCTAssertLessThan(w, PaneStripOverflowEstimator.pillMaxWidth)
    }

    // MARK: - Predicate

    /// Fewer than two panes never triggers the chevron — it'd be a
    /// menu with nothing useful to switch to.
    func test_shouldShowChevron_falseForOneOrZeroPanes() {
        XCTAssertFalse(
            PaneStripOverflowEstimator.shouldShowChevron(
                panes: [],
                availableWidth: 1000
            )
        )
        XCTAssertFalse(
            PaneStripOverflowEstimator.shouldShowChevron(
                panes: [Pane(id: "p", title: "Tab", kind: .claude)],
                availableWidth: 1000
            )
        )
    }

    /// `availableWidth == 0` happens for one frame at startup before
    /// layout has measured the strip. Suppress the chevron during
    /// that window so it doesn't flash on.
    func test_shouldShowChevron_falseWhenAvailableWidthUnmeasured() {
        let panes = (0..<10).map {
            Pane(id: "p\($0)", title: "Long Title \($0)", kind: .claude)
        }
        XCTAssertFalse(
            PaneStripOverflowEstimator.shouldShowChevron(
                panes: panes,
                availableWidth: 0
            )
        )
    }

    /// Two short-title pills in a wide window fit comfortably — no
    /// chevron, no overflow.
    func test_shouldShowChevron_falseWhenContentFitsComfortably() {
        let panes = [
            Pane(id: "p1", title: "a", kind: .claude),
            Pane(id: "p2", title: "b", kind: .terminal),
        ]
        XCTAssertFalse(
            PaneStripOverflowEstimator.shouldShowChevron(
                panes: panes,
                availableWidth: 1200
            )
        )
    }

    /// Many max-title pills forced into a small viewport overflow
    /// reliably — chevron appears.
    func test_shouldShowChevron_trueWhenContentClearlyOverflows() {
        let panes = (0..<10).map {
            Pane(
                id: "p\($0)",
                title: String(repeating: "X", count: 40),
                kind: .claude
            )
        }
        XCTAssertTrue(
            PaneStripOverflowEstimator.shouldShowChevron(
                panes: panes,
                availableWidth: 600
            )
        )
    }

    /// The chevron + new-tab slots are always reserved in the
    /// calculation regardless of whether the chevron is currently
    /// shown — without that reservation, showing the chevron would
    /// shrink the strip and could re-hide the chevron in a feedback
    /// loop. This test pins the contract: a content width that
    /// JUST fits inside `availableWidth - chevron - newTab` is OK,
    /// while exceeding that triggers the chevron.
    func test_shouldShowChevron_reservesChevronAndNewTabSlots() {
        // Estimated width of one "x" pill is `chrome + tiny` < 220.
        let onePillWidth = PaneStripOverflowEstimator.estimatedPillWidth(
            for: Pane(id: "p", title: "x", kind: .claude)
        )
        let panes = [
            Pane(id: "p1", title: "x", kind: .claude),
            Pane(id: "p2", title: "x", kind: .claude),
        ]
        let twoPillsWidth = onePillWidth * 2
            + PaneStripOverflowEstimator.pillSpacing
        let reserved = PaneStripOverflowEstimator.chevronSlotWidth
            + PaneStripOverflowEstimator.newTabSlotWidth

        // Available is just barely enough for the pills + reserved
        // chrome.
        let snug = twoPillsWidth + reserved + 0.5
        XCTAssertFalse(
            PaneStripOverflowEstimator.shouldShowChevron(
                panes: panes,
                availableWidth: snug
            ),
            "When content fits inside (available - reserved chrome), chevron should not show"
        )

        // One pixel less than the snug fit and the predicate flips.
        let tight = twoPillsWidth + reserved - 1
        XCTAssertTrue(
            PaneStripOverflowEstimator.shouldShowChevron(
                panes: panes,
                availableWidth: tight
            ),
            "Removing reserved-chrome space should trigger the chevron"
        )
    }
}
