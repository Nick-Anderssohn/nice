//
//  TabModelInsertHandoffChildTests.swift
//  NiceUnitTests
//
//  Pins `TabModel.insertHandoffChild(_:underTabId:)` — the depth-1
//  lineage-placement method used by the Nice Handoff feature. Mirrors
//  the setup and assertion style in AppStateBranchTrackingTests.
//
//  Contract:
//    • Originating tab is a root (parentTabId nil) → child's parentTabId
//      == originating id; child inserted immediately after originating;
//      returns true.
//    • Originating tab is already a child (parentTabId == root id) →
//      depth-1 rule applies — child inherits the same root, not the
//      originating tab; returns true.
//    • Unknown underTabId → returns false, no insertion.
//    • Terminals-project tab → returns false (Terminals never nests
//      handoff children).
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class TabModelInsertHandoffChildTests: XCTestCase {

    private var tabs: TabModel!

    override func setUp() {
        super.setUp()
        tabs = TabModel(initialMainCwd: "/tmp")
    }

    override func tearDown() {
        tabs = nil
        super.tearDown()
    }

    // MARK: - Root originating tab

    func test_rootOriginatingTab_childParentIsOriginatingId_returnsTrue() {
        // Seed a Claude tab at root (no parentTabId).
        TabModelFixtures.seedClaudeTab(
            into: tabs, projectId: "p", tabId: "t1"
        )

        let child = makeHandoffTab(id: "child1", cwd: "/tmp/p")
        let inserted = tabs.insertHandoffChild(child, underTabId: "t1")

        XCTAssertTrue(inserted, "insertHandoffChild must return true for a known non-Terminals tab")

        let project = projectById("p")
        XCTAssertEqual(project.tabs.count, 2)
        // Child is inserted immediately after the originating tab.
        XCTAssertEqual(project.tabs[0].id, "t1",  "originating tab stays at index 0")
        XCTAssertEqual(project.tabs[1].id, "child1", "handoff child is placed right after originating")
        XCTAssertEqual(
            project.tabs[1].parentTabId, "t1",
            "child's parentTabId must equal the originating tab id (root anchor)"
        )
    }

    // MARK: - Originating tab already a child (depth-1 rule)

    func test_originatingTabIsChild_childInheritsGrandparent_returnsTrue() {
        // Build a 3-tab family: root → originating (child of root).
        // Handoff from originating must nest the new tab under root,
        // not under originating — preserving the depth-1 invariant.
        TabModelFixtures.seedClaudeTab(
            into: tabs, projectId: "p", tabId: "root"
        )
        // Seed "originating" as a tab that already points at root.
        let claudePaneId = "originating-claude"
        var originatingTab = Tab(
            id: "originating",
            title: "Originating",
            cwd: "/tmp/p",
            branch: nil,
            panes: [Pane(id: claudePaneId, title: "Claude", kind: .claude)],
            activePaneId: claudePaneId,
            claudeSessionId: "session-orig"
        )
        originatingTab.parentTabId = "root"
        guard let pi = tabs.projects.firstIndex(where: { $0.id == "p" }) else {
            return XCTFail("project 'p' not found")
        }
        tabs.projects[pi].tabs.append(originatingTab)

        let child = makeHandoffTab(id: "child1", cwd: "/tmp/p")
        let inserted = tabs.insertHandoffChild(child, underTabId: "originating")

        XCTAssertTrue(inserted)
        let project = projectById("p")

        let handoffTab = project.tabs.first(where: { $0.id == "child1" })
        XCTAssertNotNil(handoffTab, "handoff child must exist in the project")
        XCTAssertEqual(
            handoffTab?.parentTabId, "root",
            "depth-1 rule: child of a child inherits the root, not the direct parent"
        )
    }

    // MARK: - Unknown underTabId

    func test_unknownUnderTabId_returnsFalse_noInsertion() {
        TabModelFixtures.seedClaudeTab(
            into: tabs, projectId: "p", tabId: "t1"
        )
        let before = projectById("p").tabs.count

        let child = makeHandoffTab(id: "child1", cwd: "/tmp/p")
        let inserted = tabs.insertHandoffChild(child, underTabId: "does-not-exist")

        XCTAssertFalse(inserted, "unknown underTabId must return false")
        XCTAssertEqual(
            projectById("p").tabs.count, before,
            "tab list must not grow when insertHandoffChild returns false"
        )
    }

    // MARK: - Terminals project tab

    func test_terminalsProjectTab_returnsFalse_noInsertion() {
        // Terminals tabs never host handoff children — the caller
        // falls back to addTabToProjects for a top-level insert instead.
        let mainTabId = TabModel.mainTerminalTabId
        let terminalsProject = tabs.projects.first(where: {
            $0.id == TabModel.terminalsProjectId
        })!
        let terminalsBefore = terminalsProject.tabs.count

        let child = makeHandoffTab(id: "child1", cwd: "/tmp")
        let inserted = tabs.insertHandoffChild(child, underTabId: mainTabId)

        XCTAssertFalse(inserted, "Terminals-project tab must refuse handoff child")

        let terminalsAfter = tabs.projects.first(where: {
            $0.id == TabModel.terminalsProjectId
        })!
        XCTAssertEqual(
            terminalsAfter.tabs.count, terminalsBefore,
            "Terminals project tab count must not change"
        )
    }

    // MARK: - Helpers

    /// Build a minimal fully-formed Tab that can be passed to
    /// `insertHandoffChild`. Title and session id are arbitrary;
    /// only the structural fields (parentTabId, panes, activePaneId)
    /// need to be plausible.
    private func makeHandoffTab(id: String, cwd: String) -> Tab {
        let claudePaneId = "\(id)-claude"
        var tab = Tab(
            id: id,
            title: "[HANDOFF] Some task",
            cwd: cwd,
            branch: nil,
            panes: [
                Pane(id: claudePaneId, title: "Claude", kind: .claude),
                Pane(id: "\(id)-t1",   title: "Terminal 1", kind: .terminal),
            ],
            activePaneId: claudePaneId,
            claudeSessionId: UUID().uuidString.lowercased()
        )
        tab.titleManuallySet = true
        return tab
    }

    private func projectById(_ id: String) -> Project {
        guard let p = tabs.projects.first(where: { $0.id == id }) else {
            XCTFail("project '\(id)' not found"); return Project(id: id, name: id, path: "/", tabs: [])
        }
        return p
    }
}
