//
//  NiceServices.swift
//  Nice
//
//  App-wide services shared across every window:
//    • `Tweaks` and `KeyboardShortcuts` — user preferences. macOS
//      appearance is a process-level concern, and keybindings are a
//      single preference regardless of how many windows are open.
//    • `WindowRegistry` — lets the process-wide keyboard monitor and
//      termination hook route to the focused window.
//    • `resolvedClaudePath` — the `which claude` result is the same for
//      every window; computing it once at app launch keeps second-
//      window open below the user's latency threshold.
//
//  Per-window services (`AppState`, `NiceControlSocket`) stay with
//  their owning window so each window is fully isolated.
//

import AppKit
import Foundation

@MainActor
final class NiceServices: ObservableObject {
    let tweaks: Tweaks
    let shortcuts: KeyboardShortcuts
    let fontSettings: FontSettings
    let registry: WindowRegistry
    let terminalThemeCatalog: TerminalThemeCatalog

    /// Absolute path to the `claude` binary if resolvable; nil falls
    /// back to zsh inside claude panes. Computed once at init so
    /// opening a second window doesn't re-run the login-shell probe.
    let resolvedClaudePath: String?

    /// Process-wide ZDOTDIR directory whose stub `.zshrc` chains back to
    /// the user's real `$HOME/.zshrc` and shadows `claude` to talk to
    /// our control socket. Owned here (not per-AppState) so multi-window
    /// scenarios share one dir and a closing window can't yank it out
    /// from under another window's still-spawning shells. Created at
    /// init *after* `cleanupStaleTempFiles` so the cleanup never wipes
    /// the dir we just wrote. Deleted by the `willTerminate` observer.
    let zdotdirPath: String?

    private var terminateObserver: NSObjectProtocol?

    init() {
        self.tweaks = Tweaks()
        self.shortcuts = KeyboardShortcuts()
        self.fontSettings = FontSettings()
        self.registry = WindowRegistry()
        self.terminalThemeCatalog = TerminalThemeCatalog(
            supportDirectory: TerminalThemeCatalog.defaultSupportDirectory()
        )
        // Sweep `$TMPDIR` debris from prior crashed runs *before*
        // writing this run's zdotdir — otherwise the cleanup would
        // race the freshly-written dir and delete it, causing every
        // companion shell spawned after onAppear to source nothing.
        Self.cleanupStaleTempFiles()
        do {
            self.zdotdirPath = try MainTerminalShellInject.make().path
        } catch {
            NSLog("NiceServices: ZDOTDIR inject failed: \(error)")
            self.zdotdirPath = nil
        }
        self.resolvedClaudePath = ProcessInfo.processInfo.environment["NICE_CLAUDE_OVERRIDE"]
            ?? Self.runWhich(binary: "claude")
    }

    /// Idempotent process-wide wiring. Installs the single keyboard
    /// monitor and registers the terminate observer that tears every
    /// window down and removes the shared ZDOTDIR.
    private var booted = false
    func bootstrap() {
        guard !booted else { return }
        booted = true

        KeyboardShortcutMonitor.install(
            registry: registry,
            shortcuts: shortcuts,
            fontSettings: fontSettings
        )

        terminateObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            guard let self else { return }
            MainActor.assumeIsolated {
                for state in self.registry.allAppStates {
                    state.tearDown()
                }
                if let zdotdirPath = self.zdotdirPath {
                    try? FileManager.default.removeItem(atPath: zdotdirPath)
                }
            }
        }
    }

    // MARK: - Resolving `claude`

    /// Run `/bin/zsh -ilc 'command -v -- <binary>'` and return the
    /// absolute path if found. Runs as a login-interactive shell so the
    /// user's `.zshenv` / `.zshrc` PATH additions are respected — Nice
    /// launched from Finder/Spotlight otherwise inherits only the macOS
    /// default PATH.
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
            let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            guard trimmed.hasPrefix("/") else { return nil }
            return trimmed
        } catch {
            return nil
        }
    }

    // MARK: - Temp cleanup

    /// Remove `nice-*.sock` and `nice-zdotdir-*` leftovers from prior
    /// runs that crashed without running `tearDown`. Files belonging to
    /// *this* process's pid are left alone: live windows may still own
    /// the socket, and the next step of init writes the zdotdir we
    /// must not delete here.
    private nonisolated static func cleanupStaleTempFiles() {
        let tmp = URL(fileURLWithPath: NSTemporaryDirectory())
        let fm = FileManager.default
        let pid = getpid()
        let pidSocketPrefix = "nice-\(pid)-"
        let pidZdotdirName = "nice-zdotdir-\(pid)"
        guard let contents = try? fm.contentsOfDirectory(
            at: tmp, includingPropertiesForKeys: nil
        ) else { return }
        for url in contents {
            let name = url.lastPathComponent
            let isSocket = name.hasPrefix("nice-") && name.hasSuffix(".sock")
            let isZdotdir = name.hasPrefix("nice-zdotdir-")
            guard isSocket || isZdotdir else { continue }
            if isSocket, name.hasPrefix(pidSocketPrefix) { continue }
            if isZdotdir, name == pidZdotdirName { continue }
            try? fm.removeItem(at: url)
        }
    }
}
