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
import os

/// File-scoped logger for migration aborts. Every early-return after the
/// claim routes through this so a no-op is always greppable (graft 1).
/// Shares the "tearoff" category with the tear-off / adopt paths.
private let migrationLog = Logger(
    subsystem: "dev.nickanderssohn.nice", category: "tearoff"
)

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
            migrationLog.error("commitCrossWindowMove aborted: source window \(handle.sourceWindowSessionId, privacy: .public) not found in registry (closed mid-drag?) for pane \(handle.paneId, privacy: .public)")
            return false
        }

        // Snapshot the source context BEFORE mutating the source tree —
        // `extractPane` removes the pane and may shift focus, and we need
        // the pane kind, its title, the tab's Claude session id, and the
        // owning project identity to reconstruct the pane on the far side.
        guard let sourcePane = source.tabs.tab(for: handle.sourceTabId)?
            .panes.first(where: { $0.id == handle.paneId }) else {
            live.withdraw(paneId: handle.paneId)
            migrationLog.error("commitCrossWindowMove aborted: pane \(handle.paneId, privacy: .public) not found in source tab \(handle.sourceTabId, privacy: .public)")
            return false
        }
        let claudeSessionId = source.tabs.tab(for: handle.sourceTabId)?.claudeSessionId
        let sourceProject: (id: String, name: String, path: String)? = {
            guard let (pi, _) = source.tabs.projectTabIndex(for: handle.sourceTabId)
            else { return nil }
            let p = source.tabs.projects[pi]
            return (p.id, p.name, p.path)
        }()

        // Claim the source pane to a `PaneClaim` (one-shot; detaches the
        // live entry for `.live`, or reports `.notSpawned(cwd:)` for a
        // deferred pane). The tri-state forces us to handle the unspawned
        // case explicitly instead of silently no-op'ing (BUG A).
        switch live.claim(paneId: handle.paneId) {
        case nil, .some((_, .gone)):
            // No handle, or the pane is neither live nor modelled. Abort
            // BEFORE any source mutation — extractPane has not run yet.
            live.withdraw(paneId: handle.paneId)
            migrationLog.error("commitCrossWindowMove aborted: claim for pane \(handle.paneId, privacy: .public) returned .gone / no handle — pane already exited")
            return false

        case .some((_, .live(let entry))):
            // Live pane: extract from the source, then adopt the live
            // entry on the far side exactly as before.
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
                // A Claude pane can't join an existing strip (one Claude
                // per tab). Land it as a new tab under the matching
                // project; if the source project identity is somehow
                // unavailable, fall back to the target tab's own project
                // path so the pane still has a home.
                let proj = sourceProject ?? fallbackProject(for: target, tabId: targetTabId)
                target.sessions.adoptClaudePaneAsNewTab(
                    entry: entry, paneId: handle.paneId, title: sourcePane.title,
                    claudeSessionId: claudeSessionId,
                    projectId: proj.id, projectName: proj.name, projectPath: proj.path
                )
            }

        case .some((_, .notSpawned(let cwd))):
            // Deferred pane: extract from the source, then SPAWN it fresh
            // in the destination instead of adopting a (non-existent)
            // live entry. The terminal path uses the SESSION-CREATING
            // `ensurePaneSpawned` (not `ensureActivePaneSpawned`) so a
            // drop into a session-less target tab spawns the pane rather
            // than silently no-op'ing.
            _ = source.tabs.extractPane(handle.paneId, fromTab: handle.sourceTabId)
            switch sourcePane.kind {
            case .terminal:
                target.tabs.insertPane(
                    sourcePane, inTab: targetTabId,
                    relativeTo: relativeToPaneId, placeAfter: placeAfter
                )
                target.tabs.activeTabId = targetTabId
                // Spawn with the CARRIED cwd BEFORE focusing: `setActivePane`
                // would otherwise run `ensureActivePaneSpawned`, which spawns
                // the now-active pane with the TARGET-resolved cwd, losing the
                // source pane's directory. Spawning first means the pane is
                // already live when `setActivePane`'s spawn guard runs, so it
                // no-ops and the claim cwd wins (graft 0).
                target.sessions.ensurePaneSpawned(
                    tabId: targetTabId, paneId: handle.paneId, cwd: cwd
                )
                target.sessions.setActivePane(tabId: targetTabId, paneId: handle.paneId)

            case .claude:
                // A deferred Claude pane lands as a new tab in
                // `.resumeDeferred` mode (entry: nil); without this it
                // would be dropped AFTER extractPane — worse than today.
                let proj = sourceProject ?? fallbackProject(for: target, tabId: targetTabId)
                target.sessions.adoptClaudePaneAsNewTab(
                    entry: nil, paneId: handle.paneId, title: sourcePane.title,
                    claudeSessionId: claudeSessionId,
                    projectId: proj.id, projectName: proj.name, projectPath: proj.path
                )
            }
        }

        // Dissolve the source tab if the move emptied it (existing
        // last-pane rules, multi-window-safe).
        source.dissolveTabIfEmpty(tabId: handle.sourceTabId)

        // Bug 3 (shared gap): `extractPane` shifts the source's active
        // pane to a deferred neighbor without spawning it, and a dissolve
        // may switch focus to a different tab. Spawn whichever tab is
        // active on the source now so it doesn't render blank. No-op when
        // already spawned / Claude / no session.
        if let activeTabId = source.tabs.activeTabId {
            source.sessions.ensureActivePaneSpawned(tabId: activeTabId)
        }
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
