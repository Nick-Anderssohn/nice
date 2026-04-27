//
//  PaneStripOverflowEstimator.swift
//  Nice
//
//  Decides whether the toolbar's overflow chevron should render, given
//  `InlinePaneStrip`'s outer bounds and the active tab's panes.
//
//  Background: the obvious approach — observe per-pill frames inside
//  the strip's `ScrollView` and check if any extend past the visible
//  area — fails because SwiftUI virtualizes off-screen content. Pills
//  outside the viewport simply stop emitting `GeometryReader`
//  preferences, so any overflow check derived from per-pill frames
//  silently goes false at scroll-zero or before the user has scrolled
//  enough to bring every pill into view.
//
//  This estimator sidesteps the problem by computing both sides of the
//  overflow check directly:
//
//    • `availableWidth` is `InlinePaneStrip`'s OWN bounds, measured
//      *outside* the `ScrollView` where preferences propagate normally.
//    • The content width is estimated by summing each pill's expected
//      rendered width, derived from its title text via AppKit's
//      attributed-string measurement. The pill chrome (icon, padding,
//      reserved-but-may-be-hidden close button) is a known constant
//      from `InlinePanePill` (see `WindowToolbarView.swift`).
//
//  The check always reserves space for the chevron and the trailing
//  "+" button regardless of whether the chevron is currently shown,
//  which avoids a feedback loop: showing the chevron shrinks the
//  strip, which would otherwise nudge it back to "fits" and re-hide
//  the chevron.
//
//  Cost: O(panes.count) per evaluation; AppKit text measurement is
//  microseconds per call. Negligible for the typical handful-to-few-
//  dozen panes on a tab.
//

import AppKit
import CoreGraphics

enum PaneStripOverflowEstimator {

    // MARK: - Pill metrics (matches `InlinePanePill` in WindowToolbarView.swift)

    /// Sum of fixed chrome inside a pill: leading padding (10) + icon
    /// (12) + spacing (7) before text + spacing (7) after text + close
    /// button (16, slot is reserved even when hidden) + trailing
    /// padding (6).
    static let pillChromeWidth: CGFloat = 10 + 12 + 7 + 7 + 16 + 6

    /// `InlinePanePill` clamps its width with `.frame(maxWidth: 220)`.
    static let pillMaxWidth: CGFloat = 220

    /// Spacing between pills inside the inner HStack (matches the
    /// `HStack(spacing: 2)` in `strip(for:)`).
    static let pillSpacing: CGFloat = 2

    /// Width consumed by the chevron's slot (button + the
    /// `.padding(.leading, 4)` in front + the HStack's 2pt spacing).
    /// Reserved unconditionally so the predicate has no feedback loop.
    static let chevronSlotWidth: CGFloat = 22 + 4 + 2

    /// Width consumed by the trailing "+" button's slot, same shape
    /// as the chevron's.
    static let newTabSlotWidth: CGFloat = 22 + 4 + 2

    // MARK: - Width estimation

    /// Font used to render pill titles. Inactive pills use `.medium`,
    /// active uses `.semibold` — `.medium` is the common case and
    /// the difference at 12pt is sub-pixel. Built per call rather
    /// than as a static so we don't have to wrestle with `NSFont`'s
    /// `Sendable` story under strict concurrency; `systemFont(ofSize:
    /// weight:)` is internally cached, so repeated calls are cheap.
    private static func pillFont() -> NSFont {
        .systemFont(ofSize: 12, weight: .medium)
    }

    /// Estimated rendered width of a single pill given its pane's
    /// title. Capped at `pillMaxWidth` to mirror the SwiftUI clamp.
    static func estimatedPillWidth(for pane: Pane) -> CGFloat {
        let title = NSAttributedString(
            string: pane.title,
            attributes: [.font: pillFont()]
        )
        let text = ceil(title.size().width)
        let total = pillChromeWidth + text
        return min(pillMaxWidth, total)
    }

    /// Sum of estimated pill widths plus the inter-pill spacing.
    static func estimatedContentWidth(for panes: [Pane]) -> CGFloat {
        guard !panes.isEmpty else { return 0 }
        let pills = panes.reduce(into: CGFloat(0)) {
            $0 += estimatedPillWidth(for: $1)
        }
        let gaps = CGFloat(panes.count - 1) * pillSpacing
        return pills + gaps
    }

    // MARK: - Predicate

    /// Whether the chevron should render. False with fewer than two
    /// panes (the menu is pointless with only one pill) or when the
    /// available width hasn't been measured yet (avoid flashing on
    /// first appearance).
    static func shouldShowChevron(
        panes: [Pane],
        availableWidth: CGFloat
    ) -> Bool {
        guard panes.count >= 2 else { return false }
        guard availableWidth > 0 else { return false }
        let strip = availableWidth - chevronSlotWidth - newTabSlotWidth
        return estimatedContentWidth(for: panes) > strip
    }
}
