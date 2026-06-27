//! objc2 NSView embedding seam (RECON 2).
//!
//! Bridges gpui's raw window pointer -> objc2 `Retained<NSView>` and performs
//! all AppKit manipulation in objc2, exactly like the committed spikes.
//!
//! The key facts this implements (verified against gpui 0.2.2 source):
//!   * gpui hands out a `raw_window_handle::AppKit` handle whose `ns_view` is
//!     gpui's OWN `GPUIView` (its Metal renderer), NOT the window contentView.
//!   * gpui's metal layer is `opaque=false` and clears to alpha 0, so every
//!     transparent pixel of GPUIView reveals a SIBLING view beneath it.
//!   * gpui itself inserts a `BlurredView` sibling under contentView via
//!     `addSubview:positioned:NSWindowBelow relativeTo:nil` — we mirror that to
//!     place the terminal as a sibling just BELOW GPUIView.
//!
//! ⚠️ LOAD-BEARING SEAM (RECON 2 §5): with the terminal BELOW GPUIView (the
//! arrangement that makes "transparent GPUI over terminal" work, PoC item 5),
//! default NSView hit-testing makes GPUIView win EVERY mouse hit — the terminal
//! sibling gets no mouse events. Visual compositing and mouse hit-testing want
//! OPPOSITE z-orders. See `input.rs` and the README for the two resolutions
//! (hitTest: override/swizzle, or terminal-on-top-within-its-rect). The mouse
//! half is the genuine unknown that routes Path A vs objc2-hybrid.

use objc2::rc::Retained;
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSView, NSWindow, NSWindowOrderingMode,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use gpui::Window;

use crate::bridge::NsViewPtr;

/// The three native handles the embedding needs, all retained (we do NOT take
/// ownership of gpui's view — gpui owns it; we balance with the retain).
pub struct NativeHandles {
    /// gpui's `GPUIView` (its Metal renderer / `native_view`).
    pub gpui_view: Retained<NSView>,
    /// The hosting `NSWindow` (`GPUIWindow`).
    pub window: Retained<NSWindow>,
    /// The window's `contentView` — the ownership seam we insert siblings under.
    pub content: Retained<NSView>,
}

/// Pull gpui's native handles out of a live `&Window`.
///
/// MUST run on the main thread AFTER the window is on screen (otherwise
/// `gpui_view.window()` is nil). Returns `None` if the window is not yet
/// realized — callers should retry on a later frame.
pub fn native_handles(window: &Window) -> Option<NativeHandles> {
    // NOTE: `Window` has an inherent `window_handle()` returning gpui's
    // AnyWindowHandle, which shadows the trait method. Call the trait method
    // explicitly to get the raw-window-handle one.
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return None;
    };

    let gpui_view_ptr = appkit.ns_view.as_ptr() as *mut NSView;
    // Retain (don't own) — gpui keeps ownership of GPUIView.
    let gpui_view: Retained<NSView> = unsafe { Retained::retain(gpui_view_ptr) }?;
    let window_obj: Retained<NSWindow> = gpui_view.window()?;
    let content: Retained<NSView> = window_obj.contentView()?;

    Some(NativeHandles {
        gpui_view,
        window: window_obj,
        content,
    })
}

const RESIZE_BOTH: NSAutoresizingMaskOptions = NSAutoresizingMaskOptions(
    NSAutoresizingMaskOptions::ViewWidthSizable.0 | NSAutoresizingMaskOptions::ViewHeightSizable.0,
);

/// Insert the terminal `NSView` as a sibling under `contentView`, positioned
/// just BELOW `GPUIView`, so gpui composites its (transparent) chrome OVER it.
/// This is the arrangement that proves PoC item 5 (transparent over terminal).
///
/// `term_ptr` is the `NSView*` handed back by `st_nsview`.
///
/// # Safety
/// `term_ptr` must be a valid `NSView*` retained for at least the lifetime of
/// the `Terminal` that produced it.
pub unsafe fn embed_below_chrome(h: &NativeHandles, term_ptr: NsViewPtr) {
    let term: Retained<NSView> = Retained::retain(term_ptr as *mut NSView)
        .expect("st_nsview returned null");
    term.setFrame(h.content.bounds());
    term.setAutoresizingMask(RESIZE_BOTH);
    h.content.addSubview_positioned_relativeTo(
        &term,
        NSWindowOrderingMode::Below, // = NSWindowBelow (-1)
        Some(&h.gpui_view),
    );
}

/// Alternative arrangement (RECON 2 §5 option 2): terminal ABOVE the chrome but
/// confined to a sub-rect, so mouse + keyboard route to it naturally. gpui can
/// no longer composite OVER the terminal; chrome lives AROUND it. This is the
/// structural objc2-hybrid fallback. Frame is in `content` coordinates.
///
/// # Safety
/// See `embed_below_chrome`.
pub unsafe fn embed_above_in_rect(h: &NativeHandles, term_ptr: NsViewPtr, frame: NSRect) {
    let term: Retained<NSView> = Retained::retain(term_ptr as *mut NSView)
        .expect("st_nsview returned null");
    term.setFrame(frame);
    h.content.addSubview_positioned_relativeTo(
        &term,
        NSWindowOrderingMode::Above,
        Some(&h.gpui_view),
    );
}

/// Drive the terminal's frame from gpui layout (the partial-rect case). Call on
/// the main thread whenever the computed terminal bounds change.
///
/// # Safety
/// `term_ptr` must be a valid `NSView*`.
pub unsafe fn sync_terminal_frame(term_ptr: NsViewPtr, frame: NSRect) {
    let term = &*(term_ptr as *mut NSView);
    term.setFrame(frame);
}

/// Make the terminal the window's first responder (keyboard + IME owner).
/// Returns whether AppKit accepted it.
///
/// # Safety
/// `term_ptr` must be a valid `NSView*`.
pub unsafe fn make_terminal_first_responder(h: &NativeHandles, term_ptr: NsViewPtr) -> bool {
    let term: Retained<NSView> = Retained::retain(term_ptr as *mut NSView)
        .expect("st_nsview returned null");
    h.window.makeFirstResponder(Some(&term))
}

/// Give keyboard focus back to gpui's chrome (command palette, etc.).
pub fn make_chrome_first_responder(h: &NativeHandles) -> bool {
    h.window.makeFirstResponder(Some(&h.gpui_view))
}

/// Tear-off: reparent the SAME live terminal view into a second window's
/// content view, below that window's GPUIView. The caller MUST then toggle the
/// terminal's Metal layer off->on (`Terminal::set_use_metal(false/true)`) to
/// rebind `CAMetalLayer` `contentsScale`/`drawableSize` against the new screen
/// (PoC item 6).
///
/// # Safety
/// `term_ptr` must be a valid `NSView*`.
pub unsafe fn reparent_to(h2: &NativeHandles, term_ptr: NsViewPtr) {
    let term: Retained<NSView> = Retained::retain(term_ptr as *mut NSView)
        .expect("st_nsview returned null");
    term.removeFromSuperview();
    term.setFrame(h2.content.bounds());
    term.setAutoresizingMask(RESIZE_BOTH);
    h2.content.addSubview_positioned_relativeTo(
        &term,
        NSWindowOrderingMode::Below,
        Some(&h2.gpui_view),
    );
    h2.window.makeFirstResponder(Some(&term));
}

/// Convenience: a full-content-bounds rect (for the default full-window embed).
pub fn full_bounds(h: &NativeHandles) -> NSRect {
    h.content.bounds()
}

/// Build an `NSRect` from plain f64s (so callers don't import objc2_foundation).
pub fn rect(x: f64, y: f64, w: f64, ht: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, ht))
}

/// (max refresh fps, human label) for the display the window is CURRENTLY on.
///
/// Captured so every measurement self-documents the monitor + refresh rate it
/// paced to: `NSView.displayLink`/CoreAnimation pace the present loop to the
/// screen the window sits on, so a hot-plugged display silently changes the
/// vsync and can corrupt a cross-run comparison. `maximumFramesPerSecond` is
/// the display's nominal max (e.g. 120 on ProMotion, 60 on a typical external);
/// pair it with the measured present interval to tell every-vsync from
/// every-other-vsync. Returns `(0, "<no screen>")` if the window is off-screen.
pub fn screen_info(h: &NativeHandles) -> (i64, String) {
    match h.window.screen() {
        Some(screen) => {
            let fps = screen.maximumFramesPerSecond() as i64;
            let name = screen.localizedName().to_string();
            let f = screen.frame();
            (
                fps,
                format!(
                    "{name} [{}x{} @ {},{}]",
                    f.size.width as i64,
                    f.size.height as i64,
                    f.origin.x as i64,
                    f.origin.y as i64
                ),
            )
        }
        None => (0, "<no screen>".to_string()),
    }
}
