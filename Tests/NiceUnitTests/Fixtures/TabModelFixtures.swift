//
//  TabModelFixtures.swift
//  NiceUnitTests
//
//  Shared seed helpers for tests that build a `TabModel.projects` tree
//  by hand — close-project, pane-lifecycle, claude-session-update,
//  file-browser, and rename tests all want the same shape (a project
//  with a Claude+terminal tab) without driving the real pty path.
//
//  Helpers operate on a `TabModel` directly so callers can pass either
//  `appState.tabs` or a stand-alone `TabModel(initialMainCwd:)`.
//

import Foundation
@testable import Nice

@MainActor
enum TabModelFixtures {

    /// IDs minted alongside the tab so callers can reach the panes
    /// without re-deriving the strings.
    struct SeededClaudeTab {
        let tabId: String
        let claudePaneId: String
        let terminalPaneId: String
    }

    /// Seed a Claude + terminal tab into `tabs.projects` under
    /// `projectId`. Mirrors the shape `createTabFromMainTerminal`
    /// produces but stays purely in the model layer (no pty, no
    /// socket).
    ///
    /// - Parameters:
    ///   - tabs: `TabModel` to mutate.
    ///   - projectId: Project to create or append to.
    ///   - tabId: Stable tab id; used to derive pane ids.
    ///   - sessionId: `Tab.claudeSessionId` to seed; defaults to a
    ///     deterministic value derived from `tabId`.
    ///   - projectName: `Project.name`; defaults to `projectId`
    ///     uppercased.
    ///   - projectPath: `Project.path` and `Tab.cwd`; defaults to
    ///     `/tmp/<projectId>`.
    ///   - appendToExisting: When `true`, append to the existing
    ///     project of that id instead of creating a fresh one.
    ///   - isClaudeRunning: Initial value of the seeded Claude pane's
    ///     `isClaudeRunning` flag. Defaults to `true` to match
    ///     production's `createTab*` paths (every live-Claude spawn
    ///     site sets this true synchronously before the pty starts)
    ///     and to satisfy `paneTitleChanged`'s `isClaudeRunning` gate,
    ///     which existing braille/sparkle/auto-title tests now
    ///     depend on — without the gate, restored deferred-resume
    ///     panes' zsh OSC titles would clobber the persisted Claude
    ///     session label. Pass `false` to exercise the deferred-
    ///     resume / `/branch` parent path explicitly.
    @discardableResult
    static func seedClaudeTab(
        into tabs: TabModel,
        projectId: String,
        tabId: String,
        sessionId: String? = nil,
        projectName: String? = nil,
        projectPath: String? = nil,
        appendToExisting: Bool = false,
        isClaudeRunning: Bool = true
    ) -> SeededClaudeTab {
        let claudePaneId = "\(tabId)-claude"
        let terminalPaneId = "\(tabId)-t1"
        let resolvedPath = projectPath ?? "/tmp/\(projectId)"
        var claudePane = Pane(id: claudePaneId, title: "Claude", kind: .claude)
        claudePane.isClaudeRunning = isClaudeRunning
        let tab = Tab(
            id: tabId,
            title: "New tab",
            cwd: resolvedPath,
            branch: nil,
            panes: [
                claudePane,
                Pane(id: terminalPaneId, title: "Terminal 1", kind: .terminal),
            ],
            activePaneId: claudePaneId,
            claudeSessionId: sessionId ?? "session-\(tabId)"
        )
        if appendToExisting,
           let pi = tabs.projects.firstIndex(where: { $0.id == projectId }) {
            tabs.projects[pi].tabs.append(tab)
        } else {
            let project = Project(
                id: projectId,
                name: projectName ?? projectId.uppercased(),
                path: resolvedPath, tabs: [tab]
            )
            tabs.projects.append(project)
        }
        return SeededClaudeTab(
            tabId: tabId,
            claudePaneId: claudePaneId,
            terminalPaneId: terminalPaneId
        )
    }

    /// Seed a Claude + terminal tab under a fresh project with
    /// UUID-based ids. Useful when callers want multiple distinct
    /// tabs without having to mint ids by hand. Returns the tab id.
    @discardableResult
    static func injectClaudeTab(
        into tabs: TabModel,
        projectName: String = "TestProject"
    ) -> String {
        let uid = UUID().uuidString
        return seedClaudeTab(
            into: tabs,
            projectId: "p-\(uid)",
            tabId: "t-\(uid)",
            projectName: projectName,
            projectPath: "/tmp/\(projectName)"
        ).tabId
    }

    /// Inject a tab with a single pane (Claude or terminal) under a
    /// project keyed by path. Generates fresh UUIDs for tab and pane
    /// ids so callers can mint multiple distinct tabs in the same
    /// test. Appends to an existing project at `projectPath` when one
    /// is present.
    @discardableResult
    static func injectTab(
        into tabs: TabModel,
        title: String = "New tab",
        projectPath: String,
        kind: PaneKind = .terminal
    ) -> String {
        let uid = UUID().uuidString
        let tabId = "t-\(uid)"
        let paneId: String
        let paneTitle: String
        switch kind {
        case .claude:
            paneId = "\(tabId)-claude"
            paneTitle = "Claude"
        case .terminal:
            paneId = "\(tabId)-term"
            paneTitle = "Terminal"
        }
        let tab = Tab(
            id: tabId,
            title: title,
            cwd: projectPath,
            branch: nil,
            panes: [Pane(id: paneId, title: paneTitle, kind: kind)],
            activePaneId: paneId
        )
        if let idx = tabs.projects.firstIndex(where: { $0.path == projectPath }) {
            tabs.projects[idx].tabs.append(tab)
        } else {
            let project = Project(
                id: "p-\(uid)",
                name: (projectPath as NSString).lastPathComponent,
                path: projectPath,
                tabs: [tab]
            )
            tabs.projects.append(project)
        }
        return tabId
    }

    /// Append a bare project to the sidebar with a single seed
    /// terminal tab. Used by bucketing/repair tests that need an
    /// existing non-Terminals group as a baseline.
    static func seedTerminalProject(
        into tabs: TabModel,
        id: String,
        name: String,
        path: String
    ) {
        let project = Project(id: id, name: name, path: path, tabs: [
            Tab(
                id: "\(id)-seed", title: "seed", cwd: path,
                panes: [Pane(id: "\(id)-seed-p0", title: "zsh", kind: .terminal)],
                activePaneId: "\(id)-seed-p0"
            )
        ])
        tabs.projects.append(project)
    }

    /// Flip every claude pane inside `projectId` to `status`. Used to
    /// force the `isBusy` path for tests that want a pending-close
    /// alert instead of an immediate tear-down.
    static func setClaudeStatusOnEveryTab(
        in tabs: TabModel,
        projectId: String,
        status: TabStatus
    ) {
        var projects = tabs.projects
        guard let pi = projects.firstIndex(where: { $0.id == projectId }) else { return }
        for ti in projects[pi].tabs.indices {
            for pxi in projects[pi].tabs[ti].panes.indices
            where projects[pi].tabs[ti].panes[pxi].kind == .claude {
                projects[pi].tabs[ti].panes[pxi].status = status
            }
        }
        tabs.projects = projects
    }
}
