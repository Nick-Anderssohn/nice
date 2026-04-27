//
//  PaneStripGeometry.swift
//  Nice
//
//  Pure value type capturing the cosmetic chrome of the toolbar's pane
//  strip: which pills are scrolled past which edge of the viewport.
//  Drives the leading/trailing edge fades and the offscreen-pane set
//  used by the overflow chevron's attention badge. The chevron's own
//  existence is decided separately by `PaneStripOverflowEstimator`,
//  which doesn't depend on per-pill frames at all.
//
//  Coordinate convention: pill frames are reported in the ScrollView's
//  named coordinate space, where the viewport is fixed at
//  `[0, visibleWidth]` regardless of the current scroll offset. A
//  pill is fully offscreen iff `frame.maxX <= 0` (scrolled past the
//  leading edge) or `frame.minX >= visibleWidth` (past the trailing
//  edge); partially clipped iff its frame straddles either edge.
//

import CoreGraphics

struct PaneStripGeometry: Equatable {
    let paneFrames: [String: CGRect]
    let visibleWidth: CGFloat

    /// Tolerance for floating-point rounding in frame math. Sub-pixel
    /// drift below this magnitude does not flip an edge — keeps the
    /// fades from flickering on layout snaps.
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
