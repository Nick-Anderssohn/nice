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
//! ## `scrollback-keys` / `keybind-scheme` / `splits` / `copy-mode` — grant-free keystroke scenarios
//!
//! All four drive `Window::dispatch_keystroke` instead of CGEvents (the exact
//! path an OS key event takes AFTER the platform hop), so none needs an
//! Accessibility grant: `scrollback-keys` is Phase 0's keyboard-scrollback gate,
//! `keybind-scheme` is Phase 1's held-`⌃⌘` scheme gate, `splits` is Phase
//! 2's pane-verb gate (splits, directional focus, resize, swap, zoom,
//! break-pane, pane close, layout persistence), and `copy-mode` is Phase 3's
//! copy-mode + scrollback-search gate (vi motions, paging, yank, the search
//! bar's confirm/`n`/`N`, and P4's "nothing leaks to the pty while VI is on").

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
/// file would have shown a leak. The chords the 2026-08-11 revisions FREED
/// (`⌃⌘]` / `⌃⌘[` from the hjkl ladder, `⌃⌘U` / `⌃⌘D` from the half-page move)
/// get the mirror assertion — no effect and no bytes — so "unbound" is proven to
/// mean inert rather than "falls through and types".
///
/// Scope: the bare-`⌃⌘` container rung only. The ladder's pane rungs (`⌃⌘⇧`
/// focus, `⌃⌥⌘` resize, `⌃⌥⌘⇧` swap) and the rest of Phase 2's board (`⌃⌘\`,
/// `⌃⌘-`, `⌃⌘z`, `⌃⌘b`, and the freed `⌃⌘v`/`⌃⌘s` — D2 spent the split verbs on
/// the divider mnemonics instead) belong to the [`splits`](open_splits_window)
/// scenario, which has a pane tree to move them against; Phase 3's two rungs
/// (`⌃⌘c` copy mode, `⌃⌘/` search scrollback) belong to
/// [`copy-mode`](open_copy_mode_window), which has the scrollback to move them
/// against. `⌃⌘/` is deliberately NOT in the freed-chord leg below: Phase 3
/// spent `RESERVED_COMBOS`' last `FuturePhase` entry on it, so it is a bound
/// default now and asserting it inert would be asserting the opposite of what
/// ships.
///
/// Keystrokes ride `Window::dispatch_keystroke`, so no Accessibility grant is
/// needed — at the cost of one BLIND SPOT worth naming: injection happens
/// downstream of the OS hotkey layer, so a chord macOS itself intercepts (⌃⌘D →
/// dictionary lookup) passes here while doing nothing on a real keyboard. That
/// is how the shipped `⌃⌘D` half-page default survived this gate and died in
/// hand-testing. Chord CHOICE cannot be validated here; only dispatch can.
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
///
/// Takes the state + session directly rather than a fixture, so both the
/// `keybind-scheme` and `splits` scenarios can assert freedom with one helper.
#[allow(clippy::too_many_arguments)]
async fn freed_chord(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    state: &Entity<crate::window_state::WindowState>,
    session_id: &str,
    cap_path: &Path,
    failures: &mut Vec<String>,
    keystroke: &str,
) {
    let before = active_window_id(cx, state, session_id);
    let leaked = chord_leak(cx, window, cap_path, keystroke).await;
    if !leaked.is_empty() {
        failures.push(format!(
            "{keystroke} is freed but leaked \"{}\" to the pty",
            esc(&leaked)
        ));
    }
    let after = active_window_id(cx, state, session_id);
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

    // --- §4 the FREED chords do nothing (the 2026-08-11 revisions) ---------
    // ⌃⌘] / ⌃⌘[ were the shipped prev/next-pill defaults; the ladder moved that
    // pair onto ⌃⌘H/⌃⌘L. ⌃⌘U / ⌃⌘D were the half-page pair, which moved to the
    // arrows after macOS's dictionary hotkey turned out to swallow a real ⌃⌘D
    // before Nice saw it. All four are bound to nothing now — and unbound must
    // mean INERT, not "falls through and types", so assert both halves.
    //
    // NOTE the blind spot this leg does NOT cover: `dispatch_keystroke` injects
    // downstream of the OS hotkey layer, so a chord macOS intercepts still looks
    // live here. That is exactly how the ⌃⌘D defect reached a hand-test.
    let (state, session) = (fixture.state.clone(), fixture.session_id.clone());
    freed_chord(cx, window, &state, &session, &cap_path, &mut failures, "ctrl-cmd-]").await;
    freed_chord(cx, window, &state, &session, &cap_path, &mut failures, "ctrl-cmd-[").await;
    freed_chord(cx, window, &state, &session, &cap_path, &mut failures, "ctrl-cmd-u").await;
    freed_chord(cx, window, &state, &session, &cap_path, &mut failures, "ctrl-cmd-d").await;

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

    // --- §6 ⌃⌘↑ / ⌃⌘↓ half-page scrollback ---------------------------------
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
    // moves, the second doubles it (equal steps), and two ⌃⌘↓ undo them exactly.
    let mut half_leaks: Vec<(&str, Vec<u8>)> = Vec::new();
    let up1 = chord_leak(cx, window, &cap_path, "ctrl-cmd-up").await;
    half_leaks.push(("ctrl-cmd-up", up1));
    let off1 = offset(cx);
    if off1 == 0 {
        failures.push("ctrl-cmd-up: display offset stayed 0 (no half-page scroll)".into());
    }
    let up2 = chord_leak(cx, window, &cap_path, "ctrl-cmd-up").await;
    half_leaks.push(("ctrl-cmd-up", up2));
    let off2 = offset(cx);
    if off2 != off1 * 2 {
        failures.push(format!(
            "ctrl-cmd-up: second half-page landed at {off2}, expected {} (two equal steps)",
            off1 * 2
        ));
    }
    let down1 = chord_leak(cx, window, &cap_path, "ctrl-cmd-down").await;
    half_leaks.push(("ctrl-cmd-down", down1));
    if offset(cx) != off1 {
        failures.push(format!(
            "ctrl-cmd-down: landed at {}, expected {off1} (same magnitude, opposite sign)",
            offset(cx)
        ));
    }
    let down2 = chord_leak(cx, window, &cap_path, "ctrl-cmd-down").await;
    half_leaks.push(("ctrl-cmd-down", down2));
    if offset(cx) != 0 {
        failures.push(format!(
            "ctrl-cmd-down: viewport is at {} instead of parked at the bottom",
            offset(cx)
        ));
    }
    for (chord, leaked) in half_leaks {
        if !leaked.is_empty() {
            failures.push(format!("{chord}: leaked \"{}\" to the pty", esc(&leaked)));
        }
    }

    // --- §7 the differential: a plain `u` still reaches the pty -------------
    // Without this the zero-byte assertions above could pass vacuously (an
    // unwired keymap leaks nothing either).
    let start = cap_len(&cap_path);
    dispatch_key(cx, window, "u");
    settle(cx, 250).await;
    expect_bytes(&mut failures, "plain-u", b"u", &cap_since(&cap_path, start));

    // --- §8 alt screen: the half-page chords do nothing at all --------------
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
        let leaked = chord_leak(cx, window, &cap_path, "ctrl-cmd-up").await;
        if !leaked.is_empty() {
            failures.push(format!(
                "alt-screen ctrl-cmd-up: leaked \"{}\" to the pty",
                esc(&leaked)
            ));
        }
        if offset(cx) != 0 {
            failures.push("alt-screen: ctrl-cmd-up moved the viewport".into());
        }
        let _ = write_child(cx, &handle, b"\x1b[?1049l");
    }

    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "held-⌃⌘ scheme OK end to end: ⌃⌘L/⌃⌘H cycled the pills (wrapping), ⌃⌘1/⌃⌘2 \
                     jumped by index, ⌃⌘O bounced between the last two, the freed ⌃⌘]/⌃⌘[/⌃⌘U/⌃⌘D \
                     did nothing at all, ⌃⌘J/⌃⌘K stepped the sidebar sessions, ⌃⌘↑/⌃⌘↓ half-paged \
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

// -- scenario: splits (Phase 2 pane verbs, end to end) -----------------------

/// The pane area's painted size the `splits` scenario stashes on `WindowState`.
/// The scenario mounts ONE `TerminalView` (the `keybind-scheme` shape), not the
/// shipped `WindowHostView`, so nothing paints a pane tree here and nothing
/// writes that stash — but `SplitDown`/`SplitRight`'s P6 refusal and every
/// `ResizePane*` step are px-denominated against it, so the driver supplies it
/// explicitly. 1200×800 is roomy enough that no split is refused and the resize
/// legs have a long way to travel before they hit the P6 clamp.
const SPLITS_CONTENT_W: f32 = 1200.0;
const SPLITS_CONTENT_H: f32 = 800.0;

/// Everything [`run_splits`] needs from the setup phase.
struct SplitsFixture {
    state: Entity<crate::window_state::WindowState>,
    /// The capture-tee pty of the pill's FIRST pane — the pane the mounted view
    /// shows, and the one surface every zero-leak assertion measures.
    handle: Entity<TerminalSessionHandle>,
    session_id: String,
    /// The seeded pill (the only one until break-pane mints a second).
    window_id: String,
    /// The first pane's id. `TermWindow::new` makes it the window's own id, so
    /// a never-split pill's pane is the pill.
    pane0: String,
    /// Where the persistence leg installs its session store.
    store_path: PathBuf,
}

/// The `splits` scenario: Phase 2's pane verbs, end to end, over the SHIPPED
/// dispatch path — the `keybind-scheme` gate's Phase 2 twin, and built the same
/// way (real keymap + a `WindowState` in the `WindowRegistry` + a capture-tee
/// pty spawned THROUGH that state's `PtyManager`), for the same reason: a bare
/// view has no keymap and no state, so every pane chord would no-op and a
/// zero-leak assertion would pass vacuously.
///
/// Each chord is asserted on its EFFECT — the pane the model focused, the leaf
/// count, the split's ratio, the pill count — AND on the pty (`0` bytes
/// captured), with a plain `u` at the end as the differential proving the
/// capture file would have shown a leak. The split panes are REAL `zsh`
/// (`SpawnSpec::shell` has no fixture injection point), which is what makes the
/// close legs honest: a pane is closed by writing `exit\n` through its
/// `pane_handle` and the tree collapse is observed, not simulated.
///
/// What it deliberately does NOT cover, and why:
///
/// * **Paint.** No `WindowHostView` is mounted, so zoom, the focused-pane
///   affordance and the divider drags are model/unit-tested (`app_shell`'s own
///   tests) rather than asserted here; the driver stands in for the paint by
///   stashing [`SPLITS_CONTENT_W`]×[`SPLITS_CONTENT_H`] as the pane area.
/// * **The Claude-pane refusals (P3).** Standing a real Claude up is not
///   hermetic; `keymap`'s and `pty_manager`'s unit tests pin
///   break-pane-refuses-the-Claude-leaf. The refusals this scenario CAN prove
///   honestly — zoom and break-pane on a single-leaf pill — it does prove.
/// * **Chord DELIVERY.** `dispatch_keystroke` injects downstream of the OS
///   hotkey layer, so a chord macOS itself swallows still looks live here (the
///   `⌃⌘D` lesson). Sixteen of Phase 2's chords are Hyper-cluster rungs; only
///   the hand feel-check can gate those.
pub fn open_splits_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    use crate::pty_manager::WindowShellEnv;
    use crate::window_registry::WindowRegistry;
    use crate::window_state::WindowState;

    let base = prepare_dir("splits")?;
    let cap_path = base.join("capture.bin");
    let base_s = base.to_string_lossy().to_string();
    let cap_s = cap_path.to_string_lossy().to_string();
    let store_path = base.join("sessions.json");

    // The first pane is the capture-tee child (the `input-live` pattern): every
    // byte the view sends lands in the capture file verbatim, so "this chord
    // leaked nothing" is a file-length assertion rather than a guess.
    let inner = format!("stty raw -echo; exec tee {cap_s}");
    let spec = SpawnSpec::command(format!("sh -c '{inner}'"), base_s.clone())
        .with_env(vec![("ZDOTDIR".to_string(), base_s.clone())])
        .with_size(ROWS, COLS);

    let fixture = cx.update(|app| -> Result<SplitsFixture> {
        crate::keymap::install_shortcuts(app);
        app.set_global(crate::shortcuts_store::ShortcutBindings::with_defaults(
            base.join("ui_settings.json"),
        ));
        crate::keymap::rebuild_keymap(app);

        let state = app.new(|_cx| WindowState::new(base_s.clone()));
        let (session_id, window_id) = state.update(app, |s, _cx| {
            // Split panes are real login shells. Point their `ZDOTDIR` at the
            // scenario's own (empty) dir so no user rc runs in them — the
            // `input-shell` hermeticity trick, applied to the panes the split
            // verb spawns for itself. (Handed over as raw injection pairs: this
            // scenario never bootstraps a `ShellRuntime`, so the panes spawn as
            // the historical plain zsh and only this `ZDOTDIR` rides along.)
            s.ptys.set_window_shell_env(WindowShellEnv {
                socket_path: None,
                inject_pairs: vec![("ZDOTDIR".to_string(), base_s.clone())],
                compose_conf: None,
            });
            let session_id = s
                .workspace
                .active_session_id()
                .map(str::to_owned)
                .unwrap_or_default();
            let window_id = s
                .workspace
                .session_for(&session_id)
                .and_then(|sess| sess.windows.first().map(|w| w.id.clone()))
                .unwrap_or_default();
            (session_id, window_id)
        });
        if session_id.is_empty() || window_id.is_empty() {
            return Err(anyhow!("splits: a fresh WindowState seeded no pill"));
        }

        state.update(app, |s, cx| {
            s.ptys.spawn_window(&session_id, &window_id, spec, cx)
        })?;
        state.update(app, |s, _cx| {
            s.window_strip_actions
                .select_window(&mut s.workspace, &session_id, &window_id);
            // Stand in for the shipped host's painted-size stash (see
            // `SPLITS_CONTENT_W`).
            s.set_pane_content_size(Some((SPLITS_CONTENT_W, SPLITS_CONTENT_H)));
        });
        let pane0 = state
            .read(app)
            .workspace
            .session_for(&session_id)
            .and_then(|sess| sess.windows.iter().find(|w| w.id == window_id))
            .map(|w| w.effective_pane_id())
            .ok_or_else(|| anyhow!("splits: the seeded pill vanished"))?;
        let handle = state
            .read(app)
            .ptys
            .pane_handle(&session_id, &window_id, &pane0)
            .ok_or_else(|| anyhow!("splits: the seeded pane has no pty handle"))?;

        Ok(SplitsFixture {
            state,
            handle,
            session_id,
            window_id,
            pane0,
            store_path,
        })
    })?;

    let terminal = make_view(fixture.handle.clone(), cx);

    let whandle = cx.open_window(crate::app::window_options(), {
        let terminal = terminal.clone();
        let state = fixture.state.clone();
        move |window, cx| {
            // `register`, not `install` — the close observer's quit-when-empty
            // would end the suite when this window closes (`keybind-scheme`).
            let id = window.window_handle().window_id();
            WindowRegistry::register(cx, id, state.clone());
            cx.new(|_cx| InputTermView { terminal })
        }
    })?;
    let window: AnyWindowHandle = whandle.into();
    crate::app::install_present_kick(&fixture.handle, window, cx);

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_splits(acx, window, terminal, fixture, cap_path).await;
        eprintln!("[selftest] scenario 'splits': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(window)
}

/// The pill under test, cloned out of the model the pane verbs mutate.
fn splits_pill(cx: &mut AsyncApp, fixture: &SplitsFixture) -> Option<nice_model::TermWindow> {
    splits_pill_by_id(cx, fixture, &fixture.window_id)
}

fn splits_pill_by_id(
    cx: &mut AsyncApp,
    fixture: &SplitsFixture,
    window_id: &str,
) -> Option<nice_model::TermWindow> {
    fixture.state.update(cx, |s, _cx| {
        s.workspace
            .session_for(&fixture.session_id)?
            .windows
            .iter()
            .find(|w| w.id == window_id)
            .cloned()
    })
}

/// The session's pill ids, in strip order.
fn splits_pill_ids(cx: &mut AsyncApp, fixture: &SplitsFixture) -> Vec<String> {
    fixture.state.update(cx, |s, _cx| {
        s.workspace
            .session_for(&fixture.session_id)
            .map(|sess| sess.windows.iter().map(|w| w.id.clone()).collect())
            .unwrap_or_default()
    })
}

/// One split node's ratio, by its path from the tree root.
fn ratio_at(window: &nice_model::TermWindow, path: &[nice_model::Side]) -> Option<f32> {
    match window.layout.node_at(path) {
        Some(nice_model::PaneLayout::Split { ratio, .. }) => Some(*ratio),
        _ => None,
    }
}

/// Everything a chord that must change NOTHING could disturb: the tree
/// (structure, ids and ratios) plus which leaf holds focus.
fn layout_fingerprint(window: &nice_model::TermWindow) -> String {
    format!("{:?}@{}", window.layout, window.active_pane_id)
}

/// Assert a float landed within `eps` of `want`.
fn expect_close(failures: &mut Vec<String>, label: &str, got: f32, want: f32, eps: f32) {
    if (got - want).abs() > eps {
        failures.push(format!("{label}: ratio is {got}, expected {want} (±{eps})"));
    }
}

/// Dispatch a pane chord, then assert BOTH halves: focus landed on `want_pane`
/// and the chord wrote nothing to the pty. The pane-level twin of
/// [`nav_chord`] — same shape, one level down the tree.
#[allow(clippy::too_many_arguments)]
async fn pane_chord(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    fixture: &SplitsFixture,
    cap_path: &Path,
    failures: &mut Vec<String>,
    keystroke: &str,
    want_pane: &str,
    label: &str,
) {
    let leaked = chord_leak(cx, window, cap_path, keystroke).await;
    if !leaked.is_empty() {
        failures.push(format!(
            "{keystroke} ({label}): leaked \"{}\" to the pty",
            esc(&leaked)
        ));
    }
    let got = splits_pill(cx, fixture).map(|w| w.active_pane_id);
    if got.as_deref() != Some(want_pane) {
        failures.push(format!(
            "{keystroke} ({label}): focused pane is {got:?}, expected {want_pane}"
        ));
    }
}

/// Poll `check` until it holds or the tries run out; returns whether it held.
async fn poll_until(
    cx: &mut AsyncApp,
    tries: usize,
    ms: u64,
    mut check: impl FnMut(&mut AsyncApp) -> bool,
) -> bool {
    for _ in 0..tries {
        if check(cx) {
            return true;
        }
        settle(cx, ms).await;
    }
    check(cx)
}

/// Wait for a split pane's login shell to print SOMETHING (its prompt) before
/// typing at it — a fixed sleep races `zsh`'s startup.
async fn splits_shell_ready(
    cx: &mut AsyncApp,
    fixture: &SplitsFixture,
    window_id: &str,
    pane_id: &str,
) -> bool {
    let (window_id, pane_id) = (window_id.to_string(), pane_id.to_string());
    let session_id = fixture.session_id.clone();
    let state = fixture.state.clone();
    poll_until(cx, 60, 100, move |cx| {
        let handle = state.update(cx, |s, _cx| {
            s.ptys.pane_handle(&session_id, &window_id, &pane_id)
        });
        let Some(handle) = handle else {
            return false;
        };
        handle
            .update(cx, |h, _| h.session().grid_lines().join(""))
            .chars()
            .any(|c| !c.is_whitespace())
    })
    .await
}

/// One pane's whole visible grid, joined — what the driver polls when it needs
/// to know a pane's shell has caught up with what it was told to run.
fn splits_grid(
    cx: &mut AsyncApp,
    fixture: &SplitsFixture,
    window_id: &str,
    pane_id: &str,
) -> String {
    let handle = fixture.state.update(cx, |s, _cx| {
        s.ptys
            .pane_handle(&fixture.session_id, window_id, pane_id)
    });
    match handle {
        Some(handle) => handle.update(cx, |h, _| h.session().grid_lines().join("\n")),
        None => String::new(),
    }
}

/// One pane's viewport offset: `0` is parked at the live bottom, anything above
/// it means that pane is showing scrollback. The observable the half-page verb
/// writes, read per-PANE so "which pane did it scroll" is answerable.
fn splits_display_offset(
    cx: &mut AsyncApp,
    fixture: &SplitsFixture,
    window_id: &str,
    pane_id: &str,
) -> Option<usize> {
    let handle = fixture.state.update(cx, |s, _cx| {
        s.ptys
            .pane_handle(&fixture.session_id, window_id, pane_id)
    })?;
    Some(handle.update(cx, |h, _| h.display_offset()))
}

/// Run `exit 0` in one pane's shell — the only way to close a split pane from a
/// driver, since `SpawnSpec::shell` has no fixture injection point.
///
/// The status is SPELLED OUT, and that is load-bearing. A bare `exit` returns
/// `$?`, which for a login `zsh` is whatever `/etc/zshrc` last left behind — and
/// a non-zero status is a HELD exit (`should_hold_on_exit`: any non-zero code
/// holds), which deliberately keeps the pane and its corpse on screen instead of
/// collapsing the tree. This leg is testing the CLEAN-exit path, so it must ask
/// for a clean exit rather than inherit one.
fn splits_exit_pane(
    cx: &mut AsyncApp,
    fixture: &SplitsFixture,
    window_id: &str,
    pane_id: &str,
) -> Result<()> {
    let handle = fixture
        .state
        .update(cx, |s, _cx| {
            s.ptys
                .pane_handle(&fixture.session_id, window_id, pane_id)
        })
        .ok_or_else(|| anyhow!("no pty for pane {pane_id}"))?;
    write_child(cx, &handle, b"exit 0\r")
}

async fn run_splits(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    terminal: Entity<TerminalView>,
    fixture: SplitsFixture,
    cap_path: PathBuf,
) -> CadenceReport {
    use nice_model::Side;

    let _ = cx.update(|app| app.activate(true));
    settle(cx, 500).await;

    // Focus the mounted view — `dispatch_keystroke` walks the focus path, and
    // the plain-`u` differential needs the view's own input path live.
    let _ = window.update(cx, |_root, window, cx| {
        let fh = terminal.read(cx).focus_handle_ref().clone();
        window.focus(&fh, cx);
    });
    settle(cx, 200).await;

    let mut failures: Vec<String> = Vec::new();
    let pane0 = fixture.pane0.clone();

    // Precondition: one pill, one leaf, focused on it — a never-split pill.
    match splits_pill(cx, &fixture) {
        Some(pill) if pill.layout.leaf_count() == 1 && pill.active_pane_id == pane0 => {}
        other => {
            return CadenceReport::error(format!(
                "splits: the seeded pill is not a single focused leaf: {other:?}"
            ));
        }
    }

    // --- §1 ⌃⌘\ and ⌃⌘- split the pill (D2's divider mnemonics) ------------
    // Focus follows the NEW pane (tmux), so the second split bisects the first
    // split's product — the tree ends up mixed-orientation:
    //   Beside{ pane0, Stacked{ pane1, pane2 } }.
    let leaked = chord_leak(cx, window, &cap_path, "cmd-ctrl-\\").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl-\\: leaked \"{}\"", esc(&leaked)));
    }
    let Some(pane1) = splits_pill(cx, &fixture).and_then(|pill| {
        (pill.layout.leaf_count() == 2 && pill.active_pane_id != pane0)
            .then(|| pill.active_pane_id.clone())
    }) else {
        return CadenceReport::error(
            "splits: ⌃⌘\\ did not split the pill in two with focus on the new pane".to_string(),
        );
    };
    let leaked = chord_leak(cx, window, &cap_path, "cmd-ctrl--").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl--: leaked \"{}\"", esc(&leaked)));
    }
    let Some((pane2, leaves)) = splits_pill(cx, &fixture).map(|pill| {
        (
            pill.active_pane_id.clone(),
            pill.layout
                .leaves()
                .iter()
                .map(|p| p.id.clone())
                .collect::<Vec<_>>(),
        )
    }) else {
        return CadenceReport::error("splits: the pill vanished mid-split".to_string());
    };
    if leaves != vec![pane0.clone(), pane1.clone(), pane2.clone()] {
        return CadenceReport::error(format!(
            "splits: expected leaves [{pane0}, {pane1}, {pane2}] after two splits, got {leaves:?}"
        ));
    }
    if let Some(pill) = splits_pill(cx, &fixture) {
        if ratio_at(&pill, &[]).is_none() || ratio_at(&pill, &[Side::Second]).is_none() {
            failures.push(format!(
                "splits: the tree is not Beside{{leaf, Stacked{{leaf, leaf}}}}: {:?}",
                pill.layout
            ));
        }
        if !pill.layout_is_valid() {
            failures.push("splits: the split tree violates its own invariants".into());
        }
    }

    // The px the two dividers actually divide (the same arithmetic
    // `split_available_px` performs, spelled out so the expectations below are
    // independent of the code under test).
    let across = SPLITS_CONTENT_W - crate::app_shell::PANE_DIVIDER_PX;
    let down = SPLITS_CONTENT_H - crate::app_shell::PANE_DIVIDER_PX;
    let step_down = 40.0 / down;
    let min_down = crate::app_shell::PANE_MIN_HEIGHT / down;

    // --- §2 ⌃⌥⌘k walks the stacked divider, and stops at the P6 minimum ----
    let leaked = chord_leak(cx, window, &cap_path, "cmd-ctrl-alt-k").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl-alt-k: leaked \"{}\"", esc(&leaked)));
    }
    if let Some(pill) = splits_pill(cx, &fixture) {
        if let Some(ratio) = ratio_at(&pill, &[Side::Second]) {
            // One 40 px step off the even split, denominated in the px that
            // divider divides — not a fixed ratio nudge.
            expect_close(
                &mut failures,
                "⌃⌥⌘k first step",
                ratio,
                0.5 - step_down,
                1e-4,
            );
        }
        if let Some(root) = ratio_at(&pill, &[]) {
            expect_close(
                &mut failures,
                "⌃⌥⌘k left the BESIDE ancestor alone (P7)",
                root,
                0.5,
                1e-6,
            );
        }
    }
    // Seven more steps overshoot the minimum-height band, so the eighth lands
    // ON the clamp instead of below it.
    for _ in 0..7 {
        let leaked = chord_leak(cx, window, &cap_path, "cmd-ctrl-alt-k").await;
        if !leaked.is_empty() {
            failures.push(format!("cmd-ctrl-alt-k: leaked \"{}\"", esc(&leaked)));
        }
    }
    let pinned = splits_pill(cx, &fixture).and_then(|p| ratio_at(&p, &[Side::Second]));
    if let Some(ratio) = pinned {
        expect_close(
            &mut failures,
            "⌃⌥⌘k clamped at PANE_MIN_HEIGHT (P6)",
            ratio,
            min_down,
            1e-3,
        );
    }
    // A chord against the clamp is a no-op, not a slow drift past it.
    let leaked = chord_leak(cx, window, &cap_path, "cmd-ctrl-alt-k").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl-alt-k: leaked \"{}\"", esc(&leaked)));
    }
    let after_clamp = splits_pill(cx, &fixture).and_then(|p| ratio_at(&p, &[Side::Second]));
    if after_clamp != pinned {
        failures.push(format!(
            "⌃⌥⌘k moved a divider already pinned at the minimum: {pinned:?} -> {after_clamp:?}"
        ));
    }

    // --- §3 ⌃⌘⇧hjkl walks focus spatially; the edges are no-ops (P5) -------
    pane_chord(
        cx,
        window,
        &fixture,
        &cap_path,
        &mut failures,
        "cmd-ctrl-shift-k",
        &pane1,
        "up to the pane above",
    )
    .await;
    pane_chord(
        cx,
        window,
        &fixture,
        &cap_path,
        &mut failures,
        "cmd-ctrl-shift-j",
        &pane2,
        "back down",
    )
    .await;
    pane_chord(
        cx,
        window,
        &fixture,
        &cap_path,
        &mut failures,
        "cmd-ctrl-shift-h",
        &pane0,
        "left into the full-height pane",
    )
    .await;
    // P5: no wrap, and no fall-through to pill nav — bare ⌃⌘h is how you leave
    // the pill, so the pane rung simply stops.
    pane_chord(
        cx,
        window,
        &fixture,
        &cap_path,
        &mut failures,
        "cmd-ctrl-shift-h",
        &pane0,
        "the left edge is a no-op",
    )
    .await;
    pane_chord(
        cx,
        window,
        &fixture,
        &cap_path,
        &mut failures,
        "cmd-ctrl-shift-j",
        &pane0,
        "nothing below a full-height pane",
    )
    .await;
    // Two panes sit to the right; the one sharing the longer edge wins (the
    // stacked divider is pinned near the top, so that is the bottom one).
    pane_chord(
        cx,
        window,
        &fixture,
        &cap_path,
        &mut failures,
        "cmd-ctrl-shift-l",
        &pane2,
        "right, by largest shared edge",
    )
    .await;

    // --- §4 ⌃⌥⌘h reaches the BESIDE ancestor, not the stacked one (P7) -----
    let leaked = chord_leak(cx, window, &cap_path, "cmd-ctrl-alt-h").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl-alt-h: leaked \"{}\"", esc(&leaked)));
    }
    if let Some(pill) = splits_pill(cx, &fixture) {
        if let Some(root) = ratio_at(&pill, &[]) {
            expect_close(
                &mut failures,
                "⌃⌥⌘h stepped the root divider left",
                root,
                0.5 - 40.0 / across,
                1e-4,
            );
        }
        if let (Some(inner), Some(want)) = (ratio_at(&pill, &[Side::Second]), pinned) {
            expect_close(
                &mut failures,
                "⌃⌥⌘h left the STACKED divider alone (P7)",
                inner,
                want,
                1e-6,
            );
        }
    }

    // --- §5 ⌃⌥⌘⇧k swaps payloads; focus follows the content (P8) -----------
    let before =
        splits_pill(cx, &fixture).map(|p| (ratio_at(&p, &[]), ratio_at(&p, &[Side::Second])));
    let leaked = chord_leak(cx, window, &cap_path, "cmd-ctrl-alt-shift-k").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl-alt-shift-k: leaked \"{}\"", esc(&leaked)));
    }
    if let Some(pill) = splits_pill(cx, &fixture) {
        let leaves: Vec<String> = pill.layout.leaves().iter().map(|p| p.id.clone()).collect();
        if leaves != vec![pane0.clone(), pane2.clone(), pane1.clone()] {
            failures.push(format!(
                "⌃⌥⌘⇧k: expected the two right-hand payloads to trade slots, got {leaves:?}"
            ));
        }
        if pill.active_pane_id != pane2 {
            failures.push(format!(
                "⌃⌥⌘⇧k: focus should follow the content ({pane2}), got {}",
                pill.active_pane_id
            ));
        }
        let now = Some((ratio_at(&pill, &[]), ratio_at(&pill, &[Side::Second])));
        if now != before {
            failures.push(format!(
                "⌃⌥⌘⇧k moved the structure it was only supposed to re-fill: {before:?} -> {now:?}"
            ));
        }
    }
    // Swap back, so the geometry the later legs reason about is the §4 one.
    let leaked = chord_leak(cx, window, &cap_path, "cmd-ctrl-alt-shift-j").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl-alt-shift-j: leaked \"{}\"", esc(&leaked)));
    }
    if let Some(pill) = splits_pill(cx, &fixture) {
        let leaves: Vec<String> = pill.layout.leaves().iter().map(|p| p.id.clone()).collect();
        if leaves != vec![pane0.clone(), pane1.clone(), pane2.clone()] {
            failures.push(format!("⌃⌥⌘⇧j: the swap did not undo itself, got {leaves:?}"));
        }
    }

    // --- §6 ⌃⌘z zooms; the next focus move un-zooms and applies (P4) -------
    let leaked = chord_leak(cx, window, &cap_path, "cmd-ctrl-z").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl-z: leaked \"{}\"", esc(&leaked)));
    }
    if splits_pill(cx, &fixture).is_some_and(|p| !p.zoomed) {
        failures.push("⌃⌘z did not zoom a 3-pane pill".into());
    }
    pane_chord(
        cx,
        window,
        &fixture,
        &cap_path,
        &mut failures,
        "cmd-ctrl-shift-h",
        &pane0,
        "a focus move out of a zoom",
    )
    .await;
    if splits_pill(cx, &fixture).is_some_and(|p| p.zoomed) {
        failures.push("P4: the focus move should have un-zoomed first".into());
    }

    // --- §7 ⌃⌘b breaks a shell pane out into a pill of its own (P3) --------
    pane_chord(
        cx,
        window,
        &fixture,
        &cap_path,
        &mut failures,
        "cmd-ctrl-shift-l",
        &pane2,
        "back onto a shell pane",
    )
    .await;
    let leaked = chord_leak(cx, window, &cap_path, "cmd-ctrl-b").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl-b: leaked \"{}\"", esc(&leaked)));
    }
    let pills = splits_pill_ids(cx, &fixture);
    let Some(broken_out) = pills.iter().find(|id| **id != fixture.window_id).cloned() else {
        return CadenceReport::error(format!(
            "splits: ⌃⌘b minted no second pill (pills: {pills:?})"
        ));
    };
    if pills.len() != 2 || pills.first() != Some(&fixture.window_id) {
        failures.push(format!(
            "⌃⌘b: expected the new pill right after the source one, got {pills:?}"
        ));
    }
    if active_window_id(cx, &fixture.state, &fixture.session_id).as_deref() != Some(&broken_out) {
        failures.push("⌃⌘b: focus should follow the pane out to its new pill".into());
    }
    if let Some(moved) = splits_pill_by_id(cx, &fixture, &broken_out) {
        if moved.layout.single_leaf().map(|p| p.id.clone()) != Some(pane2.clone()) {
            failures.push(format!(
                "⌃⌘b: the new pill should be the moved pane alone, got {:?}",
                moved.layout
            ));
        }
    }
    if let Some(source) = splits_pill(cx, &fixture) {
        if source.layout.leaf_count() != 2 {
            failures.push(format!(
                "⌃⌘b: the source pill should have collapsed to two leaves, got {:?}",
                source.layout
            ));
        }
        // Spatial refocus, not index-neighbor: the departed pane shared its
        // whole left edge with pane0 and only its top edge with pane1.
        if source.active_pane_id != pane0 {
            failures.push(format!(
                "⌃⌘b: the source pill should refocus the shared-edge neighbor ({pane0}), got {}",
                source.active_pane_id
            ));
        }
    }
    // The refusals a hermetic scenario CAN prove: both verbs decline on the
    // single-leaf pill they just made (the Claude-pane refusal is unit-tested).
    let leaked = chord_leak(cx, window, &cap_path, "cmd-ctrl-z").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl-z: leaked \"{}\"", esc(&leaked)));
    }
    if splits_pill_by_id(cx, &fixture, &broken_out).is_some_and(|p| p.zoomed) {
        failures.push("⌃⌘z zoomed a single-pane pill (nothing to zoom)".into());
    }
    let leaked = chord_leak(cx, window, &cap_path, "cmd-ctrl-b").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl-b: leaked \"{}\"", esc(&leaked)));
    }
    if splits_pill_ids(cx, &fixture).len() != 2 {
        failures.push("⌃⌘b broke the only pane out of a single-pane pill".into());
    }

    // --- §8 the layout round-trips through the session store ---------------
    persistence_checks(cx, &fixture, &pane0, &pane1, &mut failures);

    // --- §9 a pane exits: the tree collapses, the last leaf closes the pill -
    if !splits_shell_ready(cx, &fixture, &broken_out, &pane2).await {
        failures.push("the broken-out pane's shell never printed a prompt".into());
    } else if let Err(e) = splits_exit_pane(cx, &fixture, &broken_out, &pane2) {
        failures.push(format!("could not write `exit` to the broken-out pane: {e}"));
    } else {
        let gone = {
            let fixture = &fixture;
            poll_until(cx, 80, 100, move |cx| splits_pill_ids(cx, fixture).len() == 1).await
        };
        if !gone {
            failures.push("exiting the LAST pane of a pill did not close the pill".into());
        } else if active_window_id(cx, &fixture.state, &fixture.session_id).as_deref()
            != Some(fixture.window_id.as_str())
        {
            failures.push("the pill close did not refocus the surviving pill".into());
        }
        // The pty map must lose the pane with the pill — a surviving entry is a
        // leaked handle whose events would route to a window that is gone.
        let still_keyed = fixture.state.update(cx, |s, _cx| {
            s.ptys.has_pane(&fixture.session_id, &broken_out, &pane2)
        });
        if still_keyed {
            failures.push("the closed pill left its pane handle in the pty map".into());
        }
    }

    // The surviving pill still holds two panes; close the focused one and the
    // tree must collapse onto its shared-edge neighbor rather than die with it.
    pane_chord(
        cx,
        window,
        &fixture,
        &cap_path,
        &mut failures,
        "cmd-ctrl-shift-l",
        &pane1,
        "onto the pane about to exit",
    )
    .await;

    // --- §9a ⌃⌘↑/⌃⌘↓ half-page the pane the user is LOOKING at ------------
    // Phase 1's chord is pill-scoped, so WHICH pane it lands on is a resolution
    // question splits made real — and `⌃⌘⇧hjkl` moves focus without ever
    // re-activating the pill, so any pty-side "active pane" cache still names
    // the pane focus left behind. Both halves are asserted: the focused pane
    // scrolled, and the previously-focused one did not move at all.
    scroll_targets_focused_pane(cx, window, &fixture, &cap_path, &pane0, &pane1, &mut failures)
        .await;

    if !splits_shell_ready(cx, &fixture, &fixture.window_id.clone(), &pane1).await {
        failures.push("the split pane's shell never printed a prompt".into());
    } else if let Err(e) = splits_exit_pane(cx, &fixture, &fixture.window_id.clone(), &pane1) {
        failures.push(format!("could not write `exit` to the split pane: {e}"));
    } else {
        let collapsed = {
            let fixture = &fixture;
            poll_until(cx, 80, 100, move |cx| {
                splits_pill(cx, fixture).is_some_and(|p| p.layout.leaf_count() == 1)
            })
            .await
        };
        if !collapsed {
            failures.push("a pane exit did not collapse the split".into());
        }
        match splits_pill(cx, &fixture) {
            Some(pill) => {
                if pill.active_pane_id != pane0 {
                    failures.push(format!(
                        "spatial refocus picked {} after the exit, expected {pane0}",
                        pill.active_pane_id
                    ));
                }
                if !pill.is_alive || !pill.layout_is_valid() {
                    failures.push("the surviving pill is not a valid, live single-leaf pill".into());
                }
            }
            None => failures.push("the surviving pill went away with its second pane".into()),
        }
        if splits_pill_ids(cx, &fixture).len() != 1 {
            failures.push("a non-last pane exit changed the pill count".into());
        }
    }

    // --- §10 ⌃⌘v / ⌃⌘s are freed, not re-spent (D2) ------------------------
    // The split verbs took the divider mnemonics instead, so the two chords the
    // roadmap once penciled in for splits end bound to nothing — and unbound
    // must mean INERT, for the tree as well as for the pty.
    let before = splits_pill(cx, &fixture).map(|p| layout_fingerprint(&p));
    let (state, session) = (fixture.state.clone(), fixture.session_id.clone());
    freed_chord(cx, window, &state, &session, &cap_path, &mut failures, "cmd-ctrl-v").await;
    freed_chord(cx, window, &state, &session, &cap_path, &mut failures, "cmd-ctrl-s").await;
    let after = splits_pill(cx, &fixture).map(|p| layout_fingerprint(&p));
    if after != before {
        failures.push(format!(
            "the freed ⌃⌘v/⌃⌘s changed the pane tree: {before:?} -> {after:?}"
        ));
    }

    // --- §11 the differential: a plain `u` still reaches the pty -----------
    let start = cap_len(&cap_path);
    dispatch_key(cx, window, "u");
    settle(cx, 250).await;
    expect_bytes(&mut failures, "plain-u", b"u", &cap_since(&cap_path, start));

    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "Phase 2 pane verbs OK end to end: ⌃⌘\\ / ⌃⌘- built a mixed-orientation tree, \
                     ⌃⌥⌘k walked the stacked divider by a px step and stopped at the P6 minimum, \
                     ⌃⌘⇧hjkl walked focus spatially with inert edges, ⌃⌥⌘h moved only the beside \
                     ancestor, ⌃⌥⌘⇧k/j traded payloads without moving the structure, ⌃⌘z zoomed \
                     until a focus move un-zoomed it, ⌃⌘b broke a shell out into its own pill (and \
                     declined on the single-leaf one it made), the layout round-tripped through \
                     the store while a mangled one fell back to a single leaf, `exit 0` collapsed \
                     a split then closed a pill, ⌃⌘↑/⌃⌘↓ half-paged the FOCUSED pane (leaving the \
                     one focus had left behind parked), and ⌃⌘v/⌃⌘s did nothing at all — every \
                     chord silent to the pty while a plain `u` still encoded"
                .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!("splits FAILED:\n  - {}", failures.join("\n  - ")),
        }
    }
}

/// §9a: `⌃⌘↑` must scroll the pane the user is looking at, and only that one.
///
/// The chord is a Phase 1 keymap action with a `(session, window)` scope, so
/// splits turned it into a resolution question: the pane it lands on has to be
/// read from the MODEL's focus, because focus moves (`⌃⌘⇧hjkl`, a pane click, a
/// split) never re-activate the pill and so never refresh anything the pty side
/// might have cached. Landing on the pane focus left behind would scroll a
/// terminal that, while zoomed, is not even on screen.
///
/// The focused pane gets 200 lines of real `seq` output first — a pane with no
/// scrollback cannot scroll, and would pass this vacuously.
#[allow(clippy::too_many_arguments)]
async fn scroll_targets_focused_pane(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    fixture: &SplitsFixture,
    cap_path: &Path,
    left_behind: &str,
    focused: &str,
    failures: &mut Vec<String>,
) {
    let window_id = fixture.window_id.clone();
    if !splits_shell_ready(cx, fixture, &window_id, focused).await {
        failures.push("⌃⌘↑: the focused pane's shell never printed a prompt".into());
        return;
    }
    let handle = fixture.state.update(cx, |s, _cx| {
        s.ptys
            .pane_handle(&fixture.session_id, &window_id, focused)
    });
    let Some(handle) = handle else {
        failures.push("⌃⌘↑: the focused pane has no pty handle".into());
        return;
    };
    if let Err(e) = write_child(cx, &handle, b"seq 1 200\r") {
        failures.push(format!(
            "⌃⌘↑: could not fill the focused pane's scrollback: {e}"
        ));
        return;
    }
    let printed = poll_until(cx, 60, 100, |cx| {
        splits_grid(cx, fixture, &window_id, focused).contains("200")
    })
    .await;
    if !printed {
        failures.push("⌃⌘↑: the focused pane never printed the 200 lines it was given".into());
        return;
    }

    let leaked = chord_leak(cx, window, cap_path, "cmd-ctrl-up").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl-up: leaked \"{}\"", esc(&leaked)));
    }
    let moved = splits_display_offset(cx, fixture, &window_id, focused);
    let parked = splits_display_offset(cx, fixture, &window_id, left_behind);
    if moved.is_none_or(|offset| offset == 0) {
        failures.push(format!(
            "⌃⌘↑: the focused pane did not scroll (display_offset {moved:?})"
        ));
    }
    if parked != Some(0) {
        failures.push(format!(
            "⌃⌘↑ scrolled a pane the user is not looking at: the pane focus left behind is at \
             display_offset {parked:?}"
        ));
    }

    // ⌃⌘↓ undoes it on the same pane, which also parks the viewport back at the
    // bottom for the exit leg that follows.
    let leaked = chord_leak(cx, window, cap_path, "cmd-ctrl-down").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl-down: leaked \"{}\"", esc(&leaked)));
    }
    let back = splits_display_offset(cx, fixture, &window_id, focused);
    if back != Some(0) {
        failures.push(format!(
            "⌃⌘↓ did not undo ⌃⌘↑ on the focused pane (display_offset {back:?})"
        ));
    }
}

/// §8: snapshot the live model into a real [`SessionStore`] on disk, read it
/// back, and assert the pane tree survived the trip — plus the loader-tolerance
/// half: a layout whose focused leaf does not exist hydrates as a single-leaf
/// pill instead of failing the load (a session file that fails to load loses the
/// user's work, so the loader may never error).
///
/// The store global is installed only for this leg and cleared immediately, so
/// no other scenario's `save_to_store` lands in this scenario's temp file.
///
/// [`SessionStore`]: crate::session_store::SessionStore
fn persistence_checks(
    cx: &mut AsyncApp,
    fixture: &SplitsFixture,
    pane0: &str,
    pane1: &str,
    failures: &mut Vec<String>,
) {
    use crate::session_store::{self, SessionStore};

    session_store::install_global(SessionStore::open(fixture.store_path.clone()));
    fixture.state.update(cx, |s, _cx| s.save_to_store());
    session_store::flush();
    let saved = session_store::read_state(&fixture.store_path);
    session_store::clear_global();

    let Some(session) = saved
        .windows
        .iter()
        .flat_map(|w| w.projects.iter())
        .flat_map(|p| p.sessions.iter())
        .find(|s| s.id == fixture.session_id)
    else {
        failures.push("persistence: the seeded session never reached the store".into());
        return;
    };
    let Some(persisted) = session.windows.iter().find(|w| w.id == fixture.window_id) else {
        failures.push("persistence: the split pill never reached the store".into());
        return;
    };

    // A never-split pill writes NO layout keys at all — that is what keeps
    // `sessions.json` byte-identical for everyone who never splits.
    if let Some(single) = session.windows.iter().find(|w| w.id != fixture.window_id) {
        if single.layout.is_some() || single.active_leaf_id.is_some() {
            failures.push("persistence: a single-leaf pill wrote layout keys".into());
        }
    }

    if persisted.layout.is_none() || persisted.active_leaf_id.as_deref() != Some(pane0) {
        failures.push(format!(
            "persistence: the split pill wrote layout {:?} / activeLeafId {:?}",
            persisted.layout, persisted.active_leaf_id
        ));
        return;
    }

    let restored = persisted.hydrate();
    let leaves: Vec<String> = restored
        .layout
        .leaves()
        .iter()
        .map(|p| p.id.clone())
        .collect();
    if leaves != vec![pane0.to_string(), pane1.to_string()] {
        failures.push(format!(
            "persistence: the tree came back as {leaves:?}, expected [{pane0}, {pane1}]"
        ));
    }
    if restored.active_pane_id != pane0 {
        failures.push(format!(
            "persistence: the focused leaf came back as {}",
            restored.active_pane_id
        ));
    }
    let live = splits_pill(cx, fixture).and_then(|p| ratio_at(&p, &[]));
    if let (Some(live), Some(back)) = (live, ratio_at(&restored, &[])) {
        expect_close(failures, "persistence: the ratio round-tripped", back, live, 1e-6);
    }

    // Loader tolerance: point the focused leaf at a pane that isn't in the tree
    // (the shape a hand-edited or truncated file lands in) — the window must
    // still hydrate, as the single-leaf pill Nice has always restored.
    let mut mangled = persisted.clone();
    mangled.active_leaf_id = Some("no-such-pane".to_string());
    let fallback = mangled.hydrate();
    if fallback.layout.leaf_count() != 1 || !fallback.layout_is_valid() {
        failures.push(format!(
            "persistence: a mangled layout hydrated as {:?} instead of a single leaf",
            fallback.layout
        ));
    }
}

// -- scenario: copy-mode (Phase 3 copy mode + scrollback search) --------------

/// Seeded output lines for the copy-mode legs — eight screens' worth at the
/// spawn size, so history stays deep even after the mounted view refits the pty
/// to the (taller) real window, and `g`/`⌃u` have somewhere to go.
const COPY_SEED_LINES: usize = ROWS as usize * 8;

/// The token the search legs look for. Deliberately not a substring of the
/// `copyline-N` body, so "how many matches" is exactly
/// [`COPY_NEEDLE_LINES`]`.len()`.
const COPY_NEEDLE: &str = "needlezz";

/// Which seeded lines carry [`COPY_NEEDLE`]. THREE of them, spread through the
/// history: two is not enough to tell `n` (keep going) from `N` (come back) —
/// with two matches both verbs land on the same other match.
const COPY_NEEDLE_LINES: [usize; 3] = [20, 80, 140];

/// Everything [`run_copy_mode`] needs from the setup phase.
struct CopyModeFixture {
    state: Entity<crate::window_state::WindowState>,
    /// The capture-tee pty of the pill's only pane — the surface every
    /// zero-leak assertion measures.
    handle: Entity<TerminalSessionHandle>,
    session_id: String,
    window_id: String,
    /// The pane copy mode and the search bar both belong to.
    pane0: String,
}

/// The `copy-mode` scenario: Phase 3's copy mode and scrollback search, end to
/// end, over the SHIPPED dispatch path — the `keybind-scheme` / `splits` gate's
/// Phase 3 twin, and built the same way (real keymap + a `WindowState` in the
/// `WindowRegistry` + a capture-tee pty spawned THROUGH that state's
/// `PtyManager`), for the same reason: `⌃⌘c` and `⌃⌘/` are keymap actions that
/// resolve through the model, so a bare view would no-op every one of them and
/// the zero-leak assertions would pass vacuously.
///
/// The two halves it gates are the two P4 promises worth automating: the mode's
/// verbs do what vi does (motions move `vi_cursor_point`, paging moves
/// `display_offset`, `v`+`y` puts the seeded line on the clipboard), and while
/// the mode is on NOTHING reaches the pty — asserted with the `chord_leak`
/// byte-counter on every key, and kept honest by the plain-`u` differential
/// after each exit, which proves the same capture file WOULD have shown a leak.
///
/// What it deliberately does NOT cover, and why:
///
/// * **The IME path.** `dispatch_keystroke` enters below the
///   `NSTextInputClient`, so dead keys and live compositions — B1's third gate
///   — never run here at all. Those gates are unit-tested as pure predicates in
///   `nice-term-view`'s `input.rs` and hand-checked at the feel-check.
/// * **Chord DELIVERY.** Injection happens downstream of the OS hotkey layer,
///   so a chord macOS itself swallows still looks live here (the `⌃⌘D`
///   lesson). `⌃⌘c` and `⌃⌘/` arriving on a real keyboard is a hand check.
/// * **Paint.** No `WindowHostView` is mounted, so the `COPY` badge, the block
///   vi cursor and the match tints are unit + feel-check territory. The search
///   BAR is likewise unpainted: this driver feeds it the way the host's key
///   handler does (`dispatch_search_key` → push the query → confirm/close), so
///   the engine and the bar's own state machine are gated while gpui's focus
///   routing into the field is not.
/// * **Mouse reporting suspension (P10)** and the **held-pane gate order**: the
///   seeded pane is a plain capture-tee child with no mouse-mode TUI and no
///   corpse, so both are unit-tested predicates plus feel-check items.
pub fn open_copy_mode_window(cx: &mut AsyncApp) -> Result<AnyWindowHandle> {
    use crate::window_registry::WindowRegistry;
    use crate::window_state::WindowState;

    let base = prepare_dir("copy-mode")?;
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

    let fixture = cx.update(|app| -> Result<CopyModeFixture> {
        crate::keymap::install_shortcuts(app);
        app.set_global(crate::shortcuts_store::ShortcutBindings::with_defaults(
            store_path,
        ));
        crate::keymap::rebuild_keymap(app);

        let state = app.new(|_cx| WindowState::new(base_s.clone()));
        let (session_id, window_id) = state.update(app, |s, _cx| {
            let session_id = s
                .workspace
                .active_session_id()
                .map(str::to_owned)
                .unwrap_or_default();
            let window_id = s
                .workspace
                .session_for(&session_id)
                .and_then(|sess| sess.windows.first().map(|w| w.id.clone()))
                .unwrap_or_default();
            (session_id, window_id)
        });
        if session_id.is_empty() || window_id.is_empty() {
            return Err(anyhow!("copy-mode: a fresh WindowState seeded no pill"));
        }

        state.update(app, |s, cx| {
            let spawned = s.ptys.spawn_window(&session_id, &window_id, spec, cx);
            // The SHIPPED per-pane subscription, in the same update as the spawn
            // (the `session_lifecycle` precedent). In-mode `/` and `?` reach the
            // bar only through it — the view emits `SearchRequested` and this
            // subscription routes it to `open_search_bar` — so without the sweep
            // the §6b leg below would be asserting against a dead wire.
            s.subscribe_spawned_windows(cx);
            spawned
        })?;
        state.update(app, |s, _cx| {
            s.window_strip_actions
                .select_window(&mut s.workspace, &session_id, &window_id);
        });
        let pane0 = state
            .read(app)
            .workspace
            .session_for(&session_id)
            .and_then(|sess| sess.windows.iter().find(|w| w.id == window_id))
            .map(|w| w.effective_pane_id())
            .ok_or_else(|| anyhow!("copy-mode: the seeded pill vanished"))?;
        let handle = state
            .read(app)
            .ptys
            .pane_handle(&session_id, &window_id, &pane0)
            .ok_or_else(|| anyhow!("copy-mode: the seeded pane has no pty handle"))?;

        Ok(CopyModeFixture {
            state,
            handle,
            session_id,
            window_id,
            pane0,
        })
    })?;

    let terminal = make_view(fixture.handle.clone(), cx);

    let whandle = cx.open_window(crate::app::window_options(), {
        let terminal = terminal.clone();
        let state = fixture.state.clone();
        move |window, cx| {
            // `register`, not `install` — the close observer's quit-when-empty
            // would end the suite when this window closes (`keybind-scheme`).
            let id = window.window_handle().window_id();
            WindowRegistry::register(cx, id, state.clone());
            cx.new(|_cx| InputTermView { terminal })
        }
    })?;
    let window: AnyWindowHandle = whandle.into();
    crate::app::install_present_kick(&fixture.handle, window, cx);

    cx.spawn(async move |acx: &mut AsyncApp| {
        let report = run_copy_mode(acx, window, terminal, fixture, cap_path).await;
        eprintln!("[selftest] scenario 'copy-mode': {}", report.detail);
        nice_harness::selftest::report_gate(report);
    })
    .detach();

    Ok(window)
}

/// Whether the scenario's pane has `TermMode::VI` set — copy mode's single
/// source of truth (P1), read exactly as every shipped gate reads it.
fn copy_mode_on(cx: &mut AsyncApp, fixture: &CopyModeFixture) -> bool {
    fixture.handle.update(cx, |h, _| h.copy_mode_active())
}

/// The vi cursor as `(buffer line, column)` — negative lines are scrollback.
/// Flattened to plain integers so the driver never has to name alacritty's
/// `Point` (the `nice` crate does not depend on `alacritty_terminal`).
fn vi_point(cx: &mut AsyncApp, fixture: &CopyModeFixture) -> Option<(i32, usize)> {
    fixture
        .handle
        .update(cx, |h, _| h.vi_cursor_point().map(|p| (p.line.0, p.column.0)))
}

/// The focused match's START, same flattening as [`vi_point`].
fn active_match_start(cx: &mut AsyncApp, fixture: &CopyModeFixture) -> Option<(i32, usize)> {
    fixture.handle.update(cx, |h, _| {
        h.active_match().map(|m| (m.start().line.0, m.start().column.0))
    })
}

/// The pane's viewport offset: `0` is parked at the live bottom.
fn copy_offset(cx: &mut AsyncApp, fixture: &CopyModeFixture) -> usize {
    fixture.handle.update(cx, |h, _| h.display_offset())
}

/// Whether the search bar is open, and in which direction.
fn search_bar_open(cx: &mut AsyncApp, fixture: &CopyModeFixture) -> Option<bool> {
    fixture
        .state
        .update(cx, |ws, _| ws.search_bar().map(|bar| bar.backward))
}

/// The text the open field is holding, if a bar is open.
fn search_bar_query(cx: &mut AsyncApp, fixture: &CopyModeFixture) -> Option<String> {
    fixture
        .state
        .update(cx, |ws, _| ws.search_bar().map(|bar| bar.query()))
}

/// Close the bar the way the host's sweep does, so the next leg starts from a
/// known state (nothing paints here, so the sweep itself never runs).
fn close_search_bar(cx: &mut AsyncApp, fixture: &CopyModeFixture) {
    fixture.state.update(cx, |ws, _| {
        let _ = ws.close_search_bar();
    });
}

/// Dispatch one in-mode key and assert it reached the pty with NOTHING to show
/// for it — P4's leak-proof guarantee, measured the `chord_leak` way (byte
/// count on the capture file) on keys that are not chords at all.
async fn copy_key(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    cap_path: &Path,
    failures: &mut Vec<String>,
    keystroke: &str,
    label: &str,
) {
    let leaked = chord_leak(cx, window, cap_path, keystroke).await;
    if !leaked.is_empty() {
        failures.push(format!(
            "{keystroke} ({label}): leaked \"{}\" to the pty in copy mode",
            esc(&leaked)
        ));
    }
}

/// Feed one keystroke to the open search bar exactly as `WindowHostView`'s key
/// handler does — dispatch into the field's editor, then act on the outcome
/// (push the query, confirm it, or close the bar).
///
/// The host itself cannot run here (no `WindowHostView` is mounted, so nothing
/// paints the bar and nothing focuses it), so this driver stands in for the
/// FOCUS ROUTING and nothing else: the editor, the query push and the two verbs
/// are the shipped ones. Returns `None` when no bar is open.
async fn search_bar_key(
    cx: &mut AsyncApp,
    fixture: &CopyModeFixture,
    key: &str,
    key_char: Option<&str>,
) -> Option<crate::search_bar::SearchKeyOutcome> {
    use crate::search_bar::{dispatch_search_key, SearchKeyOutcome};

    let stepped = fixture.state.update(cx, |ws, wcx| {
        let bar = ws.search_bar_mut()?;
        let outcome = dispatch_search_key(
            &mut bar.editor,
            &mut **wcx,
            key,
            key_char,
            false,
            false,
            false,
            false,
            false,
        );
        Some((outcome, bar.query()))
    });
    let (outcome, query) = stepped?;

    match outcome {
        SearchKeyOutcome::Edited => {
            fixture.handle.update(cx, |h, hcx| {
                h.set_search_query(&query);
                hcx.notify();
            });
        }
        SearchKeyOutcome::Confirm => {
            fixture.handle.update(cx, |h, hcx| {
                // The final query first: the keystroke before Enter may have
                // been the one that completed it.
                h.set_search_query(&query);
                h.confirm_search();
                hcx.notify();
            });
            fixture.state.update(cx, |ws, _| {
                let _ = ws.close_search_bar();
            });
        }
        SearchKeyOutcome::Close => {
            fixture.state.update(cx, |ws, _| {
                let _ = ws.close_search_bar();
            });
        }
        SearchKeyOutcome::Ignored => {}
    }
    settle(cx, 60).await;
    Some(outcome)
}

/// Put a known string on the clipboard so "the yank wrote this" is provable
/// rather than inherited from whatever was there before.
fn seed_clipboard(cx: &mut AsyncApp, sentinel: &str) {
    let sentinel = sentinel.to_string();
    let _ = cx.update(|app| app.write_to_clipboard(ClipboardItem::new_string(sentinel)));
}

/// The clipboard's text, if any.
fn clipboard_text(cx: &mut AsyncApp) -> Option<String> {
    cx.update(|app| app.read_from_clipboard().and_then(|item| item.text()))
}

async fn run_copy_mode(
    cx: &mut AsyncApp,
    window: AnyWindowHandle,
    terminal: Entity<TerminalView>,
    fixture: CopyModeFixture,
    cap_path: PathBuf,
) -> CadenceReport {
    let _ = cx.update(|app| app.activate(true));
    settle(cx, 500).await;

    // Focus the mounted view — `dispatch_keystroke` walks the focus path, and
    // the plain-`u` differential needs the view's own input path live.
    let _ = window.update(cx, |_root, window, cx| {
        let fh = terminal.read(cx).focus_handle_ref().clone();
        window.focus(&fh, cx);
    });
    settle(cx, 200).await;

    let mut failures: Vec<String> = Vec::new();
    let last_line = format!("copyline-{COPY_SEED_LINES}");

    // --- §0 seed the scrollback --------------------------------------------
    let seed: String = (1..=COPY_SEED_LINES)
        .map(|i| {
            if COPY_NEEDLE_LINES.contains(&i) {
                format!("copyline-{i} {COPY_NEEDLE}\r\n")
            } else {
                format!("copyline-{i}\r\n")
            }
        })
        .collect();
    if let Err(e) = write_child(cx, &fixture.handle, seed.as_bytes()) {
        return CadenceReport::error(format!("copy-mode: seeding failed: {e}"));
    }
    let mut seeded = false;
    for _ in 0..80 {
        settle(cx, 100).await;
        let text = fixture
            .handle
            .update(cx, |h, _| h.session().grid_lines().join("\n"));
        if text.contains(&last_line) {
            seeded = true;
            break;
        }
    }
    if !seeded {
        return CadenceReport::error(format!(
            "copy-mode: the seed never rendered ({last_line} absent from the grid)"
        ));
    }
    if copy_offset(cx, &fixture) != 0 {
        failures.push("seed: viewport not parked at the bottom after output".into());
    }
    if copy_mode_on(cx, &fixture) {
        return CadenceReport::error("copy-mode: the pane started IN copy mode".to_string());
    }

    // --- §1 ⌃⌘c enters, and the mode swallows everything -------------------
    let leaked = chord_leak(cx, window, &cap_path, "cmd-ctrl-c").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl-c: leaked \"{}\"", esc(&leaked)));
    }
    if !copy_mode_on(cx, &fixture) {
        return CadenceReport::error("copy-mode: ⌃⌘c did not enter copy mode".to_string());
    }
    let Some(entry) = vi_point(cx, &fixture) else {
        return CadenceReport::error("copy-mode: no vi cursor after entering".to_string());
    };

    // Every one of these moves the vi cursor and must write nothing at all —
    // the whole point of the mode's default-`Swallow` arm.
    for key in ["h", "j", "k", "l", "w", "b", "e", "^"] {
        copy_key(cx, window, &cap_path, &mut failures, key, "motion").await;
    }

    // --- §2 motions move the vi cursor as vi moves it ----------------------
    // Back to a known cell first: `G` parks on the terminal cursor's line,
    // `0` on its first column.
    copy_key(cx, window, &cap_path, &mut failures, "shift-g", "G").await;
    copy_key(cx, window, &cap_path, &mut failures, "0", "0").await;
    let Some(home) = vi_point(cx, &fixture) else {
        return CadenceReport::error("copy-mode: the vi cursor vanished mid-run".to_string());
    };
    if home != (entry.0, 0) {
        failures.push(format!(
            "G then 0: cursor is {home:?}, expected line {} column 0",
            entry.0
        ));
    }
    copy_key(cx, window, &cap_path, &mut failures, "k", "k").await;
    if vi_point(cx, &fixture).map(|p| p.0) != Some(home.0 - 1) {
        failures.push(format!(
            "k: cursor is {:?}, expected one line above {home:?}",
            vi_point(cx, &fixture)
        ));
    }
    copy_key(cx, window, &cap_path, &mut failures, "$", "$").await;
    match vi_point(cx, &fixture) {
        Some((line, col)) if line == home.0 - 1 && col > 0 => {}
        other => failures.push(format!("$: cursor is {other:?}, expected the line's last column")),
    }
    copy_key(cx, window, &cap_path, &mut failures, "0", "0 again").await;
    if vi_point(cx, &fixture).map(|p| p.1) != Some(0) {
        failures.push("0: cursor did not return to the first column".into());
    }

    // `g` / `G` — the ends of the buffer.
    copy_key(cx, window, &cap_path, &mut failures, "g", "g").await;
    match (vi_point(cx, &fixture), copy_offset(cx, &fixture)) {
        (Some((line, _)), offset) if line < 0 && offset > 0 => {}
        (point, offset) => failures.push(format!(
            "g: cursor {point:?} at offset {offset}, expected a history line with the viewport \
             scrolled up"
        )),
    }
    copy_key(cx, window, &cap_path, &mut failures, "shift-g", "G").await;
    if vi_point(cx, &fixture).map(|p| p.0) != Some(entry.0) || copy_offset(cx, &fixture) != 0 {
        failures.push(format!(
            "G: cursor {:?} at offset {}, expected line {} parked at the bottom",
            vi_point(cx, &fixture),
            copy_offset(cx, &fixture),
            entry.0
        ));
    }

    // --- §3 paging moves the viewport, and Shift+PageUp still works IN mode -
    copy_key(cx, window, &cap_path, &mut failures, "ctrl-u", "⌃u").await;
    let half = copy_offset(cx, &fixture);
    if half == 0 {
        failures.push("ctrl-u: display offset stayed 0 (no half-page scroll)".into());
    }
    // I4: today's scrollback keys must not go dead exactly while the user is
    // navigating scrollback — they live inside `dispatch_key`, which the
    // copy-mode gate never reaches, so the key table re-maps them.
    copy_key(cx, window, &cap_path, &mut failures, "shift-pageup", "Shift+PageUp").await;
    if copy_offset(cx, &fixture) <= half {
        failures.push(format!(
            "shift-pageup: offset {} did not move past the half-page {half}",
            copy_offset(cx, &fixture)
        ));
    }
    copy_key(cx, window, &cap_path, &mut failures, "shift-end", "Shift+End").await;
    if copy_offset(cx, &fixture) != 0 {
        failures.push("shift-end: the viewport did not return to the bottom".into());
    }

    // --- §4 v + motions + y: the seeded line lands on the clipboard --------
    seed_clipboard(cx, "copy-mode-sentinel-yank");
    copy_key(cx, window, &cap_path, &mut failures, "k", "onto the last seeded line").await;
    copy_key(cx, window, &cap_path, &mut failures, "0", "0").await;
    copy_key(cx, window, &cap_path, &mut failures, "v", "v").await;
    copy_key(cx, window, &cap_path, &mut failures, "$", "$").await;
    copy_key(cx, window, &cap_path, &mut failures, "y", "y").await;
    match clipboard_text(cx) {
        Some(text) if text.contains(&last_line) => {}
        other => failures.push(format!(
            "y: the clipboard holds {other:?}, expected it to contain {last_line}"
        )),
    }
    if copy_mode_on(cx, &fixture) {
        failures.push("y: yanking did not leave copy mode (P4)".into());
    }
    if copy_offset(cx, &fixture) != 0 {
        failures.push("y: the viewport did not return to the live bottom (P6)".into());
    }

    // The differential: with the mode off, a plain `u` reaches the pty again —
    // without it every zero-byte assertion above could pass vacuously.
    let start = cap_len(&cap_path);
    dispatch_key(cx, window, "u");
    settle(cx, 250).await;
    expect_bytes(&mut failures, "post-yank u", b"u", &cap_since(&cap_path, start));

    // --- §5 ⌃⌘/ searches the scrollback ------------------------------------
    let leaked = chord_leak(cx, window, &cap_path, "cmd-ctrl-/").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl-/: leaked \"{}\"", esc(&leaked)));
    }
    // Opening the field ENTERS copy mode (P7/D1: search is copy mode with a
    // query) and searches backward (the "find what scrolled past" direction).
    if !copy_mode_on(cx, &fixture) {
        failures.push("cmd-ctrl-/: did not enter copy mode".into());
    }
    if search_bar_open(cx, &fixture) != Some(true) {
        return CadenceReport::error(format!(
            "copy-mode: ⌃⌘/ left the search bar {:?}, expected an open backward search",
            search_bar_open(cx, &fixture)
        ));
    }
    for ch in COPY_NEEDLE.chars() {
        let key = ch.to_string();
        if search_bar_key(cx, &fixture, &key, Some(&key)).await.is_none() {
            failures.push(format!("search: the bar closed while typing {COPY_NEEDLE}"));
            break;
        }
    }
    let query = fixture
        .handle
        .update(cx, |h, _| h.active_search_query().map(str::to_owned));
    if query.as_deref() != Some(COPY_NEEDLE) {
        failures.push(format!(
            "search: the live query is {query:?}, expected {COPY_NEEDLE}"
        ));
    }
    if search_bar_key(cx, &fixture, "enter", None).await
        != Some(crate::search_bar::SearchKeyOutcome::Confirm)
    {
        failures.push("search: Enter did not confirm the query".into());
    }
    if search_bar_open(cx, &fixture).is_some() {
        failures.push("search: Enter left the bar open".into());
    }
    if !copy_mode_on(cx, &fixture) {
        failures.push("search: confirming dropped out of copy mode (D1)".into());
    }
    let Some(first) = active_match_start(cx, &fixture) else {
        return CadenceReport::error(
            "copy-mode: confirming the search found no match at all".to_string(),
        );
    };
    if vi_point(cx, &fixture) != Some(first) {
        failures.push(format!(
            "search: the cursor is {:?}, expected the match at {first:?}",
            vi_point(cx, &fixture)
        ));
    }

    // `n` repeats in the confirmed (backward) direction — further into history;
    // `N` reverses and comes straight back. Asserted as an ORDER on the buffer
    // lines rather than absolute numbers, because the mounted view refits the
    // pty and the driver cannot know the laid-out row count.
    copy_key(cx, window, &cap_path, &mut failures, "n", "n").await;
    let Some(second) = active_match_start(cx, &fixture) else {
        return CadenceReport::error("copy-mode: `n` found no further match".to_string());
    };
    if second.0 >= first.0 {
        failures.push(format!(
            "n: landed on {second:?}, expected a match above {first:?}"
        ));
    }
    copy_key(cx, window, &cap_path, &mut failures, "shift-n", "N").await;
    if active_match_start(cx, &fixture) != Some(first) {
        failures.push(format!(
            "N: landed on {:?}, expected to come back to {first:?}",
            active_match_start(cx, &fixture)
        ));
    }

    // --- §6 the Esc ladder: field → copy mode → normal ---------------------
    let leaked = chord_leak(cx, window, &cap_path, "cmd-ctrl-/").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl-/ (reopen): leaked \"{}\"", esc(&leaked)));
    }
    if search_bar_open(cx, &fixture).is_none() {
        failures.push("search: ⌃⌘/ did not re-open the bar".into());
    }
    // A click into the pane leaves the bar OPEN and merely unfocused, and P7
    // says ⌃⌘/ then refocuses it — so a second ⌃⌘/ over the same pane, pointing
    // the same way, must keep what is typed rather than start over. (Nothing
    // paints here, so "unfocused" is the state this driver is always in.)
    for ch in COPY_NEEDLE.chars().take(3) {
        let key = ch.to_string();
        if search_bar_key(cx, &fixture, &key, Some(&key)).await.is_none() {
            failures.push("search: the bar closed while typing the refocus query".into());
            break;
        }
    }
    let typed = search_bar_query(cx, &fixture);
    let leaked = chord_leak(cx, window, &cap_path, "cmd-ctrl-/").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl-/ (refocus): leaked \"{}\"", esc(&leaked)));
    }
    if search_bar_query(cx, &fixture) != typed || search_bar_open(cx, &fixture) != Some(true) {
        failures.push(format!(
            "search: ⌃⌘/ over the bar it already opened left it {:?}/{:?}, expected the typed \
             {typed:?} refocused (P7)",
            search_bar_open(cx, &fixture),
            search_bar_query(cx, &fixture)
        ));
    }
    let live = fixture
        .handle
        .update(cx, |h, _| h.active_search_query().map(str::to_owned));
    if live != typed {
        failures.push(format!(
            "search: the refocus left the live query {live:?}, expected the field's {typed:?}"
        ));
    }
    if search_bar_key(cx, &fixture, "escape", None).await
        != Some(crate::search_bar::SearchKeyOutcome::Close)
    {
        failures.push("search: Escape did not close the field".into());
    }
    if search_bar_open(cx, &fixture).is_some() {
        failures.push("search: Escape left the bar open".into());
    }
    if !copy_mode_on(cx, &fixture) {
        failures.push("search: Escape in the field also left copy mode (P7 says it stays)".into());
    }
    copy_key(cx, window, &cap_path, &mut failures, "escape", "escape").await;
    if copy_mode_on(cx, &fixture) {
        failures.push("escape: the second Escape did not leave copy mode".into());
    }

    // --- §6b in-mode `/` and `?` open the bar through the EVENT path -------
    // Everything above drove the bar in through the `⌃⌘/` ACTION, which calls
    // `open_search_bar` directly. The two in-mode keys take the OTHER route:
    // `perform_copy_mode` emits `TerminalEvent::SearchRequested` and the
    // per-pane subscription routes it. That routing sits behind a match whose
    // wildcard arm swallows an unrouted variant SILENTLY — deleting the arm
    // still compiles and every other assertion here still passes — so this leg
    // is its guard. `/` searches forward, `?` (shift-folded `/`) backward.
    copy_key(cx, window, &cap_path, &mut failures, "cmd-ctrl-c", "re-enter for `/`").await;
    if !copy_mode_on(cx, &fixture) {
        failures.push("`/` leg: ⌃⌘c did not re-enter copy mode".into());
    }
    copy_key(cx, window, &cap_path, &mut failures, "/", "in-mode /").await;
    if search_bar_open(cx, &fixture) != Some(false) {
        failures.push(format!(
            "`/`: the bar is {:?}, expected an open FORWARD search (the routed SearchRequested)",
            search_bar_open(cx, &fixture)
        ));
    }
    close_search_bar(cx, &fixture);
    copy_key(cx, window, &cap_path, &mut failures, "shift-/", "in-mode ?").await;
    if search_bar_open(cx, &fixture) != Some(true) {
        failures.push(format!(
            "`?`: the bar is {:?}, expected an open BACKWARD search (the routed SearchRequested)",
            search_bar_open(cx, &fixture)
        ));
    }
    close_search_bar(cx, &fixture);
    copy_key(cx, window, &cap_path, &mut failures, "escape", "escape out of the `/` leg").await;
    if copy_mode_on(cx, &fixture) {
        failures.push("`/` leg: Escape did not leave copy mode".into());
    }

    // --- §7 ⌘C copies and STAYS in the mode (P4) ---------------------------
    seed_clipboard(cx, "copy-mode-sentinel-cmd-c");
    let leaked = chord_leak(cx, window, &cap_path, "cmd-ctrl-c").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl-c (re-enter): leaked \"{}\"", esc(&leaked)));
    }
    copy_key(cx, window, &cap_path, &mut failures, "k", "onto the last seeded line").await;
    copy_key(cx, window, &cap_path, &mut failures, "0", "0").await;
    copy_key(cx, window, &cap_path, &mut failures, "v", "v").await;
    copy_key(cx, window, &cap_path, &mut failures, "$", "$").await;
    copy_key(cx, window, &cap_path, &mut failures, "cmd-c", "⌘C").await;
    match clipboard_text(cx) {
        Some(text) if text.contains(&last_line) => {}
        other => failures.push(format!(
            "⌘C: the clipboard holds {other:?}, expected it to contain {last_line}"
        )),
    }
    if !copy_mode_on(cx, &fixture) {
        failures.push("⌘C: copying left copy mode (P4 says it stays)".into());
    }

    // --- §8 ⌃⌘c toggles out from any state, bar open included --------------
    let leaked = chord_leak(cx, window, &cap_path, "cmd-ctrl-/").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl-/ (toggle leg): leaked \"{}\"", esc(&leaked)));
    }
    if search_bar_open(cx, &fixture).is_none() {
        failures.push("toggle: ⌃⌘/ did not open the bar for the toggle-out leg".into());
    }
    let leaked = chord_leak(cx, window, &cap_path, "cmd-ctrl-c").await;
    if !leaked.is_empty() {
        failures.push(format!("cmd-ctrl-c (toggle out): leaked \"{}\"", esc(&leaked)));
    }
    if copy_mode_on(cx, &fixture) {
        failures.push("toggle: ⌃⌘c with the bar open did not leave copy mode".into());
    }
    // The bar's CLOSE is the host's render sweep, which no mounted view runs
    // here — so assert the predicate that sweep consults (I2). Then close the
    // bar so the scenario ends on the state the host would have left.
    if !crate::search_bar::search_bar_is_stale(&fixture.pane0, Some(&fixture.pane0), false) {
        failures.push("toggle: the bar over a pane that left copy mode did not read as stale".into());
    }
    fixture.state.update(cx, |ws, _| {
        let _ = ws.close_search_bar();
    });

    // --- §9 the pane types again ------------------------------------------
    let start = cap_len(&cap_path);
    dispatch_key(cx, window, "u");
    settle(cx, 250).await;
    expect_bytes(&mut failures, "final u", b"u", &cap_since(&cap_path, start));

    // The fixture's ids are load-bearing for the pane-keyed legs above; name
    // them in the failure text so a broken run says WHICH pane it was driving.
    if failures.is_empty() {
        CadenceReport {
            passed: true,
            stats: IntervalStats::default(),
            detail: "copy mode OK end to end: ⌃⌘c entered and every in-mode key stayed off the \
                     pty, hjkl/0/$/g/G moved the vi cursor, ⌃u and Shift+PageUp paged the \
                     viewport, v+y put the seeded line on the clipboard and returned to the live \
                     bottom, ⌃⌘/ opened a backward search whose Enter landed on a match and \
                     whose n/N walked them, a second ⌃⌘/ refocused the open bar with its query \
                     intact, in-mode / and ? opened it forward/backward through the routed \
                     SearchRequested, the Esc ladder unwound field → mode → normal, ⌘C copied \
                     without leaving, and ⌃⌘c toggled out with the bar open — with a plain `u` \
                     reaching the pty after every exit"
                .to_string(),
        }
    } else {
        CadenceReport {
            passed: false,
            stats: IntervalStats::default(),
            detail: format!(
                "copy-mode FAILED (session {} / pill {} / pane {}):\n  - {}",
                fixture.session_id,
                fixture.window_id,
                fixture.pane0,
                failures.join("\n  - ")
            ),
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
