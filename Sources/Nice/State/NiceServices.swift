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
import os

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

    /// Process-wide side channel for handing a live pane (running pty +
    /// `NiceTerminalView`) between windows during a cross-window move or
    /// tear-off. The pasteboard only carries the pane id; the live entry
    /// rides this registry instead. See `LivePaneRegistry`.
    let livePaneRegistry: LivePaneRegistry

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
    /// the user's real config and shadows `claude` to talk to our
    /// control socket. Owned here (not per-AppState) so multi-window
    /// scenarios share one dir and a closing window can't yank it out
    /// from under another window's still-spawning shells. Written by
    /// `bootstrap()` to a fixed, per-variant Application Support
    /// location (see `MainTerminalShellInject.defaultLocation`) that
    /// macOS's temp cleanup never sweeps, and reused across launches —
    /// so it is intentionally *not* deleted on terminate.
    private(set) var zdotdirPath: String?

    /// The `ZDOTDIR` value Nice inherited from its launch environment
    /// — set if the user had `launchctl setenv ZDOTDIR …` or a parent
    /// process exported it; nil otherwise. Plumbed into pty children as
    /// `NICE_USER_ZDOTDIR` so the synthetic `.zshenv` can restore it
    /// (and the user's intended layout) after our injection runs.
    /// Captured once at `bootstrap()` because Nice's own process env
    /// doesn't change during a session.
    private(set) var userZDotDir: String?

    @ObservationIgnored
    private var terminateObserver: NSObjectProtocol?

    /// Owns the close/quit lifecycle for every window in the process.
    /// `WindowRegistry` shares this same instance so the per-window
    /// close path and the app-terminate cascade resolve through one
    /// object — see `SessionLifecycleController`.
    @ObservationIgnored
    let lifecycleController: SessionLifecycleController

    /// Shared `windowSessionId` claim set. Every `AppState` this
    /// process spawns gets the same instance, so
    /// `WindowSession.restoreSavedWindow`'s adoption logic sees a
    /// process-wide view of which saved slots are already held.
    /// Threaded through `AppState.init`; the launch-time fan-out
    /// reads it directly via `WindowSession.unclaimedSavedWindowCount`.
    @ObservationIgnored
    let claimLedger: WindowClaimLedger

    /// One-shot guard for the launch-time multi-window fan-out. The
    /// first-mounted `AppShellHost.task` calls
    /// `consumeMultiWindowRestoreSlot` after `appState.start()` returns;
    /// only the call that flips this from false to true does the
    /// `openWindow(id: "main")` loop. Subsequent windows (sibling
    /// windows we spawn here, future ⌘N opens, anything AppKit may
    /// auto-restore) skip the fan-out so we don't double-spawn.
    @ObservationIgnored
    private var multiWindowRestoreFired = false

    /// Returns `true` on the FIRST call, `false` thereafter. MainActor
    /// isolation makes the read-modify-write atomic without a lock.
    func consumeMultiWindowRestoreSlot() -> Bool {
        guard !multiWindowRestoreFired else { return false }
        multiWindowRestoreFired = true
        return true
    }

    // MARK: - Tear-off seed pairing

    /// A live pane entry queued to be adopted into the new window a
    /// tear-off gesture opens. The entry holds a running pty, so it must
    /// be `@ObservationIgnored` (non-Observable payload). Each seed is
    /// paired to its window by an explicit UUID token (see
    /// `pendingTearOffs`) rather than by "the next window to mount", so a
    /// ⌘N / restore window that happens to mount first can never steal a
    /// tear-off's seed.
    struct PendingTearOff {
        /// The live pty entry detached from the source window, or nil
        /// when the torn-off pane was modelled-but-deferred in the source
        /// (never spawned). A nil entry means the destination must spawn
        /// the pane fresh — using `cwd` — rather than adopt a live one.
        let entry: TabPtySession.PaneEntry?
        /// Stable id of the pane being torn off.
        let paneId: String
        /// Display title of the pane (pill label).
        let title: String
        /// Kind of the pane being torn off (.terminal or .claude).
        let kind: PaneKind
        /// Claude session id if `kind == .claude`; nil for terminals.
        let claudeSessionId: String?
        /// Project identity to recreate in the destination window when
        /// the project is absent (id / name / path triple).
        let projectId: String
        let projectName: String
        let projectPath: String
        /// Resolved spawn cwd for the torn-off pane, carried from the
        /// SOURCE model at claim time so a deferred (nil-`entry`) pane
        /// spawns in the right directory in the destination window. Set
        /// for both the `.live` and `.notSpawned` claims; nil only as a
        /// defensive fallback. (Graft 0 — cwd-carrying claim.)
        let cwd: String?
        /// Screen-coordinate origin where the new window should appear,
        /// matching the drag release point so the window "pops out" at
        /// the cursor.
        let screenPoint: NSPoint
    }

    /// Pending tear-off seeds keyed by the UUID token minted for the
    /// window that will adopt them. `@ObservationIgnored` because
    /// `PendingTearOff` carries a live `TabPtySession.PaneEntry` (not a
    /// value type suitable for `@Observable` diffing).
    ///
    /// Token pairing replaces the old temporal FIFO: a window only
    /// consumes the seed deposited under ITS token (`WindowGroup(for:)`
    /// hands the token straight to the matching window), so a ⌘N /
    /// restore window opened concurrently — which carries no token —
    /// can never pop a seed that belonged to a tear-off.
    @ObservationIgnored
    private var pendingTearOffs: [String: PendingTearOff] = [:]

    /// Insertion order of `pendingTearOffs` keys, used for bounded
    /// eviction only. A deposited window that never opens (the tear-off
    /// `openWindow` failed, or the app quit before SwiftUI mounted the
    /// new window) would otherwise leak its seed forever; capping the
    /// outstanding count and evicting the oldest bounds that leak.
    @ObservationIgnored
    private var pendingTearOffOrder: [String] = []

    /// Largest number of un-consumed seeds we keep before evicting the
    /// oldest. In practice at most one tear-off is ever outstanding, so
    /// this only trips when seeds orphan (window never opened) — a small
    /// cap is plenty and keeps the orphan-seed leak bounded. (Static, so
    /// outside the `@Observable` instance machinery — no
    /// `@ObservationIgnored` needed.)
    private static let pendingTearOffCap = 8

    private static let tearOffSeedLog = Logger(
        subsystem: "dev.nickanderssohn.nice", category: "tearoff"
    )

    /// Enqueue a tear-off seed under `token`. Called by
    /// `PaneTearOffController` just before it triggers
    /// `openWindow(id: "main", value: token)`. The window SwiftUI opens
    /// for that token consumes it via `consumeTearOffSeed(token:)` from
    /// its `.task`. If the outstanding count exceeds the cap (an orphan
    /// leak — a deposited window never opened), the oldest token is
    /// evicted from both the map and the order list.
    func enqueueTearOff(_ seed: PendingTearOff, token: String) {
        pendingTearOffs[token] = seed
        // Production mints a fresh UUID per call so a token is never
        // re-deposited, but guard the invariant anyway: a duplicate order
        // entry would let one consume leave a stale key behind and could
        // mis-trip the cap-eviction count. Drop any prior occurrence so
        // the order list stays a faithful 1:1 with the map's keys.
        pendingTearOffOrder.removeAll { $0 == token }
        pendingTearOffOrder.append(token)
        if pendingTearOffOrder.count > Self.pendingTearOffCap {
            let oldest = pendingTearOffOrder.removeFirst()
            pendingTearOffs.removeValue(forKey: oldest)
            Self.tearOffSeedLog.warning(
                "evicted orphan tear-off seed for token \(oldest, privacy: .public): outstanding count exceeded cap \(Self.pendingTearOffCap, privacy: .public) (a deposited window never opened?)"
            )
        }
    }

    /// Remove and return the tear-off seed deposited under `token`, or
    /// nil when none was (a ⌘N / restore window with no token, or a
    /// fan-out window whose token had no seed). One-shot and MainActor-
    /// atomic. Called from `AppShellHost.task` in the new window so the
    /// seed is consumed exactly once, by the window it was paired to.
    func consumeTearOffSeed(token: String) -> PendingTearOff? {
        guard let seed = pendingTearOffs.removeValue(forKey: token) else {
            return nil
        }
        pendingTearOffOrder.removeAll { $0 == token }
        return seed
    }

    /// One-shot guard for the first-launch "Install the Nice Handoff
    /// skill?" prompt. `AppShellHost.task` calls this after the
    /// multi-window fan-out; only the first call (from the first-mounted
    /// window on the first launch after the prompt becomes eligible)
    /// returns true and triggers the alert. All later calls — sibling
    /// windows, future relaunches — return false so the prompt fires
    /// exactly once per process.
    @ObservationIgnored
    private var handoffSkillPromptFired = false

    /// Returns `true` on the FIRST call, `false` thereafter. MainActor
    /// isolation makes the read-modify-write atomic without a lock.
    func consumeHandoffSkillPromptSlot() -> Bool {
        guard !handoffSkillPromptFired else { return false }
        handoffSkillPromptFired = true
        return true
    }

    init() {
        self.tweaks = Tweaks()
        self.shortcuts = KeyboardShortcuts()
        self.fontSettings = FontSettings()
        self.fileBrowserSortSettings = FileBrowserSortSettings()
        let lifecycleController = SessionLifecycleController()
        self.lifecycleController = lifecycleController
        // Hand the registry the same controller instance so per-window
        // close routing and the willTerminate cascade resolve through
        // one object.
        self.registry = WindowRegistry(lifecycleController: lifecycleController)
        self.claimLedger = WindowClaimLedger()
        self.terminalThemeCatalog = TerminalThemeCatalog(
            supportDirectory: TerminalThemeCatalog.defaultSupportDirectory()
        )
        self.releaseChecker = ReleaseChecker()
        self.editorDetector = EditorDetector()
        self.livePaneRegistry = LivePaneRegistry()
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
            openWithProvider: OpenWithProvider(),
            registry: self.registry
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

        // Replace AppKit's window restoration with our own. With
        // `NSQuitAlwaysKeepsWindows = YES` (driven by macOS's "Close
        // windows when quitting an application" toggle in System
        // Settings → Desktop & Dock), AppKit auto-restores extra
        // windows in parallel with our fan-out spawn loop, double-
        // counting saved slots. We drive multi-window restore entirely
        // from `sessions.json` instead — see
        // `WindowSession.unclaimedSavedWindowCount` and the spawn
        // loop in `AppShellHost.task`.
        UserDefaults.standard.set(false, forKey: "NSQuitAlwaysKeepsWindows")

        // Sweep `$TMPDIR` debris from prior crashed runs: stale
        // `nice-*.sock` control sockets, plus legacy `nice-zdotdir-*`
        // dirs left by builds that predate moving the ZDOTDIR into
        // Application Support. Our own zdotdir no longer lives in
        // `$TMPDIR`, so this sweep can't race it.
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
        // Capture Nice's own inherited ZDOTDIR before any pty children
        // get our overridden value. Read straight from the process env
        // (not from a make() return) so this also works if make() threw:
        // even with no temp dir, a pty child still benefits from being
        // told the user's intended ZDOTDIR via NICE_USER_ZDOTDIR.
        self.userZDotDir = ProcessInfo.processInfo.environment["ZDOTDIR"]
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

        // Install or remove the `/nice-handoff` skill and its companion
        // shell helper depending on the user's preference. Idempotent:
        // both the install and uninstall paths are no-ops when the
        // on-disk state already matches the flag.
        SkillInstaller.sync(enabled: tweaks.installHandoffSkill)

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
                // Window-lifecycle work — detach close observers (so
                // SwiftUI's scene-teardown burst can't retroactively
                // re-enter `WindowRegistry.handleClose`) then tear
                // every registered window down with
                // `.appTerminating` — lives on the controller. See
                // `SessionLifecycleController.handleAppWillTerminate`
                // for the ordering rationale.
                let registry = self.registry
                self.lifecycleController.handleAppWillTerminate(
                    allAppStates: registry.allAppStates,
                    detachObservers: { registry.detachAllCloseObservers() }
                )
                // The ZDOTDIR now lives in a stable Application Support
                // location, shared across this variant's windows and
                // reused on the next launch — so it is deliberately not
                // removed here. Deleting it would also break a second
                // running instance of the same variant mid-session.
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
