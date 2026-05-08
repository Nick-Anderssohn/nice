//
//  CloseRequestCoordinator.swift
//  Nice
//
//  Per-window close-confirmation flow. Decides whether closing a
//  pane / tab / project needs the "processes still running" alert
//  or can SIGTERM immediately, stages the alert state, and tears
//  things down on confirm.
//
//  Holds weak references to `TabModel` (for tree reads / mutations
//  on the synchronous-dissolve fallback) and `SessionsModel` (for
//  the busy check and `terminatePane` on confirm). Cross-cutting
//  cleanup that doesn't belong here — file-browser store removal,
//  the all-projects-empty terminate, file-browser persistence —
//  stays on `AppState`'s `finalizeDissolvedTab`. Two callbacks
//  bridge back:
//
//  - `onSyncFinalizeDissolve` — `hardKillTab`'s all-unspawned path
//    has no async pane-exit to wait on, so we synchronously empty
//    the tab's panes array and ask AppState to run the dissolve
//    cascade right now.
//  - `onScheduleSave` — `hardKillProject`'s empty-project early
//    return removes a project row directly and needs persistence
//    to fire (no `activeTabId.didSet` to ride on, in general).
//
//  `projectsPendingRemoval` is the set of project ids the user
//  asked to close in full. AppState's `finalizeDissolvedTab` calls
//  `consumeProjectPendingRemoval(_:)` during the dissolve cascade
//  to learn whether the now-empty project row should also be
//  dropped. The Terminals project is excluded upstream; the set
//  never contains its id.
//

import AppKit
import Foundation
import Observation

@MainActor
@Observable
final class CloseRequestCoordinator {
    /// In-flight "processes still running" confirmation. Set by
    /// `requestClosePane` / `requestCloseTab` / `requestCloseProject`
    /// / `requestCloseTabs` when they find something busy; cleared by
    /// `confirmPendingClose` (after the kill) or `cancelPendingClose`
    /// (user backs out). `AppShellView` binds a single `.alert` to
    /// this and switches on `scope` for the body wording, so every
    /// close confirmation goes through the same modal.
    var pendingCloseRequest: PendingCloseRequest?

    /// Project ids the user asked to fully close. When a tab in one of
    /// these projects finishes dissolving in `paneExited`, the empty
    /// project row is also removed from `projects`. The Terminals
    /// project is excluded upstream (its id is never added).
    @ObservationIgnored
    private var projectsPendingRemoval: Set<String> = []

    @ObservationIgnored
    private weak var tabs: TabModel?
    @ObservationIgnored
    private weak var sessions: SessionsModel?

    /// AppState wires this so `hardKillTab`'s synchronous dissolve
    /// path (every pane on the tab was unspawned, so no async
    /// `paneExited` will fire to drive the cascade) can run the
    /// dissolve cleanup right now.
    @ObservationIgnored
    var onSyncFinalizeDissolve: ((_ tabId: String, _ projectIndex: Int, _ tabIndex: Int) -> Void)?

    /// AppState wires this so `hardKillProject`'s empty-project early
    /// return (no tabs to kill, project removed directly) can fire
    /// the debounced persistence save.
    @ObservationIgnored
    var onScheduleSave: (() -> Void)?

    init(tabs: TabModel, sessions: SessionsModel) {
        self.tabs = tabs
        self.sessions = sessions
    }

    /// True iff the user asked to close `projectId` in full and we
    /// haven't yet finalized that removal. Non-mutating; clearing is
    /// `clearProjectPendingRemoval(_:)`. Split into read/clear because
    /// only the dissolve of the *last* tab should clear the flag —
    /// dissolving an earlier tab in a multi-tab project leaves the
    /// rest of the dissolve cascade pending and must not clear it.
    func isProjectPendingRemoval(_ projectId: String) -> Bool {
        projectsPendingRemoval.contains(projectId)
    }

    /// Drop `projectId` from the pending-removal set. Called by
    /// AppState's dissolve cascade once the empty project row has
    /// actually been removed from the tree.
    func clearProjectPendingRemoval(_ projectId: String) {
        projectsPendingRemoval.remove(projectId)
    }

    // MARK: - Public requests

    /// Request to close a pane. If the pane is busy — a thinking or
    /// waiting Claude, or a shell with a foreground child — stage a
    /// confirmation prompt; the UI binds an alert to
    /// `pendingCloseRequest` and calls `confirmPendingClose` /
    /// `cancelPendingClose`. Idle panes are killed immediately.
    func requestClosePane(tabId: String, paneId: String) {
        guard let tab = tabs?.tab(for: tabId),
              let pane = tab.panes.first(where: { $0.id == paneId })
        else { return }

        if isBusy(tabId: tabId, pane: pane) {
            pendingCloseRequest = PendingCloseRequest(
                scope: .pane(tabId: tabId, paneId: paneId),
                busyPanes: [describe(pane: pane)]
            )
        } else {
            hardKillPane(tabId: tabId, paneId: paneId)
        }
    }

    /// Request to close an entire tab. If any live pane on the tab is
    /// busy, show the confirmation alert; otherwise tear all panes
    /// down. The tab dissolves when its last pane exits (see
    /// `SessionsModel.paneExited`).
    func requestCloseTab(tabId: String) {
        guard let tab = tabs?.tab(for: tabId) else { return }

        let busy = tab.panes.filter { $0.isAlive && isBusy(tabId: tabId, pane: $0) }
        if !busy.isEmpty {
            pendingCloseRequest = PendingCloseRequest(
                scope: .tab(tabId: tabId),
                busyPanes: busy.map(describe(pane:))
            )
        } else {
            hardKillTab(tabId: tabId)
        }
    }

    /// Request to close several tabs at once (sidebar multi-select →
    /// "Close N Tabs"). Idle tabs are killed immediately so the user
    /// sees the rows disappear right away. Busy tabs are aggregated
    /// into a single combined `.tabs` confirmation alert instead of
    /// stacking one per-tab prompt — confirming hard-kills every
    /// busy tab in the batch, cancelling leaves them open. The
    /// single-id case forwards to `requestCloseTab` so the existing
    /// `.tab` scope handles it (same alert wording).
    func requestCloseTabs(ids: [String]) {
        guard let tabs else { return }

        // Single-id fast path keeps behavior identical to the
        // singular flow — same alert wording, same Scope.tab branch.
        if ids.count == 1, let only = ids.first {
            requestCloseTab(tabId: only)
            return
        }

        // Defensive: don't stack alerts. SwiftUI presents `.alert`
        // serially per-window; firing a second `requestCloseTabs`
        // while a confirmation is already up would silently win the
        // binding race and replace the prior request without the
        // user noticing. Log so the swallowed call is debuggable.
        if let pending = pendingCloseRequest {
            print("[close] dropped requestCloseTabs(\(ids.count)) — alert already in flight: \(pending.scope)")
            return
        }

        var idle: [String] = []
        var busy: [BusyTabEntry] = []
        for id in ids {
            guard let tab = tabs.tab(for: id) else { continue }
            let busyPanes = tab.panes.filter { $0.isAlive && isBusy(tabId: id, pane: $0) }
            if busyPanes.isEmpty {
                idle.append(id)
            } else {
                busy.append(BusyTabEntry(tabId: id, title: tab.title, panes: busyPanes))
            }
        }

        // The loop iterates `[String]` ids (not project indexes), so
        // a synchronous `hardKillTab` cascade — `onSyncFinalizeDissolve`
        // → `finalizeDissolvedTab` mutating `projects` — can't shift
        // anything out from under us mid-loop.
        for id in idle {
            hardKillTab(tabId: id)
        }

        guard !busy.isEmpty else { return }

        pendingCloseRequest = PendingCloseRequest(
            scope: .tabs(busy.map(\.tabId)),
            busyPanes: busy.map(\.summary)
        )
    }

    /// Local row used by `requestCloseTabs` to bucket each tab in the
    /// batch as it's classified. `summary` is the alert-body line we
    /// hand to `PendingCloseRequest.busyPanes` — pre-formatting it
    /// here keeps the message helper in `AppShellView` symmetric with
    /// the singular scopes (one entry per busy unit).
    private struct BusyTabEntry {
        let tabId: String
        let title: String
        let panes: [Pane]
        var summary: String {
            let panesList = panes.map { pane in
                switch pane.kind {
                case .claude:   return "Claude (\(pane.title))"
                case .terminal: return pane.title
                }
            }.joined(separator: ", ")
            return "\(title) (\(panesList))"
        }
    }

    /// Request to close an entire project: every tab's panes plus the
    /// project row itself. Refused for the pinned Terminals project,
    /// which is always present by design. If any pane in any tab is
    /// busy, show the confirmation alert; otherwise tear everything
    /// down. The project dissolves once its last tab dissolves (see
    /// `SessionsModel.paneExited`).
    func requestCloseProject(projectId: String) {
        guard let tabs,
              projectId != TabModel.terminalsProjectId,
              let project = tabs.projects.first(where: { $0.id == projectId })
        else { return }

        let busy = project.tabs.flatMap { tab in
            tab.panes.filter { $0.isAlive && isBusy(tabId: tab.id, pane: $0) }
        }
        if !busy.isEmpty {
            pendingCloseRequest = PendingCloseRequest(
                scope: .project(projectId: projectId),
                busyPanes: busy.map(describe(pane:))
            )
        } else {
            hardKillProject(projectId: projectId)
        }
    }

    /// User confirmed the pending close — force the kill. For a
    /// `.tabs` batch, this only acts on the BUSY survivors; idle
    /// tabs in the original batch were already torn down when the
    /// alert went up (see `requestCloseTabs`).
    func confirmPendingClose() {
        guard let pending = pendingCloseRequest else { return }
        pendingCloseRequest = nil
        switch pending.scope {
        case let .pane(tabId, paneId):
            hardKillPane(tabId: tabId, paneId: paneId)
        case let .tab(tabId):
            hardKillTab(tabId: tabId)
        case let .project(projectId):
            hardKillProject(projectId: projectId)
        case let .tabs(tabIds):
            for id in tabIds {
                hardKillTab(tabId: id)
            }
        }
    }

    /// User dismissed the pending close — leave everything running.
    /// For a `.tabs` batch, this leaves the busy survivors alive;
    /// the idle tabs already closed before the alert.
    func cancelPendingClose() {
        pendingCloseRequest = nil
    }

    // MARK: - Busy classification

    private func isBusy(tabId: String, pane: Pane) -> Bool {
        guard pane.isAlive else { return false }
        switch pane.kind {
        case .claude:
            // `.thinking` is an active computation; `.waiting` is a live
            // conversation the user might not want to throw away. Only
            // the pre-first-title `.idle` state counts as disposable.
            return pane.status == .thinking || pane.status == .waiting
        case .terminal:
            return sessions?.shellHasForegroundChild(tabId: tabId, paneId: pane.id) ?? false
        }
    }

    private func describe(pane: Pane) -> String {
        switch pane.kind {
        case .claude:   return "Claude (\(pane.title))"
        case .terminal: return pane.title
        }
    }

    // MARK: - Hard-kill

    private func hardKillPane(tabId: String, paneId: String) {
        // `terminatePane` sends SIGTERM and tears down the pty; the
        // usual `paneExited` delegate fires and removes the pane from
        // the model, dissolving the tab if it was the last pane.
        sessions?.terminatePane(tabId: tabId, paneId: paneId)
    }

    private func hardKillTab(tabId: String) {
        guard let tabs, let sessions, let tab = tabs.tab(for: tabId) else { return }

        // Split panes by whether they've actually been spawned.
        // `terminatePane` is a no-op for unspawned panes (the lazy
        // companion terminal on a Claude tab the user never focused,
        // for example), so if we only SIGHUP we'd leave those panes
        // in the model and the tab would never dissolve — on Claude
        // tabs `ensureActivePaneSpawned` would then start the
        // companion shell and the tab would keep living as a
        // terminal. Drop unspawned panes from the model directly so
        // the tab reaches empty-panes and dissolves.
        var spawnedIds: [String] = []
        var unspawnedIds: [String] = []
        for pane in tab.panes {
            if sessions.paneIsSpawned(tabId: tabId, paneId: pane.id) {
                spawnedIds.append(pane.id)
            } else {
                unspawnedIds.append(pane.id)
            }
        }

        // Drop unspawned rows from the model BEFORE terminating the
        // spawned ones. Held panes (process already dead, view kept
        // around so the user could read the exit output) take a
        // synchronous fast path through `terminatePane` →
        // `paneExited`; if we left their unspawned siblings in the
        // panes array at that moment, the post-removal panes-empty
        // check inside `paneExited` would fail and `onTabBecameEmpty`
        // would never fire — the panes would vanish but the sidebar
        // tab would stay. The order also works for the all-async path
        // (live ptys whose exits fire later): pruning the unspawned
        // rows here is what the comment used to say happened "later"
        // in the else-branch — same outcome, just rephrased to a
        // single up-front step that's correct regardless of whether
        // the spawned-pane exits sync or async.
        if !unspawnedIds.isEmpty {
            if spawnedIds.isEmpty {
                // Nothing async (or sync) to hook into — finalize now.
                tabs.mutateTab(id: tabId) { tab in
                    tab.panes.removeAll()
                    tab.activePaneId = nil
                }
                if let (pi, ti) = tabs.projectTabIndex(for: tabId) {
                    onSyncFinalizeDissolve?(tabId, pi, ti)
                }
                return
            }
            let toDrop = Set(unspawnedIds)
            tabs.mutateTab(id: tabId) { tab in
                tab.panes.removeAll { toDrop.contains($0.id) }
                if let active = tab.activePaneId, toDrop.contains(active) {
                    tab.activePaneId = tab.panes.first?.id
                }
            }
        }

        for id in spawnedIds {
            sessions.terminatePane(tabId: tabId, paneId: id)
        }
    }

    /// Force-close every tab in a project and mark the project for
    /// removal so `paneExited` drops the empty row once the last tab
    /// dissolves. Empty projects (no tabs) are removed synchronously
    /// since there's no async pane-exit to wait on.
    private func hardKillProject(projectId: String) {
        guard let tabs,
              projectId != TabModel.terminalsProjectId,
              let idx = tabs.projects.firstIndex(where: { $0.id == projectId })
        else { return }

        let tabIds = tabs.projects[idx].tabs.map(\.id)
        if tabIds.isEmpty {
            tabs.projects.remove(at: idx)
            if let active = tabs.activeTabId, tabs.tab(for: active) == nil {
                tabs.activeTabId = tabs.firstAvailableTabId()
            }
            onScheduleSave?()
            return
        }

        projectsPendingRemoval.insert(projectId)
        for id in tabIds {
            hardKillTab(tabId: id)
        }
    }
}
