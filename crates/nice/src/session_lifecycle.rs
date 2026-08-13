//! `session-lifecycle` self-test scenario — the R13 slice-3 session-manager gate.
//!
//! Where the ported unit suites (in `pty_manager::tests`) pin the pure model
//! routing case-by-case, this scenario drives the **real** per-window
//! [`PtyManager`](crate::pty_manager::PtyManager) on a real
//! [`WindowState`] with **real ptys** end to end
//! — the action-seam rewiring (What-to-build #3), the focus/spawn plumbing (#4),
//! and the live `cx.subscribe` that feeds
//! [`route_terminal_event`](crate::pty_manager::PtyManager::route_terminal_event)
//! from a window's session entity. It covers the six lifecycle behaviors Milestone 2
//! rests on:
//!
//! 1. **Immediate explicit-add spawn** — the sidebar `Terminals +` / ⌘T
//!    create-and-spawn path (a new terminal session + its `Terminal 1`) and the strip
//!    `+` path ([`add_terminal_to_active_session`]) both spawn their pty **synchronously**
//!    (Swift `addPane` semantics — an explicit add is never deferred).
//! 2. **Claude spawns now; companion spawns on focus** — the project `+` seam
//!    builds the `[Claude, Terminal 1]` shape through the ONE shared constructor,
//!    which (R15) spawns the Claude window **immediately** (claude-kind windows never
//!    lazy-spawn; the window execs the hermetic `NICE_CLAUDE_OVERRIDE` stub) while
//!    the companion terminal stays **deferred**; selecting the companion runs
//!    [`ensure_active_window_spawned`] and its pty forks on that first focus.
//! 3. **Clean-exit neighbor refocus** — exiting the active terminal's shell with a
//!    clean `exit 0` (not held) removes the window and re-points the active window to
//!    the slot neighbor via the live `Exited { held: false }` subscription.
//! 4. **Last-window dissolve + Terminals-order fallback** — exiting the session's last
//!    window dissolves the session and the active-session selection falls back to the
//!    first navigable session (the pinned `Terminals` group's Main session).
//! 5. **Held detour** — a `sh -c 'echo FINAL; exit 3'` window exits non-zero, so the
//!    `Exited { held: true }` subscription flips it dead-but-mounted
//!    (`is_alive == false`, still in the strip) rather than removing it.
//! 6. **Orphan sweep** — [`WindowState::teardown`](crate::window_state::WindowState::teardown)
//!    drops every session, tearing each child process group down (SIGHUP→SIGKILL),
//!    so no zsh survives the window (asserted externally by `ps` per the R3
//!    teardown contract — Validation §5).
//! 7. **Quit freeze** — with the [`AppQuitting`](crate::lifecycle::AppQuitting)
//!    latch set, an `Exited { held: false }` delivered through the SHIPPED
//!    subscription ([`WindowState::subscribe_spawned_windows`]) is dropped and the
//!    session survives in the model — the lost-session quit race: `quit_cascade`'s
//!    teardown kills classify `held: false` (intentional), and routing one
//!    between the step-2 flush and the `on_app_quit` re-flush dissolved the session
//!    from the re-snapshotted model. A control phase proves the same exit
//!    dissolves when the latch is absent (delivery works, so survival is real).
//!
//! [`WindowState::subscribe_spawned_windows`]: crate::window_state::WindowState::subscribe_spawned_windows
//!
//! ## Why no view is mounted
//!
//! Every assertion here is **model + session state** (`has_window`, `is_alive`, the
//! active session / window, session presence), which
//! [`route_terminal_event`](crate::pty_manager::PtyManager::route_terminal_event)
//! resolves in full. So the scenario drives the manager headless — no
//! [`TerminalView`](nice_term_view::TerminalView) — over a minimal RAF window that
//! only keeps the compositor alive for the harness. The two GPUI-only side effects
//! the window-exit resolution carries (the deferred-companion spawn on refocus, and
//! the every-project-empty **terminus** that closes the window / quits) are
//! composed by the live window root where a `Window` is in scope; this scenario is
//! constructed so the terminus stays [`None`](crate::pty_manager) (Main and the
//! project both survive every dissolve) and a refocus never lands on an unspawned
//! companion, so routing the model through the entity subscription is sufficient
//! and correct for what it asserts. Self-reported gate ([`Gate::SelfReported`](nice_harness::selftest)):
//! the criterion is these state transitions, not frame cadence.
//!
//! [`add_terminal_to_active_session`]: crate::pty_manager::PtyManager::add_terminal_to_active_session
//! [`ensure_active_window_spawned`]: crate::pty_manager::PtyManager::ensure_active_window_spawned

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use anyhow::Result;
use gpui::{div, prelude::*, AnyWindowHandle, AsyncApp, Context, Entity, IntoElement, Render, Window};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_term_core::SpawnSpec;
use nice_term_view::{TerminalEvent, TerminalSessionHandle};

use crate::pty_manager::{ClaudeSessionPlacement, ClaudeSessionSpec};
use crate::window_state::WindowState;

// -- fixed geometry / timing -------------------------------------------------

const ROWS: u16 = 24;
const COLS: u16 = 80;

/// A short app-level launch-overlay grace so the arm → promote deadline path
/// exercises quickly (the window's first output clears it well before this fires in
/// practice — the arm is wired for completeness, not asserted here).
const LAUNCH_GRACE: Duration = Duration::from_millis(300);

/// Poll cap for a shell to print its readiness marker (`READY`) — a ZDOTDIR-blanked
/// login shell exec'ing the fixture, on the real pty clock.
const READY_POLLS: usize = 60;
/// Poll cap for a routed model mutation (window removal / session dissolve / held flip)
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
/// driver can drive its real [`PtyManager`](crate::pty_manager::PtyManager)
/// directly.
pub fn open_session_lifecycle_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let base =
        std::env::temp_dir().join(format!("nice-session-lifecycle-{}", std::process::id()));
    std::fs::create_dir_all(&base)?;
    let cwd = base.to_string_lossy().to_string();

    // R15: the project-+ leg now spawns a real Claude window. Point `claude` at a
    // hermetic stub via NICE_CLAUDE_OVERRIDE (the spawn path re-reads it) so the
    // regression suite never launches the machine's real claude — the async probe
    // never runs under `run_selftest`, but the override is belt-and-suspenders and
    // matches the shipped seam. The stub just idles (this leg asserts the window
    // SPAWNED, not its output).
    install_stub_claude_override(&base)?;

    // The per-window state (the real R12 composition root, filled with the R13
    // PtyManager). Created before the window so the async driver owns a handle.
    // `AsyncApp`'s `update` / entity `update` return the value directly (they panic
    // if the app is gone), so no `?` — matching the landed `multiwindow` scenario.
    let state = cx.update(|app| app.new(|_cx| WindowState::new(cwd.clone())));
    state.update(cx, |s, _cx| s.ptys.set_launch_overlay_grace(LAUNCH_GRACE));

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
/// env). The stub idles so the spawned window stays live; it NEVER the machine's
/// real claude (hermeticity). Overwrite-always so a re-run / prior scenario's
/// override is replaced by this one.
fn install_stub_claude_override(base: &std::path::Path) -> Result<()> {
    let bin = base.join("bin");
    std::fs::create_dir_all(&bin)?;
    let stub = bin.join("claude");
    std::fs::write(&stub, "#!/bin/sh\nexec sleep 2147483647\n")?;
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))?;
    // SAFETY: single-threaded scenario setup, before any window forks; matches the
    // existing `std::env::set_var` seams (nice-harness selftest, spawn.rs).
    unsafe { std::env::set_var("NICE_CLAUDE_OVERRIDE", &stub) };
    Ok(())
}

// ---------------------------------------------------------------------------
// Live action-seam wiring — the create-and-spawn / activate / spawn+subscribe
// compositions the R10/R11 action seams route through, over the real
// PtyManager (What-to-build #3 / #4).
// ---------------------------------------------------------------------------

/// Spawn a window's pty via the manager, wire its app-level launch overlay, and
/// subscribe the window state to its session entity so the window's OSC / exit
/// events route into the model. This is the reusable core every create/add path
/// composes (the "create-and-spawn" half of the rewiring); it is race-free because
/// the spawn + subscribe run in one synchronous update, so the drain task cannot
/// deliver an event before the subscription exists.
fn spawn_and_subscribe(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    session_id: &str,
    term_window_id: &str,
    spec: SpawnSpec,
) {
    let session_id = session_id.to_string();
    let term_window_id = term_window_id.to_string();
    let _ = state.update(cx, |s, cx| {
        if s.ptys.spawn_window(&session_id, &term_window_id, spec, cx).is_err() {
            return;
        }
        // App-level "Launching…" overlay: record Pending, and (grace > 0) arm the
        // App-Nap-safe promotion deadline. The subscription clears it on first
        // output / exit / held, so a fast window's overlay never appears.
        if s.ptys.register_window_launch(&term_window_id, "terminal") {
            let deadline = crate::platform::launch_deadline();
            let term_window = term_window_id.clone();
            cx.spawn(async move |this, acx| {
                (deadline)(LAUNCH_GRACE).await;
                let _ = this.update(acx, |s2, _cx| s2.ptys.promote_window_launch(&term_window));
            })
            .detach();
        }
        // The live `cx.subscribe` that feeds `route_terminal_event` from the window's
        // session entity (the slice-3 subscription seam). The `RoutedEvent`'s
        // GPUI-only side effects are composed by the live window root (see the
        // module docs); here the routed model mutation is the whole observable,
        // so the value is DISCARDED on purpose.
        //
        // This scenario is NOT where a routed side effect gets wired — the
        // shipped one is `WindowState::subscribe_spawned_windows`. Actuating
        // something here (app-typed prefill, say) would pass this scenario while
        // the real app never did it.
        if let Some(handle) = s.ptys.term_window_handle(&session_id, &term_window_id) {
            let (t, p) = (session_id.clone(), term_window_id.clone());
            cx.subscribe(&handle, move |s2, _handle, event: &TerminalEvent, cx2| {
                let _ =
                    s2.ptys
                        .route_terminal_event(&mut s2.workspace, &mut s2.selection, &t, &p, event);
                cx2.notify();
            })
            .detach();
        }
    });
}

/// The `Terminals +` / ⌘T create-and-spawn path: build the terminal session's model
/// shape through the R10 sidebar seam, then spawn its seeded `Terminal 1` window
/// **immediately**. Returns `(session_id, term_window_id)`.
fn create_and_spawn_terminal_session(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    cwd: &str,
) -> Option<(String, String)> {
    let ids = state.update(cx, |s, _cx| {
        let session_id = s.sidebar_actions.create_terminal_session(&mut s.workspace)?;
        let term_window_id = s.workspace.session_for(&session_id)?.windows.first()?.id.clone();
        Some((session_id, term_window_id))
    })?;
    spawn_and_subscribe(cx, state, &ids.0, &ids.1, clean_exit_spec(cwd));
    Some(ids)
}

/// The strip `+` path: append a terminal window to the active session via the manager's
/// [`add_terminal_to_active_session`](crate::pty_manager::PtyManager::add_terminal_to_active_session)
/// and spawn it **immediately** (explicit adds are never deferred). Returns the
/// new window id.
fn strip_add_and_spawn(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    cwd: &str,
) -> Option<String> {
    let term_window_id =
        state.update(cx, |s, _cx| s.ptys.add_terminal_to_active_session(&mut s.workspace))?;
    let session_id = active_session(cx, state)?;
    spawn_and_subscribe(cx, state, &session_id, &term_window_id, clean_exit_spec(cwd));
    Some(term_window_id)
}

/// The `project +` seam: build the `[Claude, Terminal 1]` shape in `PROJECT_ID`
/// through the ONE shared constructor [`PtyManager::create_claude_session`](crate::pty_manager::PtyManager::create_claude_session),
/// which (R15) **spawns the Claude window immediately** (claude-kind windows never
/// lazy-spawn) while the companion terminal stays deferred. The Claude window execs
/// the `NICE_CLAUDE_OVERRIDE` stub (hermetic — never the machine's real claude).
/// Returns `(session_id, claude_window_id, companion_window_id)`.
fn project_new_claude_session(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
) -> Option<(String, String, String)> {
    state.update(cx, |s, cx| {
        let workspace = &mut s.workspace;
        let ptys = &mut s.ptys;
        let session_id = ptys.create_claude_session(
            workspace,
            ClaudeSessionPlacement::Project {
                project_id: PROJECT_ID.to_string(),
            },
            &[],
            ClaudeSessionSpec::mint(),
            None,
            cx,
        )?;
        let session = s.workspace.session_for(&session_id)?;
        let claude = session.windows.first()?.id.clone();
        let companion = session.windows.get(1)?.id.clone();
        Some((session_id, claude, companion))
    })
}

/// Activate a window the model half + the deferred-spawn half of Swift's
/// `setActivePane`: [`set_active_window`](crate::pty_manager::PtyManager::set_active_window)
/// (ack a waiting viewed window) + [`ensure_active_window_spawned`](crate::pty_manager::PtyManager::ensure_active_window_spawned)
/// (a deferred terminal companion forks on first focus). The key-focus half
/// (`focus_active_window`) is the Window-level effect the live window root composes;
/// no view is mounted here.
fn activate_term_window(cx: &mut AsyncApp, state: &Entity<WindowState>, session_id: &str, term_window_id: &str) {
    let _ = state.update(cx, |s, cx| {
        s.ptys.set_active_window(&mut s.workspace, session_id, term_window_id);
        // No Claude sessions in this scenario, so no `--settings` provider is needed.
        s.ptys.ensure_active_window_spawned(&s.workspace, session_id, None, cx);
    });
}

/// A window that prints `READY`, blocks on one line of input, then exits **cleanly**
/// (status 0 → not held). The driver polls the grid for `READY`, then writes a
/// line to trigger the clean exit — the "exit the active window's shell with `exit`"
/// step, made deterministic.
fn clean_exit_spec(cwd: &str) -> SpawnSpec {
    SpawnSpec::command(
        format!("sh -c 'echo {READY_MARKER}; read _line; exit 0'"),
        cwd.to_string(),
    )
    .with_env(vec![("ZDOTDIR".to_string(), cwd.to_string())])
    .with_size(ROWS, COLS)
}

/// Leg-7 phase-B assert window: how long to let a latched-out `Exited` NOT
/// mutate the model before declaring survival. There is no edge to poll for on
/// a dropped event, so a fixed settle is the assert window; the control phase
/// proves delivery normally lands well inside it (ROUTE cadence, ~100–300 ms).
const FREEZE_SETTLE_MS: u64 = 1_500;

/// A window that prints then exits **non-zero** (status 3 → the R3 held
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
        s.workspace.ensure_project(PROJECT_ID, "Proj", &cwd);
    });

    // === 1. create-and-spawn a terminal session: the window spawns immediately ======
    let Some((t_session, t_p1)) = create_and_spawn_terminal_session(cx, &state, &cwd) else {
        return CadenceReport::error(
            "session-lifecycle: create_terminal_session (the Terminals-+ seam) produced no session",
        );
    };
    if !has_window(cx, &state, &t_session, &t_p1) {
        failures.push(
            "create-and-spawn: the new terminal session's Terminal 1 did not spawn its pty \
             synchronously (explicit adds are never deferred)"
                .into(),
        );
    }

    // === 2. strip-+ explicit add: the window spawns immediately =================
    let t_p2 = strip_add_and_spawn(cx, &state, &cwd);
    match &t_p2 {
        Some(p) if has_window(cx, &state, &t_session, p) => {}
        Some(_) => failures.push(
            "strip-+: the explicitly-added terminal window did not spawn its pty synchronously".into(),
        ),
        None => failures.push("strip-+: add_terminal_to_active_session returned no window".into()),
    }

    // === 3. project-+ claude session: Claude window spawns now; companion on focus ====
    // R15 rewrote this leg: the Claude window now spawns immediately through the ONE
    // shared constructor (claude-kind windows never lazy-spawn), while the companion
    // terminal stays deferred until first focus.
    match project_new_claude_session(cx, &state) {
        Some((c_session, c_claude, c_companion)) => {
            if !has_window(cx, &state, &c_session, &c_claude) {
                failures.push(
                    "project-+: the Claude window did not spawn its pty up front (claude-kind \
                     windows never lazy-spawn)"
                        .into(),
                );
            }
            if has_window(cx, &state, &c_session, &c_companion) {
                failures.push(
                    "project-+: the companion terminal spawned a pty up front (it must stay \
                     deferred until first focus)"
                        .into(),
                );
            }
            activate_term_window(cx, &state, &c_session, &c_companion);
            if !has_window(cx, &state, &c_session, &c_companion) {
                failures.push(
                    "deferred spawn: focusing the companion terminal did not fork its pty".into(),
                );
            }
        }
        None => failures.push(
            "project-+: create_claude_session (the project-+ seam) produced no session".into(),
        ),
    }

    // === 4. clean-exit neighbor refocus (within the terminal session) =============
    // Re-select the terminal session so its later last-window dissolve triggers the
    // active-session fallback (the dissolve re-selects only when the dissolved session was
    // the active one).
    select_session(cx, &state, &t_session);
    if let Some(p2) = t_p2.clone() {
        set_active_window(cx, &state, &t_session, &p2);
        if !exit_window_cleanly(cx, &state, &t_session, &p2).await {
            failures.push("clean-exit: the active terminal window never became ready to exit".into());
        } else if !poll_window_gone(cx, &state, &t_session, &p2).await {
            failures.push(
                "clean-exit: the cleanly-exited window was never removed (the Exited{held:false} \
                 subscription did not route)"
                    .into(),
            );
        } else {
            let active = active_window_of(cx, &state, &t_session);
            if active.as_deref() != Some(t_p1.as_str()) {
                failures.push(format!(
                    "clean-exit: neighbor refocus did not land on the surviving Terminal 1 \
                     (active window = {active:?})"
                ));
            }
        }
    }

    // === 5. last-window dissolve + Terminals-order fallback =====================
    if !exit_window_cleanly(cx, &state, &t_session, &t_p1).await {
        failures.push("dissolve: the session's last window never became ready to exit".into());
    } else if !poll_session_gone(cx, &state, &t_session).await {
        failures.push(
            "dissolve: the session was not removed after its last window exited (the dissolve cascade \
             did not run)"
                .into(),
        );
    } else {
        let active = active_session(cx, &state);
        if active.as_deref() != Some(nice_model::WorkspaceModel::MAIN_TERMINAL_SESSION_ID) {
            failures.push(format!(
                "dissolve: the active-session fallback did not select the Terminals-order session (the \
                 pinned Main session); active session = {active:?}"
            ));
        }
    }

    // === 6. held detour: a non-zero exit stays mounted, is_alive == false ======
    match add_and_spawn_held_window(cx, &state, &cwd) {
        Some((h_session, h_window)) => {
            if !poll_window_held(cx, &state, &h_session, &h_window).await {
                failures.push(
                    "held: the non-zero-exit window did not enter the held state (expected still \
                     mounted with is_alive == false)"
                        .into(),
                );
            }
        }
        None => failures.push("held: could not add the held-detour window".into()),
    }

    // === 7. quit freeze: exits landing after quit begins must not dissolve =====
    // Phase A (control): without the latch, a clean exit routed through the
    // SHIPPED subscription (`subscribe_spawned_windows` — not this scenario's
    // local closure, which deliberately mirrors it without the gate) dissolves
    // the session, proving event delivery works in this harness so phase B's
    // survival assert is meaningful.
    match create_and_spawn_shipped(cx, &state, &cwd) {
        Some((qa_session, qa_window)) => {
            if !exit_window_cleanly(cx, &state, &qa_session, &qa_window).await {
                failures
                    .push("quit-freeze control: the window never became ready to exit".into());
            } else if !poll_session_gone(cx, &state, &qa_session).await {
                failures.push(
                    "quit-freeze control: a clean exit through the shipped subscription \
                     (subscribe_spawned_windows) did not dissolve the session"
                        .into(),
                );
            }
        }
        None => failures.push("quit-freeze control: could not create the control session".into()),
    }
    // Phase B: the same shape with `AppQuitting` set before the exit — the
    // shipped callback must drop the event and the session must survive (the
    // lost-session quit race: a teardown-kill `Exited { held: false }` routed
    // between quit_cascade's flush and the on_app_quit re-flush dissolved the
    // session from the re-snapshotted model).
    match create_and_spawn_shipped(cx, &state, &cwd) {
        Some((qb_session, qb_window)) => {
            let _ = cx.update(|app| app.set_global(crate::lifecycle::AppQuitting));
            if !exit_window_cleanly(cx, &state, &qb_session, &qb_window).await {
                failures.push("quit-freeze: the window never became ready to exit".into());
            } else {
                settle(cx, FREEZE_SETTLE_MS).await;
                let (session_alive, window_alive) = state.update(cx, |s, _cx| {
                    let window_alive = s
                        .workspace
                        .session_for(&qb_session)
                        .map(|t| t.windows.iter().any(|w| w.id == qb_window))
                        .unwrap_or(false);
                    (s.workspace.session_for(&qb_session).is_some(), window_alive)
                });
                if !session_alive || !window_alive {
                    failures.push(format!(
                        "quit-freeze: an Exited{{held:false}} delivered after AppQuitting \
                         mutated the model (session present: {session_alive}, window present: \
                         {window_alive}) — the lost-session quit race regressed"
                    ));
                }
            }
            // Un-latch before teardown so this in-process suite's later
            // scenarios see the normal (not-quitting) close routing.
            let _ = cx.update(|app| {
                let _ = app.remove_global::<crate::lifecycle::AppQuitting>();
            });
        }
        None => failures.push("quit-freeze: could not create the frozen-phase session".into()),
    }

    // === teardown: drop every session so no zsh outlives the window ===========
    let _ = state.update(cx, |s, _cx| s.teardown());
    settle(cx, 150).await;

    build_report(failures)
}

/// Leg-7 fixture: create a terminal session through the sidebar seam but wire its
/// window through the SHIPPED subscription sweep
/// ([`WindowState::subscribe_spawned_windows`](crate::window_state::WindowState::subscribe_spawned_windows))
/// instead of this scenario's local closure — the quit-freeze gate under test
/// lives in the shipped callback. Spawn + sweep run in one synchronous update,
/// so no event can outrun the subscription. Returns `(session_id, term_window_id)`.
fn create_and_spawn_shipped(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    cwd: &str,
) -> Option<(String, String)> {
    let spec = clean_exit_spec(cwd);
    state.update(cx, |s, cx| {
        let session_id = s.sidebar_actions.create_terminal_session(&mut s.workspace)?;
        let term_window_id = s.workspace.session_for(&session_id)?.windows.first()?.id.clone();
        if s.ptys.spawn_window(&session_id, &term_window_id, spec, cx).is_err() {
            return None;
        }
        s.subscribe_spawned_windows(cx);
        Some((session_id, term_window_id))
    })
}

/// Add a held-detour window to the Main session via the manager's `add_window` and spawn
/// its non-zero-exit fixture. Returns `(session_id, term_window_id)`.
fn add_and_spawn_held_window(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    cwd: &str,
) -> Option<(String, String)> {
    let session_id = nice_model::WorkspaceModel::MAIN_TERMINAL_SESSION_ID.to_string();
    let term_window_id = state.update(cx, |s, _cx| s.ptys.add_window(&mut s.workspace, &session_id, None))?;
    spawn_and_subscribe(cx, state, &session_id, &term_window_id, held_spec(cwd));
    Some((session_id, term_window_id))
}

/// Poll the window's grid for its `READY` marker, then write a line to trigger its
/// clean `exit 0`. Returns whether readiness was observed (a `false` means the
/// fixture never came up — a real failure, not a flaky timeout).
async fn exit_window_cleanly(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    session_id: &str,
    term_window_id: &str,
) -> bool {
    let Some(handle) = term_window_handle(cx, state, session_id, term_window_id) else {
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

fn term_window_handle(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    session_id: &str,
    term_window_id: &str,
) -> Option<Entity<TerminalSessionHandle>> {
    state.update(cx, |s, _cx| s.ptys.term_window_handle(session_id, term_window_id))
}

fn has_window(cx: &mut AsyncApp, state: &Entity<WindowState>, session_id: &str, term_window_id: &str) -> bool {
    state.update(cx, |s, _cx| s.ptys.has_window(session_id, term_window_id))
}

fn active_session(cx: &mut AsyncApp, state: &Entity<WindowState>) -> Option<String> {
    state.update(cx, |s, _cx| s.workspace.active_session_id().map(str::to_string))
}

fn active_window_of(cx: &mut AsyncApp, state: &Entity<WindowState>, session_id: &str) -> Option<String> {
    state.update(cx, |s, _cx| {
        s.workspace
            .session_for(session_id)
            .and_then(|t| t.active_window_id.clone())
    })
}

fn select_session(cx: &mut AsyncApp, state: &Entity<WindowState>, session_id: &str) {
    let _ = state.update(cx, |s, _cx| s.workspace.select_session(session_id));
}

fn set_active_window(cx: &mut AsyncApp, state: &Entity<WindowState>, session_id: &str, term_window_id: &str) {
    let _ = state.update(cx, |s, _cx| {
        s.ptys.set_active_window(&mut s.workspace, session_id, term_window_id)
    });
}

/// Poll until the window is gone from its session's `term_windows` array (routed removal), or
/// the poll cap elapses.
async fn poll_window_gone(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    session_id: &str,
    term_window_id: &str,
) -> bool {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        let gone = state.update(cx, |s, _cx| {
            s.workspace
                .session_for(session_id)
                .map(|t| !t.windows.iter().any(|w| w.id == term_window_id))
                // Session itself gone also counts as the window being gone.
                .unwrap_or(true)
        });
        if gone {
            return true;
        }
    }
    false
}

/// Poll until the session is gone from the model (routed dissolve), or the cap elapses.
async fn poll_session_gone(cx: &mut AsyncApp, state: &Entity<WindowState>, session_id: &str) -> bool {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        let gone = state.update(cx, |s, _cx| s.workspace.session_for(session_id).is_none());
        if gone {
            return true;
        }
    }
    false
}

/// Poll until the window is held: still mounted in its session, but `is_alive == false`.
async fn poll_window_held(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    session_id: &str,
    term_window_id: &str,
) -> bool {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        let held = state.update(cx, |s, _cx| {
            s.workspace
                .session_for(session_id)
                .and_then(|t| t.windows.iter().find(|w| w.id == term_window_id))
                .map(|w| !w.is_alive)
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
                     synchronously; the project-+ [Claude, Terminal 1] session spawned its Claude window \
                     immediately (hermetic stub) while the companion stayed deferred and forked on \
                     first focus; a clean exit refocused the slot neighbor; the last-window exit \
                     dissolved the session and fell back to the Terminals-order Main session; a non-zero \
                     exit held its window (is_alive == false, still mounted); once AppQuitting was \
                     set, a clean exit through the shipped subscription no longer mutated the \
                     model (the lost-session quit freeze); teardown dropped every session."
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
