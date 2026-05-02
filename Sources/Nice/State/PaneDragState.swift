//
//  PaneDragState.swift
//  Nice
//
//  Per-window observable state for an in-flight pane-pill drag. Owned
//  by `AppShellHost` and propagated via `.environment(_:)` so both
//  `WindowToolbarView` (the source pill + the pane-strip drop target)
//  and `SidebarView` (the row drop target) can paint feedback during
//  the same drag.
//
//  Each window has its own instance; for cross-window drops, the
//  destination window's state drives the destination indicator while
//  the source window's state drives the source-pill fade.
//

import Foundation
import Observation

/// Where the cursor currently sits during a pane drag, in the
/// destination window's coordinate model.
enum PaneDropTarget: Equatable {
    /// Pane strip — between two pills (or at an edge). `visualSlot`
    /// is the cursor-based slot in `[0, destPaneCount]` for indicator
    /// rendering; `finalIndex` is the post-move position in the
    /// destination tab's `panes` array.
    case paneStrip(tabId: String, visualSlot: Int, finalIndex: Int)
    /// Sidebar row — drop on a tab row to join (terminal) or to spawn
    /// a new tab (Claude payload).
    case sidebarTabRow(tabId: String)
}

struct PaneDragSession: Equatable {
    let payload: PaneDragPayload
    var target: PaneDropTarget?
    /// Set by a drop delegate's `performDrop` when it accepted the
    /// drag, so `PaneDragSource.draggingSession(_:endedAt:operation:)`
    /// can distinguish "fell into a target" from "tear-off into empty
    /// space".
    var didDropOnTarget: Bool = false
}

@MainActor
@Observable
final class PaneDragState {
    var session: PaneDragSession?
}
