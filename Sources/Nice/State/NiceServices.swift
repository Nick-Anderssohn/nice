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
//  Per-window services (`AppState`, `NiceMCPServer`, `NiceControlSocket`)
//  stay with their owning window so each window is fully isolated.
//

import AppKit
import Foundation

@MainActor
final class NiceServices: ObservableObject {
    let tweaks: Tweaks
    let shortcuts: KeyboardShortcuts
    let fontSettings: FontSettings
    let registry: WindowRegistry

    /// Absolute path to the `claude` binary if resolvable; nil falls
    /// back to zsh inside claude panes. Computed once at init so
    /// opening a second window doesn't re-run the login-shell probe.
    let resolvedClaudePath: String?

    private var terminateObserver: NSObjectProtocol?

    init() {
        self.tweaks = Tweaks()
        self.shortcuts = KeyboardShortcuts()
        self.fontSettings = FontSettings()
        self.registry = WindowRegistry()
        self.resolvedClaudePath = ProcessInfo.processInfo.environment["NICE_CLAUDE_OVERRIDE"]
            ?? Self.runWhich(binary: "claude")
    }

    /// Idempotent process-wide wiring. Installs the single keyboard
    /// monitor, cleans up `$TMPDIR` debris from prior crashed runs, and
    /// registers the terminate observer that tears every window down.
    private var booted = false
    func bootstrap() {
        guard !booted else { return }
        booted = true

        Self.cleanupStaleTempFiles()

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
    /// runs that crashed without running `tearDown`. Socket files from
    /// *this* process's pid are left alone — they may belong to live
    /// windows registered earlier this launch.
    private nonisolated static func cleanupStaleTempFiles() {
        let tmp = URL(fileURLWithPath: NSTemporaryDirectory())
        let fm = FileManager.default
        let pidPrefix = "nice-\(getpid())-"
        guard let contents = try? fm.contentsOfDirectory(
            at: tmp, includingPropertiesForKeys: nil
        ) else { return }
        for url in contents {
            let name = url.lastPathComponent
            let isSocket = name.hasPrefix("nice-") && name.hasSuffix(".sock")
            let isZdotdir = name.hasPrefix("nice-zdotdir-")
            guard isSocket || isZdotdir else { continue }
            if isSocket, name.hasPrefix(pidPrefix) { continue }
            try? fm.removeItem(at: url)
        }
    }
}
