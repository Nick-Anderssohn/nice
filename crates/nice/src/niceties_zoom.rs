//! `niceties-zoom` self-test scenario — the T11 live zoom + pty re-metric path
//! (R7 Validation §2).
//!
//! Drives the shipped ⌘+/⌘−/⌘0 zoom keybindings with **real CGEvents** posted to
//! nice's own pid (`crate::platform`, the same edge the R5 `input-*`
//! scenarios use) over a real login shell, and asserts the whole T11 chain end
//! to end:
//!
//! 1. **cell metrics grow** — the shared [`FontSettings`] the view observes
//!    reports a larger point size + cell box after ⌘+ ×3 (read through the
//!    entity, the plan's "exposed metrics" option);
//! 2. **the pty re-metrics** — the view recomputes the grid to fill the window at
//!    the new cell size and pushes `(rows, cols)` to the pty via the R3/R4 resize
//!    path; asserted two ways: the core `Term`'s grid dimensions equal the
//!    independently-computed [`fit_grid`], and `stty size` run in the child prints
//!    those same dimensions (proving the winsize / SIGWINCH reached the shell);
//! 3. **⌘0 restores the baseline exactly** — point size, cell metrics, and the
//!    fitted grid return to their pre-zoom values.
//!
//! It self-reports its verdict ([`Gate::SelfReported`]) like the `input-*`
//! scenarios: the pass criterion is these state assertions, not frame cadence.
//! Accessibility (TCC) is preflighted and a missing grant FAILs loudly (a
//! silently-dropped CGEvent would make every zoom a no-op).

use std::time::Duration;

use anyhow::Result;
use gpui::{
    div, prelude::*, AnyWindowHandle, AsyncApp, Context, Entity, IntoElement, Render, Window,
};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_term_core::{SpawnSpec, DEFAULT_SCROLLBACK_LINES};
use nice_term_view::{
    fit_grid, snap_metrics_to_scale, FontSettings, TerminalMetrics, TerminalSessionHandle,
    TerminalTheme, TerminalView,
    DEFAULT_TERMINAL_FONT_PX,
};
use nice_theme::AccentPreset;

use crate::platform;

// -- fixed pane geometry ----------------------------------------------------

/// Initial spawn grid. Arbitrary (the first zoom re-fits it to the window); a
/// mid-size that the shell comes up in cleanly.
const ROWS: u16 = 30;
const COLS: u16 = 100;

// macOS virtual keycodes (`CGKeyCode`) for the zoom chords.
const KC_EQUAL: u16 = 24; // kVK_ANSI_Equal → ⌘= (zoom in)
const KC_ZERO: u16 = 29; // kVK_ANSI_0 → ⌘0 (reset)

/// Points ⌘+ is pressed (Validation §2: "post ⌘+ CGEvents ×3").
const ZOOM_IN_STEPS: usize = 3;

/// Accessibility-grant remediation (shared wording with the `input-*`
/// scenarios): without the TCC grant `CGEventPostToPid` is silently dropped, so
/// every synthetic zoom chord is a no-op and the scenario can never pass.
const ACCESSIBILITY_REMEDIATION: &str = "\
Accessibility (TCC) grant missing: AXIsProcessTrusted() == false, so \
CGEventPostToPid is SILENTLY DROPPED and no injected ⌘+/⌘0 can reach the window. \
Fix: System Settings → Privacy & Security → Accessibility → enable the process \
hosting this run. If it shows ON but this persists, the grant is STALE — remove \
it with '-' and re-add it, then re-run. Verify: swift -e 'import \
ApplicationServices; print(AXIsProcessTrusted())'";

/// The animated container hosting the live [`TerminalView`]: it requests the next
/// animation frame every render so the view keeps painting (publishing fresh
/// element bounds the re-metric fit reads) while the driver posts events, and
/// stamps a frame so the harness's per-scenario reset stays consistent.
struct ZoomTermView {
    terminal: Entity<TerminalView>,
}

impl Render for ZoomTermView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        nice_harness::frame::stamp();
        window.request_animation_frame();
        div().size_full().child(self.terminal.clone())
    }
}

/// Open the `niceties-zoom` scenario window (a real `zsh -il`) and spawn the
/// CGEvent-driven zoom + re-metric assertions (self-reported gate).
pub fn open_niceties_zoom_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let base = std::env::temp_dir().join(format!("nice-niceties-zoom-{}", std::process::id()));
    std::fs::create_dir_all(&base)?;
    let base_s = base.to_string_lossy().to_string();
    // A real login shell, user rc suppressed via an empty ZDOTDIR so the grid is
    // predictable and `stty size` output is easy to find.
    let spec = SpawnSpec::shell(base_s.clone())
        .with_env(vec![("ZDOTDIR".to_string(), base_s)])
        .with_size(ROWS, COLS);

    let handle = TerminalSessionHandle::spawn(cx, spec, DEFAULT_SCROLLBACK_LINES)?;
    let theme = TerminalTheme::nice_default_dark();
    let accent = AccentPreset::Terracotta.color();

    // R12: install the app-wide shortcut keymap and take its process-level,
    // RESOLVED font entity (the shipped SF Mono → JetBrains Mono NL → system-mono
    // chain, metrics derived from the resolved font). The ⌘=/⌘0 chords this
    // scenario posts are now handled by the app's IncreaseFontSize / ResetFontSizes
    // actions (they mutate this shared entity), NOT the old view-local zoom chord —
    // so this exercises the migrated end-to-end path (Validation §6). The view
    // below observes the same entity, so a zoom re-metrics it and resizes the pty.
    let font = cx.update(|app| {
        crate::keymap::install_shortcuts(app);
        crate::keymap::shared_font_settings(app)
    });
    let terminal = {
        let font = font.clone();
        let handle = handle.clone();
        cx.new(move |cx| {
            let mut v = TerminalView::new(handle, theme, accent, font, cx);
            v.set_keycode_probe(std::sync::Arc::new(platform::current_event_keycode));
            v
        })
    };

    let window = cx.open_window(crate::app::window_options(), {
        let terminal = terminal.clone();
        move |_window, cx| cx.new(|_cx| ZoomTermView { terminal })
    })?;
    let window: AnyWindowHandle = window.into();
    crate::app::install_present_kick(&handle, window, cx);

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_niceties_zoom(acx, window, handle, font).await;
        eprintln!("[selftest] scenario 'niceties-zoom': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(window)
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

/// Post one zoom chord (⌘ + `keycode`) to `pid`, then yield so AppKit dispatches
/// it into the window before the next event. No unicode override — gpui derives
/// the key from the hardware keycode + the ⌘ layout (`chars_for_modified_key`).
async fn tap_cmd(cx: &mut AsyncApp, pid: i32, keycode: u16) {
    platform::post_key_tap(pid, keycode, platform::FLAG_COMMAND, None);
    settle(cx, 120).await;
}

/// Read the shared font state's current `(px, metrics)`.
fn read_font(cx: &mut AsyncApp, font: &Entity<FontSettings>) -> (f32, TerminalMetrics) {
    font.update(cx, |f, _| (f.px(), f.metrics()))
}

/// Read the core `Term`'s current grid dimensions `(rows, cols)`, or `None` if
/// the (deferred) session has not spawned.
fn read_dims(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>) -> Option<(u16, u16)> {
    handle.update(cx, |h, _| h.session().dimensions())
}

/// The window's content viewport size in logical px + its backing scale — the
/// reference the view's re-metric fit uses (its element fills the content
/// area), so [`fit_grid`] over it reproduces the grid the view resized the pty
/// to. The scale matters because the view fits at the DEVICE-SNAPPED cell box
/// ([`snap_metrics_to_scale`]); on a 2× display the snap is a no-op, but on a
/// 1× display the shared `FontSettings` box alone would predict too many
/// columns.
fn read_viewport(cx: &mut AsyncApp, window: AnyWindowHandle) -> Option<(f32, f32, f32)> {
    window
        .update(cx, |_root, window, _app| {
            let vp = window.viewport_size();
            (f32::from(vp.width), f32::from(vp.height), window.scale_factor())
        })
        .ok()
}

/// The child's grid as text (each visible row joined by newlines) — used to find
/// the `stty size` echo.
fn grid_text(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>) -> String {
    handle.update(cx, |h, _| h.session().grid_lines().join("\n"))
}

async fn run_niceties_zoom(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    handle: Entity<TerminalSessionHandle>,
    font: Entity<FontSettings>,
) -> CadenceReport {
    // Frontmost/key + painted once (registers the input handler) before events.
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 700).await;

    if !platform::accessibility_trusted() {
        return CadenceReport::error(ACCESSIBILITY_REMEDIATION.to_string());
    }

    // Wait for `zsh -il` to print its prompt before zooming — the SIGWINCH from
    // the re-metric must reach a live shell, and `stty size` needs a running one.
    let mut ready = false;
    for _ in 0..50 {
        settle(cx, 150).await;
        if grid_text(cx, &handle)
            .chars()
            .any(|c| !c.is_whitespace())
        {
            ready = true;
            break;
        }
    }
    if !ready {
        return CadenceReport::error(
            "niceties-zoom: zsh never printed a prompt (grid stayed blank) — cannot drive zoom"
                .to_string(),
        );
    }
    // Re-assert frontmost/key right before the first chord so the CGEvents route.
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 250).await;

    let pid = std::process::id() as i32;
    let mut failures: Vec<String> = Vec::new();

    // --- Baseline (default size) ------------------------------------------
    let (px0, m0) = read_font(cx, &font);
    let Some((vp_w, vp_h, win_scale)) = read_viewport(cx, window) else {
        return CadenceReport::error(
            "niceties-zoom: could not read the window viewport".to_string(),
        );
    };
    if px0 != DEFAULT_TERMINAL_FONT_PX {
        failures.push(format!(
            "baseline size is {px0} pt, expected the default {DEFAULT_TERMINAL_FONT_PX} pt"
        ));
    }

    // --- Zoom in ×3 (⌘+) ---------------------------------------------------
    for _ in 0..ZOOM_IN_STEPS {
        tap_cmd(cx, pid, KC_EQUAL).await;
    }
    settle(cx, 300).await;
    let (px1, m1) = read_font(cx, &font);
    let Some(dims1) = read_dims(cx, &handle) else {
        return CadenceReport::error(
            "niceties-zoom: session not spawned (no grid dimensions) after zoom".to_string(),
        );
    };
    let expect1 = fit_grid(vp_w, vp_h, snap_metrics_to_scale(m1, win_scale));

    // Size stepped up by exactly ZOOM_IN_STEPS points.
    let want_px1 = px0 + ZOOM_IN_STEPS as f32;
    if px1 != want_px1 {
        failures.push(format!(
            "after ⌘+ ×{ZOOM_IN_STEPS}: size is {px1} pt, expected {want_px1} pt"
        ));
    }
    // Cell metrics grew on both axes.
    if !(m1.cell_w > m0.cell_w && m1.cell_h > m0.cell_h) {
        failures.push(format!(
            "cell metrics did not grow: baseline {:.3}×{:.3}, after zoom {:.3}×{:.3}",
            m0.cell_w, m0.cell_h, m1.cell_w, m1.cell_h
        ));
    }
    // The pty re-metriced to fill the window at the new cell box.
    if dims1 != expect1 {
        failures.push(format!(
            "grid did not re-fit after zoom: core Term is {}×{} (rows×cols), expected fit \
             {}×{} for a {:.0}×{:.0} viewport at cell {:.3}×{:.3}",
            dims1.0, dims1.1, expect1.0, expect1.1, vp_w, vp_h, m1.cell_w, m1.cell_h
        ));
    }
    // …and the grid actually changed (guards a coincidental equal-to-spawn fit).
    let dims0 = (ROWS, COLS);
    if dims1 == dims0 {
        failures.push(format!(
            "grid unchanged after zoom (still {}×{}) — the re-metric did not resize the pty",
            dims1.0, dims1.1
        ));
    }

    // SIGWINCH reached the child: `stty size` prints the new winsize. Injected on
    // the shell's stdin (the re-metric already set the pty winsize; stty reads it
    // back via TIOCGWINSZ), so this proves the resize propagated past the Term.
    let _ = handle.update(cx, |h, _| h.session().write_input(b"stty size\n"));
    settle(cx, 600).await;
    let want_stty = format!("{} {}", dims1.0, dims1.1);
    let grid = grid_text(cx, &handle);
    if !grid.contains(&want_stty) {
        failures.push(format!(
            "`stty size` did not report the resized winsize '{want_stty}' — SIGWINCH / winsize \
             did not reach the child. Grid:\n{grid}"
        ));
    }

    // --- Reset (⌘0) restores the baseline exactly -------------------------
    tap_cmd(cx, pid, KC_ZERO).await;
    settle(cx, 300).await;
    let (px2, m2) = read_font(cx, &font);
    let dims2 = read_dims(cx, &handle).unwrap_or((0, 0));
    let expect2 = fit_grid(vp_w, vp_h, snap_metrics_to_scale(m0, win_scale));

    if px2 != px0 {
        failures.push(format!(
            "⌘0 did not restore the baseline size: {px2} pt, expected {px0} pt"
        ));
    }
    if m2 != m0 {
        failures.push(format!(
            "⌘0 did not restore the baseline metrics exactly: {:.3}×{:.3}, expected {:.3}×{:.3}",
            m2.cell_w, m2.cell_h, m0.cell_w, m0.cell_h
        ));
    }
    if dims2 != expect2 {
        failures.push(format!(
            "grid did not re-fit to the baseline after ⌘0: core Term is {}×{}, expected {}×{}",
            dims2.0, dims2.1, expect2.0, expect2.1
        ));
    }

    build_report(failures, px0, px1, m0, m1, dims1)
}

fn build_report(
    failures: Vec<String>,
    px0: f32,
    px1: f32,
    m0: TerminalMetrics,
    m1: TerminalMetrics,
    dims1: (u16, u16),
) -> CadenceReport {
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: format!(
                "live zoom OK: ⌘+ ×{ZOOM_IN_STEPS} grew {px0}→{px1} pt (cell {:.2}×{:.2} → \
                 {:.2}×{:.2}), pty re-fitted to {}×{} + `stty size` confirmed the winsize; ⌘0 \
                 restored the baseline exactly",
                m0.cell_w, m0.cell_h, m1.cell_w, m1.cell_h, dims1.0, dims1.1
            ),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} niceties-zoom assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
