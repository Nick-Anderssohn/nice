//
//  WindowBridge.swift
//  Nice
//
//  SYNCHRONOUS replacement for the old `WindowAccessor`. Both hand SwiftUI
//  code the host `NSWindow`, but the timing differs in a way that matters:
//
//    • `WindowAccessor` deferred its callback one runloop tick
//      (`DispatchQueue.main.async` in `makeNSView`) because `view.window`
//      wasn't populated at `makeNSView` time. That tick happened to land
//      AFTER SwiftUI finished configuring the window.
//
//    • `WindowBridge` fires from `viewDidMoveToWindow` — synchronously, the
//      instant the view is attached to a window, BEFORE first draw. It also
//      re-fires if the view is later moved to a different window.
//
//  Synchronous attach is what lets the controller's `isMovable` policy and
//  the traffic-light placer go live as early as possible (then self-heal
//  via their own observers once the styleMask settles). It is the WRONG
//  timing, however, for one-shot writes that only "stick" because they ran
//  after SwiftUI's window finalization (the `CloseConfirmationDelegate`
//  wrap, the Settings `.resizable` insert, the UITest frame pin) — those
//  callers must defer one runloop themselves. The bridge makes no timing
//  promise beyond "fires at attach"; ordering-sensitive work is the
//  caller's responsibility.
//
//  Idempotence is also the callee's job — `viewDidMoveToWindow` can fire
//  more than once (the view moving windows), so `onAttach` may be invoked
//  repeatedly for the same window. `WindowChromeController.adopt` and
//  `WindowRegistry.register` are both already idempotent.
//

import AppKit
import SwiftUI

struct WindowBridge: NSViewRepresentable {
    let onAttach: (NSWindow) -> Void

    func makeNSView(context: Context) -> BridgeView {
        let view = BridgeView()
        view.onAttach = onAttach
        return view
    }

    func updateNSView(_ nsView: BridgeView, context: Context) {
        nsView.onAttach = onAttach
    }
}

/// The backing `NSView`. Fires `onAttach` synchronously from
/// `viewDidMoveToWindow` whenever it gains a window.
final class BridgeView: NSView {
    var onAttach: ((NSWindow) -> Void)?

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if let window {
            onAttach?(window)
        }
    }
}
