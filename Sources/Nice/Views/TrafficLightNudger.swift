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
@MainActor
enum TrafficLightNudger {
    /// Windows we've already installed observers on — keyed by identity
    /// so a second `nudge(window:)` call for the same window is a no-op.
    private static var observed: Set<ObjectIdentifier> = []

    /// Per-button canonical (pre-nudge) origin, captured on first touch
    /// and reused on every subsequent `applyOffset` so the offset is
    /// idempotent instead of compounding on each focus / resize event.
    ///
    /// Keyed by `ObjectIdentifier(button)` rather than an
    /// `objc_setAssociatedObject` key — Swift `String` literals don't
    /// produce stable pointers for the associated-object API, which
    /// caused the lookup to miss and re-capture the already-nudged
    /// origin (bug: traffic lights drifted inward + down on every
    /// Settings-window close, which fires `didBecomeKey` on the main
    /// window).
    private static var canonicalOrigins: [ObjectIdentifier: CGPoint] = [:]

    static func nudge(window: NSWindow, dx: CGFloat, dy: CGFloat) {
        // Skip windows without a full-size content view — those have
        // their own title bar chrome and shouldn't be touched (Settings
        // window, any future standard-chrome window).
        guard window.styleMask.contains(.fullSizeContentView) else { return }

        // Guard against double-install (WindowAccessor can fire its
        // callback more than once if the view moves windows).
        let windowKey = ObjectIdentifier(window)
        guard observed.insert(windowKey).inserted else { return }

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
            MainActor.assumeIsolated {
                applyOffset(to: window, dx: dx, dy: dy)
            }
        }
        center.addObserver(
            forName: NSWindow.didResizeNotification,
            object: window,
            queue: .main
        ) { [weak window] _ in
            guard let window else { return }
            MainActor.assumeIsolated {
                applyOffset(to: window, dx: dx, dy: dy)
            }
        }
    }

    private static func applyOffset(to window: NSWindow, dx: CGFloat, dy: CGFloat) {
        for kind in [NSWindow.ButtonType.closeButton,
                     .miniaturizeButton,
                     .zoomButton] {
            guard let button = window.standardWindowButton(kind) else { continue }

            // Record the canonical origin on first touch; reuse it on
            // every subsequent call so the offset doesn't compound.
            let buttonKey = ObjectIdentifier(button)
            let canonical = canonicalOrigins[buttonKey] ?? {
                let origin = button.frame.origin
                canonicalOrigins[buttonKey] = origin
                return origin
            }()

            button.setFrameOrigin(CGPoint(x: canonical.x + dx, y: canonical.y + dy))
        }
    }
}
