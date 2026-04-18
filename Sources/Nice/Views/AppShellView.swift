//
//  AppShellView.swift
//  Nice
//
//  Three render modes once a tab is selected:
//    1. Claude tab with live chatView  — chat on the left, CompanionPaneView
//       on the right (fixed 400pt), a 1pt `niceLine` divider between them.
//    2. Claude tab with chatView still building (rare edge case while a
//       session warms up) — CompanionPaneView fills the whole column.
//    3. Terminal-only tab (hasClaudePane == false) — CompanionPaneView
//       fills the whole column.
//  With no tab selected, the shared `MainTerminalSession` view takes
//  over the whole column (Main Terminal mode).
//
//  The Main Terminal quit alert hangs off `appState.showQuitPrompt`.
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
                // Left: floating inset "sidebar card"
                SidebarView()
                    .frame(width: 240)
                    .background(
                        RoundedRectangle(cornerRadius: 10, style: .continuous)
                            .fill(Color.niceBg2(scheme))
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 10, style: .continuous)
                            .strokeBorder(Color.niceLine(scheme), lineWidth: 1)
                    )
                    .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                    .shadow(color: Color.black.opacity(0.25), radius: 20, x: 0, y: 10)
                    .padding(.horizontal, 10)
                    .padding(.top, 10)
                    .padding(.bottom, 10)

                mainContent
            }
        }
        .background(Color.niceBg2(scheme).ignoresSafeArea())
        .alert("Quit NICE?", isPresented: $appState.showQuitPrompt) {
            Button("Quit", role: .destructive) { NSApp.terminate(nil) }
            Button("Cancel", role: .cancel) { appState.cancelQuitPrompt() }
        } message: {
            Text("Your Main Terminal just exited. You still have open tabs.")
        }
        .task {
            // Phase 6: boot the in-process MCP server exactly once.
            // `bootstrap()` is idempotent via `NiceMCPServer.isRunning`
            // so a re-render firing `.task` again is harmless.
            await appState.bootstrap()
        }
        .onAppear { appState.updateScheme(scheme) }
        .onChange(of: scheme) { _, newScheme in
            appState.updateScheme(newScheme)
        }
    }

    // MARK: - Middle + right column dispatch

    @ViewBuilder
    private var mainContent: some View {
        if let activeId = appState.activeTabId,
           let tab = appState.tab(for: activeId) {
            let session = appState.session(for: activeId)
            if tab.hasClaudePane {
                if let chat = session.chatView {
                    TerminalHost(view: chat)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .background(Color.nicePanel(scheme))

                    Rectangle()
                        .fill(Color.niceLine(scheme))
                        .frame(width: 1)

                    CompanionPaneView(tabId: activeId)
                        .frame(width: 400)
                } else {
                    // Claude pane advertised but view not yet built —
                    // fall back to the companion pane filling the
                    // column so the tab still shows something live.
                    CompanionPaneView(tabId: activeId)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            } else {
                // Terminal-only tab: companions take the full column.
                CompanionPaneView(tabId: activeId)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        } else {
            TerminalHost(view: appState.mainTerminal.view)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Color.niceBg3(scheme))
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
