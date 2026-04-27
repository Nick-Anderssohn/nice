//
//  AppState.swift
//  Nice
//
//  Central app state. Owns the long-lived pty sessions (cached in
//  `ptySessions` keyed by tab id) and fans process-exit / title-change
//  events back into the data model so the sidebar and toolbar can react.
//
//  The "Terminals" group at the top of the sidebar is a regular
//  `Project` with the reserved id `AppState.terminalsProjectId`. It is
//  always present at index 0 and cannot be removed by the user, but its
//  tabs are ordinary `Tab` values with terminal-only panes. On first
//  launch the group holds one "Main" tab; users can add more via the
//  group's `+` button, and the group may be emptied freely.
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
    /// Forwarder onto `TabModel.fileBrowserHeaderTitle` — view-side
    /// callers go through AppState today; the rename pass will point
    /// them at `tabs` directly.
    func fileBrowserHeaderTitle(forTab id: String) -> String {
        tabs.fileBrowserHeaderTitle(forTab: id)
    }

    /// Called by the keyboard monitor when all relevant shortcut
    /// modifiers have been released. The view's separate mouse-hover
    /// pin keeps the overlay rendered if the cursor is over it.
    func endSidebarPeek() {
        sidebarPeeking = false
    }

    // MARK: - Process plumbing

    private(set) var ptySessions: [String: TabPtySession] = [:]

    /// Launch state per pane, used to overlay a "Launching…" placeholder
    /// while a freshly-spawned child is still silent. Entries are created
    /// by `registerPaneLaunch` at spawn time (`.pending`), flip to
    /// `.visible` if the child stays quiet for more than 0.75 s, and are
    /// cleared on first pty byte or pane exit. The 0.75 s grace window
    /// exists so fast-starting processes (regular `claude`, a plain
    /// shell) never flash the overlay — the common case is uninterrupted.
    private(set) var paneLaunchStates: [String: PaneLaunchStatus] = [:]

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

    /// Tracks the SwiftUI `ColorScheme` currently showing. New sessions
    /// are themed at creation using this.
    @ObservationIgnored
    private var currentScheme: ColorScheme = .dark

    /// Tracks the active chrome `Palette` (nice | macOS). New sessions
    /// are themed at creation using this alongside `currentScheme`.
    @ObservationIgnored
    private var currentPalette: Palette = .nice

    /// Tracks the user's active accent as an `NSColor`, used to paint
    /// the terminal caret so the blinking cursor matches the app tint.
    /// Seeded with terracotta; `updateScheme` overwrites on every call.
    @ObservationIgnored
    private var currentAccent: NSColor = AccentPreset.terracotta.nsColor

    /// Tracks the user's terminal font size. New sessions pick this up
    /// at creation; `updateTerminalFontSize` fans changes out to every
    /// live `TabPtySession`.
    @ObservationIgnored
    private var currentTerminalFontSize: CGFloat = FontSettings.defaultTerminalSize

    /// Tracks the terminal theme that every live pane is currently
    /// painted with. Seeded from Nice's built-in dark default so new
    /// sessions created before `updateTerminalTheme` runs still get
    /// sensible colors. `AppShellHost` calls `updateTerminalTheme`
    /// eagerly on first appear, so this only acts as a fallback.
    @ObservationIgnored
    private var currentTerminalTheme: TerminalTheme = BuiltInTerminalThemes.niceDefaultDark

    /// Tracks the user-chosen terminal font family. `nil` => default
    /// chain (SF Mono → JetBrains Mono NL → system monospaced).
    @ObservationIgnored
    private var currentTerminalFontFamily: String? = nil

    /// Absolute path to the `claude` binary if we've resolved it; nil
    /// falls back to zsh inside claude panes. Mirrors
    /// `services.resolvedClaudePath` and is updated by re-arming
    /// `withObservationTracking` when the async probe completes.
    @ObservationIgnored
    private var resolvedClaudePath: String?

    /// Toggled true once `trackClaudePath` has armed its first
    /// observation closure for this AppState's services. Prevents
    /// re-arming when there's no live services pointer.
    @ObservationIgnored
    private weak var trackedServices: NiceServices?

    // MARK: - Control socket

    @ObservationIgnored
    private var controlSocket: NiceControlSocket?
    /// Process-wide ZDOTDIR path owned by `NiceServices`. Stored here
    /// so terminal-pane spawns can inject it as an env var without
    /// reaching back through the services reference. Never deleted by
    /// this AppState — the owning `NiceServices` cleans it up at app
    /// terminate.
    @ObservationIgnored
    private var zdotdirPath: String?
    @ObservationIgnored
    private var controlSocketExtraEnv: [String: String] = [:]

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

        // Seed scheme / palette / accent / terminal-theme / font
        // family from `Tweaks` so the very first `makeSession` call
        // (for the Terminals tab, in `start()`) paints with the user's
        // real preferences. Without this seeding the session is themed
        // against the defaults above (.dark / .nice / terracotta)
        // and only repainted when `AppShellView.onAppear` broadcasts
        // `updateScheme` / `updateTerminalTheme` — a visible flash
        // on launch, and a stubborn mis-theme for chrome-coupled
        // Nice Defaults because their bg/fg derivation reads the
        // session's stale palette.
        if let tweaks = services?.tweaks {
            self.currentScheme = tweaks.scheme
            self.currentPalette = tweaks.activeChromePalette
            self.currentAccent = tweaks.accent.nsColor
            self.currentTerminalFontFamily = tweaks.terminalFontFamily
            if let catalog = services?.terminalThemeCatalog {
                self.currentTerminalTheme = tweaks.effectiveTerminalTheme(
                    for: tweaks.scheme,
                    catalog: catalog
                )
            }
        }
        self.currentTerminalFontSize = services?.fontSettings.terminalFontSize
            ?? FontSettings.defaultTerminalSize

        // Preview / unit-test path: `services == nil` means `start()`
        // is unlikely to be called, so seed `NICE_CLAUDE_OVERRIDE`
        // here. Production reads `services.resolvedClaudePath` in
        // `start()` (after `services.bootstrap()` has populated it).
        if services == nil {
            self.resolvedClaudePath = ProcessInfo.processInfo.environment["NICE_CLAUDE_OVERRIDE"]
        }

        // Seed the pinned Terminals project with one "Main" tab
        // hosting a single terminal pane. The pty session itself is
        // spawned in `start()` so the init is side-effect free.
        self.tabs = TabModel(initialMainCwd: resolvedMainCwd)

        // Stash the services pointer (weak) so `start()` can read
        // `zdotdirPath` / `resolvedClaudePath` and arm claude-path
        // tracking. Doubles as the "services available?" flag for
        // `armClaudePathTracking`.
        self.trackedServices = services

        // Wire tree-mutation save fan-out. `scheduleSessionSave` is
        // gated by `isInitializing` and `persistenceEnabled`, so it's
        // safe for `tabs` to fire this callback synchronously during
        // init (e.g. from `activeTabId`'s `didSet`). Set after the
        // model is built so the very-first seed assignment doesn't
        // bounce through a partially-initialized AppState.
        self.tabs.onTreeMutation = { [weak self] in
            self?.scheduleSessionSave()
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
        if let services = trackedServices {
            self.zdotdirPath = services.zdotdirPath
            self.resolvedClaudePath = services.resolvedClaudePath
        }

        // Allocate the control socket *before* spawning any ptys —
        // the shells need NICE_SOCKET in their environment at startup
        // or the `claude()` shadow can't reach us. Each window owns
        // its own socket so a `claude` invocation in one window's
        // Main Terminal only opens a tab in that window.
        let socket = NiceControlSocket()
        self.controlSocket = socket

        var extraEnv: [String: String] = [:]
        extraEnv["NICE_SOCKET"] = socket.path
        if let zdotdirPath {
            extraEnv["ZDOTDIR"] = zdotdirPath
        }
        self.controlSocketExtraEnv = extraEnv

        // Spawn the seed Main terminal pty. `restoreSavedWindow`
        // below may dissolve and rebuild this if a snapshot exists —
        // that's the existing choreography and is preserved here.
        if let mainTab = tabs.projects.first(where: { $0.id == TabModel.terminalsProjectId })?.tabs.first {
            _ = makeSession(for: mainTab.id, cwd: mainTab.cwd)
        }

        do {
            try socket.start { [weak self] message in
                Task { @MainActor [weak self] in
                    guard let self else { return }
                    switch message {
                    case let .claude(cwd, args, tabId, paneId, reply):
                        self.handleClaudeSocketRequest(
                            cwd: cwd, args: args,
                            tabId: tabId, paneId: paneId,
                            reply: reply
                        )
                    case let .sessionUpdate(paneId, sessionId):
                        self.handleClaudeSessionUpdate(
                            paneId: paneId, sessionId: sessionId
                        )
                    }
                }
            }
        } catch {
            NSLog("AppState: control socket failed to bind: \(error)")
        }

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
    /// `services.resolvedClaudePath` into our local cache. The
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
                self.resolvedClaudePath = services.resolvedClaudePath
                self.armClaudePathTracking()
            }
        }
    }

    /// Snapshot of this window's live panes grouped by kind. Used by
    /// the quit / window-close confirmation alerts to word the prompt
    /// ("N Claude sessions and M terminals") without exposing the model
    /// to callers outside AppState.
    var livePaneCounts: (claude: Int, terminal: Int) {
        var claude = 0
        var terminal = 0
        for project in projects {
            for tab in project.tabs {
                for pane in tab.panes where pane.isAlive {
                    switch pane.kind {
                    case .claude: claude += 1
                    case .terminal: terminal += 1
                    }
                }
            }
        }
        return (claude, terminal)
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

        for session in ptySessions.values {
            session.terminateAll()
        }
        ptySessions.removeAll()
        controlSocket?.stop()
        controlSocket = nil
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

    /// Pick which pane is focused in `tabId`. No-op if `paneId` isn't a
    /// pane on the tab.
    func setActivePane(tabId: String, paneId: String) {
        let viewing = activeTabId == tabId
        tabs.mutateTab(id: tabId) { tab in
            guard let pi = tab.panes.firstIndex(where: { $0.id == paneId }) else {
                return
            }
            tab.activePaneId = paneId
            if viewing {
                tab.panes[pi].markAcknowledgedIfWaiting()
            }
        }
        ensureActivePaneSpawned(tabId: tabId)
    }

    /// Spawn the active pane's PTY if it was deferred at tab creation.
    /// The companion terminal in Claude tabs is modelled up front but its
    /// shell isn't started until the user first switches to it (via click,
    /// keyboard shortcut, or auto-focus after the Claude pane exits).
    private func ensureActivePaneSpawned(tabId: String) {
        guard let tab = tabs.tab(for: tabId),
              let paneId = tab.activePaneId,
              let pane = tab.panes.first(where: { $0.id == paneId }),
              pane.kind == .terminal,
              let session = ptySessions[tabId],
              session.panes[paneId] == nil
        else { return }
        _ = session.addTerminalPane(
            id: paneId, cwd: tabs.resolvedSpawnCwd(for: tab, pane: pane)
        )
    }

    // MARK: - Tab creation

    /// Open a new tab rooted at `cwd`, running `claude` with any `args`
    /// forwarded through. Called from the control socket's `newtab`
    /// handler when a zsh shadow's `claude` fires.
    func createTabFromMainTerminal(cwd: String, args: [String]) {
        let newId = "t\(Int(Date().timeIntervalSince1970 * 1000))"
        let title: String = {
            guard !args.isEmpty else { return "New tab" }
            let joined = args.joined(separator: " ")
            let trimmed = String(joined.prefix(40))
                .trimmingCharacters(in: .whitespaces)
            return trimmed.isEmpty ? "New tab" : trimmed
        }()
        let claudePaneId = "\(newId)-claude"
        let terminalPaneId = "\(newId)-t1"
        // Pre-mint the session UUID so we can pass --session-id to
        // claude and persist the same id for later --resume.
        let sessionId = UUID().uuidString.lowercased()
        var claudePane = Pane(id: claudePaneId, title: "Claude", kind: .claude)
        claudePane.isClaudeRunning = true

        // If the user ran `claude -w <name>`, the Claude CLI creates
        // (and runs inside) a worktree at
        // `<cwd>/.claude/worktrees/<name>`. Keep `projectPath` pointing
        // at the original $PWD so sidebar bucketing still lands under
        // the parent project, and store the worktree path in `Tab.cwd`
        // so the companion terminal follows the session in.
        let projectPath = cwd
        let sessionCwd: String = {
            guard let name = TabModel.extractWorktreeName(from: args) else { return cwd }
            // Claude sanitizes `/` to `+` when deriving the on-disk
            // directory name from the `-w` value (so `foo/bar` becomes
            // `foo+bar`). Mirror that here so the companion terminal
            // lands in the same directory Claude actually created.
            let sanitized = name.replacingOccurrences(of: "/", with: "+")
            return (cwd as NSString).appendingPathComponent(".claude/worktrees/\(sanitized)")
        }()

        let tab = Tab(
            id: newId,
            title: title,
            cwd: sessionCwd,
            branch: nil,
            panes: [
                claudePane,
                Pane(id: terminalPaneId, title: "Terminal 1", kind: .terminal),
            ],
            activePaneId: claudePaneId,
            claudeSessionId: sessionId
        )

        tabs.addTabToProjects(tab, cwd: projectPath)
        activeTabId = newId
        // The companion terminal pane is modelled up front so its pill
        // renders in the toolbar, but its PTY is deferred until the user
        // first focuses it — see `ensureActivePaneSpawned`.
        // Claude pane still launches from `projectPath` so `exec claude
        // -w <name>` continues to resolve/create the worktree itself.
        _ = makeSession(
            for: newId, cwd: projectPath,
            extraClaudeArgs: args,
            initialClaudePaneId: claudePaneId,
            initialTerminalPaneId: nil,
            claudeSessionMode: .new(id: sessionId)
        )
        scheduleSessionSave()
    }

    /// Handle a `claude` invocation from a pane's zsh wrapper. The
    /// wrapper is blocked reading a single-line reply from the socket;
    /// we must call `reply` exactly once. Three outcomes:
    ///
    /// - "newtab": no promotion candidate (the sending tab lives in
    ///   the pinned Terminals group, unknown tabId, or the target
    ///   sidebar tab already has a live Claude). Open a fresh sidebar
    ///   tab via `createTabFromMainTerminal`.
    /// - "inplace": promote the sending pane — flip its kind to
    ///   `.claude` and mark it running. The wrapper `exec`s claude
    ///   with the user's args as-is (they already contain `--resume`
    ///   or `--session-id`).
    /// - "inplace <uuid>": same promotion, but mint a new session id
    ///   so we can later resume it. The wrapper prepends
    ///   `--session-id <uuid>`.
    private func handleClaudeSocketRequest(
        cwd: String,
        args: [String],
        tabId: String,
        paneId: String,
        reply: @Sendable (String) -> Void
    ) {
        // No/unknown tabId, or the request came from a tab in the
        // pinned Terminals group: always open a new sidebar tab.
        guard !tabId.isEmpty,
              !tabs.isTerminalsProjectTab(tabId),
              let existingTab = tabs.tab(for: tabId),
              existingTab.panes.contains(where: { $0.id == paneId })
        else {
            reply("newtab")
            self.createTabFromMainTerminal(cwd: cwd, args: args)
            return
        }

        // Sidebar tab already has a running Claude: spawn-in-place
        // would create a second Claude pane in this tab, violating
        // the "at most one Claude pane per tab" invariant. Open a
        // new tab instead.
        if existingTab.panes.contains(where: { $0.isClaudeRunning }) {
            reply("newtab")
            self.createTabFromMainTerminal(cwd: cwd, args: args)
            return
        }

        // Promotion path. Extract --resume/--session-id from args if
        // present (e.g. the pre-typed `claude --resume <uuid>` on a
        // restored tab); otherwise mint a fresh session id so we can
        // persist it for next relaunch.
        let parsedId = TabModel.extractClaudeSessionId(from: args)
        let sessionId = parsedId ?? UUID().uuidString.lowercased()

        tabs.mutateTab(id: tabId) { tab in
            guard let idx = tab.panes.firstIndex(where: { $0.id == paneId }) else {
                return
            }
            tab.panes[idx].kind = .claude
            tab.panes[idx].isClaudeRunning = true
            // Let the upcoming OSC title from claude set the real label;
            // seed with "Claude" so the pill doesn't render stale text.
            tab.panes[idx].title = "Claude"
            tab.activePaneId = paneId
            tab.claudeSessionId = sessionId
        }
        scheduleSessionSave()

        if parsedId != nil {
            reply("inplace")
        } else {
            reply("inplace \(sessionId)")
        }
    }

    // MARK: - Theme

    func updateScheme(_ scheme: ColorScheme, palette: Palette, accent: NSColor) {
        currentScheme = scheme
        currentPalette = palette
        currentAccent = accent
        for session in ptySessions.values {
            session.applyTheme(scheme, palette: palette, accent: accent)
        }
    }

    /// Fan a new terminal font size out to every live session. Called
    /// by `AppShellHost` on launch and whenever `FontSettings.terminalFontSize`
    /// changes (slider drag or Cmd+/-).
    func updateTerminalFontSize(_ size: CGFloat) {
        currentTerminalFontSize = size
        for session in ptySessions.values {
            session.applyTerminalFont(size: size)
        }
    }

    /// Fan out a terminal-theme change to every live session. Called by
    /// `AppShellHost` when the user picks a new theme in Settings, when
    /// the active scheme flips (sync-with-OS), or when an imported
    /// theme is removed while selected.
    func updateTerminalTheme(_ theme: TerminalTheme) {
        currentTerminalTheme = theme
        for session in ptySessions.values {
            session.applyTerminalTheme(theme)
        }
    }

    /// Fan out a terminal-font-family change. `nil` resets to the
    /// default chain defined in `TabPtySession.terminalFont(named:size:)`.
    func updateTerminalFontFamily(_ name: String?) {
        currentTerminalFontFamily = name
        for session in ptySessions.values {
            session.applyTerminalFontFamily(name)
        }
    }

    // MARK: - Launch overlay

    /// Seam for the pending → visible grace window. Unit tests set this
    /// to 0 so promotion is synchronous.
    var launchOverlayGraceSeconds: Double = 0.75

    /// Record that a pane was just spawned and start the grace timer. If
    /// `clearPaneLaunch` is called before the timer fires (first byte
    /// arrived, or the pane exited) the overlay never appears. If the
    /// timer fires first the entry is promoted to `.visible` and
    /// `AppShellView` starts rendering the "Launching…" overlay.
    func registerPaneLaunch(paneId: String, command: String) {
        paneLaunchStates[paneId] = .pending(command: command)
        let grace = launchOverlayGraceSeconds
        let promote: @MainActor () -> Void = { [weak self] in
            guard let self,
                  case .pending(let cmd)? = self.paneLaunchStates[paneId]
            else { return }
            self.paneLaunchStates[paneId] = .visible(command: cmd)
        }
        if grace <= 0 {
            promote()
        } else {
            DispatchQueue.main.asyncAfter(deadline: .now() + grace, execute: promote)
        }
    }

    /// Remove any pending or visible overlay for this pane. Called from
    /// `NiceTerminalView.onFirstData` on first pty byte and from
    /// `paneExited` so a process that dies before emitting anything
    /// doesn't leave an orphan entry.
    func clearPaneLaunch(paneId: String) {
        paneLaunchStates[paneId] = nil
    }

    // MARK: - Lifecycle handlers

    /// A pane exited. Remove it from its tab, pick a neighbor to focus,
    /// and dissolve the tab if nothing remains. If the last tab in any
    /// project empties out (including the pinned Terminals group), the
    /// project stays in place but its tab list goes to zero — the user
    /// re-adds from the sidebar `+`. If every project is empty after
    /// the dissolve, terminate the app.
    func paneExited(tabId: String, paneId: String, exitCode: Int32?) {
        clearPaneLaunch(paneId: paneId)
        tabs.mutateTab(id: tabId) { tab in
            guard let idx = tab.panes.firstIndex(where: { $0.id == paneId }) else {
                return
            }
            tab.panes.remove(at: idx)
            if tab.activePaneId == paneId {
                if idx < tab.panes.count {
                    tab.activePaneId = tab.panes[idx].id
                } else if idx > 0 {
                    tab.activePaneId = tab.panes[idx - 1].id
                } else {
                    tab.activePaneId = nil
                }
            }
        }

        ptySessions[tabId]?.removePane(id: paneId)
        // If focus auto-switched onto the lazily-spawned companion
        // terminal as a result of this exit, start its shell now.
        ensureActivePaneSpawned(tabId: tabId)

        guard let (pi, ti) = tabs.projectTabIndex(for: tabId),
              tabs.projects[pi].tabs[ti].panes.isEmpty
        else { return }

        finalizeDissolvedTab(projectIndex: pi, tabIndex: ti, tabId: tabId)
    }

    /// Finish tearing down a tab whose panes array has gone to zero:
    /// drop it from its project, release the pty session, reassign
    /// `activeTabId` if it was focused, and drop the project row
    /// itself when the user asked to close the whole project. Called
    /// from `paneExited` after an async pane exit empties the panes
    /// list, and from `hardKillTab` when every pane was unspawned and
    /// there's no async exit to wait on.
    private func finalizeDissolvedTab(
        projectIndex pi: Int,
        tabIndex ti: Int,
        tabId: String
    ) {
        tabs.projects[pi].tabs.remove(at: ti)
        ptySessions.removeValue(forKey: tabId)
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

    /// A pane emitted a window-title update via OSC 0/1/2. Claude panes
    /// encode thinking/waiting as a leading braille-spinner or asterisk;
    /// the trailing text is the session label (e.g. "fix-top-bar-height")
    /// which becomes the sidebar tab title. The claude-pane pill itself
    /// stays pinned to "Claude". Terminal panes take the emitted title
    /// verbatim as their toolbar pill label.
    func paneTitleChanged(tabId: String, paneId: String, title: String) {
        guard let tab = tabs.tab(for: tabId),
              let pane = tab.panes.first(where: { $0.id == paneId })
        else { return }

        if pane.kind == .terminal {
            let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty else { return }
            let clipped: String = {
                guard trimmed.count > 40 else { return trimmed }
                let idx = trimmed.index(trimmed.startIndex, offsetBy: 40)
                return String(trimmed[..<idx]).trimmingCharacters(in: .whitespaces)
            }()
            tabs.mutateTab(id: tabId) { tab in
                guard let pi = tab.panes.firstIndex(where: { $0.id == paneId }) else {
                    return
                }
                if tab.panes[pi].title != clipped {
                    tab.panes[pi].title = clipped
                }
            }
            return
        }

        // Claude pane: split off the status prefix, update pane/tab
        // status, and feed the trailing label into the tab title.
        guard let first = title.unicodeScalars.first else { return }
        let newStatus: TabStatus?
        let labelStart: String.Index
        if first.value >= 0x2800 && first.value <= 0x28FF {
            // Braille-spinner prefix: Claude is thinking.
            newStatus = .thinking
            labelStart = title.index(after: title.startIndex)
        } else if first == "\u{2733}" {
            // Sparkle: Claude is waiting for input.
            newStatus = .waiting
            labelStart = title.index(after: title.startIndex)
        } else {
            newStatus = nil
            labelStart = title.startIndex
        }

        if let newStatus {
            let viewing = (activeTabId == tabId)
            tabs.mutateTab(id: tabId) { tab in
                guard let pi = tab.panes.firstIndex(where: { $0.id == paneId }) else {
                    return
                }
                let isActivePane = (tab.activePaneId == paneId)
                tab.panes[pi].applyStatusTransition(
                    to: newStatus,
                    isCurrentlyBeingViewed: viewing && isActivePane
                )
            }
        }

        let rawLabel = title[labelStart...]
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard !rawLabel.isEmpty else { return }
        // Ignore Claude's generic placeholder before a session is named.
        if rawLabel == "Claude Code" { return }
        tabs.applyAutoTitle(tabId: tabId, rawTitle: rawLabel)
    }

    /// A pane's shell emitted OSC 7 with a new working directory. Stash
    /// it on `Pane.cwd` so a relaunch respawns the pane in the same
    /// place. Persistence is debounced inside `SessionStore`, so naive
    /// save-on-every-update is cheap. We deliberately don't touch
    /// `Tab.cwd` — that field is load-bearing for `claude --resume`'s
    /// working dir on Claude tabs, and overwriting it from a companion
    /// terminal's cwd would silently relocate the session on restore.
    func paneCwdChanged(tabId: String, paneId: String, cwd: String) {
        var changed = false
        tabs.mutateTab(id: tabId) { tab in
            guard let pi = tab.panes.firstIndex(where: { $0.id == paneId })
            else { return }
            if tab.panes[pi].cwd != cwd {
                tab.panes[pi].cwd = cwd
                changed = true
            }
        }
        if changed {
            scheduleSessionSave()
        }
    }

    /// Forwarder onto `TabModel.applyAutoTitle`. Kept on AppState so
    /// any external caller (and the existing internal call from
    /// `paneTitleChanged` before its own move) keeps working.
    func applyAutoTitle(tabId: String, rawTitle: String) {
        tabs.applyAutoTitle(tabId: tabId, rawTitle: rawTitle)
    }

    /// Hand AppKit first-responder status back to the active pane's
    /// terminal view. Call after any SwiftUI control (e.g. the sidebar
    /// rename field) finishes editing — SwiftUI does not restore focus
    /// to an embedded `NSView` when a TextField is torn down, so keys
    /// fall off the responder chain until the user clicks the terminal.
    /// The async hop lets SwiftUI finish its current update before the
    /// responder change, matching the pattern in `TerminalHost`.
    func focusActiveTerminal() {
        guard let tabId = activeTabId,
              let tab = tabs.tab(for: tabId),
              let paneId = tab.activePaneId,
              let session = ptySessions[tabId],
              let view = session.panes[paneId]
        else { return }
        view.wantsFocusOnAttach = true
        DispatchQueue.main.async {
            view.window?.makeFirstResponder(view)
        }
    }

    /// Forwarder onto `TabModel.renameTab`. Views call
    /// `appState.renameTab(...)` from the sidebar inline editor; the
    /// rename pass will eventually point them at `tabs` directly.
    func renameTab(id tabId: String, to newTitle: String) {
        tabs.renameTab(id: tabId, to: newTitle)
    }

    /// Append a new terminal-only tab to the pinned Terminals group,
    /// focus it, and spawn its pty. Used by the sidebar's group-level
    /// `+` button. First tab added to an empty group is titled "Main";
    /// subsequent tabs are auto-numbered "Main 2", "Main 3", etc.
    /// Cwd inherits the Terminals project's path.
    @discardableResult
    func createTerminalTab() -> String? {
        guard let pi = tabs.projects.firstIndex(where: { $0.id == TabModel.terminalsProjectId }) else {
            return nil
        }
        let project = tabs.projects[pi]
        let title: String
        if project.tabs.isEmpty {
            title = "Main"
        } else {
            title = "Main \(project.tabs.count + 1)"
        }
        let newId = "tt\(Int(Date().timeIntervalSince1970 * 1000))"
        let paneId = "\(newId)-p0"
        let cwd = project.path
        let tab = Tab(
            id: newId,
            title: title,
            cwd: cwd,
            branch: nil,
            panes: [Pane(id: paneId, title: "zsh", kind: .terminal)],
            activePaneId: paneId
        )
        tabs.projects[pi].tabs.append(tab)
        activeTabId = newId
        _ = makeSession(for: newId, cwd: cwd)
        scheduleSessionSave()
        return newId
    }

    /// Create a fresh Claude tab in an existing project group. Mirrors
    /// `createTabFromMainTerminal` but targets `projectId` directly so
    /// the sidebar's per-project `+` button can add into that project
    /// instead of bucketing by cwd. No-op for the pinned Terminals
    /// group (which only holds terminal tabs).
    @discardableResult
    func createClaudeTabInProject(projectId: String) -> String? {
        guard projectId != TabModel.terminalsProjectId,
              let pi = tabs.projects.firstIndex(where: { $0.id == projectId })
        else { return nil }
        let project = tabs.projects[pi]
        let newId = "t\(Int(Date().timeIntervalSince1970 * 1000))"
        let claudePaneId = "\(newId)-claude"
        let terminalPaneId = "\(newId)-t1"
        let sessionId = UUID().uuidString.lowercased()
        var claudePane = Pane(id: claudePaneId, title: "Claude", kind: .claude)
        claudePane.isClaudeRunning = true
        let tab = Tab(
            id: newId,
            title: "New tab",
            cwd: project.path,
            branch: nil,
            panes: [
                claudePane,
                Pane(id: terminalPaneId, title: "Terminal 1", kind: .terminal),
            ],
            activePaneId: claudePaneId,
            claudeSessionId: sessionId
        )
        tabs.projects[pi].tabs.append(tab)
        activeTabId = newId
        _ = makeSession(
            for: newId, cwd: project.path,
            extraClaudeArgs: [],
            initialClaudePaneId: claudePaneId,
            initialTerminalPaneId: nil,
            claudeSessionMode: .new(id: sessionId)
        )
        scheduleSessionSave()
        return newId
    }

    /// True when `tabId` lives inside the pinned Terminals project.
    /// Forwarder onto `TabModel.isTerminalsProjectTab`.
    func isTerminalsProjectTab(_ tabId: String) -> Bool {
        tabs.isTerminalsProjectTab(tabId)
    }

    // MARK: - Pane management

    /// Append a new terminal pane to `tabId`, spawn its pty, and focus
    /// it. Returns the new pane id, or nil if the tab doesn't exist.
    ///
    /// `command`, when set, runs that command instead of a plain login
    /// shell (used by the File Explorer's "Open in Editor Pane" path).
    /// On exit the pane drops via the existing `paneExited` flow.
    @discardableResult
    func addPane(
        tabId: String,
        kind: PaneKind = .terminal,
        cwd: String? = nil,
        title: String? = nil,
        command: String? = nil
    ) -> String? {
        // Only terminal kind is exposed to callers. Claude panes are
        // created exclusively by `createTabFromMainTerminal` — this
        // preserves the "at most one Claude pane per tab" invariant.
        guard kind == .terminal else { return nil }

        guard let tab = tabs.tab(for: tabId) else { return nil }
        let newId = "\(tabId)-p\(Int(Date().timeIntervalSince1970 * 1000))"
        let termCount = tab.panes.filter { $0.kind == .terminal }.count
        let resolvedTitle = title ?? "Terminal \(termCount + 1)"

        // Resolve the spawn cwd before mutating the tab — once we
        // re-point `activePaneId` at the new pane below, the "spawning"
        // pane is no longer recoverable.
        let spawnCwd = tabs.spawnCwdForNewPane(in: tab, callerProvided: cwd)

        tabs.mutateTab(id: tabId) { tab in
            tab.panes.append(
                Pane(id: newId, title: resolvedTitle, kind: .terminal)
            )
            tab.activePaneId = newId
        }

        let session: TabPtySession
        if let existing = ptySessions[tabId] {
            session = existing
        } else {
            session = makeSession(for: tabId, cwd: spawnCwd)
        }
        _ = session.addTerminalPane(id: newId, cwd: spawnCwd, command: command)
        return newId
    }

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
            return ptySessions[tabId]?.shellHasForegroundChild(id: pane.id) ?? false
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
        ptySessions[tabId]?.terminatePane(id: paneId)
    }

    private func hardKillTab(tabId: String) {
        guard let tab = tabs.tab(for: tabId) else { return }
        let session = ptySessions[tabId]

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
            if session?.panes[pane.id] != nil {
                spawnedIds.append(pane.id)
            } else {
                unspawnedIds.append(pane.id)
            }
        }

        for id in spawnedIds {
            session?.terminatePane(id: id)
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

    /// Move focus to the next pane within the active tab, wrapping. No-op
    /// when the active tab has fewer than two panes.
    func selectNextPane() { stepActivePane(by: +1) }

    /// Move focus to the previous pane within the active tab, wrapping.
    func selectPrevPane() { stepActivePane(by: -1) }

    private func stepActivePane(by offset: Int) {
        guard let tabId = activeTabId, let tab = tabs.tab(for: tabId) else { return }
        guard tab.panes.count > 1, let activeId = tab.activePaneId,
              let currentIdx = tab.panes.firstIndex(where: { $0.id == activeId })
        else { return }
        let nextIdx = ((currentIdx + offset) % tab.panes.count + tab.panes.count) % tab.panes.count
        setActivePane(tabId: tabId, paneId: tab.panes[nextIdx].id)
    }

    /// Append a new terminal pane to the active tab and focus it. No-op
    /// when there is no active tab.
    func addTerminalToActiveTab() {
        guard let id = activeTabId else { return }
        _ = addPane(tabId: id, kind: .terminal)
    }

    // MARK: - Pty sessions

    /// Return the pty session for `tabId`, creating and caching one if
    /// it doesn't exist yet. Spawns initial panes based on the tab's
    /// model state.
    @discardableResult
    private func makeSession(
        for tabId: String,
        cwd: String,
        extraClaudeArgs: [String] = [],
        initialClaudePaneId: String? = nil,
        initialTerminalPaneId: String? = nil,
        claudeSessionMode: TabPtySession.ClaudeSessionMode = .none
    ) -> TabPtySession {
        if let existing = ptySessions[tabId] {
            return existing
        }
        let resolvedCwd = TabModel.expandTilde(cwd)

        // Work out which panes to spawn. Callers can pass ids explicitly
        // (e.g. createTabFromMainTerminal) or we infer them from the
        // model.
        var claudePaneId = initialClaudePaneId
        var terminalPaneId = initialTerminalPaneId
        if claudePaneId == nil && terminalPaneId == nil {
            if let tab = tabs.tab(for: tabId) {
                for pane in tab.panes {
                    switch pane.kind {
                    case .claude where claudePaneId == nil:
                        claudePaneId = pane.id
                    case .terminal where terminalPaneId == nil:
                        terminalPaneId = pane.id
                    default:
                        break
                    }
                }
            }
        }

        let session = TabPtySession(
            tabId: tabId,
            cwd: resolvedCwd,
            claudeBinary: resolvedClaudePath,
            extraClaudeArgs: extraClaudeArgs,
            initialClaudePaneId: claudePaneId,
            initialTerminalPaneId: terminalPaneId,
            socketPath: controlSocket?.path,
            zdotdirPath: zdotdirPath,
            claudeSessionMode: claudeSessionMode,
            onPaneExit: { [weak self] paneId, code in
                self?.paneExited(tabId: tabId, paneId: paneId, exitCode: code)
            },
            onPaneTitleChange: { [weak self] paneId, title in
                self?.paneTitleChanged(tabId: tabId, paneId: paneId, title: title)
            },
            onPaneCwdChange: { [weak self] paneId, cwd in
                self?.paneCwdChanged(tabId: tabId, paneId: paneId, cwd: cwd)
            },
            onPaneLaunched: { [weak self] paneId, command in
                self?.registerPaneLaunch(paneId: paneId, command: command)
            },
            onPaneFirstOutput: { [weak self] paneId in
                self?.clearPaneLaunch(paneId: paneId)
            }
        )
        session.applyTerminalFontFamily(currentTerminalFontFamily)
        // applyTheme must run before applyTerminalTheme so the session
        // has its current scheme / palette cached — the Nice Default
        // (chrome-coupled) paths in applyTerminalTheme derive
        // bg / fg from those values, and reading them stale paints
        // the terminal with the wrong light/dark variant.
        session.applyTheme(currentScheme, palette: currentPalette, accent: currentAccent)
        session.applyTerminalTheme(currentTerminalTheme)
        session.applyTerminalFont(size: currentTerminalFontSize)
        ptySessions[tabId] = session
        return session
    }

    // MARK: - Lookup

    /// Forwarder onto `TabModel.tab(for:)`. Views and tests still call
    /// `appState.tab(for:)`; the rename pass will retarget them at
    /// `tabs` directly.
    func tab(for id: String) -> Tab? {
        tabs.tab(for: id)
    }

    // MARK: - Session persistence

    /// Handle a `session_update` socket message from Claude Code's
    /// UserPromptSubmit hook. Looks up the tab whose pane set contains
    /// `paneId` and forwards to `updateClaudeSessionId`. Silent no-op
    /// if the pane is stale (exited while the hook's `nc` was in
    /// flight) or isn't a claude pane.
    /// `internal` so unit tests can drive the dispatch path directly
    /// without standing up a real socket — matches `paneExited`'s
    /// access level for the same reason.
    func handleClaudeSessionUpdate(paneId: String, sessionId: String) {
        guard let tabId = tabs.tabIdOwning(paneId: paneId) else { return }
        updateClaudeSessionId(tabId: tabId, sessionId: sessionId)
    }

    /// Update `tab.claudeSessionId` when claude rotates its session
    /// mid-process — `/clear`, `/compact`, and `/branch` all swap the
    /// UUID without restarting the process, so the pre-minted id we
    /// stored at tab creation goes stale. Persist the new id immediately
    /// so an unexpected Nice shutdown still resumes the correct
    /// conversation. No-op if the tab already has this id or no longer
    /// exists.
    private func updateClaudeSessionId(tabId: String, sessionId: String) {
        var changed = false
        tabs.mutateTab(id: tabId) { tab in
            if tab.claudeSessionId != sessionId {
                tab.claudeSessionId = sessionId
                changed = true
            }
        }
        if changed {
            scheduleSessionSave()
        }
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
            ptySessions[mainTabId]?.terminateAll()
            ptySessions.removeValue(forKey: mainTabId)
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
                    _ = self.makeSession(
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
                    self.ensureActivePaneSpawned(tabId: spawn.tabId)
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
            _ = makeSession(for: tabId, cwd: cwd)
        }
    }

    /// Append one restored tab's model to `projects[projectIndex]`.
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
            _ = makeSession(
                for: tab.id,
                cwd: paneCwd,
                initialTerminalPaneId: pane.id
            )
        } else {
            _ = makeSession(for: tab.id, cwd: spawnCwd)
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
