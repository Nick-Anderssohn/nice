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
    @Published var sidebarCollapsed: Bool = UserDefaults.standard.bool(forKey: "sidebarCollapsed")

    func toggleSidebar() {
        sidebarCollapsed.toggle()
        UserDefaults.standard.set(sidebarCollapsed, forKey: "sidebarCollapsed")
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

    // MARK: - MCP server

    @Published private(set) var mcp = NiceMCPServer()

    /// Absolute path to the `claude` binary if we've resolved it; nil
    /// falls back to zsh inside claude panes.
    private var resolvedClaudePath: String?

    // MARK: - Control socket

    private var controlSocket: NiceControlSocket?
    private var zdotdirPath: String?
    private var controlSocketExtraEnv: [String: String] = [:]

    init() {
        self.projects = Project.seed

        let storedMainCwd = UserDefaults.standard.string(forKey: "mainTerminalCwd")
            ?? NSHomeDirectory()
        self.storedMainCwd = storedMainCwd

        // Allocate the control socket + write the ZDOTDIR inject
        // *before* spawning any ptys — the shells need NICE_SOCKET +
        // ZDOTDIR in their environment at startup or the `claude()`
        // shadow never loads.
        let socket = NiceControlSocket()
        self.controlSocket = socket

        var extraEnv: [String: String] = [:]
        extraEnv["NICE_SOCKET"] = socket.path
        do {
            let zdotdir = try MainTerminalShellInject.make(socketPath: socket.path)
            extraEnv["ZDOTDIR"] = zdotdir.path
            self.zdotdirPath = zdotdir.path
        } catch {
            NSLog("AppState: ZDOTDIR inject failed: \(error)")
            self.zdotdirPath = nil
        }
        self.controlSocketExtraEnv = extraEnv

        // Resolve `claude` synchronously on launch (cheap). If missing,
        // claude panes fall back to zsh.
        self.resolvedClaudePath = ProcessInfo.processInfo.environment["NICE_CLAUDE_OVERRIDE"]
            ?? Self.runWhich(binary: "claude")

        // Seed the built-in Terminals tab with one terminal pane.
        let initialPaneId = "\(Self.terminalsTabId)-p\(Int(Date().timeIntervalSince1970 * 1000))"
        let initialPane = Pane(id: initialPaneId, title: "zsh", kind: .terminal)
        self.terminalsTab = Tab(
            id: Self.terminalsTabId,
            title: "Terminals",
            status: .idle,
            cwd: storedMainCwd,
            branch: nil,
            isBuiltIn: true,
            panes: [initialPane],
            activePaneId: initialPaneId
        )
        self.activeTabId = Self.terminalsTabId

        // All stored properties set — now bring up the session for the
        // Terminals tab and start the control socket.
        _ = self.makeSession(for: Self.terminalsTabId, cwd: storedMainCwd)

        do {
            try socket.start { [weak self] message in
                Task { @MainActor [weak self] in
                    guard let self else { return }
                    switch message {
                    case let .newtab(cwd, args):
                        self.createTabFromMainTerminal(cwd: cwd, args: args)
                    case let .promoteTab(tabId, args):
                        self.promoteTabToClaude(tabId: tabId, args: args)
                    }
                }
            }
        } catch {
            NSLog("AppState: control socket failed to bind: \(error)")
        }

        NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification,
            object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                for session in self.ptySessions.values {
                    session.terminateAll()
                }
                self.controlSocket?.stop()
            }
        }
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
            status: .idle,
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

    // MARK: - Bootstrap

    func bootstrap() async {
        let defaults = UserDefaults.standard
        let shouldAutoStart = (defaults.object(forKey: "mcpAutoStart") as? Bool) ?? true
        guard shouldAutoStart else { return }
        await mcp.start(appState: self)
    }

    // MARK: - Theme

    func updateScheme(_ scheme: ColorScheme, palette: Palette) {
        currentScheme = scheme
        currentPalette = palette
        for session in ptySessions.values {
            session.applyTheme(scheme, palette: palette)
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
                if isActivePane && tab.status != newStatus {
                    tab.status = newStatus
                }
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

    /// Promote a terminal-only tab back to Claude-tab state. Called
    /// from the control socket's `promoteTab` handler when a terminal
    /// pane's shadowed `claude()` fires (the pty is already about to
    /// exec claude inline — this just flips the pane's kind and adds a
    /// fresh terminal pane alongside).
    func promoteTabToClaude(tabId: String, args: [String]) {
        // The Terminals built-in never hosts a claude pane — the zsh
        // shadow there triggers `newtab`, not `promoteTab`. Belt &
        // braces guard.
        guard tabId != Self.terminalsTabId else {
            NSLog("AppState: promoteTab on Terminals tab — ignoring")
            return
        }

        guard let tab = tab(for: tabId) else {
            NSLog("AppState: promoteTabToClaude for unknown tab \(tabId) — ignoring")
            return
        }
        guard let activeId = tab.activePaneId,
              let active = tab.panes.first(where: { $0.id == activeId })
        else {
            NSLog("AppState: promoteTabToClaude with no active pane on \(tabId)")
            return
        }
        if active.kind == .claude {
            // Already a claude pane — the shadow will still exec claude
            // inline, giving the user a second claude inside. Nothing
            // for the model to do.
            return
        }

        // Flip the active pane to claude kind and retheme.
        let promotedId = activeId
        mutateTab(id: tabId) { tab in
            if let i = tab.panes.firstIndex(where: { $0.id == promotedId }) {
                tab.panes[i].kind = .claude
                tab.panes[i].isAlive = true
                tab.panes[i].title = "Claude"
            }
        }
        ptySessions[tabId]?.promotePaneToClaude(id: promotedId)

        // Append a fresh terminal pane so the session still has a shell.
        let newPaneId = "\(tabId)-t\(Int(Date().timeIntervalSince1970 * 1000))"
        let tabCwd: String = {
            if let updated = self.tab(for: tabId) { return updated.cwd }
            return tab.cwd
        }()
        mutateTab(id: tabId) { tab in
            let title = "Terminal \(tab.panes.filter { $0.kind == .terminal }.count + 1)"
            tab.panes.append(Pane(id: newPaneId, title: title, kind: .terminal))
        }
        _ = ptySessions[tabId]?.addTerminalPane(id: newPaneId, cwd: tabCwd)
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
        // created by `createTabFromMainTerminal` or `promoteTabToClaude`.
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

    // MARK: - MCP tool handlers

    func mcpSwitchTab(tabId: String?, titleQuery: String?) -> String? {
        if let tabId, tab(for: tabId) != nil {
            activeTabId = tabId
            return tabId
        }
        if let q = titleQuery?.lowercased(), !q.isEmpty {
            if terminalsTab.title.lowercased().contains(q) {
                activeTabId = terminalsTab.id
                return terminalsTab.id
            }
            for project in projects {
                if let hit = project.tabs.first(where: {
                    $0.title.lowercased().contains(q)
                }) {
                    activeTabId = hit.id
                    return hit.id
                }
            }
        }
        return nil
    }

    /// Flatten every tab (Terminals + user sessions) into dicts for MCP.
    func mcpListTabs() -> [[String: String]] {
        var rows: [[String: String]] = []
        rows.append([
            "id": terminalsTab.id,
            "title": terminalsTab.title,
            "cwd": terminalsTab.cwd,
            "branch": "",
            "status": terminalsTab.status.rawValue,
            "project": "terminals",
        ])
        for project in projects {
            for tab in project.tabs {
                rows.append([
                    "id": tab.id,
                    "title": tab.title,
                    "cwd": tab.cwd,
                    "branch": tab.branch ?? "",
                    "status": tab.status.rawValue,
                    "project": project.id,
                ])
            }
        }
        return rows
    }

    /// Spawn a new terminal pane in a tab. Defaults to the active tab
    /// when `tabId` is nil; uses the tab's cwd when `cwd` is nil.
    func mcpOpenTerminal(tabId: String?, cwd: String?, title: String?) -> String? {
        let targetId = tabId ?? activeTabId
        guard let targetId else { return nil }
        guard tab(for: targetId) != nil else { return nil }
        return addPane(tabId: targetId, kind: .terminal, cwd: cwd, title: title)
    }

    /// Write `command + "\n"` into the target tab's active pane if it's
    /// a terminal; otherwise the first terminal pane on the tab.
    func mcpRun(tabId: String?, command: String) -> Bool {
        let targetId = tabId ?? activeTabId
        guard let targetId else { return false }
        guard let tab = tab(for: targetId) else { return false }

        let paneId: String? = {
            if let active = tab.activePane, active.kind == .terminal {
                return active.id
            }
            return tab.panes.first { $0.kind == .terminal }?.id
        }()
        guard let paneId else { return false }

        let session: TabPtySession
        if let existing = ptySessions[targetId] {
            session = existing
        } else {
            session = makeSession(for: targetId, cwd: tab.cwd)
        }
        session.sendToPane(command, paneId: paneId)
        return true
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
        let cfgPath: URL? = {
            do {
                return try ClaudeConfigWriter.writeConfig(port: mcp.port)
            } catch {
                NSLog("AppState: ClaudeConfigWriter failed: \(error)")
                return nil
            }
        }()

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

        // Built-in Terminals session: skip `NICE_TAB_ID` injection so
        // the zsh shadow's `claude()` falls through to the `newtab`
        // flow (open a new sidebar session), same behaviour as the old
        // Main Terminal. For user sessions, keep injecting so the
        // shadow fires `promoteTab` inside companion terminals.
        let injectTabIdEnv = (tabId != Self.terminalsTabId)

        let session = TabPtySession(
            tabId: tabId,
            cwd: resolvedCwd,
            claudeBinary: resolvedClaudePath,
            mcpConfigPath: cfgPath,
            extraClaudeArgs: extraClaudeArgs,
            initialClaudePaneId: claudePaneId,
            initialTerminalPaneId: terminalPaneId,
            socketPath: controlSocket?.path,
            zdotdirPath: zdotdirPath,
            injectTabIdEnv: injectTabIdEnv,
            onPaneExit: { [weak self] paneId, code in
                self?.paneExited(tabId: tabId, paneId: paneId, exitCode: code)
            },
            onPaneTitleChange: { [weak self] paneId, title in
                self?.paneTitleChanged(tabId: tabId, paneId: paneId, title: title)
            }
        )
        session.applyTheme(currentScheme, palette: currentPalette)
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
