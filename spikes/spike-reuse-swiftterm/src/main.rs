//! SPIKE: Reuse the proven SwiftTerm/Metal terminal NSView under a non-Swift (Rust) chrome.
//!
//! This binary uses the `objc2` crates to:
//!   1. Boot a real NSApplication (Regular activation policy) from Rust.
//!   2. Create an NSWindow + content view entirely from Rust.
//!   3. Embed a SYSTEM AppKit view (NSVisualEffectView) as a subview — the
//!      "we don't own the class, we just host it" case, exactly like hosting
//!      an MTKView / SwiftTerm TerminalView produced elsewhere.
//!   4. Define a CUSTOM NSView subclass IN RUST (`SpikeTermView`) that stands
//!      in for SwiftTerm's MTKView: it overrides drawRect:, mouseDown:,
//!      keyDown:, and acceptsFirstResponder — i.e. the real first-responder /
//!      event plumbing a terminal needs.
//!   5. Exercise resize propagation (autoresizing masks) and event delivery
//!      WITHOUT a window server, so it can run headless in CI: we resize the
//!      window and read back subview frames, make the term view first
//!      responder, and dispatch a synthesized key/mouse event straight at the
//!      Rust overrides to prove the method registration is live.
//!
//! Run headless (default): builds the hierarchy, runs assertions, prints a
//! report, exits 0. Set SPIKE_RUN=1 to instead enter a real `app.run()` loop
//! and show the window (for eyeballing on a machine with a display).

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSAutoresizingMaskOptions, NSBackingStoreType,
    NSColor, NSEvent, NSEventModifierFlags, NSEventType, NSRectFill, NSView, NSVisualEffectView,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};

/// Instance variables for our stand-in terminal view. Plain interior-mutable
/// counters; the view is main-thread-only so no atomics needed.
struct TermIvars {
    mouse_downs: Cell<u32>,
    key_downs: Cell<u32>,
    draws: Cell<u32>,
}

define_class!(
    // SAFETY:
    // - Superclass NSView has no extra subclassing requirements beyond being
    //   used on the main thread (enforced by MainThreadOnly below).
    // - This class does not implement Drop (the ivars' Drop is generated).
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "SpikeTermView"]
    #[ivars = TermIvars]
    struct SpikeTermView;

    impl SpikeTermView {
        /// A terminal view must be able to take key focus.
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            println!("[rust override] becomeFirstResponder -> true");
            true
        }

        /// Stand-in for the Metal draw: fill our bounds with a solid color.
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            let n = self.ivars().draws.get() + 1;
            self.ivars().draws.set(n);
            unsafe {
                let c = NSColor::colorWithSRGBRed_green_blue_alpha(0.10, 0.55, 0.30, 1.0);
                c.set();
                NSRectFill(dirty);
            }
            println!("[rust override] drawRect: fired (count={n})");
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let n = self.ivars().mouse_downs.get() + 1;
            self.ivars().mouse_downs.set(n);
            let p: NSPoint = unsafe { event.locationInWindow() };
            println!(
                "[rust override] mouseDown: at ({:.0},{:.0}) (count={n})",
                p.x, p.y
            );
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let n = self.ivars().key_downs.get() + 1;
            self.ivars().key_downs.set(n);
            let chars = unsafe { event.characters() }
                .map(|s| s.to_string())
                .unwrap_or_default();
            println!("[rust override] keyDown: chars={chars:?} (count={n})");
        }
    }
);

impl SpikeTermView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = mtm.alloc::<SpikeTermView>().set_ivars(TermIvars {
            mouse_downs: Cell::new(0),
            key_downs: Cell::new(0),
            draws: Cell::new(0),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        unsafe { this.setWantsLayer(true) };
        this
    }
}

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    // ---- Build window + content view from Rust ----
    let content_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(900.0, 600.0));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Resizable
        | NSWindowStyleMask::Miniaturizable;
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
    window.setTitle(&NSString::from_str("Nice spike: SwiftTerm-NSView under Rust chrome"));

    let content: Retained<NSView> = window.contentView().expect("content view");
    let bounds = content.frame();

    // ---- (3) Host a SYSTEM AppKit view we did NOT subclass ----
    let fx = unsafe { NSVisualEffectView::initWithFrame(mtm.alloc(), bounds) };
    unsafe {
        fx.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        content.addSubview(&fx);
    }

    // ---- (4) Host our Rust-defined custom NSView on top ----
    let term = SpikeTermView::new(mtm, bounds);
    unsafe {
        term.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        content.addSubview(&term);
    }

    println!("== hierarchy built ==");
    println!("content.frame   = {:?}", content.frame());
    println!("fx.frame        = {:?}", fx.frame());
    println!("term.frame      = {:?}", term.frame());

    // ---- (5a) First-responder plumbing ----
    let became = window.makeFirstResponder(Some(&term));
    let fr = window.firstResponder();
    let fr_is_term = fr
        .as_ref()
        .map(|r| {
            let r_ptr: *const AnyObject = (&**r as *const _) as *const AnyObject;
            let t_ptr: *const AnyObject = (&*term as *const _) as *const AnyObject;
            std::ptr::eq(r_ptr, t_ptr)
        })
        .unwrap_or(false);
    println!("makeFirstResponder -> {became}; firstResponder==term -> {fr_is_term}");

    // ---- (5b) Resize propagation via autoresizing (no window server needed) ----
    window.setContentSize(NSSize::new(1280.0, 800.0));
    let term_after = term.frame();
    let fx_after = fx.frame();
    println!("after resize -> term.frame = {term_after:?}");
    println!("after resize -> fx.frame   = {fx_after:?}");
    let resize_ok = (term_after.size.width - 1280.0).abs() < 0.5
        && (term_after.size.height - 800.0).abs() < 0.5
        && (fx_after.size.width - 1280.0).abs() < 0.5;

    // ---- (5c) Event delivery straight at the Rust overrides ----
    // Synthesize a keyDown and a mouseDown and send them to the view; this
    // proves the Objective-C method registration done by define_class! is live
    // and that the responder receives terminal-style input.
    let win_num: isize = window.windowNumber();
    let key_event = unsafe {
        NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            NSEventType::KeyDown,
            NSPoint::new(10.0, 10.0),
            NSEventModifierFlags::empty(),
            0.0,
            win_num,
            None,
            &NSString::from_str("a"),
            &NSString::from_str("a"),
            false,
            0,
        )
    }
    .expect("synth key event");
    let _: () = unsafe { msg_send![&*term, keyDown: &*key_event] };

    let mouse_event = unsafe {
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            NSEventType::LeftMouseDown,
            NSPoint::new(120.0, 80.0),
            NSEventModifierFlags::empty(),
            0.0,
            win_num,
            None,
            0,
            1,
            1.0,
        )
    }
    .expect("synth mouse event");
    let _: () = unsafe { msg_send![&*term, mouseDown: &*mouse_event] };

    // Force a draw pass (drawRect:) without a display server.
    unsafe { term.display() };

    let iv = term.ivars();
    println!("== event counters ==");
    println!("key_downs={} mouse_downs={} draws={}", iv.key_downs.get(), iv.mouse_downs.get(), iv.draws.get());

    // ---- Verdict ----
    let ok = became
        && fr_is_term
        && resize_ok
        && iv.key_downs.get() >= 1
        && iv.mouse_downs.get() >= 1;
    println!("\n== SPIKE RESULT ==");
    println!("embed_system_view(NSVisualEffectView): OK");
    println!("embed_rust_custom_nsview:              OK");
    println!("first_responder_plumbing:              {}", if became && fr_is_term { "OK" } else { "FAIL" });
    println!("resize_propagation(autoresizing):      {}", if resize_ok { "OK" } else { "FAIL" });
    println!("event_overrides_fire:                  {}", if iv.key_downs.get() >= 1 && iv.mouse_downs.get() >= 1 { "OK" } else { "FAIL" });
    println!("OVERALL: {}", if ok { "PASS" } else { "FAIL" });

    if std::env::var("SPIKE_RUN").is_ok() {
        println!("(SPIKE_RUN set) entering app.run() — Ctrl-C to quit");
        window.center();
        window.makeKeyAndOrderFront(None);
        app.activate();
        app.run();
    }

    std::process::exit(if ok { 0 } else { 1 });
}
