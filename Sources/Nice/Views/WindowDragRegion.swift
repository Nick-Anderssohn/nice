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
//     ancestor chain looking for the `mouseDownCanMoveWindow = true`
//     marker we planted via `WindowDragRegion`, and if found calls
//     `NSWindow.performZoom(_:)` — the same action the native title
//     bar would have performed.
//

import AppKit
import SwiftUI

struct WindowDragRegion: NSViewRepresentable {
    func makeNSView(context: Context) -> NSView {
        DragView()
    }

    func updateNSView(_ nsView: NSView, context: Context) {}

    fileprivate final class DragView: NSView {
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
            guard let hit = window.contentView?.hitTest(event.locationInWindow) else {
                return event
            }
            // Walk up from the hit view — the draggable marker may be
            // on an ancestor if SwiftUI wraps the representable in its
            // own hosting layer.
            var cursor: NSView? = hit
            while let v = cursor {
                if v.mouseDownCanMoveWindow {
                    window.performZoom(nil)
                    return nil
                }
                cursor = v.superview
            }
            return event
        }
    }
}
