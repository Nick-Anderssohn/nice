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
                .environmentObject(services.terminalThemeCatalog)
                .environmentObject(services.releaseChecker)
                .environment(\.palette, services.tweaks.activeChromePalette)
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
        // Workaround for a SwiftUI 7.3.2 / AttributeGraph 7.0.80 launch
        // crash on macOS 26.3: SwiftUI's internal AppDelegate calls
        // `applicationDidChangeScreenParameters` during
        // `applicationWillFinishLaunching` and writes to its own scene
        // graph mid-update, tripping AttributeGraph's "setting value
        // during update" precondition. Reproducible on the GitHub
        // Actions `macos-26` runner (1024×768 default display) and not
        // locally. Pinning a `defaultSize` gives SwiftUI an unambiguous
        // initial scene size so it doesn't depend on the screen-
        // parameter computation that races its own scene init.
        .defaultSize(width: 1200, height: 750)

        // ⌘, binds to this scene automatically on macOS. SettingsView
        // sets its own 640×440 frame, but we repeat it here so the
        // window resizes correctly even before the child view lays out.
        Settings {
            SettingsView()
                .environmentObject(services)
                .environmentObject(services.tweaks)
                .environmentObject(services.shortcuts)
                .environmentObject(services.fontSettings)
                .environmentObject(services.terminalThemeCatalog)
                .environmentObject(services.releaseChecker)
                .environment(\.palette, services.tweaks.activeChromePalette)
                .frame(width: 640, height: 440)
                .tint(services.tweaks.accent.color)
        }
    }
}
