//! Visual exemplar harness proof — **execution model: real-MacPlatform
//! [`gpui::VisualTestAppContext`], run in a `harness = false` main-thread
//! binary, serially.**
//!
//! Real NSWindows are main-thread-only and libtest runs every case on a worker
//! thread (and there is no `#[gpui::visual_test]` macro at the pin), so a
//! pixel/screenshot test cannot be a libtest case — it owns the platform in
//! `fn main` on the process main thread and exits nonzero on failure.
//! `cargo test -p nice-itests` builds and runs this binary and gates on its exit
//! code, exactly like a libtest case.
//!
//! **Templates: a future test that mounts a real gpui view, feeds it a fixture,
//! and asserts the rendered screenshot pixel-for-pixel.** This one mounts a real
//! [`nice_term_view::TerminalView`] over a `cat` fixture session that paints a
//! truecolor swatch row, forces a repaint, captures the off-screen window's
//! drawable via `capture_screenshot`, and asserts each swatch cell's centre
//! matches its emitted colour within the shared `±8/255` band. It asserts
//! **pixels only** — never cadence/perf/timing (that is the live suite's job).
//!
//! The window is off-screen (−10000,−10000, `focus: false`), so the run steals no
//! focus and prompts no TCC (`capture_screenshot` reads the Metal drawable
//! directly — no ScreenCaptureKit). The reusable pixel-sampling / geometry /
//! fixture helpers come from the crate lib (`nice_itests::{pixels, session}`);
//! only the ~10-line `VisualTestAppContext` boot is inline here, because the lib's
//! behavior fixtures are `test-support`-gated and a downstream visual binary
//! writes its own boot the same way (it declares its own `gpui_platform`
//! `test-support` dev-dep).

use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use gpui::{div, prelude::*, px, size, Context, Entity, SharedString, VisualTestAppContext, Window};

use nice_itests::{pixels, session};
use nice_term_core::DEFAULT_SCROLLBACK_LINES;
use nice_term_view::{
    FontSettings, TerminalMetrics, TerminalSessionHandle, TerminalTheme, TerminalView,
};
use nice_theme::AccentPreset;

const ROWS: u16 = 6;
const COLS: u16 = 20;
const CELL_W: f32 = 8.0;
const CELL_H: f32 = 16.0;
const SWATCH_ROW: usize = 0;
/// Distinct, clearly-non-default-background truecolors, one per cell.
const SWATCHES: [(u8, u8, u8); 3] = [(220, 50, 47), (0, 200, 0), (30, 144, 255)];
/// Bounded readiness wait: the feeder parses `cat`'s bytes on an OS thread and the
/// compositor needs a forced repaint. Poll the captured pixels, fail loud on
/// timeout. Not a timing assertion.
const RENDER_TIMEOUT: Duration = Duration::from_secs(8);

/// The minimal root view hosting the terminal (same shape as the behavior
/// fixtures' `FixtureRoot`, re-declared here because that one is `test-support`-
/// gated and invisible to this binary).
struct FixtureRoot {
    terminal: Entity<TerminalView>,
}

impl Render for FixtureRoot {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.terminal.clone())
    }
}

fn main() {
    match run() {
        Ok(()) => {
            println!("VISUAL EXEMPLAR PASS visual_terminal_screenshot");
        }
        Err(e) => {
            eprintln!("VISUAL EXEMPLAR FAIL visual_terminal_screenshot: {e:#}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<()> {
    let dir = session::temp_dir("visual")?;
    let fixture_bytes = session::bg_swatch_row(SWATCH_ROW, &SWATCHES);
    let fixture = session::write_fixture(&dir, "fixture.bin", &fixture_bytes)?;
    let spec = session::cat_fixture_spec(&dir, &fixture, ROWS, COLS);

    // Boot the real-MacPlatform visual context on the main thread.
    let mut cx = VisualTestAppContext::new(gpui_platform::current_platform(false));

    let metrics = TerminalMetrics::new(CELL_W, CELL_H);
    let handle = TerminalSessionHandle::spawn(&mut cx, spec, DEFAULT_SCROLLBACK_LINES)?;
    // Deterministic scheduling here too (VisualTestAppContext drives a
    // TestDispatcher), so opt out of the event-driven drain wake before the first
    // `run_until_parked`: the pty feeder thread must not wake a gpui task under
    // the test scheduler. The renderer reads the shared `Term` directly and the
    // loop below forces its own `refresh()`, so the drain is not relied upon.
    // See `TerminalSessionHandle::set_event_wake_enabled`.
    handle.update(&mut cx, |h, _cx| h.set_event_wake_enabled(false));
    let theme = TerminalTheme::nice_default_dark();
    let accent = AccentPreset::Terracotta.color();
    let font = cx.new(|_cx| FontSettings::fixed(SharedString::from("Menlo"), 13.0, metrics));
    let terminal = cx.new(|c| TerminalView::new(handle.clone(), theme, accent, font, c));

    let window = cx.open_offscreen_window(size(px(320.0), px(240.0)), {
        let terminal = terminal.clone();
        move |_window, app| app.new(|_cx| FixtureRoot { terminal })
    })?;
    let window = window.into();

    // Resolve the sample points from the live content height (T4 bottom-anchored
    // layout) so they land where the swatch row actually paints.
    let (content_h, scale) = cx.update_window(window, |_, w, _| {
        (f32::from(w.viewport_size().height), w.scale_factor())
    })?;
    let points: Vec<(f32, f32)> = (0..SWATCHES.len())
        .map(|col| pixels::cell_center(content_h, ROWS as usize, metrics, SWATCH_ROW, col))
        .collect();

    // Poll: force a repaint and capture until the swatch pixels have rendered
    // (feeder parses on a real thread), up to the timeout.
    let deadline = Instant::now() + RENDER_TIMEOUT;
    let mut last: Option<Vec<[u8; 4]>> = None;
    loop {
        cx.run_until_parked();
        let _ = cx.update_window(window, |_, w, _| w.refresh());
        cx.run_until_parked();

        if let Ok(img) = cx.capture_screenshot(window) {
            if let Ok(samples) =
                pixels::sample_rgba_pixels(img.as_raw(), img.width(), img.height(), &points, scale)
            {
                let matched = SWATCHES
                    .iter()
                    .zip(&samples)
                    .all(|(want, got)| pixels::channels_within(*got, *want, pixels::DEFAULT_PIXEL_TOLERANCE));
                if matched {
                    let labeled: Vec<(String, (u8, u8, u8), [u8; 4])> = SWATCHES
                        .iter()
                        .zip(&samples)
                        .enumerate()
                        .map(|(i, (want, got))| (format!("swatch[{i}]"), *want, *got))
                        .collect();
                    // A final labeled assert makes any late regression legible.
                    pixels::assert_channels_within(&labeled, pixels::DEFAULT_PIXEL_TOLERANCE)?;
                    return Ok(());
                }
                last = Some(samples);
            }
        }

        if Instant::now() >= deadline {
            bail!(
                "swatch pixels never matched within ±{}/255 in {:?}; last sampled = {:?}, expected = {:?}",
                pixels::DEFAULT_PIXEL_TOLERANCE,
                RENDER_TIMEOUT,
                last,
                SWATCHES
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
