//
//  TrafficLightNudger.swift
//  Nice
//
//  The app uses `.hiddenTitleBar` so the native traffic lights float on
//  top of the sidebar card. By default macOS positions them at roughly
//  `(x: 20, y: 15)` in window-content coordinates, which — once the
//  sidebar card is pulled in to 6pt on all sides — leaves them flush
//  against the card's rounded corner. Xcode insets its traffic lights
//  further into the sidebar so the buttons sit in the sidebar with
//  breathing room.
//
//  We replicate that by reaching through SwiftUI to the host `NSWindow`
//  and shifting the three standard window buttons' frame origins. The
//  nudge is applied:
//    • once when the window first becomes available (via `WindowAccessor`), and
//    • on every `windowDidBecomeKey` / `windowDidResize` — AppKit has a
//      habit of re-laying-out these buttons on focus and resize, so we
//      re-apply to keep the offset sticky.
//
//  The Settings window uses a standard (non-hidden) title bar, so its
//  buttons are left untouched; `TrafficLightNudger.nudge` only modifies
//  windows whose `styleMask` contains `.fullSizeContentView` (the tell
//  for `.hiddenTitleBar`).
//

import AppKit
import SwiftUI

/// A transparent `NSViewRepresentable` whose sole job is to hand back
/// the `NSWindow` hosting it, so SwiftUI code can reach into AppKit for
/// window-level customisation. Used here for the traffic-light nudge.
struct WindowAccessor: NSViewRepresentable {
    let callback: (NSWindow) -> Void

    func makeNSView(context: Context) -> NSView {
        let view = NSView(frame: .zero)
        // `view.window` isn't populated at makeNSView time; defer until
        // the next runloop tick when the view has been attached.
        DispatchQueue.main.async { [weak view] in
            if let window = view?.window {
                callback(window)
            }
        }
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {}
}

/// Repositions the three standard window buttons (close / minimise /
/// zoom) by a fixed offset. Also installs observers so the offset
/// sticks across focus + resize events (AppKit otherwise resets the
/// buttons to their default positions).
enum TrafficLightNudger {
    private static let sentinelKey = "dev.nickanderssohn.nice.trafficLightsNudged"

    static func nudge(window: NSWindow, dx: CGFloat, dy: CGFloat) {
        // Skip windows without a full-size content view — those have
        // their own title bar chrome and shouldn't be touched (Settings
        // window, any future standard-chrome window).
        guard window.styleMask.contains(.fullSizeContentView) else { return }

        // Guard against double-install (WindowAccessor can fire its
        // callback more than once if the view moves windows).
        if objc_getAssociatedObject(window, sentinelKey) as? Bool == true { return }
        objc_setAssociatedObject(window, sentinelKey, true, .OBJC_ASSOCIATION_RETAIN)

        applyOffset(to: window, dx: dx, dy: dy)

        // AppKit relays out the buttons on focus and resize. Re-apply
        // the offset in both cases to keep it sticky.
        let center = NotificationCenter.default
        center.addObserver(
            forName: NSWindow.didBecomeKeyNotification,
            object: window,
            queue: .main
        ) { [weak window] _ in
            guard let window else { return }
            applyOffset(to: window, dx: dx, dy: dy)
        }
        center.addObserver(
            forName: NSWindow.didResizeNotification,
            object: window,
            queue: .main
        ) { [weak window] _ in
            guard let window else { return }
            applyOffset(to: window, dx: dx, dy: dy)
        }
    }

    /// Offset stored per-button so repeated applications are idempotent
    /// — we record each button's "canonical" origin on first touch and
    /// always apply the nudge to that, rather than stacking offsets.
    private static let canonicalKey = "dev.nickanderssohn.nice.buttonCanonicalOrigin"

    private static func applyOffset(to window: NSWindow, dx: CGFloat, dy: CGFloat) {
        for kind in [NSWindow.ButtonType.closeButton,
                     .miniaturizeButton,
                     .zoomButton] {
            guard let button = window.standardWindowButton(kind) else { continue }

            // Record the canonical origin on first touch; reuse it on
            // every subsequent call so the offset doesn't compound.
            let canonical: CGPoint
            if let stored = objc_getAssociatedObject(button, canonicalKey) as? NSValue {
                canonical = stored.pointValue
            } else {
                canonical = button.frame.origin
                objc_setAssociatedObject(
                    button,
                    canonicalKey,
                    NSValue(point: canonical),
                    .OBJC_ASSOCIATION_RETAIN
                )
            }

            button.setFrameOrigin(CGPoint(x: canonical.x + dx, y: canonical.y + dy))
        }
    }
}
