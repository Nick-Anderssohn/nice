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

/// Disable CoreGraphics font-smoothing glyph dilation for this process, matching
/// Nice's shipping `fontSmoothing=false`.
///
/// GPUI-main thickens glyph strokes by an amount that depends on the foreground
/// color's luminance whenever the `AppleFontSmoothing` pref is not an explicit
/// `0` (`gpui_macos::text_system::glyph_dilation_for_color` /
/// `font_smoothing_allowed_by_user`, read once via `OnceLock`). That dilation
/// is a *different* effect from the bg-luminance composition curve the renderer
/// relies on to match SwiftTerm, and with it enabled a light-on-dark glyph is
/// dilated far more than a dark-on-light one — swamping the curve. Nice never
/// wanted CoreGraphics smoothing (it ships `fontSmoothing=false`), so we set the
/// pref to `0` before the first glyph rasterizes; gpui then reads `0` and skips
/// the dilation, leaving the bg-luminance curve as the sole antialiasing shaping.
///
/// This writes the `AppleFontSmoothing` key into this app's own preferences
/// domain (`nice-rs`), exactly the domain + API gpui_macos's reader uses
/// (`CFPreferencesCopyAppValue(_, kCFPreferencesCurrentApplication)`), so the
/// same-process read sees it. Mirrors the phase-0 aa-gamma spike's
/// `set_apple_font_smoothing(false)`. Call once, before `Application::run`.
pub fn disable_font_smoothing() {
    use core_foundation::base::TCFType;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use core_foundation_sys::preferences::{
        kCFPreferencesCurrentApplication, CFPreferencesAppSynchronize, CFPreferencesSetAppValue,
    };

    let key = CFString::new("AppleFontSmoothing");
    let value = CFNumber::from(0i64);
    // SAFETY: `key`/`value` are live CF objects for the duration of the calls;
    // `kCFPreferencesCurrentApplication` is a valid constant domain. The set is
    // in-memory (+ synchronize flushes it) so gpui's later `OnceLock` read sees
    // it. No aliasing — we own both CF objects until they drop after this scope.
    unsafe {
        CFPreferencesSetAppValue(
            key.as_concrete_TypeRef(),
            value.as_CFTypeRef(),
            kCFPreferencesCurrentApplication,
        );
        CFPreferencesAppSynchronize(kCFPreferencesCurrentApplication);
    }
}

/// The backing `NSView` pointer for a gpui window, via raw-window-handle.
/// Null if the window has no AppKit handle yet (not on screen).
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
