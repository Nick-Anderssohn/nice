//
//  WindowDragRegion.swift
//  Nice
//
//  A transparent NSView that drags the window on click-drag and
//  zooms on double-click. Sits in the empty chrome regions of the
//  custom toolbar so they feel like a native title bar even though
//  the window uses `.hiddenTitleBar` + `.fullSizeContentView`.
//
//  Why imperative `performDrag(with:)` instead of the cooperative
//  `mouseDownCanMoveWindow=true` pattern: the cooperative path
//  walks the hit-tested view's ancestor chain, and any sibling /
//  ancestor that returns `true` engages the title-bar drag
//  tracker — including for clicks that SwiftUI hit-tests as nil
//  (e.g. the rounded-corner pixels of an interactive widget with a
//  `RoundedRectangle` content shape). Result: dragging a widget
//  near its corner also drags the window. Apple's own forum
//  guidance (developer.apple.com/forums/thread/81149) and BJ
//  Homer's well-known gist both recommend the imperative path
//  instead. Widgets consume their own events naturally; only
//  events that fall past them reach this view's `mouseDown(with:)`,
//  which then explicitly hands tracking to AppKit via
//  `performDrag(with:)` (or `performZoom(_:)` for double-click).
//

import AppKit
import SwiftUI

struct WindowDragRegion: NSViewRepresentable {
    func makeNSView(context: Context) -> NSView { ChromeDragView() }

    func updateNSView(_ nsView: NSView, context: Context) {}

    fileprivate final class ChromeDragView: NSView {
        // Disable AppKit's cooperative title-bar drag tracker for
        // this view. Default for a transparent `NSView` is `true`,
        // which would let AppKit engage window-drag *before*
        // `mouseDown(with:)` is dispatched — bypassing our
        // imperative path entirely. Returning `false` ensures
        // normal event dispatch always reaches `mouseDown` so we
        // can decide explicitly between `performDrag` and
        // `performZoom`.
        override var mouseDownCanMoveWindow: Bool { false }

        // Allow drag/zoom even when the window is not yet key —
        // matches a native title bar's behaviour.
        override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

        override func mouseDown(with event: NSEvent) {
            guard let window = window else { return }
            if event.clickCount == 2 {
                window.performZoom(nil)
                return
            }
            window.performDrag(with: event)
        }
    }
}
