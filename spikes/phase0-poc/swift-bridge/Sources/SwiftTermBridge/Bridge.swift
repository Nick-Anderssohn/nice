// SwiftTermBridge/Bridge.swift
//
// @_cdecl C-ABI wrappers around the REAL SwiftTerm `MacTerminalView`
// (the Metal-backed terminal NSView) + a reverse-FFI delegate that fans
// SwiftTerm's callbacks out to registered C function pointers, + the
// measurement-harness present/draw hooks.
//
// Implements the "SwiftTerm Real-View API + C-ABI Bridge Spec" verbatim
// against the fork at /Users/nick/Projects/SwiftTerm
// @ 2f2a0b727feaa0d51659a9aaa21d47d752a16e0b.
//
// ALL `st_*` calls must run on the AppKit MAIN THREAD (the same thread GPUI's
// platform loop runs on). Handles cross the boundary as opaque pointers; the
// TerminalView (an NSView) is handed back so GPUI can addSubview: it.
//
// The matching Rust `extern "C"` block lives in ../../src/bridge.rs. The
// headless stub at ../../swift-embed/StubBridge.swift exposes the IDENTICAL
// symbol set with a plain NSView (no SwiftTerm, no Metal) so the Rust crate
// compiles and runs the harness logic without a display or the fork.

import AppKit
import MetalKit
import Darwin           // mach_absolute_time for the harness clock (same clock as Rust)
import SwiftTerm

// MARK: - Reverse-FFI callback function-pointer types (C convention) ---------

public typealias STSendCB      = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<UInt8>?, Int) -> Void
public typealias STTitleCB     = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<CChar>?) -> Void
public typealias STSizeCB      = @convention(c) (UnsafeMutableRawPointer?, Int32, Int32) -> Void
public typealias STBellCB      = @convention(c) (UnsafeMutableRawPointer?) -> Void
public typealias STDirCB       = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<CChar>?) -> Void
public typealias STClipCopyCB  = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<UInt8>?, Int) -> Void

// MARK: - Harness frame hooks ------------------------------------------------
//
// NOTE (load-bearing seam): the original harness spec patches the fork's
// `MetalTerminalRenderer.draw(in:)` to add a `commandBuffer.addCompletedHandler`
// that fires `onPresent` with a GPU-COMPLETE timestamp. The fork is READ-ONLY
// for this PoC, so we do NOT patch it. Instead:
//   * `onDrawAttempt` fires from `st_present_now`/`st_feed_bytes` BEFORE we ask
//     the MTKView to draw  — a CPU "frame requested" timestamp.
//   * `onPresent` fires from `st_present_now` AFTER `mtkView.draw()` returns —
//     a CPU "frame submitted/committed" timestamp (draw(in:) ran synchronously).
// Neither is the true GPU-complete time. For real GPU-complete present timing,
// run with env SWIFTTERM_PROFILE=1 and stream the fork's existing OSSignposter
// "Metal.Draw" interval (subsystem "org.tirania.SwiftTerm", category
// "MetalProfile"); see ../README.md §Harness and §Caveats. This is recorded as
// a TODO/seam, not faked.
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

// MARK: - Delegate (reverse-FFI) ---------------------------------------------

final class BridgeDelegate: NSObject, TerminalViewDelegate {
    var userdata: UnsafeMutableRawPointer?
    var onSend:  STSendCB?
    var onTitle: STTitleCB?
    var onSize:  STSizeCB?
    var onBell:  STBellCB?
    var onDir:   STDirCB?
    var onClip:  STClipCopyCB?

    // Optional local-echo loopback for the §C.3 seam-latency profile: when set,
    // bytes the terminal wants to write to the pty are immediately re-fed into
    // the view, so keyDown -> insertText -> send -> feed -> draw closes without
    // a real shell. Toggled via st_set_loopback.
    weak var loopbackView: TerminalView?

    func send(source: TerminalView, data: ArraySlice<UInt8>) {
        if let v = loopbackView {
            v.feed(byteArray: data)
        }
        guard let onSend = onSend else { return }
        let arr = Array(data)
        arr.withUnsafeBufferPointer { onSend(userdata, $0.baseAddress, $0.count) }
    }
    func setTerminalTitle(source: TerminalView, title: String) {
        title.withCString { onTitle?(userdata, $0) }
    }
    func sizeChanged(source: TerminalView, newCols: Int, newRows: Int) {
        onSize?(userdata, Int32(newCols), Int32(newRows))
    }
    func hostCurrentDirectoryUpdate(source: TerminalView, directory: String?) {
        (directory ?? "").withCString { onDir?(userdata, $0) }
    }
    func scrolled(source: TerminalView, position: Double) {}
    func rangeChanged(source: TerminalView, startY: Int, endY: Int) {}
    func bell(source: TerminalView) { onBell?(userdata) }
    func clipboardCopy(source: TerminalView, content: Data) {
        guard let onClip = onClip else { return }
        content.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
            onClip(userdata, raw.bindMemory(to: UInt8.self).baseAddress, content.count)
        }
    }
}

// Box that keeps the view + delegate alive together.
final class STHandle {
    let view: TerminalView
    let delegate = BridgeDelegate()
    init(view: TerminalView) { self.view = view }
}

@inline(__always) private func box(_ h: UnsafeMutableRawPointer) -> STHandle {
    Unmanaged<STHandle>.fromOpaque(h).takeUnretainedValue()
}

// MARK: - Lifecycle ----------------------------------------------------------

@_cdecl("st_create")
public func st_create(_ x: Double, _ y: Double, _ w: Double, _ h: Double) -> UnsafeMutableRawPointer {
    let v = TerminalView(frame: CGRect(x: x, y: y, width: w, height: h), font: nil)
    let handle = STHandle(view: v)
    v.terminalDelegate = handle.delegate
    return Unmanaged.passRetained(handle).toOpaque()
}

@_cdecl("st_nsview")
public func st_nsview(_ h: UnsafeMutableRawPointer) -> UnsafeMutableRawPointer {
    Unmanaged.passUnretained(box(h).view).toOpaque()
}

@_cdecl("st_destroy")
public func st_destroy(_ h: UnsafeMutableRawPointer) {
    let handle = box(h)
    handle.view.removeFromSuperview()
    Unmanaged<STHandle>.fromOpaque(h).release()
}

/// 1 on success, 0 on Metal failure (deviceUnavailable / pipeline / missing
/// SwiftTerm_SwiftTerm.bundle next to the dylib).
@_cdecl("st_set_use_metal")
public func st_set_use_metal(_ h: UnsafeMutableRawPointer, _ enabled: Int32) -> Int32 {
    do { try box(h).view.setUseMetal(enabled != 0); return 1 } catch { return 0 }
}

@_cdecl("st_is_using_metal")
public func st_is_using_metal(_ h: UnsafeMutableRawPointer) -> Int32 {
    box(h).view.isUsingMetalRenderer ? 1 : 0
}

/// Enable/disable local-echo loopback (seam-latency profile, §C.3).
@_cdecl("st_set_loopback")
public func st_set_loopback(_ h: UnsafeMutableRawPointer, _ enabled: Int32) {
    let handle = box(h)
    handle.delegate.loopbackView = (enabled != 0) ? handle.view : nil
}

// MARK: - Feed / resize / present -------------------------------------------

@_cdecl("st_feed_bytes")
public func st_feed_bytes(_ h: UnsafeMutableRawPointer, _ ptr: UnsafePointer<UInt8>, _ len: Int) {
    NiceHarness.onDrawAttempt?(mach_absolute_time())
    let buf = UnsafeBufferPointer(start: ptr, count: len)
    box(h).view.feed(byteArray: ArraySlice(buf))
}

@_cdecl("st_resize")
public func st_resize(_ h: UnsafeMutableRawPointer, _ cols: Int32, _ rows: Int32) {
    box(h).view.resize(cols: Int(cols), rows: Int(rows))
}

@_cdecl("st_set_frame")
public func st_set_frame(_ h: UnsafeMutableRawPointer, _ x: Double, _ y: Double, _ w: Double, _ d: Double) {
    box(h).view.frame = CGRect(x: x, y: y, width: w, height: d)
}

/// Force ONE synchronous Metal frame; returns 1 if a frame was issued.
/// Mirrors the fork's requestMetalDisplay() == metalView.draw().
@_cdecl("st_present_now")
public func st_present_now(_ h: UnsafeMutableRawPointer) -> Int32 {
    guard let mtk = box(h).view.subviews.compactMap({ $0 as? MTKView }).first else { return 0 }
    NiceHarness.onDrawAttempt?(mach_absolute_time())
    mtk.draw()                                   // public, synchronous -> draw(in:)
    NiceHarness.onPresent?(mach_absolute_time()) // CPU frame-submitted (see NiceHarness note)
    return 1
}

// MARK: - Font / colors ------------------------------------------------------

@_cdecl("st_set_font")
public func st_set_font(_ h: UnsafeMutableRawPointer, _ name: UnsafePointer<CChar>?, _ size: Double) {
    let v = box(h).view
    if let name = name, let s = String(validatingUTF8: name), let f = NSFont(name: s, size: CGFloat(size)) {
        v.font = f
    } else {
        v.font = NSFont.monospacedSystemFont(ofSize: CGFloat(size), weight: .regular)
    }
}

/// fg/bg are 0x00RRGGBB; palette16 is 16 * 0x00RRGGBB (ANSI 0..15) or NULL.
@_cdecl("st_set_colors")
public func st_set_colors(_ h: UnsafeMutableRawPointer, _ fg: UInt32, _ bg: UInt32,
                          _ palette16: UnsafePointer<UInt32>?) {
    let v = box(h).view
    func ns(_ c: UInt32) -> NSColor {
        NSColor(srgbRed: CGFloat((c >> 16) & 0xff)/255, green: CGFloat((c >> 8) & 0xff)/255,
                blue: CGFloat(c & 0xff)/255, alpha: 1)
    }
    func col(_ c: UInt32) -> SwiftTerm.Color {   // 8-bit -> 0..65535
        SwiftTerm.Color(red: UInt16((c >> 16) & 0xff) * 257,
                        green: UInt16((c >> 8) & 0xff) * 257,
                        blue: UInt16(c & 0xff) * 257)
    }
    if let p = palette16 {
        v.installColors((0..<16).map { col(p[$0]) })
    }
    v.nativeForegroundColor = ns(fg)
    v.nativeBackgroundColor = ns(bg)
}

// MARK: - Selection ----------------------------------------------------------

/// malloc'd UTF-8 C string (free via st_string_free) or NULL.
@_cdecl("st_get_selection")
public func st_get_selection(_ h: UnsafeMutableRawPointer) -> UnsafeMutablePointer<CChar>? {
    guard let s = box(h).view.getSelection() else { return nil }
    return strdup(s)
}

@_cdecl("st_string_free")
public func st_string_free(_ p: UnsafeMutablePointer<CChar>?) { free(p) }

// MARK: - Callback registration (reverse-FFI) --------------------------------

@_cdecl("st_register_callbacks")
public func st_register_callbacks(_ h: UnsafeMutableRawPointer,
                                  _ userdata: UnsafeMutableRawPointer?,
                                  _ onSend: STSendCB?, _ onTitle: STTitleCB?,
                                  _ onSize: STSizeCB?, _ onBell: STBellCB?,
                                  _ onDir: STDirCB?, _ onClipCopy: STClipCopyCB?) {
    let d = box(h).delegate
    d.userdata = userdata
    d.onSend = onSend; d.onTitle = onTitle; d.onSize = onSize
    d.onBell = onBell; d.onDir = onDir; d.onClip = onClipCopy
}
