//
//  PaneDragSource.swift
//  Nice
//
//  AppKit-driven drag source for pane pills. Wraps a SwiftUI `Content`
//  (the pill) inside an `NSHostingView` subclass that intercepts
//  `mouseDown` / `mouseDragged` / `mouseUp` to disambiguate two outcomes
//  from a single press:
//
//    • Tap (movement under threshold, mouse released)
//        → SwiftUI gestures fire normally. We forward `mouseDown`
//          via `super` and let `mouseUp` complete the click — so the
//          pill's select / rename / close / hover all keep working.
//    • Drag past the threshold (any direction)
//        → `beginDraggingSession(with:event:source:)` — the pill becomes
//          a drag source for reorder / cross-window move / tear-off.
//
//  Why this exists at all: only an `NSDraggingSource` sees
//  `draggingSession(_:endedAt:operation:)` with `operation == []`, which
//  is how we detect "released over empty desktop → tear off into a new
//  window." Pure SwiftUI `.onDrag` owns its drag session and exposes no
//  end callback, so it cannot drive tear-off.
//
//  ⚠️ WINDOW-DRAG SELECTIVITY — load-bearing, behavioral invariant.
//  Swapping the pill's SwiftUI `.onDrag` for this AppKit source removes
//  the higher-priority child gesture that made the toolbar's
//  `windowDragGesture` yield. An AppKit view consuming `mouseDown` does
//  NOT make an ancestor SwiftUI `DragGesture` yield (gesture recognizers
//  see events independently of subview `mouseDown`). So this source ALSO
//  re-solves the yield explicitly: it flips `WindowDragGate
//  .pillPressInProgress` true on `mouseDown` and `windowDragGesture`
//  refuses to move the window while that flag is set. The flag is cleared
//  on `mouseUp` (tap) or `draggingSession(_:endedAt:)` (drag). The
//  `WindowDragUITests` / `PaneReorderUITests` regression net is the only
//  real check that this stays correct — keep it green.
//

import AppKit
import SwiftUI

/// Shared one-bit channel that lets a pane-pill press veto the toolbar's
/// empty-chrome window-drag gesture. Owned by `WindowToolbarView` (as
/// `@State`) and injected into the pill subtree via `.environment` so the
/// AppKit drag source (`PaneDragSource.PaneDragHostingView`) can set it and
/// `windowDragGesture` can read it.
///
/// This is the explicit replacement for the gesture-priority arbitration
/// that the pill's old SwiftUI `.onDrag` provided for free. See the file
/// header for the full invariant.
@MainActor
@Observable
final class WindowDragGate {
    /// True between a `mouseDown` that landed on a pill and the end of
    /// that press (its `mouseUp`, or the end of the drag session it
    /// spawned). While true, `windowDragGesture` must not `performDrag`.
    ///
    /// Read IMPERATIVELY from `windowDragGesture`'s `.onChanged` closure
    /// at event time — no view's `body` depends on it, so the
    /// `@Observable` conformance here exists only to satisfy
    /// `@Environment(WindowDragGate.self)` injection, not to drive any
    /// reactive re-render. Don't add a `body`-level read expecting one.
    var pillPressInProgress = false
}

/// Pure classification of how a pane-pill drag ended, factored out of the
/// `NSDraggingSource` callback so the load-bearing decision (does this
/// drag tear off, snap back, or was it already handled by a drop target?)
/// is unit-testable without a live `NSDraggingSession` or real windows.
enum PaneDragEnd {
    enum Outcome: Equatable {
        /// Released over empty desktop → open the pane in a new window.
        case tearOff
        /// Released over an app window's non-target chrome, or cancelled
        /// (Esc), or released in a no-screen dead zone → snap back.
        case withdraw
        /// A drop target (reorder / cross-window strip) already accepted
        /// the drag (`operation == .move`) → the drop delegate owns the
        /// cleanup; do nothing here.
        case ignore
    }

    /// Decide the end outcome from the drag operation, the release point
    /// (global Cocoa screen coordinates, origin bottom-left), the frames
    /// of the app's real content windows, and the screens' frames.
    ///
    /// - `operation != []` ⇒ `.ignore` (a destination accepted it).
    /// - point inside any content window ⇒ `.withdraw` (released over our
    ///   own / another window's chrome, not a drop target).
    /// - point on no screen (a multi-display dead zone) ⇒ `.withdraw`
    ///   (never tear off into a place the new window can't be seen). An
    ///   empty `screenFrames` skips this guard (test convenience).
    /// - otherwise (empty desktop) ⇒ `.tearOff`.
    static func outcome(
        operation: NSDragOperation,
        screenPoint: NSPoint,
        contentWindowFrames: [NSRect],
        screenFrames: [NSRect]
    ) -> Outcome {
        guard operation == [] else { return .ignore }
        if contentWindowFrames.contains(where: { $0.contains(screenPoint) }) {
            return .withdraw
        }
        if !screenFrames.isEmpty,
           !screenFrames.contains(where: { $0.contains(screenPoint) }) {
            return .withdraw
        }
        return .tearOff
    }
}

/// Wraps a pill view in an AppKit host that owns the press → tap-vs-drag
/// decision and acts as the `NSDraggingSource` for cross-window move and
/// tear-off. The drop side (reorder + cross-window move) is unchanged: the
/// pasteboard carries the same plain pane-id string the old `.onDrag` put
/// there, so the existing `.onDrop` strip delegates keep working.
struct PaneDragSource<Content: View>: NSViewRepresentable {
    /// Identity + source context for the dragged pane.
    let paneId: String
    let sourceTabId: String
    let sourceIndex: Int
    let sourceWindowSessionId: String

    /// App-global services (live-pane registry + window registry + the
    /// tear-off controller) and this window's `SessionsModel` (the live
    /// detach used by the registry handle's `claim`).
    let services: NiceServices
    let sessions: SessionsModel
    /// This strip's ephemeral drag state — the coordinator sets
    /// `.session` at drag start so the reorder insertion line is live
    /// from the first hover frame, and clears it when the drag ends.
    let dragState: PaneStripDragState
    /// The window-drag veto flag (see `WindowDragGate`).
    let dragGate: WindowDragGate
    /// `openWindow(id: "main")` wrapped in a closure so the tear-off
    /// controller (a struct) can open a window without `@Environment`.
    let openWindow: () -> Void

    @ViewBuilder let content: () -> Content

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> NSView {
        let coordinator = context.coordinator
        let hosting = PaneDragHostingView(rootView: AnyView(content()))
        hosting.translatesAutoresizingMaskIntoConstraints = false
        hosting.coordinator = coordinator
        coordinator.hostView = hosting
        coordinator.dragGate = dragGate
        apply(to: coordinator)
        return hosting
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        let coordinator = context.coordinator
        coordinator.dragGate = dragGate
        apply(to: coordinator)
        if let hosting = nsView as? PaneDragHostingView {
            hosting.rootView = AnyView(content())
        }
    }

    /// SwiftUI tears the representable down (pane closed, tab dissolved,
    /// row diffed out). If a drag was still in flight, unwind its
    /// published registry handle + the window-drag gate here so neither
    /// leaks past the pill's lifetime.
    static func dismantleNSView(_ nsView: NSView, coordinator: Coordinator) {
        coordinator.cancelInFlightDrag()
    }

    /// Refresh the coordinator's drag config on every SwiftUI update so
    /// `sourceIndex` (which changes when the strip reorders) and the
    /// captured closures stay current.
    private func apply(to coordinator: Coordinator) {
        coordinator.config = Coordinator.Config(
            paneId: paneId,
            sourceTabId: sourceTabId,
            sourceIndex: sourceIndex,
            sourceWindowSessionId: sourceWindowSessionId,
            services: services,
            sessions: sessions,
            dragState: dragState,
            openWindow: openWindow
        )
    }

    /// `NSHostingView` subclass that owns the press disambiguation. Two
    /// responsibilities:
    ///
    /// 1. Keep AppKit's title-bar drag tracker out of the picture by
    ///    returning `false` from `mouseDownCanMoveWindow` and claiming
    ///    `self` for every in-bounds point in `hitTest(_:)`. SwiftUI's
    ///    hosting machinery sprinkles transparent descendants that report
    ///    `mouseDownCanMoveWindow == true`; without the hit-test override
    ///    AppKit's drag-region walk could land on one of those.
    /// 2. Override `mouseDown` / `mouseDragged` / `mouseUp` to drive the
    ///    tap-vs-drag decision. SwiftUI's gesture router still runs inside
    ///    the host once `super.mouseDown` is forwarded, so taps / hovers /
    ///    the close button keep working for presses that never drag.
    final class PaneDragHostingView: NSHostingView<AnyView> {
        weak var coordinator: Coordinator?

        /// Total motion (pt) required before we commit to a drag. Slightly
        /// above AppKit's 4pt slop so a stationary press on a trackpad
        /// (palm contact, micro-jitter) still registers as a tap.
        private let dragThreshold: CGFloat = 6

        /// The `mouseDown` event saved at press time. Both
        /// `beginDraggingSession` and the threshold math want the
        /// gesture-initiating event, not whichever `mouseDragged` tipped
        /// us over the line.
        private var pressEvent: NSEvent?
        private var pressLocationInWindow: NSPoint?
        private var didDecideDrag = false

        override var mouseDownCanMoveWindow: Bool { false }

        override func hitTest(_ point: NSPoint) -> NSView? {
            let local = convert(point, from: superview)
            return NSPointInRect(local, bounds) ? self : nil
        }

        override func mouseDown(with event: NSEvent) {
            pressEvent = event
            pressLocationInWindow = event.locationInWindow
            didDecideDrag = false
            // Veto the toolbar's window-drag gesture for the lifetime of
            // this press: a drag that began on a pill must never move the
            // window. Set BEFORE any movement so `windowDragGesture`'s
            // `.onChanged` (which only fires after the 2pt minimum) always
            // sees the flag.
            coordinator?.dragGate?.pillPressInProgress = true
            // Forward so SwiftUI's gesture router sees the press; a tap
            // (no drag) then completes normally on `mouseUp`.
            super.mouseDown(with: event)
        }

        override func mouseDragged(with event: NSEvent) {
            guard !didDecideDrag,
                  let press = pressEvent,
                  let start = pressLocationInWindow
            else {
                if !didDecideDrag { super.mouseDragged(with: event) }
                return
            }
            let dx = event.locationInWindow.x - start.x
            let dy = event.locationInWindow.y - start.y
            guard dx * dx + dy * dy >= dragThreshold * dragThreshold else {
                super.mouseDragged(with: event)
                return
            }
            // Any drag past the threshold is a pane drag — pill presses
            // never move the window (that's the empty-chrome gesture's
            // job). Stop forwarding motion to SwiftUI now that the AppKit
            // drag session is taking over.
            didDecideDrag = true
            coordinator?.beginPaneDragSession(initialEvent: press)
        }

        override func mouseUp(with event: NSEvent) {
            pressEvent = nil
            pressLocationInWindow = nil
            didDecideDrag = false
            // Tap path: clear the window-drag veto here (the drag path
            // clears it in `draggingSession(_:endedAt:)` instead, since a
            // drag session swallows this `mouseUp`).
            coordinator?.dragGate?.pillPressInProgress = false
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
        /// Everything the drag needs, refreshed from SwiftUI on each
        /// update so `sourceIndex` and the closures don't go stale.
        struct Config {
            let paneId: String
            let sourceTabId: String
            let sourceIndex: Int
            let sourceWindowSessionId: String
            let services: NiceServices
            let sessions: SessionsModel
            let dragState: PaneStripDragState
            let openWindow: () -> Void
        }

        var config: Config?
        weak var dragGate: WindowDragGate?
        weak var hostView: NSView?

        /// True from `beginDraggingSession` until `draggingSession(_:
        /// endedAt:)` (or a teardown). Lets `cancelInFlightDrag` know
        /// whether there's published registry state + a raised window-drag
        /// gate to unwind if the pill is removed mid-drag.
        private var isDragInFlight = false

        func beginPaneDragSession(initialEvent event: NSEvent) {
            guard let view = hostView, let c = config else { return }

            // Mirror the old `.onDrag` side effects: stash the origin for
            // synchronous hover access (the pasteboard only yields its
            // payload at drop time) and publish a live-pane handle so a
            // drop in another window — or the tear-off controller — can
            // claim this pane's running pty + view from the registry.
            c.dragState.session = PaneDragSession(
                origin: PaneDragOrigin(
                    paneId: c.paneId,
                    sourceTabId: c.sourceTabId,
                    sourceIndex: c.sourceIndex,
                    sourceWindowSessionId: c.sourceWindowSessionId
                ),
                target: nil
            )
            c.services.livePaneRegistry.publish(
                LivePaneRegistry.Handle(
                    paneId: c.paneId,
                    sourceWindowSessionId: c.sourceWindowSessionId,
                    sourceTabId: c.sourceTabId,
                    claim: { [weak sessions = c.sessions, tabId = c.sourceTabId, paneId = c.paneId] in
                        sessions?.detachLivePane(tabId: tabId, paneId: paneId)
                    }
                )
            )

            // Same plain pane-id string the old `.onDrag` wrote, so the
            // existing `.onDrop(of: [.text])` strip delegates (reorder +
            // cross-window move) keep validating and committing unchanged.
            let item = NSPasteboardItem()
            item.setString(c.paneId, forType: .string)
            let dragItem = NSDraggingItem(pasteboardWriter: item)
            dragItem.setDraggingFrame(view.bounds, contents: Self.snapshot(view: view))

            isDragInFlight = true
            view.beginDraggingSession(with: [dragItem], event: event, source: self)
        }

        /// Unwind a drag that's still in flight when the pill leaves the
        /// view tree (pane closed from a menu, tab dissolved, SwiftUI diff
        /// dropped the row). AppKit normally guarantees
        /// `draggingSession(_:endedAt:)`, but if the host view is removed
        /// from its window the end callback isn't guaranteed — without
        /// this the published `LivePaneRegistry` handle would stay
        /// `currentDrag` (blocking future drags) and `pillPressInProgress`
        /// could stay raised (freezing empty-chrome window dragging).
        func cancelInFlightDrag() {
            guard isDragInFlight else { return }
            isDragInFlight = false
            if let c = config {
                c.services.livePaneRegistry.withdraw(paneId: c.paneId)
                c.dragState.session = nil
            }
            dragGate?.pillPressInProgress = false
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

        func draggingSession(
            _ session: NSDraggingSession,
            sourceOperationMaskFor context: NSDraggingContext
        ) -> NSDragOperation {
            switch context {
            case .outsideApplication:
                // Refuse drops in other apps: we want the empty `operation`
                // signal so `endedAt` can recognise "released over the
                // desktop → tear off."
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
            isDragInFlight = false
            guard let c = config else {
                dragGate?.pillPressInProgress = false
                return
            }

            // Classify the end via the pure helper (unit-tested in
            // `PaneDragEndTests`): `.tearOff` (empty desktop), `.withdraw`
            // (over chrome / cancel / off-screen), or `.ignore` (a drop
            // delegate already accepted it — it owns the handle cleanup).
            switch PaneDragEnd.outcome(
                operation: operation,
                screenPoint: screenPoint,
                contentWindowFrames: Self.contentWindowFrames(),
                screenFrames: NSScreen.screens.map(\.frame)
            ) {
            case .tearOff:
                PaneTearOffController(services: c.services).tearOff(
                    paneId: c.paneId,
                    sourceWindowSessionId: c.sourceWindowSessionId,
                    at: screenPoint,
                    openWindow: c.openWindow
                )
            case .withdraw:
                c.services.livePaneRegistry.withdraw(paneId: c.paneId)
            case .ignore:
                break
            }

            // Always clear the ephemeral drag state and the window-drag
            // veto so the source pill un-sticks even if a drop delegate
            // forgot to (defensive).
            c.dragState.session = nil
            dragGate?.pillPressInProgress = false
        }

        /// Frames of the app's real CONTENT windows in global Cocoa screen
        /// space. Filters out transient AppKit/SwiftUI helper windows
        /// (panels, popovers, color/field-editor panels, tooltips,
        /// zero-size helpers) — if one of those happened to cover the
        /// release point it would wrongly suppress a real desktop
        /// tear-off. The filter strictly KEEPS genuine content windows, so
        /// it never turns a drop onto an actual window into a tear-off.
        private static func contentWindowFrames() -> [NSRect] {
            NSApp.windows
                .filter { window in
                    window.isVisible
                        && !(window is NSPanel)
                        && window.contentView != nil
                        && !window.frame.isEmpty
                }
                .map(\.frame)
        }
    }
}
