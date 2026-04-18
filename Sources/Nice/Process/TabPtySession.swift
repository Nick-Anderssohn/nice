//
//  TabPtySession.swift
//  Nice
//
//  Each sidebar tab owns:
//    - a `chatView` running the `claude` CLI (or a zsh fallback when the
//      claude binary isn't on PATH). `chatView` is optional: it goes nil
//      when Claude exits (`closeClaude()`), at which point `AppShellView`
//      stops rendering the middle column for that tab.
//    - one or more `companion` terminals — `zsh -il` shells — keyed by
//      `CompanionTerminal.id` in the `terminals` dict.
//
//  Every spawned view installs its own `ProcessTerminationDelegate` so
//  the owning `AppState` can react to exits on a per-view basis.
//  Sessions are retained by `AppState`, so the underlying processes
//  persist across tab switches and SwiftUI redraws.
//

import AppKit
import SwiftTerm
import SwiftUI

@MainActor
final class TabPtySession: ObservableObject {
    let tabId: String
    let cwd: String
    /// The middle-column view hosting `claude` (or zsh fallback). Nil
    /// once the underlying process has exited (see `closeClaude()`), or
    /// after promotion swaps a companion into this slot and leaves the
    /// previous one detached. `AppShellView` checks both `hasClaudePane`
    /// (on the `Tab`) and the optionality of this view to decide the
    /// three render modes (claude-alive, claude-dead, terminal-only).
    var chatView: LocalProcessTerminalView?
    /// False once the claude/zsh in `chatView` has exited. `AppState`
    /// also stores the same signal on `Tab.hasClaudePane` so the UI
    /// layer (in a later phase) can flip icon + layout without peeking
    /// into the session.
    private(set) var isClaudeAlive: Bool
    /// Companion terminals keyed by `CompanionTerminal.id`.
    var terminals: [String: LocalProcessTerminalView] = [:]
    /// Retains the per-view termination delegates so SwiftTerm's weak
    /// `processDelegate` reference stays live. Keyed by `"chat"` for
    /// the Claude pane and by `CompanionTerminal.id` for companions.
    private var delegates: [String: ProcessTerminationDelegate] = [:]
    /// Stored so later `addCompanion(...)` calls (from UI "+" or MCP)
    /// can wire the same handler the initial companion used.
    private let onChatExit: @MainActor (Int32?) -> Void
    private let onChatTitleChange: @MainActor (String) -> Void
    private let onCompanionExit: @MainActor (String, Int32?) -> Void
    /// Cached SwiftUI `ColorScheme` so companions spawned after the
    /// session already exists (via `addCompanion`) can be themed at
    /// creation without round-tripping through `AppState`.
    private var currentScheme: ColorScheme = .dark
    /// Unix-domain-socket path to inject into companion shells as
    /// `NICE_SOCKET`. Captured at init so every subsequent
    /// `addCompanion(...)` can thread it in without re-plumbing from
    /// the call site.
    private let socketPath: String?
    /// ZDOTDIR directory (same one the Main Terminal uses) to inject
    /// into companion shells. Enables the shadowed `claude()` zsh
    /// function inside companion ptys, which in turn drives the
    /// promote-tab flow via the control socket.
    private let zdotdirPath: String?

    init(
        tabId: String,
        cwd: String,
        claudeBinary: String?,
        mcpConfigPath: URL? = nil,
        extraClaudeArgs: [String] = [],
        initialCompanionId: String,
        socketPath: String? = nil,
        zdotdirPath: String? = nil,
        onChatExit: @escaping @MainActor (Int32?) -> Void,
        onChatTitleChange: @escaping @MainActor (String) -> Void = { _ in },
        onCompanionExit: @escaping @MainActor (String, Int32?) -> Void
    ) {
        self.tabId = tabId
        self.cwd = cwd
        self.onChatExit = onChatExit
        self.onChatTitleChange = onChatTitleChange
        self.onCompanionExit = onCompanionExit
        self.isClaudeAlive = (claudeBinary != nil)
        self.socketPath = socketPath
        self.zdotdirPath = zdotdirPath

        let font = Self.terminalFont()
        let resolvedCwd = Self.expandTilde(cwd)

        // `LocalProcessTerminalView` only exposes `init(frame:)` on
        // macOS; the font is set via the inherited `TerminalView.font`
        // property.
        let chat = LocalProcessTerminalView(frame: .zero)
        chat.font = font
        self.chatView = chat

        let chatDelegate = ProcessTerminationDelegate(
            role: .claude(tabId: tabId),
            onExit: { [onChatExit] _, code in onChatExit(code) },
            onTitleChange: { [onChatTitleChange] _, title in onChatTitleChange(title) }
        )
        chat.processDelegate = chatDelegate
        self.delegates["chat"] = chatDelegate
        // Note: even when `claudeBinary == nil`, we still spawn zsh in
        // the chat slot as a live fallback (behavior from Phase 0).
        // The type is now optional, but the initial value is non-nil.

        // Spawn claude if available; otherwise spawn zsh as a fallback
        // so the middle column shows a live prompt. claude shells out
        // to `/bin/sh -c "node …"` for sub-tools, so it needs the
        // user's full PATH. Three reasons the default env is
        // insufficient: (1) apps opened from Finder inherit only
        // launchd's minimal PATH (no `/opt/homebrew/bin` etc.);
        // (2) SwiftTerm's `getEnvironmentVariables` deliberately omits
        // PATH from the forwarded keys; and (3) node managers like
        // nvm only activate in `.zshrc` (interactive), not
        // `.zprofile` (login-only). Running `zsh -ilc` sources both,
        // then `exec` swaps the shell for claude so the process tree
        // looks clean.
        let isOverride = ProcessInfo.processInfo.environment["NICE_CLAUDE_OVERRIDE"] != nil
        if let claude = claudeBinary {
            var parts = ["exec", Self.shellQuote(claude)]
            if !isOverride {
                if let cfg = mcpConfigPath?.path {
                    parts.append("--mcp-config")
                    parts.append(Self.shellQuote(cfg))
                }
                for a in extraClaudeArgs {
                    parts.append(Self.shellQuote(a))
                }
            }
            chat.startProcess(
                executable: "/bin/zsh",
                args: ["-ilc", parts.joined(separator: " ")],
                environment: nil,
                execName: nil,
                currentDirectory: resolvedCwd
            )
        } else {
            chat.startProcess(
                executable: "/bin/zsh",
                args: ["-il"],
                environment: nil,
                execName: nil,
                currentDirectory: resolvedCwd
            )
        }

        // Always bring up the initial companion — every live tab keeps
        // at least one zsh pane so the layout has something to render
        // on the terminal side.
        _ = addCompanion(id: initialCompanionId, cwd: cwd)
    }

    // MARK: - Companion lifecycle

    /// Allocate a fresh `LocalProcessTerminalView`, install a
    /// per-companion `ProcessTerminationDelegate`, start `zsh -il` in
    /// `cwd` (defaulting to the session's cwd), and insert into
    /// `terminals` + `delegates`. Returns the new view so the caller
    /// (SwiftUI layer) can host it.
    ///
    /// `socketPath` / `zdotdirPath` override the session-level
    /// defaults captured at init, if a caller needs to point a single
    /// companion elsewhere. Both default to the stored values so the
    /// common case is the old two-argument call.
    @discardableResult
    func addCompanion(
        id: String,
        cwd: String? = nil,
        socketPath: String? = nil,
        zdotdirPath: String? = nil
    ) -> LocalProcessTerminalView {
        let view = LocalProcessTerminalView(frame: .zero)
        view.font = Self.terminalFont()
        let onCompanionExit = self.onCompanionExit
        let delegate = ProcessTerminationDelegate(
            role: .companion(tabId: tabId, companionId: id),
            onExit: { [onCompanionExit] role, code in
                if case let .companion(_, companionId) = role {
                    onCompanionExit(companionId, code)
                }
            }
        )
        view.processDelegate = delegate
        terminals[id] = view
        delegates[id] = delegate

        // Build extra env for the companion: socket path + ZDOTDIR so
        // the shadowed `claude()` function loads, plus NICE_TAB_ID so
        // the shadow knows which tab to promote. Same merge pattern as
        // MainTerminalSession.buildEnv. If no socket/zdotdir were
        // supplied, fall back to a plain env — running `claude` inside
        // the companion will just exec the real binary.
        var extraEnv: [String: String] = [:]
        if let sp = socketPath ?? self.socketPath {
            extraEnv["NICE_SOCKET"] = sp
        }
        if let zp = zdotdirPath ?? self.zdotdirPath {
            extraEnv["ZDOTDIR"] = zp
        }
        extraEnv["NICE_TAB_ID"] = tabId

        let resolvedCwd = Self.expandTilde(cwd ?? self.cwd)
        view.startProcess(
            executable: "/bin/zsh",
            args: ["-il"],
            environment: Self.buildEnv(extraEnv: extraEnv),
            execName: nil,
            currentDirectory: resolvedCwd
        )

        // Paint with the currently-cached scheme so the pane doesn't
        // flash default colors before the next `applyTheme` call.
        applyTheme(currentScheme, to: view)
        return view
    }

    /// Merge `extraEnv` on top of SwiftTerm's default forwarded env
    /// (TERM, COLORTERM, LANG, LOGNAME, USER, HOME). Mirrors the
    /// helper in `MainTerminalSession`.
    private static func buildEnv(extraEnv: [String: String]) -> [String] {
        var env = SwiftTerm.Terminal.getEnvironmentVariables()
        for (k, v) in extraEnv {
            env.append("\(k)=\(v)")
        }
        return env
    }

    /// Drop the companion's view + delegate from the dicts. Does NOT
    /// terminate the underlying process — callers invoke this from the
    /// delegate's exit hook, by which time the process is already gone.
    func removeCompanion(id: String) {
        terminals.removeValue(forKey: id)
        delegates.removeValue(forKey: id)
    }

    /// Move a companion's view into the chat slot. Removes the
    /// companion from `terminals` / `delegates`, installs a fresh
    /// claude-role `ProcessTerminationDelegate` on the view (so a
    /// subsequent exit routes through `onChatExit`), and assigns the
    /// view to `self.chatView`. Returns the view so callers can act on
    /// it (e.g. to focus it in the layout). Nil if `id` isn't a known
    /// companion.
    @discardableResult
    func promoteCompanionToChat(id: String) -> LocalProcessTerminalView? {
        guard let view = terminals[id] else { return nil }
        terminals.removeValue(forKey: id)
        delegates.removeValue(forKey: id)

        let onChatExit = self.onChatExit
        let onChatTitleChange = self.onChatTitleChange
        let delegate = ProcessTerminationDelegate(
            role: .claude(tabId: tabId),
            onExit: { [onChatExit] _, code in onChatExit(code) },
            onTitleChange: { [onChatTitleChange] _, title in onChatTitleChange(title) }
        )
        view.processDelegate = delegate
        delegates["chat"] = delegate
        self.chatView = view
        isClaudeAlive = true
        return view
    }

    /// Mark the chat view's process as gone and drop the view so the
    /// UI layer stops rendering the middle column for this tab. The
    /// matching `Tab.hasClaudePane` flag is maintained by `AppState`.
    func closeClaude() {
        isClaudeAlive = false
        delegates.removeValue(forKey: "chat")
        chatView = nil
    }

    // MARK: - IO

    /// Single-quote a string for safe inclusion in a zsh `-c` command.
    /// Embedded single quotes are escaped with the standard `'\''`
    /// close-open-escape-reopen sequence.
    private static func shellQuote(_ s: String) -> String {
        "'" + s.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }

    /// Send `text` plus a newline into the specified companion's pty.
    /// No-op if `companionId` isn't currently hosted in this session.
    func sendToTerminal(_ text: String, companionId: String) {
        guard let view = terminals[companionId] else { return }
        let data = Array((text + "\n").utf8)
        view.send(data: ArraySlice(data))
    }

    // MARK: - Theming

    /// Paint every live pane with the Nice palette for the given color
    /// scheme. Called from `AppShellView` on appear and whenever the
    /// effective `ColorScheme` flips. Subsequently spawned companions
    /// pick up the cached scheme inside `addCompanion`.
    func applyTheme(_ scheme: ColorScheme) {
        currentScheme = scheme
        if let chat = chatView {
            applyTheme(scheme, to: chat)
        }
        for view in terminals.values {
            applyTheme(scheme, to: view)
        }
    }

    private func applyTheme(_ scheme: ColorScheme, to view: LocalProcessTerminalView) {
        // `Color` is ambiguous in this file — SwiftTerm also exports one.
        // Qualify with `SwiftUI.` to pick up the palette extensions.
        let bg = SwiftUI.Color.niceBg3NS(scheme)
        let fg = SwiftUI.Color.niceInkNS(scheme)
        let palette = NiceANSIPalette.colors(for: scheme)
        view.nativeBackgroundColor = bg
        view.nativeForegroundColor = fg
        view.installColors(palette)
    }

    // MARK: - Helpers

    private static func terminalFont() -> NSFont {
        NSFont(name: "JetBrainsMono-Regular", size: 12)
            ?? NSFont.userFixedPitchFont(ofSize: 12)
            ?? NSFont.systemFont(ofSize: 12)
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
