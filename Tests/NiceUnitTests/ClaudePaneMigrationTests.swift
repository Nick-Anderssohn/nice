//
//  ClaudePaneMigrationTests.swift
//  NiceUnitTests
//
//  Tests the Claude-pane asymmetry of cross-window migration: a Claude
//  pane can't join an existing tab's pane set (one alive Claude per
//  tab), so it lands as a brand-new sidebar tab under the destination
//  project matched by path (recreated when absent), carrying its
//  `claudeSessionId` and live pty entry. See
//  `SessionsModel.adoptClaudePaneAsNewTab`.
//

import AppKit
import XCTest
@testable import Nice

@MainActor
final class ClaudePaneMigrationTests: XCTestCase {

    /// Seed a Claude tab (Claude + companion terminal) into `appState`
    /// and arm its pty with the Claude pane live. Returns the Claude
    /// pane id.
    @discardableResult
    private func seedClaudeTab(
        into appState: AppState,
        projectId: String,
        projectPath: String,
        tabId: String,
        sessionId: String
    ) -> String {
        let claudeId = "\(tabId)-claude"
        var claudePane = Pane(id: claudeId, title: "Claude", kind: .claude)
        claudePane.isClaudeRunning = true
        let tab = Tab(
            id: tabId, title: "Repo", cwd: projectPath,
            panes: [claudePane, Pane(id: "\(tabId)-t1", title: "Terminal 1", kind: .terminal)],
            activePaneId: claudeId, claudeSessionId: sessionId
        )
        appState.tabs.projects = [
            appState.tabs.projects[0],
            Project(id: projectId, name: projectId.uppercased(), path: projectPath, tabs: [tab]),
        ]
        _ = appState.sessions.makeSession(
            for: tabId, cwd: projectPath, initialClaudePaneId: claudeId
        )
        return claudeId
    }

    func test_adoptClaudePane_recreatesProjectByPath_whenAbsent() {
        let src = AppState()
        let dst = AppState()
        let claudeId = seedClaudeTab(into: src, projectId: "p-repo",
                                     projectPath: "/tmp/repo", tabId: "src-tab",
                                     sessionId: "sess-123")
        let entry = src.sessions.detachLivePane(tabId: "src-tab", paneId: claudeId)!

        let newTabId = dst.sessions.adoptClaudePaneAsNewTab(
            entry: entry, paneId: claudeId, title: "Repo",
            claudeSessionId: "sess-123",
            projectId: "p-repo", projectName: "REPO", projectPath: "/tmp/repo"
        )
        XCTAssertNotNil(newTabId)

        // Project recreated by path, copying the source identity.
        let proj = dst.tabs.projects.first { $0.path == "/tmp/repo" }
        XCTAssertEqual(proj?.id, "p-repo")
        XCTAssertEqual(proj?.tabs.count, 1)

        // New tab shape: [Claude, companion terminal], Claude focused,
        // session id carried.
        let newTab = dst.tabs.tab(for: newTabId!)
        XCTAssertEqual(newTab?.claudeSessionId, "sess-123")
        XCTAssertEqual(newTab?.activePaneId, claudeId)
        XCTAssertEqual(newTab?.panes.count, 2)
        XCTAssertEqual(newTab?.panes.first?.id, claudeId)
        XCTAssertEqual(newTab?.panes.first?.kind, .claude)
        XCTAssertEqual(newTab?.panes.first?.isClaudeRunning, true)
        XCTAssertEqual(newTab?.panes.last?.kind, .terminal)

        // Live entry adopted into the new tab's session, delegate re-pointed.
        XCTAssertEqual(dst.sessions.ptySessions[newTabId!]?.hasPane(claudeId), true)
        XCTAssertEqual(
            dst.sessions.ptySessions[newTabId!]?.entries[claudeId]?.delegate.routedPane?.tabId,
            newTabId
        )
        XCTAssertEqual(dst.tabs.activeTabId, newTabId)
    }

    func test_adoptClaudePane_appendsToExistingProject_byPath() {
        let src = AppState()
        let dst = AppState()
        // Destination already has the same repo open (same path, even a
        // different project id) — the new tab must append there, not
        // duplicate the project.
        let existing = Project(id: "dst-local-id", name: "REPO",
                               path: "/tmp/repo", tabs: [])
        dst.tabs.projects = [dst.tabs.projects[0], existing]

        let claudeId = seedClaudeTab(into: src, projectId: "p-repo",
                                     projectPath: "/tmp/repo", tabId: "src-tab",
                                     sessionId: "sess-9")
        let entry = src.sessions.detachLivePane(tabId: "src-tab", paneId: claudeId)!

        let countBefore = dst.tabs.projects.count
        let newTabId = dst.sessions.adoptClaudePaneAsNewTab(
            entry: entry, paneId: claudeId, title: "Repo",
            claudeSessionId: "sess-9",
            projectId: "p-repo", projectName: "REPO", projectPath: "/tmp/repo"
        )
        XCTAssertEqual(dst.tabs.projects.count, countBefore,
                       "Same-path project must not be duplicated.")
        let proj = dst.tabs.projects.first { $0.path == "/tmp/repo" }
        XCTAssertEqual(proj?.id, "dst-local-id", "Matched the existing project by path.")
        XCTAssertEqual(proj?.tabs.count, 1)
        XCTAssertEqual(proj?.tabs.first?.id, newTabId)
    }
}
