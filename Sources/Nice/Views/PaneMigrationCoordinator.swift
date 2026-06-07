//
//  PaneMigrationCoordinator.swift
//  Nice
//
//  Orchestrates moving a LIVE pane from one window to another (and, in a
//  later increment, tearing it off into a new window). Sits above the
//  per-window `AppState`s: it reaches the source window through
//  `WindowRegistry` (by the dragged pane's `sourceWindowSessionId`),
//  claims the live `PaneEntry` from `LivePaneRegistry`, removes the pane
//  model from the source tab, and inserts it into the target — as a
//  strip pill for terminal panes, or as a fresh sidebar tab for Claude
//  panes (which are one-per-tab).
//
//  Keeping this off `AppState` avoids giving either window's state object
//  a reference to the other; the coordinator is the one place that holds
//  both sides at once, created on demand from the app-global
//  `NiceServices`.
//

import AppKit

@MainActor
struct PaneMigrationCoordinator {
    let services: NiceServices

    /// Commit a cross-window MOVE of the in-flight dragged pane into
    /// `target`'s tab `targetTabId`. Terminal panes insert at the
    /// resolved slot (`relativeToPaneId` / `placeAfter`, a nil target
    /// appends); Claude panes ignore the slot and become a new sidebar
    /// tab under the matching project. Returns true when a migration
    /// happened.
    ///
    /// No-op (returns false) when there's no in-flight drag, when the
    /// drag originated in `target` itself (that's an intra-window
    /// reorder — handled by the normal `.onDrop` path), or when the
    /// source window / pane can't be resolved.
    @discardableResult
    func commitCrossWindowMove(
        into target: AppState,
        targetTabId: String,
        relativeToPaneId: String?,
        placeAfter: Bool
    ) -> Bool {
        let live = services.livePaneRegistry
        guard let handle = live.currentDrag else { return false }
        guard handle.sourceWindowSessionId != target.windowSession.windowSessionId
        else { return false }
        guard let source = services.registry.appState(
            forSessionId: handle.sourceWindowSessionId
        ) else {
            live.withdraw(paneId: handle.paneId)
            return false
        }

        // Snapshot the source context BEFORE mutating the source tree —
        // `extractPane` removes the pane and may shift focus, and we need
        // the pane kind, its title, the tab's Claude session id, and the
        // owning project identity to reconstruct the pane on the far side.
        guard let sourcePane = source.tabs.tab(for: handle.sourceTabId)?
            .panes.first(where: { $0.id == handle.paneId }) else {
            live.withdraw(paneId: handle.paneId)
            return false
        }
        let claudeSessionId = source.tabs.tab(for: handle.sourceTabId)?.claudeSessionId
        let sourceProject: (id: String, name: String, path: String)? = {
            guard let (pi, _) = source.tabs.projectTabIndex(for: handle.sourceTabId)
            else { return nil }
            let p = source.tabs.projects[pi]
            return (p.id, p.name, p.path)
        }()

        // Claim the live pty entry (this detaches it from the source
        // session without killing the process), then remove the pane
        // model from the source tab.
        guard let (_, entry) = live.claim(paneId: handle.paneId) else { return false }
        _ = source.tabs.extractPane(handle.paneId, fromTab: handle.sourceTabId)

        switch sourcePane.kind {
        case .terminal:
            target.tabs.insertPane(
                sourcePane, inTab: targetTabId,
                relativeTo: relativeToPaneId, placeAfter: placeAfter
            )
            target.sessions.adoptLivePane(
                tabId: targetTabId, paneId: handle.paneId, entry: entry
            )
            target.tabs.activeTabId = targetTabId
            target.sessions.setActivePane(tabId: targetTabId, paneId: handle.paneId)

        case .claude:
            // A Claude pane can't join an existing strip (one Claude per
            // tab). Land it as a new tab under the matching project; if
            // the source project identity is somehow unavailable, fall
            // back to the target tab's own project path so the pane still
            // has a home.
            let proj = sourceProject ?? fallbackProject(for: target, tabId: targetTabId)
            target.sessions.adoptClaudePaneAsNewTab(
                entry: entry, paneId: handle.paneId, title: sourcePane.title,
                claudeSessionId: claudeSessionId,
                projectId: proj.id, projectName: proj.name, projectPath: proj.path
            )
        }

        // Dissolve the source tab if the move emptied it (existing
        // last-pane rules, multi-window-safe).
        source.dissolveTabIfEmpty(tabId: handle.sourceTabId)
        return true
    }

    /// Project identity to file a Claude pane under when the source
    /// project can't be resolved. Uses the target tab's owning project,
    /// or a synthesized home-rooted identity as a last resort.
    private func fallbackProject(
        for target: AppState, tabId: String
    ) -> (id: String, name: String, path: String) {
        if let (pi, _) = target.tabs.projectTabIndex(for: tabId) {
            let p = target.tabs.projects[pi]
            return (p.id, p.name, p.path)
        }
        let home = NSHomeDirectory()
        return ("p-\(UUID().uuidString.prefix(8).lowercased())",
                (home as NSString).lastPathComponent.uppercased(), home)
    }
}
