//
//  ChromeEventRouter.swift
//  Nice
//
//  The SINGLE arbitration point for every chrome press. One process-wide
//  local `NSEvent` monitor classifies each `.leftMouseDown` once — pill
//  vs. empty-chrome strip vs. pass-through — and drives the three native
//  title-bar behaviours Nice has to synthesize under `.hiddenTitleBar` +
//  `.fullSizeContentView`:
//
//    • drag empty chrome to move the window,
//    • double-click empty chrome to run the user's `AppleActionOnDoubleClick`,
//    • never let a pane-pill press ride either path (BUG C).
//
//  This replaces THREE older mechanisms that used to cooperate (and could
//  conflict) for these behaviours:
//
//    • `TitleBarZoomMonitor` — the old local monitor that walked for ANY
//      ancestor with `mouseDownCanMoveWindow == true` (excluding
//      `NSVisualEffectView`) to recognise empty chrome for double-click
//      zoom, and owned the event-time `isMovable = false` invariant.
//    • `windowDraggable` — a SwiftUI `DragGesture` that fished
//      `NSApp.keyWindow` / `NSApp.currentEvent` to `performDrag`.
//    • `WindowDragGate` — a one-bit flag the pill press flipped so the
//      `DragGesture` would yield to a pill drag.
//
//  All three die here. The veto a pill press needs is no longer a flag:
//  it is this router's per-press hit-test. A pill press hit-tests to a
//  `PaneDragHosting` view, so the router passes the event through and
//  never arms a window drag — selectivity by construction, not by a flag
//  that can stick.
//
//  CLASSIFICATION — class-walk PLUS an attribute-walk fallback. The hit
//  view's ancestor chain is classified `PaneDragHosting` → `.pill`,
//  `ChromeDragStripView` → `.strip`. The SIDEBAR strip resolves directly
//  to its `ChromeDragStripView` marker. The TOOLBAR strip does NOT: it
//  lives in a `.background` `ZStack` behind the toolbar's HStack, and
//  SwiftUI resolves an empty-toolbar press to a transparent hosting
//  wrapper that is a sibling ABOVE the strip — never a descendant of it —
//  so the pure class-walk dead-spotted empty-toolbar drag + double-click
//  zoom (caught by `WindowDragUITests`). So `hitChain` ALSO classifies
//  `.strip` via the deleted `TitleBarZoomMonitor`'s proven predicate: any
//  non-`NSVisualEffectView` ancestor with `mouseDownCanMoveWindow == true`
//  (the default for SwiftUI hosting wrappers). This only WIDENS window
//  drag / zoom; it can never let a pill ride the drag, because a pill is
//  caught by the `PaneDragHosting` branch first and `decision()` gives
//  `.pill` precedence over `.strip`.
//
//  STATE — `pendingDrag` is the only state: a single struct, OVERWRITTEN
//  on every `.leftMouseDown`, CLEARED on every `.leftMouseDown` AND
//  `.leftMouseUp`. There is no stuck-bit failure mode — a press whose
//  `mouseUp` is swallowed by a button's tracking loop is disarmed by the
//  very next press regardless.
//
//  ISOLATION — the monitor handler is effectively main-actor under this
//  SDK (it accesses `event.window`, `performZoom`, `DoubleClickTitleBarAction
//  .current` directly), exactly as the deleted `TitleBarZoomMonitor` did,
//  so it needs no `MainActor.assumeIsolated` wrapper. `installIfNeeded()`
//  is called from `WindowChromeController.start()` so the install happens
//  once, process-wide, the first time any Nice window is adopted.
//

import AppKit

@MainActor
enum ChromeEventRouter {

    // MARK: - State

    /// A single armed empty-chrome drag: the window the press landed in and
    /// the press point (window coordinates). Overwritten on every mouseDown,
    /// cleared on every mouseDown and mouseUp — the router's only state.
    private struct PendingDrag {
        let window: NSWindow
        let start: NSPoint
    }

    private static var pendingDrag: PendingDrag?
    private static var installed = false

    // MARK: - Pure decision (unit-tested)

    /// What the hit view's ancestor chain classified as, frontmost first.
    enum HitKind { case pill, strip, other }

    /// The routing decision for a `.leftMouseDown`.
    enum Routing: Equatable {
        /// Let AppKit / SwiftUI have the event untouched (pill press,
        /// out-of-band, full screen, or non-chrome content).
        case passThrough
        /// Empty-chrome single click: record `pendingDrag` and pass the
        /// event through (a stationary click does nothing; a drag past the
        /// threshold turns into `performDrag`).
        case armDrag
        /// Empty-chrome double click: run `DoubleClickTitleBarAction` and
        /// consume the event.
        case doubleClickAction
    }

    /// Pure classification, extracted so the decision table is unit-testable
    /// without a live window. A pill press wins over the strip (pill
    /// precedence) and never zooms; the strip arms a drag on a single click
    /// and runs the double-click action on two; everything out-of-band or in
    /// full screen passes through.
    static func decision(
        hitChain: [HitKind],
        clickCount: Int,
        inBand: Bool,
        isFullScreen: Bool
    ) -> Routing {
        guard inBand && !isFullScreen else { return .passThrough }
        // Pill owns its press — precedence over the strip even when both are
        // in the chain (a pill drawn on top of the strip background).
        if hitChain.contains(.pill) { return .passThrough }
        if hitChain.contains(.strip) {
            return clickCount >= 2 ? .doubleClickAction : .armDrag
        }
        return .passThrough
    }

    // MARK: - Install

    /// Installs the single process-wide local monitor. Safe to call
    /// repeatedly — only the first call installs it. Called from
    /// `WindowChromeController.start()` so the install is owned by the
    /// per-window controller but happens just once.
    static func installIfNeeded() {
        guard !installed else { return }
        installed = true

        NSEvent.addLocalMonitorForEvents(
            matching: [.leftMouseDown, .leftMouseDragged, .leftMouseUp]
        ) { event in
            handle(event)
        }
    }

    // MARK: - Event handling

    private static func handle(_ event: NSEvent) -> NSEvent? {
        switch event.type {
        case .leftMouseDown:    return handleMouseDown(event)
        case .leftMouseDragged: return handleMouseDragged(event)
        case .leftMouseUp:      pendingDrag = nil; return event
        default:                return event
        }
    }

    private static func handleMouseDown(_ event: NSEvent) -> NSEvent? {
        // CLEARED ON EVERY mouseDown (ISSUE 8c) — must precede every other
        // branch so a strip press whose `mouseUp` was swallowed by a button
        // tracking loop can never leave the router armed.
        pendingDrag = nil

        // Positive identity: only Nice chrome windows are routed. A
        // non-adopted window (the Settings window, any AppKit panel) is
        // untouched — the controller registry is the seam.
        guard let window = event.window,
              WindowChromeController.controller(for: window) != nil else {
            return event
        }

        // Event-time `isMovable` invariant, absorbed from the deleted
        // `TitleBarZoomMonitor`. Local monitors run BEFORE NSApplication
        // dispatches the event to AppKit's title-bar tracker, so the tracker
        // can never observe `isMovable == true` on a press — even for a
        // window born mid-drag whose properties AppKit re-finalized back to
        // the default `true`. The `if isMovable` guard avoids a redundant
        // write (and a redundant KVO fire in the controller).
        if window.isMovable { window.isMovable = false }

        guard let contentView = window.contentView else { return event }

        let isFullScreen = window.styleMask.contains(.fullScreen)
        // Gate on the top chrome strip. `locationInWindow` has origin
        // bottom-left, so `bounds.height - y` is the distance from the top
        // edge. Sourced from the shared constant so the band can't desync
        // from the chrome the rest of the layout draws.
        let yFromTop = contentView.bounds.height - event.locationInWindow.y
        let inBand = yFromTop <= WindowChrome.topBarHeight
        let chain = hitChain(in: contentView, at: event.locationInWindow)

        switch decision(
            hitChain: chain,
            clickCount: event.clickCount,
            inBand: inBand,
            isFullScreen: isFullScreen
        ) {
        case .passThrough:
            return event
        case .doubleClickAction:
            // Honor the user's title-bar double-click preference. We've
            // already confirmed a double-click in the band on empty chrome,
            // so consume the event in every case (including `.none` — there
            // is nothing below the chrome to receive it).
            switch DoubleClickTitleBarAction.current {
            case .zoom:     window.performZoom(nil)
            case .minimize: window.performMiniaturize(nil)
            case .none:     break
            }
            return nil
        case .armDrag:
            pendingDrag = PendingDrag(window: window, start: event.locationInWindow)
            return event
        }
    }

    private static func handleMouseDragged(_ event: NSEvent) -> NSEvent? {
        guard let pending = pendingDrag,
              let window = event.window,
              window === pending.window else {
            return event
        }
        let dx = event.locationInWindow.x - pending.start.x
        let dy = event.locationInWindow.y - pending.start.y
        // 2pt threshold — matches today's `DragGesture(minimumDistance: 2)`
        // drag-start feel.
        if dx * dx + dy * dy >= 4 {
            pendingDrag = nil
            // The REAL dragged event from the RIGHT window. This replaces the
            // deleted `windowDraggable` modifier's `NSApp.keyWindow` /
            // `NSApp.currentEvent` fishing, which could resolve the wrong
            // window mid-tear-off. `performDrag` moves the window even though
            // `isMovable == false` (which only gates user-initiated native
            // title-bar moves).
            window.performDrag(with: event)
        }
        return event
    }

    // MARK: - Hit-test classification

    /// Hit-tests once at `point`, then walks the ancestor chain (frontmost
    /// first) classifying each view: `PaneDragHosting` → `.pill`,
    /// `ChromeDragStripView` → `.strip`. Returns the chain so the pure
    /// `decision` can apply pill precedence.
    private static func hitChain(in contentView: NSView, at point: NSPoint) -> [HitKind] {
        guard let hit = contentView.hitTest(point) else { return [] }
        var chain: [HitKind] = []
        var cursor: NSView? = hit
        while let v = cursor {
            if v is PaneDragHosting {
                chain.append(.pill)
            } else if v is ChromeDragStripView {
                chain.append(.strip)
            } else if v.mouseDownCanMoveWindow && !(v is NSVisualEffectView) {
                // ATTRIBUTE-WALK FALLBACK (ENABLED). ISSUE-5 confirmed at
                // runtime: a press on the TOOLBAR's empty chrome hit-tests to
                // a SwiftUI hosting wrapper that is a sibling ABOVE the
                // `.background` ZStack's `ChromeDragStripView`, not a
                // descendant of it — so the class-walk above never reaches the
                // strip and empty-toolbar drag + double-click-zoom became dead
                // spots (`WindowDragUITests.testEmptyToolbarDragMovesWindow` /
                // `testEmptyToolbarDoubleClickZoomsWindow`). The SIDEBAR strip
                // resolves directly to `ChromeDragStripView`, so it relies on
                // the clean branch above; only the toolbar needs this.
                //
                // Restore today's PROVEN breadth: any non-`NSVisualEffectView`
                // ancestor reporting `mouseDownCanMoveWindow == true` (the
                // default for SwiftUI's transparent hosting wrappers) is empty
                // chrome. This is exactly the predicate the deleted
                // `TitleBarZoomMonitor` used. It can only ever WIDEN window
                // drag / zoom — never let a pill ride the drag: a pill is
                // caught by the first branch (`PaneDragHosting`), and
                // `decision()` gives `.pill` precedence over `.strip`, so a
                // pill press still passes through. (`ChromeDragStripView` and
                // `PaneDragHostingView` both report `mouseDownCanMoveWindow ==
                // false`, so neither is matched here.)
                chain.append(.strip)
            }
            cursor = v.superview
        }
        return chain
    }
}

/// The action macOS performs when the user double-clicks a window's
/// title bar, read live from `NSGlobalDomain`'s
/// `AppleActionOnDoubleClick`. Our custom band has to honor this itself
/// because we draw our own chrome instead of using a native title bar.
///
/// Moved here from `WindowDragRegion.swift` when `ChromeEventRouter`
/// absorbed double-click handling; the type is unchanged, so
/// `DoubleClickTitleBarActionTests` keep passing (same module).
enum DoubleClickTitleBarAction: Equatable {
    case zoom
    case minimize
    case none

    /// Pure mapping from the raw `AppleActionOnDoubleClick` string to an
    /// action. Absent or unrecognized ⇒ `.zoom` (the macOS default).
    /// Split out from `current` so the mapping is unit-testable without
    /// touching the global `UserDefaults`.
    init(rawSetting: String?) {
        switch rawSetting {
        case "Minimize": self = .minimize
        case "None":     self = .none
        case "Maximize": self = .zoom
        default:         self = .zoom
        }
    }

    /// Current setting. Read fresh on each double-click so a change in
    /// System Settings → Desktop & Dock takes effect without relaunch.
    @MainActor
    static var current: DoubleClickTitleBarAction {
        DoubleClickTitleBarAction(
            rawSetting: UserDefaults.standard.string(forKey: "AppleActionOnDoubleClick")
        )
    }
}
