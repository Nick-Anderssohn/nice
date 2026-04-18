//
//  TabPtySession.swift
//  Nice
//
//  Phase 4: each sidebar tab owns a pair of pty-backed SwiftTerm views —
//  the middle "chat" column (running the `claude` CLI if resolvable, else
//  a zsh fallback), and the right "terminal" column (always zsh). The
//  session is retained by `AppState` so the underlying processes keep
//  running across tab switches and SwiftUI redraws.
//

import AppKit
import SwiftTerm
import SwiftUI

@MainActor
final class TabPtySession: ObservableObject {
    let tabId: String
    let cwd: String
    let chatView: LocalProcessTerminalView
    let terminalView: LocalProcessTerminalView
    private let delegateBridge = TerminalDelegateBridge()

    init(
        tabId: String,
        cwd: String,
        claudeBinary: String?,
        mcpConfigPath: URL? = nil,
        extraClaudeArgs: [String] = []
    ) {
        self.tabId = tabId
        self.cwd = cwd
        let font = NSFont(name: "JetBrainsMono-Regular", size: 12)
            ?? NSFont.userFixedPitchFont(ofSize: 12)
            ?? NSFont.systemFont(ofSize: 12)
        // `LocalProcessTerminalView` only exposes `init(frame:)` on macOS;
        // the font is set via the inherited `TerminalView.font` property.
        self.chatView = LocalProcessTerminalView(frame: .zero)
        self.terminalView = LocalProcessTerminalView(frame: .zero)
        chatView.font = font
        terminalView.font = font
        chatView.processDelegate = delegateBridge
        terminalView.processDelegate = delegateBridge

        // Spawn claude if available; otherwise spawn zsh as a fallback so
        // the user always sees a live prompt in the middle pane.
        //
        // claude shells out to `/bin/sh -c "node …"` for sub-tools, so
        // it needs the user's full PATH. Three reasons the default env
        // is insufficient: (1) apps opened from Finder inherit only
        // launchd's minimal PATH (no `/opt/homebrew/bin` etc.);
        // (2) SwiftTerm's `getEnvironmentVariables` deliberately omits
        // PATH from the forwarded keys; and (3) node managers like nvm
        // only activate in `.zshrc` (interactive), not `.zprofile`
        // (login-only). Running `zsh -ilc` sources both, then `exec`
        // swaps the shell for claude so the process tree looks clean.
        let resolvedCwd = Self.expandTilde(cwd)
        if let claude = claudeBinary {
            var parts = ["exec", Self.shellQuote(claude)]
            if let cfg = mcpConfigPath?.path {
                parts.append("--mcp-config")
                parts.append(Self.shellQuote(cfg))
            }
            // Pass-through args from the Main Terminal's `claude …`
            // invocation. Already-quoted on the way in via shellQuote,
            // so paths with spaces / embedded quotes survive intact.
            for a in extraClaudeArgs {
                parts.append(Self.shellQuote(a))
            }
            chatView.startProcess(
                executable: "/bin/zsh",
                args: ["-ilc", parts.joined(separator: " ")],
                environment: nil,
                execName: nil,
                currentDirectory: resolvedCwd
            )
        } else {
            chatView.startProcess(
                executable: "/bin/zsh",
                args: ["-il"],
                environment: nil,
                execName: nil,
                currentDirectory: resolvedCwd
            )
        }
        terminalView.startProcess(
            executable: "/bin/zsh",
            args: ["-il"],
            environment: nil,
            execName: nil,
            currentDirectory: resolvedCwd
        )
    }

    /// Single-quote a string for safe inclusion in a zsh `-c` command.
    /// Embedded single quotes are escaped with the standard `'\''`
    /// close-open-escape-reopen sequence.
    private static func shellQuote(_ s: String) -> String {
        "'" + s.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }

    /// Send `text` plus a newline into the tab's right-side terminal.
    func sendToTerminal(_ text: String) {
        let data = Array((text + "\n").utf8)
        terminalView.send(data: ArraySlice(data))
    }

    /// Paint both panes with the Nice palette for the current color
    /// scheme. Called from `AppShellView` on appear and whenever the
    /// effective `ColorScheme` flips.
    func applyTheme(_ scheme: ColorScheme) {
        // `Color` is ambiguous in this file — SwiftTerm also exports one.
        // Qualify with `SwiftUI.` to pick up the palette extensions.
        let bg = SwiftUI.Color.niceBg3NS(scheme)
        let fg = SwiftUI.Color.niceInkNS(scheme)
        let palette = NiceANSIPalette.colors(for: scheme)
        for view in [chatView, terminalView] {
            view.nativeBackgroundColor = bg
            view.nativeForegroundColor = fg
            view.installColors(palette)
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
