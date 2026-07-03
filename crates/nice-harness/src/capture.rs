//! Screenshot capture via `Window::render_to_image()`.
//!
//! `render_to_image` is public but gated `#[cfg(any(test, feature =
//! "test-support"))]` in gpui; the macOS renderer implements it by reading the
//! drawable texture back, which requires `CAMetalLayer.framebufferOnly = false`
//! — a flag gpui_macos only clears under that same cfg, PROCESS-WIDE. So the
//! whole capture facility is behind this crate's `capture` feature (the app
//! crate's `selftest` feature forwards to it); shipped builds omit it and keep a
//! framebuffer-only layer.
//!
//! We deliberately do NOT use `VisualTestAppContext::capture_screenshot`: that
//! is a `TestDispatcher` context (off-screen windows, deterministic
//! scheduling) and would invalidate the live cadence assertions the same
//! scenarios make. Capture runs against the REAL on-screen window.

use std::path::Path;

use gpui::{AnyWindowHandle, AsyncApp};

/// Capture the current rendered frame of `handle` to a PNG at `path`.
///
/// Without the `capture` feature the facility is not compiled, so this returns
/// an actionable error instead of silently doing nothing.
#[cfg(feature = "capture")]
pub fn capture_window_png(
    handle: AnyWindowHandle,
    cx: &mut AsyncApp,
    path: &Path,
) -> anyhow::Result<()> {
    let image = handle.update(cx, |_view, window, _app| window.render_to_image())??;
    image.save(path)?;
    Ok(())
}

/// Stub used when the `capture` feature is off (shipped builds). Requesting a
/// capture in that configuration is a hard error, not a silent no-op.
#[cfg(not(feature = "capture"))]
pub fn capture_window_png(
    _handle: AnyWindowHandle,
    _cx: &mut AsyncApp,
    _path: &Path,
) -> anyhow::Result<()> {
    anyhow::bail!(
        "screenshot capture requires the `selftest` feature (gpui test-support); \
         rebuild crates/nice with `--features selftest`"
    )
}
