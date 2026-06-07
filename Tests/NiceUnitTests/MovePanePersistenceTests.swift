//
//  MovePanePersistenceTests.swift
//  NiceUnitTests
//
//  Persistence round-trip: after `movePane` the reordered panes must
//  survive a `snapshotPersistedWindow` → JSON encode → decode cycle.
//  Mirrors the round-trip style from `TabModelRenameTests` —
//  `snapshotPersistedWindow()` is the same entry point the real
//  `SessionStore` calls on every debounced save.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class MovePanePersistenceTests: XCTestCase {

    private var appState: AppState!

    // Stable ids used throughout — mirrors TabModelMovePaneTests.
    private let projectId = "persist-mp"
    private let tabId     = "persist-mp-tab"
    private let p0        = "persist-mp-p0"
    private let p1        = "persist-mp-p1"
    private let p2        = "persist-mp-p2"

    override func setUp() {
        super.setUp()
        appState = AppState()
        let tab = Tab(
            id: tabId,
            title: "Persist test",
            cwd: "/tmp/persist-mp",
            panes: [
                Pane(id: p0, title: "Terminal 1", kind: .terminal),
                Pane(id: p1, title: "Terminal 2", kind: .terminal),
                Pane(id: p2, title: "Terminal 3", kind: .terminal),
            ],
            activePaneId: p0
        )
        let project = Project(
            id: projectId, name: "PERSIST-MP", path: "/tmp/persist-mp", tabs: [tab]
        )
        appState.tabs.projects = [appState.tabs.projects[0], project]
    }

    override func tearDown() {
        appState = nil
        super.tearDown()
    }

    // MARK: - Round-trip

    /// After `movePane` the reordered pane sequence must survive an
    /// encode/decode round-trip so a relaunched Nice shows the same order.
    func test_movePane_panesOrderSurvivesPersistedWindowRoundTrip() throws {
        // [p0, p1, p2] — move p0 to after p2 → expected [p1, p2, p0].
        appState.tabs.movePane(p0, inTab: tabId, relativeTo: p2, placeAfter: true)

        // Snapshot the window tree exactly as the real SessionStore does.
        let snapshot = appState.windowSession.snapshotPersistedWindow()

        // Encode → decode.
        let data = try JSONEncoder().encode(snapshot)
        let decoded = try JSONDecoder().decode(PersistedWindow.self, from: data)

        // Find the tab in the decoded snapshot.
        let persistedTab = try XCTUnwrap(
            decoded.projects
                .first(where: { $0.id == projectId })?
                .tabs.first(where: { $0.id == tabId }),
            "Tab must survive the encode/decode round-trip."
        )

        XCTAssertEqual(
            persistedTab.panes.map(\.id), [p1, p2, p0],
            "Pane order must match the post-movePane state after encode/decode."
        )
    }

    /// A no-op `movePane` (same id) must leave the persisted pane order
    /// unchanged — the pre-move order round-trips unmodified.
    func test_movePane_noOp_persistedOrderUnchanged() throws {
        // No mutation — pane order stays [p0, p1, p2].
        appState.tabs.movePane(p0, inTab: tabId, relativeTo: p0, placeAfter: false)

        let snapshot = appState.windowSession.snapshotPersistedWindow()
        let data = try JSONEncoder().encode(snapshot)
        let decoded = try JSONDecoder().decode(PersistedWindow.self, from: data)

        let persistedTab = try XCTUnwrap(
            decoded.projects
                .first(where: { $0.id == projectId })?
                .tabs.first(where: { $0.id == tabId })
        )

        XCTAssertEqual(persistedTab.panes.map(\.id), [p0, p1, p2])
    }
}
