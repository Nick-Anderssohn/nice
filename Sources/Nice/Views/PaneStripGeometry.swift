//
//  PaneStripGeometry.swift
//  Nice
//
//  Pure value type capturing the layout state of the toolbar's pane
//  strip: which pills are where, how wide the visible viewport is, and
//  the derived facts the strip's chrome cares about (overflow, scroll
//  affordances, offscreen pane ids). Lives in its own file so unit
//  tests can exercise the math without spinning up SwiftUI — the
//  `InlinePaneStrip` view in `WindowToolbarView.swift` is a thin shell
//  around this struct.
//
//  Coordinate convention: pill frames are reported in the ScrollView's
//  named coordinate space, where the viewport is fixed at
//  `[0, visibleWidth]` regardless of the current scroll offset. So a
//  pill is fully offscreen iff `frame.maxX <= 0` (scrolled off the
//  leading edge) or `frame.minX >= visibleWidth` (off the trailing
//  edge), and partially clipped iff its frame straddles either edge.
//
//  Why `isOverflowing` is derived from `(maxX - minX)` of all frames
//  instead of `canScrollLeading || canScrollTrailing`: the difference
//  is invariant under scroll (every frame shifts by the same offset,
//  so the span doesn't change), which means it doesn't blink as the
//  user scrolls. The OR-of-edges form is mathematically equivalent
//  *when frames are consistent*, but races where the two booleans flip
//  on different layout passes can briefly leave both false even though
//  the strip is overflowing.
//

import CoreGraphics

struct PaneStripGeometry: Equatable {
    let paneFrames: [String: CGRect]
    let visibleWidth: CGFloat

    /// Tolerance for floating-point rounding in frame math. Sub-pixel
    /// drift below this magnitude does not count as overflowing — keeps
    /// the chevron and edge fades from flickering on layout snaps.
    static let edgeTolerance: CGFloat = 0.5

    /// Total width spanned by the pills, derived as `max(maxX) - min(minX)`
    /// across `paneFrames`. Invariant under scroll because every frame
    /// shifts by the same offset.
    var contentWidth: CGFloat {
        guard !paneFrames.isEmpty else { return 0 }
        let minX = paneFrames.values.map(\.minX).min() ?? 0
        let maxX = paneFrames.values.map(\.maxX).max() ?? 0
        return maxX - minX
    }

    /// Whether the strip's content can't fit in the viewport. Drives
    /// whether the overflow chevron renders. OR of the two edges so that
    /// it remains true when content extends past the trailing edge OR
    /// has been scrolled past the leading edge — equivalent to
    /// `contentWidth > visibleWidth` *if frames are mutually consistent*,
    /// but doesn't blink false in the brief layout pass where SwiftUI
    /// has reported a partial set of frames.
    var isOverflowing: Bool {
        canScrollLeading || canScrollTrailing
    }

    /// Some pill is hidden past the leading edge. Drives the leading
    /// edge fade.
    var canScrollLeading: Bool {
        paneFrames.values.contains { $0.minX < -Self.edgeTolerance }
    }

    /// Some pill is hidden past the trailing edge. Drives the trailing
    /// edge fade.
    var canScrollTrailing: Bool {
        guard visibleWidth > 0 else { return false }
        return paneFrames.values.contains {
            $0.maxX > visibleWidth + Self.edgeTolerance
        }
    }

    /// Pane ids whose frames sit entirely outside the viewport.
    /// Partially-clipped panes are *not* in this set: the user can still
    /// see their pulse / icon and they don't need a separate badge.
    var offscreenPaneIds: Set<String> {
        guard visibleWidth > 0 else { return [] }
        var ids: Set<String> = []
        for (id, frame) in paneFrames {
            if frame.maxX <= Self.edgeTolerance
                || frame.minX >= visibleWidth - Self.edgeTolerance
            {
                ids.insert(id)
            }
        }
        return ids
    }
}
