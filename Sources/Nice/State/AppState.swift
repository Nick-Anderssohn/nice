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

    /// Absolute path to the `claude` binary if we've resolved it; nil
    /// means either we haven't finished resolving yet, or it isn't on
    /// the user's PATH. `TabPtySession` falls back to zsh when nil.
    private var resolvedClaudePath: String?

    init() {
        self.projects = Project.seed
        self.activeTabId = "t1"

        // Initial cwd for the main terminal mirrors the sidebar's
        // `@AppStorage("mainTerminalCwd")` default ($HOME). If the user
        // has customised it, the stored value wins.
        let storedMainCwd = UserDefaults.standard.string(forKey: "mainTerminalCwd")
            ?? NSHomeDirectory()
        self.mainTerminal = MainTerminalSession(cwd: storedMainCwd)

        // Resolve `claude` synchronously on launch (takes <10ms) so the
        // very first tab spawned in init below actually runs claude, not
        // the zsh fallback. If it's not on PATH, resolvedClaudePath stays
        // nil and all tabs fall back to zsh.
        self.resolvedClaudePath = Self.runWhich(binary: "claude")

        // Warm the pty for the initial selection so the first render
        // shows a live terminal, not a white frame.
        if let id = activeTabId, let tab = tab(for: id) {
            _ = session(for: id, cwd: tab.cwd)
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

    /// Prepend a freshly created tab to the first project, spawn its
    /// pty pair, and select it.
    func newTab() {
        guard !projects.isEmpty else { return }
        let newId = "t\(Int(Date().timeIntervalSince1970 * 1000))"
        let first = projects[0]
        let tab = Tab(
            id: newId,
            title: "New tab",
            status: .idle,
            cwd: first.path,
            branch: nil
        )
        projects[0].tabs.insert(tab, at: 0)
        activeTabId = newId
        // Warm the pty so switching lands on a live view.
        _ = session(for: newId, cwd: tab.cwd)
    }

    // MARK: - Pty sessions

    /// Return the pty session for `tabId`, creating and caching one if
    /// it doesn't exist yet. The `cwd` argument is used only on first
    /// creation; subsequent lookups return the existing session as-is.
    func session(for tabId: String, cwd: String? = nil) -> TabPtySession {
        if let existing = ptySessions[tabId] {
            return existing
        }
        let resolvedCwd = cwd ?? tab(for: tabId)?.cwd ?? NSHomeDirectory()
        let session = TabPtySession(
            tabId: tabId,
            cwd: resolvedCwd,
            claudeBinary: resolvedClaudePath
        )
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
