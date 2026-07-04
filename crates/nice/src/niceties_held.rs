//! `niceties-held` self-test scenario — the T10 held-pane UX (R7 Validation §5).
//!
//! A pane running `sh -c 'echo FINAL; exit 3'` exits non-zero, so the R3
//! classification holds it. The scenario asserts the whole held-pane contract end
//! to end over a real session:
//!
//! 1. **the pane is held** — `is_held()` latches after the non-zero exit;
//! 2. **output stays readable** — `FINAL` is still in the grid;
//! 3. **the dim footer is in the buffer** — the ported `[Process exited (status 3)]`
//!    line was fed into the held term;
//! 4. **input is inert** — a real CGEvent keystroke changes nothing (grid
//!    unchanged, still held, no crash): the dead pty is never written and the key
//!    never falls through to AppKit's beep;
//! 5. **dismiss respawns a fresh shell** — the dismiss seam frees the held term
//!    and a fresh `zsh -il` takes its place (grid no longer holds `FINAL` / the
//!    footer, and a new prompt appears), the only path that frees the term.
//!
//! The dismiss is driven through the public seam (deterministic, like
//! `niceties-drop`); the click / Enter affordances wire to the same seam for real
//! users. The inert-typing check posts a real CGEvent, so it preflights the
//! Accessibility (TCC) grant and FAILs loudly if it is missing (a silently-dropped
//! event would make the check vacuous). Self-reported gate: the pass criterion is
//! these state + grid assertions, not frame cadence.

use std::time::Duration;

use anyhow::Result;
use gpui::{
    div, prelude::*, AnyWindowHandle, AsyncApp, Context, Entity, IntoElement, Render, SharedString,
    Window,
};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_term_core::{SpawnSpec, DEFAULT_SCROLLBACK_LINES};
use nice_term_view::{
    FontSettings, TerminalMetrics, TerminalSessionHandle, TerminalTheme, TerminalView,
};
use nice_theme::AccentPreset;

use crate::platform;

// -- fixed geometry (font resolution / zoom is covered by niceties-zoom) -----

const ROWS: u16 = 24;
const COLS: u16 = 80;
const FONT_FAMILY: &str = "Menlo";
const FONT_PX: f32 = 13.0;
const CELL_W: f32 = 8.0;
const CELL_H: f32 = 16.0;

/// macOS virtual keycode for `A` (`kVK_ANSI_A`) — the inert keystroke posted at a
/// held pane. Plain (no modifiers), unicode "a".
const KC_A: u16 = 0;

/// Accessibility-grant remediation (shared wording with the other CGEvent
/// scenarios): without the TCC grant `CGEventPostToPid` is silently dropped, so
/// the inert-typing check could never observe a real keystroke.
const ACCESSIBILITY_REMEDIATION: &str = "\
Accessibility (TCC) grant missing: AXIsProcessTrusted() == false, so \
CGEventPostToPid is SILENTLY DROPPED and no injected keystroke can reach the \
held pane. Fix: System Settings → Privacy & Security → Accessibility → enable \
the process hosting this run. If it shows ON but this persists, the grant is \
STALE — remove it with '-' and re-add it, then re-run.";

/// The animated container hosting the live [`TerminalView`] (RAF each render so it
/// keeps painting the held grid / the dismiss affordance; frame stamp for the
/// harness's per-scenario reset).
struct HeldTermView {
    terminal: Entity<TerminalView>,
}

impl Render for HeldTermView {
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

/// The child's grid as one newline-joined string.
fn grid_text(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>) -> String {
    handle.update(cx, |h, _| h.session().grid_lines().join("\n"))
}

/// Open the `niceties-held` scenario window (the non-zero-exit pane) and spawn the
/// held-pane assertions (self-reported gate).
pub fn open_niceties_held_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let base = std::env::temp_dir().join(format!("nice-rs-niceties-held-{}", std::process::id()));
    std::fs::create_dir_all(&base)?;
    let base_s = base.to_string_lossy().to_string();
    // A pane that prints then exits non-zero → the R3 classification holds it.
    let spec = SpawnSpec::command("sh -c 'echo FINAL; exit 3'".to_string(), base_s.clone())
        .with_env(vec![("ZDOTDIR".to_string(), base_s)])
        .with_size(ROWS, COLS);

    let handle = TerminalSessionHandle::spawn(cx, spec, DEFAULT_SCROLLBACK_LINES)?;
    let theme = TerminalTheme::nice_default_dark();
    let accent = AccentPreset::Terracotta.color();
    let font = cx.new(|_cx| {
        FontSettings::fixed(
            SharedString::from(FONT_FAMILY),
            FONT_PX,
            TerminalMetrics::new(CELL_W, CELL_H),
        )
    });
    let terminal = {
        let handle = handle.clone();
        cx.new(move |cx| {
            let mut v = TerminalView::new(handle, theme, accent, font, cx);
            // Wire the keyCode side-channel so the injected CGEvent is decoded the
            // same way the shipped app decodes real keystrokes.
            v.set_keycode_probe(std::sync::Arc::new(platform::current_event_keycode));
            v
        })
    };

    let window = cx.open_window(crate::app::window_options(), {
        let terminal = terminal.clone();
        move |_window, cx| cx.new(|_cx| HeldTermView { terminal })
    })?;
    let window: AnyWindowHandle = window.into();
    crate::app::install_present_kick(&handle, window, cx);

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_niceties_held(acx, handle, terminal).await;
        eprintln!("[selftest] scenario 'niceties-held': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(window)
}

async fn run_niceties_held(
    cx: &mut AsyncApp,
    handle: Entity<TerminalSessionHandle>,
    terminal: Entity<TerminalView>,
) -> CadenceReport {
    // Frontmost/key + painted once (registers the input handler) before events.
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 700).await;

    if !platform::accessibility_trusted() {
        return CadenceReport::error(ACCESSIBILITY_REMEDIATION.to_string());
    }

    let mut failures: Vec<String> = Vec::new();

    // --- Wait for the non-zero exit → held --------------------------------
    let mut held = false;
    for _ in 0..40 {
        settle(cx, 150).await;
        if terminal.update(cx, |v, _| v.is_held()) {
            held = true;
            break;
        }
    }
    if !held {
        return CadenceReport::error(
            "niceties-held: the pane never entered the held state after `exit 3` — the R3 \
             Exited{held} event did not reach the view"
                .to_string(),
        );
    }

    // --- Output + footer readable -----------------------------------------
    let grid = grid_text(cx, &handle);
    if !grid.contains("FINAL") {
        failures.push(format!(
            "held grid does not still show the process output `FINAL`:\n{grid}"
        ));
    }
    if !grid.contains("exited (status 3)") {
        failures.push(format!(
            "held grid is missing the dim in-buffer exit footer `[… exited (status 3)]`:\n{grid}"
        ));
    }

    // --- Input is inert: a real keystroke changes nothing -----------------
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 200).await;
    let before = grid_text(cx, &handle);
    let pid = std::process::id() as i32;
    platform::post_key_tap(pid, KC_A, 0, Some("a"));
    settle(cx, 250).await;
    let after = grid_text(cx, &handle);
    if after != before {
        failures.push(format!(
            "typing at a held pane changed the grid (input should be inert)\n  before:\n{before}\n\
             \n  after:\n{after}"
        ));
    }
    if !terminal.update(cx, |v, _| v.is_held()) {
        failures.push("a non-dismiss keystroke un-held the pane (only ⏎ / dismiss should)".into());
    }

    // --- Dismiss → a fresh shell replaces the held pane -------------------
    terminal.update(cx, |v, cx| v.dismiss_held(cx));
    if terminal.update(cx, |v, _| v.is_held()) {
        failures.push("dismiss_held did not clear the held state".into());
    }
    // Poll for the fresh shell: a new (empty) term, then a zsh prompt — so the grid
    // no longer holds the old FINAL / footer and is non-empty again.
    let mut fresh = false;
    for _ in 0..40 {
        settle(cx, 150).await;
        let g = grid_text(cx, &handle);
        let has_prompt = g.chars().any(|c| !c.is_whitespace());
        if has_prompt && !g.contains("FINAL") && !g.contains("exited (status 3)") {
            fresh = true;
            break;
        }
    }
    if !fresh {
        let g = grid_text(cx, &handle);
        failures.push(format!(
            "after dismiss no fresh shell appeared (expected a new prompt with no `FINAL` / \
             footer):\n{g}"
        ));
    }

    build_report(failures)
}

fn build_report(failures: Vec<String>) -> CadenceReport {
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail:
                "held pane OK: `exit 3` held the pane (FINAL + dim `[… exited (status 3)]` footer \
                 readable), a real keystroke was inert (grid unchanged, still held, no crash), and \
                 dismiss respawned a fresh shell (the only path that frees the held term)."
                    .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} niceties-held assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
