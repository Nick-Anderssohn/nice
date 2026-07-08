//! `handoff` self-test scenario — the R26 handoff gate.
//!
//! Exercises the whole R26 handoff surface over the **SHIPPED window** (opened
//! through `crate::app::open_managed_window` / `build_window_root`, the exact path
//! `crate::app::run` uses) with a **real control socket** + **real ptys**, plus
//! the pure-installer round-trip against injected scratch dirs. Four legs:
//!
//! * **(a) installer round-trip (hermetic, injected dirs)** — `sync_with(true, …)`
//!   lays both `-rs` files down (helper at 0o755), a re-run leaves their mtimes
//!   stable (idempotent), and `sync_with(false, …)` removes the skill subtree +
//!   helper file while the shared helper dir survives. NEVER the real `~/.claude`
//!   / `~/.nice`.
//! * **(b) handoff socket → nested `[HANDOFF]` tab** — a raw-`UnixStream`
//!   `handoff` message naming a seeded originating Claude tab replies `ok`; a NEW
//!   tab appears nested one indent under it (`parent_tab_id` → the originating id)
//!   with the LOCKED title `[HANDOFF] <originating title>` and
//!   `title_manually_set == true`; the stub `claude`'s recorded argv carries
//!   `--session-id <v4 uuid>` then `--model <m> --effort <e>` then the prompt
//!   `Read the handoff notes at <handoffFile>. <instructions>` as the FINAL
//!   positional.
//! * **(c) top-level fallback on a miss** — a `handoff` with an empty `tabId`
//!   still replies `ok` and opens a TOP-LEVEL `[HANDOFF] Session` tab
//!   (`parent_tab_id == None`), proving a miss is a fallback, not a drop.
//! * **(d) empty model/effort omit their flags** — a `handoff` with `model:""` /
//!   `effort:""` records argv carrying NEITHER `--model` NOR `--effort`, with the
//!   prompt still the final positional.
//!
//! ## Hermeticity
//!
//! The stub `claude` is seeded via the process-global
//! `cx.set_global(ResolvedClaudePath(Some(stub)))` and `NICE_CLAUDE_OVERRIDE` is
//! REMOVED (so `is_override` stays `false` and the Nice-injected
//! `--session-id`/`--model`/`--effort`/prompt argv the legs assert is actually
//! emitted). The machine's real `claude` is NEVER spawned — the stub only records
//! its argv, then idles. `HOME` is a sandbox with no rc for the driver's lifetime,
//! so every login shell sources nothing. The installer leg drives only the
//! injected scratch dirs (auto-removed on drop) — never the developer's real
//! `~/.claude` / `~/.nice`. `Gate::SelfReported`; registered BEFORE `multiwindow`
//! (its `build_window_root` only `register`s — no `WindowRegistry` close observer,
//! so its window never trips the quit-when-empty terminus `multiwindow` owns as
//! the last gate).

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, Result};
use gpui::{AnyWindowHandle, AsyncApp, Entity, WindowHandle};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_model::{Pane, PaneKind, Tab};

use crate::app_shell::AppShellView;
use crate::session_manager::ResolvedClaudePath;
use crate::skill_installer::{HELPER_FILENAME, SKILL_FILENAME};
use crate::window_registry::WindowRegistry;
use crate::window_state::WindowState;

// -- timing ------------------------------------------------------------------

/// Poll cap for a routed model mutation (tab creation) after a socket message —
/// the drain-task hop, on the real clock.
const ROUTE_POLLS: usize = 60;
/// Poll cap for the stub `claude` to record its argv after its pane spawns.
const ARGV_POLLS: usize = 60;
/// Interval between polls (real wall-clock; the pty children run on OS threads).
const POLL_MS: u64 = 100;

// -- fixture -----------------------------------------------------------------

/// The sandboxed fixture: a fake `$HOME` (no rc), the injected installer scratch
/// dirs, an argv-recording stub `claude`, and its capture dir.
struct Fixture {
    base: PathBuf,
    home: PathBuf,
    work: PathBuf,
    skill_dir: PathBuf,
    helper_dir: PathBuf,
    capture_dir: PathBuf,
    /// The prior `$HOME`, restored at teardown.
    prev_home: Option<String>,
}

impl Fixture {
    fn build() -> Result<Self> {
        let base = std::env::temp_dir().join(format!("nice-rs-handoff-{}", std::process::id()));
        std::fs::create_dir_all(&base).context("create fixture base")?;
        let base = base.canonicalize().context("canonicalize fixture base")?;

        let home = base.join("home");
        let work = base.join("work");
        // The injected installer dirs mirror the real layout but under the scratch
        // root: `<base>/claude/skills/nice-handoff-rs` + `<base>/nice`. They are
        // NOT pre-created — the installer `create_dir_all`s them.
        let skill_dir = base.join("claude").join("skills").join("nice-handoff-rs");
        let helper_dir = base.join("nice");
        let capture_dir = base.join("argv");
        let bin = base.join("bin");
        for d in [&home, &work, &capture_dir, &bin] {
            std::fs::create_dir_all(d).context("create fixture dir")?;
        }

        // The stub `claude`: record its full argv (one arg per line) to a
        // per-invocation file in the capture dir, then block reading stdin so the
        // pane stays alive until teardown. NEVER the machine's real claude
        // (hermeticity). The capture dir is baked in (not read from env) so the
        // stub is self-contained.
        let stub = bin.join("claude");
        let capture = capture_dir.to_string_lossy().into_owned();
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\n\
                 printf '%s\\n' \"$@\" > \"{capture}/argv-$$\"\n\
                 while IFS= read -r _l; do : ; done\n"
            ),
        )
        .context("write stub claude")?;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .context("chmod stub claude")?;

        // Point the spawn path's `resolve_claude_binary` at the stub via the
        // process-global (seeded in `open_handoff_window`), and REMOVE any prior
        // scenario's `NICE_CLAUDE_OVERRIDE` so `is_override` stays false and the
        // Nice-injected flags emit (the argv legs depend on it).
        // SAFETY: single-threaded scenario setup, before any Claude pane forks;
        // matches the existing `std::env::set_var`/`remove_var` seams.
        unsafe { std::env::remove_var("NICE_CLAUDE_OVERRIDE") };

        let prev_home = std::env::var("HOME").ok();
        Ok(Fixture {
            base,
            home,
            work,
            skill_dir,
            helper_dir,
            capture_dir,
            prev_home,
        })
    }

    fn stub_path(&self) -> String {
        self.base.join("bin").join("claude").to_string_lossy().into_owned()
    }

    fn home_str(&self) -> String {
        self.home.to_string_lossy().into_owned()
    }

    fn work_str(&self) -> String {
        self.work.to_string_lossy().into_owned()
    }

    /// Restore `$HOME` and drop the whole scratch tree.
    fn teardown(&self) {
        // SAFETY: single-threaded teardown after the driver's last spawn.
        match &self.prev_home {
            Some(h) => unsafe { std::env::set_var("HOME", h) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

// -- scenario wiring ---------------------------------------------------------

/// Open the `handoff` window through the SHIPPED builder and spawn its driver
/// (self-reported gate). Seeds the stub-`claude` `ResolvedClaudePath` global and
/// sandboxes `HOME` for the driver's lifetime (restored at teardown).
pub fn open_handoff_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let fixture = Fixture::build()?;
    let stub = fixture.stub_path();
    let home = fixture.home_str();
    let whandle: WindowHandle<AppShellView> = cx.update(|app| {
        // The shipped builder reads the process-global `SharedFontSettings` (via
        // the pane host); `install_shortcuts` seeds it (idempotent).
        crate::keymap::install_shortcuts(app);
        // Seed the stub as the resolved claude binary (env override unset ⇒
        // `is_override` false ⇒ the full Nice argv reaches the stub).
        app.set_global(ResolvedClaudePath(Some(stub.clone())));
        // Sandbox HOME (no rc) for the whole driver; restored at teardown.
        // SAFETY: single-threaded setup; the Main pane forks synchronously inside
        // `open_managed_window` under this HOME.
        unsafe { std::env::set_var("HOME", &home) };
        crate::app::open_managed_window(app)
    })?;
    let any: AnyWindowHandle = whandle.into();

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_handoff(acx, whandle, fixture).await;
        eprintln!("[selftest] scenario 'handoff': {}", report.detail);
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

async fn run_handoff(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fixture: Fixture,
) -> CadenceReport {
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 500).await;

    let mut failures: Vec<String> = Vec::new();

    // === (a) installer round-trip against the INJECTED scratch dirs =============
    installer_round_trip_leg(&fixture, &mut failures);

    // Resolve the shipped window's per-window state + its control socket path.
    let id = AnyWindowHandle::from(whandle).window_id();
    let Some(state) = cx.update(|app| WindowRegistry::state_for_window(app, id)) else {
        fixture.teardown();
        return CadenceReport::error(
            "handoff: the shipped builder did not register the window's WindowState",
        );
    };
    let Some(socket_path) = state.update(cx, |s, _cx| s.control_socket_path()) else {
        fixture.teardown();
        return CadenceReport::error(
            "handoff: the shipped window armed no control socket (no path to drive)",
        );
    };
    let work = fixture.work_str();

    // === (b) handoff socket → nested [HANDOFF] tab with locked title + argv ======
    // Seed a model-only originating Claude tab (present, non-Terminals, owns its
    // pane) so the handoff nests under it.
    seed_originating_claude_tab(cx, &state, &work, "orig-tab", "orig-pane", "my-project");
    let before_b = all_tab_ids(cx, &state);
    let hf_b = format!("{work}/handoff-b.md");
    let reply_b = send_handoff(
        cx,
        &socket_path,
        &work,
        &hf_b,
        "orig-tab",
        "orig-pane",
        "please continue the migration",
        "claude-opus-4-8",
        "xhigh",
    )
    .await;
    if reply_b.as_deref().map(str::trim_end) != Some("ok") {
        failures.push(format!("(b) nested: expected reply 'ok', got {reply_b:?}"));
    }
    match poll_new_tab(cx, &state, &before_b).await {
        None => failures.push("(b) nested: the handoff produced no new tab in the model".into()),
        Some(new_tab) => {
            let snap = state.update(cx, |s, _cx| {
                s.model.tab_for(&new_tab).map(|t| {
                    (
                        t.title.clone(),
                        t.title_manually_set,
                        t.parent_tab_id.clone(),
                    )
                })
            });
            match snap {
                None => failures.push("(b) nested: the new tab vanished before assertion".into()),
                Some((title, locked, parent)) => {
                    if title != "[HANDOFF] my-project" {
                        failures.push(format!(
                            "(b) nested: title must be '[HANDOFF] my-project', got {title:?}"
                        ));
                    }
                    if !locked {
                        failures.push(
                            "(b) nested: the handoff tab's title must be LOCKED (title_manually_set == true)".into(),
                        );
                    }
                    if parent.as_deref() != Some("orig-tab") {
                        failures.push(format!(
                            "(b) nested: the handoff tab must nest under the originating tab \
                             (parent_tab_id == 'orig-tab'), got {parent:?}"
                        ));
                    }
                }
            }
        }
    }
    // The stub argv: `--session-id <v4> --model <m> --effort <e> <prompt-last>`.
    match poll_argv_for(cx, &fixture, &hf_b).await {
        None => failures.push(
            "(b) argv: the stub `claude` never recorded an argv naming the handoff file (the pane \
             did not spawn, or is_override suppressed the flags)"
                .into(),
        ),
        Some(argv) => assert_handoff_argv(
            &argv,
            Some(("claude-opus-4-8", "xhigh")),
            &format!("Read the handoff notes at {hf_b}. please continue the migration"),
            "b",
            &mut failures,
        ),
    }

    // === (c) top-level fallback on a miss: empty tabId ⇒ [HANDOFF] Session ========
    let before_c = all_tab_ids(cx, &state);
    let hf_c = format!("{work}/handoff-c.md");
    let reply_c = send_handoff(cx, &socket_path, &work, &hf_c, "", "", "", "", "").await;
    if reply_c.as_deref().map(str::trim_end) != Some("ok") {
        failures.push(format!("(c) fallback: expected reply 'ok' on a miss, got {reply_c:?}"));
    }
    match poll_new_tab(cx, &state, &before_c).await {
        None => failures.push("(c) fallback: the miss produced no top-level tab".into()),
        Some(new_tab) => {
            let snap = state.update(cx, |s, _cx| {
                s.model
                    .tab_for(&new_tab)
                    .map(|t| (t.title.clone(), t.parent_tab_id.clone()))
            });
            match snap {
                None => failures.push("(c) fallback: the new tab vanished before assertion".into()),
                Some((title, parent)) => {
                    if title != "[HANDOFF] Session" {
                        failures.push(format!(
                            "(c) fallback: a miss must title '[HANDOFF] Session', got {title:?}"
                        ));
                    }
                    if parent.is_some() {
                        failures.push(format!(
                            "(c) fallback: a miss must open TOP-LEVEL (parent_tab_id == None), got {parent:?}"
                        ));
                    }
                }
            }
        }
    }

    // === (d) empty model/effort omit their flags =================================
    let hf_d = format!("{work}/handoff-d.md");
    let reply_d = send_handoff(cx, &socket_path, &work, &hf_d, "", "", "just read it", "", "").await;
    if reply_d.as_deref().map(str::trim_end) != Some("ok") {
        failures.push(format!("(d) omit-flags: expected reply 'ok', got {reply_d:?}"));
    }
    match poll_argv_for(cx, &fixture, &hf_d).await {
        None => failures.push("(d) omit-flags: the stub recorded no argv naming the handoff file".into()),
        Some(argv) => assert_handoff_argv(
            &argv,
            None,
            &format!("Read the handoff notes at {hf_d}. just read it"),
            "d",
            &mut failures,
        ),
    }

    // === teardown: drop every session so no zsh / stub outlives the window ========
    let _ = state.update(cx, |s, _cx| s.teardown());
    settle(cx, 200).await;
    // Clear the stub from the process-global before its file is removed, so a
    // later scenario in an `all` sweep can't resolve a now-deleted path (it would
    // fall back to a plain shell anyway, but keep the global honest).
    let _ = cx.update(|app| app.set_global(ResolvedClaudePath(None)));
    fixture.teardown();

    build_report(failures)
}

/// Leg (a): install lands both `-rs` files (helper 0o755), re-install is
/// mtime-stable, uninstall removes the skill subtree + helper FILE while the
/// shared helper dir survives. Injected scratch dirs only.
fn installer_round_trip_leg(fixture: &Fixture, failures: &mut Vec<String>) {
    let skill_dir = &fixture.skill_dir;
    let helper_dir = &fixture.helper_dir;

    crate::skill_installer::sync_with(true, skill_dir, helper_dir);
    let skill_path = skill_dir.join(SKILL_FILENAME);
    let helper_path = helper_dir.join(HELPER_FILENAME);
    if !skill_path.exists() {
        failures.push("(a) install: SKILL.md was not written to the injected skill dir".into());
    }
    if !helper_path.exists() {
        failures.push("(a) install: the helper was not written to the injected helper dir".into());
    } else {
        match std::fs::metadata(&helper_path) {
            Ok(m) if m.permissions().mode() & 0o777 == 0o755 => {}
            Ok(m) => failures.push(format!(
                "(a) install: the helper must be mode 0o755, got {:o}",
                m.permissions().mode() & 0o777
            )),
            Err(e) => failures.push(format!("(a) install: cannot stat the helper: {e}")),
        }
    }

    // Idempotent: a re-run over identical files leaves both mtimes stable.
    let m1 = (mtime(&skill_path), mtime(&helper_path));
    crate::skill_installer::sync_with(true, skill_dir, helper_dir);
    let m2 = (mtime(&skill_path), mtime(&helper_path));
    if m1.0 != m2.0 || m1.1 != m2.1 {
        failures.push("(a) idempotent: a re-run rewrote an unchanged file (mtime churned)".into());
    }

    // Plant the R16 hook sibling in the SHARED helper dir; it must survive.
    let sibling = helper_dir.join("nice-claude-hook.sh");
    let _ = std::fs::write(&sibling, b"#!/usr/bin/env bash\nexit 0");

    crate::skill_installer::sync_with(false, skill_dir, helper_dir);
    if skill_dir.exists() {
        failures.push("(a) uninstall: the nice-handoff-rs/ skill subtree must be removed".into());
    }
    if helper_path.exists() {
        failures.push("(a) uninstall: the -rs helper FILE must be removed".into());
    }
    if !helper_dir.exists() {
        failures.push("(a) uninstall: the SHARED helper dir must survive (only the file is removed)".into());
    }
    if !sibling.exists() {
        failures.push("(a) uninstall: the planted R16 hook sibling must be untouched".into());
    }
}

fn mtime(path: &PathBuf) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn build_report(failures: Vec<String>) -> CadenceReport {
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "handoff OK: sync_with round-tripped the two -rs files against injected \
                     scratch dirs (helper 0o755, idempotent re-run, uninstall removed the skill \
                     subtree + helper file while the shared dir + R16 sibling survived); a socket \
                     `handoff` naming a seeded Claude tab replied `ok` and opened a nested \
                     [HANDOFF]-titled tab (locked, parented under the originating tab) whose stub \
                     argv carried --session-id <v4> --model --effort then the prompt last; a miss \
                     (empty tabId) still replied `ok` and opened a top-level [HANDOFF] Session tab; \
                     and empty model/effort omitted both flags with the prompt still last."
                .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} handoff assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}

/// Assert the stub `claude` argv: `--session-id <v4 uuid>` present, the optional
/// `--model <m>` / `--effort <e>` flags present-or-absent per `want_flags`, and
/// `prompt` the FINAL element. Token-based (tolerates an optional leading
/// `--settings <path>` should a prior scenario have left the theme gate on).
fn assert_handoff_argv(
    argv: &[String],
    want_flags: Option<(&str, &str)>,
    prompt: &str,
    leg: &str,
    failures: &mut Vec<String>,
) {
    // --session-id <v4 uuid>
    match flag_value(argv, "--session-id") {
        Some(id) if is_v4_uuid(id) => {}
        Some(id) => failures.push(format!("({leg}) argv: --session-id value {id:?} is not a v4 uuid")),
        None => failures.push(format!("({leg}) argv: missing --session-id in {argv:?}")),
    }
    match want_flags {
        Some((model, effort)) => {
            if flag_value(argv, "--model") != Some(model) {
                failures.push(format!("({leg}) argv: --model must be {model:?} in {argv:?}"));
            }
            if flag_value(argv, "--effort") != Some(effort) {
                failures.push(format!("({leg}) argv: --effort must be {effort:?} in {argv:?}"));
            }
        }
        None => {
            if argv.iter().any(|a| a == "--model") {
                failures.push(format!("({leg}) argv: --model must be OMITTED for empty model, got {argv:?}"));
            }
            if argv.iter().any(|a| a == "--effort") {
                failures.push(format!("({leg}) argv: --effort must be OMITTED for empty effort, got {argv:?}"));
            }
        }
    }
    // The prompt is the single positional arg claude auto-runs — ALWAYS last.
    if argv.last().map(String::as_str) != Some(prompt) {
        failures.push(format!(
            "({leg}) argv: the prompt must be the FINAL positional {prompt:?}, got last {:?}",
            argv.last()
        ));
    }
}

/// The value following `flag` in `argv` (`None` if the flag is absent or last).
fn flag_value<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    argv.iter()
        .position(|a| a == flag)
        .and_then(|i| argv.get(i + 1))
        .map(String::as_str)
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

// -- raw-socket `handoff` drive ----------------------------------------------

/// Drive a raw `handoff` request over the control socket on a DEDICATED thread
/// (so the blocking read never wedges the foreground drain that answers it), then
/// poll its reply channel between settles. Returns the trimmed reply line (`None`
/// on timeout / no reply).
#[allow(clippy::too_many_arguments)]
async fn send_handoff(
    cx: &mut AsyncApp,
    socket_path: &str,
    cwd: &str,
    handoff_file: &str,
    tab_id: &str,
    pane_id: &str,
    instructions: &str,
    model: &str,
    effort: &str,
) -> Option<String> {
    let payload = handoff_json(cwd, handoff_file, tab_id, pane_id, instructions, model, effort);
    let rx = raw_request(socket_path.to_string(), payload);
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        match rx.try_recv() {
            Ok(Some(bytes)) => return Some(String::from_utf8_lossy(&bytes).into_owned()),
            Ok(None) => return None,
            Err(std::sync::mpsc::TryRecvError::Empty) => continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return None,
        }
    }
    None
}

/// Connect, write one newline-terminated JSON payload, read the reply to EOF (the
/// handler drops the server end after replying). Retries the connect until a
/// newline-terminated reply arrives or a deadline elapses.
fn raw_request(path: String, payload: String) -> Receiver<Option<Vec<u8>>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut result: Option<Vec<u8>> = None;
        while Instant::now() < deadline {
            if let Ok(mut s) = UnixStream::connect(&path) {
                let _ = s.set_read_timeout(Some(Duration::from_millis(800)));
                if s.write_all(payload.as_bytes()).is_ok() && s.write_all(b"\n").is_ok() {
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

/// Build the frozen `handoff` NDJSON request line (the schema the installed
/// helper posts).
#[allow(clippy::too_many_arguments)]
fn handoff_json(
    cwd: &str,
    handoff_file: &str,
    tab_id: &str,
    pane_id: &str,
    instructions: &str,
    model: &str,
    effort: &str,
) -> String {
    format!(
        "{{\"action\":\"handoff\",\"cwd\":\"{}\",\"handoffFile\":\"{}\",\"tabId\":\"{}\",\"paneId\":\"{}\",\"instructions\":\"{}\",\"model\":\"{}\",\"effort\":\"{}\"}}",
        json_escape(cwd),
        json_escape(handoff_file),
        json_escape(tab_id),
        json_escape(pane_id),
        json_escape(instructions),
        json_escape(model),
        json_escape(effort),
    )
}

/// Minimal JSON string escaping (temp paths + kebab args carry no control chars).
fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// -- model / argv readers ----------------------------------------------------

fn all_tab_ids(cx: &mut AsyncApp, state: &Entity<WindowState>) -> Vec<String> {
    state.update(cx, |s, _cx| {
        s.model
            .projects
            .iter()
            .flat_map(|p| p.tabs.iter().map(|t| t.id.clone()))
            .collect()
    })
}

/// Poll until a tab id appears that was not in `before`, returning it.
async fn poll_new_tab(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    before: &[String],
) -> Option<String> {
    for _ in 0..ROUTE_POLLS {
        settle(cx, POLL_MS).await;
        let now = all_tab_ids(cx, state);
        if let Some(new) = now.iter().find(|t| !before.contains(t)) {
            return Some(new.clone());
        }
    }
    None
}

/// Poll the capture dir for the argv file whose recorded prompt names
/// `handoff_file` (each leg uses a distinct file, so the match is unambiguous),
/// returning its recorded argv lines.
async fn poll_argv_for(
    cx: &mut AsyncApp,
    fixture: &Fixture,
    handoff_file: &str,
) -> Option<Vec<String>> {
    for _ in 0..ARGV_POLLS {
        // The stub records its argv on its own pty child process; settle (never
        // block the foreground) between reads.
        settle(cx, POLL_MS).await;
        if let Ok(entries) = std::fs::read_dir(&fixture.capture_dir) {
            for entry in entries.flatten() {
                if let Ok(contents) = std::fs::read_to_string(entry.path()) {
                    if contents.contains(handoff_file) {
                        let lines: Vec<String> = contents.lines().map(str::to_string).collect();
                        return Some(lines);
                    }
                }
            }
        }
    }
    None
}

/// Seed a model-only originating Claude tab into a fresh non-Terminals project —
/// present, non-Terminals, owning `pane_id`, so the handoff resolves + nests. The
/// pane needs no live pty (resolution is model-only).
fn seed_originating_claude_tab(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    cwd: &str,
    tab_id: &str,
    pane_id: &str,
    title: &str,
) {
    let _ = state.update(cx, |s, _cx| {
        s.model.ensure_project("orig-proj", "Orig", cwd);
        let mut claude = Pane::new(pane_id, "Claude", PaneKind::Claude);
        claude.is_claude_running = true;
        let mut tab = Tab::new(tab_id, title, cwd);
        tab.panes = vec![
            claude,
            Pane::new(&format!("{tab_id}-t1"), "Terminal 1", PaneKind::Terminal),
        ];
        tab.active_pane_id = Some(pane_id.to_string());
        tab.next_terminal_index = 2;
        if let Some(pi) = s.model.projects.iter().position(|p| p.id == "orig-proj") {
            s.model.projects[pi].tabs.push(tab);
        }
        s.model.select_tab(tab_id);
    });
}
