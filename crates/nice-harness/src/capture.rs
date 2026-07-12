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

/// Read back straight `[r, g, b, a]` bytes at each given **logical** point of
/// `handle`'s current rendered frame.
///
/// Each logical point (gpui `px` units, content-view top-left origin) is scaled
/// to a device pixel by the window's `scale_factor()` — so callers lay out in
/// logical coordinates and never hardcode the backing scale. This is the same
/// `Window::render_to_image()` drawable read-back that [`capture_window_png`]
/// uses (a scenario's own pixel assertions and the `NICE_CAPTURE` PNG go
/// through one path), so it is likewise gated behind the `capture` feature.
///
/// Errors if any point falls outside the captured image (a layout bug the caller
/// wants surfaced, not silently clamped).
#[cfg(feature = "capture")]
pub fn sample_window_pixels(
    handle: AnyWindowHandle,
    cx: &mut AsyncApp,
    logical_points: &[(f32, f32)],
) -> anyhow::Result<Vec<[u8; 4]>> {
    use image::Pixel;

    let (image, scale) = handle.update(cx, |_view, window, _app| {
        let image = window.render_to_image()?;
        anyhow::Ok((image, window.scale_factor()))
    })??;

    let (width, height) = image.dimensions();
    let mut out = Vec::with_capacity(logical_points.len());
    for &(lx, ly) in logical_points {
        let dx = (lx * scale).round() as i64;
        let dy = (ly * scale).round() as i64;
        anyhow::ensure!(
            dx >= 0 && dy >= 0 && (dx as u32) < width && (dy as u32) < height,
            "sample point ({lx}, {ly}) logical -> ({dx}, {dy}) device is outside the \
             {width}x{height} captured image (scale {scale})"
        );
        let ch = image.get_pixel(dx as u32, dy as u32).channels();
        out.push([ch[0], ch[1], ch[2], ch[3]]);
    }
    Ok(out)
}

/// Stub used when the `capture` feature is off (shipped builds). Pixel readback,
/// like PNG capture, needs `Window::render_to_image()`, so it is a hard error
/// here rather than a silent no-op.
#[cfg(not(feature = "capture"))]
pub fn sample_window_pixels(
    _handle: AnyWindowHandle,
    _cx: &mut AsyncApp,
    _logical_points: &[(f32, f32)],
) -> anyhow::Result<Vec<[u8; 4]>> {
    anyhow::bail!(
        "pixel readback requires the `selftest` feature (gpui test-support); \
         rebuild crates/nice with `--features selftest`"
    )
}
