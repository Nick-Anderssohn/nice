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
//! * the widget's translate/strip/conf functions — `shell_inject`'s real-zsh
//!   e2e tests (stdin piping, conf flags, fence strip, parser agreement);
//! * the trigger byte/text agreement — `shell_inject`'s static pins.
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

use std::path::PathBuf;
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

use crate::shell_inject::COMPOSE_TRIGGER_SEQ;

const ROWS: u16 = 24;
const COLS: u16 = 80;

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
    crate::shell_inject::write_stubs(&base.join("zdotdir"))?;

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
        let report = run_compose_live(acx, handle).await;
        eprintln!("[selftest] scenario 'compose-live': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(window)
}

async fn run_compose_live(
    cx: &mut AsyncApp,
    handle: Entity<TerminalSessionHandle>,
) -> CadenceReport {
    let fail = |detail: String| CadenceReport {
        passed: false,
        stats: IntervalStats::default(),
        detail,
    };

    // 1. Wait for the injected zsh to reach its prompt.
    if !poll_grid(cx, &handle, 8000, |t| t.chars().any(|c| !c.is_whitespace())).await {
        return fail("zsh never printed a prompt (grid stayed blank)".into());
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

    // 2. Type the English request (pty bytes == typing at ZLE) and see it echo.
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
            "spinner line never painted. Grid:\n{}",
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
            "trigger → ZLE widget → fake claude → spinner painted → buffer replaced with \
             '{COMPOSED}' (not executed) → user Enter ran it; busy-window gate signal verified \
             (fg_child flips, kitty_super stays off)"
        ),
    }
}
