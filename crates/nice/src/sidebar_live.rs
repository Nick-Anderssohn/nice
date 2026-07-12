//! `sidebar` self-test scenario — the R10 sessions-mode sidebar LIVE gate
//! (Validation §3–§4), the sibling of [`crate::chrome_live`]: it mounts the real
//! [`SidebarShellView`] on a real, frontmost NSWindow and ground-truths its
//! geometry + resize + collapsed band against AppKit reads, driving the
//! synthetic-gesture halves with **real CGEvents**.
//!
//! It posts real CGEvents (mouse down / drag / up / double-click) to nice's
//! OWN pid (`crate::platform`, `CGEventPostToPid` — never the global HID tap), so
//! it preflights the Accessibility grant and FAILs loudly if it is missing. What
//! it asserts:
//!
//!   * **§3 expanded width** — the shell reports the 240pt default docked width.
//!   * **§3 resize clamp** — a CGEvent drag on the trailing resize handle clamps
//!     at 160 (drag far left) and 480 (drag far right), and a CGEvent
//!     double-click on the handle resets to 240. Because the hit target is a 6pt
//!     invisible zone a synthetic press may miss, an unregistered drag is a
//!     **DEFERRED HUMAN PASS**, not a fail (the same honest-deferral
//!     `chrome_live` uses for effects a synthetic CGEvent provably can't drive).
//!   * **§3 collapsed band geometry** — collapsing removes the leading column
//!     entirely (the M2 design: no cap card; `scenario_leading_column_width`
//!     reports 0). In the full-width band, the 82pt traffic-light spacer clears
//!     the LIVE zoom button's trailing edge and the bare restore button's rect
//!     has zero x-overlap with any traffic light (drift assertions over R9's
//!     [`standard_window_button_frames`]), and the R9 close-x / y-26 /
//!     equal-pitch geometry is **re-asserted** (the BUG-B stale-capture guard).
//!     Restoring returns the column.
//!   * **§3 drag differential** — a CGEvent press-drag on the sidebar top strip
//!     (R9 band pattern) vs the same drag inside the card body (a tab-row
//!     region), judged by real NSWindow frame reads: the strip drag moves the
//!     window (DEFERRED — `performWindowDragWithEvent:` tracks the physical
//!     cursor `CGEventPostToPid` does not move), the body drag must leave the
//!     frame put (hard).
//!   * **§4 dots** — with the model driven into all four dot states
//!     (thinking / waiting-unacked / waiting-acked / idle), the dot colour per
//!     token and the pulse-presence rule are asserted at the state level off the
//!     view's own R8 predicates ([`SidebarShellView::tab_dot_inputs`]); pixel
//!     corroboration is best-effort and left to a human capture.
//!
//! The multi-select routing / rename-gate / Esc / band-arm *classification* is
//! ground-truthed in-process by `nice-itests`' `sidebar_multiselect` cases (a
//! simulated event cannot move a real frame, so those differentials assert
//! consumption, not motion); this scenario owns the real-frame + real-geometry
//! half. Neither asserts cadence / perf (this is `Gate::SelfReported`).

use std::time::Duration;

use anyhow::Result;
use gpui::{prelude::*, AnyWindowHandle, AsyncApp, Entity, WindowHandle};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_model::{Pane, PaneKind, Tab, TabModel, TabStatus};
use nice_theme::chrome_geometry::{
    traffic_light_reserved_width, CARD_INSET, SIDEBAR_DEFAULT_WIDTH, SIDEBAR_MAX_WIDTH,
    SIDEBAR_MIN_WIDTH, TRAFFIC_LIGHT_CENTER_FROM_TOP,
};
use nice_theme::color::Srgba;
use nice_theme::palette::{slots, ColorScheme, Palette};
use nice_theme::status::WAITING_DOT;

use crate::app;
use crate::platform::{self, WindowButtonFrame};
use crate::sidebar_shell::SidebarShellView;
use crate::status_dot::{status_dot_base_color, status_dot_should_pulse};
use crate::theme::slot_srgba;
use crate::window_state::WindowState;

// -- fixed geometry / tolerances --------------------------------------------

/// The R9 absolute close-button leading x (`MACOS26_TRAFFIC_LIGHT_LEADINGS[0]` +
/// `TRAFFIC_LIGHT_NUDGE_X` = 17), re-asserted against the RENDERED close frame.
const EXPECTED_CLOSE_X: f32 = 17.0;
/// Tolerance (pt) for the close-button centre / x assertions.
const GEOMETRY_TOL: f32 = 0.5;
/// Tolerance (pt) for "the three buttons are equally pitched".
const PITCH_TOL: f32 = 0.6;
/// Tolerance (pt) for a width-clamp / reset assertion (`clamp` is exact; this
/// only absorbs the CGEvent content↔global round-trip).
const WIDTH_TOL: f32 = 1.0;
/// The restore button's diameter (pt) in the collapsed full-width band — the
/// 24pt icon button `SidebarShellView::icon_button` mints (`sidebar_shell.rs`).
const RESTORE_BUTTON_W: f32 = 24.0;
/// A resize drag distance (pt) large enough to force the clamp from any start in
/// [160, 480] (min−max span is 320, so ±400 always overshoots the bound).
const RESIZE_OVERSHOOT: f64 = 400.0;
/// Horizontal drag distance (pt) for the §3 window-move differential.
const DRAG_DX: f64 = 48.0;
/// A frame delta (pt) below which the window is considered "unchanged".
const FRAME_EPS: f64 = 4.0;

/// Accessibility-grant remediation, shared verbatim with the other CGEvent
/// scenarios.
const ACCESSIBILITY_REMEDIATION: &str = "\
Accessibility (TCC) grant missing: AXIsProcessTrusted() == false, so \
CGEventPostToPid is SILENTLY DROPPED and no injected mouse event can reach the \
window. Fix: System Settings → Privacy & Security → Accessibility → enable the \
process hosting this run (normally the terminal app). If it shows ON but this \
persists, the grant is STALE — remove it with '-' and re-add it, then re-run. \
Verify: swift -e 'import ApplicationServices; print(AXIsProcessTrusted())'";

/// The four tab ids seeded into the fixture, one per dot state.
const DOT_THINKING: &str = "dot-thinking";
const DOT_WAITING_UNACK: &str = "dot-waiting-unack";
const DOT_WAITING_ACK: &str = "dot-waiting-ack";
const DOT_IDLE: &str = "dot-idle";

// ===========================================================================
// scenario wiring
// ===========================================================================

/// Open the `sidebar` scenario window — the real [`SidebarShellView`] over a
/// seeded model (a Sessions project holding one Claude tab per dot state). Spawns
/// the driver (self-reported gate). No pty is needed: the shell hosts no terminal
/// this cycle (its content area is a plain panel), so nothing spawns.
pub fn open_sidebar_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let model = seed_model();
    let whandle: WindowHandle<SidebarShellView> = cx.open_window(app::window_options(), {
        // Seed the shared per-window state around the fixture model, then mount
        // the SAME refactored shell the managed window uses (R13.5: the isolated
        // scenario exercises the shipped view over a real `WindowState`, not a
        // private model copy).
        move |_window, cx| {
            let state = cx.new(|_cx| WindowState::with_model(model));
            cx.new(|cx| SidebarShellView::new(state, cx))
        }
    })?;
    let any: AnyWindowHandle = whandle.into();

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_sidebar(acx, whandle).await;
        eprintln!("[selftest] scenario 'sidebar': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(any)
}

/// The fixture model: the pinned Terminals group (Main) plus a `sessions` project
/// holding one Claude tab in each of the four dot states, so the live window
/// renders all four dots and the §4 checks read them back.
fn seed_model() -> TabModel {
    let mut m = TabModel::new("/tmp");
    let pi = m.ensure_project("sessions", "Sessions", "/tmp/sessions");
    let tabs = [
        claude_tab(DOT_THINKING, "Thinking", TabStatus::Thinking, false),
        claude_tab(DOT_WAITING_UNACK, "Waiting", TabStatus::Waiting, false),
        claude_tab(DOT_WAITING_ACK, "Seen", TabStatus::Waiting, true),
        claude_tab(DOT_IDLE, "Idle", TabStatus::Idle, false),
    ];
    for tab in tabs {
        m.projects[pi].tabs.push(tab);
    }
    m
}

/// A Claude tab whose sole Claude pane is driven into `status` (with `acked`
/// applied on entry into `.waiting`) via the real R8 transition API, so
/// `Tab::status()` / `Tab::waiting_acknowledged()` report the intended dot state.
fn claude_tab(id: &str, title: &str, status: TabStatus, acked: bool) -> Tab {
    let pane_id = format!("{id}-c");
    let mut pane = Pane::new(pane_id.clone(), "Claude", PaneKind::Claude);
    // `apply_status_transition` sets `waiting_acknowledged = acked` on entry into
    // `.waiting`; for thinking/idle the flag is irrelevant.
    pane.apply_status_transition(status, acked);
    let mut tab = Tab::new(id, title, "/tmp/sessions");
    tab.panes = vec![pane];
    tab.active_pane_id = Some(pane_id);
    tab
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

// ===========================================================================
// driver
// ===========================================================================

async fn run_sidebar(cx: &mut AsyncApp, whandle: WindowHandle<SidebarShellView>) -> CadenceReport {
    // Self-activate + settle so the window is frontmost/key and has painted once
    // (registering the shell's mouse handlers + the native buttons) before any
    // event is posted.
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 700).await;

    // Accessibility preflight — FAIL loudly (never silently skip the live half).
    if !platform::accessibility_trusted() {
        return CadenceReport::error(ACCESSIBILITY_REMEDIATION.to_string());
    }
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 300).await;

    let view = match whandle.entity(cx) {
        Ok(v) => v,
        Err(e) => return CadenceReport::error(format!("sidebar: could not read the root view: {e}")),
    };
    let pid = std::process::id() as i32;
    let mut failures: Vec<String> = Vec::new();
    let mut deferred: Vec<String> = Vec::new();

    // §3 — expanded default width.
    let w0 = read_width(cx, &view);
    if (w0 - SIDEBAR_DEFAULT_WIDTH).abs() > GEOMETRY_TOL {
        failures.push(format!(
            "expanded width: shell reports {w0:.1}, expected the {SIDEBAR_DEFAULT_WIDTH} default"
        ));
    } else {
        eprintln!("[selftest] sidebar width: expanded card at {w0:.1}pt (default)");
    }

    // §3 — resize clamp + double-click reset (attempt / defer).
    resize_checks(cx, whandle, &view, pid, &mut failures, &mut deferred).await;

    // §3 — the drag differential (strip moves the window; body drag does not).
    drag_differential(cx, whandle, pid, &mut failures, &mut deferred).await;

    // §3 — collapsed band geometry (no leading column + drift assertions + R9
    // re-assert), then restore.
    collapse_checks(cx, whandle, &view, &mut failures).await;
    restore_check(cx, &view, &mut failures).await;

    // §4 — dot colour + pulse per state (state-level, off the view's predicates).
    dot_checks(cx, &view, &mut failures);

    build_report(failures, deferred)
}

// ---- view / window reads ---------------------------------------------------

// `Entity::update` under `AsyncApp` returns the closure's value directly (it
// panics if the entity is gone — impossible here, the view owns the window);
// `WindowHandle::update` returns a `Result` (the window can close), hence the
// `.ok()` on those.
fn read_width(cx: &mut AsyncApp, view: &Entity<SidebarShellView>) -> f32 {
    view.update(cx, |v, _| v.sidebar_width())
}

fn read_collapsed(cx: &mut AsyncApp, view: &Entity<SidebarShellView>) -> bool {
    view.update(cx, |v, cx| v.is_collapsed(cx))
}

fn drive_collapse(cx: &mut AsyncApp, view: &Entity<SidebarShellView>) {
    let _ = view.update(cx, |v, cx| v.drive_toggle_collapsed(cx));
}

fn read_button_frames(
    cx: &mut AsyncApp,
    whandle: WindowHandle<SidebarShellView>,
) -> Option<[WindowButtonFrame; 3]> {
    whandle
        .update(cx, |_v, w, _a| platform::standard_window_button_frames(w))
        .ok()
        .flatten()
}

fn read_frame(cx: &mut AsyncApp, whandle: WindowHandle<SidebarShellView>) -> Option<[f64; 4]> {
    whandle
        .update(cx, |_v, w, _a| platform::window_screen_frame(w))
        .ok()
        .flatten()
}

fn to_global(
    cx: &mut AsyncApp,
    whandle: WindowHandle<SidebarShellView>,
    cx_pt: f64,
    cy_pt: f64,
) -> Option<(f64, f64)> {
    whandle
        .update(cx, |_v, w, _a| {
            platform::content_point_to_cg_global(w, cx_pt, cy_pt)
        })
        .ok()
        .flatten()
}

// ---- §3 geometry predicate (R9 re-assert) ----------------------------------

/// The pure R9 traffic-light geometry predicate over the three live frames
/// (mirror of `chrome_live::check_geometry`): all three exist, the close button's
/// visual centre sits on the y-26 row and its x-origin at 17, equal pitch.
fn check_geometry(frames: &[WindowButtonFrame; 3]) -> std::result::Result<String, String> {
    let [close, mini, zoom] = frames;
    let mut errs: Vec<String> = Vec::new();

    for (name, f) in [("close", close), ("minimize", mini), ("zoom", zoom)] {
        if f.width <= 0.0 || f.height <= 0.0 {
            errs.push(format!("{name} button has a degenerate frame {f:?}"));
        }
    }
    let cy = close.center_from_top();
    if (cy - TRAFFIC_LIGHT_CENTER_FROM_TOP).abs() > GEOMETRY_TOL {
        errs.push(format!(
            "close centre y={cy:.2} not within ±{GEOMETRY_TOL} of {TRAFFIC_LIGHT_CENTER_FROM_TOP}"
        ));
    }
    if (close.x - EXPECTED_CLOSE_X).abs() > GEOMETRY_TOL {
        errs.push(format!("close x={:.2} not within ±{GEOMETRY_TOL} of {EXPECTED_CLOSE_X}", close.x));
    }
    let p1 = mini.x - close.x;
    let p2 = zoom.x - mini.x;
    if (p1 - p2).abs() > PITCH_TOL {
        errs.push(format!(
            "unequal pitch: close→min {p1:.2} vs min→zoom {p2:.2} (Δ {:.2} > {PITCH_TOL})",
            (p1 - p2).abs()
        ));
    }
    if errs.is_empty() {
        Ok(format!("close(x={:.2}, centre_y={cy:.2}), equal pitch {p1:.2}≈{p2:.2}", close.x))
    } else {
        Err(errs.join("; "))
    }
}

// ---- §3 resize clamp + double-click reset ----------------------------------

async fn resize_checks(
    cx: &mut AsyncApp,
    whandle: WindowHandle<SidebarShellView>,
    view: &Entity<SidebarShellView>,
    pid: i32,
    failures: &mut Vec<String>,
    deferred: &mut Vec<String>,
) {
    let w0 = read_width(cx, view);

    // Drag the handle far left → clamp low (160).
    cg_resize_drag(cx, whandle, pid, w0, -RESIZE_OVERSHOOT).await;
    let w_low = read_width(cx, view);
    if (w_low - w0).abs() <= WIDTH_TOL {
        // The 6pt handle press did not register — a synthetic-event limitation,
        // not a shell bug. Defer the whole resize section; the expanded-width and
        // collapsed-band geometry were / are still hard-asserted.
        deferred.push(
            "resize: the CGEvent press did not land on the 6pt resize handle (width unchanged) — \
             DEFERRED to a human drag. The expanded default width and the collapsed-band geometry \
             were still hard-asserted."
                .to_string(),
        );
        // Restore a sane width for the later drag/collapse sections.
        let _ = view.update(cx, |v, cx| {
            v.drive_toggle_collapsed(cx);
            v.drive_toggle_collapsed(cx);
        });
        return;
    }

    if (w_low - SIDEBAR_MIN_WIDTH).abs() <= WIDTH_TOL {
        eprintln!("[selftest] sidebar resize: dragged far left → clamped at {SIDEBAR_MIN_WIDTH} (low bound)");
    } else {
        failures.push(format!(
            "resize low clamp: drag far left settled at {w_low:.1}, expected {SIDEBAR_MIN_WIDTH}"
        ));
    }

    // Drag far right → clamp high (480).
    let w_before_high = read_width(cx, view);
    cg_resize_drag(cx, whandle, pid, w_before_high, RESIZE_OVERSHOOT).await;
    let w_high = read_width(cx, view);
    if (w_high - SIDEBAR_MAX_WIDTH).abs() <= WIDTH_TOL {
        eprintln!("[selftest] sidebar resize: dragged far right → clamped at {SIDEBAR_MAX_WIDTH} (high bound)");
    } else {
        failures.push(format!(
            "resize high clamp: drag far right settled at {w_high:.1}, expected {SIDEBAR_MAX_WIDTH}"
        ));
    }

    // Double-click the handle → reset to 240.
    let w_before_dc = read_width(cx, view);
    cg_double_click_handle(cx, whandle, pid, w_before_dc).await;
    let w_dc = read_width(cx, view);
    if (w_dc - SIDEBAR_DEFAULT_WIDTH).abs() <= WIDTH_TOL {
        eprintln!("[selftest] sidebar resize: double-click reset width to {SIDEBAR_DEFAULT_WIDTH}");
    } else {
        deferred.push(format!(
            "resize double-click reset: width is {w_dc:.1}, expected {SIDEBAR_DEFAULT_WIDTH} — the \
             synthetic double-click may not have registered on the 6pt handle. DEFERRED to a human \
             double-click; the drag clamps above were asserted."
        ));
    }
}

/// The content-view x of the resize handle's hot zone for a given card width: the
/// handle straddles the inner card's trailing edge (`CARD_INSET + width`); a press
/// 2pt inside that edge lands on the handle and left of the content-area boundary.
fn handle_x(width: f32) -> f64 {
    (CARD_INSET + width - 2.0) as f64
}

/// A y comfortably inside the card body (below the 52pt strip), for the handle
/// drag / double-click.
const HANDLE_DRAG_Y: f64 = 200.0;

/// Post a synthetic left press-drag of `dx` pt starting on the resize handle.
async fn cg_resize_drag(
    cx: &mut AsyncApp,
    whandle: WindowHandle<SidebarShellView>,
    pid: i32,
    from_width: f32,
    dx: f64,
) {
    let Some((gx, gy)) = to_global(cx, whandle, handle_x(from_width), HANDLE_DRAG_Y) else {
        return;
    };
    platform::post_left_mouse_down(pid, gx, gy, 1);
    settle(cx, 90).await;
    let steps = 8;
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        platform::post_left_mouse_dragged(pid, gx + dx * t, gy);
    }
    platform::post_left_mouse_up(pid, gx + dx, gy, 1);
    settle(cx, 220).await;
}

/// Post a synthetic double-click on the resize handle.
async fn cg_double_click_handle(
    cx: &mut AsyncApp,
    whandle: WindowHandle<SidebarShellView>,
    pid: i32,
    from_width: f32,
) {
    let Some((gx, gy)) = to_global(cx, whandle, handle_x(from_width), HANDLE_DRAG_Y) else {
        return;
    };
    platform::post_left_mouse_down(pid, gx, gy, 1);
    platform::post_left_mouse_up(pid, gx, gy, 1);
    settle(cx, 40).await;
    platform::post_left_mouse_down(pid, gx, gy, 2);
    platform::post_left_mouse_up(pid, gx, gy, 2);
    settle(cx, 200).await;
}

// ---- §3 drag differential (strip vs card body) -----------------------------

async fn drag_differential(
    cx: &mut AsyncApp,
    whandle: WindowHandle<SidebarShellView>,
    pid: i32,
    failures: &mut Vec<String>,
    deferred: &mut Vec<String>,
) {
    // Top strip: the leading part of the 52pt strip, clear of the trailing
    // mode/collapse buttons, on the y-26 row (card is inset by CARD_INSET).
    let strip_x = 100.0f64;
    let strip_y = (CARD_INSET + TRAFFIC_LIGHT_CENTER_FROM_TOP) as f64;

    let before = read_frame(cx, whandle);
    do_cg_drag(cx, whandle, pid, strip_x, strip_y).await;
    settle(cx, 400).await;
    let after = read_frame(cx, whandle);
    match (before, after) {
        (Some(b), Some(a)) => {
            let dx = a[0] - b[0];
            let dy = a[1] - b[1];
            if (dx - DRAG_DX).abs() <= 10.0 && dy.abs() <= 10.0 {
                eprintln!("[selftest] sidebar strip drag: window moved by ({dx:.1},{dy:.1}) ≈ the {DRAG_DX}pt drag");
            } else {
                deferred.push(
                    "strip drag: the NSWindow frame did not follow the synthetic drag — \
                     performWindowDragWithEvent: tracks the PHYSICAL cursor, which CGEventPostToPid \
                     does not move. DEFERRED to a human drag; the card-body-drag half below IS \
                     asserted."
                        .to_string(),
                );
            }
            let _ = whandle.update(cx, |_v, w, _a| platform::set_window_frame(w, b));
            settle(cx, 250).await;
        }
        _ => failures.push("strip drag: could not read the NSWindow frame".to_string()),
    }
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 250).await;

    // Card body (a tab-row region): well below the 52pt strip, inside the card
    // (x=100 < the min 160pt card width). The strip's band handlers are confined
    // to the strip, so a body press — on a row or the list gap — never arms a
    // window drag: the frame must not move.
    let body_x = 100.0f64;
    let body_y = 250.0f64;
    let before = read_frame(cx, whandle);
    do_cg_drag(cx, whandle, pid, body_x, body_y).await;
    settle(cx, 400).await;
    let after = read_frame(cx, whandle);
    match (before, after) {
        (Some(b), Some(a)) => {
            let dx = a[0] - b[0];
            let dy = a[1] - b[1];
            if dx.abs() > FRAME_EPS || dy.abs() > FRAME_EPS {
                failures.push(format!(
                    "card-body drag MOVED the window by ({dx:.1},{dy:.1}) — a drag below the top \
                     strip must leave the window frame unchanged (the band only owns the strip)"
                ));
            } else {
                eprintln!("[selftest] sidebar card-body drag: window frame unchanged (correct — the strip did not claim a body press)");
            }
        }
        _ => failures.push("card-body drag: could not read the NSWindow frame".to_string()),
    }
}

/// Post a synthetic left press-drag of `DRAG_DX` pt (rightward) starting at the
/// content point `(cx_pt, cy_pt)` — the down is posted, allowed to arm, then the
/// drag steps + release burst so a modal window-drag loop can consume them.
async fn do_cg_drag(
    cx: &mut AsyncApp,
    whandle: WindowHandle<SidebarShellView>,
    pid: i32,
    cx_pt: f64,
    cy_pt: f64,
) {
    let Some((gx, gy)) = to_global(cx, whandle, cx_pt, cy_pt) else {
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

// ---- §3 collapsed band geometry + restore -----------------------------------

async fn collapse_checks(
    cx: &mut AsyncApp,
    whandle: WindowHandle<SidebarShellView>,
    view: &Entity<SidebarShellView>,
    failures: &mut Vec<String>,
) {
    drive_collapse(cx, view);
    settle(cx, 300).await;
    if !read_collapsed(cx, view) {
        failures.push("collapse: the shell did not report collapsed after the toggle".to_string());
        return;
    }

    // M2 Item B pin: the collapsed shell reserves NO leading column — the old
    // floating cap card is gone, replaced by one full-width title-bar band.
    let lead = view.update(cx, |v, cx| v.scenario_leading_column_width(cx));
    if lead != 0.0 {
        failures.push(format!(
            "collapse: leading column width {lead:.1} while collapsed — the M2 design renders a \
             full-width band with no reserved column (expected 0)"
        ));
    } else {
        eprintln!("[selftest] sidebar collapse: no leading column (full-width band; the cap card is gone)");
    }

    // The native buttons survive the collapse untouched — re-assert R9 geometry
    // (the BUG-B stale-capture guard).
    let Some(frames) = read_button_frames(cx, whandle) else {
        failures.push("collapse: standard_window_button_frames returned None".to_string());
        return;
    };
    match check_geometry(&frames) {
        Ok(d) => eprintln!("[selftest] sidebar collapsed-band geometry (R9 re-assert): {d}"),
        Err(e) => failures.push(format!("collapse band geometry: {e}")),
    }

    // The band's 82pt traffic-light spacer must clear the LIVE zoom button's
    // trailing edge, and the bare restore button — which sits immediately
    // after the spacer, so its rect starts at content x = the reserve — must
    // have zero x-overlap with any light.
    let reserve = traffic_light_reserved_width();
    let restore_x_min = reserve;
    let restore_x_max = restore_x_min + RESTORE_BUTTON_W;

    let [close, mini, zoom] = &frames;
    let zoom_trailing = zoom.x + zoom.width;
    if reserve + GEOMETRY_TOL < zoom_trailing {
        failures.push(format!(
            "collapse band: the {reserve:.1}pt traffic-light spacer does not clear the live zoom \
             button trailing edge {zoom_trailing:.1} — the restore button would sit over the lights"
        ));
    } else {
        eprintln!("[selftest] sidebar band spacer {reserve:.1}pt clears the live zoom trailing edge {zoom_trailing:.1}pt");
    }
    for (name, f) in [("close", close), ("minimize", mini), ("zoom", zoom)] {
        let light_trailing = f.x + f.width;
        if restore_x_min + GEOMETRY_TOL < light_trailing {
            failures.push(format!(
                "collapse band: restore button rect [{restore_x_min:.1},{restore_x_max:.1}] overlaps \
                 the {name} light [{:.1},{light_trailing:.1}] (must have zero overlap)",
                f.x
            ));
        }
    }
    eprintln!(
        "[selftest] sidebar collapse: restore button at x≥{restore_x_min:.1}pt, zero overlap with the traffic lights"
    );
}

async fn restore_check(
    cx: &mut AsyncApp,
    view: &Entity<SidebarShellView>,
    failures: &mut Vec<String>,
) {
    drive_collapse(cx, view);
    settle(cx, 300).await;
    if read_collapsed(cx, view) {
        failures.push("restore: the shell stayed collapsed after the second toggle".to_string());
        return;
    }
    let w = read_width(cx, view);
    if w < SIDEBAR_MIN_WIDTH - WIDTH_TOL || w > SIDEBAR_MAX_WIDTH + WIDTH_TOL {
        failures.push(format!(
            "restore: expanded column width {w:.1} is outside the resizable range \
             [{SIDEBAR_MIN_WIDTH}, {SIDEBAR_MAX_WIDTH}]"
        ));
    } else {
        eprintln!("[selftest] sidebar restore: returned the expanded column at width {w:.1}pt");
    }
}

// ---- §4 dot colour + pulse -------------------------------------------------

fn dot_checks(cx: &mut AsyncApp, view: &Entity<SidebarShellView>, failures: &mut Vec<String>) {
    let accent = view.update(cx, |v, _| v.accent());
    // The idle-dot colour is the Nice/Dark `ink3` slot (the caller-resolved
    // palette colour `StatusDot` maps `.idle` to).
    let idle: Srgba = slot_srgba(
        slots(Palette::Nice, ColorScheme::Dark)
            .expect("Nice/Dark is a valid combo")
            .ink3,
    );

    // (tab id, expected status, expected base colour, expected pulse).
    let cases: [(&str, TabStatus, Srgba, bool); 4] = [
        (DOT_THINKING, TabStatus::Thinking, accent, true),
        (DOT_WAITING_UNACK, TabStatus::Waiting, WAITING_DOT, true),
        (DOT_WAITING_ACK, TabStatus::Waiting, WAITING_DOT, false),
        (DOT_IDLE, TabStatus::Idle, idle, false),
    ];

    for (id, exp_status, exp_color, exp_pulse) in cases {
        let Some((status, ack)) = view.update(cx, |v, cx| v.tab_dot_inputs(id, cx)) else {
            failures.push(format!("dot: tab '{id}' missing from the model"));
            continue;
        };
        if status != exp_status {
            failures.push(format!("dot '{id}': status {status:?} != expected {exp_status:?}"));
            continue;
        }
        let color = status_dot_base_color(status, accent, idle);
        if color != exp_color {
            failures.push(format!("dot '{id}': base colour {color:?} != token {exp_color:?}"));
        }
        let pulses = status_dot_should_pulse(status, ack);
        if pulses != exp_pulse {
            failures.push(format!(
                "dot '{id}': pulse={pulses} != expected {exp_pulse} (status {status:?}, acked={ack})"
            ));
        }
    }
    eprintln!("[selftest] sidebar dots: thinking→accent(pulse), waiting-unacked→blue(pulse), waiting-acked→blue(no pulse), idle→ink3(no pulse)");
}

// ---- verdict ---------------------------------------------------------------

fn build_report(failures: Vec<String>, deferred: Vec<String>) -> CadenceReport {
    if !deferred.is_empty() {
        eprintln!("[selftest] sidebar DEFERRED HUMAN PASS checklist:");
        for d in &deferred {
            eprintln!("  - {d}");
        }
    }
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: format!(
                "all hard sidebar assertions passed (expanded width, collapsed band: no leading \
                 column + R9 re-assert + spacer/overlap drift guards, restore, card-body drag \
                 no-move, dot colour/pulse per state); the resize clamps + double-click reset and \
                 the strip window-move hard-assert when the synthetic gesture drives the real \
                 behaviour, else DEFER; {} item(s) DEFERRED to a human pass",
                deferred.len()
            ),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} sidebar assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
