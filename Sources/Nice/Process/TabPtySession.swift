//
//  TabPtySession.swift
//  Nice
//
//  Each sidebar tab (session) owns a set of `Pane`s — claude or terminal.
//  Each pane backs a `LocalProcessTerminalView` stored in `panes[paneId]`.
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

@MainActor
final class TabPtySession: ObservableObject {
    let tabId: String
    let cwd: String

    /// All live panes (claude + terminal) keyed by pane id.
    /// Typed as `NiceTerminalView` (subclass of `LocalProcessTerminalView`)
    /// so `applyGpuRendering` can call subclass-only helpers; the values
    /// still pass anywhere the supertype is expected (e.g. `TerminalHost`).
    var panes: [String: NiceTerminalView] = [:]
    /// Retains the per-view termination delegates so SwiftTerm's weak
    /// `processDelegate` reference stays live.
    private var delegates: [String: ProcessTerminationDelegate] = [:]

    private let onPaneExit: @MainActor (String, Int32?) -> Void
    private let onPaneTitleChange: @MainActor (String, String) -> Void

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
    /// `applyTerminalFont` pick it up at construction. Seeded with the
    /// pre-feature 12pt default.
    private var currentTerminalFontSize: CGFloat = FontSettings.defaultSize

    /// Cached "GPU rendering" preference. Read by every pane's
    /// `gpuPreferenceProvider` closure so a Settings toggle propagates
    /// without restarting the session. Defaults to `true` to match
    /// `Tweaks.gpuRendering`'s default.
    private var currentGpuRendering: Bool = true

    /// Cached "Smooth scrolling" preference. Same wiring story as
    /// `currentGpuRendering` — panes read it through a closure so the
    /// Settings toggle takes effect live. Defaults match
    /// `Tweaks.smoothScrolling` (on).
    private var currentSmoothScrolling: Bool = true

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
        claudeSessionMode: ClaudeSessionMode = .none,
        onPaneExit: @escaping @MainActor (String, Int32?) -> Void,
        onPaneTitleChange: @escaping @MainActor (String, String) -> Void
    ) {
        self.tabId = tabId
        self.cwd = cwd
        self.onPaneExit = onPaneExit
        self.onPaneTitleChange = onPaneTitleChange
        self.socketPath = socketPath
        self.zdotdirPath = zdotdirPath
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
        view.gpuPreferenceProvider = { [weak self] in self?.currentGpuRendering ?? true }
        view.smoothScrollPreferenceProvider = { [weak self] in self?.currentSmoothScrolling ?? true }
        let delegate = makePaneDelegate(paneId: id)
        view.processDelegate = delegate
        panes[id] = view
        delegates[id] = delegate

        let resolvedCwd = Self.expandTilde(cwd)
        let isOverride = ProcessInfo.processInfo.environment["NICE_CLAUDE_OVERRIDE"] != nil
        // Claude Code gates OSC title emission on TERM_PROGRAM ∈
        // {"iTerm.app","ghostty","WezTerm","Apple_Terminal"}. Advertise
        // as ghostty so the in-app session label updates automatically
        // — SwiftTerm handles the resulting OSC 0/1/2 sequences natively
        // and the choice doesn't trigger iTerm-specific OSC extensions.
        var claudeExtraEnv: [String: String] = ["TERM_PROGRAM": "ghostty"]
        // Pane/tab identity for the zsh `claude()` wrapper's handshake
        // with Nice's control socket. Only meaningful for the deferred
        // path (the wrapper is what runs the pre-typed command), but
        // harmless for the direct-exec paths too.
        claudeExtraEnv["NICE_TAB_ID"] = tabId
        claudeExtraEnv["NICE_PANE_ID"] = id

        if case .resumeDeferred(let sessionId) = claudeSessionMode {
            // Pane renders as Claude but the pty is a plain shell with
            // the resume command pre-typed. The socket handshake will
            // flip the pane to actually running-Claude when the user
            // hits Enter and the wrapper promotes this pane in place.
            if let sp = socketPath { claudeExtraEnv["NICE_SOCKET"] = sp }
            if let zp = zdotdirPath { claudeExtraEnv["ZDOTDIR"] = zp }
            claudeExtraEnv["NICE_PREFILL_COMMAND"] = "claude --resume \(sessionId)"
            view.startProcess(
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
            view.startProcess(
                executable: "/bin/zsh",
                args: ["-ilc", command],
                environment: Self.buildEnv(extraEnv: claudeExtraEnv),
                execName: nil,
                currentDirectory: resolvedCwd
            )
        } else {
            view.startProcess(
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
    @discardableResult
    func addTerminalPane(
        id: String,
        cwd: String? = nil,
        socketPath: String? = nil,
        zdotdirPath: String? = nil
    ) -> LocalProcessTerminalView {
        let view = NiceTerminalView(frame: .zero)
        view.font = Self.terminalFont(named: currentTerminalFontFamily, size: currentTerminalFontSize)
        view.gpuPreferenceProvider = { [weak self] in self?.currentGpuRendering ?? true }
        view.smoothScrollPreferenceProvider = { [weak self] in self?.currentSmoothScrolling ?? true }
        let delegate = makePaneDelegate(paneId: id)
        view.processDelegate = delegate
        panes[id] = view
        delegates[id] = delegate

        var extraEnv: [String: String] = [:]
        if let sp = socketPath ?? self.socketPath {
            extraEnv["NICE_SOCKET"] = sp
        }
        if let zp = zdotdirPath ?? self.zdotdirPath {
            extraEnv["ZDOTDIR"] = zp
        }
        // Tab/pane identity for the zsh `claude()` wrapper's handshake.
        // The wrapper includes these in its socket payload so Nice can
        // decide whether to open a new sidebar tab or promote this pane.
        extraEnv["NICE_TAB_ID"] = tabId
        extraEnv["NICE_PANE_ID"] = id

        let resolvedCwd = Self.expandTilde(cwd ?? self.cwd)
        view.startProcess(
            executable: "/bin/zsh",
            args: ["-il"],
            environment: Self.buildEnv(extraEnv: extraEnv),
            execName: nil,
            currentDirectory: resolvedCwd
        )

        applyTerminalTheme(currentTerminalTheme, to: view)
        return view
    }

    /// Build a `.pane` role delegate that routes exit + title change
    /// back to `AppState`.
    private func makePaneDelegate(paneId: String) -> ProcessTerminationDelegate {
        let onExit = self.onPaneExit
        let onTitleChange = self.onPaneTitleChange
        return ProcessTerminationDelegate(
            role: .pane(tabId: tabId, paneId: paneId),
            onExit: { [onExit] role, code in
                if case let .pane(_, paneId) = role {
                    onExit(paneId, code)
                }
            },
            onTitleChange: { [onTitleChange] role, title in
                if case let .pane(_, paneId) = role {
                    onTitleChange(paneId, title)
                }
            }
        )
    }

    /// Drop a pane's view + delegate from the dicts. Does NOT terminate
    /// the underlying process — callers invoke this from the pane's
    /// exit hook, by which time the process is already gone.
    func removePane(id: String) {
        panes.removeValue(forKey: id)
        delegates.removeValue(forKey: id)
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
    func terminatePane(id: String) {
        guard let view = panes[id] else { return }
        let pid = view.process.shellPid
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
        guard let view = panes[id] else { return false }
        let fd = view.process.childfd
        let pid = view.process.shellPid
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
        guard let view = panes[paneId] else { return }
        let data = Array((text + "\n").utf8)
        view.send(data: ArraySlice(data))
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
            for view in panes.values {
                view.caretColor = accent
            }
        }
    }

    /// Repaint every live pane with `theme`. Each theme is self-
    /// contained: bg / fg / ANSI come straight from `theme`, and
    /// the caret uses `theme.cursor` when set or falls back to the
    /// current accent otherwise.
    func applyTerminalTheme(_ theme: TerminalTheme) {
        currentTerminalTheme = theme
        for view in panes.values {
            applyTerminalTheme(theme, to: view)
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
    }

    /// Rebuild the font for every live pane with the new family,
    /// preserving the current size. `nil` resets to the default chain.
    func applyTerminalFontFamily(_ name: String?) {
        currentTerminalFontFamily = name
        let font = Self.terminalFont(named: name, size: currentTerminalFontSize)
        for view in panes.values {
            view.font = font
        }
    }

    /// Terminate every live pane's process. Used when a tab is being
    /// closed while its panes are still running (e.g. the user closed
    /// the last tab; model-driven teardown). Pane-exit callbacks still
    /// fire and drive cleanup through the normal path.
    func terminateAll() {
        for view in panes.values {
            view.process.terminate()
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
        for view in panes.values {
            view.font = font
        }
    }

    /// Re-apply the GPU rendering preference to every live pane.
    /// Called by `AppState.updateGpuRendering` when the user flips the
    /// Settings toggle. Each pane's `gpuPreferenceProvider` reads back
    /// through `currentGpuRendering`, so updating the cached value plus
    /// calling `applyGpuPreference` is enough to flip every live pane.
    func applyGpuRendering(enabled: Bool) {
        currentGpuRendering = enabled
        for view in panes.values {
            view.applyGpuPreference()
        }
    }

    /// Re-apply the smooth-scrolling preference to every live pane.
    /// Mirrors `applyGpuRendering` — the preference is read live by
    /// each pane's `smoothScrollPreferenceProvider`, so a Settings
    /// toggle propagates instantly without restarting any pty.
    func applySmoothScrolling(enabled: Bool) {
        currentSmoothScrolling = enabled
        for view in panes.values {
            view.applySmoothScrollPreference()
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
