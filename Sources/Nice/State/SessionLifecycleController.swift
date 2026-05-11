//
//  SessionLifecycleController.swift
//  Nice
//
//  Single place that owns the close/quit lifecycle for every window
//  in the process. Two surfaces:
//
//   • `handleAppWillTerminate` — called from `NiceServices`'s
//     `willTerminateNotification` observer. Detaches per-window close
//     observers *before* the per-window tearDown loop so SwiftUI's
//     scene-teardown burst (which posts `willCloseNotification` for
//     every live window during `app.terminate(_:)`) cannot reach
//     `WindowRegistry.handleClose` and re-route an already-torn-down
//     window through the `.userClosedWindow` path.
//
//   • `handleWindowWillClose` — called from `WindowRegistry.handleClose`
//     once it has removed the entry and notification observers.
//     Reads `AppState.userInitiatedClose` (set only by
//     `CloseConfirmationDelegate.windowShouldClose` for genuine
//     red-button / ⌘W closes) and routes the tearDown reason
//     accordingly.
//
//  Pure orchestrator: no stored dependencies. Callers hand it the
//  AppStates and an observer-detacher closure, the controller decides
//  on order and reason. Lets a unit test fire willTerminate followed
//  by willCloseNotification (the sequence that wiped one window's
//  tabs in prod) against a single object without standing up a
//  full NiceServices.
//

import Foundation

@MainActor
final class SessionLifecycleController {
    /// Drive the app-terminate cascade for every still-registered
    /// window. Order is load-bearing: `detachObservers` MUST run
    /// before the tearDown loop so the scene-teardown burst can't
    /// fire `willCloseNotification` on a window whose AppState we're
    /// about to tear down with `.appTerminating` — that would route
    /// the same window through `.userClosedWindow` and remove the
    /// snapshot we just upserted.
    func handleAppWillTerminate(
        allAppStates: [AppState],
        detachObservers: () -> Void
    ) {
        detachObservers()
        for state in allAppStates {
            state.tearDown(reason: .appTerminating)
        }
    }

    /// Decide the tearDown reason for a single closing window and
    /// dispatch. `WindowRegistry` has already removed its own entry
    /// and notification-center observers before calling this — the
    /// controller's job is reason routing + tearDown, nothing else.
    ///
    /// The flag is the only authoritative signal: AppKit delivers
    /// `windowShouldClose` exclusively for user-initiated closes
    /// (never during `app.terminate(_:)`), so
    /// `CloseConfirmationDelegate` flipping it on confirm is
    /// unambiguous. Default-false → preserve the snapshot, which is
    /// the safer failure mode if any future close path forgets to
    /// flip the flag.
    func handleWindowWillClose(appState: AppState?) {
        let reason: WindowSession.TearDownReason =
            (appState?.userInitiatedClose ?? false)
                ? .userClosedWindow
                : .appTerminating
        appState?.tearDown(reason: reason)
    }
}
