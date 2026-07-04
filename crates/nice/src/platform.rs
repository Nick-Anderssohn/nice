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
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

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
