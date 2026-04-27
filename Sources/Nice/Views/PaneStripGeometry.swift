//
//  PaneStripGeometry.swift
//  Nice
//
//  Pure value type capturing the layout state of the toolbar's pane
//  strip: which pills are where, how wide the visible viewport is, and
//  the derived facts the strip's chrome cares about (overflow direction,
//  offscreen pane ids). Lives in its own file so unit tests can exercise
//  the math without spinning up SwiftUI — the `InlinePaneStrip` view in
//  `WindowToolbarView.swift` is a thin shell around this struct.
//
//  Coordinate convention: pill frames are measured in the named scroll-
//  view coordinate space, where the viewport is fixed at
//  `[0, visibleWidth]` regardless of the current scroll offset. So a
//  pill is fully offscreen iff `frame.maxX <= 0` (scrolled off the
//  leading edge) or `frame.minX >= visibleWidth` (off the trailing
//  edge), and partially clipped iff its frame straddles either edge.
//

import CoreGraphics

struct PaneStripGeometry: Equatable {
    let paneFrames: [String: CGRect]
    let visibleWidth: CGFloat

    /// Tolerance for floating-point rounding in frame math. Frames that
    /// stray below 0 or past `visibleWidth` by less than this don't
    /// count as overflowing — keeps the chevron and edge fades from
    /// flickering when a pill snaps to a sub-pixel boundary.
    static let edgeTolerance: CGFloat = 0.5

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

    /// The strip can't show every pill in full — drives whether the
    /// overflow chevron renders.
    var isOverflowing: Bool {
        canScrollLeading || canScrollTrailing
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
