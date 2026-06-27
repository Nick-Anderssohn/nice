//! SPIKE Phase B: embed and drive a *Swift-built* NSView (stand-in for
//! SwiftTerm's TerminalView) from a Rust/objc2 chrome, across a C ABI.
//!
//! The Swift side (`SwiftTermStub.swift`, compiled to libswifttermstub.dylib)
//! hands back an opaque, +1-retained `NSView*` via `spike_make_terminal_view`
//! and lets us drive it via `spike_terminal_feed`. This is precisely the
//! boundary you'd build to reuse the real SwiftTerm TerminalView under a
//! non-Swift chrome. Here we prove the Rust host can:
//!   - call the Swift factory and take ownership of the returned NSView,
//!   - add it as a subview of a Rust-created NSWindow's content view,
//!   - resize it via autoresizing,
//!   - make it first responder (its Swift becomeFirstResponder override fires),
//!   - feed it data and force a draw pass (its Swift draw override fires).

use std::ffi::{c_char, c_void, CString};

use objc2::rc::Retained;
use objc2::{msg_send, ClassType};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSAutoresizingMaskOptions, NSBackingStoreType,
    NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};

// C ABI exported by the Swift dylib.
extern "C" {
    fn spike_make_terminal_view(w: f64, h: f64) -> *mut c_void;
    fn spike_terminal_feed(view: *mut c_void, cstr: *const c_char);
}

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    let content_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(900.0, 600.0));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Resizable;
    let window: Retained<NSWindow> = unsafe {
        let w = mtm.alloc::<NSWindow>();
        msg_send![
            w,
            initWithContentRect: content_rect,
            styleMask: style,
            backing: NSBackingStoreType::Buffered,
            defer: false,
        ]
    };
    window.setTitle(&NSString::from_str("Swift TerminalView under Rust chrome"));
    let content: Retained<NSView> = window.contentView().expect("content view");
    let bounds = content.frame();

    // ---- Call into Swift to build the NSView ----
    let raw = unsafe { spike_make_terminal_view(bounds.size.width, bounds.size.height) };
    assert!(!raw.is_null(), "Swift factory returned null");
    // The Swift side returned a +1-retained NSView; take ownership in Rust.
    let term: Retained<NSView> =
        unsafe { Retained::from_raw(raw.cast::<NSView>()) }.expect("non-null NSView");

    // Confirm we really got an NSView subclass back across the boundary.
    let is_view: bool = unsafe { msg_send![&*term, isKindOfClass: NSView::class()] };
    let cls_name = term.class().name();
    println!("Swift returned class = {cls_name:?}, isKindOfClass(NSView) = {is_view}");

    // ---- Embed + autoresize ----
    unsafe {
        term.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        content.addSubview(&term);
    }
    println!("embedded Swift view, frame = {:?}", term.frame());

    // ---- First responder: triggers the Swift becomeFirstResponder override ----
    let became = window.makeFirstResponder(Some(&term));
    println!("makeFirstResponder(swiftView) -> {became}");

    // ---- Resize propagation ----
    window.setContentSize(NSSize::new(1280.0, 800.0));
    let after = term.frame();
    let resize_ok =
        (after.size.width - 1280.0).abs() < 0.5 && (after.size.height - 800.0).abs() < 0.5;
    println!("after resize -> swiftView.frame = {after:?} (ok={resize_ok})");

    // ---- Drive the Swift view from Rust (mimics SwiftTerm.feed) ----
    let msg = CString::new("hello from the Rust chrome").unwrap();
    unsafe { spike_terminal_feed(raw, msg.as_ptr()) };
    // Force the Swift draw(_:) override to run without a window server.
    unsafe { term.display() };

    let ok = is_view && became && resize_ok;
    println!("\n== PHASE B RESULT ==");
    println!("swift_factory_returned_nsview: {}", if is_view { "OK" } else { "FAIL" });
    println!("embed_swift_view_in_rust:      OK");
    println!("first_responder:               {}", if became { "OK" } else { "FAIL" });
    println!("resize_propagation:            {}", if resize_ok { "OK" } else { "FAIL" });
    println!("drive_swift_view(feed+draw):   OK (see [swift] log lines above)");
    println!("OVERALL: {}", if ok { "PASS" } else { "FAIL" });

    if std::env::var("SPIKE_RUN").is_ok() {
        window.center();
        window.makeKeyAndOrderFront(None);
        app.activate();
        app.run();
    }
    std::process::exit(if ok { 0 } else { 1 });
}
