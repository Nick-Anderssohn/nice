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
final class AppState: ObservableObject {
    /// Reserved id for the pinned Terminals project at index 0 of
    /// `projects`. The project is always present and cannot be deleted
    /// by the user; its tabs are ordinary terminal-only tabs.
    static let terminalsProjectId = "terminals"
    /// Stable id for the default "Main" tab seeded into the Terminals
    /// project on fresh launches. UI tests key off a `sidebar.terminals`
    /// accessibility alias on this tab.
    static let mainTerminalTabId = "terminals-main"

    /// Set of `windowSessionId`s already claimed by live AppStates in
    /// this process. Populated by `restoreSavedWindow` after it picks
    /// a slot. `restoreSavedWindow` consults this to decide whether a
    /// miss-on-match should adopt an unclaimed saved entry (legitimate
    /// first-launch migration) or stay fresh (⌘N opened a second
    /// window; adopting the first window's slot would duplicate pane
    /// ids and defeat per-window isolation).
    private static var claimedWindowIds: Set<String> = []

    @Published var projects: [Project]
    /// Currently-selected tab. Defaults to the Main terminal tab on
    /// launch.
    @Published var activeTabId: String? {
        didSet {
            // Viewing a tab dismisses the attention pulse on its active
            // pane's waiting state — centralised here so every call site
            // that flips `activeTabId` gets the same acknowledgment.
            if let id = activeTabId, id != oldValue {
                acknowledgeWaitingOnActivePane(tabId: id)
                scheduleSessionSave()
            }
        }
    }
    /// Whether the sidebar is collapsed. Seeded from the per-window
    /// `@SceneStorage` value by the owning view so each window keeps
    /// its own state; the view writes back on changes.
    @Published var sidebarCollapsed: Bool = false

    /// Which content the sidebar is showing (tabs vs file browser).
    /// Seeded from the per-window `@SceneStorage` value upstream so
    /// each window restores its last-used mode across relaunch.
    @Published var sidebarMode: SidebarMode = .tabs

    /// Per-window catalog of file-browser states keyed by `Tab.id`.
    /// Lifecycle: states are lazily created on first access and
    /// removed in `finalizeDissolvedTab` when a tab dissolves. The
    /// store is `@Published` indirectly via its own `ObservableObject`
    /// surface — views observing it pick up changes without
    /// `AppState` re-publishing.
    let fileBrowserStore: FileBrowserStore = FileBrowserStore()

    /// Transient: sidebar is floating over the terminal as a peek
    /// triggered by the tab-cycling shortcut while collapsed. Set by
    /// `KeyboardShortcutMonitor` after a sidebar-tab dispatch, cleared
    /// when the user releases the shortcut's modifiers. Never set while
    /// `sidebarCollapsed == false`. The view layer ORs this with its own
    /// mouse-hover pin so a hovered peek stays open after the keys lift.
    @Published var sidebarPeeking: Bool = false

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

    /// Project that owns the given tab, or `nil` if no such tab is
    /// currently in the model. Used internally — callers outside
    /// `AppState` should prefer the more specific helpers below
    /// (e.g. `fileBrowserHeaderTitle(forTab:)`) so they don't grow
    /// dependencies on `Project`'s shape.
    private func project(forTab id: String) -> Project? {
        for project in projects where project.tabs.contains(where: { $0.id == id }) {
            return project
        }
        return nil
    }

    /// Title to show at the top of the file browser for `tabId`.
    /// Encapsulates the rule "use the owning project's name unless
    /// the tab is in the pinned Terminals project (whose name is
    /// generic), in which case fall back to the tab's own title."
    /// Lives here so the file-browser view doesn't have to know
    /// about `terminalsProjectId` or how tabs nest into projects.
    func fileBrowserHeaderTitle(forTab id: String) -> String {
        let tabTitle = tab(for: id)?.title
        guard let project = project(forTab: id) else {
            return tabTitle ?? "Files"
        }
        if project.id == Self.terminalsProjectId {
            return tabTitle ?? project.name
        }
        return project.name
    }

    /// Called by the keyboard monitor when all relevant shortcut
    /// modifiers have been released. The view's separate mouse-hover
    /// pin keeps the overlay rendered if the cursor is over it.
    func endSidebarPeek() {
        sidebarPeeking = false
    }

    // MARK: - Process plumbing

    @Published private(set) var ptySessions: [String: TabPtySession] = [:]

    /// Launch state per pane, used to overlay a "Launching…" placeholder
    /// while a freshly-spawned child is still silent. Entries are created
    /// by `registerPaneLaunch` at spawn time (`.pending`), flip to
    /// `.visible` if the child stays quiet for more than 0.75 s, and are
    /// cleared on first pty byte or pane exit. The 0.75 s grace window
    /// exists so fast-starting processes (regular `claude`, a plain
    /// shell) never flash the overlay — the common case is uninterrupted.
    @Published private(set) var paneLaunchStates: [String: PaneLaunchStatus] = [:]

    /// In-flight "processes still running" confirmation. Set by
    /// `requestClosePane` / `requestCloseTab` / `requestCloseProject`
    /// when they find something busy; cleared by `confirmPendingClose`
    /// (after the kill) or `cancelPendingClose` (user backs out).
    /// `AppShellView` binds an `.alert` to this.
    @Published var pendingCloseRequest: PendingCloseRequest?

    /// Project ids the user asked to fully close. When a tab in one of
    /// these projects finishes dissolving in `paneExited`, the empty
    /// project row is also removed from `projects`. The Terminals
    /// project is excluded upstream (its id is never added).
    private var projectsPendingRemoval: Set<String> = []

    /// Stable identifier for this window's entry in `sessions.json`.
    /// Pulled in from `@SceneStorage("windowSessionId")` on
    /// `AppShellView`; survives quits via standard SwiftUI scene
    /// storage so the same window restores the same tab list on
    /// relaunch. `@Published` so the view layer can mirror adoption
    /// changes back into SceneStorage — `restoreSavedWindow` may
    /// switch us to the bootstrap id, and that re-pairing must
    /// persist.
    @Published private(set) var windowSessionId: String

    /// Blocks `scheduleSessionSave` while `init` is still running.
    /// Swift fires `activeTabId`'s `didSet` for the seed assignment
    /// in some optional-typed cases, which would otherwise upsert an
    /// empty window entry before `restoreSavedWindow` has a chance to
    /// adopt the bootstrap. Cleared on the last line of `init`.
    private var isInitializing: Bool = true

    /// False in preview/test mode (`services == nil` at init). Blocks
    /// `scheduleSessionSave` so unit tests can't pollute the real
    /// `~/Library/Application Support/Nice/sessions.json` by exercising
    /// the tab-mutation surface.
    private let persistenceEnabled: Bool

    /// Tracks the SwiftUI `ColorScheme` currently showing. New sessions
    /// are themed at creation using this.
    private var currentScheme: ColorScheme = .dark

    /// Tracks the active chrome `Palette` (nice | macOS). New sessions
    /// are themed at creation using this alongside `currentScheme`.
    private var currentPalette: Palette = .nice

    /// Tracks the user's active accent as an `NSColor`, used to paint
    /// the terminal caret so the blinking cursor matches the app tint.
    /// Seeded with terracotta; `updateScheme` overwrites on every call.
    private var currentAccent: NSColor = AccentPreset.terracotta.nsColor

    /// Tracks the user's terminal font size. New sessions pick this up
    /// at creation; `updateTerminalFontSize` fans changes out to every
    /// live `TabPtySession`.
    private var currentTerminalFontSize: CGFloat = FontSettings.defaultTerminalSize

    /// Tracks the GPU rendering preference (`Tweaks.gpuRendering`). New
    /// sessions seed from this; `updateGpuRendering` fans changes out
    /// to every live `TabPtySession` so the Metal renderer toggles in
    /// place. Defaults to `true` to match `Tweaks.gpuRendering`.
    private var currentGpuRendering: Bool = true

    /// Tracks the smooth-scrolling preference (`Tweaks.smoothScrolling`).
    /// Same fan-out story as `currentGpuRendering`. Defaults match
    /// `Tweaks.smoothScrolling` (on).
    private var currentSmoothScrolling: Bool = true

    /// Tracks the terminal theme that every live pane is currently
    /// painted with. Seeded from Nice's built-in dark default so new
    /// sessions created before `updateTerminalTheme` runs still get
    /// sensible colors. `AppShellHost` calls `updateTerminalTheme`
    /// eagerly on first appear, so this only acts as a fallback.
    private var currentTerminalTheme: TerminalTheme = BuiltInTerminalThemes.niceDefaultDark

    /// Tracks the user-chosen terminal font family. `nil` => default
    /// chain (SF Mono → JetBrains Mono NL → system monospaced).
    private var currentTerminalFontFamily: String? = nil

    /// Absolute path to the `claude` binary if we've resolved it; nil
    /// falls back to zsh inside claude panes.
    private var resolvedClaudePath: String?

    // MARK: - Control socket

    private var controlSocket: NiceControlSocket?
    /// Process-wide ZDOTDIR path owned by `NiceServices`. Stored here
    /// so terminal-pane spawns can inject it as an env var without
    /// reaching back through the services reference. Never deleted by
    /// this AppState — the owning `NiceServices` cleans it up at app
    /// terminate.
    private var zdotdirPath: String?
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

        // Allocate the control socket *before* spawning any ptys — the
        // shells need NICE_SOCKET in their environment at startup or
        // the `claude()` shadow can't reach us. Each window owns its
        // own socket so a `claude` invocation in one window's Main
        // Terminal only opens a tab in that window. The ZDOTDIR is
        // process-wide and written by `NiceServices` before the first
        // AppState is constructed; we just read its path here.
        let socket = NiceControlSocket()
        self.controlSocket = socket
        self.zdotdirPath = services?.zdotdirPath

        // Seed scheme / palette / accent / terminal-theme / font
        // family from `Tweaks` so the very first `makeSession` call
        // below (for the Terminals tab) paints with the user's real
        // preferences. Without this seeding the session is themed
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
            self.currentGpuRendering = tweaks.gpuRendering
            self.currentSmoothScrolling = tweaks.smoothScrolling
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

        var extraEnv: [String: String] = [:]
        extraEnv["NICE_SOCKET"] = socket.path
        if let zdotdirPath {
            extraEnv["ZDOTDIR"] = zdotdirPath
        }
        self.controlSocketExtraEnv = extraEnv

        // Prefer the process-wide cached `claude` path from services;
        // fall back to probing if services isn't available (previews /
        // unit tests). Probing a login shell costs 200–500ms so the
        // cache materially improves second-window open latency.
        if let cached = services?.resolvedClaudePath {
            self.resolvedClaudePath = cached
        } else {
            self.resolvedClaudePath = ProcessInfo.processInfo.environment["NICE_CLAUDE_OVERRIDE"]
                ?? Self.runWhich(binary: "claude")
        }

        // Seed the pinned Terminals project with one "Main" tab
        // hosting a single terminal pane.
        let mainTabId = Self.mainTerminalTabId
        let initialPaneId = "\(mainTabId)-p\(Int(Date().timeIntervalSince1970 * 1000))"
        let initialPane = Pane(id: initialPaneId, title: "zsh", kind: .terminal)
        let mainTab = Tab(
            id: mainTabId,
            title: "Main",
            cwd: resolvedMainCwd,
            branch: nil,
            panes: [initialPane],
            activePaneId: initialPaneId
        )
        let terminalsProject = Project(
            id: Self.terminalsProjectId,
            name: "Terminals",
            path: resolvedMainCwd,
            tabs: [mainTab]
        )
        self.projects = [terminalsProject]
        self.activeTabId = mainTabId

        // All stored properties set — now bring up the session for the
        // Main terminal tab and start the control socket.
        _ = self.makeSession(for: mainTabId, cwd: resolvedMainCwd)

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

    func selectTab(_ id: String) {
        activeTabId = id
    }

    /// Pick which pane is focused in `tabId`. No-op if `paneId` isn't a
    /// pane on the tab.
    func setActivePane(tabId: String, paneId: String) {
        let viewing = activeTabId == tabId
        mutateTab(id: tabId) { tab in
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
        guard let tab = tab(for: tabId),
              let paneId = tab.activePaneId,
              let pane = tab.panes.first(where: { $0.id == paneId }),
              pane.kind == .terminal,
              let session = ptySessions[tabId],
              session.panes[paneId] == nil
        else { return }
        _ = session.addTerminalPane(
            id: paneId, cwd: resolvedSpawnCwd(for: tab, pane: pane)
        )
    }

    /// Clear the waiting-attention pulse on whichever pane is currently
    /// focused in `tabId`. Called from the `activeTabId` `didSet` when
    /// the user navigates to a different tab.
    private func acknowledgeWaitingOnActivePane(tabId: String) {
        mutateTab(id: tabId) { tab in
            guard let paneId = tab.activePaneId,
                  let pi = tab.panes.firstIndex(where: { $0.id == paneId })
            else { return }
            tab.panes[pi].markAcknowledgedIfWaiting()
        }
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
            guard let name = Self.extractWorktreeName(from: args) else { return cwd }
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

        addTabToProjects(tab, cwd: projectPath)
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
              !isTerminalsProjectTab(tabId),
              let existingTab = self.tab(for: tabId),
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
        let parsedId = Self.extractClaudeSessionId(from: args)
        let sessionId = parsedId ?? UUID().uuidString.lowercased()

        mutateTab(id: tabId) { tab in
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

    /// Scan `args` for the session UUID the user already supplied via
    /// `--resume <id>`, `--session-id <id>`, `--resume=<id>`, or
    /// `--session-id=<id>`. Returns nil if none is present.
    private static func extractClaudeSessionId(from args: [String]) -> String? {
        var i = 0
        while i < args.count {
            let a = args[i]
            if a == "--resume" || a == "--session-id" {
                if i + 1 < args.count {
                    return args[i + 1]
                }
            } else if a.hasPrefix("--resume=") {
                return String(a.dropFirst("--resume=".count))
            } else if a.hasPrefix("--session-id=") {
                return String(a.dropFirst("--session-id=".count))
            }
            i += 1
        }
        return nil
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

    /// Pushed by `AppShellView` whenever `Tweaks.gpuRendering` changes
    /// (and once on launch). Updates the cached value used to seed
    /// future sessions and broadcasts to every live one.
    func updateGpuRendering(_ enabled: Bool) {
        currentGpuRendering = enabled
        for session in ptySessions.values {
            session.applyGpuRendering(enabled: enabled)
        }
    }

    /// Mirror of `updateGpuRendering` for the smooth-scrolling toggle.
    func updateSmoothScrolling(_ enabled: Bool) {
        currentSmoothScrolling = enabled
        for session in ptySessions.values {
            session.applySmoothScrolling(enabled: enabled)
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
        mutateTab(id: tabId) { tab in
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

        guard let (pi, ti) = projectTabIndex(for: tabId),
              projects[pi].tabs[ti].panes.isEmpty
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
        projects[pi].tabs.remove(at: ti)
        ptySessions.removeValue(forKey: tabId)
        fileBrowserStore.removeState(forTab: tabId)
        if activeTabId == tabId {
            activeTabId = firstAvailableTabId()
        }

        // If the user asked to close this whole project (right-click →
        // Close Project), drop the now-empty project row too. Terminals
        // is guarded upstream but double-check here defensively.
        let projectId = projects[pi].id
        if projectsPendingRemoval.contains(projectId),
           projects[pi].tabs.isEmpty,
           projectId != Self.terminalsProjectId {
            projectsPendingRemoval.remove(projectId)
            projects.remove(at: pi)
        }

        scheduleSessionSave()

        if projects.allSatisfy({ $0.tabs.isEmpty }) {
            NSApp.terminate(nil)
        }
    }

    /// First tab id in sidebar order (Terminals project, then project
    /// tabs). Used to fall back to a sensible selection when the
    /// active tab dissolves. Returns nil when no tab exists anywhere.
    ///
    /// Internal — not private — so `AppState+FileExplorer.openInEditorPane`
    /// can use it as the active-tab fallback when the user clicks an
    /// editor entry while no tab is focused. Keep at internal; no
    /// external module needs it.
    func firstAvailableTabId() -> String? {
        for project in projects {
            if let id = project.tabs.first?.id { return id }
        }
        return nil
    }

    /// A pane emitted a window-title update via OSC 0/1/2. Claude panes
    /// encode thinking/waiting as a leading braille-spinner or asterisk;
    /// the trailing text is the session label (e.g. "fix-top-bar-height")
    /// which becomes the sidebar tab title. The claude-pane pill itself
    /// stays pinned to "Claude". Terminal panes take the emitted title
    /// verbatim as their toolbar pill label.
    func paneTitleChanged(tabId: String, paneId: String, title: String) {
        guard let tab = tab(for: tabId),
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
            mutateTab(id: tabId) { tab in
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
            mutateTab(id: tabId) { tab in
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
        applyAutoTitle(tabId: tabId, rawTitle: rawLabel)
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
        mutateTab(id: tabId) { tab in
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

    /// Apply a Claude-generated session title to the tab. Humanizes the
    /// kebab-case string Claude records (e.g. "fix-top-bar-height") into
    /// sentence-case ("Fix top bar height"). Skipped entirely once the
    /// user has manually renamed the tab, so late-arriving auto-titles
    /// can't clobber a user edit. The guard is keyed on `tabId`, so
    /// manually renaming one tab never affects another tab's flow.
    func applyAutoTitle(tabId: String, rawTitle: String) {
        guard let existing = tab(for: tabId), !existing.titleManuallySet else {
            return
        }
        let humanized = Self.humanizeSessionTitle(rawTitle)
        guard !humanized.isEmpty else { return }
        var changed = false
        mutateTab(id: tabId) { tab in
            if tab.title != humanized {
                tab.title = humanized
                changed = true
            }
            tab.titleAutoGenerated = true
        }
        if changed { scheduleSessionSave() }
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
              let tab = tab(for: tabId),
              let paneId = tab.activePaneId,
              let session = ptySessions[tabId],
              let view = session.panes[paneId]
        else { return }
        view.wantsFocusOnAttach = true
        DispatchQueue.main.async {
            view.window?.makeFirstResponder(view)
        }
    }

    /// User-initiated rename from the sidebar inline editor. Trims
    /// whitespace, ignores empty input, and marks the tab so subsequent
    /// `applyAutoTitle` calls skip it. Persists via `scheduleSessionSave`
    /// so the rename survives an app relaunch.
    func renameTab(id tabId: String, to newTitle: String) {
        let trimmed = newTitle.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        var changed = false
        mutateTab(id: tabId) { tab in
            if tab.title != trimmed || !tab.titleManuallySet {
                tab.title = trimmed
                tab.titleManuallySet = true
                changed = true
            }
        }
        if changed { scheduleSessionSave() }
    }

    private static func humanizeSessionTitle(_ raw: String) -> String {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return "" }
        let pieces = trimmed
            .split(whereSeparator: { $0 == "-" || $0 == "_" })
            .map(String.init)
        guard !pieces.isEmpty else { return "" }
        var joined = pieces.joined(separator: " ")
        if let first = joined.first, first.isLowercase {
            joined = first.uppercased() + joined.dropFirst()
        }
        if joined.count > 40 {
            let idx = joined.index(joined.startIndex, offsetBy: 40)
            joined = String(joined[..<idx]).trimmingCharacters(in: .whitespaces)
        }
        return joined
    }

    /// Append a new terminal-only tab to the pinned Terminals group,
    /// focus it, and spawn its pty. Used by the sidebar's group-level
    /// `+` button. First tab added to an empty group is titled "Main";
    /// subsequent tabs are auto-numbered "Main 2", "Main 3", etc.
    /// Cwd inherits the Terminals project's path.
    @discardableResult
    func createTerminalTab() -> String? {
        guard let pi = projects.firstIndex(where: { $0.id == Self.terminalsProjectId }) else {
            return nil
        }
        let project = projects[pi]
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
        projects[pi].tabs.append(tab)
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
        guard projectId != Self.terminalsProjectId,
              let pi = projects.firstIndex(where: { $0.id == projectId })
        else { return nil }
        let project = projects[pi]
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
        projects[pi].tabs.append(tab)
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
    /// Used by the socket handler to treat `claude` invocations from
    /// the Terminals group as "always open a new tab elsewhere".
    func isTerminalsProjectTab(_ tabId: String) -> Bool {
        guard let terminals = projects.first(where: { $0.id == Self.terminalsProjectId }) else {
            return false
        }
        return terminals.tabs.contains { $0.id == tabId }
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

        guard let tab = self.tab(for: tabId) else { return nil }
        let newId = "\(tabId)-p\(Int(Date().timeIntervalSince1970 * 1000))"
        let termCount = tab.panes.filter { $0.kind == .terminal }.count
        let resolvedTitle = title ?? "Terminal \(termCount + 1)"

        mutateTab(id: tabId) { tab in
            tab.panes.append(
                Pane(id: newId, title: resolvedTitle, kind: .terminal)
            )
            tab.activePaneId = newId
        }

        let tabCwd = cwd ?? tab.cwd
        let session: TabPtySession
        if let existing = ptySessions[tabId] {
            session = existing
        } else {
            session = makeSession(for: tabId, cwd: tabCwd)
        }
        _ = session.addTerminalPane(id: newId, cwd: tabCwd, command: command)
        return newId
    }

    /// Request to close a pane. If the pane is busy — a thinking or
    /// waiting Claude, or a shell with a foreground child — stage a
    /// confirmation prompt; the UI binds an alert to
    /// `pendingCloseRequest` and calls `confirmPendingClose` /
    /// `cancelPendingClose`. Idle panes are killed immediately.
    func requestClosePane(tabId: String, paneId: String) {
        guard let tab = tab(for: tabId),
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
        guard let tab = tab(for: tabId) else { return }

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
        guard projectId != Self.terminalsProjectId,
              let project = projects.first(where: { $0.id == projectId })
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
        guard let tab = tab(for: tabId) else { return }
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
            mutateTab(id: tabId) { tab in
                tab.panes.removeAll()
                tab.activePaneId = nil
            }
            if let (pi, ti) = projectTabIndex(for: tabId) {
                finalizeDissolvedTab(projectIndex: pi, tabIndex: ti, tabId: tabId)
            }
        } else {
            // At least one spawned pane will fire `paneExited` later;
            // clear the unspawned rows now so that exit sees an empty
            // panes list and dissolves through the normal path.
            let toDrop = Set(unspawnedIds)
            mutateTab(id: tabId) { tab in
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
        guard projectId != Self.terminalsProjectId,
              let idx = projects.firstIndex(where: { $0.id == projectId })
        else { return }

        let tabIds = projects[idx].tabs.map(\.id)
        if tabIds.isEmpty {
            projects.remove(at: idx)
            if let active = activeTabId, tab(for: active) == nil {
                activeTabId = firstAvailableTabId()
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

    /// Move `tabId` to a new slot within the same project, relative to
    /// `targetTabId`: either just before it (`placeAfter == false`) or
    /// just after it. No-op when the two tabs aren't in the same
    /// project, when either id is unknown, or when the move wouldn't
    /// change order. Tabs inside the pinned Terminals project reorder
    /// like any other project's tabs.
    func moveTab(_ tabId: String, relativeTo targetTabId: String, placeAfter: Bool) {
        guard tabId != targetTabId else { return }
        guard let (srcProject, srcIndex) = projectTabIndex(for: tabId),
              let (dstProject, dstIndex) = projectTabIndex(for: targetTabId),
              srcProject == dstProject
        else { return }
        // `placeAfter` picks the slot just past the target; then account
        // for the fact that removing the source first shifts everything
        // after it down by one.
        var insertIndex = placeAfter ? dstIndex + 1 : dstIndex
        if srcIndex < insertIndex { insertIndex -= 1 }
        guard insertIndex != srcIndex else { return }
        let tab = projects[srcProject].tabs.remove(at: srcIndex)
        projects[srcProject].tabs.insert(tab, at: insertIndex)
        scheduleSessionSave()
    }

    /// Mirrors `moveTab` without mutating — returns true iff the drop
    /// would actually reorder. The sidebar drop indicator uses this to
    /// suppress the insertion line for no-op drops (e.g. hovering the
    /// top half of a tab that already sits just after the source).
    func wouldMoveTab(_ tabId: String, relativeTo targetTabId: String, placeAfter: Bool) -> Bool {
        guard tabId != targetTabId,
              let (srcProject, srcIndex) = projectTabIndex(for: tabId),
              let (dstProject, dstIndex) = projectTabIndex(for: targetTabId),
              srcProject == dstProject
        else { return false }
        var insertIndex = placeAfter ? dstIndex + 1 : dstIndex
        if srcIndex < insertIndex { insertIndex -= 1 }
        return insertIndex != srcIndex
    }

    // MARK: - Keyboard navigation

    /// Flat list of sidebar tab ids in displayed order. The pinned
    /// Terminals project is always first, so its tabs lead; project
    /// tabs follow in project/then-tab order. Used by the keyboard
    /// shortcut handlers to walk a deterministic visible set.
    var navigableSidebarTabIds: [String] {
        projects.flatMap { $0.tabs.map(\.id) }
    }

    /// Move focus to the next sidebar tab, wrapping. No-op when there's
    /// only one navigable tab (Terminals alone).
    func selectNextSidebarTab() { stepSidebarTab(by: +1) }

    /// Move focus to the previous sidebar tab, wrapping.
    func selectPrevSidebarTab() { stepSidebarTab(by: -1) }

    private func stepSidebarTab(by offset: Int) {
        let ids = navigableSidebarTabIds
        guard ids.count > 1 else { return }
        let currentIdx = activeTabId.flatMap { ids.firstIndex(of: $0) } ?? 0
        let nextIdx = ((currentIdx + offset) % ids.count + ids.count) % ids.count
        activeTabId = ids[nextIdx]
    }

    /// Move focus to the next pane within the active tab, wrapping. No-op
    /// when the active tab has fewer than two panes.
    func selectNextPane() { stepActivePane(by: +1) }

    /// Move focus to the previous pane within the active tab, wrapping.
    func selectPrevPane() { stepActivePane(by: -1) }

    private func stepActivePane(by offset: Int) {
        guard let tabId = activeTabId, let tab = tab(for: tabId) else { return }
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
        let resolvedCwd = Self.expandTilde(cwd)

        // Work out which panes to spawn. Callers can pass ids explicitly
        // (e.g. createTabFromMainTerminal) or we infer them from the
        // model.
        var claudePaneId = initialClaudePaneId
        var terminalPaneId = initialTerminalPaneId
        if claudePaneId == nil && terminalPaneId == nil {
            if let tab = self.tab(for: tabId) {
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
        session.applyGpuRendering(enabled: currentGpuRendering)
        session.applySmoothScrolling(enabled: currentSmoothScrolling)
        ptySessions[tabId] = session
        return session
    }

    // MARK: - Claude binary resolution

    /// Resolve `binary` via a login+interactive zsh so `.zprofile` /
    /// `.zshrc` PATH customizations (Homebrew, nvm, `~/.local/bin`) are
    /// applied. Nice launched from Finder/Spotlight inherits only the
    /// macOS default PATH, so `/usr/bin/which` misses anything the user
    /// put on PATH from their shell rc — the common case for `claude`.
    private nonisolated static func runWhich(binary: String) -> String? {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/bin/zsh")
        proc.arguments = ["-ilc", "command -v -- \(binary)"]
        let pipe = Pipe()
        proc.standardOutput = pipe
        proc.standardError = Pipe()
        do {
            try proc.run()
            proc.waitUntilExit()
            guard proc.terminationStatus == 0 else { return nil }
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            guard let raw = String(data: data, encoding: .utf8) else { return nil }
            // `command -v` on a shell function or alias prints the name
            // or a definition rather than an absolute path — only accept
            // an absolute path.
            let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            guard trimmed.hasPrefix("/") else { return nil }
            return trimmed
        } catch {
            return nil
        }
    }

    // MARK: - Lookup

    /// Look up a tab by id across every project, including the pinned
    /// Terminals group.
    func tab(for id: String) -> Tab? {
        for project in projects {
            if let hit = project.tabs.first(where: { $0.id == id }) {
                return hit
            }
        }
        return nil
    }

    /// Mutate the tab identified by `id` in place. Calls `transform`
    /// with the right backing storage (Terminals tab, or an element of
    /// `projects`). Returns true if the tab was found.
    @discardableResult
    private func mutateTab(id: String, _ transform: (inout Tab) -> Void) -> Bool {
        guard let (pi, ti) = projectTabIndex(for: id) else { return false }
        transform(&projects[pi].tabs[ti])
        return true
    }

    /// Project + tab index for the tab with id `id`, for in-place
    /// mutation in the `projects` array.
    private func projectTabIndex(for id: String) -> (Int, Int)? {
        for (pi, project) in projects.enumerated() {
            if let ti = project.tabs.firstIndex(where: { $0.id == id }) {
                return (pi, ti)
            }
        }
        return nil
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
        guard let tabId = tabIdOwning(paneId: paneId) else { return }
        updateClaudeSessionId(tabId: tabId, sessionId: sessionId)
    }

    /// Reverse-index: walk every project's tabs for the one whose pane
    /// list includes `paneId`. Cheap — pane counts are small — and
    /// tolerant of projects added/removed mid-loop because the socket
    /// handler runs on the main actor.
    private func tabIdOwning(paneId: String) -> String? {
        for project in projects {
            for tab in project.tabs {
                if tab.panes.contains(where: { $0.id == paneId }) {
                    return tab.id
                }
            }
        }
        return nil
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
        mutateTab(id: tabId) { tab in
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
        for project in projects {
            var tabs: [PersistedTab] = []
            for tab in project.tabs {
                let panes = tab.panes.map {
                    PersistedPane(
                        id: $0.id, title: $0.title, kind: $0.kind, cwd: $0.cwd
                    )
                }
                tabs.append(PersistedTab(
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
            if tabs.isEmpty && project.id != Self.terminalsProjectId {
                continue
            }
            persistedProjects.append(PersistedProject(
                id: project.id,
                name: project.name,
                path: project.path,
                tabs: tabs
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
            ensureTerminalsProjectSeeded()
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
        let previousMainTabId = projects.first(where: { $0.id == Self.terminalsProjectId })?.tabs.first?.id
        if let mainTabId = previousMainTabId {
            ptySessions[mainTabId]?.terminateAll()
            ptySessions.removeValue(forKey: mainTabId)
        }
        projects.removeAll()

        // Build the Tab/Pane model now so the sidebar shows the tabs
        // immediately; defer the Claude pty spawn so views can lay out
        // first. Trust the saved project grouping — don't re-bucket by
        // cwd.
        var pendingClaudeSpawns: [(tabId: String, cwd: String, claudePaneId: String?, claudeSessionId: String)] = []
        for persistedProject in snapshot.projects {
            let projectIdx = ensureProject(
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
        repairProjectStructure()
        scheduleSessionSave()

        if let active = snapshot.activeTabId, tab(for: active) != nil {
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

    /// Guarantee a pinned Terminals project sits at `projects[0]`. If
    /// it's absent (first launch of this build, or a restore adopted
    /// a snapshot predating the Terminals group), synthesize one with
    /// a "Main" tab holding a fresh terminal pane. If it's present
    /// but not at index 0, move it. Called at the tail of
    /// `restoreSavedWindow`.
    private func ensureTerminalsProjectSeeded() {
        if let idx = projects.firstIndex(where: { $0.id == Self.terminalsProjectId }) {
            if idx != 0 {
                let project = projects.remove(at: idx)
                projects.insert(project, at: 0)
            }
            if activeTabId == nil, let firstId = projects[0].tabs.first?.id {
                activeTabId = firstId
            }
            return
        }

        let cwd = NSHomeDirectory()
        let mainTabId = Self.mainTerminalTabId
        let paneId = "\(mainTabId)-p\(Int(Date().timeIntervalSince1970 * 1000))"
        let pane = Pane(id: paneId, title: "zsh", kind: .terminal)
        let mainTab = Tab(
            id: mainTabId,
            title: "Main",
            cwd: cwd,
            branch: nil,
            panes: [pane],
            activePaneId: paneId
        )
        let project = Project(
            id: Self.terminalsProjectId,
            name: "Terminals",
            path: cwd,
            tabs: [mainTab]
        )
        projects.insert(project, at: 0)
        if activeTabId == nil {
            activeTabId = mainTabId
        }
        _ = makeSession(for: mainTabId, cwd: cwd)
    }

    /// Look up `projects` by saved id; append a fresh `Project` with
    /// the saved name/path if absent. Returns the index of the
    /// matched-or-appended project. Used by restore to bypass the
    /// cwd-based bucketing that would otherwise split a multi-worktree
    /// project like "NICE" into one project per worktree on relaunch.
    private func ensureProject(id: String, name: String, path: String) -> Int {
        if let existing = projects.firstIndex(where: { $0.id == id }) {
            return existing
        }
        projects.append(Project(id: id, name: name, path: path, tabs: []))
        return projects.count - 1
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

        projects[projectIndex].tabs.append(tab)

        // Resolve after appending so `resolvedSpawnCwd` can see the tab's
        // new project context. Falls back to the project path if the
        // persisted cwd (e.g. a worktree directory) has been deleted
        // since the last launch.
        let spawnCwd = resolvedSpawnCwd(for: tab)

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
            let paneCwd = resolvedSpawnCwd(for: tab, pane: pane)
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

    // MARK: - Helpers

    private static func expandTilde(_ path: String) -> String {
        if path == "~" { return NSHomeDirectory() }
        if path.hasPrefix("~/") {
            return NSHomeDirectory() + path.dropFirst(1)
        }
        return path
    }

    /// Strip any `<X>/.claude/worktrees/<name>/...` suffix and return
    /// `<X>`. A Nice-specific convention: sessions running inside a
    /// Nice-managed worktree should resolve to the parent repo, not
    /// to the worktree's own internal `.git` marker. Mirrors the
    /// pre-worktree cwd that `createTabFromMainTerminal` passes when
    /// `claude -w` is invoked.
    static func stripNiceWorktreeSuffix(_ path: String) -> String {
        guard let range = path.range(of: "/.claude/worktrees/") else {
            return path
        }
        return String(path[..<range.lowerBound])
    }

    /// Walk up from `cwd` (after stripping any Nice worktree suffix),
    /// returning the absolute path of the nearest ancestor directory
    /// that contains a `.git` entry — matches both `.git/` (normal
    /// repo) and `.git` files (submodules and git worktrees). Returns
    /// nil if no `.git` is found before reaching the filesystem root.
    static func findGitRoot(forCwd cwd: String) -> String? {
        var current = stripNiceWorktreeSuffix(cwd)
        while !current.isEmpty && current != "/" {
            let dotGit = (current as NSString).appendingPathComponent(".git")
            if FileManager.default.fileExists(atPath: dotGit) {
                return current
            }
            let parent = (current as NSString).deletingLastPathComponent
            if parent == current { break }
            current = parent
        }
        return nil
    }

    /// Extract the value of `-w` / `--worktree` from Claude args. Only
    /// the space-delimited form is recognized (matches Claude Code's
    /// CLI). Returns nil if the flag is absent, trailing with no
    /// value, or the value is empty.
    static func extractWorktreeName(from args: [String]) -> String? {
        var i = 0
        while i < args.count {
            let a = args[i]
            if (a == "-w" || a == "--worktree") && i + 1 < args.count {
                let v = args[i + 1]
                return v.isEmpty ? nil : v
            }
            i += 1
        }
        return nil
    }

    /// Resolve the cwd to use when spawning a pane for `tab`. Prefers
    /// `tab.cwd` (which may be a worktree path Claude Code created via
    /// `-w`), falling back to the containing project's path if the
    /// tab's cwd no longer exists on disk — covers the case where a
    /// user deleted a worktree between app launches.
    func resolvedSpawnCwd(for tab: Tab) -> String {
        let expanded = Self.expandTilde(tab.cwd)
        if FileManager.default.fileExists(atPath: expanded) { return expanded }
        if let project = projects.first(where: { p in
            p.tabs.contains(where: { $0.id == tab.id })
        }) {
            return Self.expandTilde(project.path)
        }
        return expanded
    }

    /// Per-pane variant: prefers `pane.cwd` (last-observed via OSC 7)
    /// when set and still exists on disk. Falls back to the tab-level
    /// resolution when nil or pointing at a deleted directory — covers
    /// the case where the pane's last cwd was a worktree or temp dir
    /// the user removed between app launches.
    func resolvedSpawnCwd(for tab: Tab, pane: Pane) -> String {
        if let raw = pane.cwd {
            let expanded = Self.expandTilde(raw)
            if FileManager.default.fileExists(atPath: expanded) {
                return expanded
            }
        }
        return resolvedSpawnCwd(for: tab)
    }

    /// Bucket `tab` into the project that anchors `cwd`'s git repo,
    /// creating a new project at the git root when none matches. Falls
    /// back to longest-prefix matching when `cwd` is not inside any
    /// git repo, preserving the legacy behavior for ad-hoc non-repo
    /// directories.
    private func addTabToProjects(_ tab: Tab, cwd: String) {
        let normalizedCwd = Self.expandTilde(cwd)
        if let gitRoot = Self.findGitRoot(forCwd: normalizedCwd) {
            appendOrInsert(tab, intoProjectAt: gitRoot)
            return
        }
        // No git root: legacy longest-prefix behavior. Excludes the
        // pinned Terminals group, whose path is seeded from the Main
        // Terminal's cwd (typically $HOME) and would otherwise prefix-
        // match almost any cwd and swallow new Claude tabs that belong
        // in a fresh project group.
        if let idx = projects.enumerated()
            .filter({ $0.element.id != Self.terminalsProjectId })
            .filter({ normalizedCwd.hasPrefix(Self.expandTilde($0.element.path)) })
            .max(by: { $0.element.path.count < $1.element.path.count })?
            .offset
        {
            projects[idx].tabs.append(tab)
        } else {
            appendNewProject(at: normalizedCwd, with: tab)
        }
    }

    /// Append `tab` to the existing non-Terminals project rooted at
    /// `path`, or create a new project there if none matches.
    private func appendOrInsert(_ tab: Tab, intoProjectAt path: String) {
        if let idx = firstIndex(ofNonTerminalsProjectAt: path) {
            projects[idx].tabs.append(tab)
        } else {
            appendNewProject(at: path, with: tab)
        }
    }

    /// Index of the first non-Terminals project whose `path` (after
    /// `expandTilde`) equals `path`. Single source of truth for
    /// project lookup by anchor — used by `addTabToProjects`,
    /// `repairProjectStructure`, and any future code that needs to
    /// find a project by its filesystem anchor.
    private func firstIndex(ofNonTerminalsProjectAt path: String) -> Int? {
        projects.firstIndex {
            $0.id != Self.terminalsProjectId
                && Self.expandTilde($0.path) == path
        }
    }

    /// Append a fresh project rooted at `path`, deriving the display
    /// name from the path's last component. Centralised so creation
    /// from `addTabToProjects` and `repairProjectStructure` stays in
    /// sync (id format, name casing). Uses a UUID prefix instead of a
    /// timestamp so back-to-back appends in the same millisecond
    /// (e.g. inside the repair tab-move loop) can't collide on `id`.
    private func appendNewProject(at path: String, with tab: Tab) {
        let dirName = (path as NSString).lastPathComponent.uppercased()
        let projectId = "p-\(dirName.lowercased())-\(UUID().uuidString.prefix(8).lowercased())"
        projects.append(Project(
            id: projectId, name: dirName, path: path, tabs: [tab]
        ))
    }

    /// Self-heal the persisted project structure. Idempotent. Skips
    /// the pinned Terminals project entirely.
    ///
    /// Four passes:
    /// 1. Promote each non-Terminals project's `path` to its enclosing
    ///    git root if `path` is a strict descendant of one.
    /// 2. Move tabs whose own git-root anchor (computed from
    ///    `tab.cwd`) differs from the containing project's path. Tabs
    ///    whose `cwd` no longer exists on disk stay put.
    /// 3. Merge non-Terminals projects that ended up at the same
    ///    expanded path (lowest-index wins; later dupes are emptied).
    /// 4. Drop empty non-Terminals projects.
    ///
    /// Internal (not private) so tests can drive it directly with
    /// hand-seeded `projects` state.
    func repairProjectStructure() {
        // 1. Promote project paths to their git roots.
        for i in projects.indices where projects[i].id != Self.terminalsProjectId {
            let path = Self.expandTilde(projects[i].path)
            guard FileManager.default.fileExists(atPath: path),
                  let root = Self.findGitRoot(forCwd: path),
                  root != path
            else { continue }
            projects[i].path = root
            projects[i].name = (root as NSString).lastPathComponent.uppercased()
        }

        // 2. Collect mis-bucketed tabs, then re-insert them at the
        //    right anchor. Two phases so the index-stable mutation
        //    (rewriting each project's tabs in place) finishes before
        //    we start appending new projects for unmatched anchors.
        struct Move { let tab: Tab; let targetGitRoot: String }
        var moves: [Move] = []
        for i in projects.indices where projects[i].id != Self.terminalsProjectId {
            let projectAnchor = Self.expandTilde(projects[i].path)
            var keep: [Tab] = []
            keep.reserveCapacity(projects[i].tabs.count)
            for tab in projects[i].tabs {
                let tabCwd = Self.expandTilde(tab.cwd)
                guard FileManager.default.fileExists(atPath: tabCwd) else {
                    keep.append(tab)
                    continue
                }
                let anchor = Self.findGitRoot(forCwd: tabCwd) ?? tabCwd
                if anchor == projectAnchor {
                    keep.append(tab)
                } else {
                    moves.append(Move(tab: tab, targetGitRoot: anchor))
                }
            }
            projects[i].tabs = keep
        }
        for move in moves {
            appendOrInsert(move.tab, intoProjectAt: move.targetGitRoot)
        }

        // 3. Merge duplicates targeting the same expanded path.
        var canonicalIndexByPath: [String: Int] = [:]
        var dupes: [Int] = []
        for i in projects.indices where projects[i].id != Self.terminalsProjectId {
            let key = Self.expandTilde(projects[i].path)
            if let canonical = canonicalIndexByPath[key] {
                projects[canonical].tabs.append(contentsOf: projects[i].tabs)
                dupes.append(i)
            } else {
                canonicalIndexByPath[key] = i
            }
        }
        for idx in dupes.sorted(by: >) {
            projects.remove(at: idx)
        }

        // 4. Drop empty non-Terminals projects.
        projects.removeAll {
            $0.id != Self.terminalsProjectId && $0.tabs.isEmpty
        }
    }
}
