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
/// item-based `.alert`.
struct PendingCloseRequest: Identifiable, Equatable {
    enum Scope: Equatable {
        case pane(tabId: String, paneId: String)
        case tab(tabId: String)
        case project(projectId: String)
    }

    let id = UUID()
    let scope: Scope
    /// Human-readable descriptions of the busy panes for the alert body.
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

/// In-flight tear-off awaiting absorption by a freshly-spawned
/// window's `AppState`. Lives in a process-wide slot on
/// `NiceServices`; the new window's `AppShellHost.task` consumes it
/// after `appState.start()` and calls `AppState.absorbTearOff(_:)`.
struct PendingTearOff {
    let payload: PaneDragPayload
    /// Live `NiceTerminalView` detached from source (nil if the pane
    /// had no spawned pty — model-only tear-off).
    let view: NiceTerminalView?
    let pane: Pane
    let sourceTab: Tab
    /// Identity of the originating `AppState`. The originator is
    /// excluded from `consumeTearOff` so it can't accidentally re-
    /// absorb its own tear-off on the next view rebuild.
    let originAppStateId: ObjectIdentifier
    /// Window-session-id of the new window the source pre-minted via
    /// `requestPaneTearOff`. Only the freshly-spawned `AppState`
    /// whose `windowSessionId` matches this will absorb. Prevents an
    /// unrelated ⌘N at the wrong moment from stealing the tear-off.
    let destinationWindowSessionId: String
    let projectAnchor: ProjectAnchor
    /// Cursor position at drag-release in screen coordinates
    /// (Cocoa: origin bottom-left). Used to position the new window's
    /// top-left near the cursor.
    let cursorScreenPoint: CGPoint
    /// Offset of the source pill within the strip at drag start (pill
    /// minX → cursor x). Subtracted from `cursorScreenPoint` so the
    /// migrated pill — not the new window's traffic-light corner —
    /// lands under the cursor on release.
    let pillOriginOffset: CGSize
    let pendingLaunchState: PaneLaunchStatus?
    /// Wall-clock timestamp the tear-off was minted at. Stale entries
    /// older than `NiceServices.tearOffTTL` are dropped on consume.
    let createdAt: Date
}

/// Where in the destination window's sidebar a torn-off / migrated
/// new tab should land. `terminals` pins to the reserved Terminals
/// project; `repoPath` matches an existing non-Terminals project by
/// path or creates a fresh one rooted there. Used by the drag-and-drop
/// new-tab path (`AppState.absorbAsNewTab`).
enum ProjectAnchor: Sendable, Equatable {
    case terminals
    case repoPath(String)

    /// Pick the natural anchor for `sourceTabId`'s migrated panes:
    /// pinned Terminals when the tab lives there, otherwise the
    /// owning project's path. Returns `nil` when the source tab can't
    /// be resolved (e.g. source window closed mid-drag) — caller
    /// decides how to recover instead of silently anchoring to
    /// Terminals.
    @MainActor
    static func from(sourceTabId: String, sourceAppState: AppState) -> ProjectAnchor? {
        if sourceAppState.tabs.isTerminalsProjectTab(sourceTabId) {
            return .terminals
        }
        if let proj = sourceAppState.tabs.projects.first(where: { p in
            p.tabs.contains(where: { $0.id == sourceTabId })
        }) {
            return .repoPath(proj.path)
        }
        return nil
    }
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

    /// Absorb a `PendingTearOff` produced by the source window's drag
    /// release. Thin wrapper around `absorbAsNewTab`; the new
    /// window's top-left is positioned near the cursor release point.
    @discardableResult
    func absorbTearOff(_ pending: PendingTearOff) -> String {
        absorbAsNewTab(
            pane: pending.pane,
            sourceTab: pending.sourceTab,
            view: pending.view,
            projectAnchor: pending.projectAnchor,
            pendingLaunchState: pending.pendingLaunchState
        )
        // Window-position assignment is handed back to the caller —
        // it has direct access to the NSWindow via `WindowAccessor`,
        // and we don't want this method to depend on `WindowRegistry`
        // resolving the still-mounting window.
    }

    /// Detach a Claude pane from `sourceAppState` and absorb it into
    /// THIS window as a fresh tab. Used by both drop delegates (pane
    /// strip + sidebar row) when a Claude pane lands on an existing
    /// window — Claude can't join an existing tab so it always spawns
    /// a new one. The pty stays alive across the migration.
    ///
    /// Returns the new tab id, or `nil` for invalid input (non-Claude
    /// payload, missing source).
    @discardableResult
    func absorbClaudeAsNewTab(
        from sourceAppState: AppState,
        payload: PaneDragPayload
    ) -> String? {
        guard payload.kind == .claude else { return nil }
        guard let sourceTab = sourceAppState.tabs.tab(for: payload.tabId),
              let pane = sourceTab.panes.first(where: { $0.id == payload.paneId })
        else { return nil }

        // Resolve anchor on the source side BEFORE mutating it. If the
        // source tab's project can't be found (shouldn't happen — we
        // just resolved the tab — but defensive), bail rather than
        // silently anchoring into Terminals.
        guard let anchor = ProjectAnchor.from(
            sourceTabId: payload.tabId, sourceAppState: sourceAppState
        ) else { return nil }

        let sourcePty = sourceAppState.sessions.ptySessions[payload.tabId]
        let detachedView = sourcePty?.detachPane(id: payload.paneId)
        let launchState = sourceAppState.sessions.paneLaunchStates[payload.paneId]

        var sourceBecameEmpty = false
        sourceAppState.tabs.mutateTab(id: payload.tabId) { tab in
            guard let idx = tab.panes.firstIndex(where: { $0.id == payload.paneId })
            else { return }
            tab.panes.remove(at: idx)
            if tab.activePaneId == payload.paneId {
                if idx < tab.panes.count {
                    tab.activePaneId = tab.panes[idx].id
                } else if idx > 0 {
                    tab.activePaneId = tab.panes[idx - 1].id
                } else {
                    tab.activePaneId = nil
                }
            }
            sourceBecameEmpty = tab.panes.isEmpty
        }
        if launchState != nil {
            sourceAppState.sessions.clearPaneLaunch(paneId: payload.paneId)
        }
        if sourceBecameEmpty,
           let (pi, ti) = sourceAppState.tabs.projectTabIndex(for: payload.tabId) {
            sourceAppState.sessions.onTabBecameEmpty?(payload.tabId, pi, ti)
        }

        let newTabId = absorbAsNewTab(
            pane: pane,
            sourceTab: sourceTab,
            view: detachedView,
            projectAnchor: anchor,
            pendingLaunchState: launchState
        )
        sourceAppState.sessions.onSessionMutation?()
        return newTabId
    }

    /// Adopt a pane (kind: claude OR terminal) as the sole occupant
    /// of a NEW tab in this window. Used for:
    ///   - Claude pane drops onto another window's pane strip /
    ///     sidebar row (Claude can't join an existing tab — must be at
    ///     index 0).
    ///   - Tear-off into a new window (any pane kind).
    ///
    /// The migrated `view` is attached to a freshly-minted
    /// `TabPtySession` so the live pty + scrollback survive. Carries
    /// `claudeSessionId` from `sourceTab` for Claude panes so future
    /// restores can `claude --resume`.
    ///
    /// Returns the newly-minted tab id.
    @discardableResult
    func absorbAsNewTab(
        pane: Pane,
        sourceTab: Tab,
        view: NiceTerminalView?,
        projectAnchor: ProjectAnchor,
        pendingLaunchState: PaneLaunchStatus? = nil
    ) -> String {
        let newTabId = "t-\(UUID().uuidString)"
        let carriedSessionId: String? =
            (pane.kind == .claude) ? sourceTab.claudeSessionId : nil
        let title: String = {
            switch pane.kind {
            case .claude: return sourceTab.title
            case .terminal:
                return pane.title.isEmpty ? "Terminal" : pane.title
            }
        }()
        let newTab = Tab(
            id: newTabId,
            title: title,
            cwd: sourceTab.cwd,
            branch: sourceTab.branch,
            panes: [pane],
            activePaneId: pane.id,
            titleManuallySet: sourceTab.titleManuallySet,
            claudeSessionId: carriedSessionId
        )

        // Bucket into the right project before wiring up the pty
        // session so the tab is fully placed when `makeSession` runs.
        switch projectAnchor {
        case .terminals:
            tabs.ensureTerminalsProjectSeeded()
            if let pi = tabs.projects.firstIndex(
                where: { $0.id == TabModel.terminalsProjectId }
            ) {
                tabs.projects[pi].tabs.append(newTab)
            }
        case .repoPath(let path):
            tabs.appendOrInsert(newTab, intoProjectAt: path)
        }

        // Spin up a TabPtySession and migrate the live view in.
        let cwd = tabs.resolvedSpawnCwd(for: newTab)
        let session = sessions.makeSession(for: newTabId, cwd: cwd)
        if let view {
            session.attachPane(id: pane.id, view: view)
        }
        if let pendingLaunchState {
            sessions.adoptPaneLaunchState(
                paneId: pane.id, status: pendingLaunchState
            )
        }

        tabs.activeTabId = newTabId
        windowSession.scheduleSessionSave()
        return newTabId
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
        tabs.projects[pi].tabs.remove(at: ti)
        sessions.removePtySession(tabId: tabId)
        fileBrowserStore.removeState(forTab: tabId)
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
