//
//  NiceApp.swift
//  Nice
//
//  Phase 2: the shell now owns a shared `AppState` injected into the view
//  tree via `.environmentObject`.
//

import SwiftUI

@main
struct NiceApp: App {
    @StateObject private var appState = AppState()

    var body: some Scene {
        WindowGroup {
            AppShellView()
                .environmentObject(appState)
                .frame(minWidth: 1180, minHeight: 680)
                .preferredColorScheme(nil) // inherits OS appearance
        }
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentMinSize)
    }
}
