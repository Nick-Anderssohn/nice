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

import AppKit
import SwiftUI

/// Real `@main` entry point. When the unit-test bundle is injected
/// into the app (xctest sets `XCTestConfigurationFilePath` and loads
/// `XCTestCase`), skip the SwiftUI launch entirely and run a bare
/// AppKit event loop instead.
///
/// Why: on the GitHub Actions `macos-26` runner (build 25D125,
/// SwiftUI 7.3.2, AttributeGraph 7.0.80), SwiftUI's internal
/// AppDelegate adapter crashes inside `applicationDidChangeScreenParameters`
/// while still in `applicationWillFinishLaunching`, with
/// `AG::precondition_failure: setting value during update`. The crash
/// is in SwiftUI's own scene-graph init — nothing in our app touches
/// it — and reliably aborts the unit-test host before any test runs.
/// Not reproducible on local macOS 26.3.1 (build 25D2128).
///
/// Unit tests don't need a real SwiftUI scene — they construct the
/// types they need (AppState, FileBrowserStore, …) directly. UI tests
/// run in a separate `XCUIApplication` process that doesn't have
/// XCTest injected, so they take the production branch and get the
/// real app.
@main
struct NiceAppLauncher {
    static func main() {
        if NSClassFromString("XCTestCase") != nil {
            // Bare AppKit run loop. `applicationDidFinishLaunching`
            // still fires, which is what `libXCTestBundleInject.dylib`
            // observes to discover and run the test bundle. xctest
            // exits the process when tests complete.
            let app = NSApplication.shared
            let delegate = TestHostStubDelegate()
            app.delegate = delegate
            app.run()
        } else {
            NiceApp.main()
        }
    }
}

/// Minimal NSApplicationDelegate used only when the host is hosting a
/// unit-test injection. No SwiftUI, no scenes, no windows — just
/// enough to satisfy AppKit so the XCTest bundle injector can fire.
@MainActor
private final class TestHostStubDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {}
}

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
                .environmentObject(services.editorDetector)
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
                .environmentObject(services.editorDetector)
                .environment(\.palette, services.tweaks.activeChromePalette)
                .frame(width: 640, height: 440)
                .tint(services.tweaks.accent.color)
        }
    }
}
