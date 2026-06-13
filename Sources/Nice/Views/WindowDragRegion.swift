//
//  WindowDragRegion.swift
//  Nice
//
//  Two cooperating pieces that give the 52pt top bar native title-bar
//  behaviour (drag to move, double-click to zoom) even though the
//  window uses `.hiddenTitleBar` + `.fullSizeContentView`:
//
//  1. `WindowDragRegion` — a transparent `NSView` with
//     `mouseDownCanMoveWindow = true`. SwiftUI lays it into the empty
//     chrome *behind* interactive controls. AppKit's own window-drag
//     machinery picks this up so click-and-drag moves the window.
//
//  2. `TitleBarZoomMonitor` — a local `NSEvent` monitor. AppKit's
//     title-bar hit-test doesn't reliably cross into NSViews embedded
//     by SwiftUI's hosting machinery, so `mouseDownCanMoveWindow`
//     alone doesn't trigger double-click-to-zoom. The monitor fills
//     that gap: on a double left-click, it walks the hit view's
//     ancestor chain and zooms if any view reports
//     `mouseDownCanMoveWindow = true`. `NSVisualEffectView` with
//     `.behindWindow` blending returns true by default, so we skip
//     that class — otherwise double-clicks anywhere in the vibrancy-
//     tinted sidebar would zoom.
//
//     The same monitor also OWNS the `isMovable = false` policy: on
//     every leftMouseDown in a full-size-content window it forces
//     `isMovable = false` before AppKit's title-bar tracker reads the
//     flag. Because a local monitor runs before NSApplication dispatch,
//     the tracker can never see `isMovable == true` on a press — so a
//     window born mid-drag (whose properties AppKit re-finalizes back to
//     `true`) can't let a pill press ride the native title-bar drag
//     (BUG C). This replaced the open-loop `isMovable` re-assert timers
//     that previously lived in `AppShellView`.
//
//  Phase A of the title-bar refactor tried to fold (2) into a
//  `mouseDown(_:)` override on `DragView` that calls `performZoom`
//  for `clickCount >= 2` — eliminating the process-wide event hook.
//  It does not work in either drag-mechanism configuration:
//
//    • With `mouseDownCanMoveWindow = true`, AppKit's title-bar
//      tracker takes over the gesture and `mouseDown` is never
//      delivered to the view for stationary clicks (verified by
//      `WindowDragUITests.testEmptyToolbarDoubleClickZoomsWindow`
//      against this configuration).
//    • With `mouseDownCanMoveWindow = false`, `mouseDown` is
//      delivered, but calling `performDrag` for the single-click
//      case transfers event tracking to AppKit, which absorbs the
//      second click of a double-click; `clickCount` never reaches
//      2 (also caught by the same test).
//
//  The process-wide monitor sits cleanly outside both code paths, so
//  it can observe the second mouseDown event regardless of what
//  AppKit's drag tracker is doing. That's why it exists.
//

import AppKit
import SwiftUI

struct WindowDragRegion: NSViewRepresentable {
    func makeNSView(context: Context) -> NSView {
        DragView()
    }

    func updateNSView(_ nsView: NSView, context: Context) {}

    final class DragView: NSView {
        // NOTE: this flag no longer drives the window drag. The window's
        // `isMovable = false` policy stops pane pills from riding the
        // native title-bar drag, and that also disables the
        // `mouseDownCanMoveWindow` drag path entirely — so empty-chrome
        // dragging is now handled by a SwiftUI `DragGesture` →
        // `performDrag` in `WindowToolbarView`. `isMovable = false` is now
        // OWNED by the per-press event-time invariant in
        // `TitleBarZoomMonitor`'s local monitor (asserted on every
        // leftMouseDown before AppKit's title-bar tracker reads it);
        // `AppShellView` keeps only the harmless initial assignment at
        // window attach. We keep the flag `true`
        // only because `TitleBarZoomMonitor` walks for a
        // `mouseDownCanMoveWindow == true` view (excluding
        // `NSVisualEffectView`) to recognise empty chrome for
        // double-click-to-zoom. Removing this view would break that
        // detection; it is otherwise inert.
        override var mouseDownCanMoveWindow: Bool { true }
    }
}

/// Makes a view an empty-chrome **window-drag surface** that works even
/// when `window.isMovable == false` (which Nice sets to disable native
/// title-bar drag so pane-pill drags don't move the window). A SwiftUI
/// `DragGesture` hands the live mouse event to `window.performDrag`, which
/// moves the window despite `isMovable == false` — and, unlike a view
/// `mouseDown` or `mouseDownCanMoveWindow`, IS driven by XCUITest's
/// synthesized drag, so it's regression-testable.
///
/// Attached as a **plain** `.gesture` (not `.simultaneousGesture`) so it
/// yields to any higher-priority child gesture: buttons and pane pills
/// claim their own presses, leaving only empty chrome to move the window.
///
/// `isBlocked` is evaluated per drag event so a caller can veto the drag
/// at fire time — the toolbar passes the pane-pill veto
/// (`WindowDragGate.pillPressInProgress`) so a pill drag never moves the
/// window; the sidebar's top strip has no pills and uses the default.
private struct WindowDraggableModifier: ViewModifier {
    let isBlocked: () -> Bool

    func body(content: Content) -> some View {
        content.gesture(
            DragGesture(minimumDistance: 2, coordinateSpace: .global)
                .onChanged { _ in
                    guard !isBlocked() else { return }
                    guard let window = NSApp.keyWindow,
                          let event = NSApp.currentEvent else { return }
                    window.performDrag(with: event)
                }
        )
    }
}

extension View {
    /// Attach the empty-chrome window-drag gesture. See
    /// `WindowDraggableModifier`. Scope it to the title-bar-height strip
    /// you want draggable (not the whole window). `isBlocked` (evaluated
    /// per drag event) lets the caller veto the drag — the toolbar vetoes
    /// while a pane-pill press is in flight.
    func windowDraggable(isBlocked: @escaping () -> Bool = { false }) -> some View {
        modifier(WindowDraggableModifier(isBlocked: isBlocked))
    }
}

/// The action macOS performs when the user double-clicks a window's
/// title bar, read live from `NSGlobalDomain`'s
/// `AppleActionOnDoubleClick`. Our custom band has to honor this itself
/// because we draw our own chrome instead of using a native title bar.
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

/// Installs a single process-wide local `NSEvent` monitor that turns
/// double-clicks on any `WindowDragRegion` into the user's configured
/// title-bar double-click action (`AppleActionOnDoubleClick`). Safe
/// to call repeatedly — only the first call installs the monitor.
@MainActor
enum TitleBarZoomMonitor {
    private static var installed = false

    static func install() {
        guard !installed else { return }
        installed = true

        NSEvent.addLocalMonitorForEvents(matching: .leftMouseDown) { event in
            // BUG C event-time invariant: on EVERY left mouse-down in a
            // full-size-content window, force `isMovable = false` before
            // anything else. Local monitors run BEFORE NSApplication
            // dispatches the event to AppKit's title-bar tracker, so the
            // tracker can never observe `isMovable == true` on a press —
            // even for a window born mid-drag whose properties AppKit
            // re-finalized back to the default `true` after the
            // synchronous `WindowAccessor` pass. This is the event-driven
            // replacement for the deleted 0/0.05/0.2s `AppShellView`
            // timer loop (which lost the race when re-finalization landed
            // after the last tick); it is exactly Design 2's
            // ChromeEventFunnel `isMovable` policy shipped early. Runs for
            // any leftMouseDown in any styleMask but only flips
            // full-size-content windows (so the Settings window with
            // standard chrome is untouched). Falls through to the
            // double-click logic below unchanged.
            if let w = event.window,
               w.styleMask.contains(.fullSizeContentView),
               w.isMovable {
                w.isMovable = false
            }

            guard event.clickCount == 2 else { return event }
            guard let window = event.window else { return event }
            guard let contentView = window.contentView else { return event }

            // No title-bar semantics in full screen: there's no native
            // title bar, the traffic lights are hidden in the menu-bar
            // overlay, and the custom band's zoom action is meaningless
            // (the window already fills the screen). Let the event pass
            // through untouched.
            guard !window.styleMask.contains(.fullScreen) else { return event }

            // Gate on the top 52pt chrome strip. Several AppKit views
            // lower in the window (NSVisualEffectView in the sidebar,
            // SwiftTerm's terminal view, etc.) report
            // `mouseDownCanMoveWindow = true` either by default or via
            // subclass overrides, so the hit-test walk alone would zoom
            // on double-clicks in the sidebar body or terminal pane.
            // Restricting the monitor to the visual chrome row (which
            // spans both the sidebar card's top strip and the toolbar,
            // edge-to-edge at window y=0..topBarHeight) matches the
            // native title-bar's own footprint. Sourced from the shared
            // constant so the gate can't desync from the band height the
            // rest of the chrome lays out against.
            let yFromTop = contentView.bounds.height - event.locationInWindow.y
            guard yFromTop <= WindowChrome.topBarHeight else { return event }

            guard let hit = contentView.hitTest(event.locationInWindow) else {
                return event
            }
            // Walk up from the hit view — the draggable marker may be
            // on an ancestor if SwiftUI wraps the representable in its
            // own hosting layer.
            var cursor: NSView? = hit
            while let v = cursor {
                if v.mouseDownCanMoveWindow && !(v is NSVisualEffectView) {
                    // Honor the user's title-bar double-click preference
                    // instead of always zooming. We've already confirmed
                    // this is a double-click in our title-bar band, so
                    // consume the event in every case (including .none —
                    // there's nothing below the chrome to receive it).
                    switch DoubleClickTitleBarAction.current {
                    case .zoom:     window.performZoom(nil)
                    case .minimize: window.performMiniaturize(nil)
                    case .none:     break
                    }
                    return nil
                }
                cursor = v.superview
            }
            return event
        }
    }
}
