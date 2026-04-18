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

@MainActor
final class TabPtySession: ObservableObject {
    let tabId: String
    let cwd: String
    let chatView: LocalProcessTerminalView
    let terminalView: LocalProcessTerminalView
    private let delegateBridge = TerminalDelegateBridge()

    init(tabId: String, cwd: String, claudeBinary: String?) {
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
        let resolvedCwd = Self.expandTilde(cwd)
        if let claude = claudeBinary {
            chatView.startProcess(
                executable: claude,
                args: [],
                environment: nil,
                execName: nil,
                currentDirectory: resolvedCwd
            )
        } else {
            chatView.startProcess(
                executable: "/bin/zsh",
                args: ["-l"],
                environment: nil,
                execName: nil,
                currentDirectory: resolvedCwd
            )
        }
        terminalView.startProcess(
            executable: "/bin/zsh",
            args: ["-l"],
            environment: nil,
            execName: nil,
            currentDirectory: resolvedCwd
        )
    }

    /// Send `text` plus a newline into the tab's right-side terminal.
    func sendToTerminal(_ text: String) {
        let data = Array((text + "\n").utf8)
        terminalView.send(data: ArraySlice(data))
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
