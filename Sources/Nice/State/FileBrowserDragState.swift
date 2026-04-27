//
//  FileBrowserDragState.swift
//  Nice
//
//  Ephemeral, view-layer state for an in-flight file-browser drag.
//  Owned by `FileBrowserContent` via `@State` and propagated to the
//  row subtree via `.environment(_:)` — same pattern as
//  `SidebarDragState`. Off `AppState` deliberately so transient drag
//  scratchpads don't show up in the persistent model.
//
//  The drop delegate writes `targetPath` on enter / update / exit so
//  the destination folder row's hover-style highlight reacts without
//  the source row needing to know which folder is hot.
//

import Foundation

/// Whether a drop should move or copy. Resolved per-drop from
/// modifier flags + same/cross-volume comparison; see
/// `FileBrowserDropResolver.operation(...)`.
enum FileDragOperation: Equatable, Sendable {
    case move
    case copy
}

/// One in-flight file-browser drag, start to finish. `paths` is the
/// snapshot of source paths (single row, or the whole selection if
/// the drag started on a selected row). `targetPath` is the directory
/// row currently hovered, or nil when the cursor is outside any
/// folder row — drives the destination's drop-highlight.
struct FileBrowserDragSession: Equatable {
    let paths: [String]
    var targetPath: String?
}

@MainActor
@Observable
final class FileBrowserDragState {
    var session: FileBrowserDragSession?
}
