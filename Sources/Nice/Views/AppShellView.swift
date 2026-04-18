//
//  AppShellView.swift
//  Nice
//
//  Phase 1 placeholder 3-column layout used to verify the theme pipeline.
//  Later phases replace each column with real sidebar / chat / terminal
//  views.
//

import SwiftUI

struct AppShellView: View {
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        HStack(spacing: 0) {
            // Left: floating inset "sidebar card"
            sidebarPlaceholder
                .frame(width: 240)
                .padding(10)

            // Middle: chat pane (flex)
            chatPlaceholder
                .frame(maxWidth: .infinity, maxHeight: .infinity)

            // Right: terminal pane
            terminalPlaceholder
                .frame(width: 400)
        }
        .background(Color.niceBg2(scheme).ignoresSafeArea())
    }

    // MARK: - Columns

    private var sidebarPlaceholder: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(Color.niceBg2(scheme))
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .strokeBorder(Color.niceLine(scheme), lineWidth: 1)
            Text("Sidebar")
                .font(.niceUI)
                .foregroundStyle(Color.niceInk2(scheme))
        }
        .shadow(color: Color.black.opacity(0.25), radius: 20, x: 0, y: 10)
    }

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
        .frame(width: 1180, height: 680)
        .preferredColorScheme(.light)
}

#Preview("Dark") {
    AppShellView()
        .frame(width: 1180, height: 680)
        .preferredColorScheme(.dark)
}
