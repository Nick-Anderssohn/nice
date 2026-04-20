//
//  AppState.swift
//  Nice
//
//  Central app state. Owns the long-lived pty sessions (cached in
//  `ptySessions` keyed by tab id) and fans process-exit / title-change
//  events back into the data model so the sidebar and toolbar can react.
//
//  The "Terminals" row at the top of the sidebar is a built-in `Tab`
//  (`isBuiltIn = true`) — it participates in the same tab/pane model as
//  every user session, so it can host the same toolbar pill bar. It is
//  created once at launch, cannot be dissolved, and its pane lifecycle
//  still drives the "Quit NICE?" alert.
//

import AppKit
import Foundation
import SwiftUI

@MainActor
final class AppState: ObservableObject {
    /// Reserved id for the built-in Terminals tab.
    static let terminalsTabId = "terminals"

    @Published var projects: [Project]
    /// Built-in "Terminals" session. Always present, never removable.
    @Published var terminalsTab: Tab
    /// Currently-selected tab. Defaults to the Terminals tab on launch.
    @Published var activeTabId: String? {
        didSet {
            // Viewing a tab dismisses the attention pulse on its active
            // pane's waiting state — centralised here so every call site
            // that flips `activeTabId` gets the same acknowledgment.
            if let id = activeTabId, id != oldValue {
                acknowledgeWaitingOnActivePane(tabId: id)
            }
        }
    }
    @Published var sidebarQuery: String = ""
    /// Whether the sidebar is collapsed. Seeded from the per-window
    /// `@SceneStorage` value by the owning view so each window keeps
    /// its own state; the view writes back on changes.
    @Published var sidebarCollapsed: Bool = false

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

    /// Called by the keyboard monitor when all relevant shortcut
    /// modifiers have been released. The view's separate mouse-hover
    /// pin keeps the overlay rendered if the cursor is over it.
    func endSidebarPeek() {
        sidebarPeeking = false
    }

    // MARK: - Process plumbing

    @Published private(set) var ptySessions: [String: TabPtySession] = [:]

    /// Surfaces a "Quit NICE?" alert when the Terminals tab's last pane
    /// exits while user sessions still exist. `AppShellView` binds its
    /// `.alert` to this flag and calls `cancelQuitPrompt()` /
    /// `NSApp.terminate(nil)` from the two buttons.
    @Published var showQuitPrompt: Bool = false

    /// Cached cwd for the Terminals tab so `cancelQuitPrompt` / directory
    /// changes can respawn at the same place.
    private var storedMainCwd: String

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
    private var currentTerminalFontSize: CGFloat = FontSettings.defaultSize

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

    /// Convenience init for `#Preview` blocks and unit tests. Each
    /// AppState is otherwise expected to be constructed by
    /// `AppShellView` passing its window's `NiceServices` and the
    /// per-window `@SceneStorage` values.
    convenience init() {
        self.init(services: nil, initialSidebarCollapsed: false, initialMainCwd: nil)
    }

    init(
        services: NiceServices?,
        initialSidebarCollapsed: Bool,
        initialMainCwd: String?
    ) {
        self.projects = Project.seed
        self.sidebarCollapsed = initialSidebarCollapsed

        let resolvedMainCwd = initialMainCwd ?? NSHomeDirectory()
        self.storedMainCwd = resolvedMainCwd

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
            ?? FontSettings.defaultSize

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

        // Seed the built-in Terminals tab with one terminal pane.
        let initialPaneId = "\(Self.terminalsTabId)-p\(Int(Date().timeIntervalSince1970 * 1000))"
        let initialPane = Pane(id: initialPaneId, title: "zsh", kind: .terminal)
        self.terminalsTab = Tab(
            id: Self.terminalsTabId,
            title: "Terminals",
            cwd: resolvedMainCwd,
            branch: nil,
            isBuiltIn: true,
            panes: [initialPane],
            activePaneId: initialPaneId
        )
        self.activeTabId = Self.terminalsTabId

        // All stored properties set — now bring up the session for the
        // Terminals tab and start the control socket.
        _ = self.makeSession(for: Self.terminalsTabId, cwd: resolvedMainCwd)

        do {
            try socket.start { [weak self] message in
                Task { @MainActor [weak self] in
                    guard let self else { return }
                    switch message {
                    case let .newtab(cwd, args):
                        self.createTabFromMainTerminal(cwd: cwd, args: args)
                    }
                }
            }
        } catch {
            NSLog("AppState: control socket failed to bind: \(error)")
        }
    }

    /// Snapshot of this window's live panes grouped by kind. Used by
    /// the quit / window-close confirmation alerts to word the prompt
    /// ("N Claude sessions and M terminals") without exposing the model
    /// to callers outside AppState.
    var livePaneCounts: (claude: Int, terminal: Int) {
        var claude = 0
        var terminal = 0
        let allTabs = [terminalsTab] + projects.flatMap { $0.tabs }
        for tab in allTabs {
            for pane in tab.panes where pane.isAlive {
                switch pane.kind {
                case .claude: claude += 1
                case .terminal: terminal += 1
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
        for session in ptySessions.values {
            session.terminateAll()
        }
        ptySessions.removeAll()
        controlSocket?.stop()
        controlSocket = nil
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
        let tab = Tab(
            id: newId,
            title: title,
            cwd: cwd,
            branch: nil,
            isBuiltIn: false,
            panes: [
                Pane(id: claudePaneId, title: "Claude", kind: .claude),
                Pane(id: terminalPaneId, title: "Terminal 1", kind: .terminal),
            ],
            activePaneId: claudePaneId
        )

        let normalizedCwd = cwd.replacingOccurrences(of: "~", with: NSHomeDirectory())
        if let idx = projects.enumerated()
            .filter({ normalizedCwd.hasPrefix($0.element.path.replacingOccurrences(of: "~", with: NSHomeDirectory())) })
            .max(by: { $0.element.path.count < $1.element.path.count })?
            .offset
        {
            projects[idx].tabs.insert(tab, at: 0)
        } else {
            let dirName = (normalizedCwd as NSString).lastPathComponent.uppercased()
            let projectId = "p-\(dirName.lowercased())-\(Int(Date().timeIntervalSince1970))"
            let newProject = Project(
                id: projectId, name: dirName, path: normalizedCwd, tabs: [tab]
            )
            projects.append(newProject)
        }
        activeTabId = newId
        _ = makeSession(
            for: newId, cwd: cwd,
            extraClaudeArgs: args,
            initialClaudePaneId: claudePaneId,
            initialTerminalPaneId: terminalPaneId
        )
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

    // MARK: - Lifecycle handlers

    /// A pane exited. Remove it from its tab, pick a neighbor to focus,
    /// and dissolve the tab if nothing remains (non-builtin tabs only).
    /// For the built-in Terminals tab: surface the quit prompt when all
    /// panes close while user sessions still exist; terminate the app
    /// otherwise.
    func paneExited(tabId: String, paneId: String, exitCode: Int32?) {
        let isBuiltIn = (tabId == Self.terminalsTabId)

        var removedActiveFromBuiltIn = false
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
            if tab.panes.isEmpty && isBuiltIn {
                removedActiveFromBuiltIn = true
            }
        }

        ptySessions[tabId]?.removePane(id: paneId)

        if isBuiltIn {
            // The Terminals tab lives forever. If its last pane just
            // exited, show the quit prompt (unless nothing else is
            // open, in which case terminate outright).
            if removedActiveFromBuiltIn {
                if projects.allSatisfy({ $0.tabs.isEmpty }) {
                    NSApp.terminate(nil)
                } else {
                    showQuitPrompt = true
                }
            }
            return
        }

        // Non-builtin: drop the tab entirely once it has no panes left.
        if let (pi, ti) = projectTabIndex(for: tabId),
           projects[pi].tabs[ti].panes.isEmpty {
            projects[pi].tabs.remove(at: ti)
            ptySessions.removeValue(forKey: tabId)
            if activeTabId == tabId {
                activeTabId = Self.terminalsTabId
            }
        }
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

    /// Apply a Claude-generated session title to the tab. Humanizes the
    /// kebab-case string Claude records (e.g. "fix-top-bar-height") into
    /// sentence-case ("Fix top bar height") and sets the auto-generated
    /// flag so a future manual rename can opt out of being clobbered.
    func applyAutoTitle(tabId: String, rawTitle: String) {
        let humanized = Self.humanizeSessionTitle(rawTitle)
        guard !humanized.isEmpty else { return }
        mutateTab(id: tabId) { tab in
            tab.title = humanized
            tab.titleAutoGenerated = true
        }
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

    /// Cancel the post-exit quit prompt: hide the alert and bring a
    /// fresh terminal pane back up in the Terminals tab.
    func cancelQuitPrompt() {
        showQuitPrompt = false
        let newId = "\(Self.terminalsTabId)-p\(Int(Date().timeIntervalSince1970 * 1000))"
        mutateTab(id: Self.terminalsTabId) { tab in
            tab.panes.append(Pane(id: newId, title: "zsh", kind: .terminal))
            tab.activePaneId = newId
        }
        if let session = ptySessions[Self.terminalsTabId] {
            _ = session.addTerminalPane(id: newId, cwd: storedMainCwd)
        } else {
            _ = makeSession(for: Self.terminalsTabId, cwd: storedMainCwd)
        }
    }

    // MARK: - Pane management

    /// Append a new terminal pane to `tabId`, spawn its pty, and focus
    /// it. Returns the new pane id, or nil if the tab doesn't exist.
    @discardableResult
    func addPane(
        tabId: String,
        kind: PaneKind = .terminal,
        cwd: String? = nil,
        title: String? = nil
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
        _ = session.addTerminalPane(id: newId, cwd: tabCwd)
        return newId
    }

    /// Ask a pane to quit by writing `"exit\n"` into its pty. The
    /// process-exit delegate (`paneExited`) handles cleanup once the
    /// shell actually dies, so this is a "soft" close.
    func requestClosePane(tabId: String, paneId: String) {
        guard let session = ptySessions[tabId] else { return }
        session.sendToPane("exit", paneId: paneId)
    }

    // MARK: - Keyboard navigation

    /// Flat list of sidebar tab ids in displayed order, respecting
    /// `filteredProjects`. Terminals tab is always first; project tabs
    /// follow in project/then-tab order. Used by the keyboard shortcut
    /// handlers to walk a deterministic visible set — when the user has
    /// typed into the search field, navigation cycles only through the
    /// matching tabs.
    var navigableSidebarTabIds: [String] {
        var ids: [String] = [Self.terminalsTabId]
        for project in filteredProjects {
            ids.append(contentsOf: project.tabs.map(\.id))
        }
        return ids
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
        initialTerminalPaneId: String? = nil
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
            onPaneExit: { [weak self] paneId, code in
                self?.paneExited(tabId: tabId, paneId: paneId, exitCode: code)
            },
            onPaneTitleChange: { [weak self] paneId, title in
                self?.paneTitleChanged(tabId: tabId, paneId: paneId, title: title)
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

    /// Called from the sidebar when the user picks a new directory for
    /// the Terminals tab. Replaces the Terminals tab's first terminal
    /// pane with a fresh one rooted at `cwd`.
    func restartTerminalsFirstPane(cwd: String) {
        storedMainCwd = cwd
        mutateTab(id: Self.terminalsTabId) { tab in
            tab.cwd = cwd
        }
        guard let session = ptySessions[Self.terminalsTabId],
              let firstId = terminalsTab.panes.first?.id else { return }
        // Terminate the existing pane; its exit delegate will remove
        // the pane from the model and session. Then add a fresh one.
        session.panes[firstId]?.process.terminate()
        // Schedule the respawn slightly after — the delegate's exit
        // removes the old pane first. We queue on main so the model
        // update from `paneExited` lands before our insert.
        let newId = "\(Self.terminalsTabId)-p\(Int(Date().timeIntervalSince1970 * 1000))"
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) { [weak self] in
            guard let self else { return }
            self.mutateTab(id: Self.terminalsTabId) { tab in
                tab.panes.append(Pane(id: newId, title: "zsh", kind: .terminal))
                tab.activePaneId = newId
            }
            if let session = self.ptySessions[Self.terminalsTabId] {
                _ = session.addTerminalPane(id: newId, cwd: cwd)
            }
        }
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

    // MARK: - Filtering / lookup

    /// Case-insensitive title filter over user projects (the Terminals
    /// tab isn't part of any project and is rendered separately).
    var filteredProjects: [Project] {
        let q = sidebarQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !q.isEmpty else { return projects }
        let needle = q.lowercased()
        return projects.compactMap { project in
            let matches = project.tabs.filter {
                $0.title.lowercased().contains(needle)
            }
            guard !matches.isEmpty else { return nil }
            var copy = project
            copy.tabs = matches
            return copy
        }
    }

    /// Look up a tab by id, including the built-in Terminals tab.
    func tab(for id: String) -> Tab? {
        if id == Self.terminalsTabId { return terminalsTab }
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
        if id == Self.terminalsTabId {
            transform(&terminalsTab)
            return true
        }
        guard let (pi, ti) = projectTabIndex(for: id) else { return false }
        transform(&projects[pi].tabs[ti])
        return true
    }

    /// Project + tab index for the tab with id `id`, for in-place
    /// mutation in the `projects` array. Returns nil for the built-in
    /// Terminals tab.
    private func projectTabIndex(for id: String) -> (Int, Int)? {
        for (pi, project) in projects.enumerated() {
            if let ti = project.tabs.firstIndex(where: { $0.id == id }) {
                return (pi, ti)
            }
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
}
