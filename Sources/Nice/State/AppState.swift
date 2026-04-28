//
//  AppState.swift
//  Nice
//
//  Per-window composition root. Holds the five sub-models that own
//  the per-window state — `TabModel` (the projects/tabs/panes tree),
//  `SessionsModel` (long-lived pty/socket subsystem), `SidebarModel`
//  (sidebar UI flags), `CloseRequestCoordinator` (close-confirmation
//  alert flow), and `WindowSession` (window identity + disk
//  persistence) — wires their callbacks together, and runs the
//  cross-cutting `start()` / `tearDown()` choreography.
//
//  Public surface is preserved as forwarders so views and unit tests
//  keep calling `appState.tab(for:)`, `appState.selectTab(...)`,
//  `appState.paneLaunchStates[...]`, etc. The view-side rename pass
//  that points UI directly at the most specific sub-model is a
//  separate Phase 2 step.
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
        /// Close one pane inside the given tab.
        case pane(tabId: String, paneId: String)
        /// Close every pane on the tab (and dissolve the tab).
        case tab(tabId: String)
        /// Close every tab in the project and remove the project row.
        case project(projectId: String)
    }

    let id = UUID()
    let scope: Scope
    /// Human-readable descriptions of the busy panes, one per entry,
    /// for display in the alert body.
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
    /// Reserved id for the pinned Terminals project at index 0 of
    /// `projects`. Forwarded to `TabModel` so the source-of-truth is
    /// the model that owns the value.
    static var terminalsProjectId: String { TabModel.terminalsProjectId }
    /// Stable id for the default "Main" tab seeded into the Terminals
    /// project on fresh launches. UI tests key off a `sidebar.terminals`
    /// accessibility alias on this tab. Forwarded to `TabModel`.
    static var mainTerminalTabId: String { TabModel.mainTerminalTabId }

    /// Per-window data model: projects, tabs, panes, and active-tab
    /// selection plus the pure helpers that operate on them. AppState
    /// wires `tabs.onTreeMutation` to `windowSession.scheduleSessionSave`
    /// in `init` so persistence keeps firing on tree edits.
    let tabs: TabModel

    /// Per-window pty / socket / theme-fan-out subsystem. AppState
    /// wires its `onSessionMutation` callback to
    /// `windowSession.scheduleSessionSave` (so socket-driven mutations
    /// like in-place Claude promotion persist) and `onTabBecameEmpty`
    /// to `finalizeDissolvedTab` (so the dissolve cascade keeps
    /// running on the orchestrator).
    let sessions: SessionsModel

    /// Per-window sidebar UI state (collapsed flag, mode, peek). The
    /// `@SceneStorage` bridge in `AppShellView` seeds the initial
    /// values via AppState.init and writes back on changes via the
    /// forwarders below.
    let sidebar: SidebarModel

    /// Per-window close-confirmation flow: pendingCloseRequest +
    /// requestClose*/confirm/cancel + busy classification + hard-kill.
    /// Holds weak references to `tabs` and `sessions`; AppState wires
    /// `onSyncFinalizeDissolve` to `finalizeDissolvedTab` so the
    /// all-unspawned hard-kill path runs the dissolve cascade
    /// synchronously, and `onScheduleSave` to
    /// `windowSession.scheduleSessionSave` so empty-project removal
    /// persists.
    let closer: CloseRequestCoordinator

    /// Per-window identity + disk persistence. Owns
    /// `windowSessionId`, the static `claimedWindowIds` registry,
    /// `scheduleSessionSave`, `snapshotPersistedWindow`,
    /// `restoreSavedWindow`, and `addRestoredTabModel`. Holds weak
    /// references to `tabs`, `sessions`, and `sidebar` so it can
    /// snapshot and rebuild without going through AppState.
    let windowSession: WindowSession

    /// Forwarder for `tabs.projects` so existing call sites (views,
    /// tests) keep working unchanged. The view-side rename pass that
    /// points UI directly at `tabs.projects` is a separate step.
    var projects: [Project] {
        get { tabs.projects }
        set { tabs.projects = newValue }
    }

    /// Forwarder for `tabs.activeTabId`. The mutating `didSet` (waiting
    /// ack + save fan-out via `onTreeMutation`) lives on `TabModel`.
    var activeTabId: String? {
        get { tabs.activeTabId }
        set { tabs.activeTabId = newValue }
    }

    /// Forwarder for `sidebar.sidebarCollapsed` so existing call
    /// sites (views, tests, KeyboardShortcutMonitor) keep working.
    var sidebarCollapsed: Bool {
        get { sidebar.sidebarCollapsed }
        set { sidebar.sidebarCollapsed = newValue }
    }

    /// Forwarder for `sidebar.sidebarMode`.
    var sidebarMode: SidebarMode {
        get { sidebar.sidebarMode }
        set { sidebar.sidebarMode = newValue }
    }

    /// Per-window catalog of file-browser states keyed by `Tab.id`.
    /// Lifecycle: states are lazily created on first access and
    /// removed in `finalizeDissolvedTab` when a tab dissolves. The
    /// store is its own `@Observable` surface — views observing it
    /// pick up changes without `AppState` re-emitting.
    let fileBrowserStore: FileBrowserStore = FileBrowserStore()

    /// Forwarder for `sidebar.sidebarPeeking`.
    var sidebarPeeking: Bool {
        get { sidebar.sidebarPeeking }
        set { sidebar.sidebarPeeking = newValue }
    }

    /// Forwarder onto `SidebarModel.toggleSidebar`.
    func toggleSidebar() {
        sidebar.toggleSidebar()
    }

    /// Forwarder onto `SidebarModel.toggleSidebarMode`.
    func toggleSidebarMode() {
        sidebar.toggleSidebarMode()
    }

    /// Flip the active tab's file-browser hidden-file visibility.
    /// Bound to `ShortcutAction.toggleHiddenFiles` (default ⌘⇧.) and
    /// the eye toggle in the file browser's breadcrumb. Mirrors
    /// Finder's standard ⌘⇧. shortcut.
    ///
    /// Gated on `sidebarMode == .files` so pressing the shortcut
    /// from tabs mode is a true no-op — no allocation, no published
    /// change for a feature the user isn't looking at. The store's
    /// `toggleHiddenFilesIfExists` further skips toggling for tabs
    /// that have never opened the file browser.
    func toggleFileBrowserHiddenFiles() {
        guard sidebarMode == .files,
              let tabId = activeTabId else { return }
        fileBrowserStore.toggleHiddenFilesIfExists(forTab: tabId)
    }

    /// Title to show at the top of the file browser for `tabId`.
    /// Forwarder onto `TabModel.fileBrowserHeaderTitle`.
    func fileBrowserHeaderTitle(forTab id: String) -> String {
        tabs.fileBrowserHeaderTitle(forTab: id)
    }

    /// Forwarder onto `SidebarModel.endSidebarPeek`.
    func endSidebarPeek() {
        sidebar.endSidebarPeek()
    }

    // MARK: - Window-state plumbing

    /// In-flight "processes still running" confirmation. Forwarder
    /// onto `CloseRequestCoordinator.pendingCloseRequest`. Settable so
    /// `AppShellView`'s alert binding can clear it via `nil`-write.
    var pendingCloseRequest: PendingCloseRequest? {
        get { closer.pendingCloseRequest }
        set { closer.pendingCloseRequest = newValue }
    }

    /// Forwarder for `windowSession.windowSessionId`. Read-only —
    /// adoption mutations happen inside `restoreSavedWindow`. Views
    /// (`AppShellView.onChange(of: appState.windowSessionId)`) and
    /// tests still observe via this path.
    var windowSessionId: String {
        windowSession.windowSessionId
    }

    /// Live `NiceServices` pointer (weak) used by `start()` to read
    /// `zdotdirPath` / `resolvedClaudePath` and by `armClaudePathTracking`
    /// to mirror async path resolution into `SessionsModel`.
    @ObservationIgnored
    private weak var trackedServices: NiceServices?

    /// File-browser context-menu services. Set at init time —
    /// production passes the shared instances from `NiceServices`,
    /// tests pass private instances built against a fake pasteboard
    /// and trasher. `nil` for `#Preview` and unit-test paths that
    /// don't exercise the orchestration.
    let fileExplorer: FileExplorerServices?

    /// User preferences (editor mappings, palette, …). Stored so
    /// orchestration methods like `openInEditorPane` can resolve a
    /// configured editor without reaching back through services.
    let tweaks: Tweaks?

    /// Auto-detected terminal editors discovered at startup. Used by
    /// `editorPaneEntries` so the File Explorer's context menu can
    /// surface vim/nvim/etc. without prior user config.
    let editorDetector: EditorDetector?

    /// Convenience init for `#Preview` blocks and unit tests. Each
    /// AppState is otherwise expected to be constructed by
    /// `AppShellView` passing its window's `NiceServices` and the
    /// per-window `@SceneStorage` values.
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
        fileExplorer: FileExplorerServices? = nil
    ) {
        // Production: take the file-explorer triple from the shared
        // NiceServices. Tests can override by passing their own.
        self.fileExplorer = fileExplorer ?? services?.fileExplorer
        self.tweaks = services?.tweaks
        self.editorDetector = services?.editorDetector
        self.sidebar = SidebarModel(
            initialCollapsed: initialSidebarCollapsed,
            initialMode: initialSidebarMode
        )

        let resolvedMainCwd = initialMainCwd ?? NSHomeDirectory()

        // Build the data model first (its init seeds the Terminals
        // project + Main tab) and then the sessions subsystem which
        // holds a weak pointer back to the model. The closer and
        // windowSession hold weak references to the models below —
        // fine because AppState owns all five and they share its
        // lifetime.
        self.tabs = TabModel(initialMainCwd: resolvedMainCwd)
        self.sessions = SessionsModel(tabs: tabs)
        self.closer = CloseRequestCoordinator(tabs: tabs, sessions: sessions)
        // WindowSession last so its weak refs all point at fully-
        // constructed models. Persistence is gated on
        // `services != nil` (preview/test paths skip disk I/O); the
        // model's own `isInitializing` gate covers the didSet bounce
        // from the seed assignment above.
        self.windowSession = WindowSession(
            tabs: tabs,
            sessions: sessions,
            sidebar: sidebar,
            windowSessionId: windowSessionId,
            persistenceEnabled: services != nil
        )
        self.trackedServices = services

        // Seed scheme / palette / accent / terminal-theme / font
        // family from `Tweaks` so the very first `makeSession` call
        // (for the Terminals tab, in `start()`) paints with the user's
        // real preferences. Without this seeding the session is themed
        // against `SessionsModel`'s defaults (.dark / .nice /
        // terracotta) and only repainted when `AppShellView.onAppear`
        // broadcasts `updateScheme` / `updateTerminalTheme` — a
        // visible flash on launch, and a stubborn mis-theme for
        // chrome-coupled Nice Defaults because their bg/fg derivation
        // reads the session's stale palette.
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

        // Preview / unit-test path: `services == nil` means `start()`
        // is unlikely to be called, so seed `NICE_CLAUDE_OVERRIDE`
        // here. Production reads `services.resolvedClaudePath` in
        // `start()` (after `services.bootstrap()` has populated it).
        if services == nil {
            sessions.setResolvedClaudePath(
                ProcessInfo.processInfo.environment["NICE_CLAUDE_OVERRIDE"]
            )
        }

        // Wire callbacks last so any `didSet` triggered during seed
        // assignment above bounces through a fully-constructed
        // AppState. The `scheduleSessionSave` gates
        // (`isInitializing` / `persistenceEnabled`) live on
        // `windowSession`; AppState's callbacks just route to it.
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

    /// Bring the per-window subsystem online: allocate the control
    /// socket, spawn the seed Main terminal pty, restore any saved
    /// window snapshot, and arm the claude-path observation. Idempotent
    /// — safe to call from `.task` on the owning view, which can fire
    /// more than once across SwiftUI lifecycle edges.
    ///
    /// Side effects are deliberately kept out of `init` so the owning
    /// view can use a plain `@State` for the AppState. With `@State`,
    /// `AppState(...)` would otherwise re-evaluate on every parent
    /// body re-render — each call would bind a new socket, only one of
    /// which `@State`'s identity rule keeps. Socket messages then
    /// mutate AppStates the views never see, breaking the `claude`
    /// shadow handshake.
    @ObservationIgnored
    private var started = false
    func start() {
        guard !started else { return }
        started = true

        // Read the services-owned path values now that
        // `services.bootstrap()` has populated them. Tests with
        // `services == nil` rely on the env-var override seeded in
        // `init`.
        var zdotdirPath: String?
        if let services = trackedServices {
            zdotdirPath = services.zdotdirPath
            sessions.setResolvedClaudePath(services.resolvedClaudePath)
        }

        // Allocate the control socket *before* spawning any ptys —
        // the shells need NICE_SOCKET in their environment at startup
        // or the `claude()` shadow can't reach us. Each window owns
        // its own socket so a `claude` invocation in one window's
        // Main Terminal only opens a tab in that window.
        sessions.bootstrapSocket(zdotdirPath: zdotdirPath)

        // Spawn the seed Main terminal pty. `restoreSavedWindow`
        // below may dissolve and rebuild this if a snapshot exists —
        // that's the existing choreography and is preserved here.
        if let mainTab = tabs.projects.first(where: { $0.id == TabModel.terminalsProjectId })?.tabs.first {
            _ = sessions.makeSession(for: mainTab.id, cwd: mainTab.cwd)
        }

        sessions.startSocketListener()

        // Restore runs after the control socket is up so respawned
        // tabs can reach it if they spawn children via the shadow.
        // Preview/test callers pass `services: nil` (persistence
        // disabled), and `WindowSession.scheduleSessionSave` /
        // `restoreSavedWindow`'s file I/O are gated on that flag —
        // unit tests can't pick up the user's real sessions.json.
        if trackedServices != nil {
            windowSession.restoreSavedWindow()
        }

        // Release the save-gate now that restore has populated the
        // tab list. Before this point a didSet-triggered save would
        // write a ghost empty window we'd trip over next launch.
        windowSession.markInitializationComplete()
        windowSession.scheduleSessionSave()

        // Track the services' async claude-path resolution so a tab
        // spawned via `makeSession` after the probe completes picks
        // up the real binary. `withObservationTracking` only fires on
        // the *next* mutation, so it naturally skips the synchronous
        // value already snapshot above.
        if trackedServices != nil {
            armClaudePathTracking()
        }
    }

    /// Re-arm a one-shot observation closure that mirrors
    /// `services.resolvedClaudePath` into `SessionsModel`'s cache. The
    /// `onChange` handler fires once per mutation and must re-call
    /// this method to stay subscribed for the next change. Bails out
    /// if `trackedServices` has been released — covers the deinit
    /// race so we don't reinstall after the AppState is going away.
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

    /// Snapshot of this window's live panes grouped by kind. Used by
    /// the quit / window-close confirmation alerts to word the prompt
    /// ("N Claude sessions and M terminals") without exposing the model
    /// to callers outside AppState. Forwarder onto `TabModel`.
    var livePaneCounts: (claude: Int, terminal: Int) {
        tabs.livePaneCounts
    }

    /// Stop every resource this window owns. Called by
    /// `WindowRegistry` when its `NSWindow` closes, and by
    /// `NiceServices` for every live AppState on app terminate.
    /// Safe to call more than once. The shared ZDOTDIR is owned by
    /// `NiceServices` and removed at app terminate, not here.
    func tearDown() {
        // Persist before killing any ptys so auto-titles that
        // arrived mid-session make it to disk, then release the
        // session-id claim. WindowSession's `tearDown` is a no-op
        // when persistence is disabled.
        windowSession.tearDown()
        sessions.tearDown()
    }

    // MARK: - Selection

    /// Forwarder so views and tests keep using `appState.selectTab(...)`.
    /// The body lives on `TabModel`.
    func selectTab(_ id: String) {
        tabs.selectTab(id)
    }

    /// Pick which pane is focused in `tabId`. Forwarder onto
    /// `SessionsModel.setActivePane`.
    func setActivePane(tabId: String, paneId: String) {
        sessions.setActivePane(tabId: tabId, paneId: paneId)
    }

    // MARK: - Tab creation

    /// Open a new tab rooted at `cwd`, running `claude` with any `args`
    /// forwarded through. Forwarder onto `SessionsModel`.
    func createTabFromMainTerminal(cwd: String, args: [String]) {
        sessions.createTabFromMainTerminal(cwd: cwd, args: args)
    }

    // MARK: - Theme

    /// Forwarder onto `SessionsModel.updateScheme`.
    func updateScheme(_ scheme: ColorScheme, palette: Palette, accent: NSColor) {
        sessions.updateScheme(scheme, palette: palette, accent: accent)
    }

    /// Forwarder onto `SessionsModel.updateTerminalFontSize`.
    func updateTerminalFontSize(_ size: CGFloat) {
        sessions.updateTerminalFontSize(size)
    }

    /// Forwarder onto `SessionsModel.updateTerminalTheme`.
    func updateTerminalTheme(_ theme: TerminalTheme) {
        sessions.updateTerminalTheme(theme)
    }

    /// Forwarder onto `SessionsModel.updateTerminalFontFamily`.
    func updateTerminalFontFamily(_ name: String?) {
        sessions.updateTerminalFontFamily(name)
    }

    // MARK: - Launch overlay / pty cache (forwarders)

    /// Read-only forwarder for `SessionsModel.ptySessions`. Views
    /// (`AppShellView`'s focus restoration path) and tests
    /// (`AppStateProjectBucketingTests`) read this.
    var ptySessions: [String: TabPtySession] {
        sessions.ptySessions
    }

    /// Read-only forwarder for `SessionsModel.paneLaunchStates`.
    /// `AppShellView` binds the launch overlay to per-pane entries.
    var paneLaunchStates: [String: PaneLaunchStatus] {
        sessions.paneLaunchStates
    }

    /// Test seam — forwarder onto `SessionsModel.launchOverlayGraceSeconds`.
    var launchOverlayGraceSeconds: Double {
        get { sessions.launchOverlayGraceSeconds }
        set { sessions.launchOverlayGraceSeconds = newValue }
    }

    /// Forwarder onto `SessionsModel.registerPaneLaunch`.
    func registerPaneLaunch(paneId: String, command: String) {
        sessions.registerPaneLaunch(paneId: paneId, command: command)
    }

    /// Forwarder onto `SessionsModel.clearPaneLaunch`.
    func clearPaneLaunch(paneId: String) {
        sessions.clearPaneLaunch(paneId: paneId)
    }

    // MARK: - Lifecycle handlers

    /// Forwarder onto `SessionsModel.paneExited`. Tests call this
    /// directly to drive the dispatch path without standing up a real
    /// pty.
    func paneExited(tabId: String, paneId: String, exitCode: Int32?) {
        sessions.paneExited(tabId: tabId, paneId: paneId, exitCode: exitCode)
    }

    /// Forwarder onto `SessionsModel.paneTitleChanged`.
    func paneTitleChanged(tabId: String, paneId: String, title: String) {
        sessions.paneTitleChanged(tabId: tabId, paneId: paneId, title: title)
    }

    /// Forwarder onto `SessionsModel.paneCwdChanged`.
    func paneCwdChanged(tabId: String, paneId: String, cwd: String) {
        sessions.paneCwdChanged(tabId: tabId, paneId: paneId, cwd: cwd)
    }

    /// Finish tearing down a tab whose panes array has gone to zero:
    /// drop it from its project, release the pty session, reassign
    /// `activeTabId` if it was focused, and drop the project row
    /// itself when the user asked to close the whole project. Called
    /// by `SessionsModel.paneExited` (via the `onTabBecameEmpty`
    /// callback) after an async pane exit empties the panes list, and
    /// from `hardKillTab` when every pane was unspawned and there's no
    /// async exit to wait on.
    private func finalizeDissolvedTab(
        projectIndex pi: Int,
        tabIndex ti: Int,
        tabId: String
    ) {
        tabs.projects[pi].tabs.remove(at: ti)
        sessions.removePtySession(tabId: tabId)
        fileBrowserStore.removeState(forTab: tabId)
        if activeTabId == tabId {
            activeTabId = tabs.firstAvailableTabId()
        }

        // If the user asked to close this whole project (right-click →
        // Close Project), drop the now-empty project row too. Terminals
        // is guarded upstream but double-check here defensively. Read
        // the flag without clearing first — earlier-tab dissolves in a
        // multi-tab project must leave the flag set so subsequent
        // dissolves still see it.
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

    /// Forwarder onto `TabModel.firstAvailableTabId()`. Used by
    /// `AppState+FileExplorer.openInEditorPane` as the active-tab
    /// fallback when the user clicks an editor entry while no tab is
    /// focused.
    func firstAvailableTabId() -> String? {
        tabs.firstAvailableTabId()
    }

    /// Forwarder onto `TabModel.applyAutoTitle`.
    func applyAutoTitle(tabId: String, rawTitle: String) {
        tabs.applyAutoTitle(tabId: tabId, rawTitle: rawTitle)
    }

    /// Forwarder onto `SessionsModel.focusActiveTerminal`.
    func focusActiveTerminal() {
        sessions.focusActiveTerminal()
    }

    /// Forwarder onto `TabModel.renameTab`.
    func renameTab(id tabId: String, to newTitle: String) {
        tabs.renameTab(id: tabId, to: newTitle)
    }

    /// Append a new terminal-only tab to the pinned Terminals group,
    /// focus it, and spawn its pty. Forwarder onto `SessionsModel`.
    @discardableResult
    func createTerminalTab() -> String? {
        sessions.createTerminalTab()
    }

    /// Create a fresh Claude tab in an existing project group.
    /// Forwarder onto `SessionsModel`.
    @discardableResult
    func createClaudeTabInProject(projectId: String) -> String? {
        sessions.createClaudeTabInProject(projectId: projectId)
    }

    /// True when `tabId` lives inside the pinned Terminals project.
    /// Forwarder onto `TabModel.isTerminalsProjectTab`.
    func isTerminalsProjectTab(_ tabId: String) -> Bool {
        tabs.isTerminalsProjectTab(tabId)
    }

    // MARK: - Pane management

    /// Append a new terminal pane to `tabId`, spawn its pty, and focus
    /// it. Forwarder onto `SessionsModel.addPane`.
    @discardableResult
    func addPane(
        tabId: String,
        kind: PaneKind = .terminal,
        cwd: String? = nil,
        title: String? = nil,
        command: String? = nil
    ) -> String? {
        sessions.addPane(
            tabId: tabId, kind: kind, cwd: cwd, title: title, command: command
        )
    }

    // MARK: - Close coordinator (forwarders)

    /// Forwarder onto `CloseRequestCoordinator.requestClosePane`.
    func requestClosePane(tabId: String, paneId: String) {
        closer.requestClosePane(tabId: tabId, paneId: paneId)
    }

    /// Forwarder onto `CloseRequestCoordinator.requestCloseTab`.
    func requestCloseTab(tabId: String) {
        closer.requestCloseTab(tabId: tabId)
    }

    /// Forwarder onto `CloseRequestCoordinator.requestCloseProject`.
    func requestCloseProject(projectId: String) {
        closer.requestCloseProject(projectId: projectId)
    }

    /// Forwarder onto `CloseRequestCoordinator.confirmPendingClose`.
    func confirmPendingClose() {
        closer.confirmPendingClose()
    }

    /// Forwarder onto `CloseRequestCoordinator.cancelPendingClose`.
    func cancelPendingClose() {
        closer.cancelPendingClose()
    }

    // MARK: - Reordering

    /// Forwarder onto `TabModel.moveTab`.
    func moveTab(_ tabId: String, relativeTo targetTabId: String, placeAfter: Bool) {
        tabs.moveTab(tabId, relativeTo: targetTabId, placeAfter: placeAfter)
    }

    /// Forwarder onto `TabModel.wouldMoveTab`.
    func wouldMoveTab(_ tabId: String, relativeTo targetTabId: String, placeAfter: Bool) -> Bool {
        tabs.wouldMoveTab(tabId, relativeTo: targetTabId, placeAfter: placeAfter)
    }

    // MARK: - Keyboard navigation

    /// Forwarder onto `TabModel.navigableSidebarTabIds`.
    var navigableSidebarTabIds: [String] {
        tabs.navigableSidebarTabIds
    }

    /// Forwarder onto `TabModel.selectNextSidebarTab`.
    func selectNextSidebarTab() {
        tabs.selectNextSidebarTab()
    }

    /// Forwarder onto `TabModel.selectPrevSidebarTab`.
    func selectPrevSidebarTab() {
        tabs.selectPrevSidebarTab()
    }

    /// Forwarder onto `SessionsModel.selectNextPane`.
    func selectNextPane() {
        sessions.selectNextPane()
    }

    /// Forwarder onto `SessionsModel.selectPrevPane`.
    func selectPrevPane() {
        sessions.selectPrevPane()
    }

    /// Forwarder onto `SessionsModel.addTerminalToActiveTab`.
    func addTerminalToActiveTab() {
        sessions.addTerminalToActiveTab()
    }

    // MARK: - Lookup

    /// Forwarder onto `TabModel.tab(for:)`. Views and tests still call
    /// `appState.tab(for:)`; the rename pass will retarget them at
    /// `tabs` directly.
    func tab(for id: String) -> Tab? {
        tabs.tab(for: id)
    }

    // MARK: - Session persistence

    /// Forwarder onto `SessionsModel.handleClaudeSessionUpdate`. Tests
    /// drive this directly without standing up a real socket.
    func handleClaudeSessionUpdate(paneId: String, sessionId: String) {
        sessions.handleClaudeSessionUpdate(paneId: paneId, sessionId: sessionId)
    }

    /// Forwarder onto `WindowSession.snapshotPersistedWindow`. Unit
    /// tests assert the serialization contract through this surface;
    /// the implementation reads `tabs`, `sidebar`, and the window's
    /// own `windowSessionId` directly inside `WindowSession`.
    func snapshotPersistedWindow() -> PersistedWindow {
        windowSession.snapshotPersistedWindow()
    }

    /// Forwarder onto `WindowSession.addRestoredTabModel`. Unit
    /// tests still drive restore through this surface to assert the
    /// per-pane cwd fallback and the active-pane spawn behaviour.
    @discardableResult
    func addRestoredTabModel(
        _ persisted: PersistedTab,
        toProjectIndex projectIndex: Int
    ) -> (tabId: String, cwd: String, claudePaneId: String?, claudeSessionId: String)? {
        windowSession.addRestoredTabModel(persisted, toProjectIndex: projectIndex)
    }

    // MARK: - Helper forwarders

    /// Forwarder onto `TabModel.stripNiceWorktreeSuffix`.
    static func stripNiceWorktreeSuffix(_ path: String) -> String {
        TabModel.stripNiceWorktreeSuffix(path)
    }

    /// Forwarder onto `TabModel.findGitRoot(forCwd:)`.
    static func findGitRoot(forCwd cwd: String) -> String? {
        TabModel.findGitRoot(forCwd: cwd)
    }

    /// Forwarder onto `TabModel.extractWorktreeName(from:)`.
    static func extractWorktreeName(from args: [String]) -> String? {
        TabModel.extractWorktreeName(from: args)
    }

    /// Forwarder onto `TabModel.resolvedSpawnCwd(for:)`. Used by tests
    /// and by `AppState+FileExplorer`.
    func resolvedSpawnCwd(for tab: Tab) -> String {
        tabs.resolvedSpawnCwd(for: tab)
    }

    /// Forwarder onto `TabModel.spawnCwdForNewPane(in:callerProvided:)`.
    func spawnCwdForNewPane(in tab: Tab, callerProvided cwd: String?) -> String {
        tabs.spawnCwdForNewPane(in: tab, callerProvided: cwd)
    }

    /// Forwarder onto `TabModel.resolvedSpawnCwd(for:pane:)`.
    func resolvedSpawnCwd(for tab: Tab, pane: Pane) -> String {
        tabs.resolvedSpawnCwd(for: tab, pane: pane)
    }

    /// Forwarder onto `TabModel.repairProjectStructure()`.
    func repairProjectStructure() {
        tabs.repairProjectStructure()
    }
}
