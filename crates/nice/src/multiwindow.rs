//! `multiwindow` self-test scenario — the R12 multi-window + shortcut-dispatch
//! gate (Validation §2–§5), driven end-to-end against the **real**
//! `WindowRegistry` / `WindowState` / `keymap` with **real CGEvents** posted to
//! nice-rs's own pid (`crate::platform`, the same edge the R5 `input-*` / R7
//! `niceties-zoom` scenarios use).
//!
//! Where the in-process `nice-itests` `multiwindow` cases prove the routing /
//! isolation / peek *logic* deterministically over mirrors, this proves the shipped
//! wiring on **real `NSWindow`s**: two windows tracked in the process-wide registry,
//! ⌘N opening a second isolated window, the 13 keymap actions dispatching through
//! GPUI's action system, and — the one place R12 touches tranche-1 code — the
//! terminal **pass-through contract** (a matched chord is consumed and leaks zero
//! bytes into the pty; an unmatched key reaches the pty byte-identically).
//!
//! ## What it drives (all against a capture-tee window A + a ⌘N-opened window B)
//!
//! 1. **⌘N opens a second window** — the registry count and the real
//!    `App::windows()` count both step 1 → 2 (Validation §3).
//! 2. **⌘T routes to the key window B** — with B activated, ⌘T adds a pane to B's
//!    `WindowState` model only; A's model signature is unchanged (§3).
//! 3. **Font fan-out (§2)** — ⌘= grows the one process-level `FontSettings` every
//!    window observes, and leaks **zero** bytes into the focused capture-tee pty.
//! 4. **Pass-through differential (§2)** — a plain `x` arrives at the pty as `x`;
//!    ⌘⌥↓ changes the sidebar (the active tab cycles) and leaks **zero** capture
//!    bytes.
//! 5. **Live peek (§5)** — with A's sidebar collapsed, ⌘⌥↓ floats the peek; a
//!    modifiers-release clears it (the window-level `on_modifiers_changed` observer).
//! 6. **Close / deregister / fallback (§3)** — closing B deregisters it (count
//!    drops, real `NSWindow` count drops) and a window-scoped action then falls
//!    back to the surviving window A.
//!
//! Self-reported ([`Gate::SelfReported`]): the pass criterion is registry/model/pty
//! state, not cadence. Accessibility (TCC) is preflighted and a missing grant FAILs
//! loudly (a silently-dropped CGEvent would make every chord a no-op). Registered
//! **last** in `selftest_scenarios`: it installs the real `WindowRegistry` whose
//! close observer quits when the registry empties, so it must be the final scenario
//! in the `all` suite (the harness closes window A after it, emptying the registry).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gpui::{
    div, prelude::*, AnyWindowHandle, AsyncApp, Capslock, Context, Entity, IntoElement, Modifiers,
    ModifiersChangedEvent, PlatformInput, Render, Window,
};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_term_core::{SpawnSpec, DEFAULT_SCROLLBACK_LINES};
use nice_term_view::{
    FontSettings, TerminalSessionHandle, TerminalTheme, TerminalView,
};
use nice_theme::AccentPreset;

use crate::platform;
use crate::window_registry::WindowRegistry;
use crate::window_state::WindowState;

// -- fixed geometry ---------------------------------------------------------

const ROWS: u16 = 24;
const COLS: u16 = 80;

// macOS virtual keycodes (`CGKeyCode`) the driver posts.
const KC_N: u16 = 45; // ⌘N — New Window
const KC_T: u16 = 17; // ⌘T — new terminal pane
const KC_B: u16 = 11; // ⌘B — toggle sidebar
const KC_DOWN: u16 = 125; // ⌘⌥↓ — next sidebar tab
const KC_EQUAL: u16 = 24; // ⌘= — increase font
const KC_ZERO: u16 = 29; // ⌘0 — reset font
const KC_X: u16 = 7; // plain x — the pass-through control char

/// Accessibility-grant remediation (shared wording with the other CGEvent
/// scenarios): without the TCC grant `CGEventPostToPid` is silently dropped, so
/// every injected chord is a no-op and the scenario can never pass.
const ACCESSIBILITY_REMEDIATION: &str = "\
Accessibility (TCC) grant missing: AXIsProcessTrusted() == false, so \
CGEventPostToPid is SILENTLY DROPPED and no injected chord can reach the window. \
Fix: System Settings → Privacy & Security → Accessibility → enable the process \
hosting this run. If it shows ON but this persists, the grant is STALE — remove \
it with '-' and re-add it, then re-run. Verify: swift -e 'import \
ApplicationServices; print(AXIsProcessTrusted())'";

/// Window A's root: the live capture-tee [`TerminalView`] plus the window-level
/// peek-clear observer, mirroring the shipped `WindowChromeView`. Requests the next
/// animation frame each render so the window keeps painting (and re-registers its
/// input handler) while the driver posts events.
struct MultiWindowRoot {
    terminal: Entity<TerminalView>,
}

impl Render for MultiWindowRoot {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        nice_harness::frame::stamp();
        window.request_animation_frame();
        div()
            .size_full()
            // The real peek-clear observer (Swift's flagsChanged monitor analog):
            // end the key window's peek once the shortcut's modifiers all release.
            .on_modifiers_changed(|event, _window, cx| {
                crate::keymap::on_window_modifiers_changed(event, cx)
            })
            .child(self.terminal.clone())
    }
}

/// Open the `multiwindow` scenario window (window A: a capture-tee session in a
/// registry-tracked managed window) and spawn the CGEvent driver + assertions
/// (self-reported gate).
pub fn open_multiwindow_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let base = std::env::temp_dir().join(format!("nice-rs-multiwindow-{}", std::process::id()));
    std::fs::create_dir_all(&base)?;
    let cap_path = base.join("capture.bin");
    let base_s = base.to_string_lossy().to_string();
    let cap_s = cap_path.to_string_lossy().to_string();

    // Capture-tee child (the input-live pattern): raw mode, then `tee` copies the
    // view's pty writes verbatim into the capture file so the pass-through /
    // zero-leak assertions can read exactly what reached the pty.
    let inner = format!("stty raw -echo; exec tee {cap_s}");
    let spec = SpawnSpec::command(format!("sh -c '{inner}'"), base_s.clone())
        .with_env(vec![("ZDOTDIR".to_string(), base_s.clone())])
        .with_size(ROWS, COLS);

    let handle = TerminalSessionHandle::spawn(cx, spec, DEFAULT_SCROLLBACK_LINES)?;

    // Install the real app wiring, exactly as the shipped `run` does (all idempotent
    // enough to coexist with the other scenarios in the suite):
    //   * the process-wide WindowRegistry + its close observer,
    //   * the ⌘N / File ▸ New Window command (its handler opens a real managed
    //     window through `open_managed_window`, which registers it),
    //   * the 13-action keymap + the hoisted shared FontSettings.
    cx.update(|app| {
        WindowRegistry::install(app);
        crate::app::install_new_window_command(app);
        crate::keymap::install_shortcuts(app);
    });

    let theme = TerminalTheme::nice_default_dark();
    let accent = AccentPreset::Terracotta.color();

    let window = cx.open_window(crate::app::window_options(), {
        let handle = handle.clone();
        let cwd = base_s.clone();
        move |window, cx| {
            // Read the shared, process-level font entity every window observes (the
            // fan-out target), matching the shipped window builder.
            let font = crate::keymap::shared_font_settings(cx);
            let terminal = cx.new(|cx| {
                let mut v = TerminalView::new(handle, theme, accent, font, cx);
                v.set_keycode_probe(Arc::new(platform::current_event_keycode));
                v
            });

            // Window A's per-window state, seeded with two extra terminal tabs so a
            // ⌘⌥↓ sidebar-tab cycle genuinely moves the active tab (a fresh window
            // has a single navigable tab, which cannot cycle).
            let state = cx.new(|_cx| WindowState::new(cwd));
            state.update(cx, |s, _cx| {
                s.sidebar_actions.create_terminal_tab(&mut s.model);
                s.sidebar_actions.create_terminal_tab(&mut s.model);
                // Re-seed the active tab back to Main so the first cycle is a clean
                // Main → next step.
                s.model.select_tab(nice_model::TabModel::MAIN_TERMINAL_TAB_ID);
            });

            // Register A + track its activation for the registry MRU — the same
            // wiring the shipped `build_window_root` installs (mirrored inline
            // because that builder also mounts the chrome band, which A does not).
            let id = window.window_handle().window_id();
            WindowRegistry::register(cx, id, state.clone());
            state
                .update(cx, |_s, cx| {
                    cx.observe_window_activation(window, |_s, window, cx| {
                        if window.is_window_active() {
                            WindowRegistry::note_active(cx, window.window_handle().window_id());
                        }
                    })
                    .detach();
                });

            cx.new(|_cx| MultiWindowRoot { terminal })
        }
    })?;
    let window: AnyWindowHandle = window.into();
    crate::app::install_present_kick(&handle, window, cx);

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_multiwindow(acx, window, handle, cap_path).await;
        eprintln!("[selftest] scenario 'multiwindow': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(window)
}

// -- small async / io helpers ----------------------------------------------

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

/// Post one key tap (with `flags`) to our own pid, then yield so AppKit dispatches
/// it into the key window before the next event.
async fn tap(cx: &mut AsyncApp, pid: i32, keycode: u16, flags: u64, unicode: Option<&str>) {
    platform::post_key_tap(pid, keycode, flags, unicode);
    settle(cx, 120).await;
}

fn cap_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Bytes appended to the capture file since offset `start`.
fn cap_since(path: &Path, start: u64) -> Vec<u8> {
    match std::fs::read(path) {
        Ok(all) if (all.len() as u64) >= start => all[start as usize..].to_vec(),
        Ok(all) => all,
        Err(_) => Vec::new(),
    }
}

/// Whether the capture file contains `needle` anywhere.
fn cap_contains(path: &Path, needle: &[u8]) -> bool {
    let all = cap_since(path, 0);
    all.windows(needle.len()).any(|w| w == needle)
}

fn esc(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        match b {
            0x1b => out.push_str("\\e"),
            0x0d => out.push_str("\\r"),
            0x0a => out.push_str("\\n"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

/// A compact signature of a window's tab tree (`active` + per-navigable-tab pane
/// counts) — the "model hash" the routing check asserts is unchanged on window A.
fn model_sig(cx: &mut AsyncApp, state: &Entity<WindowState>) -> String {
    state.update(cx, |s, _cx| {
        let active = s.model.active_tab_id().unwrap_or("").to_string();
        let tabs: Vec<String> = s
            .model
            .navigable_sidebar_tab_ids()
            .iter()
            .map(|id| format!("{id}:{}", s.model.tab_for(id).map_or(0, |t| t.panes.len())))
            .collect();
        format!("active={active};{}", tabs.join(","))
    })
}

/// The active tab's pane count for the window state (used for the ⌘T routing +
/// A-unchanged assertions).
fn active_pane_count(cx: &mut AsyncApp, state: &Entity<WindowState>) -> usize {
    state.update(cx, |s, _cx| {
        s.model
            .active_tab_id()
            .and_then(|id| s.model.tab_for(id))
            .map_or(0, |t| t.panes.len())
    })
}

fn active_tab_id(cx: &mut AsyncApp, state: &Entity<WindowState>) -> Option<String> {
    state.update(cx, |s, _cx| s.model.active_tab_id().map(str::to_string))
}

fn sidebar_collapsed(cx: &mut AsyncApp, state: &Entity<WindowState>) -> bool {
    state.update(cx, |s, _cx| s.sidebar.collapsed())
}

fn sidebar_peeking(cx: &mut AsyncApp, state: &Entity<WindowState>) -> bool {
    state.update(cx, |s, _cx| s.sidebar.peeking())
}

fn registry_count(cx: &mut AsyncApp) -> usize {
    cx.update(|app| WindowRegistry::count(app))
}

fn windows_len(cx: &mut AsyncApp) -> usize {
    cx.update(|app| app.windows().len())
}

fn font_px(cx: &mut AsyncApp) -> f32 {
    let font: Entity<FontSettings> = cx.update(|app| crate::keymap::shared_font_settings(app));
    font.update(cx, |f, _| f.px())
}

/// The state registered for window `id`, via the registry's id-keyed lookup.
fn state_for(cx: &mut AsyncApp, id: gpui::WindowId) -> Option<Entity<WindowState>> {
    cx.update(|app| WindowRegistry::state_for_window(app, id))
}

/// This window's persisted session id (R18: the persisted window id in
/// `sessions.json`).
fn session_id_of(cx: &mut AsyncApp, state: &Entity<WindowState>) -> String {
    state.update(cx, |s, _cx| s.session_id().to_string())
}

/// Whether `id` is a lowercased RFC-4122 v4 UUID (the shape R18 mints for a fresh
/// / ⌘N window id — retiring the old `win-<seq>` stand-in): `8-4-4-4-12` hex with
/// version nibble `4` and variant nibble in `[89ab]`.
fn is_uuid_v4(id: &str) -> bool {
    let b = id.as_bytes();
    if b.len() != 36 {
        return false;
    }
    for (i, c) in b.iter().enumerate() {
        match i {
            8 | 13 | 18 | 23 => {
                if *c != b'-' {
                    return false;
                }
            }
            14 => {
                if *c != b'4' {
                    return false;
                }
            }
            19 => {
                if !matches!(c, b'8' | b'9' | b'a' | b'b') {
                    return false;
                }
            }
            _ => {
                if !c.is_ascii_hexdigit() || c.is_ascii_uppercase() {
                    return false;
                }
            }
        }
    }
    true
}

async fn run_multiwindow(
    cx: &mut AsyncApp,
    window_a: AnyWindowHandle,
    handle: Entity<TerminalSessionHandle>,
    cap_path: PathBuf,
) -> CadenceReport {
    // Frontmost/key + painted once (registers the input handler) before events.
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 700).await;

    if !platform::accessibility_trusted() {
        return CadenceReport::error(ACCESSIBILITY_REMEDIATION.to_string());
    }

    // Wait for the capture-tee child to come up: write a probe straight to A's pty
    // and poll the capture file for it (raw mode is live once it echoes back).
    let mut ready = false;
    for _ in 0..40 {
        let _ = handle.update(cx, |h, _| h.session().write_input(b"__ready__"));
        settle(cx, 100).await;
        if cap_contains(&cap_path, b"__ready__") {
            ready = true;
            break;
        }
    }
    if !ready {
        return CadenceReport::error(
            "multiwindow: capture-tee child never echoed its readiness probe".to_string(),
        );
    }

    let a_id = window_a.window_id();
    let pid = std::process::id() as i32;
    let mut failures: Vec<String> = Vec::new();

    let Some(a_state) = state_for(cx, a_id) else {
        return CadenceReport::error(
            "multiwindow: window A is not registered in the WindowRegistry".to_string(),
        );
    };

    // Re-assert frontmost/key right before the first chord so CGEvents route.
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 250).await;

    // === §3 — ⌘N opens a second, registry-tracked window =====================
    let reg_before = registry_count(cx);
    let win_before = windows_len(cx);
    tap(cx, pid, KC_N, platform::FLAG_COMMAND, None).await;
    settle(cx, 500).await;
    let reg_after = registry_count(cx);
    let win_after = windows_len(cx);
    if reg_after != reg_before + 1 {
        failures.push(format!(
            "⌘N did not register a second window: registry count {reg_before} → {reg_after}"
        ));
    }
    if win_after != win_before + 1 {
        failures.push(format!(
            "⌘N did not open a second real NSWindow: App::windows() {win_before} → {win_after}"
        ));
    }

    // Identify window B (the new handle) for the routing + close checks.
    let b_handle = cx
        .update(|app| app.windows().into_iter().find(|w| w.window_id() != a_id));

    // === R18 (L2) — ⌘N mints a UUID window id, distinct from A's =============
    // The ⌘N window's `session_id` IS its persisted `sessions.json` id, so it must
    // be a real minted UUID (never the retired `win-<seq>` stand-in, which
    // restarts at 1 each launch and would collide across relaunches) and distinct
    // from A's.
    if let Some(b_handle) = b_handle {
        if let (Some(a_st), Some(b_st)) =
            (state_for(cx, a_id), state_for(cx, b_handle.window_id()))
        {
            let a_sid = session_id_of(cx, &a_st);
            let b_sid = session_id_of(cx, &b_st);
            if !is_uuid_v4(&a_sid) {
                failures.push(format!("window A's session id is not a minted UUID: '{a_sid}'"));
            }
            if !is_uuid_v4(&b_sid) {
                failures.push(format!(
                    "⌘N window B's session id is not a minted UUID: '{b_sid}'"
                ));
            }
            if a_sid == b_sid {
                failures.push(format!(
                    "⌘N minted the same window id as A ('{a_sid}') — ids must be unique per window"
                ));
            }
        }
    }

    // === §3 — ⌘T routes to the key window B; A's model is unchanged ==========
    if let Some(b_handle) = b_handle {
        let b_id = b_handle.window_id();
        let a_sig_before = model_sig(cx, &a_state);

        // Drive B frontmost/key, then confirm it actually became key before posting
        // ⌘T (so a routing miss reports as "B never keyed", not a confusing count).
        let _ = b_handle.update(cx, |_v, window, _app| window.activate_window());
        settle(cx, 400).await;
        let b_is_key = cx.update(|app| app.active_window().map(|w| w.window_id())) == Some(b_id);

        if !b_is_key {
            failures.push(
                "⌘T routing: window B never became the key window, so the chord could not be \
                 routed to it (a live activation limitation, not a routing bug)"
                    .to_string(),
            );
        } else if let Some(b_state) = state_for(cx, b_id) {
            let b_panes_before = active_pane_count(cx, &b_state);
            tap(cx, pid, KC_T, platform::FLAG_COMMAND, None).await;
            settle(cx, 300).await;
            let b_panes_after = active_pane_count(cx, &b_state);
            if b_panes_after != b_panes_before + 1 {
                failures.push(format!(
                    "⌘T to key window B did not add a pane to B: active-tab pane count \
                     {b_panes_before} → {b_panes_after}"
                ));
            }
            let a_sig_after = model_sig(cx, &a_state);
            if a_sig_after != a_sig_before {
                failures.push(format!(
                    "⌘T to B mutated A's model: '{a_sig_before}' → '{a_sig_after}' (isolation \
                     broken)"
                ));
            }
        } else {
            failures.push("⌘T routing: window B is not registered".to_string());
        }
    } else {
        failures.push("could not identify the ⌘N-opened window B".to_string());
    }

    // === Re-key window A for the pty-observing checks ========================
    let _ = window_a.update(cx, |_v, window, _app| window.activate_window());
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 400).await;

    // === §2 — ⌘= grows the shared font (fan-out) + zero pty bytes ============
    {
        let px0 = font_px(cx);
        let start = cap_len(&cap_path);
        tap(cx, pid, KC_EQUAL, platform::FLAG_COMMAND, None).await;
        settle(cx, 200).await;
        let px1 = font_px(cx);
        let leaked = cap_since(&cap_path, start);
        if !(px1 > px0) {
            failures.push(format!(
                "⌘= did not grow the shared font (every window observes it): {px0} → {px1} pt"
            ));
        }
        if !leaked.is_empty() {
            failures.push(format!(
                "⌘= leaked {} byte(s) into the pty ('{}') — a matched chord must consume",
                leaked.len(),
                esc(&leaked)
            ));
        }
        // Restore the baseline so no font state leaks to a later scenario.
        tap(cx, pid, KC_ZERO, platform::FLAG_COMMAND, None).await;
        settle(cx, 200).await;
    }

    // === §2 — pass-through differential: plain x, then ⌘⌥↓ ===================
    {
        // A plain key falls through the keymap to the pty byte-identically.
        let start = cap_len(&cap_path);
        tap(cx, pid, KC_X, 0, Some("x")).await;
        settle(cx, 200).await;
        let got = cap_since(&cap_path, start);
        if got != b"x" {
            failures.push(format!(
                "plain 'x' did not reach the pty as 'x': got '{}'",
                esc(&got)
            ));
        }

        // A matched chord (⌘⌥↓) changes the sidebar (active tab cycles) and leaks
        // zero bytes.
        let tab_before = active_tab_id(cx, &a_state);
        let start = cap_len(&cap_path);
        tap(cx, pid, KC_DOWN, platform::FLAG_COMMAND | platform::FLAG_OPTION, None).await;
        settle(cx, 200).await;
        let tab_after = active_tab_id(cx, &a_state);
        let leaked = cap_since(&cap_path, start);
        if tab_after == tab_before {
            failures.push(format!(
                "⌘⌥↓ did not cycle the active sidebar tab (still {tab_before:?}) — the chord did \
                 not route to A"
            ));
        }
        if !leaked.is_empty() {
            failures.push(format!(
                "⌘⌥↓ leaked {} byte(s) into the pty ('{}') — a matched chord must consume",
                leaked.len(),
                esc(&leaked)
            ));
        }
    }

    // === §5 — live peek: collapse, ⌘⌥↓ floats it, release clears it ==========
    {
        // Collapse A's sidebar via ⌘B (assert it took, so the peek trigger's
        // collapsed precondition holds).
        if !sidebar_collapsed(cx, &a_state) {
            tap(cx, pid, KC_B, platform::FLAG_COMMAND, None).await;
            settle(cx, 200).await;
        }
        if !sidebar_collapsed(cx, &a_state) {
            failures.push("⌘B did not collapse A's sidebar (peek precondition)".to_string());
        } else {
            // ⌘⌥↓ on the collapsed sidebar floats the peek.
            tap(cx, pid, KC_DOWN, platform::FLAG_COMMAND | platform::FLAG_OPTION, None).await;
            settle(cx, 200).await;
            if !sidebar_peeking(cx, &a_state) {
                failures.push("⌘⌥↓ on the collapsed sidebar did not float the peek".to_string());
            }
            // Releasing the modifiers clears it (the window-level observer). A real
            // per-pid flagsChanged is not synthesizable via CGEventPostToPid, so the
            // release is driven as a real ModifiersChangedEvent through GPUI's own
            // dispatch — the same on_modifiers_changed path the flagsChanged monitor
            // feeds.
            dispatch_modifiers_release(cx, window_a);
            settle(cx, 150).await;
            if sidebar_peeking(cx, &a_state) {
                failures.push(
                    "releasing the modifiers did not clear the peek (on_modifiers_changed)"
                        .to_string(),
                );
            }
        }
    }

    // === §3 — close B: deregister + fallback to the surviving window A =======
    // R18: `remove_window()` is a PROGRAMMATIC close — by design it bypasses the
    // `on_window_should_close` confirmation gate (programmatic ≠ a user red-button
    // / ⌘W close), so B closes straight through with no modal even though the gate
    // is now wired on every window. B carries only a live terminal here, so the
    // gate would otherwise confirm; that it doesn't is the invariant this leg pins.
    if let Some(b_handle) = b_handle {
        let reg_before = registry_count(cx);
        let win_before = windows_len(cx);
        let _ = b_handle.update(cx, |_v, window, _app| window.remove_window());
        settle(cx, 400).await;
        let reg_after = registry_count(cx);
        let win_after = windows_len(cx);
        if reg_after != reg_before - 1 {
            failures.push(format!(
                "closing B did not deregister it: registry count {reg_before} → {reg_after}"
            ));
        }
        if win_after != win_before - 1 {
            failures.push(format!(
                "closing B did not drop a real NSWindow: App::windows() {win_before} → {win_after}"
            ));
        }

        // A window-scoped action now falls back to the surviving window A.
        let _ = window_a.update(cx, |_v, window, _app| window.activate_window());
        let _ = cx.update(|app| app.activate(true));
        settle(cx, 300).await;
        let collapsed_before = sidebar_collapsed(cx, &a_state);
        tap(cx, pid, KC_B, platform::FLAG_COMMAND, None).await;
        settle(cx, 200).await;
        if sidebar_collapsed(cx, &a_state) == collapsed_before {
            failures.push(
                "with B closed, ⌘B did not fall back to the surviving window A (its sidebar did \
                 not toggle)"
                    .to_string(),
            );
        }
    }

    build_report(failures)
}

/// Drive an all-modifiers-released event into `window` through GPUI's real
/// dispatch, so the root's `on_modifiers_changed` observer runs (the peek-clear
/// path). The per-pid flagsChanged a CGEvent would produce is not synthesizable, so
/// this exercises the same handler directly.
fn dispatch_modifiers_release(cx: &mut AsyncApp, window: AnyWindowHandle) {
    let event = ModifiersChangedEvent {
        modifiers: Modifiers::default(),
        capslock: Capslock { on: false },
    };
    let _ = window.update(cx, |_root, window, cx| {
        window.dispatch_event(PlatformInput::ModifiersChanged(event), cx);
    });
}

fn build_report(failures: Vec<String>) -> CadenceReport {
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "multi-window OK: ⌘N opened + registered a second window, ⌘T routed to the \
                     key window (A unchanged), ⌘= fanned out the font with zero pty leak, plain x \
                     passed through byte-identically, ⌘⌥↓ cycled the sidebar with zero leak, the \
                     collapsed peek set + cleared, and closing B deregistered + fell back to A"
                .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} multiwindow assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}
