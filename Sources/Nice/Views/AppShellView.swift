//
//  AppShellView.swift
//  Nice
//
//  Phase 3: root restructured as a vertical stack — the new
//  `WindowToolbarView` sits above the existing 3-column layout so the
//  sidebar, chat, and terminal all hang under a unified top bar. The
//  sidebar's top inset is trimmed so its card aligns flush with the
//  toolbar's bottom divider.
//
//  Chat + terminal placeholders are unchanged; real panes land in later
//  phases.
//

import SwiftUI

struct AppShellView: View {
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

                // Middle: chat pane (flex) — Phase 3 placeholder.
                chatPlaceholder
                    .frame(maxWidth: .infinity, maxHeight: .infinity)

                // Right: terminal pane — Phase 4 placeholder.
                terminalPlaceholder
                    .frame(width: 400)
            }
        }
        .background(Color.niceBg2(scheme).ignoresSafeArea())
    }

    // MARK: - Columns

    private var chatPlaceholder: some View {
        ZStack {
            Color.nicePanel(scheme)
            Text("Chat pane")
                .font(.niceUI)
                .foregroundStyle(Color.niceInk(scheme))
        }
    }

    private var terminalPlaceholder: some View {
        ZStack {
            Color.niceBg3(scheme)
            Text("Terminal")
                .font(.niceMono)
                .foregroundStyle(Color.niceInk(scheme))
        }
    }
}

#Preview("Light") {
    AppShellView()
        .environmentObject(AppState())
        .frame(width: 1180, height: 680)
        .preferredColorScheme(.light)
}

#Preview("Dark") {
    AppShellView()
        .environmentObject(AppState())
        .frame(width: 1180, height: 680)
        .preferredColorScheme(.dark)
}
