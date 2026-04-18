//
//  MainTerminalSession.swift
//  Nice
//
//  Singleton zsh pty that backs the "Main terminal" sidebar row.
//  Owned by `AppState`; the view identity is stable, so `TerminalHost`
//  can relay it as-is without re-creating on every redraw.
//
//  Termination is handled via a `ProcessTerminationDelegate` in the
//  `.main` role — when the shell exits (Ctrl+D, `exit`, crash),
//  AppState decides whether to show a quit prompt or terminate the app
//  outright.
//

import AppKit
import SwiftTerm
import SwiftUI

@MainActor
final class MainTerminalSession: ObservableObject {
    let view: LocalProcessTerminalView
    /// Retained so SwiftTerm's weak `processDelegate` reference stays
    /// alive for the lifetime of the session.
    private var delegate: ProcessTerminationDelegate
    /// Closure AppState passes in; re-used by `restart(cwd:)` when we
    /// reinstall the delegate on a fresh view.
    private let onExit: @MainActor (Int32?) -> Void
    private(set) var cwd: String
    /// Extra env vars (merged on top of SwiftTerm's defaults) passed
    /// into zsh every time we spawn. Used to carry `ZDOTDIR` and
    /// `NICE_SOCKET` so the shadowed `claude()` function loads and
    /// knows where to talk to.
    private let extraEnv: [String: String]

    init(
        cwd: String,
        extraEnv: [String: String] = [:],
        onExit: @escaping @MainActor (Int32?) -> Void
    ) {
        self.cwd = cwd
        self.extraEnv = extraEnv
        self.onExit = onExit
        let font = NSFont(name: "JetBrainsMono-Regular", size: 12)
            ?? NSFont.userFixedPitchFont(ofSize: 12)
            ?? NSFont.systemFont(ofSize: 12)
        // `LocalProcessTerminalView` only exposes `init(frame:)` on macOS;
        // the font is set via the inherited `TerminalView.font` property.
        self.view = LocalProcessTerminalView(frame: .zero)
        self.view.font = font
        self.delegate = ProcessTerminationDelegate(
            role: .main,
            onExit: { [onExit] _, code in onExit(code) }
        )
        self.view.processDelegate = self.delegate
        self.view.startProcess(
            executable: "/bin/zsh",
            args: ["-il"],
            environment: Self.buildEnv(extraEnv: extraEnv),
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
    /// from the sidebar's "Change directory…" action and from
    /// `AppState.cancelQuitPrompt` when the user backs out of the
    /// Main-Terminal-exit quit confirmation. Preserves the same
    /// `extraEnv` as the initial spawn so ZDOTDIR/NICE_SOCKET stick
    /// across restarts, and reinstalls the termination delegate so the
    /// fresh shell's exit still routes back to AppState.
    func restart(cwd: String) {
        self.cwd = cwd
        view.process.terminate()
        let onExit = self.onExit
        let fresh = ProcessTerminationDelegate(
            role: .main,
            onExit: { [onExit] _, code in onExit(code) }
        )
        self.delegate = fresh
        view.processDelegate = fresh
        view.startProcess(
            executable: "/bin/zsh",
            args: ["-il"],
            environment: Self.buildEnv(extraEnv: extraEnv),
            execName: nil,
            currentDirectory: Self.expandTilde(cwd)
        )
    }

    /// Merge `extraEnv` on top of `Terminal.getEnvironmentVariables()`
    /// (TERM, COLORTERM, LANG, LOGNAME, USER, HOME — PATH is
    /// intentionally omitted; zsh -il sources .zprofile/.zshrc to
    /// populate it). Returns the `KEY=VALUE` list `startProcess`
    /// expects.
    private static func buildEnv(extraEnv: [String: String]) -> [String] {
        var env = Terminal.getEnvironmentVariables()
        for (k, v) in extraEnv {
            env.append("\(k)=\(v)")
        }
        return env
    }

    private static func expandTilde(_ path: String) -> String {
        if path == "~" { return NSHomeDirectory() }
        if path.hasPrefix("~/") {
            return NSHomeDirectory() + path.dropFirst(1)
        }
        return path
    }
}
