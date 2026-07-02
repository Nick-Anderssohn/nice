//! IME live spike (Phase-0 §13 spike 2, live half) — a from-scratch
//! GPUI-native terminal-ish `InputHandler` on CURRENT gpui main (pinned zed
//! rev 10b07951838e422722e34641f4a9c0bfec9037ff + bg-luminance patch — the
//! same local ../zed-main-patched checkout the matrix bin builds against).
//!
//! REQUIRES A DISPLAY AND A HUMAN AT THE KEYBOARD (IME composition cannot be
//! driven headlessly). Gated on NICE_IME_SPIKE_RUN=1 so a stray `cargo run`
//! can never open a window. Runbook: spikes/phase0-poc/ime-spike/RUN-IME.md;
//! plan: spikes/phase0-poc/ime-spike/IMPLEMENTATION-PLAN.md.
//!
//! What this binary demonstrates (plan §Trait methods, §Enter, §DeadKeys,
//! §Preedit rendering):
//!   * All 11 `InputHandler` methods, implemented DIRECTLY on the platform
//!     trait (not via `ElementInputHandler`: its blanket impl forwards
//!     `prefers_ime_for_printable_keys` to `accepts_text_input`, which must
//!     be `true` for a terminal — but the terminal needs `false` there so raw
//!     printable keys reach the pty, matching Zed's own terminal policy).
//!   * `bounds_for_range` NEVER returns `None` (the zed#46055 fix): it always
//!     anchors at the alacritty grid cursor cell (plus the shaped preedit
//!     prefix for sub-range queries), so the candidate window can never fall
//!     back to the screen's bottom-left NSRect(0,0,0,0).
//!   * The Enter-during-composition commit-swallow flag (zed#23003): a
//!     commit that ended a composition arms a flag; the key-down callback
//!     swallows an Enter re-dispatched in the same native key cycle
//!     (`doCommandBySelector(insertNewline:)` path) and a foreground task
//!     disarms it at end of cycle so a later bare Enter still sends CR.
//!   * Committed IME text is DATA: raw UTF-8 fed to the vte parser (the pty
//!     stand-in), never through a key encoder. The kitty key encoder is
//!     audit G7 and intentionally out of scope; arrows/backspace get
//!     placeholder local-echo sequences purely so the cursor can be moved
//!     around the grid to exercise the candidate-window anchor.
//!
//! "Terminal-ish" view: no shell, no real pty. Committed bytes are advanced
//! straight into an alacritty_terminal grid (local echo) and rendered
//! per-cell with gpui text runs; the preedit is painted as an underlined
//! overlay at the grid cursor (it never enters the grid model). A HUD below
//! the grid live-logs every NSTextInputClient call so the human checklist in
//! RUN-IME.md can be verified by eye.

mod ime_state;

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point as TermPoint};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;

use gpui::{
    App, AppContext, Bounds, Context, Entity, FocusHandle, Font, FontFeatures, FontStyle,
    FontWeight, InputHandler, IntoElement, KeyBinding, KeyDownEvent, Pixels, Point, Render,
    SharedString, Styled, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, WindowBounds,
    WindowOptions, actions, canvas, div, fill, point, prelude::*, px, rgb, size,
};
use gpui_platform::application;

use ime_state::{ImeState, utf16_to_byte};

// ---- fixed spike parameters --------------------------------------------------

const COLS: usize = 72;
const ROWS: usize = 12;
const FONT_FAMILY: &str = "Menlo"; // always installed; CJK via CoreText fallback
const FONT_PX: f32 = 13.0;
const CELL_H: f32 = 16.0;
const INSET: f32 = 10.0; // grid inset inside the canvas, pt

const BG: u32 = 0x090705; // Nice dark theme bg/fg (BuiltInTerminalThemes)
const FG: u32 = 0xf4f0ef;
const HUD_BG: u32 = 0x171310;
const HUD_DIM: u32 = 0x8f8a84;
const PREEDIT_FG: u32 = 0x64e6e6; // bright cyan: unmistakably "not committed"
const PREEDIT_STRIP: u32 = 0x496ee1; // selection strip behind the preedit
const CARET: u32 = 0xead423;
const CURSOR: u32 = 0xf4f0ef;

actions!(ime_spike, [Quit, ProbeCmdLeft, ProbeCmdRight, ProbeCmdK]);

// ---- alacritty local-echo scaffolding (as in the matrix bin) ------------------

#[derive(Clone, Copy)]
struct TermSize;
impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        ROWS
    }
    fn screen_lines(&self) -> usize {
        ROWS
    }
    fn columns(&self) -> usize {
        COLS
    }
}

#[derive(Clone)]
struct EventProxy;
impl EventListener for EventProxy {
    fn send_event(&self, _event: Event) {}
}

#[derive(Clone, Copy)]
struct CellMetrics {
    cell_w: f32,
    cell_h: f32,
}

fn term_font(family: &str) -> Font {
    Font {
        family: SharedString::new(family.to_string()),
        features: FontFeatures::default(),
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
        fallbacks: None,
    }
}

// ---- the view -----------------------------------------------------------------

struct ImeSpikeView {
    focus_handle: FocusHandle,
    term: Term<EventProxy>,
    parser: Processor,
    ime: ImeState,
    metrics: Option<CellMetrics>,
    option_as_meta: bool,
    event_log: VecDeque<String>,
    pty_log: VecDeque<String>,
    pty_writes_total: usize,
    bounds_calls: usize,
    last_anchor: String,
}

impl ImeSpikeView {
    fn new(option_as_meta: bool, cx: &mut Context<Self>) -> Self {
        let mut view = ImeSpikeView {
            focus_handle: cx.focus_handle(),
            term: Term::new(Config::default(), &TermSize, EventProxy),
            parser: Processor::new(),
            ime: ImeState::new(),
            metrics: None,
            option_as_meta,
            event_log: VecDeque::new(),
            pty_log: VecDeque::new(),
            pty_writes_total: 0,
            bounds_calls: 0,
            last_anchor: "(none yet)".to_string(),
        };
        view.feed_pty_quiet(
            b"IME live spike \xe2\x80\x94 gpui main @10b0795 (zed-main-patched)\r\n\
              No shell: committed bytes local-echo into this alacritty grid.\r\n\
              \r\n$ ",
        );
        view
    }

    /// The pty stand-in: committed/echoed bytes advance the vte parser.
    fn feed_pty_quiet(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    fn feed_pty(&mut self, bytes: &[u8]) {
        self.feed_pty_quiet(bytes);
        self.pty_writes_total += 1;
        let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
        self.push_log_to(
            LogKind::Pty,
            format!(
                "pty ← {:?} [{}]",
                String::from_utf8_lossy(bytes),
                hex.join(" ")
            ),
        );
    }

    fn log(&mut self, entry: impl Into<String>) {
        self.push_log_to(LogKind::Event, entry.into());
    }

    fn push_log_to(&mut self, kind: LogKind, entry: String) {
        let (log, cap) = match kind {
            LogKind::Event => (&mut self.event_log, 12),
            LogKind::Pty => (&mut self.pty_log, 4),
        };
        log.push_back(entry);
        while log.len() > cap {
            log.pop_front();
        }
    }

    fn metrics_or_default(&self) -> CellMetrics {
        self.metrics.unwrap_or(CellMetrics {
            cell_w: 8.0,
            cell_h: CELL_H,
        })
    }

    fn cursor_point(&self) -> (usize, usize) {
        let p = self.term.grid().cursor.point;
        (p.line.0.max(0) as usize, p.column.0)
    }

    // -- InputHandler backing methods (called by TermInputHandler) --------------

    fn ime_set_marked(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_sel: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) {
        self.ime.set_marked_text(range.clone(), new_text, new_sel.clone());
        self.log(format!(
            "setMarkedText({new_text:?}, repl {range:?}, sel {new_sel:?}) \
             → preedit {:?} sel {:?}",
            self.ime.preedit(),
            self.ime.selected_range_utf16()
        ));
        cx.notify();
    }

    fn ime_commit(&mut self, range: Option<Range<usize>>, text: &str, cx: &mut Context<Self>) {
        let outcome = self.ime.commit_text(range.clone(), text);
        self.log(format!(
            "insertText({text:?}, repl {range:?}) → COMMIT{}",
            if outcome.was_composing {
                "; swallow armed"
            } else {
                ""
            }
        ));
        if let Some(r) = outcome.unhonored_replacement {
            self.log(format!(
                "  ⚠ replacement {r:?} targets text already at the pty \
                 — inserted instead (doc = preedit only)"
            ));
        }
        self.feed_pty(outcome.pty_text.as_bytes());
        if outcome.was_composing {
            // End-of-native-key-cycle disarm: runs after any synchronous
            // doCommandBySelector re-dispatch, before the next keypress.
            cx.spawn(async move |this, cx| {
                this.update(cx, |view, _| view.ime.disarm_commit_swallow())
                    .ok();
            })
            .detach();
        }
        cx.notify();
    }

    fn ime_unmark(&mut self, cx: &mut Context<Self>) {
        if let Some(pending) = self.ime.unmark() {
            self.log(format!("unmarkText → committed pending {pending:?}"));
            self.feed_pty(pending.as_bytes());
        } else {
            self.log("unmarkText (idle)");
        }
        cx.notify();
    }

    /// The zed#46055 fix: ALWAYS a rect, anchored at the grid cursor cell.
    /// A full-screen TUI that parks or hides the hardware cursor still has a
    /// grid cursor position, so this is total by construction — there is no
    /// code path that yields None.
    fn ime_anchor_bounds(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
    ) -> Bounds<Pixels> {
        let m = self.metrics_or_default();
        let (row, col) = self.cursor_point();
        let mut x = element_bounds.origin.x + px(INSET + col as f32 * m.cell_w);
        let y = element_bounds.origin.y + px(INSET + row as f32 * m.cell_h);
        // Sub-range queries anchor within the rendered preedit overlay, like
        // Terminal.app; range 0 (or idle) is exactly the cursor cell.
        if self.ime.is_composing() && range_utf16.start > 0 {
            let preedit: SharedString = SharedString::new(self.ime.preedit().to_string());
            let byte = utf16_to_byte(&preedit, range_utf16.start);
            let run = TextRun {
                len: preedit.len(),
                font: term_font(FONT_FAMILY),
                color: rgb(PREEDIT_FG).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped = window
                .text_system()
                .shape_line(preedit, px(FONT_PX), &[run], None);
            x += shaped.x_for_index(byte);
        }
        let bounds = Bounds {
            origin: point(x, y),
            size: size(px(m.cell_w), px(m.cell_h)),
        };
        self.bounds_calls += 1;
        self.last_anchor = format!(
            "cell ({row},{col}) → window px ({:.1}, {:.1}) {}x{}",
            f32::from(bounds.origin.x),
            f32::from(bounds.origin.y),
            m.cell_w,
            m.cell_h
        );
        bounds
    }

    // -- key handling (plan §Enter; kitty encoder deliberately absent) ----------

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let mods = keystroke.modifiers;
        // Read+clear at the start of every key cycle that reaches the app:
        // only an Enter re-dispatched in the SAME cycle as a commit sees true.
        let swallow = self.ime.take_commit_swallow();
        let plain = !mods.control && !mods.platform && !mods.function;

        match keystroke.key.as_str() {
            // Tab and Enter are the two keys the historical Zed fix
            // (PR #27572) covered: both can confirm an IME candidate and
            // both must NOT additionally reach the pty in that same cycle.
            key @ ("enter" | "tab") if plain => {
                if swallow {
                    self.log(format!(
                        "key {key} SWALLOWED — same-cycle IME commit (zed#23003 policy)"
                    ));
                } else if key == "enter" {
                    self.feed_pty(b"\r\n"); // local echo stand-in for shell CR
                    self.log("key ⏎ → CR LF");
                } else {
                    self.feed_pty(b"\t");
                    self.log("key ⇥ → HT (local echo)");
                }
                cx.notify();
                cx.stop_propagation();
            }
            "backspace" if plain && !mods.alt => {
                self.feed_pty(b"\x08 \x08"); // local-echo erase; encoder is G7
                self.log("key ⌫ → BS SP BS (local echo)");
                cx.notify();
                cx.stop_propagation();
            }
            key @ ("left" | "right" | "up" | "down") if plain && !mods.alt && !mods.shift => {
                // Cursor-move probe so the candidate-window anchor can be
                // tested at arbitrary grid positions (kitty encoder = G7).
                let csi: &[u8] = match key {
                    "left" => b"\x1b[D",
                    "right" => b"\x1b[C",
                    "up" => b"\x1b[A",
                    _ => b"\x1b[B",
                };
                self.feed_pty(csi);
                self.log(format!("key {key} → CSI cursor move (anchor probe)"));
                cx.notify();
                cx.stop_propagation();
            }
            "f2" if plain => {
                self.feed_pty(b"\x1b[6;30H"); // park cursor mid-grid, TUI-style
                self.log("key f2 → CUP 6;30 (TUI-style cursor park; anchor must follow)");
                cx.notify();
                cx.stop_propagation();
            }
            "escape" if plain => {
                self.log("key esc (reached app ⇒ IME did not consume it)");
                cx.notify();
                cx.stop_propagation();
            }
            key if self.option_as_meta
                && mods.alt
                && plain
                && key.len() == 1
                && key.is_ascii() =>
            {
                // Option-as-Meta ON: ESC-prefix the key and bypass the IME —
                // this is the config-driven side of the dead-key conflict
                // (plan §DeadKeys). stop_propagation() keeps the event from
                // reaching NSTextInputClient, so no dead-key composition.
                let mut bytes = vec![0x1b];
                bytes.extend_from_slice(key.as_bytes());
                self.feed_pty(&bytes);
                self.log(format!(
                    "key ⌥{key} → ESC {key} (option-as-meta ON; IME bypassed)"
                ));
                cx.notify();
                cx.stop_propagation();
            }
            _ => {
                // Printable keys (and anything else unhandled) propagate to
                // the platform, which offers them to NSTextInputClient — the
                // IME composes or insertText commits. This is the same path
                // real typed text takes in the eventual terminal.
                self.log(format!(
                    "key {} (key_char {:?}) ↓ passthrough → NSTextInputClient",
                    keystroke.unparse(),
                    keystroke.key_char
                ));
                cx.notify();
            }
        }
    }

    fn probe(&mut self, name: &str, cx: &mut Context<Self>) {
        self.log(format!("KEYBINDING {name} fired — not eaten by IME plumbing"));
        cx.notify();
    }

    fn grid_snapshot(&self) -> Vec<Vec<char>> {
        let mut rows = Vec::with_capacity(ROWS);
        for line in 0..ROWS {
            let mut row = Vec::with_capacity(COLS);
            for col in 0..COLS {
                let cell = &self.term.grid()[TermPoint::new(Line(line as i32), Column(col))];
                let ch = if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    '\0' // spacer of a wide char: skip at paint
                } else if cell.c == '\0' {
                    ' '
                } else {
                    cell.c
                };
                row.push(ch);
            }
            rows.push(row);
        }
        rows
    }
}

enum LogKind {
    Event,
    Pty,
}

// ---- the platform InputHandler ------------------------------------------------
// Implemented directly (not via ElementInputHandler) so that
// prefers_ime_for_printable_keys can be `false` while accepts_text_input is
// `true` — ElementInputHandler's blanket impl ties them together.

struct TermInputHandler {
    view: Entity<ImeSpikeView>,
    element_bounds: Bounds<Pixels>,
}

impl InputHandler for TermInputHandler {
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        // Never None: some IMEs misbehave on it (plan §Trait methods).
        Some(UTF16Selection {
            range: self.view.read(cx).ime.selected_range_utf16(),
            reversed: false,
        })
    }

    fn marked_text_range(&mut self, _window: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        self.view.read(cx).ime.marked_range_utf16()
    }

    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<String> {
        let (text, actual) = self.view.read(cx).ime.text_for_range_utf16(range_utf16)?;
        *adjusted_range = Some(actual);
        Some(text)
    }

    fn replace_text_in_range(
        &mut self,
        replacement_range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.view
            .update(cx, |view, cx| view.ime_commit(replacement_range, text, cx));
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.view.update(cx, |view, cx| {
            view.ime_set_marked(range_utf16, new_text, new_selected_range, cx)
        });
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut App) {
        self.view.update(cx, |view, cx| view.ime_unmark(cx));
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        // ALWAYS Some — the zed#46055 fix. None would make gpui report
        // NSRect(0,0,0,0), which AppKit resolves to the screen bottom-left.
        let element_bounds = self.element_bounds;
        Some(self.view.update(cx, |view, _| {
            view.ime_anchor_bounds(range_utf16, element_bounds, window)
        }))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<usize> {
        // Minimal-but-total, per plan: low value for a terminal, must not
        // panic or return NSNotFound while composing.
        Some(0)
    }

    fn apple_press_and_hold_enabled(&mut self) -> bool {
        false // terminal convention (iTerm2): held key = key-repeat, no popover
    }

    fn accepts_text_input(&mut self, _window: &mut Window, _cx: &mut App) -> bool {
        true
    }

    fn prefers_ime_for_printable_keys(&mut self, _window: &mut Window, _cx: &mut App) -> bool {
        false // Zed terminal policy: raw printable keys reach the pty
    }
}

// ---- render ---------------------------------------------------------------------

impl Render for ImeSpikeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.metrics.is_none() {
            let probe = window.text_system().shape_line(
                SharedString::new_static("W"),
                px(FONT_PX),
                &[TextRun {
                    len: 1,
                    font: term_font(FONT_FAMILY),
                    color: gpui::black(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            );
            let advance: f32 = probe.width.into();
            // SwiftTerm-style half-point snap keeps the grid pixel-aligned @2x.
            self.metrics = Some(CellMetrics {
                cell_w: (advance * 2.0).ceil() / 2.0,
                cell_h: CELL_H,
            });
        }
        let m = self.metrics_or_default();
        let grid = Arc::new(self.grid_snapshot());
        let (cursor_row, cursor_col) = self.cursor_point();
        let composing = self.ime.is_composing();
        let preedit = self.ime.preedit().to_string();
        let sel16 = self.ime.selected_range_utf16();
        let sel_bytes =
            utf16_to_byte(&preedit, sel16.start)..utf16_to_byte(&preedit, sel16.end);
        let entity = cx.entity();
        let focus_handle = self.focus_handle.clone();
        let preedit_for_paint = preedit.clone();
        let grid_w = COLS as f32 * m.cell_w + 2.0 * INSET;
        let grid_h = ROWS as f32 * m.cell_h + 2.0 * INSET;

        let grid_canvas = canvas(
            move |_bounds, _window, _cx| {},
            move |bounds: Bounds<Pixels>, _state, window: &mut Window, cx: &mut App| {
                // Register the IME handler for the next frame — paint-phase
                // only, active while our focus handle has focus.
                window.handle_input(
                    &focus_handle,
                    TermInputHandler {
                        view: entity.clone(),
                        element_bounds: bounds,
                    },
                    cx,
                );

                window.paint_quad(fill(bounds, rgb(BG)));
                let ox = bounds.origin.x + px(INSET);
                let oy = bounds.origin.y + px(INSET);

                // Committed grid, one shaped cell per glyph at exact cell
                // origins (the matrix bin's placement scheme, monochrome).
                for (r, row) in grid.iter().enumerate() {
                    let y = oy + px(r as f32 * m.cell_h);
                    for (c, &ch) in row.iter().enumerate() {
                        if ch == ' ' || ch == '\0' {
                            continue;
                        }
                        let mut buf = [0u8; 4];
                        let s: &str = ch.encode_utf8(&mut buf);
                        let run = TextRun {
                            len: s.len(),
                            font: term_font(FONT_FAMILY),
                            color: rgb(FG).into(),
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        let shaped = window.text_system().shape_line(
                            SharedString::new(s.to_string()),
                            px(FONT_PX),
                            &[run],
                            None,
                        );
                        let x = ox + px(c as f32 * m.cell_w);
                        shaped
                            .paint(point(x, y), px(m.cell_h), TextAlign::Left, None, window, cx)
                            .expect("paint grid cell");
                    }
                }

                let cur_x = ox + px(cursor_col as f32 * m.cell_w);
                let cur_y = oy + px(cursor_row as f32 * m.cell_h);

                if !composing {
                    // Block cursor.
                    let cursor_color: gpui::Hsla = rgb(CURSOR).into();
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(cur_x, cur_y),
                            size: size(px(m.cell_w), px(m.cell_h)),
                        },
                        cursor_color.opacity(0.35),
                    ));
                } else {
                    // Preedit overlay at the cursor (plan §Preedit rendering):
                    // never enters the grid model. Whole preedit underlined
                    // thin; the IME's selected sub-range underlined thick.
                    let font = term_font(FONT_FAMILY);
                    let underline = |thickness: f32| {
                        Some(UnderlineStyle {
                            thickness: px(thickness),
                            color: Some(rgb(PREEDIT_FG).into()),
                            wavy: false,
                        })
                    };
                    let seg = |len: usize, thick: bool| TextRun {
                        len,
                        font: font.clone(),
                        color: rgb(PREEDIT_FG).into(),
                        background_color: None,
                        underline: underline(if thick { 2.0 } else { 1.0 }),
                        strikethrough: None,
                    };
                    let runs: Vec<TextRun> = [
                        seg(sel_bytes.start, false),
                        seg(sel_bytes.end - sel_bytes.start, true),
                        seg(preedit_for_paint.len() - sel_bytes.end, false),
                    ]
                    .into_iter()
                    .filter(|run| run.len > 0)
                    .collect();
                    let shaped = window.text_system().shape_line(
                        SharedString::new(preedit_for_paint.clone()),
                        px(FONT_PX),
                        &runs,
                        None,
                    );
                    let strip: gpui::Hsla = rgb(PREEDIT_STRIP).into();
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(cur_x, cur_y),
                            size: size(shaped.width, px(m.cell_h)),
                        },
                        strip.opacity(0.25),
                    ));
                    shaped
                        .paint(
                            point(cur_x, cur_y),
                            px(m.cell_h),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        )
                        .expect("paint preedit");
                    // Composition caret at the selection start.
                    let caret_x = cur_x + shaped.x_for_index(sel_bytes.start);
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(caret_x, cur_y),
                            size: size(px(2.0), px(m.cell_h)),
                        },
                        rgb(CARET),
                    ));
                }
            },
        )
        .w(px(grid_w))
        .h(px(grid_h));

        let status = format!(
            "composing: {} | preedit: {:?} sel {:?} (utf16) | swallow-armed: {} | option-as-meta: {}",
            if composing { "YES" } else { "no" },
            preedit,
            sel16,
            if self.ime.commit_swallow_armed() {
                "YES"
            } else {
                "no"
            },
            if self.option_as_meta { "ON" } else { "off" },
        );
        let anchor = format!(
            "candidate anchor (bounds_for_range, {} calls, Some by construction): {}",
            self.bounds_calls, self.last_anchor
        );
        let pty = format!(
            "pty writes ({} total): {}",
            self.pty_writes_total,
            if self.pty_log.is_empty() {
                "(none)".to_string()
            } else {
                self.pty_log
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("  ")
            }
        );

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(HUD_BG))
            .text_color(rgb(FG))
            .font_family(FONT_FAMILY)
            .text_size(px(11.0))
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(|this, _: &ProbeCmdLeft, _, cx| this.probe("cmd-left", cx)))
            .on_action(cx.listener(|this, _: &ProbeCmdRight, _, cx| this.probe("cmd-right", cx)))
            .on_action(cx.listener(|this, _: &ProbeCmdK, _, cx| this.probe("cmd-k", cx)))
            .child(grid_canvas)
            .child(div().px_2().pt_2().text_color(rgb(PREEDIT_FG)).child(status))
            .child(div().px_2().text_color(rgb(CARET)).child(anchor))
            .child(div().px_2().child(pty))
            .child(
                div()
                    .px_2()
                    .pt_1()
                    .text_color(rgb(HUD_DIM))
                    .child("--- NSTextInputClient / key log (newest last) ---"),
            )
            .children(
                self.event_log
                    .iter()
                    .map(|line| div().px_2().text_color(rgb(HUD_DIM)).child(line.clone())),
            )
            .child(
                div().px_2().pt_1().text_color(rgb(HUD_DIM)).child(
                    "checklist: Pinyin nihao+Enter (commit, NO newline) | dead key \u{2325}e,e \u{2192} \u{e9} | \
                     hold e = repeat | arrows/F2 move cursor \u{2192} anchor follows | cmd-left/right/k probes",
                ),
            )
    }
}

// ---- main -----------------------------------------------------------------------

fn main() {
    // Build-safety gate: this is a LIVE spike (opens a real window; needs a
    // display + human). Subagents must never set this variable.
    if std::env::var("NICE_IME_SPIKE_RUN").as_deref() != Ok("1") {
        eprintln!(
            "[ime-spike] Live IME spike (opens a GUI window; needs a display and a human\n\
             [ime-spike] at the keyboard). Refusing to run without the explicit gate.\n\
             [ime-spike]\n\
             [ime-spike]   NICE_IME_SPIKE_RUN=1 cargo run --bin ime-spike [-- --option-as-meta]\n\
             [ime-spike]\n\
             [ime-spike] Checklist + interpretation: spikes/phase0-poc/ime-spike/RUN-IME.md"
        );
        return;
    }
    let option_as_meta = std::env::args().any(|arg| arg == "--option-as-meta");

    application().run(move |cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            // The historical zed#27572-revert casualties: these firing while
            // an IME is active-but-idle is test-matrix case 7.
            KeyBinding::new("cmd-left", ProbeCmdLeft, None),
            KeyBinding::new("cmd-right", ProbeCmdRight, None),
            KeyBinding::new("cmd-k", ProbeCmdK, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());

        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(120.0), px(120.0)),
                        size: size(px(660.0), px(560.0)),
                    })),
                    focus: true,
                    show: true,
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| ImeSpikeView::new(option_as_meta, cx)),
            )
            .expect("open window");

        window
            .update(cx, |view, window, cx| {
                window.focus(&view.focus_handle, cx);
                cx.activate(true);
            })
            .expect("focus spike window");
    });
}
