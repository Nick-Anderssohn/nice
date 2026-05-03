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
@Observable
final class NiceServices {
    let tweaks: Tweaks
    let shortcuts: KeyboardShortcuts
    let fontSettings: FontSettings
    let fileBrowserSortSettings: FileBrowserSortSettings
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
    /// back to zsh inside claude panes. Resolved off the main thread
    /// so SwiftUI scene-graph init isn't blocked on a login-shell
    /// probe — `Process.waitUntilExit` pumps a CFRunLoop while
    /// waiting, which re-enters SwiftUI mid-`@State` update and
    /// trips AttributeGraph's "setting value during update" abort
    /// (see Sources/Nice/NiceApp.swift). Stays nil until the probe
    /// returns; `AppState` re-arms `withObservationTracking` so tabs
    /// created after resolution still pick up the path.
    private(set) var resolvedClaudePath: String?

    /// Process-wide ZDOTDIR directory whose stub `.zshrc` chains back to
    /// the user's real `$HOME/.zshrc` and shadows `claude` to talk to
    /// our control socket. Owned here (not per-AppState) so multi-window
    /// scenarios share one dir and a closing window can't yank it out
    /// from under another window's still-spawning shells. Written by
    /// `bootstrap()` *after* `cleanupStaleTempFiles` so the cleanup
    /// never wipes the dir we just wrote. Deleted by the
    /// `willTerminate` observer.
    private(set) var zdotdirPath: String?

    @ObservationIgnored
    private var terminateObserver: NSObjectProtocol?

    /// One pending tear-off, claimed by the new window minted by
    /// `requestPaneTearOff`. Set immediately before `openWindow(id:)`
    /// fires; consumed by the matching window's `AppShellHost.task`
    /// after `appState.start()`. The detached `NiceTerminalView` is
    /// parent-less while parked here for at most a few runloop hops —
    /// AppKit allows that.
    var pendingTearOff: PendingTearOff?

    /// How long a `pendingTearOff` slot stays valid before
    /// `consumeTearOff` drops it as stale. Long enough for `openWindow`
    /// to spawn even when Stage Manager / Mission Control quirks delay
    /// the new scene; short enough that an unrelated ⌘N a few seconds
    /// later doesn't pick up the abandoned pane.
    static let tearOffTTL: TimeInterval = 2.0

    init() {
        self.tweaks = Tweaks()
        self.shortcuts = KeyboardShortcuts()
        self.fontSettings = FontSettings()
        self.fileBrowserSortSettings = FileBrowserSortSettings()
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
    }

    /// Idempotent process-wide wiring. Sweeps stale temp files, writes
    /// this run's ZDOTDIR, kicks off the async `which claude` probe,
    /// installs the keyboard monitor / Claude-Code hook, and registers
    /// the terminate observer that tears every window down. Called
    /// from `AppShellHost.task` before any AppState's `start()` so
    /// `zdotdirPath` and the env-var override are populated by the
    /// time pty children inherit env.
    @ObservationIgnored
    private var booted = false
    func bootstrap() {
        guard !booted else { return }
        booted = true

        // Sweep `$TMPDIR` debris from prior crashed runs *before*
        // writing this run's zdotdir — otherwise the cleanup would
        // race the freshly-written dir and delete it, causing every
        // companion shell spawned after onAppear to source nothing.
        Self.cleanupStaleTempFiles()

        // Reap zsh processes orphaned by prior crashes / SIGKILLs
        // before any new pane spawns, so we don't inherit a starved
        // pty table. macOS caps `kern.tty.ptmx_max` at 511; without
        // this, accumulated orphans (especially from aborted UITest
        // runs) cause `forkpty()` inside SwiftTerm to fail and panes
        // hang on "Launching terminal…" forever.
        let reaped = OrphanShellReaper.reap()
        if reaped > 0 {
            NSLog("NiceServices: reaped \(reaped) orphan zsh shell(s) from prior runs")
        }
        do {
            self.zdotdirPath = try MainTerminalShellInject.make().path
        } catch {
            NSLog("NiceServices: ZDOTDIR inject failed: \(error)")
            self.zdotdirPath = nil
        }
        // The env-var override is a cheap dict read — apply it
        // synchronously so UI tests that set NICE_CLAUDE_OVERRIDE see
        // the path immediately (no resolution race for tabs spawned
        // during early launch). The expensive login-shell probe is
        // deferred to `Task.detached` to avoid blocking SwiftUI's
        // scene-graph init on `Process.waitUntilExit`.
        if let override = ProcessInfo.processInfo.environment["NICE_CLAUDE_OVERRIDE"] {
            self.resolvedClaudePath = override
        } else {
            Task.detached(priority: .utility) { [weak self] in
                let resolved = Self.runWhich(binary: "claude")
                await MainActor.run { [weak self] in
                    self?.resolvedClaudePath = resolved
                }
            }
        }

        // Kick off editor auto-detection on a background queue. The
        // scan probes the user's PATH for well-known terminal editors
        // (vim, nvim, hx, …) so the File Explorer's "Open in Editor
        // Pane" submenu can populate without any prior config. Empty
        // until the scan returns; UI just shows whatever's
        // user-configured in the meantime.
        editorDetector.scan()

        // Install Claude Code's SessionStart hook so Nice-spawned
        // claudes phone home with their current session id whenever
        // they rotate it (/clear, /compact, /branch). The tab's
        // pre-minted UUID otherwise misses these in-process rotations.
        // Idempotent and safe to run on every launch.
        ClaudeHookInstaller.install()

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

    // MARK: - Pane tear-off

    /// Mint a destination window-session-id, detach the source pane's
    /// view, and stash the migrated bundle on `pendingTearOff` so the
    /// new window's `AppShellHost.task` can absorb it. Source-side
    /// model mutation happens here too — but `onSessionMutation`
    /// (which schedules disk persistence) is **deferred** until the
    /// destination acknowledges absorption (`completeTearOff(_:)`).
    /// That way a failed window spawn can roll back without losing
    /// the pane on the source's next disk read.
    ///
    /// Returns the minted destination window-session-id — caller
    /// passes this to `openWindow(id:value:)` so the destination can
    /// claim its tear-off via `consumeTearOff(forWindowSessionId:)`.
    /// Returns `nil` when the source pane / tab can't be resolved.
    @discardableResult
    func requestPaneTearOff(
        from sourceAppState: AppState,
        tabId: String,
        paneId: String,
        cursorScreenPoint: CGPoint,
        pillOriginOffset: CGSize
    ) -> String? {
        guard let sourceTab = sourceAppState.tabs.tab(for: tabId),
              let pane = sourceTab.panes.first(where: { $0.id == paneId })
        else { return nil }
        guard let anchor = ProjectAnchor.from(
            sourceTabId: tabId, sourceAppState: sourceAppState
        ) else { return nil }

        let payload = PaneDragPayload(
            windowSessionId: sourceAppState.windowSession.windowSessionId,
            tabId: tabId, paneId: paneId, kind: pane.kind
        )

        let sourcePty = sourceAppState.sessions.ptySessions[tabId]
        let detachedView = sourcePty?.detachPane(id: paneId)
        let launchState = sourceAppState.sessions.paneLaunchStates[paneId]

        // Mutate source TabModel — remove pane, recover activePaneId.
        var sourceBecameEmpty = false
        sourceAppState.tabs.mutateTab(id: tabId) { tab in
            guard let idx = tab.panes.firstIndex(where: { $0.id == paneId })
            else { return }
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
            sourceBecameEmpty = tab.panes.isEmpty
        }
        if launchState != nil {
            sourceAppState.sessions.clearPaneLaunch(paneId: paneId)
        }
        if sourceBecameEmpty,
           let (pi, ti) = sourceAppState.tabs.projectTabIndex(for: tabId) {
            sourceAppState.sessions.onTabBecameEmpty?(tabId, pi, ti)
        }
        // Source persistence is deferred — `completeTearOff` schedules
        // it once the destination has absorbed.

        let destWindowSessionId = UUID().uuidString
        pendingTearOff = PendingTearOff(
            payload: payload,
            view: detachedView,
            pane: pane,
            sourceTab: sourceTab,
            originAppStateId: ObjectIdentifier(sourceAppState),
            destinationWindowSessionId: destWindowSessionId,
            projectAnchor: anchor,
            cursorScreenPoint: cursorScreenPoint,
            pillOriginOffset: pillOriginOffset,
            pendingLaunchState: launchState,
            createdAt: Date()
        )
        return destWindowSessionId
    }

    /// Claim the pending tear-off iff `windowSessionId` matches the
    /// destination tag minted in `requestPaneTearOff` AND the entry
    /// hasn't aged past `tearOffTTL`. Stale entries are dropped on
    /// inspection so a later window spawn can't pick them up.
    func consumeTearOff(forWindowSessionId windowSessionId: String) -> PendingTearOff? {
        guard let pending = pendingTearOff else { return nil }
        if Date().timeIntervalSince(pending.createdAt) > Self.tearOffTTL {
            pendingTearOff = nil
            return nil
        }
        guard pending.destinationWindowSessionId == windowSessionId else {
            return nil
        }
        pendingTearOff = nil
        return pending
    }

    /// Notify that destination absorption succeeded — schedules the
    /// deferred source persistence so the disk record reflects the
    /// completed move. Called by the new window's `AppShellHost.task`
    /// right after `absorbTearOff(_:)`.
    func completeTearOff(_ pending: PendingTearOff) {
        guard let source = registry.appState(forSessionId: pending.payload.windowSessionId) else {
            return
        }
        source.sessions.onSessionMutation?()
    }

    /// Source-side rollback when no destination claimed the pending
    /// tear-off (window spawn failed, TTL expired before mount). Re-
    /// inserts the detached view + pane back into the source tab so the
    /// pty isn't orphaned. Idempotent — safe to call from multiple
    /// recovery paths.
    func recoverAbandonedTearOff() {
        guard let pending = pendingTearOff else { return }
        // Only reclaim if the entry is stale; otherwise the destination
        // window may still mount and absorb.
        guard Date().timeIntervalSince(pending.createdAt) > Self.tearOffTTL else {
            return
        }
        pendingTearOff = nil
        guard let source = registry.appState(forSessionId: pending.payload.windowSessionId) else {
            return
        }
        // Re-insert the pane back into the source tab. If the source
        // tab has been dissolved in the meantime, route through
        // `absorbAsNewTab` so the pane lands somewhere reachable.
        if source.tabs.tab(for: pending.payload.tabId) != nil {
            source.tabs.mutateTab(id: pending.payload.tabId) { tab in
                tab.panes.append(pending.pane)
                tab.activePaneId = pending.pane.id
            }
            if let view = pending.view,
               let pty = source.sessions.ptySessions[pending.payload.tabId] {
                pty.attachPane(id: pending.pane.id, view: view)
            }
        } else {
            source.absorbAsNewTab(
                pane: pending.pane,
                sourceTab: pending.sourceTab,
                view: pending.view,
                projectAnchor: pending.projectAnchor,
                pendingLaunchState: pending.pendingLaunchState
            )
        }
        source.sessions.onSessionMutation?()
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
