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

    /// `NSHostingView` subclass that takes AppKit's cooperative
    /// window-drag paths out of play for the pill region. Three
    /// defences, layered:
    ///
    /// 1. `NSWindow.isMovable = false` — set in `AppShellView`'s
    ///    `WindowAccessor`, disables the entire AppKit title-bar
    ///    tracker for the window. (`performDrag(with:)` still
    ///    works, so chrome drag remains functional.)
    /// 2. `mouseDownCanMoveWindow = false` — opt out of the
    ///    cooperative `mouseDownCanMoveWindow` chain. Defends
    ///    against `isMovable` being flipped back on.
    /// 3. `hitTest(_:)` claims `self` for every in-bounds point,
    ///    short-circuiting AppKit's descent into transparent
    ///    SwiftUI internals (which inherit `mouseDownCanMoveWindow
    ///    == true` and would otherwise be the leaf).
    ///
    /// SwiftUI's tap/hover/drag gestures still work: they're
    /// dispatched by SwiftUI's own event router inside
    /// `NSHostingView` once the view receives the event, descending
    /// the SwiftUI tree internally — independent of AppKit's NSView
    /// hit-test result.
    /// Covered by `PaneDragWindowMoveUITests`.
    final class NonDraggableHostingView: NSHostingView<AnyView> {
        override var mouseDownCanMoveWindow: Bool { false }

        override func hitTest(_ point: NSPoint) -> NSView? {
            let local = convert(point, from: superview)
            return NSPointInRect(local, bounds) ? self : nil
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
