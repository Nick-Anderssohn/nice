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
    /// Tear-off pairing token handed down by the value-presenting
    /// `WindowGroup(id: "main", for: String.self)` in `NiceApp`. nil for
    /// plain ⌘N / AppKit auto-restore; a fresh UUID for a tear-off or a
    /// launch fan-out window. `AppShellHost.task` consumes the seed
    /// deposited under this token (if any). Defaults to nil so the two
    /// `#Preview` `AppShellView()` call sites still compile.
    var tearOffToken: String? = nil
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
            tearOffToken: tearOffToken,
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
    /// `openWindow(id: "main", value:)` once per saved entry that no live
    /// `AppState` has claimed yet, so every window in `sessions.json`
    /// reopens on relaunch. A fresh token is minted per call (see the
    /// `.task` fan-out) so the value-presenting `WindowGroup` opens a
    /// distinct new window each time rather than de-duping on nil.
    @Environment(\.openWindow) private var openWindow

    @State private var appState: AppState
    let services: NiceServices
    /// Tear-off pairing token for this window (see `AppShellView`). nil
    /// for ⌘N / auto-restore; the `.task` only consumes a seed when this
    /// is non-nil AND a seed was deposited under it (a fan-out token has
    /// no deposited seed, so the window starts normally).
    let tearOffToken: String?
    @Binding var sidebarCollapsedBinding: Bool
    @Binding var sidebarModeBinding: SidebarMode
    @Binding var windowSessionIdBinding: String

    /// True while the cursor is over the floating peek sidebar. Keeps
    /// the overlay rendered after the keyboard modifiers are released
    /// so the user can click a tab without holding any keys.
    @State private var peekMousePinned: Bool = false

    /// Controls the first-launch "Install the Nice Handoff skill?" alert.
    /// Set to true once by `consumeHandoffSkillPromptSlot` when the user
    /// hasn't yet been asked. The alert fires at most once per process
    /// (the one-shot guard in NiceServices prevents re-entry), and the
    /// `handoffSkillPromptSeen` flag in Tweaks prevents it from appearing
    /// on future launches after the user responds.
    @State private var showHandoffSkillPrompt = false

    /// Whether to skip the first-launch handoff-skill prompt because we're
    /// running under the UITest harness. The suite launches the app with
    /// `NICE_APPLICATION_SUPPORT_ROOT` set (the same ephemeral-environment
    /// seam `SessionStore` / `MainTerminalShellInject` key off — never set
    /// in production); without this, the alert would appear on every
    /// UITest launch and cover the UI under test, because the shared
    /// dev-bundle UserDefaults never persists "seen" (tests don't tap the
    /// buttons). The dedicated handoff-prompt UITest opts back in by also
    /// setting `NICE_FORCE_FIRST_LAUNCH_PROMPT`.
    private static var shouldSuppressFirstLaunchPrompt: Bool {
        let env = ProcessInfo.processInfo.environment
        let inTestEnv = env["NICE_APPLICATION_SUPPORT_ROOT"] != nil
        let forced = env["NICE_FORCE_FIRST_LAUNCH_PROMPT"] != nil
        return inTestEnv && !forced
    }

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
        tearOffToken: String?,
        initialSidebarCollapsed: Bool,
        initialSidebarMode: SidebarMode,
        windowSessionId: String,
        sidebarCollapsedBinding: Binding<Bool>,
        sidebarModeBinding: Binding<SidebarMode>,
        windowSessionIdBinding: Binding<String>
    ) {
        self.services = services
        self.tearOffToken = tearOffToken
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

    /// Launch-arg gate for the UITest-only tear-off hooks. When present,
    /// `body` renders two hidden zero-size buttons —
    /// `accessibilityIdentifier == "test.tearOffActivePane"` and
    /// `"test.tearOffInactivePane"` — that perform a REAL programmatic
    /// tear-off of, respectively, the active tab's ACTIVE pane and its
    /// first NON-ACTIVE pane. XCUITest can't synthesize the cross-window
    /// "drag onto empty desktop" gesture, so this is how UITests get a
    /// genuine second window. Absent in production: the buttons aren't
    /// even built.
    private static let tearOffHookEnabled =
        ProcessInfo.processInfo.arguments.contains("--uitest-tearoff-hook")

    var body: some View {
        shell
        .overlay(alignment: .bottomTrailing) {
            // Zero-impact in production: the whole overlay is elided when
            // the launch arg is absent (the buttons are never built, so
            // they can't disturb layout or other tests). Bottom-trailing
            // keeps them clear of the traffic lights / sidebar / toolbar
            // chrome. The two hooks are stacked vertically (each 24x24)
            // so they never occlude each other for XCUITest hit-testing.
            if Self.tearOffHookEnabled {
                VStack(spacing: 8) {
                    testTearOffHook
                    testTearOffInactiveHook
                }
            }
        }
        .ignoresSafeArea(edges: .top)
        .background(
            // Host-window reach-through. `WindowBridge` fires SYNCHRONOUSLY
            // at view-attach (before first draw), unlike the old
            // `WindowAccessor` which deferred one runloop. Ownership after
            // this redesign:
            //   • `WindowChromeController` owns the chrome AppKit state —
            //     the `isMovable = false` policy (with a KVO re-assert for
            //     the server-side drag-init path) and the traffic lights
            //     (positioned absolutely to Nice's window-y 26 top-bar row,
            //     OS-version-robust, by `TrafficLightPlacer`). `adopt()`
            //     registers UNCONDITIONALLY and self-heals via its own
            //     focus / frame observers once the `.hiddenTitleBar` mask
            //     lands, so it's safe to call this early at attach.
            //   • `ChromeEventRouter` (installed by `adopt` below) owns
            //     double-click-zoom, empty-chrome drag, AND the per-press
            //     event-time `isMovable` invariant — the controller's policy
            //     is a complementary focus / KVO defense (both set isMovable
            //     false; no conflict).
            //
            // TIMING — two writes are DEFERRED one runloop because they only
            // stick when run AFTER SwiftUI finalizes the window (the same
            // reason the old `WindowAccessor` deferred everything):
            //   • the `NICE_UITEST_WINDOW_FRAME` pin — SwiftUI's initial
            //     sizing pass under `.windowResizability(.contentSize)` can
            //     override an attach-time `setFrame`, resurrecting the zoom
            //     flake the pin kills.
            //   • `registry.register` — it wraps `window.delegate` in
            //     `CloseConfirmationDelegate`; at synchronous attach SwiftUI
            //     may not have installed its scene delegate yet and would
            //     later replace our confirmer, silently losing busy-pane
            //     close confirmation.
            WindowBridge { window in
                // SYNCHRONOUS at attach: wire the NSWindow into
                // WindowSession before anything else so the first save
                // (which can fire as early as a tab mutation triggered
                // during start()) captures the real frame instead of
                // persisting `frame: nil` — the whole reason the bridge is
                // synchronous.
                appState.windowSession.window = window
                // SYNCHRONOUS at attach: register the chrome controller.
                // Unconditional registration + self-healing observers mean
                // the placer and the isMovable policy go live early without
                // waiting for the styleMask; the controller is idempotent.
                WindowChromeController.adopt(window)

                // DEFERRED one runloop — these must land after SwiftUI's
                // window finalization (see the comment above).
                DispatchQueue.main.async { [weak window] in
                    guard let window else { return }
                    // Pin the window to a deterministic, sub-screen frame so
                    // UITests that toggle zoom
                    // (`WindowDragUITests.testEmptyToolbarDoubleClickZoomsWindow`)
                    // get a known un-zoomed starting geometry. Without it,
                    // AppKit's saved window state can relaunch the window
                    // already maximized — and a window opened directly at
                    // its zoom frame has no distinct "user" frame, so
                    // `performZoom` is a no-op and the size never changes
                    // (the test's intermittent failure on a second run).
                    // Only the tests set this env var; production launches
                    // are untouched.
                    if let spec = ProcessInfo.processInfo.environment["NICE_UITEST_WINDOW_FRAME"] {
                        let parts = spec.split(separator: ",").compactMap { Double($0) }
                        if parts.count == 4 {
                            window.setFrame(
                                NSRect(x: parts[0], y: parts[1], width: parts[2], height: parts[3]),
                                display: true
                            )
                        }
                    }

                    // Register the window so shortcuts and termination route
                    // to this AppState, and install the close-confirmation
                    // delegate wrapper around whatever SwiftUI set.
                    services.registry.register(appState: appState, window: window)
                }
                // The chrome event router (double-click-zoom + empty-chrome
                // drag + the per-press event-time `isMovable` invariant) is
                // installed by `WindowChromeController.adopt` above — once,
                // process-wide — so there is nothing to install here.
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
        // First-launch prompt for the Nice Handoff skill. Shown at most
        // once per install — `handoffSkillPromptSeen` gates future
        // launches and `consumeHandoffSkillPromptSlot` gates siblings
        // within the same process. Both paths mark the prompt seen so
        // it never re-appears regardless of which button is tapped.
        .alert(
            "Install the Nice Handoff skill?",
            isPresented: $showHandoffSkillPrompt
        ) {
            // "Install" — enable the skill, write it to disk, mark seen.
            Button("Install") {
                tweaks.installHandoffSkill = true
                SkillInstaller.sync(enabled: true)
                tweaks.handoffSkillPromptSeen = true
            }
            .accessibilityIdentifier("handoffPrompt.install")
            // "Not Now" — leave the skill uninstalled, mark seen so the
            // prompt doesn't reappear. The user can always enable it
            // later via Settings → Claude.
            Button("Not Now", role: .cancel) {
                tweaks.installHandoffSkill = false
                SkillInstaller.sync(enabled: false)
                tweaks.handoffSkillPromptSeen = true
            }
            .accessibilityIdentifier("handoffPrompt.notNow")
        } message: {
            Text("The /nice-handoff skill lets Claude hand off the current work to a fresh session in a new tab. You can change this anytime in Settings.")
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
                // Mint a FRESH token per spawned window. The value-
                // presenting `WindowGroup(id: "main", for: String.self)`
                // keys window uniqueness on the presented value, and a
                // plain nil value can de-dup to the existing (nil-token)
                // first window — collapsing the whole fan-out to one
                // window. A distinct UUID per call forces a NEW window
                // each time. These fan-out tokens have NO deposited seed,
                // so each new window's `consumeTearOffSeed(token:)`
                // returns nil and it starts normally (adopting its
                // unclaimed `sessions.json` slot, not a tear-off).
                for _ in 0..<toSpawn {
                    openWindow(id: "main", value: UUID().uuidString)
                }
            }
            // Tear-off seed: a pane was dragged off a window and its
            // `PaneTearOffController` opened THIS window — paired by an
            // explicit token — to receive it. Consume the seed deposited
            // under THIS window's token; nil when this window carries no
            // token (⌘N / auto-restore) or a token with no deposited seed
            // (a launch fan-out window) — either case skips this block and
            // the window starts normally. The seed carries the live pty
            // entry plus the project identity needed to reconstruct the
            // sidebar tree; we adopt it here, AFTER `start()` has brought
            // the socket and sessions subsystem online, so `adoptPane`
            // can re-point the entry's delegate at this window's
            // `SessionsModel`.
            if let token = tearOffToken,
               let seed = services.consumeTearOffSeed(token: token) {
                switch seed.kind {
                case .claude:
                    appState.sessions.adoptClaudePaneAsNewTab(
                        entry: seed.entry,
                        paneId: seed.paneId,
                        title: seed.title,
                        claudeSessionId: seed.claudeSessionId,
                        projectId: seed.projectId,
                        projectName: seed.projectName,
                        projectPath: seed.projectPath
                    )
                case .terminal:
                    // A terminal torn off the TERMINALS section REPLACES
                    // this new window's pristine auto-seeded Main terminal
                    // (exactly one TERMINALS section; the torn-off pane IS
                    // the Main). A companion terminal torn off a Claude
                    // project keeps the per-project-section behavior.
                    if seed.projectId == TabModel.terminalsProjectId {
                        // `seed.entry` is optional: an unspawned torn-off
                        // pane (nil entry) spawns fresh in `seed.cwd` so
                        // it opens in the right directory (BUG A / graft 0).
                        appState.sessions.adoptTerminalPaneAsMainTerminal(
                            entry: seed.entry,
                            paneId: seed.paneId,
                            title: seed.title,
                            spawnCwd: seed.cwd
                        )
                    } else {
                        appState.sessions.adoptTerminalPaneAsNewTab(
                            entry: seed.entry,
                            paneId: seed.paneId,
                            title: seed.title,
                            projectId: seed.projectId,
                            projectName: seed.projectName,
                            projectPath: seed.projectPath,
                            spawnCwd: seed.cwd
                        )
                    }
                }
                // Position the new window at the drag-release point so it
                // "pops out" at the cursor. Best-effort: `window` may be
                // nil on a headless build; skip silently in that case.
                appState.windowSession.window?.setFrameOrigin(seed.screenPoint)
            }
            // Show the one-time handoff-skill prompt on first launch.
            // Runs after the multi-window fan-out so the extra windows
            // are already open before the alert appears. Three guards
            // must pass:
            //   • `handoffSkillPromptSeen` — prevents the alert from
            //     re-appearing after the user has already responded.
            //   • `consumeHandoffSkillPromptSlot` — prevents sibling
            //     windows from each raising their own copy within the
            //     same process lifetime.
            //   • not running under the UITest harness — otherwise the
            //     alert would pop on every UITest launch (the shared
            //     dev-bundle UserDefaults never persists "seen" because
            //     tests don't tap the buttons) and cover the UI under
            //     test. See `shouldSuppressFirstLaunchPrompt`.
            if !Self.shouldSuppressFirstLaunchPrompt
                && !tweaks.handoffSkillPromptSeen
                && services.consumeHandoffSkillPromptSlot() {
                showHandoffSkillPrompt = true
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
            appState.sessions.updateSmoothScrolling(tweaks.smoothScrolling)
            // Reconcile the per-window cache with the persisted toggle.
            // Load-bearing that this runs AFTER updateScheme/updateTerminalTheme:
            // the cache seeds sync OFF, so those calls don't write the theme
            // file; this line then enables (if persisted ON) and performs the
            // single write with the correct colors already cached — and an
            // opted-out user never writes at all. Mirrors the smoothScrolling seed.
            appState.sessions.updateSyncClaudeTheme(tweaks.syncClaudeTheme)
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
        .onChange(of: appState.tabs.activeTabId) { _, newTabId in
            // Bug 3 (single choke-point): switching to a tab whose active
            // pane is a deferred terminal that was never spawned (e.g. a
            // terminal-only tab selected for the first time via the
            // sidebar / keyboard, where the selection path only syncs the
            // id) would render blank. Spawn it on every active-tab change
            // so `mainContent` always has a hosted view. No-op when the
            // active pane is already spawned / is a Claude pane / has no
            // session yet.
            guard let newTabId else { return }
            appState.sessions.ensureActivePaneSpawned(tabId: newTabId)
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
        .onChange(of: tweaks.smoothScrolling) { _, newValue in
            appState.sessions.updateSmoothScrolling(newValue)
        }
        .onChange(of: tweaks.syncClaudeTheme) { _, newValue in
            // Every open window observes the shared Tweaks, so this fans the
            // toggle out to each window's cache — newly spawned panes pick up
            // / drop the --settings pointer, and enabling rewrites nice.json.
            appState.sessions.updateSyncClaudeTheme(newValue)
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

    /// UITest-only: a hidden button that tears off the active tab's
    /// active pane via `PaneTearOffController.tearOff` directly (bypassing
    /// `PaneDragEnd.outcome`, so it always tears off and always opens a
    /// real second window). Lives here because this view has `openWindow`,
    /// `appState`, and `services` all in scope. Built only when
    /// `--uitest-tearoff-hook` is passed.
    private var testTearOffHook: some View {
        Button {
            guard let tabId = appState.tabs.activeTabId,
                  let paneId = appState.tabs.tab(for: tabId)?.activePaneId
            else { return }
            // `tearOff` consumes the in-flight live-pane handle that a real
            // drag would have published on drag-start (`PaneDragSource`).
            // The hook skips the drag, so publish the same handle here
            // first — otherwise `tearOff` bails (no handle to claim).
            services.livePaneRegistry.publish(
                LivePaneRegistry.Handle(
                    paneId: paneId,
                    sourceWindowSessionId: appState.windowSession.windowSessionId,
                    sourceTabId: tabId,
                    claim: { [weak sessions = appState.sessions] in
                        sessions?.claimPaneForTransfer(tabId: tabId, paneId: paneId) ?? .gone
                    }
                )
            )
            // A fixed point well inside the main screen's visible frame so
            // the torn-off window lands on-screen for the assertions.
            let frame = NSScreen.main?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1440, height: 900)
            let point = NSPoint(x: frame.minX + 80, y: frame.maxY - 200)
            PaneTearOffController(services: services).tearOff(
                paneId: paneId,
                sourceWindowSessionId: appState.windowSession.windowSessionId,
                at: point,
                openWindow: { token in openWindow(id: "main", value: token) }
            )
        } label: {
            // A small but real, hittable hit area. Transparent so it's
            // invisible; `contentShape` makes the whole rect clickable for
            // XCUITest. Pinned to the bottom-trailing corner where there
            // is no interactive chrome to occlude it.
            Color.clear
                .frame(width: 24, height: 24)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("test.tearOffActivePane")
        .frame(width: 24, height: 24)
        .zIndex(999)
    }

    /// UITest-only: a sibling of `testTearOffHook` that tears off the
    /// active tab's FIRST NON-ACTIVE pane instead of the active one.
    /// This exists to exercise the BUG A unspawned-pane path that the
    /// active-pane hook structurally cannot reach: the active pane is
    /// always spawned (`ensureActivePaneSpawned` / restore brings it up),
    /// whereas a restored-but-never-focused non-active terminal pane has
    /// its pty spawn DEFERRED — it is `.notSpawned` until first focus
    /// (see `WindowSession.swift`'s terminal-spawn branch). Tearing that
    /// pane off must SPAWN it in the destination window (Phase A's
    /// `PaneClaim.notSpawned` path) rather than silently no-op'ing, which
    /// is exactly the BUG A regression `TearOffHookUITests` pins.
    /// Built only when `--uitest-tearoff-hook` is passed; everything else
    /// mirrors `testTearOffHook` (same handle publish + `tearOff` call) —
    /// only the pane selection differs.
    private var testTearOffInactiveHook: some View {
        Button {
            guard let tabId = appState.tabs.activeTabId,
                  let tab = appState.tabs.tab(for: tabId),
                  let inactive = tab.panes.first(where: { $0.id != tab.activePaneId })
            else { return }
            let paneId = inactive.id
            // `tearOff` consumes the in-flight live-pane handle that a real
            // drag would have published on drag-start (`PaneDragSource`).
            // The hook skips the drag, so publish the same handle here
            // first — otherwise `tearOff` bails (no handle to claim). The
            // claim closure routes through `claimPaneForTransfer`, which
            // returns `.notSpawned(cwd:)` for this never-focused pane.
            services.livePaneRegistry.publish(
                LivePaneRegistry.Handle(
                    paneId: paneId,
                    sourceWindowSessionId: appState.windowSession.windowSessionId,
                    sourceTabId: tabId,
                    claim: { [weak sessions = appState.sessions] in
                        sessions?.claimPaneForTransfer(tabId: tabId, paneId: paneId) ?? .gone
                    }
                )
            )
            // A fixed point well inside the main screen's visible frame so
            // the torn-off window lands on-screen for the assertions.
            let frame = NSScreen.main?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1440, height: 900)
            let point = NSPoint(x: frame.minX + 80, y: frame.maxY - 200)
            PaneTearOffController(services: services).tearOff(
                paneId: paneId,
                sourceWindowSessionId: appState.windowSession.windowSessionId,
                at: point,
                openWindow: { token in openWindow(id: "main", value: token) }
            )
        } label: {
            // A small but real, hittable hit area. Transparent so it's
            // invisible; `contentShape` makes the whole rect clickable for
            // XCUITest. Stacked above `testTearOffHook` in the bottom-
            // trailing corner where there is no interactive chrome.
            Color.clear
                .frame(width: 24, height: 24)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("test.tearOffInactivePane")
        .frame(width: 24, height: 24)
        .zIndex(999)
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
                    .frame(height: WindowChrome.topBarHeight)
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
                // The `WindowDragRegion` above vends a `ChromeDragStripView`
                // marker; `ChromeEventRouter` hit-tests it per-press and owns
                // this strip's empty-chrome drag + double-click-zoom. The
                // trailing mode/collapse buttons claim their own presses
                // (they hit-test to themselves, not the strip), so the router
                // passes those through.
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
    /// the sidebar is collapsed. The leading reserve
    /// (`WindowChrome.trafficLightReservedWidth`) hosts the three native
    /// traffic lights — derived from the same nudge `TrafficLightPlacer`
    /// applies, so the cap and the buttons can't drift apart; the restore
    /// button sits just past them. Vertical padding centers it within the
    /// 52pt top bar row so it reads as a distinct card rather than
    /// blending into either the chrome above or the content below.
    private var collapsedCap: some View {
        SidebarBackground(palette: palette, scheme: scheme) {
            HStack(spacing: 0) {
                // Leading reserve hosts the traffic lights; the drag
                // region underneath makes that strip (and any empty space
                // past the restore button) behave like a title bar for
                // drag + double-click zoom.
                WindowDragRegion().frame(width: WindowChrome.trafficLightReservedWidth)
                SidebarToggleButton(
                    help: "Expand sidebar",
                    accessibilityId: "sidebar.expand"
                ) {
                    appState.sidebar.toggleSidebar()
                }
                WindowDragRegion()
            }
        }
        // Total = traffic-light reserve + room for the restore button and
        // a small trailing drag strip, so the cap grows with the reserve.
        .frame(width: WindowChrome.trafficLightReservedWidth + 42, height: 40)
        // The `WindowDragRegion`s inside vend `ChromeDragStripView` markers;
        // `ChromeEventRouter` owns the cap's empty-chrome drag + double-click
        // zoom per-press (the expand button hit-tests to itself and is passed
        // through) — same model as the expanded sidebar's top strip.
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
            .frame(height: WindowChrome.topBarHeight)

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
            // Constant gap from the window's bottom edge, a touch below the
            // floating sidebar card's bottom (its `.padding(.bottom, 6)`).
            // `TerminalHost` bottom-anchors the row-quantized grid within
            // this area, so the sub-row remainder is parked at the top
            // (under the chrome) rather than wandering below the prompt as
            // the window resizes. Applied before `.background` so the gap is
            // painted in the terminal color.
            .padding(.bottom, 9)
            .padding(.leading, 20)
            .background(terminalBackgroundColor)
            // Present ONLY when a pty view is actually hosted for the
            // active pane — the blank `else` branch below renders nothing
            // with this id. UITests assert this exists to prove the active
            // pane renders a terminal rather than a blank background
            // (bug 3). A dedicated leaf element (rather than a container
            // identifier, which SwiftUI may not surface to XCUITest)
            // keyed to the pane id. Zero-size + clear → no visual effect.
            .overlay(alignment: .topLeading) {
                Color.clear
                    .frame(width: 1, height: 1)
                    .accessibilityElement()
                    .accessibilityIdentifier("mainContent.hostedPane")
                    .accessibilityValue(paneId)
            }
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
