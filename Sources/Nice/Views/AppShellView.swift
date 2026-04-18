//
//  AppShellView.swift
//  Nice
//
//  Phase 4: middle + right columns are now real SwiftTerm-backed
//  terminals. When a tab is selected we render its `TabPtySession` pair
//  (chat pane running `claude` or zsh fallback, right pane running zsh).
//  When the "Main terminal" row is selected, the middle column expands
//  to host the shared `MainTerminalSession` view and the right column
//  disappears.
//
//  No ChatPane header is drawn (per the design decision in the chat
//  log). The only divider between chat and terminal is a single 1pt
//  line painted with `niceLine`.
//

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

                // Middle + right: real ptys.
                if let activeId = appState.activeTabId {
                    let session = appState.session(for: activeId)
                    TerminalHost(view: session.chatView)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .background(Color.nicePanel(scheme))

                    // Single 1pt divider between chat and terminal.
                    Rectangle()
                        .fill(Color.niceLine(scheme))
                        .frame(width: 1)

                    TerminalHost(view: session.terminalView)
                        .frame(width: 400)
                        .background(Color.niceBg3(scheme))
                } else {
                    TerminalHost(view: appState.mainTerminal.view)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .background(Color.niceBg3(scheme))
                }
            }
        }
        .background(Color.niceBg2(scheme).ignoresSafeArea())
        .task {
            // Phase 6: boot the in-process MCP server exactly once.
            // `bootstrap()` is idempotent via `NiceMCPServer.isRunning`
            // so a re-render firing `.task` again is harmless.
            await appState.bootstrap()
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
