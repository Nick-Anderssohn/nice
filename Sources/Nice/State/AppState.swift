//
//  AppState.swift
//  Nice
//
//  Phase 4: real process plumbing. In addition to the sidebar state from
//  phase 2, `AppState` now owns the long-lived pty sessions — one
//  `MainTerminalSession` (the "Main terminal" sidebar row) and a cache
//  of `TabPtySession` values keyed by tab id (middle + right panes per
//  tab). Sessions outlive SwiftUI redraws, so processes and scrollback
//  persist across tab switches.
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
        } catch {
            NSLog("AppState: ZDOTDIR inject failed: \(error)")
        }

        self.mainTerminal = MainTerminalSession(
            cwd: storedMainCwd,
            extraEnv: extraEnv
        )

        // Resolve `claude` synchronously on launch (takes <10ms) so the
        // first tab the user clicks actually runs claude, not the zsh
        // fallback. If it's not on PATH, resolvedClaudePath stays nil
        // and all tabs fall back to zsh.
        self.resolvedClaudePath = Self.runWhich(binary: "claude")

        // Stored properties are all initialized — now safe to capture
        // `self` and bind the listener. The handler fires on
        // `NiceControlSocket`'s background queue, so hop to MainActor
        // before touching app state. Bind failure is non-fatal:
        // claude() in the Main Terminal will detect the unreachable
        // socket and fall back to running claude in-place.
        do {
            try socket.start { [weak self] cwd, args in
                Task { @MainActor [weak self] in
                    self?.createTabFromMainTerminal(cwd: cwd, args: args)
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
        let tab = Tab(
            id: newId,
            title: title,
            status: .idle,
            cwd: cwd,
            branch: nil
        )
        if !projects.isEmpty {
            projects[0].tabs.insert(tab, at: 0)
        } else {
            // Defensive: seed is non-empty today, but keep the app
            // functional if that ever changes.
            projects.append(
                Project(id: "default", name: "default", path: cwd, tabs: [tab])
            )
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

    /// Write `command + "\n"` into the target tab's right-side zsh.
    /// Defaults to the active tab when `tabId` is nil. Returns false if
    /// no suitable session is found.
    func mcpRun(tabId: String?, command: String) -> Bool {
        let targetId = tabId ?? activeTabId
        guard let targetId else { return false }
        let session: TabPtySession
        if let existing = ptySessions[targetId] {
            session = existing
        } else if let tab = tab(for: targetId) {
            session = self.session(for: targetId, cwd: tab.cwd)
        } else {
            return false
        }
        session.sendToTerminal(command)
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
        let session = TabPtySession(
            tabId: tabId,
            cwd: resolvedCwd,
            claudeBinary: resolvedClaudePath,
            mcpConfigPath: cfgPath,
            extraClaudeArgs: extraClaudeArgs
        )
        session.applyTheme(currentScheme)
        ptySessions[tabId] = session
        return session
    }

    /// Called from the sidebar when the user picks a new directory for
    /// the main terminal. Re-spawns zsh rooted at `cwd`.
    func restartMainTerminal(cwd: String) {
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

    /// Look up a tab by id across all projects.
    private func tab(for id: String) -> Tab? {
        for project in projects {
            if let hit = project.tabs.first(where: { $0.id == id }) {
                return hit
            }
        }
        return nil
    }
}
