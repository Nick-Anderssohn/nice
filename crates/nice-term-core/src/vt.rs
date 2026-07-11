//! The `alacritty_terminal` VT glue + the grid read API (What-to-build item 3).
//!
//! This is the headless VT core the [`crate::session::TermSession`] owns: the
//! [`EventListener`] the `Term` reports through, the [`Dimensions`] adapter used
//! at construction/resize, the shared-`Term` type the renderer (R4) locks, and
//! the owned grid snapshots tests and the renderer read **without holding the
//! `FairMutex<Term>` lock across paints** — every read here locks briefly,
//! copies to owned data, and unlocks before returning.
//!
//! There is deliberately no rendering here (per-cell colour/flags is R4) and no
//! `alacritty_terminal::tty` — the pty is Nice's own libc [`crate::PtyProcess`].

use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{
    cursor_icon::CursorIcon, Attr, CharsetIndex, ClearMode, CursorShape, CursorStyle, Handler,
    Hyperlink, KeyboardModes, KeyboardModesApplyBehavior, LineClearMode, Mode, ModifyOtherKeys,
    PrivateMode, Rgb, ScpCharPath, ScpUpdateMode, StandardCharset, TabulationClearMode,
};

use crate::deferred::SessionEvent;

/// Parity scrollback limit: SwiftTerm's `TerminalOptions` default, which today's
/// Nice never overrides. `alacritty_terminal`'s own `Config` default is 10_000,
/// so a session MUST set this explicitly for parity — the knob is per-session
/// (`TermSession::spawn`), and perf/memory validations pass a larger value.
pub const DEFAULT_SCROLLBACK_LINES: usize = 500;

/// The `Term` behind a [`FairMutex`], shared between the feeder thread (which
/// parses pty bytes into it) and readers (the renderer in R4; the grid read API
/// here). This is the crate's exported threading seam: parse off-main, share the
/// `Arc<FairMutex<Term>>`, wake the reader via the damage callback.
pub type SharedTerm = Arc<FairMutex<Term<EventProxy>>>;

/// The [`EventListener`] a session's `Term` reports through.
///
/// It carries the pty master fd so terminal **replies** — Device Attributes /
/// cursor-position reports and the like, which alacritty surfaces as
/// [`Event::PtyWrite`] — are written straight back to the child. Without that, a
/// login+interactive zsh with a query-happy prompt (the user's powerlevel10k)
/// stalls waiting for a DA/DSR answer.
///
/// It also carries an optional clone of the owning [`Session`]'s outward event
/// [`Sender`] so **OSC 0/2 window-title** changes reach the typed stream: an
/// [`Event::Title`] becomes [`SessionEvent::TitleChanged`] and an
/// [`Event::ResetTitle`] becomes [`SessionEvent::TitleReset`]. (OSC 1 icon-title
/// is dropped end-to-end — matching SwiftTerm's handling — so alacritty never
/// raises a distinct event for it and there is nothing to forward.) When there
/// is no `Session` (a bare [`crate::session::TermSession`]) the sink is `None`
/// and titles are dropped. Every remaining event (`Bell`/`Wakeup`/
/// `ColorRequest`/clipboard/…) is ignored: damage is signalled by the feeder
/// after each parsed chunk (not via `Wakeup`), and OSC 7 cwd is teed off the raw
/// byte stream in the feeder (see [`crate::osc7`]), not here.
///
/// `send_event` is called from inside `parser.advance(&mut term, …)` while the
/// feeder holds the `Term` lock, so it MUST NOT re-lock the `Term`. Neither
/// writing to the pty fd nor sending on the channel touches the lock, so there
/// is no re-entrancy.
///
/// [`Session`]: crate::deferred::Session
pub struct EventProxy {
    pty_fd: RawFd,
    /// The `Session`'s outward event sink, if this `Term` belongs to one.
    events: Option<Sender<SessionEvent>>,
}

impl EventProxy {
    /// Wire the proxy to the pty master fd its `Term` writes replies to, and to
    /// the optional outward event sink title changes are forwarded on.
    /// Crate-internal: only [`crate::session::TermSession`] constructs one, with
    /// the fd of the `PtyProcess` it owns (kept alive for the proxy's lifetime)
    /// and — when a [`Session`](crate::deferred::Session) owns it — a clone of
    /// that session's event `Sender`.
    pub(crate) fn new(pty_fd: RawFd, events: Option<Sender<SessionEvent>>) -> EventProxy {
        EventProxy { pty_fd, events }
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(text) => write_all_fd(self.pty_fd, text.as_bytes()),
            // OSC 0/2 set the window/tab title; the payload is already decoded
            // UTF-8 (emoji/CJK/braille intact — Stage 3 depends on that
            // fidelity). Best-effort send: a dropped receiver is not worth
            // failing the parse over.
            Event::Title(title) => {
                if let Some(events) = &self.events {
                    let _ = events.send(SessionEvent::TitleChanged(title));
                }
            }
            Event::ResetTitle => {
                if let Some(events) = &self.events {
                    let _ = events.send(SessionEvent::TitleReset);
                }
            }
            _ => {}
        }
    }
}

/// Best-effort blocking write of a terminal reply to the pty master. Replies are
/// short control sequences (DA/DSR/…), so this never meaningfully blocks; on any
/// error (incl. a closed fd during teardown) the reply is dropped rather than
/// surfaced — a missed status reply is not worth failing the parse over.
fn write_all_fd(fd: RawFd, data: &[u8]) {
    let mut off = 0usize;
    while off < data.len() {
        let n = unsafe {
            libc::write(
                fd,
                data[off..].as_ptr() as *const libc::c_void,
                data.len() - off,
            )
        };
        if n > 0 {
            off += n as usize;
        } else if n == 0 {
            return;
        } else {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return;
        }
    }
}

/// SwiftTerm-parity VT handler: a thin forwarding wrapper around alacritty's
/// `Term` that the feeder parses into instead of the bare `Term`, overriding
/// exactly one control: **ED(2) — `CSI 2 J`, erase-all — erases the screen in
/// place** instead of alacritty's xterm-alike "scroll the viewport into
/// history" (`Grid::clear_viewport`).
///
/// Why: prod (SwiftTerm fork, `Terminal.swift` `cmdEraseInDisplay` case 2)
/// resets each viewport line in place — erased content is *gone*, not pushed
/// into scrollback. macOS `/usr/bin/clear` emits `ESC[3J ESC[H ESC[2J` — ED(3)
/// **first** — so under alacritty's semantics ED(3) clears the old history and
/// then ED(2) pushes the just-cleared prompt into it, leaving the pre-`clear`
/// screen reachable by scrolling up. Prod leaves nothing. Parity = in-place.
///
/// Everything else forwards verbatim to `Term`'s own [`Handler`] impl via
/// UFCS (never method syntax, which could resolve to a same-named inherent
/// method). The forward list is the complete `Handler` surface of the pinned
/// `vte` 0.15 (71 methods, all `()`-returning, all default-no-op) minus
/// `clear_screen`; on a vte/alacritty upgrade, re-diff the trait — a missed
/// new method would silently no-op here.
///
/// Damage note (binding since fix round r5b): the renderer damage-gates its
/// per-row snapshot on `Term::damage()` (it re-copies/re-plans ONLY damaged
/// viewport rows — the full-grid rebuild per draw measured 0.42 cpu_s idle /
/// 7.67 cpu_s at 120 cps vs Swift's 0.05 / 2.68 on 2026-07-10). The in-place
/// ED(2) branch mutates the grid through `Grid::reset_region`, which alacritty's
/// own damage tracking cannot see (`Term::mark_fully_damaged` is private, and
/// the vanilla `clear_screen` path we bypass is what would have set it) — so
/// this wrapper raises `full_damage`, an out-of-band "the whole viewport
/// changed" flag the feeder shares with the renderer
/// ([`crate::session::TermSession::take_forced_full_damage`]). Every OTHER
/// forwarded control marks its own damage inside vanilla alacritty 0.26; any
/// future override that mutates the grid directly MUST raise this flag too, or
/// the damage-gated renderer will leave stale rows on screen.
pub(crate) struct ParityTerm<'a> {
    term: &'a mut Term<EventProxy>,
    /// Set (never cleared here) when an override mutated the grid outside
    /// alacritty's damage tracking. Written under the `Term` lock (the feeder
    /// parses while holding it); the renderer takes-and-clears it under the
    /// same lock, so no set can race a snapshot.
    full_damage: &'a AtomicBool,
}

impl<'a> ParityTerm<'a> {
    /// Wrap `term` for one parse pass, wiring the out-of-band damage flag the
    /// in-place ED(2) override raises (see the struct docs).
    pub(crate) fn new(term: &'a mut Term<EventProxy>, full_damage: &'a AtomicBool) -> Self {
        ParityTerm { term, full_damage }
    }
}

/// Forward `Handler` methods verbatim to the wrapped `Term`'s impl.
macro_rules! forward_handler {
    ($($name:ident($($arg:ident: $ty:ty),*);)+) => {
        $(
            #[inline]
            fn $name(&mut self, $($arg: $ty),*) {
                Handler::$name(&mut *self.term, $($arg),*)
            }
        )+
    };
}

impl Handler for ParityTerm<'_> {
    /// ED — with the parity override for ED(2) on the primary screen. The
    /// alt screen has no history, so alacritty's own behaviour there is
    /// already in-place; delegate it (and every other clear mode) unchanged.
    fn clear_screen(&mut self, mode: ClearMode) {
        if matches!(mode, ClearMode::All) && !self.term.mode().contains(TermMode::ALT_SCREEN) {
            // SwiftTerm `cmdEraseInDisplay` case 2: reset every viewport line
            // in place (to the cursor template, i.e. the current erase
            // attributes) — do NOT rotate the region into scrollback.
            self.term.grid_mut().reset_region(..);
            // ED(2) also drops any selection (alacritty's All branch does the
            // same unconditionally).
            self.term.selection = None;
            // `reset_region` bypasses alacritty's damage tracking (see the
            // struct docs): tell the damage-gated renderer the whole viewport
            // changed, or it would keep painting the pre-clear rows.
            self.full_damage.store(true, Ordering::Release);
        } else {
            Handler::clear_screen(&mut *self.term, mode);
        }
    }

    forward_handler! {
        set_title(title: Option<String>);
        set_cursor_style(style: Option<CursorStyle>);
        set_cursor_shape(shape: CursorShape);
        input(c: char);
        goto(line: i32, col: usize);
        goto_line(line: i32);
        goto_col(col: usize);
        insert_blank(n: usize);
        move_up(n: usize);
        move_down(n: usize);
        identify_terminal(intermediate: Option<char>);
        device_status(n: usize);
        move_forward(col: usize);
        move_backward(col: usize);
        move_down_and_cr(row: usize);
        move_up_and_cr(row: usize);
        put_tab(count: u16);
        backspace();
        carriage_return();
        linefeed();
        bell();
        substitute();
        newline();
        set_horizontal_tabstop();
        scroll_up(n: usize);
        scroll_down(n: usize);
        insert_blank_lines(n: usize);
        delete_lines(n: usize);
        erase_chars(n: usize);
        delete_chars(n: usize);
        move_backward_tabs(count: u16);
        move_forward_tabs(count: u16);
        save_cursor_position();
        restore_cursor_position();
        clear_line(mode: LineClearMode);
        clear_tabs(mode: TabulationClearMode);
        set_tabs(interval: u16);
        reset_state();
        reverse_index();
        terminal_attribute(attr: Attr);
        set_mode(mode: Mode);
        unset_mode(mode: Mode);
        report_mode(mode: Mode);
        set_private_mode(mode: PrivateMode);
        unset_private_mode(mode: PrivateMode);
        report_private_mode(mode: PrivateMode);
        set_scrolling_region(top: usize, bottom: Option<usize>);
        set_keypad_application_mode();
        unset_keypad_application_mode();
        set_active_charset(index: CharsetIndex);
        configure_charset(index: CharsetIndex, charset: StandardCharset);
        set_color(index: usize, color: Rgb);
        dynamic_color_sequence(prefix: String, index: usize, terminator: &str);
        reset_color(index: usize);
        clipboard_store(clipboard: u8, base64: &[u8]);
        clipboard_load(clipboard: u8, terminator: &str);
        decaln();
        push_title();
        pop_title();
        text_area_size_pixels();
        text_area_size_chars();
        set_hyperlink(hyperlink: Option<Hyperlink>);
        set_mouse_cursor_icon(icon: CursorIcon);
        report_keyboard_mode();
        push_keyboard_mode(mode: KeyboardModes);
        pop_keyboard_modes(to_pop: u16);
        set_keyboard_mode(mode: KeyboardModes, behavior: KeyboardModesApplyBehavior);
        set_modify_other_keys(mode: ModifyOtherKeys);
        report_modify_other_keys();
        set_scp(char_path: ScpCharPath, update_mode: ScpUpdateMode);
    }
}

/// The [`Dimensions`] alacritty needs at `Term::new` / `Term::resize`. Only the
/// three required methods are meaningful; `history_size` (a provided method)
/// falls out of `total_lines - screen_lines == 0` here, which is correct — the
/// scrollback limit is carried by the `Term`'s own `Config`, not this hint.
#[derive(Clone, Copy)]
pub struct TermSize {
    pub rows: usize,
    pub cols: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// An owned snapshot of the **visible viewport** (honouring the scroll/display
/// offset), top row to bottom. Owned so the caller never holds the `Term` lock
/// while it paints or asserts. The renderer (R4) will grow a richer per-cell
/// form; this text view is what the grid read API and headless tests need.
#[derive(Clone, Debug)]
pub struct GridSnapshot {
    /// Grid width in columns at snapshot time.
    pub cols: usize,
    /// Viewport height in rows (== the visible `screen_lines`).
    pub screen_rows: usize,
    /// One `String` per visible row, top→bottom, trailing blank cells trimmed.
    pub rows: Vec<String>,
}

impl GridSnapshot {
    /// Whether any visible row contains `needle`. (For scrollback-inclusive
    /// searches use [`crate::session::TermSession::grid_contains`].)
    pub fn contains(&self, needle: &str) -> bool {
        self.rows.iter().any(|r| r.contains(needle))
    }

    /// The viewport as one newline-joined string (top→bottom).
    pub fn text(&self) -> String {
        self.rows.join("\n")
    }
}

/// Build a visible-viewport [`GridSnapshot`] from a locked `Term`. Caller holds
/// the lock only for this call; the result is fully owned.
pub(crate) fn visible_snapshot(term: &Term<EventProxy>) -> GridSnapshot {
    let screen_rows = term.screen_lines();
    let cols = term.columns();
    let display_offset = term.grid().display_offset() as i32;
    let mut rows = Vec::with_capacity(screen_rows);
    for line in 0..screen_rows {
        // Viewport row `line` maps to buffer line `line - display_offset`.
        rows.push(row_text(term, Line(line as i32 - display_offset), cols));
    }
    GridSnapshot {
        cols,
        screen_rows,
        rows,
    }
}

/// Every buffer line — scrollback history first, then the visible screen — as
/// trailing-trimmed owned `String`s. Independent of the display offset (it walks
/// absolute buffer lines `-history_size ..= screen-1`), so a marker that has
/// scrolled into history is still found. Used by the scrollback-inclusive
/// "grid contains string" read tests rely on.
pub(crate) fn all_buffer_lines(term: &Term<EventProxy>) -> Vec<String> {
    let cols = term.columns();
    let history = term.grid().history_size() as i32;
    let screen = term.screen_lines() as i32;
    let mut out = Vec::with_capacity((history + screen) as usize);
    for line in -history..screen {
        out.push(row_text(term, Line(line), cols));
    }
    out
}

/// One buffer row as text: each cell's char (NUL rendered as a space), trailing
/// blanks trimmed. Trailing cells are ASCII spaces, so the byte truncation lands
/// on a char boundary.
fn row_text(term: &Term<EventProxy>, line: Line, cols: usize) -> String {
    let mut s = String::with_capacity(cols);
    for col in 0..cols {
        let c = term.grid()[Point::new(line, Column(col))].c;
        s.push(if c == '\0' { ' ' } else { c });
    }
    let end = s.trim_end().len();
    s.truncate(end);
    s
}

#[cfg(test)]
mod tests {
    //! Pins the SwiftTerm-parity ED semantics of [`ParityTerm`] (headless: a
    //! bare `Term` + `Processor`, no pty). Prod reference:
    //! `SwiftTerm/Sources/SwiftTerm/Terminal.swift` `cmdEraseInDisplay` —
    //! case 2 erases viewport lines in place (never pushes into scrollback),
    //! case 3 trims the scrollback.

    use super::*;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

    const ROWS: usize = 5;
    const COLS: usize = 20;

    fn new_term() -> Term<EventProxy> {
        let config = Config {
            scrolling_history: DEFAULT_SCROLLBACK_LINES,
            ..Config::default()
        };
        // fd -1: no reply sequences are exercised here, writes are dropped.
        Term::new(
            config,
            &TermSize {
                rows: ROWS,
                cols: COLS,
            },
            EventProxy::new(-1, None),
        )
    }

    /// Feed bytes through the same parity handler the session feeder uses,
    /// returning whether the pass raised the out-of-band full-damage flag
    /// (the r5b renderer side-channel — most tests ignore it).
    fn feed(term: &mut Term<EventProxy>, bytes: &[u8]) -> bool {
        let mut parser: Processor = Processor::new();
        let full_damage = AtomicBool::new(false);
        parser.advance(&mut ParityTerm::new(term, &full_damage), bytes);
        full_damage.load(Ordering::Acquire)
    }

    /// Print enough numbered lines that some scroll into history.
    fn fill_with_history(term: &mut Term<EventProxy>) {
        for i in 0..(ROWS + 4) {
            feed(term, format!("line-{i}\r\n").as_bytes());
        }
        assert!(
            term.grid().history_size() > 0,
            "setup: expected lines to have scrolled into history"
        );
    }

    fn viewport_is_blank(term: &Term<EventProxy>) -> bool {
        visible_snapshot(term).rows.iter().all(|r| r.is_empty())
    }

    /// macOS `/usr/bin/clear` (`ESC[3J ESC[H ESC[2J` — ED(3) FIRST): afterwards
    /// the scrollback is empty and scrolling up can reveal nothing, matching
    /// prod. Under bare alacritty semantics ED(2) would re-push the viewport
    /// into the just-cleared history, leaving it scrollable — the M7.8 bug.
    #[test]
    fn macos_clear_sequence_leaves_no_scrollback() {
        let mut term = new_term();
        fill_with_history(&mut term);

        feed(&mut term, b"\x1b[3J\x1b[H\x1b[2J");

        assert_eq!(
            term.grid().history_size(),
            0,
            "clear must leave nothing scrollable"
        );
        assert!(viewport_is_blank(&term), "clear must blank the viewport");
        assert!(
            all_buffer_lines(&term).iter().all(|r| r.is_empty()),
            "no buffer line (history or screen) may retain content"
        );
    }

    /// xterm-order clear (`ESC[H ESC[2J ESC[3J`) must end in the same state.
    #[test]
    fn xterm_clear_sequence_leaves_no_scrollback() {
        let mut term = new_term();
        fill_with_history(&mut term);

        feed(&mut term, b"\x1b[H\x1b[2J\x1b[3J");

        assert_eq!(term.grid().history_size(), 0);
        assert!(viewport_is_blank(&term));
    }

    /// ED(2) alone, prod parity (SwiftTerm `cmdEraseInDisplay` case 2): the
    /// viewport is erased **in place** — pre-existing scrollback is kept
    /// exactly as-is, and the erased screen content is NOT added to it.
    #[test]
    fn ed2_alone_erases_in_place_without_touching_history() {
        let mut term = new_term();
        fill_with_history(&mut term);
        let history_before = term.grid().history_size();

        feed(&mut term, b"\x1b[2J");

        assert_eq!(
            term.grid().history_size(),
            history_before,
            "ED(2) must neither push the screen into history nor clear it"
        );
        assert!(viewport_is_blank(&term), "ED(2) must blank the viewport");
    }

    /// ED(2) on the alt screen delegates to alacritty unchanged (the alt
    /// screen has no history, and in-place erase is already its behaviour).
    #[test]
    fn ed2_on_alt_screen_stays_in_place() {
        let mut term = new_term();
        feed(&mut term, b"\x1b[?1049halt-content");
        assert!(visible_snapshot(&term).contains("alt-content"));

        feed(&mut term, b"\x1b[2J");

        assert_eq!(term.grid().history_size(), 0);
        assert!(viewport_is_blank(&term));
    }

    // ---- r5b out-of-band damage flag ------------------------------------
    //
    // The renderer damage-gates per-row snapshots on `Term::damage()` (fix
    // round r5b); the in-place ED(2) override mutates the grid where alacritty
    // cannot see it, so it must raise the side-channel flag instead — and
    // ONLY it (a spurious flag would defeat the gating; a missing one leaves
    // stale rows on screen).

    /// The in-place ED(2) branch (primary screen) bypasses alacritty's damage
    /// tracking, so it must raise the out-of-band flag for the renderer.
    #[test]
    fn ed2_in_place_raises_the_full_damage_flag() {
        let mut term = new_term();
        feed(&mut term, b"some-content");

        assert!(
            feed(&mut term, b"\x1b[2J"),
            "the in-place ED(2) erase must flag full damage out of band"
        );
    }

    /// Ordinary output marks its own damage inside vanilla alacritty; the
    /// side-channel must stay quiet or every frame would full-invalidate.
    #[test]
    fn plain_output_does_not_raise_the_full_damage_flag() {
        let mut term = new_term();
        assert!(!feed(&mut term, b"hello\r\nworld"));
        // ED(3) and ED(0) delegate to alacritty (which tracks its own damage).
        assert!(!feed(&mut term, b"\x1b[3J\x1b[J"));
    }

    /// The delegated alt-screen ED(2) runs alacritty's own `clear_screen`,
    /// which marks the `Term` fully damaged itself — the side-channel is not
    /// needed and must stay quiet there.
    #[test]
    fn ed2_on_alt_screen_uses_alacritty_damage_not_the_flag() {
        use alacritty_terminal::term::TermDamage;

        let mut term = new_term();
        feed(&mut term, b"\x1b[?1049halt-content");
        term.reset_damage();

        assert!(!feed(&mut term, b"\x1b[2J"), "delegated ED(2) must not flag");
        assert!(
            matches!(term.damage(), TermDamage::Full),
            "alacritty's own clear_screen marks the Term fully damaged"
        );
    }
}
