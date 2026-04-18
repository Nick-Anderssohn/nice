//
//  AppState.swift
//  Nice
//
//  Phase 4 + terminal lifecycle cleanup. `AppState` owns the long-lived
//  pty sessions (one `MainTerminalSession` for the "Main terminal"
//  sidebar row, and a cache of `TabPtySession` keyed by tab id) and
//  fans process-exit events back into the data model so the sidebar
//  and split view can react to ctrl+D, `exit`, and crashes.
//

import AppKit
import Foundation
import SwiftUI

@MainActor
final class AppState: ObservableObject {
    @Published var projects: [Project]
    /// `nil` = the "Main terminal" row is selected.
    @Published var activeTabId: String?
    @Published var sidebarQuery: String = ""

    // MARK: - Phase 4 process plumbing

    @Published private(set) var ptySessions: [String: TabPtySession] = [:]
    @Published private(set) var mainTerminal: MainTerminalSession

    /// Surfaces a "Quit NICE?" alert when the Main Terminal exits while
    /// open tabs still exist. `AppShellView` binds its `.alert` to this
    /// flag and calls `cancelQuitPrompt()` / `NSApp.terminate(nil)`
    /// from the two buttons.
    @Published var showQuitPrompt: Bool = false

    /// Cached cwd for the Main Terminal so `cancelQuitPrompt` can
    /// respawn the shell at the same place the user started from.
    /// Kept in sync with `mainTerminal.cwd` via `restartMainTerminal`.
    private var storedMainCwd: String

    /// Tracks the SwiftUI `ColorScheme` currently showing. `AppShellView`
    /// keeps this in sync via `updateScheme(_:)` so new sessions can be
    /// themed at creation without the view layer plumbing the scheme in
    /// manually.
    private var currentScheme: ColorScheme = .dark

    // MARK: - Phase 6 MCP server

    /// In-process HTTP MCP server. Started from `bootstrap()` after the
    /// app launches; injects its port into every spawned claude via
    /// `--mcp-config`.
    @Published private(set) var mcp = NiceMCPServer()

    /// Absolute path to the `claude` binary if we've resolved it; nil
    /// means either we haven't finished resolving yet, or it isn't on
    /// the user's PATH. `TabPtySession` falls back to zsh when nil.
    private var resolvedClaudePath: String?

    // MARK: - Phase 7 control socket

    /// Unix-domain-socket listener the Main Terminal's shadowed
    /// `claude()` zsh function posts newtab requests to. Exposed here
    /// so the socket path can be injected into the Main Terminal's env
    /// as `NICE_SOCKET`.
    private var controlSocket: NiceControlSocket?

    /// Filesystem path of the ZDOTDIR directory produced by
    /// `MainTerminalShellInject.make`. Captured at init so every new
    /// `TabPtySession` can hand it to its companion spawns, making the
    /// shadowed `claude()` zsh function available inside companion
    /// terminals as well.
    private var zdotdirPath: String?

    init() {
        self.projects = Project.seed
        // Main terminal is the default selection — `AppShellView` renders
        // just the terminal (no chat pane) in that state, which is the
        // right blank-slate for a fresh launch. Users opt into a tab by
        // clicking one or creating a new one.
        self.activeTabId = nil

        // Initial cwd for the main terminal mirrors the sidebar's
        // `@AppStorage("mainTerminalCwd")` default ($HOME). If the user
        // has customised it, the stored value wins.
        let storedMainCwd = UserDefaults.standard.string(forKey: "mainTerminalCwd")
            ?? NSHomeDirectory()
        self.storedMainCwd = storedMainCwd

        // Allocate the control socket + write the ZDOTDIR inject
        // *before* spawning the Main Terminal's zsh — the shell needs
        // NICE_SOCKET + ZDOTDIR in its environment at startup or the
        // `claude()` shadow never loads. The socket path is
        // pid-derived, so we can build `extraEnv` from it without the
        // listener being bound yet; we call `start(handler:)` only
        // after every stored property is initialized so `self`-capture
        // in the handler is legal.
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

        // Route Main Terminal exits through `mainTerminalExited`. Capture
        // weakly so the session's retained delegate doesn't keep the
        // AppState alive past app teardown.
        //
        // NB: we need a reference to pass into the MainTerminalSession
        // initializer, and we can't reference `self` until stored props
        // are set. So we stash a placeholder here and patch the closure
        // target through a local `weak self` box after init. In
        // practice: define the closure inline with `[weak self]` and
        // wait for full init before anything can fire.
        let mainExitBox = WeakSelfBox()
        self.mainTerminal = MainTerminalSession(
            cwd: storedMainCwd,
            extraEnv: extraEnv,
            onExit: { code in
                mainExitBox.value?.mainTerminalExited(exitCode: code)
            }
        )

        // Resolve `claude` synchronously on launch (takes <10ms) so the
        // first tab the user clicks actually runs claude, not the zsh
        // fallback. If it's not on PATH, resolvedClaudePath stays nil
        // and all tabs fall back to zsh.
        self.resolvedClaudePath = ProcessInfo.processInfo.environment["NICE_CLAUDE_OVERRIDE"]
            ?? Self.runWhich(binary: "claude")

        // Stored properties are all initialized — now safe to wire the
        // weak-box, start the socket listener, and register the
        // teardown observer.
        mainExitBox.value = self

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

        // Tear down the socket on app quit. Missing this is tolerable
        // — `unlink` before bind on next launch clears stale files —
        // but it keeps $TMPDIR tidy during normal usage.
        NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification,
            object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.controlSocket?.stop()
            }
        }
    }

    // MARK: - Selection

    func selectTab(_ id: String) {
        activeTabId = id
    }

    func selectMainTerminal() {
        activeTabId = nil
    }

    // MARK: - Tab creation

    /// Open a new tab rooted at `cwd`, running `claude` in the chat
    /// pane with any `args` forwarded through. Called from the
    /// `NiceControlSocket` handler when the Main Terminal's shadowed
    /// `claude()` function posts a newtab message.
    ///
    /// `cwd` comes straight from the Main Terminal's `$PWD` at the
    /// moment of invocation; `args` is the raw argv the user typed.
    func createTabFromMainTerminal(cwd: String, args: [String]) {
        let newId = "t\(Int(Date().timeIntervalSince1970 * 1000))"
        let title: String = {
            guard !args.isEmpty else { return "New tab" }
            let joined = args.joined(separator: " ")
            let trimmed = String(joined.prefix(40))
                .trimmingCharacters(in: .whitespaces)
            return trimmed.isEmpty ? "New tab" : trimmed
        }()
        let companionId = "\(newId)-c1"
        let tab = Tab(
            id: newId,
            title: title,
            status: .idle,
            cwd: cwd,
            branch: nil,
            hasClaudePane: true,
            companions: [CompanionTerminal(id: companionId, title: "Terminal 1")],
            activeCompanionId: companionId
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
            let newProject = Project(id: projectId, name: dirName, path: normalizedCwd, tabs: [tab])
            projects.append(newProject)
        }
        activeTabId = newId
        _ = session(for: newId, cwd: cwd, extraClaudeArgs: args)
    }

    // MARK: - Bootstrap

    /// Called from `AppShellView.task` on first render. Starts the MCP
    /// server exactly once, unless the user has unchecked
    /// `@AppStorage("mcpAutoStart")` in Settings → MCP. Guarded by
    /// `mcp.isRunning` so SwiftUI refiring `.task` is harmless.
    func bootstrap() async {
        let defaults = UserDefaults.standard
        let shouldAutoStart = (defaults.object(forKey: "mcpAutoStart") as? Bool) ?? true
        guard shouldAutoStart else { return }
        await mcp.start(appState: self)
    }

    // MARK: - Theme

    /// Called from `AppShellView` whenever the effective color scheme
    /// changes (and once on first appear). Stores the value so new
    /// sessions spawn pre-themed, and re-applies to every existing
    /// session so pre-existing tabs repaint live.
    func updateScheme(_ scheme: ColorScheme) {
        currentScheme = scheme
        for session in ptySessions.values {
            session.applyTheme(scheme)
        }
        mainTerminal.applyTheme(scheme)
    }

    // MARK: - Lifecycle handlers

    /// The Claude process in `tabId` exited. The tab stays in its
    /// project group (so its companions keep running) but its icon
    /// flips and the chat pane goes away.
    func claudePaneExited(tabId: String, exitCode: Int32?) {
        guard let (pi, ti) = tabIndex(for: tabId) else { return }
        projects[pi].tabs[ti].hasClaudePane = false
        ptySessions[tabId]?.closeClaude()
    }

    /// A companion in `tabId` exited. Drop it from the tab's companion
    /// list and the session's `terminals` dict. If it was the active
    /// pill, focus an adjacent one. If that was the last companion AND
    /// Claude is already gone, remove the tab entirely.
    func companionExited(tabId: String, companionId: String, exitCode: Int32?) {
        guard let (pi, ti) = tabIndex(for: tabId) else { return }
        var tab = projects[pi].tabs[ti]
        guard let compIdx = tab.companions.firstIndex(where: { $0.id == companionId }) else {
            return
        }
        tab.companions.remove(at: compIdx)

        // Pick a neighbor if the removed companion was active. Prefer
        // the one that took its slot (index `compIdx`), else the
        // previous, else nil (covered below).
        if tab.activeCompanionId == companionId {
            if compIdx < tab.companions.count {
                tab.activeCompanionId = tab.companions[compIdx].id
            } else if compIdx > 0 {
                tab.activeCompanionId = tab.companions[compIdx - 1].id
            } else {
                tab.activeCompanionId = nil
            }
        }

        // Drop the session's view/delegate regardless of what's left.
        ptySessions[tabId]?.removeCompanion(id: companionId)

        // If the tab has nothing left to host, dissolve it entirely.
        if tab.companions.isEmpty && tab.hasClaudePane == false {
            projects[pi].tabs.remove(at: ti)
            ptySessions.removeValue(forKey: tabId)
            if activeTabId == tabId {
                activeTabId = nil
            }
        } else {
            projects[pi].tabs[ti] = tab
        }
    }

    /// The Main Terminal's zsh exited. Terminate the app if nothing
    /// else is running; otherwise surface a "Quit NICE?" confirmation
    /// that lets the user back out by respawning the shell.
    func mainTerminalExited(exitCode: Int32?) {
        if projects.flatMap({ $0.tabs }).isEmpty {
            NSApp.terminate(nil)
        } else {
            showQuitPrompt = true
        }
    }

    /// Cancel the post-exit quit prompt: hide the alert and bring the
    /// Main Terminal's zsh back up in the same cwd.
    func cancelQuitPrompt() {
        showQuitPrompt = false
        mainTerminal.restart(cwd: storedMainCwd)
    }

    /// Promote a terminal-only tab back to Claude-tab state. Called
    /// from the control socket's `promoteTab` handler when a companion
    /// shell's shadowed `claude()` fires (it's already about to
    /// `exec claude` in the same pty — this just flips the data model
    /// so the layout shows it as the chat pane).
    ///
    /// Assumption: the shadow that fired this message ran in the
    /// companion the user currently has focused, so `activeCompanionId`
    /// is the promote source. This is true by construction because the
    /// shell only executes inside whichever pty the user is typing in.
    /// The wire payload deliberately doesn't carry a companion id — we
    /// infer it here.
    func promoteTabToClaude(tabId: String, args: [String]) {
        guard let (pi, ti) = tabIndex(for: tabId) else {
            NSLog("AppState: promoteTabToClaude for unknown tab \(tabId) — ignoring")
            return
        }
        // Tab is already a Claude tab → the shadow will still
        // `exec claude` inline, giving the user a second claude in
        // that companion. Nothing for the layout to do.
        if projects[pi].tabs[ti].hasClaudePane {
            return
        }
        guard let activeCompanionId = projects[pi].tabs[ti].activeCompanionId else {
            NSLog("AppState: promoteTabToClaude with no active companion on \(tabId)")
            return
        }

        // Move the companion's delegate/role into the chat slot. The
        // Phase 1 implementation of `promoteCompanionToChat` flips
        // `isClaudeAlive` and rewires the delegate without physically
        // moving the view (deferred to Phase 3 when chatView becomes
        // optional). That's sufficient here.
        if let session = ptySessions[tabId] {
            _ = session.promoteCompanionToChat(id: activeCompanionId)
        }

        // Strip the promoted companion from the tab's companion list —
        // it is now the chat pane, not a companion.
        projects[pi].tabs[ti].companions.removeAll { $0.id == activeCompanionId }
        projects[pi].tabs[ti].hasClaudePane = true

        // If nothing remains on the companion side, spawn a fresh one
        // so the layout has a terminal to render next to the chat.
        if projects[pi].tabs[ti].companions.isEmpty {
            let tabCwd = projects[pi].tabs[ti].cwd
            _ = addCompanion(tabId: tabId, cwd: tabCwd)
            // addCompanion already updates activeCompanionId to the
            // newly spawned id.
        } else {
            // At least one companion still exists — focus the first.
            projects[pi].tabs[ti].activeCompanionId =
                projects[pi].tabs[ti].companions.first?.id
        }
    }

    // MARK: - Companion management

    /// Append a new companion to `tabId`, spawn its pty, and focus it.
    /// Returns the new companion id, or nil if the tab doesn't exist.
    @discardableResult
    func addCompanion(
        tabId: String,
        cwd: String? = nil,
        title: String? = nil
    ) -> String? {
        guard let (pi, ti) = tabIndex(for: tabId) else { return nil }
        let newId = "\(tabId)-c\(Int(Date().timeIntervalSince1970 * 1000))"
        let resolvedTitle = title ?? "Terminal \(projects[pi].tabs[ti].companions.count + 1)"
        projects[pi].tabs[ti].companions.append(
            CompanionTerminal(id: newId, title: resolvedTitle)
        )
        projects[pi].tabs[ti].activeCompanionId = newId

        // Ensure a session exists for this tab before asking it to host
        // a new companion. Use the tab's cwd if no override is given.
        let tabCwd = projects[pi].tabs[ti].cwd
        let session: TabPtySession
        if let existing = ptySessions[tabId] {
            session = existing
        } else {
            session = self.session(for: tabId, cwd: tabCwd)
        }
        _ = session.addCompanion(id: newId, cwd: cwd ?? tabCwd)
        return newId
    }

    /// Focus a specific companion in `tabId`. No-op if the id isn't
    /// actually one of the tab's companions.
    func setActiveCompanion(tabId: String, companionId: String) {
        guard let (pi, ti) = tabIndex(for: tabId) else { return }
        guard projects[pi].tabs[ti].companions.contains(where: { $0.id == companionId }) else {
            return
        }
        projects[pi].tabs[ti].activeCompanionId = companionId
    }

    /// Ask a companion to quit by writing `"exit\n"` into its pty. The
    /// process-exit delegate (`companionExited`) handles cleanup once
    /// the shell actually dies, so this is a "soft" close — if zsh is
    /// blocked on a running child (e.g. vim), nothing happens.
    func requestCloseCompanion(tabId: String, companionId: String) {
        guard let session = ptySessions[tabId] else { return }
        session.sendToTerminal("exit", companionId: companionId)
    }

    // MARK: - MCP tool handlers

    /// Switch to a tab by id or fuzzy title match. Returns the resolved
    /// id, or nil if nothing matches.
    func mcpSwitchTab(tabId: String?, titleQuery: String?) -> String? {
        if let tabId, tab(for: tabId) != nil {
            activeTabId = tabId
            return tabId
        }
        if let q = titleQuery?.lowercased(), !q.isEmpty {
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

    /// Flatten every tab across every project into dicts suitable for
    /// JSON encoding over the MCP wire.
    func mcpListTabs() -> [[String: String]] {
        var rows: [[String: String]] = []
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

    /// Spawn a new companion terminal in a tab. Defaults to the active
    /// tab when `tabId` is nil; uses the tab's cwd when `cwd` is nil;
    /// uses "Terminal N" when `title` is nil. Returns the new companion
    /// id, or nil if no valid target tab could be resolved.
    func mcpOpenTerminal(tabId: String?, cwd: String?, title: String?) -> String? {
        let targetId = tabId ?? activeTabId
        guard let targetId else { return nil }
        guard tab(for: targetId) != nil else { return nil }
        return addCompanion(tabId: targetId, cwd: cwd, title: title)
    }

    /// Write `command + "\n"` into the target tab's active companion.
    /// Defaults to the active tab when `tabId` is nil. Returns false if
    /// no suitable session/companion is found.
    func mcpRun(tabId: String?, command: String) -> Bool {
        let targetId = tabId ?? activeTabId
        guard let targetId else { return false }
        guard let tab = tab(for: targetId) else { return false }
        guard let companionId = tab.activeCompanionId ?? tab.companions.first?.id else {
            return false
        }
        let session: TabPtySession
        if let existing = ptySessions[targetId] {
            session = existing
        } else {
            session = self.session(for: targetId, cwd: tab.cwd)
        }
        session.sendToTerminal(command, companionId: companionId)
        return true
    }

    // MARK: - Pty sessions

    /// Return the pty session for `tabId`, creating and caching one if
    /// it doesn't exist yet. The `cwd` argument is used only on first
    /// creation; subsequent lookups return the existing session as-is.
    ///
    /// On first creation, we also synthesize a temp `.mcp.json` and
    /// thread its path into claude via `--mcp-config`, so the claude
    /// inside that tab can call back into the Nice MCP server. A
    /// failure to write the config is non-fatal — we fall back to
    /// spawning claude without it.
    ///
    /// `extraClaudeArgs` is forwarded to `TabPtySession` and appended
    /// after `--mcp-config` on the claude command line. Populated only
    /// on the Main-Terminal-driven creation path
    /// (`createTabFromMainTerminal`); other callers (MCP tools that
    /// need to warm a session, tab-row taps) pass an empty array.
    ///
    /// Invariant: every tab that has a session also has ≥1
    /// `CompanionTerminal` whose id matches a key in
    /// `session.terminals`. If the tab doesn't already have a
    /// companion on file (e.g. an MCP `nice.run` warm-up arrives
    /// before `createTabFromMainTerminal` seeded one), we synthesize
    /// one here and insert it into the `Tab` before constructing the
    /// session so caller assumptions hold.
    func session(
        for tabId: String,
        cwd: String? = nil,
        extraClaudeArgs: [String] = []
    ) -> TabPtySession {
        if let existing = ptySessions[tabId] {
            return existing
        }
        let resolvedCwd = cwd ?? tab(for: tabId)?.cwd ?? NSHomeDirectory()
        let cfgPath: URL? = {
            do {
                return try ClaudeConfigWriter.writeConfig(port: mcp.port)
            } catch {
                NSLog("AppState: ClaudeConfigWriter failed: \(error)")
                return nil
            }
        }()

        // Resolve the initial companion id. Prefer an id already on the
        // tab (set up by `createTabFromMainTerminal` or the seed data).
        // Otherwise mint a fresh one and patch it into the tab so the
        // model + session agree.
        let initialCompanionId: String
        if let (pi, ti) = tabIndex(for: tabId),
           let first = projects[pi].tabs[ti].companions.first?.id {
            initialCompanionId = first
            if projects[pi].tabs[ti].activeCompanionId == nil {
                projects[pi].tabs[ti].activeCompanionId = first
            }
        } else if let (pi, ti) = tabIndex(for: tabId) {
            let synth = "\(tabId)-c1"
            projects[pi].tabs[ti].companions.append(
                CompanionTerminal(id: synth, title: "Terminal 1")
            )
            projects[pi].tabs[ti].activeCompanionId = synth
            initialCompanionId = synth
        } else {
            // No matching tab in the model — rare; still need an id so
            // the session is well-formed. Caller is responsible for
            // either adding a tab or accepting the orphan session.
            initialCompanionId = "\(tabId)-c1"
        }

        let session = TabPtySession(
            tabId: tabId,
            cwd: resolvedCwd,
            claudeBinary: resolvedClaudePath,
            mcpConfigPath: cfgPath,
            extraClaudeArgs: extraClaudeArgs,
            initialCompanionId: initialCompanionId,
            socketPath: controlSocket?.path,
            zdotdirPath: zdotdirPath,
            onChatExit: { [weak self] code in
                self?.claudePaneExited(tabId: tabId, exitCode: code)
            },
            onCompanionExit: { [weak self] companionId, code in
                self?.companionExited(
                    tabId: tabId,
                    companionId: companionId,
                    exitCode: code
                )
            }
        )
        session.applyTheme(currentScheme)
        ptySessions[tabId] = session
        return session
    }

    /// Called from the sidebar when the user picks a new directory for
    /// the main terminal. Re-spawns zsh rooted at `cwd`.
    func restartMainTerminal(cwd: String) {
        storedMainCwd = cwd
        mainTerminal.restart(cwd: cwd)
    }

    // MARK: - Claude binary resolution

    /// Synchronous `/usr/bin/which <binary>` helper. Returns the first
    /// line of stdout (trimmed) on success, or nil if the binary can't
    /// be resolved or `Process` throws. Called once at launch; cheap
    /// enough to run on the main thread.
    private nonisolated static func runWhich(binary: String) -> String? {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/usr/bin/which")
        proc.arguments = [binary]
        let pipe = Pipe()
        proc.standardOutput = pipe
        proc.standardError = Pipe()
        do {
            try proc.run()
            proc.waitUntilExit()
            guard proc.terminationStatus == 0 else { return nil }
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            guard let raw = String(data: data, encoding: .utf8) else { return nil }
            let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            return trimmed.isEmpty ? nil : trimmed
        } catch {
            return nil
        }
    }

    // MARK: - Filtering / lookup

    /// Case-insensitive title filter. Projects with zero matching tabs are
    /// dropped from the returned list (mirrors sidebar.jsx behaviour).
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

    /// Look up a tab by id across all projects. Returns a copy.
    func tab(for id: String) -> Tab? {
        for project in projects {
            if let hit = project.tabs.first(where: { $0.id == id }) {
                return hit
            }
        }
        return nil
    }

    /// Look up `(projectIndex, tabIndex)` for the tab with id `id`, so
    /// callers can mutate the tab in place without the cost of
    /// re-searching. `Tab` is a struct, so mutating the copy returned
    /// by `tab(for:)` wouldn't write back to `projects`.
    private func tabIndex(for id: String) -> (Int, Int)? {
        for (pi, project) in projects.enumerated() {
            if let ti = project.tabs.firstIndex(where: { $0.id == id }) {
                return (pi, ti)
            }
        }
        return nil
    }
}

// MARK: - Init-time weak self routing

/// Tiny weak-reference box used during `AppState.init` to wire the Main
/// Terminal's exit handler. `MainTerminalSession` needs the closure at
/// construction time, but `self` isn't available until every stored
/// property has been initialised. We hand the session a closure that
/// captures this box; after init completes, `box.value = self` is set
/// and the closure starts routing correctly.
private final class WeakSelfBox: @unchecked Sendable {
    weak var value: AppState?
}
