//
//  AppShellView.swift
//  Nice
//
//  Single-pane main content. The upper toolbar hosts the tab pills
//  (claude + terminal panes for the active session), and the main area
//  shows exactly one pane — the active pane of the active tab. Built-in
//  tabs (Terminals) and user sessions follow the same shape.
//
//  The "Quit NICE?" alert still hangs off `appState.showQuitPrompt`.
//

import AppKit
import SwiftUI

struct AppShellView: View {
    @EnvironmentObject private var appState: AppState
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(spacing: 0) {
            WindowToolbarView()

            HStack(spacing: 0) {
                SidebarView()
                    .frame(width: appState.sidebarCollapsed ? 52 : 240)
                    .animation(.easeInOut(duration: 0.22), value: appState.sidebarCollapsed)
                    .background(
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .fill(.ultraThinMaterial)
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .strokeBorder(Color.niceLine(scheme).opacity(0.5), lineWidth: 0.5)
                    )
                    .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                    .shadow(color: Color.black.opacity(0.15), radius: 4, x: 0, y: 2)
                    .padding(.horizontal, 10)
                    .padding(.top, 10)
                    .padding(.bottom, 10)

                mainContent
            }
        }
        .ignoresSafeArea(edges: .top)
        .background(Color.nicePanel(scheme).ignoresSafeArea())
        .alert("Quit NICE?", isPresented: $appState.showQuitPrompt) {
            Button("Quit", role: .destructive) { NSApp.terminate(nil) }
            Button("Cancel", role: .cancel) { appState.cancelQuitPrompt() }
        } message: {
            Text("Your last terminal just exited. You still have open sessions.")
        }
        .task {
            await appState.bootstrap()
        }
        .onAppear { appState.updateScheme(scheme) }
        .onChange(of: scheme) { _, newScheme in
            appState.updateScheme(newScheme)
        }
    }

    // MARK: - Main content

    @ViewBuilder
    private var mainContent: some View {
        if let tabId = appState.activeTabId,
           let tab = appState.tab(for: tabId),
           let paneId = tab.activePaneId,
           let session = appState.ptySessions[tabId],
           let view = session.panes[paneId] {
            TerminalHost(view: view, focus: true)
                .id(paneId)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .padding(.top, 12)
                .background(Color.nicePanel(scheme))
        } else {
            // Transient: no pane currently hosted (e.g. Terminals tab
            // with its last pane just exited, awaiting the quit alert).
            Color.nicePanel(scheme)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

#Preview("Light") {
    AppShellView()
        .environmentObject(AppState())
        .environmentObject(Tweaks())
        .frame(width: 1180, height: 680)
        .preferredColorScheme(.light)
}

#Preview("Dark") {
    AppShellView()
        .environmentObject(AppState())
        .environmentObject(Tweaks())
        .frame(width: 1180, height: 680)
        .preferredColorScheme(.dark)
}
