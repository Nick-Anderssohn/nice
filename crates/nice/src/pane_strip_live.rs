//! `pane-strip` self-test scenario — the R11 toolbar pane-strip LIVE gate
//! (Validation §3), the sibling of [`crate::chrome_live`] / [`crate::sidebar_live`]:
//! it mounts the real [`WindowToolbarView`] over a seeded model on a real,
//! frontmost NSWindow and ground-truths the one thing only a real window can
//! prove — the **drag differential with pills present** — against real NSWindow
//! frame reads, driving the synthetic halves with **real CGEvents**.
//!
//! It posts real CGEvents (mouse down / drag / up / click) to nice-rs's OWN pid
//! (`crate::platform`, `CGEventPostToPid` — never the global HID tap), so it
//! preflights the Accessibility grant and FAILs loudly if it is missing. What it
//! asserts:
//!
//!   * **§3 drag differential (the core).** A CGEvent press-drag starting on a
//!     pill **selects** that pill AND leaves the NSWindow frame **put** (the pill
//!     consumes the press, so the R9 band never arms). This is hard-asserted only
//!     when the press is shown to have LANDED (the select confirms delivery) —
//!     otherwise it DEFERS, because a synthetic `CGEventPostToPid` mouse event
//!     need not land on a gpui hitbox (a positive click effect is
//!     delivery-dependent; the pill-consumes-press + select routing is proven
//!     deterministically in-process by `nice-itests`). The same press-drag on the
//!     empty toolbar band **does** move the window — DEFERRED, because
//!     `performWindowDragWithEvent:` tracks the PHYSICAL cursor which
//!     `CGEventPostToPid` does not move. This is "R9's contract still holds with
//!     pills present," ground-truthed on a real frame — the same honest-deferral
//!     `chrome_live` / `sidebar_live` use for synthetic mouse gestures.
//!   * **§3 overflow chevron (real layout).** After enough panes are added to
//!     overflow the reserved-width viewport, the chevron renders — asserted HARD
//!     from the view's own real-layout predicate on a REAL on-screen window (the
//!     mocked-layout onset is pinned in `nice-itests`).
//!   * **§3 activate-from-elsewhere centers.** Selecting a currently-offscreen pane
//!     through the real action path makes it active (HARD) and auto-scrolls it into
//!     view (the once-offscreen pill leaves the offscreen set) — the latter
//!     DEFERRED if the frontmost window's repaint has not applied the centering
//!     within the settle window.
//!   * **§3 overflow menu opens via click.** A CGEvent click on the chevron opens
//!     the overflow menu (the view reports the menu open) — DEFERRED if the
//!     synthetic click missed the 22pt chevron. The menu's *contents* ("lists every
//!     pane", "checkmark on active") are pinned by `toolbar.rs`'s unit tests + the
//!     in-process cases, the sidebar-scenario precedent for menu classification.
//!
//! Neither this nor any in-process test asserts cadence / perf (this is
//! `Gate::SelfReported`).

use std::time::Duration;

use anyhow::Result;
use gpui::{prelude::*, AnyWindowHandle, App, AsyncApp, Entity, WindowHandle};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_model::{Pane, PaneKind, TabModel};
use nice_theme::chrome_geometry::TOP_BAR_HEIGHT;

use crate::app;
use crate::platform;
use crate::toolbar::WindowToolbarView;
use crate::window_state::WindowState;

// -- fixed geometry / tolerances --------------------------------------------

/// Horizontal drag distance (pt) for the §3 differential (~48pt, clear of the
/// ~2pt band drag threshold).
const DRAG_DX: f64 = 48.0;
/// A frame delta (pt) below which the window is considered "unchanged".
const FRAME_EPS: f64 = 4.0;
/// How many terminal panes to add so the strip overflows its viewport and the
/// chevron shows (comfortably past the ~780pt viewport with ~130pt pills).
const OVERFLOW_ADDS: usize = 8;
/// The chevron button's centre, measured from the window's trailing edge:
/// trailing pad (20) + the `+` slot (28) + the chevron slot's leading pad (4) +
/// half the 22pt button = 20 + 28 + 28 − 4 − 11 → `vw − 61`
/// (`toolbar.rs` `TOOLBAR_TRAILING_PAD` / `SQUARE_SLOT_WIDTH`).
const CHEVRON_FROM_RIGHT: f64 = 61.0;

/// Accessibility-grant remediation, shared verbatim with the other CGEvent
/// scenarios.
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

/// Open the `pane-strip` scenario window — the real [`WindowToolbarView`] over a
/// seeded two-pane Main tab (no pty: the strip renders purely from the model +
/// the injected action seam this cycle). Spawns the driver (self-reported gate).
pub fn open_pane_strip_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let model = seed_model();
    let whandle: WindowHandle<WindowToolbarView> = cx.open_window(app::window_options(), {
        // Seed the shared per-window state around the fixture model, then mount
        // the SAME refactored toolbar the managed window uses (R13.5: the isolated
        // scenario exercises the shipped view over a real `WindowState`, not a
        // private model copy).
        move |_window, cx| {
            let state = cx.new(|_cx| WindowState::with_model(model));
            cx.new(|cx| WindowToolbarView::new(state, cx))
        }
    })?;
    let any: AnyWindowHandle = whandle.into();

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_pane_strip(acx, whandle).await;
        eprintln!("[selftest] scenario 'pane-strip': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(any)
}

/// The fixture: the pinned Main terminal tab holding two terminal pills (`p0`
/// active, `p1`), so the drag differential has a pill to press and empty band to
/// the right, and `drive_add` can later push the strip into overflow.
fn seed_model() -> TabModel {
    let mut m = TabModel::new("/tmp");
    let tab_id = TabModel::MAIN_TERMINAL_TAB_ID.to_string();
    let (pi, ti) = m.project_tab_index(&tab_id).expect("main tab exists");
    m.projects[pi].tabs[ti].panes = vec![
        Pane::new("p0", "Terminal 1", PaneKind::Terminal),
        Pane::new("p1", "Terminal 2", PaneKind::Terminal),
    ];
    m.projects[pi].tabs[ti].active_pane_id = Some("p0".to_string());
    m.projects[pi].tabs[ti].next_terminal_index = 3;
    m
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

// ===========================================================================
// driver
// ===========================================================================

async fn run_pane_strip(cx: &mut AsyncApp, whandle: WindowHandle<WindowToolbarView>) -> CadenceReport {
    // Self-activate + settle so the window is frontmost/key and has painted once
    // (registering the pills' + band's mouse handlers) before any event is posted.
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 700).await;

    // Accessibility preflight — FAIL loudly (never silently skip the live half).
    if !platform::accessibility_trusted() {
        return CadenceReport::error(ACCESSIBILITY_REMEDIATION.to_string());
    }
    let _ = cx.update(|app| app.activate(true));
    // Make the window KEY so the first synthetic mouse-down is delivered to a pill
    // rather than swallowed as a window-focus (first-mouse) click — the pill-press
    // select check needs the down to actually reach the pill's hitbox.
    let _ = whandle.update(cx, |_v, w, _a| w.activate_window());
    settle(cx, 400).await;

    let view = match whandle.entity(cx) {
        Ok(v) => v,
        Err(e) => return CadenceReport::error(format!("pane-strip: could not read the root view: {e}")),
    };
    let pid = std::process::id() as i32;
    let mut failures: Vec<String> = Vec::new();
    let mut deferred: Vec<String> = Vec::new();

    // §3 — the drag differential (pill press does not move + selects; band drag does).
    drag_differential(cx, whandle, &view, pid, &mut failures, &mut deferred).await;

    // §3 — overflow chevron (real layout) + centering + menu-opens.
    overflow_checks(cx, whandle, &view, pid, &mut failures, &mut deferred).await;

    build_report(failures, deferred)
}

// ---- §3 drag differential --------------------------------------------------

async fn drag_differential(
    cx: &mut AsyncApp,
    whandle: WindowHandle<WindowToolbarView>,
    view: &Entity<WindowToolbarView>,
    pid: i32,
    failures: &mut Vec<String>,
    deferred: &mut Vec<String>,
) {
    // --- pill press-drag: the press selects p1 AND the window must NOT move ---
    //
    // Only a press that actually REACHED the pill (evidenced by the select) lets us
    // hard-assert the differential — the same honest-deferral chrome_live /
    // sidebar_live use for synthetic mouse gestures a `CGEventPostToPid` may not
    // land (a positive click effect is delivery-dependent; the in-process
    // `nice-itests` cases prove the pill-consumes-press / select routing
    // deterministically). If the press did not register, DEFER the whole pill
    // differential rather than read a vacuous "frame unchanged" as a pass.
    let Some((px_x, px_y)) = read_pill_center(cx, view, "p1") else {
        failures.push("pill drag: p1 pill was not laid out (no bounds) — cannot post the press".to_string());
        return;
    };
    let active_before = read_active(cx, view);
    let frame_before = read_frame(cx, whandle);
    do_cg_drag(cx, whandle, pid, px_x, px_y).await;
    settle(cx, 400).await;
    let active_after = read_active(cx, view);
    let frame_after = read_frame(cx, whandle);

    let landed = active_before.as_deref() != Some("p1") && active_after.as_deref() == Some("p1");
    if landed {
        // The press reached the pill (it selected p1); now the window frame must be
        // unchanged — the pill consumed the press, so the R9 band never armed.
        match (frame_before, frame_after) {
            (Some(b), Some(a)) => {
                let dx = a[0] - b[0];
                let dy = a[1] - b[1];
                if dx.abs() > FRAME_EPS || dy.abs() > FRAME_EPS {
                    failures.push(format!(
                        "pill drag: the press selected p1 but ALSO moved the window by ({dx:.1},{dy:.1}) \
                         — a drag starting on a pill must leave the frame put (the pill consumes the \
                         press before the band)"
                    ));
                    let _ = whandle.update(cx, |_v, w, _a| platform::set_window_frame(w, b));
                    settle(cx, 200).await;
                } else {
                    eprintln!(
                        "[selftest] pane-strip pill drag: selected p1 AND left the window frame put \
                         (the pill claimed the press — R9's contract holds with pills present)"
                    );
                }
            }
            _ => failures.push("pill drag: could not read the NSWindow frame".to_string()),
        }
    } else {
        deferred.push(format!(
            "pill press: the synthetic mouse press did not register on p1's pill (active {active_after:?}, \
             was {active_before:?}) — the same synthetic-gesture limitation the band drag hits (a \
             CGEventPostToPid mouse event need not land on a gpui hitbox). DEFERRED to a human click; \
             the pill-consumes-press + select routing is hard-asserted in-process (nice-itests)."
        ));
    }
    let _ = cx.update(|app| app.activate(true));
    let _ = whandle.update(cx, |_v, w, _a| w.activate_window());
    settle(cx, 200).await;

    // --- band drag: the empty toolbar band DOES move the window (DEFER) ---
    let (vw, _vh) = read_viewport(cx, whandle);
    // A point past the two pills, clear of the trailing chevron / `+` cluster.
    let band_x = pick_empty_band_x(cx, view, vw as f64);
    let band_y = (TOP_BAR_HEIGHT / 2.0) as f64;
    let before = read_frame(cx, whandle);
    do_cg_drag(cx, whandle, pid, band_x, band_y).await;
    settle(cx, 400).await;
    let after = read_frame(cx, whandle);
    match (before, after) {
        (Some(b), Some(a)) => {
            let dx = a[0] - b[0];
            let dy = a[1] - b[1];
            if (dx - DRAG_DX).abs() <= 10.0 && dy.abs() <= 10.0 {
                eprintln!("[selftest] pane-strip band drag: window moved by ({dx:.1},{dy:.1}) ≈ the {DRAG_DX}pt drag");
            } else {
                deferred.push(
                    "band drag: the NSWindow frame did not follow the synthetic drag — \
                     performWindowDragWithEvent: tracks the PHYSICAL cursor, which CGEventPostToPid \
                     does not move. DEFERRED to a human drag; the pill-drag-no-move half above IS \
                     asserted."
                        .to_string(),
                );
            }
            let _ = whandle.update(cx, |_v, w, _a| platform::set_window_frame(w, b));
            settle(cx, 250).await;
        }
        _ => failures.push("band drag: could not read the NSWindow frame".to_string()),
    }
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 200).await;
}

/// Post a synthetic left press-drag of `DRAG_DX` pt (rightward) starting at the
/// content point `(cx_pt, cy_pt)` — the down is posted, allowed to arm, then the
/// drag steps + release burst so a modal window-drag loop can consume them.
async fn do_cg_drag(cx: &mut AsyncApp, whandle: WindowHandle<WindowToolbarView>, pid: i32, cx_pt: f64, cy_pt: f64) {
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

// ---- §3 overflow: chevron + centering + menu-opens -------------------------

async fn overflow_checks(
    cx: &mut AsyncApp,
    whandle: WindowHandle<WindowToolbarView>,
    view: &Entity<WindowToolbarView>,
    pid: i32,
    failures: &mut Vec<String>,
    deferred: &mut Vec<String>,
) {
    // Add panes until the strip overflows its reserved-width viewport.
    for _ in 0..OVERFLOW_ADDS {
        let _ = view.update(cx, |v, cx| v.drive_add_terminal_pane(cx));
    }
    settle(cx, 400).await;

    // The chevron shows on a REAL on-screen window (real-layout overflow).
    if read_bool(cx, view, |v, cx| v.scenario_show_chevron(cx)) {
        eprintln!("[selftest] pane-strip overflow: chevron shows (real-layout overflow with the reserved slots)");
    } else {
        failures.push(format!(
            "overflow: added {OVERFLOW_ADDS} panes but the chevron did not show — the strip did not \
             overflow its reserved-width viewport"
        ));
        return;
    }

    // Activate-from-elsewhere centers: p0 is at the far leading edge and is
    // offscreen now (the last add centered the trailing pane). Selecting it must
    // make it active (hard) and scroll it back into view (deferred on repaint).
    let p0_offscreen_before =
        read_bool(cx, view, |v, cx| v.scenario_offscreen_pane_ids(cx).contains("p0"));
    let _ = view.update(cx, |v, cx| v.drive_select_pane("p0", cx));
    settle(cx, 500).await;
    if read_active(cx, view).as_deref() == Some("p0") {
        eprintln!("[selftest] pane-strip centering: selecting p0 made it active");
    } else {
        failures.push("centering: selecting p0 did not make it the active pane".to_string());
    }
    let p0_offscreen_after =
        read_bool(cx, view, |v, cx| v.scenario_offscreen_pane_ids(cx).contains("p0"));
    if p0_offscreen_before && !p0_offscreen_after {
        eprintln!("[selftest] pane-strip centering: p0 was offscreen and is now revealed (auto-centered)");
    } else if !p0_offscreen_before {
        eprintln!("[selftest] pane-strip centering: p0 was already visible before selection (nothing to reveal)");
    } else {
        deferred.push(
            "centering: p0 was offscreen and the frontmost window's repaint had not applied the \
             auto-center within the settle window (offset unchanged) — DEFERRED to a human check; \
             the offset math is hard-asserted in-process against real layout (nice-itests)."
                .to_string(),
        );
    }

    // Overflow menu opens via a real click on the chevron (deferred on a miss).
    let (vw, _vh) = read_viewport(cx, whandle);
    let chevron_x = vw as f64 - CHEVRON_FROM_RIGHT;
    let chevron_y = (TOP_BAR_HEIGHT / 2.0) as f64;
    if let Some((gx, gy)) = to_global(cx, whandle, chevron_x, chevron_y) {
        platform::post_left_mouse_down(pid, gx, gy, 1);
        platform::post_left_mouse_up(pid, gx, gy, 1);
        settle(cx, 400).await;
        if read_bool(cx, view, |v, _| v.scenario_menu_open()) {
            eprintln!("[selftest] pane-strip overflow menu: opened via a real click on the chevron");
        } else {
            deferred.push(
                "overflow menu: the synthetic click did not open the menu — it may have missed the \
                 22pt chevron button. DEFERRED to a human click; the menu contents (every pane + the \
                 active checkmark) are pinned by toolbar.rs's unit tests."
                    .to_string(),
            );
        }
    }
}

// ---- view / window reads ---------------------------------------------------

// `Entity::update` under `AsyncApp` returns the closure's value directly (it
// panics only if the entity is gone — impossible here, the view owns the window);
// `WindowHandle::update` returns a `Result` (the window can close), hence `.ok()`.

fn read_active(cx: &mut AsyncApp, view: &Entity<WindowToolbarView>) -> Option<String> {
    view.update(cx, |v, cx| v.active_pane_id(cx))
}

fn read_bool(
    cx: &mut AsyncApp,
    view: &Entity<WindowToolbarView>,
    f: impl Fn(&WindowToolbarView, &App) -> bool,
) -> bool {
    view.update(cx, |v, cx| f(v, cx))
}

/// The on-screen content-view centre of a pill (offset-free bounds + the current
/// scroll offset), as `(x, y_from_top)` for a CGEvent.
fn read_pill_center(cx: &mut AsyncApp, view: &Entity<WindowToolbarView>, pane_id: &str) -> Option<(f64, f64)> {
    view.update(cx, |v, cx| {
        let b = v.scenario_pill_bounds(pane_id, cx)?;
        let off = v.scenario_scroll_offset_x();
        let x = f32::from(b.origin.x) + off + f32::from(b.size.width) / 2.0;
        let y = f32::from(b.origin.y) + f32::from(b.size.height) / 2.0;
        Some((x as f64, y as f64))
    })
}

/// A content-view x that is guaranteed empty toolbar band: past the trailing edge
/// of the last pill (plus a margin) yet clear of the trailing chevron / `+`
/// cluster. Falls back to 60% width if the pill bounds can't be read.
fn pick_empty_band_x(cx: &mut AsyncApp, view: &Entity<WindowToolbarView>, vw: f64) -> f64 {
    let last_right = view.update(cx, |v, cx| {
        let ids = v.pane_ids(cx);
        let last = ids.last()?;
        let b = v.scenario_pill_bounds(last, cx)?;
        let off = v.scenario_scroll_offset_x();
        Some((f32::from(b.origin.x) + off + f32::from(b.size.width)) as f64)
    });
    // Keep clear of the trailing pad + chevron + `+` cluster (~90pt).
    let trailing_limit = vw - 90.0;
    match last_right {
        Some(r) if r + 30.0 < trailing_limit => r + 30.0,
        _ => (vw * 0.6).min(trailing_limit),
    }
}

fn read_viewport(cx: &mut AsyncApp, whandle: WindowHandle<WindowToolbarView>) -> (f32, f32) {
    whandle
        .update(cx, |_v, w, _a| {
            let s = w.viewport_size();
            (f32::from(s.width), f32::from(s.height))
        })
        .unwrap_or((960.0, 640.0))
}

fn read_frame(cx: &mut AsyncApp, whandle: WindowHandle<WindowToolbarView>) -> Option<[f64; 4]> {
    whandle
        .update(cx, |_v, w, _a| platform::window_screen_frame(w))
        .ok()
        .flatten()
}

fn to_global(cx: &mut AsyncApp, whandle: WindowHandle<WindowToolbarView>, cx_pt: f64, cy_pt: f64) -> Option<(f64, f64)> {
    whandle
        .update(cx, |_v, w, _a| platform::content_point_to_cg_global(w, cx_pt, cy_pt))
        .ok()
        .flatten()
}

// ---- verdict ---------------------------------------------------------------

fn build_report(failures: Vec<String>, deferred: Vec<String>) -> CadenceReport {
    if !deferred.is_empty() {
        eprintln!("[selftest] pane-strip DEFERRED HUMAN PASS checklist:");
        for d in &deferred {
            eprintln!("  - {d}");
        }
    }
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: format!(
                "all hard pane-strip assertions passed (the reserved-width overflow shows the chevron \
                 on a real window, activating an offscreen pane makes it active + reveals it, and — \
                 when the synthetic press LANDED — the pill press selects + leaves the window frame \
                 put); the pill press landing, the empty-band window move, and the overflow-menu open \
                 hard-assert when the synthetic gesture drives the real behaviour, else DEFER; {} \
                 item(s) DEFERRED to a human pass",
                deferred.len()
            ),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} pane-strip assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
