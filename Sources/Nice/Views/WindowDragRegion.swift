//
//  WindowDragRegion.swift
//  Nice
//
//  A single AppKit marker view that the chrome event router hit-tests to
//  recognise EMPTY chrome (drag to move, double-click to zoom) under
//  `.hiddenTitleBar` + `.fullSizeContentView`.
//
//  `ChromeDragStripView` carries NO behaviour of its own: native drag is
//  already disabled (each Nice window sets `isMovable = false`, asserted
//  per-press by `ChromeEventRouter`), so `mouseDownCanMoveWindow` is
//  `false`. The view exists purely so the router's per-press hit-test can
//  classify a press on empty chrome as `.strip` by finding this class in
//  the hit view's ancestor chain. SwiftUI lays it into the chrome
//  `.background` behind the interactive controls, so pills / buttons claim
//  their own presses and only empty chrome resolves to the strip.
//
//  Drag, double-click-zoom, and the event-time `isMovable = false`
//  invariant are ALL owned by `ChromeEventRouter` now (installed by each
//  window's `WindowChromeController`). This file used to also host
//  `windowDraggable` (a SwiftUI drag gesture), `TitleBarZoomMonitor` (the
//  process-wide double-click monitor), and `DoubleClickTitleBarAction` —
//  the router replaced the first two and absorbed the third
//  (`DoubleClickTitleBarAction` now lives in `ChromeEventRouter.swift`).
//

import AppKit
import SwiftUI

struct WindowDragRegion: NSViewRepresentable {
    func makeNSView(context: Context) -> NSView {
        ChromeDragStripView()
    }

    func updateNSView(_ nsView: NSView, context: Context) {}
}

/// Pure marker for the router's hit-test. `mouseDownCanMoveWindow` is
/// `false` — native title-bar drag is off via `isMovable = false`, and the
/// router (not this flag) owns the empty-chrome drag. Its only job is to be
/// a recognisable class in the ancestor chain so `ChromeEventRouter`
/// classifies a press on this region as empty chrome.
final class ChromeDragStripView: NSView {
    override var mouseDownCanMoveWindow: Bool { false }
}
