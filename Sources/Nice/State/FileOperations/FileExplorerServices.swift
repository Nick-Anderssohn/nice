//
//  FileExplorerServices.swift
//  Nice
//
//  Holder for the three pieces an `AppState` needs to drive the
//  file-browser context menu: the pasteboard adapter, the global
//  undo history, and the Open With provider. Bundled into a single
//  value type so production wiring (`NiceServices`) and test setup
//  pass the same shape and the orchestration extension reads from
//  one place.
//

import Foundation

@MainActor
struct FileExplorerServices {
    let pasteboard: FilePasteboardAdapter
    let history: FileOperationHistory
    let openWithProvider: OpenWithProvider
    /// Weak handle to the process-wide window registry. Read by the
    /// rename pre-flight to walk every open window's panes for CWD
    /// invalidation. Held weak because the registry outlives the
    /// services bundle (both are owned by `NiceServices`), but the
    /// references shouldn't be retain cycles. `nil` in unit tests
    /// that don't need cross-window scanning — the orchestrator
    /// treats `nil` as "no panes to invalidate".
    weak var registry: WindowRegistry? = nil

    /// Pure FS worker used for the orchestration's copy/cut/trash
    /// calls. Shared with `history.service` so a fake `FileManager`
    /// or `Trasher` injected via `history.service` reaches every
    /// code path, not just undo/redo.
    var service: FileOperationsService { history.service }
}
