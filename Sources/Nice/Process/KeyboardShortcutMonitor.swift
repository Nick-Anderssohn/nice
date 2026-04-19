//
//  KeyboardShortcutMonitor.swift
//  Nice
//
//  A single process-wide local `NSEvent` monitor that translates keyDown
//  events into `AppState` actions according to the bindings owned by
//  `KeyboardShortcuts`. Mirrors `TitleBarZoomMonitor`'s install-once
//  pattern: the first call wires up the monitor and stashes weak refs to
//  the two stores; subsequent calls are no-ops.
//
//  Local monitors run on the app's main thread/run loop, so we can
//  reach `@MainActor` state via `MainActor.assumeIsolated` instead of
//  hopping through a Task.
//
//  We deliberately match on `event.keyCode` (the layout-independent
//  virtual key id) rather than `event.charactersIgnoringModifiers`, so
//  bindings work the same on non-US keyboard layouts.
//
//  Two gates keep the monitor from firing when it shouldn't:
//
//  • `isRecording` — flipped on by `KeyRecorderField` while it's
//    capturing a new combo. The recorder installs its own higher-priority
//    monitor and we stand down so the user's keystrokes reach it instead
//    of triggering an action.
//  • `event.window` must be the main window (or nil for app-targeted
//    events). Without this, pressing ⌘B inside Settings would collapse
//    the sidebar of the main window — surprising — and could swallow
//    keys destined for text fields in Settings.
//

import AppKit
import Foundation

@MainActor
enum KeyboardShortcutMonitor {
    /// Set by `KeyRecorderField` to suspend the global monitor while the
    /// user is recording a new combo. The recorder restores it to `false`
    /// on exit (success, cancel, or view disappear).
    static var isRecording = false

    private static var installed = false

    /// Install the monitor. Safe to call repeatedly — only the first call
    /// installs the underlying `NSEvent` handler. Captures `appState` /
    /// `shortcuts` weakly so the monitor doesn't keep them alive past
    /// app teardown (in practice both are `@StateObject`s on `NiceApp`
    /// and live for the process, so this is belt-and-braces).
    static func install(appState: AppState, shortcuts: KeyboardShortcuts) {
        guard !installed else { return }
        installed = true

        NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak appState, weak shortcuts] event in
            // The closure runs on the main run loop, but it isn't
            // statically MainActor-isolated. We hop via `assumeIsolated`
            // and return a `Bool` (Sendable) rather than `NSEvent?`
            // (non-Sendable) — then reconstruct the event return outside.
            let consumed: Bool = MainActor.assumeIsolated {
                consume(event: event, appState: appState, shortcuts: shortcuts)
            }
            return consumed ? nil : event
        }
    }

    /// Returns `true` if the event matched a binding and was dispatched
    /// (caller should swallow), `false` if it should pass through to
    /// downstream handlers (terminal, Claude pane, SwiftUI focus).
    private static func consume(
        event: NSEvent,
        appState: AppState?,
        shortcuts: KeyboardShortcuts?
    ) -> Bool {
        guard !isRecording else { return false }
        guard let appState, let shortcuts else { return false }

        // Only fire when the user is interacting with the main window.
        // `event.window` is nil for app-level events (menubar, etc.) — we
        // accept those too. Anything else (Settings window, accessory
        // panels) passes through.
        if let window = event.window, window !== NSApp.mainWindow {
            return false
        }

        guard let action = shortcuts.actionMatching(
            keyCode: event.keyCode,
            modifiers: event.modifierFlags
        ) else {
            return false
        }

        dispatch(action, on: appState)
        return true
    }

    private static func dispatch(_ action: ShortcutAction, on appState: AppState) {
        switch action {
        case .nextSidebarTab:  appState.selectNextSidebarTab()
        case .prevSidebarTab:  appState.selectPrevSidebarTab()
        case .nextPane:        appState.selectNextPane()
        case .prevPane:        appState.selectPrevPane()
        case .newTerminalPane: appState.addTerminalToActiveTab()
        case .toggleSidebar:   appState.toggleSidebar()
        }
    }
}
