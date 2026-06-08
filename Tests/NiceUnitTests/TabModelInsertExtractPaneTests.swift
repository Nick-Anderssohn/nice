//
//  TabModelInsertExtractPaneTests.swift
//  NiceUnitTests
//
//  Tests for the cross-window-move model primitives on TabModel:
//  `extractPane` (remove a pane + neighbor re-focus, return the model),
//  `insertPane` (drop a foreign pane into a strip), and
//  `ensureProjectByPath` (match a destination project by path, else
//  recreate it). Mirrors TabModelMovePaneTests in fixture + assertion
//  style.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class TabModelInsertExtractPaneTests: XCTestCase {

    private var appState: AppState!

    private let projectId = "ie"
    private let tabId     = "ie-tab"
    private let p0        = "ie-tab-p0"
    private let p1        = "ie-tab-p1"
    private let p2        = "ie-tab-p2"

    override func setUp() {
        super.setUp()
        appState = AppState()
        let tab = Tab(
            id: tabId,
            title: "Insert/extract test",
            cwd: "/tmp/ie",
            panes: [
                Pane(id: p0, title: "Terminal 1", kind: .terminal),
                Pane(id: p1, title: "Terminal 2", kind: .terminal),
                Pane(id: p2, title: "Terminal 3", kind: .terminal),
            ],
            activePaneId: p0
        )
        let project = Project(id: projectId, name: "IE", path: "/tmp/ie", tabs: [tab])
        appState.tabs.projects = [appState.tabs.projects[0], project]
    }

    override func tearDown() {
        appState = nil
        super.tearDown()
    }

    private func paneIds(_ id: String) -> [String] {
        appState.tabs.tab(for: id)?.panes.map(\.id) ?? []
    }

    // MARK: - extractPane

    func test_extractPane_removesAndReturnsPane() {
        let removed = appState.tabs.extractPane(p1, fromTab: tabId)
        XCTAssertEqual(removed?.id, p1)
        XCTAssertEqual(paneIds(tabId), [p0, p2])
    }

    func test_extractPane_nonActive_leavesActiveUnchanged() {
        _ = appState.tabs.extractPane(p1, fromTab: tabId)
        XCTAssertEqual(appState.tabs.tab(for: tabId)?.activePaneId, p0)
    }

    func test_extractPane_active_refocusesSlotNeighbor() {
        // active = p1 (middle). Removing it focuses the pane that slid
        // into its slot: p2.
        appState.tabs.mutateTab(id: tabId) { $0.activePaneId = p1 }
        _ = appState.tabs.extractPane(p1, fromTab: tabId)
        XCTAssertEqual(appState.tabs.tab(for: tabId)?.activePaneId, p2)
    }

    func test_extractPane_activeLast_refocusesPrevious() {
        appState.tabs.mutateTab(id: tabId) { $0.activePaneId = p2 }
        _ = appState.tabs.extractPane(p2, fromTab: tabId)
        XCTAssertEqual(appState.tabs.tab(for: tabId)?.activePaneId, p1)
    }

    func test_extractPane_lastRemaining_clearsActive() {
        _ = appState.tabs.extractPane(p1, fromTab: tabId)
        _ = appState.tabs.extractPane(p2, fromTab: tabId)
        appState.tabs.mutateTab(id: tabId) { $0.activePaneId = p0 }
        _ = appState.tabs.extractPane(p0, fromTab: tabId)
        XCTAssertEqual(paneIds(tabId), [])
        XCTAssertNil(appState.tabs.tab(for: tabId)?.activePaneId)
    }

    func test_extractPane_unknownPane_returnsNil_noMutation() {
        var count = 0
        appState.tabs.onTreeMutation = { count += 1 }
        let removed = appState.tabs.extractPane("ghost", fromTab: tabId)
        XCTAssertNil(removed)
        XCTAssertEqual(paneIds(tabId), [p0, p1, p2])
        XCTAssertEqual(count, 0)
    }

    func test_extractPane_realRemoval_firesOnTreeMutationOnce() {
        var count = 0
        appState.tabs.onTreeMutation = { count += 1 }
        _ = appState.tabs.extractPane(p1, fromTab: tabId)
        XCTAssertEqual(count, 1)
    }

    // MARK: - insertPane

    func test_insertPane_before_target() {
        let foreign = Pane(id: "fx", title: "Foreign", kind: .terminal)
        appState.tabs.insertPane(foreign, inTab: tabId, relativeTo: p1, placeAfter: false)
        XCTAssertEqual(paneIds(tabId), [p0, "fx", p1, p2])
    }

    func test_insertPane_after_target() {
        let foreign = Pane(id: "fx", title: "Foreign", kind: .terminal)
        appState.tabs.insertPane(foreign, inTab: tabId, relativeTo: p1, placeAfter: true)
        XCTAssertEqual(paneIds(tabId), [p0, p1, "fx", p2])
    }

    func test_insertPane_nilTarget_appends() {
        let foreign = Pane(id: "fx", title: "Foreign", kind: .terminal)
        appState.tabs.insertPane(foreign, inTab: tabId, relativeTo: nil, placeAfter: false)
        XCTAssertEqual(paneIds(tabId), [p0, p1, p2, "fx"])
    }

    func test_insertPane_unknownTarget_appends() {
        let foreign = Pane(id: "fx", title: "Foreign", kind: .terminal)
        appState.tabs.insertPane(foreign, inTab: tabId, relativeTo: "ghost", placeAfter: true)
        XCTAssertEqual(paneIds(tabId), [p0, p1, p2, "fx"])
    }

    func test_insertPane_duplicateId_isNoOp() {
        var count = 0
        appState.tabs.onTreeMutation = { count += 1 }
        let dup = Pane(id: p1, title: "Dup", kind: .terminal)
        appState.tabs.insertPane(dup, inTab: tabId, relativeTo: p0, placeAfter: true)
        XCTAssertEqual(paneIds(tabId), [p0, p1, p2])
        XCTAssertEqual(count, 0)
    }

    func test_insertPane_doesNotChangeActivePaneId() {
        let foreign = Pane(id: "fx", title: "Foreign", kind: .terminal)
        appState.tabs.insertPane(foreign, inTab: tabId, relativeTo: p1, placeAfter: false)
        XCTAssertEqual(appState.tabs.tab(for: tabId)?.activePaneId, p0)
    }

    func test_insertPane_realInsert_firesOnTreeMutationOnce() {
        var count = 0
        appState.tabs.onTreeMutation = { count += 1 }
        let foreign = Pane(id: "fx", title: "Foreign", kind: .terminal)
        appState.tabs.insertPane(foreign, inTab: tabId, relativeTo: p1, placeAfter: false)
        XCTAssertEqual(count, 1)
    }

    // MARK: - ensureProjectByPath

    func test_ensureProjectByPath_matchesExistingByPath() {
        let idx = appState.tabs.ensureProjectByPath(
            id: "different-id", name: "Different", path: "/tmp/ie"
        )
        // Matched the seeded project (index 1), not appended a new one.
        XCTAssertEqual(idx, 1)
        XCTAssertEqual(appState.tabs.projects[idx].id, projectId)
        XCTAssertEqual(appState.tabs.projects.count, 2)
    }

    func test_ensureProjectByPath_recreatesWhenAbsent_copyingIdentity() {
        let countBefore = appState.tabs.projects.count
        let idx = appState.tabs.ensureProjectByPath(
            id: "p-new", name: "NEW", path: "/tmp/brand-new"
        )
        XCTAssertEqual(appState.tabs.projects.count, countBefore + 1)
        XCTAssertEqual(appState.tabs.projects[idx].id, "p-new")
        XCTAssertEqual(appState.tabs.projects[idx].name, "NEW")
        XCTAssertEqual(appState.tabs.projects[idx].path, "/tmp/brand-new")
    }

    func test_ensureProjectByPath_ignoresTerminalsProject() {
        // The pinned Terminals project (index 0) shares whatever path it
        // was seeded with; ensureProjectByPath must never match it even
        // if paths coincide — Claude tabs never live in Terminals.
        let terminalsPath = appState.tabs.projects[0].path
        let idx = appState.tabs.ensureProjectByPath(
            id: "p-x", name: "X", path: terminalsPath
        )
        XCTAssertNotEqual(idx, 0)
        XCTAssertEqual(appState.tabs.projects[idx].id, "p-x")
    }

    func test_ensureProjectByPath_neverDuplicatesTerminalsProject() {
        // Bug 1 hardening: asking for the reserved Terminals id must
        // resolve to the existing pinned project at index 0 — never append
        // a SECOND project carrying `id == terminalsProjectId` (the root
        // cause of the duplicate "TERMINALS" sidebar section).
        let countBefore = appState.tabs.projects.count
        let idx = appState.tabs.ensureProjectByPath(
            id: TabModel.terminalsProjectId,
            name: "Terminals",
            path: "/some/other/path"
        )
        XCTAssertEqual(idx, 0)
        XCTAssertEqual(appState.tabs.projects.count, countBefore,
                       "Must not append a duplicate terminals project")
        XCTAssertEqual(
            appState.tabs.projects.filter { $0.id == TabModel.terminalsProjectId }.count,
            1, "Exactly one project may carry the terminals id"
        )
    }
}
