//
//  WindowRegistry.swift
//  Nice
//
//  Maps each live `NSWindow` to the `AppState` that owns its per-window
//  state. Exists so process-wide subsystems (`KeyboardShortcutMonitor`,
//  app termination) can route work to the focused window without the
//  views themselves holding a reference to a single shared AppState.
//
//  Each `AppShellView` registers on mount via `WindowAccessor`. The
//  registry observes `willCloseNotification` so it can tear down the
//  window's AppState (terminate ptys, stop control socket, stop MCP
//  server) without each view needing its own `.onDisappear` plumbing.
//  `didBecomeKeyNotification` updates `lastActiveAppState` so MCP tool
//  calls arriving while the app is backgrounded still have a sensible
//  target.
//

import AppKit
import Foundation

@MainActor
final class WindowRegistry: ObservableObject {
    /// One entry per registered window. `appState` and `window` are
    /// weak so we don't keep either alive past natural SwiftUI teardown;
    /// `handleClose` performs eager cleanup when the window actually
    /// closes.
    private struct Entry {
        weak var appState: AppState?
        weak var window: NSWindow?
        let closeObserver: NSObjectProtocol
        let becomeKeyObserver: NSObjectProtocol
    }

    private var entries: [ObjectIdentifier: Entry] = [:]
    private weak var lastActiveAppState: AppState?

    func register(appState: AppState, window: NSWindow) {
        let id = ObjectIdentifier(window)
        if entries[id] != nil { return }

        let closeObserver = NotificationCenter.default.addObserver(
            forName: NSWindow.willCloseNotification,
            object: window,
            queue: .main
        ) { [weak self, weak window] _ in
            guard let self, let window else { return }
            MainActor.assumeIsolated {
                self.handleClose(window: window)
            }
        }

        let becomeKeyObserver = NotificationCenter.default.addObserver(
            forName: NSWindow.didBecomeKeyNotification,
            object: window,
            queue: .main
        ) { [weak self, weak appState] _ in
            guard let self, let appState else { return }
            MainActor.assumeIsolated {
                self.lastActiveAppState = appState
            }
        }

        entries[id] = Entry(
            appState: appState,
            window: window,
            closeObserver: closeObserver,
            becomeKeyObserver: becomeKeyObserver
        )

        // Seed the fallback so the first window is reachable from MCP
        // tool calls before it ever becomes key.
        if lastActiveAppState == nil {
            lastActiveAppState = appState
        }
    }

    /// AppState for `NSApp.keyWindow` when `preferKey`; otherwise the
    /// most recently keyed window, or the first registered as a last
    /// resort. Returns `nil` only when no windows are alive.
    func activeAppState(preferKey: Bool = false) -> AppState? {
        if preferKey, let key = NSApp.keyWindow {
            if let state = entries[ObjectIdentifier(key)]?.appState {
                return state
            }
        }
        if let last = lastActiveAppState {
            return last
        }
        return entries.values.lazy.compactMap { $0.appState }.first
    }

    /// Snapshot of every live AppState. Used for app-wide teardown.
    var allAppStates: [AppState] {
        entries.values.compactMap { $0.appState }
    }

    /// True when the given window hosts the Settings scene. The key
    /// monitor uses this to skip shortcut dispatch while the user is in
    /// Preferences so ⌘B et al. don't reach through into the focused
    /// main window.
    func isSettingsWindow(_ window: NSWindow) -> Bool {
        entries[ObjectIdentifier(window)] == nil
    }

    // MARK: - Cleanup

    private func handleClose(window: NSWindow) {
        let id = ObjectIdentifier(window)
        guard let entry = entries.removeValue(forKey: id) else { return }
        NotificationCenter.default.removeObserver(entry.closeObserver)
        NotificationCenter.default.removeObserver(entry.becomeKeyObserver)
        entry.appState?.tearDown()
        if lastActiveAppState == nil || entry.appState === lastActiveAppState {
            lastActiveAppState = entries.values.lazy.compactMap { $0.appState }.first
        }
    }
}
