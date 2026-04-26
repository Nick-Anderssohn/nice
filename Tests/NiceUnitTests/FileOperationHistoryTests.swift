//
//  FileOperationHistoryTests.swift
//  NiceUnitTests
//
//  Coverage for `FileOperationHistory` — the app-wide undo/redo
//  stack that drives ⌘Z / ⌘⇧Z for file operations. Uses a real
//  `FileOperationsService` against a temp directory plus a
//  `FakeTrasher`, with `registry: nil` so the focus-follow behaviour
//  is exercised separately by `CrossWindowUndoTests`.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class FileOperationHistoryTests: XCTestCase {

    private var tempDir: URL!
    private var trashLocation: URL!

    override func setUp() {
        super.setUp()
        tempDir = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-history-test-\(UUID().uuidString)",
                isDirectory: true
            )
        try? FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
        trashLocation = tempDir.appendingPathComponent("Trash", isDirectory: true)
        try? FileManager.default.createDirectory(at: trashLocation, withIntermediateDirectories: true)
    }

    override func tearDown() {
        if let tempDir { try? FileManager.default.removeItem(at: tempDir) }
        tempDir = nil
        trashLocation = nil
        super.tearDown()
    }

    // MARK: - Push

    func test_push_pushesOntoUndoStack_clearsRedo() {
        let history = makeHistory()
        history.push(makeCopyOp())
        // Simulate a redo entry from a previous undo.
        history.undo() // inverse-applied; the redo stack now has an entry

        XCTAssertEqual(history.redoStack.count, 1)

        // New op clears redo.
        history.push(makeCopyOp(name: "second"))

        XCTAssertEqual(history.redoStack.count, 0)
        XCTAssertEqual(history.undoStack.count, 1)
    }

    // MARK: - Undo

    func test_undo_appliesInverse_pushesToRedo() throws {
        let history = makeHistory()
        let src = makeFile("a.txt")
        let dest = makeDir("dest")
        let op = try FileOperationsService().copy(items: [src], into: dest, origin: origin())
        history.push(op)
        // Sanity: copied file exists.
        XCTAssertTrue(fileExists(dest.appendingPathComponent("a.txt")))

        history.undo()

        XCTAssertFalse(fileExists(dest.appendingPathComponent("a.txt")))
        XCTAssertEqual(history.undoStack.count, 0)
        XCTAssertEqual(history.redoStack.count, 1)
    }

    func test_redo_reappliesOriginal_pushesToUndo() throws {
        let history = makeHistory()
        let src = makeFile("a.txt", body: "hi")
        let dest = makeDir("dest")
        let op = try FileOperationsService().move(items: [src], into: dest, origin: origin())
        history.push(op)
        // Undo: file moves back.
        history.undo()
        XCTAssertTrue(fileExists(src))
        XCTAssertFalse(fileExists(dest.appendingPathComponent("a.txt")))

        // Redo: file moves to dest again.
        history.redo()
        XCTAssertFalse(fileExists(src))
        XCTAssertTrue(fileExists(dest.appendingPathComponent("a.txt")))
    }

    func test_undo_emptyStack_isNoOp() {
        let history = makeHistory()
        history.undo()
        XCTAssertNil(history.lastDriftMessage)
    }

    func test_redo_emptyStack_isNoOp() {
        let history = makeHistory()
        history.redo()
        XCTAssertNil(history.lastDriftMessage)
    }

    // MARK: - Drift

    func test_drift_undoCopy_destinationGone_silent() throws {
        let history = makeHistory()
        let src = makeFile("a.txt")
        let dest = makeDir("dest")
        let op = try FileOperationsService().copy(items: [src], into: dest, origin: origin())
        history.push(op)

        // User deleted the copied file via Finder before pressing
        // ⌘Z — undo should still complete (the copy is already
        // gone; treating that as a successful undo is fine).
        try FileManager.default.removeItem(at: dest.appendingPathComponent("a.txt"))

        history.undo()

        // No drift message — the user's manual delete already did
        // the inverse, and the redo stack still gets a record.
        XCTAssertNil(history.lastDriftMessage)
        XCTAssertEqual(history.redoStack.count, 1)
    }

    func test_drift_undoMove_sourceMissing_publishesMessage() throws {
        let history = makeHistory()
        let src = makeFile("a.txt", body: "hi")
        let dest = makeDir("dest")
        let op = try FileOperationsService().move(items: [src], into: dest, origin: origin())
        history.push(op)
        // User deleted the moved file from `dest` via Finder.
        try FileManager.default.removeItem(at: dest.appendingPathComponent("a.txt"))

        history.undo()

        XCTAssertNotNil(history.lastDriftMessage)
        XCTAssertTrue(history.lastDriftMessage!.contains("a.txt"))
        // Drift drops the record — nothing to redo since the move
        // can't be inverted cleanly.
        XCTAssertEqual(history.redoStack.count, 0)
    }

    func test_drift_undoTrash_emptied_publishesMessage() throws {
        let history = makeHistory()
        let src = makeFile("a.txt")
        let trasher = FakeTrasher(trashRoot: trashLocation)
        let op = try FileOperationsService(trasher: trasher).trash(items: [src], origin: origin())
        history.push(op)
        // Empty the trash item out from under us.
        if case let .trash(items, _) = op {
            try FileManager.default.removeItem(at: items[0].trashed)
        }

        history.undo()

        XCTAssertNotNil(history.lastDriftMessage)
        XCTAssertTrue(history.lastDriftMessage!.contains("emptied"))
        XCTAssertEqual(history.redoStack.count, 0,
                       "Drift dropped the record rather than re-pushing to redo.")
    }

    // MARK: - Origin preservation

    func test_lastDriftMessage_overwritesAcrossSuccessiveDrifts() throws {
        let history = makeHistory()

        // First op: a move whose source we manually delete after
        // pushing to force drift on undo.
        let s1 = makeFile("a.txt", body: "1")
        let d1 = makeDir("dest")
        let op1 = try FileOperationsService().move(items: [s1], into: d1, origin: origin())
        history.push(op1)
        try FileManager.default.removeItem(at: d1.appendingPathComponent("a.txt"))
        history.undo()
        let firstMessage = try XCTUnwrap(history.lastDriftMessage)

        // Second op: same shape, different file.
        let s2 = makeFile("b.txt", body: "2")
        let op2 = try FileOperationsService().move(items: [s2], into: d1, origin: origin())
        history.push(op2)
        try FileManager.default.removeItem(at: d1.appendingPathComponent("b.txt"))
        history.undo()
        let secondMessage = try XCTUnwrap(history.lastDriftMessage)

        XCTAssertNotEqual(firstMessage, secondMessage,
                          "Successive drifts must produce distinct messages — second overwrites first.")
        XCTAssertTrue(secondMessage.contains("b.txt"))
    }

    func test_originPreservedThroughUndoRedo() throws {
        let history = makeHistory()
        let src = makeFile("a.txt", body: "hi")
        let dest = makeDir("dest")
        let origin = FileOperationOrigin(windowSessionId: "win-7", tabId: "tab-99")
        let op = try FileOperationsService().move(items: [src], into: dest, origin: origin)
        history.push(op)

        history.undo()
        XCTAssertEqual(history.redoStack.last?.origin, origin)

        history.redo()
        XCTAssertEqual(history.undoStack.last?.origin, origin)
    }

    // MARK: - Helpers

    private func origin(tabId: String? = "tab-1") -> FileOperationOrigin {
        FileOperationOrigin(windowSessionId: "win-1", tabId: tabId)
    }

    private func makeHistory() -> FileOperationHistory {
        FileOperationHistory(
            service: FileOperationsService(trasher: FakeTrasher(trashRoot: trashLocation)),
            registry: nil
        )
    }

    private func makeCopyOp(name: String = "a.txt") -> FileOperation {
        let src = makeFile(name)
        let dest = makeDir("dest-\(UUID().uuidString.prefix(8))")
        let service = FileOperationsService()
        return (try? service.copy(items: [src], into: dest, origin: origin())) ?? .copy(items: [], origin: origin())
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
