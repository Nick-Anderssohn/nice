//
//  AppShellView.swift
//  Nice
//
//  Per-window root: owns this window's `AppState` as a `@StateObject`
//  (so every `WindowGroup` instance gets its own), bridges SwiftUI's
//  per-scene `@SceneStorage` to AppState for things like the collapsed
//  sidebar state, and registers the window with the app-wide
//  `WindowRegistry` once AppKit hands us a real `NSWindow`.
//
//  Two layout modes, toggled by `appState.sidebarCollapsed`:
//
//  • Expanded — floor-to-ceiling floating sidebar card on the left
//    (Xcode / Finder / Mail style), with the toolbar + main content
//    stacked to its right. Native traffic lights float on top of the
//    card's upper 52pt.
//
//  • Collapsed — no sidebar column at all. A small chrome rectangle
//    sits in the upper-left as a seamless continuation of the top
//    bar, just wide enough to host the three native traffic lights
//    plus a restore icon that re-expands the sidebar. The main
//    terminal area extends all the way to the window's left edge
//    beneath it.
//
//  The sidebar's expanded background depends on the active palette:
//    • `.nice`  — flat `niceBg2` panel
//    • `.macOS` — `NSVisualEffectView` with `.sidebar` material and
//                 `.behindWindow` blending, so the OS's Desktop Tinting
//                 mixes wallpaper color into the chrome.
//

import AppKit
import SwiftUI

struct AppShellView: View {
    @EnvironmentObject private var services: NiceServices
    @SceneStorage("sidebarCollapsed") private var storedSidebarCollapsed: Bool = false
    /// Stable per-window id that survives quits via scene storage.
    /// `AppState` uses it to look up this window's entry in
    /// `sessions.json` so restore rebuilds the right tabs.
    @SceneStorage("windowSessionId") private var storedWindowSessionId: String = ""

    var body: some View {
        AppShellHost(
            services: services,
            initialSidebarCollapsed: storedSidebarCollapsed,
            windowSessionId: storedWindowSessionId,
            sidebarCollapsedBinding: $storedSidebarCollapsed,
            windowSessionIdBinding: $storedWindowSessionId
        )
    }
}

/// The stateful inner view. Splitting it out lets us read
/// `@EnvironmentObject services` and `@SceneStorage` values from
/// `AppShellView` before constructing the per-window `AppState`
/// (`@StateObject` can't reach environment in its own `init`).
private struct AppShellHost: View {
    @EnvironmentObject private var tweaks: Tweaks
    @EnvironmentObject private var shortcuts: KeyboardShortcuts
    @EnvironmentObject private var fontSettings: FontSettings
    @Environment(\.colorScheme) private var scheme

    @StateObject private var appState: AppState
    let services: NiceServices
    @Binding var sidebarCollapsedBinding: Bool
    @Binding var windowSessionIdBinding: String

    /// True while the cursor is over the floating peek sidebar. Keeps
    /// the overlay rendered after the keyboard modifiers are released
    /// so the user can click a tab without holding any keys.
    @State private var peekMousePinned: Bool = false

    init(
        services: NiceServices,
        initialSidebarCollapsed: Bool,
        windowSessionId: String,
        sidebarCollapsedBinding: Binding<Bool>,
        windowSessionIdBinding: Binding<String>
    ) {
        self.services = services
        _appState = StateObject(wrappedValue: AppState(
            services: services,
            initialSidebarCollapsed: initialSidebarCollapsed,
            initialMainCwd: nil,
            windowSessionId: windowSessionId
        ))
        _sidebarCollapsedBinding = sidebarCollapsedBinding
        _windowSessionIdBinding = windowSessionIdBinding
    }

    private var palette: Palette { tweaks.activeChromePalette }

    /// Body text for the "processes still running" alert. Lists the
    /// busy pane(s) so the user knows what they'd be force-quitting.
    private func pendingCloseMessage(_ request: PendingCloseRequest) -> String {
        let scopeHint: String
        switch request.scope {
        case .pane:
            scopeHint = "Closing this pane will force it to quit."
        case .tab:
            scopeHint = "Closing this tab will force everything in it to quit."
        }
        let list = request.busyPanes.joined(separator: ", ")
        let running = request.busyPanes.count == 1
            ? "\(list) is still running."
            : "These are still running: \(list)."
        return "\(running) \(scopeHint)"
    }

    var body: some View {
        shell
        .ignoresSafeArea(edges: .top)
        .background(
            // Host-window reach-through: once the shell is mounted,
            // register the window so shortcuts and termination route to
            // this AppState, and nudge the native traffic lights deeper
            // into the sidebar card so they don't sit flush against the
            // rounded corner.
            WindowAccessor { window in
                TrafficLightNudger.nudge(window: window, dx: 8, dy: -8)
                TitleBarZoomMonitor.install()
                services.registry.register(appState: appState, window: window)
            }
        )
        .background(windowBackground.ignoresSafeArea())
        .environment(\.palette, palette)
        .environmentObject(appState)
        .alert(
            "Processes are still running",
            isPresented: Binding(
                get: { appState.pendingCloseRequest != nil },
                set: { if !$0 { appState.cancelPendingClose() } }
            ),
            presenting: appState.pendingCloseRequest
        ) { _ in
            Button("Cancel", role: .cancel) { appState.cancelPendingClose() }
            Button("Force quit", role: .destructive) { appState.confirmPendingClose() }
        } message: { request in
            Text(pendingCloseMessage(request))
        }
        .onAppear {
            // Brand-new scene: write the id AppState minted back
            // into SceneStorage so this window restores the same
            // slot on relaunch.
            if windowSessionIdBinding.isEmpty {
                windowSessionIdBinding = appState.windowSessionId
            }
            appState.updateTerminalFontFamily(tweaks.terminalFontFamily)
            // updateScheme before updateTerminalTheme — see
            // `AppState.makeSession` for why ordering matters (the
            // chrome-coupled Nice Defaults read the session's cached
            // scheme, so it must be current before their bg / fg
            // derivation runs).
            appState.updateScheme(scheme, palette: palette, accent: tweaks.accent.nsColor)
            appState.updateTerminalTheme(
                tweaks.effectiveTerminalTheme(for: scheme, catalog: services.terminalThemeCatalog)
            )
            appState.updateTerminalFontSize(fontSettings.terminalFontSize)
            appState.updateGpuRendering(tweaks.gpuRendering)
            appState.updateSmoothScrolling(tweaks.smoothScrolling)
        }
        .onChange(of: scheme) { _, newScheme in
            appState.updateScheme(newScheme, palette: palette, accent: tweaks.accent.nsColor)
            appState.updateTerminalTheme(
                tweaks.effectiveTerminalTheme(for: newScheme, catalog: services.terminalThemeCatalog)
            )
        }
        .onChange(of: palette) { _, newPalette in
            appState.updateScheme(scheme, palette: newPalette, accent: tweaks.accent.nsColor)
        }
        .onChange(of: tweaks.accent) { _, newAccent in
            appState.updateScheme(scheme, palette: palette, accent: newAccent.nsColor)
        }
        .onChange(of: fontSettings.terminalFontSize) { _, newSize in
            appState.updateTerminalFontSize(newSize)
        }
        .onChange(of: tweaks.gpuRendering) { _, newValue in
            appState.updateGpuRendering(newValue)
        }
        .onChange(of: tweaks.smoothScrolling) { _, newValue in
            appState.updateSmoothScrolling(newValue)
        }
        .onChange(of: tweaks.terminalThemeLightId) { _, _ in
            // Only applies if the active scheme is light — otherwise the
            // dark slot is active and this change is latent until the
            // next scheme flip.
            guard scheme == .light else { return }
            appState.updateTerminalTheme(
                tweaks.effectiveTerminalTheme(for: scheme, catalog: services.terminalThemeCatalog)
            )
        }
        .onChange(of: tweaks.terminalThemeDarkId) { _, _ in
            guard scheme == .dark else { return }
            appState.updateTerminalTheme(
                tweaks.effectiveTerminalTheme(for: scheme, catalog: services.terminalThemeCatalog)
            )
        }
        .onChange(of: tweaks.terminalFontFamily) { _, newValue in
            appState.updateTerminalFontFamily(newValue)
        }
        // Per-window SceneStorage bridges: persist this window's
        // collapsed-sidebar state across relaunch. Also clear any
        // in-flight peek state when the sidebar is explicitly
        // expanded (via ⌘B or the chevron) so we don't carry a stale
        // peek flag into the expanded shell.
        .onChange(of: appState.sidebarCollapsed) { _, new in
            sidebarCollapsedBinding = new
            if !new {
                appState.sidebarPeeking = false
                peekMousePinned = false
            }
        }
        // Mirror AppState's windowSessionId back to scene storage —
        // restore may have adopted a different slot (e.g. bootstrap)
        // and the pairing must survive relaunch.
        .onChange(of: appState.windowSessionId) { _, new in
            if new != windowSessionIdBinding {
                windowSessionIdBinding = new
            }
        }
    }

    // MARK: - Layout modes

    @ViewBuilder
    private var shell: some View {
        if appState.sidebarCollapsed {
            collapsedShell
        } else {
            expandedShell
        }
    }

    /// Floor-to-ceiling floating sidebar card on the left, toolbar + main
    /// content stacked to its right.
    private var expandedShell: some View {
        HStack(spacing: 0) {
            floatingSidebarCard

            VStack(spacing: 0) {
                WindowToolbarView()
                mainContent
            }
        }
    }

    /// The 240pt floating sidebar card. Used both as the leading column
    /// of `expandedShell` and as a transient overlay above the
    /// terminal in `collapsedShell` when a sidebar-tab shortcut is
    /// peeking. The native traffic lights are positioned in absolute
    /// window coordinates by macOS and render on top of whatever's
    /// here; the 52pt clear spacer inside the VStack keeps the
    /// sidebar's own content clear of them visually. The card is
    /// inset so that the traffic lights (~x:20, y:15, 14pt diameter)
    /// have at least ~8pt of clearance on both sides: the leading edge
    /// sits at ~12pt and the top edge at ~40pt. Bottom mirrors the top
    /// so the card looks visually symmetric around the vertical axis.
    /// No trailing padding — the gap between sidebar and main content
    /// is just the card's own edge. Tweak pixel values here if it
    /// starts to look off relative to Xcode in dark mode.
    private var floatingSidebarCard: some View {
        SidebarBackground(palette: palette, scheme: scheme) {
            VStack(spacing: 0) {
                // Reserve the traffic-light safe zone at the top of
                // the sidebar. 52pt matches the classic hidden-title-
                // bar chrome height and aligns with the toolbar row
                // on the right. WindowDragRegion makes this strip
                // behave like a title bar (drag + double-click zoom);
                // the traffic lights themselves are standard
                // NSButtons layered above and keep their own clicks.
                // The collapse toggle lives at the trailing edge so
                // its vertical band matches the collapsed cap's
                // restore button.
                WindowDragRegion()
                    .frame(height: 52)
                    .overlay(alignment: .topTrailing) {
                        // Button top at strip-y=8 places the 24pt
                        // button's center at strip-y=20, i.e. 26pt
                        // from the window top — matching the collapsed
                        // cap's button (40pt card, 6pt top padding,
                        // HStack-centered button → same window-y=26).
                        SidebarToggleButton(
                            help: "Collapse sidebar",
                            accessibilityId: "sidebar.collapse"
                        ) {
                            appState.toggleSidebar()
                        }
                        .padding(.top, 8)
                        .padding(.trailing, 10)
                    }
                SidebarView()
            }
        }
        .frame(width: 240)
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
        // Lift the card above the main content in Z so its shadow
        // isn't clipped by the opaque nicePanel / niceChrome
        // backgrounds of the toolbar and terminal host next to it.
        .zIndex(1)
    }

    /// Collapsed: no sidebar column. A small floating card sits in the
    /// upper-left behind the three traffic lights and hosts a restore
    /// icon to re-expand the sidebar. The card is constrained within the
    /// top bar's 52pt vertical band — same styling as the expanded
    /// sidebar card (rounded corners, border, shadow, sidebar material),
    /// just sized down. The main content fills the full width below.
    ///
    /// When a sidebar-tab shortcut is held (`appState.sidebarPeeking`)
    /// or the cursor is pinning a peek (`peekMousePinned`), the full
    /// 240pt sidebar card overlays the terminal at top-leading without
    /// reflowing the layout below.
    private var collapsedShell: some View {
        VStack(spacing: 0) {
            HStack(spacing: 0) {
                collapsedCap
                WindowToolbarView()
            }
            mainContent
        }
        .overlay(alignment: .topLeading) {
            if appState.sidebarPeeking || peekMousePinned {
                floatingSidebarCard
                    .onHover { hovering in
                        peekMousePinned = hovering
                    }
                    .transition(
                        .move(edge: .leading).combined(with: .opacity)
                    )
                    // Sit above the collapsedCap (zIndex 1) so the
                    // peek visually replaces it, not slides under.
                    .zIndex(2)
            }
        }
        .animation(.easeOut(duration: 0.15), value: appState.sidebarPeeking)
        .animation(.easeOut(duration: 0.15), value: peekMousePinned)
    }

    /// Floating card that lives in the top bar's upper-left corner when
    /// the sidebar is collapsed. The leading 82pt reserves space for the
    /// three native traffic lights (nudged to x≈28 with 14pt diameter
    /// and 6pt spacing, last edge ≈82); the restore button sits just
    /// past them. Vertical padding centers it within the 52pt top bar
    /// row so it reads as a distinct card rather than blending into
    /// either the chrome above or the content below.
    private var collapsedCap: some View {
        SidebarBackground(palette: palette, scheme: scheme) {
            HStack(spacing: 0) {
                // Leading 82pt hosts the traffic lights; the drag region
                // underneath makes that strip (and any empty space past
                // the restore button) behave like a title bar for
                // drag + double-click zoom.
                WindowDragRegion().frame(width: 82)
                SidebarToggleButton(
                    help: "Expand sidebar",
                    accessibilityId: "sidebar.expand"
                ) {
                    appState.toggleSidebar()
                }
                WindowDragRegion()
            }
        }
        .frame(width: 124, height: 40)
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
        .padding(.vertical, 6)
        // Lift above the adjacent toolbar's opaque chrome so the shadow
        // isn't clipped at the card's trailing edge.
        .zIndex(1)
    }

    // MARK: - Window background

    /// Bleed the terminal theme's background across the entire window
    /// when the user has picked a non-Nice-default theme — otherwise
    /// the chrome-colored gap between the floating sidebar and the
    /// terminal pane, and the 12pt strip under the toolbar, spoil the
    /// look by revealing chrome against (e.g.) Solarized cream.
    ///
    /// Nice Defaults fall back to the existing per-palette rules so
    /// the macOS palette keeps its vibrant `NSVisualEffectView`
    /// sidebar blending (the sidebar pulls wallpaper pixels through
    /// the transparent `windowBackgroundColor`) and the Nice palette
    /// keeps its flat `nicePanel` underlay.
    @ViewBuilder
    private var windowBackground: some View {
        // Toolbar chrome runs edge-to-edge across the window top so
        // the 6pt gap around the sidebar card's top / leading edges
        // reveals the same white band that the toolbar shows to the
        // right of the sidebar — otherwise the strip cuts off at the
        // sidebar's left edge, which looks asymmetric.
        //
        // 52pt matches `WindowToolbarView`'s fixed height; the 1pt
        // bottom border matches its `.overlay(alignment: .bottom)`
        // `niceLine` separator so the toolbar's visual footprint is
        // continuous across the full window width.
        VStack(spacing: 0) {
            ZStack(alignment: .bottom) {
                Color.niceChrome(scheme, palette)
                Rectangle()
                    .fill(Color.niceLine(scheme, palette))
                    .frame(height: 1)
            }
            .frame(height: 52)

            terminalBackgroundColor
        }
    }

    /// The active terminal theme's background color, used to paint
    /// the window body (area around the terminal pane, including
    /// the gap behind the floating sidebar card) so every theme —
    /// Nice Defaults included — bleeds a unified color behind the
    /// terminal instead of revealing chrome underneath.
    private var terminalBackgroundColor: Color {
        let theme = tweaks.effectiveTerminalTheme(
            for: scheme,
            catalog: services.terminalThemeCatalog
        )
        return Color(nsColor: theme.background.nsColor)
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
                .background(terminalBackgroundColor)
        } else {
            // Transient: no pane currently hosted (e.g. every tab in
            // every project just dissolved — the app is about to
            // terminate, or the user emptied the Terminals group and
            // hasn't hit `+` yet).
            terminalBackgroundColor
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .padding(.leading, 20)
        }
    }
}

// MARK: - Sidebar toggle button

/// The chevron that toggles `sidebarCollapsed`. Used both in the collapsed
/// top-bar cap (to expand) and in the expanded sidebar's 52pt top strip
/// (to collapse). Styling mirrors `SidebarIconButton` so the hover
/// feedback feels consistent with the sidebar's own controls.
private struct SidebarToggleButton: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let help: String
    let accessibilityId: String
    let action: () -> Void

    @State private var hover = false

    var body: some View {
        Image(systemName: "sidebar.left")
            .font(.system(size: 14, weight: .regular))
            .foregroundStyle(Color.niceInk2(scheme, palette))
            .frame(width: 24, height: 24)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(hover ? Color.niceInk(scheme, palette).opacity(0.08) : Color.clear)
            )
            .contentShape(Rectangle())
            .onHover { hover = $0 }
            .onTapGesture { action() }
            .help(help)
            .accessibilityIdentifier(accessibilityId)
    }
}

#Preview("Light") {
    AppShellView()
        .environmentObject(NiceServices())
        .environmentObject(Tweaks())
        .environmentObject(KeyboardShortcuts())
        .environmentObject(FontSettings())
        .frame(width: 1180, height: 680)
        .preferredColorScheme(.light)
}

#Preview("Dark") {
    AppShellView()
        .environmentObject(NiceServices())
        .environmentObject(Tweaks())
        .environmentObject(KeyboardShortcuts())
        .environmentObject(FontSettings())
        .frame(width: 1180, height: 680)
        .preferredColorScheme(.dark)
}
