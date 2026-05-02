//
//  PaneDragSource.swift
//  Nice
//
//  AppKit-driven drag source for pane pills. Wraps a SwiftUI
//  `Content` (the pill) inside an `NSHostingView`, then attaches an
//  `NSPanGestureRecognizer` so a sustained mouse drag triggers an
//  `NSDraggingSession` while a plain click still falls through to
//  the SwiftUI tap gesture inside the pill.
//
//  Why AppKit and not SwiftUI's `.onDrag`: the tear-off case requires
//  knowing when a drag ended outside any drop target, which only an
//  `NSDraggingSource` sees (via `draggingSession(_:endedAt:operation:)`
//  with `operation == []`). Keeping the full drag flow on the AppKit
//  side avoids two parallel state machines (one SwiftUI for in-window
//  drops, one AppKit for tear-off).
//

import AppKit
import SwiftUI

struct PaneDragSource<Content: View>: NSViewRepresentable {
    let payload: PaneDragPayload
    let dragState: PaneDragState
    /// Called when the drag session ended outside any drop target.
    /// `screenPoint` is the cursor position at release in screen
    /// coordinates (Cocoa flipped: origin bottom-left).
    let onTearOff: (CGPoint) -> Void
    @ViewBuilder let content: () -> Content

    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    func makeNSView(context: Context) -> NSView {
        let coordinator = context.coordinator
        let hosting = NonDraggableHostingView(rootView: AnyView(content()))
        hosting.translatesAutoresizingMaskIntoConstraints = false

        let pan = NSPanGestureRecognizer(
            target: coordinator,
            action: #selector(Coordinator.handlePan(_:))
        )
        // 4pt is AppKit's default drag-recognition slop; we accept the
        // default by not setting `numberOfTouchesRequired` etc.
        hosting.addGestureRecognizer(pan)

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
        if let hosting = nsView as? NonDraggableHostingView {
            hosting.rootView = AnyView(content())
        }
    }

    /// `NSHostingView` subclass that opts out of AppKit's
    /// click-to-move-window walk for the pill region. Two cooperating
    /// overrides are needed — `mouseDownCanMoveWindow = false` alone
    /// is not enough:
    ///
    /// AppKit's window-drag tracker queries `mouseDownCanMoveWindow`
    /// on the LEAF NSView returned by `hitTest:`, not on this wrapper.
    /// The pill's `.contentShape(RoundedRectangle(...))` makes
    /// SwiftUI's hit-test return nil for the four rounded corners
    /// (transparent to gestures). On a `nil` hit, AppKit descends to
    /// the next z-order view at that point — which is the
    /// `WindowDragRegion` sitting in the toolbar's chrome background.
    /// That view DOES report `true`, so window-drag engages.
    ///
    /// The `hitTest:` override claims `self` for any in-bounds point
    /// where SwiftUI's normal hit-test would have returned nil (the
    /// transparent corners). When SwiftUI returns a real leaf — pill
    /// body, close-X button — we forward to that leaf so SwiftUI
    /// taps and the close button keep working unchanged. AppKit's
    /// query then lands on `self` (corners) or on the leaf (body /
    /// button); both report `mouseDownCanMoveWindow = false` and the
    /// fall-through to `WindowDragRegion` is blocked.
    final class NonDraggableHostingView: NSHostingView<AnyView> {
        override var mouseDownCanMoveWindow: Bool { false }

        override func hitTest(_ point: NSPoint) -> NSView? {
            let local = self.convert(point, from: superview)
            guard self.bounds.contains(local) else { return nil }
            if let leaf = super.hitTest(point), leaf !== self {
                return leaf
            }
            // Transparent pixel inside our bounds — claim it so
            // AppKit's window-drag walk stops at us.
            return self
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
        var onTearOff: ((CGPoint) -> Void)?
        weak var hostView: NSView?

        @objc func handlePan(_ recognizer: NSPanGestureRecognizer) {
            // Only start the drag session at gesture start; subsequent
            // events flow through the AppKit drag-session loop.
            guard recognizer.state == .began else { return }
            guard let view = hostView,
                  let payload = self.payload,
                  let dragState = self.dragState
            else { return }
            // Need a real NSEvent to seed beginDraggingSession.
            // `NSApp.currentEvent` is the mouseDragged that triggered
            // the recognizer's `began` state.
            guard let event = NSApp.currentEvent else { return }

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
            let acceptedInApp = dragState?.session?.didDropOnTarget ?? false
            if operation == [] && !acceptedInApp {
                onTearOff?(CGPoint(x: screenPoint.x, y: screenPoint.y))
            }
            // Always clear so source-pill fade un-sticks even when the
            // drop delegate forgot to (defensive).
            dragState?.session = nil
        }
    }
}
