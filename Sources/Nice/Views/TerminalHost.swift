//
//  TerminalHost.swift
//  Nice
//
//  Phase 4: SwiftUI wrapper around a stable `LocalProcessTerminalView`
//  instance owned by the containing session. `updateNSView` is a no-op
//  by design — the session object (and therefore the view) outlives any
//  individual SwiftUI redraw, keeping the pty and its scrollback alive
//  across tab switches.
//

import SwiftTerm
import SwiftUI

struct TerminalHost: NSViewRepresentable {
    let view: LocalProcessTerminalView
    var focus: Bool = false

    func makeNSView(context: Context) -> LocalProcessTerminalView {
        view.scrollerStyle = .overlay
        if let scroller = findScroller(in: view) {
            scroller.isHidden = true
            context.coordinator.startObserving(scroller)
        }
        // Arm the focus latch before AppKit attaches the view so the
        // ensuing `viewDidMove…` callbacks grab first responder atomically.
        // Without this, the window's first responder is briefly nil between
        // the outgoing pane's teardown and the async hop below — any key
        // pressed in that window (common after ctrl+D ctrl+D exits Claude)
        // falls off the responder chain and beeps.
        if focus, let nice = view as? NiceTerminalView {
            nice.wantsFocusOnAttach = true
        }
        return view
    }

    func updateNSView(_ nsView: LocalProcessTerminalView, context: Context) {
        if focus {
            // Belt-and-suspenders: when SwiftUI keeps the same NSView
            // attached across a rebuild (no viewDidMove… fires), fall back
            // to the original async makeFirstResponder. A latch re-arm also
            // covers the case where the view leaves and rejoins the window
            // without this host being recreated.
            if let nice = nsView as? NiceTerminalView {
                nice.wantsFocusOnAttach = true
            }
            DispatchQueue.main.async {
                nsView.window?.makeFirstResponder(nsView)
            }
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    private func findScroller(in view: NSView) -> NSScroller? {
        for subview in view.subviews {
            if let scroller = subview as? NSScroller {
                return scroller
            }
        }
        return nil
    }

    final class Coordinator {
        private var timer: Timer?

        func startObserving(_ scroller: NSScroller) {
            timer = Timer.scheduledTimer(withTimeInterval: 0.25, repeats: true) { [weak scroller] _ in
                guard let scroller else { return }
                scroller.isHidden = !scroller.isEnabled
            }
        }

        deinit {
            timer?.invalidate()
        }
    }
}
