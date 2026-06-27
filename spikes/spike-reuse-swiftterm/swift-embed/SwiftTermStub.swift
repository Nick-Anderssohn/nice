// Stand-in for SwiftTerm's TerminalView: a Swift NSView subclass with custom
// drawing + first-responder behavior, exported to non-Swift callers via a C
// ABI. This is exactly the boundary you'd add to reuse the real SwiftTerm
// TerminalView (a Swift class) under a Rust chrome: Swift builds the NSView,
// hands back an opaque NSView*, and exposes a few C entry points to drive it.

import AppKit

final class StubTerminalView: NSView {
    // Mimic a terminal's scrollback: lines we "rendered".
    private var lines: [String] = ["StubTerminalView ready (Swift-built NSView)"]

    override var acceptsFirstResponder: Bool { true }

    override func becomeFirstResponder() -> Bool {
        NSLog("[swift] StubTerminalView becameFirstResponder")
        return true
    }

    override func draw(_ dirtyRect: NSRect) {
        // Stand-in for the Metal draw pass.
        NSColor(srgbRed: 0.06, green: 0.07, blue: 0.10, alpha: 1.0).set()
        dirtyRect.fill()
        let attrs: [NSAttributedString.Key: Any] = [
            .foregroundColor: NSColor(srgbRed: 0.6, green: 0.9, blue: 0.6, alpha: 1.0),
            .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular),
        ]
        var y = bounds.height - 20
        for line in lines.suffix(40) {
            (line as NSString).draw(at: NSPoint(x: 8, y: y), withAttributes: attrs)
            y -= 16
        }
        NSLog("[swift] StubTerminalView.draw fired, \(lines.count) lines")
    }

    override func keyDown(with event: NSEvent) {
        NSLog("[swift] StubTerminalView.keyDown chars=\(event.characters ?? "")")
        if let s = event.characters { lines.append("input: \(s)") }
        needsDisplay = true
    }

    func feed(_ text: String) {
        lines.append(text)
        needsDisplay = true
    }
}

// MARK: - C ABI surface

/// Build a Swift NSView and hand back an owned (retained +1) opaque pointer.
/// Caller is responsible for releasing it (or letting AppKit own it once it is
/// added as a subview).
@_cdecl("spike_make_terminal_view")
public func spike_make_terminal_view(_ w: Double, _ h: Double) -> UnsafeMutableRawPointer {
    let v = StubTerminalView(frame: NSRect(x: 0, y: 0, width: w, height: h))
    v.wantsLayer = true
    // Return +1 retained; Rust will hand ownership to AppKit via addSubview and
    // then balance with a release, OR keep it. We pass retained so the object
    // survives crossing the FFI boundary regardless of Swift ARC.
    return Unmanaged.passRetained(v).toOpaque()
}

/// Drive the Swift view from the non-Swift host (mimics SwiftTerm.feed).
@_cdecl("spike_terminal_feed")
public func spike_terminal_feed(_ view: UnsafeMutableRawPointer, _ cstr: UnsafePointer<CChar>) {
    let v = Unmanaged<StubTerminalView>.fromOpaque(view).takeUnretainedValue()
    v.feed(String(cString: cstr))
}
