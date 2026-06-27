//! Rust `extern "C"` declarations matching the Swift `@_cdecl` bridge
//! (swift-bridge/Sources/SwiftTermBridge/Bridge.swift and its headless twin
//! swift-embed/StubBridge.swift), plus a safe `Terminal` wrapper.
//!
//! Every `st_*` call must run on the AppKit MAIN THREAD. The wrapper does not
//! enforce that with a type marker (gpui already pins us to the main thread);
//! call sites are all inside gpui render / main-thread closures.

use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr;

// ---- Opaque handle types ----------------------------------------------------

/// Opaque `STHandle*` from the Swift side (keeps the view + delegate alive).
pub type StHandle = *mut c_void;
/// `id` / `*mut NSView` for `addSubview:` on the AppKit side.
pub type NsViewPtr = *mut c_void;

// ---- Reverse-FFI callback types (must match @convention(c) in Swift) --------

pub type StSendCb = extern "C" fn(*mut c_void, *const u8, usize);
pub type StTitleCb = extern "C" fn(*mut c_void, *const c_char);
pub type StSizeCb = extern "C" fn(*mut c_void, i32, i32);
pub type StBellCb = extern "C" fn(*mut c_void);
pub type StDirCb = extern "C" fn(*mut c_void, *const c_char);
pub type StClipCopyCb = extern "C" fn(*mut c_void, *const u8, usize);

/// Harness frame hooks (one `u64` mach_absolute_time tick).
pub type NiceFrameCb = extern "C" fn(u64);

extern "C" {
    // lifecycle
    pub fn st_create(x: f64, y: f64, w: f64, h: f64) -> StHandle;
    pub fn st_nsview(h: StHandle) -> NsViewPtr;
    pub fn st_destroy(h: StHandle);
    pub fn st_set_use_metal(h: StHandle, enabled: i32) -> i32; // 1 ok, 0 metal failed/stub
    pub fn st_is_using_metal(h: StHandle) -> i32;
    pub fn st_set_loopback(h: StHandle, enabled: i32);

    // feed / resize / present
    pub fn st_feed_bytes(h: StHandle, ptr: *const u8, len: usize);
    pub fn st_resize(h: StHandle, cols: i32, rows: i32);
    pub fn st_set_frame(h: StHandle, x: f64, y: f64, w: f64, d: f64);
    pub fn st_present_now(h: StHandle) -> i32; // forces 1 synchronous frame; FPS hook
    pub fn st_present_async(h: StHandle); // coalesced async present (fork's production path)
    pub fn st_start_present_link(h: StHandle) -> i32; // decoupled CADisplayLink present loop (1 ok, 0 stub)
    pub fn st_stop_present_link(h: StHandle);

    // font / colors  (0x00RRGGBB; palette16 = ptr to 16 u32 or null)
    pub fn st_set_font(h: StHandle, name: *const c_char, size: f64);
    pub fn st_set_colors(h: StHandle, fg: u32, bg: u32, palette16: *const u32);

    // selection (free returned ptr with st_string_free)
    pub fn st_get_selection(h: StHandle) -> *mut c_char;
    pub fn st_string_free(p: *mut c_char);

    // reverse-FFI registration
    pub fn st_register_callbacks(
        h: StHandle,
        userdata: *mut c_void,
        on_send: Option<StSendCb>,
        on_title: Option<StTitleCb>,
        on_size: Option<StSizeCb>,
        on_bell: Option<StBellCb>,
        on_dir: Option<StDirCb>,
        on_clip_copy: Option<StClipCopyCb>,
    );

    // harness frame hooks
    pub fn nice_harness_set_present_cb(cb: NiceFrameCb);
    pub fn nice_harness_set_draw_cb(cb: NiceFrameCb);
}

// ---- Safe wrapper -----------------------------------------------------------

/// RAII wrapper over an `STHandle`. `Drop` calls `st_destroy`.
///
/// `Terminal` is intentionally `!Send`/`!Sync` (raw pointer) — it must only be
/// touched on the main thread.
pub struct Terminal {
    handle: StHandle,
}

impl Terminal {
    /// Create a terminal view at the given pixel frame. Call on the main thread.
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        let handle = unsafe { st_create(x, y, w, h) };
        assert!(!handle.is_null(), "st_create returned null");
        Terminal { handle }
    }

    /// The underlying `NSView*` so AppKit/objc2 can `addSubview:` it.
    pub fn nsview_ptr(&self) -> NsViewPtr {
        unsafe { st_nsview(self.handle) }
    }

    /// Enable the Metal renderer. Returns true on success. With the headless
    /// stub this is always false (no Metal) — the honest "stub" signal.
    pub fn set_use_metal(&self, enabled: bool) -> bool {
        unsafe { st_set_use_metal(self.handle, enabled as i32) == 1 }
    }

    pub fn is_using_metal(&self) -> bool {
        unsafe { st_is_using_metal(self.handle) == 1 }
    }

    /// Local-echo loopback for the seam-latency profile (§C.3).
    pub fn set_loopback(&self, enabled: bool) {
        unsafe { st_set_loopback(self.handle, enabled as i32) }
    }

    pub fn feed_bytes(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        unsafe { st_feed_bytes(self.handle, bytes.as_ptr(), bytes.len()) }
    }

    pub fn resize(&self, cols: i32, rows: i32) {
        unsafe { st_resize(self.handle, cols, rows) }
    }

    pub fn set_frame(&self, x: f64, y: f64, w: f64, h: f64) {
        unsafe { st_set_frame(self.handle, x, y, w, h) }
    }

    /// Force one synchronous frame; returns true if a frame was issued.
    pub fn present_now(&self) -> bool {
        unsafe { st_present_now(self.handle) == 1 }
    }

    /// Queue ONE coalesced async present on the main queue (the fork's production
    /// path). Returns immediately; the present runs on a later run-loop turn.
    pub fn present_async(&self) {
        unsafe { st_present_async(self.handle) }
    }

    /// Start the decoupled CADisplayLink present loop (terminal presents at the
    /// display refresh on its own run-loop source). Returns true if scheduled
    /// (real bridge); false with the headless stub — caller then keeps issuing
    /// `present_now()` synchronously per GPUI frame.
    pub fn start_present_link(&self) -> bool {
        unsafe { st_start_present_link(self.handle) == 1 }
    }

    /// Stop the decoupled present loop. Idempotent.
    pub fn stop_present_link(&self) {
        unsafe { st_stop_present_link(self.handle) }
    }

    pub fn set_font(&self, name: Option<&str>, size: f64) {
        match name {
            Some(n) => {
                let c = CString::new(n).unwrap();
                unsafe { st_set_font(self.handle, c.as_ptr(), size) }
            }
            None => unsafe { st_set_font(self.handle, ptr::null(), size) },
        }
    }

    pub fn set_colors(&self, fg: u32, bg: u32, palette16: Option<&[u32; 16]>) {
        let p = palette16.map(|a| a.as_ptr()).unwrap_or(ptr::null());
        unsafe { st_set_colors(self.handle, fg, bg, p) }
    }

    pub fn selection(&self) -> Option<String> {
        unsafe {
            let p = st_get_selection(self.handle);
            if p.is_null() {
                return None;
            }
            let s = CStr::from_ptr(p).to_string_lossy().into_owned();
            st_string_free(p);
            Some(s)
        }
    }

    /// Register reverse-FFI callbacks. `userdata` is passed back to each.
    #[allow(clippy::too_many_arguments)]
    pub fn register_callbacks(
        &self,
        userdata: *mut c_void,
        on_send: Option<StSendCb>,
        on_title: Option<StTitleCb>,
        on_size: Option<StSizeCb>,
        on_bell: Option<StBellCb>,
        on_dir: Option<StDirCb>,
        on_clip_copy: Option<StClipCopyCb>,
    ) {
        unsafe {
            st_register_callbacks(
                self.handle,
                userdata,
                on_send,
                on_title,
                on_size,
                on_bell,
                on_dir,
                on_clip_copy,
            )
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe { st_destroy(self.handle) }
    }
}

/// Install the two harness frame hooks (present + draw-attempt).
pub fn install_frame_hooks(on_draw: NiceFrameCb, on_present: NiceFrameCb) {
    unsafe {
        nice_harness_set_draw_cb(on_draw);
        nice_harness_set_present_cb(on_present);
    }
}
