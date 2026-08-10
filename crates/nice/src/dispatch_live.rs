//! `dispatch` self-test scenario — the dispatch-to-a-worktree gate.
//!
//! The `/nice-dispatch` twin of [`crate::handoff_live`]: it drives the whole
//! dispatch surface over the **SHIPPED window** (opened through
//! `crate::app::open_managed_window` / `build_window_root`, the exact path
//! `crate::app::run` uses) with a **real control socket** + **real ptys**. Two
//! legs:
//!
//! * **(a) raw-socket dispatch → nested `[DISPATCH]` session, opened in the
//!   BACKGROUND** — a raw-`UnixStream` `dispatch` message naming a seeded
//!   originating Claude session replies `ok`; a NEW session appears nested one indent
//!   under it (`parent_session_id` → the originating id) with the LOCKED title
//!   `[DISPATCH] <worktree name>` and `title_manually_set == true`; the
//!   DISPATCHER session is still `active_session_id()` (a dispatch never steals focus)
//!   while the unselected child's Claude window is nonetheless
//!   `is_claude_running`; the session's `cwd` is the PAYLOAD cwd, not the
//!   dispatcher's own; and the stub `claude`'s recorded argv carries
//!   `--session-id <v4 uuid>` then `--add-dir <dirname(taskFile)>` IMMEDIATELY
//!   followed by `--worktree <name>` (the variadic-`--add-dir` terminator),
//!   then `--model` / `--effort`, then the prompt as the FINAL positional.
//! * **(b) the REAL installed helper, end to end** — `sync_with(true, …)`
//!   installs the pair into the fixture's sandboxed dirs, and THAT installed
//!   `nice-dispatch.sh` is then run as a real subprocess from a SUBDIRECTORY of
//!   a `git init`-ed scratch repo with `NICE_SOCKET` / `NICE_TAB_ID` /
//!   `NICE_PANE_ID` pointing at the live window + the seeded dispatcher session. It
//!   exits 0 saying the session is opening, and the session that appears is nested under
//!   the dispatcher, `[DISPATCH]`-titled, unselected — and its `cwd` is the REPO
//!   ROOT rather than the subdir the helper ran in, which is the helper's
//!   `git rev-parse --path-format=absolute --git-common-dir` main-root
//!   resolution proven for real. With model/effort left empty (the default
//!   dispatch) the argv carries NEITHER flag, so the prompt sits directly after
//!   `--worktree <name>` — the exact shape the `--add-dir`-swallows-the-prompt
//!   hazard is ordered against.
//!
//! ## Hermeticity
//!
//! The stub `claude` is seeded via the process-global
//! `cx.set_global(ResolvedClaudePath(Some(stub)))` and `NICE_CLAUDE_OVERRIDE` is
//! REMOVED (so `is_override` stays `false` and the Nice-injected
//! `--session-id`/`--add-dir`/`--worktree`/prompt argv the legs assert is
//! actually emitted). The machine's real `claude` is NEVER spawned — the stub
//! only records its argv, then idles. `HOME` is a sandbox with no rc for the
//! driver's lifetime, so every login shell sources nothing. The installer writes
//! ONLY the injected scratch dirs (auto-removed on drop) — never the developer's
//! real `~/.claude` / `~/.nice`, and the helper the leg executes is the scratch
//! copy, never `~/.nice/nice-dispatch.sh`. The scratch repo is `git init`-ed
//! under the fixture root, so no real repository is touched.
//!
//! What this scenario deliberately does NOT prove: that the real Claude CLI
//! parses the launch line the way `dispatch_extra_args` assumes (the stub only
//! records argv) or that `claude --worktree` creates a worktree. That is the
//! post-merge interactive feel-check.
//!
//! `Gate::SelfReported`; registered BEFORE `multiwindow` (its `build_window_root`
//! only `register`s — no `WindowRegistry` close observer, so its window never
//! trips the quit-when-empty terminus `multiwindow` owns as the last gate).

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use gpui::{AnyWindowHandle, AsyncApp, Entity, WindowHandle};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_model::{TermWindow, TermWindowKind, Session};

use crate::app_shell::AppShellView;
use crate::pty_manager::ResolvedClaudePath;
use crate::skill_installer::DISPATCH_HELPER_FILENAME;
use crate::window_registry::WindowRegistry;
use crate::window_state::WindowState;

// -- timing ------------------------------------------------------------------

/// Poll cap for a routed model mutation (session creation) after a socket message —
/// the drain-task hop, on the real clock.
const ROUTE_POLLS: usize = 60;
/// Poll cap for the stub `claude` to record its argv after its window spawns.
const ARGV_POLLS: usize = 60;
/// Poll cap for the real helper subprocess to exit (it posts, then waits on
/// `nc -U -w 2` for the window's reply — which only arrives once the foreground
/// drain runs, so this MUST be polled, never blocked on).
const HELPER_POLLS: usize = 100;
/// Interval between polls (real wall-clock; the pty children and the helper
/// subprocess run on OS threads / processes).
const POLL_MS: u64 = 100;

/// The seeded dispatcher session's id / window id — the `NICE_TAB_ID` / `NICE_PANE_ID`
/// the helper leg exports, and the session that must stay active throughout.
const DISPATCHER_SESSION: &str = "dispatcher-tab";
const DISPATCHER_WINDOW: &str = "dispatcher-pane";

// -- fixture -----------------------------------------------------------------

/// The sandboxed fixture: a fake `$HOME` (no rc), the injected installer scratch
/// dirs, a `git init`-ed scratch repo the helper leg runs inside, an
/// argv-recording stub `claude`, and its capture dir.
struct Fixture {
    base: PathBuf,
    home: PathBuf,
    work: PathBuf,
    /// The injected skills PARENT the installer hangs every pair's dir off.
    skills_root: PathBuf,
    /// The injected `~/.nice` twin the helpers are installed into.
    helper_dir: PathBuf,
    /// The `git init`-ed scratch repo standing in for the main checkout.
    repo: PathBuf,
    /// A subdirectory of [`Fixture::repo`] the helper is RUN from, so the main
    /// root it posts can only have come from `--git-common-dir` (it differs from
    /// the helper's `$PWD`).
    repo_subdir: PathBuf,
    capture_dir: PathBuf,
    /// The prior `$HOME`, restored at teardown.
    prev_home: Option<String>,
}

impl Fixture {
    fn build() -> Result<Self> {
        let base = std::env::temp_dir().join(format!("nice-dispatch-{}", std::process::id()));
        std::fs::create_dir_all(&base).context("create fixture base")?;
        let base = base.canonicalize().context("canonicalize fixture base")?;

        let home = base.join("home");
        let work = base.join("work");
        // The injected installer dirs mirror the real layout under the scratch
        // root: `<base>/claude/skills` (the root every installed pair hangs off)
        // + `<base>/nice`. They are NOT pre-created — the installer
        // `create_dir_all`s them, matching the production path.
        let skills_root = base.join("claude").join("skills");
        let helper_dir = base.join("nice");
        let repo = base.join("repo");
        let repo_subdir = repo.join("crates").join("nice");
        let capture_dir = base.join("argv");
        let bin = base.join("bin");
        for d in [&home, &work, &repo_subdir, &capture_dir, &bin] {
            std::fs::create_dir_all(d).context("create fixture dir")?;
        }

        // A REAL git repo: the helper resolves the main checkout root from
        // `git rev-parse --path-format=absolute --git-common-dir`, so the leg
        // only means something against an actual repository. `HOME` is pinned to
        // the sandbox so the developer's git config cannot steer `init`.
        let status = Command::new("git")
            .arg("init")
            .arg("--quiet")
            .current_dir(&repo)
            .env("HOME", &home)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("run git init in the scratch repo")?;
        if !status.success() {
            anyhow::bail!("git init failed in the scratch repo ({status})");
        }

        // The stub `claude`: record its full argv (one arg per line) to a
        // per-invocation file in the capture dir, then block reading stdin so the
        // window stays alive until teardown. NEVER the machine's real claude
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
        // process-global (seeded in `open_dispatch_window`), and REMOVE any prior
        // scenario's `NICE_CLAUDE_OVERRIDE` so `is_override` stays false and the
        // Nice-injected flags emit (the argv legs depend on it).
        // SAFETY: single-threaded scenario setup, before any Claude window forks;
        // matches the existing `std::env::set_var`/`remove_var` seams.
        unsafe { std::env::remove_var("NICE_CLAUDE_OVERRIDE") };

        let prev_home = std::env::var("HOME").ok();
        Ok(Fixture {
            base,
            home,
            work,
            skills_root,
            helper_dir,
            repo,
            repo_subdir,
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

    fn repo_str(&self) -> String {
        self.repo.to_string_lossy().into_owned()
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

/// Open the `dispatch` window through the SHIPPED builder and spawn its driver
/// (self-reported gate). Seeds the stub-`claude` `ResolvedClaudePath` global and
/// sandboxes `HOME` for the driver's lifetime (restored at teardown).
pub fn open_dispatch_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let fixture = Fixture::build()?;
    let stub = fixture.stub_path();
    let home = fixture.home_str();
    let whandle: WindowHandle<AppShellView> = cx.update(|app| {
        // The shipped builder reads the process-global `SharedFontSettings` (via
        // the window host); `install_shortcuts` seeds it (idempotent).
        crate::keymap::install_shortcuts(app);
        // Seed the stub as the resolved claude binary (env override unset ⇒
        // `is_override` false ⇒ the full Nice argv reaches the stub).
        app.set_global(ResolvedClaudePath(Some(stub.clone())));
        // Sandbox HOME (no rc) for the whole driver; restored at teardown.
        // SAFETY: single-threaded setup; the Main window forks synchronously inside
        // `open_managed_window` under this HOME.
        unsafe { std::env::set_var("HOME", &home) };
        crate::app::open_managed_window(app)
    })?;
    let any: AnyWindowHandle = whandle.into();

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_dispatch(acx, whandle, fixture).await;
        eprintln!("[selftest] scenario 'dispatch': {}", report.detail);
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

async fn run_dispatch(
    cx: &mut AsyncApp,
    whandle: WindowHandle<AppShellView>,
    fixture: Fixture,
) -> CadenceReport {
    cx.update(|app| app.activate(true));
    settle(cx, 500).await;

    let mut failures: Vec<String> = Vec::new();

    // Resolve the shipped window's per-window state + its control socket path.
    let id = AnyWindowHandle::from(whandle).window_id();
    let Some(state) = cx.update(|app| WindowRegistry::state_for_window(app, id)) else {
        fixture.teardown();
        return CadenceReport::error(
            "dispatch: the shipped builder did not register the window's WindowState",
        );
    };
    let Some(socket_path) = state.update(cx, |s, _cx| s.control_socket_path()) else {
        fixture.teardown();
        return CadenceReport::error(
            "dispatch: the shipped window armed no control socket (no path to drive)",
        );
    };

    // The seeded dispatcher session: present, non-Terminals, owning the window the
    // helper names. Its own cwd is the fixture `work` dir — deliberately NOT the
    // repo root, so leg (a)'s payload-cwd assertion can tell the two apart.
    let work = fixture.work_str();
    seed_dispatcher_session(cx, &state, &work);

    // === (a) raw-socket dispatch → nested, unselected [DISPATCH] session ============
    let before_a = all_session_ids(cx, &state);
    let task_a = format!("{work}/briefs/dispatch-a.md");
    let reply_a = send_dispatch(
        cx,
        &socket_path,
        &fixture.repo_str(),
        "fix-drag-crash",
        &task_a,
        DISPATCHER_SESSION,
        DISPATCHER_WINDOW,
        "keep it on the branch",
        "claude-opus-4-8",
        "xhigh",
    )
    .await;
    if reply_a.as_deref().map(str::trim_end) != Some("ok") {
        failures.push(format!("(a) socket: expected reply 'ok', got {reply_a:?}"));
    }
    assert_dispatch_session(
        cx,
        &state,
        &before_a,
        "a",
        "[DISPATCH] fix-drag-crash",
        &fixture.repo_str(),
        &mut failures,
    )
    .await;
    match poll_argv_for(cx, &fixture, &task_a).await {
        None => failures.push(
            "(a) argv: the stub `claude` never recorded an argv naming the task file (the window \
             did not spawn, or is_override suppressed the flags)"
                .into(),
        ),
        Some(argv) => assert_dispatch_argv(
            &argv,
            &format!("{work}/briefs"),
            "fix-drag-crash",
            Some(("claude-opus-4-8", "xhigh")),
            &format!(
                "Read the dispatch task file at {task_a}, then start working on the task it \
                 describes. keep it on the branch"
            ),
            "a",
            &mut failures,
        ),
    }

    // === (b) the REAL installed helper, run inside the scratch repo =============
    real_helper_leg(cx, &state, &fixture, &socket_path, &mut failures).await;

    // === teardown: drop every session so no zsh / stub outlives the window ======
    state.update(cx, |s, _cx| s.teardown());
    settle(cx, 200).await;
    // Clear the stub from the process-global before its file is removed, so a
    // later scenario in an `all` sweep can't resolve a now-deleted path.
    cx.update(|app| app.set_global(ResolvedClaudePath(None)));
    fixture.teardown();

    build_report(failures)
}

/// Leg (b): install the pair into the INJECTED scratch dirs, then run THAT
/// installed `nice-dispatch.sh` as a real subprocess from a subdirectory of the
/// `git init`-ed scratch repo. Proves the shipped helper bytes actually post a
/// payload this build's parser accepts, and that the `cwd` they post is the
/// repo ROOT (the `--git-common-dir` resolution) rather than the helper's `$PWD`.
async fn real_helper_leg(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    fixture: &Fixture,
    socket_path: &str,
    failures: &mut Vec<String>,
) {
    crate::skill_installer::sync_with(true, &fixture.skills_root, &fixture.helper_dir);
    let helper = fixture.helper_dir.join(DISPATCH_HELPER_FILENAME);
    if !helper.exists() {
        failures.push(format!(
            "(b) helper: the installer laid down no {DISPATCH_HELPER_FILENAME} in the injected \
             helper dir (nothing to run)"
        ));
        return;
    }

    // The dispatcher-side skill writes the brief into the MAIN checkout's
    // `.claude/dispatch/`; do the same so `--add-dir` names a real directory.
    let dispatch_dir = fixture.repo.join(".claude").join("dispatch");
    if let Err(e) = std::fs::create_dir_all(&dispatch_dir) {
        failures.push(format!("(b) helper: cannot create the repo's .claude/dispatch: {e}"));
        return;
    }
    let task_file = dispatch_dir.join("sidebar-perf-20240315-143022.md");
    if let Err(e) = std::fs::write(&task_file, b"# brief\n\nMake the sidebar fast.\n") {
        failures.push(format!("(b) helper: cannot write the task brief: {e}"));
        return;
    }
    let task_file = task_file.to_string_lossy().into_owned();

    let before = all_session_ids(cx, state);
    let output = run_helper(
        cx,
        &helper,
        &[
            "sidebar-perf",
            &task_file,
            "report back when done",
            "", // model — a default dispatch inherits nothing
            "", // effort — likewise
        ],
        fixture,
        socket_path,
    )
    .await;
    match output {
        None => failures.push(
            "(b) helper: the installed nice-dispatch.sh never exited (it blocks on the window's \
             socket reply)"
                .into(),
        ),
        Some(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            if !out.status.success() {
                failures.push(format!(
                    "(b) helper: the installed nice-dispatch.sh exited {:?} (stdout {stdout:?}, \
                     stderr {stderr:?})",
                    out.status.code()
                ));
            }
            if !stdout.contains("dispatch session opening") {
                failures.push(format!(
                    "(b) helper: the helper must report the session is opening, got stdout {stdout:?} \
                     / stderr {stderr:?}"
                ));
            }
        }
    }

    // The session the REAL payload produced: nested, locked-title, unselected — and
    // its cwd is the repo ROOT, not the subdir the helper ran in.
    assert_dispatch_session(
        cx,
        state,
        &before,
        "b",
        "[DISPATCH] sidebar-perf",
        &fixture.repo_str(),
        failures,
    )
    .await;
    match poll_argv_for(cx, fixture, &task_file).await {
        None => failures.push(
            "(b) argv: the stub `claude` never recorded an argv naming the helper-posted task file"
                .into(),
        ),
        Some(argv) => assert_dispatch_argv(
            &argv,
            &dispatch_dir.to_string_lossy(),
            "sidebar-perf",
            None,
            &format!(
                "Read the dispatch task file at {task_file}, then start working on the task it \
                 describes. report back when done"
            ),
            "b",
            failures,
        ),
    }
}

fn build_report(failures: Vec<String>) -> CadenceReport {
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "dispatch OK: a raw-socket `dispatch` naming a seeded dispatcher session replied \
                     `ok` and opened a nested, UNSELECTED [DISPATCH]-titled session (locked title, \
                     parented under the dispatcher, cwd = the PAYLOAD cwd, its unrendered Claude \
                     window running) whose stub argv carried --session-id <v4> then --add-dir \
                     <brief dir> --worktree <name> --model --effort with the prompt last; and the \
                     INSTALLED nice-dispatch.sh, run for real from a subdirectory of a git \
                     init-ed scratch repo, exited 0, posted the repo ROOT as cwd (its \
                     --git-common-dir main-root resolution, not $PWD) and produced a second \
                     nested [DISPATCH] session whose argv omitted --model/--effort with the prompt \
                     still directly after --worktree."
                .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} dispatch assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}

// -- assertions --------------------------------------------------------------

/// Poll for the session a dispatch produced and assert the shape both legs share:
/// nested under the dispatcher with a LOCKED `[DISPATCH] …` title, spawned from
/// `want_cwd` (the payload cwd), the dispatcher still active, and the unselected
/// child's Claude window running.
async fn assert_dispatch_session(
    cx: &mut AsyncApp,
    state: &Entity<WindowState>,
    before: &[String],
    leg: &str,
    want_title: &str,
    want_cwd: &str,
    failures: &mut Vec<String>,
) {
    let Some(new_session) = poll_new_session(cx, state, before).await else {
        failures.push(format!("({leg}) session: the dispatch produced no new session in the model"));
        return;
    };
    let snap = state.update(cx, |s, _cx| {
        s.workspace.session_for(&new_session).map(|t| {
            (
                t.title.clone(),
                t.title_manually_set,
                t.parent_session_id.clone(),
                t.cwd.clone(),
                t.windows
                    .iter()
                    .any(|p| p.kind == TermWindowKind::Claude && p.is_claude_running),
            )
        })
    });
    match snap {
        None => failures.push(format!("({leg}) session: the new session vanished before assertion")),
        Some((title, locked, parent, cwd, claude_running)) => {
            if title != want_title {
                failures.push(format!("({leg}) session: title must be {want_title:?}, got {title:?}"));
            }
            if !locked {
                failures.push(format!(
                    "({leg}) session: the dispatch session's title must be LOCKED \
                     (title_manually_set == true)"
                ));
            }
            if parent.as_deref() != Some(DISPATCHER_SESSION) {
                failures.push(format!(
                    "({leg}) session: the dispatch session must nest under the dispatcher \
                     (parent_session_id == {DISPATCHER_SESSION:?}), got {parent:?}"
                ));
            }
            if cwd != want_cwd {
                failures.push(format!(
                    "({leg}) session: the dispatch must spawn from the PAYLOAD cwd {want_cwd:?} \
                     (never the dispatcher's own cwd), got {cwd:?}"
                ));
            }
            if !claude_running {
                failures.push(format!(
                    "({leg}) session: the unselected nested child must still have a RUNNING Claude \
                     window (its pty is owned by the session entity, not the view)"
                ));
            }
        }
    }

    // The dispatch opened in the BACKGROUND: selection never moved off the
    // dispatcher session.
    let active = state.update(cx, |s, _cx| s.workspace.active_session_id().map(str::to_string));
    if active.as_deref() != Some(DISPATCHER_SESSION) {
        failures.push(format!(
            "({leg}) no-focus: a dispatch must NOT select its session — the dispatcher \
             {DISPATCHER_SESSION:?} must still be active, got {active:?}"
        ));
    }
}

/// Assert the stub `claude` argv: `--session-id <v4 uuid>`, then the LOAD-BEARING
/// `--add-dir <add_dir>` immediately followed by `--worktree <worktree>` (the
/// single-token option that terminates `--add-dir`'s variadic list before it can
/// swallow the prompt), the optional `--model` / `--effort` per `want_flags`, and
/// `prompt` as the FINAL element. Token-based (tolerates an optional leading
/// `--settings <path>` should a prior scenario have left the theme gate on).
fn assert_dispatch_argv(
    argv: &[String],
    add_dir: &str,
    worktree: &str,
    want_flags: Option<(&str, &str)>,
    prompt: &str,
    leg: &str,
    failures: &mut Vec<String>,
) {
    match flag_value(argv, "--session-id") {
        Some(id) if is_v4_uuid(id) => {}
        Some(id) => failures.push(format!("({leg}) argv: --session-id value {id:?} is not a v4 uuid")),
        None => failures.push(format!("({leg}) argv: missing --session-id in {argv:?}")),
    }
    match argv.iter().position(|a| a == "--add-dir") {
        None => failures.push(format!(
            "({leg}) argv: missing --add-dir (the brief lives outside the child's worktree cwd) \
             in {argv:?}"
        )),
        Some(i) => {
            if argv.get(i + 1).map(String::as_str) != Some(add_dir) {
                failures.push(format!(
                    "({leg}) argv: --add-dir must name the brief's directory {add_dir:?}, got \
                     {:?}",
                    argv.get(i + 1)
                ));
            }
            // The variadic-terminator ordering: nothing may sit between
            // `--add-dir <dir>` and `--worktree <name>`, or `--add-dir` swallows
            // it as a second directory.
            if argv.get(i + 2).map(String::as_str) != Some("--worktree") {
                failures.push(format!(
                    "({leg}) argv: --worktree must IMMEDIATELY follow --add-dir's value (it \
                     terminates the variadic list), got {:?} in {argv:?}",
                    argv.get(i + 2)
                ));
            }
            if argv.get(i + 3).map(String::as_str) != Some(worktree) {
                failures.push(format!(
                    "({leg}) argv: --worktree must be {worktree:?}, got {:?}",
                    argv.get(i + 3)
                ));
            }
        }
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
            // A dispatch inherits NOTHING: an empty model/effort omits the flag
            // and the child runs on the user's configured defaults.
            if argv.iter().any(|a| a == "--model") {
                failures.push(format!(
                    "({leg}) argv: --model must be OMITTED for an empty model, got {argv:?}"
                ));
            }
            if argv.iter().any(|a| a == "--effort") {
                failures.push(format!(
                    "({leg}) argv: --effort must be OMITTED for an empty effort, got {argv:?}"
                ));
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

// -- the real helper subprocess ----------------------------------------------

/// Run the INSTALLED `nice-dispatch.sh` from a subdirectory of the scratch repo,
/// polling for its exit between settles. Never blocks the foreground: the helper
/// waits on the window's socket reply, which only the foreground drain can send,
/// so a blocking `wait()` here would deadlock the scenario.
async fn run_helper(
    cx: &mut AsyncApp,
    helper: &Path,
    args: &[&str],
    fixture: &Fixture,
    socket_path: &str,
) -> Option<std::process::Output> {
    let child = Command::new(helper)
        .args(args)
        .current_dir(&fixture.repo_subdir)
        .env("HOME", &fixture.home)
        .env("NICE_SOCKET", socket_path)
        .env("NICE_TAB_ID", DISPATCHER_SESSION)
        .env("NICE_PANE_ID", DISPATCHER_WINDOW)
        // Dispatch must NOT inherit the dispatcher's effort (the locked decision
        // the helper has no `CLAUDE_EFFORT` fallback for) — seed one so a
        // regression that re-added the fallback would surface as an `--effort`
        // flag leg (b) forbids.
        .env("CLAUDE_EFFORT", "xhigh")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[selftest] dispatch: cannot spawn the installed helper: {e}");
            return None;
        }
    };
    for _ in 0..HELPER_POLLS {
        settle(cx, POLL_MS).await;
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => continue,
            Err(_) => return None,
        }
    }
    let _ = child.kill();
    None
}

// -- raw-socket `dispatch` drive ---------------------------------------------

/// Drive a raw `dispatch` request over the control socket on a DEDICATED thread
/// (so the blocking read never wedges the foreground drain that answers it), then
/// poll its reply channel between settles. Returns the trimmed reply line (`None`
/// on timeout / no reply).
#[allow(clippy::too_many_arguments)]
async fn send_dispatch(
    cx: &mut AsyncApp,
    socket_path: &str,
    cwd: &str,
    worktree_name: &str,
    task_file: &str,
    session_id: &str,
    term_window_id: &str,
    instructions: &str,
    model: &str,
    effort: &str,
) -> Option<String> {
    let payload = dispatch_json(
        cwd,
        worktree_name,
        task_file,
        session_id,
        term_window_id,
        instructions,
        model,
        effort,
    );
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

/// Build the frozen `dispatch` NDJSON request line — the exact schema the
/// installed helper posts (leg (b) proves the two agree by driving the helper).
#[allow(clippy::too_many_arguments)]
fn dispatch_json(
    cwd: &str,
    worktree_name: &str,
    task_file: &str,
    session_id: &str,
    term_window_id: &str,
    instructions: &str,
    model: &str,
    effort: &str,
) -> String {
    format!(
        "{{\"action\":\"dispatch\",\"cwd\":\"{}\",\"worktreeName\":\"{}\",\"taskFile\":\"{}\",\"tabId\":\"{}\",\"paneId\":\"{}\",\"instructions\":\"{}\",\"model\":\"{}\",\"effort\":\"{}\"}}",
        json_escape(cwd),
        json_escape(worktree_name),
        json_escape(task_file),
        json_escape(session_id),
        json_escape(term_window_id),
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

fn all_session_ids(cx: &mut AsyncApp, state: &Entity<WindowState>) -> Vec<String> {
    state.update(cx, |s, _cx| {
        s.workspace
            .projects
            .iter()
            .flat_map(|p| p.sessions.iter().map(|t| t.id.clone()))
            .collect()
    })
}

/// Poll until a session id appears that was not in `before`, returning it.
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

/// Poll the capture dir for the argv file whose recorded prompt names
/// `task_file` (each leg uses a distinct file, so the match is unambiguous),
/// returning its recorded argv lines.
async fn poll_argv_for(
    cx: &mut AsyncApp,
    fixture: &Fixture,
    task_file: &str,
) -> Option<Vec<String>> {
    for _ in 0..ARGV_POLLS {
        // The stub records its argv on its own pty child process; settle (never
        // block the foreground) between reads.
        settle(cx, POLL_MS).await;
        if let Ok(entries) = std::fs::read_dir(&fixture.capture_dir) {
            for entry in entries.flatten() {
                if let Ok(contents) = std::fs::read_to_string(entry.path()) {
                    if contents.contains(task_file) {
                        let lines: Vec<String> = contents.lines().map(str::to_string).collect();
                        return Some(lines);
                    }
                }
            }
        }
    }
    None
}

/// Seed a model-only dispatcher Claude session into a fresh non-Terminals project —
/// present, non-Terminals, owning [`DISPATCHER_WINDOW`], so a dispatch resolves it
/// and nests under it. The window needs no live pty (resolution is model-only). Its
/// `cwd` is deliberately NOT the scratch repo: a dispatch must spawn from the
/// PAYLOAD cwd, and only a differing session cwd can prove that.
fn seed_dispatcher_session(cx: &mut AsyncApp, state: &Entity<WindowState>, cwd: &str) {
    state.update(cx, |s, _cx| {
        s.workspace.ensure_project("dispatcher-proj", "Dispatcher", cwd);
        let mut claude = TermWindow::new(DISPATCHER_WINDOW, "Claude", TermWindowKind::Claude);
        claude.is_claude_running = true;
        let mut session = Session::new(DISPATCHER_SESSION, "dispatcher", cwd);
        session.windows = vec![
            claude,
            TermWindow::new(format!("{DISPATCHER_SESSION}-t1"), "Terminal 1", TermWindowKind::Terminal),
        ];
        session.active_window_id = Some(DISPATCHER_WINDOW.to_string());
        session.next_terminal_index = 2;
        if let Some(pi) = s.workspace.projects.iter().position(|p| p.id == "dispatcher-proj") {
            s.workspace.projects[pi].sessions.push(session);
        }
        s.workspace.select_session(DISPATCHER_SESSION);
    });
}
