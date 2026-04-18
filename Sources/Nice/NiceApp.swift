//
//  NiceApp.swift
//  Nice
//
//  Phase 5: the shell now owns a second observable store (`Tweaks`) that
//  drives user-selected theme + accent, and registers a standard
//  `Settings { … }` scene so ⌘, opens the preferences window on macOS.
//  Both environment objects are injected into the main window *and* the
//  Settings window, and the Settings window is explicitly sized to
//  640×440 per the design mock.
//

import SwiftUI

@main
struct NiceApp: App {
    @StateObject private var appState = AppState()
    @StateObject private var tweaks = Tweaks()

    var body: some Scene {
        WindowGroup {
            AppShellView()
                .environmentObject(appState)
                .environmentObject(tweaks)
                .frame(minWidth: 1180, minHeight: 680)
                .preferredColorScheme(tweaks.theme.scheme)
                .tint(tweaks.accent.color)
        }
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentMinSize)

        // ⌘, binds to this scene automatically on macOS. SettingsView
        // sets its own 640×440 frame, but we repeat it here so the
        // window resizes correctly even before the child view lays out.
        Settings {
            SettingsView()
                .environmentObject(appState)
                .environmentObject(tweaks)
                .frame(width: 640, height: 440)
                .preferredColorScheme(tweaks.theme.scheme)
                .tint(tweaks.accent.color)
        }
    }
}
