//
//  AppShellView.swift
//  Nice
//
//  Phase 2 shell: the left column now hosts the real `SidebarView`, wired
//  to the shared `AppState`. Chat + terminal placeholders still come
//  in Phase 3/4.
//

import SwiftUI

struct AppShellView: View {
    @Environment(\.colorScheme) private var scheme

    var body: some View {
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
                .padding(10)

            // Middle: chat pane (flex) — Phase 3 placeholder.
            chatPlaceholder
                .frame(maxWidth: .infinity, maxHeight: .infinity)

            // Right: terminal pane — Phase 4 placeholder.
            terminalPlaceholder
                .frame(width: 400)
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
