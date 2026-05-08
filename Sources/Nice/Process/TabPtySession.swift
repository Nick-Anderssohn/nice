//
//  TabPtySession.swift
//  Nice
//
//  Each sidebar tab (session) owns a set of `Pane`s — claude or terminal.
//  Each pane backs a `LocalProcessTerminalView` stored in `entries[paneId]`.
//  Panes are spawned at session init (one claude pane for user-created
//  sessions + one terminal pane) and on demand via `addTerminalPane`.
//
//  Every pane installs its own `ProcessTerminationDelegate` with a
//  `.pane(tabId:, paneId:)` role so `AppState` can fan exit and title
//  callbacks to the right pane. Sessions are retained by `AppState`, so
//  the underlying processes persist across tab switches and SwiftUI
//  redraws.
//

import AppKit
import SwiftTerm
import SwiftUI

/// Theme-fan-out surface that `SessionsModel` invokes for every live
/// session when `updateScheme` / `updateTerminalFontSize` /
/// `updateTerminalTheme` / `updateTerminalFontFamily` fires. Extracted
/// as a protocol so unit tests can substitute a `FakeTabPtySession`
/// recording the calls without standing up real ptys —
/// `SessionsModel`'s `ptySessions` itself stays typed as
/// `TabPtySession` (real production sessions need the full subprocess
/// surface), and tests reach the fan-out through a separate test-only
/// receivers list on `SessionsModel`.
@MainActor
protocol TabPtySessionThemeable: AnyObject {
    func applyTheme(_ scheme: ColorScheme, palette: Palette, accent: NSColor)
    func applyTerminalFont(size: CGFloat)
    func applyTerminalTheme(_ theme: TerminalTheme)
    func applyTerminalFontFamily(_ name: String?)
}

@MainActor
@Observable
final class TabPtySession: TabPtySessionThemeable {
    let tabId: String
    let cwd: String

    /// All panes (claude + terminal) keyed by pane id. Each entry
    /// bundles the SwiftTerm view, its kind, the retained termination
    /// delegate, and per-pane lifecycle flags (held, intentional-kill).
    /// Single dict so adding any future per-pane field doesn't reopen
    /// the parallel-dicts coherence problem — `removePane` is the one
    /// place an entry exits the system, and an `entries.removeValue`
    /// there is enough to clear *every* per-pane bit at once.
    ///
    /// Entries outlive the underlying child process — when a pane
    /// exits non-cleanly we keep its view mounted so the user can
    /// read whatever the process printed on the way out (see
    /// `handlePaneExit`). The view's terminal buffer is owned by
    /// SwiftTerm, not the pty, so keeping the view in the dict
    /// preserves the scrollback even though the child is gone.
    private(set) var entries: [String: PaneEntry] = [:]

    /// Per-pane state. Bundles every field that's keyed by pane id so
    /// the lifecycle invariants (everything appears at spawn,
    /// everything disappears at `removePane`) are enforceable from
    /// one place.
    struct PaneEntry {
        let view: NiceTerminalView
        let kind: PaneKind
        /// Strong-ref retain for SwiftTerm's weak `processDelegate`.
        let delegate: ProcessTerminationDelegate
        /// True once the pane's child process has exited and we've
        /// decided to keep the view mounted so the user can read the
        /// final output. A later user-driven dismiss (`terminatePane`)
        /// or session teardown (`terminateAll`) reads `heldExitCode`
        /// and synthesizes the deferred `onPaneExit` so the upper
        /// layer's dissolve cascade fires through its normal path.
        var isHeld: Bool = false
        /// Saved exit code for a held pane; ignored when `isHeld` is
        /// false. `nil` means "killed by signal, no waitstatus" — the
        /// same semantics SwiftTerm reports.
        var heldExitCode: Int32? = nil
        /// True iff Nice itself initiated the kill of this pane
        /// (`terminatePane`, `terminateAll`). Set BEFORE SIGHUP — and
        /// before any `pid > 0` guard — so that the resulting `onExit`
        /// delegate, regardless of exit code, drops through
        /// `handlePaneExit` straight to `onPaneExit` instead of being
        /// held with a "[Process exited]" footer the user explicitly
        /// asked to dismiss. The flag is consumed (cleared) on read in
        /// `handlePaneExit` so a future re-entry on the same id starts
        /// from a clean state.
        var intentionalTerminate: Bool = false
    }

    private let onPaneExit: @MainActor (String, Int32?) -> Void
    private let onPaneTitleChange: @MainActor (String, String) -> Void
    /// Called when a pane's shell emits OSC 7 with a new working
    /// directory (via the injected zsh `chpwd_functions` hook). Second
    /// argument is the absolute path. Optional so tests/previews can
    /// instantiate without wiring it up.
    private let onPaneCwdChange: (@MainActor (String, String) -> Void)?
    /// Called when a new pane is spawned. Second argument is the
    /// display-friendly command (e.g. `claude -w foo`) that the overlay
    /// should show if the pane stays silent past the grace window.
    /// `nil` callback leaves the overlay wiring disabled entirely — kept
    /// optional so tests / previews can instantiate `TabPtySession`
    /// without bothering with the placeholder lifecycle.
    private let onPaneLaunched: (@MainActor (String, String) -> Void)?
    /// Called the first time the pane's child process writes any byte
    /// to the pty. AppState uses this to clear the "Launching…" overlay
    /// for that pane.
    private let onPaneFirstOutput: (@MainActor (String) -> Void)?
    /// Called when a pane's process has exited but Nice has chosen to
    /// keep the view mounted so the user can read its final output. The
    /// upper layer flips `Pane.isAlive = false` and dismisses the
    /// launch overlay; the pane stays in the tab's `panes` array (and
    /// in `self.entries`) until a later dismiss path tears it down.
    /// `nil` callback degrades the feature to "drop on exit as before"
    /// — kept optional so tests / previews can construct a session
    /// without wiring it.
    private let onPaneHeld: (@MainActor (String, Int32?) -> Void)?

    /// Cached SwiftUI `ColorScheme` so panes added after init can be
    /// themed at creation without round-tripping through `AppState`.
    private var currentScheme: ColorScheme = .dark

    /// Cached active `Palette` (nice | macOS). Defaults to `.nice` so
    /// panes spawned before the first theme update fall back to the
    /// custom literals rather than accidentally rendering against
    /// `NSColor` semantic colors that haven't been appearance-resolved.
    private var currentPalette: Palette = .nice

    /// Cached active accent as an `NSColor`, used to paint the caret so
    /// the blinking cursor matches the app's tint. Seeded with the
    /// terracotta fallback; `applyTheme` overwrites it on every call.
    private var currentAccent: NSColor = AccentPreset.terracotta.nsColor

    /// Cached terminal font size so panes spawned after the first
    /// `applyTerminalFont` pick it up at construction. Seeded with
    /// the Xcode-matched 13pt terminal default.
    private var currentTerminalFontSize: CGFloat = FontSettings.defaultTerminalSize

    /// Cached terminal font family (PostScript name). `nil` means "use
    /// the default chain" (SF Mono → JetBrains Mono NL → system
    /// monospaced), matching `Tweaks.terminalFontFamily`'s default.
    private var currentTerminalFontFamily: String? = nil

    /// Cached terminal theme. Seeded with the Nice dark default so
    /// panes created before `applyTerminalTheme` has been called still
    /// get reasonable colors — `AppState.makeSession` seeds with the
    /// actual current theme immediately, so this only matters if
    /// something creates a `TabPtySession` directly (tests, previews).
    private var currentTerminalTheme: TerminalTheme = BuiltInTerminalThemes.niceDefaultDark

    /// Unix-domain-socket path injected into panes as `NICE_SOCKET`.
    private let socketPath: String?
    /// ZDOTDIR directory injected into terminal panes so the shadowed
    /// `claude()` function is available inside them.
    private let zdotdirPath: String?
    /// `ZDOTDIR` Nice inherited from its launch env (or nil). Forwarded
    /// to ptys as `NICE_USER_ZDOTDIR` so the synthetic .zshenv can
    /// restore it after our injection runs — see
    /// `MainTerminalShellInject` for the full handshake.
    private let userZDotDir: String?

    /// Captured for the optional initial claude pane spawn.
    private let claudeBinary: String?
    private let extraClaudeArgs: [String]
    private let claudeSessionMode: ClaudeSessionMode

    /// How to attach this tab to the Claude CLI's session layer.
    enum ClaudeSessionMode: Sendable, Equatable {
        /// No session id; the CLI picks one. Kept for previews/tests
        /// and any pre-feature call site that hasn't been updated.
        case none
        /// Fresh session under a caller-provided UUID. Emits
        /// `--session-id <uuid>`; `extraClaudeArgs` are appended.
        case new(id: String)
        /// Resume a prior session by UUID. Emits `--resume <uuid>`;
        /// `extraClaudeArgs` are ignored (the transcript already
        /// carries the session's flags).
        case resume(id: String)
        /// Restore path: don't run claude. Spawn a plain `zsh -il` with
        /// `claude --resume <uuid>` pre-typed at the prompt via
        /// `print -z`. User hits Enter to actually resume — avoids the
        /// auto-resume token cost for sessions they weren't going to
        /// reopen anyway.
        case resumeDeferred(id: String)
    }

    init(
        tabId: String,
        cwd: String,
        claudeBinary: String?,
        extraClaudeArgs: [String] = [],
        initialClaudePaneId: String? = nil,
        initialTerminalPaneId: String? = nil,
        socketPath: String? = nil,
        zdotdirPath: String? = nil,
        userZDotDir: String? = nil,
        claudeSessionMode: ClaudeSessionMode = .none,
        onPaneExit: @escaping @MainActor (String, Int32?) -> Void,
        onPaneTitleChange: @escaping @MainActor (String, String) -> Void,
        onPaneCwdChange: (@MainActor (String, String) -> Void)? = nil,
        onPaneLaunched: (@MainActor (String, String) -> Void)? = nil,
        onPaneFirstOutput: (@MainActor (String) -> Void)? = nil,
        onPaneHeld: (@MainActor (String, Int32?) -> Void)? = nil
    ) {
        self.tabId = tabId
        self.cwd = cwd
        self.onPaneExit = onPaneExit
        self.onPaneTitleChange = onPaneTitleChange
        self.onPaneCwdChange = onPaneCwdChange
        self.onPaneLaunched = onPaneLaunched
        self.onPaneFirstOutput = onPaneFirstOutput
        self.onPaneHeld = onPaneHeld
        self.socketPath = socketPath
        self.zdotdirPath = zdotdirPath
        self.userZDotDir = userZDotDir
        self.claudeBinary = claudeBinary
        self.extraClaudeArgs = extraClaudeArgs
        self.claudeSessionMode = claudeSessionMode

        if let claudeId = initialClaudePaneId {
            _ = spawnClaudePane(id: claudeId, cwd: cwd)
        }
        if let termId = initialTerminalPaneId {
            _ = addTerminalPane(id: termId, cwd: cwd)
        }
    }

    // MARK: - Pane lookup (read-only)

    /// SwiftTerm view for a hosted pane, or nil if the pane has never
    /// been spawned (lazy companion terminal) or has been torn down.
    /// Returns the view for both live and held panes — held panes are
    /// "still hosted," just with a dead pty.
    func view(forPane id: String) -> NiceTerminalView? {
        entries[id]?.view
    }

    /// True iff this session currently owns an entry for `id` —
    /// includes both live panes and held-but-dead panes (whose view is
    /// kept around so the user can read the exit output). False for
    /// unspawned panes (the lazy companion terminal that has never
    /// been focused).
    func hasPane(_ id: String) -> Bool {
        entries[id] != nil
    }

    // MARK: - Pane spawn

    /// Spawn a claude-kind pane. Runs claude inside a login+interactive
    /// zsh via `zsh -ilc "exec claude ..."` so zshrc/zprofile (PATH,
    /// nvm, etc.) are sourced before the exec. When `claudeBinary` is
    /// nil, falls back to a plain zsh — the pane still renders as a
    /// claude pane at the model layer, but what's actually running
    /// inside is just a shell.
    ///
    /// The `.resumeDeferred(id:)` mode is special: it spawns a plain
    /// shell (no `exec claude ...`) with `NICE_PREFILL_COMMAND` set so
    /// the injected zshrc pre-types `claude --resume <uuid>` at the
    /// prompt. Nothing runs until the user hits Enter.
    @discardableResult
    private func spawnClaudePane(id: String, cwd: String) -> LocalProcessTerminalView {
        let view = NiceTerminalView(frame: .zero)
        view.font = Self.terminalFont(named: currentTerminalFontFamily, size: currentTerminalFontSize)
        let delegate = makePaneDelegate(paneId: id)
        view.processDelegate = delegate
        entries[id] = PaneEntry(view: view, kind: .claude, delegate: delegate)
        installLaunchOverlayHooks(on: view, paneId: id, kind: .claude)

        let resolvedCwd = Self.expandTilde(cwd)
        let isOverride = ProcessInfo.processInfo.environment["NICE_CLAUDE_OVERRIDE"] != nil
        // Claude Code gates OSC title emission on TERM_PROGRAM ∈
        // {"iTerm.app","ghostty","WezTerm","Apple_Terminal"}. Advertise
        // as ghostty so the in-app session label updates automatically
        // — SwiftTerm handles the resulting OSC 0/1/2 sequences natively
        // and the choice doesn't trigger iTerm-specific OSC extensions.
        let claudeExtraEnv = Self.buildClaudeExtraEnv(
            mode: claudeSessionMode,
            tabId: tabId,
            paneId: id,
            socketPath: socketPath,
            zdotdirPath: zdotdirPath,
            userZDotDir: userZDotDir
        )

        if case .resumeDeferred = claudeSessionMode {
            // Pane renders as Claude but the pty is a plain shell with
            // the resume command pre-typed. The socket handshake will
            // flip the pane to actually running-Claude when the user
            // hits Enter and the wrapper promotes this pane in place.
            view.armDeferredSpawn(
                executable: "/bin/zsh",
                args: ["-il"],
                environment: Self.buildEnv(extraEnv: claudeExtraEnv),
                execName: nil,
                currentDirectory: resolvedCwd
            )
        } else if let claude = claudeBinary {
            let command = Self.buildClaudeExecCommand(
                claude: claude,
                mode: claudeSessionMode,
                extraClaudeArgs: extraClaudeArgs,
                isOverride: isOverride
            )
            view.armDeferredSpawn(
                executable: "/bin/zsh",
                args: ["-ilc", command],
                environment: Self.buildEnv(extraEnv: claudeExtraEnv),
                execName: nil,
                currentDirectory: resolvedCwd
            )
        } else {
            view.armDeferredSpawn(
                executable: "/bin/zsh",
                args: ["-il"],
                environment: nil,
                execName: nil,
                currentDirectory: resolvedCwd
            )
        }

        applyTerminalTheme(currentTerminalTheme, to: view)
        return view
    }

    /// Spawn a terminal-kind pane — a plain `zsh -il` with injected
    /// `ZDOTDIR` + `NICE_SOCKET` so the shadowed `claude()` function
    /// is available inside.
    ///
    /// When `command` is non-nil, the spawn replaces zsh with the
    /// supplied command via `zsh -ilc "exec <command>"`. The login
    /// shell still runs the user's rc files first so PATH (Homebrew,
    /// asdf, mise, …) is the same as a regular terminal pane. On
    /// editor exit, the pty closes and the pane drops via the
    /// existing `paneExited` flow — same UX as a Claude pane.
    @discardableResult
    func addTerminalPane(
        id: String,
        cwd: String? = nil,
        socketPath: String? = nil,
        zdotdirPath: String? = nil,
        userZDotDir: String? = nil,
        command: String? = nil
    ) -> LocalProcessTerminalView {
        let view = NiceTerminalView(frame: .zero)
        view.font = Self.terminalFont(named: currentTerminalFontFamily, size: currentTerminalFontSize)
        let delegate = makePaneDelegate(paneId: id)
        view.processDelegate = delegate
        entries[id] = PaneEntry(view: view, kind: .terminal, delegate: delegate)
        installLaunchOverlayHooks(
            on: view, paneId: id, kind: .terminal, displayCommand: command
        )

        var extraEnv: [String: String] = [:]
        if let sp = socketPath ?? self.socketPath {
            extraEnv["NICE_SOCKET"] = sp
        }
        if let zp = zdotdirPath ?? self.zdotdirPath {
            extraEnv["ZDOTDIR"] = zp
        }
        // Always set NICE_USER_ZDOTDIR (empty when Nice didn't inherit
        // one) so the synthetic .zshenv's check is unambiguous —
        // empty string means "fall back to sourcing ~/.zshenv".
        extraEnv["NICE_USER_ZDOTDIR"] = userZDotDir ?? self.userZDotDir ?? ""
        // Tab/pane identity for the zsh `claude()` wrapper's handshake.
        // The wrapper includes these in its socket payload so Nice can
        // decide whether to open a new sidebar tab or promote this pane.
        extraEnv["NICE_TAB_ID"] = tabId
        extraEnv["NICE_PANE_ID"] = id

        let resolvedCwd = Self.expandTilde(cwd ?? self.cwd)
        let args = Self.buildExecArgs(command: command)
        view.armDeferredSpawn(
            executable: "/bin/zsh",
            args: args,
            environment: Self.buildEnv(extraEnv: extraEnv),
            execName: nil,
            currentDirectory: resolvedCwd
        )

        applyTerminalTheme(currentTerminalTheme, to: view)
        return view
    }

    /// Build a `.pane` role delegate that routes exit + title change
    /// back to `AppState`. The exit branch goes through
    /// `handlePaneExit` (not directly to `onPaneExit`) so the hold-vs-
    /// drop decision can intercept non-zero / unexpected exits before
    /// the upper layer dissolves the pane and destroys its scrollback.
    private func makePaneDelegate(paneId: String) -> ProcessTerminationDelegate {
        let onTitleChange = self.onPaneTitleChange
        let onCwdChange = self.onPaneCwdChange
        let cwdForwarder: (@MainActor (ProcessTerminationDelegate.Role, String) -> Void)?
        if let onCwdChange {
            cwdForwarder = { role, cwd in
                if case let .pane(_, paneId) = role {
                    onCwdChange(paneId, cwd)
                }
            }
        } else {
            cwdForwarder = nil
        }
        return ProcessTerminationDelegate(
            role: .pane(tabId: tabId, paneId: paneId),
            onExit: { [weak self] role, code in
                if case let .pane(_, paneId) = role {
                    self?.handlePaneExit(paneId: paneId, exitCode: code)
                }
            },
            onTitleChange: { [onTitleChange] role, title in
                if case let .pane(_, paneId) = role {
                    onTitleChange(paneId, title)
                }
            },
            onCwdChange: cwdForwarder
        )
    }

    /// Decide whether a freshly-exited pane drops immediately or holds
    /// its view (and scrollback) so the user can read what the process
    /// printed before dying. Holding writes a dim footer line into the
    /// SwiftTerm buffer and routes the `onPaneHeld` callback so the
    /// model can flip `Pane.isAlive = false`; the actual model removal
    /// is deferred until the user closes the tab (which calls back into
    /// `terminatePane`, which then synthesizes the deferred
    /// `onPaneExit`).
    ///
    /// The hold path requires the entry (so we know the kind for the
    /// footer label and have the view to feed). A missing entry is
    /// defensive — a freshly-instantiated session that somehow exited
    /// before its spawn methods finished — and falls back to a drop
    /// so the model never gets stuck on a half-initialized pane.
    private func handlePaneExit(paneId: String, exitCode: Int32?) {
        guard var entry = entries[paneId] else {
            onPaneExit(paneId, exitCode)
            return
        }
        // Consume the intentional-kill flag whether we hold or drop —
        // the next exit on this id (impossible today, but cheap) must
        // start from a clean state.
        let intentional = entry.intentionalTerminate
        entry.intentionalTerminate = false

        let shouldHold = Self.shouldHoldOnExit(
            exitCode: exitCode, intentionallyTerminated: intentional
        )
        guard shouldHold, onPaneHeld != nil else {
            // Drop. Persist the cleared `intentionalTerminate` flag
            // for the (vanishingly small) window before SessionsModel
            // calls `removePane` and the entry leaves entirely.
            entries[paneId] = entry
            onPaneExit(paneId, exitCode)
            return
        }
        let footer = Self.paneExitFooter(kind: entry.kind, exitCode: exitCode)
        entry.view.getTerminal().feed(text: footer)
        entry.isHeld = true
        entry.heldExitCode = exitCode
        entries[paneId] = entry
        onPaneHeld?(paneId, exitCode)
    }

    /// Pure: should a pane that just emitted an exit be held open
    /// (true) or dropped immediately (false)?
    ///
    /// Hold any non-zero or signal-truncated exit that wasn't initiated
    /// by Nice itself; drop everything else. The asymmetry matches user
    /// intent: clean exits (`exit 0`, `/exit` from claude, `vim` saved
    /// + quit) are deliberate and the user wants the pane gone, while
    /// non-zero exits are usually error conditions whose output the
    /// user needs to read. `intentionallyTerminated == true` short-
    /// circuits the hold so Cmd+W on a still-busy tab doesn't leave a
    /// "[Process killed by signal]" footer behind.
    nonisolated static func shouldHoldOnExit(
        exitCode: Int32?,
        intentionallyTerminated: Bool
    ) -> Bool {
        if intentionallyTerminated { return false }
        guard let code = exitCode else {
            // nil = no waitstatus (signal). Could be the OS, the user's
            // own `kill <pid>` from another shell, or a parent process
            // group hangup — Nice didn't ask for it via the UI, so hold
            // and surface whatever the process printed last.
            return true
        }
        return code != 0
    }

    /// Pure: render the dim footer line written into a held pane's
    /// SwiftTerm buffer announcing the exit. Format intentionally
    /// minimal — iTerm-style — because we can't intercept keystrokes
    /// from the dead pty so there's no "press a key to dismiss"
    /// affordance to advertise; the user closes the tab to dismiss.
    ///
    /// ANSI: `\r\n` (visual gap from the last byte the process wrote)
    /// + `ESC[2m` (dim) + the bracketed footer + `ESC[0m` (reset) +
    /// `\r\n`. Carriage returns matter because the cursor's column
    /// when the process died is unknown — `\r` snaps it to column 0
    /// before the footer prints, otherwise a process that exited
    /// mid-line would render the footer indented.
    nonisolated static func paneExitFooter(
        kind: PaneKind, exitCode: Int32?
    ) -> String {
        let label = kind == .claude ? "claude" : "Process"
        let codeStr: String
        if let code = exitCode {
            codeStr = "status \(code)"
        } else {
            codeStr = "killed by signal"
        }
        return "\r\n\u{1b}[2m[\(label) exited (\(codeStr))]\u{1b}[0m\r\n"
    }

    /// Drop the pane's entry — view, delegate, kind, held flag, and
    /// intentional-kill flag, all of which live on `PaneEntry`. Does
    /// NOT terminate the underlying process; callers invoke this from
    /// the pane's exit hook, by which time the process is already
    /// gone. Single removal point per the consolidation invariant: a
    /// recycled pane id (none today, but cheap insurance) starts
    /// from a clean slate because everything left the dict at once.
    func removePane(id: String) {
        entries.removeValue(forKey: id)
    }

    /// Wire up the "Launching…" placeholder for a newly-spawned pane.
    /// Calls `onPaneLaunched` so AppState starts the grace timer and
    /// sets `view.onFirstData` so the overlay lifts on first pty byte.
    /// The `.resumeDeferred` path is suppressed for claude panes because
    /// that pane is really a quiescent shell with a pre-typed command —
    /// no child is "launching", just waiting for the user to hit Enter.
    private func installLaunchOverlayHooks(
        on view: NiceTerminalView,
        paneId: String,
        kind: PaneKind,
        displayCommand: String? = nil
    ) {
        if kind == .claude, case .resumeDeferred = claudeSessionMode {
            return
        }
        // When the caller supplied an explicit command (e.g. an editor
        // pane spawned by the File Explorer), show that instead of
        // generic "zsh".
        let command = displayCommand ?? launchDisplayCommand(kind: kind)
        onPaneLaunched?(paneId, command)
        let handler = onPaneFirstOutput
        view.onFirstData = { [handler, paneId] in
            handler?(paneId)
        }
    }

    /// Produce the human-readable command string shown in the overlay.
    /// Deliberately skips the `zsh -ilc "exec …"` wrapper and the
    /// `--session-id <uuid>` plumbing Nice injects — the point is to
    /// show the user what *they* asked for, not the shell arithmetic.
    private func launchDisplayCommand(kind: PaneKind) -> String {
        switch kind {
        case .claude:
            switch claudeSessionMode {
            case .resume:
                return "claude --resume"
            case .none, .new, .resumeDeferred:
                // .resumeDeferred is excluded by the caller; the case is
                // listed so the switch stays exhaustive.
                if extraClaudeArgs.isEmpty {
                    return "claude"
                }
                return (["claude"] + extraClaudeArgs).joined(separator: " ")
            }
        case .terminal:
            return "zsh"
        }
    }

    /// Force the pane's child process to exit. Sends SIGHUP first
    /// (the traditional "your tty is gone" signal that interactive
    /// shells like zsh handle by exiting cleanly); after a short
    /// grace, follows up with SIGKILL for anything that ignored it
    /// (e.g. a script catching SIGHUP). Uses `kill(2)` directly rather
    /// than `LocalProcess.terminate()` because SwiftTerm's helper
    /// cancels its own child-exit monitor before the child actually
    /// dies, which swallows the delegate notification we rely on for
    /// model cleanup. `kill` alone lets the monitor observe the real
    /// exit and drive `paneExited` as usual.
    ///
    /// Held-pane fast path: if the pane is already being held open
    /// (its child exited earlier, view is still mounted so the user
    /// can read the output), the pty is gone and `kill` would no-op.
    /// Synthesize the deferred `onPaneExit` instead so the upper
    /// layer's tab-dissolve cascade fires through its normal path.
    /// Marking the pane as intentionally-terminated before the SIGHUP
    /// — and crucially before any `pid > 0` guard — also ensures
    /// that if SIGHUP causes the child to exit non-zero (interactive
    /// shells often do), `handlePaneExit` drops it rather than
    /// holding a "[Process exited]" footer the user never asked to
    /// see. The flag must be set before the pid guard so a pane
    /// whose child never had a real pid (e.g. a spawn that failed
    /// before allocating one) still drops cleanly if its delegate
    /// later fires.
    func terminatePane(id: String) {
        guard var entry = entries[id] else { return }

        if entry.isHeld {
            let code = entry.heldExitCode
            entry.isHeld = false
            entry.heldExitCode = nil
            entries[id] = entry
            onPaneExit(id, code)
            return
        }

        entry.intentionalTerminate = true
        entries[id] = entry

        let pid = entry.view.process.shellPid
        guard pid > 0 else { return }
        kill(pid, SIGHUP)
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
            // If the child is still alive half a second later, be blunt.
            // `kill(pid, 0)` probes liveness without sending a signal —
            // returns 0 iff we can signal it (i.e. it exists and is ours).
            if kill(pid, 0) == 0 {
                kill(pid, SIGKILL)
            }
        }
    }

    /// Whether the shell inside `id` currently has a foreground child —
    /// i.e. the user has something running at an interactive prompt.
    /// Compares the pty's foreground process group to the shell's own
    /// pgrp; they differ only when the shell has `fork+setpgrp+tcsetpgrp`'d
    /// a subprocess. Returns `false` if the pane isn't hosted, the pty
    /// isn't alive, or the query fails — callers treat that as "idle".
    func shellHasForegroundChild(id: String) -> Bool {
        guard let entry = entries[id] else { return false }
        let fd = entry.view.process.childfd
        let pid = entry.view.process.shellPid
        guard fd >= 0, pid > 0 else { return false }
        let fgPgrp = tcgetpgrp(fd)
        guard fgPgrp > 0 else { return false }
        let shellPgrp = getpgid(pid)
        guard shellPgrp > 0 else { return false }
        return fgPgrp != shellPgrp
    }

    // MARK: - IO

    /// Send `text` plus a newline into the specified pane's pty.
    /// No-op if `paneId` isn't currently hosted in this session.
    func sendToPane(_ text: String, paneId: String) {
        guard let entry = entries[paneId] else { return }
        let data = Array((text + "\n").utf8)
        entry.view.send(data: ArraySlice(data))
    }

    // MARK: - Theming

    /// Caches the chrome scheme / palette / accent. Every terminal
    /// theme (Nice Defaults included) is self-contained and carries
    /// its own concrete bg / fg / ANSI, so chrome changes no longer
    /// re-paint terminal colors. The only chrome-driven bit left is
    /// the caret — for themes that leave `cursor = nil` it follows
    /// the accent, and this method re-applies it so accent changes
    /// hot-update live.
    func applyTheme(_ scheme: ColorScheme, palette: Palette, accent: NSColor) {
        currentScheme = scheme
        currentPalette = palette
        currentAccent = accent
        if currentTerminalTheme.cursor == nil {
            for entry in entries.values {
                entry.view.caretColor = accent
            }
        }
    }

    /// Repaint every live pane with `theme`. Each theme is self-
    /// contained: bg / fg / ANSI / selection come straight from
    /// `theme`, and the caret uses `theme.cursor` when set or falls
    /// back to the current accent otherwise.
    func applyTerminalTheme(_ theme: TerminalTheme) {
        currentTerminalTheme = theme
        for entry in entries.values {
            applyTerminalTheme(theme, to: entry.view)
        }
    }

    private func applyTerminalTheme(
        _ theme: TerminalTheme,
        to view: LocalProcessTerminalView
    ) {
        view.nativeBackgroundColor = theme.background.nsColor
        view.nativeForegroundColor = theme.foreground.nsColor
        view.installColors(theme.ansi.map(\.swiftTermColor))
        view.caretColor = theme.cursor?.nsColor ?? currentAccent
        view.selectedTextBackgroundColor = theme.selection?.nsColor
            ?? NSColor.selectedTextBackgroundColor
    }

    /// Rebuild the font for every live pane with the new family,
    /// preserving the current size. `nil` resets to the default chain.
    func applyTerminalFontFamily(_ name: String?) {
        currentTerminalFontFamily = name
        let font = Self.terminalFont(named: name, size: currentTerminalFontSize)
        for entry in entries.values {
            entry.view.font = font
        }
    }

    /// Terminate every live pane's process. Used when a tab is being
    /// closed while its panes are still running (e.g. the user closed
    /// the last tab; model-driven teardown). Pane-exit callbacks still
    /// fire and drive cleanup through the normal path.
    ///
    /// Held panes have an already-dead pty, so `process.terminate()`
    /// would no-op and the upper-layer dissolve would never fire.
    /// Snapshot the held entries up front and synthesize their
    /// `onPaneExit` calls — `onPaneExit` re-enters via `removePane`,
    /// which mutates `entries` mid-loop, so we capture (id, code)
    /// pairs before iterating. Live panes are flagged intentional
    /// first so the SIGTERM-induced exit codes don't trigger a hold
    /// on the way out.
    func terminateAll() {
        let heldSnapshot: [(id: String, exitCode: Int32?)] = entries.compactMap {
            id, entry in
            entry.isHeld ? (id, entry.heldExitCode) : nil
        }
        for id in entries.keys {
            entries[id]?.intentionalTerminate = true
            entries[id]?.isHeld = false
            entries[id]?.heldExitCode = nil
        }
        for held in heldSnapshot {
            onPaneExit(held.id, held.exitCode)
        }
        for entry in entries.values {
            entry.view.process.terminate()
        }
    }

    /// Merge `extraEnv` on top of SwiftTerm's default forwarded env
    /// (TERM, COLORTERM, LANG, LOGNAME, USER, HOME).
    private static func buildEnv(extraEnv: [String: String]) -> [String] {
        var env = SwiftTerm.Terminal.getEnvironmentVariables()
        for (k, v) in extraEnv {
            env.append("\(k)=\(v)")
        }
        return env
    }

    // MARK: - Helpers

    /// Assemble the `exec <claude> ...` command line for the inner
    /// `zsh -ilc` invocation. Pure — factored out so unit tests can
    /// lock down the flag ordering contract without spawning a pty.
    ///
    /// Flag-order rule: `--session-id` / `--resume` and their UUID
    /// arguments must precede `extraClaudeArgs` so the UUID isn't
    /// consumed as the value of the trailing flag.
    ///
    /// `isOverride == true` (set when `NICE_CLAUDE_OVERRIDE` is in the
    /// environment) suppresses every Nice-injected flag — the caller
    /// is responsible for the full argv via their override wrapper.
    /// `.resumeDeferred` is handled outside this helper (it spawns a
    /// plain shell, not `exec claude`) and passing it here returns
    /// just `exec <claude>` defensively.
    ///
    /// `nonisolated` because the function is pure — it touches no
    /// instance or static state — so tests can call it from their
    /// default nonisolated `XCTestCase.test*` methods without hopping
    /// onto the main actor.
    /// Build the extra-env dictionary for a Claude pane. Pure helper so
    /// per-mode contracts (every mode injects `NICE_SOCKET` so the
    /// SessionStart hook can reach Nice; only `.resumeDeferred`
    /// needs `ZDOTDIR` + `NICE_PREFILL_COMMAND` for the wrapper-driven
    /// pre-typed `claude --resume <uuid>`) are checkable from tests
    /// without spinning up a pty. `nonisolated` for the same reason
    /// as `buildClaudeExecCommand`.
    nonisolated static func buildClaudeExtraEnv(
        mode: ClaudeSessionMode,
        tabId: String,
        paneId: String,
        socketPath: String?,
        zdotdirPath: String?,
        userZDotDir: String?
    ) -> [String: String] {
        var env: [String: String] = ["TERM_PROGRAM": "ghostty"]
        env["NICE_TAB_ID"] = tabId
        env["NICE_PANE_ID"] = paneId
        if let sp = socketPath { env["NICE_SOCKET"] = sp }
        if case .resumeDeferred(let sessionId) = mode {
            if let zp = zdotdirPath { env["ZDOTDIR"] = zp }
            // Pair NICE_USER_ZDOTDIR with ZDOTDIR — the .zshenv stub
            // depends on it to resolve the user's intended layout
            // before our injection unwinds.
            env["NICE_USER_ZDOTDIR"] = userZDotDir ?? ""
            env["NICE_PREFILL_COMMAND"] = "claude --resume \(sessionId)"
        }
        return env
    }

    /// Pure projection of an optional editor/command override to the
    /// arg list passed to `/bin/zsh`. `nil` means "spawn a plain
    /// login-interactive shell" (`-il`); a command means "run that
    /// command in place of the shell" (`-ilc "exec <cmd>"`). The
    /// `exec` form is important: it replaces the login shell with the
    /// editor process so quitting the editor closes the pty (matching
    /// Claude-pane lifecycle), and signals/resize events forward to
    /// the editor instead of being eaten by an intermediate shell.
    nonisolated static func buildExecArgs(command: String?) -> [String] {
        if let command {
            return ["-ilc", "exec \(command)"]
        }
        return ["-il"]
    }

    nonisolated static func buildClaudeExecCommand(
        claude: String,
        mode: ClaudeSessionMode,
        extraClaudeArgs: [String],
        isOverride: Bool
    ) -> String {
        var parts = ["exec", shellSingleQuote(claude)]
        if !isOverride {
            switch mode {
            case .none:
                parts.append(contentsOf: extraClaudeArgs.map(shellSingleQuote))
            case .new(let id):
                parts.append("--session-id")
                parts.append(shellSingleQuote(id))
                parts.append(contentsOf: extraClaudeArgs.map(shellSingleQuote))
            case .resume(let id):
                parts.append("--resume")
                parts.append(shellSingleQuote(id))
            case .resumeDeferred:
                break
            }
        }
        return parts.joined(separator: " ")
    }

    static func terminalFont(named name: String?, size: CGFloat) -> NSFont {
        if let name, let font = NSFont(name: name, size: size) { return font }
        return NSFont(name: "SFMono-Regular", size: size)
            ?? NSFont(name: "JetBrainsMonoNL-Regular", size: size)
            ?? NSFont.monospacedSystemFont(ofSize: size, weight: .regular)
    }

    /// Re-apply the terminal font to every live pane on this session.
    /// Called by `AppState.updateTerminalFontSize` when the user drags
    /// the Font-pane slider or presses Cmd+/-.
    /// SwiftTerm's `LocalProcessTerminalView.font` setter rebuilds its
    /// internal `FontSet`, recomputes cell dimensions, and resizes the
    /// pty — the terminal reflows automatically.
    func applyTerminalFont(size: CGFloat) {
        currentTerminalFontSize = size
        let font = Self.terminalFont(named: currentTerminalFontFamily, size: size)
        for entry in entries.values {
            entry.view.font = font
        }
    }

    /// Expand a leading `~` to `$HOME` so `startProcess`'s working
    /// directory argument resolves cleanly. Paths without `~` pass
    /// through unchanged.
    private static func expandTilde(_ path: String) -> String {
        if path == "~" { return NSHomeDirectory() }
        if path.hasPrefix("~/") {
            return NSHomeDirectory() + path.dropFirst(1)
        }
        return path
    }
}
