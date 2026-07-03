//! os_signpost emission on subsystem `dev.nickanderssohn.nice-rs`
//! (category `selftest`, name `Frame`).
//!
//! The actual emission lives in the C shim `signpost.c` (linked by `build.rs`)
//! — the os_signpost macros must run from C to place their strings in the
//! `__TEXT` sections Instruments reads. This module is the thin FFI + a
//! monotonic frame counter that exists even when no Instruments recorder is
//! attached.
//!
//! On non-macOS targets the shim is not built, so the FFI is replaced by
//! no-ops (there is no os_signpost off macOS anyway).

use std::sync::atomic::{AtomicU64, Ordering};

/// Total `frame_begin` calls this process — cheap liveness signal independent
/// of whether a signpost recorder is attached.
static FRAME_COUNT: AtomicU64 = AtomicU64::new(0);

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn nice_rs_signpost_frame_begin() -> u64;
    fn nice_rs_signpost_frame_end(spid: u64);
}

/// Begin a "Frame" signpost interval; returns the id to pass to
/// [`frame_end`]. Returns 0 (a no-op id) when signposts are disabled.
#[inline]
pub fn frame_begin() -> u64 {
    FRAME_COUNT.fetch_add(1, Ordering::Relaxed);
    #[cfg(target_os = "macos")]
    unsafe {
        nice_rs_signpost_frame_begin()
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

/// End the "Frame" signpost interval opened by [`frame_begin`]. A 0 id is a
/// no-op.
#[inline]
pub fn frame_end(spid: u64) {
    #[cfg(target_os = "macos")]
    unsafe {
        nice_rs_signpost_frame_end(spid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = spid;
    }
}

/// Number of frame signposts begun so far this process.
#[inline]
pub fn frame_count() -> u64 {
    FRAME_COUNT.load(Ordering::Relaxed)
}
