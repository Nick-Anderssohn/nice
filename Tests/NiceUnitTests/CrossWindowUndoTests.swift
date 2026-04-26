//
//  CrossWindowUndoTests.swift
//  NiceUnitTests
//
//  Coverage for the focus-follow behaviour the user requested:
//  ⌘Z in window B undoes an op originated in window A, and the
//  undo system flips back to window A so the user sees the change
//  land. Tests use a fake `FileOperationFocusRouter` so we don't
//  have to construct real `NSWindow`s.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class CrossWindowUndoTests: XCTestCase {

    private var tempDir: URL!
    private var trashLocation: URL!

    override func setUp() {
        super.setUp()
        tempDir = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-cross-window-\(UUID().uuidString)",
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

    // MARK: - Cross-window routing

    func test_undo_routesFocusToOriginatingAppState_whenDifferent() throws {
        let router = FakeFocusRouter()
        let history = makeHistory(router: router)

        // Window A: trashes a file.
        let stateA = makeAppState(sessionId: "win-A", initialMode: .tabs)
        let stateB = makeAppState(sessionId: "win-B", initialMode: .tabs)
        router.register(stateA, sessionId: "win-A")
        router.register(stateB, sessionId: "win-B")

        let src = makeFile("a.txt")
        let op = try FileOperationsService(
            trasher: FakeTrasher(trashRoot: trashLocation)
        ).trash(items: [src], origin: .init(windowSessionId: "win-A", tabId: "tab-A"))
        history.push(op)

        // User pressed ⌘Z while focused on Window B.
        history.undo()

        XCTAssertEqual(stateA.sidebarMode, .files,
                       "Originating window must switch to file browser.")
    }

    func test_undo_setsSidebarToFiles_andSelectsOriginatingTab() throws {
        let router = FakeFocusRouter()
        let history = makeHistory(router: router)
        let stateA = makeAppState(sessionId: "win-A", initialMode: .tabs)
        router.register(stateA, sessionId: "win-A")

        let src = makeFile("file.txt", body: "data")
        let op = try FileOperationsService(
            trasher: FakeTrasher(trashRoot: trashLocation)
        ).trash(items: [src], origin: .init(windowSessionId: "win-A", tabId: "tab-XYZ"))
        history.push(op)

        // Pre-undo, stateA is on a different tab.
        // (selectTab is a no-op when the tab id doesn't exist on the
        // model, but the property still updates.)
        XCTAssertEqual(stateA.sidebarMode, .tabs)

        history.undo()

        XCTAssertEqual(stateA.sidebarMode, .files)
        XCTAssertEqual(stateA.activeTabId, "tab-XYZ",
                       "Undo must select the originating tab so the user sees the change.")
    }

    func test_undo_originatingWindowGone_appliesHeadless_publishesMessage_pushesToRedo() throws {
        let router = FakeFocusRouter()
        let history = makeHistory(router: router)

        // No AppState registered for win-A — simulates the user
        // having closed the window between the trash and the ⌘Z.
        let src = makeFile("file.txt")
        let op = try FileOperationsService(
            trasher: FakeTrasher(trashRoot: trashLocation)
        ).trash(items: [src], origin: .init(windowSessionId: "win-A", tabId: "tab-A"))
        history.push(op)

        history.undo()

        XCTAssertTrue(FileManager.default.fileExists(atPath: src.path),
                      "Filesystem inverse must apply even when origin window is gone.")
        XCTAssertEqual(history.redoStack.count, 1,
                       "Headless undo still pushes to redo so the user can redo if they want.")
        XCTAssertNotNil(history.lastDriftMessage,
                        "User must be told the change landed in a window they can't see.")
        XCTAssertTrue(history.lastDriftMessage!.contains("closed window"))
    }

    func test_undo_routedToLiveAppState_doesNotPublishHeadlessMessage() throws {
        let router = FakeFocusRouter()
        let history = makeHistory(router: router)
        let stateA = makeAppState(sessionId: "win-A", initialMode: .tabs)
        router.register(stateA, sessionId: "win-A")

        let src = makeFile("file.txt")
        let op = try FileOperationsService(
            trasher: FakeTrasher(trashRoot: trashLocation)
        ).trash(items: [src], origin: .init(windowSessionId: "win-A", tabId: "tab-A"))
        history.push(op)

        history.undo()

        XCTAssertNil(history.lastDriftMessage,
                     "When focus follow succeeds, no headless heads-up message is needed.")
        XCTAssertEqual(router.bringToFrontCalls, ["win-A"],
                       "Router was asked to bring the originating window forward.")
    }

    func test_redo_routesFocusToOriginatingAppState() throws {
        let router = FakeFocusRouter()
        let history = makeHistory(router: router)
        let stateA = makeAppState(sessionId: "win-A", initialMode: .tabs)
        router.register(stateA, sessionId: "win-A")

        let src = makeFile("file.txt")
        let op = try FileOperationsService(
            trasher: FakeTrasher(trashRoot: trashLocation)
        ).trash(items: [src], origin: .init(windowSessionId: "win-A", tabId: "tab-A"))
        history.push(op)
        history.undo()
        // Reset stateA's sidebar to tabs so we can detect the redo
        // flipping it back.
        stateA.sidebarMode = .tabs

        history.redo()

        XCTAssertEqual(stateA.sidebarMode, .files)
    }

    // MARK: - Helpers

    private func makeHistory(router: FileOperationFocusRouter) -> FileOperationHistory {
        FileOperationHistory(
            service: FileOperationsService(
                trasher: FakeTrasher(trashRoot: trashLocation)
            ),
            registry: router
        )
    }

    private func makeAppState(
        sessionId: String,
        initialMode: SidebarMode
    ) -> AppState {
        AppState(
            services: nil,
            initialSidebarCollapsed: false,
            initialSidebarMode: initialMode,
            initialMainCwd: tempDir.path,
            windowSessionId: sessionId
        )
    }

    @discardableResult
    private func makeFile(_ name: String, body: String = "") -> URL {
        let url = tempDir.appendingPathComponent(name)
        FileManager.default.createFile(atPath: url.path, contents: Data(body.utf8))
        return url
    }
}

/// In-test focus router. Returns whatever `AppState`s the test
/// registered, indexed by session id, and records every
/// `bringToFront(sessionId:)` call so tests can assert focus
/// followed correctly. Lets the cross-window suite exercise
/// `FileOperationHistory.followFocus` without importing AppKit.
@MainActor
final class FakeFocusRouter: FileOperationFocusRouter {
    private var states: [String: AppState] = [:]
    private(set) var bringToFrontCalls: [String] = []

    func register(_ appState: AppState, sessionId: String) {
        states[sessionId] = appState
    }

    func appState(forSessionId id: String) -> AppState? {
        states[id]
    }

    func bringToFront(sessionId id: String) {
        bringToFrontCalls.append(id)
    }
}
