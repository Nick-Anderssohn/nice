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

/// Tracks whether the current key window is in native full screen, so
/// the View-menu command can show "Enter" vs "Exit Full Screen". Nice
/// declares no `.commands` otherwise, which is why the standard
/// full-screen menu item (and its ⌃⌘F shortcut) was absent entirely —
/// the green traffic-light button was the only way in.
@MainActor
@Observable
final class FullScreenTracker {
    var keyWindowIsFullScreen: Bool = false

    @ObservationIgnored
    private var observers: [NSObjectProtocol] = []

    init() {
        let center = NotificationCenter.default
        // `object: nil` → observe every window; we recompute from
        // whichever window is key, so the title follows the frontmost
        // window across enter/exit transitions and key-window changes.
        let names: [NSNotification.Name] = [
            NSWindow.didEnterFullScreenNotification,
            NSWindow.didExitFullScreenNotification,
            NSWindow.didBecomeKeyNotification,
            NSWindow.didResignKeyNotification,
        ]
        for name in names {
            observers.append(
                center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                    MainActor.assumeIsolated { self?.recompute() }
                }
            )
        }
    }

    private func recompute() {
        keyWindowIsFullScreen =
            NSApp.keyWindow?.styleMask.contains(.fullScreen) ?? false
    }
}

struct NiceApp: App {
    @State private var services = NiceServices()
    @State private var fullScreen = FullScreenTracker()
    // Owns `applicationShouldTerminate` so ⌘Q / Quit-menu goes through
    // the "you have live panes" confirmation before willTerminate fires.
    // The adaptor instantiates the delegate before SwiftUI builds the
    // body, so we late-bind the registry pointer in `onAppear`.
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate

    var body: some Scene {
        // Value-presenting window group: the presented value is a
        // tear-off pairing token (`String?`). `$tearOffToken` is the
        // value SwiftUI hands to the window it opens —
        //   • nil      — plain ⌘N and AppKit auto-restore (no token).
        //   • a UUID    — a tear-off (`PaneTearOffController`) or a
        //                 launch fan-out `openWindow`, each minting a
        //                 fresh token per call.
        // `AppShellHost.task` consumes the seed deposited under its
        // token (if any) via `services.consumeTearOffSeed(token:)`, so a
        // seed is paired to ITS window explicitly rather than to "the
        // next window to mount" — killing the seed-steal class the old
        // FIFO had. The fan-out mints a distinct token per call so each
        // `openWindow(id: "main", value:)` is forced to open a NEW
        // window: a plain nil value can de-dup to the existing nil-value
        // window, which would collapse multi-window restore to one.
        WindowGroup(id: "main", for: String.self) { $tearOffToken in
            AppShellView(tearOffToken: tearOffToken)
                .environment(services)
                .environment(services.tweaks)
                .environment(services.shortcuts)
                .environment(services.fontSettings)
                .environment(services.fileBrowserSortSettings)
                .environment(services.terminalThemeCatalog)
                .environment(services.releaseChecker)
                .environment(services.editorDetector)
                .environment(\.palette, services.tweaks.activeChromePalette)
                .tint(services.tweaks.accent.color)
                .onAppear {
                    AppDelegate.registryProvider = { [weak services] in
                        services?.registry
                    }
                }
        }
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentSize)
        .commands {
            // Replace SwiftUI's auto File ▸ New Window item. The default
            // item opens the value-presenting `WindowGroup(id: "main",
            // for: String.self)` with a NIL value, and SwiftUI can de-dup
            // a nil-value window to the existing nil-value (launch) window
            // — focusing it instead of opening a new one. That collapses
            // ⌘N to a no-op and regresses multi-window isolation.
            //
            // Every OTHER open path already mints a fresh UUID token (the
            // launch fan-out at AppShellView, and tear-off via
            // PaneTearOffController), forcing a distinct presented value
            // so the group is obliged to open a NEW window. ⌘N was the one
            // path still presenting nil. We mint a token here too: the
            // token has NO deposited tear-off seed, so the new window's
            // `consumeTearOffSeed(token:)` returns nil and it starts
            // normally — behaviourally identical to a plain ⌘N, but never
            // focus-existing.
            CommandGroup(replacing: .newItem) {
                NewWindowButton()
            }
            // Restore the standard full-screen menu item + ⌃⌘F. It's in
            // the View menu (where macOS conventionally puts it) via the
            // `.sidebar` placement. `toggleFullScreen` works because the
            // window already advertises `.fullScreenPrimary` (the green
            // button enters full screen); only the menu binding was
            // missing.
            CommandGroup(after: .sidebar) {
                Button(
                    fullScreen.keyWindowIsFullScreen
                        ? "Exit Full Screen"
                        : "Enter Full Screen"
                ) {
                    NSApp.keyWindow?.toggleFullScreen(nil)
                }
                .keyboardShortcut("f", modifiers: [.control, .command])
            }
        }

        // ⌘, binds to this scene automatically on macOS. SettingsView
        // declares its own min / ideal / max frame; mirroring it here
        // means the window picks the right initial size before
        // SettingsView's body has run, while still letting users drag
        // the window edges (`.windowResizability(.contentSize)`).
        Settings {
            SettingsView()
                .environment(services)
                .environment(services.tweaks)
                .environment(services.shortcuts)
                .environment(services.fontSettings)
                .environment(services.terminalThemeCatalog)
                .environment(services.releaseChecker)
                .environment(services.editorDetector)
                .environment(\.palette, services.tweaks.activeChromePalette)
                .frame(
                    minWidth: 560, idealWidth: 640, maxWidth: .infinity,
                    minHeight: 380, idealHeight: 440, maxHeight: .infinity
                )
                .tint(services.tweaks.accent.color)
        }
        .windowResizability(.contentMinSize)
    }
}

/// The File ▸ New Window menu item (⌘N), wired to mint a fresh tear-off
/// pairing token per invocation.
///
/// Lives in its own `View` rather than calling `openWindow` from the
/// `App` body so it can hold `@Environment(\.openWindow)` — the action is
/// reliably populated in a `View` rendered inside a `CommandGroup`, and
/// reading it from the `App`/`Scene` level is brittle.
///
/// Why a token at all: the scene is `WindowGroup(id: "main", for:
/// String.self)`. A nil presented value can de-dup to the existing
/// nil-value (launch) window, so plain ⌘N may focus-existing instead of
/// opening a new window. A fresh `UUID().uuidString` is a distinct value,
/// forcing a NEW window. The token has no deposited seed, so the new
/// window consumes nothing and starts as a pristine ⌘N window would.
private struct NewWindowButton: View {
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Button("New Window") {
            openWindow(id: "main", value: UUID().uuidString)
        }
        .keyboardShortcut("n", modifiers: .command)
    }
}
