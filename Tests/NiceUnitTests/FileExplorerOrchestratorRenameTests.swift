//
//  FileExplorerOrchestratorRenameTests.swift
//  NiceUnitTests
//
//  Coverage for `FileExplorerOrchestrator.rename(from:to:)` against a
//  real temp-directory fixture. The orchestrator stitches the rename
//  validator, the CWD-impact pre-flight, the file-ops service, and
//  the undo history together; these tests pin the easy paths
//  (success, collision, invalid input). The CWD-impact alert flow is
//  exercised purely via `FileBrowserCWDImpactCheckTests` since
//  stubbing a real `WindowRegistry` is impractical.
//
//  Pattern mirrors `AppStateFileOperationsTests.makeState()` — a
//  bare `AppState` (services: nil) wired to a private
//  `FileExplorerServices` so the orchestration runs end-to-end
//  against a private temp dir and an in-test pasteboard.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class FileExplorerOrchestratorRenameTests: XCTestCase {

    private var tempDir: URL!
    private var pasteboardName: NSPasteboard.Name!
    private var pasteboard: NSPasteboard!

    override func setUp() {
        super.setUp()
        tempDir = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-rename-orch-\(UUID().uuidString)",
                isDirectory: true
            )
        try? FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
        pasteboardName = NSPasteboard.Name("nice-rename-orch-\(UUID().uuidString)")
        pasteboard = NSPasteboard(name: pasteboardName)
    }

    override func tearDown() {
        if let tempDir { try? FileManager.default.removeItem(at: tempDir) }
        if let pasteboard { pasteboard.releaseGlobally() }
        tempDir = nil
        pasteboard = nil
        pasteboardName = nil
        super.tearDown()
    }

    // MARK: - Success path

    func test_rename_renamesFile_andPushesUndoEntry() {
        let (state, history) = makeState()
        let src = makeFile("foo.txt", body: "data")

        state.fileExplorerOrchestrator.rename(
            from: src.path, to: "bar.txt", originatingTabId: nil
        )

        XCTAssertFalse(fileExists(src))
        XCTAssertTrue(fileExists(tempDir.appendingPathComponent("bar.txt")))
        XCTAssertEqual(history.undoStack.count, 1)
        XCTAssertNil(history.lastDriftMessage)
    }

    func test_rename_undoRestoresOriginalName() {
        let (state, history) = makeState()
        let src = makeFile("foo.txt", body: "data")

        state.fileExplorerOrchestrator.rename(
            from: src.path, to: "bar.txt", originatingTabId: nil
        )
        history.undo()

        XCTAssertTrue(fileExists(src))
        XCTAssertFalse(fileExists(tempDir.appendingPathComponent("bar.txt")))
    }

    func test_rename_renamesDirectory() {
        let (state, _) = makeState()
        let folder = makeDir("oldname")
        FileManager.default.createFile(
            atPath: folder.appendingPathComponent("inside.txt").path,
            contents: Data("hi".utf8)
        )

        state.fileExplorerOrchestrator.rename(
            from: folder.path, to: "newname", originatingTabId: nil
        )

        XCTAssertFalse(fileExists(folder))
        let renamed = tempDir.appendingPathComponent("newname")
        XCTAssertTrue(fileExists(renamed))
        XCTAssertTrue(fileExists(renamed.appendingPathComponent("inside.txt")))
    }

    // MARK: - Drift paths

    func test_rename_collidingDestination_setsDriftMessage_andDoesNotRename() {
        let (state, history) = makeState()
        let src = makeFile("foo.txt", body: "src")
        _ = makeFile("bar.txt", body: "occupied")

        state.fileExplorerOrchestrator.rename(
            from: src.path, to: "bar.txt", originatingTabId: nil
        )

        XCTAssertTrue(fileExists(src), "source must remain when collision blocks rename")
        XCTAssertEqual(
            try? String(contentsOf: tempDir.appendingPathComponent("bar.txt"), encoding: .utf8),
            "occupied",
            "destination must be untouched"
        )
        XCTAssertTrue(history.undoStack.isEmpty)
        XCTAssertNotNil(history.lastDriftMessage)
        // Stable phrasing — pinned via `renameDriftMessage(forUnderlying:newName:)`.
        XCTAssertTrue(
            history.lastDriftMessage?.contains("'bar.txt'") ?? false,
            "drift message must quote the colliding name; got \(history.lastDriftMessage ?? "nil")"
        )
    }

    // MARK: - Pure helper for the drift message

    func test_renameDriftMessage_forFileExistsUnderlying_returnsCleanMessage() {
        let msg = FileExplorerOrchestrator.renameDriftMessage(
            forUnderlying: "The file \"foo.txt\" couldn’t be saved because a file with the same name already exists.",
            newName: "foo.txt"
        )
        XCTAssertEqual(msg, "Couldn't rename: 'foo.txt' already exists.")
    }

    func test_renameDriftMessage_forUnknownUnderlying_passesThrough() {
        let msg = FileExplorerOrchestrator.renameDriftMessage(
            forUnderlying: "Permission denied", newName: "foo.txt"
        )
        XCTAssertEqual(msg, "Rename failed: Permission denied")
    }

    // MARK: - Early-return guards

    func test_rename_emptyName_isNoOp() {
        let (state, history) = makeState()
        let src = makeFile("foo.txt", body: "data")

        state.fileExplorerOrchestrator.rename(
            from: src.path, to: "", originatingTabId: nil
        )
        state.fileExplorerOrchestrator.rename(
            from: src.path, to: "   ", originatingTabId: nil
        )

        XCTAssertTrue(fileExists(src))
        XCTAssertTrue(history.undoStack.isEmpty)
        XCTAssertNil(history.lastDriftMessage)
    }

    func test_rename_slashOrColonInName_isNoOp() {
        let (state, history) = makeState()
        let src = makeFile("foo.txt", body: "data")

        state.fileExplorerOrchestrator.rename(
            from: src.path, to: "bar/baz.txt", originatingTabId: nil
        )
        state.fileExplorerOrchestrator.rename(
            from: src.path, to: "bar:baz.txt", originatingTabId: nil
        )

        XCTAssertTrue(fileExists(src))
        XCTAssertTrue(history.undoStack.isEmpty)
    }

    // MARK: - Focus restoration after rename
    //
    // Regression coverage for "after renaming a file you can't click
    // back into the terminal and type". Root cause: SwiftUI doesn't
    // restore first responder to embedded `NSView`s when a TextField
    // is torn down — same trap the pane pill rename hit. The
    // orchestrator now calls `focusActiveTerminal()` in a `defer`
    // inside `rename(...)` so EVERY exit path restores focus.

    func test_rename_success_callsFocusActiveTerminal() {
        let (state, _) = makeState()
        let src = makeFile("foo.txt", body: "data")
        let before = state.fileExplorerOrchestrator.focusActiveTerminalCallCount

        state.fileExplorerOrchestrator.rename(
            from: src.path, to: "bar.txt", originatingTabId: nil
        )

        XCTAssertEqual(
            state.fileExplorerOrchestrator.focusActiveTerminalCallCount,
            before + 1,
            "Successful rename must hand first responder back to the terminal so the user can keep typing."
        )
    }

    func test_rename_collisionDrift_stillCallsFocusActiveTerminal() {
        // Drift path: the rename field has been torn down by the row
        // BEFORE we get here, so we have to restore focus even though
        // the apply throws.
        let (state, _) = makeState()
        let src = makeFile("foo.txt", body: "src")
        _ = makeFile("bar.txt", body: "occupied")
        let before = state.fileExplorerOrchestrator.focusActiveTerminalCallCount

        state.fileExplorerOrchestrator.rename(
            from: src.path, to: "bar.txt", originatingTabId: nil
        )

        XCTAssertEqual(
            state.fileExplorerOrchestrator.focusActiveTerminalCallCount,
            before + 1
        )
    }

    func test_rename_formatGuardEarlyReturn_stillCallsFocusActiveTerminal() {
        // Even when the rename method bails on the format guard
        // (empty / slash / colon), the row has already torn down the
        // field — focus must be restored.
        let (state, _) = makeState()
        let src = makeFile("foo.txt", body: "data")
        let before = state.fileExplorerOrchestrator.focusActiveTerminalCallCount

        state.fileExplorerOrchestrator.rename(
            from: src.path, to: "", originatingTabId: nil
        )
        state.fileExplorerOrchestrator.rename(
            from: src.path, to: "bad/name.txt", originatingTabId: nil
        )

        XCTAssertEqual(
            state.fileExplorerOrchestrator.focusActiveTerminalCallCount,
            before + 2,
            "Each rename() call must restore focus on its own exit path."
        )
    }

    func test_focusActiveTerminal_withNilSessions_doesNotCrash() {
        // The test state's orchestrator has `weak var sessions = nil`
        // — calling focusActiveTerminal must be a no-op, not a crash.
        let (state, _) = makeState()
        state.fileExplorerOrchestrator.focusActiveTerminal()
        // Survival is the assertion. Counter still increments.
        XCTAssertGreaterThanOrEqual(
            state.fileExplorerOrchestrator.focusActiveTerminalCallCount, 1
        )
    }

    // MARK: - beginRename gate

    func test_beginRename_onFilesystemRoot_doesNotPublishPath() {
        let (state, _) = makeState()
        // Even if a rogue caller invokes beginRename for "/", the
        // orchestrator's `canRename` guard must prevent the
        // pendingRenamePath from being set.
        state.fileExplorerOrchestrator.beginRename(path: "/", tabId: nil)
        XCTAssertNil(state.fileExplorerOrchestrator.pendingRenamePath)
    }

    func test_beginRename_publishesPath_forOrdinaryPath() {
        let (state, _) = makeState()
        state.fileExplorerOrchestrator.beginRename(
            path: "/Users/nick/Projects/foo.txt", tabId: nil
        )
        XCTAssertEqual(
            state.fileExplorerOrchestrator.pendingRenamePath,
            "/Users/nick/Projects/foo.txt"
        )
    }

    // MARK: - Helpers

    /// Build a bare `AppState` wired to a private `FileExplorerServices`
    /// so the orchestrator's rename runs end-to-end against the temp
    /// dir without touching the user's clipboard or real Trash.
    private func makeState() -> (AppState, FileOperationHistory) {
        let pasteboardAdapter = FilePasteboardAdapter(pasteboard: pasteboard)
        let service = FileOperationsService()
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
        return (state, history)
    }

    @discardableResult
    private func makeFile(_ name: String, body: String = "") -> URL {
        let url = tempDir.appendingPathComponent(name)
        FileManager.default.createFile(atPath: url.path, contents: Data(body.utf8))
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
