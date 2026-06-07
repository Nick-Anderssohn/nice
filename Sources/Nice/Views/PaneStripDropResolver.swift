//
//  PaneStripDropResolver.swift
//  Nice
//
//  Pure slot-math resolver for reordering pane pills in the horizontal
//  top-bar strip. Horizontal analog of `SidebarDropResolver` (which
//  works on the y-axis for vertical sidebar tabs).
//

import CoreGraphics

/// Where the drop indicator sits relative to a pane pill. Horizontal
/// analog of the sidebar's `DropIndicator`.
enum PaneDropIndicator: Equatable {
    case paneBefore(String)
    case paneAfter(String)
}

/// The dragged pane's identity AND where it came from. The source
/// window id lets a cross-window / tear-off drop find the origin window
/// (via `WindowRegistry`) to detach the live pane from it.
struct PaneDragOrigin: Equatable {
    let paneId: String
    let sourceTabId: String
    let sourceIndex: Int
    /// `windowSessionId` of the window the drag started in. The drop
    /// side compares it against its own window id to tell an intra-
    /// window reorder from a cross-window move.
    let sourceWindowSessionId: String
}

/// Resolved drop destination. `.slot` is the intra-strip reorder;
/// `.otherWindowStrip` / `.otherWindowNewTab` / `.newWindow` are the
/// cross-window-move and tear-off destinations. Modeling these as enum
/// cases (rather than resolver-signature changes) keeps the slot math
/// pure and the cross-window branching at the call site.
enum PaneDropDestination: Equatable {
    /// Reorder within the current strip, before/after `targetId`.
    case slot(targetId: String, placeAfter: Bool)
    /// Move a terminal pane into another window's tab strip at a slot.
    case otherWindowStrip(windowSessionId: String, tabId: String,
                          targetId: String?, placeAfter: Bool)
    /// Move a Claude pane into another window as a new sidebar tab under
    /// the project matching `projectPath`.
    case otherWindowNewTab(windowSessionId: String, projectPath: String)
    /// Tear off into a brand-new window (kind decides the seeded shape).
    case newWindow
}

/// Pure, side-effect-free slot math for reordering pane pills within a
/// horizontal strip. Mirrors `SidebarDropResolver` but on the x-axis.
enum PaneStripDropResolver {
    struct Outcome: Equatable {
        let draggedId: String
        let destination: PaneDropDestination

        /// The slot's target pane id, or `nil` for non-slot destinations.
        var targetId: String? {
            guard case .slot(let id, _) = destination else { return nil }
            return id
        }

        /// Whether the dragged pane is placed after the target, or `nil`
        /// for non-slot destinations.
        var placeAfter: Bool? {
            guard case .slot(_, let after) = destination else { return nil }
            return after
        }

        /// Drop indicator for the resolved slot, or `nil` for non-slot
        /// destinations.
        var indicator: PaneDropIndicator? {
            guard case .slot(let id, let after) = destination else { return nil }
            return after ? .paneAfter(id) : .paneBefore(id)
        }
    }

    /// Resolve a drag hovering inside the strip into a reorder outcome.
    ///
    /// - Parameters:
    ///   - draggedPaneId: id of the pane being dragged.
    ///   - location: cursor point in the strip's coordinate space.
    ///   - paneOrder: pane ids in display order (left→right).
    ///   - paneFrames: per-pane frames in the strip's coordinate space.
    ///   - wouldMovePane: no-op predicate. Injected so the resolver
    ///     stays pure — callers pass `TabModel.wouldMovePane`. Signature:
    ///     `(draggedId, targetId, placeAfter) -> Bool`.
    static func resolve(
        draggedPaneId: String,
        location: CGPoint,
        paneOrder: [String],
        paneFrames: [String: CGRect],
        wouldMovePane: (String, String, Bool) -> Bool
    ) -> Outcome? {
        guard let (targetId, placeAfter) = paneTarget(
            x: location.x,
            paneOrder: paneOrder,
            paneFrames: paneFrames
        ) else { return nil }
        guard wouldMovePane(draggedPaneId, targetId, placeAfter) else { return nil }
        return Outcome(draggedId: draggedPaneId,
                       destination: .slot(targetId: targetId, placeAfter: placeAfter))
    }

    /// Pick the pane slot a cursor x-coordinate points at within a
    /// horizontal strip: left of the first pill → before it; right of
    /// the last pill → after it; over a pill → midpoint split
    /// (`placeAfter = x > frame.midX`).
    static func paneTarget(
        x: CGFloat,
        paneOrder: [String],
        paneFrames: [String: CGRect]
    ) -> (targetId: String, placeAfter: Bool)? {
        guard !paneOrder.isEmpty else { return nil }
        if let firstId = paneOrder.first, let firstFrame = paneFrames[firstId], x < firstFrame.minX {
            return (firstId, false)
        }
        if let lastId = paneOrder.last, let lastFrame = paneFrames[lastId], x > lastFrame.maxX {
            return (lastId, true)
        }
        for id in paneOrder {
            guard let frame = paneFrames[id] else { continue }
            if x >= frame.minX, x <= frame.maxX {
                return (id, x > frame.midX)
            }
        }
        return nil
    }
}
