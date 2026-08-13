//! `compose-live` self-test scenario — Command Compose end-to-end against a
//! REAL zsh under the real shell injection: trigger bytes → ZLE keymap match →
//! async widget → (fake) `claude -p` → spinner paints → the line buffer is
//! REPLACED in place → nothing executes until a genuine Enter.
//!
//! This is the only place the full app↔shell chain is proven live; the seams it
//! composes are each unit-covered elsewhere:
//!
//! * the Nice-side gate + byte routing — `WindowState::compose_route`'s truth
//!   table + the itests keymap dispatch (⌘↩ → the window-scoped handler);
//! * the widget's translate/strip/conf functions — `shell::zsh`'s real-zsh
//!   e2e tests (stdin piping, conf flags, fence strip, parser agreement);
//! * the trigger byte/text agreement — `shell::zsh`'s static pins.
//!
//! So THIS scenario drives the pty with [`COMPOSE_TRIGGER_SEQ`] exactly as
//! `dispatch_command_compose`'s `Trigger` route does (standing up a full
//! `WindowState`/`PtyManager` here would re-prove what those tests pin),
//! and asserts what only a live ZLE can show: the sequence matches the keymap
//! atomically, the spinner line paints while the fake claude thinks, the
//! buffer is replaced without executing, and the user's own Enter runs it.
//! Drives the pty directly — no CGEvents, so no Accessibility grant needed
//! (the `niceties-drop` precedent).
//!
//! The busy-window leg asserts the GATE SIGNALS on the live session (a foreground
//! child flips `has_foreground_child`; a plain prompt never has kitty
//! super-forwarding) rather than writing the trigger into a busy pty — the
//! whole point of the Nice-side gate is that those bytes are never sent then.
//!
//! `compose-live-bash` is the same drive against a REAL modern bash and the
//! `bind -x` handler (design §10's "one added bash smoke scenario"). It shares
//! [`run_compose_live`] and differs only in what it spawns; see
//! [`open_compose_live_bash_window`] for why the shell selection is entirely
//! in-code in both scenarios and `NICE_SHELL` plays no part.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use gpui::{
    div, prelude::*, AnyWindowHandle, AsyncApp, Context, Entity, IntoElement, Render,
    SharedString, Window,
};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_term_core::{SpawnSpec, DEFAULT_SCROLLBACK_LINES};
use nice_term_view::{
    FontSettings, TerminalMetrics, TerminalSessionHandle, TerminalTheme, TerminalView,
};
use nice_theme::AccentPreset;

use crate::shell::bash::{find_compose_bash, BashProfile};
use crate::shell::{InjectPaths, ShellProfile, SpawnCtx, COMPOSE_TRIGGER_SEQ};

const ROWS: u16 = 24;
const COLS: u16 = 80;

/// The per-shell words in [`run_compose_live`]'s messages. The drive itself is
/// identical for both scenarios — same bytes in, same three things asserted —
/// so the shell differs only in what gets spawned and what a failure is called.
struct Dialect {
    /// Names the shell in failure messages.
    shell: &'static str,
    /// What the trigger bytes reach on the shell side, for the pass detail.
    handler: &'static str,
}

const ZSH: Dialect = Dialect {
    shell: "zsh",
    handler: "ZLE widget",
};

const BASH: Dialect = Dialect {
    shell: "bash",
    handler: "bind -x handler",
};

/// The English the "user" types at the prompt (no shell-command lookalikes, so
/// the not-executed assertion is unambiguous).
const REQUEST: &str = "say the compose marker please";
/// What the fake claude composes. `COMPOSED_OK` appears with `echo ` in front
/// while it sits un-executed in the buffer; alone on a line only after Enter.
const COMPOSED: &str = "echo COMPOSED_OK";

/// The animated scenario host (the `input_live` pattern): re-paints every frame
/// so the grid tracks the pty while the driver polls.
struct ComposeTermView {
    terminal: Entity<TerminalView>,
}

impl Render for ComposeTermView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        nice_harness::frame::stamp();
        window.request_animation_frame();
        div().size_full().child(self.terminal.clone())
    }
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

fn grid_text(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>) -> String {
    handle.update(cx, |h, _| h.session().grid_lines().join("\n"))
}

/// Poll `pred` over the grid until true or ~`budget_ms` elapses.
async fn poll_grid(
    cx: &mut AsyncApp,
    handle: &Entity<TerminalSessionHandle>,
    budget_ms: u64,
    pred: impl Fn(&str) -> bool,
) -> bool {
    let mut elapsed = 0;
    loop {
        if pred(&grid_text(cx, handle)) {
            return true;
        }
        if elapsed >= budget_ms {
            return false;
        }
        settle(cx, 50).await;
        elapsed += 50;
    }
}

/// Scratch layout: `<base>/home` (empty `$HOME`, no user rc), the REAL
/// injection stubs in `<base>/zdotdir`, a fake `claude` in `<base>/bin`, and a
/// production-shaped conf at `<base>/compose.json`. Returns
/// `(base, home, path)` where `path` is the fake-bin-first `$PATH`.
fn prepare_scratch() -> Result<(PathBuf, PathBuf, String)> {
    use std::os::unix::fs::PermissionsExt;
    let base = std::env::temp_dir().join(format!("nice-compose-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    std::fs::create_dir_all(&home)?;
    crate::shell::zsh::write_stubs(&base.join("zdotdir"))?;

    let bin = base.join("bin");
    std::fs::create_dir_all(&bin)?;
    // Thinks ~1s (so the spinner is observably animated), then replies.
    let fake = format!("#!/bin/zsh\nsleep 1\nprint -rn -- '{COMPOSED}'\n");
    let claude = bin.join("claude");
    std::fs::write(&claude, fake)?;
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755))?;

    std::fs::write(
        base.join("compose.json"),
        r##"{"accent":"#7a94db","model":"sonnet","effort":"medium"}"##,
    )?;

    let path = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display());
    Ok((base, home, path))
}

/// Open the `compose-live` scenario window and spawn the driver (self-reported).
pub fn open_compose_live_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let (base, home, path) = prepare_scratch()?;

    let spec = SpawnSpec::shell(home.to_string_lossy().into_owned())
        .with_env(vec![
            ("HOME".to_string(), home.to_string_lossy().into_owned()),
            ("PATH".to_string(), path),
            (
                "ZDOTDIR".to_string(),
                base.join("zdotdir").to_string_lossy().into_owned(),
            ),
            ("NICE_USER_ZDOTDIR".to_string(), String::new()),
            (
                "NICE_COMPOSE_CONF".to_string(),
                base.join("compose.json").to_string_lossy().into_owned(),
            ),
        ])
        .with_size(ROWS, COLS);

    let handle = TerminalSessionHandle::spawn(cx, spec, DEFAULT_SCROLLBACK_LINES)?;
    let font = cx.new(|_cx| {
        FontSettings::fixed(
            SharedString::from("Menlo"),
            13.0,
            TerminalMetrics::new(8.0, 16.0),
        )
    });
    let terminal = cx.new(|cx| {
        TerminalView::new(
            handle.clone(),
            TerminalTheme::nice_default_dark(),
            AccentPreset::Terracotta.color(),
            font,
            cx,
        )
    });

    let window = cx.open_window(crate::app::window_options(), {
        let terminal = terminal.clone();
        move |_window, cx| cx.new(|_cx| ComposeTermView { terminal })
    })?;
    let window: AnyWindowHandle = window.into();
    crate::app::install_present_kick(&handle, window, cx);

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_compose_live(acx, handle, &ZSH).await;
        eprintln!("[selftest] scenario 'compose-live': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(window)
}

/// Scratch layout for the bash variant: `<base>/home` doubling as the scratch
/// `$HOME`, the REAL `nice.bashrc` written by `BashProfile` into
/// `<base>/shellrc`, a fake `claude` in `<base>/home/bin`, and the same
/// production-shaped conf. Returns `(home, rc_paths, path, conf)`.
///
/// The `.bash_profile` is not optional: the injected rc emulates the login
/// chain, `/etc/profile` runs `path_helper`, and `path_helper` REBUILDS `PATH`
/// from `/etc/paths` — demoting the fixture `bin/` behind `/usr/bin` and hiding
/// the fake `claude`. Re-prepending it from the profile is the same workaround
/// `shell::bash::hermetic::ScratchHome::write_path_restoring_bash_profile`
/// documents for the test suite.
fn prepare_bash_scratch(bash: &Path) -> Result<(PathBuf, InjectPaths, String, PathBuf)> {
    use std::os::unix::fs::PermissionsExt;
    let base = std::env::temp_dir().join(format!("nice-compose-live-bash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin)?;

    let paths = BashProfile::new(bash.to_string_lossy().into_owned())
        .write_rc_files(&base.join("shellrc"))?;

    // Thinks ~1s (so the status line is observably painted), then replies.
    let claude = bin.join("claude");
    std::fs::write(&claude, format!("#!/bin/bash\nsleep 1\nprintf '%s' '{COMPOSED}'\n"))?;
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755))?;

    std::fs::write(
        home.join(".bash_profile"),
        format!("export PATH=\"{}:$PATH\"\n", bin.display()),
    )?;
    let conf = base.join("compose.json");
    std::fs::write(
        &conf,
        r##"{"accent":"#7a94db","model":"sonnet","effort":"medium"}"##,
    )?;

    let path = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display());
    Ok((home, paths, path, conf))
}

/// Open the `compose-live-bash` scenario window and spawn the driver — the same
/// drive as [`open_compose_live_window`] against a real bash ≥ 4.3 and the
/// `bind -x` handler.
///
/// **`NICE_SHELL` plays no part in either scenario.** It is read in exactly one
/// place, the live resolution chain in `shell::resolve`, which runs only from
/// the shell-inject bootstrap — and self-tests run under `run_selftest`, which
/// never bootstraps a `ShellRuntime`. What pins each scenario to its shell is
/// entirely in-code: the zsh one writes zsh stubs and takes `SpawnSpec`'s
/// zsh-defaulting argv, and this one writes `nice.bashrc` through `BashProfile`
/// and hands over that profile's own `spawn_argv` explicitly.
///
/// Self-skips (reporting a pass with a "skipped" detail) when the machine has
/// no bash ≥ 4.3 — the same [`find_compose_bash`] search the real-pty e2e uses.
pub fn open_compose_live_bash_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let Some(bash) = find_compose_bash() else {
        // The verdict slot was cleared by the driver immediately before this
        // call, so posting from here (rather than from a spawned task) is safe
        // and keeps the skip from costing a window's worth of settle time.
        let window = cx.open_window(crate::app::window_options(), |_window, cx| {
            cx.new(|_cx| SkipView)
        })?;
        let detail = "skipped: no bash >= 4.3 on this machine (set NICE_TEST_BASH, or \
                      install a homebrew bash, to run it)"
            .to_string();
        eprintln!("[selftest] scenario 'compose-live-bash': {detail}");
        nice_harness::selftest::report_gate(CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail,
        });
        return Ok(window.into());
    };

    let (home, paths, path, conf) = prepare_bash_scratch(&bash)?;
    let profile = BashProfile::new(bash.to_string_lossy().into_owned());

    let spec = SpawnSpec::shell(home.to_string_lossy().into_owned())
        .with_argv(profile.spawn_argv(&SpawnCtx {
            inject: Some(&paths),
            command: None,
        }))
        .with_env(vec![
            ("HOME".to_string(), home.to_string_lossy().into_owned()),
            ("PATH".to_string(), path),
            (
                "NICE_COMPOSE_CONF".to_string(),
                conf.to_string_lossy().into_owned(),
            ),
        ])
        .with_size(ROWS, COLS);

    let handle = TerminalSessionHandle::spawn(cx, spec, DEFAULT_SCROLLBACK_LINES)?;
    let font = cx.new(|_cx| {
        FontSettings::fixed(
            SharedString::from("Menlo"),
            13.0,
            TerminalMetrics::new(8.0, 16.0),
        )
    });
    let terminal = cx.new(|cx| {
        TerminalView::new(
            handle.clone(),
            TerminalTheme::nice_default_dark(),
            AccentPreset::Terracotta.color(),
            font,
            cx,
        )
    });

    let window = cx.open_window(crate::app::window_options(), {
        let terminal = terminal.clone();
        move |_window, cx| cx.new(|_cx| ComposeTermView { terminal })
    })?;
    let window: AnyWindowHandle = window.into();
    crate::app::install_present_kick(&handle, window, cx);

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_compose_live(acx, handle, &BASH).await;
        eprintln!("[selftest] scenario 'compose-live-bash': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(window)
}

/// The window a self-skipped scenario still has to open — it paints nothing but
/// keeps the driver's activation preamble satisfied.
struct SkipView;

impl Render for SkipView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        nice_harness::frame::stamp();
        window.request_animation_frame();
        div().size_full()
    }
}

async fn run_compose_live(
    cx: &mut AsyncApp,
    handle: Entity<TerminalSessionHandle>,
    dialect: &Dialect,
) -> CadenceReport {
    let fail = |detail: String| CadenceReport {
        passed: false,
        stats: IntervalStats::default(),
        detail,
    };

    // 1. Wait for the injected shell to reach its prompt.
    if !poll_grid(cx, &handle, 8000, |t| t.chars().any(|c| !c.is_whitespace())).await {
        return fail(format!(
            "{} never printed a prompt (grid stayed blank)",
            dialect.shell
        ));
    }
    settle(cx, 300).await;

    // Gate-signal sanity at the idle prompt: exactly the state in which
    // `compose_route` picks `Trigger`.
    let fg = handle.update(cx, |h, _| h.has_foreground_child());
    let kitty = handle.update(cx, |h, _| h.session().kitty_forwards_super());
    if fg || kitty {
        return fail(format!(
            "idle prompt reports fg_child={fg} kitty_super={kitty}; expected false/false"
        ));
    }

    // 2. Type the English request (pty bytes == typing at the line editor) and
    //    see it echo.
    let _ = handle.update(cx, |h, _| h.session().write_input(REQUEST.as_bytes()));
    if !poll_grid(cx, &handle, 4000, |t| t.contains(REQUEST)).await {
        return fail(format!("typed request never echoed. Grid:\n{}", grid_text(cx, &handle)));
    }

    // 3. Fire the compose trigger — the exact bytes `dispatch_command_compose`'s
    //    `Trigger` route writes.
    let _ = handle.update(cx, |h, _| h.session().write_input(COMPOSE_TRIGGER_SEQ));

    // 4. The spinner line paints under the prompt while the fake claude thinks.
    if !poll_grid(cx, &handle, 4000, |t| t.contains("Composing")).await {
        return fail(format!(
            "status line never painted. Grid:\n{}",
            grid_text(cx, &handle)
        ));
    }

    // 5. The buffer is REPLACED in place: composed command present, spinner and
    //    the English gone — and NOT executed (`COMPOSED_OK` appears only behind
    //    its `echo`).
    let replaced = poll_grid(cx, &handle, 8000, |t| {
        t.contains(COMPOSED) && !t.contains("Composing") && !t.contains(REQUEST)
    })
    .await;
    if !replaced {
        return fail(format!(
            "buffer was not replaced by the composed command. Grid:\n{}",
            grid_text(cx, &handle)
        ));
    }
    let executed_early = grid_text(cx, &handle)
        .lines()
        .any(|l| l.contains("COMPOSED_OK") && !l.contains("echo"));
    if executed_early {
        return fail(format!(
            "compose EXECUTED without the user's Enter. Grid:\n{}",
            grid_text(cx, &handle)
        ));
    }

    // 6. The user's own Enter runs it: bare `COMPOSED_OK` output appears.
    let _ = handle.update(cx, |h, _| h.session().write_input(b"\r"));
    let ran = poll_grid(cx, &handle, 4000, |t| {
        t.lines().any(|l| l.contains("COMPOSED_OK") && !l.contains("echo"))
    })
    .await;
    if !ran {
        return fail(format!(
            "Enter did not run the composed command. Grid:\n{}",
            grid_text(cx, &handle)
        ));
    }

    // 7. Busy leg: a foreground child flips the gate signal Nice consults, so
    //    `compose_route` would pick Noop (legacy shell) — asserted on the live
    //    session; the routing itself is the unit-tested truth table.
    let _ = handle.update(cx, |h, _| h.session().write_input(b"sleep 30\n"));
    let mut busy = false;
    for _ in 0..40 {
        settle(cx, 100).await;
        if handle.update(cx, |h, _| h.has_foreground_child()) {
            busy = true;
            break;
        }
    }
    if !busy {
        return fail("foreground child (sleep 30) never flipped has_foreground_child".into());
    }
    let kitty_busy = handle.update(cx, |h, _| h.session().kitty_forwards_super());
    if kitty_busy {
        return fail("a plain busy shell unexpectedly reports kitty super-forwarding".into());
    }
    // Interrupt the sleep so the window returns to a prompt before teardown.
    let _ = handle.update(cx, |h, _| h.session().write_input(b"\x03"));

    CadenceReport {
        passed: true,
        stats: IntervalStats::default(),
        detail: format!(
            "trigger → {handler} → fake claude → status painted → buffer replaced with \
             '{COMPOSED}' (not executed) → user Enter ran it; busy-window gate signal verified \
             (fg_child flips, kitty_super stays off)",
            handler = dialect.handler,
        ),
    }
}
