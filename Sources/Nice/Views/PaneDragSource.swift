//
//  PaneDragSource.swift
//  Nice
//
//  AppKit-driven drag source for pane pills. Wraps a SwiftUI
//  `Content` (the pill) inside an `NSHostingView` subclass that
//  intercepts `mouseDown` / `mouseDragged` to disambiguate three
//  outcomes from a single press:
//
//    • Tap (movement under threshold, mouse released)
//        → SwiftUI gestures fire normally. We forward `mouseDown`
//          via `super` and let `mouseUp` complete the click.
//    • Predominantly horizontal drag past the threshold
//        → `beginDraggingSession(with:event:source:)` — the pill
//          becomes a drag source for cross-window / tear-off.
//    • Predominantly vertical (or any non-horizontal) drag
//        → `window.performDrag(with:event)` — explicit window
//          drag. Bypasses `isMovable`, so it works even if AppKit's
//          cooperative title-bar tracker is disabled.
//
//  This is the audit-recommended pattern from `docs/research/
//  synthesis.md` step 3 / `audit-title-bar.md` "Recommended
//  direction" #5: the pill self-disambiguates instead of relying
//  on AppKit's drag-region computation. As a side effect it fixes
//  the pre-existing main behaviour where a pill press also dragged
//  the window — a press-and-drag now starts pane-drag (horizontal)
//  or window-drag (vertical), never both.
//
//  Why not `NSPanGestureRecognizer`: the v1 attempt did exactly
//  that, but the recognizer fires only after AppKit's mouse-down
//  delivery; with `isMovable=true` the title-bar tracker can win
//  the gesture before the recognizer transitions to `.began`.
//  Overriding `mouseDownCanMoveWindow=false` on the host plus
//  claiming `self` from `hitTest(_:)` for in-bounds points keeps
//  the title-bar tracker out of the way; the explicit
//  `mouseDown` override then owns the disambiguation.
//
//  Why tear-off requires AppKit and not SwiftUI's `.onDrag`: only
//  an `NSDraggingSource` sees `draggingSession(_:endedAt:operation:)`
//  with `operation == []`, which is how we detect "released over
//  empty space → spawn a new window."
//

import AppKit
import SwiftUI

struct PaneDragSource<Content: View>: NSViewRepresentable {
    let payload: PaneDragPayload
    let dragState: PaneDragState
    /// Called when the drag session ended outside any drop target.
    /// `screenPoint` is the cursor position at release in screen
    /// coordinates (Cocoa flipped: origin bottom-left). `pillOriginOffset`
    /// is the cursor offset within the pill at drag start (cursor x − pill
    /// minX, cursor y − pill maxY) so the destination window can subtract
    /// it from `screenPoint` to land the pill — not the traffic-light
    /// corner — under the cursor.
    let onTearOff: (_ screenPoint: CGPoint, _ pillOriginOffset: CGSize) -> Void
    @ViewBuilder let content: () -> Content

    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    func makeNSView(context: Context) -> NSView {
        let coordinator = context.coordinator
        let hosting = PaneDragHostingView(rootView: AnyView(content()))
        hosting.translatesAutoresizingMaskIntoConstraints = false
        hosting.coordinator = coordinator
        coordinator.hostView = hosting
        coordinator.payload = payload
        coordinator.dragState = dragState
        coordinator.onTearOff = onTearOff
        return hosting
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        let coordinator = context.coordinator
        coordinator.payload = payload
        coordinator.dragState = dragState
        coordinator.onTearOff = onTearOff
        if let hosting = nsView as? PaneDragHostingView {
            hosting.rootView = AnyView(content())
        }
    }

    /// `NSHostingView` subclass that owns the press disambiguation.
    /// Two responsibilities:
    ///
    /// 1. Keep AppKit's title-bar tracker out of the picture by
    ///    returning `false` from `mouseDownCanMoveWindow` and by
    ///    claiming `self` for every in-bounds point in `hitTest(_:)`.
    ///    SwiftUI's hosting machinery sprinkles transparent
    ///    descendants throughout the pill that report
    ///    `mouseDownCanMoveWindow == true` by default — without the
    ///    hit-test override, AppKit's drag-region walk lands on one
    ///    of those and the title-bar tracker fires before our
    ///    `mouseDown` does.
    ///
    /// 2. Override `mouseDown` / `mouseDragged` / `mouseUp` to drive
    ///    the tap-vs-pane-drag-vs-window-drag decision (see file
    ///    header). SwiftUI's gesture router runs internally inside
    ///    the host once `super.mouseDown(with:)` is called, so
    ///    `onTap` / `onHover` / button taps still fire as long as
    ///    we forward (i.e. for taps with no drag).
    final class PaneDragHostingView: NSHostingView<AnyView> {
        weak var coordinator: Coordinator?

        /// pt of total motion required before we commit to a
        /// pane-drag or window-drag. Slightly higher than AppKit's
        /// 4pt slop so a stationary press registers as a tap on
        /// trackpads with palm contact.
        private let dragThreshold: CGFloat = 6

        /// `mouseDown` event saved at press time; needed verbatim
        /// when we eventually call `beginDraggingSession` or
        /// `performDrag` (both want the gesture-initiating event,
        /// not whichever `mouseDragged` tipped us over the
        /// threshold).
        private var pressEvent: NSEvent?
        private var pressLocationInWindow: NSPoint?
        private var didDecideDrag: Bool = false

        override var mouseDownCanMoveWindow: Bool { false }

        override func hitTest(_ point: NSPoint) -> NSView? {
            // Strict in-bounds claim: convert from parent space and
            // return self iff the point is inside our bounds.
            // `NSPointInRect` treats edge pixels consistently, so
            // boundary points don't fall through to the SwiftUI
            // descendants we're trying to mask.
            let local = convert(point, from: superview)
            return NSPointInRect(local, bounds) ? self : nil
        }

        override func mouseDown(with event: NSEvent) {
            // Capture so `mouseDragged` can compare deltas, and so
            // the eventual drag-session call has the gesture-
            // initiating event.
            pressEvent = event
            pressLocationInWindow = event.locationInWindow
            didDecideDrag = false
            // Forward to super so SwiftUI's gesture router sees the
            // press. If movement never crosses the threshold,
            // `mouseUp` lets SwiftUI's tap fire normally.
            super.mouseDown(with: event)
        }

        override func mouseDragged(with event: NSEvent) {
            // Once we've committed to a drag we stop forwarding
            // motion to SwiftUI — its gesture state will be cleaned
            // up by the next press or by the AppKit drag-session
            // takeover.
            guard !didDecideDrag,
                  let press = pressEvent,
                  let start = pressLocationInWindow
            else {
                if !didDecideDrag { super.mouseDragged(with: event) }
                return
            }
            let dx = event.locationInWindow.x - start.x
            let dy = event.locationInWindow.y - start.y
            let dist2 = dx * dx + dy * dy
            guard dist2 >= dragThreshold * dragThreshold else {
                super.mouseDragged(with: event)
                return
            }

            didDecideDrag = true
            // Predominantly horizontal motion → pane drag (the user
            // is reordering / tearing off). Anything else is window
            // movement. The `>=` on the horizontal branch keeps
            // perfectly diagonal motion in pane-drag — pane-drag is
            // the "interesting" gesture; window-drag is the fallback.
            if abs(dx) >= abs(dy) {
                coordinator?.beginPaneDragSession(initialEvent: press)
            } else {
                self.window?.performDrag(with: press)
            }
        }

        override func mouseUp(with event: NSEvent) {
            pressEvent = nil
            pressLocationInWindow = nil
            didDecideDrag = false
            super.mouseUp(with: event)
        }

        required init(rootView: AnyView) {
            super.init(rootView: rootView)
        }

        @MainActor required dynamic init?(coder aDecoder: NSCoder) {
            super.init(coder: aDecoder)
        }
    }

    @MainActor
    final class Coordinator: NSObject, NSDraggingSource {
        var payload: PaneDragPayload?
        var dragState: PaneDragState?
        var onTearOff: ((CGPoint, CGSize) -> Void)?
        weak var hostView: NSView?

        /// Cursor offset within the pill at drag start, computed
        /// once at `beginDraggingSession` time. Saved here so the
        /// `endedAt:operation:` callback can pass it to `onTearOff`
        /// for the destination window's origin correction.
        private var pillOriginOffset: CGSize = .zero

        func beginPaneDragSession(initialEvent event: NSEvent) {
            guard let view = hostView,
                  let payload = self.payload,
                  let dragState = self.dragState
            else { return }

            // Cursor offset within the pill: subtract pill's minX
            // from cursor x, and (pill maxY − cursor y) from y so
            // the value is what the destination window should
            // subtract from `screenPoint` to make the pill land
            // under the cursor on tear-off.
            let local = view.convert(event.locationInWindow, from: nil)
            pillOriginOffset = CGSize(
                width: local.x - view.bounds.minX,
                height: view.bounds.maxY - local.y
            )

            let preview = Self.snapshot(view: view)
            let item = NSPasteboardItem()
            item.setData(
                payload.encoded(),
                forType: NSPasteboard.PasteboardType(PaneDragPayload.utTypeIdentifier)
            )
            let dragItem = NSDraggingItem(pasteboardWriter: item)
            dragItem.setDraggingFrame(view.bounds, contents: preview)

            // Pre-set the drag session so source-pill fade + drop
            // indicators are live from the first hover frame.
            dragState.session = PaneDragSession(payload: payload)

            view.beginDraggingSession(
                with: [dragItem], event: event, source: self
            )
        }

        private static func snapshot(view: NSView) -> NSImage {
            let bounds = view.bounds
            guard bounds.width > 0, bounds.height > 0,
                  let rep = view.bitmapImageRepForCachingDisplay(in: bounds)
            else { return NSImage(size: bounds.size) }
            view.cacheDisplay(in: bounds, to: rep)
            let image = NSImage(size: bounds.size)
            image.addRepresentation(rep)
            return image
        }

        // MARK: - NSDraggingSource

        nonisolated func draggingSession(
            _ session: NSDraggingSession,
            sourceOperationMaskFor context: NSDraggingContext
        ) -> NSDragOperation {
            switch context {
            case .outsideApplication:
                // Other apps refuse pane drags; we want the no-target
                // signal so our endedAt handler can spawn a new window.
                return []
            case .withinApplication:
                return .move
            @unknown default:
                return .move
            }
        }

        func draggingSession(
            _ session: NSDraggingSession,
            endedAt screenPoint: NSPoint,
            operation: NSDragOperation
        ) {
            // Phase B keeps the SwiftUI DropDelegate destinations
            // (transition to NSDraggingDestination is task #4), so
            // we still consult `didDropOnTarget` rather than the
            // `operation` argument — SwiftUI's DropDelegate doesn't
            // propagate `NSDragOperation` back through the system to
            // the source.
            let acceptedInApp = dragState?.session?.didDropOnTarget ?? false
            if operation == [] && !acceptedInApp {
                onTearOff?(
                    CGPoint(x: screenPoint.x, y: screenPoint.y),
                    pillOriginOffset
                )
            }
            // Always clear so source-pill fade un-sticks even when
            // the drop delegate forgot to (defensive).
            dragState?.session = nil
            pillOriginOffset = .zero
        }
    }
}
