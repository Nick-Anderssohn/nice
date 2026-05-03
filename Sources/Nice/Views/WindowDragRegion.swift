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
        override var mouseDownCanMoveWindow: Bool { true }
    }
}

/// Installs a single process-wide local `NSEvent` monitor that turns
/// double-clicks on any `WindowDragRegion` into `performZoom(_:)`. Safe
/// to call repeatedly — only the first call installs the monitor.
@MainActor
enum TitleBarZoomMonitor {
    private static var installed = false

    static func install() {
        guard !installed else { return }
        installed = true

        NSEvent.addLocalMonitorForEvents(matching: .leftMouseDown) { event in
            guard event.clickCount == 2 else { return event }
            guard let window = event.window else { return event }
            guard let contentView = window.contentView else { return event }

            // Gate on the top 52pt chrome strip. Several AppKit views
            // lower in the window (NSVisualEffectView in the sidebar,
            // SwiftTerm's terminal view, etc.) report
            // `mouseDownCanMoveWindow = true` either by default or via
            // subclass overrides, so the hit-test walk alone would zoom
            // on double-clicks in the sidebar body or terminal pane.
            // Restricting the monitor to the visual chrome row (which
            // spans both the sidebar card's top strip and the toolbar,
            // edge-to-edge at window y=0..52) matches the native title-
            // bar's own footprint.
            let yFromTop = contentView.bounds.height - event.locationInWindow.y
            guard yFromTop <= 52 else { return event }

            guard let hit = contentView.hitTest(event.locationInWindow) else {
                return event
            }
            // Walk up from the hit view — the draggable marker may be
            // on an ancestor if SwiftUI wraps the representable in its
            // own hosting layer.
            var cursor: NSView? = hit
            while let v = cursor {
                if v.mouseDownCanMoveWindow && !(v is NSVisualEffectView) {
                    window.performZoom(nil)
                    return nil
                }
                cursor = v.superview
            }
            return event
        }
    }
}
