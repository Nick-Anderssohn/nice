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

    /// Pure FS worker used for the orchestration's copy/cut/trash
    /// calls. Shared with `history.service` so a fake `FileManager`
    /// or `Trasher` injected via `history.service` reaches every
    /// code path, not just undo/redo.
    var service: FileOperationsService { history.service }
}
