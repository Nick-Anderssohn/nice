//! App module — owns window creation and the root view.
//!
//! Two entry points share one window layout:
//!   * [`run`] — the normal app: one static "Nice RS Dev" window painting a
//!     solid background + the version line.
//!   * [`run_selftest`] — the `NICE_RS_SELFTEST` harness path: the same window,
//!     but the root view animates (stamps a frame + repaints every tick) so the
//!     harness can measure frame cadence. Scenario orchestration, the cadence
//!     gate, capture, and the watchdog all live in `nice_harness::selftest`;
//!     this module only supplies the concrete gpui view + window.
//!
//! Later cycles slot richer chrome into `RootView` (real title bar is R9) and
//! register more scenarios in [`selftest_scenarios`].

use std::io::Write;
use std::time::Duration;

use anyhow::Result;
use gpui::{
    div, point, prelude::*, px, rgb, size, AnyWindowHandle, App, AppContext, AsyncApp, Bounds,
    Context, IntoElement, Render, Rgba, TitlebarOptions, Window, WindowBackgroundAppearance,
    WindowBounds, WindowKind, WindowOptions,
};

use nice_harness::selftest::Scenario;
use nice_theme::color::Srgba;
use nice_theme::palette::{slots, ColorScheme, Palette, SlotColor};
use nice_theme::AccentPreset;

/// The application's root view: a solid background with one line of text (the
/// version string). In self-test mode it also drives a continuous animated
/// repaint and stamps each frame for the cadence gate.
struct RootView {
    /// When true, stamp a frame + request the next animation frame on every
    /// render (the self-test measurement loop). When false (the shipped app),
    /// paint once and stay static.
    animated: bool,
    frame: u64,
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // Self-test mode: bracket the frame with an os_signpost interval, stamp
        // the frame clock, and keep the composite running via RAF. The interval
        // covers element construction (paint happens later in the pipeline);
        // later cycles wanting present-complete intervals hook the renderer.
        let signpost = if self.animated {
            let id = nice_harness::signpost::frame_begin();
            nice_harness::frame::stamp();
            self.frame += 1;
            window.request_animation_frame();
            Some(id)
        } else {
            None
        };

        // A moving accent bar so each animated frame genuinely differs (real
        // per-frame compositing work, and a non-uniform screenshot capture).
        let accent_x = 40.0 + ((self.frame % 200) as f64) * 1.5;
        let version = concat!("Nice RS Dev v", env!("CARGO_PKG_VERSION"));

        let element = div()
            .size_full()
            .bg(rgb(0x11141b))
            .text_color(rgb(0xe6e9ef))
            .font_family("Helvetica")
            .child(
                div()
                    .absolute()
                    .top(px(80.0))
                    .left(px(accent_x as f32))
                    .w(px(120.0))
                    .h(px(6.0))
                    .rounded(px(3.0))
                    .bg(rgb(0x6e59f5)),
            )
            .child(
                div()
                    .absolute()
                    .top(px(140.0))
                    .left(px(40.0))
                    .text_xl()
                    .child(version),
            );

        if let Some(id) = signpost {
            nice_harness::signpost::frame_end(id);
        }
        element
    }
}

/// Fixed, sensible default window geometry + chrome defaults (real chrome is
/// R9). Shared by the shipped window and every self-test scenario window.
fn window_options() -> WindowOptions {
    let bounds = Bounds {
        origin: point(px(160.0), px(160.0)),
        size: size(px(960.0), px(640.0)),
    };
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_background: WindowBackgroundAppearance::Opaque,
        titlebar: Some(TitlebarOptions {
            title: Some("Nice RS Dev".into()),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        kind: WindowKind::Normal,
        is_resizable: true,
        focus: true,
        show: true,
        ..Default::default()
    }
}

/// Run the shipped application: one static window, quit on window close.
pub fn run() {
    gpui_platform::application().run(|cx: &mut App| {
        cx.activate(true);
        cx.on_window_closed(|cx, _id| cx.quit()).detach();
        if let Err(e) = cx
            .open_window(window_options(), |_window, cx| {
                cx.new(|_cx| RootView {
                    animated: false,
                    frame: 0,
                })
            })
        {
            eprintln!("nice-rs: failed to open window: {e:#}");
            std::process::exit(1);
        }
    });
}

/// Open the self-test scenario window (animated root view). Handed to the
/// harness as a [`Scenario`] opener.
fn open_selftest_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let handle = cx.open_window(window_options(), |_window, cx| {
        cx.new(|_cx| RootView {
            animated: true,
            frame: 0,
        })
    })?;
    Ok(handle.into())
}

// ---------------------------------------------------------------------------
// `tokens` self-test scenario — the design-token render gate (R2).
//
// Renders a deterministic swatch grid from the nice-theme tokens, then reads
// each swatch centre back through `Window::render_to_image()` and asserts it
// matches the token's sRGB value within a per-channel tolerance. This proves the
// tokens survive the trip through gpui's fill pipeline + Metal compositing, not
// just unit arithmetic. The pixel read-back is gated behind the app's
// `selftest` feature (same `render_to_image` path as `NICE_RS_CAPTURE`); without
// it the read-back bails and the scenario FAILs.
//
// Contract note: the `Scenario` shape ({ name, open }) and the driver are
// unchanged. The scenario samples pixels and hard-exits nonzero on mismatch
// itself (from the spawned task below); on success it returns quietly and the
// unchanged driver prints `SELFTEST PASS tokens`.
// ---------------------------------------------------------------------------

/// Backdrop painted under the swatches (the shipped app's dark background). Each
/// swatch overpaints its own cell, so this only shows through the gaps and never
/// affects a sampled centre.
const TOKENS_BACKDROP: u32 = 0x11141b;
/// Swatch grid layout in logical `px`: a `TOKENS_COLS`-wide grid of opaque
/// colour cells inset from the content-view top-left.
const TOKENS_COLS: usize = 4;
const TOKENS_MARGIN: f32 = 24.0;
const TOKENS_SWATCH_W: f32 = 140.0;
const TOKENS_SWATCH_H: f32 = 90.0;
const TOKENS_GAP: f32 = 16.0;
/// Y of the per-frame moving marker — below all four swatch rows (row 3's bottom
/// is `24 + 3*(90+16) + 90 = 432`), so it never overlaps a sampled centre.
const TOKENS_MARKER_Y: f32 = 440.0;
/// Per-channel tolerance (out of 255) for the sampled-vs-token comparison.
/// Covers gpui's u8 → Hsla fill round-trip (~±1/255) plus aa-gamma compositing —
/// the threshold the plan fixes at ±8/255.
const TOKENS_CHANNEL_TOLERANCE: u8 = 8;

/// One deterministic swatch: a label (diagnostics only) and the token colour it
/// paints. Only rgb is asserted — see the opaque paint at the render site.
#[derive(Clone, Copy)]
struct Swatch {
    label: &'static str,
    color: Srgba,
}

/// Top-left logical origin of swatch `i` (row-major, `TOKENS_COLS` per row).
fn swatch_origin(i: usize) -> (f32, f32) {
    let col = (i % TOKENS_COLS) as f32;
    let row = (i / TOKENS_COLS) as f32;
    (
        TOKENS_MARGIN + col * (TOKENS_SWATCH_W + TOKENS_GAP),
        TOKENS_MARGIN + row * (TOKENS_SWATCH_H + TOKENS_GAP),
    )
}

/// Logical centre of swatch `i` — the point the assertion samples.
fn swatch_center(i: usize) -> (f32, f32) {
    let (x, y) = swatch_origin(i);
    (x + TOKENS_SWATCH_W / 2.0, y + TOKENS_SWATCH_H / 2.0)
}

/// Quantise a gamma-encoded sRGB channel (`0.0..=1.0`) to 8-bit, matching how a
/// captured pixel is stored.
fn to_u8(c: f32) -> u8 {
    (c * 255.0).round().clamp(0.0, 255.0) as u8
}

/// The swatch set the `tokens` scenario renders and asserts: every slot of one
/// concrete palette × scheme (Nice / Dark — the combo whose slots are all sRGB
/// literals, with no paint-time macOS system colours) followed by the five
/// accent presets. 11 + 5 = 16 swatches, exactly filling the 4×4 grid.
fn tokens_swatches() -> Vec<Swatch> {
    let s = slots(Palette::Nice, ColorScheme::Dark)
        .expect("Nice + Dark is a valid palette/scheme combo");
    let palette_slots: [(&'static str, SlotColor); 11] = [
        ("background", s.background),
        ("background2", s.background2),
        ("background3", s.background3),
        ("panel", s.panel),
        ("ink", s.ink),
        ("ink2", s.ink2),
        ("ink3", s.ink3),
        ("line", s.line),
        ("line_strong", s.line_strong),
        ("user_bubble", s.user_bubble),
        ("chrome", s.chrome),
    ];

    let mut swatches = Vec::with_capacity(palette_slots.len() + AccentPreset::ALL.len());
    for (label, slot) in palette_slots {
        let color = match slot {
            SlotColor::Srgb(c) => c,
            // Nice/Dark carries no system slots; guard so a future palette swap
            // that introduces one fails loudly instead of asserting a colour we
            // cannot resolve here without NSColor.
            SlotColor::System { .. } => {
                panic!("tokens scenario expects only sRGB slots; '{label}' is a system slot")
            }
        };
        swatches.push(Swatch { label, color });
    }
    for preset in AccentPreset::ALL {
        swatches.push(Swatch {
            label: preset.raw_value(),
            color: preset.color(),
        });
    }
    swatches
}

/// The `tokens` scenario's root view: the deterministic swatch grid. Animates
/// like every scenario (frame stamp + RAF) so the driver's cadence gate applies,
/// but the swatches themselves stay put so their centres are stable to sample.
struct SwatchGridView {
    animated: bool,
    frame: u64,
    swatches: Vec<Swatch>,
}

impl Render for SwatchGridView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // Cadence instrumentation, identical to `RootView`: bracket the frame,
        // stamp the clock, and keep compositing via RAF on a frontmost window.
        let signpost = if self.animated {
            let id = nice_harness::signpost::frame_begin();
            nice_harness::frame::stamp();
            self.frame += 1;
            window.request_animation_frame();
            Some(id)
        } else {
            None
        };

        let mut root = div().size_full().bg(rgb(TOKENS_BACKDROP));
        for (i, sw) in self.swatches.iter().enumerate() {
            let (x, y) = swatch_origin(i);
            root = root.child(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(TOKENS_SWATCH_W))
                    .h(px(TOKENS_SWATCH_H))
                    // Token → gpui::Rgba adapter: paint OPAQUE (alpha forced to
                    // 1) so the sampled centre pixel is the token's straight rgb,
                    // not a blend over the backdrop. A token's own alpha (the
                    // translucent `chrome` slot) is covered by nice-theme's unit
                    // tests, not by this pixel read-back.
                    .bg(Rgba {
                        r: sw.color.r,
                        g: sw.color.g,
                        b: sw.color.b,
                        a: 1.0,
                    }),
            );
        }

        // A small moving marker BELOW the swatch rows so each animated frame
        // genuinely differs (real per-frame compositing work) without ever
        // touching a swatch centre the assertion samples.
        let marker_x = TOKENS_MARGIN + ((self.frame % 200) as f32) * 1.5;
        root = root.child(
            div()
                .absolute()
                .top(px(TOKENS_MARKER_Y))
                .left(px(marker_x))
                .w(px(80.0))
                .h(px(4.0))
                .rounded(px(2.0))
                .bg(rgb(0x6e59f5)),
        );

        if let Some(id) = signpost {
            nice_harness::signpost::frame_end(id);
        }
        root
    }
}

/// Open the `tokens` scenario window (the swatch grid) and spawn its pixel
/// assertion. The spawned task reads each swatch centre back shortly after first
/// paint and hard-exits nonzero on any out-of-tolerance channel; on success it
/// returns quietly so the unchanged driver prints `SELFTEST PASS tokens`.
fn open_tokens_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let swatches = tokens_swatches();
    let handle = cx.open_window(window_options(), {
        let swatches = swatches.clone();
        move |_window, cx| {
            cx.new(move |_cx| SwatchGridView {
                animated: true,
                frame: 0,
                swatches,
            })
        }
    })?;
    let handle: AnyWindowHandle = handle.into();

    cx.spawn(async move |acx: &mut AsyncApp| {
        // Sample well inside the driver's 0.5s warm-up: the window has painted
        // the grid by now, and this single read-back lands before the
        // measurement window opens, so it can't perturb the cadence percentiles.
        acx.background_executor()
            .timer(Duration::from_millis(250))
            .await;
        if let Err(e) = assert_tokens(handle, acx, &swatches) {
            eprintln!("SELFTEST FAIL tokens: {e:#}");
            println!("SELFTEST FAIL tokens");
            let _ = std::io::stdout().flush();
            std::process::exit(1);
        }
    })
    .detach();

    Ok(handle)
}

/// Read each swatch centre back and compare to its token colour within
/// [`TOKENS_CHANNEL_TOLERANCE`] per rgb channel. Diagnostics name the offending
/// swatch and its channel deltas. Errors (including the feature-off read-back
/// bail) propagate to the caller, which turns them into `SELFTEST FAIL tokens`.
fn assert_tokens(handle: AnyWindowHandle, cx: &mut AsyncApp, swatches: &[Swatch]) -> Result<()> {
    let points: Vec<(f32, f32)> = (0..swatches.len()).map(swatch_center).collect();
    let samples = nice_harness::capture::sample_window_pixels(handle, cx, &points)?;

    let mut failures = Vec::new();
    for (sw, got) in swatches.iter().zip(samples.iter()) {
        let want = [to_u8(sw.color.r), to_u8(sw.color.g), to_u8(sw.color.b)];
        let dr = want[0].abs_diff(got[0]);
        let dg = want[1].abs_diff(got[1]);
        let db = want[2].abs_diff(got[2]);
        if dr.max(dg).max(db) > TOKENS_CHANNEL_TOLERANCE {
            failures.push(format!(
                "'{}': want rgb({},{},{}) got rgb({},{},{}) (Δ {},{},{} > {})",
                sw.label, want[0], want[1], want[2], got[0], got[1], got[2], dr, dg, db,
                TOKENS_CHANNEL_TOLERANCE,
            ));
        }
    }

    if failures.is_empty() {
        eprintln!(
            "[selftest] scenario 'tokens': all {} swatches within ±{}/255",
            swatches.len(),
            TOKENS_CHANNEL_TOLERANCE
        );
        Ok(())
    } else {
        anyhow::bail!(
            "{} of {} swatch(es) out of tolerance:\n  {}",
            failures.len(),
            swatches.len(),
            failures.join("\n  ")
        )
    }
}

/// The scenario registry the harness iterates. Later cycles push more
/// [`Scenario`]s here (terminal streaming, input latency, …); the `smoke`
/// scenario is the minimal "the window opens and paints at a sane cadence" gate,
/// and `tokens` is the design-token render gate (R2).
pub fn selftest_scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "smoke",
            open: open_selftest_window,
        },
        Scenario {
            name: "tokens",
            open: open_tokens_window,
        },
    ]
}

/// Run the `NICE_RS_SELFTEST` harness path inside one `Application::run`.
pub fn run_selftest(selector: String) {
    let scenarios = selftest_scenarios();
    gpui_platform::application().run(move |cx: &mut App| {
        nice_harness::selftest::drive(cx, &selector, scenarios);
    });
}
