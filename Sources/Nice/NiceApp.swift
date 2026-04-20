//
//  NiceApp.swift
//  Nice
//
//  App entry point. App-wide services (`Tweaks`, `KeyboardShortcuts`,
//  the shared `WindowRegistry`, and the cached `claude` path) live on
//  one `NiceServices` instance owned here. Per-window state
//  (`AppState`, `NiceControlSocket`) lives inside each `AppShellView`
//  — every `WindowGroup` instance gets its own, so opening a second
//  window via ⌘N yields a fully isolated window with its own tabs,
//  panes, and pty sessions.
//
//  Theme is driven via `NSApp.appearance` from `Tweaks` rather than
//  `.preferredColorScheme`, because the latter can't clear a
//  previously applied non-nil scheme — switching back to "Match
//  system" would leave windows pinned to the last explicit choice.
//

import SwiftUI

@main
struct NiceApp: App {
    @StateObject private var services = NiceServices()
    // Owns `applicationShouldTerminate` so ⌘Q / Quit-menu goes through
    // the "you have live panes" confirmation before willTerminate fires.
    // The adaptor instantiates the delegate before SwiftUI builds the
    // body, so we late-bind the registry pointer in `onAppear`.
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate

    var body: some Scene {
        WindowGroup {
            AppShellView()
                .environmentObject(services)
                .environmentObject(services.tweaks)
                .environmentObject(services.shortcuts)
                .environmentObject(services.fontSettings)
                .environment(\.palette, services.tweaks.theme.palette)
                .tint(services.tweaks.accent.color)
                .onAppear {
                    AppDelegate.registryProvider = { [weak services] in
                        services?.registry
                    }
                    services.bootstrap()
                }
        }
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentSize)

        // ⌘, binds to this scene automatically on macOS. SettingsView
        // sets its own 640×440 frame, but we repeat it here so the
        // window resizes correctly even before the child view lays out.
        Settings {
            SettingsView()
                .environmentObject(services)
                .environmentObject(services.tweaks)
                .environmentObject(services.shortcuts)
                .environmentObject(services.fontSettings)
                .environment(\.palette, services.tweaks.theme.palette)
                .frame(width: 640, height: 440)
                .tint(services.tweaks.accent.color)
        }
    }
}
