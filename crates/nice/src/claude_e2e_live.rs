//! `claude-e2e` self-test scenario — the R17 Milestone-3 shipped-surface gate.
//!
//! Where `claude-lifecycle` (R15/R16) drives the Claude flow over the SHIPPED
//! window but pokes it through **raw sockets**, this scenario drives the same
//! shipped composition the way a **user** does: it types `claude\n` into a real
//! pty whose login shell carries the R14 `claude()` shadow, and asserts the
//! Milestone-3 clauses end-to-end — "typing `claude` anywhere opens/promotes
//! sessions; statuses pulse; `/clear`/`/branch` tracked" — WITH R17's theme sync ON.
//!
//! Legs (all fail-loud, grid/model-poll bounded — never sleep-and-hope):
//!
//! * **(a) typed newtab + minted uuid + theme sync ON** — `claude\n` into the real
//!   Main window's pty: the shadow handshakes over `NICE_SOCKET`, the Terminals-group
//!   Main session forces `newtab`, and a fresh Claude session appears with its stub SPAWNED
//!   (`is_claude_running` from creation), a valid v4 session UUID, and — because the
//!   process theme-sync gate is ON — the window's `--settings` provider resolved to
//!   the sandbox pointer file (leg (e) proves the file). (The Main-session newtab spawn
//!   runs under `NICE_CLAUDE_OVERRIDE`, so `build_claude_exec_command` suppresses
//!   the Nice flags — the wrapper-spliced `--settings`/`--session-id` argv is
//!   asserted concretely in leg (c), where the zsh wrapper, not the override spawn,
//!   owns the argv.)
//! * **(b) status pulse Thinking → Waiting** — the new Claude window's braille-prefixed
//!   stub OSC title drives the shipped session's sidebar-dot status to Thinking, then
//!   (after a line of input unblocks its `read`) its ✳ OSC → Waiting, over the
//!   SHIPPED window's subscription (the dot-input read on the shipped entity).
//! * **(c) typed in-place promotion through the real zsh wrapper** — a live terminal
//!   window in a non-Terminals project, typing `claude\n`: the reply is
//!   `inplace <uuid> <ptr>` (theme sync ON), the wrapper `exec`s the stub, and the
//!   grid shows its `--settings <ptr> --session-id <uuid>` argv verbatim, while the
//!   model flips (kind → Claude, `is_claude_running` true).
//! * **(d) rotation on the shipped sidebar** — a raw-socket `session_update` with
//!   `source:"resume"` + a new id materializes the branch parent at ROOT
//!   (`parent_session_id == None`) with the originating session re-parented + indented
//!   beneath it (root promotion), then a `source:"clear"` update rotates the id in
//!   place with NO new session (`/clear` tracking).
//! * **(e) theme + pointer files present** — the theme file exists at the `nice`
//!   slug carrying `"_niceManaged": true`, and the `--settings` pointer file has the
//!   exact `{"theme":"custom:nice"}` bytes.
//! * **(f) gate-OFF parity** — with the theme-sync gate flipped OFF (a scenario
//!   seam) and the window provider re-filled, repeating leg (c)'s typed promotion in
//!   a fresh terminal session yields a settings-less reply: the wrapper `exec`s the stub
//!   with `--session-id <uuid>` and NO `--settings` (byte-identical to the
//!   pre-theming protocol).
//!
//! ## Hermeticity
//!
//! Fully sandboxed (tranche-3 rule): a fake `$HOME` with a marker `.zshrc`, a stub
//! `claude` on `PATH` **and** exported as `NICE_CLAUDE_OVERRIDE`, a `ZDOTDIR`
//! written by the R14 stub writer against a temp dir, and the theme/pointer files
//! written against sandbox paths — never the machine's real `claude`, real
//! `~/.claude` / `~/.nice`, or Application Support. It installs no `WindowRegistry`
//! close observer (its `build_window_root` only `register`s), so it is registered
//! BEFORE `multiwindow` (which owns the quit-when-empty terminus and must be last);
//! at teardown it resets the scenario `ShellRuntime` so `multiwindow`'s windows
//! fork socket-only exactly as before.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use gpui::{AnyWindowHandle, AsyncApp, Entity, WindowHandle};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_model::{TermWindow, TermWindowKind, Session, WorkspaceModel, SessionStatus};
use nice_term_core::SpawnSpec;
use nice_term_view::{TerminalSessionHandle, TerminalTheme};
use nice_theme::{AccentPreset, ColorScheme};

use crate::app_shell::AppShellView;
use crate::window_registry::WindowRegistry;
use crate::window_state::WindowState;

// -- timing ------------------------------------------------------------------

/// Poll cap for a real login shell to source the synthetic chain + fixture rc and
/// print its READY marker, on the real pty clock.
const READY_POLLS: usize = 90;
/// Poll cap for a routed model mutation / socket reply to land after its typed
/// handshake or raw drive (the accept thread → mpsc → waker-woken drain hop).
const ROUTE_POLLS: usize = 80;
/// Interval between polls (real wall-clock; pty children + socket threads run on OS
/// threads the simulated dispatcher does not drive).
const POLL_MS: u64 = 100;

/// The marker the sandbox `~/.zshrc` echoes once sourced — the chain-back proof and
/// the shell-readiness signal the driver polls for before typing `claude`. The stub
/// `claude` additionally echoes its argv behind `STUB_CLAUDE_ARGV:` so the
/// wrapper-spliced `--settings`/`--session-id` flags are observable in the grid
/// (legs (c)/(f)).
const READY_MARKER: &str = "NICERS__CLAUDE__E2E__READY";

const ROWS: u16 = 24;
const COLS: u16 = 118;

// -- fixture -----------------------------------------------------------------

/// The sandboxed fixture: a fake `$HOME` + marker `.zshrc`, a stub `claude` on a
/// private `PATH` dir, a stub-written `ZDOTDIR`, and two promotion work dirs.
struct Fixture {
    /// Canonicalized (symlinks resolved) so `$HOME`-derived paths compare equal.
    home: PathBuf,
    stub: PathBuf,
    zdotdir: PathBuf,
    /// Non-Terminals invocation dir for leg (c)'s typed promotion.
    work_c: PathBuf,
    /// Non-Terminals invocation dir for leg (f)'s gate-OFF promotion.
    work_f: PathBuf,
}

impl Fixture {
    fn build() -> Result<Self> {
        let base = std::env::temp_dir().join(format!("nice-claude-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&base).context("create fixture base")?;
        // Canonicalize so /var/folders (a symlink to /private/var/folders on macOS)
        // resolves — a spawned shell's $HOME-derived pointer path must compare equal
        // to the window provider computed from the same canonical $HOME.
        let base = base.canonicalize().context("canonicalize fixture base")?;

        let home = base.join("home");
        let stub_bin = base.join("bin");
        let zdotdir = base.join("zdotdir");
        let work_c = base.join("work-c");
        let work_f = base.join("work-f");
        for d in [&home, &stub_bin, &zdotdir, &work_c, &work_f] {
            std::fs::create_dir_all(d).context("create fixture dir")?;
        }

        // The sandbox ~/.zshrc: put the stub `claude` + system `nc` on PATH (so the
        // wrapper's `command claude` resolves the stub and its `nc -U` reaches the
        // socket) and echo the readiness marker. The synthetic .zshrc stub sources
        // THIS after restoring ZDOTDIR, so `claude()` / the OSC 7 hook layer on top.
        std::fs::write(
            home.join(".zshrc"),
            format!(
                "export PATH=\"{}:/usr/bin:/bin:/usr/sbin:/sbin\"\nprint -r -- {READY_MARKER}\n",
                stub_bin.display()
            ),
        )
        .context("write sandbox .zshrc")?;

        // The stub `claude`: echo its argv (so the wrapper-spliced flags are visible
        // in the grid), then BURST a braille-prefixed ("thinking") OSC title a few
        // times (so at least one lands after the shipped subscription is
        // established — the socket-spawn → notify → render → subscribe race), block
        // on one line of input, emit a ✳-prefixed ("waiting") OSC title, then idle.
        // NEVER the machine's real claude (hermeticity). `\u{2801}` (⠁) is inside the
        // braille spinner range 0x2800..=0x28FF; `\u{2733}` (✳) is the sparkle.
        let stub = stub_bin.join("claude");
        std::fs::write(
            &stub,
            "#!/bin/sh\n\
             printf '%s %s\\n' 'STUB_CLAUDE_ARGV:' \"$*\"\n\
             n=0\n\
             while [ \"$n\" -lt 15 ]; do\n\
             \x20 printf '\\033]2;\u{2801} build-thing\\007'\n\
             \x20 n=$((n + 1))\n\
             \x20 sleep 0.1\n\
             done\n\
             IFS= read -r _line\n\
             printf '\\033]2;\u{2733} needs-input\\007'\n\
             while IFS= read -r _l; do : ; done\n",
        )
        .context("write stub claude")?;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .context("chmod stub claude")?;

        // The ZDOTDIR: write the FROZEN R14 stubs against this temp dir (never the
        // real Application Support location).
        crate::shell::zsh::write_stubs(&zdotdir).context("write ZDOTDIR stubs")?;

        Ok(Fixture {
            home,
            stub,
            zdotdir,
            work_c,
            work_f,
        })
    }

    fn home_str(&self) -> String {
        self.home.to_string_lossy().into_owned()
    }
    fn zdotdir_str(&self) -> String {
        self.zdotdir.to_string_lossy().into_owned()
    }
    fn stub_str(&self) -> String {
        self.stub.to_string_lossy().into_owned()
    }
    /// The theme file the writer lands at the `nice` slug (leg (e)).
    fn theme_file(&self) -> PathBuf {
        crate::claude_theme_sync::themes_dir(&self.home, None)
            .join(format!("{}.json", crate::claude_theme_sync::SLUG))
    }
    /// The `--settings` pointer path (the provider's ensure-on-read target and the
    /// value spliced into the in-place reply).
    fn pointer(&self) -> PathBuf {
        crate::claude_theme_sync::theme_settings_path(&self.home)
    }
}

// -- scenario wiring ---------------------------------------------------------

/// Open the `claude-e2e` window through the SHIPPED builder and spawn its driver
/// (self-reported gate). Before opening, it (1) writes the theme + pointer files
/// against sandbox paths (the bootstrap-write mirror `run_selftest` skips), (2)
/// installs the theme-sync gate ON so the SHIPPED provider fill lights up, and (3)
/// installs the scenario `ShellRuntime` so the Main window forks WITH the
/// `claude()` shadow. `HOME` is sandboxed only around `open_managed_window` (the
/// Main window inherits it at fork), then restored.
pub fn open_claude_e2e_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let fixture = Fixture::build()?;
    let home = fixture.home_str();
    let zdotdir = fixture.zdotdir_str();
    let stub = fixture.stub_str();
    let themes_dir = crate::claude_theme_sync::themes_dir(&fixture.home, None);
    let pointer = fixture.pointer();

    // Point the spawn path's `resolve_claude_binary` at the stub (process env,
    // re-read every spawn) — overwrite-always so a prior scenario's override is
    // replaced by this argv-echoing stub.
    // SAFETY: single-threaded scenario setup, before any window forks; matches the
    // existing `std::env::set_var` seams (spawn.rs, claude-lifecycle).
    unsafe { std::env::set_var("NICE_CLAUDE_OVERRIDE", &stub) };

    let whandle: WindowHandle<AppShellView> = cx.update(|app| {
        // The shipped builder reads the process-global font settings (via the window
        // host); `install_shortcuts` seeds it (idempotent across scenarios).
        crate::keymap::install_shortcuts(app);

        // Mirror the bootstrap write against sandbox paths (leg (e)): the theme file
        // at the `nice` slug + the pointer file. `run_selftest` never runs the
        // real `app::run` bootstrap write, so the scenario does it hermetically.
        crate::claude_theme_sync::write_with(
            &TerminalTheme::nice_default_dark(),
            ColorScheme::Dark,
            AccentPreset::Terracotta.color(),
            &themes_dir,
            &pointer,
        );

        // Theme sync ON, through the SHIPPED provider fill: `open_managed_window`
        // reads this gate and sets each window's `--settings` provider from it.
        crate::app::set_claude_theme_sync_gate(app, true);
        // Give the SHIPPED Main window the synthetic ZDOTDIR shadow (points at the
        // fixture stubs, never the real Application Support location).
        crate::app::set_scenario_shell_inject_config(app, Some(zdotdir.clone()), None);

        let prev = std::env::var("HOME").ok();
        // SAFETY: single-threaded setup; restored immediately after the (synchronous)
        // Main-window spawn inside `open_managed_window`.
        unsafe { std::env::set_var("HOME", &home) };
        let opened = crate::app::open_managed_window(app);
        match prev {
            Some(h) => unsafe { std::env::set_var("HOME", h) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        opened
    })?;
    let any: AnyWindowHandle = whandle.into();

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_claude_e2e(acx, whandle, fixture).await;
        eprintln!("[selftest] scenario 'claude-e2e': {}", report.detail);
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

// -- driver ------------------------------------------------------------------

async fn run_claude_e2e(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fixture: Fixture,
) -> CadenceReport {
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 500).await;

    let id = AnyWindowHandle::from(whandle).window_id();
    let Some(state) = cx.update(|app| WindowRegistry::state_for_window(app, id)) else {
        return CadenceReport::error(
            "claude-e2e: the shipped builder did not register the window's WindowState",
        );
    };
    let Some(socket_path) = state.update(cx, |s, _cx| s.control_socket_path()) else {
        return CadenceReport::error(
            "claude-e2e: the shipped window armed no control socket (no path to drive session_update)",
        );
    };

    // Theme sync ON must have filled the window's `--settings` provider through the
    // SHIPPED path (the pointer resolved against the sandbox $HOME).
    let want_pointer = fixture.pointer().to_string_lossy().into_owned();
    let mut failures: Vec<String> = Vec::new();
    match state.update(cx, |s, _cx| s.claude_settings_path_provider()) {
        Some(p) if p == want_pointer => {}
        other => failures.push(format!(
            "(a) theme sync ON: the shipped window provider must be the sandbox pointer {want_pointer:?}, got {other:?}"
        )),
    }

    // The real Main window (Terminals-group Main session), spawned with the shadow.
    let main_session = WorkspaceModel::MAIN_TERMINAL_SESSION_ID.to_string();
    let Some(main_window) = state.update(cx, |s, _cx| {
        s.workspace.session_for(&main_session).and_then(|t| t.windows.first()).map(|p| p.id.clone())
    }) else {
        return CadenceReport::error("claude-e2e: the shipped window has no Main terminal window");
    };
    let Some(main_handle) = term_window_handle(cx, &state, &main_session, &main_window) else {
        return CadenceReport::error("claude-e2e: the Main window never spawned its pty");
    };
    if !poll_grid_contains(cx, &main_handle, READY_MARKER).await {
        return CadenceReport::error(
            "claude-e2e: the Main window's login shell never printed READY — the synthetic ZDOTDIR \
             chain did not source the sandbox ~/.zshrc (no claude() shadow)",
        );
    }

    // === (a) typed `claude` in the Main window ⇒ newtab + spawned running Claude =====
    let sessions_before = all_session_ids(cx, &state);
    write_line(cx, &main_handle, b"claude\n");
    let claude_session = match poll_new_session(cx, &state, &sessions_before).await {
        Some(t) => t,
        None => {
            return CadenceReport::error(
                "claude-e2e (a): typing `claude` in the Main window produced no new Claude session (the \
                 shadow did not handshake / the Terminals session did not force newtab)",
            )
        }
    };
    let (claude_window, _companion) = match session_window_ids(cx, &state, &claude_session) {
        Some(p) => p,
        None => {
            return CadenceReport::error(
                "claude-e2e (a): the new Claude session has no [Claude, Terminal 1] windows",
            )
        }
    };
    if !window_is_claude_running(cx, &state, &claude_session, &claude_window) {
        failures.push("(a) the new Claude window is not is_claude_running from creation".into());
    }
    if !poll_has_window(cx, &state, &claude_session, &claude_window).await {
        failures.push("(a) the new Claude window never spawned its pty (the stub did not run)".into());
    }
    match session_claude_id(cx, &state, &claude_session) {
        Some(sid) if is_v4_uuid(&sid) => {}
        Some(sid) => failures.push(format!("(a) the session's Claude session id {sid:?} is not a valid v4 UUID")),
        None => failures.push("(a) the new Claude session carries no minted session id".into()),
    }

    // === (b) the shipped sidebar-dot status pulses Thinking → Waiting =============
    if !poll_session_status(cx, &state, &claude_session, SessionStatus::Thinking).await {
        failures.push(
            "(b) the Claude window's braille OSC title did not drive the shipped session status to \
             Thinking (the subscription did not route the title)"
                .into(),
        );
    } else {
        write_window_line(cx, &state, &claude_session, &claude_window, b"go\n");
        if !poll_session_status(cx, &state, &claude_session, SessionStatus::Waiting).await {
            failures.push(
                "(b) the Claude window's ✳ OSC title did not drive the shipped session status to Waiting"
                    .into(),
            );
        }
    }

    // === (c) typed in-place promotion through the real zsh wrapper ================
    let want_ptr = want_pointer.clone();
    match run_typed_promotion(cx, &state, &fixture, "e2e-c", &fixture.work_c).await {
        Ok((session_c, window_c, handle_c)) => {
            if !poll_window_promoted(cx, &state, &session_c, &window_c).await {
                failures.push(
                    "(c) promotion: the terminal window did not flip to a running Claude window in the model"
                        .into(),
                );
            }
            // The wrapper `exec`ed the stub with the theme pointer + minted uuid.
            let uuid = session_claude_id(cx, &state, &session_c).unwrap_or_default();
            let want_argv = format!("--settings {want_ptr} --session-id {uuid}");
            if !poll_grid_contains_wrapped(cx, &handle_c, &want_argv).await {
                let grid = grid_of(cx, &handle_c);
                failures.push(format!(
                    "(c) promotion: the wrapper did not exec the stub with {want_argv:?} \
                     (theme sync ON). grid tail: {:?}",
                    grid_tail(&grid)
                ));
            }
            // -- (d) rotation on the shipped sidebar (reuses the leg-(c) session) -------
            drive_rotation(cx, &state, &socket_path, &fixture, &session_c, &window_c, &mut failures).await;
        }
        Err(e) => failures.push(format!("(c) promotion setup failed: {e}")),
    }

    // === (e) theme + pointer files present =======================================
    match std::fs::read(fixture.theme_file()) {
        Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(v) if v.get("_niceManaged").and_then(|m| m.as_bool()) == Some(true) => {}
            Ok(_) => failures.push("(e) the theme file at the nice slug lacks _niceManaged:true".into()),
            Err(_) => failures.push("(e) the theme file at the nice slug is not valid JSON".into()),
        },
        Err(_) => failures.push(format!(
            "(e) no theme file at the nice slug ({:?})",
            fixture.theme_file()
        )),
    }
    match std::fs::read(fixture.pointer()) {
        Ok(bytes) if bytes == b"{\n  \"theme\": \"custom:nice\"\n}" => {}
        Ok(bytes) => failures.push(format!(
            "(e) the pointer file bytes are not the exact contract: {:?}",
            String::from_utf8_lossy(&bytes)
        )),
        Err(_) => failures.push(format!("(e) no pointer file at {:?}", fixture.pointer())),
    }

    // === (f) gate OFF ⇒ settings-less promotion parity ===========================
    cx.update(|app| crate::app::set_claude_theme_sync_gate(app, false));
    // Re-fill the window's provider from the flipped gate (the OFF value is None) —
    // mirrors `open_managed_window`'s provider fill (R21/R23 re-source this live).
    state.update(cx, |s, _cx| {
        s.set_claude_settings_path(crate::claude_theme_sync::settings_path_for_gate(false))
    });
    match run_typed_promotion(cx, &state, &fixture, "e2e-f", &fixture.work_f).await {
        Ok((session_f, window_f, handle_f)) => {
            if !poll_window_promoted(cx, &state, &session_f, &window_f).await {
                failures.push("(f) gate-off: the terminal window did not flip to a running Claude window".into());
            }
            let uuid = session_claude_id(cx, &state, &session_f).unwrap_or_default();
            let want_argv = format!("--session-id {uuid}");
            if !poll_grid_contains_wrapped(cx, &handle_f, &want_argv).await {
                failures.push(format!(
                    "(f) gate-off: the wrapper did not exec the stub with the settings-less {want_argv:?}"
                ));
            }
            // The settings-less form: NO `--settings` reached the stub's argv.
            if strip_ws(&grid_of(cx, &handle_f)).contains("--settings") {
                failures.push(
                    "(f) gate-off: `--settings` leaked into the promotion argv — the gate-OFF reply \
                     must be byte-identical to the pre-theming settings-less form"
                        .into(),
                );
            }
        }
        Err(e) => failures.push(format!("(f) gate-off promotion setup failed: {e}")),
    }

    // === teardown: reap every session, reset the scenario shell-inject config ======
    let _ = state.update(cx, |s, _cx| s.teardown());
    // Reset so the later `multiwindow` scenario's windows fork socket-only (no stale
    // fixture ZDOTDIR pointing at a temp dir).
    cx.update(|app| crate::app::set_scenario_shell_inject_config(app, None, None));
    settle(cx, 200).await;

    build_report(failures)
}

fn build_report(failures: Vec<String>) -> CadenceReport {
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "claude-e2e OK (shipped surface, theme sync ON): typing `claude` in the real \
                     Main window handshaked over the socket and opened a running Claude session with a \
                     minted v4 uuid whose stub OSC titles pulsed the shipped sidebar-dot status \
                     Thinking → Waiting; a typed in-place promotion through the real zsh wrapper \
                     exec'd the stub with `--settings <ptr> --session-id <uuid>` and flipped the \
                     model; a `session_update` /branch rotation materialized a root branch parent \
                     with the originating session re-parented beneath it, and a /clear rotated the id \
                     in place with no new session; the theme file carries _niceManaged:true and the \
                     pointer file holds the exact bytes; and with the gate flipped OFF a fresh \
                     typed promotion exec'd the stub settings-less (`--session-id <uuid>`, no \
                     `--settings`)."
                .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} claude-e2e assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}

// -- typed-promotion leg (shared by (c) and (f)) -----------------------------

/// Seed a live terminal window in a fresh non-Terminals project, wait for its shell
/// to source the shadow, type `claude\n`, and return `(session, window, handle)`. The
/// window spawns through the manager's live spawn path so the window env injection
/// (`NICE_SOCKET` / `ZDOTDIR` / this window's `NICE_TAB_ID` / `NICE_PANE_ID`) applies;
/// `HOME` is spec-provided (spec-wins) so the chain sources the sandbox `~/.zshrc`
/// (PATH → the stub). The caller polls the promotion + the wrapper-spliced argv.
async fn run_typed_promotion(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    fixture: &Fixture,
    key: &str,
    cwd: &Path,
) -> Result<(String, String, Entity<TerminalSessionHandle>)> {
    let proj = format!("{key}-proj");
    let session = format!("{key}-tab");
    let term_window = format!("{key}-pane");
    let cwd = cwd.to_string_lossy().into_owned();

    // Seed the non-Terminals project + session + terminal window, and select it so the
    // shipped shell renders it (Swift's promotable-target shape).
    let seeded = state.update(cx, |s, _cx| {
        s.workspace.ensure_project(&proj, "E2E", &cwd);
        let mut t = Session::new(session.as_str(), "term", cwd.as_str());
        t.windows = vec![TermWindow::new(term_window.as_str(), "Terminal 1", TermWindowKind::Terminal)];
        t.active_window_id = Some(term_window.clone());
        t.next_terminal_index = 2;
        if let Some(pi) = s.workspace.projects.iter().position(|p| p.id == proj) {
            s.workspace.projects[pi].sessions.push(t);
            s.workspace.select_session(&session);
            true
        } else {
            false
        }
    });
    if !seeded {
        return Err(anyhow::anyhow!("could not seed the promotable project"));
    }

    // Spawn the window's live pty. HOME=sandbox is spec-provided (spec-wins) so the
    // synthetic chain sources the sandbox ~/.zshrc; NICE_CLAUDE_OVERRIDE keeps a
    // consistent claude for any resolve; the window injection adds the socket + ids.
    let spec = SpawnSpec::shell(cwd)
        .with_env(vec![
            ("HOME".to_string(), fixture.home_str()),
            ("NICE_CLAUDE_OVERRIDE".to_string(), fixture.stub_str()),
        ])
        .with_size(ROWS, COLS);
    let spawned = state.update(cx, |s, cx| {
        s.ptys.spawn_window(&session, &term_window, spec, cx).is_ok()
    });
    if !spawned {
        return Err(anyhow::anyhow!("could not spawn the promotable window's pty"));
    }
    // Re-render so the host's sweep subscribes the fresh window.
    let _ = state.update(cx, |_s, cx| cx.notify());

    let Some(handle) = term_window_handle(cx, state, &session, &term_window) else {
        return Err(anyhow::anyhow!("no handle for the promotable window"));
    };
    if !poll_grid_contains(cx, &handle, READY_MARKER).await {
        return Err(anyhow::anyhow!("the promotable window's shell never became ready (no READY)"));
    }
    write_line(cx, &handle, b"claude\n");
    Ok((session, term_window, handle))
}

// -- rotation leg (d) --------------------------------------------------------

/// Drive the `/branch` (resume + new id) then `/clear` rotations over the socket
/// against the just-promoted session, asserting root promotion + `/clear` in-place
/// tracking (Swift `TabModel.swift:336-362`).
async fn drive_rotation(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    socket_path: &str,
    fixture: &Fixture,
    session: &str,
    term_window: &str,
    failures: &mut Vec<String>,
) {
    let proj = format!("{}-proj", session.trim_end_matches("-tab"));
    let Some(old_sid) = session_claude_id(cx, state, session) else {
        failures.push("(d) rotation: the promoted session carries no session id to rotate".into());
        return;
    };

    // -- /branch: source=resume + a NEW id + a cwd move.
    let branch_wt = format!("{}/.claude/worktrees/branch-wt", fixture.work_c.display());
    send_session_update(cx, socket_path, term_window, "e2e-branch-id", Some("resume"), Some(&branch_wt)).await;
    match poll_branch_parent(cx, state, &proj, &old_sid, session).await {
        None => failures.push(
            "(d) branch: no sibling parent session pinned to the OLD id materialized on the shipped sidebar".into(),
        ),
        Some(parent_id) => {
            let (parent, orig) = state.update(cx, |s, _cx| {
                (s.workspace.session_for(&parent_id).cloned(), s.workspace.session_for(session).cloned())
            });
            match (parent, orig) {
                (Some(parent), Some(orig)) => {
                    // Root promotion: the new parent renders at ROOT (parent_session_id None).
                    if parent.parent_session_id.is_some() {
                        failures.push(format!(
                            "(d) branch: ROOT PROMOTION — the new parent must render at root \
                             (parent_session_id == None), got {:?}",
                            parent.parent_session_id
                        ));
                    }
                    // The originating session re-parents UNDER the new root (renders indented).
                    if orig.parent_session_id.as_deref() != Some(parent_id.as_str()) {
                        failures.push(format!(
                            "(d) branch: the originating session must re-parent under the new root \
                             (indented), got parent_session_id {:?}",
                            orig.parent_session_id
                        ));
                    }
                    // The originating session carries the NEW id.
                    if orig.claude_session_id.as_deref() != Some("e2e-branch-id") {
                        failures.push(format!(
                            "(d) branch: the originating session must carry the NEW session id, got {:?}",
                            orig.claude_session_id
                        ));
                    }
                }
                _ => failures.push("(d) branch: the branch parent / originating session vanished".into()),
            }
        }
    }

    // -- /clear: source=clear + a new id ⇒ id updates in place, NO new session.
    let count_before = project_session_count(cx, state, &proj);
    send_session_update(cx, socket_path, term_window, "e2e-cleared-id", Some("clear"), None).await;
    if !poll_session_claude_id(cx, state, session, "e2e-cleared-id").await {
        failures.push("(d) clear: /clear must update the originating session's session id in place".into());
    }
    if project_session_count(cx, state, &proj) != count_before {
        failures.push("(d) clear: /clear must NOT materialize a new session".into());
    }
}

// -- raw-socket session_update drive -----------------------------------------

/// Fire-and-forget a `session_update` over the control socket on a dedicated thread
/// (it carries no reply — the parser drops the client fd before dispatch), writing
/// the framed line exactly once, then let the foreground drain route it.
async fn send_session_update(
    cx: &mut AsyncApp,
    socket_path: &str,
    term_window_id: &str,
    claude_session_id: &str,
    source: Option<&str>,
    cwd: Option<&str>,
) {
    let payload = session_update_json(term_window_id, claude_session_id, source, cwd);
    let path = socket_path.to_string();
    let done = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            match UnixStream::connect(&path) {
                Ok(mut s) => {
                    let _ = s.write_all(payload.as_bytes());
                    let _ = s.write_all(b"\n");
                    let _ = s.flush();
                    return;
                }
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => return,
            }
        }
    });
    let _ = done.join();
    settle(cx, POLL_MS).await;
}

fn session_update_json(term_window_id: &str, claude_session_id: &str, source: Option<&str>, cwd: Option<&str>) -> String {
    let mut fields = format!(
        "\"action\":\"session_update\",\"paneId\":\"{}\",\"sessionId\":\"{}\"",
        json_escape(term_window_id),
        json_escape(claude_session_id),
    );
    if let Some(src) = source {
        fields.push_str(&format!(",\"source\":\"{}\"", json_escape(src)));
    }
    if let Some(c) = cwd {
        fields.push_str(&format!(",\"cwd\":\"{}\"", json_escape(c)));
    }
    format!("{{{fields}}}")
}

/// Minimal JSON string escaping (temp paths + kebab ids carry no control chars).
fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Whether `s` is a lowercase RFC-4122 v4 UUID (`8-4-4-4-12`, version nibble `4`,
/// variant nibble in `[89ab]`) — the shape `mint_session_uuid` produces.
fn is_v4_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    for (i, &c) in b.iter().enumerate() {
        let ok = match i {
            8 | 13 | 18 | 23 => c == b'-',
            14 => c == b'4',
            19 => matches!(c, b'8' | b'9' | b'a' | b'b'),
            _ => c.is_ascii_hexdigit() && !c.is_ascii_uppercase(),
        };
        if !ok {
            return false;
        }
    }
    true
}

// -- model / session readers -------------------------------------------------

fn all_session_ids(cx: &mut AsyncApp, state: &Entity<WindowState>) -> Vec<String> {
    state.update(cx, |s, _cx| {
        s.workspace
            .projects
            .iter()
            .flat_map(|p| p.sessions.iter().map(|t| t.id.clone()))
            .collect()
    })
}

async fn poll_new_session(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    before: &[String],
) -> Option<String> {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        let now = all_session_ids(cx, state);
        if let Some(new) = now.iter().find(|t| !before.contains(t)) {
            return Some(new.clone());
        }
    }
    None
}

fn session_window_ids(cx: &mut AsyncApp, state: &Entity<WindowState>, session_id: &str) -> Option<(String, String)> {
    state.update(cx, |s, _cx| {
        let session = s.workspace.session_for(session_id)?;
        let claude = session.windows.first()?.id.clone();
        let companion = session.windows.get(1)?.id.clone();
        Some((claude, companion))
    })
}

fn window_is_claude_running(cx: &mut AsyncApp, state: &Entity<WindowState>, session_id: &str, term_window_id: &str) -> bool {
    state.update(cx, |s, _cx| {
        s.workspace
            .session_for(session_id)
            .and_then(|t| t.windows.iter().find(|p| p.id == term_window_id))
            .map(|p| p.is_claude_running)
            .unwrap_or(false)
    })
}

fn session_claude_id(cx: &mut AsyncApp, state: &Entity<WindowState>, session_id: &str) -> Option<String> {
    state.update(cx, |s, _cx| s.workspace.session_for(session_id).and_then(|t| t.claude_session_id.clone()))
}

async fn poll_branch_parent(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    project_id: &str,
    claude_session_id: &str,
    exclude_session: &str,
) -> Option<String> {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        let found = state.update(cx, |s, _cx| {
            s.workspace.projects.iter().find(|p| p.id == project_id).and_then(|p| {
                p.sessions
                    .iter()
                    .find(|t| t.id != exclude_session && t.claude_session_id.as_deref() == Some(claude_session_id))
                    .map(|t| t.id.clone())
            })
        });
        if found.is_some() {
            return found;
        }
    }
    None
}

fn project_session_count(cx: &mut AsyncApp, state: &Entity<WindowState>, project_id: &str) -> usize {
    state.update(cx, |s, _cx| {
        s.workspace.projects.iter().find(|p| p.id == project_id).map(|p| p.sessions.len()).unwrap_or(0)
    })
}

async fn poll_session_claude_id(cx: &mut AsyncApp, state: &Entity<WindowState>, session_id: &str, want: &str) -> bool {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        if session_claude_id(cx, state, session_id).as_deref() == Some(want) {
            return true;
        }
    }
    false
}

async fn poll_session_status(cx: &mut AsyncApp, state: &Entity<WindowState>, session_id: &str, want: SessionStatus) -> bool {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        let got = state.update(cx, |s, _cx| s.workspace.session_for(session_id).map(|t| t.status()));
        if got == Some(want) {
            return true;
        }
    }
    false
}

async fn poll_has_window(cx: &mut AsyncApp, state: &Entity<WindowState>, session_id: &str, term_window_id: &str) -> bool {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        if state.update(cx, |s, _cx| s.ptys.has_window(session_id, term_window_id)) {
            return true;
        }
    }
    false
}

async fn poll_window_promoted(cx: &mut AsyncApp, state: &Entity<WindowState>, session_id: &str, term_window_id: &str) -> bool {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        let ok = state.update(cx, |s, _cx| {
            s.workspace
                .session_for(session_id)
                .and_then(|t| t.windows.iter().find(|p| p.id == term_window_id))
                .map(|p| p.kind == TermWindowKind::Claude && p.is_claude_running)
                .unwrap_or(false)
        });
        if ok {
            return true;
        }
    }
    false
}

// -- grid / pty helpers ------------------------------------------------------

fn term_window_handle(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    session_id: &str,
    term_window_id: &str,
) -> Option<Entity<TerminalSessionHandle>> {
    state.update(cx, |s, _cx| s.ptys.term_window_handle(session_id, term_window_id))
}

fn grid_of(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>) -> String {
    handle.update(cx, |h, _cx| h.session().grid_lines().join("\n"))
}

/// The last few non-blank grid lines, for a failure message.
fn grid_tail(grid: &str) -> String {
    grid.lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .join(" | ")
}

async fn poll_grid_contains(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>, needle: &str) -> bool {
    for _ in 0..READY_POLLS {
        settle(cx, POLL_MS).await;
        if grid_of(cx, handle).contains(needle) {
            return true;
        }
    }
    false
}

/// All whitespace removed — the terminal HARD-WRAPS a long argv line across grid
/// rows (and [`grid_lines`](nice_term_core::Session::grid_lines) joins rows with
/// `\n`), so a `--settings <path> --session-id <uuid>` never appears as one
/// contiguous line. Stripping whitespace from both sides reconstructs the wrapped
/// token stream; the paths/uuids are long + specific enough that no false match is
/// possible.
fn strip_ws(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Whitespace-insensitive [`poll_grid_contains`] — for asserting a wrapped argv.
async fn poll_grid_contains_wrapped(
    cx: &mut AsyncApp,
    handle: &Entity<TerminalSessionHandle>,
    needle: &str,
) -> bool {
    let want = strip_ws(needle);
    for _ in 0..READY_POLLS {
        settle(cx, POLL_MS).await;
        if strip_ws(&grid_of(cx, handle)).contains(&want) {
            return true;
        }
    }
    false
}

fn write_line(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>, bytes: &[u8]) {
    let _ = handle.update(cx, |h, _cx| {
        let _ = h.session().write_input(bytes);
    });
}

fn write_window_line(cx: &mut AsyncApp, state: &Entity<WindowState>, session_id: &str, term_window_id: &str, bytes: &[u8]) {
    if let Some(handle) = term_window_handle(cx, state, session_id, term_window_id) {
        write_line(cx, &handle, bytes);
    }
}
