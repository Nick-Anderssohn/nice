//
//  AppState.swift
//  Nice
//
//  Per-window composition root. Holds the six sub-models, wires
//  their callbacks, and runs `start()` / `tearDown()`. Views and
//  tests address sub-models directly — only composition-root
//  concerns live here (lifecycle, dissolve cascade, the cross-
//  cutting `toggleFileBrowserHiddenFiles`).
//

import AppKit
import Foundation
import Observation
import SwiftUI

/// Pending "processes still running" confirmation. Lives outside
/// `AppState` so it can be used as an `Identifiable` for SwiftUI's
/// item-based `.alert`. One field, four scope kinds — the alert body
/// in `AppShellView.pendingCloseMessage` switches on `scope` so a
/// single `.alert(...)` block covers every close confirmation in
/// the app (singular pane / tab / project, and the multi-tab batch
/// from sidebar multi-select).
struct PendingCloseRequest: Identifiable, Equatable {
    enum Scope: Equatable {
        case pane(tabId: String, paneId: String)
        case tab(tabId: String)
        case project(projectId: String)
        /// Multi-tab batch from sidebar multi-select. The list
        /// contains only the BUSY tabs — idle ones in the original
        /// batch were torn down synchronously before the alert went
        /// up. `busyPanes` carries one human-readable line per tab,
        /// formatted as `"TabTitle (Pane1, Pane2)"`.
        case tabs([String])
    }

    let id = UUID()
    let scope: Scope
    /// Human-readable descriptions of the busy work for the alert
    /// body. For `.pane` / `.tab` / `.project` scopes this is one
    /// entry per busy pane (`"Claude (foo)"`, `"Terminal 1"`); for
    /// `.tabs` it is one entry per busy tab, each a pre-formatted
    /// `"TabTitle (Pane1, Pane2)"` summary.
    let busyPanes: [String]
}

/// Per-pane placeholder lifecycle. `pending` is set the instant a pane is
/// spawned; if the child emits its first byte before the 0.75 s grace
/// window elapses the entry is cleared and the overlay never appears. If
/// the grace window elapses first the entry is promoted to `visible` and
/// the "Launching…" overlay shows with the captured command string. On
/// first byte (or pane exit) the entry is removed entirely.
enum PaneLaunchStatus: Equatable {
    case pending(command: String)
    case visible(command: String)
}

@MainActor
@Observable
final class AppState {
    let tabs: TabModel
    let sessions: SessionsModel
    let sidebar: SidebarModel
    let closer: CloseRequestCoordinator
    let windowSession: WindowSession
    let fileExplorerOrchestrator: FileExplorerOrchestrator

    /// Per-window file-browser states keyed by `Tab.id`. Removed in
    /// `finalizeDissolvedTab` when a tab dissolves.
    let fileBrowserStore: FileBrowserStore = FileBrowserStore()

    /// Sidebar multi-tab selection. Transient — not persisted with the
    /// rest of the tab tree. Pruned in `finalizeDissolvedTab` so a
    /// dissolved tab can never linger as a stale id in the set.
    let tabSelection: SidebarTabSelection = SidebarTabSelection()

    @ObservationIgnored
    private weak var trackedServices: NiceServices?

    @ObservationIgnored
    private var started = false

    /// Convenience init for previews and tests.
    convenience init() {
        self.init(
            services: nil,
            initialSidebarCollapsed: false,
            initialSidebarMode: .tabs,
            initialMainCwd: nil,
            windowSessionId: UUID().uuidString
        )
    }

    init(
        services: NiceServices?,
        initialSidebarCollapsed: Bool,
        initialSidebarMode: SidebarMode = .tabs,
        initialMainCwd: String?,
        windowSessionId: String,
        fileExplorer: FileExplorerServices? = nil,
        store: SessionStorePersisting? = nil
    ) {
        self.sidebar = SidebarModel(
            initialCollapsed: initialSidebarCollapsed,
            initialMode: initialSidebarMode
        )

        let resolvedMainCwd = initialMainCwd ?? NSHomeDirectory()

        // Build tabs first (init seeds Terminals + Main), then sessions
        // (weak ref back to tabs); closer and windowSession also hold
        // weak refs to the models above — AppState owns all six.
        self.tabs = TabModel(initialMainCwd: resolvedMainCwd)
        self.sessions = SessionsModel(tabs: tabs)
        self.closer = CloseRequestCoordinator(tabs: tabs, sessions: sessions)
        // Persistence is enabled when the caller wired up real
        // services, OR when a test injected a `store` (so unit tests
        // can pin the per-window-close → flush chain end-to-end
        // without spinning up real NiceServices). When `store` is
        // nil, WindowSession picks up `SessionStore.shared`.
        self.windowSession = WindowSession(
            tabs: tabs,
            sessions: sessions,
            sidebar: sidebar,
            windowSessionId: windowSessionId,
            persistenceEnabled: services != nil || store != nil,
            store: store ?? SessionStore.shared
        )
        self.fileExplorerOrchestrator = FileExplorerOrchestrator(
            fileExplorer: fileExplorer ?? services?.fileExplorer,
            tweaks: services?.tweaks,
            editorDetector: services?.editorDetector
        )
        self.fileExplorerOrchestrator.tabs = tabs
        self.fileExplorerOrchestrator.sessions = sessions
        self.fileExplorerOrchestrator.windowSession = windowSession
        self.trackedServices = services

        // Seed theme/palette/font from `Tweaks` so the first
        // `makeSession` (Terminals tab, in `start()`) paints with
        // user prefs. Without this the launch shows a visible
        // re-theme flash and chrome-coupled Defaults mis-theme
        // because their bg/fg reads the session's stale palette.
        if let tweaks = services?.tweaks {
            sessions.updateScheme(
                tweaks.scheme,
                palette: tweaks.activeChromePalette,
                accent: tweaks.accent.nsColor
            )
            sessions.updateTerminalFontFamily(tweaks.terminalFontFamily)
            if let catalog = services?.terminalThemeCatalog {
                sessions.updateTerminalTheme(
                    tweaks.effectiveTerminalTheme(
                        for: tweaks.scheme,
                        catalog: catalog
                    )
                )
            }
        }
        if let fontSize = services?.fontSettings.terminalFontSize {
            sessions.updateTerminalFontSize(fontSize)
        }

        // Preview/test path: `services == nil` means `start()` is
        // unlikely to be called, so seed `NICE_CLAUDE_OVERRIDE` here.
        // Production reads it in `start()` after `bootstrap()`.
        if services == nil {
            sessions.setResolvedClaudePath(
                ProcessInfo.processInfo.environment["NICE_CLAUDE_OVERRIDE"]
            )
        }

        // Wire callbacks last so any `didSet` triggered by seed
        // assignment bounces through a fully-constructed AppState.
        // Save gates live on `windowSession`.
        self.tabs.onTreeMutation = { [weak self] in
            self?.windowSession.scheduleSessionSave()
        }
        self.sessions.onSessionMutation = { [weak self] in
            self?.windowSession.scheduleSessionSave()
        }
        self.sessions.onTabBecameEmpty = { [weak self] tabId, pi, ti in
            self?.finalizeDissolvedTab(projectIndex: pi, tabIndex: ti, tabId: tabId)
        }
        self.closer.onSyncFinalizeDissolve = { [weak self] tabId, pi, ti in
            self?.finalizeDissolvedTab(projectIndex: pi, tabIndex: ti, tabId: tabId)
        }
        self.closer.onScheduleSave = { [weak self] in
            self?.windowSession.scheduleSessionSave()
        }
    }

    /// Bring the per-window subsystem online: control socket, seed
    /// Main pty, snapshot restore, claude-path observation.
    /// Idempotent — `.task` may fire more than once.
    ///
    /// Side effects stay out of `init` so the owning view can use
    /// plain `@State`. Otherwise re-evaluation on parent re-render
    /// would bind a fresh socket per call, only one of which
    /// `@State` keeps — the rest mutate AppStates views never see,
    /// breaking the `claude` shadow handshake.
    func start() {
        guard !started else { return }
        started = true

        var zdotdirPath: String?
        if let services = trackedServices {
            zdotdirPath = services.zdotdirPath
            sessions.setResolvedClaudePath(services.resolvedClaudePath)
        }

        // Socket *before* any ptys — shells need NICE_SOCKET in env
        // at startup or the `claude()` shadow can't reach us. One
        // socket per window keeps `claude` invocations scoped.
        sessions.bootstrapSocket(zdotdirPath: zdotdirPath)

        // `restoreSavedWindow` below may dissolve and rebuild this.
        if let mainTab = tabs.projects.first(where: { $0.id == TabModel.terminalsProjectId })?.tabs.first {
            _ = sessions.makeSession(for: mainTab.id, cwd: mainTab.cwd)
        }

        sessions.startSocketListener()

        // Restore after the socket is up. Persistence I/O is gated
        // on `services != nil` so tests can't pick up real disk.
        if trackedServices != nil {
            windowSession.restoreSavedWindow()
        }

        // Release the save-gate; a didSet save before this would
        // write a ghost empty window we'd trip over next launch.
        windowSession.markInitializationComplete()
        windowSession.scheduleSessionSave()

        // Mirror async claude-path resolution into `SessionsModel`
        // so post-probe tab spawns pick up the real binary.
        if trackedServices != nil {
            armClaudePathTracking()
        }
    }

    /// Re-arm one-shot observation of `services.resolvedClaudePath`.
    /// `onChange` fires once and must re-call to stay subscribed.
    /// Bails on released services to cover the deinit race.
    @MainActor
    private func armClaudePathTracking() {
        guard let services = trackedServices else { return }
        withObservationTracking {
            _ = services.resolvedClaudePath
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self, let services = self.trackedServices else { return }
                self.sessions.setResolvedClaudePath(services.resolvedClaudePath)
                self.armClaudePathTracking()
            }
        }
    }

    /// Stop every resource this window owns. Called from
    /// `WindowRegistry` on close and `NiceServices` on app terminate.
    /// Safe to call more than once. Persists *before* killing ptys
    /// so mid-session auto-titles make it to disk.
    func tearDown() {
        windowSession.tearDown()
        sessions.tearDown()
    }

    /// Flip the active tab's file-browser hidden-file visibility.
    /// Lives on AppState because it spans SidebarModel (mode gate)
    /// and FileBrowserStore (per-tab toggle). Gated on
    /// `sidebarMode == .files` so the shortcut from tabs mode is a
    /// true no-op.
    func toggleFileBrowserHiddenFiles() {
        guard sidebar.sidebarMode == .files,
              let tabId = tabs.activeTabId else { return }
        fileBrowserStore.toggleHiddenFilesIfExists(forTab: tabId)
    }

    /// Finish tearing down a tab whose panes array reached zero:
    /// drop it from its project, release the pty session, reassign
    /// `activeTabId` if focused, drop the project row itself if the
    /// user asked to close the whole project. Called from
    /// `SessionsModel.paneExited` (via `onTabBecameEmpty`) and
    /// `hardKillTab` when every pane was unspawned.
    private func finalizeDissolvedTab(
        projectIndex pi: Int,
        tabIndex ti: Int,
        tabId: String
    ) {
        // Single removal entry point — `removeTab` does the array
        // remove plus the parent-pointer sweep atomically so we can't
        // accidentally orphan a /branch child by adding a future
        // close path that forgets the sweep.
        tabs.removeTab(projectIndex: pi, tabIndex: ti)
        sessions.removePtySession(tabId: tabId)
        fileBrowserStore.removeState(forTab: tabId)
        // Drop the dissolved tab id from the multi-selection set
        // before any view re-renders against the shrunken tree.
        // Covers external-dissolve paths (pane crash) too.
        tabSelection.prune(validIds: Set(tabs.navigableSidebarTabIds))
        if tabs.activeTabId == tabId {
            tabs.activeTabId = tabs.firstAvailableTabId()
        }

        // Read the project-pending-removal flag without clearing —
        // earlier-tab dissolves in a multi-tab project must leave
        // the flag set so subsequent dissolves still see it.
        let projectId = tabs.projects[pi].id
        if closer.isProjectPendingRemoval(projectId),
           tabs.projects[pi].tabs.isEmpty,
           projectId != TabModel.terminalsProjectId {
            closer.clearProjectPendingRemoval(projectId)
            tabs.projects.remove(at: pi)
        }

        windowSession.scheduleSessionSave()

        if tabs.projects.allSatisfy({ $0.tabs.isEmpty }) {
            NSApp.terminate(nil)
        }
    }
}
