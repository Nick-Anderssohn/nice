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
use std::sync::mpsc::Sender;
use std::sync::Arc;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;

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
