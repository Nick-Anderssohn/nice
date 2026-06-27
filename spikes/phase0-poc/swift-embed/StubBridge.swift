// StubBridge.swift — HEADLESS-COMPILE FALLBACK for the Phase-0 PoC.
//
// Exposes the EXACT SAME C-ABI symbol set as swift-bridge/Sources/SwiftTermBridge/Bridge.swift
// (st_create, st_nsview, ..., nice_harness_set_present_cb, ...), but backed by a
// plain AppKit NSView instead of the real SwiftTerm Metal TerminalView. It has
// NO SwiftTerm dependency and NO Metal, so it builds with a single
//     swiftc -emit-library StubBridge.swift -o libswifttermbridge.dylib
// offline, with no display, and without the read-only fork. The Rust crate
// links whichever dylib build.rs produced (stub by default; the real bridge
// when NICE_POC_REAL_BRIDGE=1).
//
// What the stub CAN exercise headlessly: the full Rust↔Swift FFI surface, the
// reverse-FFI delegate callbacks (it echoes fed bytes back through onSend when
// loopback is on), the harness counters (st_present_now fires onDrawAttempt +
// onPresent), font/color/selection plumbing, and resize. What it CANNOT prove:
// the real Metal renderer, true GPU present timing, IME, or VT mouse — those
// need the real bridge + a key window (see ../README.md §Display-gated).

import AppKit
import Darwin

// MARK: - C-convention callback types (must match Bridge.swift + bridge.rs) --

public typealias STSendCB      = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<UInt8>?, Int) -> Void
public typealias STTitleCB     = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<CChar>?) -> Void
public typealias STSizeCB      = @convention(c) (UnsafeMutableRawPointer?, Int32, Int32) -> Void
public typealias STBellCB      = @convention(c) (UnsafeMutableRawPointer?) -> Void
public typealias STDirCB       = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<CChar>?) -> Void
public typealias STClipCopyCB  = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<UInt8>?, Int) -> Void

enum NiceHarness {
    static var onPresent: ((UInt64) -> Void)?
    static var onDrawAttempt: ((UInt64) -> Void)?
}

@_cdecl("nice_harness_set_present_cb")
public func nice_harness_set_present_cb(_ cb: @convention(c) (UInt64) -> Void) {
    NiceHarness.onPresent = cb
}
@_cdecl("nice_harness_set_draw_cb")
public func nice_harness_set_draw_cb(_ cb: @convention(c) (UInt64) -> Void) {
    NiceHarness.onDrawAttempt = cb
}

// MARK: - Stub view ----------------------------------------------------------

final class StubTerminalView: NSView {
    var lines: [String] = ["StubTerminalView ready (no SwiftTerm, no Metal)"]
    var feedBytes: UInt64 = 0
    // reverse-FFI sinks
    var userdata: UnsafeMutableRawPointer?
    var onSend: STSendCB?
    var onTitle: STTitleCB?
    var onSize: STSizeCB?
    var onBell: STBellCB?
    var onDir: STDirCB?
    var onClip: STClipCopyCB?
    var loopback = false
    var cols: Int32 = 80
    var rows: Int32 = 24

    override var acceptsFirstResponder: Bool { true }
    override func becomeFirstResponder() -> Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        NSColor(srgbRed: 0.06, green: 0.07, blue: 0.10, alpha: 1.0).set()
        dirtyRect.fill()
    }

    override func keyDown(with event: NSEvent) {
        // Mimic SwiftTerm: echo the typed bytes back out via the `send` delegate.
        if let s = event.characters, let cb = onSend {
            let bytes = Array(s.utf8)
            if loopback { feed(Array(bytes)) }
            bytes.withUnsafeBufferPointer { cb(userdata, $0.baseAddress, $0.count) }
        }
        needsDisplay = true
    }

    func feed(_ bytes: [UInt8]) {
        feedBytes &+= UInt64(bytes.count)
        needsDisplay = true
    }
}

final class STHandle {
    let view = StubTerminalView(frame: .zero)
}

@inline(__always) private func box(_ h: UnsafeMutableRawPointer) -> STHandle {
    Unmanaged<STHandle>.fromOpaque(h).takeUnretainedValue()
}

// MARK: - Lifecycle ----------------------------------------------------------

@_cdecl("st_create")
public func st_create(_ x: Double, _ y: Double, _ w: Double, _ h: Double) -> UnsafeMutableRawPointer {
    let handle = STHandle()
    handle.view.frame = NSRect(x: x, y: y, width: w, height: h)
    handle.view.wantsLayer = true
    return Unmanaged.passRetained(handle).toOpaque()
}

@_cdecl("st_nsview")
public func st_nsview(_ h: UnsafeMutableRawPointer) -> UnsafeMutableRawPointer {
    Unmanaged.passUnretained(box(h).view).toOpaque()
}

@_cdecl("st_destroy")
public func st_destroy(_ h: UnsafeMutableRawPointer) {
    box(h).view.removeFromSuperview()
    Unmanaged<STHandle>.fromOpaque(h).release()
}

/// Stub has no Metal: always returns 0 (Metal unavailable). This is the
/// honest "stub" signal the harness records as metal=unavailable.
@_cdecl("st_set_use_metal")
public func st_set_use_metal(_ h: UnsafeMutableRawPointer, _ enabled: Int32) -> Int32 { 0 }

@_cdecl("st_is_using_metal")
public func st_is_using_metal(_ h: UnsafeMutableRawPointer) -> Int32 { 0 }

@_cdecl("st_set_loopback")
public func st_set_loopback(_ h: UnsafeMutableRawPointer, _ enabled: Int32) {
    box(h).view.loopback = (enabled != 0)
}

// MARK: - Feed / resize / present -------------------------------------------

@_cdecl("st_feed_bytes")
public func st_feed_bytes(_ h: UnsafeMutableRawPointer, _ ptr: UnsafePointer<UInt8>, _ len: Int) {
    NiceHarness.onDrawAttempt?(mach_absolute_time())
    box(h).view.feed(Array(UnsafeBufferPointer(start: ptr, count: len)))
}

@_cdecl("st_resize")
public func st_resize(_ h: UnsafeMutableRawPointer, _ cols: Int32, _ rows: Int32) {
    let v = box(h).view
    v.cols = cols; v.rows = rows
    v.onSize?(v.userdata, cols, rows)
}

@_cdecl("st_set_frame")
public func st_set_frame(_ h: UnsafeMutableRawPointer, _ x: Double, _ y: Double, _ w: Double, _ d: Double) {
    box(h).view.frame = NSRect(x: x, y: y, width: w, height: d)
}

/// Stub: there is no Metal drawable, so we just stamp the harness counters and
/// invalidate. Returns 1 so the FPS-counter wiring is exercised headlessly.
@_cdecl("st_present_now")
public func st_present_now(_ h: UnsafeMutableRawPointer) -> Int32 {
    NiceHarness.onDrawAttempt?(mach_absolute_time())
    box(h).view.needsDisplay = true
    NiceHarness.onPresent?(mach_absolute_time())
    return 1
}

/// Stub has no Metal present loop: report 0 so the Rust driver keeps its
/// synchronous per-frame st_present_now() fallback (preserving prior behavior).
@_cdecl("st_start_present_link")
public func st_start_present_link(_ h: UnsafeMutableRawPointer) -> Int32 { 0 }

@_cdecl("st_stop_present_link")
public func st_stop_present_link(_ h: UnsafeMutableRawPointer) {}

/// Stub: fire the harness hooks synchronously (no real Metal to defer).
@_cdecl("st_present_async")
public func st_present_async(_ h: UnsafeMutableRawPointer) {
    NiceHarness.onDrawAttempt?(mach_absolute_time())
    box(h).view.needsDisplay = true
    NiceHarness.onPresent?(mach_absolute_time())
}

/// Stub: no CAMetalLayer to configure — report -1 (no MTKView).
@_cdecl("st_set_display_sync")
public func st_set_display_sync(_ h: UnsafeMutableRawPointer, _ enabled: Int32) -> Int32 { -1 }

/// Stub: no Metal renderer to opt into transactional present — report 0.
@_cdecl("st_set_presents_with_transaction")
public func st_set_presents_with_transaction(_ h: UnsafeMutableRawPointer, _ enabled: Int32) -> Int32 { 0 }

// MARK: - Font / colors / selection -----------------------------------------

@_cdecl("st_set_font")
public func st_set_font(_ h: UnsafeMutableRawPointer, _ name: UnsafePointer<CChar>?, _ size: Double) {}

@_cdecl("st_set_colors")
public func st_set_colors(_ h: UnsafeMutableRawPointer, _ fg: UInt32, _ bg: UInt32,
                          _ palette16: UnsafePointer<UInt32>?) {}

@_cdecl("st_get_selection")
public func st_get_selection(_ h: UnsafeMutableRawPointer) -> UnsafeMutablePointer<CChar>? {
    return strdup("stub-selection")
}

@_cdecl("st_string_free")
public func st_string_free(_ p: UnsafeMutablePointer<CChar>?) { free(p) }

/// Stub has no real selection model — report 0 (no active range). The headless
/// link still resolves the symbol; the real range proof runs only on a display.
@_cdecl("st_selection_has_range")
public func st_selection_has_range(_ h: UnsafeMutableRawPointer) -> Int32 { 0 }

// MARK: - Callback registration ----------------------------------------------

@_cdecl("st_register_callbacks")
public func st_register_callbacks(_ h: UnsafeMutableRawPointer,
                                  _ userdata: UnsafeMutableRawPointer?,
                                  _ onSend: STSendCB?, _ onTitle: STTitleCB?,
                                  _ onSize: STSizeCB?, _ onBell: STBellCB?,
                                  _ onDir: STDirCB?, _ onClipCopy: STClipCopyCB?) {
    let v = box(h).view
    v.userdata = userdata
    v.onSend = onSend; v.onTitle = onTitle; v.onSize = onSize
    v.onBell = onBell; v.onDir = onDir; v.onClip = onClipCopy
}
