//! Live responder-chain input routing (RECON 2 §3) — the part the earlier stub
//! spike could NOT prove because it invoked overridden selectors directly.
//!
//! Everything here synthesizes REAL `NSEvent`s and dispatches them through
//! `NSApplication::sendEvent`, so they traverse windowserver -> key window ->
//! first responder, hitting SwiftTerm's real `keyDown:`/`flagsChanged:`/
//! `NSTextInputClient` path. Requires the window to be the genuine key window
//! (`makeKeyAndOrderFront` + `app.activate()`), which needs a DISPLAY — so the
//! actual injection is DISPLAY-GATED behind the runtime flag in `main.rs`.
//!
//! PASS/FAIL is reported SEPARATELY for keyboard/IME vs mouse, because:
//!   * keyboard + IME routing is EXPECTED TO PASS (gpui's GPUIWindow does not
//!     override `sendEvent:`, so first-responder routing is intact).
//!   * the MOUSE half is the GENUINE UNKNOWN (RECON 2 §5): with the terminal
//!     BELOW GPUIView, default hit-testing gives every mouse hit to GPUIView, so
//!     VT mouse / selection never reaches the terminal through normal routing.
//!     See `MOUSE_SEAM` below.

use std::ffi::c_void;
use std::ptr::{null_mut, NonNull};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Method, Sel};
use objc2::sel;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventMask, NSEventModifierFlags,
    NSEventType, NSView, NSWindow,
};
use objc2_foundation::{NSPoint, NSString};

/// Human-readable description of the load-bearing mouse seam, surfaced in the
/// harness report so the decision tree is not misread.
pub const MOUSE_SEAM: &str = "\
MOUSE ROUTING IS THE LOAD-BEARING UNKNOWN (RECON 2 §5):
  terminal-below-GPUIView (needed for transparent-over-terminal) => GPUIView
  wins every hit-test => terminal gets NO mouse events. Resolutions:
   (1) override/swizzle GPUIView.hitTest: so the terminal rect returns the
       terminal — NOW IMPLEMENTED in `install_hittest_shim` (objc2 runtime
       class_addMethod on the GPUIView class; the live driver measures it);
   (2) terminal-on-top-within-its-rect (embed_above_in_rect) — mouse+keyboard
       route naturally but gpui can no longer composite OVER the terminal
       (this is structurally the objc2-hybrid fallback).
  The live driver wires (1) and reports PASS/FAIL/UNPROVEN from real evidence.";

/// Build a synthetic key-down event bound to a window number. Mirrors the
/// constructor used in spike-reuse-swiftterm/src/main.rs.
pub fn synth_key(
    ty: NSEventType,
    window_number: isize,
    chars: &str,
    chars_ignoring_mods: &str,
    is_repeat: bool,
    key_code: u16,
    mods: NSEventModifierFlags,
) -> Option<Retained<NSEvent>> {
    // Safe in objc2-app-kit 0.3: the key-event constructor takes no raw pointers.
    NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        ty,
        NSPoint::new(0.0, 0.0),
        mods,
        0.0,
        window_number,
        None,
        &NSString::from_str(chars),
        &NSString::from_str(chars_ignoring_mods),
        is_repeat,
        key_code,
    )
}

/// Convenience for a plain key-down of a single character.
pub fn key_down(window_number: isize, ch: &str, key_code: u16) -> Option<Retained<NSEvent>> {
    synth_key(
        NSEventType::KeyDown,
        window_number,
        ch,
        ch,
        false,
        key_code,
        NSEventModifierFlags::empty(),
    )
}

/// Convenience for the matching key-up.
pub fn key_up(window_number: isize, ch: &str, key_code: u16) -> Option<Retained<NSEvent>> {
    synth_key(
        NSEventType::KeyUp,
        window_number,
        ch,
        ch,
        false,
        key_code,
        NSEventModifierFlags::empty(),
    )
}

/// A `flagsChanged` event (modifier press/release), e.g. Command down.
pub fn flags_changed(window_number: isize, mods: NSEventModifierFlags, key_code: u16) -> Option<Retained<NSEvent>> {
    synth_key(
        NSEventType::FlagsChanged,
        window_number,
        "",
        "",
        false,
        key_code,
        mods,
    )
}

/// Build a synthetic mouse event (VT mouse / selection driver). See `MOUSE_SEAM`
/// — delivery to the terminal through normal routing is the open unknown.
pub fn synth_mouse(
    ty: NSEventType,
    location_in_window: NSPoint,
    window_number: isize,
    click_count: isize,
) -> Option<Retained<NSEvent>> {
    // Safe in objc2-app-kit 0.3: the mouse-event constructor takes no raw pointers.
    NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        ty,
        location_in_window,
        NSEventModifierFlags::empty(),
        0.0,
        window_number,
        None,
        0,
        click_count,
        1.0,
    )
}

/// Dispatch an event through the REAL responder chain (window -> firstResponder
/// -> NSTextInputClient). This is the whole point vs a direct selector call.
pub fn send_event(app: &NSApplication, event: &NSEvent) {
    app.sendEvent(event);
}

/// PoC item 7: install a process-wide local NSEvent monitor that can SWALLOW
/// (return null) or PASS THROUGH (return the event) key events, matched by
/// LAYOUT-INDEPENDENT `keyCode`. Proves a rebindable-shortcut monitor coexists
/// with gpui focus (gpui installs no competing swallowing monitor).
///
/// Returns the opaque monitor token (`removeMonitor:` it to uninstall). The
/// `swallow_key_code` is swallowed; everything else passes through.
///
/// # Safety
/// Installs a global event tap callback; must run on the main thread.
pub unsafe fn install_swallow_monitor(swallow_key_code: u16) -> Option<Retained<AnyObject>> {
    let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        let ev = event.as_ref();
        let code = ev.keyCode();
        if code == swallow_key_code {
            // Swallow: the event does not continue to gpui or the terminal.
            std::ptr::null_mut()
        } else {
            // Pass through unchanged.
            event.as_ptr()
        }
    });
    NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler)
}

/// Remove a monitor token from `install_swallow_monitor`.
///
/// # Safety
/// `token` must be a monitor returned by `addLocalMonitorForEventsMatchingMask`.
pub unsafe fn remove_monitor(token: &AnyObject) {
    NSEvent::removeMonitor(token);
}

// ===========================================================================
// THE MOUSE HIT-TEST SEAM (RECON 2 §5 option 1) — the load-bearing test that
// decides Path A vs objc2-hybrid.
//
// With the terminal embedded BELOW `GPUIView` (so gpui can composite its
// transparent chrome OVER it), default AppKit hit-testing gives EVERY mouse hit
// to `GPUIView` and the terminal sibling never sees VT mouse / selection. We fix
// that by adding a `hitTest:` override to the runtime-declared `GPUIView` class
// (verified: GPUIView does NOT override hitTest:, so we `class_addMethod` an
// override rather than `method_setImplementation`, which would clobber every
// NSView in the process). The override returns the terminal NSView for points
// inside the terminal frame and OUTSIDE the top chrome bar; everything else
// falls through to the original NSView behaviour (gpui chrome stays clickable).
// ===========================================================================

/// The embedded terminal `NSView*` the swizzled `hitTest:` should route to.
static SWZ_TERM: AtomicPtr<NSView> = AtomicPtr::new(null_mut());
/// Height (points) of the top chrome bar that must keep hitting gpui (f64 bits).
static SWZ_CHROME_H: AtomicU64 = AtomicU64::new(0);
/// The original `hitTest:` IMP (NSView's), called for chrome / out-of-bounds.
static SWZ_ORIG: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static SWZ_INSTALLED: AtomicBool = AtomicBool::new(false);
/// Diagnostics: how many hits the override routed to the terminal vs. fell
/// through to gpui. Lets the live driver print concrete evidence.
static SWZ_HITS_TERM: AtomicU64 = AtomicU64::new(0);
static SWZ_HITS_CHROME: AtomicU64 = AtomicU64::new(0);

/// Concrete signature of `-[NSView hitTest:]` for transmuting the IMP.
type HitTestFn = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, NSPoint) -> *mut AnyObject;

/// Our replacement `hitTest:`. `point` arrives in the receiver's SUPERVIEW
/// (contentView) coordinates — the same space as the terminal sibling's frame.
unsafe extern "C-unwind" fn hittest_override(
    this: *mut AnyObject,
    cmd: Sel,
    point: NSPoint,
) -> *mut AnyObject {
    let term = SWZ_TERM.load(Ordering::SeqCst);
    if !term.is_null() {
        let term_ref: &NSView = &*(term as *const NSView);
        let frame = term_ref.frame();
        let chrome_h = f64::from_bits(SWZ_CHROME_H.load(Ordering::SeqCst));
        let in_x = point.x >= frame.origin.x && point.x < frame.origin.x + frame.size.width;
        let in_y = point.y >= frame.origin.y && point.y < frame.origin.y + frame.size.height;
        // Chrome bar is the TOP `chrome_h` points => high y in AppKit (contentView
        // is bottom-left origin). Inside the terminal frame but below the bar =>
        // hand the hit to the terminal.
        let in_chrome_bar = point.y >= frame.origin.y + frame.size.height - chrome_h;
        if in_x && in_y && !in_chrome_bar {
            SWZ_HITS_TERM.fetch_add(1, Ordering::Relaxed);
            // Return the TerminalView itself (not its MTKView subview): it is the
            // responder that implements mouseDown:/mouseDragged: for selection.
            return term as *mut AnyObject;
        }
    }
    SWZ_HITS_CHROME.fetch_add(1, Ordering::Relaxed);
    let orig = SWZ_ORIG.load(Ordering::SeqCst);
    if !orig.is_null() {
        let orig: HitTestFn = std::mem::transmute(orig);
        orig(this, cmd, point)
    } else {
        this
    }
}

/// Install the `hitTest:` override on gpui's `GPUIView` class so terminal-region
/// mouse hits route to `term`, and chrome hits stay with gpui. Idempotent.
///
/// Returns whether the override is now installed.
///
/// # Safety
/// `gpui_view` must be a live `GPUIView` instance and `term` a live `NSView*`
/// kept alive for the rest of the run. Main thread only.
pub unsafe fn install_hittest_shim(gpui_view: &NSView, term: *mut NSView, chrome_h: f64) -> bool {
    SWZ_TERM.store(term, Ordering::SeqCst);
    SWZ_CHROME_H.store(chrome_h.to_bits(), Ordering::SeqCst);
    if SWZ_INSTALLED.swap(true, Ordering::SeqCst) {
        return true; // already swizzled; just refreshed term + chrome height
    }

    let any: &AnyObject = gpui_view;
    let cls: &AnyClass = any.class(); // dynamic class == GPUIView
    let sel = sel!(hitTest:);

    // Capture the ORIGINAL (inherited NSView) implementation BEFORE adding ours,
    // so the override can delegate chrome / out-of-bounds points to it.
    let inherited: *const Method = objc2::ffi::class_getInstanceMethod(cls, sel);
    if inherited.is_null() {
        SWZ_INSTALLED.store(false, Ordering::SeqCst);
        return false;
    }
    if let Some(orig_imp) = objc2::ffi::method_getImplementation(inherited) {
        SWZ_ORIG.store(std::mem::transmute::<Imp, *mut c_void>(orig_imp), Ordering::SeqCst);
    }

    let our_imp: Imp = std::mem::transmute(hittest_override as HitTestFn);
    // hitTest: returns id, takes (self, _cmd, NSPoint/CGPoint{dd}).
    let types = c"@@:{CGPoint=dd}".as_ptr();
    let added = objc2::ffi::class_addMethod(cls as *const AnyClass as *mut AnyClass, sel, our_imp, types);

    if added.as_bool() {
        true
    } else {
        // GPUIView already had its own hitTest: (not expected) — swizzle it in
        // place instead, capturing the real original.
        let own: *const Method = objc2::ffi::class_getInstanceMethod(cls, sel);
        if let Some(prev) = objc2::ffi::method_setImplementation(own, our_imp) {
            SWZ_ORIG.store(std::mem::transmute::<Imp, *mut c_void>(prev), Ordering::SeqCst);
            true
        } else {
            SWZ_INSTALLED.store(false, Ordering::SeqCst);
            false
        }
    }
}

/// Returns the raw `NSView*` that the (swizzled) `hitTest:` resolves for `point`
/// (contentView coordinates). Used to PROVE routing deterministically without
/// the windowserver: a terminal-region point must return `term`, a chrome point
/// must return `gpui_view`.
///
/// # Safety
/// `gpui_view` must be a live `GPUIView`; main thread only.
pub unsafe fn hittest_resolves(gpui_view: &NSView, point: NSPoint) -> *mut NSView {
    match gpui_view.hitTest(point) {
        Some(v) => Retained::as_ptr(&v) as *mut NSView,
        None => null_mut(),
    }
}

/// (terminal-routed hits, chrome/fallthrough hits) since install — concrete
/// evidence for the §5 report.
pub fn hittest_counts() -> (u64, u64) {
    (
        SWZ_HITS_TERM.load(Ordering::Relaxed),
        SWZ_HITS_CHROME.load(Ordering::Relaxed),
    )
}

/// Synthesize a left mouse-down + multi-step drag + mouse-up through the REAL
/// responder chain (`NSApp.sendEvent`), so — IF the hit-test seam routes the
/// terminal region to the TerminalView — SwiftTerm's mouseDown:/mouseDragged:
/// run and a text selection forms. Locations are in window base coordinates
/// (bottom-left origin). At least two drag steps are sent so the selection has a
/// non-empty range (SwiftTerm starts the selection on the first drag).
///
/// # Safety
/// `app` must be the shared application and `window_number` a live key window.
pub unsafe fn synth_drag_select(
    app: &NSApplication,
    window_number: isize,
    from: NSPoint,
    to: NSPoint,
    steps: usize,
) {
    if let Some(down) = synth_mouse(NSEventType::LeftMouseDown, from, window_number, 1) {
        app.sendEvent(&down);
    }
    let n = steps.max(2);
    for i in 1..=n {
        let t = i as f64 / n as f64;
        let p = NSPoint::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
        if let Some(drag) = synth_mouse(NSEventType::LeftMouseDragged, p, window_number, 1) {
            app.sendEvent(&drag);
        }
    }
    if let Some(up) = synth_mouse(NSEventType::LeftMouseUp, to, window_number, 1) {
        app.sendEvent(&up);
    }
}

/// Promote the process to a regular foreground app and bring it to the front so
/// injected `NSApp.sendEvent` events traverse the REAL responder chain
/// (windowserver -> key window -> first responder). gpui opens the window but
/// may leave the app unfocused/behind; this forces activation (§C/§G4).
///
/// # Safety
/// Main thread only.
pub unsafe fn activate_front(app: &NSApplication) {
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    // `activateIgnoringOtherApps:` is universally available (the modern no-arg
    // `activate` is macOS 14+ only); gpui's `cx.activate(true)` calls the same.
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
}

/// Make `window` the genuine key + main + front window and re-assert it as the
/// terminal's host so synthetic key/mouse events route correctly.
///
/// # Safety
/// Main thread only; `window` must be live.
pub unsafe fn make_key_front(window: &NSWindow) {
    window.makeKeyAndOrderFront(None);
    window.orderFrontRegardless();
}

/// IME NOTE (DISPLAY-GATED): real marked-text (dead keys / CJK) is produced by
/// the system input source, not by a synthesizable NSEvent. Sending real
/// `keyDown:` via `send_event` already drives SwiftTerm's
/// `NSTextInputClient.insertText:` for committed text. To exercise
/// `setMarkedText:`/`unmarkText` deterministically, call `interpretKeyEvents:`
/// on the terminal first responder with a marked-text-producing key sequence
/// under an active input source — only meaningful with a real key window.
pub const IME_NOTE: &str = "marked-text/IME requires a real input source + key window (display-gated)";
