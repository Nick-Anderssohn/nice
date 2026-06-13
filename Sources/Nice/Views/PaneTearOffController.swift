//
//  PaneTearOffController.swift
//  Nice
//
//  Orchestrates tearing a live pane out of its source window and
//  opening it in a brand-new window. This is the model/lifecycle
//  counterpart to the AppKit drag trigger in `WindowToolbarView` —
//  the drag code calls `tearOff(…)` once the gesture ends over
//  empty desktop and the controller handles everything from there.
//
//  Structure mirrors `PaneMigrationCoordinator.commitCrossWindowMove`:
//    1. Resolve the source `AppState` through `WindowRegistry`.
//    2. Snapshot the pane's kind, title, and project identity BEFORE
//       any mutation (extractPane may shift focus; snapshot first).
//    3. `claim(paneId:)` — detaches the live pty entry one-shot.
//    4. `extractPane` — removes the pane model from the source tab.
//    5. Enqueue a `PendingTearOff` seed on `NiceServices` under a
//       freshly-minted UUID token so the window opened for that token
//       (and only that window) can adopt the entry.
//    6. Call `openWindow(token)` — the SwiftUI scene action, passed in
//       as a closure because a struct can't read `@Environment`
//       directly. Deferred one runloop turn (see below) so a new window
//       is never born mid-`NSDraggingSession`.
//    7. `dissolveTabIfEmpty` — dissolve the source tab if it's now
//       empty (multi-window-safe; won't terminate while another window
//       exists).
//
//  Owned by whoever drives the tear-off gesture (e.g. the pill drag
//  handler in `WindowToolbarView`). Constructed on demand from the
//  app-global `NiceServices`.
//

import AppKit
import os

/// File-scoped logger for tear-off aborts. Every early-return in
/// `tearOff` routes through this so a no-op is always greppable (graft
/// 1: no silent no-ops). Shares the "tearoff" category with the adopt
/// paths in `SessionsModel`.
private let tearOffLog = Logger(
    subsystem: "dev.nickanderssohn.nice", category: "tearoff"
)

@MainActor
struct PaneTearOffController {
    let services: NiceServices

    /// Execute the tear-off: detach the live pane from its source
    /// window, enqueue a seed for the new window, open it, then
    /// dissolve the now-possibly-empty source tab.
    ///
    /// - Parameters:
    ///   - paneId: The id of the pane being torn off.
    ///   - sourceWindowSessionId: `windowSessionId` of the window the
    ///     pane is leaving.
    ///   - screenPoint: The screen coordinate where the new window's
    ///     origin should be placed (the drag-release point).
    ///   - openWindow: The SwiftUI `openWindow(id:value:)` action wrapped
    ///     in a closure taking the pairing token. Callers pass
    ///     `{ token in openWindow(id: "main", value: token) }` — this
    ///     lets the struct invoke the action without needing
    ///     `@Environment` access, while pairing the new window to the
    ///     seed deposited under `token`.
    func tearOff(
        paneId: String,
        sourceWindowSessionId: String,
        at screenPoint: NSPoint,
        openWindow: @escaping (String) -> Void
    ) {
        let live = services.livePaneRegistry

        // Resolve the source AppState. If the window is already gone
        // (e.g. closed mid-drag), discard the handle and bail.
        guard let source = services.registry.appState(
            forSessionId: sourceWindowSessionId
        ) else {
            live.withdraw(paneId: paneId)
            tearOffLog.error("tearOff aborted: source window \(sourceWindowSessionId, privacy: .public) not found in registry (closed mid-drag?) for pane \(paneId, privacy: .public)")
            return
        }

        // Read the sourceTabId from the registry handle BEFORE calling
        // `claim` — claim removes the handle. `handle(forPaneId:)` is
        // the non-destructive lookup.
        guard let handle = live.handle(forPaneId: paneId) else {
            tearOffLog.error("tearOff aborted: no live handle registered for pane \(paneId, privacy: .public) (already withdrawn / never published)")
            return
        }
        let sourceTabId = handle.sourceTabId

        // Snapshot the source context BEFORE mutating the source tree.
        // `extractPane` can shift focus and may dissolve neighbors; all
        // the values we need for the seed must be read from the
        // current (pre-mutation) model.
        guard let sourcePane = source.tabs.tab(for: sourceTabId)?
            .panes.first(where: { $0.id == paneId })
        else {
            live.withdraw(paneId: paneId)
            tearOffLog.error("tearOff aborted: pane \(paneId, privacy: .public) not found in source tab \(sourceTabId, privacy: .public)")
            return
        }

        let claudeSessionId = source.tabs.tab(for: sourceTabId)?.claudeSessionId
        let sourceProject: (id: String, name: String, path: String)? = {
            guard let (pi, _) = source.tabs.projectTabIndex(for: sourceTabId)
            else { return nil }
            let p = source.tabs.projects[pi]
            return (p.id, p.name, p.path)
        }()
        // Fall back to a home-directory identity when the source
        // project can't be resolved (defensive; shouldn't happen in
        // practice since the pane came from a real project tab).
        let proj: (id: String, name: String, path: String) = sourceProject ?? {
            let home = NSHomeDirectory()
            return (
                "p-\(UUID().uuidString.prefix(8).lowercased())",
                (home as NSString).lastPathComponent.uppercased(),
                home
            )
        }()

        // One-shot claim: resolves the source pane to a `PaneClaim` and
        // removes the handle from the registry (clearing `currentDrag`).
        // `seedEntry` is the live entry for the `.live` case and nil for
        // the `.notSpawned` case (the destination spawns it fresh). The
        // seed always carries a usable `cwd` (graft 0): the carried cwd
        // for `.notSpawned`, the source pane's resolved cwd for `.live`,
        // so a deferred pane tears off into the right directory.
        let seedEntry: TabPtySession.PaneEntry?
        let seedCwd: String
        switch live.claim(paneId: paneId) {
        case nil, .some((_, .gone)):
            // No handle registered, or the pane is neither live nor
            // modelled (already exited). Withdraw defensively (claim with
            // a registered handle already removed it; nil leaves nothing
            // to withdraw) and abort BEFORE any source mutation.
            live.withdraw(paneId: paneId)
            tearOffLog.error("tearOff aborted: claim for pane \(paneId, privacy: .public) returned .gone / no handle — pane already exited")
            return
        case .some((_, .live(let entry))):
            seedEntry = entry
            // The source tab still exists here (extractPane runs below);
            // resolve the live pane's cwd so the seed carries a usable
            // directory even though the destination adopts a live entry.
            seedCwd = source.tabs.tab(for: sourceTabId).map {
                source.tabs.resolvedSpawnCwd(for: $0, pane: sourcePane)
            } ?? proj.path
        case .some((_, .notSpawned(let cwd))):
            seedEntry = nil
            seedCwd = cwd
        }

        // Remove the pane model from the source tab.
        _ = source.tabs.extractPane(paneId, fromTab: sourceTabId)

        // Mint a fresh pairing token and enqueue the seed under it. Only
        // the window SwiftUI opens for THIS token will find the seed
        // (`consumeTearOffSeed(token:)`), so a ⌘N / restore window opened
        // concurrently can never steal it.
        let tearOffToken = UUID().uuidString
        services.enqueueTearOff(NiceServices.PendingTearOff(
            entry: seedEntry,
            paneId: paneId,
            title: sourcePane.title,
            kind: sourcePane.kind,
            claudeSessionId: claudeSessionId,
            projectId: proj.id,
            projectName: proj.name,
            projectPath: proj.path,
            cwd: seedCwd,
            screenPoint: screenPoint
        ), token: tearOffToken)

        // Open the new window, DEFERRED one runloop turn so it is never
        // born mid-`NSDraggingSession` (graft 2/3 — AppKit re-finalizing
        // a window's properties while a drag session is unwinding is the
        // BUG C race). The seed is already deposited synchronously above,
        // so by the time SwiftUI builds the new `AppShellHost` and its
        // `.task` calls `consumeTearOffSeed(token:)`, the paired seed is
        // waiting. The dissolve + respawn epilogue below stays
        // synchronous — it runs before this deferred open.
        DispatchQueue.main.async { [openWindow, tearOffToken] in
            openWindow(tearOffToken)
        }

        // Dissolve the source tab if tearing off the last pane left it
        // empty. Multi-window-safe: won't terminate the app while
        // another window (the one we just opened) is alive.
        source.dissolveTabIfEmpty(tabId: sourceTabId)

        // Bug 3: `extractPane` shifts `activePaneId` to a neighbor (and a
        // dissolve may switch focus to a different tab) WITHOUT spawning
        // it — a deferred companion terminal that becomes active would
        // otherwise render blank. Spawn whichever tab is active now (the
        // in-tab neighbor, or the post-dissolve tab). No-op when the
        // active pane is already spawned / is a Claude pane / has no
        // session.
        if let activeTabId = source.tabs.activeTabId {
            source.sessions.ensureActivePaneSpawned(tabId: activeTabId)
        }
    }
}
