//
//  AppStateBranchTrackingTests.swift
//  NiceUnitTests
//
//  Locks down /branch tracking — the half of
//  `handleClaudeSessionUpdate` that classifies a session rotation by
//  its `source` and, for `/branch` (and `--fork-session`), spawns a
//  sibling parent tab pinned to the pre-rotation session id so the
//  user can resume the original conversation from the sidebar.
//
//  Coverage:
//    • source=resume + id-change creates a parent tab inserted right
//      above the originating tab; titles inherit; old id pinned to
//      parent; new id stays on originating tab; parent's parentTabId
//      is nil (parents are root); originating tab now points at
//      parent.
//    • source=clear + id-change does NOT create a parent (just
//      mutates claudeSessionId in place).
//    • source nil/missing does NOT create a parent. Defensive against
//      older hook payloads still in flight during an upgrade and
//      future Claude versions that drop the field.
//    • source=resume with the SAME id is a no-op (real /resume keeps
//      the id stable).
//    • Two /branch rotations on the same tab produce two parent tabs
//      at root level (flat-siblings UX); the originating tab's
//      parentTabId always points at the most recent parent. The
//      first parent stays at root with `parentTabId == nil` even
//      after the second branch.
//    • Closing a parent (driving `paneExited` on every pane) clears
//      the child's `parentTabId` via the dangling-reference sweep.
//    • Rotations on the pinned Terminals tab don't materialize a
//      parent (Terminals never hosts Claude sessions; defensive
//      guard).
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class AppStateBranchTrackingTests: XCTestCase {

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

    // MARK: - Branch detection

    func test_branch_resumeWithIdChange_createsParentTab() {
        seedClaudeTab(
            projectId: "p",
            tabId: "t1",
            sessionId: "OLD",
            title: "wire up the foo"
        )

        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude", sessionId: "NEW", source: "resume"
        )

        let project = projectById("p")
        XCTAssertEqual(
            project.tabs.count, 2,
            "branch must add exactly one sibling parent tab"
        )

        // Parent inserted immediately above the originating tab so
        // visual order reads [parent, child].
        let parent = project.tabs[0]
        let child = project.tabs[1]

        XCTAssertEqual(child.id, "t1", "originating tab keeps its id")
        XCTAssertEqual(
            child.claudeSessionId, "NEW",
            "originating tab adopts the post-rotation id"
        )
        XCTAssertEqual(
            child.parentTabId, parent.id,
            "originating tab points at the new parent"
        )

        XCTAssertEqual(
            parent.claudeSessionId, "OLD",
            "parent tab is pinned to the pre-rotation id (the user's recovery handle)"
        )
        XCTAssertNil(
            parent.parentTabId,
            "parent itself stays at root (parentTabId nil)"
        )
        XCTAssertEqual(
            parent.title, "wire up the foo",
            "parent inherits the originating tab's title"
        )
        XCTAssertEqual(
            parent.cwd, child.cwd,
            "parent inherits the originating tab's cwd"
        )

        // Parent must have the standard claude+terminal pane shape so
        // the deferred-resume path's NICE_PREFILL_COMMAND can prefill
        // the companion terminal.
        XCTAssertEqual(parent.panes.count, 2)
        XCTAssertTrue(
            parent.panes.contains { $0.kind == .claude },
            "parent must have a claude pane"
        )
        XCTAssertTrue(
            parent.panes.contains { $0.kind == .terminal },
            "parent must have a companion terminal pane"
        )
    }

    func test_clear_withIdChange_doesNotCreateParent() {
        seedClaudeTab(projectId: "p", tabId: "t1", sessionId: "OLD")

        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude", sessionId: "NEW", source: "clear"
        )

        let project = projectById("p")
        XCTAssertEqual(
            project.tabs.count, 1,
            "/clear must not spawn a parent tab — user wanted to discard the conversation"
        )
        XCTAssertEqual(
            project.tabs[0].claudeSessionId, "NEW",
            "/clear still updates the session id in place"
        )
        XCTAssertNil(project.tabs[0].parentTabId)
    }

    func test_missingSource_doesNotCreateParent() {
        // Older hook payloads (still on disk during an upgrade window
        // before ensureScriptInstalled rewrites them) and any future
        // Claude version that drops the `source` field both surface as
        // nil. We'd rather miss a /branch than misclassify a /clear,
        // so nil source is the conservative no-parent path.
        seedClaudeTab(projectId: "p", tabId: "t1", sessionId: "OLD")

        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude", sessionId: "NEW", source: nil
        )

        let project = projectById("p")
        XCTAssertEqual(
            project.tabs.count, 1,
            "missing source must not spawn a parent tab"
        )
        XCTAssertEqual(project.tabs[0].claudeSessionId, "NEW")
    }

    func test_resumeWithSameId_doesNotCreateParent() {
        // A real `claude --resume <id>` keeps the id stable — the
        // updateClaudeSessionId short-circuit absorbs it, and the
        // branch-detection guard requires `oldId != sessionId`. So
        // resumes that didn't actually rotate must never spawn a
        // parent.
        seedClaudeTab(projectId: "p", tabId: "t1", sessionId: "SAME")

        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude", sessionId: "SAME", source: "resume"
        )

        let project = projectById("p")
        XCTAssertEqual(
            project.tabs.count, 1,
            "resume without rotation must not spawn a parent tab"
        )
        XCTAssertEqual(project.tabs[0].claudeSessionId, "SAME")
    }

    // MARK: - Multi-branch (depth-1 tree under original)

    func test_firstBranch_promotesParentToRoot_andOriginatingBecomesChild() {
        seedClaudeTab(projectId: "p", tabId: "t1", sessionId: "S0")

        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude", sessionId: "S1", source: "resume"
        )

        let project = projectById("p")
        XCTAssertEqual(project.tabs.count, 2)
        let parent = project.tabs[0]
        let originating = project.tabs[1]
        XCTAssertNil(parent.parentTabId, "first parent becomes the lineage root")
        XCTAssertEqual(
            originating.parentTabId, parent.id,
            "originating tab is pulled in as a depth-1 child of the new root"
        )
    }

    func test_secondBranch_addsSiblingChildUnderSameRoot() {
        // Depth-1 layout: the FIRST /branch establishes a root and the
        // originating tab becomes its child. Every subsequent /branch
        // adds another parent that is a SIBLING under that same root,
        // not a new root and not a deeper indent. The originating tab
        // keeps pointing at the original root.
        seedClaudeTab(projectId: "p", tabId: "t1", sessionId: "S0")

        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude", sessionId: "S1", source: "resume"
        )
        let rootId = projectById("p").tabs[0].id

        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude", sessionId: "S2", source: "resume"
        )

        let after = projectById("p")
        XCTAssertEqual(after.tabs.count, 3)

        let root = after.tabs[0]
        let secondParent = after.tabs[1]
        let originating = after.tabs[2]

        XCTAssertEqual(root.id, rootId, "root never changes once established")
        XCTAssertEqual(root.claudeSessionId, "S0",
                       "root pins the very first pre-/branch session")
        XCTAssertNil(root.parentTabId, "root stays at depth 0")

        XCTAssertEqual(originating.id, "t1")
        XCTAssertEqual(originating.claudeSessionId, "S2",
                       "originating tab carries the freshest session id")
        XCTAssertEqual(
            originating.parentTabId, rootId,
            "originating tab keeps pointing at the original root after subsequent branches"
        )

        XCTAssertEqual(secondParent.claudeSessionId, "S1",
                       "second parent pins the id that was current right before the second /branch")
        XCTAssertEqual(
            secondParent.parentTabId, rootId,
            "second parent is a sibling under the same root (depth-1 layout)"
        )
    }

    func test_thirdBranch_keepsAddingSiblingsUnderSameRoot() {
        // Three branches in a row exercise the steady-state path:
        // every new parent inherits the originating tab's existing
        // root pointer, the originating tab's pointer never changes
        // again after the first /branch, and indent depth never grows
        // past 1 no matter how many parents accumulate.
        seedClaudeTab(projectId: "p", tabId: "t1", sessionId: "S0")

        for (i, newSession) in ["S1", "S2", "S3"].enumerated() {
            appState.sessions.handleClaudeSessionUpdate(
                paneId: "t1-claude", sessionId: newSession, source: "resume"
            )
            let project = projectById("p")
            // i+2 because each iteration adds one parent and the
            // originating tab is always present.
            XCTAssertEqual(
                project.tabs.count, i + 2,
                "after \(i + 1) /branch(es) the project should hold \(i + 2) tabs"
            )
        }

        let final = projectById("p")
        let root = final.tabs[0]
        XCTAssertNil(root.parentTabId)
        XCTAssertEqual(root.claudeSessionId, "S0")

        // Every non-root tab in the family points at the root — flat
        // depth-1 layout, no chains.
        for tab in final.tabs.dropFirst() {
            XCTAssertEqual(
                tab.parentTabId, root.id,
                "every non-root tab in the lineage must point at the original root, found \(String(describing: tab.parentTabId)) on \(tab.id)"
            )
        }

        XCTAssertEqual(final.tabs.last?.id, "t1",
                       "originating tab stays at the bottom of its family in display order")
        XCTAssertEqual(final.tabs.last?.claudeSessionId, "S3")
    }

    // MARK: - Lifecycle: closing a parent

    func test_closingParent_clearsChildParentTabId() {
        seedClaudeTab(projectId: "p", tabId: "t1", sessionId: "OLD")

        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude", sessionId: "NEW", source: "resume"
        )
        let parent = projectById("p").tabs[0]
        XCTAssertEqual(
            projectById("p").tabs[1].parentTabId, parent.id,
            "precondition: child points at parent"
        )

        // Dissolve the parent by exiting all of its panes (model-level
        // dissolve cascade — no real pty needed). Snapshot the pane
        // list first because each exit mutates `parent.panes` in place.
        for pane in parent.panes {
            appState.sessions.paneExited(
                tabId: parent.id, paneId: pane.id, exitCode: 0
            )
        }

        let after = projectById("p")
        XCTAssertEqual(
            after.tabs.count, 1,
            "parent tab is gone after its panes all exited"
        )
        XCTAssertEqual(after.tabs[0].id, "t1")
        XCTAssertNil(
            after.tabs[0].parentTabId,
            "child's parentTabId is cleared when parent dissolves"
        )
    }

    // MARK: - Lifecycle: closing a child (not parent)

    func test_closingChild_doesNotMutateParent() {
        // Mirror of the parent-close case: the dangling-pointer sweep
        // walks the whole project, but only mutates tabs that pointed
        // at the removed id. Closing a child (which nothing else
        // points at) must leave the surviving parent's `parentTabId`
        // exactly as it was — the parent doesn't suddenly orphan
        // itself just because its companion child went away.
        seedClaudeTab(projectId: "p", tabId: "t1", sessionId: "OLD")
        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude", sessionId: "NEW", source: "resume"
        )
        let parent = projectById("p").tabs[0]
        let child = projectById("p").tabs[1]
        XCTAssertNil(parent.parentTabId, "precondition: parent at root")
        XCTAssertEqual(child.parentTabId, parent.id, "precondition: child under parent")

        // Dissolve the child by exiting all of its panes.
        for pane in child.panes {
            appState.sessions.paneExited(
                tabId: child.id, paneId: pane.id, exitCode: 0
            )
        }

        let after = projectById("p")
        XCTAssertEqual(after.tabs.count, 1, "child is gone, parent remains")
        XCTAssertEqual(after.tabs[0].id, parent.id)
        XCTAssertNil(
            after.tabs[0].parentTabId,
            "parent's parentTabId must NOT be cleared when an unrelated child closes"
        )
    }

    // MARK: - Per-window scoping
    //
    // `materializeBranchParent` reaches `tabs.projectTabIndex(for:)`
    // and `tabs.tab(for:)`, both of which scope the lookup to a single
    // `TabModel`. `AppStateClaudeSessionUpdateTests` already pins the
    // id-update side; this mirror pins the parent-spawn side so a
    // future "centralize the index" refactor can't accidentally
    // cross-route /branch into a sibling window.

    func test_branchMaterialization_isScopedToOwningWindow() {
        // Window A and B each own a Claude tab. A /branch-shaped
        // signal (resume + id-change) addressed to B's pane is
        // dispatched into A's handler. A's `tabIdOwning` returns nil
        // (B's pane isn't in A's projects), so neither A nor B should
        // grow a parent — the dispatch went to A only, and A had no
        // matching pane to act on.
        seedClaudeTab(projectId: "pA", tabId: "tA", sessionId: "A0")

        let stateB = AppState()
        defer { stateB.tearDown() }
        TabModelFixtures.seedClaudeTab(
            into: stateB.tabs,
            projectId: "pB", tabId: "tB", sessionId: "B0"
        )

        // Cross-window send into A — must be a no-op on both windows
        // because A doesn't own paneId "tB-claude".
        appState.sessions.handleClaudeSessionUpdate(
            paneId: "tB-claude", sessionId: "B-LEAKED", source: "resume"
        )

        XCTAssertEqual(
            projectById("pA").tabs.count, 1,
            "A must not materialize a parent for a B-shaped paneId"
        )
        XCTAssertEqual(
            appState.tabs.tab(for: "tA")?.claudeSessionId, "A0",
            "A's tab must be untouched by a B-shaped paneId"
        )
        XCTAssertEqual(
            stateB.tabs.projects.first(where: { $0.id == "pB" })?.tabs.count, 1,
            "B must not materialize a parent — A's handler was the dispatch target, not B's"
        )
        XCTAssertEqual(
            stateB.tabs.tab(for: "tB")?.claudeSessionId, "B0",
            "B's tab must be untouched until B's own handler runs"
        )

        // B's own handler does materialize a parent — confirms the
        // scoping wasn't a "/branch never works" false negative.
        stateB.sessions.handleClaudeSessionUpdate(
            paneId: "tB-claude", sessionId: "B1", source: "resume"
        )

        XCTAssertEqual(
            stateB.tabs.projects.first(where: { $0.id == "pB" })?.tabs.count, 2,
            "B's own /branch must materialize a parent in B"
        )
        XCTAssertEqual(
            projectById("pA").tabs.count, 1,
            "B's /branch must not bleed a parent into A"
        )
        XCTAssertEqual(
            appState.tabs.tab(for: "tA")?.claudeSessionId, "A0",
            "B's /branch must not mutate A's claudeSessionId"
        )
    }

    // MARK: - /branch on a root tab (re-parents former children)

    func test_branchOnRoot_preservesDepth1_byReparentingFormerChildren() {
        // Spec: depth-1 invariant. After /branch on the lineage root,
        // the new parent must become the new root, the old root must
        // slide to a depth-1 child of the new root, AND every tab
        // that was already pointing at the old root must be
        // re-parented to the new root — otherwise those tabs would
        // silently become depth-2 in the lineage tree.
        seedClaudeTab(projectId: "p", tabId: "t1", sessionId: "S0")
        // First /branch establishes the lineage:
        // tRoot (S0) at root; t1 (S1) child of tRoot.
        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude", sessionId: "S1", source: "resume"
        )
        let oldRoot = projectById("p").tabs[0]
        XCTAssertNil(oldRoot.parentTabId, "precondition: oldRoot is the root")

        // Second /branch on t1 adds a sibling parent under oldRoot.
        appState.sessions.handleClaudeSessionUpdate(
            paneId: "t1-claude", sessionId: "S2", source: "resume"
        )
        let secondParentId = projectById("p").tabs.first(where: {
            $0.id != oldRoot.id && $0.id != "t1"
        })?.id
        XCTAssertNotNil(secondParentId)

        // Now the user opens oldRoot, runs the deferred resume, the
        // claude session comes alive (which the test simulates by
        // firing the SessionStart hook from oldRoot's claude pane),
        // then /branches oldRoot. Without re-parenting, the former
        // children of oldRoot (the second parent and t1) would still
        // point at oldRoot — and oldRoot would now be a depth-1 child
        // of a brand-new root, making them effectively depth-2 in the
        // lineage. With re-parenting, every former child of oldRoot
        // moves to the new root, preserving the flat depth-1 layout.
        let oldRootClaudePaneId = oldRoot.panes.first(where: {
            $0.kind == .claude
        })?.id
        XCTAssertNotNil(oldRootClaudePaneId)
        appState.sessions.handleClaudeSessionUpdate(
            paneId: oldRootClaudePaneId!,
            sessionId: "S0-PRIME",
            source: "resume"
        )

        let after = projectById("p")
        // The new root is whatever tab now has parentTabId == nil.
        guard let newRoot = after.tabs.first(where: { $0.parentTabId == nil }) else {
            return XCTFail("expected exactly one root after /branch on the old root")
        }
        XCTAssertNotEqual(newRoot.id, oldRoot.id,
                          "old root must no longer be at depth 0")
        XCTAssertEqual(
            after.tabs.filter { $0.parentTabId == nil }.count, 1,
            "exactly one root tab must remain in the lineage"
        )
        // Old root, the second parent, and t1 must all point at the
        // new root.
        for tab in after.tabs where tab.id != newRoot.id {
            XCTAssertEqual(
                tab.parentTabId, newRoot.id,
                "tab \(tab.id) (was depth-? under \(String(describing: tab.parentTabId))) must be re-parented to the new root"
            )
        }
        // Sanity: the t1 originating tab still carries the freshest
        // session id from its second /branch, untouched by the
        // /branch on the root.
        XCTAssertEqual(after.tabs.first(where: { $0.id == "t1" })?.claudeSessionId, "S2")
        // The new root holds the session id that was current on
        // oldRoot right before its /branch (i.e. S0).
        XCTAssertEqual(newRoot.claudeSessionId, "S0")
        // oldRoot now holds its post-rotation id.
        XCTAssertEqual(
            after.tabs.first(where: { $0.id == oldRoot.id })?.claudeSessionId,
            "S0-PRIME"
        )
    }

    // MARK: - Defensive guards

    func test_branchOn_nilClaudeSessionId_isNoOp() {
        // Pre-condition: a Claude tab whose `claudeSessionId` is nil
        // (claude not yet started, or hook fired before the session
        // id was minted). The branch-detection guard requires a
        // non-nil oldId; the rotation must update the tab's session
        // id but NOT spawn a parent.
        let projectId = "p-nil"
        let tabId = "t-nil"
        let claudePaneId = "\(tabId)-claude"
        // Hand-craft because TabModelFixtures.seedClaudeTab always
        // sets a sessionId; we want the explicitly-nil case here.
        let tab = Tab(
            id: tabId,
            title: "Pre-claude",
            cwd: "/tmp/\(projectId)",
            branch: nil,
            panes: [
                Pane(id: claudePaneId, title: "Claude", kind: .claude),
                Pane(id: "\(tabId)-t1", title: "Terminal 1", kind: .terminal),
            ],
            activePaneId: claudePaneId,
            claudeSessionId: nil
        )
        appState.tabs.projects.append(Project(
            id: projectId, name: "P", path: "/tmp/\(projectId)", tabs: [tab]
        ))

        appState.sessions.handleClaudeSessionUpdate(
            paneId: claudePaneId, sessionId: "FIRST", source: "resume"
        )

        let project = projectById(projectId)
        XCTAssertEqual(
            project.tabs.count, 1,
            "no parent tab should be spawned when the originating tab had no prior session id"
        )
        XCTAssertEqual(
            project.tabs[0].claudeSessionId, "FIRST",
            "id should still be set in place"
        )
        XCTAssertNil(project.tabs[0].parentTabId)
    }

    func test_branchSignalOnTerminalsTab_isNoOp() {
        // The pinned Terminals project never hosts Claude sessions, so
        // a hook firing from there would already be a model violation.
        // Guard defensively: no parent tab is materialized even if a
        // resume+rotation message arrives addressed to a Terminals
        // pane.
        let initialTerminalsCount = terminalsProject().tabs.count
        guard let main = terminalsProject().tabs.first else {
            return XCTFail("Terminals project must have a Main tab seeded by AppState init")
        }
        guard let mainPaneId = main.panes.first?.id else {
            return XCTFail("Main terminal tab must have a pane")
        }

        appState.sessions.handleClaudeSessionUpdate(
            paneId: mainPaneId, sessionId: "FRESH", source: "resume"
        )

        XCTAssertEqual(
            terminalsProject().tabs.count, initialTerminalsCount,
            "Terminals project tab count must not change on a spurious branch signal"
        )
    }

    // MARK: - Persistence round-trip

    func test_persistedTab_parentTabId_roundTrips() throws {
        let tab = PersistedTab(
            id: "t-child",
            title: "Child",
            cwd: "/tmp/x",
            branch: nil,
            claudeSessionId: "abc",
            activePaneId: nil,
            panes: [],
            titleManuallySet: nil,
            parentTabId: "t-parent"
        )
        let data = try JSONEncoder().encode(tab)
        let decoded = try JSONDecoder().decode(PersistedTab.self, from: data)
        XCTAssertEqual(decoded.parentTabId, "t-parent")
    }

    func test_persistedTab_legacyJsonWithoutParentTabId_decodesAsNil() throws {
        // v3 sessions.json files written before /branch tracking
        // existed don't carry the field. The optional must decode
        // cleanly to nil so an upgrader's restored tabs all render at
        // root (their original layout) instead of failing to decode.
        let legacyJson = """
        {
          "id": "t-legacy",
          "title": "Legacy",
          "cwd": "/tmp/legacy",
          "branch": null,
          "claudeSessionId": "abc",
          "activePaneId": null,
          "panes": []
        }
        """
        let data = try XCTUnwrap(legacyJson.data(using: .utf8))
        let decoded = try JSONDecoder().decode(PersistedTab.self, from: data)
        XCTAssertNil(decoded.parentTabId)
        XCTAssertEqual(decoded.id, "t-legacy")
    }

    // MARK: - Helpers

    private func seedClaudeTab(
        projectId: String,
        tabId: String,
        sessionId: String,
        title: String? = nil
    ) {
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs,
            projectId: projectId,
            tabId: tabId,
            sessionId: sessionId
        )
        if let title {
            appState.tabs.mutateTab(id: tabId) { $0.title = title }
        }
    }

    private func projectById(_ id: String) -> Project {
        guard let project = appState.tabs.projects.first(where: { $0.id == id }) else {
            XCTFail("project '\(id)' not found")
            return Project(id: id, name: id, path: "/", tabs: [])
        }
        return project
    }

    private func terminalsProject() -> Project {
        guard let project = appState.tabs.projects.first(
            where: { $0.id == TabModel.terminalsProjectId }
        ) else {
            XCTFail("Terminals project must exist")
            return Project(id: "x", name: "x", path: "/", tabs: [])
        }
        return project
    }
}
