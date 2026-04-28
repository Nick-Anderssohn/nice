//
//  AppStateSerializationTests.swift
//  NiceUnitTests
//
//  Pins down `AppState.snapshotPersistedWindow()` — the function that
//  converts the live data model into the `PersistedWindow` written to
//  sessions.json. Regressions here cause silent data loss: fields
//  dropped from the snapshot come back nil on restore.
//
//  The convenience `AppState()` init disables disk persistence
//  (`services == nil`), so these tests assert the in-memory snapshot
//  shape without touching any SessionStore on disk.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class AppStateSerializationTests: XCTestCase {

    private var appState: AppState!
    private var homeSandbox: TestHomeSandbox!

    override func setUp() {
        super.setUp()
        homeSandbox = TestHomeSandbox()
        appState = AppState()
    }

    override func tearDown() {
        appState = nil
        homeSandbox.teardown()
        homeSandbox = nil
        super.tearDown()
    }

    // MARK: - Basic shape

    func test_snapshot_includesWindowSessionId() {
        let snap = appState.windowSession.snapshotPersistedWindow()
        XCTAssertFalse(snap.id.isEmpty,
                       "Snapshot must carry windowSessionId so SessionStore can upsert by window identity.")
    }

    func test_snapshot_preservesTerminalsProjectEvenWhenEmpty() {
        // Start from a fresh AppState and empty the Terminals group —
        // the project itself must still persist so its cwd survives
        // relaunch even with zero tabs.
        appState.tabs.projects = [
            Project(id: TabModel.terminalsProjectId, name: "Terminals",
                    path: "/tmp/terminals", tabs: [])
        ]

        let snap = appState.windowSession.snapshotPersistedWindow()
        XCTAssertEqual(snap.projects.count, 1)
        XCTAssertEqual(snap.projects.first?.id, TabModel.terminalsProjectId,
                       "Terminals project must survive a snapshot even with zero tabs; user's picked cwd would otherwise be lost on quit.")
    }

    func test_snapshot_dropsEmptyNonTerminalsProjects() {
        // Seed an empty project alongside the Terminals group. It
        // should be dropped from the snapshot — empty user projects
        // are noise on restore.
        appState.tabs.projects.append(
            Project(id: "empty", name: "Empty", path: "/tmp/empty", tabs: [])
        )
        let snap = appState.windowSession.snapshotPersistedWindow()
        let ids = snap.projects.map(\.id)
        XCTAssertFalse(ids.contains("empty"),
                       "Empty non-terminals projects must be pruned — user can re-add.")
    }

    // MARK: - Tab / pane round-trip

    func test_snapshot_preservesClaudeTabFields() {
        let claudeId = "t1-claude"
        let terminalId = "t1-t1"
        let tab = Tab(
            id: "t1",
            title: "Fix top bar height",
            cwd: "/Users/nick/Projects/nice",
            branch: "main",
            panes: [
                Pane(id: claudeId, title: "Claude", kind: .claude),
                Pane(id: terminalId, title: "Terminal 1", kind: .terminal),
            ],
            activePaneId: claudeId,
            claudeSessionId: "e4f1a2b3-c0d4-4e5f-9a0b-1c2d3e4f5a6b"
        )
        appState.tabs.projects.append(
            Project(id: "nice", name: "Nice",
                    path: "/Users/nick/Projects/nice", tabs: [tab])
        )

        let snap = appState.windowSession.snapshotPersistedWindow()
        guard let persistedProject = snap.projects.first(where: { $0.id == "nice" }),
              let persistedTab = persistedProject.tabs.first
        else {
            XCTFail("Expected seeded project + tab in snapshot")
            return
        }

        XCTAssertEqual(persistedTab.id, tab.id)
        XCTAssertEqual(persistedTab.title, tab.title)
        XCTAssertEqual(persistedTab.cwd, tab.cwd)
        XCTAssertEqual(persistedTab.branch, tab.branch)
        XCTAssertEqual(persistedTab.claudeSessionId, tab.claudeSessionId,
                       "claudeSessionId is the ONLY way to resume the transcript on next launch — must survive.")
        XCTAssertEqual(persistedTab.activePaneId, claudeId,
                       "Active pane selection must survive so restore focuses the right pane.")
        XCTAssertEqual(persistedTab.panes.count, 2)
        XCTAssertEqual(persistedTab.panes[0].kind, .claude)
        XCTAssertEqual(persistedTab.panes[1].kind, .terminal)
    }

    func test_snapshot_preservesActiveTabIdAndSidebarCollapsed() {
        appState.sidebar.sidebarCollapsed = true
        appState.tabs.activeTabId = TabModel.mainTerminalTabId

        let snap = appState.windowSession.snapshotPersistedWindow()
        XCTAssertEqual(snap.activeTabId, TabModel.mainTerminalTabId)
        XCTAssertTrue(snap.sidebarCollapsed)
    }

    func test_snapshot_preservesTerminalOnlyTabInTerminalsGroup() {
        // Terminal-only tabs (including everything in the pinned
        // Terminals group) carry claudeSessionId == nil. They must
        // still round-trip — the Terminals group's persistence was
        // the point of the v2→v3 schema bump.
        let snap = appState.windowSession.snapshotPersistedWindow()
        guard let terminals = snap.projects.first(where: { $0.id == TabModel.terminalsProjectId }),
              let mainTab = terminals.tabs.first
        else {
            XCTFail("Expected Main terminal tab in snapshot")
            return
        }
        XCTAssertNil(mainTab.claudeSessionId,
                     "Terminal-only tabs in the Terminals group carry nil claudeSessionId.")
        XCTAssertFalse(mainTab.panes.isEmpty,
                       "Main tab's terminal pane must be persisted.")
        XCTAssertEqual(mainTab.panes.first?.kind, .terminal)
    }

    // MARK: - Codable

    func test_snapshot_isRoundTripCodable() throws {
        // Seed a representative state, snapshot, encode, decode,
        // compare. This pins down the wire format — if someone adds a
        // Codable field without migrating decoders, the round-trip
        // either loses it (field missing from decoded struct) or
        // crashes (field required without default).
        let claudeId = "t1-claude"
        let tab = Tab(
            id: "t1", title: "Fix top bar height",
            cwd: "/Users/nick/Projects/nice", branch: "main",
            panes: [Pane(id: claudeId, title: "Claude", kind: .claude)],
            activePaneId: claudeId,
            claudeSessionId: "sid-123"
        )
        appState.tabs.projects.append(
            Project(id: "nice", name: "Nice",
                    path: "/Users/nick/Projects/nice", tabs: [tab])
        )

        let snap = appState.windowSession.snapshotPersistedWindow()
        let encoded = try JSONEncoder().encode(snap)
        let decoded = try JSONDecoder().decode(PersistedWindow.self, from: encoded)
        XCTAssertEqual(decoded, snap)
    }
}
