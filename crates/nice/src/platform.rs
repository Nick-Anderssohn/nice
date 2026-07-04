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
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
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

/// The `CGEventFlags` ⌘ (Command) mask — carried on a synthesized ⌘V. The other
/// modifier masks (shift `0x20000`, control `0x40000`, alternate `0x80000`) are
/// not needed by the current live scenarios, so they are intentionally omitted
/// until a chord assertion needs one.
pub const FLAG_COMMAND: u64 = 0x0010_0000;

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
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
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
// it; these are the two array accessors + CFRelease this module uses).
extern "C" {
    fn CFArrayGetCount(array: CfArrayRef) -> CfIndex;
    fn CFArrayGetValueAtIndex(array: CfArrayRef, idx: CfIndex) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

/// Whether this process holds the Accessibility (TCC) grant. Without it
/// `CGEventPostToPid` is a **silent no-op** — every injected keystroke is
/// dropped — so the live scenarios must gate on this and FAIL loudly (never
/// silently skip) when it is missing. Mirrors `keyinject.swift`'s preflight.
pub fn accessibility_trusted() -> bool {
    // SAFETY: `AXIsProcessTrusted` takes no arguments and is always safe to call.
    unsafe { AXIsProcessTrusted() != 0 }
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
