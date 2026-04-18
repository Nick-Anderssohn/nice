//
//  MainTerminalSession.swift
//  Nice
//
//  Phase 4: singleton zsh pty that backs the "Main terminal" sidebar row.
//  Owned by `AppState`; the view identity is stable, so `TerminalHost`
//  can relay it as-is without re-creating on every redraw.
//

import AppKit
import SwiftTerm
import SwiftUI

@MainActor
final class MainTerminalSession: ObservableObject {
    let view: LocalProcessTerminalView
    private let delegateBridge = TerminalDelegateBridge()
    private(set) var cwd: String

    init(cwd: String) {
        self.cwd = cwd
        let font = NSFont(name: "JetBrainsMono-Regular", size: 12)
            ?? NSFont.userFixedPitchFont(ofSize: 12)
            ?? NSFont.systemFont(ofSize: 12)
        // `LocalProcessTerminalView` only exposes `init(frame:)` on macOS;
        // the font is set via the inherited `TerminalView.font` property.
        self.view = LocalProcessTerminalView(frame: .zero)
        self.view.font = font
        self.view.processDelegate = delegateBridge
        self.view.startProcess(
            executable: "/bin/zsh",
            args: ["-il"],
            environment: nil,
            execName: nil,
            currentDirectory: Self.expandTilde(cwd)
        )
    }

    /// Paint the pane with the Nice palette for the current color scheme.
    /// Called from `AppShellView` on appear and on scheme changes.
    func applyTheme(_ scheme: ColorScheme) {
        // Qualify with `SwiftUI.` to disambiguate from `SwiftTerm.Color`.
        view.nativeBackgroundColor = SwiftUI.Color.niceBg3NS(scheme)
        view.nativeForegroundColor = SwiftUI.Color.niceInkNS(scheme)
        view.installColors(NiceANSIPalette.colors(for: scheme))
    }

    /// Terminate the current zsh (if any) and re-spawn in `cwd`. Called
    /// from the sidebar's "Change directory…" action.
    func restart(cwd: String) {
        self.cwd = cwd
        view.process.terminate()
        view.startProcess(
            executable: "/bin/zsh",
            args: ["-il"],
            environment: nil,
            execName: nil,
            currentDirectory: Self.expandTilde(cwd)
        )
    }

    private static func expandTilde(_ path: String) -> String {
        if path == "~" { return NSHomeDirectory() }
        if path.hasPrefix("~/") {
            return NSHomeDirectory() + path.dropFirst(1)
        }
        return path
    }
}
