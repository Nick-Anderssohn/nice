//
//  TrafficLightPlacer.swift
//  Nice
//
//  Positions the three standard window buttons (close / miniaturize /
//  zoom) for one `.hiddenTitleBar` window so they sit inside the sidebar
//  card, aligned to Nice's own top-bar row instead of macOS's default
//  ~16pt-from-top cluster. Owned by `WindowChromeController`.
//
//  Also lays out `pinButton` (a `ChromePinButton`) as a fourth "light"
//  immediately to zoom's right — same size, same window-y 26 row, one
//  native inter-button pitch across. It rides the same absolute-target
//  apply()/observer machinery, so it holds its spot through resize, focus
//  changes, and the full-screen transition just like the lights do.
//
//  GEOMETRY (the why behind the two pure functions below):
//
//    • y is ABSOLUTE and OS-independent. Every Nice top-bar element
//      centers on window-y 26 (= `WindowChrome.topBarHeight / 2`): the
//      toolbar pills and `+` (HStack-centered in the 52pt band), the
//      sidebar mode / collapse icons (card inset 6 + .padding(.top, 8) +
//      24/2 = 26), the collapsed-cap restore button. We target that row
//      directly: `originY = windowHeight - centerFromTop - buttonHeight/2`.
//      This does NOT depend on macOS's default button y, so it's robust
//      across macOS 14 / 15 / 26. (On macOS 26 it happens to equal the old
//      nudger's default 577 + nudgeY(-10) = 567 exactly.)
//
//    • x is DEFAULT-RELATIVE, not hardcoded. We take each button's OWN
//      native default leading x (the value AppKit lays it out at) and add
//      a uniform `WindowChrome.trafficLightNudgeX` (8pt) inward. A uniform
//      translation PRESERVES the OS-native inter-button pitch — 23pt on
//      macOS 26, 20pt on macOS ≤ 15 — and reproduces today's shipping
//      pixels EXACTLY on every OS version. Hardcoding 28/48/68 (or a fixed
//      20pt pitch) is the stale-macOS regression: on this host it would
//      shove the lights ~11pt right and compress the spacing. The 8pt
//      inset clears the sidebar card's 8pt rounded corner (the card's
//      leading edge is window-x 6) with breathing room.
//
//  CAPTURING THE NATIVE DEFAULT WITHOUT THE BUG B "capture-then-pin" trap:
//  the only thing this class caches is each button INSTANCE's native
//  default x, keyed by `ObjectIdentifier(button)`, recorded the first time
//  `apply()` sees that instance — strictly BEFORE we ever move it (within
//  a single `apply()` we capture-then-move; the cache survives across
//  apply()s). Because the desired position is ABSOLUTE (default + offset),
//  not relative-to-current, re-applying never compounds. When AppKit
//  swaps a button for a new instance, the new instance has a new
//  `ObjectIdentifier` and its true default is captured fresh on first
//  sight — never our already-moved value. The cache is cleared in `stop()`.
//
//  STAYING APPLIED across AppKit's relayouts: we observe each button's OWN
//  `frameDidChange` (so we react when AppKit moves it) AND lazily
//  re-resolve `standardWindowButton(kind)` + its superview inside every
//  `apply()` (so a swapped instance is picked up even on a window that
//  opens already-key and is never resized — the original torn-off-window
//  bug). Window focus / resize / move re-resolve + re-apply too. Full
//  screen suspends the placer between will-enter and did-exit so we don't
//  fight AppKit's transition animation frame-by-frame.
//

import AppKit

@MainActor
final class TrafficLightPlacer {

    /// Held weakly — the controller (and ultimately the registry's weak
    /// key) owns the window's lifetime; we never extend it.
    private weak var window: NSWindow?

    /// The three standard buttons we position, in leading-to-trailing
    /// order. The order matters only documentarily; each is positioned
    /// from its own captured default, so the cluster keeps the OS pitch.
    private let kinds: [NSWindow.ButtonType] = [.closeButton, .miniaturizeButton, .zoomButton]

    /// The custom pin toggle, laid out as a fourth "light" one native pitch
    /// to the right of zoom. Owned here (created once) and re-parented into
    /// the lights' superview inside `apply()`; removed in `stop()`. Exposed
    /// `internal` so tests can assert its placement + state.
    let pinButton = ChromePinButton()

    /// Per-kind `frameDidChange` observer tokens, so we can drop the stale
    /// one when a button instance is replaced.
    private var frameTokens: [NSWindow.ButtonType: NSObjectProtocol] = [:]

    /// Window-level observer tokens (focus / resize / move / fullscreen).
    private var windowTokens: [NSObjectProtocol] = []

    /// The button instance each kind's frame observer is currently bound
    /// to, so `resolveAndObserve` can detect an instance swap.
    private var observedButtons: [NSWindow.ButtonType: NSView] = [:]

    /// Each button instance's native default leading x in WINDOW
    /// coordinates, captured once before we ever move that instance. The
    /// OS ground-truth default — never our nudged value. Cleared in
    /// `stop()`.
    private var defaultOriginX: [ObjectIdentifier: CGFloat] = [:]

    /// Suspends `apply()` for the duration of a full-screen transition so
    /// we don't snap the buttons mid-animation (flicker). Set true on
    /// will-enter, false on did-exit.
    private var suspended = false

    init(window: NSWindow) {
        self.window = window
    }

    // MARK: - Pure math (unit-tested)

    /// Desired WINDOW-coord leading x for a button: its native default x
    /// shifted uniformly inward by `nudge`. Uniform translation preserves
    /// the OS-native inter-button pitch and is correct on every macOS
    /// version (the default x carries the OS geometry; we only add 8).
    static func desiredOriginX(
        nativeDefaultX: CGFloat,
        nudge: CGFloat = WindowChrome.trafficLightNudgeX
    ) -> CGFloat {
        nativeDefaultX + nudge
    }

    /// Desired WINDOW-coord origin y (bottom edge, AppKit y grows up) so
    /// the button's visual center lands `centerFromTop` points below the
    /// window's top edge. Absolute and OS-independent.
    static func desiredOriginY(
        windowHeight: CGFloat,
        buttonHeight: CGFloat,
        centerFromTop: CGFloat = WindowChrome.trafficLightCenterFromTop
    ) -> CGFloat {
        windowHeight - centerFromTop - buttonHeight / 2
    }

    // MARK: - Lifecycle

    /// Resolve + observe the buttons, install the window-level observers,
    /// and apply once.
    func start() {
        resolveAndObserve()

        guard let window else { return }
        let center = NotificationCenter.default

        // Focus / resize / move all re-resolve (in case AppKit swapped a
        // button instance) and re-apply. `didMove` specifically covers the
        // tear-off window's post-open `setFrameOrigin` reposition, which
        // the old nudger needed an explicit observer for. `didBecomeMain`
        // joins `didBecomeKey` so a background/fan-out window that becomes
        // main without becoming key still re-resolves.
        for name in [NSWindow.didBecomeKeyNotification,
                     NSWindow.didBecomeMainNotification,
                     NSWindow.didResizeNotification,
                     NSWindow.didMoveNotification] {
            let token = center.addObserver(
                forName: name,
                object: window,
                queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    self.resolveAndObserve()
                    self.apply()
                }
            }
            windowTokens.append(token)
        }

        // Suspend across the full-screen transition. Snapping on every
        // frameDidChange while AppKit animates the title-bar band in/out
        // can flicker, so we go quiet between will-enter and did-exit, then
        // re-resolve + re-apply once the buttons settle in windowed mode.
        let willEnter = center.addObserver(
            forName: NSWindow.willEnterFullScreenNotification,
            object: window,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.suspended = true
                // Hide the pin for the duration of full screen — the whole
                // custom top-bar row goes away, so a pin floating at window-y
                // 26 would be stranded. `apply()` on did-exit re-shows +
                // re-positions it at exactly its windowed spot.
                self.pinButton.isHidden = true
            }
        }
        windowTokens.append(willEnter)

        let didExit = center.addObserver(
            forName: NSWindow.didExitFullScreenNotification,
            object: window,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.suspended = false
                self.resolveAndObserve()
                self.apply()
            }
        }
        windowTokens.append(didExit)

        apply()
    }

    /// For each kind, resolve the current button instance and, if it
    /// differs from the one we're observing (instance swap, or first
    /// time), move the `frameDidChange` observer onto the new instance.
    /// Superview re-resolution happens inside `apply()`, so this only
    /// tracks button-instance identity + frame observers.
    private func resolveAndObserve() {
        let center = NotificationCenter.default
        for kind in kinds {
            guard let button = window?.standardWindowButton(kind) else { continue }
            if observedButtons[kind] === button { continue }

            // Drop the stale observer for this kind, if any.
            if let stale = frameTokens[kind] {
                center.removeObserver(stale)
                frameTokens[kind] = nil
            }

            button.postsFrameChangedNotifications = true
            let token = center.addObserver(
                forName: NSView.frameDidChangeNotification,
                object: button,
                queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated { self?.apply() }
            }
            frameTokens[kind] = token
            observedButtons[kind] = button
        }
    }

    // MARK: - Apply

    /// Recompute each button's ABSOLUTE target and move it there if it's
    /// drifted by more than 0.5pt. Nothing relative is cached: we read the
    /// live default (captured once per instance, before moving) and the
    /// live window height every time, so this is convergent and never
    /// compounds.
    func apply() {
        guard let window,
              !suspended,
              window.styleMask.contains(.fullSizeContentView),
              !window.styleMask.contains(.fullScreen),
              let contentView = window.contentView
        else { return }

        let windowHeight = contentView.bounds.height

        // Captured while we walk the three lights, so the pin can be laid
        // out relative to zoom afterwards (one native pitch to its right).
        var miniDefaultX: CGFloat?
        var zoomContext: (button: NSView, superview: NSView, defaultX: CGFloat)?

        for kind in kinds {
            // Lazy re-resolve: read the button + superview fresh every
            // apply so a swapped instance is handled even without a
            // frame/window event. guard-let superview (NO force-unwrap):
            // during full-screen reparenting a button can transiently lack
            // its expected superview.
            guard let button = window.standardWindowButton(kind),
                  let superview = button.superview
            else { continue }

            // Capture this instance's native default x ONCE, before we ever
            // move it. `button.frame` is in superview coords; convert to
            // WINDOW coords (`to: nil`) for the OS-default baseline.
            let oid = ObjectIdentifier(button)
            let defaultX = defaultOriginX[oid] ?? {
                let v = superview.convert(button.frame, to: nil).origin.x
                defaultOriginX[oid] = v
                return v
            }()

            // Desired origin in WINDOW coords, then back to this button's
            // superview coords (superview differs across kinds in some
            // layouts, so we resolve per button — never cache it).
            let desiredWin = CGPoint(
                x: Self.desiredOriginX(nativeDefaultX: defaultX),
                y: Self.desiredOriginY(
                    windowHeight: windowHeight,
                    buttonHeight: button.frame.height
                )
            )
            let desiredSuper = superview.convert(desiredWin, from: nil)

            // Idempotence / recursion guard: our own setFrameOrigin
            // re-fires frameDidChange; on the re-entry cur == desired so we
            // stop. Convergent, no compounding.
            let cur = button.frame.origin
            if abs(cur.x - desiredSuper.x) > 0.5 || abs(cur.y - desiredSuper.y) > 0.5 {
                button.setFrameOrigin(desiredSuper)
            }

            switch kind {
            case .miniaturizeButton: miniDefaultX = defaultX
            case .zoomButton: zoomContext = (button, superview, defaultX)
            default: break
            }
        }

        placePin(after: zoomContext, miniDefaultX: miniDefaultX, windowHeight: windowHeight)
    }

    /// Size + position the pin as a fourth light. Its size matches zoom's;
    /// its leading x sits ONE native inter-button pitch to zoom's right
    /// (`pitch = zoomDefaultX - miniDefaultX`, the same gap the three lights
    /// preserve); its center shares the absolute window-y 26 top-bar row.
    /// The math is ABSOLUTE (default-relative x + absolute y), so — like the
    /// lights — re-applying converges and never compounds.
    private func placePin(
        after zoomContext: (button: NSView, superview: NSView, defaultX: CGFloat)?,
        miniDefaultX: CGFloat?,
        windowHeight: CGFloat
    ) {
        // Need both zoom (anchor) and miniaturize (to derive the OS pitch).
        // On any real window all three lights exist; if not, leave the pin
        // untouched rather than guess a pitch.
        guard let zoom = zoomContext, let miniX = miniDefaultX else { return }

        let pitch = zoom.defaultX - miniX
        let leadingX = Self.desiredOriginX(nativeDefaultX: zoom.defaultX) + pitch
        let size = zoom.button.frame.size
        let originY = Self.desiredOriginY(windowHeight: windowHeight, buttonHeight: size.height)

        // Keep the pin in the SAME superview as the lights so it shares their
        // coordinate space + z-layer; re-parent if AppKit swapped the
        // container (e.g. across a full-screen reparent).
        if pinButton.superview !== zoom.superview {
            pinButton.removeFromSuperview()
            zoom.superview.addSubview(pinButton)
        }

        let originSuper = zoom.superview.convert(CGPoint(x: leadingX, y: originY), from: nil)
        let desired = CGRect(origin: originSuper, size: size)
        let cur = pinButton.frame
        if abs(cur.origin.x - desired.origin.x) > 0.5
            || abs(cur.origin.y - desired.origin.y) > 0.5
            || abs(cur.width - desired.width) > 0.5
            || abs(cur.height - desired.height) > 0.5 {
            pinButton.frame = desired
        }
        pinButton.isHidden = false
    }

    // MARK: - Teardown

    /// Remove every observer and drop all cached state, including the
    /// per-instance default cache.
    func stop() {
        let center = NotificationCenter.default
        frameTokens.values.forEach { center.removeObserver($0) }
        windowTokens.forEach { center.removeObserver($0) }
        frameTokens = [:]
        windowTokens = []
        observedButtons = [:]
        defaultOriginX = [:]
        pinButton.removeFromSuperview()
    }
}
