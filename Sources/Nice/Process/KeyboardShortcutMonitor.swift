//
//  KeyboardShortcutMonitor.swift
//  Nice
//
//  A single process-wide local `NSEvent` monitor that translates keyDown
//  events into `AppState` actions. Multi-window routing: the monitor
//  asks the `WindowRegistry` which AppState is currently focused and
//  dispatches there, so hitting ⌘T in window B adds a pane to window B
//  (not whichever window happened to register first).
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
//    capturing a new combo. The recorder installs its own higher-
//    priority monitor and we stand down so the user's keystrokes reach
//    it instead of triggering an action.
//  • Settings window — the registry tracks which windows host
//    `AppShellView`. Events from a window that isn't registered (i.e.
//    Settings, or any accessory panel) are passed through so shortcuts
//    don't leak into those surfaces.
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

    /// Install the monitor. Safe to call repeatedly — only the first
    /// call installs the underlying `NSEvent` handler. Captures the
    /// registry, shortcuts, and font settings weakly so teardown isn't
    /// blocked by the monitor's retain; in practice all live for the
    /// whole process.
    static func install(
        registry: WindowRegistry,
        shortcuts: KeyboardShortcuts,
        fontSettings: FontSettings
    ) {
        guard !installed else { return }
        installed = true

        NSEvent.addLocalMonitorForEvents(matching: .keyDown) {
            [weak registry, weak shortcuts, weak fontSettings] event in
            let consumed: Bool = MainActor.assumeIsolated {
                consume(
                    event: event,
                    registry: registry,
                    shortcuts: shortcuts,
                    fontSettings: fontSettings
                )
            }
            return consumed ? nil : event
        }

        // Observer-only monitor for modifier-release: ends the
        // sidebar-peek overlay once the user lets go of all the
        // modifiers from the sidebar-tab shortcuts.
        NSEvent.addLocalMonitorForEvents(matching: .flagsChanged) {
            [weak registry, weak shortcuts] event in
            MainActor.assumeIsolated {
                endPeekIfModifiersReleased(
                    event: event,
                    registry: registry,
                    shortcuts: shortcuts
                )
            }
            return event
        }
    }

    /// Returns `true` if the event matched a binding and was dispatched
    /// (caller should swallow), `false` if it should pass through to
    /// downstream handlers (terminal, Claude pane, SwiftUI focus, or
    /// the Settings window's controls).
    private static func consume(
        event: NSEvent,
        registry: WindowRegistry?,
        shortcuts: KeyboardShortcuts?,
        fontSettings: FontSettings?
    ) -> Bool {
        guard !isRecording else { return false }
        guard let registry, let shortcuts else { return false }

        // Events from unregistered windows (Settings, accessory panels)
        // pass through so text fields and the key recorder work normally.
        // App-level events (`event.window == nil`, e.g. from the menubar)
        // route to the key window via the registry below.
        if let window = event.window, registry.isSettingsWindow(window) {
            return false
        }

        guard let action = shortcuts.actionMatching(
            keyCode: event.keyCode,
            modifiers: event.modifierFlags
        ) else {
            return false
        }

        // Font actions dispatch to the process-wide FontSettings — they
        // don't need a focused window. Handle them before the AppState
        // lookup so Cmd+=/-/0 still zoom even with no windows open
        // (e.g. only Settings visible).
        if let fontSettings, dispatchFontAction(action, fontSettings: fontSettings) {
            return true
        }

        guard let appState = registry.activeAppState(preferKey: true) else {
            return false
        }
        dispatch(action, on: appState)

        // Sidebar-tab shortcuts triggered while the sidebar is collapsed
        // float the sidebar over the terminal so the user can see which
        // tab they're cycling toward. The peek dismisses on modifier
        // release (see flagsChanged monitor in `install`).
        if (action == .nextSidebarTab || action == .prevSidebarTab),
           appState.sidebarCollapsed {
            appState.sidebarPeeking = true
        }

        return true
    }

    /// Observer for `.flagsChanged`. If the focused window is currently
    /// peeking, end the peek once none of the sidebar-tab shortcuts'
    /// modifiers are held anymore. Reads bindings live so a rebound
    /// shortcut takes effect without restarting.
    private static func endPeekIfModifiersReleased(
        event: NSEvent,
        registry: WindowRegistry?,
        shortcuts: KeyboardShortcuts?
    ) {
        guard let registry, let shortcuts else { return }
        guard let appState = registry.activeAppState(preferKey: true) else { return }
        guard appState.sidebarPeeking else { return }

        var relevant: NSEvent.ModifierFlags = []
        if let combo = shortcuts.binding(for: .nextSidebarTab) {
            relevant.formUnion(NSEvent.ModifierFlags(rawValue: combo.modifierFlagsRaw))
        }
        if let combo = shortcuts.binding(for: .prevSidebarTab) {
            relevant.formUnion(NSEvent.ModifierFlags(rawValue: combo.modifierFlagsRaw))
        }
        relevant.formIntersection(KeyCombo.relevantModifierMask)
        guard !relevant.isEmpty else {
            // Both sidebar-tab shortcuts are unbound; nothing can hold
            // the peek open, so close it.
            appState.endSidebarPeek()
            return
        }

        let stillHeld = !event.modifierFlags
            .intersection(KeyCombo.relevantModifierMask)
            .intersection(relevant)
            .isEmpty
        if !stillHeld {
            appState.endSidebarPeek()
        }
    }

    /// Handle font-zoom actions. Returns `true` if `action` was a font
    /// action (and was dispatched); `false` if it's unrelated and the
    /// caller should continue the normal window-scoped dispatch.
    private static func dispatchFontAction(
        _ action: ShortcutAction,
        fontSettings: FontSettings
    ) -> Bool {
        switch action {
        case .increaseFontSize: fontSettings.zoom(by: +1); return true
        case .decreaseFontSize: fontSettings.zoom(by: -1); return true
        case .resetFontSizes:   fontSettings.resetToDefaults(); return true
        default: return false
        }
    }

    private static func dispatch(_ action: ShortcutAction, on appState: AppState) {
        switch action {
        case .nextSidebarTab:  appState.selectNextSidebarTab()
        case .prevSidebarTab:  appState.selectPrevSidebarTab()
        case .nextPane:        appState.selectNextPane()
        case .prevPane:        appState.selectPrevPane()
        case .newTerminalPane: appState.addTerminalToActiveTab()
        case .toggleSidebar:   appState.toggleSidebar()
        case .toggleSidebarMode: appState.toggleSidebarMode()
        case .toggleHiddenFiles: appState.toggleFileBrowserHiddenFiles()
        case .increaseFontSize, .decreaseFontSize, .resetFontSizes:
            // Handled by dispatchFontAction before we reach here.
            break
        }
    }
}
