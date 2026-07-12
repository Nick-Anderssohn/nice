//! `update-check` self-test scenario — the R27 update pill + popover LIVE gate
//! (Validation §4), the update-checker sibling of [`crate::pane_strip_live`]. It
//! mounts the real [`WindowToolbarView`] over a seeded tab on a real frontmost
//! NSWindow and drives the WHOLE nudge through the INJECTED
//! [`crate::release_check::ReleaseFetcherGlobal`] recording fake — never the real
//! network / `github.com`, and never the launch timer (the worker is not started
//! under `run_selftest`).
//!
//! Two legs:
//!
//!   * **Success leg.** Script the recording fetcher to a newer tag (`v9.9.9`),
//!     drive the foreground `check_now`, and assert `update_available` flips. The
//!     trailing pill then renders — proven BOTH deterministically (the render gate)
//!     AND on the real AX tree (an `AXButton` titled `"Update available"`). A real
//!     guarded-HID click on the pill (behind the mandatory preflight: activate +
//!     raise + `CGWindowListCopyWindowInfo` frontmost-at-point) opens the popover —
//!     hard-asserted when the preflight passes, else DEFERRED LOUDLY (never a blind
//!     global post). The popover's contents (`brew update` + `brew upgrade --cask
//!     nice` + a Copy per row) and one Copy → clipboard write are asserted
//!     deterministically in-process, so a DEFERRED real click never weakens the
//!     content coverage.
//!   * **Error leg.** Reset the checker to a clean state, script a fetch ERROR,
//!     drive `check_now`, and assert `update_available` STAYS false and NO pill
//!     renders — the silent-error + layout-stability contract
//!     (`ReleaseChecker.swift:140-142`).
//!
//! Keys are never typed here (no chord), so no `SavedInputSource` is needed; the
//! only synthetic input is the one guarded global-HID click, fenced by its
//! preflight. Neither this nor any in-process test asserts cadence / perf.

use std::time::{Duration, Instant};

use anyhow::Result;
use gpui::{prelude::*, AnyWindowHandle, App, AsyncApp, Entity, WindowHandle};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_model::{Pane, PaneKind, TabModel};

use crate::app;
use crate::platform;
use crate::release_check::{self, release_fetch};
use crate::toolbar::WindowToolbarView;
use crate::window_state::WindowState;

/// The newer tag the recording fetcher reports (clearly `> 0.1.0`, the unbundled
/// `CARGO_PKG_VERSION` the checker compares against under `cargo run`).
const NEWER_TAG: &str = "v9.9.9";
/// The first popover command — the clipboard assertion target.
const BREW_UPDATE: &str = "brew update";
/// The second popover command (`brew upgrade --cask <cask>`, the frozen cask).
const BREW_UPGRADE: &str = "brew upgrade --cask nice";
/// The pill's AX title (`aria_label`) + expected role.
const PILL_AX_TITLE: &str = "Update available";
const PILL_AX_ROLE: &str = "AXButton";

/// How long to poll for a state/AX/popover transition before giving up.
const POLL_TIMEOUT: Duration = Duration::from_secs(4);

/// Accessibility-grant remediation, shared verbatim with the other CGEvent
/// scenarios.
const ACCESSIBILITY_REMEDIATION: &str = "\
Accessibility (TCC) grant missing: AXIsProcessTrusted() == false, so synthetic \
events are SILENTLY DROPPED and no injected click can reach the window. Fix: \
System Settings → Privacy & Security → Accessibility → enable the process hosting \
this run (normally the terminal app). If it shows ON but this persists, the grant \
is STALE — remove it with '-' and re-add it, then re-run. Verify: swift -e \
'import ApplicationServices; print(AXIsProcessTrusted())'";

// ===========================================================================
// scenario wiring
// ===========================================================================

/// Open the `update-check` scenario window — the real [`WindowToolbarView`] over a
/// seeded single-pane Main tab. Spawns the driver (self-reported gate).
pub fn open_update_check_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let model = seed_model();
    let whandle: WindowHandle<WindowToolbarView> = cx.open_window(app::window_options(), {
        move |_window, cx| {
            let state = cx.new(|_cx| WindowState::with_model(model));
            cx.new(|cx| WindowToolbarView::new(state, cx))
        }
    })?;
    let any: AnyWindowHandle = whandle.into();

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_update_check(acx, whandle).await;
        eprintln!("[selftest] scenario 'update-check': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(any)
}

/// The fixture: the pinned Main terminal tab with a single terminal pill (the
/// strip renders purely from the model; the pill visibility comes from the
/// process-wide checker, not the model).
fn seed_model() -> TabModel {
    let mut m = TabModel::new("/tmp");
    let tab_id = TabModel::MAIN_TERMINAL_TAB_ID.to_string();
    let (pi, ti) = m.project_tab_index(&tab_id).expect("main tab exists");
    m.projects[pi].tabs[ti].panes = vec![Pane::new("p0", "Terminal 1", PaneKind::Terminal)];
    m.projects[pi].tabs[ti].active_pane_id = Some("p0".to_string());
    m.projects[pi].tabs[ti].next_terminal_index = 2;
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

async fn run_update_check(cx: &mut AsyncApp, whandle: WindowHandle<WindowToolbarView>) -> CadenceReport {
    // Self-activate + settle so the window is frontmost/key and has painted once.
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 700).await;

    // The recording fetcher was installed process-wide by `run_selftest` — grab
    // its handle to script the tag/error. Its absence is a wiring bug, not a DEFER.
    let Some(fake) = release_fetch::selftest_fake() else {
        return CadenceReport::error(
            "update-check: the recording ReleaseFetcher was not installed by run_selftest \
             (release_fetch::selftest_fake() is None) — the hermetic fetch seam is missing"
                .to_string(),
        );
    };

    let view = match whandle.entity(cx) {
        Ok(v) => v,
        Err(e) => return CadenceReport::error(format!("update-check: could not read the root view: {e}")),
    };
    let pid = std::process::id() as i32;
    let mut failures: Vec<String> = Vec::new();
    let mut deferred: Vec<String> = Vec::new();

    // --- ERROR LEG (first: the checker is still clean — empty selftest cache) ---
    // A fetch ERROR must leave update_available false + render no pill (the silent-
    // error + layout-stability contract, `ReleaseChecker.swift:140-142`). Run this
    // BEFORE the success leg caches a newer tag, so the clean start is genuine.
    if cx.update(|app| release_check::update_available(app)).is_some() {
        // Defensive: the run_selftest checker seeds from an empty temp cache.
        failures.push("error: the checker did not start clean (update_available already set)".to_string());
    }
    fake.set_error(release_fetch::FetchError::Transport("offline".into()));
    cx.update(|app| release_check::check_now(app));
    settle(cx, 500).await;
    // Give the marshal-back ample time; the flag must NOT flip.
    let stayed_false = !poll_until(cx, Duration::from_millis(800), |app| {
        release_check::update_available(app).is_some()
    })
    .await;
    if stayed_false {
        eprintln!("[selftest] update-check: a fetch error left update_available false (no pill) — the silent-error contract holds");
    } else {
        failures.push("error: a fetch error flipped update_available true — errors must be swallowed (no pill)".to_string());
    }
    if read_bool(cx, &view, |v, cx| v.scenario_update_pill_visible(cx)) {
        failures.push("error: the pill render gate is true after a fetch error — no pill may render".to_string());
    }

    // --- SUCCESS LEG -------------------------------------------------------
    fake.set_tag(NEWER_TAG);
    cx.update(|app| release_check::check_now(app));
    // The recording fetch is instant; poll the foreground flag as the marshal-back
    // applies it (+ a `refresh_windows` that repaints the pill).
    let flipped = poll_until(cx, POLL_TIMEOUT, |app| {
        release_check::update_available(app).as_deref() == Some(NEWER_TAG)
    })
    .await;
    if !flipped {
        failures.push(format!(
            "success: after check_now with the fetcher set to {NEWER_TAG}, \
             release_check::update_available did not become Some(\"{NEWER_TAG}\")"
        ));
        return build_report(failures, deferred);
    }

    // The pill's render gate is satisfied (deterministic).
    if !read_bool(cx, &view, |v, cx| v.scenario_update_pill_visible(cx)) {
        failures.push("success: update_available is set but the pill render gate is false".to_string());
    }

    // The pill surfaces on the REAL AX tree as an AXButton titled "Update available".
    if let Some(role) = poll_ax(cx, &view, pid, PILL_AX_TITLE).await {
        if role == PILL_AX_ROLE {
            eprintln!("[selftest] update-check: the pill is exposed as an {PILL_AX_ROLE} titled '{PILL_AX_TITLE}'");
        } else {
            failures.push(format!(
                "success: the pill's AX element is titled '{PILL_AX_TITLE}' but its role is '{role}', not '{PILL_AX_ROLE}'"
            ));
        }
    } else {
        failures.push(format!(
            "success: no AX element titled '{PILL_AX_TITLE}' surfaced within {POLL_TIMEOUT:?} — the \
             pill did not expose on the AX tree"
        ));
    }

    // Real guarded-HID click on the pill → the popover opens (hard when the
    // preflight passes; DEFER LOUDLY otherwise — never a blind global post).
    real_pill_click_leg(cx, whandle, &view, &mut failures, &mut deferred).await;

    // Deterministic content: open the popover in-process and assert the two exact
    // brew commands + one Copy → clipboard (so a DEFERRED real click never weakens
    // the content coverage — the pane-strip in-process-pins-content precedent).
    content_leg(cx, whandle, &view, &mut failures).await;

    // Drop the popover — the scenario's assertions are complete.
    let _ = view.update(cx, |v, cx| v.drive_dismiss_update_popover(cx));
    settle(cx, 150).await;

    build_report(failures, deferred)
}

/// The real guarded-HID pill-click leg. Preflight (activate + raise +
/// frontmost-at-point) then post; hard-assert the popover opened. DEFER LOUDLY on
/// a failed preflight or an unread pill centre — never a blind global post.
async fn real_pill_click_leg(
    cx: &mut AsyncApp,
    whandle: WindowHandle<WindowToolbarView>,
    view: &Entity<WindowToolbarView>,
    failures: &mut Vec<String>,
    deferred: &mut Vec<String>,
) {
    if !platform::accessibility_trusted() {
        // No point posting a click that will be dropped — surface the grant issue
        // as a DEFER (the content leg still runs in-process).
        deferred.push(format!("real click: {ACCESSIBILITY_REMEDIATION}"));
        return;
    }
    // Ensure the popover is closed so the click's job is unambiguous (open it).
    let _ = view.update(cx, |v, cx| v.drive_dismiss_update_popover(cx));
    settle(cx, 150).await;

    // Activate + raise our window (the preflight's first half).
    let _ = cx.update(|app| app.activate(true));
    let _ = whandle.update(cx, |_v, w, _a| w.activate_window());
    settle(cx, 300).await;

    let Some((cx_pt, cy_pt)) = view.update(cx, |v, _cx| v.scenario_update_pill_center()) else {
        deferred.push(
            "real click: the pill's painted bounds were not recorded — cannot target the pill for a \
             guarded-HID click. DEFERRED; the popover open + contents are asserted in-process."
                .to_string(),
        );
        return;
    };
    let Some((gx, gy)) = to_global(cx, whandle, cx_pt, cy_pt) else {
        deferred.push("real click: could not convert the pill centre to CG-global coords — DEFERRED".to_string());
        return;
    };
    // The z-order half of the preflight: our window must own the pill point.
    if !platform::frontmost_window_owns_point(gx, gy) {
        deferred.push(format!(
            "real click: the frontmost-at-point preflight FAILED — our window does not own the pill \
             point ({gx:.0},{gy:.0}) per CGWindowListCopyWindowInfo (another window is on top, or the \
             point is off our window). DEFERRED LOUDLY; NO global click was posted. Bring the nice \
             window frontmost and re-run for the real-click assertion; the popover open + contents are \
             asserted in-process."
        ));
        return;
    }
    // Preflight passed — post the real global-HID click and HARD-ASSERT the open.
    platform::post_global_left_click(gx, gy, 1);
    let opened = poll_until_view(cx, view, POLL_TIMEOUT, |v, _cx| v.scenario_update_popover_open()).await;
    if opened {
        eprintln!("[selftest] update-check: a real guarded-HID click on the pill opened the popover (preflight passed)");
        let _ = view.update(cx, |v, cx| v.drive_dismiss_update_popover(cx));
        settle(cx, 150).await;
    } else {
        failures.push(
            "real click: the frontmost-at-point preflight passed and a global-HID click was posted at \
             the pill centre, but the popover did not open — the pill click handler did not present it"
                .to_string(),
        );
    }
}

/// Assert the popover's contents deterministically: open it in-process, read the
/// two exact brew commands (a Copy per row), and drive one Copy → clipboard.
async fn content_leg(
    cx: &mut AsyncApp,
    whandle: WindowHandle<WindowToolbarView>,
    view: &Entity<WindowToolbarView>,
    failures: &mut Vec<String>,
) {
    // The toolbar IS this window's root view — drive it through the window-update
    // context (a window is needed to focus-grab the popover).
    let _ = whandle.update(cx, |v, window, cx| v.drive_open_update_popover(window, cx));
    settle(cx, 200).await;

    let commands = view.update(cx, |v, cx| v.scenario_update_popover_commands(cx));
    match commands {
        Some(cmds) => {
            let has_update = cmds.iter().any(|c| c == BREW_UPDATE);
            let has_upgrade = cmds.iter().any(|c| c == BREW_UPGRADE);
            if cmds.len() == 2 && has_update && has_upgrade && cmds[0] == BREW_UPDATE && cmds[1] == BREW_UPGRADE {
                eprintln!("[selftest] update-check: the popover shows '{BREW_UPDATE}' then '{BREW_UPGRADE}' (two Copy rows)");
            } else {
                failures.push(format!(
                    "content: the popover commands were {cmds:?}, not exactly ['{BREW_UPDATE}', '{BREW_UPGRADE}']"
                ));
            }
        }
        None => failures.push("content: the popover was not open after drive_open_update_popover".to_string()),
    }

    // Drive one Copy (row 0) and assert the clipboard holds the command.
    let _ = view.update(cx, |v, cx| v.drive_copy_update_command(0, cx));
    settle(cx, 100).await;
    let clip = cx.update(|app| app.read_from_clipboard().and_then(|it| it.text()));
    if clip.as_deref() == Some(BREW_UPDATE) {
        eprintln!("[selftest] update-check: Copy on row 0 wrote '{BREW_UPDATE}' to the clipboard");
    } else {
        failures.push(format!(
            "content: after Copy on row 0 the clipboard held {clip:?}, not Some(\"{BREW_UPDATE}\")"
        ));
    }
    let _ = view.update(cx, |v, cx| v.drive_dismiss_update_popover(cx));
    settle(cx, 100).await;
}

// ---- polling / reads -------------------------------------------------------

/// Poll `pred` against the foreground `App` until true or `timeout`, repainting
/// the root view each tick (so a Global-driven state change surfaces + AccessKit
/// refreshes). Returns whether it became true.
async fn poll_until(
    cx: &mut AsyncApp,
    timeout: Duration,
    pred: impl Fn(&App) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if cx.update(|app| pred(app)) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        settle(cx, 80).await;
    }
}

/// Poll `pred` against the root view until true or `timeout`. Returns whether it
/// became true.
async fn poll_until_view(
    cx: &mut AsyncApp,
    view: &Entity<WindowToolbarView>,
    timeout: Duration,
    pred: impl Fn(&WindowToolbarView, &App) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if view.update(cx, |v, cx| pred(v, cx)) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        settle(cx, 80).await;
    }
}

/// Poll the process AX tree for an element titled `title`, forcing a repaint each
/// tick (AccessKit lazily activates on the first query then materializes on a
/// later frame — the `app-shell` precedent). Returns its role, or `None` on
/// timeout. Runs the query on THIS main-thread task (a same-process AX query
/// dispatches inline; a background query would race gpui's per-frame borrow).
async fn poll_ax(
    cx: &mut AsyncApp,
    view: &Entity<WindowToolbarView>,
    pid: i32,
    title: &str,
) -> Option<String> {
    let deadline = Instant::now() + POLL_TIMEOUT;
    loop {
        let _ = view.update(cx, |_v, cx| cx.notify());
        settle(cx, 120).await;
        if let Ok(role) = platform::ax_find_titled_role(pid, title) {
            return Some(role);
        }
        if Instant::now() >= deadline {
            return None;
        }
    }
}

fn read_bool(
    cx: &mut AsyncApp,
    view: &Entity<WindowToolbarView>,
    f: impl Fn(&WindowToolbarView, &App) -> bool,
) -> bool {
    view.update(cx, |v, cx| f(v, cx))
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
        eprintln!("[selftest] update-check DEFERRED HUMAN PASS checklist:");
        for d in &deferred {
            eprintln!("  - {d}");
        }
    }
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: format!(
                "all hard update-check assertions passed (a newer tag flips update_available + exposes \
                 the pill as an AXButton; the popover shows both exact brew commands + a Copy per row, \
                 one Copy writes to the clipboard; a fetch error stays silent with no pill); the real \
                 guarded-HID pill click hard-asserts when its preflight passes, else DEFERS; {} item(s) \
                 DEFERRED to a human pass",
                deferred.len()
            ),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} update-check assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
