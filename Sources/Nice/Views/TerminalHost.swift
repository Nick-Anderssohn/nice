//
//  TerminalHost.swift
//  Nice
//
//  Phase 4: SwiftUI wrapper around a stable `LocalProcessTerminalView`
//  instance owned by the containing session. `updateNSView` only
//  re-asserts focus on the hosted terminal — it never rebuilds the view.
//  The session owns the view, so the pty and its scrollback survive
//  SwiftUI redraws and tab switches.
//
//  The terminal view is not hosted directly: it sits inside a
//  `TerminalContainerView` that sizes it to a whole number of rows and
//  pins it to the bottom. SwiftTerm computes `rows = Int(height /
//  cellHeight)` and renders the grid top-anchored, so the leftover
//  sub-row pixels (0…cellHeight-1) would otherwise pile up below the
//  prompt and visibly wander as the window is resized. Bottom-anchoring
//  the row-quantized terminal frame keeps that remainder at the *top*,
//  under the toolbar/chrome where it's invisible, so the gap below the
//  prompt stays constant during a resize.
//

import SwiftTerm
import SwiftUI

/// Hosts the terminal view and bottom-anchors it to a whole-row height.
///
/// The container fills the area SwiftUI hands it; on every layout pass it
/// sizes its single terminal subview to `floor(height / cellHeight) *
/// cellHeight` and pins it to the bottom edge (AppKit's non-flipped
/// origin is bottom-left, so `y = bottomInset`). The sub-row remainder
/// therefore sits above the grid, under the window chrome.
final class TerminalContainerView: NSView {
    let terminal: LocalProcessTerminalView

    /// Constant gap kept below the last row, in points. 0 keeps the grid
    /// flush with the content area's bottom (matching the layout's
    /// "no bottom padding" intent); bump if a breathing-room inset reads
    /// better.
    var bottomInset: CGFloat = 0

    init(terminal: LocalProcessTerminalView) {
        self.terminal = terminal
        super.init(frame: .zero)
        terminal.autoresizingMask = []
        addSubview(terminal)
    }

    required init?(coder: NSCoder) {
        fatalError("TerminalContainerView is created in code only")
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        // Plain (non-AutoLayout) views aren't relaid out on a frame
        // change unless asked, so flag it — `layout()` re-derives the
        // bottom-anchored terminal frame from the new bounds.
        needsLayout = true
    }

    override func layout() {
        super.layout()
        let available = bounds.height - bottomInset
        let cell = terminal.cellHeight
        // Quantize to whole rows so SwiftTerm's `Int(height / cellHeight)`
        // leaves no sub-row remainder inside the terminal's own frame.
        // If the cell height isn't known yet, or the window is shorter
        // than a single row, fall back to the full height so the terminal
        // still gets a real non-zero frame — that's what fires
        // `NiceTerminalView`'s deferred shell spawn.
        if cell > 0 && available >= cell {
            let rowsHeight = (available / cell).rounded(.down) * cell
            terminal.frame = NSRect(
                x: 0, y: bottomInset, width: bounds.width, height: rowsHeight)
        } else {
            terminal.frame = bounds
        }
    }
}

struct TerminalHost: NSViewRepresentable {
    let view: LocalProcessTerminalView
    var focus: Bool = false

    func makeNSView(context: Context) -> TerminalContainerView {
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
        return TerminalContainerView(terminal: view)
    }

    func updateNSView(_ nsView: TerminalContainerView, context: Context) {
        if focus {
            // Belt-and-suspenders: when SwiftUI keeps the same NSView
            // attached across a rebuild (no viewDidMove… fires), fall back
            // to the original async makeFirstResponder. A latch re-arm also
            // covers the case where the view leaves and rejoins the window
            // without this host being recreated.
            if let nice = nsView.terminal as? NiceTerminalView {
                nice.wantsFocusOnAttach = true
            }
            DispatchQueue.main.async {
                nsView.terminal.window?.makeFirstResponder(nsView.terminal)
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
