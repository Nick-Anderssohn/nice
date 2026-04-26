//
//  KeyboardShortcutMonitorDispatchTests.swift
//  NiceUnitTests
//
//  Coverage for `KeyboardShortcutMonitor.dispatchHistory(action:history:)`
//  — the seam that ⌘Z and ⌘⇧Z reach. Verifies the history is invoked
//  for the right actions and untouched for unrelated ones.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class KeyboardShortcutMonitorDispatchTests: XCTestCase {

    private var tempDir: URL!
    private var trashLocation: URL!

    override func setUp() {
        super.setUp()
        tempDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("nice-monitor-\(UUID().uuidString)", isDirectory: true)
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

    func test_dispatch_undoFileOperationAction_callsHistoryUndo() throws {
        let history = makeHistory()
        let op = try seedTrashOp(history: history)
        XCTAssertEqual(history.undoStack.last?.origin, op.origin)

        let consumed = KeyboardShortcutMonitor.dispatchHistory(
            action: .undoFileOperation,
            history: history
        )

        XCTAssertTrue(consumed)
        XCTAssertEqual(history.undoStack.count, 0,
                       "undoFileOperation must pop the history's undo stack.")
        XCTAssertEqual(history.redoStack.count, 1)
    }

    func test_dispatch_redoFileOperationAction_callsHistoryRedo() throws {
        let history = makeHistory()
        _ = try seedTrashOp(history: history)
        history.undo()
        XCTAssertEqual(history.redoStack.count, 1)

        let consumed = KeyboardShortcutMonitor.dispatchHistory(
            action: .redoFileOperation,
            history: history
        )

        XCTAssertTrue(consumed)
        XCTAssertEqual(history.redoStack.count, 0)
        XCTAssertEqual(history.undoStack.count, 1)
    }

    func test_dispatch_otherAction_doesNotTouchHistory() throws {
        let history = makeHistory()
        _ = try seedTrashOp(history: history)
        let undoBefore = history.undoStack.count
        let redoBefore = history.redoStack.count

        let consumed = KeyboardShortcutMonitor.dispatchHistory(
            action: .toggleSidebar,
            history: history
        )

        XCTAssertFalse(consumed)
        XCTAssertEqual(history.undoStack.count, undoBefore)
        XCTAssertEqual(history.redoStack.count, redoBefore)
    }

    func test_dispatch_undoWithNilHistory_returnsTrue_butNoCrash() {
        // The monitor still claims the action so the keystroke
        // doesn't fall through to the responder chain even if no
        // history has been wired (e.g. settings-only window state).
        let consumed = KeyboardShortcutMonitor.dispatchHistory(
            action: .undoFileOperation,
            history: nil
        )
        XCTAssertTrue(consumed)
    }

    // MARK: - Helpers

    private func makeHistory() -> FileOperationHistory {
        FileOperationHistory(
            service: FileOperationsService(trasher: FakeTrasher(trashRoot: trashLocation)),
            registry: nil
        )
    }

    private func seedTrashOp(history: FileOperationHistory) throws -> FileOperation {
        let src = tempDir.appendingPathComponent("seed.txt")
        FileManager.default.createFile(atPath: src.path, contents: Data())
        let op = try FileOperationsService(trasher: FakeTrasher(trashRoot: trashLocation))
            .trash(items: [src],
                   origin: FileOperationOrigin(windowSessionId: "w", tabId: "t"))
        history.push(op)
        return op
    }
}
