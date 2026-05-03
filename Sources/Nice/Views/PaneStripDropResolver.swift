//
//  PaneStripDropResolver.swift
//  Nice
//
//  Pure x-axis drop-slot picker for pane-pill drags landing on a
//  pane strip. Mirror of `SidebarDropResolver` but horizontal.
//
//  Encapsulates three rules:
//    1. Claude pane reordering within its OWN tab is forbidden
//       (Claude must always be at index 0; reordering moves it).
//    2. Terminal panes cannot land at index 0 in a tab whose first
//       pane is Claude — the resolver silently clamps to ≥1.
//    3. Same-tab drops that would land on the source's existing
//       position are no-ops (suppress the indicator).
//
//  Lives in Views/ because it consumes UI-layer geometry, but is
//  side-effect-free so unit tests can exercise it without spinning
//  up SwiftUI.
//

import CoreGraphics
import Foundation

enum PaneStripDropResolver {
    struct Outcome: Equatable {
        /// Position the dragged pane will end up in the destination
        /// tab's `panes` array AFTER the move completes. Pass to
        /// `TabModel.movePane` / `SessionsModel.adoptPane` as
        /// `insertAt`.
        let finalIndex: Int

        /// Visual insertion slot in `[0, destPaneCount]` — used to
        /// paint the indicator line:
        ///   - `0`: leading edge of the first pill
        ///   - `destPaneCount`: trailing edge of the last pill
        ///   - else: between pill `(visualSlot - 1)` and pill
        ///     `visualSlot`.
        let visualSlot: Int
    }

    /// Resolve the slot a pane drag would land in.
    /// Returns `nil` for invalid or no-op drops:
    ///   - Claude payload reordering inside its own tab.
    ///   - Same-tab terminal drop landing on its current position.
    ///
    /// - Parameters:
    ///   - payload: the in-flight drag payload.
    ///   - destTabId: id of the tab whose pane strip the cursor is over.
    ///   - destPaneOrder: ids of the destination tab's panes in
    ///     display order.
    ///   - destHasClaudeAtZero: true when `destPaneOrder.first` is a
    ///     Claude pane — drives the terminal-clamp rule.
    ///   - cursorX: cursor x in the pane strip's coordinate space.
    ///   - paneFrames: per-pill frames in the same coordinate space.
    static func resolve(
        payload: PaneDragPayload,
        destTabId: String,
        destPaneOrder: [String],
        destHasClaudeAtZero: Bool,
        cursorX: CGFloat,
        paneFrames: [String: CGRect]
    ) -> Outcome? {
        // Rule 1: Claude reorder within its own tab is forbidden.
        if payload.kind == .claude && payload.tabId == destTabId {
            return nil
        }

        let n = destPaneOrder.count
        var visualSlot = computeRawSlot(
            cursorX: cursorX,
            paneOrder: destPaneOrder,
            paneFrames: paneFrames
        )

        // Rule 2: Terminals can't pass a Claude pinned at index 0.
        // (Claude payloads route to absorbAsNewTab, never insert here.)
        if payload.kind == .terminal && destHasClaudeAtZero {
            visualSlot = max(visualSlot, 1)
        }

        let isSameTab = (payload.tabId == destTabId)
        let finalIndex: Int
        if isSameTab {
            // Same-tab: convert raw slot to post-removal position.
            guard let srcIdx = destPaneOrder.firstIndex(of: payload.paneId)
            else { return nil }
            var f = visualSlot
            if srcIdx < f { f -= 1 }
            // Same-tab final position lives in [0, n-1].
            f = max(0, min(f, n - 1))
            // Rule 3: drop on current position is a no-op.
            if f == srcIdx { return nil }
            finalIndex = f
        } else {
            // Cross-tab: raw slot IS the final position (no source
            // removal to account for in destination).
            finalIndex = max(0, min(visualSlot, n))
        }
        return Outcome(finalIndex: finalIndex, visualSlot: visualSlot)
    }

    /// Cursor → raw insertion slot in `[0, paneOrder.count]`.
    /// Pure positional logic, ignoring source identity.
    private static func computeRawSlot(
        cursorX: CGFloat,
        paneOrder: [String],
        paneFrames: [String: CGRect]
    ) -> Int {
        guard !paneOrder.isEmpty else { return 0 }
        if let firstId = paneOrder.first,
           let firstFrame = paneFrames[firstId],
           cursorX < firstFrame.minX {
            return 0
        }
        if let lastId = paneOrder.last,
           let lastFrame = paneFrames[lastId],
           cursorX > lastFrame.maxX {
            return paneOrder.count
        }
        for (idx, id) in paneOrder.enumerated() {
            guard let frame = paneFrames[id] else { continue }
            if cursorX >= frame.minX && cursorX <= frame.maxX {
                return cursorX < frame.midX ? idx : idx + 1
            }
        }
        return paneOrder.count
    }
}
