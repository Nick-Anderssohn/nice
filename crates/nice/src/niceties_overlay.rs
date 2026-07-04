//! `niceties-overlay` self-test scenario — the T9 "Launching…" overlay timing
//! machine (R7 Validation §4).
//!
//! Two cases over the real overlay state machine + the App-Nap-safe grace
//! deadline (`crate::platform::launch_deadline`), asserted primarily via the
//! view's exposed overlay state (deterministic, feature-independent) and, when
//! the `capture` feature is compiled, corroborated with a pixel probe of the
//! accent status dot:
//!
//! * **Slow, silent pane** (`sh -c 'sleep 3; echo up'`, a **short** grace): the
//!   pane stays silent past the grace window, so the overlay shows
//!   ([`overlay_visible`] true + the accent dot on the window centre line); when
//!   `up` finally prints, the first-output event clears it (overlay gone, no
//!   accent dot).
//! * **Instant-prompt pane** (a normal `zsh -il`, the default grace): the prompt
//!   beats the grace window, so the overlay must **never** flash — the state-
//!   machine counter [`overlay_ever_visible`] stays `false`.
//!
//! Self-reported gate ([`Gate::SelfReported`]): the pass criterion is the overlay
//! state transitions, not frame cadence. The pixel corroboration is best-effort
//! (a plain build without the `selftest`/`capture` feature verifies via state
//! alone); the real on-screen look is a human capture pass.
//!
//! [`overlay_visible`]: nice_term_view::TerminalView::overlay_visible
//! [`overlay_ever_visible`]: nice_term_view::TerminalView::overlay_ever_visible

use std::time::Duration;

use anyhow::Result;
use gpui::{
    div, prelude::*, AnyWindowHandle, AsyncApp, Context, Entity, IntoElement, Render, SharedString,
    Window,
};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_term_core::{SpawnSpec, DEFAULT_SCROLLBACK_LINES};
use nice_term_view::{
    FontSettings, TerminalMetrics, TerminalSessionHandle, TerminalTheme, TerminalView,
};
use nice_theme::AccentPreset;

use crate::platform;

// -- fixed geometry (font resolution / zoom is covered by niceties-zoom) -----

const ROWS: u16 = 30;
const COLS: u16 = 100;
const FONT_FAMILY: &str = "Menlo";
const FONT_PX: f32 = 13.0;
const CELL_W: f32 = 8.0;
const CELL_H: f32 = 16.0;

/// Short grace for the slow-pane case so the overlay shows promptly (the default
/// 0.75 s is exercised by the fast-pane case, which must beat it).
const SLOW_GRACE: Duration = Duration::from_millis(400);
/// Per-channel tolerance for the accent-dot pixel probe. Loose: the only nearby
/// accent is the dot itself (the terminal bg is near-black, far from Terracotta),
/// so a loose bound cannot false-match while absorbing gamma/AA at the dot.
const ACCENT_TOL: u8 = 24;

/// A live overlay-test pane: its window + the session handle + the view (whose
/// overlay state the assertions read).
struct Pane {
    window: AnyWindowHandle,
    handle: Entity<TerminalSessionHandle>,
    terminal: Entity<TerminalView>,
}

/// The animated container: RAF each render so the view keeps painting (arming the
/// overlay deadline on the first paint and showing the overlay when it fires),
/// plus a frame stamp for the harness's per-scenario reset.
struct OverlayTermView {
    terminal: Entity<TerminalView>,
}

impl Render for OverlayTermView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        nice_harness::frame::stamp();
        window.request_animation_frame();
        div().size_full().child(self.terminal.clone())
    }
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

/// The child's grid as one newline-joined string (find first-output text like `up`).
fn grid_text(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>) -> String {
    handle.update(cx, |h, _| h.session().grid_lines().join("\n"))
}

/// The window's content viewport size in logical px (the overlay centres on it).
fn read_viewport(cx: &mut AsyncApp, window: AnyWindowHandle) -> Option<(f32, f32)> {
    window
        .update(cx, |_root, window, _app| {
            let vp = window.viewport_size();
            (f32::from(vp.width), f32::from(vp.height))
        })
        .ok()
}

fn within(p: [u8; 4], want: (u8, u8, u8), tol: u8) -> bool {
    p[0].abs_diff(want.0).max(p[1].abs_diff(want.1)).max(p[2].abs_diff(want.2)) <= tol
}

/// Best-effort accent-dot probe: sample a band across the window's vertical centre
/// (where the overlay's status dot sits) and report whether the accent colour
/// appears there. `None` when pixel capture is unavailable (a build without the
/// `capture` feature) — the caller then relies on the state assertion alone.
fn center_band_accent(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    accent: (u8, u8, u8),
) -> Option<bool> {
    let (w, h) = read_viewport(cx, window)?;
    let cy = h / 2.0;
    // Dense enough that at least one sample lands inside the ~11px dot, at three
    // y-levels straddling the centre (the dot is ~11px tall).
    let mut points: Vec<(f32, f32)> = Vec::new();
    for dy in [-4.0f32, 0.0, 4.0] {
        for i in 0..120 {
            let t = i as f32 / 119.0;
            points.push((0.2 * w + 0.6 * w * t, cy + dy));
        }
    }
    match nice_harness::capture::sample_window_pixels(window, cx, &points) {
        Ok(samples) => Some(samples.iter().any(|p| within(*p, accent, ACCENT_TOL))),
        // Capture feature off (or a read-back error): skip the corroboration.
        Err(_) => None,
    }
}

/// Build a live overlay-test pane: session + view (with the App-Nap-safe deadline
/// injected, and an optional short grace override) + its RAF-animated window.
fn build_pane(cx: &mut AsyncApp, spec: SpawnSpec, grace: Option<Duration>) -> Result<Pane> {
    let handle = TerminalSessionHandle::spawn(cx, spec, DEFAULT_SCROLLBACK_LINES)?;
    let theme = TerminalTheme::nice_default_dark();
    let accent = AccentPreset::Terracotta.color();
    let font = cx.new(|_cx| {
        FontSettings::fixed(
            SharedString::from(FONT_FAMILY),
            FONT_PX,
            TerminalMetrics::new(CELL_W, CELL_H),
        )
    });
    let terminal = {
        let handle = handle.clone();
        cx.new(move |cx| {
            let mut v = TerminalView::new(handle, theme, accent, font, cx);
            // Exercise the real spike-6 App-Nap-safe deadline (not the fallback
            // gpui timer) — the mechanism T9 is about.
            v.set_launch_deadline(platform::launch_deadline());
            if let Some(g) = grace {
                v.set_overlay_grace(g);
            }
            v
        })
    };
    let window = cx.open_window(crate::app::window_options(), {
        let terminal = terminal.clone();
        move |_window, cx| cx.new(|_cx| OverlayTermView { terminal })
    })?;
    let window: AnyWindowHandle = window.into();
    crate::app::install_present_kick(&handle, window, cx);
    Ok(Pane {
        window,
        handle,
        terminal,
    })
}

/// Open the `niceties-overlay` scenario window (the slow, silent case-A pane) and
/// spawn the overlay-timing assertions (self-reported gate). Case B (the instant-
/// prompt pane) is opened + closed inside the task.
pub fn open_niceties_overlay_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let base = std::env::temp_dir().join(format!("nice-rs-niceties-overlay-{}", std::process::id()));
    std::fs::create_dir_all(&base)?;
    let base_s = base.to_string_lossy().to_string();
    // A slow, SILENT pane: `exec sh -c 'sleep 3; echo up'` emits nothing until the
    // sleep ends (empty ZDOTDIR → no zsh startup bytes before the exec), so the
    // grace window elapses in silence and the overlay shows.
    let spec = SpawnSpec::command("sh -c 'sleep 3; echo up'".to_string(), base_s.clone())
        .with_env(vec![("ZDOTDIR".to_string(), base_s)])
        .with_size(ROWS, COLS);
    let pane = build_pane(cx, spec, Some(SLOW_GRACE))?;
    let window = pane.window;

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_niceties_overlay(acx, pane).await;
        eprintln!("[selftest] scenario 'niceties-overlay': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(window)
}

async fn run_niceties_overlay(cx: &mut AsyncApp, pane: Pane) -> CadenceReport {
    let _ = cx.update(|app| app.activate(true));
    // Past the short grace: the silent pane should now be showing the overlay.
    settle(cx, 900).await;

    let accent = AccentPreset::Terracotta.rgb8();
    let mut failures: Vec<String> = Vec::new();

    // --- Case A: overlay visible during the silent window --------------------
    let (visible, ever) = pane
        .terminal
        .update(cx, |v, _| (v.overlay_visible(), v.overlay_ever_visible()));
    if !visible {
        failures.push(
            "case A: overlay is not visible after the grace window elapsed on a silent pane"
                .to_string(),
        );
    }
    if !ever {
        failures.push("case A: overlay_ever_visible is false while the overlay should be up".into());
    }
    match center_band_accent(cx, pane.window, accent) {
        Some(true) => {
            eprintln!("[selftest] niceties-overlay case A: accent dot present at centre (visible)")
        }
        Some(false) => failures
            .push("case A: overlay visible in state but no accent dot pixel found at centre".into()),
        None => eprintln!(
            "[selftest] niceties-overlay case A: pixel capture unavailable (no selftest/capture \
             feature) — overlay verified via state machine only"
        ),
    }

    // --- Case A: first output (`up`) clears the overlay ----------------------
    let mut got_up = false;
    for _ in 0..40 {
        settle(cx, 150).await;
        if grid_text(cx, &pane.handle).contains("up") {
            got_up = true;
            break;
        }
    }
    if !got_up {
        failures.push("case A: the pane never printed `up` (sleep pane wedged?)".into());
    }
    // Let the OutputStarted event drain + clear the overlay before re-reading.
    settle(cx, 300).await;
    if pane.terminal.update(cx, |v, _| v.overlay_visible()) {
        failures.push("case A: overlay did not clear on first output".into());
    }
    match center_band_accent(cx, pane.window, accent) {
        Some(true) => failures
            .push("case A: accent dot still painted at centre after the overlay cleared".into()),
        Some(false) => eprintln!(
            "[selftest] niceties-overlay case A: no accent dot at centre after clear (gone)"
        ),
        None => {}
    }

    // --- Case B: an instant-prompt pane never flashes the overlay ------------
    let base_b =
        std::env::temp_dir().join(format!("nice-rs-niceties-overlay-fast-{}", std::process::id()));
    if std::fs::create_dir_all(&base_b).is_ok() {
        let base_b_s = base_b.to_string_lossy().to_string();
        // A normal login shell at the DEFAULT grace (0.75 s): its prompt prints
        // well within the grace, so the overlay must never become visible.
        let spec_fast = SpawnSpec::shell(base_b_s.clone())
            .with_env(vec![("ZDOTDIR".to_string(), base_b_s)])
            .with_size(ROWS, COLS);
        match build_pane(cx, spec_fast, None) {
            Ok(pane_b) => {
                let _ = cx.update(|app| app.activate(true));
                // Well past the default grace: if the overlay were going to flash,
                // it would have by now.
                settle(cx, 1200).await;
                let grid_nonempty = grid_text(cx, &pane_b.handle)
                    .chars()
                    .any(|c| !c.is_whitespace());
                let ever_b = pane_b.terminal.update(cx, |v, _| v.overlay_ever_visible());
                if !grid_nonempty {
                    failures.push(
                        "case B: zsh never printed a prompt — cannot conclude the overlay was \
                         skipped (rather than merely pending)"
                            .into(),
                    );
                }
                if ever_b {
                    failures.push(
                        "case B: the overlay flashed for an instant-prompt pane \
                         (overlay_ever_visible is true; first output should have beaten the grace)"
                            .into(),
                    );
                }
                // The harness only manages the primary (case-A) window — close ours.
                let _ = pane_b
                    .window
                    .update(cx, |_v, window, _app| window.remove_window());
            }
            Err(e) => failures.push(format!("case B: could not open the fast-pane window: {e}")),
        }
    } else {
        failures.push("case B: could not create the fast-pane temp dir".into());
    }

    build_report(failures)
}

fn build_report(failures: Vec<String>) -> CadenceReport {
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: format!(
                "launch overlay OK: a silent pane shows the overlay past a {}ms grace and clears \
                 it on first output; an instant-prompt pane never flashes it (state-machine \
                 counter). Pixel corroboration best-effort (capture-feature builds).",
                SLOW_GRACE.as_millis()
            ),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} niceties-overlay assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
