//! Platform module — the single home for foreign AppKit / objc2 access
//! (all-Rust rule: "Foreign AppKit access, when unavoidable, goes through
//! objc2 / objc2-app-kit and lives behind one platform module").
//!
//! For R1 this holds exactly one thing: the demand-present kick, plus the two
//! present-timing facts that motivate it. Later cycles grow this module (real
//! input handling, chrome, tear-off) — they add objc2 here, not scattered
//! across the app.
//!
//! ## Two gpui present-timing facts every later cycle must respect
//!
//! 1. **`cx.notify()` never PRESENTS while the CVDisplayLink is stopped.** gpui
//!    stops a window's display link when the window is occluded
//!    (`window_did_change_occlusion_state`). While stopped, marking a view dirty
//!    with `cx.notify()` does NOT reach `MetalRenderer::draw` — nothing
//!    presents. A demand-driven repaint on such a window needs an explicit
//!    `setNeedsDisplay` kick to the backing `NSView` + its `CAMetalLayer`, which
//!    fires `displayLayer:` on the next CA commit independently of the link
//!    state. That kick is [`present_kick`]. (The R1 `smoke` self-test sidesteps
//!    this by driving continuous `request_animation_frame` repaints on a visible
//!    window, so it never needs the kick — but later demand-driven scenarios do,
//!    which is why the helper lives here now.)
//!
//! 2. **zed-main frame-caps INACTIVE windows at ~33 ms** (`min_frame_interval`):
//!    a backgrounded window animates at ~30 fps regardless of the panel refresh.
//!    Frame-cadence assertions must therefore run on a FRONTMOST, FOCUSED window
//!    — which is why the self-test driver calls `cx.activate(true)` and the
//!    runbook requires the window be frontmost.

use std::ffi::c_void;

use gpui::Window;
use objc2::msg_send;
use objc2::runtime::AnyObject;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// The backing `NSView` pointer for a gpui window, via raw-window-handle.
/// Null if the window has no AppKit handle yet (not on screen).
#[allow(dead_code)]
pub fn ns_view_of(window: &Window) -> *mut c_void {
    // UFCS: gpui's `Window` has an inherent `window_handle()` returning
    // `gpui::AnyWindowHandle` that shadows the raw-window-handle trait method;
    // call the trait explicitly to reach the AppKit `NSView` pointer.
    match HasWindowHandle::window_handle(window) {
        Ok(handle) => match handle.as_raw() {
            RawWindowHandle::AppKit(appkit) => appkit.ns_view.as_ptr(),
            _ => std::ptr::null_mut(),
        },
        Err(_) => std::ptr::null_mut(),
    }
}

/// Force a demand present on an occluded / display-link-stopped window: mark the
/// `NSView` and its `CAMetalLayer` as needing display so the next CA commit
/// drives `displayLayer:` -> gpui request-frame -> `Window::present()` ->
/// `MetalRenderer::draw`, independent of the display-link state (fact 1 above).
///
/// # Safety
/// `ns_view` must be a valid `NSView*` (e.g. from [`ns_view_of`]) or null.
#[allow(dead_code)]
pub unsafe fn present_kick(ns_view: *mut c_void) {
    if ns_view.is_null() {
        return;
    }
    let view = ns_view as *mut AnyObject;
    let _: () = msg_send![view, setNeedsDisplay: true];
    let layer: *mut AnyObject = msg_send![view, layer];
    if !layer.is_null() {
        let _: () = msg_send![layer, setNeedsDisplay];
    }
}
