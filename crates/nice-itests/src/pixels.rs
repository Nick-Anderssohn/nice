//! Screenshot sampling + pixel-assert helpers, and the bottom-anchored cell
//! geometry the samplers key off.
//!
//! Pure arithmetic over an RGBA8 pixel buffer plus `nice-term-view`'s public
//! layout constants — **no gpui `test-support`**, so these compile in any build
//! and are reused by both execution models (the behavior context never reads
//! pixels, but the geometry + tolerance convention are shared). The tolerance is
//! the same `±8/255` per-channel band the live `tokens` / `term-render`
//! scenarios use, so an in-process pixel assertion cannot silently diverge from
//! the live floor.

use anyhow::{ensure, Result};
use nice_term_view::{TerminalMetrics, TERMINAL_BOTTOM_GAP};

/// Per-channel tolerance (out of 255) for a pixel match — the same band the live
/// `tokens` / `term-render` self-tests assert within. Keeping one convention
/// across the live suite and the in-process harness is deliberate: a pixel that
/// passes here would pass there.
pub const DEFAULT_PIXEL_TOLERANCE: u8 = 8;

/// Logical centre `(x, y)` of grid cell `(row, col)` under the T4 bottom-anchored
/// layout, for a content view `content_h` logical px tall holding `rows` grid
/// rows at cell `metrics`. Mirrors [`nice_term_view::grid_top_y`] with the
/// element origin at `(0, 0)` (the terminal fills the window content area). The
/// origin y can go negative when the grid is taller than the view (top rows clip)
/// — the caller keeps the sampled row on-screen.
pub fn cell_center(
    content_h: f32,
    rows: usize,
    metrics: TerminalMetrics,
    row: usize,
    col: usize,
) -> (f32, f32) {
    let grid_h = rows as f32 * metrics.cell_h;
    let origin_y = content_h - TERMINAL_BOTTOM_GAP - grid_h;
    let x = col as f32 * metrics.cell_w + metrics.cell_w / 2.0;
    let y = origin_y + row as f32 * metrics.cell_h + metrics.cell_h / 2.0;
    (x, y)
}

/// Whether `got` is within `tol` of `want` on every RGB channel (alpha ignored).
pub fn channels_within(got: [u8; 4], want: (u8, u8, u8), tol: u8) -> bool {
    let d = |a: u8, b: u8| a.abs_diff(b) <= tol;
    d(got[0], want.0) && d(got[1], want.1) && d(got[2], want.2)
}

/// Read straight `[r, g, b, a]` bytes at each **logical** point of a captured
/// frame.
///
/// `raw` is a tightly-packed RGBA8 buffer, `width × height` device px, row-major
/// (exactly `RgbaImage::as_raw()` from `capture_screenshot` /
/// `Window::render_to_image()`). Each logical point is scaled to a device pixel
/// by `scale` (the window's backing scale factor) — callers lay out in logical
/// coordinates and never hardcode the backing scale. Errors if any point falls
/// outside the frame (a layout bug the caller wants surfaced, not clamped).
pub fn sample_rgba_pixels(
    raw: &[u8],
    width: u32,
    height: u32,
    logical_points: &[(f32, f32)],
    scale: f32,
) -> Result<Vec<[u8; 4]>> {
    let mut out = Vec::with_capacity(logical_points.len());
    for &(lx, ly) in logical_points {
        let dx = (lx * scale).round() as i64;
        let dy = (ly * scale).round() as i64;
        ensure!(
            dx >= 0 && dy >= 0 && (dx as u32) < width && (dy as u32) < height,
            "sample point ({lx}, {ly}) logical -> ({dx}, {dy}) device is outside the \
             {width}x{height} captured frame (scale {scale})"
        );
        let idx = ((dy as u32 * width + dx as u32) * 4) as usize;
        ensure!(
            idx + 4 <= raw.len(),
            "pixel byte index {idx} out of RGBA buffer of {} bytes ({width}x{height})",
            raw.len()
        );
        out.push([raw[idx], raw[idx + 1], raw[idx + 2], raw[idx + 3]]);
    }
    Ok(out)
}

/// Assert every `(label, want, got)` triple matches within `tol`, collecting all
/// mismatches into one error with per-channel deltas (so a failure reports every
/// off swatch at once, not just the first).
pub fn assert_channels_within(labeled: &[(String, (u8, u8, u8), [u8; 4])], tol: u8) -> Result<()> {
    let mut failures: Vec<String> = Vec::new();
    for (label, want, got) in labeled {
        if !channels_within(*got, *want, tol) {
            failures.push(format!(
                "{label}: want ({},{},{}) got ({},{},{}) [Δ {},{},{}]",
                want.0,
                want.1,
                want.2,
                got[0],
                got[1],
                got[2],
                got[0].abs_diff(want.0),
                got[1].abs_diff(want.1),
                got[2].abs_diff(want.2),
            ));
        }
    }
    ensure!(
        failures.is_empty(),
        "{} of {} sample(s) outside ±{tol}/255:\n  {}",
        failures.len(),
        labeled.len(),
        failures.join("\n  ")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within_band_is_inclusive() {
        assert!(channels_within([100, 100, 100, 255], (108, 92, 100), 8));
        assert!(!channels_within([100, 100, 100, 255], (109, 100, 100), 8));
    }

    #[test]
    fn cell_center_is_bottom_anchored() {
        // 6 rows of 16px in a 200px-tall view: grid occupies the bottom 96px, so
        // row 0 sits at the top of that block (origin_y = 200 - 96 = 104).
        let m = TerminalMetrics::new(8.0, 16.0);
        let (x, y) = cell_center(200.0, 6, m, 0, 0);
        assert_eq!(x, 4.0);
        assert_eq!(y, 104.0 + 8.0);
        // Column strides by the cell width.
        let (x2, _) = cell_center(200.0, 6, m, 0, 2);
        assert_eq!(x2, 2.0 * 8.0 + 4.0);
    }

    #[test]
    fn sample_reads_scaled_device_pixel() {
        // 2x2 logical (@2x -> 4x4 device) RGBA buffer; sample logical (0.5, 0.5)
        // -> device (1, 1).
        let w = 4u32;
        let h = 4u32;
        let mut raw = vec![0u8; (w * h * 4) as usize];
        let put = |raw: &mut [u8], x: u32, y: u32, c: [u8; 4]| {
            let i = ((y * w + x) * 4) as usize;
            raw[i..i + 4].copy_from_slice(&c);
        };
        put(&mut raw, 1, 1, [10, 20, 30, 255]);
        let got = sample_rgba_pixels(&raw, w, h, &[(0.5, 0.5)], 2.0).unwrap();
        assert_eq!(got[0], [10, 20, 30, 255]);
    }

    #[test]
    fn sample_out_of_bounds_errors() {
        let raw = vec![0u8; 4 * 4 * 4];
        assert!(sample_rgba_pixels(&raw, 4, 4, &[(100.0, 0.0)], 1.0).is_err());
    }
}
