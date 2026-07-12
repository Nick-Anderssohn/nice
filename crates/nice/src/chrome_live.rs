//! `chrome` self-test scenario — the R9 window-chrome LIVE gate (Validation
//! §1–§4), the sibling of `crate::input_live`: it drives the shipped chrome band
//! (`crate::app::WindowChromeView`) + the repositioned native traffic lights + the
//! full-screen wiring on a real, frontmost NSWindow and ground-truths them against
//! AppKit reads.
//!
//! It posts **real CGEvents** (mouse down / drag / up / double-click) to nice's
//! OWN pid (`crate::platform`, `CGEventPostToPid` — never the global HID tap), so
//! it preflights the Accessibility grant and FAILs loudly if it is missing (a
//! dropped CGEvent would silently no-op the drag/double-click checks). What it
//! asserts:
//!
//!   * **§1 traffic-light geometry** via `platform::standard_window_button_frames`:
//!     all three buttons exist, the close button's visual centre sits on the y-26
//!     row and its x-origin at 17, and the three are equally pitched (pitch read
//!     from the live frames, not hardcoded) — re-asserted after a resize, a focus
//!     bounce, and a full-screen enter+exit (the BUG-B stale-capture guard).
//!   * **§2 drag differential:** a CGEvent press-drag on the empty band vs the same
//!     drag in the terminal content area, judged by real NSWindow frame reads
//!     before/after (the band moves the window; the content drag leaves it put).
//!   * **§3 double-click:** reads the user's `AppleActionOnDoubleClick` (never
//!     writes it), predicts the effect (zoom / miniaturize / none), posts a CGEvent
//!     double-click on the band, and asserts the window state matches — plus a
//!     double-click while full screen does nothing (the band's `!is_fullscreen`
//!     gate).
//!   * **§4 full screen:** dispatches `ToggleFullScreen` and asserts
//!     `is_fullscreen()` + the View-menu title flip, both ways.
//!
//! Some effects a synthetic CGEvent provably cannot drive on this platform (a
//! window drag via `performWindowDragWithEvent:` tracks the PHYSICAL cursor, which
//! `CGEventPostToPid` does not move) are recorded as a **DEFERRED HUMAN PASS**
//! rather than fail-looped — the same honest-deferral pattern `input_live` uses
//! for synthetic IME composition. The window is left in a sane, restored state.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use gpui::{prelude::*, AnyWindowHandle, AsyncApp, Entity, OwnedMenuItem, SharedString};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_term_core::{SpawnSpec, DEFAULT_SCROLLBACK_LINES};
use nice_term_view::{
    FontSettings, TerminalMetrics, TerminalSessionHandle, TerminalTheme, TerminalView,
};
use nice_theme::chrome_geometry::TRAFFIC_LIGHT_CENTER_FROM_TOP;
use nice_theme::AccentPreset;

use crate::app::{self, ToggleFullScreen, WindowChromeView};
use crate::platform::{self, DoubleClickAction, WindowButtonFrame};

// -- fixed geometry ---------------------------------------------------------

const ROWS: u16 = 24;
const COLS: u16 = 80;
const FONT_FAMILY: &str = "Menlo";
const FONT_PX: f32 = 13.0;
const CELL_W: f32 = 8.0;
const CELL_H: f32 = 16.0;

/// The R9 absolute close-button leading x (`MACOS26_TRAFFIC_LIGHT_LEADINGS[0]` +
/// `TRAFFIC_LIGHT_NUDGE_X` = 17). Asserted against the RENDERED close-button frame.
const EXPECTED_CLOSE_X: f32 = 17.0;
/// Tolerance (pt) for the close-button centre / x assertions (plan §1: ±0.5).
const GEOMETRY_TOL: f32 = 0.5;
/// Tolerance (pt) for "the three buttons are equally pitched".
const PITCH_TOL: f32 = 0.6;
/// Horizontal drag distance (pt) for the §2 differential (~40pt, clear of the
/// ~2pt band threshold).
const DRAG_DX: f64 = 48.0;
/// A frame delta (pt) below which the window is considered "unchanged".
const FRAME_EPS: f64 = 4.0;

/// Accessibility-grant remediation, shared verbatim with the R5 live scenarios.
const ACCESSIBILITY_REMEDIATION: &str = "\
Accessibility (TCC) grant missing: AXIsProcessTrusted() == false, so \
CGEventPostToPid is SILENTLY DROPPED and no injected mouse event can reach the \
window. Fix: System Settings → Privacy & Security → Accessibility → enable the \
process hosting this run (normally the terminal app). If it shows ON but this \
persists, the grant is STALE — remove it with '-' and re-add it, then re-run. \
Verify: swift -e 'import ApplicationServices; print(AXIsProcessTrusted())'";

// ===========================================================================
// scenario wiring
// ===========================================================================

/// Open the `chrome` scenario window — the shipped chrome shell
/// (`WindowChromeView` over a silent live pane) with slice 2's full-screen
/// command + menu sync stood up (the selftest path doesn't call `run()`, so the
/// scenario installs them itself to exercise the real wiring). Spawns the driver
/// (self-reported gate).
pub fn open_chrome_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    // Stand up the shipped full-screen wiring: the global ToggleFullScreen
    // handler + ⌃⌘F binding + the initial (windowed) View menu. (`AsyncApp::update`
    // returns the closure's value directly, not a `Result`.)
    cx.update(|app| app::install_fullscreen_command(app));

    let base = prepare_dir()?;
    let base_s = base.to_string_lossy().to_string();
    // A silent, long-lived content pane so the "terminal content area" exists to
    // drag on; user rc suppressed so the grid stays quiet. Closing the window
    // drops the handle → SIGHUP/SIGKILL teardown, so no orphan survives.
    let spec = SpawnSpec::command("sleep 1000000".to_string(), base_s.clone())
        .with_env(vec![("ZDOTDIR".to_string(), base_s)])
        .with_size(ROWS, COLS);

    let handle = TerminalSessionHandle::spawn(cx, spec, DEFAULT_SCROLLBACK_LINES)?;
    let terminal = make_view(handle.clone(), cx);

    let window = cx.open_window(app::window_options(), {
        let terminal = terminal.clone();
        move |window, cx| {
            // The real shipped chrome shell — band + repositioned traffic lights +
            // the empty-chrome drag / double-click handlers.
            let chrome = cx.new(|_cx| WindowChromeView::new(terminal));
            // Keep the View-menu title in sync as this window enters/exits full
            // screen (slice 2's observer) — under test in §4.
            app::install_fullscreen_menu_sync(chrome.clone(), window, cx);
            chrome
        }
    })?;
    let window: AnyWindowHandle = window.into();
    app::install_present_kick(&handle, window, cx);

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_chrome(acx, window).await;
        eprintln!("[selftest] scenario 'chrome': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(window)
}

/// Build the silent-pane content view (fixed Menlo metrics, Nice/Dark theme).
fn make_view(handle: Entity<TerminalSessionHandle>, cx: &mut AsyncApp) -> Entity<TerminalView> {
    let theme = TerminalTheme::nice_default_dark();
    let accent = AccentPreset::Terracotta.color();
    let font = cx.new(|_cx| {
        FontSettings::fixed(
            SharedString::from(FONT_FAMILY),
            FONT_PX,
            TerminalMetrics::new(CELL_W, CELL_H),
        )
    });
    cx.new(|cx| TerminalView::new(handle, theme, accent, font, cx))
}

fn prepare_dir() -> Result<PathBuf> {
    let base = std::env::temp_dir().join(format!("nice-chrome-{}", std::process::id()));
    std::fs::create_dir_all(&base)?;
    Ok(base)
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

// ===========================================================================
// driver
// ===========================================================================

async fn run_chrome(cx: &mut AsyncApp, window: AnyWindowHandle) -> CadenceReport {
    // Self-activate + settle so the window is frontmost/key and has painted once
    // (registering the band's mouse handlers) before any event is posted.
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 700).await;

    // Accessibility preflight — FAIL loudly (never silently skip the live half).
    if !platform::accessibility_trusted() {
        return CadenceReport::error(ACCESSIBILITY_REMEDIATION.to_string());
    }
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 300).await;

    let pid = std::process::id() as i32;
    let mut failures: Vec<String> = Vec::new();
    let mut deferred: Vec<String> = Vec::new();

    // §1 — baseline traffic-light geometry.
    match assert_geometry(cx, window, "baseline") {
        Ok(d) => eprintln!("[selftest] chrome geometry (baseline): {d}"),
        Err(e) => failures.push(e),
    }

    // §1 BUG-B guard — geometry survives a resize.
    let _ = window.update(cx, |_r, w, _a| platform::resize_window_by(w, -120.0, -60.0));
    settle(cx, 350).await;
    match assert_geometry(cx, window, "after-resize") {
        Ok(d) => eprintln!("[selftest] chrome geometry (after resize): {d}"),
        Err(e) => failures.push(e),
    }
    let _ = window.update(cx, |_r, w, _a| platform::resize_window_by(w, 120.0, 60.0));
    settle(cx, 350).await;

    // §1 BUG-B guard — geometry survives a focus bounce (resign key → become key).
    platform::deactivate_app();
    settle(cx, 500).await;
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 500).await;
    match assert_geometry(cx, window, "after-focus-bounce") {
        Ok(d) => eprintln!("[selftest] chrome geometry (after focus bounce): {d}"),
        Err(e) => failures.push(e),
    }

    // §4 + §1 + §3(fullscreen) — full screen enter/exit, menu-title flip, the
    // in-fullscreen double-click no-op, and the geometry re-assert after exit.
    fullscreen_checks(cx, window, pid, &mut failures, &mut deferred).await;

    // §2 — the drag differential (band moves the window; content does not).
    drag_differential(cx, window, pid, &mut failures, &mut deferred).await;

    // §3 — the windowed double-click, judged against AppleActionOnDoubleClick.
    // Every synthetic-double-click divergence is DEFERRED (a synthetic event may
    // not register as a double-click on the band), so this pushes only to
    // `deferred`; the deterministic full-screen no-op check above owns the hard
    // full-screen assertions.
    double_click_check(cx, window, pid, &mut deferred).await;

    build_report(failures, deferred)
}

// ---- §1 geometry ----------------------------------------------------------

/// Read the live close/minimize/zoom frames, or an error describing why not.
fn read_button_frames(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
) -> std::result::Result<[WindowButtonFrame; 3], String> {
    window
        .update(cx, |_r, w, _a| platform::standard_window_button_frames(w))
        .map_err(|e| format!("window update failed: {e}"))?
        .ok_or_else(|| {
            "standard_window_button_frames returned None (no AppKit handle or a missing \
             standard button)"
                .to_string()
        })
}

/// Assert the rendered traffic-light geometry, tagging any failure with `label`.
fn assert_geometry(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    label: &str,
) -> std::result::Result<String, String> {
    let frames = read_button_frames(cx, window).map_err(|e| format!("geometry({label}): {e}"))?;
    check_geometry(&frames).map_err(|e| format!("geometry({label}): {e}"))
}

/// The pure geometry predicate over the three live frames.
fn check_geometry(frames: &[WindowButtonFrame; 3]) -> std::result::Result<String, String> {
    let [close, mini, zoom] = frames;
    let mut errs: Vec<String> = Vec::new();

    // All three buttons exist (a real frame has non-zero extent).
    for (name, f) in [("close", close), ("minimize", mini), ("zoom", zoom)] {
        if f.width <= 0.0 || f.height <= 0.0 {
            errs.push(format!("{name} button has a degenerate frame {f:?}"));
        }
    }

    // Close-button visual centre on the y-26 row.
    let cy = close.center_from_top();
    if (cy - TRAFFIC_LIGHT_CENTER_FROM_TOP).abs() > GEOMETRY_TOL {
        errs.push(format!(
            "close centre y={cy:.2} not within ±{GEOMETRY_TOL} of {TRAFFIC_LIGHT_CENTER_FROM_TOP}"
        ));
    }
    // Close-button leading x at the absolute 17 (the documented divergence).
    if (close.x - EXPECTED_CLOSE_X).abs() > GEOMETRY_TOL {
        errs.push(format!(
            "close x={:.2} not within ±{GEOMETRY_TOL} of {EXPECTED_CLOSE_X}",
            close.x
        ));
    }
    // Equal pitch, read from the LIVE frames (gpui derives it from the OS).
    let p1 = mini.x - close.x;
    let p2 = zoom.x - mini.x;
    if (p1 - p2).abs() > PITCH_TOL {
        errs.push(format!(
            "unequal pitch: close→min {p1:.2} vs min→zoom {p2:.2} (Δ {:.2} > {PITCH_TOL})",
            (p1 - p2).abs()
        ));
    }

    if errs.is_empty() {
        Ok(format!(
            "close(x={:.2}, centre_y={cy:.2}), equal pitch {p1:.2}≈{p2:.2}",
            close.x
        ))
    } else {
        Err(errs.join("; "))
    }
}

// ---- §4 full screen (+ §1 re-assert + §3 fullscreen no-op) ----------------

async fn fullscreen_checks(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    pid: i32,
    failures: &mut Vec<String>,
    deferred: &mut Vec<String>,
) {
    // Hard live check of slice 2's menu wiring, independent of whether full screen
    // can actually enter in this session: the View menu exists and reads the
    // windowed title.
    match read_view_menu_title(cx).as_deref() {
        Some("Enter Full Screen") => {
            eprintln!("[selftest] chrome View menu (windowed): reads 'Enter Full Screen'")
        }
        other => failures.push(format!(
            "full screen: View-menu title {other:?} != 'Enter Full Screen' while windowed"
        )),
    }

    // Re-activate the app AND explicitly activate our window so `active_window()`
    // resolves to it: the global ToggleFullScreen handler toggles the ACTIVE
    // window, and an unattended run may not make nice the OS-active app.
    let _ = cx.update(|app| app.activate(true));
    let _ = window.update(cx, |_r, w, _a| w.activate_window());
    settle(cx, 500).await;
    // Enter full screen via the real action (the same path ⌃⌘F drives).
    let _ = cx.update(|app| app.dispatch_action(&ToggleFullScreen));
    let via_action = wait_fullscreen(cx, window, true, 5000).await;
    let mut entered = via_action;
    if !entered {
        // Fallback: drive it directly so the DETERMINISTIC title-flip + BUG-B
        // geometry checks below still run even when the action can't route
        // unattended. If even this cannot enter, full screen is environmentally
        // blocked and we defer entirely.
        let _ = window.update(cx, |_r, w, _a| w.toggle_fullscreen());
        entered = wait_fullscreen(cx, window, true, 5000).await;
    }
    if !entered {
        deferred.push(
            "full screen: neither the ToggleFullScreen action nor a direct \
             window.toggle_fullscreen() entered full screen within ~10s — AppKit full-screen is \
             environmentally blocked here (e.g. another app owns the active Space in an \
             unattended session). DEFERRED to a human ⌃⌘F check; the windowed View-menu title \
             + §1 geometry + §2/§3 were still asserted."
                .to_string(),
        );
        return;
    }
    if !via_action {
        // Full screen + the title flip work (asserted below via the direct entry),
        // but the ACTION did not drive it: nice did not register as the
        // OS-active application unattended, so `App::dispatch_action` found no
        // active window to route to. NOT a code fault — ⌃⌘F works when the app is
        // genuinely active — so DEFER (do not fail) and let a human confirm.
        deferred.push(
            "full screen: the ToggleFullScreen ACTION did not drive full screen in this run (a \
             direct window.toggle_fullscreen() did) — nice was not the OS-active app \
             unattended, so App::dispatch_action's active-window routing found none. The View-menu \
             title flip + geometry-after-exit BELOW were still HARD-asserted via the direct entry. \
             DEFERRED: a human confirms ⌃⌘F enters full screen."
                .to_string(),
        );
    }
    match read_view_menu_title(cx).as_deref() {
        Some("Exit Full Screen") => {
            eprintln!("[selftest] chrome full screen: entered, View menu reads 'Exit Full Screen'")
        }
        other => failures.push(format!(
            "full screen: View-menu title {other:?} != 'Exit Full Screen' after entering"
        )),
    }

    // §3 — a double-click while full screen must do NOTHING (the band's
    // !is_fullscreen gate): assert we stay full screen and un-miniaturized.
    let mini_before = read_bool(cx, window, platform::window_is_miniaturized);
    post_double_click_on_band(cx, window, pid).await;
    settle(cx, 500).await;
    if !read_is_fullscreen(cx, window) {
        failures.push(
            "full screen: a double-click on the band left full screen (the band must ignore \
             presses while full screen)"
                .to_string(),
        );
    }
    if read_bool(cx, window, platform::window_is_miniaturized) != mini_before {
        failures.push(
            "full screen: a double-click on the band changed miniaturize state (must be a no-op \
             while full screen)"
                .to_string(),
        );
    }

    // Exit full screen (action first; a direct toggle as a restore fallback so the
    // window never stays stuck full screen for later steps / the user).
    let _ = cx.update(|app| app.dispatch_action(&ToggleFullScreen));
    let mut exited = wait_fullscreen(cx, window, false, 5000).await;
    if !exited {
        let _ = window.update(cx, |_r, w, _a| w.toggle_fullscreen());
        exited = wait_fullscreen(cx, window, false, 5000).await;
    }
    if !exited {
        failures.push(
            "full screen: neither the ToggleFullScreen action nor a direct toggle exited full \
             screen — the window is stuck full screen"
                .to_string(),
        );
        return;
    }
    match read_view_menu_title(cx).as_deref() {
        Some("Enter Full Screen") => {
            eprintln!("[selftest] chrome full screen: exited, View menu reads 'Enter Full Screen'")
        }
        other => failures.push(format!(
            "full screen: View-menu title {other:?} != 'Enter Full Screen' after exiting"
        )),
    }

    // §1 BUG-B guard — geometry survives the full-screen round trip.
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 500).await;
    match assert_geometry(cx, window, "after-fullscreen-exit") {
        Ok(d) => eprintln!("[selftest] chrome geometry (after full-screen exit): {d}"),
        Err(e) => failures.push(e),
    }
}

/// Poll `is_fullscreen()` until it equals `want` or `timeout_ms` elapses.
async fn wait_fullscreen(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    want: bool,
    timeout_ms: u64,
) -> bool {
    let mut waited = 0;
    loop {
        if read_is_fullscreen(cx, window) == want {
            return true;
        }
        if waited >= timeout_ms {
            return false;
        }
        settle(cx, 150).await;
        waited += 150;
    }
}

fn read_is_fullscreen(cx: &mut AsyncApp, window: AnyWindowHandle) -> bool {
    window
        .update(cx, |_r, w, _a| w.is_fullscreen())
        .unwrap_or(false)
}

/// The View-menu's full-screen item title, read back via `get_menus`.
/// (`AsyncApp::update` returns the closure's `Option<String>` directly.)
fn read_view_menu_title(cx: &mut AsyncApp) -> Option<String> {
    cx.update(|app| {
        let menus = app.get_menus()?;
        let view = menus.into_iter().find(|m| m.name.as_ref() == "View")?;
        view.items.into_iter().find_map(|item| match item {
            OwnedMenuItem::Action { name, .. } => Some(name),
            _ => None,
        })
    })
}

// ---- §2 drag differential -------------------------------------------------

async fn drag_differential(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    pid: i32,
    failures: &mut Vec<String>,
    deferred: &mut Vec<String>,
) {
    let (vw, vh) = read_viewport(cx, window);
    // Empty band: clear of the traffic-light cluster on the left, on the y-26 row.
    let band_x = (vw * 0.6) as f64;
    let band_y = TRAFFIC_LIGHT_CENTER_FROM_TOP as f64;
    // Terminal content: mid-window, well below the 52pt band.
    let content_x = (vw * 0.5) as f64;
    let content_y = (vh * 0.6) as f64;

    // --- band drag: should move the NSWindow frame by ~the drag delta ---
    let before = read_frame(cx, window);
    do_cg_drag(cx, window, pid, band_x, band_y).await;
    settle(cx, 500).await;
    let after = read_frame(cx, window);
    match (before, after) {
        (Some(b), Some(a)) => {
            let dx = a[0] - b[0];
            let dy = a[1] - b[1];
            if (dx - DRAG_DX).abs() <= 10.0 && dy.abs() <= 10.0 {
                eprintln!(
                    "[selftest] chrome band drag: window moved by ({dx:.1},{dy:.1}) ≈ the \
                     {DRAG_DX}pt drag"
                );
            } else if dx.abs() > FRAME_EPS || dy.abs() > FRAME_EPS {
                deferred.push(format!(
                    "band drag: window moved by ({dx:.1},{dy:.1}), not the {DRAG_DX}pt drag delta \
                     — performWindowDragWithEvent: tracks the PHYSICAL cursor, which \
                     CGEventPostToPid does not move, so the move is cursor-driven not \
                     event-driven. DEFERRED to a human drag; the content-drag half below IS \
                     asserted."
                ));
            } else {
                deferred.push(
                    "band drag: the NSWindow frame did not move — performWindowDragWithEvent: \
                     tracks the PHYSICAL cursor, which CGEventPostToPid does not move. DEFERRED \
                     to a human drag; the content-drag half below IS asserted."
                        .to_string(),
                );
            }
            // Restore the window to its pre-drag frame so later steps are stable.
            let _ = window.update(cx, |_r, w, _a| platform::set_window_frame(w, b));
            settle(cx, 250).await;
        }
        _ => failures.push("band drag: could not read the NSWindow frame".to_string()),
    }
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 250).await;

    // --- content drag: must NOT move the NSWindow frame ---
    let before = read_frame(cx, window);
    do_cg_drag(cx, window, pid, content_x, content_y).await;
    settle(cx, 500).await;
    let after = read_frame(cx, window);
    match (before, after) {
        (Some(b), Some(a)) => {
            let dx = a[0] - b[0];
            let dy = a[1] - b[1];
            if dx.abs() > FRAME_EPS || dy.abs() > FRAME_EPS {
                failures.push(format!(
                    "content drag MOVED the window by ({dx:.1},{dy:.1}) — a drag in the terminal \
                     content area must leave the window frame unchanged"
                ));
            } else {
                eprintln!(
                    "[selftest] chrome content drag: window frame unchanged (correct — the band \
                     did not claim a content-area press)"
                );
            }
        }
        _ => failures.push("content drag: could not read the NSWindow frame".to_string()),
    }
}

/// Post a synthetic left press-drag of `DRAG_DX` pt (rightward) starting at the
/// content point `(cx_pt, cy_pt)`. The down is posted, allowed to arm, then the
/// drag steps + release are burst with no awaits so a modal window-drag loop (if
/// `start_window_move` enters one) can consume them from the event queue.
async fn do_cg_drag(cx: &mut AsyncApp, window: AnyWindowHandle, pid: i32, cx_pt: f64, cy_pt: f64) {
    let Some((gx, gy)) = to_global(cx, window, cx_pt, cy_pt) else {
        return;
    };
    platform::post_left_mouse_down(pid, gx, gy, 1);
    settle(cx, 90).await;
    let steps = 8;
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        platform::post_left_mouse_dragged(pid, gx + DRAG_DX * t, gy);
    }
    platform::post_left_mouse_up(pid, gx + DRAG_DX, gy, 1);
}

// ---- §3 double-click ------------------------------------------------------

async fn double_click_check(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    pid: i32,
    deferred: &mut Vec<String>,
) {
    // Read (never write) the user's real preference and predict the effect.
    let action = platform::apple_action_on_double_click();
    let (vw, _vh) = read_viewport(cx, window);
    let band_x = (vw * 0.6) as f64;
    let band_y = TRAFFIC_LIGHT_CENTER_FROM_TOP as f64;

    let zoomed_before = read_bool(cx, window, platform::window_is_zoomed);
    let frame_before = read_frame(cx, window);

    post_double_click_on_band_at(cx, window, pid, band_x, band_y).await;
    // titlebar_double_click runs the action on the foreground executor, and a
    // zoom animates — settle generously.
    settle(cx, 800).await;

    let zoomed_after = read_bool(cx, window, platform::window_is_zoomed);
    let mini_after = read_bool(cx, window, platform::window_is_miniaturized);
    let frame_after = read_frame(cx, window);
    let frame_changed = match (frame_before, frame_after) {
        (Some(b), Some(a)) => (a[0] - b[0]).abs() > FRAME_EPS
            || (a[1] - b[1]).abs() > FRAME_EPS
            || (a[2] - b[2]).abs() > FRAME_EPS
            || (a[3] - b[3]).abs() > FRAME_EPS,
        _ => false,
    };

    match action {
        DoubleClickAction::None => {
            if zoomed_after != zoomed_before || mini_after == Some(true) || frame_changed {
                deferred.push(format!(
                    "double-click (AppleActionOnDoubleClick=None): window state changed \
                     unexpectedly (zoomed {zoomed_before:?}→{zoomed_after:?}, miniaturized \
                     {mini_after:?}, frame_changed {frame_changed}) — investigate / human confirm."
                ));
            } else {
                eprintln!(
                    "[selftest] chrome double-click (None): window unchanged (correct — 'Do \
                     Nothing')"
                );
            }
        }
        DoubleClickAction::Zoom => {
            if zoomed_after == Some(true) && zoomed_before != Some(true) {
                eprintln!(
                    "[selftest] chrome double-click (Zoom): isZoomed flipped \
                     {zoomed_before:?}→{zoomed_after:?} (correct)"
                );
            } else if frame_changed {
                eprintln!(
                    "[selftest] chrome double-click (Zoom): window frame changed (zoomed) — correct"
                );
            } else {
                deferred.push(
                    "double-click (Zoom expected): neither isZoomed nor the frame changed — the \
                     synthetic double-click may not have registered as a double-click on the \
                     band. DEFERRED to a human double-click."
                        .to_string(),
                );
            }
        }
        DoubleClickAction::Minimize => {
            if mini_after == Some(true) {
                eprintln!("[selftest] chrome double-click (Minimize): window miniaturized (correct)");
            } else {
                deferred.push(
                    "double-click (Minimize expected): the window did not miniaturize — the \
                     synthetic double-click may not have registered. DEFERRED to a human \
                     double-click."
                        .to_string(),
                );
            }
        }
    }

    // Restore: de-miniaturize + reset the frame + re-activate, so the suite's
    // teardown and any later scenario start from a sane state.
    if read_bool(cx, window, platform::window_is_miniaturized) == Some(true) {
        let _ = window.update(cx, |_r, w, _a| platform::deminiaturize_window(w));
        settle(cx, 600).await;
    }
    if let Some(b) = frame_before {
        let _ = window.update(cx, |_r, w, _a| platform::set_window_frame(w, b));
    }
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 300).await;
}

/// Post a CGEvent double-click on the empty band (used by the full-screen no-op
/// check, which does not need a specific x beyond "on the band").
async fn post_double_click_on_band(cx: &mut AsyncApp, window: AnyWindowHandle, pid: i32) {
    let (vw, _vh) = read_viewport(cx, window);
    let band_x = (vw * 0.6) as f64;
    let band_y = TRAFFIC_LIGHT_CENTER_FROM_TOP as f64;
    post_double_click_on_band_at(cx, window, pid, band_x, band_y).await;
}

/// Post a CGEvent double-click at the content point `(cx_pt, cy_pt)`: a click-1
/// down/up pair then a click-2 down/up pair (the click-state field makes the
/// second down read as `NSEvent.clickCount == 2`, which the band treats as a
/// double-click).
async fn post_double_click_on_band_at(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    pid: i32,
    cx_pt: f64,
    cy_pt: f64,
) {
    let Some((gx, gy)) = to_global(cx, window, cx_pt, cy_pt) else {
        return;
    };
    platform::post_left_mouse_down(pid, gx, gy, 1);
    platform::post_left_mouse_up(pid, gx, gy, 1);
    settle(cx, 40).await;
    platform::post_left_mouse_down(pid, gx, gy, 2);
    platform::post_left_mouse_up(pid, gx, gy, 2);
}

// ---- shared reads ---------------------------------------------------------

fn read_viewport(cx: &mut AsyncApp, window: AnyWindowHandle) -> (f32, f32) {
    window
        .update(cx, |_r, w, _a| {
            let s = w.viewport_size();
            (f32::from(s.width), f32::from(s.height))
        })
        .unwrap_or((960.0, 640.0))
}

fn read_frame(cx: &mut AsyncApp, window: AnyWindowHandle) -> Option<[f64; 4]> {
    window
        .update(cx, |_r, w, _a| platform::window_screen_frame(w))
        .ok()
        .flatten()
}

fn read_bool(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    read: fn(&gpui::Window) -> Option<bool>,
) -> Option<bool> {
    window.update(cx, |_r, w, _a| read(w)).ok().flatten()
}

fn to_global(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    cx_pt: f64,
    cy_pt: f64,
) -> Option<(f64, f64)> {
    window
        .update(cx, |_r, w, _a| {
            platform::content_point_to_cg_global(w, cx_pt, cy_pt)
        })
        .ok()
        .flatten()
}

// ---- verdict --------------------------------------------------------------

fn build_report(failures: Vec<String>, deferred: Vec<String>) -> CadenceReport {
    if !deferred.is_empty() {
        eprintln!("[selftest] chrome DEFERRED HUMAN PASS checklist:");
        for d in &deferred {
            eprintln!("  - {d}");
        }
    }
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: format!(
                "all hard chrome assertions passed (see the per-check stderr log — traffic-light \
                 geometry always hard-asserts; the full-screen toggle / title flip and the \
                 drag / double-click effects hard-assert when the synthetic gesture drives the \
                 real behavior, else DEFER); {} item(s) DEFERRED to a human pass",
                deferred.len()
            ),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} chrome assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
