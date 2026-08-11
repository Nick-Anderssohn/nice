//! `input-live` / `input-shell` self-test scenarios — the R5 live input path
//! driven by **real CGEvents** posted to nice's own pid (`crate::platform`),
//! asserting byte-exact pty receipt through the whole edge (CGEvent → AppKit →
//! gpui → `TerminalView` → encoder / IME → pty).
//!
//! The pure encoders are covered headlessly by `nice-term-input`'s `cargo test`;
//! this is the end-to-end half the plan's Validation §2–§6 calls for, the part a
//! unit test cannot reach because it needs a frontmost, focused window and the
//! macOS event pipeline.
//!
//! ## `input-live` — byte-exact typed path + IME anchor + IME go/no-go probe
//!
//! One capture-tee session (`sh -c 'stty raw -echo; exec tee <cap>'`): the child
//! copies everything the view sends to the pty verbatim into a capture file (raw
//! mode, no line discipline, no cooked-mode signals) **and** echoes it back so
//! the terminal core still tracks output — which is how a DECSET the harness
//! injects reaches the parser. The driver posts real CGEvents and asserts the
//! bytes appended to the capture file match the expected VT sequences exactly:
//!
//! 1. plain ASCII (rides the IME `insertText` path → pty as data);
//! 2. ⌘V paste with DECSET 2004 **off** (raw) then **on** (bracketed);
//! 3. arrow keys (legacy `ESC[A/B/C/D`);
//! 4. the G1 **item-4 candidate anchor**, asserted programmatically: park the
//!    grid cursor mid-grid (CUP), drive a composition through the real
//!    `TermInputHandler` (the OS-IME `setMarkedText` analog), and assert
//!    `bounds_for_range` returns a rect at the grid-cursor cell (never `None` —
//!    the zed#46055 failure mode);
//! 5. the IME **go/no-go probe** (TIS → Pinyin): if synthetic composition
//!    engages, assert items 1–3 + 5 mechanically; if it does not (the plan flags
//!    this as UNPROVEN), **do not fail-loop** — record a DEFERRED HUMAN PASS and
//!    still pass on the headless state-machine tests + the live typed path +
//!    item 4. The user's keyboard input source is **always** restored.
//!
//! ## `input-shell` — real-shell CGEvent sanity (Validation §5)
//!
//! A real `zsh -il` session (user rc suppressed via an empty `ZDOTDIR`): the
//! driver types a marker `echo` command entirely via CGEvents and asserts the
//! grid shows both the echoed command and its output, proving the whole path
//! reaches a real login shell and its output round-trips back to the grid.
//!
//! ## `scrollback-keys` / `keybind-scheme` — grant-free keystroke scenarios
//!
//! Both drive `Window::dispatch_keystroke` instead of CGEvents (the exact path
//! an OS key event takes AFTER the platform hop), so neither needs an
//! Accessibility grant: `scrollback-keys` is Phase 0's keyboard-scrollback gate,
//! `keybind-scheme` is Phase 1's held-`⌃⌘` scheme gate.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use gpui::{
    div, point, prelude::*, px, AnyWindowHandle, AsyncApp, Bounds, ClipboardItem, Context, Entity,
    InputHandler, IntoElement, Render, SharedString, Window,
};

use nice_harness::frame::{CadenceReport, IntervalStats};
use nice_term_core::{SpawnSpec, DEFAULT_SCROLLBACK_LINES};
use nice_term_view::{
    grid_top_y, FontSettings, TerminalMetrics, TerminalSessionHandle, TerminalTheme, TerminalView,
    TermInputHandler,
};
use nice_theme::AccentPreset;

use crate::platform;

// -- fixed geometry (font resolution / zoom is R7) --------------------------

const ROWS: u16 = 24;
const COLS: u16 = 80;
const FONT_FAMILY: &str = "Menlo";
const FONT_PX: f32 = 13.0;
const CELL_W: f32 = 8.0;
const CELL_H: f32 = 16.0;

/// Grid cell the item-4 anchor test parks the cursor on (0-indexed). Set by the
/// CUP `ESC[15;30H` below — 1-indexed row 15 / col 30 → 0-indexed (14, 29).
/// Mid-grid, not a corner, so the anchor genuinely tracks the cursor.
const ANCHOR_ROW: usize = 14;
const ANCHOR_COL: usize = 29;

// macOS virtual keycodes (`CGKeyCode`) used by the drivers.
const KC_V: u16 = 9;
const KC_RETURN: u16 = 36;
const KC_DELETE: u16 = 51; // Backspace (kVK_Delete)
const KC_UP: u16 = 126;
const KC_DOWN: u16 = 125;
const KC_LEFT: u16 = 123;
const KC_RIGHT: u16 = 124;
const KC_N: u16 = 45;
const KC_I: u16 = 34;

/// The Accessibility-grant remediation shown when `AXIsProcessTrusted()` is false
/// (from `baseline/ACCESSIBILITY-GRANT.md`). The live scenarios FAIL loudly with
/// this rather than silently skipping the CGEvent half.
const ACCESSIBILITY_REMEDIATION: &str = "\
Accessibility (TCC) grant missing: AXIsProcessTrusted() == false, so \
CGEventPostToPid is SILENTLY DROPPED and no injected keystroke can reach the \
window. Fix: System Settings → Privacy & Security → Accessibility → enable the \
process hosting this run (normally the terminal app). If it shows ON but this \
persists, the grant is STALE — remove it with '-' and re-add it, then re-run. \
Verify: swift -e 'import ApplicationServices; print(AXIsProcessTrusted())'";

/// The animated container hosting the live [`TerminalView`]: it requests the next
/// animation frame every render so the element re-paints (and re-registers the
/// platform input handler) continuously while the driver posts events, and stamps
/// a frame so the harness's per-scenario reset stays consistent. The view owns
/// focus + caret state.
struct InputTermView {
    terminal: Entity<TerminalView>,
}

impl Render for InputTermView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        nice_harness::frame::stamp();
        window.request_animation_frame();
        div().size_full().child(self.terminal.clone())
    }
}

/// Create the per-scenario temp dir (reused as an empty `ZDOTDIR` so no user rc
/// pollutes a real-shell grid) and return it.
fn prepare_dir(tag: &str) -> Result<PathBuf> {
    let base = std::env::temp_dir().join(format!("nice-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&base)?;
    Ok(base)
}

/// Build a live [`TerminalView`] entity over `handle` with the keyCode
/// side-channel wired (matching the shipped window), so the encoder behaves
/// exactly as in production.
fn make_view(handle: Entity<TerminalSessionHandle>, cx: &mut AsyncApp) -> Entity<TerminalView> {
    let theme = TerminalTheme::nice_default_dark();
    let accent = AccentPreset::Terracotta.color();
    // Fixed-metrics font state (Menlo/13px/8×16): these scenarios assert byte-exact
    // pty receipt + the IME anchor geometry at a known pitch, not font resolution
    // / zoom (which the niceties-zoom scenario covers).
    let font = cx.new(|_cx| {
        FontSettings::fixed(
            SharedString::from(FONT_FAMILY),
            FONT_PX,
            TerminalMetrics::new(CELL_W, CELL_H),
        )
    });
    cx.new(|cx| {
        let mut v = TerminalView::new(handle, theme, accent, font, cx);
        v.set_keycode_probe(Arc::new(platform::current_event_keycode));
        v
    })
}

// -- small async / io helpers ----------------------------------------------

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

/// Post one key tap to our own pid, then yield the run loop so AppKit dispatches
/// it into the window before the next event.
async fn tap(cx: &mut AsyncApp, pid: i32, keycode: u16, flags: u64, unicode: Option<&str>) {
    platform::post_key_tap(pid, keycode, flags, unicode);
    settle(cx, 45).await;
}

/// Type an ASCII string as individual key taps (each char inserted via its
/// unicode override, so it is keyboard-layout independent — it rides the IME
/// `insertText` path to the pty exactly like real typing).
async fn type_ascii(cx: &mut AsyncApp, pid: i32, s: &str) {
    for ch in s.chars() {
        let mut buf = [0u8; 4];
        let one = ch.encode_utf8(&mut buf);
        tap(cx, pid, ascii_keycode(ch), 0, Some(one)).await;
    }
}

/// A plausible virtual keycode for an ASCII char. The char is layout-independent
/// via the unicode override, so this only feeds the keyCode side-channel; an
/// unmapped char falls back to `0` (harmless — printables never hit the encoder).
fn ascii_keycode(c: char) -> u16 {
    match c.to_ascii_lowercase() {
        'a' => 0, 'b' => 11, 'c' => 8, 'd' => 2, 'e' => 14, 'f' => 3, 'g' => 5, 'h' => 4,
        'i' => 34, 'j' => 38, 'k' => 40, 'l' => 37, 'm' => 46, 'n' => 45, 'o' => 31, 'p' => 35,
        'q' => 12, 'r' => 15, 's' => 1, 't' => 17, 'u' => 32, 'v' => 9, 'w' => 13, 'x' => 7,
        'y' => 16, 'z' => 6, ' ' => 49, '0' => 29, '1' => 18, '2' => 19, '3' => 20, '4' => 21,
        '5' => 23, '6' => 22, '7' => 26, '8' => 28, '9' => 25, _ => 0,
    }
}

/// Write bytes to the child (pty). For the capture-tee session this reaches
/// `tee` (which echoes it to the parser + copies it to the capture file).
/// (A strong `Entity::update` under an `AsyncApp` returns the closure's value
/// directly — the entity is alive for this task's lifetime.)
fn write_child(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>, bytes: &[u8]) -> Result<()> {
    handle
        .update(cx, |h, _| h.session().write_input(bytes))
        .map_err(|e| anyhow!("pty write failed: {e}"))
}

fn bracketed_active(cx: &mut AsyncApp, handle: &Entity<TerminalSessionHandle>) -> bool {
    handle.update(cx, |h, _| h.session().bracketed_paste_active())
}

fn cap_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Bytes appended to the capture file since offset `start`.
fn cap_since(path: &Path, start: u64) -> Vec<u8> {
    match std::fs::read(path) {
        Ok(all) if (all.len() as u64) >= start => all[start as usize..].to_vec(),
        Ok(all) => all, // truncated unexpectedly; return what's there for the diff
        Err(_) => Vec::new(),
    }
}

/// Render bytes with non-printables escaped, for readable byte diffs.
fn esc(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        match b {
            0x1b => out.push_str("\\e"),
            0x0d => out.push_str("\\r"),
            0x0a => out.push_str("\\n"),
            0x09 => out.push_str("\\t"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

/// Assert `got == want`, pushing a byte-diff into `failures` on mismatch.
fn expect_bytes(failures: &mut Vec<String>, label: &str, want: &[u8], got: &[u8]) {
    if got != want {
        failures.push(format!(
            "{label}: pty bytes mismatch\n    want: \"{}\"\n    got:  \"{}\"",
            esc(want),
            esc(got)
        ));
    }
}

// ===========================================================================
// input-live
// ===========================================================================

/// Open the `input-live` scenario window (capture-tee session) and spawn the
/// CGEvent driver + assertions (self-reported gate).
pub fn open_input_live_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let base = prepare_dir("input-live")?;
    let cap_path = base.join("capture.bin");
    let base_s = base.to_string_lossy().to_string();
    let cap_s = cap_path.to_string_lossy().to_string();

    // Capture-tee child: raw mode (no line discipline / echo / signals), then
    // `tee` copies stdin verbatim into the capture file AND echoes it to the pty
    // so the core still tracks output (how an injected DECSET reaches the parser).
    let inner = format!("stty raw -echo; exec tee {cap_s}");
    let spec = SpawnSpec::command(format!("sh -c '{inner}'"), base_s.clone())
        .with_env(vec![("ZDOTDIR".to_string(), base_s)])
        .with_size(ROWS, COLS);

    let handle = TerminalSessionHandle::spawn(cx, spec, DEFAULT_SCROLLBACK_LINES)?;
    let terminal = make_view(handle.clone(), cx);

    let window = cx.open_window(crate::app::window_options(), {
        let terminal = terminal.clone();
        move |_window, cx| cx.new(|_cx| InputTermView { terminal })
    })?;
    let window: AnyWindowHandle = window.into();
    crate::app::install_present_kick(&handle, window, cx);

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_input_live(acx, window, handle, terminal, cap_path).await;
        eprintln!("[selftest] scenario 'input-live': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(window)
}

async fn run_input_live(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    handle: Entity<TerminalSessionHandle>,
    terminal: Entity<TerminalView>,
    cap_path: PathBuf,
) -> CadenceReport {
    // Self-activate + settle so the window is frontmost/key and has painted once
    // (registering the input handler) before any event is posted.
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 700).await;

    // Accessibility preflight — FAIL loudly (never silently skip the live half).
    if !platform::accessibility_trusted() {
        return CadenceReport::error(ACCESSIBILITY_REMEDIATION.to_string());
    }

    // Re-assert frontmost/key immediately before the first keystroke so the
    // CGEvents route to the window even if activation lagged the initial paint.
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 250).await;

    let pid = std::process::id() as i32;
    let mut failures: Vec<String> = Vec::new();
    let mut deferred: Vec<String> = Vec::new();

    // --- Phase 1: plain ASCII rides insertText → pty as data ----------------
    {
        let start = cap_len(&cap_path);
        type_ascii(cx, pid, "abc").await;
        settle(cx, 200).await;
        expect_bytes(&mut failures, "plain-ascii", b"abc", &cap_since(&cap_path, start));
    }

    // --- Phase 2: ⌘V paste, DECSET 2004 OFF → raw -------------------------
    {
        if bracketed_active(cx, &handle) {
            failures.push("paste-off: DECSET 2004 unexpectedly active at session start".into());
        }
        cx.update(|app| app.write_to_clipboard(ClipboardItem::new_string("hello".to_string())));
        settle(cx, 120).await;
        let start = cap_len(&cap_path);
        tap(cx, pid, KC_V, platform::FLAG_COMMAND, None).await;
        settle(cx, 200).await;
        expect_bytes(&mut failures, "paste-off", b"hello", &cap_since(&cap_path, start));
    }

    // --- Phase 3: ⌘V paste, DECSET 2004 ON → bracketed --------------------
    {
        // The child echoes this DECSET back so the parser sets the mode bit.
        if let Err(e) = write_child(cx, &handle, b"\x1b[?2004h") {
            failures.push(format!("paste-on: could not enable DECSET 2004: {e}"));
        }
        // Wait for the round-trip to land the mode bit.
        let mut on = false;
        for _ in 0..40 {
            if bracketed_active(cx, &handle) {
                on = true;
                break;
            }
            settle(cx, 25).await;
        }
        if !on {
            failures.push("paste-on: DECSET 2004 never became active after ESC[?2004h".into());
        }
        // Extra settle so tee has flushed the echoed DECSET bytes to the capture
        // file, so the offset recorded next excludes them.
        settle(cx, 150).await;
        cx.update(|app| app.write_to_clipboard(ClipboardItem::new_string("world".to_string())));
        settle(cx, 120).await;
        let start = cap_len(&cap_path);
        tap(cx, pid, KC_V, platform::FLAG_COMMAND, None).await;
        settle(cx, 200).await;
        expect_bytes(
            &mut failures,
            "paste-on",
            b"\x1b[200~world\x1b[201~",
            &cap_since(&cap_path, start),
        );
    }

    // --- Phase 4: arrow keys → legacy CSI ---------------------------------
    {
        let start = cap_len(&cap_path);
        tap(cx, pid, KC_UP, 0, None).await;
        tap(cx, pid, KC_DOWN, 0, None).await;
        tap(cx, pid, KC_RIGHT, 0, None).await;
        tap(cx, pid, KC_LEFT, 0, None).await;
        settle(cx, 200).await;
        expect_bytes(
            &mut failures,
            "arrows",
            b"\x1b[A\x1b[B\x1b[C\x1b[D",
            &cap_since(&cap_path, start),
        );
    }

    // --- Phase 5: item-4 candidate anchor (programmatic) ------------------
    // Park the grid cursor mid-grid via CUP (echoed by tee → parser), then drive
    // a composition through the real TermInputHandler and assert bounds_for_range
    // anchors at that cell and is never None while composing.
    if let Err(e) = write_child(cx, &handle, b"\x1b[15;30H") {
        failures.push(format!("anchor: could not park cursor: {e}"));
    }
    settle(cx, 200).await;
    match assert_anchor(cx, window, &terminal) {
        Ok(detail) => eprintln!("[selftest] input-live anchor: {detail}"),
        Err(e) => failures.push(format!("anchor(item-4): {e}")),
    }

    // --- Phase 6: IME go/no-go probe (TIS → Pinyin) -----------------------
    run_ime_probe(cx, window, &handle, &terminal, &cap_path, pid, &mut deferred).await;

    build_input_live_report(failures, deferred)
}

/// The item-4 anchor assertion: drive a composition through the real
/// `TermInputHandler` and check `bounds_for_range` is `Some` at the parked
/// grid-cursor cell. Returns a human diagnostic on success.
fn assert_anchor(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    terminal: &Entity<TerminalView>,
) -> std::result::Result<String, String> {
    let terminal = terminal.clone();
    window
        .update(cx, move |_root, window, app| {
            let vp = window.viewport_size();
            let eb = Bounds {
                origin: point(px(0.0), px(0.0)),
                size: vp,
            };
            let mut ih = TermInputHandler {
                view: terminal.clone(),
                element_bounds: eb,
            };
            // Idle: not composing yet.
            if ih.marked_text_range(window, app).is_some() {
                return Err("view was already composing before the anchor probe".to_string());
            }
            // OS-IME setMarkedText analog: enter a composition.
            ih.replace_and_mark_text_in_range(None, "ni", None, window, app);
            let composing = ih.marked_text_range(window, app).is_some();
            let rect = ih.bounds_for_range(0..2, window, app);
            // Clean up the forced composition.
            ih.unmark_text(window, app);

            if !composing {
                return Err("setMarkedText did not put the view into a composing state".into());
            }
            let Some(rect) = rect else {
                return Err(
                    "bounds_for_range returned None while composing (the zed#46055 failure mode)"
                        .into(),
                );
            };

            // Expected rect at the parked grid-cursor cell, computed the same way
            // the renderer lays the grid out (top-anchored). If the anchor were
            // wrong/degenerate, this fails loudly.
            let grid_top = grid_top_y(eb);
            let want_x = f32::from(eb.origin.x) + ANCHOR_COL as f32 * CELL_W;
            let want_y = grid_top + ANCHOR_ROW as f32 * CELL_H;
            let gx = f32::from(rect.origin.x);
            let gy = f32::from(rect.origin.y);
            let gw = f32::from(rect.size.width);
            let gh = f32::from(rect.size.height);
            let tol = 0.75_f32;
            if (gx - want_x).abs() > tol
                || (gy - want_y).abs() > tol
                || (gw - CELL_W).abs() > tol
                || (gh - CELL_H).abs() > tol
            {
                return Err(format!(
                    "anchor rect ({gx:.1},{gy:.1} {gw:.1}x{gh:.1}) != grid cursor cell \
                     ({want_x:.1},{want_y:.1} {CELL_W:.1}x{CELL_H:.1}) at row {ANCHOR_ROW} \
                     col {ANCHOR_COL}"
                ));
            }
            Ok(format!(
                "bounds_for_range Some at ({gx:.1},{gy:.1}) == grid cursor cell \
                 (row {ANCHOR_ROW}, col {ANCHOR_COL}); never None while composing"
            ))
        })
        .map_err(|e| format!("window update failed: {e}"))?
}

/// The IME go/no-go probe: switch to Pinyin, post letters, and check whether
/// synthetic composition engages. On success, assert G1 items 1–3 + 5
/// mechanically; on failure (the plan's UNPROVEN case) record a DEFERRED HUMAN
/// PASS. The input source is ALWAYS restored.
async fn run_ime_probe(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    handle: &Entity<TerminalSessionHandle>,
    terminal: &Entity<TerminalView>,
    cap_path: &Path,
    pid: i32,
    deferred: &mut Vec<String>,
) {
    // `saved` restores the user's input source on drop (mandatory — even on an
    // early return or panic below), so no explicit restore call is needed.
    let _saved = platform::current_input_source();
    let selected = platform::select_pinyin_input_source();

    let Some(source_id) = selected else {
        // Record what WAS enumerated, so "no Pinyin" is an honest, debuggable
        // conclusion (proving the TIS enumeration ran) rather than a silent gap.
        let ids = platform::input_source_ids();
        let chinese: Vec<&String> = ids
            .iter()
            .filter(|id| {
                let l = id.to_ascii_lowercase();
                l.contains("scim") || l.contains("pinyin") || l.contains("tcim")
            })
            .collect();
        deferred.push(format!(
            "IME items 1-3,5: no Pinyin input source was selectable ({} sources installed; \
             Chinese-related: {:?}) — installed-but-not-enabled sources cannot be selected. \
             Synthetic composition not attempted — DEFERRED to a human-at-keyboard pass. \
             (Item 4 anchor + the 22 headless ime_state transition tests DID run.)",
            ids.len(),
            chinese
        ));
        return;
    };

    // Let the input-source switch settle, then post letters that would begin a
    // Pinyin composition if the IME engages synthetically.
    settle(cx, 400).await;
    let compose_start = cap_len(cap_path);
    tap(cx, pid, KC_N, 0, None).await;
    tap(cx, pid, KC_I, 0, None).await;
    settle(cx, 350).await;

    let composing = is_composing(cx, window, terminal);
    let leaked = cap_since(cap_path, compose_start);

    if composing && leaked.is_empty() {
        // Probe SUCCEEDED — assert the mechanical items.
        assert_ime_items_live(cx, window, handle, terminal, cap_path, pid, deferred).await;
    } else {
        deferred.push(format!(
            "IME items 1-3,5: Pinyin selected ({source_id}) but synthetic composition did NOT \
             engage (composing={composing}, pty leak={:?}) — CGEvents cannot drive macOS \
             composition here (plan-flagged UNPROVEN). DEFERRED HUMAN PASS: a human must verify \
             (1) pty-silent compose/commit, (2) Enter mid-composition swallowed (no \\r/\\n), \
             (3) pty-silent preedit edits, (5) a ⌘-binding fires with the IME active-idle. \
             (Item 4 anchor + the 22 headless ime_state transition tests DID run.)",
            esc(&leaked)
        ));
        // Best-effort: clear any half-open composition so it can't leak later.
        clear_composition(cx, window, terminal);
    }

    // `_saved` restores the user's input source when it drops at end of scope.
    // The bundled-app IME smoke (below) is a human step regardless of the probe.
    deferred.push(
        "Bundled-app IME smoke (Validation §4) + ⌃⌘Space character-palette summon: a LaunchServices\
         -context human step (scripts/rust-bundle.sh + run the bundle) — DEFERRED; text services \
         behave differently for a bare cargo binary than a bundled .app."
            .to_string(),
    );
}

/// Whether the view is currently composing, read through the real input handler.
fn is_composing(cx: &mut AsyncApp, window: AnyWindowHandle, terminal: &Entity<TerminalView>) -> bool {
    let terminal = terminal.clone();
    window
        .update(cx, move |_root, window, app| {
            let mut ih = TermInputHandler {
                view: terminal.clone(),
                element_bounds: Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: window.viewport_size(),
                },
            };
            ih.marked_text_range(window, app).is_some()
        })
        .unwrap_or(false)
}

/// Drop any in-progress composition (unmark), best-effort.
fn clear_composition(cx: &mut AsyncApp, window: AnyWindowHandle, terminal: &Entity<TerminalView>) {
    let terminal = terminal.clone();
    let _ = window.update(cx, move |_root, window, app| {
        let mut ih = TermInputHandler {
            view: terminal.clone(),
            element_bounds: Bounds {
                origin: point(px(0.0), px(0.0)),
                size: window.viewport_size(),
            },
        };
        if ih.marked_text_range(window, app).is_some() {
            ih.unmark_text(window, app);
        }
    });
}

/// The probe-succeeded branch: assert G1 items 1-3 (+5 best-effort) mechanically
/// under a live synthetic Pinyin composition. Only reached if composition
/// genuinely engaged.
async fn assert_ime_items_live(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    _handle: &Entity<TerminalSessionHandle>,
    terminal: &Entity<TerminalView>,
    cap_path: &Path,
    pid: i32,
    deferred: &mut Vec<String>,
) {
    let mut notes: Vec<String> = Vec::new();

    // Item 1 already observed by the caller (composing + no pty bytes). Item 3:
    // a preedit edit (backspace) must stay pty-silent.
    let before = cap_len(cap_path);
    tap(cx, pid, KC_DELETE, 0, None).await;
    settle(cx, 200).await;
    let edit_leak = cap_since(cap_path, before);
    if !edit_leak.is_empty() {
        notes.push(format!("item-3 preedit-edit leaked {:?} to pty", esc(&edit_leak)));
    }

    // Item 2: Enter mid-composition commits + is swallowed (no \r/\n reaches the
    // pty). What DOES reach the pty is the committed CJK text (data), never a CR.
    let before = cap_len(cap_path);
    tap(cx, pid, KC_RETURN, 0, None).await;
    settle(cx, 250).await;
    let after_enter = cap_since(cap_path, before);
    if after_enter.contains(&0x0d) || after_enter.contains(&0x0a) {
        notes.push(format!(
            "item-2 Enter mid-composition leaked a CR/LF: {:?}",
            esc(&after_enter)
        ));
    }
    // The view should no longer be composing after the commit.
    if is_composing(cx, window, terminal) {
        notes.push("item-2 still composing after Enter commit".to_string());
    }

    if notes.is_empty() {
        deferred.push(
            "IME items 1-3: synthetic Pinyin composition ENGAGED and was machine-verified \
             (pty-silent compose + preedit edit; Enter mid-composition committed + swallowed, no \
             CR/LF). Item 5 (⌘-binding fires while IME active-idle) + the visual candidate-window \
             position remain a human check."
                .to_string(),
        );
    } else {
        // A genuine regression under a real composition — surface it, but as a
        // deferred note (the primary live path already passed); a human confirms.
        deferred.push(format!(
            "IME items under live synthetic composition FOUND ISSUES (human must confirm): {}",
            notes.join("; ")
        ));
    }
}

/// Assemble the `input-live` verdict: fail on any hard byte/anchor mismatch,
/// else pass, carrying the DEFERRED HUMAN PASS checklist in the detail + stderr.
fn build_input_live_report(failures: Vec<String>, deferred: Vec<String>) -> CadenceReport {
    if !deferred.is_empty() {
        eprintln!("[selftest] input-live DEFERRED HUMAN PASS checklist:");
        for d in &deferred {
            eprintln!("  - {d}");
        }
    }
    if failures.is_empty() {
        let detail = format!(
            "live typed path byte-exact (plain ASCII, ⌘V raw + bracketed, arrows) + item-4 anchor \
             verified; {} item(s) DEFERRED to a human pass (see stderr)",
            deferred.len()
        );
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail,
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "{} live-input assertion(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            ),
        }
    }
}

// ===========================================================================
// input-shell — real-shell CGEvent sanity (Validation §5)
// ===========================================================================

/// A marker whose echoed command AND command output both contain it (>= 2
/// occurrences prove the keystrokes reached a real shell and its output round-
/// tripped to the grid). Unlikely to appear in a default zsh prompt.
const SHELL_MARKER: &str = "rsokxyz";

/// Open the `input-shell` scenario window (a real `zsh -il`) and spawn the
/// CGEvent-driven sanity check (self-reported gate).
pub fn open_input_shell_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let base = prepare_dir("input-shell")?;
    let base_s = base.to_string_lossy().to_string();
    // A real login shell, user rc suppressed via an empty ZDOTDIR so the grid is
    // predictable (no p10k / plugins).
    let spec = SpawnSpec::shell(base_s.clone())
        .with_env(vec![("ZDOTDIR".to_string(), base_s)])
        .with_size(ROWS, COLS);

    let handle = TerminalSessionHandle::spawn(cx, spec, DEFAULT_SCROLLBACK_LINES)?;
    let terminal = make_view(handle.clone(), cx);

    let window = cx.open_window(crate::app::window_options(), {
        let terminal = terminal.clone();
        move |_window, cx| cx.new(|_cx| InputTermView { terminal })
    })?;
    let window: AnyWindowHandle = window.into();
    crate::app::install_present_kick(&handle, window, cx);

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_input_shell(acx, handle).await;
        eprintln!("[selftest] scenario 'input-shell': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(window)
}

// -- scenario: scrollback-keys (Phase 0 keyboard scrollback, end to end) ------

/// Seeded output lines — three screens' worth, so page jumps have room to move.
const SCROLLBACK_SEED_LINES: usize = ROWS as usize * 3;

/// The `scrollback-keys` scenario: the Phase 0 keyboard-scrollback gate, end to
/// end. A real [`TerminalView`] over a real pty (the same capture-tee child as
/// `input-live`, so pty-bound bytes are observable), driven with REAL
/// keystrokes through the window's key-dispatch tree
/// (`Window::dispatch_keystroke` — the exact path an OS key event takes after
/// the platform hop), so it needs **no Accessibility grant**. Asserts the whole
/// shipped policy: Shift+PageUp scrolls the viewport a page and encodes
/// NOTHING to the pty; Shift+Home jumps to the top and Shift+End back to the
/// bottom; plain PageUp encodes `ESC[5~` (less/vim keep working); and on the
/// alternate screen Shift+PageUp encodes `ESC[5;2~` while the viewport stays
/// parked (the TUI owns the keys).
pub fn open_scrollback_keys_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    let base = prepare_dir("scrollback-keys")?;
    let cap_path = base.join("capture.bin");
    let base_s = base.to_string_lossy().to_string();
    let cap_s = cap_path.to_string_lossy().to_string();

    // Capture-tee child (the `input-live` pattern): raw mode, then `tee` copies
    // pty-bound bytes into the capture file AND echoes them back out, so writes
    // via `write_child` render as terminal OUTPUT (the scrollback seed) and
    // encoded keystrokes are observable in the capture.
    let inner = format!("stty raw -echo; exec tee {cap_s}");
    let spec = SpawnSpec::command(format!("sh -c '{inner}'"), base_s.clone())
        .with_env(vec![("ZDOTDIR".to_string(), base_s)])
        .with_size(ROWS, COLS);

    let handle = TerminalSessionHandle::spawn(cx, spec, DEFAULT_SCROLLBACK_LINES)?;
    let terminal = make_view(handle.clone(), cx);

    let whandle = cx.open_window(crate::app::window_options(), {
        let terminal = terminal.clone();
        move |_window, cx| cx.new(|_cx| InputTermView { terminal })
    })?;
    let window: AnyWindowHandle = whandle.into();
    crate::app::install_present_kick(&handle, window, cx);

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_scrollback_keys(acx, window, handle, terminal, cap_path).await;
        eprintln!("[selftest] scenario 'scrollback-keys': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(window)
}

/// Dispatch one keystroke (e.g. `"shift-pageup"`) through the window's real
/// key-dispatch tree. Returns whether anything consumed it.
///
/// Routed through `AnyWindowHandle::update`, which hands the root out as an
/// untyped `AnyView` and leases NOTHING — deliberately not `WindowHandle<V>::
/// update`, which leases the root entity for the whole call. `dispatch_key_event`
/// re-`draw`s the window first whenever the invalidator is dirty (terminal damage
/// from a seed write, a focus change, a handler's `notify`), and that draw
/// re-enters the root view: under the typed handle that is a double-lease abort,
/// which is exactly how this scenario family aborts intermittently.
fn dispatch_key(cx: &mut AsyncApp, window: AnyWindowHandle, keystroke: &str) -> bool {
    let ks = gpui::Keystroke::parse(keystroke).expect("valid keystroke literal");
    window
        .update(cx, |_root, window, cx| window.dispatch_keystroke(ks, cx))
        .unwrap_or(false)
}

async fn run_scrollback_keys(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    handle: Entity<TerminalSessionHandle>,
    terminal: Entity<TerminalView>,
    cap_path: PathBuf,
) -> CadenceReport {
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 500).await;

    // Focus the terminal view: `dispatch_keystroke` walks the focus path, so
    // the view's key listener only runs while its focus handle is focused.
    let _ = window.update(cx, |_root, window, cx| {
        let fh = terminal.read(cx).focus_handle_ref().clone();
        window.focus(&fh, cx);
    });
    settle(cx, 200).await;

    let mut failures: Vec<String> = Vec::new();
    let offset = |cx: &mut AsyncApp| handle.update(cx, |h, _| h.display_offset());

    // Seed three screens of numbered output through the tee echo, then poll the
    // grid for the last marker (never sleep-and-hope).
    let seed: String = (1..=SCROLLBACK_SEED_LINES)
        .map(|i| format!("seed-{i}\r\n"))
        .collect();
    if let Err(e) = write_child(cx, &handle, seed.as_bytes()) {
        return CadenceReport::error(format!("scrollback-keys: seeding failed: {e}"));
    }
    let last_marker = format!("seed-{SCROLLBACK_SEED_LINES}");
    let mut seeded = false;
    for _ in 0..60 {
        settle(cx, 100).await;
        let text = handle.update(cx, |h, _| h.session().grid_lines().join("\n"));
        if text.contains(&last_marker) {
            seeded = true;
            break;
        }
    }
    if !seeded {
        return CadenceReport::error(format!(
            "scrollback-keys: the seed never rendered ({last_marker} absent from the grid)"
        ));
    }
    if offset(cx) != 0 {
        failures.push("seed: viewport not parked at the bottom after output".into());
    }

    // §1 Shift+PageUp scrolls one page into history and encodes NOTHING.
    let cap_start = cap_len(&cap_path);
    dispatch_key(cx, window, "shift-pageup");
    settle(cx, 150).await;
    let page_off = offset(cx);
    if page_off == 0 {
        failures.push("shift-pageup: display offset stayed 0 (no scroll)".into());
    } else {
        eprintln!("[selftest] scrollback-keys: shift-pageup scrolled to offset {page_off}");
    }

    // §2 Shift+Home jumps to the top (strictly above the single page).
    dispatch_key(cx, window, "shift-home");
    settle(cx, 150).await;
    let top_off = offset(cx);
    if top_off <= page_off {
        failures.push(format!(
            "shift-home: offset {top_off} did not jump above the page offset {page_off}"
        ));
    }

    // §3 Shift+End parks back at the bottom.
    dispatch_key(cx, window, "shift-end");
    settle(cx, 150).await;
    if offset(cx) != 0 {
        failures.push(format!("shift-end: offset {} != 0 (not at bottom)", offset(cx)));
    }

    // The three scrolling chords must have written NOTHING to the pty.
    settle(cx, 150).await;
    let scroll_bytes = cap_since(&cap_path, cap_start);
    if !scroll_bytes.is_empty() {
        failures.push(format!(
            "scroll chords leaked to the pty: {:?}",
            String::from_utf8_lossy(&scroll_bytes)
        ));
    }

    // §4 plain PageUp still encodes to the pty (less/vim keep their key).
    let cap_start = cap_len(&cap_path);
    dispatch_key(cx, window, "pageup");
    settle(cx, 200).await;
    expect_bytes(&mut failures, "plain-pageup", b"\x1b[5~", &cap_since(&cap_path, cap_start));

    // §5 alternate screen: the TUI owns the keys — Shift+PageUp encodes the
    // shifted legacy sequence and the viewport stays parked. The echoed DECSET
    // 1049 switches the parser to the alt screen (grid clears of seed text).
    if let Err(e) = write_child(cx, &handle, b"\x1b[?1049h") {
        failures.push(format!("alt-screen: could not enter: {e}"));
    }
    let mut alt = false;
    for _ in 0..40 {
        settle(cx, 50).await;
        let text = handle.update(cx, |h, _| h.session().grid_lines().join(""));
        if !text.contains("seed-") {
            alt = true;
            break;
        }
    }
    if !alt {
        failures.push("alt-screen: grid never cleared after ESC[?1049h".into());
    } else {
        let cap_start = cap_len(&cap_path);
        dispatch_key(cx, window, "shift-pageup");
        settle(cx, 200).await;
        expect_bytes(
            &mut failures,
            "alt-screen shift-pageup",
            b"\x1b[5;2~",
            &cap_since(&cap_path, cap_start),
        );
        if offset(cx) != 0 {
            failures.push("alt-screen: shift-pageup moved the viewport".into());
        }
        let _ = write_child(cx, &handle, b"\x1b[?1049l");
    }

    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "keyboard scrollback OK end to end: shift-pageup paged (silent to the pty), \
                     shift-home/end jumped top/bottom, plain pageup encoded ESC[5~, alt-screen \
                     shift-pageup encoded ESC[5;2~ with the viewport parked"
                .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!("scrollback-keys FAILED:\n  - {}", failures.join("\n  - ")),
        }
    }
}

// -- scenario: keybind-scheme (Phase 1 held-⌃⌘ scheme, end to end) -----------

/// Seeded output lines for the half-page legs — five screens' worth at the
/// spawn size, so the mounted view's refit (the real window is taller than
/// [`ROWS`]) still leaves plenty of history under the viewport.
const KEYBIND_SEED_LINES: usize = ROWS as usize * 5;

/// Everything [`run_keybind_scheme`] needs from the setup phase.
struct KeybindFixture {
    state: Entity<crate::window_state::WindowState>,
    handle: Entity<TerminalSessionHandle>,
    session_id: String,
    /// The three pills' ids, in `Session.windows` (= pill) order.
    windows: Vec<String>,
    /// The three sidebar sessions' ids, in navigable (= sidebar row) order.
    /// `sessions[0]` is [`session_id`](KeybindFixture::session_id) — the only one
    /// with a live pty.
    sessions: Vec<String>,
}

/// The `keybind-scheme` scenario: Phase 1's held-`⌃⌘` scheme, end to end, over
/// the SHIPPED dispatch path.
///
/// The `scrollback-keys` pattern alone would not gate anything here: that
/// scenario opens a bare view with no keymap, no `WindowState` and no registry,
/// so every `⌃⌘` chord would silently no-op and a "zero pty bytes" assertion
/// would pass vacuously. So this one stands the real wiring up —
/// `keymap::install_shortcuts` + a defaults `ShortcutBindings` at a temp path +
/// `keymap::rebuild_keymap`, a `WindowState` registered in the `WindowRegistry`
/// (via `register`, NOT `install`: the close observer's quit-when-empty would
/// end the suite when this window closes), and the capture-tee child spawned
/// THROUGH that state's `PtyManager` so `term_window_handle` resolves for the
/// half-page handler.
///
/// Every chord is asserted twice: once on its EFFECT (the pill or sidebar
/// session the model made active, the viewport offset) and once on the pty (`0`
/// bytes captured), with a plain `u` as the differential that proves the capture
/// file would have shown a leak. The chords the 2026-08-11 hjkl-ladder revision
/// FREED (`⌃⌘]` / `⌃⌘[`) get the mirror assertion — no effect and no bytes — so
/// "unbound" is proven to mean inert rather than "falls through and types".
/// Keystrokes ride `Window::dispatch_keystroke`, so no Accessibility grant is
/// needed.
pub fn open_keybind_scheme_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    use crate::window_registry::WindowRegistry;
    use crate::window_state::WindowState;

    let base = prepare_dir("keybind-scheme")?;
    let cap_path = base.join("capture.bin");
    let base_s = base.to_string_lossy().to_string();
    let cap_s = cap_path.to_string_lossy().to_string();
    let store_path = base.join("ui_settings.json");

    // Capture-tee child (the `input-live` pattern): pty-bound bytes land in the
    // capture file verbatim AND echo back, so `write_child` renders as terminal
    // OUTPUT (the scrollback seed) while encoded keystrokes stay observable.
    let inner = format!("stty raw -echo; exec tee {cap_s}");
    let spec = SpawnSpec::command(format!("sh -c '{inner}'"), base_s.clone())
        .with_env(vec![("ZDOTDIR".to_string(), base_s.clone())])
        .with_size(ROWS, COLS);

    let fixture = cx.update(|app| -> Result<KeybindFixture> {
        // The shipped keymap wiring (idempotent across scenarios), then the
        // rebindable store seeded with DEFAULTS at a temp path — the
        // `run_selftest` seam, so an earlier scenario's rebind can never leak
        // into this one's board — and a rebuild so the live keymap IS that map.
        crate::keymap::install_shortcuts(app);
        app.set_global(crate::shortcuts_store::ShortcutBindings::with_defaults(
            store_path,
        ));
        crate::keymap::rebuild_keymap(app);

        let state = app.new(|_cx| WindowState::new(base_s.clone()));
        // A fresh window seeds one "Terminal 1" pill; two more make the strip
        // long enough for wrap-around stepping AND a `⌃⌘2` that is neither the
        // first nor the last slot.
        let (session_id, windows) = state.update(app, |s, _cx| {
            let session_id = s
                .workspace
                .active_session_id()
                .map(str::to_owned)
                .unwrap_or_default();
            s.window_strip_actions
                .add_terminal_window(&mut s.workspace, &session_id);
            s.window_strip_actions
                .add_terminal_window(&mut s.workspace, &session_id);
            let windows: Vec<String> = s
                .workspace
                .session_for(&session_id)
                .map(|sess| sess.windows.iter().map(|w| w.id.clone()).collect())
                .unwrap_or_default();
            (session_id, windows)
        });
        if windows.len() != 3 {
            return Err(anyhow!(
                "keybind-scheme: expected a 3-window session, seeded {}",
                windows.len()
            ));
        }

        // Two EXTRA sidebar sessions through the same model seam the sidebar's
        // `+` uses (`create_terminal_session` — model-only, no pty), so the
        // ⌃⌘J/⌃⌘K session legs have somewhere to go. THREE navigable sessions,
        // not two: with only two, next and prev are the same step and the leg
        // could not tell the directions apart.
        let sessions = state.update(app, |s, _cx| {
            s.sidebar_actions.create_terminal_session(&mut s.workspace);
            s.sidebar_actions.create_terminal_session(&mut s.workspace);
            // `create_terminal_session` SELECTS what it creates — park back on
            // the seeded session so every leg starts where the fixture says.
            s.sidebar_actions
                .select_session(&mut s.workspace, &session_id);
            s.sync_selection_to_active_session();
            s.workspace.navigable_sidebar_session_ids()
        });
        if sessions.len() != 3 || sessions.first().map(String::as_str) != Some(session_id.as_str()) {
            return Err(anyhow!(
                "keybind-scheme: expected the seeded session first of three, got {sessions:?}"
            ));
        }

        // Only the FIRST pill gets a live pty — the half-page chords resolve the
        // ACTIVE window's handle, so parking the strip back on pill 1 before the
        // scroll legs is part of the scenario (and the nav legs prove the
        // parking chord works).
        state.update(app, |s, cx| {
            s.ptys.spawn_window(&session_id, &windows[0], spec, cx)
        })?;
        state.update(app, |s, _cx| {
            s.window_strip_actions
                .select_window(&mut s.workspace, &session_id, &windows[0])
        });
        let handle = state
            .read(app)
            .ptys
            .term_window_handle(&session_id, &windows[0])
            .ok_or_else(|| anyhow!("keybind-scheme: the seeded window has no pty handle"))?;

        Ok(KeybindFixture {
            state,
            handle,
            session_id,
            windows,
            sessions,
        })
    })?;

    let terminal = make_view(fixture.handle.clone(), cx);

    let whandle = cx.open_window(crate::app::window_options(), {
        let terminal = terminal.clone();
        let state = fixture.state.clone();
        move |window, cx| {
            // `register` (not `install`): `install` also wires the close observer
            // whose quit-when-empty would kill the suite when this window closes.
            // `register` goes through `default_global`, so the registry exists
            // either way — the `app-shell` / `file-browser` precedent.
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
            cx.new(|_cx| InputTermView { terminal })
        }
    })?;
    let window: AnyWindowHandle = whandle.into();
    crate::app::install_present_kick(&fixture.handle, window, cx);

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_keybind_scheme(acx, window, terminal, fixture, cap_path).await;
        eprintln!("[selftest] scenario 'keybind-scheme': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(window)
}

/// Dispatch one chord and return the bytes it leaked to the pty (expected: none
/// — a bound chord fires its action and stops propagation before the terminal's
/// key listener or input handler ever sees it).
async fn chord_leak(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    cap_path: &Path,
    keystroke: &str,
) -> Vec<u8> {
    let start = cap_len(cap_path);
    dispatch_key(cx, window, keystroke);
    settle(cx, 140).await;
    cap_since(cap_path, start)
}

/// The active SIDEBAR SESSION id, read straight off the model the keymap
/// handlers mutate.
fn active_session_id(
    cx: &mut AsyncApp,
    state: &Entity<crate::window_state::WindowState>,
) -> Option<String> {
    state.update(cx, |s, _cx| {
        s.workspace.active_session_id().map(str::to_owned)
    })
}

/// The active window id of `session_id`, read straight off the model the
/// keymap handlers mutate.
fn active_window_id(
    cx: &mut AsyncApp,
    state: &Entity<crate::window_state::WindowState>,
    session_id: &str,
) -> Option<String> {
    state.update(cx, |s, _cx| {
        s.workspace
            .session_for(session_id)
            .and_then(|sess| sess.active_window_id.clone())
    })
}

/// Dispatch a pill-navigation chord, then assert BOTH halves: the strip moved to
/// slot `want_slot` (1-based, pill order) and the chord wrote nothing to the pty.
#[allow(clippy::too_many_arguments)]
async fn nav_chord(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    fixture: &KeybindFixture,
    cap_path: &Path,
    failures: &mut Vec<String>,
    keystroke: &str,
    want_slot: usize,
) {
    let leaked = chord_leak(cx, window, cap_path, keystroke).await;
    if !leaked.is_empty() {
        failures.push(format!("{keystroke}: leaked \"{}\" to the pty", esc(&leaked)));
    }
    let want = fixture.windows[want_slot - 1].clone();
    let got = active_window_id(cx, &fixture.state, &fixture.session_id);
    if got.as_deref() != Some(want.as_str()) {
        let slot = got
            .as_ref()
            .and_then(|id| fixture.windows.iter().position(|w| w == id))
            .map(|i| (i + 1).to_string())
            .unwrap_or_else(|| "none".to_string());
        failures.push(format!(
            "{keystroke}: active pill is slot {slot} ({got:?}), expected slot {want_slot} ({want})"
        ));
    }
}

/// Dispatch a sidebar-session chord, then assert BOTH halves: the sidebar moved
/// to session `want_slot` (1-based, navigable order) and the chord wrote nothing
/// to the pty.
#[allow(clippy::too_many_arguments)]
async fn session_chord(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    fixture: &KeybindFixture,
    cap_path: &Path,
    failures: &mut Vec<String>,
    keystroke: &str,
    want_slot: usize,
) {
    let leaked = chord_leak(cx, window, cap_path, keystroke).await;
    if !leaked.is_empty() {
        failures.push(format!("{keystroke}: leaked \"{}\" to the pty", esc(&leaked)));
    }
    let want = fixture.sessions[want_slot - 1].clone();
    let got = active_session_id(cx, &fixture.state);
    if got.as_deref() != Some(want.as_str()) {
        let slot = got
            .as_ref()
            .and_then(|id| fixture.sessions.iter().position(|s| s == id))
            .map(|i| (i + 1).to_string())
            .unwrap_or_else(|| "none".to_string());
        failures.push(format!(
            "{keystroke}: active session is slot {slot} ({got:?}), expected slot {want_slot} ({want})"
        ));
    }
}

/// Dispatch a chord the scheme deliberately leaves UNBOUND and assert it did
/// nothing at all: the active pill is unchanged and not one byte reached the pty.
/// gpui matches no binding, so the keystroke falls through to the terminal's own
/// key handler — which must not encode a ⌘ chord either (`should_encode`'s
/// `control && !platform` gate), so the pty stays silent from both directions.
async fn freed_chord(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    fixture: &KeybindFixture,
    cap_path: &Path,
    failures: &mut Vec<String>,
    keystroke: &str,
) {
    let before = active_window_id(cx, &fixture.state, &fixture.session_id);
    let leaked = chord_leak(cx, window, cap_path, keystroke).await;
    if !leaked.is_empty() {
        failures.push(format!(
            "{keystroke} is freed but leaked \"{}\" to the pty",
            esc(&leaked)
        ));
    }
    let after = active_window_id(cx, &fixture.state, &fixture.session_id);
    if after != before {
        failures.push(format!(
            "{keystroke} is freed but moved the active pill {before:?} -> {after:?}"
        ));
    }
}

async fn run_keybind_scheme(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    terminal: Entity<TerminalView>,
    fixture: KeybindFixture,
    cap_path: PathBuf,
) -> CadenceReport {
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 500).await;

    // Focus the terminal view — `dispatch_keystroke` walks the focus path, and
    // the plain-`u` differential needs the view's own input path live.
    let _ = window.update(cx, |_root, window, cx| {
        let fh = terminal.read(cx).focus_handle_ref().clone();
        window.focus(&fh, cx);
    });
    settle(cx, 200).await;

    let mut failures: Vec<String> = Vec::new();
    let handle = fixture.handle.clone();
    let offset = |cx: &mut AsyncApp| handle.update(cx, |h, _| h.display_offset());

    // Precondition: the strip is parked on pill 1 (the one with the live pty).
    if active_window_id(cx, &fixture.state, &fixture.session_id).as_deref()
        != Some(fixture.windows[0].as_str())
    {
        return CadenceReport::error(
            "keybind-scheme: the seeded strip did not start on pill 1".to_string(),
        );
    }

    // --- §1 ⌃⌘L / ⌃⌘H cycle the pill strip, wrapping -----------------------
    // The hjkl ladder's bare-⌃⌘ horizontal axis, and the ONLY pill pair.
    nav_chord(cx, window, &fixture, &cap_path, &mut failures, "ctrl-cmd-l", 2).await;
    nav_chord(cx, window, &fixture, &cap_path, &mut failures, "ctrl-cmd-l", 3).await;
    // Wraps 3 → 1 rather than stopping at the end.
    nav_chord(cx, window, &fixture, &cap_path, &mut failures, "ctrl-cmd-l", 1).await;
    nav_chord(cx, window, &fixture, &cap_path, &mut failures, "ctrl-cmd-h", 3).await;

    // --- §2 ⌃⌘2 jumps straight to a slot (D2 — one row, nine chords) -------
    nav_chord(cx, window, &fixture, &cap_path, &mut failures, "ctrl-cmd-2", 2).await;

    // --- §3 ⌃⌘O bounces between the last two (tmux `last-window`) ----------
    // The jump above left pill 3 as the bounce target; the bounce then makes
    // pill 2 the target, so a second ⌃⌘O comes straight back.
    nav_chord(cx, window, &fixture, &cap_path, &mut failures, "ctrl-cmd-o", 3).await;
    nav_chord(cx, window, &fixture, &cap_path, &mut failures, "ctrl-cmd-o", 2).await;

    // --- §4 the FREED chords do nothing (the 2026-08-11 ladder revision) ---
    // ⌃⌘] / ⌃⌘[ were the shipped prev/next-pill defaults; the ladder moved that
    // pair onto ⌃⌘H/⌃⌘L and left these bound to nothing. Unbound must mean
    // INERT, not "falls through and types" — assert both halves.
    freed_chord(cx, window, &fixture, &cap_path, &mut failures, "ctrl-cmd-]").await;
    freed_chord(cx, window, &fixture, &cap_path, &mut failures, "ctrl-cmd-[").await;

    // --- §5 ⌃⌘J / ⌃⌘K step the SIDEBAR sessions ----------------------------
    // The ladder's bare-⌃⌘ vertical axis: j = down the sidebar list = next.
    // Three seeded sessions, so the two directions are distinguishable.
    session_chord(cx, window, &fixture, &cap_path, &mut failures, "ctrl-cmd-j", 2).await;
    session_chord(cx, window, &fixture, &cap_path, &mut failures, "ctrl-cmd-j", 3).await;
    session_chord(cx, window, &fixture, &cap_path, &mut failures, "ctrl-cmd-k", 2).await;
    // Back to the seeded session — the only one with a live pty, so the scroll
    // legs below resolve a handle at all.
    session_chord(cx, window, &fixture, &cap_path, &mut failures, "ctrl-cmd-k", 1).await;

    // Park back on pill 1 — the only one with a live pty — via ⌃⌘1, which also
    // pins that the digit expansion is not just the `2` binding.
    nav_chord(cx, window, &fixture, &cap_path, &mut failures, "ctrl-cmd-1", 1).await;

    // --- §5 ⌃⌘U / ⌃⌘D half-page scrollback ---------------------------------
    let seed: String = (1..=KEYBIND_SEED_LINES)
        .map(|i| format!("seed-{i}\r\n"))
        .collect();
    if let Err(e) = write_child(cx, &handle, seed.as_bytes()) {
        return CadenceReport::error(format!("keybind-scheme: seeding failed: {e}"));
    }
    let last_marker = format!("seed-{KEYBIND_SEED_LINES}");
    let mut seeded = false;
    for _ in 0..60 {
        settle(cx, 100).await;
        let text = handle.update(cx, |h, _| h.session().grid_lines().join("\n"));
        if text.contains(&last_marker) {
            seeded = true;
            break;
        }
    }
    if !seeded {
        return CadenceReport::error(format!(
            "keybind-scheme: the seed never rendered ({last_marker} absent from the grid)"
        ));
    }
    if offset(cx) != 0 {
        failures.push("seed: viewport not parked at the bottom after output".into());
    }

    // The exact step is `screen_lines / 2` on the LAID-OUT grid (the mounted
    // view refits the pty to the real window), which the driver cannot read
    // directly — so assert the invariants that pin the same math: the first step
    // moves, the second doubles it (equal steps), and two ⌃⌘D undo them exactly.
    let mut half_leaks: Vec<(&str, Vec<u8>)> = Vec::new();
    let up1 = chord_leak(cx, window, &cap_path, "ctrl-cmd-u").await;
    half_leaks.push(("ctrl-cmd-u", up1));
    let off1 = offset(cx);
    if off1 == 0 {
        failures.push("ctrl-cmd-u: display offset stayed 0 (no half-page scroll)".into());
    }
    let up2 = chord_leak(cx, window, &cap_path, "ctrl-cmd-u").await;
    half_leaks.push(("ctrl-cmd-u", up2));
    let off2 = offset(cx);
    if off2 != off1 * 2 {
        failures.push(format!(
            "ctrl-cmd-u: second half-page landed at {off2}, expected {} (two equal steps)",
            off1 * 2
        ));
    }
    let down1 = chord_leak(cx, window, &cap_path, "ctrl-cmd-d").await;
    half_leaks.push(("ctrl-cmd-d", down1));
    if offset(cx) != off1 {
        failures.push(format!(
            "ctrl-cmd-d: landed at {}, expected {off1} (same magnitude, opposite sign)",
            offset(cx)
        ));
    }
    let down2 = chord_leak(cx, window, &cap_path, "ctrl-cmd-d").await;
    half_leaks.push(("ctrl-cmd-d", down2));
    if offset(cx) != 0 {
        failures.push(format!(
            "ctrl-cmd-d: viewport is at {} instead of parked at the bottom",
            offset(cx)
        ));
    }
    for (chord, leaked) in half_leaks {
        if !leaked.is_empty() {
            failures.push(format!("{chord}: leaked \"{}\" to the pty", esc(&leaked)));
        }
    }

    // --- §6 the differential: a plain `u` still reaches the pty -------------
    // Without this the zero-byte assertions above could pass vacuously (an
    // unwired keymap leaks nothing either).
    let start = cap_len(&cap_path);
    dispatch_key(cx, window, "u");
    settle(cx, 250).await;
    expect_bytes(&mut failures, "plain-u", b"u", &cap_since(&cap_path, start));

    // --- §7 alt screen: the half-page chords do nothing at all --------------
    // They are keymap bindings, so they never encoded to the pty and there is
    // nothing to fall through TO (contrast Shift+PageUp, which does encode).
    if let Err(e) = write_child(cx, &handle, b"\x1b[?1049h") {
        failures.push(format!("alt-screen: could not enter: {e}"));
    }
    let mut alt = false;
    for _ in 0..40 {
        settle(cx, 50).await;
        let text = handle.update(cx, |h, _| h.session().grid_lines().join(""));
        if !text.contains("seed-") {
            alt = true;
            break;
        }
    }
    if !alt {
        failures.push("alt-screen: grid never cleared after ESC[?1049h".into());
    } else {
        let leaked = chord_leak(cx, window, &cap_path, "ctrl-cmd-u").await;
        if !leaked.is_empty() {
            failures.push(format!(
                "alt-screen ctrl-cmd-u: leaked \"{}\" to the pty",
                esc(&leaked)
            ));
        }
        if offset(cx) != 0 {
            failures.push("alt-screen: ctrl-cmd-u moved the viewport".into());
        }
        let _ = write_child(cx, &handle, b"\x1b[?1049l");
    }

    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "held-⌃⌘ scheme OK end to end: ⌃⌘L/⌃⌘H cycled the pills (wrapping), ⌃⌘1/⌃⌘2 \
                     jumped by index, ⌃⌘O bounced between the last two, the freed ⌃⌘]/⌃⌘[ did \
                     nothing at all, ⌃⌘J/⌃⌘K stepped the sidebar sessions, ⌃⌘U/⌃⌘D half-paged \
                     in equal steps and no-opped on the alt screen — every chord silent to the \
                     pty while a plain `u` still encoded"
                .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!("keybind-scheme FAILED:\n  - {}", failures.join("\n  - ")),
        }
    }
}

async fn run_input_shell(cx: &mut AsyncApp, handle: Entity<TerminalSessionHandle>) -> CadenceReport {
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 400).await;

    if !platform::accessibility_trusted() {
        return CadenceReport::error(ACCESSIBILITY_REMEDIATION.to_string());
    }

    // Wait for `zsh -il` to actually come up and print its prompt before typing —
    // a fixed sleep races the shell's startup (keystrokes posted before the
    // prompt / ZLE is live are lost). Poll the grid for any non-whitespace ink.
    let mut ready = false;
    for _ in 0..50 {
        settle(cx, 150).await;
        let text = handle.update(cx, |h, _| h.session().grid_lines().join(""));
        if text.chars().any(|c| !c.is_whitespace()) {
            ready = true;
            break;
        }
    }
    if !ready {
        return CadenceReport::error(
            "input-shell: zsh never printed a prompt (grid stayed blank) — cannot drive input"
                .to_string(),
        );
    }
    // Re-assert frontmost/key right before typing so the CGEvents route to the
    // window, then settle for focus.
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 400).await;

    let pid = std::process::id() as i32;
    // Type `echo rsokxyz` then Enter, entirely via CGEvents.
    type_ascii(cx, pid, &format!("echo {SHELL_MARKER}")).await;
    settle(cx, 350).await;
    platform::post_key_tap(pid, KC_RETURN, 0, None);
    settle(cx, 800).await;

    let text = handle.update(cx, |h, _| h.session().grid_lines().join("\n"));
    let count = text.matches(SHELL_MARKER).count();

    if count >= 2 {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: format!(
                "real zsh round-trip OK: '{SHELL_MARKER}' appears {count}x (typed command echo + \
                 command output) after `echo` + Enter via CGEvents"
            ),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "real-shell sanity FAILED: expected '{SHELL_MARKER}' >= 2x (command echo + \
                 output), saw {count}x. Grid:\n{text}"
            ),
        }
    }
}
