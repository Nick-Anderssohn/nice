//! Phase-0 PoC #2 — a SINGLE-STACK, GPUI-NATIVE terminal.
//!
//! This is the §12 "build the single-stack prototype" spike: an
//! `alacritty_terminal` VT core rendered through GPUI's PUBLIC paint API
//! (`window.text_system().shape_line(...).paint()` for glyphs + `paint_quad`
//! for cell backgrounds), inside GPUI's own ONE `CAMetalLayer`. There is NO
//! SwiftTerm, NO objc2 embed, NO second Metal stack — exactly the Path-B
//! architecture. It exists to MEASURE the one thing §12 said source could not
//! settle: sustained burst FPS + present pacing of a GPUI-native terminal under
//! a continuous Claude-streaming workload, on a single GPUI stack, vs the
//! Path-A dual-stack numbers (§10: txn p50 18.3 / p95 31 ms) and the
//! single-stack baseline (~16.7 ms).
//!
//! It deliberately REUSES `harness.rs` verbatim (`#[path]` include) so the
//! clock, FPS reducer, memory sampler and synthetic workload are identical to
//! the Path-A PoC — apples-to-apples.
//!
//! Run modes (env `NICE_POC_RUN`):
//!   * UNSET / "0" — HEADLESS self-test: builds the Term, feeds the workload,
//!     snapshots a few frames of the grid, asserts the render-data path is
//!     non-empty. No window, no display, safe under `cargo build`/subagents.
//!   * "1"        — DISPLAY-GATED live run: opens a GPUI window, renders the
//!     grid every frame while streaming the workload + animating a sub-pixel
//!     vertical offset (the sub-line smooth-scroll path), self-terminates after
//!     NICE_POC_SECS (default 18) and prints a populated FPS/memory summary +
//!     a raw CSV. REQUIRES a display; the writing subagent MUST NOT run it.
//!
//! Spike 8 restructure (2026-07-01): pty-equivalent feeding/parsing no longer
//! happens inside `render()` on the main thread — each session owns a feeder
//! thread that parses the workload into a `FairMutex<Term>` at a wall-clock
//! byte rate; `render()` only locks briefly to snapshot (see `Session`).
//! Multi-session flags (live run only; defaults preserve the single-window
//! behavior exactly):
//!   * NICE_POC_WINDOWS=K    open K windows (default 1, clamped 1..=16)
//!   * NICE_POC_STREAMING=M  first M windows stream the full workload + RAF-
//!     render continuously (default K; window 0 is always streaming — it is
//!     the measurement coordinator)
//!   * NICE_POC_BG_BPS=N     byte rate for the K-M background sessions
//!     (default 0 = idle-with-live-session: 1 heartbeat line/s, demand-driven
//!     redraw via a dirty-flag notify poller instead of RAF)
//!   * Spike 8 target scenario: NICE_POC_WINDOWS=7 NICE_POC_STREAMING=3
//!     (3 streaming + 4 background), optionally NICE_POC_BG_BPS=500000 to make
//!     the 4 background parsers churn at full rate off the main thread.
//!
//! Interactive keystroke-latency mode (spikes 4b/5, 2026-07-01):
//!   * NICE_POC_INTERACTIVE=1 NICE_POC_RUN=1 — ONE window, `/bin/cat` behind a
//!     REAL pty, no workload, no RAF: typed keys are written raw to the pty,
//!     the kernel canonical-mode echo triggers exactly one demand-driven
//!     Metal draw (channel wakeup -> cx.notify + setNeedsDisplay kick ->
//!     displayLayer -> present; notify alone never presents when the display
//!     link is paused), so the vendored-gpui "Draw" signpost is damage-gated
//!     (a valid latency anchor). Auto-sets NICE_POC_DAMAGE_ONLY=1 to disable
//!     gpui's 1s-after-input keepalive present. NICE_POC_SECS (default 120)
//!     then a one-line summary incl. the REAL MetalRenderer::draw count.
//!     Multi-session flags ignored.
//!   * NICE_POC_INTERACTIVE=1 alone — HEADLESS pty/echo self-test (no window).
//!
//! The vendored gpui (vendor/gpui-0.2.2, see README) additionally provides:
//!   * NICE_POC_GPUI_TXN=1 — force GPUI's Metal present into
//!     presents_with_transaction mode (spike 3, works for this bin too);
//!   * an os_signpost interval "Draw" (subsystem dev.nickanderssohn.gpui-term,
//!     category "present") around every GPUI Metal draw/present — in THIS bin
//!     (single stack) that IS the terminal present (spike 5 latency).

#![allow(dead_code)]

#[path = "harness.rs"]
mod harness;

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point as TermPoint};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};

use harness::{Workload, WorkloadProfile};

// ---- terminal geometry / style -------------------------------------------

const ROWS: usize = 40;
const COLS: usize = 120;
const FONT_PX: f32 = 14.0;
const LINE_PX: f32 = 18.0;
const DEFAULT_FG: u32 = 0x00C8_C8C8;
const DEFAULT_BG: u32 = 0x000B_0B0B;

/// Minimal `Dimensions` (alacritty's own `TermSize` is `#[cfg(test)]`).
#[derive(Clone, Copy)]
struct Size {
    rows: usize,
    cols: usize,
}
impl Dimensions for Size {
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

#[derive(Clone)]
struct EventProxy;
impl EventListener for EventProxy {
    fn send_event(&self, _event: Event) {}
}

// ---- color mapping (alacritty Color -> 0xRRGGBB) -------------------------

fn palette16(i: usize) -> u32 {
    const P: [u32; 16] = [
        0x00000000, 0x00CC0000, 0x004E9A06, 0x00C4A000, 0x003465A4, 0x0075507B, 0x0006989A,
        0x00D3D7CF, 0x00555753, 0x00EF2929, 0x008AE234, 0x00FCE94F, 0x00729FCF, 0x00AD7FA8,
        0x0034E2E2, 0x00EEEEEC,
    ];
    P[i & 0xF]
}

fn xterm256(i: u8) -> u32 {
    match i {
        0..=15 => palette16(i as usize),
        16..=231 => {
            let i = i - 16;
            let r = (i / 36) as u32;
            let g = ((i % 36) / 6) as u32;
            let b = (i % 6) as u32;
            let c = |v: u32| if v == 0 { 0 } else { v * 40 + 55 };
            (c(r) << 16) | (c(g) << 8) | c(b)
        }
        _ => {
            let v = (i as u32 - 232) * 10 + 8;
            (v << 16) | (v << 8) | v
        }
    }
}

fn color_rgb(c: Color, default: u32) -> u32 {
    match c {
        Color::Spec(rgb) => ((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | (rgb.b as u32),
        Color::Indexed(i) => xterm256(i),
        Color::Named(n) => match n {
            NamedColor::Foreground => DEFAULT_FG,
            NamedColor::Background => DEFAULT_BG,
            NamedColor::Black => palette16(0),
            NamedColor::Red => palette16(1),
            NamedColor::Green => palette16(2),
            NamedColor::Yellow => palette16(3),
            NamedColor::Blue => palette16(4),
            NamedColor::Magenta => palette16(5),
            NamedColor::Cyan => palette16(6),
            NamedColor::White => palette16(7),
            NamedColor::BrightBlack => palette16(8),
            NamedColor::BrightRed => palette16(9),
            NamedColor::BrightGreen => palette16(10),
            NamedColor::BrightYellow => palette16(11),
            NamedColor::BrightBlue => palette16(12),
            NamedColor::BrightMagenta => palette16(13),
            NamedColor::BrightCyan => palette16(14),
            NamedColor::BrightWhite => palette16(15),
            _ => default,
        },
    }
}

// ---- owned per-frame snapshot (moved into the 'static paint closure) -----

struct BgRun {
    col: usize,
    len: usize,
    rgb: u32,
}

struct RowSnap {
    text: String,
    /// (utf8-byte-len, fg-rgb) per cell, in column order.
    cells: Vec<(usize, u32)>,
    bgs: Vec<BgRun>,
}

fn snapshot(term: &Term<EventProxy>) -> Vec<RowSnap> {
    let rows = term.screen_lines();
    let cols = term.columns();
    let mut out = Vec::with_capacity(rows);
    for line in 0..rows {
        let mut text = String::with_capacity(cols);
        let mut cells = Vec::with_capacity(cols);
        let mut bgs: Vec<BgRun> = Vec::new();
        for col in 0..cols {
            let cell = &term.grid()[TermPoint::new(Line(line as i32), Column(col))];
            let inverse = cell.flags.contains(Flags::INVERSE);
            let mut fg = color_rgb(cell.fg, DEFAULT_FG);
            let mut bg = color_rgb(cell.bg, DEFAULT_BG);
            if inverse {
                std::mem::swap(&mut fg, &mut bg);
            }
            let ch = if cell.c == '\0' { ' ' } else { cell.c };
            let mut buf = [0u8; 4];
            let s = ch.encode_utf8(&mut buf);
            text.push_str(s);
            cells.push((s.len(), fg));
            if bg != DEFAULT_BG {
                if let Some(last) = bgs.last_mut() {
                    if last.rgb == bg && last.col + last.len == col {
                        last.len += 1;
                        continue;
                    }
                }
                bgs.push(BgRun { col, len: 1, rgb: bg });
            }
        }
        out.push(RowSnap { text, cells, bgs });
    }
    out
}

// =========================================================================
// Multi-session config + per-window registries (spike 8 prep).
// =========================================================================

/// Multi-window / multi-session live-run configuration (spike 8). Defaults
/// reproduce the original single-window run exactly.
#[derive(Clone, Copy, Debug)]
struct MultiCfg {
    /// NICE_POC_WINDOWS — total windows (default 1, clamped 1..=16).
    windows: usize,
    /// NICE_POC_STREAMING — first M windows stream the full workload and
    /// RAF-render continuously (default = windows, clamped 1..=windows;
    /// window 0 is always streaming — it is the measurement coordinator).
    streaming: usize,
    /// NICE_POC_BG_BPS — byte rate of the windows-streaming background
    /// sessions (default 0 = idle-with-live-session heartbeat, 1 line/s).
    bg_bps: usize,
}

fn env_usize(key: &str) -> Option<usize> {
    std::env::var(key).ok().and_then(|s| s.parse::<usize>().ok())
}

impl MultiCfg {
    fn from_env() -> Self {
        let windows = env_usize("NICE_POC_WINDOWS").unwrap_or(1).clamp(1, 16);
        let streaming = env_usize("NICE_POC_STREAMING")
            .unwrap_or(windows)
            .clamp(1, windows);
        let bg_bps = env_usize("NICE_POC_BG_BPS").unwrap_or(0);
        MultiCfg {
            windows,
            streaming,
            bg_bps,
        }
    }
}

/// Per-window bookkeeping the window-0 coordinator reads at finalize time.
/// Slots are created (all at once) BEFORE any window opens.
struct WinSlot {
    streaming: bool,
    bps: usize,
    bytes_fed: Arc<AtomicU64>,
    frames: Mutex<Vec<u64>>,
}

static WIN_SLOTS: OnceLock<Vec<WinSlot>> = OnceLock::new();

fn win_slots() -> &'static [WinSlot] {
    WIN_SLOTS.get().map(|v| v.as_slice()).unwrap_or(&[])
}

fn stamp_window_frame(index: usize) {
    if let Some(slot) = win_slots().get(index) {
        slot.frames.lock().unwrap().push(harness::clock::now());
    }
}

fn clear_window_frames() {
    for slot in win_slots() {
        slot.frames.lock().unwrap().clear();
    }
}

// =========================================================================
// Session — the pty-equivalent byte source, parsed OFF the render path
// (spike 8 prep). The audited rev generated + parsed the workload inline in
// `render()` on the main thread (old gpui_term.rs:286-289); now a per-session
// feeder thread generates the deterministic workload and parses it into the
// shared `FairMutex<Term>`, wall-clock-paced to the profile byte rate.
// `render()` only locks briefly to snapshot. The dirty flag drives the idle
// windows' notify poller (streaming windows redraw via their RAF loop).
// =========================================================================

/// Feeder pacing quantum. 5 ms x (bytes_per_sec/200) reproduces the original
/// aggregate rate (~500 KB/s default) with a finer, render-independent clock.
const FEED_TICK_MS: u64 = 5;

struct Session {
    term: Arc<FairMutex<Term<EventProxy>>>,
    /// Set by the feeder after each parse; consumed by the notify poller.
    dirty: Arc<AtomicBool>,
    bytes_fed: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    feeder: Option<std::thread::JoinHandle<()>>,
}

impl Session {
    /// `bytes_per_sec == 0` => idle-with-live-session: the feeder stays alive
    /// and emits one short heartbeat line per second (a quiet prompt),
    /// exercising the parse -> dirty -> notify -> render wakeup path with
    /// negligible load.
    fn spawn(index: usize, seed: u64, bytes_per_sec: usize) -> Self {
        let size = Size {
            rows: ROWS,
            cols: COLS,
        };
        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &size,
            EventProxy,
        )));
        let dirty = Arc::new(AtomicBool::new(false));
        let bytes_fed = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let feeder = {
            let term = Arc::clone(&term);
            let dirty = Arc::clone(&dirty);
            let bytes_fed = Arc::clone(&bytes_fed);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name(format!("poc-feeder-{index}"))
                .spawn(move || {
                    let mut parser: Processor = Processor::new();
                    if bytes_per_sec == 0 {
                        let mut beat = 0u64;
                        while !stop.load(Ordering::Relaxed) {
                            beat += 1;
                            let line = format!("\r\x1b[2K[idle w{index}] heartbeat {beat}");
                            {
                                let mut t = term.lock();
                                parser.advance(&mut *t, line.as_bytes());
                            }
                            bytes_fed.fetch_add(line.len() as u64, Ordering::Relaxed);
                            dirty.store(true, Ordering::Release);
                            std::thread::sleep(Duration::from_secs(1));
                        }
                        return;
                    }
                    let mut wl = Workload::new(WorkloadProfile {
                        seed,
                        ..WorkloadProfile::default()
                    });
                    let per_tick = ((bytes_per_sec * FEED_TICK_MS as usize) / 1000).max(64);
                    while !stop.load(Ordering::Relaxed) {
                        let t0 = std::time::Instant::now();
                        // Generate OUTSIDE the lock; hold it only to parse.
                        let chunk = wl.stream(per_tick);
                        {
                            let mut t = term.lock();
                            parser.advance(&mut *t, &chunk);
                        }
                        bytes_fed.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                        dirty.store(true, Ordering::Release);
                        if let Some(rest) =
                            Duration::from_millis(FEED_TICK_MS).checked_sub(t0.elapsed())
                        {
                            std::thread::sleep(rest);
                        }
                    }
                })
                .expect("failed to spawn session feeder thread")
        };

        Session {
            term,
            dirty,
            bytes_fed,
            stop,
            feeder: Some(feeder),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.feeder.take() {
            let _ = h.join();
        }
    }
}

// =========================================================================
// PtySession — a REAL pty (alacritty_terminal::tty) with `/bin/cat` as the
// child, for the interactive keystroke-latency mode (spikes 4b/5, Path-B
// half). The kernel tty line discipline (canonical mode + ECHO, the openpty
// defaults) echoes typed bytes straight back through the master — the
// closest analog to the Nice Dev baseline's zsh+cat loopback (kernel
// canonical-mode echo). A dedicated reader thread blocking-reads the master,
// parses into the shared Term, counts bytes, sets `dirty`, and pings the
// optional `wake` channel so the GPUI side can cx.notify() (demand-driven
// redraw — no RAF anywhere in this mode).
// =========================================================================

struct PtySession {
    term: Arc<FairMutex<Term<EventProxy>>>,
    /// Set by the reader after each parsed chunk (asserted by the headless
    /// self-test; the live mode uses the wake channel instead).
    dirty: Arc<AtomicBool>,
    /// Every byte read back from the pty master (kernel echo + cat output).
    bytes_echoed: Arc<AtomicU64>,
    /// Write side (dup of the pty master fd) — keystroke bytes go here.
    writer: std::fs::File,
    _reader: std::thread::JoinHandle<()>,
}

impl PtySession {
    fn spawn(wake: Option<futures::channel::mpsc::UnboundedSender<()>>) -> std::io::Result<Self> {
        use std::io::Read;
        use std::os::fd::AsRawFd;

        let size = Size {
            rows: ROWS,
            cols: COLS,
        };
        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &size,
            EventProxy,
        )));
        let ws = WindowSize {
            num_lines: ROWS as u16,
            num_cols: COLS as u16,
            cell_width: 8,
            cell_height: 16,
        };
        // `/bin/cat` under the pty: canonical mode + ECHO stay ON, so the
        // kernel echoes every typed byte immediately and cat re-emits the
        // whole line after Return. (If cat ever fights the tty API, the
        // documented fallback is `/bin/zsh -c 'exec cat'`.)
        let opts = tty::Options {
            shell: Some(tty::Shell::new("/bin/cat".into(), Vec::new())),
            working_directory: None,
            drain_on_exit: false,
            env: std::collections::HashMap::new(),
        };
        let mut pty = tty::new(&opts, ws, 0)?;

        // alacritty's tty::new sets the master fd NON-blocking (its own event
        // loop polls). We use a dedicated blocking reader thread instead —
        // clear O_NONBLOCK. (File status flags live on the shared open-file
        // description, so clearing via the dup'd writer applies to the
        // reader() side too.)
        let writer = pty.file().try_clone()?;
        unsafe {
            let fd = writer.as_raw_fd();
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
        }

        let dirty = Arc::new(AtomicBool::new(false));
        let bytes_echoed = Arc::new(AtomicU64::new(0));
        let reader = {
            let term = Arc::clone(&term);
            let dirty = Arc::clone(&dirty);
            let bytes = Arc::clone(&bytes_echoed);
            std::thread::Builder::new()
                .name("poc-pty-reader".into())
                .spawn(move || {
                    use alacritty_terminal::tty::EventedReadWrite;
                    let mut parser: Processor = Processor::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        match pty.reader().read(&mut buf) {
                            Ok(0) => break, // child exited / EOF
                            Ok(n) => {
                                {
                                    let mut t = term.lock();
                                    parser.advance(&mut *t, &buf[..n]);
                                }
                                bytes.fetch_add(n as u64, Ordering::Relaxed);
                                dirty.store(true, Ordering::Release);
                                if let Some(tx) = &wake {
                                    let _ = tx.unbounded_send(());
                                }
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                // Safety net if O_NONBLOCK ever reappears.
                                std::thread::sleep(Duration::from_millis(1));
                            }
                            Err(_) => break,
                        }
                    }
                })
                .expect("failed to spawn pty reader thread")
        };

        Ok(PtySession {
            term,
            dirty,
            bytes_echoed,
            writer,
            _reader: reader,
        })
    }
}

// =========================================================================
// HEADLESS self-test (no GPUI, no window) — proves the render-data path.
// =========================================================================

fn run_headless() {
    eprintln!("{}", harness::banner());
    eprintln!("[gpui-term] HEADLESS self-test (no display). Set NICE_POC_RUN=1 for the live FPS run.");

    let size = Size { rows: ROWS, cols: COLS };
    let mut term = Term::new(Config::default(), &size, EventProxy);
    let mut parser: Processor = Processor::new();
    let prof = WorkloadProfile::default();
    let mut wl = Workload::new(prof);

    // Feed a few seconds of the synthetic workload and snapshot.
    let mut nonempty_rows = 0usize;
    let mut total_glyphs = 0usize;
    for _ in 0..120 {
        let chunk = wl.stream((prof.bytes_per_sec / 60).max(64));
        parser.advance(&mut term, &chunk);
    }
    let snap = snapshot(&term);
    for r in &snap {
        let t = r.text.trim_end();
        if !t.is_empty() {
            nonempty_rows += 1;
            total_glyphs += t.chars().count();
        }
    }

    eprintln!(
        "[gpui-term] snapshot: {} rows x {} cols, {} non-empty rows, {} visible glyphs",
        snap.len(),
        COLS,
        nonempty_rows,
        total_glyphs
    );
    let ok = snap.len() == ROWS && nonempty_rows > 0 && total_glyphs > 0;
    eprintln!("RESULT: {}", if ok { "PASS (render-data path live)" } else { "FAIL" });
    std::process::exit(if ok { 0 } else { 1 });
}

/// HEADLESS interactive self-test (`NICE_POC_INTERACTIVE=1` WITHOUT
/// `NICE_POC_RUN`): proves the pty half end-to-end with no window — spawn
/// `/bin/cat` behind a real pty, write "hello\r", wait for the kernel
/// canonical-mode echo + cat's line, then assert bytes flowed, the dirty flag
/// was set, and the parsed grid holds the echoed text (twice: echo + cat).
fn run_headless_interactive() {
    eprintln!("{}", harness::banner());
    eprintln!(
        "[gpui-term] HEADLESS interactive self-test (pty /bin/cat, no display). \
         Add NICE_POC_RUN=1 for the live keystroke-latency window."
    );

    let ps = match PtySession::spawn(None) {
        Ok(ps) => ps,
        Err(e) => {
            eprintln!("RESULT: FAIL (pty spawn: {e})");
            std::process::exit(1);
        }
    };

    {
        use std::io::Write;
        let mut w = ps.writer.try_clone().expect("dup pty writer");
        w.write_all(b"hello\r").expect("pty write failed");
        let _ = w.flush();
    }

    // Expect ~14 bytes back: the kernel echo ("hello\r\n") + cat's re-emitted
    // line ("hello\n" -> ONLCR -> "hello\r\n"). Poll with a generous timeout.
    let t0 = std::time::Instant::now();
    while ps.bytes_echoed.load(Ordering::Relaxed) < 12
        && t0.elapsed() < Duration::from_secs(5)
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    // Let the parser finish the final chunk before snapshotting.
    std::thread::sleep(Duration::from_millis(50));

    let bytes = ps.bytes_echoed.load(Ordering::Relaxed);
    let dirty = ps.dirty.load(Ordering::Acquire);
    let snap = snapshot(&ps.term.lock());
    let hello_rows = snap.iter().filter(|r| r.text.contains("hello")).count();

    eprintln!(
        "[gpui-term] interactive self-test: bytes_echoed={bytes} (want >=12: echo + cat), \
         dirty={dirty}, grid rows containing \"hello\": {hello_rows} (want >=2)"
    );
    let ok = bytes >= 12 && dirty && hello_rows >= 2;
    eprintln!(
        "RESULT: {}",
        if ok {
            "PASS (pty write -> kernel echo -> parse -> grid + dirty path live)"
        } else {
            "FAIL"
        }
    );
    std::process::exit(if ok { 0 } else { 1 });
}

// =========================================================================
// LIVE GPUI run.
// =========================================================================

mod gui {
    use super::*;
    use gpui::{
        canvas, div, fill, font, point, prelude::*, px, rgb, size, App, Application, Bounds,
        Context, FocusHandle, Font, Hsla, AppContext, IntoElement, KeyDownEvent, Keystroke,
        Pixels, Render, SharedString, Styled, TextRun, TitlebarOptions, Window,
        WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
    };
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::NSView;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    extern "C" {
        /// Count of `MetalRenderer::draw` calls — actual Metal present
        /// submissions — maintained by the vendored-gpui C shim alongside the
        /// "Draw" signpost (incremented whether or not a recorder is attached).
        fn nice_signpost_draw_count() -> u64;
    }

    fn metal_draw_count() -> u64 {
        unsafe { nice_signpost_draw_count() }
    }

    struct TermView {
        /// Window index (0 = the measurement coordinator).
        index: usize,
        /// Streaming windows RAF-render continuously; background windows are
        /// demand-driven (dirty-flag notify poller).
        streaming: bool,
        cfg: MultiCfg,
        /// The session's Term + feeder thread (parsing happens there, NOT in
        /// render — spike 8 restructure).
        session: Session,
        base_font: Font,
        frame: u64,
        start_tick: u64,
        deadline_secs: f64,
        mem_idle_mib: f64,
        mem_steady_mib: f64,
        mem_peak_mib: f64,
        seed: u64,
        bps: usize,
    }

    impl TermView {
        fn new(
            index: usize,
            streaming: bool,
            cfg: MultiCfg,
            session: Session,
            deadline_secs: f64,
            seed: u64,
            bps: usize,
        ) -> Self {
            Self {
                index,
                streaming,
                cfg,
                session,
                base_font: font("Menlo"),
                frame: 0,
                start_tick: 0,
                deadline_secs,
                mem_idle_mib: 0.0,
                mem_steady_mib: 0.0,
                mem_peak_mib: 0.0,
                seed,
                bps,
            }
        }

        fn elapsed_secs(&self) -> f64 {
            if self.start_tick == 0 {
                0.0
            } else {
                harness::clock::ms_between(self.start_tick, harness::clock::now()) / 1000.0
            }
        }

        fn sample_mem(&mut self) {
            let (phys, _rss) = harness::mem::sample();
            let mib = harness::mem::mib(phys);
            self.mem_steady_mib = mib;
            if mib > self.mem_peak_mib {
                self.mem_peak_mib = mib;
            }
        }

        fn finalize_and_exit(&self, reason: &str) -> ! {
            let streams = harness::drain_frame_streams();
            // Single stack: GPUI's composite IS the terminal present. One vsync
            // on this panel = 16.67 ms (60 Hz); cliff threshold matches §10.
            let g = harness::interval_stats(&streams.gpui_composite, 16.6);

            let multi = self.cfg.windows > 1;
            let csv = if multi {
                format!(
                    "./gpui-term-multi-{}w{}s.csv",
                    self.cfg.windows, self.cfg.streaming
                )
            } else {
                "./gpui-term-gpui-native-single-stack.csv".to_string()
            };
            let _ = if multi {
                write_multi_csv(Path::new(&csv))
            } else {
                write_csv(Path::new(&csv), &streams)
            };

            eprintln!("\n================ gpui-term LIVE RESULT ({reason}) ================");
            eprintln!("architecture : Path B — single GPUI Metal stack, alacritty_terminal VT core,");
            eprintln!("               rendered via public shape_line().paint() + paint_quad()");
            eprintln!("workload     : synthetic Claude-stream, seed={} ~{} B/s, {}x{} grid", self.seed, self.bps, COLS, ROWS);
            if multi {
                let bg = self.cfg.windows - self.cfg.streaming;
                let bg_desc = if self.cfg.bg_bps == 0 {
                    "idle heartbeat 1 line/s".to_string()
                } else {
                    format!("~{} B/s", self.cfg.bg_bps)
                };
                eprintln!(
                    "sessions     : {} windows = {} streaming + {} background ({bg_desc}); \
                     one feeder thread per session (parse OFF the main thread)",
                    self.cfg.windows, self.cfg.streaming, bg
                );
            }
            eprintln!("duration     : {:.1} s, {} composited frames", self.elapsed_secs(), g.samples);
            eprintln!("-- frame interval (single stack = terminal present) --");
            eprintln!(
                "  p50 {:.2} ms ({:.1} fps) | p95 {:.2} ms | p99 {:.2} ms | cliffs>16.6ms {}",
                g.p50_ms, g.fps_p50, g.p95_ms, g.p99_ms, g.cliffs
            );
            if multi {
                eprintln!("-- per-window cadence (spike 8) --");
                for (i, slot) in win_slots().iter().enumerate() {
                    let ts = slot.frames.lock().unwrap().clone();
                    let st = harness::interval_stats(&ts, 16.6);
                    let fed = slot.bytes_fed.load(Ordering::Relaxed);
                    if slot.streaming {
                        eprintln!(
                            "  w{i} streaming : frames {:>5} | p50 {:.2} ms ({:.1} fps) | \
                             p95 {:.2} | p99 {:.2} | cliffs {} | fed {} B",
                            st.samples, st.p50_ms, st.fps_p50, st.p95_ms, st.p99_ms, st.cliffs, fed
                        );
                    } else {
                        eprintln!(
                            "  w{i} background: frames {:>5} (demand-driven — intervals track \
                             heartbeat/notify cadence, not fps) | fed {} B",
                            st.samples, fed
                        );
                    }
                }
            }
            eprintln!("-- memory (phys_footprint; whole process, incl. every session) --");
            eprintln!(
                "  idle {:.1} MiB | steady {:.1} MiB | peak {:.1} MiB",
                self.mem_idle_mib, self.mem_steady_mib, self.mem_peak_mib
            );
            eprintln!("-- reference (§10, same harness) --");
            eprintln!("  baseline single-stack  : ~16.7 / 17.1 ms (~60 fps)");
            eprintln!("  Path A dual-stack txn   : 18.3 / 31.2 ms (~54 fps), peak ~155 MiB");
            eprintln!("  Path A dual-stack sync  : 33.3 / 42.6 ms (~30 fps)");
            eprintln!("  raw CSV: {csv}");
            eprintln!("=================================================================");
            std::process::exit(0);
        }
    }

    impl Render for TermView {
        fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            if self.index == 0 {
                harness::stamp_gpui_frame();
            }
            stamp_window_frame(self.index);
            self.frame += 1;

            if self.index == 0 && self.frame == 1 {
                // Capture an idle baseline, then clear the warm-up frame so the
                // measured window starts clean.
                let (phys, _) = harness::mem::sample();
                self.mem_idle_mib = harness::mem::mib(phys);
                self.mem_peak_mib = self.mem_idle_mib;
                self.start_tick = harness::clock::now();
                harness::reset_frame_streams();
                clear_window_frames();
            }

            if self.index == 0 {
                self.sample_mem();
                if self.elapsed_secs() >= self.deadline_secs {
                    self.finalize_and_exit("measurement window elapsed");
                }
            }

            // Snapshot the grid under a SHORT FairMutex lock (parsing happens
            // on the session's feeder thread now — spike 8 restructure). The
            // snapshot is owned so the 'static paint closure can render it
            // without borrowing `self`.
            let snap = snapshot(&self.session.term.lock());
            let base = self.base_font.clone();
            // Animated sub-pixel vertical offset → exercises fractional glyph
            // placement + full re-paint every frame (the sub-line scroll path).
            let frac = (self.frame as f32 * 0.7) % LINE_PX;

            // Streaming windows composite continuously — this RAF is the
            // measurement clock (unchanged from the audited rev). Background
            // windows are demand-driven: the notify poller in run_live turns
            // the feeder's dirty flag into cx.notify() instead.
            if self.streaming {
                window.request_animation_frame();
            }

            grid_canvas(snap, base, frac)
        }
    }

    fn rgb_to_hsla(v: u32) -> Hsla {
        rgb(v).into()
    }

    /// Build the paint canvas for one grid snapshot. Shared by the workload
    /// view (`TermView`) and the interactive view (`InteractiveView`).
    fn grid_canvas(snap: Vec<RowSnap>, base: Font, frac: f32) -> impl IntoElement {
        canvas(
            move |_bounds, _window, _cx| {},
            move |bounds: Bounds<Pixels>, _state, window: &mut Window, cx: &mut App| {
                let line_h = px(LINE_PX);
                let font_size = px(FONT_PX);
                let origin_x = bounds.origin.x;
                let top = bounds.origin.y;

                // Window background.
                window.paint_quad(fill(bounds, rgb(DEFAULT_BG)));

                for (i, row) in snap.iter().enumerate() {
                    let y = top + px(i as f32 * LINE_PX + frac);

                    // Build per-cell text runs (shape_line coalesces same-style).
                    let runs: Vec<TextRun> = row
                        .cells
                        .iter()
                        .map(|(len, fg)| TextRun {
                            len: *len,
                            font: base.clone(),
                            color: rgb_to_hsla(*fg),
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        })
                        .collect();

                    let text: SharedString = row.text.clone().into();
                    let shaped =
                        window
                            .text_system()
                            .shape_line(text, font_size, &runs, None);

                    // Cell advance from the shaped row width (monospace ⇒ even).
                    let cell_w = if COLS > 0 {
                        shaped.width / (COLS as f32)
                    } else {
                        px(FONT_PX * 0.6)
                    };

                    // Cell backgrounds first (under the glyphs).
                    for bg in &row.bgs {
                        let x = origin_x + cell_w * (bg.col as f32);
                        let w = cell_w * (bg.len as f32);
                        let rect = Bounds {
                            origin: point(x, y),
                            size: size(w, line_h),
                        };
                        window.paint_quad(fill(rect, rgb(bg.rgb)));
                    }

                    let _ = shaped.paint(point(origin_x, y), line_h, window, cx);
                }
            },
        )
        .size_full()
    }

    // =====================================================================
    // INTERACTIVE keystroke-latency mode (spikes 4b/5, Path-B half).
    // =====================================================================

    /// Map a GPUI keystroke to raw pty bytes. Plain printable characters +
    /// Return (and a couple of obvious controls) only — deliberately NO
    /// kitty/CSI-u encoder and no escape sequences (spike scope; the latency
    /// harness posts keycode 0 + unicode 'a', which arrives here as
    /// key="a" / key_char=Some("a")).
    fn keystroke_bytes(ks: &Keystroke) -> Option<Vec<u8>> {
        if ks.modifiers.control || ks.modifiers.platform || ks.modifiers.alt || ks.modifiers.function
        {
            return None;
        }
        if let Some(ch) = &ks.key_char {
            if !ch.is_empty() {
                return Some(ch.as_bytes().to_vec());
            }
        }
        match ks.key.as_str() {
            "enter" => Some(b"\r".to_vec()),
            "tab" => Some(b"\t".to_vec()),
            "space" => Some(b" ".to_vec()),
            "backspace" => Some(vec![0x7f]),
            _ => None,
        }
    }

    /// Interactive keystroke-latency view: NO synthetic workload, NO RAF —
    /// this view renders ONLY when the pty reader pings the wake channel
    /// (echo arrived) or the OS itself asks for a frame (open/resize/
    /// occlusion/activation). So the vendored-gpui "Draw" signpost is
    /// inherently damage-gated: one echo batch => one cx.notify() => one
    /// render => one Draw interval.
    ///
    /// Cursor/blink: this bin renders NO cursor at all (the snapshot paints
    /// only cell text + backgrounds) and owns NO timers — there are zero
    /// timer-driven redraws in this mode by construction.
    struct InteractiveView {
        pty: PtySession,
        focus_handle: FocusHandle,
        base_font: Font,
        frame: u64,
        keys_sent: u64,
        start_tick: u64,
        /// GPUI's NSView (from raw-window-handle), captured on first render.
        /// Used to kick AppKit's damage path per echo — see
        /// `kick_platform_display`. Main-thread only (gpui entities are).
        ns_view: *mut NSView,
    }

    impl InteractiveView {
        fn on_key(&mut self, ev: &KeyDownEvent) {
            if let Some(bytes) = keystroke_bytes(&ev.keystroke) {
                use std::io::Write;
                if self.pty.writer.write_all(&bytes).is_ok() {
                    let _ = self.pty.writer.flush();
                    self.keys_sent += 1;
                }
            }
        }

        /// Make the dirty scene actually PRESENT. In gpui 0.2.2, cx.notify()
        /// only rebuilds the scene (app-side `Window::draw`, which sets
        /// `needs_present`) — the Metal present runs solely from the platform
        /// request-frame path (CVDisplayLink `step` / `displayLayer:`), and
        /// the display link is STOPPED for occluded windows, so notify alone
        /// can render 505 times with zero Metal draws (the failed live run).
        /// Marking the view + its backing CAMetalLayer as needing display
        /// forces the next CA commit to fire `displayLayer:` -> gpui's
        /// request-frame callback -> `Window::present()` ->
        /// `MetalRenderer::draw` (the "Draw" signpost), independent of the
        /// display-link state. One echo => one commit => one Metal draw.
        fn kick_platform_display(&self) {
            if self.ns_view.is_null() {
                return;
            }
            unsafe {
                let view: &NSView = &*self.ns_view;
                view.setNeedsDisplay(true);
                // The layer mark is the guaranteed CALayerDelegate
                // `displayLayer:` trigger (the view is the layer's delegate;
                // gpui's `makeBackingLayer` returns the renderer's
                // CAMetalLayer).
                let layer: *mut AnyObject = msg_send![view, layer];
                if !layer.is_null() {
                    let _: () = msg_send![layer, setNeedsDisplay];
                }
            }
        }

        fn finalize_and_exit(&self, reason: &str) -> ! {
            let secs = if self.start_tick == 0 {
                0.0
            } else {
                harness::clock::ms_between(self.start_tick, harness::clock::now()) / 1000.0
            };
            eprintln!(
                "[gpui-term interactive] {reason} after {secs:.1}s: metal draws {} (real \
                 present submissions, incl. window-open) | scene rebuilds {} | pty bytes \
                 echoed {} | keys sent {} (demand-driven: no RAF, no workload, no \
                 cursor/blink timer; keepalive present disabled via NICE_POC_DAMAGE_ONLY)",
                metal_draw_count(),
                self.frame,
                self.pty.bytes_echoed.load(Ordering::Relaxed),
                self.keys_sent
            );
            std::process::exit(0);
        }
    }

    impl Render for InteractiveView {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.frame += 1;
            if self.frame == 1 {
                self.start_tick = harness::clock::now();
                window.focus(&self.focus_handle);
                // Capture gpui's NSView for the per-echo present kick. First
                // render always precedes the first echo (the window must be
                // up and key before typing can reach the pty).
                if let Ok(handle) = HasWindowHandle::window_handle(window) {
                    if let RawWindowHandle::AppKit(appkit) = handle.as_raw() {
                        self.ns_view = appkit.ns_view.as_ptr() as *mut NSView;
                    }
                }
            }
            self.pty.dirty.store(false, Ordering::Release);

            let snap = snapshot(&self.pty.term.lock());
            // frac = 0: no sub-pixel scroll animation (that path needs a
            // continuous redraw clock, which this mode deliberately lacks).
            // NOTE: no request_animation_frame anywhere in this render.
            div()
                .size_full()
                .track_focus(&self.focus_handle)
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, _cx| {
                    this.on_key(ev);
                }))
                .child(grid_canvas(snap, self.base_font.clone(), 0.0))
        }
    }

    pub fn run_interactive() {
        // Damage-only presents: without this, gpui's stock request-frame loop
        // keeps presenting the UNCHANGED scene at refresh rate for 1 s after
        // every input while the display link runs (window.rs "prevent the
        // display from underclocking" keepalive) — 60 Hz Draw signposts that
        // sample frame phase, not latency. Set BEFORE any thread exists.
        // (Respects an explicit user-provided value.)
        if std::env::var_os("NICE_POC_DAMAGE_ONLY").is_none() {
            std::env::set_var("NICE_POC_DAMAGE_ONLY", "1");
        }

        let deadline = std::env::var("NICE_POC_SECS")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(120.0);

        eprintln!(
            "[gpui-term] INTERACTIVE keystroke-latency mode (spikes 4b/5): one window, \
             /bin/cat behind a REAL pty, no workload, no RAF, keepalive present disabled \
             (NICE_POC_DAMAGE_ONLY={}) — type (or let the harness CGEventPostToPid) and \
             every kernel echo triggers exactly one demand-driven Metal draw (notify + \
             setNeedsDisplay kick -> displayLayer -> present). Auto-exit with a one-line \
             summary after ~{deadline:.0}s (NICE_POC_SECS overrides).",
            std::env::var("NICE_POC_DAMAGE_ONLY").unwrap_or_default()
        );

        Application::new().run(move |cx: &mut App| {
            cx.activate(true);
            cx.on_window_closed(|_cx| std::process::exit(0)).detach();

            let (wake_tx, mut wake_rx) = futures::channel::mpsc::unbounded::<()>();
            let pty = PtySession::spawn(Some(wake_tx))
                .expect("failed to spawn /bin/cat behind a pty");

            let bounds = Bounds::centered(
                None,
                size(px(COLS as f32 * FONT_PX * 0.62), px(ROWS as f32 * LINE_PX + 40.0)),
                cx,
            );
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_background: WindowBackgroundAppearance::Opaque,
                    titlebar: Some(TitlebarOptions {
                        title: Some(
                            "Nice Phase-0 — gpui-term INTERACTIVE (pty: /bin/cat)".into(),
                        ),
                        appears_transparent: false,
                        traffic_light_position: None,
                    }),
                    kind: WindowKind::Normal,
                    is_resizable: true,
                    ..Default::default()
                },
                |window, cx| {
                    let view = cx.new(|cx| {
                        // Echo wakeups: pty reader thread -> unbounded channel
                        // -> this foreground task -> cx.notify() (rebuild the
                        // scene) + kick_platform_display() (force the CA
                        // commit that actually PRESENTS it — notify alone
                        // never reaches MetalRenderer::draw when the display
                        // link is paused). A channel, not a poller: the wakeup
                        // must not quantize the measured keystroke latency.
                        cx.spawn(async move |this, cx| {
                            use futures::StreamExt;
                            while wake_rx.next().await.is_some() {
                                let alive =
                                    this.update(cx, |view: &mut InteractiveView, cx| {
                                        cx.notify();
                                        view.kick_platform_display();
                                    });
                                if alive.is_err() {
                                    break;
                                }
                            }
                        })
                        .detach();

                        // The deadline is a TIMER, not a render-path check —
                        // a demand-driven window may not render for long
                        // stretches, so the exit cannot live in render().
                        let executor = cx.background_executor().clone();
                        cx.spawn(async move |this, cx| {
                            executor.timer(Duration::from_secs_f64(deadline)).await;
                            let _ = this.update(cx, |view: &mut InteractiveView, _| {
                                view.finalize_and_exit("deadline")
                            });
                        })
                        .detach();

                        InteractiveView {
                            pty,
                            focus_handle: cx.focus_handle(),
                            base_font: font("Menlo"),
                            frame: 0,
                            keys_sent: 0,
                            start_tick: 0,
                            ns_view: std::ptr::null_mut(),
                        }
                    });
                    // Make the grid div the focus target immediately so keys
                    // routed by the (externally activated) key window land in
                    // on_key_down without needing a click.
                    window.focus(&view.read(cx).focus_handle);
                    view
                },
            )
            .unwrap();
        });
    }

    /// Raw per-sample CSV (single-stack frame intervals + memory), §H.1-shaped.
    fn write_csv(path: &Path, streams: &harness::FrameStreams) -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::File::create(path)?;
        writeln!(f, "metric,stack,phase,build,idx,value,unit")?;
        let ts = &streams.gpui_composite;
        for (idx, w) in ts.windows(2).enumerate() {
            let ms = harness::clock::ms_between(w[0], w[1]);
            writeln!(f, "frame_interval,gpui-native,load,poc,{idx},{ms},ms")?;
        }
        Ok(())
    }

    /// Multi-window raw CSV (spike 8): per-window frame intervals, same schema
    /// with the window index + role folded into the `stack` column
    /// (`gpui-native-w<N>-stream` / `gpui-native-w<N>-bg`).
    fn write_multi_csv(path: &Path) -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::File::create(path)?;
        writeln!(f, "metric,stack,phase,build,idx,value,unit")?;
        for (w, slot) in win_slots().iter().enumerate() {
            let ts = slot.frames.lock().unwrap().clone();
            let kind = if slot.streaming { "stream" } else { "bg" };
            for (idx, win) in ts.windows(2).enumerate() {
                let ms = harness::clock::ms_between(win[0], win[1]);
                writeln!(f, "frame_interval,gpui-native-w{w}-{kind},load,poc,{idx},{ms},ms")?;
            }
        }
        Ok(())
    }

    pub fn run_live() {
        let deadline = std::env::var("NICE_POC_SECS")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(18.0);
        let cfg = MultiCfg::from_env();
        let prof = WorkloadProfile::default();

        eprintln!(
            "[gpui-term] LIVE single-stack GPUI-native terminal: {} window(s) \
             ({} streaming, {} background @ {} B/s), ~{deadline:.0}s then auto-exit \
             with an FPS/memory summary. (NICE_POC_SECS / NICE_POC_WINDOWS / \
             NICE_POC_STREAMING / NICE_POC_BG_BPS override.)",
            cfg.windows,
            cfg.streaming,
            cfg.windows - cfg.streaming,
            cfg.bg_bps
        );

        Application::new().run(move |cx: &mut App| {
            cx.activate(true);
            cx.on_window_closed(|_cx| std::process::exit(0)).detach();

            // Spawn every session + the per-window registry BEFORE any window
            // opens (window 0's first render can fire before later windows
            // exist, and it clears all slots when the measurement starts).
            let mut slots = Vec::with_capacity(cfg.windows);
            let mut sessions = Vec::with_capacity(cfg.windows);
            for i in 0..cfg.windows {
                let streaming = i < cfg.streaming;
                let bps = if streaming { prof.bytes_per_sec } else { cfg.bg_bps };
                // Distinct deterministic stream per window (w0 keeps the
                // original seed, so K=1 is byte-identical to the audited rev).
                let session = Session::spawn(i, prof.seed + i as u64, bps);
                slots.push(WinSlot {
                    streaming,
                    bps,
                    bytes_fed: Arc::clone(&session.bytes_fed),
                    frames: Mutex::new(Vec::new()),
                });
                sessions.push(session);
            }
            let _ = WIN_SLOTS.set(slots);

            let win_size = size(
                px(COLS as f32 * FONT_PX * 0.62),
                px(ROWS as f32 * LINE_PX + 40.0),
            );

            for (i, session) in sessions.into_iter().enumerate() {
                let streaming = i < cfg.streaming;
                let bps = if streaming { prof.bytes_per_sec } else { cfg.bg_bps };
                let bounds = if cfg.windows == 1 {
                    Bounds::centered(None, win_size, cx)
                } else {
                    // Cascade the windows so each stays (mostly) on screen.
                    Bounds {
                        origin: point(px(40.0 + 44.0 * i as f32), px(40.0 + 36.0 * i as f32)),
                        size: win_size,
                    }
                };
                let title = if cfg.windows == 1 {
                    "Nice Phase-0 — GPUI-native terminal (Path B)".to_string()
                } else {
                    format!(
                        "Nice Phase-0 — gpui-term w{i} [{}]",
                        if streaming { "streaming" } else { "background" }
                    )
                };
                cx.open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        window_background: WindowBackgroundAppearance::Opaque,
                        titlebar: Some(TitlebarOptions {
                            title: Some(title.into()),
                            appears_transparent: false,
                            traffic_light_position: None,
                        }),
                        kind: WindowKind::Normal,
                        is_resizable: true,
                        ..Default::default()
                    },
                    move |_window, cx| {
                        cx.new(move |cx| {
                            if !streaming {
                                // Demand-driven redraw for background windows:
                                // a foreground poller (~10 Hz) turns the
                                // feeder's dirty flag into cx.notify().
                                // Streaming windows redraw via RAF instead.
                                let dirty = Arc::clone(&session.dirty);
                                let executor = cx.background_executor().clone();
                                cx.spawn(async move |this, cx| {
                                    loop {
                                        executor.timer(Duration::from_millis(100)).await;
                                        if dirty.swap(false, Ordering::AcqRel)
                                            && this.update(cx, |_, cx| cx.notify()).is_err()
                                        {
                                            break;
                                        }
                                    }
                                })
                                .detach();
                            }
                            TermView::new(
                                i,
                                streaming,
                                cfg,
                                session,
                                deadline,
                                prof.seed + i as u64,
                                bps,
                            )
                        })
                    },
                )
                .unwrap();
            }
        });
    }
}

fn main() {
    let live = matches!(std::env::var("NICE_POC_RUN").as_deref(), Ok("1") | Ok("true"));
    let interactive = matches!(
        std::env::var("NICE_POC_INTERACTIVE").as_deref(),
        Ok("1") | Ok("true")
    );
    match (live, interactive) {
        // Interactive keystroke-latency window (spikes 4b/5); ignores the
        // multi-session flags (always exactly ONE window, no workload).
        (true, true) => gui::run_interactive(),
        (true, false) => gui::run_live(),
        // Headless pty/echo self-test for the interactive path.
        (false, true) => run_headless_interactive(),
        (false, false) => run_headless(),
    }
}
