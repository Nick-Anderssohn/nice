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
//  `promotePaneToClaude` handles the `claude` zsh-shadow flow — the
//  user's terminal pane is about to exec claude in place, so the session
//  just re-themes the pane to the claude background. The `Pane.kind`
//  flip lives in `AppState` alongside the model update.
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

    /// Unix-domain-socket path injected into panes as `NICE_SOCKET`.
    private let socketPath: String?
    /// ZDOTDIR directory injected into terminal panes so the shadowed
    /// `claude()` function is available inside them.
    private let zdotdirPath: String?

    /// Captured for the optional initial claude pane spawn.
    private let claudeBinary: String?
    private let mcpConfigPath: URL?
    private let extraClaudeArgs: [String]

    /// When true, terminal panes on this session get `NICE_TAB_ID` in
    /// their env so the shadowed `claude()` zsh function fires the
    /// `promoteTab` flow. For the built-in Terminals session this is
    /// false — typing `claude` there should open a new sidebar session
    /// (the `newtab` flow), just like the old Main Terminal did.
    private let injectTabIdEnv: Bool

    init(
        tabId: String,
        cwd: String,
        claudeBinary: String?,
        mcpConfigPath: URL? = nil,
        extraClaudeArgs: [String] = [],
        initialClaudePaneId: String? = nil,
        initialTerminalPaneId: String? = nil,
        socketPath: String? = nil,
        zdotdirPath: String? = nil,
        injectTabIdEnv: Bool = true,
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
        self.mcpConfigPath = mcpConfigPath
        self.extraClaudeArgs = extraClaudeArgs
        self.injectTabIdEnv = injectTabIdEnv

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
    @discardableResult
    private func spawnClaudePane(id: String, cwd: String) -> LocalProcessTerminalView {
        let view = NiceTerminalView(frame: .zero)
        view.font = Self.terminalFont(size: currentTerminalFontSize)
        view.gpuPreferenceProvider = { [weak self] in self?.currentGpuRendering ?? true }
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
        let claudeEnv = Self.buildEnv(extraEnv: ["TERM_PROGRAM": "ghostty"])
        if let claude = claudeBinary {
            var parts = ["exec", shellSingleQuote(claude)]
            if !isOverride {
                if let cfg = mcpConfigPath?.path {
                    parts.append("--mcp-config")
                    parts.append(shellSingleQuote(cfg))
                }
                for a in extraClaudeArgs {
                    parts.append(shellSingleQuote(a))
                }
            }
            view.startProcess(
                executable: "/bin/zsh",
                args: ["-ilc", parts.joined(separator: " ")],
                environment: claudeEnv,
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

        applyTheme(
            currentScheme, palette: currentPalette, accent: currentAccent, to: view,
            background: SwiftUI.Color.nicePanelNS(currentScheme, currentPalette)
        )
        return view
    }

    /// Spawn a terminal-kind pane — a plain `zsh -il` with injected
    /// `ZDOTDIR` + `NICE_SOCKET` + `NICE_TAB_ID` so the shadowed
    /// `claude()` function is available inside.
    @discardableResult
    func addTerminalPane(
        id: String,
        cwd: String? = nil,
        socketPath: String? = nil,
        zdotdirPath: String? = nil
    ) -> LocalProcessTerminalView {
        let view = NiceTerminalView(frame: .zero)
        view.font = Self.terminalFont(size: currentTerminalFontSize)
        view.gpuPreferenceProvider = { [weak self] in self?.currentGpuRendering ?? true }
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
        if injectTabIdEnv {
            extraEnv["NICE_TAB_ID"] = tabId
        }

        let resolvedCwd = Self.expandTilde(cwd ?? self.cwd)
        view.startProcess(
            executable: "/bin/zsh",
            args: ["-il"],
            environment: Self.buildEnv(extraEnv: extraEnv),
            execName: nil,
            currentDirectory: resolvedCwd
        )

        applyTheme(
            currentScheme, palette: currentPalette, accent: currentAccent, to: view,
            background: SwiftUI.Color.nicePanelNS(currentScheme, currentPalette)
        )
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

    /// Flip a terminal pane's visual role to claude. The pty already
    /// `exec`s claude inline (the zsh-shadow flow is the only caller),
    /// so there's no process swap — we just repaint with the claude
    /// background. Returns the affected view, or nil if `id` isn't
    /// currently hosted.
    @discardableResult
    func promotePaneToClaude(id: String) -> LocalProcessTerminalView? {
        guard let view = panes[id] else { return nil }
        applyTheme(
            currentScheme, palette: currentPalette, accent: currentAccent, to: view,
            background: SwiftUI.Color.nicePanelNS(currentScheme, currentPalette)
        )
        return view
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

    /// Paint every live pane with the active chrome palette for the
    /// given color scheme. Called from `AppState` on scheme, palette,
    /// or accent changes. All panes use the `nicePanel` background so
    /// the single-pane main area looks consistent across kinds.
    func applyTheme(_ scheme: ColorScheme, palette: Palette, accent: NSColor) {
        currentScheme = scheme
        currentPalette = palette
        currentAccent = accent
        for view in panes.values {
            applyTheme(
                scheme, palette: palette, accent: accent, to: view,
                background: SwiftUI.Color.nicePanelNS(scheme, palette)
            )
        }
    }

    private func applyTheme(
        _ scheme: ColorScheme,
        palette: Palette,
        accent: NSColor,
        to view: LocalProcessTerminalView,
        background: NSColor
    ) {
        let fg = SwiftUI.Color.niceInkNS(scheme, palette)
        let ansi = NiceANSIPalette.colors(for: scheme)
        view.nativeBackgroundColor = background
        view.nativeForegroundColor = fg
        view.installColors(ansi)
        view.caretColor = accent
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

    private static func terminalFont(size: CGFloat) -> NSFont {
        NSFont(name: "JetBrainsMono-Regular", size: size)
            ?? NSFont.userFixedPitchFont(ofSize: size)
            ?? NSFont.systemFont(ofSize: size)
    }

    /// Re-apply the terminal font to every live pane on this session.
    /// Called by `AppState.updateTerminalFontSize` when the user drags
    /// the Font-pane slider or presses Cmd+/-.
    /// SwiftTerm's `LocalProcessTerminalView.font` setter rebuilds its
    /// internal `FontSet`, recomputes cell dimensions, and resizes the
    /// pty — the terminal reflows automatically.
    func applyTerminalFont(size: CGFloat) {
        currentTerminalFontSize = size
        let font = Self.terminalFont(size: size)
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
