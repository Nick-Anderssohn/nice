//
//  AppDelegate.swift
//  Nice
//
//  Quit / window-close confirmation. Shows an NSAlert before the app
//  tears down any live pty sessions, so the user doesn't lose a running
//  Claude conversation or a terminal mid-command because of an
//  accidental ⌘Q or traffic-light click.
//
//  Two entry points:
//    • `applicationShouldTerminate` — ⌘Q, Quit menu item, force-quit
//      via Dock. Counts panes across every registered window.
//    • `CloseConfirmationDelegate.windowShouldClose` — red traffic
//      light or ⌘W. Counts only the window being closed.
//
//  The existing "last pane exited" path in `AppState.paneExited` already
//  surfaces its own prompt (`showQuitPrompt`); when its Quit button
//  fires it sets `skipNextTerminateConfirmation` so we don't double-ask.
//

import AppKit
import Foundation

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    /// Late-bound registry accessor. `@NSApplicationDelegateAdaptor`
    /// builds this delegate before SwiftUI instantiates `NiceServices`,
    /// so the app sets this pointer from `NiceApp` once services exist.
    static var registryProvider: (@MainActor () -> WindowRegistry?)?

    /// One-shot override flipped by flows that have already asked the
    /// user to confirm quitting (the "last pane exited" alert on the
    /// Terminals tab). Read-and-reset in `applicationShouldTerminate`.
    static var skipNextTerminateConfirmation = false

    func applicationShouldTerminate(
        _ sender: NSApplication
    ) -> NSApplication.TerminateReply {
        if Self.skipNextTerminateConfirmation {
            Self.skipNextTerminateConfirmation = false
            return .terminateNow
        }
        guard let registry = Self.registryProvider?() else {
            return .terminateNow
        }
        var claude = 0
        var terminal = 0
        for state in registry.allAppStates {
            let counts = state.livePaneCounts
            claude += counts.claude
            terminal += counts.terminal
        }
        guard claude + terminal > 0 else { return .terminateNow }

        let confirmed = QuitConfirmation.run(
            messageText: "Quit NICE?",
            claude: claude,
            terminal: terminal,
            confirmTitle: "Quit"
        )
        return confirmed ? .terminateNow : .terminateCancel
    }
}

/// NSWindowDelegate proxy that asks before the red button / ⌘W closes a
/// window with live panes. Forwards every other `NSWindowDelegate`
/// callback to whatever delegate SwiftUI had installed previously so we
/// don't break scene-level behavior we're unaware of.
@MainActor
final class CloseConfirmationDelegate: NSObject, NSWindowDelegate {
    weak var appState: AppState?
    private var originalDelegate: NSWindowDelegate?

    init(appState: AppState, originalDelegate: NSWindowDelegate?) {
        self.appState = appState
        self.originalDelegate = originalDelegate
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        let delegateAllows = originalDelegate?.windowShouldClose?(sender) ?? true
        guard delegateAllows else { return false }
        guard let state = appState else { return true }
        let counts = state.livePaneCounts
        guard counts.claude + counts.terminal > 0 else { return true }
        return QuitConfirmation.run(
            messageText: "Close this window?",
            claude: counts.claude,
            terminal: counts.terminal,
            confirmTitle: "Close"
        )
    }

    // Objective-C message forwarding: anything we don't implement goes
    // to the wrapped delegate so SwiftUI's own bookkeeping survives.
    override func responds(to aSelector: Selector!) -> Bool {
        if super.responds(to: aSelector) { return true }
        return originalDelegate?.responds(to: aSelector) ?? false
    }

    override func forwardingTarget(for aSelector: Selector!) -> Any? {
        if let d = originalDelegate, d.responds(to: aSelector) {
            return d
        }
        return nil
    }
}

// MARK: - Alert helper

@MainActor
enum QuitConfirmation {
    /// Show the modal and return `true` if the user clicked the
    /// confirm button. Factored out so the app-terminate and window-
    /// close paths share identical phrasing.
    static func run(
        messageText: String,
        claude: Int,
        terminal: Int,
        confirmTitle: String
    ) -> Bool {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = messageText
        alert.informativeText = describe(claude: claude, terminal: terminal)
        alert.addButton(withTitle: confirmTitle)
        alert.addButton(withTitle: "Cancel")
        return alert.runModal() == .alertFirstButtonReturn
    }

    private static func describe(claude: Int, terminal: Int) -> String {
        var parts: [String] = []
        if claude > 0 {
            parts.append("\(claude) Claude session\(claude == 1 ? "" : "s")")
        }
        if terminal > 0 {
            parts.append("\(terminal) terminal\(terminal == 1 ? "" : "s")")
        }
        let list = parts.joined(separator: " and ")
        // Claude sessions are saved to disk; next launch reopens the
        // tab with `claude --resume <uuid>` pre-typed at the prompt,
        // and the user decides whether to run it. Terminals don't
        // have a restorable transcript so they're just closed.
        if claude > 0 && terminal > 0 {
            return "You still have \(list) open. Claude sessions will be saved for next launch; terminals will be closed."
        }
        if claude > 0 {
            return "You still have \(list) open. They will be saved for next launch."
        }
        return "You still have \(list) open. They will be closed."
    }
}
