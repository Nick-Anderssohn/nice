//! `move-session` self-test scenario — the session-move rework's live gate
//! (session-move-rework plan § Slice 4).
//!
//! Phase 4's detach/adopt/pool round trip is gone (Nick's feel-check: "if you
//! detach a terminal, it is still in the sidebar, but just in a different
//! spot; click it and it moves back — that's weird"). What replaced it —
//! [`crate::keymap::move_session_to_window`] /
//! [`crate::keymap::move_session_to_new_window`], the direct "Move to Window"
//! context-menu verb — reuses the SAME transfer plumbing tear-off proved
//! (`detach_session` / `adopt_entry`, no respawn). None of that is observable
//! from a unit test — the pty has to be real and the windows have to be real
//! OS windows — so this scenario drives it the way `crate::multiwindow` and
//! the deleted `detach_adopt_live` drove their own real-window gates.
//!
//! Over one real `zsh -il` whose grid carries a marker across both Move legs:
//!
//! 1. **A real marker in a real pty.** Window A opens through the SHIPPED
//!    builder (`crate::app::open_managed_window` → `build_window_root`), its
//!    eager Main shell comes up, and `echo <marker>` renders back into the
//!    live grid.
//! 2. **A second real window.** Window B opens the same way (its own eager
//!    Main, unrelated to the marker session).
//! 3. **Move to Window (existing-window leg, N2/N5).** The exact function the
//!    sidebar's "Move to <label>" menu item invokes
//!    ([`crate::keymap::move_session_to_window`]) moves A's marker session
//!    into B: the SAME `Entity<TerminalSessionHandle>` lands in B (the
//!    no-respawn proof) with the marker still in its grid, and — since the
//!    marker session was A's only one — A is emptied and closes with no
//!    confirmation (N5: nothing died, so nothing is confirmed).
//! 4. **Move to New Window (new-window leg).** The exact function the
//!    sidebar's "Move to New Window" item invokes
//!    ([`crate::keymap::move_session_to_new_window`]) moves the same session
//!    out of B into a brand-new window C, seeded with no eager Main (§B1) so
//!    it hosts nothing but the moved session: the same entity again, marker
//!    intact. B keeps its own original Main and stays open (it was not
//!    emptied) — the last-session-out close in step 3 is this scenario's
//!    proof of N5; step 4 proves the new-window leg on top of it without
//!    re-emptying a window that still has content of its own.
//!
//! ## Hermeticity
//!
//! A per-run temp tree: a sandbox `HOME` (so the login shells source no user
//! rc) and a `ZDOTDIR` pointed at it (no developer `.zshrc` output can land in
//! a grid this scenario greps for its markers — the live-run rule in
//! `docs/testing.md`). No session store Global is installed — `save_to_store`
//! no-ops without one (the `close-confirmation` scenario's precedent), and
//! this scenario asserts model/pty state, not disk state. Every window it
//! opened is closed before it reports, and both env vars are restored, so the
//! next scenario runs clean.
//!
//! **Registers the `WindowRegistry` WITHOUT `install`** (`open_managed_window`'s
//! `build_window_root` only `register`s), and drives the disk-fate half of a
//! window close through a SCOPED `on_window_closed` observer calling
//! `route_close_disk_fate` — the `persistence-restore` / `detach-adopt`
//! precedent. Quit-when-empty would kill the suite the moment window A closes
//! in step 3. Registered BEFORE `multiwindow`, the sole installer, last.
//!
//! Needs **no** Accessibility grant: both Move legs are driven by calling the
//! same functions the sidebar's context-menu items call — no CGEvent, no AX
//! read. `Gate::SelfReported`.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result};
use gpui::{AnyWindowHandle, AsyncApp, Entity, WindowHandle};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_term_view::TerminalSessionHandle;

use crate::app_shell::AppShellView;
use crate::window_registry::WindowRegistry;
use crate::window_state::WindowState;

const POLL_MS: u64 = 100;
/// Poll cap for a real login shell to come up and echo a marker back.
const GRID_POLLS: usize = 80;

/// Echoed into window A's Main pty before anything moves. Distinctive enough
/// that a login shell's own output cannot spoof it.
const MARKER: &str = "NICERS__MOVE__SESSION__OK";

// ===========================================================================
// fixture
// ===========================================================================

struct Fixture {
    base: PathBuf,
    home: PathBuf,
    prev_zdotdir: Option<String>,
}

impl Fixture {
    fn build() -> Result<Self> {
        let base = std::env::temp_dir().join(format!("nice-move-session-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).context("create fixture base")?;
        let base = base.canonicalize().context("canonicalize fixture base")?;
        let home = base.join("home");
        std::fs::create_dir_all(&home).context("create fixture home")?;
        let prev_zdotdir = std::env::var("ZDOTDIR").ok();
        // SAFETY: single-threaded scenario setup, before any window forks.
        unsafe {
            std::env::set_var("ZDOTDIR", home.to_string_lossy().as_ref());
        };
        Ok(Fixture { base, home, prev_zdotdir })
    }
}

// ===========================================================================
// scenario wiring
// ===========================================================================

/// Open the `move-session` scenario's first window through the shipped
/// builder and spawn its driver (self-reported gate).
pub fn open_move_session_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let fixture = Fixture::build()?;
    let home = fixture.home.to_string_lossy().into_owned();

    let whandle: WindowHandle<AppShellView> = cx.update(|app| -> Result<_> {
        // `move_session_to_window`/`move_session_to_new_window` are plain fn
        // calls (no action dispatch), but the shipped keymap install is
        // idempotent and cheap — keep parity with the other windowed
        // scenarios that share this process.
        crate::keymap::install_shortcuts(app);
        open_sandboxed_window(app, &home)
    })?;
    let any: AnyWindowHandle = whandle.into();

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_move_session(acx, whandle, fixture).await;
        eprintln!("[selftest] scenario 'move-session': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(any)
}

/// Open a fresh managed window with `HOME` pointed at the sandbox for the
/// duration of the call: the window's cwd, its Terminals project path and —
/// load-bearing — the env its eager Main shell forks with all read the
/// process `HOME`.
fn open_sandboxed_window(app: &mut gpui::App, home: &str) -> Result<WindowHandle<AppShellView>> {
    let prev = std::env::var("HOME").ok();
    // SAFETY: single-threaded; restored immediately below, before any await.
    unsafe { std::env::set_var("HOME", home) };
    let opened = crate::app::open_managed_window(app);
    match prev {
        Some(h) => unsafe { std::env::set_var("HOME", h) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    opened
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

// ===========================================================================
// driver
// ===========================================================================

async fn run_move_session(
    cx: &mut AsyncApp,
    whandle_a: WindowHandle<AppShellView>,
    fixture: Fixture,
) -> CadenceReport {
    cx.update(|app| app.activate(true));
    for _ in 0..20 {
        settle(cx, POLL_MS).await;
        if whandle_a
            .update(cx, |_v, w, _a| w.is_window_active())
            .unwrap_or(false)
        {
            break;
        }
    }
    settle(cx, 400).await;

    let id_a = AnyWindowHandle::from(whandle_a).window_id();
    let Some(state_a) = cx.update(|app| WindowRegistry::state_for_window(app, id_a)) else {
        return teardown(
            cx,
            fixture,
            CadenceReport::error(
                "move-session: the shipped builder did not register window A".to_string(),
            ),
        )
        .await;
    };

    // The SCOPED close observer (the `persistence-restore` / `detach-adopt`
    // precedent): route the real disk fate + pty teardown WITHOUT the
    // quit-when-empty that would kill the suite when window A closes in
    // step 3. Held for the driver's lifetime.
    let _close_sub = cx.update(|app| app.on_window_closed(WindowRegistry::route_close_disk_fate));

    let mut failures: Vec<String> = Vec::new();

    // === 1. a real marker in a real pty ====================================
    let Some((session_id, term_window_id, pane_id)) = active_pane_keys(cx, &state_a) else {
        return teardown(
            cx,
            fixture,
            CadenceReport::error(
                "move-session: window A opened with no active session/window/pane".to_string(),
            ),
        )
        .await;
    };
    let Some(handle) = cx.update(|app| {
        state_a
            .read(app)
            .ptys
            .pane_handle(&session_id, &term_window_id, &pane_id)
    }) else {
        return teardown(
            cx,
            fixture,
            CadenceReport::error(
                "move-session: window A's Main pane never got a pty handle (the eager fresh-window \
                 spawn did not run)"
                    .to_string(),
            ),
        )
        .await;
    };
    let handle_id = handle.entity_id();
    write_line(cx, &handle, &format!("echo {MARKER}"));
    if !poll_grid_contains(cx, &handle, MARKER).await {
        return teardown(
            cx,
            fixture,
            CadenceReport::error(format!(
                "move-session: window A's login shell never echoed '{MARKER}' — no live pty to move"
            )),
        )
        .await;
    }

    // === 2. a second real window ============================================
    let home = fixture.home.to_string_lossy().into_owned();
    let whandle_b = match cx.update(|app| open_sandboxed_window(app, &home)) {
        Ok(h) => h,
        Err(e) => {
            return teardown(
                cx,
                fixture,
                CadenceReport::error(format!("move-session: could not open window B: {e:#}")),
            )
            .await
        }
    };
    cx.update(|app| app.activate(true));
    let _ = whandle_b.update(cx, |_v, w, _a| w.activate_window());
    settle(cx, 400).await;

    let id_b = AnyWindowHandle::from(whandle_b).window_id();
    let Some(state_b) = cx.update(|app| WindowRegistry::state_for_window(app, id_b)) else {
        return teardown(
            cx,
            fixture,
            CadenceReport::error("move-session: window B did not register".to_string()),
        )
        .await;
    };

    // === 3. Move to Window (existing-window leg, N2/N5) =====================
    cx.update(|app| {
        crate::keymap::move_session_to_window(app, &state_a, &session_id, id_b);
    });
    settle(cx, 400).await;

    if cx.update(|app| WindowRegistry::state_for_window(app, id_a).is_some()) {
        failures.push(
            "(3) window A is still registered after the move emptied it — the last-session-out \
             close (N5) never ran"
                .to_string(),
        );
    }
    // The session re-keys on the way out (`mint_session_id`), so it is not
    // reachable in B under its old id — the load-bearing identity proof is the
    // pane HANDLE, not the session id.
    match adopted_pane_handle_by_entity(cx, &state_b, handle_id) {
        Some(_) => {}
        None => failures.push(
            "(3) window B has no pane carrying the moved pty's entity id after the move — the \
             child was respawned instead of moved, or the move silently failed"
                .to_string(),
        ),
    }
    let grid = grid_text(cx, &handle);
    if !grid.contains(MARKER) {
        failures.push(format!(
            "(3) the moved pane's grid lost '{MARKER}' — scrollback did not survive the move"
        ));
    }

    // === 4. Move to New Window (new-window leg) =============================
    let Some(moved_session_id) = cx.update(|app| {
        state_b
            .read(app)
            .ptys
            .live_pane_keys()
            .into_iter()
            .find(|(s, w, p)| {
                state_b
                    .read(app)
                    .ptys
                    .pane_handle(s, w, p)
                    .map(|h| h.entity_id() == handle_id)
                    .unwrap_or(false)
            })
            .map(|(s, _w, _p)| s)
    }) else {
        return teardown(
            cx,
            fixture,
            CadenceReport::error(
                "move-session: could not find the moved session's id in window B ahead of the \
                 new-window leg"
                    .to_string(),
            ),
        )
        .await;
    };

    let windows_before: Vec<_> = cx.update(|app| WindowRegistry::all_states(app));
    cx.update(|app| {
        crate::keymap::move_session_to_new_window(app, &state_b, &moved_session_id);
    });
    settle(cx, 400).await;

    if cx.update(|app| WindowRegistry::state_for_window(app, id_b).is_none()) {
        failures.push(
            "(4) window B closed after the new-window move — it still owns its own original Main \
             session and must NOT have been emptied"
                .to_string(),
        );
    }
    let torn = cx.update(|app| {
        WindowRegistry::all_states(app)
            .into_iter()
            .find(|s| !windows_before.iter().any(|b| b == s))
    });
    match torn {
        Some(state_c) => match adopted_pane_handle_by_entity(cx, &state_c, handle_id) {
            Some(h) if grid_text(cx, &h).contains(MARKER) => {}
            Some(_) => failures.push(
                "(4) the new window hosts the moved pane but its grid lost the marker".to_string(),
            ),
            None => failures.push(
                "(4) the new window's pane handle vanished between the two reads".to_string(),
            ),
        },
        None => failures.push(
            "(4) no newly-opened window hosts the moved pane — the new-window leg respawned \
             instead of moving it"
                .to_string(),
        ),
    }

    teardown(cx, fixture, build_report(failures)).await
}

// ===========================================================================
// helpers
// ===========================================================================

/// `(session_id, term_window_id, pane_id)` of the window's focused pane.
fn active_pane_keys(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
) -> Option<(String, String, String)> {
    cx.update(|app| {
        let ws = state.read(app);
        let session_id = ws.workspace.active_session_id()?.to_string();
        let session = ws.workspace.session_for(&session_id)?;
        let term_window_id = session.active_window_id.clone()?;
        let pane_id = session
            .windows
            .iter()
            .find(|w| w.id == term_window_id)?
            .effective_pane_id();
        Some((session_id, term_window_id, pane_id))
    })
}

/// Every live pane handle a window currently holds.
fn pane_handles(state: &Entity<WindowState>, app: &gpui::App) -> Vec<Entity<TerminalSessionHandle>> {
    let ws = state.read(app);
    ws.ptys
        .live_pane_keys()
        .into_iter()
        .filter_map(|(s, w, p)| ws.ptys.pane_handle(&s, &w, &p))
        .collect()
}

/// The pane handle in `state` whose entity id is `handle_id` — the identity
/// check a re-keyed session id can't answer (the moved session takes a fresh
/// id on the way in, so the caller can't look it up by its old one).
fn adopted_pane_handle_by_entity(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    handle_id: gpui::EntityId,
) -> Option<Entity<TerminalSessionHandle>> {
    cx.update(|app| {
        pane_handles(state, app)
            .into_iter()
            .find(|h| h.entity_id() == handle_id)
    })
}

fn grid_text(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>) -> String {
    handle.update(cx, |h, _| h.session().grid_lines().join("\n"))
}

fn write_line(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>, line: &str) {
    let payload = format!("{line}\n");
    handle.update(cx, |h, _| {
        let _ = h.session().write_input(payload.as_bytes());
    });
}

async fn poll_grid_contains(
    cx: &mut AsyncApp,
    handle: &Entity<TerminalSessionHandle>,
    needle: &str,
) -> bool {
    for _ in 0..GRID_POLLS {
        settle(cx, POLL_MS).await;
        if grid_text(cx, handle).contains(needle) {
            return true;
        }
    }
    false
}

// ===========================================================================
// teardown + report
// ===========================================================================

/// Close every window this scenario opened (while the scoped close observer is
/// still alive, so their ptys are reaped), then restore the env vars so the
/// next scenario runs exactly as it did before this one.
async fn teardown(cx: &mut AsyncApp, fixture: Fixture, report: CadenceReport) -> CadenceReport {
    let handles = cx.update(|app| {
        WindowRegistry::all_states(app)
            .into_iter()
            .filter_map(|s| s.read(app).window_handle())
            .collect::<Vec<_>>()
    });
    for handle in handles {
        let _ = handle.update(cx, |_root, window, _app| window.remove_window());
    }
    settle(cx, 400).await;
    // SAFETY: teardown, single-threaded.
    unsafe {
        match &fixture.prev_zdotdir {
            Some(z) => std::env::set_var("ZDOTDIR", z),
            None => std::env::remove_var("ZDOTDIR"),
        }
    }
    let _ = std::fs::remove_dir_all(&fixture.base);
    report
}

fn build_report(failures: Vec<String>) -> CadenceReport {
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "move-session OK: Move to Window carried a live pty (no respawn, scrollback \
                     intact) from A into B and closed A once it emptied (N5), and Move to New \
                     Window carried the same pty into a fresh window C while B — which still owns \
                     its own Main — stayed open"
                .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} move-session assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
