//
//  AppState.swift
//  Nice
//
//  Per-window composition root. Holds the pure data model
//  (`TabModel`, the projects/tabs/panes tree) and the long-lived
//  pty/socket subsystem (`SessionsModel`); wires their callbacks
//  together; owns the cross-cutting concerns that don't belong on
//  either sub-model — the close-confirmation alert, sidebar UI flags,
//  the file-browser state catalog, and the per-window persistence
//  bookkeeping (`windowSessionId`, restore, debounced save,
//  `claimedWindowIds`).
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

    /// Set of `windowSessionId`s already claimed by live AppStates in
    /// this process. Populated by `restoreSavedWindow` after it picks
    /// a slot. `restoreSavedWindow` consults this to decide whether a
    /// miss-on-match should adopt an unclaimed saved entry (legitimate
    /// first-launch migration) or stay fresh (⌘N opened a second
    /// window; adopting the first window's slot would duplicate pane
    /// ids and defeat per-window isolation).
    private static var claimedWindowIds: Set<String> = []

    /// Per-window data model: projects, tabs, panes, and active-tab
    /// selection plus the pure helpers that operate on them. AppState
    /// wires `tabs.onTreeMutation` to `scheduleSessionSave` in `init`
    /// so persistence keeps firing on tree edits.
    let tabs: TabModel

    /// Per-window pty / socket / theme-fan-out subsystem. AppState
    /// wires its `onSessionMutation` callback to `scheduleSessionSave`
    /// (so socket-driven mutations like in-place Claude promotion
    /// persist) and `onTabBecameEmpty` to `finalizeDissolvedTab`
    /// (so the dissolve cascade keeps running on the orchestrator).
    let sessions: SessionsModel

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

    /// Whether the sidebar is collapsed. Seeded from the per-window
    /// `@SceneStorage` value by the owning view so each window keeps
    /// its own state; the view writes back on changes.
    var sidebarCollapsed: Bool = false

    /// Which content the sidebar is showing (tabs vs file browser).
    /// Seeded from the per-window `@SceneStorage` value upstream so
    /// each window restores its last-used mode across relaunch.
    var sidebarMode: SidebarMode = .tabs

    /// Per-window catalog of file-browser states keyed by `Tab.id`.
    /// Lifecycle: states are lazily created on first access and
    /// removed in `finalizeDissolvedTab` when a tab dissolves. The
    /// store is its own `@Observable` surface — views observing it
    /// pick up changes without `AppState` re-emitting.
    let fileBrowserStore: FileBrowserStore = FileBrowserStore()

    /// Transient: sidebar is floating over the terminal as a peek
    /// triggered by the tab-cycling shortcut while collapsed. Set by
    /// `KeyboardShortcutMonitor` after a sidebar-tab dispatch, cleared
    /// when the user releases the shortcut's modifiers. Never set while
    /// `sidebarCollapsed == false`. The view layer ORs this with its own
    /// mouse-hover pin so a hovered peek stays open after the keys lift.
    var sidebarPeeking: Bool = false

    func toggleSidebar() {
        sidebarCollapsed.toggle()
    }

    /// Flip the sidebar between projects/tabs and file-browser views.
    /// Bound to `ShortcutAction.toggleSidebarMode` (default ⌘⇧B) and
    /// the two mode icons in the sidebar header.
    func toggleSidebarMode() {
        sidebarMode = (sidebarMode == .tabs) ? .files : .tabs
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

    /// Called by the keyboard monitor when all relevant shortcut
    /// modifiers have been released. The view's separate mouse-hover
    /// pin keeps the overlay rendered if the cursor is over it.
    func endSidebarPeek() {
        sidebarPeeking = false
    }

    // MARK: - Window-state plumbing

    /// In-flight "processes still running" confirmation. Set by
    /// `requestClosePane` / `requestCloseTab` / `requestCloseProject`
    /// when they find something busy; cleared by `confirmPendingClose`
    /// (after the kill) or `cancelPendingClose` (user backs out).
    /// `AppShellView` binds an `.alert` to this.
    var pendingCloseRequest: PendingCloseRequest?

    /// Project ids the user asked to fully close. When a tab in one of
    /// these projects finishes dissolving in `paneExited`, the empty
    /// project row is also removed from `projects`. The Terminals
    /// project is excluded upstream (its id is never added).
    @ObservationIgnored
    private var projectsPendingRemoval: Set<String> = []

    /// Stable identifier for this window's entry in `sessions.json`.
    /// Pulled in from `@SceneStorage("windowSessionId")` on
    /// `AppShellView`; survives quits via standard SwiftUI scene
    /// storage so the same window restores the same tab list on
    /// relaunch. Observed so the view layer can mirror adoption
    /// changes back into SceneStorage — `restoreSavedWindow` may
    /// switch us to the bootstrap id, and that re-pairing must
    /// persist.
    private(set) var windowSessionId: String

    /// Blocks `scheduleSessionSave` while `init` is still running.
    /// Swift fires `activeTabId`'s `didSet` for the seed assignment
    /// in some optional-typed cases, which would otherwise upsert an
    /// empty window entry before `restoreSavedWindow` has a chance to
    /// adopt the bootstrap. Cleared on the last line of `init`.
    @ObservationIgnored
    private var isInitializing: Bool = true

    /// False in preview/test mode (`services == nil` at init). Blocks
    /// `scheduleSessionSave` so unit tests can't pollute the real
    /// `~/Library/Application Support/Nice/sessions.json` by exercising
    /// the tab-mutation surface.
    @ObservationIgnored
    private let persistenceEnabled: Bool

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
        // Brand-new scenes come in with an empty SceneStorage value;
        // mint a UUID here so the scene has a stable id for save/
        // restore from the first body evaluation onward. The view's
        // `onChange(of: windowSessionId)` mirrors this back to
        // SceneStorage so the pairing survives relaunch.
        self.windowSessionId = windowSessionId.isEmpty
            ? UUID().uuidString
            : windowSessionId
        self.persistenceEnabled = services != nil
        // Production: take the file-explorer triple from the shared
        // NiceServices. Tests can override by passing their own.
        self.fileExplorer = fileExplorer ?? services?.fileExplorer
        self.tweaks = services?.tweaks
        self.editorDetector = services?.editorDetector
        self.sidebarCollapsed = initialSidebarCollapsed
        self.sidebarMode = initialSidebarMode

        let resolvedMainCwd = initialMainCwd ?? NSHomeDirectory()

        // Build the data model first (its init seeds the Terminals
        // project + Main tab) and then the sessions subsystem which
        // holds a weak pointer back to the model.
        self.tabs = TabModel(initialMainCwd: resolvedMainCwd)
        self.sessions = SessionsModel(tabs: tabs)
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
        // assignment above bounces through a fully-constructed AppState
        // where `scheduleSessionSave`'s `isInitializing` /
        // `persistenceEnabled` gates are honored.
        self.tabs.onTreeMutation = { [weak self] in
            self?.scheduleSessionSave()
        }
        self.sessions.onSessionMutation = { [weak self] in
            self?.scheduleSessionSave()
        }
        self.sessions.onTabBecameEmpty = { [weak self] tabId, pi, ti in
            self?.finalizeDissolvedTab(projectIndex: pi, tabIndex: ti, tabId: tabId)
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
        // Preview/test callers pass `services: nil`; skip the disk
        // read so unit tests don't pick up the user's real
        // sessions.json.
        if persistenceEnabled {
            restoreSavedWindow()
        }

        // Release the save-gate now that restore has populated the
        // tab list. Before this point a didSet-triggered save would
        // write a ghost empty window we'd trip over next launch.
        isInitializing = false
        scheduleSessionSave()

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
        // Persist the current tab list synchronously before killing
        // any ptys. The final model state (including auto-titles
        // that arrived mid-session) must make it to disk so the next
        // launch can resume. Skipped in preview/test mode so tests
        // can't pollute the real sessions.json.
        if persistenceEnabled {
            SessionStore.shared.upsert(window: snapshotPersistedWindow())
            SessionStore.shared.flush()
        }

        sessions.tearDown()
        // Release the session-id claim so a future window in this
        // process isn't prevented from adopting this (now-closed)
        // slot if the user wants to "reopen" it.
        Self.claimedWindowIds.remove(windowSessionId)
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
        // is guarded upstream but double-check here defensively.
        let projectId = tabs.projects[pi].id
        if projectsPendingRemoval.contains(projectId),
           tabs.projects[pi].tabs.isEmpty,
           projectId != TabModel.terminalsProjectId {
            projectsPendingRemoval.remove(projectId)
            tabs.projects.remove(at: pi)
        }

        scheduleSessionSave()

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

    // MARK: - Close coordinator

    /// Request to close a pane. If the pane is busy — a thinking or
    /// waiting Claude, or a shell with a foreground child — stage a
    /// confirmation prompt; the UI binds an alert to
    /// `pendingCloseRequest` and calls `confirmPendingClose` /
    /// `cancelPendingClose`. Idle panes are killed immediately.
    func requestClosePane(tabId: String, paneId: String) {
        guard let tab = tabs.tab(for: tabId),
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
    /// `paneExited`).
    func requestCloseTab(tabId: String) {
        guard let tab = tabs.tab(for: tabId) else { return }

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

    /// Request to close an entire project: every tab's panes plus the
    /// project row itself. Refused for the pinned Terminals project,
    /// which is always present by design. If any pane in any tab is
    /// busy, show the confirmation alert; otherwise tear everything
    /// down. The project dissolves once its last tab dissolves (see
    /// `paneExited`).
    func requestCloseProject(projectId: String) {
        guard projectId != TabModel.terminalsProjectId,
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

    /// User confirmed the pending close — force the kill.
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
        }
    }

    /// User dismissed the pending close — leave everything running.
    func cancelPendingClose() {
        pendingCloseRequest = nil
    }

    private func isBusy(tabId: String, pane: Pane) -> Bool {
        guard pane.isAlive else { return false }
        switch pane.kind {
        case .claude:
            // `.thinking` is an active computation; `.waiting` is a live
            // conversation the user might not want to throw away. Only
            // the pre-first-title `.idle` state counts as disposable.
            return pane.status == .thinking || pane.status == .waiting
        case .terminal:
            return sessions.shellHasForegroundChild(tabId: tabId, paneId: pane.id)
        }
    }

    private func describe(pane: Pane) -> String {
        switch pane.kind {
        case .claude:   return "Claude (\(pane.title))"
        case .terminal: return pane.title
        }
    }

    private func hardKillPane(tabId: String, paneId: String) {
        // `terminatePane` sends SIGTERM and tears down the pty; the
        // usual `paneExited` delegate fires and removes the pane from
        // the model, dissolving the tab if it was the last pane.
        sessions.terminatePane(tabId: tabId, paneId: paneId)
    }

    private func hardKillTab(tabId: String) {
        guard let tab = tabs.tab(for: tabId) else { return }

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

        for id in spawnedIds {
            sessions.terminatePane(tabId: tabId, paneId: id)
        }

        guard !unspawnedIds.isEmpty else { return }

        if spawnedIds.isEmpty {
            // Nothing async to hook into — finalize right now.
            tabs.mutateTab(id: tabId) { tab in
                tab.panes.removeAll()
                tab.activePaneId = nil
            }
            if let (pi, ti) = tabs.projectTabIndex(for: tabId) {
                finalizeDissolvedTab(projectIndex: pi, tabIndex: ti, tabId: tabId)
            }
        } else {
            // At least one spawned pane will fire `paneExited` later;
            // clear the unspawned rows now so that exit sees an empty
            // panes list and dissolves through the normal path.
            let toDrop = Set(unspawnedIds)
            tabs.mutateTab(id: tabId) { tab in
                tab.panes.removeAll { toDrop.contains($0.id) }
                if let active = tab.activePaneId, toDrop.contains(active) {
                    tab.activePaneId = tab.panes.first?.id
                }
            }
        }
    }

    /// Force-close every tab in a project and mark the project for
    /// removal so `paneExited` drops the empty row once the last tab
    /// dissolves. Empty projects (no tabs) are removed synchronously
    /// since there's no async pane-exit to wait on.
    private func hardKillProject(projectId: String) {
        guard projectId != TabModel.terminalsProjectId,
              let idx = tabs.projects.firstIndex(where: { $0.id == projectId })
        else { return }

        let tabIds = tabs.projects[idx].tabs.map(\.id)
        if tabIds.isEmpty {
            tabs.projects.remove(at: idx)
            if let active = activeTabId, tabs.tab(for: active) == nil {
                activeTabId = tabs.firstAvailableTabId()
            }
            scheduleSessionSave()
            return
        }

        projectsPendingRemoval.insert(projectId)
        for id in tabIds {
            hardKillTab(tabId: id)
        }
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

    /// Walk projects for every Claude tab with a `claudeSessionId`,
    /// pack into a `PersistedWindow`, and hand to the debounced
    /// `SessionStore`. Called from every mutation site that changes
    /// the restorable tab set: creation, close, pane-exit dissolve,
    /// auto-title, and active-tab switches. Cheap — the store
    /// coalesces 500ms of rapid updates into a single write.
    private func scheduleSessionSave() {
        guard persistenceEnabled, !isInitializing else { return }
        let persisted = snapshotPersistedWindow()
        SessionStore.shared.upsert(window: persisted)
    }

    /// Build a `PersistedWindow` from the current model. Mirrors the
    /// sidebar's project grouping so relaunch recreates the same
    /// sidebar structure — in particular, multi-worktree projects
    /// like "NICE" stay a single project. Persists every tab,
    /// including terminal-only tabs in the pinned Terminals group
    /// (they restore with a fresh shell). Empty projects are dropped
    /// except the Terminals project, which is always persisted so
    /// its cwd survives even when every tab was closed.
    /// Internal (not private) so unit tests can assert the
    /// serialization contract without going through disk — projects
    /// round-trip, empty non-terminals projects are dropped, the
    /// Terminals project is always persisted.
    func snapshotPersistedWindow() -> PersistedWindow {
        var persistedProjects: [PersistedProject] = []
        for project in tabs.projects {
            var persistedTabs: [PersistedTab] = []
            for tab in project.tabs {
                let panes = tab.panes.map {
                    PersistedPane(
                        id: $0.id, title: $0.title, kind: $0.kind, cwd: $0.cwd
                    )
                }
                persistedTabs.append(PersistedTab(
                    id: tab.id,
                    title: tab.title,
                    cwd: tab.cwd,
                    branch: tab.branch,
                    claudeSessionId: tab.claudeSessionId,
                    activePaneId: tab.activePaneId,
                    panes: panes,
                    titleManuallySet: tab.titleManuallySet ? true : nil
                ))
            }
            if persistedTabs.isEmpty && project.id != TabModel.terminalsProjectId {
                continue
            }
            persistedProjects.append(PersistedProject(
                id: project.id,
                name: project.name,
                path: project.path,
                tabs: persistedTabs
            ))
        }
        return PersistedWindow(
            id: windowSessionId,
            activeTabId: activeTabId,
            sidebarCollapsed: sidebarCollapsed,
            projects: persistedProjects
        )
    }

    /// On init: look up this window's saved entry (by
    /// `windowSessionId`) and rebuild its tabs, spawning Claude tabs
    /// with `claude --resume <uuid>` and terminal-only tabs with a
    /// fresh shell. Falls back to adopting an unclaimed entry if
    /// nothing matches this window id — that's how the very first
    /// launch after installing this build picks up the bootstrap file
    /// that was written before `sessions.json` had any live window ids
    /// in it.
    ///
    /// The Claude spawn step is deferred to the next main-queue cycle
    /// so SwiftUI has a chance to mount the new tabs' terminal views
    /// before `startProcess` runs. Claude reads its tty size at
    /// startup and errors out on a 0×0 pty — which is what we got
    /// when the process was spawned synchronously during init, before
    /// the views were ever laid out.
    ///
    /// The pinned Terminals project is guaranteed to exist at index 0
    /// after this runs, regardless of what the snapshot contained.
    private func restoreSavedWindow() {
        let state = SessionStore.shared.load()
        // Try exact match first. If that entry has no projects at all,
        // fall through to the first entry that does — a matched-but-
        // empty slot usually means a prior launch crashed mid-restore;
        // adopting the bootstrap (or whichever window still has state)
        // is the right recovery.
        //
        // If there's no matched slot at all, it's either (a) the first
        // launch of a build where `windowSessionId` semantics changed
        // and the saved state predates it — adopt an unclaimed saved
        // slot as migration, or (b) ⌘N just opened a second window on
        // top of an already-running process — start fresh. Distinguish
        // via the process-wide `claimedWindowIds` set: if some other
        // live AppState already claimed every saved slot we could
        // adopt, we're case (b).
        let matched = state.windows.first(where: { $0.id == windowSessionId })
        let adopted: PersistedWindow?
        if let m = matched, !m.projects.isEmpty {
            adopted = m
        } else if matched != nil {
            // Matched slot exists but is empty — likely a crashed
            // mid-restore. Adopt the first non-empty unclaimed slot.
            adopted = state.windows.first(where: {
                !$0.projects.isEmpty && !Self.claimedWindowIds.contains($0.id)
            })
        } else {
            // No matched slot. Adopt an unclaimed non-empty slot on
            // first-launch migration; stay fresh if every non-empty
            // slot is already owned by another window in this process.
            adopted = state.windows.first(where: {
                !$0.projects.isEmpty && !Self.claimedWindowIds.contains($0.id)
            })
        }

        defer {
            // Claim our slot (either adopted one or our own minted id)
            // so sibling windows spawned next know not to adopt it.
            Self.claimedWindowIds.insert(windowSessionId)
            ensureTerminalsProjectSeededAndSpawn()
        }

        guard let snapshot = adopted else { return }

        // Adopt the entry's id so subsequent saves update that slot
        // instead of creating a duplicate. The view's onChange on
        // `windowSessionId` mirrors the new value back to
        // `@SceneStorage` so the pairing survives relaunch.
        if snapshot.id != windowSessionId {
            windowSessionId = snapshot.id
        }
        // Garbage-collect empty ghost entries left behind by prior
        // launches that failed mid-restore. Keep our newly-adopted
        // slot so scheduleSessionSave has something to upsert into.
        SessionStore.shared.pruneEmptyWindows(keeping: snapshot.id)

        // Drop any in-init seed from the plain constructor — we want
        // the restored Terminals project (with its own tabs and cwd)
        // to win, not collide with the default one.
        let previousMainTabId = tabs.projects.first(where: { $0.id == TabModel.terminalsProjectId })?.tabs.first?.id
        if let mainTabId = previousMainTabId {
            sessions.terminateAll(tabId: mainTabId)
            sessions.removePtySession(tabId: mainTabId)
        }
        tabs.projects.removeAll()

        // Build the Tab/Pane model now so the sidebar shows the tabs
        // immediately; defer the Claude pty spawn so views can lay out
        // first. Trust the saved project grouping — don't re-bucket by
        // cwd.
        var pendingClaudeSpawns: [(tabId: String, cwd: String, claudePaneId: String?, claudeSessionId: String)] = []
        for persistedProject in snapshot.projects {
            let projectIdx = tabs.ensureProject(
                id: persistedProject.id,
                name: persistedProject.name,
                path: persistedProject.path
            )
            for persistedTab in persistedProject.tabs {
                if let spawn = addRestoredTabModel(
                    persistedTab, toProjectIndex: projectIdx
                ) {
                    pendingClaudeSpawns.append(spawn)
                }
            }
        }

        // Self-heal any drift that pre-dates the git-aware bucketing
        // (mis-bucketed tabs from nested repos, projects rooted at
        // sub-directories of a git repo, duplicates, or empties left
        // behind by repair). Idempotent in steady state, so the cost
        // is just the .git existence checks.
        tabs.repairProjectStructure()
        scheduleSessionSave()

        if let active = snapshot.activeTabId, tabs.tab(for: active) != nil {
            activeTabId = active
        }

        // Defer Claude spawning until SwiftUI has laid out the
        // terminal views — the pty reads its size at startup. Two
        // main-queue hops: one for SwiftUI's layout pass, one for the
        // terminal view's first setFrameSize.
        //
        // The restored pane is a plain shell with `claude --resume
        // <uuid>` pre-typed at the prompt (see
        // `ClaudeSessionMode.resumeDeferred`). Nothing runs until the
        // user hits Enter, at which point the zsh `claude()` wrapper
        // handshakes with our control socket and gets promoted in
        // place (see `handleClaudeSocketRequest`).
        DispatchQueue.main.async { [weak self] in
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                for spawn in pendingClaudeSpawns {
                    _ = self.sessions.makeSession(
                        for: spawn.tabId,
                        cwd: spawn.cwd,
                        extraClaudeArgs: [],
                        initialClaudePaneId: spawn.claudePaneId,
                        initialTerminalPaneId: nil,
                        claudeSessionMode: .resumeDeferred(id: spawn.claudeSessionId)
                    )
                    // If the user was last focused on the terminal
                    // companion (rather than the Claude pane), spawn
                    // it now too so `mainContent` has a pty to render
                    // — otherwise the restored tab opens to a blank
                    // background until the user clicks something.
                    self.sessions.ensureActivePaneSpawned(tabId: spawn.tabId)
                }
            }
        }
    }

    /// AppState-side wrapper around `tabs.ensureTerminalsProjectSeeded()`
    /// that also spawns a pty for a freshly-synthesized Main tab. The
    /// pure tree-mutation half lives on `TabModel`; the pty side-effect
    /// is bolted on here so the model itself stays process-free.
    private func ensureTerminalsProjectSeededAndSpawn() {
        switch tabs.ensureTerminalsProjectSeeded() {
        case .existed:
            break
        case let .synthesized(tabId, cwd):
            _ = sessions.makeSession(for: tabId, cwd: cwd)
        }
    }

    /// Append one restored tab's model to `tabs.projects[projectIndex]`.
    /// Claude tabs (tabs with a `claudeSessionId`) return info so the
    /// caller can defer the pty spawn to `claude --resume`. Terminal-
    /// only tabs spawn their shell eagerly and return nil.
    ///
    /// Internal (not private) so tests can assert the returned spawn
    /// cwd falls back from a missing worktree to the project path.
    func addRestoredTabModel(
        _ persisted: PersistedTab,
        toProjectIndex projectIndex: Int
    ) -> (tabId: String, cwd: String, claudePaneId: String?, claudeSessionId: String)? {
        let panes = persisted.panes.map { pp in
            Pane(id: pp.id, title: pp.title, kind: pp.kind, cwd: pp.cwd)
        }
        let defaultActive = panes.first(where: { $0.kind == .claude })?.id
            ?? panes.first?.id
        let tab = Tab(
            id: persisted.id,
            title: persisted.title,
            cwd: persisted.cwd,
            branch: persisted.branch,
            panes: panes,
            activePaneId: persisted.activePaneId ?? defaultActive,
            titleAutoGenerated: persisted.claudeSessionId != nil,
            titleManuallySet: persisted.titleManuallySet ?? false,
            claudeSessionId: persisted.claudeSessionId
        )

        tabs.projects[projectIndex].tabs.append(tab)

        // Resolve after appending so `resolvedSpawnCwd` can see the tab's
        // new project context. Falls back to the project path if the
        // persisted cwd (e.g. a worktree directory) has been deleted
        // since the last launch.
        let spawnCwd = tabs.resolvedSpawnCwd(for: tab)

        if let sid = persisted.claudeSessionId {
            let claudePaneId = panes.first(where: { $0.kind == .claude })?.id
            return (
                tabId: tab.id,
                cwd: spawnCwd,
                claudePaneId: claudePaneId,
                claudeSessionId: sid
            )
        }

        // Terminal-only tab: bring its active pane's shell up now at
        // that pane's last-observed cwd. We honour `activePaneId`
        // instead of letting `makeSession` infer the first terminal —
        // if the user quit while focused on a secondary pane,
        // spawning the first instead leaves `session.panes[activePaneId]`
        // empty and `mainContent` renders the blank fallback. Per-pane
        // cwd falls back to the tab cwd when the pane has none
        // persisted, or its cwd no longer exists. Other panes stay
        // lazy until first focus.
        let activeTerminal = tab.activePaneId.flatMap { id in
            tab.panes.first(where: { $0.id == id && $0.kind == .terminal })
        } ?? tab.panes.first(where: { $0.kind == .terminal })
        if let pane = activeTerminal {
            let paneCwd = tabs.resolvedSpawnCwd(for: tab, pane: pane)
            _ = sessions.makeSession(
                for: tab.id,
                cwd: paneCwd,
                initialTerminalPaneId: pane.id
            )
        } else {
            _ = sessions.makeSession(for: tab.id, cwd: spawnCwd)
        }
        return nil
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
