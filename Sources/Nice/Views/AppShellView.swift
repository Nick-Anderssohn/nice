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
                SidebarView()
                    .frame(width: appState.sidebarCollapsed ? AppState.sidebarCollapsedWidth : appState.sidebarWidth)
                    .animation(.easeInOut(duration: 0.22), value: appState.sidebarCollapsed)
                    .background(
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .fill(.ultraThinMaterial)
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .strokeBorder(Color.niceLine(scheme).opacity(0.5), lineWidth: 0.5)
                    )
                    .overlay(alignment: .trailing) {
                        SidebarResizeHandle()
                    }
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
                    TerminalHost(view: chat, focus: true)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .padding(.top, 12)
                        .background(Color.nicePanel(scheme))

                    CompanionPaneView(tabId: activeId)
                        .frame(width: 400)
                        .overlay(alignment: .leading) {
                            Rectangle()
                                .fill(Color.niceLine(scheme))
                                .frame(width: 1)
                        }
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
            TerminalHost(view: appState.mainTerminal.view, focus: true)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .padding(.top, 12)
                .background(Color.nicePanel(scheme))
        }
    }
}

// MARK: - Sidebar resize handle

private struct SidebarResizeHandle: View {
    @EnvironmentObject private var appState: AppState

    @State private var hover = false
    @State private var dragging = false
    @State private var initialWidth: CGFloat = 0

    var body: some View {
        Rectangle()
            .fill(Color.clear)
            .frame(width: 6)
            .frame(maxHeight: .infinity)
            .contentShape(Rectangle())
            .onHover { hovering in
                hover = hovering
                if hovering || dragging {
                    NSCursor.resizeLeftRight.push()
                } else {
                    NSCursor.pop()
                }
            }
            .gesture(
                DragGesture(minimumDistance: 1)
                    .onChanged { value in
                        if !dragging {
                            dragging = true
                            initialWidth = appState.sidebarCollapsed
                                ? AppState.sidebarCollapsedWidth
                                : appState.sidebarWidth
                        }
                        let newWidth = initialWidth + value.translation.width
                        appState.resizeSidebar(to: newWidth)
                    }
                    .onEnded { _ in
                        dragging = false
                        if !hover { NSCursor.pop() }
                    }
            )
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
