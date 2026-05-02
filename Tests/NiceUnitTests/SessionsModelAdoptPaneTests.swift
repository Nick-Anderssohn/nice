//
//  SessionsModelAdoptPaneTests.swift
//  NiceUnitTests
//
//  Tests for cross-window pane adoption (`SessionsModel.adoptPane`)
//  and the new-tab path (`AppState.absorbAsNewTab`). These exercise
//  model-only moves (no live pty / NiceTerminalView) — the unit-test
//  rig doesn't spawn ptys, so we pass the view as `nil` and verify
//  the tab/pane data layer transitions.
//
//  The view-migration path is exercised manually (smoke test) and via
//  the unit test of `TabPtySession.detachPane` / `attachPane`
//  primitives elsewhere.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class SessionsModelAdoptPaneTests: XCTestCase {

    private var windowA: AppState!
    private var windowB: AppState!

    override func setUp() {
        super.setUp()
        windowA = AppState()
        windowB = AppState()
    }

    override func tearDown() {
        windowA = nil
        windowB = nil
        super.tearDown()
    }

    // MARK: - adoptPane (terminal join existing tab)

    func test_adoptPane_acrossWindows_movesTerminalToDest() {
        let srcTabId = seedTerminalsTab(in: windowA, paneIds: ["t-a", "t-b"])
        let dstTabId = seedTerminalsTab(in: windowB, paneIds: ["t-x"])
        let payload = PaneDragPayload(
            windowSessionId: windowA.windowSession.windowSessionId,
            tabId: srcTabId, paneId: "t-a", kind: .terminal
        )
        let ok = windowB.sessions.adoptPane(
            from: windowA, payload: payload,
            intoTabId: dstTabId, insertAt: nil
        )
        XCTAssertTrue(ok)
        XCTAssertEqual(paneIds(of: windowA, tabId: srcTabId), ["t-b"])
        XCTAssertEqual(paneIds(of: windowB, tabId: dstTabId), ["t-x", "t-a"])
        XCTAssertEqual(activePaneId(of: windowB, tabId: dstTabId), "t-a")
        XCTAssertEqual(windowB.tabs.activeTabId, dstTabId)
    }

    func test_adoptPane_sameWindowCrossTab_movesPane() {
        let src = seedTerminalsTab(in: windowA, paneIds: ["t-a", "t-b"])
        let dst = seedTerminalsTab(in: windowA, paneIds: ["t-x"])
        let payload = PaneDragPayload(
            windowSessionId: windowA.windowSession.windowSessionId,
            tabId: src, paneId: "t-b", kind: .terminal
        )
        XCTAssertTrue(windowA.sessions.adoptPane(
            from: windowA, payload: payload, intoTabId: dst, insertAt: nil
        ))
        XCTAssertEqual(paneIds(of: windowA, tabId: src), ["t-a"])
        XCTAssertEqual(paneIds(of: windowA, tabId: dst), ["t-x", "t-b"])
    }

    func test_adoptPane_claudePayload_isRejected() {
        let src = TabModelFixtures.seedClaudeTab(
            into: windowA.tabs, projectId: "p1", tabId: "t1"
        )
        let dst = seedTerminalsTab(in: windowB, paneIds: ["t-x"])
        let payload = PaneDragPayload(
            windowSessionId: windowA.windowSession.windowSessionId,
            tabId: src.tabId, paneId: src.claudePaneId, kind: .claude
        )
        XCTAssertFalse(windowB.sessions.adoptPane(
            from: windowA, payload: payload, intoTabId: dst, insertAt: nil
        ))
        // Source untouched.
        XCTAssertEqual(
            paneIds(of: windowA, tabId: src.tabId),
            [src.claudePaneId, src.terminalPaneId]
        )
    }

    func test_adoptPane_intoClaudeTab_clampsTerminalToIndex1() {
        let src = seedTerminalsTab(in: windowA, paneIds: ["t-a"])
        let dstSeed = TabModelFixtures.seedClaudeTab(
            into: windowB.tabs, projectId: "p1", tabId: "ct"
        )
        let payload = PaneDragPayload(
            windowSessionId: windowA.windowSession.windowSessionId,
            tabId: src, paneId: "t-a", kind: .terminal
        )
        // Try to insert at slot 0 — should clamp to 1.
        XCTAssertTrue(windowB.sessions.adoptPane(
            from: windowA, payload: payload,
            intoTabId: dstSeed.tabId, insertAt: 0
        ))
        XCTAssertEqual(
            paneIds(of: windowB, tabId: dstSeed.tabId),
            [dstSeed.claudePaneId, "t-a", dstSeed.terminalPaneId]
        )
    }

    func test_adoptPane_emptiesSource_dissolvesTab() {
        // Seed a non-Terminals project with one tab containing one
        // pane. Adopting that one pane should empty + dissolve the
        // source tab. Empty projects are NOT auto-pruned on dissolve
        // (only on explicit close-request) — the empty project row
        // stays in the sidebar until the user closes it.
        let srcProjectId = "src-proj-only"
        let srcPath = "/tmp/src-only"
        let srcTabId = "src-only-tab"
        let srcPaneId = "src-only-pane"
        windowA.tabs.projects.append(Project(
            id: srcProjectId, name: "SRC", path: srcPath,
            tabs: [Tab(
                id: srcTabId, title: "Tab",
                cwd: srcPath, branch: nil,
                panes: [Pane(id: srcPaneId, title: "zsh", kind: .terminal)],
                activePaneId: srcPaneId
            )]
        ))
        let dst = seedTerminalsTab(in: windowB, paneIds: ["t-x"])
        let payload = PaneDragPayload(
            windowSessionId: windowA.windowSession.windowSessionId,
            tabId: srcTabId, paneId: srcPaneId, kind: .terminal
        )
        XCTAssertTrue(windowB.sessions.adoptPane(
            from: windowA, payload: payload,
            intoTabId: dst, insertAt: nil
        ))
        // Source tab dissolved; project still present (empty).
        XCTAssertNil(windowA.tabs.tab(for: srcTabId))
        let srcProj = windowA.tabs.projects.first { $0.id == srcProjectId }
        XCTAssertNotNil(srcProj)
        XCTAssertEqual(srcProj?.tabs.count, 0)
    }

    func test_adoptPane_unknownDestTab_isNoOp() {
        let src = seedTerminalsTab(in: windowA, paneIds: ["t-a"])
        let payload = PaneDragPayload(
            windowSessionId: windowA.windowSession.windowSessionId,
            tabId: src, paneId: "t-a", kind: .terminal
        )
        XCTAssertFalse(windowB.sessions.adoptPane(
            from: windowA, payload: payload,
            intoTabId: "ghost", insertAt: nil
        ))
        XCTAssertEqual(paneIds(of: windowA, tabId: src), ["t-a"])
    }

    // MARK: - absorbAsNewTab (Claude + tear-off path)

    func test_absorbAsNewTab_claude_createsNewTabInDest() {
        let src = TabModelFixtures.seedClaudeTab(
            into: windowA.tabs, projectId: "p-src", tabId: "src-claude",
            sessionId: "session-uuid-123",
            projectPath: "/tmp/p-src"
        )
        guard let sourceTab = windowA.tabs.tab(for: src.tabId),
              let claudePane = sourceTab.panes.first(where: { $0.kind == .claude })
        else { return XCTFail("seed missing") }
        let newTabId = windowB.absorbAsNewTab(
            pane: claudePane,
            sourceTab: sourceTab,
            view: nil,
            projectAnchor: .repoPath("/tmp/p-src"),
            pendingLaunchState: nil
        )
        // Destination has a fresh tab with the Claude pane only.
        guard let newTab = windowB.tabs.tab(for: newTabId)
        else { return XCTFail("new tab missing") }
        XCTAssertEqual(newTab.panes.count, 1)
        XCTAssertEqual(newTab.panes.first?.id, claudePane.id)
        XCTAssertEqual(newTab.panes.first?.kind, .claude)
        XCTAssertEqual(newTab.activePaneId, claudePane.id)
        // claudeSessionId carried over so future restore can --resume.
        XCTAssertEqual(newTab.claudeSessionId, "session-uuid-123")
        // Window's active tab is the new one.
        XCTAssertEqual(windowB.tabs.activeTabId, newTabId)
    }

    func test_absorbAsNewTab_terminal_anchorsToTerminalsProject() {
        // Build a standalone Pane + Tab template (as if torn off).
        let pane = Pane(id: "torn-pane", title: "zsh", kind: .terminal)
        let template = Tab(
            id: "ignored", title: "Tab",
            cwd: NSHomeDirectory(),
            panes: [pane], activePaneId: pane.id
        )
        let newTabId = windowB.absorbAsNewTab(
            pane: pane,
            sourceTab: template,
            view: nil,
            projectAnchor: .terminals,
            pendingLaunchState: nil
        )
        // Lives in the Terminals project.
        let terminalsProj = windowB.tabs.projects.first {
            $0.id == TabModel.terminalsProjectId
        }
        XCTAssertNotNil(terminalsProj)
        XCTAssertTrue(terminalsProj?.tabs.contains(where: { $0.id == newTabId }) ?? false)
    }

    func test_absorbAsNewTab_repoPath_appendsToExistingProjectIfPresent() {
        // Pre-seed a project at /tmp/p-existing.
        let existingProj = Project(
            id: "p-existing", name: "EXISTING", path: "/tmp/p-existing",
            tabs: []
        )
        windowB.tabs.projects.append(existingProj)
        let pane = Pane(id: "pane-x", title: "zsh", kind: .terminal)
        let template = Tab(
            id: "ignored", title: "T",
            cwd: "/tmp/p-existing",
            panes: [pane], activePaneId: pane.id
        )
        let newTabId = windowB.absorbAsNewTab(
            pane: pane, sourceTab: template, view: nil,
            projectAnchor: .repoPath("/tmp/p-existing"),
            pendingLaunchState: nil
        )
        // New tab landed in the existing project, not a fresh one.
        let proj = windowB.tabs.projects.first { $0.id == "p-existing" }
        XCTAssertEqual(proj?.tabs.count, 1)
        XCTAssertEqual(proj?.tabs.first?.id, newTabId)
    }

    func test_absorbAsNewTab_terminal_doesNotCarryClaudeSessionId() {
        let pane = Pane(id: "term", title: "zsh", kind: .terminal)
        let template = Tab(
            id: "ignored", title: "T", cwd: NSHomeDirectory(),
            panes: [pane], activePaneId: pane.id,
            claudeSessionId: "should-not-carry"
        )
        let newTabId = windowB.absorbAsNewTab(
            pane: pane, sourceTab: template, view: nil,
            projectAnchor: .terminals,
            pendingLaunchState: nil
        )
        XCTAssertNil(windowB.tabs.tab(for: newTabId)?.claudeSessionId)
    }

    // MARK: - Fixtures

    @discardableResult
    private func seedTerminalsTab(
        in app: AppState,
        paneIds: [String]
    ) -> String {
        let tabId = "tab-\(UUID().uuidString.prefix(6))"
        let panes = paneIds.map { Pane(id: $0, title: $0, kind: .terminal) }
        let tab = Tab(
            id: tabId, title: "Tab", cwd: "/tmp/\(tabId)",
            panes: panes, activePaneId: panes.first?.id
        )
        app.tabs.projects.append(Project(
            id: "proj-\(tabId)", name: tabId.uppercased(),
            path: "/tmp/\(tabId)", tabs: [tab]
        ))
        return tabId
    }

    private func paneIds(of app: AppState, tabId: String) -> [String] {
        app.tabs.tab(for: tabId)?.panes.map(\.id) ?? []
    }

    private func activePaneId(of app: AppState, tabId: String) -> String? {
        app.tabs.tab(for: tabId)?.activePaneId
    }
}
