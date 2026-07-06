//! `session-lifecycle` self-test scenario — the R13 slice-3 session-manager gate.
//!
//! Where the ported unit suites (in `session_manager::tests`) pin the pure model
//! routing case-by-case, this scenario drives the **real** per-window
//! [`SessionManager`](crate::session_manager::SessionManager) on a real
//! [`WindowState`] with **real ptys** end to end
//! — the action-seam rewiring (What-to-build #3), the focus/spawn plumbing (#4),
//! and the live `cx.subscribe` that feeds
//! [`route_terminal_event`](crate::session_manager::SessionManager::route_terminal_event)
//! from a pane's session entity. It covers the six lifecycle behaviors Milestone 2
//! rests on:
//!
//! 1. **Immediate explicit-add spawn** — the sidebar `Terminals +` / ⌘T
//!    create-and-spawn path (a new terminal tab + its `Terminal 1`) and the strip
//!    `+` path ([`add_terminal_to_active_tab`]) both spawn their pty **synchronously**
//!    (Swift `addPane` semantics — an explicit add is never deferred).
//! 2. **Claude spawns now; companion spawns on focus** — the project `+` seam
//!    builds the `[Claude, Terminal 1]` shape through the ONE shared constructor,
//!    which (R15) spawns the Claude pane **immediately** (claude-kind panes never
//!    lazy-spawn; the pane execs the hermetic `NICE_CLAUDE_OVERRIDE` stub) while
//!    the companion terminal stays **deferred**; selecting the companion runs
//!    [`ensure_active_pane_spawned`] and its pty forks on that first focus.
//! 3. **Clean-exit neighbor refocus** — exiting the active terminal's shell with a
//!    clean `exit 0` (not held) removes the pane and re-points the active pane to
//!    the slot neighbor via the live `Exited { held: false }` subscription.
//! 4. **Last-pane dissolve + Terminals-order fallback** — exiting the tab's last
//!    pane dissolves the tab and the active-tab selection falls back to the
//!    first navigable tab (the pinned `Terminals` group's Main tab).
//! 5. **Held detour** — a `sh -c 'echo FINAL; exit 3'` pane exits non-zero, so the
//!    `Exited { held: true }` subscription flips it dead-but-mounted
//!    (`is_alive == false`, still in the strip) rather than removing it.
//! 6. **Orphan sweep** — [`WindowState::teardown`](crate::window_state::WindowState::teardown)
//!    drops every session, tearing each child process group down (SIGHUP→SIGKILL),
//!    so no zsh survives the window (asserted externally by `ps` per the R3
//!    teardown contract — Validation §5).
//!
//! ## Why no view is mounted
//!
//! Every assertion here is **model + session state** (`has_pane`, `is_alive`, the
//! active tab / pane, tab presence), which
//! [`route_terminal_event`](crate::session_manager::SessionManager::route_terminal_event)
//! resolves in full. So the scenario drives the manager headless — no
//! [`TerminalView`](nice_term_view::TerminalView) — over a minimal RAF window that
//! only keeps the compositor alive for the harness. The two GPUI-only side effects
//! the pane-exit resolution carries (the deferred-companion spawn on refocus, and
//! the every-project-empty **terminus** that closes the window / quits) are
//! composed by the live window root where a `Window` is in scope; this scenario is
//! constructed so the terminus stays [`None`](crate::session_manager) (Main and the
//! project both survive every dissolve) and a refocus never lands on an unspawned
//! companion, so routing the model through the entity subscription is sufficient
//! and correct for what it asserts. Self-reported gate ([`Gate::SelfReported`](nice_harness::selftest)):
//! the criterion is these state transitions, not frame cadence.
//!
//! [`add_terminal_to_active_tab`]: crate::session_manager::SessionManager::add_terminal_to_active_tab
//! [`ensure_active_pane_spawned`]: crate::session_manager::SessionManager::ensure_active_pane_spawned

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use anyhow::Result;
use gpui::{div, prelude::*, AnyWindowHandle, AsyncApp, Context, Entity, IntoElement, Render, Window};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_term_core::SpawnSpec;
use nice_term_view::{TerminalEvent, TerminalSessionHandle};

use crate::session_manager::ClaudeTabPlacement;
use crate::window_state::WindowState;

// -- fixed geometry / timing -------------------------------------------------

const ROWS: u16 = 24;
const COLS: u16 = 80;

/// A short app-level launch-overlay grace so the arm → promote deadline path
/// exercises quickly (the pane's first output clears it well before this fires in
/// practice — the arm is wired for completeness, not asserted here).
const LAUNCH_GRACE: Duration = Duration::from_millis(300);

/// Poll cap for a shell to print its readiness marker (`READY`) — a ZDOTDIR-blanked
/// login shell exec'ing the fixture, on the real pty clock.
const READY_POLLS: usize = 60;
/// Poll cap for a routed model mutation (pane removal / tab dissolve / held flip)
/// to land after its pty event — the drain task + entity subscription hop.
const ROUTE_POLLS: usize = 50;
/// Interval between polls (real wall-clock; the pty child runs on OS threads the
/// simulated dispatcher does not drive).
const POLL_MS: u64 = 100;

/// The scenario's non-Terminals project — the `project +` seam target.
const PROJECT_ID: &str = "sl-proj";
/// A marker a fixture shell prints once it is reading input, so the driver polls
/// the grid for readiness rather than sleeping (ZDOTDIR-blanked shells).
const READY_MARKER: &str = "READY";

/// Minimal RAF-animated root: keeps the window compositing (and the frame clock
/// stamped for the harness's per-scenario reset) while the headless driver runs.
struct SessionLifecycleRoot;

impl Render for SessionLifecycleRoot {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        nice_harness::frame::stamp();
        window.request_animation_frame();
        div().size_full().bg(gpui::rgb(0x11141b))
    }
}

/// Open the `session-lifecycle` scenario window and spawn its headless driver
/// (self-reported gate). The per-window [`WindowState`] is minted up front so the
/// driver can drive its real [`SessionManager`](crate::session_manager::SessionManager)
/// directly.
pub fn open_session_lifecycle_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let base =
        std::env::temp_dir().join(format!("nice-rs-session-lifecycle-{}", std::process::id()));
    std::fs::create_dir_all(&base)?;
    let cwd = base.to_string_lossy().to_string();

    // R15: the project-+ leg now spawns a real Claude pane. Point `claude` at a
    // hermetic stub via NICE_CLAUDE_OVERRIDE (the spawn path re-reads it) so the
    // regression suite never launches the machine's real claude — the async probe
    // never runs under `run_selftest`, but the override is belt-and-suspenders and
    // matches the shipped seam. The stub just idles (this leg asserts the pane
    // SPAWNED, not its output).
    install_stub_claude_override(&base)?;

    // The per-window state (the real R12 composition root, filled with the R13
    // SessionManager). Created before the window so the async driver owns a handle.
    // `AsyncApp`'s `update` / entity `update` return the value directly (they panic
    // if the app is gone), so no `?` — matching the landed `multiwindow` scenario.
    let state = cx.update(|app| app.new(|_cx| WindowState::new(cwd.clone())));
    state.update(cx, |s, _cx| s.session.set_launch_overlay_grace(LAUNCH_GRACE));

    let window = cx.open_window(crate::app::window_options(), |_window, cx| {
        cx.new(|_cx| SessionLifecycleRoot)
    })?;
    let window: AnyWindowHandle = window.into();

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_session_lifecycle(acx, state, cwd).await;
        eprintln!("[selftest] scenario 'session-lifecycle': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(window)
}

/// Write an executable stub `claude` under `base/bin` and point
/// `NICE_CLAUDE_OVERRIDE` at it (process-wide — the spawn path reads the process
/// env). The stub idles so the spawned pane stays live; it NEVER the machine's
/// real claude (hermeticity). Overwrite-always so a re-run / prior scenario's
/// override is replaced by this one.
fn install_stub_claude_override(base: &std::path::Path) -> Result<()> {
    let bin = base.join("bin");
    std::fs::create_dir_all(&bin)?;
    let stub = bin.join("claude");
    std::fs::write(&stub, "#!/bin/sh\nexec sleep 2147483647\n")?;
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))?;
    // SAFETY: single-threaded scenario setup, before any pane forks; matches the
    // existing `std::env::set_var` seams (nice-harness selftest, spawn.rs).
    unsafe { std::env::set_var("NICE_CLAUDE_OVERRIDE", &stub) };
    Ok(())
}

// ---------------------------------------------------------------------------
// Live action-seam wiring — the create-and-spawn / activate / spawn+subscribe
// compositions the R10/R11 action seams route through, over the real
// SessionManager (What-to-build #3 / #4).
// ---------------------------------------------------------------------------

/// Spawn a pane's pty via the manager, wire its app-level launch overlay, and
/// subscribe the window state to its session entity so the pane's OSC / exit
/// events route into the model. This is the reusable core every create/add path
/// composes (the "create-and-spawn" half of the rewiring); it is race-free because
/// the spawn + subscribe run in one synchronous update, so the drain task cannot
/// deliver an event before the subscription exists.
fn spawn_and_subscribe(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab_id: &str,
    pane_id: &str,
    spec: SpawnSpec,
) {
    let tab_id = tab_id.to_string();
    let pane_id = pane_id.to_string();
    let _ = state.update(cx, |s, cx| {
        if s.session.spawn_pane(&tab_id, &pane_id, spec, cx).is_err() {
            return;
        }
        // App-level "Launching…" overlay: record Pending, and (grace > 0) arm the
        // App-Nap-safe promotion deadline. The subscription clears it on first
        // output / exit / held, so a fast pane's overlay never appears.
        if s.session.register_pane_launch(&pane_id, "terminal") {
            let deadline = crate::platform::launch_deadline();
            let pane = pane_id.clone();
            cx.spawn(async move |this, acx| {
                (deadline)(LAUNCH_GRACE).await;
                let _ = this.update(acx, |s2, _cx| s2.session.promote_pane_launch(&pane));
            })
            .detach();
        }
        // The live `cx.subscribe` that feeds `route_terminal_event` from the pane's
        // session entity (the slice-3 subscription seam). The RoutedExit's
        // GPUI-only side effects are composed by the live window root (see the
        // module docs); here the routed model mutation is the whole observable.
        if let Some(handle) = s.session.pane_handle(&tab_id, &pane_id) {
            let (t, p) = (tab_id.clone(), pane_id.clone());
            cx.subscribe(&handle, move |s2, _handle, event: &TerminalEvent, cx2| {
                let _ =
                    s2.session
                        .route_terminal_event(&mut s2.model, &mut s2.selection, &t, &p, event);
                cx2.notify();
            })
            .detach();
        }
    });
}

/// The `Terminals +` / ⌘T create-and-spawn path: build the terminal tab's model
/// shape through the R10 sidebar seam, then spawn its seeded `Terminal 1` pane
/// **immediately**. Returns `(tab_id, pane_id)`.
fn create_and_spawn_terminal_tab(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    cwd: &str,
) -> Option<(String, String)> {
    let ids = state.update(cx, |s, _cx| {
        let tab_id = s.sidebar_actions.create_terminal_tab(&mut s.model)?;
        let pane_id = s.model.tab_for(&tab_id)?.panes.first()?.id.clone();
        Some((tab_id, pane_id))
    })?;
    spawn_and_subscribe(cx, state, &ids.0, &ids.1, clean_exit_spec(cwd));
    Some(ids)
}

/// The strip `+` path: append a terminal pane to the active tab via the manager's
/// [`add_terminal_to_active_tab`](crate::session_manager::SessionManager::add_terminal_to_active_tab)
/// and spawn it **immediately** (explicit adds are never deferred). Returns the
/// new pane id.
fn strip_add_and_spawn(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    cwd: &str,
) -> Option<String> {
    let pane_id =
        state.update(cx, |s, _cx| s.session.add_terminal_to_active_tab(&mut s.model))?;
    let tab_id = active_tab(cx, state)?;
    spawn_and_subscribe(cx, state, &tab_id, &pane_id, clean_exit_spec(cwd));
    Some(pane_id)
}

/// The `project +` seam: build the `[Claude, Terminal 1]` shape in `PROJECT_ID`
/// through the ONE shared constructor [`SessionManager::create_claude_tab`](crate::session_manager::SessionManager::create_claude_tab),
/// which (R15) **spawns the Claude pane immediately** (claude-kind panes never
/// lazy-spawn) while the companion terminal stays deferred. The Claude pane execs
/// the `NICE_CLAUDE_OVERRIDE` stub (hermetic — never the machine's real claude).
/// Returns `(tab_id, claude_pane_id, companion_pane_id)`.
fn project_new_claude_tab(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
) -> Option<(String, String, String)> {
    state.update(cx, |s, cx| {
        let model = &mut s.model;
        let session = &mut s.session;
        let tab_id = session.create_claude_tab(
            model,
            ClaudeTabPlacement::Project {
                project_id: PROJECT_ID.to_string(),
            },
            &[],
            None,
            cx,
        )?;
        let tab = s.model.tab_for(&tab_id)?;
        let claude = tab.panes.first()?.id.clone();
        let companion = tab.panes.get(1)?.id.clone();
        Some((tab_id, claude, companion))
    })
}

/// Activate a pane the model half + the deferred-spawn half of Swift's
/// `setActivePane`: [`set_active_pane`](crate::session_manager::SessionManager::set_active_pane)
/// (ack a waiting viewed pane) + [`ensure_active_pane_spawned`](crate::session_manager::SessionManager::ensure_active_pane_spawned)
/// (a deferred terminal companion forks on first focus). The key-focus half
/// (`focus_active_pane`) is the Window-level effect the live window root composes;
/// no view is mounted here.
fn activate_pane(cx: &mut AsyncApp, state: &Entity<WindowState>, tab_id: &str, pane_id: &str) {
    let _ = state.update(cx, |s, cx| {
        s.session.set_active_pane(&mut s.model, tab_id, pane_id);
        s.session.ensure_active_pane_spawned(&s.model, tab_id, cx);
    });
}

/// A pane that prints `READY`, blocks on one line of input, then exits **cleanly**
/// (status 0 → not held). The driver polls the grid for `READY`, then writes a
/// line to trigger the clean exit — the "exit the active pane's shell with `exit`"
/// step, made deterministic.
fn clean_exit_spec(cwd: &str) -> SpawnSpec {
    SpawnSpec::command(
        format!("sh -c 'echo {READY_MARKER}; read _line; exit 0'"),
        cwd.to_string(),
    )
    .with_env(vec![("ZDOTDIR".to_string(), cwd.to_string())])
    .with_size(ROWS, COLS)
}

/// A pane that prints then exits **non-zero** (status 3 → the R3 held
/// classification): the held-detour fixture.
fn held_spec(cwd: &str) -> SpawnSpec {
    SpawnSpec::command("sh -c 'echo FINAL; exit 3'".to_string(), cwd.to_string())
        .with_env(vec![("ZDOTDIR".to_string(), cwd.to_string())])
        .with_size(ROWS, COLS)
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

async fn run_session_lifecycle(
    cx: &mut AsyncApp,
    state: Entity<WindowState>,
    cwd: String,
) -> CadenceReport {
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 300).await;

    let mut failures: Vec<String> = Vec::new();

    // A non-Terminals project for the project-+ seam.
    let _ = state.update(cx, |s, _cx| {
        s.model.ensure_project(PROJECT_ID, "Proj", &cwd);
    });

    // === 1. create-and-spawn a terminal tab: the pane spawns immediately ======
    let Some((t_tab, t_p1)) = create_and_spawn_terminal_tab(cx, &state, &cwd) else {
        return CadenceReport::error(
            "session-lifecycle: create_terminal_tab (the Terminals-+ seam) produced no tab",
        );
    };
    if !has_pane(cx, &state, &t_tab, &t_p1) {
        failures.push(
            "create-and-spawn: the new terminal tab's Terminal 1 did not spawn its pty \
             synchronously (explicit adds are never deferred)"
                .into(),
        );
    }

    // === 2. strip-+ explicit add: the pane spawns immediately =================
    let t_p2 = strip_add_and_spawn(cx, &state, &cwd);
    match &t_p2 {
        Some(p) if has_pane(cx, &state, &t_tab, p) => {}
        Some(_) => failures.push(
            "strip-+: the explicitly-added terminal pane did not spawn its pty synchronously".into(),
        ),
        None => failures.push("strip-+: add_terminal_to_active_tab returned no pane".into()),
    }

    // === 3. project-+ claude tab: Claude pane spawns now; companion on focus ====
    // R15 rewrote this leg: the Claude pane now spawns immediately through the ONE
    // shared constructor (claude-kind panes never lazy-spawn), while the companion
    // terminal stays deferred until first focus.
    match project_new_claude_tab(cx, &state) {
        Some((c_tab, c_claude, c_companion)) => {
            if !has_pane(cx, &state, &c_tab, &c_claude) {
                failures.push(
                    "project-+: the Claude pane did not spawn its pty up front (claude-kind \
                     panes never lazy-spawn)"
                        .into(),
                );
            }
            if has_pane(cx, &state, &c_tab, &c_companion) {
                failures.push(
                    "project-+: the companion terminal spawned a pty up front (it must stay \
                     deferred until first focus)"
                        .into(),
                );
            }
            activate_pane(cx, &state, &c_tab, &c_companion);
            if !has_pane(cx, &state, &c_tab, &c_companion) {
                failures.push(
                    "deferred spawn: focusing the companion terminal did not fork its pty".into(),
                );
            }
        }
        None => failures.push(
            "project-+: create_claude_tab (the project-+ seam) produced no tab".into(),
        ),
    }

    // === 4. clean-exit neighbor refocus (within the terminal tab) =============
    // Re-select the terminal tab so its later last-pane dissolve triggers the
    // active-tab fallback (the dissolve re-selects only when the dissolved tab was
    // the active one).
    select_tab(cx, &state, &t_tab);
    if let Some(p2) = t_p2.clone() {
        set_active_pane(cx, &state, &t_tab, &p2);
        if !exit_pane_cleanly(cx, &state, &t_tab, &p2).await {
            failures.push("clean-exit: the active terminal pane never became ready to exit".into());
        } else if !poll_pane_gone(cx, &state, &t_tab, &p2).await {
            failures.push(
                "clean-exit: the cleanly-exited pane was never removed (the Exited{held:false} \
                 subscription did not route)"
                    .into(),
            );
        } else {
            let active = active_pane_of(cx, &state, &t_tab);
            if active.as_deref() != Some(t_p1.as_str()) {
                failures.push(format!(
                    "clean-exit: neighbor refocus did not land on the surviving Terminal 1 \
                     (active pane = {active:?})"
                ));
            }
        }
    }

    // === 5. last-pane dissolve + Terminals-order fallback =====================
    if !exit_pane_cleanly(cx, &state, &t_tab, &t_p1).await {
        failures.push("dissolve: the tab's last pane never became ready to exit".into());
    } else if !poll_tab_gone(cx, &state, &t_tab).await {
        failures.push(
            "dissolve: the tab was not removed after its last pane exited (the dissolve cascade \
             did not run)"
                .into(),
        );
    } else {
        let active = active_tab(cx, &state);
        if active.as_deref() != Some(nice_model::TabModel::MAIN_TERMINAL_TAB_ID) {
            failures.push(format!(
                "dissolve: the active-tab fallback did not select the Terminals-order tab (the \
                 pinned Main tab); active tab = {active:?}"
            ));
        }
    }

    // === 6. held detour: a non-zero exit stays mounted, is_alive == false ======
    match add_and_spawn_held_pane(cx, &state, &cwd) {
        Some((h_tab, h_pane)) => {
            if !poll_pane_held(cx, &state, &h_tab, &h_pane).await {
                failures.push(
                    "held: the non-zero-exit pane did not enter the held state (expected still \
                     mounted with is_alive == false)"
                        .into(),
                );
            }
        }
        None => failures.push("held: could not add the held-detour pane".into()),
    }

    // === teardown: drop every session so no zsh outlives the window ===========
    let _ = state.update(cx, |s, _cx| s.teardown());
    settle(cx, 150).await;

    build_report(failures)
}

/// Add a held-detour pane to the Main tab via the manager's `add_pane` and spawn
/// its non-zero-exit fixture. Returns `(tab_id, pane_id)`.
fn add_and_spawn_held_pane(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    cwd: &str,
) -> Option<(String, String)> {
    let tab_id = nice_model::TabModel::MAIN_TERMINAL_TAB_ID.to_string();
    let pane_id = state.update(cx, |s, _cx| s.session.add_pane(&mut s.model, &tab_id, None))?;
    spawn_and_subscribe(cx, state, &tab_id, &pane_id, held_spec(cwd));
    Some((tab_id, pane_id))
}

/// Poll the pane's grid for its `READY` marker, then write a line to trigger its
/// clean `exit 0`. Returns whether readiness was observed (a `false` means the
/// fixture never came up — a real failure, not a flaky timeout).
async fn exit_pane_cleanly(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab_id: &str,
    pane_id: &str,
) -> bool {
    let Some(handle) = pane_handle(cx, state, tab_id, pane_id) else {
        return false;
    };
    let mut ready = false;
    for _ in 0..READY_POLLS {
        settle(cx, POLL_MS).await;
        let grid = handle.update(cx, |h, _cx| h.session().grid_lines().join("\n"));
        if grid.contains(READY_MARKER) {
            ready = true;
            break;
        }
    }
    if !ready {
        return false;
    }
    // Complete the pending `read`, so the shell exits cleanly (status 0 → not held).
    let _ = handle.update(cx, |h, _cx| {
        let _ = h.session().write_input(b"x\n");
    });
    true
}

// -- small state / model readers --------------------------------------------

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

fn pane_handle(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab_id: &str,
    pane_id: &str,
) -> Option<Entity<TerminalSessionHandle>> {
    state.update(cx, |s, _cx| s.session.pane_handle(tab_id, pane_id))
}

fn has_pane(cx: &mut AsyncApp, state: &Entity<WindowState>, tab_id: &str, pane_id: &str) -> bool {
    state.update(cx, |s, _cx| s.session.has_pane(tab_id, pane_id))
}

fn active_tab(cx: &mut AsyncApp, state: &Entity<WindowState>) -> Option<String> {
    state.update(cx, |s, _cx| s.model.active_tab_id().map(str::to_string))
}

fn active_pane_of(cx: &mut AsyncApp, state: &Entity<WindowState>, tab_id: &str) -> Option<String> {
    state.update(cx, |s, _cx| {
        s.model
            .tab_for(tab_id)
            .and_then(|t| t.active_pane_id.clone())
    })
}

fn select_tab(cx: &mut AsyncApp, state: &Entity<WindowState>, tab_id: &str) {
    let _ = state.update(cx, |s, _cx| s.model.select_tab(tab_id));
}

fn set_active_pane(cx: &mut AsyncApp, state: &Entity<WindowState>, tab_id: &str, pane_id: &str) {
    let _ = state.update(cx, |s, _cx| {
        s.session.set_active_pane(&mut s.model, tab_id, pane_id)
    });
}

/// Poll until the pane is gone from its tab's `panes` array (routed removal), or
/// the poll cap elapses.
async fn poll_pane_gone(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab_id: &str,
    pane_id: &str,
) -> bool {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        let gone = state.update(cx, |s, _cx| {
            s.model
                .tab_for(tab_id)
                .map(|t| !t.panes.iter().any(|p| p.id == pane_id))
                // Tab itself gone also counts as the pane being gone.
                .unwrap_or(true)
        });
        if gone {
            return true;
        }
    }
    false
}

/// Poll until the tab is gone from the model (routed dissolve), or the cap elapses.
async fn poll_tab_gone(cx: &mut AsyncApp, state: &Entity<WindowState>, tab_id: &str) -> bool {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        let gone = state.update(cx, |s, _cx| s.model.tab_for(tab_id).is_none());
        if gone {
            return true;
        }
    }
    false
}

/// Poll until the pane is held: still mounted in its tab, but `is_alive == false`.
async fn poll_pane_held(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    tab_id: &str,
    pane_id: &str,
) -> bool {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        let held = state.update(cx, |s, _cx| {
            s.model
                .tab_for(tab_id)
                .and_then(|t| t.panes.iter().find(|p| p.id == pane_id))
                .map(|p| !p.is_alive)
                .unwrap_or(false)
        });
        if held {
            return true;
        }
    }
    false
}

fn build_report(failures: Vec<String>) -> CadenceReport {
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "session lifecycle OK: Terminals-+/strip-+ create-and-spawn forked ptys \
                     synchronously; the project-+ [Claude, Terminal 1] tab spawned its Claude pane \
                     immediately (hermetic stub) while the companion stayed deferred and forked on \
                     first focus; a clean exit refocused the slot neighbor; the last-pane exit \
                     dissolved the tab and fell back to the Terminals-order Main tab; a non-zero \
                     exit held its pane (is_alive == false, still mounted); teardown dropped every \
                     session."
                .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} session-lifecycle assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
