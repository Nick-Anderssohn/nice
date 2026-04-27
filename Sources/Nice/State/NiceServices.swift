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
    let releaseChecker: ReleaseChecker
    let editorDetector: EditorDetector

    /// Process-wide bundle of file-browser context-menu services:
    /// pasteboard adapter, undo history, OpenWith provider, and the
    /// shared FS worker. Lives here (not on AppState) so multiple
    /// windows share one undo stack and one pasteboard.
    let fileExplorer: FileExplorerServices

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
        self.releaseChecker = ReleaseChecker()
        self.editorDetector = EditorDetector()
        // Build the file-explorer service bundle. The history shares
        // its `service` with the orchestration layer so a fake
        // `FileManager` or `Trasher` injected for tests reaches
        // every code path. The history holds a weak reference to the
        // registry so cross-window undo can route focus back; both
        // outlive each other for the process lifetime.
        let foService = FileOperationsService()
        let history = FileOperationHistory(service: foService, registry: self.registry)
        self.fileExplorer = FileExplorerServices(
            pasteboard: FilePasteboardAdapter(),
            history: history,
            openWithProvider: OpenWithProvider()
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

        // Kick off editor auto-detection on a background queue. The
        // scan probes the user's PATH for well-known terminal editors
        // (vim, nvim, hx, …) so the File Explorer's "Open in Editor
        // Pane" submenu can populate without any prior config. Empty
        // until the scan returns; UI just shows whatever's
        // user-configured in the meantime.
        editorDetector.scan()

        // Install Claude Code's UserPromptSubmit hook so Nice-spawned
        // claudes phone home with their current session id on every
        // user message. Handles /clear, /compact, /branch rotations
        // that the tab's pre-minted UUID otherwise misses. Idempotent
        // and safe to run on every launch.
        ClaudeHookInstaller.install()
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
            fontSettings: fontSettings,
            fileOperationHistory: fileExplorer.history
        )

        releaseChecker.start()

        terminateObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            guard let self else { return }
            MainActor.assumeIsolated {
                self.releaseChecker.stop()
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

    /// Decision for a file encountered during the `$TMPDIR` sweep.
    /// `.ignore` — not a Nice artifact; leave it alone.
    /// `.keep` — a Nice artifact whose owning pid is still alive.
    /// `.remove` — leftover from a prior crashed run.
    enum TempFileDecision: Equatable {
        case ignore
        case keep
        case remove
    }

    /// Remove `nice-*.sock` and `nice-zdotdir-*` leftovers from prior
    /// runs that crashed without running `tearDown`. Files whose
    /// embedded pid names a still-running process are left alone — in
    /// particular, running `Nice Dev` while prod `Nice` is open must
    /// not wipe prod's zdotdir, or prod's zsh children suddenly source
    /// nothing and lose the user's `~/.zshrc` aliases.
    private nonisolated static func cleanupStaleTempFiles() {
        let tmp = URL(fileURLWithPath: NSTemporaryDirectory())
        let fm = FileManager.default
        guard let contents = try? fm.contentsOfDirectory(
            at: tmp, includingPropertiesForKeys: nil
        ) else { return }
        for url in contents {
            switch tempFileDecision(
                filename: url.lastPathComponent,
                isAlive: pidIsAlive
            ) {
            case .ignore, .keep:
                continue
            case .remove:
                try? fm.removeItem(at: url)
            }
        }
    }

    /// Pure classifier for a single temp-dir entry. Extracted from
    /// `cleanupStaleTempFiles` so the ownership policy can be unit
    /// tested without touching the filesystem or spawning siblings.
    nonisolated static func tempFileDecision(
        filename: String,
        isAlive: (pid_t) -> Bool
    ) -> TempFileDecision {
        if let pid = parsePid(fromZdotdirName: filename) {
            return isAlive(pid) ? .keep : .remove
        }
        if let pid = parsePid(fromSocketName: filename) {
            return isAlive(pid) ? .keep : .remove
        }
        return .ignore
    }

    /// Extract `<pid>` from `nice-zdotdir-<pid>`. Returns nil when the
    /// filename doesn't match the pattern or the pid isn't parseable.
    private nonisolated static func parsePid(fromZdotdirName name: String) -> pid_t? {
        let prefix = "nice-zdotdir-"
        guard name.hasPrefix(prefix) else { return nil }
        return pid_t(name.dropFirst(prefix.count))
    }

    /// Extract `<pid>` from `nice-<pid>-<suffix>.sock` (the socket
    /// naming used by `NiceControlSocket`).
    private nonisolated static func parsePid(fromSocketName name: String) -> pid_t? {
        guard name.hasPrefix("nice-"), name.hasSuffix(".sock") else { return nil }
        let body = name.dropFirst("nice-".count).dropLast(".sock".count)
        guard let dashIdx = body.firstIndex(of: "-") else { return nil }
        return pid_t(body[..<dashIdx])
    }

    /// `kill(pid, 0)` probes liveness without sending a signal. It
    /// returns 0 when the signal *would* have been delivered, -1 with
    /// `ESRCH` when the pid is gone, and -1 with `EPERM` when the
    /// process exists but we can't signal it (different user). Treat
    /// anything other than `ESRCH` as "alive" so we never reap another
    /// live Nice's tempfile.
    private nonisolated static func pidIsAlive(_ pid: pid_t) -> Bool {
        if kill(pid, 0) == 0 { return true }
        return errno != ESRCH
    }
}
