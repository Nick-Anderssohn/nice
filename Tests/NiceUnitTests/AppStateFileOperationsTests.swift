//
//  AppStateFileOperationsTests.swift
//  NiceUnitTests
//
//  Integration coverage for the file-explorer orchestration on
//  `AppState`. Each test stands up a bare AppState (services: nil)
//  and injects a private `FilePasteboardAdapter` + `FileOperationHistory`
//  via the test seam so the orchestration runs end-to-end against a
//  real temp directory without touching the user's clipboard, real
//  Trash, or `NiceServices` plumbing.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class AppStateFileOperationsTests: XCTestCase {

    private var tempDir: URL!
    private var trashLocation: URL!
    private var pasteboardName: NSPasteboard.Name!
    private var pasteboard: NSPasteboard!

    override func setUp() {
        super.setUp()
        tempDir = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-appstate-fileop-\(UUID().uuidString)",
                isDirectory: true
            )
        try? FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
        trashLocation = tempDir.appendingPathComponent("Trash", isDirectory: true)
        try? FileManager.default.createDirectory(at: trashLocation, withIntermediateDirectories: true)
        pasteboardName = NSPasteboard.Name("nice-appstate-test-\(UUID().uuidString)")
        pasteboard = NSPasteboard(name: pasteboardName)
    }

    override func tearDown() {
        if let tempDir { try? FileManager.default.removeItem(at: tempDir) }
        if let pasteboard { pasteboard.releaseGlobally() }
        tempDir = nil
        trashLocation = nil
        pasteboard = nil
        pasteboardName = nil
        super.tearDown()
    }

    // MARK: - Copy + Paste

    func test_copyAndPaste_copiesIntoTargetFolder() {
        let (state, _, _) = makeState()
        let src = makeFile("a.txt", body: "hi")
        let folder = makeDir("folder")

        state.copyToPasteboard(paths: [src.path])
        state.pasteFromPasteboard(into: folder, originatingTabId: nil)

        XCTAssertTrue(fileExists(folder.appendingPathComponent("a.txt")))
        XCTAssertTrue(fileExists(src), "Copy must leave source in place.")
    }

    func test_paste_intoFile_resolvesToParentDirectory() {
        let (state, _, _) = makeState()
        let src = makeFile("a.txt")
        let folder = makeDir("folder")
        let neighbor = makeFileIn(folder, "neighbor.txt")

        state.copyToPasteboard(paths: [src.path])
        // Right-click target is a file inside `folder`; paste should
        // land in `folder` (the file's parent), not next to neighbor.
        state.pasteFromPasteboard(into: neighbor, originatingTabId: nil)

        XCTAssertTrue(fileExists(folder.appendingPathComponent("a.txt")))
    }

    func test_paste_collisionAutoRenamesWithCopySuffix() {
        let (state, _, _) = makeState()
        let src = makeFile("a.txt", body: "alpha")
        let folder = makeDir("folder")
        FileManager.default.createFile(
            atPath: folder.appendingPathComponent("a.txt").path,
            contents: Data("existing".utf8)
        )

        state.copyToPasteboard(paths: [src.path])
        state.pasteFromPasteboard(into: folder, originatingTabId: nil)

        XCTAssertTrue(fileExists(folder.appendingPathComponent("a copy.txt")))
        // Original wasn't clobbered.
        let existing = try? String(
            contentsOf: folder.appendingPathComponent("a.txt"),
            encoding: .utf8
        )
        XCTAssertEqual(existing, "existing")
    }

    // MARK: - Cut + Paste

    func test_cutAndPaste_movesIntoTargetFolder_andClearsCutHighlight() {
        let (state, pasteboard, _) = makeState()
        let src = makeFile("file.txt", body: "x")
        let folder = makeDir("folder")

        state.cutToPasteboard(paths: [src.path])
        XCTAssertTrue(pasteboard.isCut(src))

        state.pasteFromPasteboard(into: folder, originatingTabId: nil)

        XCTAssertTrue(fileExists(folder.appendingPathComponent("file.txt")))
        XCTAssertFalse(fileExists(src), "Cut+paste must move, not copy.")
        XCTAssertFalse(pasteboard.isCut(src),
                       "Cut highlight must clear after a successful paste.")
    }

    // MARK: - Trash + undo

    func test_trash_movesToFakeTrash_andUndoRestores() {
        let (state, _, history) = makeState()
        let src = makeFile("delete.txt", body: "data")

        state.trash(paths: [src.path], originatingTabId: nil)

        XCTAssertFalse(fileExists(src))
        XCTAssertEqual(history.undoStack.count, 1)

        state.undoFileOperation()

        XCTAssertTrue(fileExists(src))
        XCTAssertEqual(history.undoStack.count, 0)
        XCTAssertEqual(history.redoStack.count, 1)
    }

    func test_undo_afterCopy_deletesCopiedFile() {
        let (state, _, history) = makeState()
        let src = makeFile("a.txt")
        let folder = makeDir("folder")

        state.copyToPasteboard(paths: [src.path])
        state.pasteFromPasteboard(into: folder, originatingTabId: nil)
        XCTAssertTrue(fileExists(folder.appendingPathComponent("a.txt")))

        state.undoFileOperation()

        XCTAssertFalse(fileExists(folder.appendingPathComponent("a.txt")))
        XCTAssertTrue(fileExists(src))
        XCTAssertEqual(history.redoStack.count, 1)
    }

    func test_undo_afterMove_restoresOriginalLocation() {
        let (state, _, _) = makeState()
        let src = makeFile("file.txt", body: "data")
        let folder = makeDir("folder")

        state.cutToPasteboard(paths: [src.path])
        state.pasteFromPasteboard(into: folder, originatingTabId: nil)
        XCTAssertFalse(fileExists(src))

        state.undoFileOperation()

        XCTAssertTrue(fileExists(src))
        XCTAssertFalse(fileExists(folder.appendingPathComponent("file.txt")))
    }

    func test_redo_afterUndo_replaysOperation() {
        let (state, _, _) = makeState()
        let src = makeFile("file.txt")

        state.trash(paths: [src.path], originatingTabId: nil)
        XCTAssertFalse(fileExists(src))
        state.undoFileOperation()
        XCTAssertTrue(fileExists(src))

        state.redoFileOperation()

        XCTAssertFalse(fileExists(src))
    }

    func test_undo_afterIndependentManualMutation_publishesDriftMessage() {
        let (state, _, history) = makeState()
        let src = makeFile("file.txt", body: "data")
        let folder = makeDir("folder")
        state.cutToPasteboard(paths: [src.path])
        state.pasteFromPasteboard(into: folder, originatingTabId: nil)
        // User deletes the moved file via Finder before pressing ⌘Z.
        try? FileManager.default.removeItem(at: folder.appendingPathComponent("file.txt"))

        state.undoFileOperation()

        XCTAssertNotNil(history.lastDriftMessage)
    }

    // MARK: - Pasteboard query

    func test_canPaste_falseWhenEmpty_trueAfterCopy() {
        let (state, _, _) = makeState()
        XCTAssertFalse(state.canPaste())

        let src = makeFile("a.txt")
        state.copyToPasteboard(paths: [src.path])

        XCTAssertTrue(state.canPaste())
    }

    // MARK: - Same-parent paste edge cases

    func test_pasteCopy_intoSameParent_appendsCopySuffix() {
        let (state, _, _) = makeState()
        let src = makeFile("bar.txt", body: "data")
        // Copy into the same parent directory — should auto-rename
        // rather than fail or overwrite.
        state.copyToPasteboard(paths: [src.path])
        state.pasteFromPasteboard(into: tempDir, originatingTabId: nil)

        XCTAssertTrue(fileExists(tempDir.appendingPathComponent("bar copy.txt")))
        XCTAssertTrue(fileExists(src), "Source must remain in place after copy.")
    }

    func test_pasteCut_intoSameParent_renamesInPlace() {
        let (state, _, _) = makeState()
        let src = makeFile("bar.txt", body: "data")

        state.cutToPasteboard(paths: [src.path])
        state.pasteFromPasteboard(into: tempDir, originatingTabId: nil)

        // Move into self auto-renames; the original file is moved
        // (not duplicated) so the destination is `bar copy.txt`
        // and the source path is gone.
        XCTAssertTrue(fileExists(tempDir.appendingPathComponent("bar copy.txt")))
        XCTAssertFalse(fileExists(src),
                       "Cut+paste-into-same-parent must move (not copy).")
    }

    // MARK: - Recursive directory cut+paste

    func test_cutAndPaste_directoryWithChildren_movesEntireTree_undoRestores() {
        let (state, _, _) = makeState()
        let folder = makeDir("project")
        let nested = folder.appendingPathComponent("nested.txt")
        FileManager.default.createFile(atPath: nested.path, contents: Data("data".utf8))
        let outer = makeDir("outer")

        state.cutToPasteboard(paths: [folder.path])
        state.pasteFromPasteboard(into: outer, originatingTabId: nil)

        XCTAssertFalse(fileExists(folder))
        let movedNested = outer.appendingPathComponent("project/nested.txt")
        XCTAssertTrue(fileExists(movedNested))

        state.undoFileOperation()

        XCTAssertTrue(fileExists(nested),
                      "Undoing a directory move must restore the whole tree.")
        XCTAssertFalse(fileExists(movedNested))
    }

    // MARK: - copyPathsToPasteboard via adapter

    func test_copyPathsToPasteboard_writesNewlineSeparatedPaths() {
        let (state, _, _) = makeState()

        state.copyPathsToPasteboard(["/a/b.txt", "/a/c.txt"])

        XCTAssertEqual(pasteboard.string(forType: .string), "/a/b.txt\n/a/c.txt")
    }

    // MARK: - Origin

    func test_paste_originIncludesActiveTab() {
        let (state, _, history) = makeState()
        let src = makeFile("a.txt")
        let folder = makeDir("folder")

        state.copyToPasteboard(paths: [src.path])
        state.pasteFromPasteboard(into: folder, originatingTabId: "explicit-tab")

        XCTAssertEqual(history.undoStack.last?.origin.tabId, "explicit-tab")
        XCTAssertEqual(history.undoStack.last?.origin.windowSessionId,
                       state.windowSessionId)
    }

    // MARK: - Drag-and-drop moveOrCopy

    func test_moveOrCopy_move_movesFileAndPushesUndo() {
        let (state, _, history) = makeState()
        let src = makeFile("file.txt", body: "data")
        let folder = makeDir("folder")

        state.moveOrCopy(
            urls: [src],
            into: folder,
            operation: .move,
            originatingTabId: nil
        )

        XCTAssertTrue(fileExists(folder.appendingPathComponent("file.txt")))
        XCTAssertFalse(fileExists(src), "Move must remove the source.")
        XCTAssertEqual(history.undoStack.count, 1)
    }

    func test_moveOrCopy_copy_leavesSourceAndPushesUndo() {
        let (state, _, history) = makeState()
        let src = makeFile("file.txt", body: "data")
        let folder = makeDir("folder")

        state.moveOrCopy(
            urls: [src],
            into: folder,
            operation: .copy,
            originatingTabId: nil
        )

        XCTAssertTrue(fileExists(folder.appendingPathComponent("file.txt")))
        XCTAssertTrue(fileExists(src), "Copy must leave the source in place.")
        XCTAssertEqual(history.undoStack.count, 1)
    }

    func test_moveOrCopy_emptyUrls_isNoOp() {
        let (state, _, history) = makeState()
        let folder = makeDir("folder")

        state.moveOrCopy(
            urls: [],
            into: folder,
            operation: .move,
            originatingTabId: nil
        )

        XCTAssertEqual(
            history.undoStack.count, 0,
            "Empty drag must not push a no-op operation onto undo."
        )
    }

    func test_moveOrCopy_collisionAutoRenamesWithCopySuffix() {
        let (state, _, _) = makeState()
        let src = makeFile("a.txt", body: "alpha")
        let folder = makeDir("folder")
        FileManager.default.createFile(
            atPath: folder.appendingPathComponent("a.txt").path,
            contents: Data("existing".utf8)
        )

        state.moveOrCopy(
            urls: [src],
            into: folder,
            operation: .copy,
            originatingTabId: nil
        )

        XCTAssertTrue(fileExists(folder.appendingPathComponent("a copy.txt")))
        let existing = try? String(
            contentsOf: folder.appendingPathComponent("a.txt"),
            encoding: .utf8
        )
        XCTAssertEqual(
            existing, "existing",
            "Auto-rename must not clobber the existing destination file."
        )
    }

    func test_moveOrCopy_undoAfterMove_restoresOriginalLocation() {
        let (state, _, history) = makeState()
        let src = makeFile("file.txt", body: "data")
        let folder = makeDir("folder")

        state.moveOrCopy(
            urls: [src],
            into: folder,
            operation: .move,
            originatingTabId: nil
        )
        XCTAssertFalse(fileExists(src))

        state.undoFileOperation()

        XCTAssertTrue(fileExists(src))
        XCTAssertFalse(fileExists(folder.appendingPathComponent("file.txt")))
        XCTAssertEqual(history.redoStack.count, 1)
    }

    func test_moveOrCopy_undoAfterCopy_deletesCopiedFile() {
        let (state, _, history) = makeState()
        let src = makeFile("file.txt", body: "data")
        let folder = makeDir("folder")

        state.moveOrCopy(
            urls: [src],
            into: folder,
            operation: .copy,
            originatingTabId: nil
        )
        XCTAssertTrue(fileExists(folder.appendingPathComponent("file.txt")))

        state.undoFileOperation()

        XCTAssertTrue(fileExists(src), "Copy undo must leave the source.")
        XCTAssertFalse(fileExists(folder.appendingPathComponent("file.txt")))
        XCTAssertEqual(history.redoStack.count, 1)
    }

    func test_moveOrCopy_sourceMissing_publishesDriftMessage_andSkipsHistory() {
        let (state, _, history) = makeState()
        let folder = makeDir("folder")
        let ghost = tempDir.appendingPathComponent("ghost.txt")

        state.moveOrCopy(
            urls: [ghost],
            into: folder,
            operation: .move,
            originatingTabId: nil
        )

        XCTAssertNotNil(history.lastDriftMessage)
        XCTAssertTrue(
            history.lastDriftMessage?.contains("ghost.txt") ?? false,
            "Drift message should name the missing file: \(history.lastDriftMessage ?? "<nil>")"
        )
        XCTAssertEqual(
            history.undoStack.count, 0,
            "Failed op must not push onto undo."
        )
    }

    func test_moveOrCopy_originIncludesPassedTabId() {
        let (state, _, history) = makeState()
        let src = makeFile("file.txt")
        let folder = makeDir("folder")

        state.moveOrCopy(
            urls: [src],
            into: folder,
            operation: .move,
            originatingTabId: "drag-source-tab"
        )

        XCTAssertEqual(history.undoStack.last?.origin.tabId, "drag-source-tab")
        XCTAssertEqual(
            history.undoStack.last?.origin.windowSessionId,
            state.windowSessionId
        )
    }

    func test_moveOrCopy_nilTabId_fallsBackToActiveTab() {
        let (state, _, history) = makeState()
        let src = makeFile("file.txt")
        let folder = makeDir("folder")
        let activeId = state.activeTabId

        state.moveOrCopy(
            urls: [src],
            into: folder,
            operation: .move,
            originatingTabId: nil
        )

        XCTAssertEqual(
            history.undoStack.last?.origin.tabId, activeId,
            "Without an explicit tab id, origin should fall back to the active tab."
        )
    }

    func test_moveOrCopy_directoryWithChildren_movesEntireTree_undoRestores() {
        let (state, _, _) = makeState()
        let folder = makeDir("project")
        let nested = folder.appendingPathComponent("nested.txt")
        FileManager.default.createFile(
            atPath: nested.path,
            contents: Data("data".utf8)
        )
        let outer = makeDir("outer")

        state.moveOrCopy(
            urls: [folder],
            into: outer,
            operation: .move,
            originatingTabId: nil
        )

        XCTAssertFalse(fileExists(folder))
        let movedNested = outer.appendingPathComponent("project/nested.txt")
        XCTAssertTrue(fileExists(movedNested))

        state.undoFileOperation()

        XCTAssertTrue(
            fileExists(nested),
            "Undoing a directory move must restore the whole tree."
        )
        XCTAssertFalse(fileExists(movedNested))
    }

    // MARK: - Helpers

    /// Build a bare AppState (services: nil) wired to a private
    /// FileExplorerServices triple that exercises orchestration
    /// paths against a private pasteboard + fake trasher without
    /// any of NiceServices' heavy plumbing.
    private func makeState() -> (
        AppState,
        FilePasteboardAdapter,
        FileOperationHistory
    ) {
        let pasteboardAdapter = FilePasteboardAdapter(pasteboard: pasteboard)
        let service = FileOperationsService(trasher: FakeTrasher(trashRoot: trashLocation))
        let history = FileOperationHistory(service: service, registry: nil)
        let explorer = FileExplorerServices(
            pasteboard: pasteboardAdapter,
            history: history,
            openWithProvider: OpenWithProvider()
        )
        let state = AppState(
            services: nil,
            initialSidebarCollapsed: false,
            initialSidebarMode: .files,
            initialMainCwd: tempDir.path,
            windowSessionId: "test-window-\(UUID().uuidString)",
            fileExplorer: explorer
        )
        return (state, pasteboardAdapter, history)
    }

    @discardableResult
    private func makeFile(_ name: String, body: String = "") -> URL {
        let url = tempDir.appendingPathComponent(name)
        FileManager.default.createFile(atPath: url.path, contents: Data(body.utf8))
        return url
    }

    @discardableResult
    private func makeFileIn(_ dir: URL, _ name: String) -> URL {
        let url = dir.appendingPathComponent(name)
        FileManager.default.createFile(atPath: url.path, contents: Data())
        return url
    }

    @discardableResult
    private func makeDir(_ name: String) -> URL {
        let url = tempDir.appendingPathComponent(name, isDirectory: true)
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    private func fileExists(_ url: URL) -> Bool {
        FileManager.default.fileExists(atPath: url.path)
    }
}
