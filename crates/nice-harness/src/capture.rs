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

use std::path::{Path, PathBuf};

use gpui::{AnyWindowHandle, AsyncApp};

/// Capture the current rendered frame of `handle` to a PNG at `path`.
///
/// The encoding is PNG — that is the documented `NICE_CAPTURE` contract — and
/// it does NOT depend on `path` carrying a recognised extension: `image`'s
/// extension sniffing would otherwise fail an extensionless path with "The
/// image format could not be determined", turning a capture request into a
/// scenario FAILURE. A path whose extension names a different encoder the
/// `image` build supports still gets that encoder.
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
    let format = image::ImageFormat::from_path(path).unwrap_or(image::ImageFormat::Png);
    image.save_with_format(path, format)?;
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

/// The `NICE_CAPTURE` path requested for this run, if any.
///
/// The driver reads the same variable for its end-of-scenario capture; a
/// scenario asks through here so a mid-scenario capture happens only when the
/// operator actually requested one.
pub fn requested_path() -> Option<PathBuf> {
    std::env::var("NICE_CAPTURE")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// Capture a frame MID-scenario, while a transient UI state is still on screen.
///
/// The driver's own capture runs *after* a scenario's verdict, when the window
/// shows its torn-down end state — so a state that exists only during the run
/// (an open rename field, a menu, a drag) can never be spot-checked from it.
/// A scenario calls this at the moment it wants recorded, and the frame lands
/// beside the requested `NICE_CAPTURE` path as `<base>-<stage>.png`.
///
/// Returns `Ok(None)` when no capture was requested (the normal gate run) and
/// `Ok(Some(path))` with what was written otherwise. A requested capture that
/// fails is a scenario failure — report the error, exactly as the driver does.
pub fn capture_stage(
    handle: AnyWindowHandle,
    cx: &mut AsyncApp,
    stage: &str,
) -> anyhow::Result<Option<PathBuf>> {
    let Some(base) = requested_path() else {
        return Ok(None);
    };
    let path = stage_path(&base, stage);
    capture_window_png(handle, cx, &path)?;
    Ok(Some(path))
}

/// `<base without its extension>-<stage>.png`, beside `base`.
fn stage_path(base: &Path, stage: &str) -> PathBuf {
    let stem = base
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("nice-capture");
    let file = format!("{stem}-{stage}.png");
    match base.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(file),
        _ => PathBuf::from(file),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_path_appends_the_stage_and_always_ends_in_png() {
        // The documented `NICE_CAPTURE=/tmp/nice-smoke.png` shape.
        assert_eq!(
            stage_path(Path::new("/tmp/nice-smoke.png"), "rename-caret"),
            PathBuf::from("/tmp/nice-smoke-rename-caret.png")
        );
        // An EXTENSIONLESS base is legal (the driver's own capture PNG-encodes
        // it) — the stage file still gets a .png so viewers open it.
        assert_eq!(
            stage_path(Path::new("/tmp/nice-plan1-capture"), "rename-selection"),
            PathBuf::from("/tmp/nice-plan1-capture-rename-selection.png")
        );
        // A bare relative name has no parent directory to join onto.
        assert_eq!(
            stage_path(Path::new("shot.png"), "x"),
            PathBuf::from("shot-x.png")
        );
    }
}
