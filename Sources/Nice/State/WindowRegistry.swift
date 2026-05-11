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
//  window's AppState (terminate ptys, stop control socket) without each
//  view needing its own `.onDisappear` plumbing.
//  `didBecomeKeyNotification` updates `lastActiveAppState` so process-
//  wide subsystems arriving while the app is backgrounded still have a
//  sensible target.
//
//  Registration also installs a `CloseConfirmationDelegate` on the
//  window so red-button / ⌘W closes with live panes surface a
//  confirmation alert before teardown. The wrapper forwards every other
//  `NSWindowDelegate` callback to the delegate SwiftUI had installed.
//

import AppKit
import Foundation

@MainActor
@Observable
final class WindowRegistry {
    /// One entry per registered window. `appState` and `window` are
    /// weak so we don't keep either alive past natural SwiftUI teardown;
    /// `handleClose` performs eager cleanup when the window actually
    /// closes.
    private struct Entry {
        weak var appState: AppState?
        weak var window: NSWindow?
        let closeObserver: NSObjectProtocol
        let becomeKeyObserver: NSObjectProtocol
        /// `NSWindow.delegate` is a weak property, so we retain the
        /// confirmation proxy here to keep it alive for the window's
        /// lifetime. Released when `handleClose` drops the entry.
        let closeConfirmer: CloseConfirmationDelegate
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

        // Install a confirmation proxy as the window's delegate,
        // wrapping whatever SwiftUI had previously set. See
        // `CloseConfirmationDelegate` for the forwarding behavior.
        let closeConfirmer = CloseConfirmationDelegate(
            appState: appState,
            originalDelegate: window.delegate
        )
        window.delegate = closeConfirmer

        entries[id] = Entry(
            appState: appState,
            window: window,
            closeObserver: closeObserver,
            becomeKeyObserver: becomeKeyObserver,
            closeConfirmer: closeConfirmer
        )

        // Seed the fallback so the first window has a stable identity
        // before it ever becomes key.
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

    /// Look up an `AppState` by its `windowSessionId`. Returns `nil`
    /// if no live window owns that session. Used by
    /// `FileOperationHistory` to route undo/redo focus back to the
    /// window where an op originated.
    func appState(forSessionId id: String) -> AppState? {
        for entry in entries.values {
            if let appState = entry.appState, appState.windowSession.windowSessionId == id {
                return appState
            }
        }
        return nil
    }

    /// Look up the live `NSWindow` for a given `windowSessionId`.
    /// Used by `FileOperationHistory` to bring the originating
    /// window to the front before applying the inverse op.
    func window(forSessionId id: String) -> NSWindow? {
        for entry in entries.values {
            if let appState = entry.appState,
               appState.windowSession.windowSessionId == id {
                return entry.window
            }
        }
        return nil
    }

    /// True when the given window hosts the Settings scene. The key
    /// monitor uses this to skip shortcut dispatch while the user is in
    /// Preferences so ⌘B et al. don't reach through into the focused
    /// main window.
    func isSettingsWindow(_ window: NSWindow) -> Bool {
        entries[ObjectIdentifier(window)] == nil
    }

    /// Detach every registered window's `willCloseNotification`
    /// observer without otherwise teardown-ing the AppStates. Used by
    /// `NiceServices`'s `willTerminate` handler as belt-and-braces:
    /// even though `userInitiatedClose` correctly routes the reason,
    /// stopping notification delivery before the per-window teardown
    /// loop runs means a regression in the flag logic can't silently
    /// re-introduce the wipe-on-quit bug.
    func detachAllCloseObservers() {
        for entry in entries.values {
            NotificationCenter.default.removeObserver(entry.closeObserver)
        }
    }

    // MARK: - Cleanup

    private func handleClose(window: NSWindow) {
        let id = ObjectIdentifier(window)
        guard let entry = entries.removeValue(forKey: id) else { return }
        NotificationCenter.default.removeObserver(entry.closeObserver)
        NotificationCenter.default.removeObserver(entry.becomeKeyObserver)
        // The reason routes persistence: drop the entry from
        // `sessions.json` if the user explicitly closed this window
        // (via red traffic light / ⌘W — `CloseConfirmationDelegate`
        // flips `userInitiatedClose` when it returns true); preserve
        // it if SwiftUI/AppKit happens to close the NSWindow during
        // process termination so the next launch still reopens it.
        //
        // Inferring intent from the notification source was the
        // original (broken) plan — SwiftUI's `WindowGroup` posts
        // `willCloseNotification` for every live window during
        // `app.terminate(_:)` too. The flag carries the intent
        // through the only AppKit surface that fires uniquely on
        // user-driven close.
        let reason: WindowSession.TearDownReason =
            (entry.appState?.userInitiatedClose ?? false)
                ? .userClosedWindow
                : .appTerminating
        entry.appState?.tearDown(reason: reason)
        if lastActiveAppState == nil || entry.appState === lastActiveAppState {
            lastActiveAppState = entries.values.lazy.compactMap { $0.appState }.first
        }
    }
}
