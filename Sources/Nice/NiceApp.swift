//
//  NiceApp.swift
//  Nice
//
//  Phase 1 scaffold: single WindowGroup hosting the empty 3-column
//  AppShellView. No real features wired yet.
//

import SwiftUI

@main
struct NiceApp: App {
    var body: some Scene {
        WindowGroup {
            AppShellView()
                .frame(minWidth: 1180, minHeight: 680)
                .preferredColorScheme(nil) // inherits OS appearance
        }
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentMinSize)
    }
}
