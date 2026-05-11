//
//  AppShellView.swift
//  Nice
//
//  Per-window root: owns this window's `AppState` as a `@State`
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
    @Environment(NiceServices.self) private var services
    @SceneStorage("sidebarCollapsed") private var storedSidebarCollapsed: Bool = false
    /// Per-window persisted sidebar content mode. Default is `.tabs`
    /// so existing users get exactly the same first-launch experience
    /// they had before the file-browser feature shipped.
    @SceneStorage("sidebarMode") private var storedSidebarMode: SidebarMode = .tabs
    /// Stable per-window id that survives quits via scene storage.
    /// `AppState` uses it to look up this window's entry in
    /// `sessions.json` so restore rebuilds the right tabs.
    @SceneStorage("windowSessionId") private var storedWindowSessionId: String = ""

    var body: some View {
        AppShellHost(
            services: services,
            initialSidebarCollapsed: storedSidebarCollapsed,
            initialSidebarMode: storedSidebarMode,
            windowSessionId: storedWindowSessionId,
            sidebarCollapsedBinding: $storedSidebarCollapsed,
            sidebarModeBinding: $storedSidebarMode,
            windowSessionIdBinding: $storedWindowSessionId
        )
    }
}

/// The stateful inner view. Splitting it out lets us read
/// `@Environment` services and `@SceneStorage` values from
/// `AppShellView` before constructing the per-window `AppState`
/// (`@State` can't reach environment in its own `init`).
private struct AppShellHost: View {
    @Environment(Tweaks.self) private var tweaks
    @Environment(KeyboardShortcuts.self) private var shortcuts
    @Environment(FontSettings.self) private var fontSettings
    @Environment(\.colorScheme) private var scheme
    /// Used by the launch-time fan-out: the first-mounted window calls
    /// `openWindow(id: "main")` once per saved entry that no live
    /// `AppState` has claimed yet, so every window in `sessions.json`
    /// reopens on relaunch.
    @Environment(\.openWindow) private var openWindow

    @State private var appState: AppState
    let services: NiceServices
    @Binding var sidebarCollapsedBinding: Bool
    @Binding var sidebarModeBinding: SidebarMode
    @Binding var windowSessionIdBinding: String

    /// True while the cursor is over the floating peek sidebar. Keeps
    /// the overlay rendered after the keyboard modifiers are released
    /// so the user can click a tab without holding any keys.
    @State private var peekMousePinned: Bool = false

    /// Current docked-sidebar width, in points. Per-window and in-memory:
    /// resets to the 240pt default on every launch by design. Only read
    /// by `floatingSidebarCard(resizable:)` in its expanded (docked)
    /// variant; the peek overlay always uses the fixed 240pt.
    @State private var sidebarWidth: CGFloat = 240

    /// Width at the start of the current drag, captured on first
    /// `onChanged`. Kept separate from `sidebarWidth` so translation is
    /// always applied to a fixed baseline — avoids accumulated error and
    /// sticky behavior when reversing direction from a clamped edge.
    @State private var dragStartWidth: CGFloat? = nil

    init(
        services: NiceServices,
        initialSidebarCollapsed: Bool,
        initialSidebarMode: SidebarMode,
        windowSessionId: String,
        sidebarCollapsedBinding: Binding<Bool>,
        sidebarModeBinding: Binding<SidebarMode>,
        windowSessionIdBinding: Binding<String>
    ) {
        self.services = services
        // `NICE_MAIN_CWD` lets UI tests pin the Main Terminal tab's
        // CWD to a sandboxed test directory. Production launches
        // don't set it; AppState falls through to NSHomeDirectory().
        let testCwd = ProcessInfo.processInfo.environment["NICE_MAIN_CWD"]
        // AppState's init is side-effect free, so re-evaluation on
        // parent body re-renders is safe — `@State` keeps the first
        // instance via View identity. `start()` (called from `.task`
        // below) is what brings the socket up and spawns ptys, and is
        // idempotent.
        _appState = State(wrappedValue: AppState(
            services: services,
            initialSidebarCollapsed: initialSidebarCollapsed,
            initialSidebarMode: initialSidebarMode,
            initialMainCwd: testCwd,
            windowSessionId: windowSessionId
        ))
        _sidebarCollapsedBinding = sidebarCollapsedBinding
        _sidebarModeBinding = sidebarModeBinding
        _windowSessionIdBinding = windowSessionIdBinding
    }

    private var palette: Palette { tweaks.activeChromePalette }

    /// Body text for the "processes still running" alert. Lists the
    /// busy work so the user knows what they'd be force-quitting.
    /// The `.tabs` multi-batch case formats as a vertical list (one
    /// tab per line) since each entry is itself a "TabTitle (Pane1,
    /// Pane2)" summary; the singular scopes inline-comma-join because
    /// each entry is a single pane.
    private func pendingCloseMessage(_ request: PendingCloseRequest) -> String {
        switch request.scope {
        case .pane:
            return runningPrefix(request.busyPanes, joiner: ", ")
                + " Closing this pane will force it to quit."
        case .tab:
            return runningPrefix(request.busyPanes, joiner: ", ")
                + " Closing this tab will force everything in it to quit."
        case .project:
            return runningPrefix(request.busyPanes, joiner: ", ")
                + " Closing this project will force every tab in it to quit."
        case .tabs(let ids):
            // The idle tabs in the original batch already closed
            // before this alert went up — only the busy survivors
            // are at stake here.
            let n = ids.count
            let lead = n == 1
                ? "1 tab is busy:"
                : "\(n) tabs are busy:"
            return "\(lead)\n"
                + request.busyPanes.joined(separator: "\n")
                + "\nClosing them will force everything in them to quit."
        }
    }

    /// "X is still running." / "These are still running: X, Y."
    /// Shared between the singular scopes; the `.tabs` scope uses a
    /// list format instead.
    private func runningPrefix(_ items: [String], joiner: String) -> String {
        let list = items.joined(separator: joiner)
        return items.count == 1
            ? "\(list) is still running."
            : "These are still running: \(list)."
    }

    var body: some View {
        shell
        .ignoresSafeArea(edges: .top)
        .background(
            // Host-window reach-through: once the shell is mounted,
            // register the window so shortcuts and termination route to
            // this AppState, and nudge the native traffic lights into
            // the sidebar card. dy:-10 places their visual centers at
            // the same window-y as the sidebar collapse/expand icon
            // (which sits at y=26pt from the window top in both the
            // expanded sidebar and the collapsed cap).
            WindowAccessor { window in
                // Wire the NSWindow into WindowSession before anything
                // else so the first save (which can fire as early as a
                // tab mutation triggered during start()) captures the
                // real frame instead of persisting `frame: nil`.
                appState.windowSession.window = window
                TrafficLightNudger.nudge(window: window, dx: 8, dy: -10)
                TitleBarZoomMonitor.install()
                services.registry.register(appState: appState, window: window)
            }
        )
        .background(windowBackground.ignoresSafeArea())
        // Bottom-anchored banner for cross-window undo / drift
        // notifications from the shared file-operation history.
        .overlay(alignment: .bottom) {
            FileOperationDriftBanner(history: services.fileExplorer.history)
                .animation(.easeInOut(duration: 0.18), value: services.fileExplorer.history.lastDriftMessage)
        }
        .environment(\.palette, palette)
        // Each sub-model is injected separately so views downstream
        // can declare exactly which slice they observe (e.g.
        // `WindowToolbarView` reads only `TabModel` + `SessionsModel`).
        // AppState itself stays in the environment for the genuinely
        // cross-cutting hooks: `start()` / `tearDown()` choreography
        // and `fileBrowserStore` lifecycle. The file-operation surface
        // is injected as `FileExplorerOrchestrator` separately.
        .environment(appState.tabs)
        .environment(appState.sessions)
        .environment(appState.sidebar)
        .environment(appState.closer)
        .environment(appState.windowSession)
        .environment(appState.fileExplorerOrchestrator)
        .environment(appState.tabSelection)
        .environment(appState)
        // Single alert covers every close confirmation in the app —
        // pane / tab / project / multi-tab batch all flow through
        // `pendingCloseRequest` and `pendingCloseMessage` switches on
        // `scope` for the body wording.
        .alert(
            "Processes are still running",
            isPresented: Binding(
                get: { appState.closer.pendingCloseRequest != nil },
                set: { if !$0 { appState.closer.cancelPendingClose() } }
            ),
            presenting: appState.closer.pendingCloseRequest
        ) { _ in
            Button("Cancel", role: .cancel) { appState.closer.cancelPendingClose() }
            Button("Force quit", role: .destructive) { appState.closer.confirmPendingClose() }
        } message: { request in
            Text(pendingCloseMessage(request))
        }
        .task {
            // Order matters: `services.bootstrap()` writes the
            // process-wide ZDOTDIR and seeds `resolvedClaudePath`
            // from the env-var override; `appState.start()` reads
            // both when building pty env. Both are idempotent so
            // it's safe if `.task` re-fires across SwiftUI lifecycle
            // edges (e.g. window restoration).
            services.bootstrap()
            appState.start()
            // First-mounted window opens the rest. Runs after
            // `start()` so `restoreSavedWindow` has already claimed
            // this window's slot — `unclaimedSavedWindowCount` then
            // sees an accurate "still needs a home" count.
            // `consumeMultiWindowRestoreSlot` is a one-shot, so
            // siblings we open here, future ⌘N windows, and anything
            // AppKit may auto-restore all skip this branch.
            if services.consumeMultiWindowRestoreSlot() {
                let toSpawn = WindowSession.unclaimedSavedWindowCount(
                    ledger: services.claimLedger
                )
                for _ in 0..<toSpawn {
                    openWindow(id: "main")
                }
            }
        }
        .onAppear {
            // Brand-new scene: write the id WindowSession minted back
            // into SceneStorage so this window restores the same
            // slot on relaunch.
            if windowSessionIdBinding.isEmpty {
                windowSessionIdBinding = appState.windowSession.windowSessionId
            }
            appState.sessions.updateTerminalFontFamily(tweaks.terminalFontFamily)
            // updateScheme before updateTerminalTheme — see
            // `SessionsModel.makeSession` for why ordering matters
            // (the chrome-coupled Nice Defaults read the session's
            // cached scheme, so it must be current before their
            // bg / fg derivation runs).
            appState.sessions.updateScheme(scheme, palette: palette, accent: tweaks.accent.nsColor)
            appState.sessions.updateTerminalTheme(
                tweaks.effectiveTerminalTheme(for: scheme, catalog: services.terminalThemeCatalog)
            )
            appState.sessions.updateTerminalFontSize(fontSettings.terminalFontSize)
        }
        .onChange(of: scheme) { _, newScheme in
            appState.sessions.updateScheme(newScheme, palette: palette, accent: tweaks.accent.nsColor)
            appState.sessions.updateTerminalTheme(
                tweaks.effectiveTerminalTheme(for: newScheme, catalog: services.terminalThemeCatalog)
            )
        }
        .onChange(of: palette) { _, newPalette in
            appState.sessions.updateScheme(scheme, palette: newPalette, accent: tweaks.accent.nsColor)
        }
        .onChange(of: tweaks.accent) { _, newAccent in
            appState.sessions.updateScheme(scheme, palette: palette, accent: newAccent.nsColor)
        }
        .onChange(of: fontSettings.terminalFontSize) { _, newSize in
            appState.sessions.updateTerminalFontSize(newSize)
        }
        .onChange(of: tweaks.terminalThemeLightId) { _, _ in
            // Only applies if the active scheme is light — otherwise the
            // dark slot is active and this change is latent until the
            // next scheme flip.
            guard scheme == .light else { return }
            appState.sessions.updateTerminalTheme(
                tweaks.effectiveTerminalTheme(for: scheme, catalog: services.terminalThemeCatalog)
            )
        }
        .onChange(of: tweaks.terminalThemeDarkId) { _, _ in
            guard scheme == .dark else { return }
            appState.sessions.updateTerminalTheme(
                tweaks.effectiveTerminalTheme(for: scheme, catalog: services.terminalThemeCatalog)
            )
        }
        .onChange(of: tweaks.terminalFontFamily) { _, newValue in
            appState.sessions.updateTerminalFontFamily(newValue)
        }
        // Per-window SceneStorage bridges: persist this window's
        // collapsed-sidebar state across relaunch. Also clear any
        // in-flight peek state when the sidebar is explicitly
        // expanded (via ⌘B or the chevron) so we don't carry a stale
        // peek flag into the expanded shell.
        .onChange(of: appState.sidebar.sidebarCollapsed) { _, new in
            sidebarCollapsedBinding = new
            if !new {
                appState.sidebar.sidebarPeeking = false
                peekMousePinned = false
            }
        }
        // Mirror SidebarModel.sidebarMode back to scene storage so
        // each window restores its last-used mode across relaunch.
        .onChange(of: appState.sidebar.sidebarMode) { _, new in
            sidebarModeBinding = new
        }
        // Mirror WindowSession.windowSessionId back to scene storage —
        // restore may have adopted a different slot (e.g. bootstrap)
        // and the pairing must survive relaunch.
        .onChange(of: appState.windowSession.windowSessionId) { _, new in
            if new != windowSessionIdBinding {
                windowSessionIdBinding = new
            }
        }
    }

    // MARK: - Layout modes

    @ViewBuilder
    private var shell: some View {
        if appState.sidebar.sidebarCollapsed {
            collapsedShell
        } else {
            expandedShell
        }
    }

    /// Floor-to-ceiling floating sidebar card on the left, toolbar + main
    /// content stacked to its right.
    private var expandedShell: some View {
        HStack(spacing: 0) {
            floatingSidebarCard(resizable: true)

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
    private func floatingSidebarCard(resizable: Bool = false) -> some View {
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
                        // Mode toggles + collapse all live as a single
                        // trailing row. Collapse stays rightmost so its
                        // window-x position is unchanged (UITests + the
                        // collapsed-cap restore button align on it).
                        // Button top at strip-y=8 places each 24pt
                        // button's center at strip-y=20, i.e. 26pt
                        // from the window top — matching the collapsed
                        // cap's button (40pt card, 6pt top padding,
                        // HStack-centered button → same window-y=26).
                        HStack(spacing: 4) {
                            SidebarModeIconButton(
                                systemImage: "list.bullet",
                                help: "Show tabs",
                                accessibilityId: "sidebar.mode.tabs",
                                active: appState.sidebar.sidebarMode == .tabs,
                                accent: tweaks.accent.color
                            ) {
                                appState.sidebar.sidebarMode = .tabs
                            }
                            SidebarModeIconButton(
                                systemImage: "folder",
                                help: "Show files",
                                accessibilityId: "sidebar.mode.files",
                                active: appState.sidebar.sidebarMode == .files,
                                accent: tweaks.accent.color
                            ) {
                                appState.sidebar.sidebarMode = .files
                            }
                            SidebarToggleButton(
                                help: "Collapse sidebar",
                                accessibilityId: "sidebar.collapse"
                            ) {
                                appState.sidebar.toggleSidebar()
                            }
                        }
                        .padding(.top, 8)
                        .padding(.trailing, 10)
                    }
                SidebarView()
            }
        }
        .frame(width: resizable ? sidebarWidth : 240)
        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(
                    Color.niceLine(scheme, palette).opacity(0.5),
                    lineWidth: 0.5
                )
        )
        .overlay(alignment: .trailing) {
            if resizable {
                sidebarResizeHandle
            }
        }
        .shadow(color: Color.black.opacity(0.15), radius: 4, x: 0, y: 2)
        .padding(.leading, 6)
        .padding(.top, 6)
        .padding(.bottom, 6)
        // Lift the card above the main content in Z so its shadow
        // isn't clipped by the opaque nicePanel / niceChrome
        // backgrounds of the toolbar and terminal host next to it.
        .zIndex(1)
    }

    /// Invisible 6pt hit zone on the sidebar's trailing edge. No visible
    /// affordance — `.onHover` flips the cursor to `.resizeLeftRight` so
    /// the edge is discoverable by feel. Drag to resize, double-click to
    /// reset to 240pt. Offset 3pt so the hit zone straddles the visible
    /// edge (3pt inside the card, 3pt in the gap).
    private var sidebarResizeHandle: some View {
        Color.clear
            .frame(width: 6)
            .contentShape(Rectangle())
            .offset(x: 3)
            .onHover { hovering in
                if hovering {
                    NSCursor.resizeLeftRight.push()
                } else {
                    NSCursor.pop()
                }
            }
            .gesture(
                TapGesture(count: 2)
                    .onEnded {
                        sidebarWidth = 240
                        dragStartWidth = nil
                    }
                    .simultaneously(with:
                        // .global, not .local: the handle moves with the
                        // sidebar as it resizes, so a .local translation
                        // would feed back on itself (widen → handle moves
                        // right → translation grows → widen more → shake).
                        DragGesture(minimumDistance: 0, coordinateSpace: .global)
                            .onChanged { value in
                                if dragStartWidth == nil {
                                    dragStartWidth = sidebarWidth
                                }
                                let baseline = dragStartWidth ?? sidebarWidth
                                sidebarWidth = min(480, max(160, baseline + value.translation.width))
                            }
                            .onEnded { _ in dragStartWidth = nil }
                    )
            )
    }

    /// Collapsed: no sidebar column. A small floating card sits in the
    /// upper-left behind the three traffic lights and hosts a restore
    /// icon to re-expand the sidebar. The card is constrained within the
    /// top bar's 52pt vertical band — same styling as the expanded
    /// sidebar card (rounded corners, border, shadow, sidebar material),
    /// just sized down. The main content fills the full width below.
    ///
    /// When a sidebar-tab shortcut is held (`sidebar.sidebarPeeking`)
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
            if appState.sidebar.sidebarPeeking || peekMousePinned {
                floatingSidebarCard()
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
        .animation(.easeOut(duration: 0.15), value: appState.sidebar.sidebarPeeking)
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
                    appState.sidebar.toggleSidebar()
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
        if let tabId = appState.tabs.activeTabId,
           let tab = appState.tabs.tab(for: tabId),
           let paneId = tab.activePaneId,
           let session = appState.sessions.ptySessions[tabId],
           let view = session.view(forPane: paneId) {
            let pane = tab.panes.first(where: { $0.id == paneId })
            ZStack {
                TerminalHost(view: view, focus: true)
                    .id(paneId)
                if case .visible(let command)? = appState.sessions.paneLaunchStates[paneId] {
                    LaunchingOverlay(
                        title: pane?.kind == .terminal
                            ? "Launching terminal…"
                            : "Launching claude…",
                        command: command
                    )
                    .transition(.opacity)
                }
            }
            .animation(.easeOut(duration: 0.12), value: appState.sessions.paneLaunchStates[paneId])
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

// MARK: - Mode toggle icon

/// Sidebar-header icon button that selects between `SidebarMode.tabs`
/// and `SidebarMode.files`. Active mode gets an accent-tinted filled
/// background (mirrors `SettingsSectionRow` styling); inactive picks
/// up only a hover background, matching `SidebarToggleButton`.
private struct SidebarModeIconButton: View {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.palette) private var palette

    let systemImage: String
    let help: String
    let accessibilityId: String
    let active: Bool
    let accent: Color
    let action: () -> Void

    @State private var hover = false

    private var backgroundFill: Color {
        if active { return Color.niceSel(scheme, accent: accent) }
        if hover  { return Color.niceInk(scheme, palette).opacity(0.08) }
        return .clear
    }

    var body: some View {
        Image(systemName: systemImage)
            .font(.system(size: 13, weight: active ? .semibold : .regular))
            .foregroundStyle(active
                ? Color.niceInk(scheme, palette)
                : Color.niceInk2(scheme, palette))
            .frame(width: 24, height: 24)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(backgroundFill)
            )
            .contentShape(Rectangle())
            .onHover { hover = $0 }
            .onTapGesture { action() }
            .help(help)
            .accessibilityIdentifier(accessibilityId)
            .accessibilityAddTraits(active ? .isSelected : [])
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
        .environment(NiceServices())
        .environment(Tweaks())
        .environment(KeyboardShortcuts())
        .environment(FontSettings())
        .environment(FileBrowserSortSettings())
        .frame(width: 1180, height: 680)
        .preferredColorScheme(.light)
}

#Preview("Dark") {
    AppShellView()
        .environment(NiceServices())
        .environment(Tweaks())
        .environment(KeyboardShortcuts())
        .environment(FontSettings())
        .environment(FileBrowserSortSettings())
        .frame(width: 1180, height: 680)
        .preferredColorScheme(.dark)
}
