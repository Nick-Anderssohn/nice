//! `close-confirmation` self-test scenario — the R20.5 busy-pane close
//! confirmation gate (Validation §3).
//!
//! Drives the **shipped window** (`open_managed_window` / `build_window_root`,
//! the exact path `run` takes) with a real, hermetic (ZDOTDIR-blanked) terminal
//! shell over ONE `Application::run`. Three legs:
//!
//! * **(a) idle close is immediate** — an idle terminal pane's pill ✕ closes it
//!   with NO modal (`pending_modal().is_none()`).
//! * **(b) busy shell is gated** (the required veto assert + the ONE true
//!   `tcgetpgrp` leg) — a pane's interactive shell is given a REAL foreground child
//!   (`sleep`), polled to `has_foreground_child()` true; the pill-✕ close vetoes
//!   (window/tab stay, `pending_modal().is_some()`, the modal's "Force quit" button
//!   is a live AX node). While that modal is up, a SECOND busy-close is issued and
//!   asserted DROPPED (the D7 re-entrancy guard — the first modal survives, nothing
//!   closes). Then **Cancel** closes nothing; a second ✕ re-opens it and **Confirm**
//!   kills the busy pane (reaping the `sleep`).
//! * **(c) `.tabs` partial-cancel** (D5) — a multi-select batch of one idle + one
//!   busy tab (the busy tab marked through the `synthetic_foreground_child` seam —
//!   the true syscall is covered once, in (b)); the idle member is hard-killed
//!   eagerly and the busy survivor is gated; on **Cancel** the idle member stays
//!   gone AND the busy survivor REMAINS — NOT a total no-op.
//!
//! ## The pill-✕ close gesture (a documented platform deviation)
//!
//! The plan specifies a **real CGEvent** click on the located pill ✕. Under the
//! shipped **full-size-content** window a `CGEventPostToPid` mouse click does not
//! hit-test to gpui content — this scenario re-verified it on-device (a
//! body-centre CGEvent click did not even select the pane), the same limitation
//! the R18 `persistence_restore_live` veto documented for the traffic-light button
//! (which drove `-[NSWindow performClose:]` instead). There is no AppKit selector
//! for a pane close, so — following that precedent — [`drive_pill_close`] asserts
//! the ✕ is a real, on-screen, locatable target (the coordinate a click would
//! strike) and then drives the EXACT pill-✕ handler (`WindowToolbarView::
//! close_pane` → the busy gate, the `app-shell` scenario's "real pill-× close
//! path"). The gate + modal are exercised end-to-end; the modal is answered via
//! `resolve`.
//!
//! ## Hermeticity
//!
//! A per-run temp tree: a sandbox `HOME`/`ZDOTDIR` (the R14 blanked stub chain)
//! and `NICE_CLAUDE_OVERRIDE` at an idle stub — the real `claude` and the real `~`
//! are NEVER touched. The modal is answered via `ConfirmationModal::resolve`. No
//! session store is installed (`save_to_store` no-ops without the Global). The
//! scenario keeps the window's Main tab populated throughout, so no close ever
//! empties the window (which would trip the dissolve terminus). Registered BEFORE
//! `multiwindow`: `open_managed_window`'s `build_window_root` only `register`s the
//! `WindowRegistry` (no quit-when-empty close observer), so `multiwindow` stays the
//! sole installer, last.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result};
use gpui::{AnyWindowHandle, AsyncApp, Entity, WindowHandle};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_term_view::TerminalSessionHandle;

use crate::app_shell::AppShellView;
use crate::toolbar::WindowToolbarView;
use crate::window_registry::WindowRegistry;
use crate::window_state::WindowState;
use crate::platform;

const POLL_MS: u64 = 100;
const READY_POLLS: usize = 60;
const READY_MARKER: &str = "NICE_CC_READY";

const ACCESSIBILITY_REMEDIATION: &str =
    "close-confirmation: Accessibility not trusted — the real-CGEvent pill-✕ close cannot be \
     posted; grant nice Accessibility and re-run";

// -- fixture -----------------------------------------------------------------

struct Fixture {
    base: PathBuf,
    home: PathBuf,
    zdotdir: PathBuf,
}

impl Fixture {
    fn build() -> Result<Self> {
        let base = std::env::temp_dir().join(format!("nice-close-confirm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).context("create fixture base")?;
        let base = base.canonicalize().context("canonicalize fixture base")?;

        let home = base.join("home");
        let zdotdir = base.join("zdotdir");
        for d in [&home, &zdotdir] {
            std::fs::create_dir_all(d).context("create fixture dir")?;
        }
        // The R14 ZDOTDIR blanked stub chain (a spec-wins rc chain — no user rc).
        crate::shell_inject::write_stubs(&zdotdir).context("write ZDOTDIR stubs")?;

        // A stub `claude` that idles forever — this scenario never spawns Claude,
        // but NICE_CLAUDE_OVERRIDE must be set so no leg can reach the real binary.
        let bin = base.join("bin");
        std::fs::create_dir_all(&bin)?;
        let stub = bin.join("claude");
        std::fs::write(&stub, "#!/bin/sh\nwhile IFS= read -r _l; do : ; done\n")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))?;
        }
        // SAFETY: single-threaded scenario setup before any pane forks.
        unsafe { std::env::set_var("NICE_CLAUDE_OVERRIDE", &stub) };

        Ok(Fixture { base, home, zdotdir })
    }
}

// -- scenario wiring ---------------------------------------------------------

/// Open the shipped `close-confirmation` window and spawn its driver. Sets the
/// hermetic `HOME`/`ZDOTDIR` injection, then opens exactly as `crate::app::run`
/// does (no `WindowRegistry` install).
pub fn open_close_confirmation_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let fixture = Fixture::build()?;
    let home = fixture.home.to_string_lossy().into_owned();
    let zdotdir = fixture.zdotdir.to_string_lossy().into_owned();

    let whandle: WindowHandle<AppShellView> = cx.update(|app| -> Result<_> {
        // Install the shipped keymap + SharedFontSettings the window builder reads
        // (run_selftest doesn't wire them; idempotent one-shot).
        crate::keymap::install_shortcuts(app);
        // The terminal panes fork with the synthetic ZDOTDIR rc chain (spec-wins),
        // so every shell in this scenario is hermetic.
        crate::app::set_scenario_shell_inject_config(app, Some(zdotdir.clone()), None);
        // SAFETY: single-threaded scenario setup.
        let prev_home = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", &home) };
        let opened = crate::app::open_managed_window(app);
        match prev_home {
            Some(h) => unsafe { std::env::set_var("HOME", h) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        Ok(opened?)
    })?;
    let any: AnyWindowHandle = whandle.into();

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_close_confirmation(acx, whandle).await;
        // Reap any lingering shells this scenario spawned, then clear the scenario
        // config + env so the later `multiwindow` scenario runs clean.
        let id = AnyWindowHandle::from(whandle).window_id();
        let _ = acx.update(|app| {
            if let Some(state) = WindowRegistry::state_for_window(app, id) {
                state.update(app, |s, _| s.teardown());
            }
            crate::app::set_scenario_shell_inject_config(app, None, None);
        });
        // SAFETY: teardown, single-threaded.
        unsafe {
            std::env::remove_var("NICE_CLAUDE_OVERRIDE");
        }
        let _ = std::fs::remove_dir_all(&fixture.base);
        eprintln!("[selftest] scenario 'close-confirmation': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(any)
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

/// Re-assert frontmost/key before a real CGEvent so it routes to this window.
async fn rekey(cx: &mut AsyncApp, whandle: WindowHandle<AppShellView>) {
    let _ = cx.update(|app| app.activate(true));
    let _ = whandle.update(cx, |_v, w, _a| w.activate_window());
    settle(cx, 250).await;
}

// -- driver ------------------------------------------------------------------

async fn run_close_confirmation(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
) -> CadenceReport {
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 700).await;

    // Accessibility preflight — FAIL loudly (never silently skip the CGEvent half).
    if !cx.update(|_| platform::accessibility_trusted()) {
        return CadenceReport::error(ACCESSIBILITY_REMEDIATION.to_string());
    }
    rekey(cx, whandle).await;

    let shell = match whandle.entity(cx) {
        Ok(v) => v,
        Err(e) => {
            return CadenceReport::error(format!(
                "close-confirmation: could not read the shell view: {e}"
            ))
        }
    };
    let toolbar = shell.update(cx, |s, _| s.scenario_toolbar());
    let id = AnyWindowHandle::from(whandle).window_id();
    let Some(state) = cx.update(|app| WindowRegistry::state_for_window(app, id)) else {
        return CadenceReport::error(
            "close-confirmation: the shipped builder did not register the window's WindowState"
                .to_string(),
        );
    };

    let pid = std::process::id() as i32;
    let mut failures: Vec<String> = Vec::new();

    let Some(main_tab) = active_tab_id(cx, &state) else {
        return CadenceReport::error(
            "close-confirmation: the shipped window has no active tab".to_string(),
        );
    };

    // (a) idle close is immediate.
    idle_close_leg(cx, whandle, &toolbar, &state, &main_tab, &mut failures).await;

    // (b) busy shell is gated (the required veto + the true tcgetpgrp leg).
    busy_close_leg(cx, whandle, &toolbar, &state, &main_tab, pid, &mut failures).await;

    // (c) .tabs partial-cancel.
    tabs_partial_cancel_leg(cx, whandle, &state, &mut failures).await;

    build_report(failures)
}

// -- leg (a): idle close is immediate ----------------------------------------

async fn idle_close_leg(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    toolbar: &Entity<WindowToolbarView>,
    state: &Entity<WindowState>,
    main_tab: &str,
    failures: &mut Vec<String>,
) {
    // Add a second terminal pane so closing it can't dissolve the Main tab.
    let Some(pane) = add_spawned_pane(cx, toolbar, state, main_tab).await else {
        failures.push("(a) could not add + spawn a second terminal pane on the Main tab".into());
        return;
    };
    // An idle shell at a prompt has no foreground child.
    if !poll_foreground_child(cx, state, main_tab, &pane, false).await {
        failures.push(format!(
            "(a) the freshly-spawned pane {pane} still reports a foreground child (never settled to \
             an idle prompt)"
        ));
        return;
    }
    if let Err(e) = drive_pill_close(cx, whandle, toolbar, &pane).await {
        failures.push(format!("(a) {e}"));
        return;
    }
    // The idle pane closes immediately, with NO modal.
    let mut gone = false;
    for _ in 0..READY_POLLS {
        settle(cx, POLL_MS).await;
        if !toolbar_pane_ids(cx, toolbar).contains(&pane) {
            gone = true;
            break;
        }
    }
    if !gone {
        failures.push(format!(
            "(a) the idle pill-✕ close did not remove pane {pane} (the real CGEvent missed the ✕, or \
             the idle path did not close)"
        ));
    }
    if state.update(cx, |s, _| s.pending_modal().is_some()) {
        failures.push("(a) an idle close presented a confirmation modal (must be immediate)".into());
        // Clear it so it can't leak into leg (b).
        resolve_modal(cx, whandle, state, false);
    }
    if gone && !state.update(cx, |s, _| s.pending_modal().is_some()) {
        eprintln!("[selftest] close-confirmation (a): idle pill-✕ closed the pane immediately, no modal");
    }
}

// -- leg (b): busy shell is gated (the required veto + true tcgetpgrp) --------

async fn busy_close_leg(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    toolbar: &Entity<WindowToolbarView>,
    state: &Entity<WindowState>,
    main_tab: &str,
    pid: i32,
    failures: &mut Vec<String>,
) {
    let Some(pane) = add_spawned_pane(cx, toolbar, state, main_tab).await else {
        failures.push("(b) could not add + spawn a busy-candidate terminal pane".into());
        return;
    };
    // Give the interactive shell a REAL foreground child in a new process group —
    // the ONLY leg exercising the true `tcgetpgrp(master_fd) != child_pid` read.
    let Some(handle) = pane_handle(cx, state, main_tab, &pane) else {
        failures.push(format!("(b) pane {pane} spawned but its session handle vanished"));
        return;
    };
    let _ = handle.update(cx, |h, _| {
        let _ = h.session().write_input(b"sleep 30\n");
    });
    if !poll_foreground_child(cx, state, main_tab, &pane, true).await {
        failures.push(format!(
            "(b) pane {pane}'s shell never reported a foreground child after `sleep 30` — the \
             interactive shell's job control did not fork it into a new pgroup (tcgetpgrp == \
             child_pid)"
        ));
        return;
    }
    eprintln!("[selftest] close-confirmation (b): the busy shell reports a real foreground child (tcgetpgrp != child_pid)");

    // First close: the busy pane is GATED — the window/tab stay, the modal shows.
    if let Err(e) = drive_pill_close(cx, whandle, toolbar, &pane).await {
        failures.push(format!("(b) {e}"));
        return;
    }
    if !poll_modal(cx, state, true).await {
        failures.push(format!(
            "(b) closing busy pane {pane} presented NO confirmation modal (the busy gate did not \
             interpose)"
        ));
        return;
    }
    if !toolbar_pane_ids(cx, toolbar).contains(&pane) {
        failures.push(format!("(b) the busy close removed pane {pane} without confirmation (no veto)"));
        return;
    }
    // The modal's confirm ("Force quit") button is a live AX node.
    if !poll_confirm_button_ax(cx, state, pid).await {
        failures.push(
            "(b) the confirmation modal's confirm button never surfaced as an AXButton in the AX tree"
                .into(),
        );
    }

    // D7 re-entrancy guard (Validation §2(d)): while THIS modal is up, a second
    // busy-close must be dropped-and-logged, never clobber the live `pending_modal`
    // (Swift's `requestCloseTabs` drop-and-log `:160-163`). The Main tab still holds
    // this busy pane, so `request_close_tab(main_tab)` WOULD present a second modal
    // if unguarded; assert instead that the FIRST modal survives untouched (same
    // entity — its completion is not stranded) and nothing closed.
    let first_modal = state.update(cx, |s, _| s.pending_modal());
    let _ = whandle.update(cx, |_root, window, app| {
        state.update(app, |ws, wcx| ws.request_close_tab(main_tab, window, wcx));
    });
    settle(cx, 200).await;
    match (first_modal, state.update(cx, |s, _| s.pending_modal())) {
        (Some(before), Some(after)) if before == after => {
            eprintln!(
                "[selftest] close-confirmation (b): a second busy-close while the modal was up was \
                 dropped (D7 re-entrancy guard) — the first modal survived intact"
            );
        }
        (Some(_), Some(_)) => failures.push(
            "(b) a second busy-close while the modal was up REPLACED the live modal (D7 guard \
             clobbered the first modal's completion)"
                .into(),
        ),
        (Some(_), None) => failures.push(
            "(b) a second busy-close while the modal was up dismissed the live modal (D7 guard \
             should have dropped the second call, leaving the first modal up)"
                .into(),
        ),
        _ => failures
            .push("(b) the busy modal vanished before the D7 re-entrancy probe could run".into()),
    }
    if !toolbar_pane_ids(cx, toolbar).contains(&pane) {
        failures.push(format!(
            "(b) the dropped second busy-close still closed pane {pane} (the D7 guard must be a \
             total no-op)"
        ));
        return;
    }

    // Cancel closes nothing.
    resolve_modal(cx, whandle, state, false);
    settle(cx, 300).await;
    if state.update(cx, |s, _| s.pending_modal().is_some()) {
        failures.push("(b) the confirmation did not dismiss on Cancel".into());
    }
    if !toolbar_pane_ids(cx, toolbar).contains(&pane) {
        failures.push(format!("(b) Cancel closed busy pane {pane} (must be a no-op)"));
        return;
    }
    eprintln!("[selftest] close-confirmation (b): the busy pill-✕ vetoed; Cancel kept the pane open");

    // Re-open the modal with a second pill-✕ close, then Confirm → the pane (and
    // its `sleep` child) is killed.
    if let Err(e) = drive_pill_close(cx, whandle, toolbar, &pane).await {
        failures.push(format!("(b) re-close {e}"));
        return;
    }
    if !poll_modal(cx, state, true).await {
        failures.push(format!("(b) the second busy close on {pane} presented no modal"));
        return;
    }
    resolve_modal(cx, whandle, state, true);
    let mut killed = false;
    for _ in 0..READY_POLLS {
        settle(cx, POLL_MS).await;
        if !toolbar_pane_ids(cx, toolbar).contains(&pane) {
            killed = true;
            break;
        }
    }
    if killed {
        eprintln!("[selftest] close-confirmation (b): Confirm force-quit the busy pane (sleep reaped)");
    } else {
        failures.push(format!("(b) Confirm did not close the busy pane {pane}"));
    }
}

// -- leg (c): .tabs partial-cancel -------------------------------------------

async fn tabs_partial_cancel_leg(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    state: &Entity<WindowState>,
    failures: &mut Vec<String>,
) {
    // Two fresh model-only terminal tabs in the Terminals group: one idle, one
    // marked busy through the synthetic seam (the true syscall is covered by (b)).
    let idle_tab = state.update(cx, |s, _| s.sidebar_actions.create_terminal_tab(&mut s.model));
    let busy_tab = state.update(cx, |s, _| s.sidebar_actions.create_terminal_tab(&mut s.model));
    let (Some(idle_tab), Some(busy_tab)) = (idle_tab, busy_tab) else {
        failures.push("(c) could not create the idle + busy terminal tabs".into());
        return;
    };
    let busy_pane = state.update(cx, |s, _| {
        s.model
            .tab_for(&busy_tab)
            .and_then(|t| t.panes.first().map(|p| p.id.clone()))
    });
    let Some(busy_pane) = busy_pane else {
        failures.push("(c) the busy tab has no seeded pane to mark busy".into());
        return;
    };
    // Mark the busy tab's shell as holding a foreground child (the test seam).
    state.update(cx, |s, _| {
        s.session.mark_synthetic_foreground_child(&busy_tab, &busy_pane);
    });

    // Drive the multi-select close over BOTH ids (the partial-eager `.tabs` flow).
    let ids = vec![idle_tab.clone(), busy_tab.clone()];
    let _ = whandle.update(cx, |_root, window, app| {
        state.update(app, |ws, wcx| ws.request_close_tabs(&ids, window, wcx));
    });
    settle(cx, 400).await;

    // The idle member is hard-killed eagerly; the busy survivor is gated.
    if tab_exists(cx, state, &idle_tab) {
        failures.push(format!("(c) the idle tab {idle_tab} was not eagerly closed before the dialog"));
    }
    if !tab_exists(cx, state, &busy_tab) {
        failures.push(format!("(c) the busy tab {busy_tab} was closed instead of gated"));
    }
    if !state.update(cx, |s, _| s.pending_modal().is_some()) {
        failures.push("(c) the multi-select busy close presented no .tabs modal".into());
        return;
    }

    // Cancel: the busy survivor REMAINS, the idle member stays gone (partial close).
    resolve_modal(cx, whandle, state, false);
    settle(cx, 300).await;
    if state.update(cx, |s, _| s.pending_modal().is_some()) {
        failures.push("(c) the .tabs confirmation did not dismiss on Cancel".into());
    }
    if !tab_exists(cx, state, &busy_tab) {
        failures.push(format!("(c) Cancel closed the busy survivor {busy_tab} (must remain alive)"));
    }
    if tab_exists(cx, state, &idle_tab) {
        failures.push(format!(
            "(c) the eagerly-closed idle tab {idle_tab} came back on Cancel (must stay closed — a \
             PARTIAL close, not a total no-op)"
        ));
    }
    if !failures.iter().any(|f| f.starts_with("(c)")) {
        eprintln!("[selftest] close-confirmation (c): partial-cancel — idle tab gone, busy survivor kept on Cancel");
    }
    // The busy survivor is a model-only tab with only a synthetic marker; the
    // driver's teardown (`state.teardown()`, which clears the synthetic sets)
    // reaps it — no dangling real child, and the modal was already cancelled.
}

// -- shared drive helpers ----------------------------------------------------

/// Add a terminal pane to `tab` through the shipped strip `+` seam and poll it
/// spawned (the pane host deferred-spawns the new active pane). Returns its id.
async fn add_spawned_pane(
    cx: &mut AsyncApp,
    toolbar: &Entity<WindowToolbarView>,
    state: &Entity<WindowState>,
    tab: &str,
) -> Option<String> {
    let before = toolbar_pane_ids(cx, toolbar);
    let _ = toolbar.update(cx, |v, cx| v.drive_add_terminal_pane(cx));
    settle(cx, 300).await;
    let after = toolbar_pane_ids(cx, toolbar);
    let pane = after.into_iter().find(|p| !before.contains(p))?;
    // Poll spawned + push a readiness echo through so the interactive shell is live.
    for _ in 0..READY_POLLS {
        if state.update(cx, |s, _| s.session.has_pane(tab, &pane)) {
            break;
        }
        settle(cx, POLL_MS).await;
    }
    let handle = pane_handle(cx, state, tab, &pane)?;
    let echo = format!("echo {READY_MARKER}\n");
    let _ = handle.update(cx, |h, _| {
        let _ = h.session().write_input(echo.as_bytes());
    });
    for _ in 0..READY_POLLS {
        settle(cx, POLL_MS).await;
        let grid = handle.update(cx, |h, _| h.session().grid_lines().join("\n"));
        if grid.contains(READY_MARKER) {
            return Some(pane);
        }
    }
    // The shell may still be usable even if the marker didn't render in time; hand
    // the pane back and let the caller's foreground-child poll be the real gate.
    Some(pane)
}

/// Drive `pane_id`'s pill-✕ close through the SHIPPED handler — the exact method
/// (`WindowToolbarView::close_pane` → [`WindowState::request_close_pane`], the
/// busy-close gate) the pill's `on_mouse_down` invokes. Selects the pane first
/// (so its ✕ is laid out + would be hit-testable) and asserts the ✕ is a real,
/// on-screen target via [`WindowToolbarView::scenario_close_button_center`].
///
/// A literal synthetic CGEvent click on the ✕ is NOT usable in the shipped window:
/// under its **full-size-content** style a `CGEventPostToPid` mouse click does not
/// hit-test to gpui content (an on-device limitation this scenario re-verified — a
/// body-centre CGEvent click did not even select the pane; the same limitation the
/// R18 `persistence_restore_live` veto documented for the traffic-light button,
/// which drove `-[NSWindow performClose:]` instead). There is no AppKit selector
/// for a pane close, so — following that pattern — we drive the real pill-✕
/// handler directly (the `app-shell` scenario's "real pill-× close path") after
/// asserting the ✕'s locatable frame; the busy gate + modal are still exercised
/// end-to-end, and the modal is answered via `resolve`.
///
/// Returns `Err(msg)` if the ✕ has no locatable on-screen target (a real
/// regression — the close affordance vanished).
async fn drive_pill_close(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    toolbar: &Entity<WindowToolbarView>,
    pane_id: &str,
) -> Result<(), String> {
    rekey(cx, whandle).await;
    let _ = whandle.update(cx, |_root, window, app| {
        toolbar.update(app, |v, cx| v.drive_select_pane(pane_id, window, cx));
    });
    settle(cx, 250).await;
    // Assert the ✕ is a real, laid-out, on-screen target (the coordinate a real
    // click would strike) before driving the handler — mirrors the persistence
    // veto asserting the traffic-light frame is locatable.
    let center = toolbar.update(cx, |v, cx| v.scenario_close_button_center(pane_id, cx));
    let vh = whandle
        .update(cx, |_v, w, _a| {
            let s = w.viewport_size();
            (f32::from(s.width), f32::from(s.height))
        })
        .ok();
    match (center, vh) {
        (Some((x, y)), Some((vw, vh)))
            if x > 0.0 && y > 0.0 && x <= vw && y <= vh =>
        {
            let gxy = whandle
                .update(cx, |_v, w, _a| {
                    platform::content_point_to_cg_global(w, x as f64, y as f64)
                })
                .ok()
                .flatten();
            if gxy.is_none() {
                return Err(format!(
                    "pane {pane_id}'s ✕ centre ({x:.0},{y:.0}) did not resolve to a screen point"
                ));
            }
        }
        (Some((x, y)), Some((vw, vh))) => {
            return Err(format!(
                "pane {pane_id}'s ✕ centre ({x:.0},{y:.0}) is off the {vw:.0}x{vh:.0} content view"
            ));
        }
        _ => return Err(format!("pane {pane_id}'s ✕ is not laid out (no locatable close target)")),
    }
    // Drive the real pill-✕ handler.
    let _ = whandle.update(cx, |_root, window, app| {
        toolbar.update(app, |v, cx| v.drive_close_pane(pane_id, window, cx));
    });
    settle(cx, 350).await;
    Ok(())
}

/// Poll until `pane`'s shell foreground-child signal matches `want` (the true
/// `tcgetpgrp` read via the live session handle).
async fn poll_foreground_child(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab: &str,
    pane: &str,
    want: bool,
) -> bool {
    for _ in 0..READY_POLLS {
        if let Some(h) = pane_handle(cx, state, tab, pane) {
            if h.update(cx, |h, _| h.has_foreground_child()) == want {
                return true;
            }
        }
        settle(cx, POLL_MS).await;
    }
    false
}

async fn poll_modal(cx: &mut AsyncApp, state: &Entity<WindowState>, want: bool) -> bool {
    for _ in 0..READY_POLLS {
        if state.update(cx, |s, _| s.pending_modal().is_some()) == want {
            return true;
        }
        settle(cx, POLL_MS).await;
    }
    false
}

/// Poll the process AX tree until the modal's confirm button surfaces as an
/// `AXButton` (forcing a repaint per tick — the modal has no RAF, and AccessKit
/// materializes lazily; the `persistence-restore` veto pattern).
async fn poll_confirm_button_ax(cx: &mut AsyncApp, state: &Entity<WindowState>, pid: i32) -> bool {
    for _ in 0..READY_POLLS {
        let _ = state.update(cx, |_s, c| c.notify());
        settle(cx, POLL_MS).await;
        if matches!(
            platform::ax_find_titled_role(pid, crate::confirmation_modal::CONFIRM_ACCEPT_ID),
            Ok(role) if role == "AXButton"
        ) {
            return true;
        }
    }
    false
}

/// Drive the pending modal's Cancel / Confirm answer directly (only the close
/// gesture is a real CGEvent, per the hermeticity rule).
fn resolve_modal(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    state: &Entity<WindowState>,
    confirmed: bool,
) {
    let modal = state.update(cx, |s, _| s.pending_modal());
    if let Some(modal) = modal {
        let _ = whandle.update(cx, |_root, window, app| {
            modal.update(app, |m, mcx| m.resolve(confirmed, window, mcx));
        });
    }
}

// -- reads -------------------------------------------------------------------

fn active_tab_id(cx: &mut AsyncApp, state: &Entity<WindowState>) -> Option<String> {
    state.update(cx, |s, _| s.model.active_tab_id().map(str::to_string))
}

fn tab_exists(cx: &mut AsyncApp, state: &Entity<WindowState>, tab: &str) -> bool {
    state.update(cx, |s, _| s.model.tab_for(tab).is_some())
}

fn toolbar_pane_ids(cx: &mut AsyncApp, toolbar: &Entity<WindowToolbarView>) -> Vec<String> {
    toolbar.update(cx, |v, cx| v.pane_ids(cx))
}

fn pane_handle(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab: &str,
    pane: &str,
) -> Option<Entity<TerminalSessionHandle>> {
    state.update(cx, |s, _| s.session.pane_handle(tab, pane))
}

// -- report ------------------------------------------------------------------

fn build_report(failures: Vec<String>) -> CadenceReport {
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "close-confirmation OK: (a) idle pill-✕ closed immediately with no modal; \
                     (b) a busy shell (real foreground child, true tcgetpgrp) was gated — modal \
                     shown with a live AXButton, a second busy-close while it was up was dropped \
                     (D7 re-entrancy guard), Cancel kept it open, Confirm force-quit it; \
                     (c) a .tabs batch partial-closed (idle eager-killed, busy survivor kept on \
                     Cancel)"
                .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} close-confirmation assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
