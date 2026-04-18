//
//  AppShellView.swift
//  Nice
//
//  Floor-to-ceiling sidebar on the left (matches Xcode / Finder / Mail);
//  the native traffic lights float on top of its upper 52pt. On the
//  right, a thin toolbar hosts the brand + tab pills, and the main area
//  shows exactly one pane — the active pane of the active tab.
//
//  The sidebar's background depends on the active palette:
//    • `.nice`  — flat `niceBg2` panel
//    • `.macOS` — `NSVisualEffectView` with `.sidebar` material and
//                 `.behindWindow` blending, so the OS's Desktop Tinting
//                 mixes wallpaper color into the chrome.
//
//  The "Quit NICE?" alert still hangs off `appState.showQuitPrompt`.
//

import AppKit
import SwiftUI

struct AppShellView: View {
    @EnvironmentObject private var appState: AppState
    @EnvironmentObject private var tweaks: Tweaks
    @Environment(\.colorScheme) private var scheme

    private var palette: Palette { tweaks.theme.palette }

    var body: some View {
        HStack(spacing: 0) {
            // Xcode-style floating sidebar card. The native traffic
            // lights are positioned in absolute window coordinates by
            // macOS and render on top of whatever's here; the 52pt
            // clear spacer inside the VStack keeps the sidebar's own
            // content clear of them visually. The card is inset so
            // that the traffic lights (~x:20, y:15, 14pt diameter)
            // have at least ~8pt of clearance on both sides: the
            // leading edge sits at ~12pt and the top edge at ~40pt.
            // Bottom mirrors the top so the card looks visually
            // symmetric around the vertical axis. No trailing
            // padding — the gap between sidebar and main content is
            // just the card's own edge. Tweak pixel values here if
            // it starts to look off relative to Xcode in dark mode.
            SidebarBackground(palette: palette, scheme: scheme) {
                VStack(spacing: 0) {
                    // Reserve the traffic-light safe zone at the top of
                    // the sidebar. 52pt matches the classic hidden-title-
                    // bar chrome height and aligns with the toolbar row
                    // on the right.
                    Color.clear.frame(height: 52)
                    SidebarView()
                }
            }
            .frame(width: appState.sidebarCollapsed ? 52 : 240)
            .animation(.easeInOut(duration: 0.22), value: appState.sidebarCollapsed)
            .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .strokeBorder(
                        Color.niceLine(scheme, palette).opacity(0.5),
                        lineWidth: 0.5
                    )
            )
            .shadow(color: Color.black.opacity(0.15), radius: 4, x: 0, y: 2)
            .padding(.leading, 6)
            .padding(.top, 6)
            .padding(.bottom, 6)

            VStack(spacing: 0) {
                WindowToolbarView()
                mainContent
            }
        }
        .ignoresSafeArea(edges: .top)
        .background(windowBackground.ignoresSafeArea())
        .environment(\.palette, palette)
        .alert("Quit NICE?", isPresented: $appState.showQuitPrompt) {
            Button("Quit", role: .destructive) { NSApp.terminate(nil) }
            Button("Cancel", role: .cancel) { appState.cancelQuitPrompt() }
        } message: {
            Text("Your last terminal just exited. You still have open sessions.")
        }
        .task {
            await appState.bootstrap()
        }
        .onAppear { appState.updateScheme(scheme, palette: palette) }
        .onChange(of: scheme) { _, newScheme in
            appState.updateScheme(newScheme, palette: palette)
        }
        .onChange(of: palette) { _, newPalette in
            appState.updateScheme(scheme, palette: newPalette)
        }
    }

    // MARK: - Window background

    /// In the macOS palette the window background is transparent so the
    /// NSVisualEffectView sidebar can pull wallpaper pixels through the
    /// window without a solid color blocking the effect at the seam
    /// between sidebar and main content. The main content area paints
    /// its own `nicePanel` underlay.
    @ViewBuilder
    private var windowBackground: some View {
        switch palette {
        case .nice:  Color.nicePanel(scheme, palette)
        case .macOS: Color(nsColor: .windowBackgroundColor)
        }
    }

    // MARK: - Main content

    @ViewBuilder
    private var mainContent: some View {
        // Leading padding here mirrors WindowToolbarView's `.padding(.trailing, 20)`
        // so the terminal text has the same breathing room from the sidebar
        // card that the tab strip gets from the window's right edge.
        // Worth a visual refinement pass if it looks off against Xcode.
        if let tabId = appState.activeTabId,
           let tab = appState.tab(for: tabId),
           let paneId = tab.activePaneId,
           let session = appState.ptySessions[tabId],
           let view = session.panes[paneId] {
            TerminalHost(view: view, focus: true)
                .id(paneId)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .padding(.top, 12)
                .padding(.leading, 20)
                .background(Color.nicePanel(scheme, palette))
        } else {
            // Transient: no pane currently hosted (e.g. Terminals tab
            // with its last pane just exited, awaiting the quit alert).
            Color.nicePanel(scheme, palette)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .padding(.leading, 20)
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
