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
//!    which is why the helper lives here now.) **The converse also holds, and
//!    the kick is gated on it (fix round r5d):** while a window IS
//!    occlusion-visible its display link is ticking, `cx.notify()` alone
//!    presents on the next ~10 ms tick, and the kick is not merely redundant
//!    but harmful — every `setNeedsDisplay` drives gpui's `displayLayer:`
//!    (`gpui_macos/src/window.rs`), which STOPS the link, draws with
//!    `presents_with_transaction`, then RECREATES the link from scratch
//!    (`start_display_link`: a new CVDisplayLink thread + dispatch source per
//!    call). Under a pty flood the drain fired that up to ~166/s, and
//!    `start_display_link` has silent-death paths (a stale occlusion read that
//!    plain-`return`s; `DisplayLink::new(..).log_err()` → `None`) — one
//!    transient failure among thousands of recreations left the link
//!    permanently stopped: the 2026-07-10 presentation wedge, where a mid-flood
//!    `sample` showed the CVDisplayLink thread parked in `waitUntil` with ZERO
//!    `display_link_callback` fires in 3 s while the app stayed fully
//!    responsive (AX ~9 ms) and the screen froze on a stale frame for minutes
//!    until an activation/occlusion edge restarted the link. So
//!    [`present_kick`] now fires the `setNeedsDisplay` ONLY when the window is
//!    NOT occlusion-visible — exactly the states in which gpui keeps the link
//!    stopped and the kick is the sole path to a present. The occluded-modal
//!    guarantee (ec0b8f3: quit/close dialogs must paint on an occluded window)
//!    is preserved verbatim: occluded → the kick fires as before.
//!
//! 2. **zed-main frame-caps INACTIVE windows at ~33 ms** (`min_frame_interval`):
//!    a backgrounded window animates at ~30 fps regardless of the panel refresh.
//!    Frame-cadence assertions must therefore run on a FRONTMOST, FOCUSED window
//!    — which is why the self-test driver calls `cx.activate(true)` and the
//!    runbook requires the window be frontmost.

use std::ffi::c_void;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use gpui::Window;
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect, NSSize};
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

/// Read a boolean from this app's own CFPreferences domain — the same
/// `kCFPreferencesCurrentApplication` domain [`disable_font_smoothing`] writes and
/// gpui's smoothing reader consults. Returns `default` when the key is absent or
/// not a valid boolean.
///
/// R17's Claude theme-sync gate (`syncClaudeTheme`, default ON) is read through
/// this once at bootstrap, so `defaults write dev.nickanderssohn.nice-rs
/// syncClaudeTheme -bool false` is the dev-time escape hatch until R23 binds a
/// Settings toggle to the same key (the `disable_font_smoothing` own-domain FFI
/// precedent, in the read direction). Call on the main thread before the first
/// window opens.
pub fn read_bool_pref(key: &str, default: bool) -> bool {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::preferences::{
        kCFPreferencesCurrentApplication, CFPreferencesGetAppBooleanValue,
    };

    let key = CFString::new(key);
    let mut exists: u8 = 0;
    // SAFETY: `key` is a live CF object for the duration of the call;
    // `kCFPreferencesCurrentApplication` is a valid constant domain; `&mut exists`
    // is a valid `Boolean` out-param. The call only READS the app domain and
    // reports via `exists` whether the key was present with a valid boolean
    // format — we fall back to `default` when it was not.
    let value = unsafe {
        CFPreferencesGetAppBooleanValue(
            key.as_concrete_TypeRef(),
            kCFPreferencesCurrentApplication,
            &mut exists,
        )
    };
    if exists != 0 {
        value != 0
    } else {
        default
    }
}

/// Write a boolean into this app's own CFPreferences domain — the write sibling
/// of [`read_bool_pref`], targeting the same `kCFPreferencesCurrentApplication`
/// domain (the [`disable_font_smoothing`] own-domain FFI precedent, in the write
/// direction) and synchronizing so the next [`read_bool_pref`] of the same key
/// (incl. R17's boot gate read) sees the new value.
///
/// R23's Settings "Sync Claude Code theme" toggle persists through this to the
/// `syncClaudeTheme` key R17 reads at boot (D4) — the single source of truth,
/// keeping the `defaults write dev.nickanderssohn.nice-rs syncClaudeTheme` dev
/// hatch valid. **Hermeticity:** this touches the REAL CFPrefs domain, so it is
/// reachable ONLY from the live toggle handler in `app::run`-installed UI — never
/// from `run_selftest` or a test (the scenario drives the toggle's LIVE arm via
/// R21's `apply_sync_claude_theme`, not this write). Call on the main thread.
pub fn write_bool_pref(key: &str, value: bool) {
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::string::CFString;
    use core_foundation_sys::preferences::{
        kCFPreferencesCurrentApplication, CFPreferencesAppSynchronize, CFPreferencesSetAppValue,
    };

    let key = CFString::new(key);
    let value = CFBoolean::from(value);
    // SAFETY: `key`/`value` are live CF objects for the duration of the calls;
    // `kCFPreferencesCurrentApplication` is a valid constant domain. The set is
    // in-memory (+ synchronize flushes it) so a later same-process/boot
    // `read_bool_pref` sees it. We own both CF objects until they drop after this
    // scope — no aliasing.
    unsafe {
        CFPreferencesSetAppValue(
            key.as_concrete_TypeRef(),
            value.as_CFTypeRef(),
            kCFPreferencesCurrentApplication,
        );
        CFPreferencesAppSynchronize(kCFPreferencesCurrentApplication);
    }
}

/// The macOS keyCode side-channel feeding the R5 keyboard encoder.
///
/// gpui's `Keystroke` on the pin carries only `{modifiers, key, key_char}` — no
/// raw hardware keyCode (a settled fact of the platform backend, not a fork
/// candidate). The kitty encoder wants the layout-independent physical key to
/// recover the base-layout alternate codepoint, so we read it off the AppKit
/// event currently being dispatched: `[[NSApp currentEvent] keyCode]`.
///
/// This is injected into the terminal view (`set_keycode_probe`), exactly like
/// the demand-present kick, so `nice-term-view` stays objc2-free. It is only ever
/// called synchronously from the view's key-down / key-up handlers, where
/// `currentEvent` is the key event being processed. It returns `None` when there
/// is no current event, or the current event is not a key/flags event (guarded so
/// `keyCode` — which raises on a non-key event — is never sent to the wrong type).
pub fn current_event_keycode() -> Option<u16> {
    // NSEventType discriminants (AppKit): keyDown = 10, keyUp = 11,
    // flagsChanged = 12. Only these carry a meaningful `keyCode`.
    const NS_EVENT_TYPE_KEY_DOWN: u64 = 10;
    const NS_EVENT_TYPE_KEY_UP: u64 = 11;
    const NS_EVENT_TYPE_FLAGS_CHANGED: u64 = 12;

    // SAFETY: `NSApplication.sharedApplication` is a live singleton once the app
    // is running; this is only called on the main thread during key dispatch.
    // `currentEvent` may be nil (-> null, handled). We read the event type before
    // `keyCode` so the selector is only sent to a key/flags event (sending
    // `keyCode` to e.g. a mouse event raises an Objective-C exception).
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return None;
        }
        let event: *mut AnyObject = msg_send![app, currentEvent];
        if event.is_null() {
            return None;
        }
        let event_type: u64 = msg_send![event, type];
        if !matches!(
            event_type,
            NS_EVENT_TYPE_KEY_DOWN | NS_EVENT_TYPE_KEY_UP | NS_EVENT_TYPE_FLAGS_CHANGED
        ) {
            return None;
        }
        let keycode: u16 = msg_send![event, keyCode];
        Some(keycode)
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

/// `NSWindowOcclusionStateVisible` (AppKit: `1UL << 1`) — the only bit of the
/// `NSWindowOcclusionState` option set. Hand-declared in this module's raw-FFI
/// style (like the `NS_EVENT_TYPE_*` discriminants above); it is also exactly
/// the bit gpui's `start_display_link` / `window_did_change_occlusion_state`
/// test, so the kick gate below and gpui's link lifecycle read the same signal.
const NS_WINDOW_OCCLUSION_STATE_VISIBLE: u64 = 1 << 1;

/// Whether `ns_view`'s window is currently occlusion-VISIBLE, or `None` when
/// the view is not hosted in a window (no `NSWindow` to ask). This is the query
/// half of the r5d kick gate; the decision half is [`present_kick_due`].
///
/// # Safety
/// `ns_view` must be a valid, non-null `NSView*`. Main thread only (an
/// `NSWindow` property read) — every kick caller already satisfies this: the
/// drain kick runs inside `window.update` on the foreground executor, and the
/// modal kick runs inside gpui entity callbacks.
unsafe fn view_window_occlusion_visible(ns_view: *mut AnyObject) -> Option<bool> {
    let window: *mut AnyObject = msg_send![ns_view, window];
    if window.is_null() {
        return None;
    }
    let state: u64 = msg_send![window, occlusionState];
    Some(state & NS_WINDOW_OCCLUSION_STATE_VISIBLE != 0)
}

/// The r5d occlusion-gate decision (pure — the unit-testable seam of
/// [`present_kick`], which is otherwise objc2 all the way down): fire the
/// `setNeedsDisplay` kick iff the window is NOT known occlusion-visible.
///
/// - `Some(true)` (visible) → **skip**: the display link is ticking (gpui stops
///   it only on occlusion), so the `cx.notify()` that always precedes a kick is
///   presented by the link's next tick within ~a frame — and firing anyway
///   would re-enter `displayLayer:`'s stop/draw/recreate cycle, whose
///   per-recreation failure odds are what wedged presentation on 2026-07-10
///   (see the module "fact 1" evidence).
/// - `Some(false)` (occluded/minimized/hidden) → **kick**: the link is stopped;
///   the kick is the only path to a present (ec0b8f3's occluded-modal fix).
/// - `None` (view not in a window yet) → **kick**: preserve the pre-gate
///   behavior in the unknown state; `setNeedsDisplay` on an unhosted view is
///   harmless, and headless/teardown callers relied on the kick being a no-op
///   rather than on it being skipped.
fn present_kick_due(occlusion_visible: Option<bool>) -> bool {
    !matches!(occlusion_visible, Some(true))
}

/// Force a demand present on an occluded / display-link-stopped window: mark the
/// `NSView` and its `CAMetalLayer` as needing display so the next CA commit
/// drives `displayLayer:` -> gpui request-frame -> `Window::present()` ->
/// `MetalRenderer::draw`, independent of the display-link state (fact 1 above).
///
/// **Occlusion-gated (r5d):** on an occlusion-VISIBLE window this is a no-op —
/// the running display link presents the already-notified dirty window on its
/// next tick, and kicking anyway would stop + recreate that link per call via
/// `displayLayer:` (up to ~166/s under the drain's throttled flood cadence),
/// which is the recreate storm that let one transient `start_display_link`
/// failure freeze presentation for minutes (module "fact 1"). All kick callers
/// funnel through here — the terminal drain (`app::install_present_kick`) and
/// the confirmation modal's present/dismiss (`WindowState::present_kick_modal`,
/// including its Esc/on-key dismiss path) — so the gate covers them uniformly
/// with no caller changes.
///
/// # Safety
/// `ns_view` must be a valid `NSView*` (e.g. from [`ns_view_of`]) or null.
/// Main thread only (see [`view_window_occlusion_visible`]).
pub unsafe fn present_kick(ns_view: *mut c_void) {
    if ns_view.is_null() {
        return;
    }
    let view = ns_view as *mut AnyObject;
    if !present_kick_due(view_window_occlusion_visible(view)) {
        return;
    }
    let _: () = msg_send![view, setNeedsDisplay: true];
    let layer: *mut AnyObject = msg_send![view, layer];
    if !layer.is_null() {
        let _: () = msg_send![layer, setNeedsDisplay];
    }
}

// ===========================================================================
// R9 window chrome — live standard-window-button (traffic light) geometry.
//
// gpui repositions the native close/minimize/zoom buttons declaratively from
// `TitlebarOptions::traffic_light_position` (see `crate::app::window_options`),
// re-applying the position itself on resize / focus / full-screen exit. The R9
// live scenario must assert the REAL rendered geometry rather than trusting the
// point we passed, so this reader queries AppKit's `standardWindowButton:`
// frames directly. It is the R9 slice's one new AppKit reach-through (all-Rust
// rule: every objc2 crossing lives in this module).
// ===========================================================================

/// One standard window button's frame in the window content view's coordinate
/// space, with `y_from_top` measured DOWNWARD from the content view's top edge —
/// so it aligns with gpui's own top-left origin and the R9 traffic-light target
/// (the close button's [`center_from_top`](Self::center_from_top) is the y-26
/// row).
#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(dead_code)] // R9 slice 3 (the `chrome` scenario + nice-itests) consume this.
pub struct WindowButtonFrame {
    /// Leading (left) origin x, content-view points.
    pub x: f32,
    /// Top edge distance from the content view's top, points.
    pub y_from_top: f32,
    /// Button frame width, points.
    pub width: f32,
    /// Button frame height, points.
    pub height: f32,
}

#[allow(dead_code)] // R9 slice 3 (the `chrome` scenario + nice-itests) consume this.
impl WindowButtonFrame {
    /// The button's visual-center y, measured from the content view's top — the
    /// value the R9 scenario asserts lands on the y-26 row for the close button.
    pub fn center_from_top(&self) -> f32 {
        self.y_from_top + self.height / 2.0
    }
}

/// Read the live close / minimize / zoom standard-window-button frames of
/// `window` (in that order) straight from AppKit (`standardWindowButton:`), so a
/// test can assert the REAL rendered traffic-light geometry instead of trusting
/// the point passed to `window_options()`. `None` if the window has no AppKit
/// handle yet, or any standard button is absent (e.g. a borderless window).
///
/// Frames are reported in the window content view's coordinate space with y
/// measured from the top (see [`WindowButtonFrame`]); the conversion is
/// flipped-view-aware, so it stays correct whether or not the content view is
/// flipped (gpui's is not today, but this does not depend on that).
///
/// # Threading
/// Main thread only — a synchronous AppKit view-geometry read, called from the
/// R9 scenario's foreground task.
#[allow(dead_code)] // R9 slice 3 (the `chrome` scenario + nice-itests) consume this.
pub fn standard_window_button_frames(window: &Window) -> Option<[WindowButtonFrame; 3]> {
    // `NSWindowButton` raw values: close = 0, miniaturize = 1, zoom = 2.
    const NS_WINDOW_BUTTONS: [u64; 3] = [0, 1, 2];

    let ns_view = ns_view_of(window);
    if ns_view.is_null() {
        return None;
    }
    // SAFETY: `ns_view` is this gpui window's live content `NSView`. We read the
    // window, its content view, and the three standard window buttons, converting
    // each button's bounds rect into content-view coordinates — all main-thread
    // AppKit accessors with no ownership transfer (get-rule; nothing to release).
    unsafe {
        let view = ns_view as *mut AnyObject;
        let ns_window: *mut AnyObject = msg_send![view, window];
        if ns_window.is_null() {
            return None;
        }
        let content_view: *mut AnyObject = msg_send![ns_window, contentView];
        if content_view.is_null() {
            return None;
        }
        let content_bounds: NSRect = msg_send![content_view, bounds];
        let content_height = content_bounds.size.height;
        let flipped: Bool = msg_send![content_view, isFlipped];
        let content_flipped = flipped.as_bool();

        let mut out = [WindowButtonFrame {
            x: 0.0,
            y_from_top: 0.0,
            width: 0.0,
            height: 0.0,
        }; 3];
        for (i, &kind) in NS_WINDOW_BUTTONS.iter().enumerate() {
            let button: *mut AnyObject = msg_send![ns_window, standardWindowButton: kind];
            if button.is_null() {
                return None;
            }
            let bounds: NSRect = msg_send![button, bounds];
            // The buttons live in the titlebar container, a sibling of the content
            // view; AppKit converts the rect across the shared window hierarchy.
            let frame: NSRect = msg_send![button, convertRect: bounds, toView: content_view];
            let y_from_top = if content_flipped {
                // Flipped view: origin is top-left, y already grows downward.
                frame.origin.y
            } else {
                // Non-flipped (AppKit default): y grows up from the bottom edge.
                content_height - (frame.origin.y + frame.size.height)
            };
            out[i] = WindowButtonFrame {
                x: frame.origin.x as f32,
                y_from_top: y_from_top as f32,
                width: frame.size.width as f32,
                height: frame.size.height as f32,
            };
        }
        Some(out)
    }
}

// ===========================================================================
// M2 feel-check Item A — SF Symbol rasterization.
//
// GPUI has no SF Symbol renderer, so the app's icons rasterize
// `NSImage(systemSymbolName:)` through AppKit at runtime (pixel parity with
// Swift Nice, no bundled assets). This is the foreign half: resolve the
// symbol, apply an `NSImageSymbolConfiguration` (point size + weight), and
// draw it into a CoreGraphics bitmap at the window's backing scale, returning
// the straight coverage mask. The Rust half (`crate::sf_symbols`) tints that
// mask into a gpui `RenderImage` and caches it — keeping every objc2/CG
// crossing in this one module (all-Rust rule).
// ===========================================================================

// AppKit `NSFontWeight` constants (each an extern `CGFloat` global). Linked
// rather than hardcoded so the exact platform values feed
// `NSImageSymbolConfiguration` — the same weights Swift's
// `.font(.system(size:weight:))` resolves.
#[link(name = "AppKit", kind = "framework")]
extern "C" {
    static NSFontWeightRegular: f64;
    static NSFontWeightSemibold: f64;
}

/// AppKit's `NSFontWeightRegular` (CGFloat).
pub fn ns_font_weight_regular() -> f64 {
    // SAFETY: reading an extern AppKit CGFloat constant.
    unsafe { NSFontWeightRegular }
}

/// AppKit's `NSFontWeightSemibold`.
pub fn ns_font_weight_semibold() -> f64 {
    // SAFETY: reading an extern AppKit CGFloat constant.
    unsafe { NSFontWeightSemibold }
}

// CoreGraphics bitmap-context FFI for the symbol rasterizer (hand-declared in
// the raw style this module already uses for CGEvent).
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
    fn CGColorSpaceRelease(space: *mut c_void);
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: *mut c_void,
        bitmap_info: u32,
    ) -> *mut c_void;
    fn CGBitmapContextGetData(ctx: *mut c_void) -> *mut c_void;
    fn CGBitmapContextGetBytesPerRow(ctx: *mut c_void) -> usize;
    fn CGContextRelease(ctx: *mut c_void);
    fn CGContextScaleCTM(ctx: *mut c_void, sx: f64, sy: f64);
}

/// `kCGImageAlphaPremultipliedLast` — RGBA with premultiplied alpha, the only
/// 32-bit-with-alpha layout CGBitmapContext supports for drawing. Only the
/// alpha channel is read back (a coverage mask), so the premultiplication of
/// the colour channels never matters.
const CG_IMAGE_ALPHA_PREMULTIPLIED_LAST: u32 = 1;

/// Opaque `CGContext` used ONLY to give the
/// `graphicsContextWithCGContext:flipped:` argument the Objective-C type
/// encoding AppKit declares (`^{CGContext=}`): objc2's `msg_send!` verifies
/// encodings in debug builds and rejects a bare `*mut c_void` (`^v`) there.
#[repr(C)]
struct OpaqueCGContext {
    _priv: [u8; 0],
}

// SAFETY: `*mut OpaqueCGContext` encodes as `^{CGContext=}`, exactly the
// `CGContextRef` encoding the AppKit method declares.
unsafe impl objc2::RefEncode for OpaqueCGContext {
    const ENCODING_REF: objc2::Encoding =
        objc2::Encoding::Pointer(&objc2::Encoding::Struct("CGContext", &[]));
}

/// `NSCompositingOperationSourceOver`.
const NS_COMPOSITING_SOURCE_OVER: u64 = 2;

/// Opaque block type used ONLY to give a nil block argument (e.g. a
/// `completionHandler:`) the Objective-C block encoding (`@?`) objc2's debug
/// `msg_send!` verification demands — passing a bare `*mut AnyObject` (`@`) there
/// panics on the encoding mismatch, the same gotcha as [`OpaqueCGContext`].
#[repr(C)]
struct OpaqueBlock {
    _priv: [u8; 0],
}

// SAFETY: `*mut OpaqueBlock` encodes as `@?`, the block-pointer encoding a
// `completionHandler:` argument declares.
unsafe impl objc2::RefEncode for OpaqueBlock {
    const ENCODING_REF: objc2::Encoding = objc2::Encoding::Block;
}

/// One rasterized SF Symbol: a straight (non-premultiplied) per-pixel coverage
/// mask, row-major, top row first, one byte per device pixel.
pub struct SymbolBitmap {
    /// Coverage (0 transparent … 255 fully inked), `px_width * px_height` bytes.
    pub coverage: Vec<u8>,
    /// Bitmap width in device pixels (`ceil(point width × scale)`).
    pub px_width: usize,
    /// Bitmap height in device pixels.
    pub px_height: usize,
}

/// Rasterize the SF Symbol `name` at `point_size` / `ns_weight` (an AppKit
/// `NSFontWeight`, see the accessors above) into a coverage mask at `scale`
/// device pixels per point (the window's backing scale). `None` when the
/// symbol name does not resolve on this OS, or any AppKit/CG step fails —
/// callers fall back to the Unicode stand-in glyph so nothing goes blank.
///
/// # Threading
/// Main thread only, with an active autorelease pool (every caller is a gpui
/// render pass, which satisfies both — same contract as the geometry readers
/// above).
pub fn rasterize_sf_symbol(
    name: &str,
    point_size: f32,
    ns_weight: f64,
    scale: f32,
) -> Option<SymbolBitmap> {
    // SAFETY: class methods on NSImage / NSImageSymbolConfiguration /
    // NSGraphicsContext return autoreleased objects (get rule — nothing to
    // release); the CG colour space / bitmap context are +1 handles released
    // below. Drawing happens inside a saved/restored NSGraphicsContext scope on
    // the main thread. The bitmap data pointer is owned by the context and read
    // before the context is released.
    unsafe {
        let ns_name = ns_string(name);
        let base: *mut AnyObject = msg_send![
            class!(NSImage),
            imageWithSystemSymbolName: ns_name,
            accessibilityDescription: std::ptr::null_mut::<AnyObject>()
        ];
        if base.is_null() {
            return None;
        }
        // Point size + weight (the palette tint is applied by the Rust half —
        // the coverage mask is colour-independent).
        let config: *mut AnyObject = msg_send![
            class!(NSImageSymbolConfiguration),
            configurationWithPointSize: point_size as f64,
            weight: ns_weight
        ];
        let image: *mut AnyObject = if config.is_null() {
            base
        } else {
            let configured: *mut AnyObject = msg_send![base, imageWithSymbolConfiguration: config];
            if configured.is_null() {
                base
            } else {
                configured
            }
        };

        let size: NSSize = msg_send![image, size];
        if size.width <= 0.0 || size.height <= 0.0 {
            return None;
        }
        let scale = f64::from(scale.max(1.0));
        let px_width = (size.width * scale).ceil() as usize;
        let px_height = (size.height * scale).ceil() as usize;
        if px_width == 0 || px_height == 0 {
            return None;
        }

        let space = CGColorSpaceCreateDeviceRGB();
        let ctx = CGBitmapContextCreate(
            std::ptr::null_mut(),
            px_width,
            px_height,
            8,
            px_width * 4,
            space,
            CG_IMAGE_ALPHA_PREMULTIPLIED_LAST,
        );
        CGColorSpaceRelease(space);
        if ctx.is_null() {
            return None;
        }
        // Draw in point coordinates; the CTM maps them onto the device-pixel
        // bitmap.
        CGContextScaleCTM(ctx, scale, scale);

        let _: () = msg_send![class!(NSGraphicsContext), saveGraphicsState];
        let gctx: *mut AnyObject = msg_send![
            class!(NSGraphicsContext),
            graphicsContextWithCGContext: ctx as *mut OpaqueCGContext,
            flipped: false
        ];
        let _: () = msg_send![class!(NSGraphicsContext), setCurrentContext: gctx];
        let rect = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size,
        };
        let zero = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: NSSize {
                width: 0.0,
                height: 0.0,
            },
        };
        let _: () = msg_send![
            image,
            drawInRect: rect,
            fromRect: zero,
            operation: NS_COMPOSITING_SOURCE_OVER,
            fraction: 1.0f64
        ];
        let _: () = msg_send![class!(NSGraphicsContext), restoreGraphicsState];

        let data = CGBitmapContextGetData(ctx) as *const u8;
        if data.is_null() {
            CGContextRelease(ctx);
            return None;
        }
        let row_bytes = CGBitmapContextGetBytesPerRow(ctx);
        // Bitmap-context memory is top row first (pixel (0,0) of the buffer is
        // the visual top-left), so a straight row walk yields a top-down mask.
        let mut coverage = vec![0u8; px_width * px_height];
        for y in 0..px_height {
            for x in 0..px_width {
                coverage[y * px_width + x] = *data.add(y * row_bytes + x * 4 + 3);
            }
        }
        CGContextRelease(ctx);

        Some(SymbolBitmap {
            coverage,
            px_width,
            px_height,
        })
    }
}

// ===========================================================================
// R7 drag-drop — raw-image pasteboard fallback.
//
// gpui's macOS backend registers only `NSFilenamesPboardType` for drags, so a
// file drop reaches the view as `ExternalPaths` (file URLs) but a *raw-image*
// drag (from a browser / Messages / Preview — image data, no file URL) carries
// no path. The Swift app handles that by transcoding the pasteboard image to a
// temp PNG and typing that path (`NiceTerminalView.swift:441-512`). This is the
// objc2 half of that fallback, injected into the view as `set_image_drop_provider`
// so `nice-term-view` stays objc2-free (like the keyCode side-channel above).
//
// NOTE: with today's stock gpui backend a raw-image drag is not delivered to the
// window at all (it accepts only filename drags), so this provider is wired but
// dormant until the backend also registers the image drag types; the file-URL
// drop path (the common case: Finder / the in-app file explorer) is fully live.
// ===========================================================================

// AppKit pasteboard constants (each is an `NSString *const` global; reading the
// extern static yields the interned NSString pointer). Hand-declared in the raw-
// FFI style this module already uses rather than pulling in objc2-app-kit.
#[link(name = "AppKit", kind = "framework")]
extern "C" {
    static NSPasteboardNameDrag: *const AnyObject;
    static NSPasteboardTypePNG: *const AnyObject;
    static NSPasteboardTypeTIFF: *const AnyObject;
    /// `NSPasteboardTypeString` — the plain-text UTI the R20 Copy-Path write uses.
    static NSPasteboardTypeString: *const AnyObject;
}

/// `NSBitmapImageFileType.png` — the file-type selector for
/// `-representationUsingType:properties:` (AppKit `NSBitmapImageFileTypePNG`).
const NS_BITMAP_FILE_TYPE_PNG: u64 = 4;

/// Read the current drag pasteboard for image data, transcode it to PNG, write it
/// to a per-process temp file, and return that path — or `None` when the drag
/// carried no image (or the write failed). The returned path is all-safe ASCII,
/// so it passes the drop handler's path filter unchanged.
///
/// Prefers a direct PNG payload; otherwise transcodes the canonical TIFF
/// representation via `NSBitmapImageRep` (browsers usually drop one or the other).
/// Mirrors Swift's `pngData(from:)` + `writeDroppedImage(_:)`.
///
/// Called synchronously on the main thread from the view's drop handler, where an
/// AppKit autorelease pool is active, so the autoreleased `NSData`/`NSBitmapImageRep`
/// need no manual release.
pub fn read_dropped_image_to_temp() -> Option<PathBuf> {
    let png = unsafe { drag_pasteboard_png_bytes()? };
    write_dropped_image(&png)
}

/// The PNG bytes for the image currently on the drag pasteboard, or `None`.
///
/// # Safety
/// Must be called on the main thread with an active autorelease pool (the drop
/// handler satisfies both).
unsafe fn drag_pasteboard_png_bytes() -> Option<Vec<u8>> {
    // [NSPasteboard pasteboardWithName: NSPasteboardNameDrag]
    let pb: *mut AnyObject = msg_send![class!(NSPasteboard), pasteboardWithName: NSPasteboardNameDrag];
    if pb.is_null() {
        return None;
    }

    // Direct PNG first (no transcode needed).
    let png_data: *mut AnyObject = msg_send![pb, dataForType: NSPasteboardTypePNG];
    if !png_data.is_null() {
        return ns_data_bytes(png_data);
    }

    // Otherwise transcode the TIFF representation to PNG via NSBitmapImageRep.
    let tiff_data: *mut AnyObject = msg_send![pb, dataForType: NSPasteboardTypeTIFF];
    if tiff_data.is_null() {
        return None;
    }
    let rep: *mut AnyObject = msg_send![class!(NSBitmapImageRep), imageRepWithData: tiff_data];
    if rep.is_null() {
        return None;
    }
    let empty_props: *mut AnyObject = msg_send![class!(NSDictionary), dictionary];
    let png_data: *mut AnyObject = msg_send![
        rep,
        representationUsingType: NS_BITMAP_FILE_TYPE_PNG,
        properties: empty_props
    ];
    if png_data.is_null() {
        return None;
    }
    ns_data_bytes(png_data)
}

/// Copy an `NSData`'s bytes into an owned `Vec<u8>`.
///
/// # Safety
/// `data` must be a valid `NSData*` (or null, guarded).
unsafe fn ns_data_bytes(data: *mut AnyObject) -> Option<Vec<u8>> {
    if data.is_null() {
        return None;
    }
    let len: usize = msg_send![data, length];
    if len == 0 {
        return None;
    }
    let ptr: *const u8 = msg_send![data, bytes];
    if ptr.is_null() {
        return None;
    }
    Some(std::slice::from_raw_parts(ptr, len).to_vec())
}

/// Write PNG `bytes` to a fresh per-process temp file and return its path, or
/// `None` if the directory / file could not be created. Port of
/// `writeDroppedImage` (a caches subdir; here the per-user temp dir — same-user
/// readable, which is all the child shell needs).
fn write_dropped_image(bytes: &[u8]) -> Option<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    // Monotonic tiebreaker so two drops in the same nanosecond never collide.
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let dir = std::env::temp_dir().join("Nice").join("dropped-images");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("drop-{}-{}-{}.png", std::process::id(), nanos, seq));
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

// ===========================================================================
// R5 live input validation — CGEvent posting, Accessibility trust, and the
// TIS keyboard-input-source switch.
//
// This is the foreign-CoreGraphics/Carbon side of the `input-live` /
// `input-shell` self-test scenarios (`crate::input_live`). It has no place in
// the shipped app — real input arrives through gpui's own event path — but the
// all-Rust rule keeps every foreign C crossing in this one platform module, so
// the CGEvent/AX/TIS FFI lives here rather than scattered in the scenario code.
//
// SAFETY INVARIANT (mirrors the phase-0 `keyinject.swift`): synthetic events are
// posted ONLY with `CGEventPostToPid` targeting one pid — never `CGEventPost`
// (the global HID tap). The scenarios post to nice-rs's OWN pid, so an injected
// keystroke can only ever reach this process, never whatever the user is typing
// into elsewhere on the machine.
//
// R27 CARVE-OUT (the ONLY exception, narrowly fenced): a global `CGEventPost`
// (the HID tap) is permitted ONLY through the two named seams
// [`post_global_left_click`] / [`post_global_left_drag`] below, ONLY from
// selftest / scenario code (the R27 §6 close-out composition leg), and ONLY
// after that leg's REQUIRED preflight has verified our window owns the target
// point (activate + raise + `CGWindowListCopyWindowInfo` frontmost-at-point
// z-order check); on preflight failure the caller DEFERS LOUDLY and does NOT
// post. The carve-out exists because pid-posted MOUSE events silently drop
// (hover paints, `mouseDown` never fires — the M6 record), so the composed
// leg's real clicks/drags MUST go through the global tap. **Keyboard synthetic
// events remain `CGEventPostToPid`-only, unchanged.** No other call site may use
// the global seams.
// ===========================================================================

// Opaque CoreFoundation / CoreGraphics / Carbon handles. All are pointer-width;
// the CF-typed ones (`CGEventRef`, `TISInputSourceRef`, the CF*Ref) are released
// with `CFRelease` per the Create/Copy rule where noted.
type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;
type TisInputSourceRef = *mut c_void;
type CfArrayRef = *const c_void;
type CfDictionaryRef = *const c_void;
type CfStringRef = *const c_void;
type CfIndex = isize;
/// An `AXUIElement` handle (an accessibility node). Created +1 for the app
/// element (Create rule → released); child handles are borrowed from their
/// parent's `AXChildren` array (owned by that array). Used by the `ax-probe`
/// self-test scenario to walk this process's macOS Accessibility tree.
type AXUIElementRef = *const c_void;
/// A generic `CFTypeRef` out-parameter for `AXUIElementCopyAttributeValue`
/// (a `CFString`, `CFArray`, or `AXUIElement`, depending on the attribute).
type CfTypeRef = *const c_void;

/// The `CGEventFlags` ⌘ (Command) mask — carried on a synthesized ⌘V / ⌘=.
pub const FLAG_COMMAND: u64 = 0x0010_0000;

/// The `CGEventFlags` ⌥ (Option / Alternate) mask — carried with ⌘ on the
/// `multiwindow` scenario's ⌘⌥↓ sidebar-tab chord.
pub const FLAG_OPTION: u64 = 0x0008_0000;

/// The `CGEventFlags` ⇧ (Shift) mask — carried with ⌘ on the R19 `file-browser`
/// scenario's ⌘⇧B (toggle sidebar mode) and ⌘⇧. (toggle hidden files) chords.
pub const FLAG_SHIFT: u64 = 0x0002_0000;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        virtual_key: u16,
        key_down: bool,
    ) -> CGEventRef;
    fn CGEventKeyboardSetUnicodeString(event: CGEventRef, length: usize, string: *const u16);
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventPostToPid(pid: i32, event: CGEventRef);
    /// `CFArrayRef CGWindowListCopyWindowInfo(CGWindowListOption, CGWindowID)` —
    /// a +1 array of per-window info dictionaries, ordered FRONT-to-back. The R27
    /// guarded-HID preflight z-order check (see [`frontmost_window_owns_point`]).
    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> CfArrayRef;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
    /// `AXUIElementRef AXUIElementCreateApplication(pid_t pid)` — a +1 handle to
    /// the accessibility element of the app with `pid` (`pid_t` is `i32`).
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    /// `AXError AXUIElementCopyAttributeValue(AXUIElementRef, CFStringRef,
    /// CFTypeRef *value)` — writes a +1 value out-param and returns
    /// `kAXErrorSuccess` (0) on success.
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CfStringRef,
        value: *mut CfTypeRef,
    ) -> i32;
}

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn TISCopyCurrentKeyboardInputSource() -> TisInputSourceRef;
    fn TISCreateInputSourceList(
        properties: CfDictionaryRef,
        include_all_installed: u8,
    ) -> CfArrayRef;
    fn TISSelectInputSource(input_source: TisInputSourceRef) -> i32;
    fn TISGetInputSourceProperty(
        input_source: TisInputSourceRef,
        property_key: CfStringRef,
    ) -> *mut c_void;
    static kTISPropertyInputSourceID: CfStringRef;
}

// CoreFoundation, hand-declared (the `core_foundation_sys` graph already links
// it; these are the two array accessors + CFRelease this module uses, plus the
// two runloop pokes the T9 launch-overlay deadline needs — see `launch_deadline`).
extern "C" {
    fn CFArrayGetCount(array: CfArrayRef) -> CfIndex;
    fn CFArrayGetValueAtIndex(array: CfArrayRef, idx: CfIndex) -> *const c_void;
    fn CFRelease(cf: *const c_void);
    /// The main runloop (always the app's; linked via AppKit/CF).
    fn CFRunLoopGetMain() -> *mut c_void;
    /// Force the main runloop out of its wait so a just-enqueued wake runs NOW —
    /// immune to timer coalescing / App Nap (the harness watchdog's belt-and-
    /// suspenders wake, reused by the launch-overlay deadline).
    fn CFRunLoopWakeUp(rl: *mut c_void);
}

/// Whether this process holds the Accessibility (TCC) grant. Without it
/// `CGEventPostToPid` is a **silent no-op** — every injected keystroke is
/// dropped — so the live scenarios must gate on this and FAIL loudly (never
/// silently skip) when it is missing. Mirrors `keyinject.swift`'s preflight.
pub fn accessibility_trusted() -> bool {
    // SAFETY: `AXIsProcessTrusted` takes no arguments and is always safe to call.
    unsafe { AXIsProcessTrusted() != 0 }
}

/// The running app's `CFBundleName` (e.g. `"Nice RS Dev"` for the shipped
/// bundle), or `None` when the process is not bundled — a bare `cargo run` or
/// the test binary has no `Info.plist`. Mirrors Swift's
/// `Bundle.main.object(forInfoDictionaryKey: "CFBundleName")`: the R14
/// shell-inject per-variant `ZDOTDIR` path keys off this so `Nice RS Dev` never
/// shares a self-healing stub directory with the Swift `Nice` / `Nice Dev`
/// builds (two apps rewriting one dir with drifting stub text would fight).
pub fn main_bundle_name() -> Option<String> {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::base::{CFGetTypeID, CFTypeRef};
    use core_foundation_sys::bundle::{
        CFBundleGetMainBundle, CFBundleGetValueForInfoDictionaryKey,
    };
    use core_foundation_sys::string::{CFStringGetTypeID, CFStringRef};

    // SAFETY: every call below is a get-rule read on the process's own main
    // bundle — thread-safe, requires no run loop, takes no ownership. We confirm
    // the value is actually a CFString (via its type id) before wrapping it, and
    // hand the borrowed ref to a get-rule wrapper that does not over-release.
    unsafe {
        let bundle = CFBundleGetMainBundle();
        if bundle.is_null() {
            return None;
        }
        let key = CFString::new("CFBundleName");
        let value: CFTypeRef =
            CFBundleGetValueForInfoDictionaryKey(bundle, key.as_concrete_TypeRef());
        if value.is_null() || CFGetTypeID(value) != CFStringGetTypeID() {
            return None;
        }
        let name = CFString::wrap_under_get_rule(value as CFStringRef);
        Some(name.to_string())
    }
}

/// The running app's `CFBundleShortVersionString` (e.g. `"0.17.0"`), or `None`
/// when the process is not bundled — a bare `cargo run` or the test binary has no
/// `Info.plist`. Mirrors Swift's
/// `Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString")`; R23's
/// About pane reads it, falling back to `CARGO_PKG_VERSION`.
pub fn main_bundle_short_version() -> Option<String> {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::base::{CFGetTypeID, CFTypeRef};
    use core_foundation_sys::bundle::{
        CFBundleGetMainBundle, CFBundleGetValueForInfoDictionaryKey,
    };
    use core_foundation_sys::string::{CFStringGetTypeID, CFStringRef};

    // SAFETY: a get-rule read of the process's own main bundle (thread-safe, no run
    // loop, takes no ownership); the value's type id is checked to be a CFString
    // before it is wrapped under the get rule (no over-release).
    unsafe {
        let bundle = CFBundleGetMainBundle();
        if bundle.is_null() {
            return None;
        }
        let key = CFString::new("CFBundleShortVersionString");
        let value: CFTypeRef =
            CFBundleGetValueForInfoDictionaryKey(bundle, key.as_concrete_TypeRef());
        if value.is_null() || CFGetTypeID(value) != CFStringGetTypeID() {
            return None;
        }
        let name = CFString::wrap_under_get_rule(value as CFStringRef);
        Some(name.to_string())
    }
}

/// `kAXErrorSuccess`.
const AX_SUCCESS: i32 = 0;

/// Copy one AX attribute of `element` as a +1 `CFTypeRef`, or null when the
/// attribute is absent or the query errored. Caller owns the result (release it).
///
/// # Safety
/// `element` must be a live `AXUIElementRef`.
unsafe fn ax_copy_attr(element: AXUIElementRef, attr: &str) -> CfTypeRef {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    let key = CFString::new(attr);
    let mut value: CfTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(
        element,
        key.as_concrete_TypeRef() as CfStringRef,
        &mut value,
    );
    if err != AX_SUCCESS {
        std::ptr::null()
    } else {
        value
    }
}

/// Read a string-valued AX attribute of `element` as an owned Rust `String`.
///
/// # Safety
/// `element` must be a live `AXUIElementRef`; `attr` must name a string-valued
/// attribute (e.g. `AXRole`, `AXTitle`).
unsafe fn ax_copy_string(element: AXUIElementRef, attr: &str) -> Option<String> {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    let value = ax_copy_attr(element, attr);
    if value.is_null() {
        return None;
    }
    // `AXUIElementCopyAttributeValue` returns a +1 value; hand ownership to the
    // CFString wrapper (releases on drop) and copy it into an owned String.
    let s = CFString::wrap_under_create_rule(value as core_foundation_sys::string::CFStringRef);
    Some(s.to_string())
}

/// Depth-first search of `element`'s AX subtree for the first node whose
/// `AXTitle` equals `label`; returns that node's `AXRole`. Bounded by `depth`
/// (levels remaining) and `budget` (total nodes visited) so a malformed tree
/// can never loop unbounded.
///
/// # Safety
/// `element` must be a live `AXUIElementRef`.
unsafe fn ax_find_role_by_title(
    element: AXUIElementRef,
    label: &str,
    depth: usize,
    budget: &mut usize,
) -> Option<String> {
    if depth == 0 || *budget == 0 {
        return None;
    }
    *budget -= 1;

    // This node: match by the unique title marker, then report its role.
    if ax_copy_string(element, "AXTitle").as_deref() == Some(label) {
        return Some(ax_copy_string(element, "AXRole").unwrap_or_default());
    }

    // Recurse into AXChildren (a +1 CFArray of borrowed child AXUIElementRefs;
    // the children stay valid while the array is alive, which spans the loop).
    let children = ax_copy_attr(element, "AXChildren");
    if children.is_null() {
        return None;
    }
    let count = CFArrayGetCount(children as CfArrayRef);
    let mut found = None;
    for i in 0..count {
        let child = CFArrayGetValueAtIndex(children as CfArrayRef, i) as AXUIElementRef;
        if child.is_null() {
            continue;
        }
        if let Some(role) = ax_find_role_by_title(child, label, depth - 1, budget) {
            found = Some(role);
            break;
        }
    }
    CFRelease(children as *const c_void);
    found
}

/// Walk this process's macOS Accessibility tree and return the `AXRole` of the
/// first element whose `AXTitle` equals `label`. `Err` if no such element is
/// exposed (the AX tree never surfaced the node) or the AX query failed.
///
/// This is the query half of the `ax-probe` self-test scenario: gpui exposes an
/// element via AccessKit only when it carries both an `.id()` and a `.role()`,
/// and maps its `aria_label` to the macOS `AXTitle`; the probe sets those on one
/// stable root element and asserts this walk finds it with the expected role.
///
/// # Threading
/// MUST be called ON the gpui main thread. A *same-process* AX query is
/// dispatched inline on the calling thread (not marshaled to the main runloop
/// and awaited), so it does NOT deadlock — but AccessKit's per-view adapter
/// state is a non-`Sync` `RefCell` that gpui also borrows every frame while
/// building the tree. Querying from a background thread races that per-frame
/// borrow and panics `RefCell already borrowed`; running on the main thread
/// serializes the walk with rendering. The `ax-probe` scenario calls this from
/// its foreground task.
pub fn ax_find_titled_role(pid: i32, label: &str) -> Result<String, String> {
    // SAFETY: `AXUIElementCreateApplication` returns a +1 handle (or null); we
    // walk its subtree with owned/borrowed CF handles released per the Create/Get
    // rules inside `ax_find_role_by_title`, then release the +1 app handle.
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return Err("AXUIElementCreateApplication returned null".to_string());
        }
        let mut budget = 5000usize;
        let result = ax_find_role_by_title(app, label, 64, &mut budget);
        CFRelease(app as *const c_void);
        result.ok_or_else(|| format!("no AX element titled '{label}' found in the tree"))
    }
}

/// Depth-first search for the first `AXWindow` titled `window_title`, then within
/// THAT window's subtree return the `AXRole` of the first node titled `label`.
/// Bounded by `depth` (levels remaining) and `budget` (total nodes visited).
///
/// # Safety
/// `element` must be a live `AXUIElementRef`.
unsafe fn ax_find_role_by_title_in_window(
    element: AXUIElementRef,
    window_title: &str,
    label: &str,
    depth: usize,
    budget: &mut usize,
) -> Option<String> {
    if depth == 0 || *budget == 0 {
        return None;
    }
    *budget -= 1;

    // The target window: scope the `label` lookup to its subtree ONLY (its own
    // title is `window_title`, never `label`, so this cannot false-match itself).
    if ax_copy_string(element, "AXRole").as_deref() == Some("AXWindow")
        && ax_copy_string(element, "AXTitle").as_deref() == Some(window_title)
    {
        return ax_find_role_by_title(element, label, depth, budget);
    }

    let children = ax_copy_attr(element, "AXChildren");
    if children.is_null() {
        return None;
    }
    let count = CFArrayGetCount(children as CfArrayRef);
    let mut found = None;
    for i in 0..count {
        let child = CFArrayGetValueAtIndex(children as CfArrayRef, i) as AXUIElementRef;
        if child.is_null() {
            continue;
        }
        if let Some(role) =
            ax_find_role_by_title_in_window(child, window_title, label, depth - 1, budget)
        {
            found = Some(role);
            break;
        }
    }
    CFRelease(children as *const c_void);
    found
}

/// Like [`ax_find_titled_role`], but the `label` search is scoped to the subtree
/// of the first `AXWindow` titled `window_title` — so a `label`-titled node in
/// ANOTHER of the process's windows or in a lingering menu cannot be mistaken for
/// the one in the window under test (the serial self-test-suite hazard: the same
/// process hosts many scenarios' windows). Same main-thread threading contract as
/// [`ax_find_titled_role`].
pub fn ax_find_titled_role_in_window(
    pid: i32,
    window_title: &str,
    label: &str,
) -> Result<String, String> {
    // SAFETY: `AXUIElementCreateApplication` returns a +1 handle (or null); the walk
    // releases every owned/borrowed CF handle per the Create/Get rules, then the
    // +1 app handle.
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return Err("AXUIElementCreateApplication returned null".to_string());
        }
        let mut budget = 5000usize;
        let result = ax_find_role_by_title_in_window(app, window_title, label, 64, &mut budget);
        CFRelease(app as *const c_void);
        result.ok_or_else(|| {
            format!("no AX element titled '{label}' found in the '{window_title}' window subtree")
        })
    }
}

/// Post one real key event (down or up) to `pid` via `CGEventPostToPid`.
///
/// `keycode` is a macOS virtual key (`CGKeyCode`); `flags` is a bitwise-or of the
/// `FLAG_*` masks (0 for none). When `unicode` is `Some`, it overrides the
/// event's inserted characters so the keystroke is keyboard-layout independent
/// (as `keyinject.swift` does) — pass it for printables, omit it for functional
/// keys (arrows/F-keys), whose meaning comes from the keycode alone.
fn post_key_event(pid: i32, keycode: u16, key_down: bool, flags: u64, unicode: Option<&str>) {
    // SAFETY: `CGEventCreateKeyboardEvent(nil, …)` returns a +1 event (or null on
    // failure, guarded). We attach an optional unicode string (its buffer lives
    // until the call returns), set flags, post to one pid, then release the +1.
    unsafe {
        let event = CGEventCreateKeyboardEvent(std::ptr::null_mut(), keycode, key_down);
        if event.is_null() {
            return;
        }
        if let Some(s) = unicode {
            let utf16: Vec<u16> = s.encode_utf16().collect();
            CGEventKeyboardSetUnicodeString(event, utf16.len(), utf16.as_ptr());
        }
        if flags != 0 {
            CGEventSetFlags(event, flags);
        }
        CGEventPostToPid(pid, event);
        CFRelease(event as *const c_void);
    }
}

/// Post a real keyDown+keyUp pair to `pid` (the standard "tap this key" gesture).
/// See [`post_key_event`] for `keycode` / `flags` / `unicode`. A held modifier is
/// expressed by carrying its `FLAG_*` bit on both the down and the up event.
pub fn post_key_tap(pid: i32, keycode: u16, flags: u64, unicode: Option<&str>) {
    post_key_event(pid, keycode, true, flags, unicode);
    post_key_event(pid, keycode, false, flags, unicode);
}

/// A saved keyboard input source, retained so the IME probe can restore the
/// user's original layout afterwards. Restoration is **mandatory and always
/// happens** — it runs on `Drop`, so it fires even if the probe returns early or
/// panics mid-flight (the plan's "restore … always — even on failure"). Holds a
/// +1 reference released after the restore.
pub struct SavedInputSource(TisInputSourceRef);

impl SavedInputSource {
    /// Re-select this input source (the user's original) now, best-effort. Also
    /// runs automatically on drop; calling it early is idempotent.
    pub fn restore(&self) {
        // SAFETY: `self.0` is a live +1 input-source ref held for our lifetime.
        unsafe {
            let _ = TISSelectInputSource(self.0);
        }
    }
}

impl Drop for SavedInputSource {
    fn drop(&mut self) {
        // Restore the user's original source, THEN release the +1 ref — so a
        // dropped `SavedInputSource` can never leave the machine stuck on Pinyin.
        self.restore();
        // SAFETY: `self.0` is the +1 ref from `TISCopyCurrentKeyboardInputSource`.
        unsafe { CFRelease(self.0 as *const c_void) }
    }
}

/// Snapshot the current keyboard input source (to restore later). `None` if the
/// system reports none.
pub fn current_input_source() -> Option<SavedInputSource> {
    // SAFETY: `TISCopyCurrentKeyboardInputSource` returns a +1 ref (Copy rule) or
    // null; `SavedInputSource` owns and releases it.
    unsafe {
        let src = TISCopyCurrentKeyboardInputSource();
        if src.is_null() {
            None
        } else {
            Some(SavedInputSource(src))
        }
    }
}

/// The `kTISPropertyInputSourceID` of one input source as a Rust `String`, or
/// `None` if the property is absent.
///
/// # Safety
/// `src` must be a valid `TISInputSourceRef`.
unsafe fn input_source_id(src: TisInputSourceRef) -> Option<String> {
    let ptr = TISGetInputSourceProperty(src, kTISPropertyInputSourceID);
    if ptr.is_null() {
        return None;
    }
    // The property is a get-rule CFStringRef (do NOT release it); copy it into an
    // owned Rust String via core-foundation's wrapper.
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    let cfstr = CFString::wrap_under_get_rule(ptr as core_foundation_sys::string::CFStringRef);
    Some(cfstr.to_string())
}

/// Every installed keyboard input source's ID (diagnostic for the IME probe's
/// record — proves the TIS enumeration actually ran and shows what was available
/// when no Pinyin source could be selected).
pub fn input_source_ids() -> Vec<String> {
    // SAFETY: same enumeration contract as `select_pinyin_input_source`.
    unsafe {
        let list = TISCreateInputSourceList(std::ptr::null(), 1);
        if list.is_null() {
            return Vec::new();
        }
        let count = CFArrayGetCount(list);
        let mut ids = Vec::new();
        for i in 0..count {
            let src = CFArrayGetValueAtIndex(list, i) as TisInputSourceRef;
            if !src.is_null() {
                if let Some(id) = input_source_id(src) {
                    ids.push(id);
                }
            }
        }
        CFRelease(list);
        ids
    }
}

/// Try to select a Pinyin (Simplified Chinese) keyboard input source for the IME
/// go/no-go probe. Returns the selected source's ID on success, or `None` if no
/// Pinyin source is installed/enabled or the selection was refused.
///
/// Matches the well-known Pinyin input-source IDs (`…SCIM.ITABC` simplified
/// Pinyin, Shuangpin, traditional Pinyin) by substring, preferring the plain
/// simplified-Pinyin source. The caller MUST restore the saved source afterwards.
pub fn select_pinyin_input_source() -> Option<String> {
    // SAFETY: `TISCreateInputSourceList(nil, true)` returns a +1 CFArray (or null,
    // guarded) of all installed sources; each element is a get-rule
    // TISInputSourceRef we do not release. We CFRelease the array when done.
    unsafe {
        let list = TISCreateInputSourceList(std::ptr::null(), 1);
        if list.is_null() {
            return None;
        }
        let count = CFArrayGetCount(list);
        // First pass: exact simplified-Pinyin (ITABC); second: any Pinyin/SCIM.
        let mut chosen: Option<String> = None;
        for want_exact in [true, false] {
            for i in 0..count {
                let src = CFArrayGetValueAtIndex(list, i) as TisInputSourceRef;
                if src.is_null() {
                    continue;
                }
                let Some(id) = input_source_id(src) else {
                    continue;
                };
                let lid = id.to_ascii_lowercase();
                let is_pinyin = if want_exact {
                    lid.contains("itabc")
                } else {
                    lid.contains("pinyin") || lid.contains("scim")
                };
                if is_pinyin && TISSelectInputSource(src) == 0 {
                    chosen = Some(id);
                    break;
                }
            }
            if chosen.is_some() {
                break;
            }
        }
        CFRelease(list);
        chosen
    }
}

/// A future that resolves after a fixed delay via the **spike-6 App-Nap-safe**
/// mechanism the T9 launch-overlay grace deadline needs.
///
/// Why not `background_executor().timer`: macOS App Nap indefinitely defers
/// coalescable libdispatch timers on an idle/occluded app (the spike observed a
/// 60 s deadline not firing within 8 minutes). The overlay-worthy case is a
/// *silent* pane — no output, no events — which is exactly the idle condition
/// that lets App Nap kick in, so the deadline cannot ride a coalescable timer.
///
/// The mechanism (the harness watchdog pattern): a **dedicated OS thread** sleeps
/// to the deadline — a `nanosleep` wakeup is scheduler-level, NOT a coalescable
/// timer — then wakes the awaiting foreground task's `Waker` AND force-wakes the
/// main CFRunLoop, so gpui's foreground executor polls this future to completion
/// even under App Nap.
struct AppNapSafeDelay {
    delay: Duration,
    done: Arc<AtomicBool>,
    /// The timer thread is spawned on the first poll (when a real `Waker` exists),
    /// then this latches so a re-poll never spawns a second thread.
    armed: bool,
}

impl Future for AppNapSafeDelay {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<()> {
        // `AppNapSafeDelay` is `Unpin` (Duration + Arc + bool), so a plain `&mut`
        // is sound to take out of the pin.
        let this = self.get_mut();
        if this.done.load(Ordering::Acquire) {
            return Poll::Ready(());
        }
        if !this.armed {
            this.armed = true;
            let done = Arc::clone(&this.done);
            let waker = cx.waker().clone();
            let delay = this.delay;
            // Best-effort: if the thread cannot spawn, the overlay simply never
            // promotes (no worse than the pre-T9 behaviour). It is a one-shot,
            // short-lived thread per launch.
            let _ = std::thread::Builder::new()
                .name("nice-rs-launch-deadline".into())
                .spawn(move || {
                    // Scheduler-level sleep — immune to libdispatch timer coalescing.
                    std::thread::sleep(delay);
                    done.store(true, Ordering::Release);
                    // Wake the awaiting task, then force the main runloop out of its
                    // wait so the foreground executor re-polls us even if napped.
                    waker.wake();
                    // SAFETY: `CFRunLoopGetMain` returns the app's main runloop (or,
                    // implausibly, null, which `CFRunLoopWakeUp` tolerates as a
                    // no-op); both take no ownership.
                    unsafe {
                        CFRunLoopWakeUp(CFRunLoopGetMain());
                    }
                });
        }
        Poll::Pending
    }
}

/// The App-Nap-safe launch-overlay grace-deadline factory (T9) injected into every
/// [`TerminalView`](nice_term_view::TerminalView) via `set_launch_deadline`.
/// Given the grace `Duration`, it hands back a future that resolves after that
/// delay through [`AppNapSafeDelay`] (dedicated OS-thread sleep + main-runloop
/// wake). Keeping this the sole foreign-code home lets `nice-term-view` stay free
/// of CF/objc2 — it only awaits the returned future.
pub fn launch_deadline() -> nice_term_view::LaunchDeadline {
    Arc::new(|delay: Duration| -> nice_term_view::LaunchDeadlineFuture {
        // The concrete future unsize-coerces to the boxed trait object at return.
        Box::pin(AppNapSafeDelay {
            delay,
            done: Arc::new(AtomicBool::new(false)),
            armed: false,
        })
    })
}

/// Force the app's main CFRunLoop out of its wait so a just-enqueued foreground
/// wake runs *now*, immune to timer coalescing / App Nap — the App-Nap-safe
/// belt-and-suspenders the R14 control-socket foreground drain
/// (`crate::control_socket::SocketSender::post`) fires after every enqueue,
/// mirroring [`AppNapSafeDelay`]. Safe to call from any thread. Production
/// consumer lands with the R14 env-injection slice's drain wiring, hence the
/// `dead_code` allow.
#[allow(dead_code)]
pub fn wake_main_runloop() {
    // SAFETY: `CFRunLoopGetMain` returns the app's main runloop (or, implausibly,
    // null, which `CFRunLoopWakeUp` tolerates as a no-op); neither takes ownership.
    unsafe {
        CFRunLoopWakeUp(CFRunLoopGetMain());
    }
}

// ===========================================================================
// R9 chrome live validation — synthetic mouse CGEvents, NSWindow frame/state
// reads, the content→screen coordinate mapping, and the
// `AppleActionOnDoubleClick` preference read.
//
// This is the foreign side of the `chrome` self-test scenario
// (`crate::chrome_live`), the R9 sibling of the R5 CGEvent input block above: it
// posts synthetic LEFT-mouse events (down / dragged / up, with a click-state
// field so a double-click reaches gpui) to nice-rs's OWN pid via
// `CGEventPostToPid` — never the global HID tap — so the scenario can assert the
// real drag / double-click behavior of the chrome band, and reads the live
// NSWindow frame + zoom/miniaturize state to ground-truth what those gestures
// did. It has no place in the shipped app (real input arrives through gpui); the
// all-Rust rule keeps every foreign C crossing in this one module.
//
// Coordinate spaces: gpui/content-view points are top-left origin, y down. Cocoa
// screen points are bottom-left origin, y up. CGEvent "global display" points are
// top-left origin, y down, relative to the main (menu-bar) display. The helpers
// convert between them explicitly.
// ===========================================================================

// CoreGraphics mouse-event types + fields (`CGEventType` / `CGEventField` /
// `CGMouseButton`). Only the left-button subset the scenario drives.
const CG_EVENT_LEFT_MOUSE_DOWN: u32 = 1;
const CG_EVENT_LEFT_MOUSE_UP: u32 = 2;
const CG_EVENT_LEFT_MOUSE_DRAGGED: u32 = 6;
const CG_MOUSE_BUTTON_LEFT: u32 = 0;
/// `kCGMouseEventClickState` — the field that carries the click count (2 = a
/// double-click), read back by AppKit as `NSEvent.clickCount`.
const CG_MOUSE_EVENT_CLICK_STATE: u32 = 1;

// The CGEvent cursor location and the AppKit point conversions below both use
// `NSPoint` (== `CGPoint`, two `CGFloat`/`f64`): it is `repr(C)` for the extern
// CGEvent call AND implements objc2's `Encode` for the `msg_send!` conversions.

/// `kCGHIDEventTap` — the global hardware-input event tap. Used ONLY by the R27
/// carve-out seams below (`post_global_left_click` / `post_global_left_drag`); the
/// pid-posted family above never touches it.
const CG_HID_EVENT_TAP: u32 = 0;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreateMouseEvent(
        source: CGEventSourceRef,
        mouse_type: u32,
        cursor: NSPoint,
        button: u32,
    ) -> CGEventRef;
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    /// Post to the GLOBAL HID tap (not a pid). R27 carve-out ONLY — see the
    /// SAFETY INVARIANT amendment above and the two seams below.
    fn CGEventPost(tap: u32, event: CGEventRef);
}

/// Post one synthetic left-mouse event of `mouse_type` at CG-global point
/// `(x, y)` to `pid`, stamping `click_count` (>=1) into the click-state field so
/// a value of 2 reaches gpui as a double-click. Same one-pid safety invariant as
/// the R5 keyboard block: `CGEventPostToPid`, never the global HID tap.
fn post_mouse_event(pid: i32, mouse_type: u32, x: f64, y: f64, click_count: i64) {
    // SAFETY: `CGEventCreateMouseEvent(nil, …)` returns a +1 event (or null,
    // guarded); we optionally set its click-state field, post it to one pid, then
    // release the +1.
    unsafe {
        let event = CGEventCreateMouseEvent(
            std::ptr::null_mut(),
            mouse_type,
            NSPoint { x, y },
            CG_MOUSE_BUTTON_LEFT,
        );
        if event.is_null() {
            return;
        }
        if click_count > 1 {
            CGEventSetIntegerValueField(event, CG_MOUSE_EVENT_CLICK_STATE, click_count);
        }
        CGEventPostToPid(pid, event);
        CFRelease(event as *const c_void);
    }
}

/// Post a synthetic left-mouse-DOWN at CG-global `(x, y)` to `pid`. `click_count`
/// (clamped to >=1) becomes `NSEvent.clickCount` — pass 2 for a double-click.
pub fn post_left_mouse_down(pid: i32, x: f64, y: f64, click_count: i64) {
    post_mouse_event(pid, CG_EVENT_LEFT_MOUSE_DOWN, x, y, click_count.max(1));
}

/// Post a synthetic left-mouse-DRAGGED at CG-global `(x, y)` to `pid`.
pub fn post_left_mouse_dragged(pid: i32, x: f64, y: f64) {
    post_mouse_event(pid, CG_EVENT_LEFT_MOUSE_DRAGGED, x, y, 1);
}

/// Post a synthetic left-mouse-UP at CG-global `(x, y)` to `pid`.
pub fn post_left_mouse_up(pid: i32, x: f64, y: f64, click_count: i64) {
    post_mouse_event(pid, CG_EVENT_LEFT_MOUSE_UP, x, y, click_count.max(1));
}

/// Post one synthetic left-mouse event of `mouse_type` at CG-global `(x, y)` to
/// the GLOBAL HID tap (`CGEventPost(kCGHIDEventTap, …)`), stamping `click_count`
/// (>=1) into the click-state field. The R27 carve-out helper — see
/// [`post_global_left_click`] / [`post_global_left_drag`] and the SAFETY INVARIANT
/// amendment above.
fn post_global_mouse_event(mouse_type: u32, x: f64, y: f64, click_count: i64) {
    // SAFETY: `CGEventCreateMouseEvent(nil, …)` returns a +1 event (or null,
    // guarded); we optionally set its click-state field, post it to the global
    // HID tap, then release the +1.
    unsafe {
        let event = CGEventCreateMouseEvent(
            std::ptr::null_mut(),
            mouse_type,
            NSPoint { x, y },
            CG_MOUSE_BUTTON_LEFT,
        );
        if event.is_null() {
            return;
        }
        if click_count > 1 {
            CGEventSetIntegerValueField(event, CG_MOUSE_EVENT_CLICK_STATE, click_count);
        }
        CGEventPost(CG_HID_EVENT_TAP, event);
        CFRelease(event as *const c_void);
    }
}

/// **R27 carve-out (SELFTEST/SCENARIO ONLY).** Post a synthetic left-mouse
/// DOWN+UP click at CG-global `(x, y)` via the GLOBAL HID tap — NOT
/// `CGEventPostToPid` — because pid-posted mouse events silently drop (hover
/// paints, `mouseDown` never fires — the M6 record). `click_state` (>=1) becomes
/// `NSEvent.clickCount`. The ONLY safe caller is the R27 §6 close-out composition
/// leg, which MUST run its preflight FIRST (activate + raise +
/// `CGWindowListCopyWindowInfo` frontmost-at-point) and DEFER LOUDLY when our
/// window does not own the point — never a blind post. See the SAFETY INVARIANT
/// amendment above. Not yet called until the §6 leg lands (slice 4).
#[allow(dead_code)]
pub fn post_global_left_click(x: f64, y: f64, click_state: i64) {
    let clicks = click_state.max(1);
    post_global_mouse_event(CG_EVENT_LEFT_MOUSE_DOWN, x, y, clicks);
    post_global_mouse_event(CG_EVENT_LEFT_MOUSE_UP, x, y, clicks);
}

/// **R27 carve-out (SELFTEST/SCENARIO ONLY).** Post a synthetic left-mouse
/// DRAGGED at CG-global `(x, y)` via the GLOBAL HID tap. Same carve-out
/// discipline as [`post_global_left_click`]: the caller sequences a
/// [`post_global_left_down`], a run of these drag steps, and a
/// [`post_global_left_up`], each behind the §6 preflight.
#[allow(dead_code)]
pub fn post_global_left_drag(x: f64, y: f64) {
    post_global_mouse_event(CG_EVENT_LEFT_MOUSE_DRAGGED, x, y, 1);
}

/// **R27 carve-out (SELFTEST/SCENARIO ONLY).** Post a synthetic left-mouse DOWN
/// (no matching UP) at CG-global `(x, y)` via the GLOBAL HID tap — the start of a
/// held drag ([`post_global_left_drag`] steps then [`post_global_left_up`]). Unlike
/// [`post_global_left_click`], which fires DOWN+UP together (a tap), this leaves the
/// button HELD so the drag steps track. Same carve-out discipline: the caller MUST
/// run the §6 preflight (activate + raise + frontmost-at-point) BEFORE it and DEFER
/// LOUDLY when our window does not own the point — never a blind post.
#[allow(dead_code)]
pub fn post_global_left_down(x: f64, y: f64) {
    post_global_mouse_event(CG_EVENT_LEFT_MOUSE_DOWN, x, y, 1);
}

/// **R27 carve-out (SELFTEST/SCENARIO ONLY).** Post a synthetic left-mouse UP at
/// CG-global `(x, y)` via the GLOBAL HID tap — the release ending a held drag begun
/// with [`post_global_left_down`]. Same carve-out discipline as
/// [`post_global_left_down`].
#[allow(dead_code)]
pub fn post_global_left_up(x: f64, y: f64) {
    post_global_mouse_event(CG_EVENT_LEFT_MOUSE_UP, x, y, 1);
}

/// `CGWindowListOption` bits: on-screen windows only, desktop elements excluded.
const CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1 << 0;
const CG_WINDOW_LIST_EXCLUDE_DESKTOP: u32 = 1 << 4;
/// `kCGNullWindowID` — the "relative to no window" sentinel.
const CG_NULL_WINDOW_ID: u32 = 0;

/// Read a CF-dictionary entry (by string key) as a raw `CFTypeRef`, borrowed from
/// the dictionary (get-rule — do NOT release). `None` when the key is absent.
///
/// # Safety
/// `dict` must be a live `CFDictionaryRef`.
unsafe fn cf_dict_value(dict: *const c_void, key: &str) -> Option<*const c_void> {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::dictionary::{CFDictionaryGetValueIfPresent, CFDictionaryRef};

    let cf_key = CFString::new(key);
    let mut value: *const c_void = std::ptr::null();
    let present = CFDictionaryGetValueIfPresent(
        dict as CFDictionaryRef,
        cf_key.as_concrete_TypeRef() as *const c_void,
        &mut value as *mut *const c_void,
    );
    if present == 0 || value.is_null() {
        None
    } else {
        Some(value)
    }
}

/// Read a `CFNumber`-valued dictionary entry as `f64`. `None` if absent / not a
/// number.
///
/// # Safety
/// `dict` must be a live `CFDictionaryRef`.
unsafe fn cf_dict_f64(dict: *const c_void, key: &str) -> Option<f64> {
    use core_foundation_sys::number::{kCFNumberFloat64Type, CFNumberGetValue, CFNumberRef};

    let value = cf_dict_value(dict, key)?;
    let mut out: f64 = 0.0;
    let ok = CFNumberGetValue(
        value as CFNumberRef,
        kCFNumberFloat64Type,
        &mut out as *mut f64 as *mut c_void,
    );
    if ok {
        Some(out)
    } else {
        None
    }
}

/// Read a `CFNumber`-valued dictionary entry as `i64`. `None` if absent / not a
/// number.
///
/// # Safety
/// `dict` must be a live `CFDictionaryRef`.
unsafe fn cf_dict_i64(dict: *const c_void, key: &str) -> Option<i64> {
    use core_foundation_sys::number::{kCFNumberSInt64Type, CFNumberGetValue, CFNumberRef};

    let value = cf_dict_value(dict, key)?;
    let mut out: i64 = 0;
    let ok = CFNumberGetValue(
        value as CFNumberRef,
        kCFNumberSInt64Type,
        &mut out as *mut i64 as *mut c_void,
    );
    if ok {
        Some(out)
    } else {
        None
    }
}

/// **R27 guarded-HID preflight (SELFTEST/SCENARIO ONLY).** Whether the topmost
/// normal, visible window covering CG-global `(x, y)` belongs to THIS process —
/// the z-order half of the mandatory preflight before any [`post_global_left_click`]
/// / [`post_global_left_drag`] (the SAFETY INVARIANT carve-out). Walks
/// `CGWindowListCopyWindowInfo`'s front-to-back list, skips the menu bar / dock /
/// status overlays (window layer `!= 0`) and fully-transparent windows, and
/// returns whether the FIRST normal window whose bounds contain the point is
/// ours. `false` — the honest default — whenever another app's window is on top
/// at that point, no window covers it, or the query fails; the caller then DEFERS
/// LOUDLY and does NOT post, so an unattended `NICE_RS_SELFTEST=all` run can never
/// send a click into another app. The caller must ALSO activate + raise our window
/// first (this is only the z-order check).
#[allow(dead_code)]
pub fn frontmost_window_owns_point(x: f64, y: f64) -> bool {
    // SAFETY: `CGWindowListCopyWindowInfo` returns a +1 CFArray (or null, guarded)
    // of borrowed CFDictionary entries; we read borrowed CFNumber/CFDictionary
    // values (get-rule, not released) and release the +1 array at the end.
    unsafe {
        let info = CGWindowListCopyWindowInfo(
            CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY | CG_WINDOW_LIST_EXCLUDE_DESKTOP,
            CG_NULL_WINDOW_ID,
        );
        if info.is_null() {
            return false;
        }
        let our_pid = std::process::id() as i64;
        let count = CFArrayGetCount(info);
        let mut owns = false;
        for i in 0..count {
            let dict = CFArrayGetValueAtIndex(info, i);
            if dict.is_null() {
                continue;
            }
            // Only normal windows decide ownership; skip overlays (layer != 0) and
            // invisible windows (alpha 0).
            if cf_dict_i64(dict, "kCGWindowLayer").unwrap_or(i64::MAX) != 0 {
                continue;
            }
            if cf_dict_f64(dict, "kCGWindowAlpha").unwrap_or(0.0) <= 0.0 {
                continue;
            }
            let Some(bounds) = cf_dict_value(dict, "kCGWindowBounds") else {
                continue;
            };
            let (Some(bx), Some(by), Some(bw), Some(bh)) = (
                cf_dict_f64(bounds, "X"),
                cf_dict_f64(bounds, "Y"),
                cf_dict_f64(bounds, "Width"),
                cf_dict_f64(bounds, "Height"),
            ) else {
                continue;
            };
            let inside = x >= bx && x < bx + bw && y >= by && y < by + bh;
            if !inside {
                continue;
            }
            // The first (topmost) normal, visible window covering the point decides
            // ownership — if it is not ours, another app is on top here.
            owns = cf_dict_i64(dict, "kCGWindowOwnerPID").unwrap_or(-1) == our_pid;
            break;
        }
        CFRelease(info as *const c_void);
        owns
    }
}

/// The window's `frame` in Cocoa screen points (bottom-left origin, y up):
/// `[x, y, width, height]`. `None` if the window has no AppKit handle yet.
pub fn window_screen_frame(window: &Window) -> Option<[f64; 4]> {
    let ns_view = ns_view_of(window);
    if ns_view.is_null() {
        return None;
    }
    // SAFETY: `ns_view` is this window's live content `NSView`; `-window`/`-frame`
    // are get-rule reads with no ownership transfer.
    unsafe {
        let view = ns_view as *mut AnyObject;
        let ns_window: *mut AnyObject = msg_send![view, window];
        if ns_window.is_null() {
            return None;
        }
        let frame: NSRect = msg_send![ns_window, frame];
        Some([
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        ])
    }
}

/// Whether the window is currently zoomed (`-[NSWindow isZoomed]`). `None` if the
/// window has no AppKit handle.
pub fn window_is_zoomed(window: &Window) -> Option<bool> {
    let ns_window = ns_window_of(window)?;
    // SAFETY: `-isZoomed` is a no-arg get-rule `-> BOOL` accessor on the live
    // NSWindow.
    unsafe {
        let value: Bool = msg_send![ns_window, isZoomed];
        Some(value.as_bool())
    }
}

/// Whether the window is currently miniaturized (`-[NSWindow isMiniaturized]`).
pub fn window_is_miniaturized(window: &Window) -> Option<bool> {
    let ns_window = ns_window_of(window)?;
    // SAFETY: `-isMiniaturized` is a no-arg get-rule `-> BOOL` accessor on the
    // live NSWindow.
    unsafe {
        let value: Bool = msg_send![ns_window, isMiniaturized];
        Some(value.as_bool())
    }
}

/// The window's backing `NSWindow` pointer via its content `NSView`, or `None` if
/// the window has no AppKit handle yet.
fn ns_window_of(window: &Window) -> Option<*mut AnyObject> {
    let ns_view = ns_view_of(window);
    if ns_view.is_null() {
        return None;
    }
    // SAFETY: `ns_view` is this window's live content `NSView`; `-window` is a
    // get-rule read returning its enclosing NSWindow (or nil, guarded).
    unsafe {
        let view = ns_view as *mut AnyObject;
        let ns_window: *mut AnyObject = msg_send![view, window];
        if ns_window.is_null() {
            None
        } else {
            Some(ns_window)
        }
    }
}

/// Perform the standard window-close action (`-[NSWindow performClose:]`) — the
/// EXACT action the red traffic-light close button's target invokes. It routes
/// through the window delegate's `windowShouldClose:` (gpui's
/// `on_window_should_close` gate) and only closes if it returns `true`, so with
/// live panes it presents the W5 veto modal and the window stays open — WITHOUT
/// invoking the should-close closure directly (AppKit invokes it via the
/// delegate). The `persistence-restore` scenario uses this to drive the veto: a
/// synthetic CGEvent click on the native traffic-light button does not
/// hit-test to it under gpui's full-size-content-view window (verified
/// on-device), so `performClose:` is the real close-button action driven through
/// the same delegate path.
pub fn perform_window_close(window: &Window) {
    perform_window_close_ptr(ns_view_of(window));
}

/// [`perform_window_close`] from a raw content-`NSView` pointer (captured earlier
/// via [`ns_view_of`]). Because `-performClose:` SYNCHRONOUSLY invokes
/// `windowShouldClose:` — which re-enters gpui to present the veto modal — it MUST
/// be called with NO outstanding gpui `App`/`Window` borrow (from the scenario's
/// task, never inside a `window.update` closure), exactly like the CGEvent posts
/// and the present-kick. Calling it inside an update borrow panics gpui's
/// async-context (the borrow-reentrancy warning at `platform.rs`).
pub fn perform_window_close_ptr(ns_view: *mut c_void) {
    if ns_view.is_null() {
        return;
    }
    // SAFETY: `ns_view` is a live content `NSView`; `-window` is a get-rule read,
    // `-performClose:` a main-thread action with a valid `nil` sender.
    unsafe {
        let view = ns_view as *mut AnyObject;
        let ns_window: *mut AnyObject = msg_send![view, window];
        if ns_window.is_null() {
            return;
        }
        let _: () = msg_send![ns_window, performClose: std::ptr::null_mut::<AnyObject>()];
    }
}

/// De-miniaturize the window (`-[NSWindow deminiaturize:]`), so the scenario can
/// recover after a double-click whose `AppleActionOnDoubleClick` is "Minimize".
pub fn deminiaturize_window(window: &Window) {
    let ns_view = ns_view_of(window);
    if ns_view.is_null() {
        return;
    }
    // SAFETY: `-deminiaturize:` on the live NSWindow; `nil` sender is valid.
    unsafe {
        let view = ns_view as *mut AnyObject;
        let ns_window: *mut AnyObject = msg_send![view, window];
        if ns_window.is_null() {
            return;
        }
        let _: () = msg_send![ns_window, deminiaturize: std::ptr::null_mut::<AnyObject>()];
    }
}

/// Set the window's `frame` (Cocoa screen points, `[x, y, width, height]`) via
/// `setFrame:display:` (no animation) — used to RESTORE the window to a known
/// geometry after a gesture that moved / zoomed / miniaturized it, so later
/// scenario steps run against a stable frame.
pub fn set_window_frame(window: &Window, frame: [f64; 4]) {
    let Some(ns_window) = ns_window_of(window) else {
        return;
    };
    // SAFETY: `setFrame:display:` on the live NSWindow with an owned rect; no
    // ownership transfer.
    unsafe {
        let rect = NSRect {
            origin: NSPoint {
                x: frame[0],
                y: frame[1],
            },
            size: NSSize {
                width: frame[2],
                height: frame[3],
            },
        };
        let _: () = msg_send![ns_window, setFrame: rect, display: true];
    }
}

/// Resize the window's frame by `(dw, dh)` points via AppKit `setFrame:display:`
/// (no animation), leaving the origin fixed. Used only to FIRE gpui's resize
/// handler for the BUG-B "traffic lights survive a resize" re-assert; pair with a
/// restoring call (`-dw, -dh`).
///
/// NOTE (verified on this pin): calling this from INSIDE a `window.update`
/// closure resizes the NSWindow, but gpui never processes the new viewport —
/// the synchronous AppKit resize callback cannot re-enter the already-borrowed
/// App, and the notification is dropped, so gpui keeps laying out at the stale
/// size (the chrome BUG-B re-assert doesn't care: its traffic-light re-apply is
/// platform-side). A scenario that needs gpui to SEE the resize (the M2 Item E
/// refit check) must capture the view pointer first and call
/// [`resize_window_ptr_by`] from its task with no App borrow outstanding — the
/// same pattern as the CGEvent posts.
pub fn resize_window_by(window: &Window, dw: f64, dh: f64) {
    resize_window_ptr_by(ns_view_of(window) as usize, dw, dh);
}

/// [`resize_window_by`] over a raw content-`NSView` pointer (captured via
/// [`ns_view_of`] inside an earlier `window.update`), callable from a scenario
/// task with NO gpui App borrow outstanding so the synchronous AppKit resize
/// callback can re-enter gpui and update its viewport. Main-thread only; the
/// pointer must belong to a still-open window.
pub fn resize_window_ptr_by(ns_view: usize, dw: f64, dh: f64) {
    let ns_view = ns_view as *mut c_void;
    if ns_view.is_null() {
        return;
    }
    // SAFETY: read the live frame, widen/heighten it, and set it back with no
    // animation; all main-thread AppKit calls with no ownership transfer.
    unsafe {
        let view = ns_view as *mut AnyObject;
        let ns_window: *mut AnyObject = msg_send![view, window];
        if ns_window.is_null() {
            return;
        }
        let mut frame: NSRect = msg_send![ns_window, frame];
        frame.size.width += dw;
        frame.size.height += dh;
        let _: () = msg_send![ns_window, setFrame: frame, display: true];
    }
}

/// Resign this app's active status (`-[NSApplication deactivate]`) so the key
/// window resigns key. Paired with a later `cx.activate(true)`, this is the
/// focus BOUNCE the BUG-B re-assert needs (gpui re-applies the traffic-light
/// position on the key-state change).
pub fn deactivate_app() {
    // SAFETY: `NSApplication.sharedApplication` is a live singleton on the main
    // thread once running; `-deactivate` takes no arguments.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return;
        }
        let _: () = msg_send![app, deactivate];
    }
}

/// Map a content-view point `(cx, cy_from_top)` (top-left origin, y down — the
/// gpui coordinate space `standard_window_button_frames` reports in) to a
/// CG-global point (top-left of the main display, y down) suitable for
/// `post_left_mouse_*`. `None` if the window / content view / main screen is
/// unavailable.
pub fn content_point_to_cg_global(window: &Window, cx: f64, cy_from_top: f64) -> Option<(f64, f64)> {
    let ns_view = ns_view_of(window);
    if ns_view.is_null() {
        return None;
    }
    let main_h = main_screen_height()?;
    // SAFETY: all AppKit point/geometry conversions on the live window +
    // content view; get-rule reads, main thread. `convertPoint:toView: nil`
    // yields window-base coords; `convertPointToScreen:` (10.12+) yields Cocoa
    // screen coords.
    unsafe {
        let view = ns_view as *mut AnyObject;
        let ns_window: *mut AnyObject = msg_send![view, window];
        if ns_window.is_null() {
            return None;
        }
        let content_view: *mut AnyObject = msg_send![ns_window, contentView];
        if content_view.is_null() {
            return None;
        }
        let bounds: NSRect = msg_send![content_view, bounds];
        let flipped: Bool = msg_send![content_view, isFlipped];
        // Content-view point: if the view is not flipped (gpui's default), y grows
        // up from the bottom, so mirror the top-down input.
        let view_y = if flipped.as_bool() {
            cy_from_top
        } else {
            bounds.size.height - cy_from_top
        };
        let view_pt = NSPoint { x: cx, y: view_y };
        let win_pt: NSPoint =
            msg_send![content_view, convertPoint: view_pt, toView: std::ptr::null_mut::<AnyObject>()];
        let screen_pt: NSPoint = msg_send![ns_window, convertPointToScreen: win_pt];
        // Cocoa screen (bottom-left, y up) → CG global (top-left, y down).
        Some((screen_pt.x, main_h - screen_pt.y))
    }
}

/// The main (menu-bar) display's height in points — the flip reference between
/// Cocoa screen coords and CG-global coords. `NSScreen.screens[0]` is the screen
/// whose origin is `(0,0)` in Cocoa space.
fn main_screen_height() -> Option<f64> {
    // SAFETY: `+[NSScreen screens]` returns an autoreleased array (get-rule); we
    // read element 0's `-frame` height. Main thread, autorelease pool active.
    unsafe {
        let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
        if screens.is_null() {
            return None;
        }
        let count: usize = msg_send![screens, count];
        if count == 0 {
            return None;
        }
        let screen: *mut AnyObject = msg_send![screens, objectAtIndex: 0usize];
        if screen.is_null() {
            return None;
        }
        let frame: NSRect = msg_send![screen, frame];
        Some(frame.size.height)
    }
}

/// The user's title-bar double-click action, read the SAME way gpui's
/// `titlebar_double_click` does (`NSGlobalDomain` persistent domain, key
/// `AppleActionOnDoubleClick`), so the `chrome` scenario can predict the effect
/// of a double-click. Read-only — the scenario NEVER writes this (it is the
/// user's real preference).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoubleClickAction {
    /// "Do Nothing".
    None,
    /// "Minimize".
    Minimize,
    /// "Maximize" / "Fill" / unset / anything else → gpui zooms (its default arm).
    Zoom,
}

/// Read `AppleActionOnDoubleClick` from `NSGlobalDomain`, mapping it exactly like
/// gpui (`gpui_macos/src/window.rs:1777-1794`): "None" → do nothing, "Minimize" →
/// miniaturize, everything else (including unset) → zoom.
pub fn apple_action_on_double_click() -> DoubleClickAction {
    // SAFETY: `+[NSUserDefaults standardUserDefaults]` is a live get-rule
    // singleton; `-persistentDomainForName:`/`-objectForKey:`/`-UTF8String` are
    // get-rule reads. Main thread, autorelease pool active. We NEVER call a setter.
    unsafe {
        let defaults: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
        if defaults.is_null() {
            return DoubleClickAction::Zoom;
        }
        let domain = ns_string("NSGlobalDomain");
        let key = ns_string("AppleActionOnDoubleClick");
        let dict: *mut AnyObject = msg_send![defaults, persistentDomainForName: domain];
        let action: *mut AnyObject = if dict.is_null() {
            std::ptr::null_mut()
        } else {
            msg_send![dict, objectForKey: key]
        };
        if action.is_null() {
            return DoubleClickAction::Zoom;
        }
        let utf8: *const std::os::raw::c_char = msg_send![action, UTF8String];
        if utf8.is_null() {
            return DoubleClickAction::Zoom;
        }
        match std::ffi::CStr::from_ptr(utf8).to_string_lossy().as_ref() {
            "None" => DoubleClickAction::None,
            "Minimize" => DoubleClickAction::Minimize,
            _ => DoubleClickAction::Zoom,
        }
    }
}

/// Build an autoreleased `NSString` from a Rust `&str`.
///
/// # Safety
/// Main thread with an active autorelease pool (every scenario read satisfies
/// this — it runs on the gpui foreground/main runloop).
unsafe fn ns_string(s: &str) -> *mut AnyObject {
    let c = std::ffi::CString::new(s).unwrap_or_default();
    msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()]
}

// ===========================================================================
// R19 file-browser workspace calls — the FIVE OS-integration primitives behind
// the `WorkspaceOps` seam (`crate::file_browser::workspace_ops`): open with the
// OS default, open with a chosen application, reveal in Finder, enumerate the
// applications that can open a file (+ the default), and the "Other…" chooser.
//
// This is the ONLY module allowed to make the workspace / open-panel objc2
// calls (the hermeticity audit greps for that: the production symbols live here
// and nowhere else in `crates/nice/src`). Every call is reached ONLY through the
// `WorkspaceOps` Global — the production impl wires these; `run_selftest`
// installs a recording fake that never enters this file. All are main-thread /
// autorelease-pool callers (a gpui menu action), the same contract as the
// geometry readers above.
// ===========================================================================

/// Extract a Rust `String` from an `NSString *` (via `-UTF8String`). `None` for
/// a null string or a null UTF-8 buffer.
///
/// # Safety
/// `s` is a valid `NSString *` or null; main thread with an autorelease pool.
unsafe fn string_from_ns(s: *mut AnyObject) -> Option<String> {
    if s.is_null() {
        return None;
    }
    let utf8: *const std::os::raw::c_char = msg_send![s, UTF8String];
    if utf8.is_null() {
        return None;
    }
    Some(std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned())
}

/// `-[NSURL fileURLWithPath:]` for `path`.
///
/// # Safety
/// Main thread with an autorelease pool.
unsafe fn file_url(path: &str) -> *mut AnyObject {
    let ns = ns_string(path);
    msg_send![class!(NSURL), fileURLWithPath: ns]
}

/// The symlink-resolved, standardized filesystem path of a file `NSURL` — the
/// key the Open-With ordering dedupes on (one bundle reachable through several
/// symlinks collapses to one entry).
///
/// # Safety
/// `url` is a valid file `NSURL *`; main thread with an autorelease pool.
unsafe fn standardized_path(url: *mut AnyObject) -> Option<String> {
    if url.is_null() {
        return None;
    }
    let resolved: *mut AnyObject = msg_send![url, URLByResolvingSymlinksInPath];
    let use_url = if resolved.is_null() { url } else { resolved };
    let path: *mut AnyObject = msg_send![use_url, path];
    string_from_ns(path)
}

/// An application bundle's display name: `CFBundleDisplayName` → `CFBundleName`
/// → the file name without its extension (`OpenWithProvider.swift` parity).
///
/// # Safety
/// `app_url` is a valid application-bundle `NSURL *`; main thread + pool.
unsafe fn app_display_name(app_url: *mut AnyObject) -> String {
    let bundle: *mut AnyObject = msg_send![class!(NSBundle), bundleWithURL: app_url];
    if !bundle.is_null() {
        for key in ["CFBundleDisplayName", "CFBundleName"] {
            let k = ns_string(key);
            let value: *mut AnyObject = msg_send![bundle, objectForInfoDictionaryKey: k];
            if let Some(name) = string_from_ns(value) {
                if !name.is_empty() {
                    return name;
                }
            }
        }
    }
    // Fallback: the last path component without the `.app` extension.
    let last: *mut AnyObject = msg_send![app_url, lastPathComponent];
    let file = string_from_ns(last).unwrap_or_default();
    file.strip_suffix(".app").unwrap_or(&file).to_string()
}

/// `-[NSWorkspace openURL:]` — open `path` with the OS default handler.
///
/// # Safety
/// Main thread with an autorelease pool.
pub fn workspace_open(path: &str) {
    unsafe {
        let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let url = file_url(path);
        let _: Bool = msg_send![ws, openURL: url];
    }
}

/// `-[NSWorkspace openURLs:withApplicationAtURL:configuration:completionHandler:]`
/// — open `path` with the application at `app_path` (nil completion handler).
///
/// # Safety
/// Main thread with an autorelease pool.
pub fn workspace_open_with(path: &str, app_path: &str) {
    unsafe {
        let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let url = file_url(path);
        let app_url = file_url(app_path);
        let urls: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: url];
        let config: *mut AnyObject =
            msg_send![class!(NSWorkspaceOpenConfiguration), configuration];
        // The completion handler is a block param (`@?`), not a plain object —
        // give the nil the block encoding so objc2's debug encoding check accepts
        // it (the same class of gotcha as `OpaqueCGContext` above).
        let null_handler: *mut OpaqueBlock = std::ptr::null_mut();
        let _: () = msg_send![
            ws,
            openURLs: urls,
            withApplicationAtURL: app_url,
            configuration: config,
            completionHandler: null_handler
        ];
    }
}

/// `-[NSWorkspace activateFileViewerSelectingURLs:]` — reveal `path` in Finder.
///
/// # Safety
/// Main thread with an autorelease pool.
pub fn workspace_reveal(path: &str) {
    unsafe {
        let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let url = file_url(path);
        let urls: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: url];
        let _: () = msg_send![ws, activateFileViewerSelectingURLs: urls];
    }
}

/// `-[NSWorkspace URLsForApplicationsToOpenURL:]` + `URLForApplicationToOpenURL:`
/// — every application that can open `path`, as `(standardized_app_path,
/// display_name)` in Launch Services order, plus the user's default app path (if
/// any). The `WorkspaceOps` production impl feeds this into the pure Open-With
/// ordering function.
///
/// # Safety
/// Main thread with an autorelease pool.
pub fn workspace_apps_for(path: &str) -> (Vec<(String, String)>, Option<String>) {
    unsafe {
        let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let target = file_url(path);

        let arr: *mut AnyObject = msg_send![ws, URLsForApplicationsToOpenURL: target];
        let mut apps: Vec<(String, String)> = Vec::new();
        if !arr.is_null() {
            let count: usize = msg_send![arr, count];
            for i in 0..count {
                let app_url: *mut AnyObject = msg_send![arr, objectAtIndex: i];
                if let Some(std_path) = standardized_path(app_url) {
                    let name = app_display_name(app_url);
                    apps.push((std_path, name));
                }
            }
        }

        let default_url: *mut AnyObject = msg_send![ws, URLForApplicationToOpenURL: target];
        let default = standardized_path(default_url);
        (apps, default)
    }
}

/// The "Other…" chooser: an `NSOpenPanel` rooted at `/Applications`, filtered to
/// application bundles, prompt "Open". Returns the chosen application's path, or
/// `None` if the user cancels. Modal — production only (the recording fake
/// answers in tests / scenarios).
///
/// # Safety
/// Main thread with an autorelease pool.
pub fn workspace_choose_application() -> Option<String> {
    // `NSModalResponseOK` — the panel's accept response.
    const NS_MODAL_RESPONSE_OK: isize = 1;
    unsafe {
        let panel: *mut AnyObject = msg_send![class!(NSOpenPanel), openPanel];
        if panel.is_null() {
            return None;
        }
        let _: () = msg_send![panel, setCanChooseFiles: true];
        let _: () = msg_send![panel, setCanChooseDirectories: false];
        let _: () = msg_send![panel, setAllowsMultipleSelection: false];
        let _: () = msg_send![panel, setResolvesAliases: true];
        let apps_dir = file_url("/Applications");
        let _: () = msg_send![panel, setDirectoryURL: apps_dir];
        let prompt = ns_string("Open");
        let _: () = msg_send![panel, setPrompt: prompt];
        // Restrict to `.app` bundles.
        let app_type = ns_string("app");
        let types: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: app_type];
        let _: () = msg_send![panel, setAllowedFileTypes: types];

        let response: isize = msg_send![panel, runModal];
        if response != NS_MODAL_RESPONSE_OK {
            return None;
        }
        let urls: *mut AnyObject = msg_send![panel, URLs];
        if urls.is_null() {
            return None;
        }
        let count: usize = msg_send![urls, count];
        if count == 0 {
            return None;
        }
        let url: *mut AnyObject = msg_send![urls, objectAtIndex: 0usize];
        standardized_path(url)
    }
}

/// R23 Import…: an `NSOpenPanel` filtered to Ghostty theme files
/// (`.ghostty` / `.conf`), prompt "Import". Returns the chosen file's path, or
/// `None` if the user cancels. Modal — production ONLY (the `RecordingFilePicker`
/// answers in tests / scenarios, so no real panel ever opens under the suite).
///
/// # Safety
/// Main thread with an autorelease pool (a gpui button handler satisfies both).
pub fn choose_theme_file() -> Option<String> {
    // `NSModalResponseOK` — the panel's accept response.
    const NS_MODAL_RESPONSE_OK: isize = 1;
    unsafe {
        let panel: *mut AnyObject = msg_send![class!(NSOpenPanel), openPanel];
        if panel.is_null() {
            return None;
        }
        let _: () = msg_send![panel, setCanChooseFiles: true];
        let _: () = msg_send![panel, setCanChooseDirectories: false];
        let _: () = msg_send![panel, setAllowsMultipleSelection: false];
        let _: () = msg_send![panel, setResolvesAliases: true];
        let prompt = ns_string("Import");
        let _: () = msg_send![panel, setPrompt: prompt];
        // Restrict to Ghostty theme files (`.ghostty` import-written, `.conf`
        // hand-placed) — the extensions R22's catalog enumerates.
        let ghostty = ns_string("ghostty");
        let conf = ns_string("conf");
        let objs: [*mut AnyObject; 2] = [ghostty, conf];
        let types: *mut AnyObject =
            msg_send![class!(NSArray), arrayWithObjects: objs.as_ptr(), count: 2usize];
        let _: () = msg_send![panel, setAllowedFileTypes: types];

        let response: isize = msg_send![panel, runModal];
        if response != NS_MODAL_RESPONSE_OK {
            return None;
        }
        let urls: *mut AnyObject = msg_send![panel, URLs];
        if urls.is_null() {
            return None;
        }
        let count: usize = msg_send![urls, count];
        if count == 0 {
            return None;
        }
        let url: *mut AnyObject = msg_send![urls, objectAtIndex: 0usize];
        standardized_path(url)
    }
}

// ===========================================================================
// R27 update check — the one synchronous NSURLSession GitHub Releases GET
// (Binding decision D1). This is the ONLY module that touches OS networking; the
// production `ReleaseFetcher` forwards here, and `run_selftest` installs a
// recording fake that never enters this file (hermeticity). ZERO new
// transport/TLS crate: `NSURLSession` reaches Security.framework's system trust
// store directly, exactly as the app's existing objc2 links reach AppKit. The
// only Foundation-block helper is `block2` (objc2 family, already in the lock).
// ===========================================================================

/// The result of a successful [`http_get`]: the HTTP status and the response body
/// bytes. The caller (the `ReleaseFetcher`) checks `200..300` and decodes the body
/// — serde stays in the app layer, not here.
pub struct HttpGetResponse {
    /// The HTTP status code off the `NSHTTPURLResponse` (clamped into `u16`).
    pub status: u16,
    /// The response body bytes (copied out of the completion handler's `NSData`).
    pub body: Vec<u8>,
}

/// One synchronous HTTP GET via `NSURLSession` (D1): build an
/// `NSMutableURLRequest` with `url` + `headers` + `timeoutInterval`, run a
/// `dataTaskWithRequest:completionHandler:`, and block THIS thread on a channel
/// until the completion handler (invoked on `NSURLSession`'s own delegate queue)
/// delivers the result. Returns `Ok(HttpGetResponse)` on ANY HTTP response (2xx
/// or not — the caller decides), or `Err(message)` on a transport failure /
/// missing HTTP response.
///
/// **Must be called off a BACKGROUND thread** (the `ReleaseChecker` worker /
/// `background_executor`), NEVER the foreground — it blocks. `objc2` + `block2`
/// reach Foundation with no new transport crate.
///
/// # Safety / threading
/// The completion block runs on a background delegate thread; it fully COPIES the
/// bytes + status into owned Rust values BEFORE signalling, so nothing autoreleased
/// escapes the delegate's pool. The `RcBlock` is kept alive across the blocking
/// `recv` (`NSURLSession` retains the block for the task's duration).
pub fn http_get(
    url: &str,
    headers: &[(&str, &str)],
    timeout_secs: f64,
) -> Result<HttpGetResponse, String> {
    use block2::RcBlock;
    use objc2::rc::autoreleasepool;

    let (tx, rx) = std::sync::mpsc::channel::<Result<HttpGetResponse, String>>();
    // completionHandler:^(NSData *data, NSURLResponse *response, NSError *error).
    // The three `id` out-params are plain object pointers (`@`); `*mut AnyObject`
    // matches. The block is created OUTSIDE any autorelease pool (block2 heap-
    // allocates it) and kept alive until after `recv`. `tx` is moved in (the sole
    // sender), so the channel closes if the task is torn down before delivering.
    let handler = RcBlock::new(
        move |data: *mut AnyObject, response: *mut AnyObject, error: *mut AnyObject| {
            // SAFETY: the completion-handler out-params, any of which may be null;
            // `extract_http_result` does read-only get-rule reads + copies bytes.
            let result = unsafe { extract_http_result(data, response, error) };
            let _ = tx.send(result);
        },
    );

    // Build + start the request inside an autorelease pool (this may run on a
    // background worker thread with no ambient pool).
    let started: Result<(), String> = autoreleasepool(|_pool| {
        // SAFETY: Foundation calls on autoreleased objects; nulls are guarded, and
        // `NSURLSession` retains the request + block for the task's duration.
        unsafe {
            let url_ns = ns_string(url);
            let ns_url: *mut AnyObject = msg_send![class!(NSURL), URLWithString: url_ns];
            if ns_url.is_null() {
                return Err(format!("invalid URL: {url}"));
            }
            let request: *mut AnyObject =
                msg_send![class!(NSMutableURLRequest), requestWithURL: ns_url];
            if request.is_null() {
                return Err("failed to build the request".to_string());
            }
            let get = ns_string("GET");
            let _: () = msg_send![request, setHTTPMethod: get];
            let _: () = msg_send![request, setTimeoutInterval: timeout_secs];
            for (name, value) in headers {
                let v = ns_string(value);
                let n = ns_string(name);
                let _: () = msg_send![request, setValue: v, forHTTPHeaderField: n];
            }
            let session: *mut AnyObject = msg_send![class!(NSURLSession), sharedSession];
            if session.is_null() {
                return Err("no shared NSURLSession".to_string());
            }
            let task: *mut AnyObject = msg_send![
                session,
                dataTaskWithRequest: request,
                completionHandler: &*handler
            ];
            if task.is_null() {
                return Err("failed to create the data task".to_string());
            }
            let _: () = msg_send![task, resume];
            Ok(())
        }
    });
    started?;

    // Block this (worker) thread until the delegate queue delivers. `handler` is
    // kept alive across the recv (NSURLSession retains it, but we hold it too).
    let out = rx
        .recv()
        .map_err(|_| "the completion handler was dropped without a result".to_string())?;
    drop(handler);
    out
}

/// Copy the completion-handler out-params into an owned [`HttpGetResponse`] or an
/// error message, on the delegate thread, before the autorelease pool drains.
///
/// # Safety
/// `data` / `response` / `error` are the `dataTaskWithRequest:completionHandler:`
/// args: an `NSData *`, an `NSURLResponse *` (expected `NSHTTPURLResponse`), and
/// an `NSError *`, any of which may be null. All reads are get-rule; the body
/// bytes are copied into an owned `Vec` before return.
unsafe fn extract_http_result(
    data: *mut AnyObject,
    response: *mut AnyObject,
    error: *mut AnyObject,
) -> Result<HttpGetResponse, String> {
    if !error.is_null() {
        let desc: *mut AnyObject = msg_send![error, localizedDescription];
        let msg = string_from_ns(desc).unwrap_or_else(|| "network error".to_string());
        return Err(msg);
    }
    if response.is_null() {
        return Err("no response".to_string());
    }
    // `statusCode` is an `NSHTTPURLResponse` property; guard against a non-HTTP
    // response (which lacks the selector) before sending it.
    let is_http: Bool = msg_send![response, isKindOfClass: class!(NSHTTPURLResponse)];
    if !is_http.as_bool() {
        return Err("non-HTTP response".to_string());
    }
    let status: isize = msg_send![response, statusCode];
    let status = status.clamp(0, u16::MAX as isize) as u16;
    let body = if data.is_null() {
        Vec::new()
    } else {
        let bytes: *const u8 = msg_send![data, bytes];
        let len: usize = msg_send![data, length];
        if bytes.is_null() || len == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(bytes, len).to_vec()
        }
    };
    Ok(HttpGetResponse { status, body })
}

// ===========================================================================
// R20 file-operations foreign side — the objc2 Trash + system-pasteboard
// primitives behind the `Trasher` / `FilePasteboard` seams
// (`crate::file_browser::{ops, pasteboard}`), plus a public App-Nap-safe delay
// wrapper for the drift banner's auto-dismiss.
//
// This is the ONLY module that touches `NSFileManager -trashItemAtURL:…` or
// `NSPasteboard` for file-URL / text interop (the hermeticity audit greps for
// exactly that: the production symbols live here and nowhere else in
// `crates/nice/src`). Tests inject a `FakeTrasher` / fake `FilePasteboard`, or —
// for the objc2 round-trip cases — a NAMED pasteboard invisible to Finder.
// All callers are main-thread / autorelease-pool contexts (a gpui menu action or
// `app::run` bootstrap), the same contract as the workspace readers above.
// ===========================================================================

/// The App-Nap-safe delay the drift banner's 3.5 s auto-dismiss rides — the
/// `pub` wrapper the plan asks for over the private [`AppNapSafeDelay`] (mirror
/// of the [`launch_deadline`] factory). A bare `background_executor().timer` is
/// indefinitely deferred by App Nap on an idle/occluded window (spike 6), which
/// is exactly the state a window sits in while a stale banner lingers; this rides
/// the dedicated-OS-thread sleep + main-runloop wake instead. `await` it inside a
/// foreground `cx.spawn`.
pub fn nap_safe_delay(delay: Duration) -> impl Future<Output = ()> {
    AppNapSafeDelay {
        delay,
        done: Arc::new(AtomicBool::new(false)),
        armed: false,
    }
}

// -- Trash (NSFileManager -trashItemAtURL:resultingItemURL:error:) ------------

/// Recycle each path in `urls` through `-[NSFileManager
/// trashItemAtURL:resultingItemURL:error:]`, returning `(original, trashed)`
/// pairs in input order. Synchronous, per-item, in order; on the FIRST failure it
/// returns `Err` with the `NSError` description — earlier items stay trashed
/// (ops are NOT transactional), matching the frozen [`Trasher`](crate::file_browser::ops::Trasher)
/// contract. The `resultingItemURL:` / `error:` out-params are `NSURL**` / `NSError**`
/// (`^@`): a `*mut *mut AnyObject` supplies that encoding, sidestepping the
/// `OpaqueCGContext`-class encoding gotcha (a bare object pointer would mis-encode).
///
/// # Safety contract
/// Main thread with an active autorelease pool (a gpui menu action satisfies both).
pub fn trash_items(urls: &[PathBuf]) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    let mut out = Vec::with_capacity(urls.len());
    // SAFETY: `defaultManager` is a get-rule singleton; `file_url` yields an
    // autoreleased NSURL; the two out-params are valid `NSURL**` / `NSError**`
    // slots (null-initialised, only written by the callee); on success `resulting`
    // is an autoreleased NSURL we only read a path from.
    unsafe {
        let fm: *mut AnyObject = msg_send![class!(NSFileManager), defaultManager];
        for original in urls {
            let path_str = original.to_string_lossy();
            let url = file_url(&path_str);
            let mut resulting: *mut AnyObject = std::ptr::null_mut();
            let mut error: *mut AnyObject = std::ptr::null_mut();
            let ok: Bool = msg_send![
                fm,
                trashItemAtURL: url,
                resultingItemURL: (&mut resulting) as *mut *mut AnyObject,
                error: (&mut error) as *mut *mut AnyObject
            ];
            if !ok.as_bool() {
                let message = if error.is_null() {
                    format!("couldn't move '{}' to the Trash", path_str)
                } else {
                    let desc: *mut AnyObject = msg_send![error, localizedDescription];
                    string_from_ns(desc)
                        .unwrap_or_else(|| format!("couldn't move '{}' to the Trash", path_str))
                };
                return Err(message);
            }
            let trashed = if resulting.is_null() {
                original.clone()
            } else {
                let path: *mut AnyObject = msg_send![resulting, path];
                string_from_ns(path)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| original.clone())
            };
            out.push((original.clone(), trashed));
        }
    }
    Ok(out)
}

// -- Pasteboard (NSPasteboard file-URL + text interop) ------------------------

/// A retained handle onto an `NSPasteboard` — the general system pasteboard
/// (`app::run` binds it once) or a NAMED test pasteboard (invisible to Finder /
/// other apps, the isolated-pasteboard-round-trip precedent). Every browser
/// pasteboard write goes through the standard `public.file-url`
/// (`writeObjects:[NSURL]`) or plain-text type so Nice interoperates with Finder
/// both directions — closing gpui's write gap (`gpui_macos` silently drops
/// `ExternalPaths`). The production [`FilePasteboard`](crate::file_browser::pasteboard::FilePasteboard)
/// impl forwards to this; the objc2 code is exercised identically by named-pasteboard
/// integration tests.
pub struct PasteboardRef {
    /// The retained `NSPasteboard *`. Released on drop (balanced with the `retain`
    /// in the constructors).
    pasteboard: *mut AnyObject,
}

impl PasteboardRef {
    /// Bind the general system pasteboard (`+[NSPasteboard generalPasteboard]`).
    /// `app::run` ONLY — the shipped Copy / Cut / Paste / Copy-Path surface.
    ///
    /// # Safety
    /// Main thread with an active autorelease pool.
    pub unsafe fn general() -> Self {
        let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        Self::retaining(pb)
    }

    /// Create an isolated NAMED pasteboard (`+[NSPasteboard pasteboardWithName:]`)
    /// — invisible to Finder / other apps. The round-trip integration tests use
    /// this so they exercise the SAME objc2 write/read path without ever mutating
    /// the general pasteboard. Call [`release_globally`](Self::release_globally)
    /// before drop to destroy the named pasteboard.
    ///
    /// # Safety
    /// Main thread with an active autorelease pool.
    pub unsafe fn named(name: &str) -> Self {
        let ns_name = ns_string(name);
        let pb: *mut AnyObject = msg_send![class!(NSPasteboard), pasteboardWithName: ns_name];
        Self::retaining(pb)
    }

    /// Retain `pb` so the handle owns a +1 reference for its lifetime.
    ///
    /// # Safety
    /// `pb` is a valid `NSPasteboard *` (or null, tolerated as a dead handle).
    unsafe fn retaining(pb: *mut AnyObject) -> Self {
        if !pb.is_null() {
            let _: *mut AnyObject = msg_send![pb, retain];
        }
        Self { pasteboard: pb }
    }

    /// Replace the pasteboard contents with `paths` as `public.file-url` items
    /// (`clearContents` + `writeObjects:[NSURL fileURLWithPath:]`).
    pub fn write_file_urls(&self, paths: &[PathBuf]) {
        if self.pasteboard.is_null() {
            return;
        }
        // SAFETY: live retained pasteboard; each `file_url` is an autoreleased
        // NSURL added to an autoreleased NSMutableArray; `writeObjects:` copies the
        // items. Main-thread / pool caller.
        unsafe {
            let _: isize = msg_send![self.pasteboard, clearContents];
            let array: *mut AnyObject = msg_send![class!(NSMutableArray), array];
            for path in paths {
                let url = file_url(&path.to_string_lossy());
                if !url.is_null() {
                    let _: () = msg_send![array, addObject: url];
                }
            }
            let _: Bool = msg_send![self.pasteboard, writeObjects: array];
        }
    }

    /// Read the pasteboard's file URLs (`readObjectsForClasses:[NSURL] options:nil`,
    /// filtered to `isFileURL`) as filesystem paths. Non-file URLs (e.g. an `http`
    /// URL another app copied) are skipped, mirroring the Swift file-URL filter.
    pub fn read_file_urls(&self) -> Vec<PathBuf> {
        if self.pasteboard.is_null() {
            return Vec::new();
        }
        // SAFETY: live retained pasteboard; `readObjectsForClasses:options:` returns
        // an autoreleased NSArray of autoreleased NSURLs; we only read `isFileURL` /
        // `path` from each. `NSURL`'s class object is passed as the sole element of
        // the classes array. Main-thread / pool caller.
        unsafe {
            let url_class: *const AnyObject =
                (class!(NSURL) as *const objc2::runtime::AnyClass).cast();
            let classes: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: url_class];
            let options: *mut AnyObject = std::ptr::null_mut();
            let objs: *mut AnyObject =
                msg_send![self.pasteboard, readObjectsForClasses: classes, options: options];
            if objs.is_null() {
                return Vec::new();
            }
            let count: usize = msg_send![objs, count];
            let mut out = Vec::with_capacity(count);
            for i in 0..count {
                let url: *mut AnyObject = msg_send![objs, objectAtIndex: i];
                if url.is_null() {
                    continue;
                }
                let is_file: Bool = msg_send![url, isFileURL];
                if !is_file.as_bool() {
                    continue;
                }
                let path: *mut AnyObject = msg_send![url, path];
                if let Some(p) = string_from_ns(path) {
                    out.push(PathBuf::from(p));
                }
            }
            out
        }
    }

    /// Replace the pasteboard contents with plain text (`clearContents` +
    /// `setString:forType:NSPasteboardTypeString`) — the Copy-Path write.
    pub fn write_text(&self, text: &str) {
        if self.pasteboard.is_null() {
            return;
        }
        // SAFETY: live retained pasteboard; `ns_string` is an autoreleased NSString;
        // `NSPasteboardTypeString` is an interned type constant. Main-thread / pool.
        unsafe {
            let _: isize = msg_send![self.pasteboard, clearContents];
            let s = ns_string(text);
            let ty = NSPasteboardTypeString;
            let _: Bool = msg_send![self.pasteboard, setString: s, forType: ty];
        }
    }

    /// The pasteboard's `changeCount` — bumped by any writer (us or another app),
    /// which is how the in-process cut companion detects an external mutation.
    pub fn change_count(&self) -> i64 {
        if self.pasteboard.is_null() {
            return 0;
        }
        // SAFETY: live retained pasteboard; `-changeCount` is a get-rule NSInteger.
        unsafe {
            let count: isize = msg_send![self.pasteboard, changeCount];
            count as i64
        }
    }

    /// Destroy a NAMED pasteboard (`-releaseGlobally`) — tests call this before
    /// dropping their isolated pasteboard so it leaves no global trace. A no-op on
    /// the general pasteboard's handle would be wrong, so only tests (which own a
    /// named pasteboard) call it.
    ///
    /// # Safety
    /// Main thread; the handle must be a named pasteboard (never the general one).
    pub unsafe fn release_globally(&self) {
        if !self.pasteboard.is_null() {
            let _: () = msg_send![self.pasteboard, releaseGlobally];
        }
    }
}

impl Drop for PasteboardRef {
    fn drop(&mut self) {
        if !self.pasteboard.is_null() {
            // SAFETY: balances the `retain` in `retaining`; after this the handle
            // relinquishes its +1. The general pasteboard is a long-lived singleton
            // AppKit also retains, so releasing our +1 never deallocates it.
            unsafe {
                let _: () = msg_send![self.pasteboard, release];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::present_kick_due;

    // ---- r5d present-kick occlusion gate --------------------------------
    //
    // `present_kick` itself is objc2 (it needs a live NSView/NSWindow), so the
    // gate is split into an unmockable query (`view_window_occlusion_visible`)
    // and this pure decision — the same decompose-then-test seam the drain
    // uses for the kick itself (`set_present_kick` injection) and for the r5
    // throttle (`present_gate`). These pin the decision table; the end-to-end
    // "occluded window still presents a modal" behavior is the orchestrator's
    // black-box check.

    /// Visible window ⇒ the display link is ticking (gpui stops it only on
    /// occlusion), so the kick must be SKIPPED — firing it re-enters
    /// `displayLayer:`'s stop/draw/recreate cycle, the ~166/s recreate storm
    /// behind the 2026-07-10 stopped-link presentation wedge.
    #[test]
    fn kick_skipped_when_window_occlusion_visible() {
        assert!(!present_kick_due(Some(true)));
    }

    /// Occluded window ⇒ the link is stopped and `cx.notify()` alone never
    /// presents (module fact 1): the kick MUST fire — this is the ec0b8f3
    /// occluded-modal guarantee, preserved verbatim.
    #[test]
    fn kick_fires_when_window_occluded() {
        assert!(present_kick_due(Some(false)));
    }

    /// View not hosted in a window (headless / teardown / pre-show) ⇒ unknown
    /// state: keep the pre-gate behavior and fire — `setNeedsDisplay` on an
    /// unhosted view is harmless, and skipping on "unknown" could starve a
    /// window whose AppKit handle lags its first damage.
    #[test]
    fn kick_fires_when_view_has_no_window() {
        assert!(present_kick_due(None));
    }
}
