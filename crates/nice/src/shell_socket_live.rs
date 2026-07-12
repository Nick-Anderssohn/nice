//! `shell-socket` self-test scenario — the R14 shell-injection + control-socket
//! transport gate.
//!
//! Where the ported unit suites pin the pure pieces (the frozen rc-stub text +
//! real-zsh chain in [`crate::shell_inject`], the socket parse/normalization +
//! self-healing in [`crate::control_socket`], the spec-wins env merge + the
//! per-mode env matrix in [`crate::session_manager`]), this scenario drives the
//! **whole transport end to end** on a real pty: it spawns real login shells
//! through the live spawn path with the window's manager env injection active
//! (`NICE_SOCKET` + the synthetic `ZDOTDIR` rc chain + per-pane ids), then asserts
//! only **transport** properties — never a handler's decision, so it survives R15
//! replacing the `claude` stub body unchanged.
//!
//! ## What it asserts (all fail-loud, grid-poll bounded — never sleep-and-hope)
//!
//! 1. **USER_RC_RAN chain-back** — a terminal pane's login shell, under the
//!    synthetic `ZDOTDIR`, restores the user's `ZDOTDIR` and sources the fixture
//!    `~/.zshrc` (which echoes `USER_RC_RAN`): proof the whole `.zshenv` →
//!    `.zshrc` chain fires and the `claude()` shadow / OSC 7 hook layer on top.
//! 2. **`claude --help` bypass** — the shadow's non-interactive short-circuit runs
//!    the stub `claude` directly (grid shows its argv echo) and sends NO socket
//!    message.
//! 3. **`claude` handshake** — a bare `claude` handshakes over `NICE_SOCKET`; the
//!    window routing point records a `claude` message carrying the pane's exact
//!    injected `tabId` / `paneId` and its `cwd`, and a raw-`UnixStream` probe
//!    confirms exactly ONE newline-terminated reply line comes back (the `Reply`
//!    one-line contract over the wire).
//! 4. **raw `session_update`** — the headless app-level driver TRANCHE-2-NOTES §1
//!    asks for: a raw `UnixStream` `session_update` line surfaces at the routing
//!    point parsed + normalized (the fire-and-forget path).
//! 5. **prefill** — a pane spawned with `NICE_PREFILL_COMMAND` in its spec env
//!    shows the pre-typed command at the prompt via the stub's `print -z` tail,
//!    and (proof nothing ran) its side-effect never happens.
//! 6. **self-heal** — deleting the socket file autonomously rebinds it at the same
//!    path (the 30 s health `stat()`, shortened here) so a fresh connect succeeds.
//! 7. **teardown unlink** — [`WindowState::teardown`](crate::window_state::WindowState::teardown)
//!    stops the socket and unlinks its file.
//!
//! ## Hermeticity
//!
//! Fully sandboxed (tranche-3 rule): a fake `$HOME` with a marker `.zshrc`, a stub
//! `claude` on `PATH` (also exported as `NICE_CLAUDE_OVERRIDE` — a no-op until
//! R15's probe consumes it, which future-proofs this scenario against the handler
//! replacement), and a `ZDOTDIR` written by calling the R14 stub writer DIRECTLY
//! against a temp path. It never launches the machine's real `claude`, never
//! writes the real `~`/Application Support, and self-activates its window. Like
//! `session-lifecycle` it installs no `WindowRegistry`, so it is registered before
//! `multiwindow` (which owns the quit-when-empty close observer).

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use gpui::{div, prelude::*, AnyWindowHandle, AsyncApp, Context, Entity, IntoElement, Render, Window};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_term_core::SpawnSpec;
use nice_term_view::TerminalSessionHandle;

use crate::control_socket::RecordedSocketMessage;
use crate::window_state::WindowState;

// -- fixed geometry / timing -------------------------------------------------

const ROWS: u16 = 24;
const COLS: u16 = 80;

/// Poll cap for a real login shell to print its rc marker / a pre-typed line, on
/// the real pty clock (a login shell sourcing the synthetic chain + fixture rc).
const READY_POLLS: usize = 80;
/// Poll cap for a routed socket message to land after its `nc` / raw drive (the
/// socket accept thread → mpsc → the waker-woken foreground drain hop).
const ROUTE_POLLS: usize = 60;
/// Interval between polls (real wall-clock; the pty child + socket thread run on
/// OS threads the simulated dispatcher does not drive).
const POLL_MS: u64 = 100;

/// A short health-check interval so the self-heal step rebinds quickly (production
/// is 30 s; the transport is identical, only the cadence differs).
const HEALTH_INTERVAL: Duration = Duration::from_millis(250);

/// The marker the fixture `~/.zshrc` echoes once sourced (the chain-back proof +
/// the shell-readiness signal the driver polls for).
const USER_RC_MARKER: &str = "USER_RC_RAN";
/// The prefix the stub `claude` echoes so the `--help` bypass is observable.
const STUB_CLAUDE_ECHO: &str = "STUB_CLAUDE_ARGV:";
/// A distinctive token in the prefill sentinel's basename, polled for in the grid.
const PREFILL_TOKEN: &str = "PREFILL_SENTINEL_9271";

/// Minimal RAF-animated root: keeps the window compositing (and the frame clock
/// stamped for the harness's per-scenario reset) while the headless driver runs.
struct ShellSocketRoot;

impl Render for ShellSocketRoot {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        nice_harness::frame::stamp();
        window.request_animation_frame();
        div().size_full().bg(gpui::rgb(0x11141b))
    }
}

/// The sandboxed fixture: a fake `$HOME` + a marker `.zshrc`, a stub `claude` on a
/// private `PATH` dir, a stub-written `ZDOTDIR`, and the prefill sentinel path.
struct Fixture {
    /// Canonicalized (symlinks resolved) so a pane's `$PWD` compares equal.
    home: PathBuf,
    stub_claude: PathBuf,
    zdotdir: PathBuf,
    /// The file the prefill command would `touch` IF executed (it must not be).
    prefill_sentinel: PathBuf,
}

impl Fixture {
    fn build() -> Result<Self> {
        let base = std::env::temp_dir().join(format!("nice-shell-socket-{}", std::process::id()));
        std::fs::create_dir_all(&base).context("create fixture base")?;
        // Canonicalize so /var/folders (a symlink to /private/var/folders on macOS)
        // resolves — a spawned shell's getcwd()-derived $PWD is the physical path,
        // so the recorded handshake cwd must be compared against the canonical form.
        let base = base.canonicalize().context("canonicalize fixture base")?;

        let home = base.join("home");
        std::fs::create_dir_all(&home).context("create fake HOME")?;
        let stub_bin = base.join("bin");
        std::fs::create_dir_all(&stub_bin).context("create stub bin")?;
        let zdotdir = base.join("zdotdir");
        let prefill_sentinel = base.join(format!("{PREFILL_TOKEN}.marker"));

        // The fixture ~/.zshrc: put the stub `claude` + `nc` (/usr/bin) on PATH and
        // echo the chain-back marker. This is what the synthetic `.zshrc` stub
        // sources (after restoring ZDOTDIR), so `claude()` / the OSC 7 hook layer
        // on top of it.
        std::fs::write(
            home.join(".zshrc"),
            format!(
                "export PATH=\"{}:/usr/bin:/bin:/usr/sbin:/sbin\"\nprint -r -- {USER_RC_MARKER}\n",
                stub_bin.display()
            ),
        )
        .context("write fixture .zshrc")?;

        // The stub `claude`: echoes its argv (so the `--help` bypass is observable)
        // and exits. NEVER the machine's real claude (hermeticity).
        let stub_claude = stub_bin.join("claude");
        std::fs::write(
            &stub_claude,
            format!("#!/bin/sh\nprintf '%s %s\\n' '{STUB_CLAUDE_ECHO}' \"$*\"\n"),
        )
        .context("write stub claude")?;
        std::fs::set_permissions(&stub_claude, std::fs::Permissions::from_mode(0o755))
            .context("chmod stub claude")?;

        // The ZDOTDIR: write the FROZEN stubs by calling the R14 writer directly
        // against this temp path (never the real Application Support location).
        crate::shell_inject::write_stubs(&zdotdir).context("write ZDOTDIR stubs")?;

        Ok(Fixture {
            home,
            stub_claude,
            zdotdir,
            prefill_sentinel,
        })
    }

    fn home_str(&self) -> String {
        self.home.to_string_lossy().into_owned()
    }
}

/// Open the `shell-socket` scenario window and spawn its headless driver
/// (self-reported gate). The per-window [`WindowState`] is minted up front so the
/// driver can arm its control socket + drive its [`SessionManager`](crate::session_manager::SessionManager)
/// directly against the fixture paths.
pub fn open_shell_socket_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let fixture = Fixture::build()?;

    // The per-window state (the real R12 composition root). Created before the
    // window so the async driver owns a handle; `AsyncApp` entity `update` returns
    // the value directly (panics if the app is gone), matching the landed scenarios.
    let state = cx.update(|app| app.new(|_cx| WindowState::new(fixture.home_str())));

    let window = cx.open_window(crate::app::window_options(), |_window, cx| {
        cx.new(|_cx| ShellSocketRoot)
    })?;
    let window: AnyWindowHandle = window.into();

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_shell_socket(acx, state, fixture).await;
        eprintln!("[selftest] scenario 'shell-socket': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(window)
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

async fn run_shell_socket(
    cx: &mut AsyncApp,
    state: Entity<WindowState>,
    fixture: Fixture,
) -> CadenceReport {
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 300).await;

    let mut failures: Vec<String> = Vec::new();

    // Arm the control socket + set the window's shell-injection env BEFORE any pane
    // forks (the env-before-fork invariant), pointing ZDOTDIR at the fixture stubs
    // and using a short health interval so the self-heal step is quick. This is the
    // exact production wiring (`crate::app::arm_window_control_socket`), so a socket
    // regression surfaces here.
    let zdotdir = fixture.zdotdir.to_string_lossy().into_owned();
    let socket_path = state.update(cx, |s, cx| {
        crate::app::arm_window_control_socket(
            s,
            cx,
            Some(zdotdir),
            // Nice inherited no ZDOTDIR in this fixture → NICE_USER_ZDOTDIR is
            // injected as the empty string (the .zshenv stub then discovers the
            // user's intended layout by sourcing the fixture ~/.zshenv, absent here,
            // so it resolves to $HOME = the fake home).
            None,
            Some(HEALTH_INTERVAL),
        )
    });

    // === 1. chain-back: spawn a real login shell, poll for USER_RC_RAN ==========
    let pane1_spec = SpawnSpec::shell(fixture.home_str())
        .with_env(vec![
            // HOME is spec-provided so it wins over the forwarded process HOME —
            // the synthetic chain sources THIS fake home's rc, never the real one.
            ("HOME".to_string(), fixture.home_str()),
            // Exported as a no-op today; R15's probe will consume it.
            (
                "NICE_CLAUDE_OVERRIDE".to_string(),
                fixture.stub_claude.to_string_lossy().into_owned(),
            ),
        ])
        .with_size(ROWS, COLS);

    let Some((tab_id, pane_id, handle)) = spawn_terminal_pane(cx, &state, pane1_spec) else {
        return CadenceReport::error(
            "shell-socket: could not create + spawn the handshake terminal pane",
        );
    };
    if !poll_grid_contains(cx, &handle, USER_RC_MARKER, READY_POLLS).await {
        return CadenceReport::error(
            "shell-socket: the login shell never printed USER_RC_RAN — the synthetic \
             ZDOTDIR chain did not source the user's ~/.zshrc",
        );
    }

    // === 2. `claude --help` bypasses the socket entirely ========================
    write_line(cx, &handle, b"claude --help\n");
    if !poll_grid_contains(cx, &handle, STUB_CLAUDE_ECHO, READY_POLLS).await {
        failures.push(
            "claude --help: the shadow did not passthrough to the stub `claude` (no argv echo)"
                .into(),
        );
    }
    // The bypass must have recorded NO socket message (nothing handshaked yet).
    if recorded(cx, &state).iter().any(is_claude) {
        failures.push(
            "claude --help: a non-interactive invocation must NOT reach the control socket".into(),
        );
    }

    // === 3. `claude` handshake: recorded ids/cwd + exactly one reply line =======
    write_line(cx, &handle, b"claude\n");
    let want_cwd = fixture.home.clone();
    let recorded_claude = poll_recorded(cx, &state, ROUTE_POLLS, |m| match m {
        RecordedSocketMessage::Claude { tab_id: t, .. } => t == &tab_id,
        _ => false,
    })
    .await;
    match recorded_claude {
        Some(RecordedSocketMessage::Claude {
            cwd,
            tab_id: t,
            pane_id: p,
            ..
        }) => {
            if t != tab_id {
                failures.push(format!(
                    "handshake: recorded tabId {t:?} != the pane's injected NICE_TAB_ID {tab_id:?}"
                ));
            }
            if p != pane_id {
                failures.push(format!(
                    "handshake: recorded paneId {p:?} != the pane's injected NICE_PANE_ID {pane_id:?}"
                ));
            }
            if !cwd_matches(&cwd, &want_cwd) {
                failures.push(format!(
                    "handshake: recorded cwd {cwd:?} != the pane's spawn cwd {want_cwd:?}"
                ));
            }
        }
        _ => failures.push(
            "handshake: a bare `claude` did not reach the control socket (no recorded \
             claude message with the pane's tabId)"
                .into(),
        ),
    }

    // A raw-UnixStream probe confirms exactly ONE newline-terminated reply line
    // comes back (the `Reply` one-line contract over the wire). Done on a helper
    // thread so the blocking read never wedges the foreground drain that answers it.
    match await_reply(cx, raw_claude_probe(socket_path.clone()), ROUTE_POLLS).await {
        Some(bytes) => {
            let newlines = bytes.iter().filter(|&&b| b == b'\n').count();
            if newlines != 1 || bytes.last() != Some(&b'\n') {
                failures.push(format!(
                    "handshake reply: expected exactly one newline-terminated line, got {} \
                     newline(s): {:?}",
                    newlines,
                    String::from_utf8_lossy(&bytes)
                ));
            }
        }
        None => failures.push("handshake reply: the socket sent no reply line to a raw claude".into()),
    }

    // === 4. raw session_update surfaces parsed + normalized =====================
    raw_fire_and_forget(
        &socket_path,
        r#"{"action":"session_update","paneId":"RAW_PANE","sessionId":"RAW_SID","source":"resume","cwd":"/raw/cwd"}"#,
    );
    let recorded_update = poll_recorded(cx, &state, ROUTE_POLLS, |m| {
        matches!(m, RecordedSocketMessage::SessionUpdate { pane_id, .. } if pane_id == "RAW_PANE")
    })
    .await;
    match recorded_update {
        Some(RecordedSocketMessage::SessionUpdate {
            session_id,
            source,
            cwd,
            ..
        }) => {
            if session_id != "RAW_SID"
                || source.as_deref() != Some("resume")
                || cwd.as_deref() != Some("/raw/cwd")
            {
                failures.push(format!(
                    "session_update: normalized fields wrong (sid={session_id:?} source={source:?} \
                     cwd={cwd:?})"
                ));
            }
        }
        _ => failures.push(
            "session_update: a raw-UnixStream session_update did not surface at the routing point"
                .into(),
        ),
    }

    // === 5. prefill: NICE_PREFILL_COMMAND pre-types, nothing executes ===========
    let prefill_cmd = format!("touch {}", fixture.prefill_sentinel.display());
    let prefill_spec = SpawnSpec::shell(fixture.home_str())
        .with_env(vec![
            ("HOME".to_string(), fixture.home_str()),
            ("NICE_PREFILL_COMMAND".to_string(), prefill_cmd),
        ])
        .with_size(ROWS, COLS);
    match spawn_terminal_pane(cx, &state, prefill_spec) {
        Some((_t, _p, prefill_handle)) => {
            // Wait for readiness (rc marker), then for the pre-typed line to render.
            let _ = poll_grid_contains(cx, &prefill_handle, USER_RC_MARKER, READY_POLLS).await;
            if !poll_grid_contains(cx, &prefill_handle, PREFILL_TOKEN, READY_POLLS).await {
                failures.push(
                    "prefill: NICE_PREFILL_COMMAND was not pre-typed at the prompt (the stub's \
                     `print -z` tail did not render it)"
                        .into(),
                );
            } else if fixture.prefill_sentinel.exists() {
                failures.push(
                    "prefill: the pre-typed command EXECUTED (its sentinel was created) — \
                     print -z must only buffer it, not run it"
                        .into(),
                );
            }
        }
        None => failures.push("prefill: could not spawn the prefill pane".into()),
    }

    // === 6. self-heal: delete the socket file, poll until it rebinds ============
    let _ = std::fs::remove_file(&socket_path);
    if !poll_path_exists(cx, &socket_path, ROUTE_POLLS).await {
        failures.push(
            "self-heal: the socket file was not rebound after deletion (the health check did \
             not rebind at the same path)"
                .into(),
        );
    } else if await_reply(cx, raw_claude_probe(socket_path.clone()), ROUTE_POLLS)
        .await
        .is_none()
    {
        failures.push(
            "self-heal: the rebound socket did not answer a fresh claude connect at the same path"
                .into(),
        );
    }

    // === 7. teardown unlinks the socket file ====================================
    let _ = state.update(cx, |s, _cx| s.teardown());
    if !poll_path_gone(cx, &socket_path, ROUTE_POLLS).await {
        failures.push("teardown: WindowState::teardown did not unlink the control socket".into());
    }
    settle(cx, 150).await;

    build_report(failures)
}

// ---------------------------------------------------------------------------
// Live spawn / grid helpers
// ---------------------------------------------------------------------------

/// Create a terminal tab via the R10 sidebar seam, spawn its seeded pane through
/// the manager's live spawn path (so the window env injection applies), and return
/// `(tab_id, pane_id, handle)`.
fn spawn_terminal_pane(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    spec: SpawnSpec,
) -> Option<(String, String, Entity<TerminalSessionHandle>)> {
    let ids = state.update(cx, |s, cx| {
        let tab_id = s.sidebar_actions.create_terminal_tab(&mut s.model)?;
        let pane_id = s.model.tab_for(&tab_id)?.panes.first()?.id.clone();
        s.session.spawn_pane(&tab_id, &pane_id, spec, cx).ok()?;
        Some((tab_id, pane_id))
    })?;
    let handle = state.update(cx, |s, _cx| s.session.pane_handle(&ids.0, &ids.1))?;
    Some((ids.0, ids.1, handle))
}

fn write_line(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>, bytes: &[u8]) {
    let _ = handle.update(cx, |h, _cx| {
        let _ = h.session().write_input(bytes);
    });
}

fn grid_of(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>) -> String {
    handle.update(cx, |h, _cx| h.session().grid_lines().join("\n"))
}

/// Poll the pane's grid for `needle`, settling between ticks. `true` on the first
/// tick it appears, `false` if the poll cap elapses (a real failure — the fixture
/// never produced it — not a flaky timeout).
async fn poll_grid_contains(
    cx: &mut AsyncApp,
    handle: &Entity<TerminalSessionHandle>,
    needle: &str,
    polls: usize,
) -> bool {
    for _ in 0..polls {
        settle(cx, POLL_MS).await;
        if grid_of(cx, handle).contains(needle) {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Routing-record helpers
// ---------------------------------------------------------------------------

fn recorded(cx: &mut AsyncApp, state: &Entity<WindowState>) -> Vec<RecordedSocketMessage> {
    state.update(cx, |s, _cx| s.recorded_socket_messages().to_vec())
}

fn is_claude(m: &RecordedSocketMessage) -> bool {
    matches!(m, RecordedSocketMessage::Claude { .. })
}

/// Poll the window's recorded socket messages for the first one matching `pred`,
/// settling between ticks (so the foreground drain routes pending messages).
async fn poll_recorded(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    polls: usize,
    pred: impl Fn(&RecordedSocketMessage) -> bool,
) -> Option<RecordedSocketMessage> {
    for _ in 0..polls {
        settle(cx, POLL_MS).await;
        if let Some(m) = recorded(cx, state).into_iter().find(|m| pred(m)) {
            return Some(m);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Raw-UnixStream client drives (the headless app-level driver, TRANCHE-2-NOTES §1)
// ---------------------------------------------------------------------------

/// Fire-and-forget: connect, write one newline-terminated payload, close. Used for
/// `session_update` (no reply) — a quick, non-blocking send from the foreground.
fn raw_fire_and_forget(path: &str, payload: &str) {
    if let Ok(mut s) = UnixStream::connect(path) {
        let _ = s.write_all(payload.as_bytes());
        let _ = s.write_all(b"\n");
        // Drop closes the fd; the socket reads + routes on its own threads.
    }
}

/// Drive a raw `claude` request that expects a reply, on a DEDICATED thread so the
/// blocking read never wedges the foreground drain that answers it. Retries the
/// connect (the self-heal step races a rebinding listener) until it reads a
/// newline-terminated reply or a deadline elapses. Returns the reply bytes over a
/// channel the driver polls between settles.
fn raw_claude_probe(path: String) -> Receiver<Option<Vec<u8>>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let payload = br#"{"action":"claude","cwd":"/raw/probe","args":[],"tabId":"","paneId":""}"#;
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut result: Option<Vec<u8>> = None;
        while Instant::now() < deadline {
            if let Ok(mut s) = UnixStream::connect(&path) {
                let _ = s.set_read_timeout(Some(Duration::from_millis(800)));
                if s.write_all(payload).is_ok() && s.write_all(b"\n").is_ok() {
                    let mut buf = Vec::new();
                    let _ = s.read_to_end(&mut buf);
                    if buf.contains(&b'\n') {
                        result = Some(buf);
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = tx.send(result);
    });
    rx
}

/// Poll a `raw_claude_probe`'s result channel, settling between ticks so the
/// foreground drain runs and replies to the probe's connect.
async fn await_reply(
    cx: &mut AsyncApp,
    rx: Receiver<Option<Vec<u8>>>,
    polls: usize,
) -> Option<Vec<u8>> {
    for _ in 0..polls {
        settle(cx, POLL_MS).await;
        match rx.try_recv() {
            Ok(v) => return v,
            Err(std::sync::mpsc::TryRecvError::Empty) => continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return None,
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

/// Compare a shell-reported `cwd` against the pane's spawn dir, tolerating symlink
/// differences (`/var` vs `/private/var`) by canonicalizing both sides.
fn cwd_matches(reported: &str, want: &Path) -> bool {
    if Path::new(reported) == want {
        return true;
    }
    match (std::fs::canonicalize(reported), std::fs::canonicalize(want)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

async fn poll_path_exists(cx: &mut AsyncApp, path: &str, polls: usize) -> bool {
    for _ in 0..polls {
        settle(cx, POLL_MS).await;
        if Path::new(path).exists() {
            return true;
        }
    }
    false
}

async fn poll_path_gone(cx: &mut AsyncApp, path: &str, polls: usize) -> bool {
    for _ in 0..polls {
        settle(cx, POLL_MS).await;
        if !Path::new(path).exists() {
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
            detail: "shell-socket transport OK: the synthetic ZDOTDIR chain sourced the user's \
                     ~/.zshrc (USER_RC_RAN); `claude --help` passed through to the stub with no \
                     socket message; a bare `claude` handshake recorded the pane's exact \
                     tabId/paneId/cwd and one newline-terminated reply line came back; a raw \
                     session_update surfaced parsed + normalized; NICE_PREFILL_COMMAND pre-typed \
                     at the prompt without executing; the deleted socket self-healed at the same \
                     path; and teardown unlinked it."
                .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} shell-socket assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
