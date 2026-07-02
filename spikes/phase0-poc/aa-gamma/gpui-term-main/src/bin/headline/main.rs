//! Phase-0 §13 spike 11 (part 1) — the HEADLINE measurements re-run on the
//! PRODUCTION-CANDIDATE stack: the pinned zed-main checkout
//! (rev 10b07951838e422722e34641f4a9c0bfec9037ff + bg-luminance patch +
//! zed-main-headline-hook.patch = ../../../zed-main-patched), NOT crates.io
//! gpui 0.2.2. The published Path-B headline numbers all came from the
//! (dormant) 0.2.2 crate (spikes/phase0-poc, bin `gpui-term`); this bin
//! reproduces the three headline measurements on the pin so the main session
//! can compare 1:1:
//!
//!   1. Streaming FPS: same deterministic synthetic Claude-stream workload
//!      (seed 42, ~500 KB/s, 120x40 grid, feeder thread OFF the main thread),
//!      RAF-driven redraw, ~20 s window, then auto-exit printing p50/p95/p99
//!      frame intervals + max + self-calibrated cliffs (1.5 x p50) + the
//!      legacy 16.6 ms cliff count + phys_footprint idle/steady/peak.
//!      [0.2.2 reference: single-window release p50 16.7 / p95 ~17.1-17.4 ms]
//!   2. Per-draw CPU cost: `MetalRenderer::draw` on the pin (scene submission:
//!      `Window::present()` -> `PlatformWindow::draw` -> `MetalRenderer::draw`
//!      in gpui_macos) is timed in-process by the additive hook
//!      (zed-main-headline-hook.patch: gpui_macos::metal_renderer::
//!      nice_draw_metrics) and bracketed with an os_signpost interval
//!      (subsystem dev.nickanderssohn.gpui-term-main, category "present",
//!      name "Draw") emitted by src/bin/headline/nice_signpost.c.
//!      [0.2.2 reference: draw CPU p50 0.076-0.145 ms; Nice Metal.Draw
//!      1.19/2.41 ms p50/p95]
//!   3. Interactive keystroke latency (spike-5 semantics): `/bin/cat` behind a
//!      REAL pty, no workload, no RAF — typed keys are written raw to the pty,
//!      the kernel canonical-mode echo triggers a demand-driven present
//!      (cx.notify() + setNeedsDisplay kick; see `kick_platform_display` for
//!      why the kick is still load-bearing on main), so the "Draw" signpost is
//!      damage-gated and usable as the latency anchor for the injection
//!      harness. Summary prints the REAL draw count vs scene rebuilds.
//!
//! It deliberately REUSES the 0.2.2 crate's harness.rs VERBATIM (`#[path]`
//! include of spikes/phase0-poc/src/harness.rs) so the clock, workload
//! generator, percentile reducer, memory sampler, and deadline watchdog are
//! byte-identical — apples-to-apples.
//!
//! Present-path note (main vs 0.2.2): the mac platform machinery is the same
//! shape on both — a per-window CVDisplayLink `step` and a CA `displayLayer:`
//! path both invoke the request-frame callback; the display link stops ONLY
//! on occlusion; `cx.notify()` alone never presents when the link is stopped.
//! Differences on main:
//!   * request_frame draws only when the invalidator is dirty and presents
//!     only when needed, so a visible idle window costs ~no Metal draws even
//!     with the link running (no NICE_POC_DAMAGE_ONLY-style patch needed);
//!   * the 0.2.2 "present for 1 s after every input" keepalive became an
//!     InputRateTracker: presents are sustained for 1 s only while input
//!     arrives at >= 60 events/s — a human/harness typing below that rate
//!     never triggers it, so draws stay damage-gated without patching;
//!   * inactive (non-key) windows are frame-capped to ~30 fps on main
//!     (window.rs min_frame_interval) — keep the streaming window frontmost
//!     AND focused or the FPS numbers will read as ~33 ms by design.
//!
//! Run modes (mirrors the 0.2.2 bin's env gating; NICE_MAIN_* not NICE_POC_*):
//!   * UNSET              — HEADLESS workload self-test (no window, subagent-safe).
//!   * NICE_MAIN_INTERACTIVE=1                 — HEADLESS pty/echo self-test.
//!   * NICE_MAIN_RUN=1                         — LIVE streaming FPS run
//!     (~NICE_MAIN_SECS s, default 20; REQUIRES a display).
//!   * NICE_MAIN_RUN=1 NICE_MAIN_INTERACTIVE=1 — LIVE keystroke-latency window
//!     (~NICE_MAIN_SECS s, default 120; REQUIRES a display + typist/harness).

#![allow(dead_code)]

// The 0.2.2 crate's harness, included verbatim (clock / workload / percentiles
// / mem / cpu / watchdog). Path is relative to THIS file:
// src/bin/headline/ -> gpui-term-main -> aa-gamma -> phase0-poc/src/harness.rs
#[path = "../../../../../src/harness.rs"]
mod harness;

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point as TermPoint};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};

use harness::{Workload, WorkloadProfile};

// ---- os_signpost shim (nice_signpost.c, linked by build.rs) ---------------

extern "C" {
    fn nice_signpost_draw_begin() -> u64;
    fn nice_signpost_draw_end(spid: u64);
}

extern "C" fn draw_begin_hook() -> u64 {
    unsafe { nice_signpost_draw_begin() }
}

extern "C" fn draw_end_hook(spid: u64) {
    unsafe { nice_signpost_draw_end(spid) }
}

/// Register the signpost emitters with the patched renderer hook. Must run
/// before any window opens (and is harmless headless).
fn install_draw_hooks() {
    gpui_macos::metal_renderer::nice_draw_metrics::set_hooks(draw_begin_hook, draw_end_hook);
}

// ---- terminal geometry / style (identical constants to the 0.2.2 bin) -----

const ROWS: usize = 40;
const COLS: usize = 120;
const FONT_PX: f32 = 14.0;
const LINE_PX: f32 = 18.0;
const DEFAULT_FG: u32 = 0x00C8_C8C8;
const DEFAULT_BG: u32 = 0x000B_0B0B;

#[derive(Clone, Copy)]
struct Size2 {
    rows: usize,
    cols: usize,
}
impl Dimensions for Size2 {
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

// ---- color mapping (alacritty Color -> 0xRRGGBB), ported verbatim ---------

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

// ---- owned per-frame snapshot (ported; no styles/selection — the headline
// runs never enable them, so this matches the audited 0.2.2 default path) ---

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
    let display_offset = term.grid().display_offset() as i32;
    let mut out = Vec::with_capacity(rows);
    for line in 0..rows {
        let buffer_line = Line(line as i32 - display_offset);
        let mut text = String::with_capacity(cols);
        let mut cells = Vec::with_capacity(cols);
        let mut bgs: Vec<BgRun> = Vec::new();
        for col in 0..cols {
            let point = TermPoint::new(buffer_line, Column(col));
            let cell = &term.grid()[point];
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
// Session — the synthetic-workload byte source, parsed OFF the render path
// (the 0.2.2 crate's spike-8 restructure, ported: a feeder thread generates
// the deterministic workload and parses it into a FairMutex<Term> at a
// wall-clock byte rate; render() only locks briefly to snapshot).
// =========================================================================

/// Feeder pacing quantum — 5 ms slices reproduce the aggregate byte rate with
/// a render-independent clock (identical to the 0.2.2 bin).
const FEED_TICK_MS: u64 = 5;

struct Session {
    term: Arc<FairMutex<Term<EventProxy>>>,
    bytes_fed: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    feeder: Option<std::thread::JoinHandle<()>>,
}

impl Session {
    fn spawn(seed: u64, bytes_per_sec: usize) -> Self {
        let size = Size2 {
            rows: ROWS,
            cols: COLS,
        };
        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &size,
            EventProxy,
        )));
        let bytes_fed = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let feeder = {
            let term = Arc::clone(&term);
            let bytes_fed = Arc::clone(&bytes_fed);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("headline-feeder".into())
                .spawn(move || {
                    let mut parser: Processor = Processor::new();
                    let mut wl = Workload::new(WorkloadProfile {
                        seed,
                        ..WorkloadProfile::default()
                    });
                    let per_tick = ((bytes_per_sec * FEED_TICK_MS as usize) / 1000).max(64);
                    while !stop.load(Ordering::Relaxed) {
                        let t0 = Instant::now();
                        // Generate OUTSIDE the lock; hold it only to parse.
                        let chunk = wl.stream(per_tick);
                        {
                            let mut t = term.lock();
                            parser.advance(&mut *t, &chunk);
                        }
                        bytes_fed.fetch_add(chunk.len() as u64, Ordering::Relaxed);
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
// child, for the interactive keystroke-latency mode. Ported verbatim from the
// 0.2.2 bin (same alacritty_terminal 0.26): kernel canonical-mode echo, a
// dedicated blocking reader thread, wake channel to the GPUI side.
// =========================================================================

struct PtySession {
    term: Arc<FairMutex<Term<EventProxy>>>,
    dirty: Arc<AtomicBool>,
    bytes_echoed: Arc<AtomicU64>,
    writer: std::fs::File,
    _reader: std::thread::JoinHandle<()>,
}

impl PtySession {
    fn spawn(wake: Option<futures::channel::mpsc::UnboundedSender<()>>) -> std::io::Result<Self> {
        use std::io::Read;
        use std::os::fd::AsRawFd;

        let size = Size2 {
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
        let opts = tty::Options {
            shell: Some(tty::Shell::new("/bin/cat".into(), Vec::new())),
            working_directory: None,
            drain_on_exit: false,
            env: std::collections::HashMap::new(),
        };
        let mut pty = tty::new(&opts, ws, 0)?;

        // alacritty's tty::new sets the master fd NON-blocking; we use a
        // blocking reader thread instead (flags live on the shared open-file
        // description, so clearing via the dup'd writer covers reader() too).
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
                .name("headline-pty-reader".into())
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
// HEADLESS self-tests (no GPUI, no window) — subagent/CI-safe.
// =========================================================================

fn run_headless() {
    eprintln!("{}", harness::banner());
    eprintln!(
        "[headline] HEADLESS workload self-test on the zed-main pin crate (no display). \
         Set NICE_MAIN_RUN=1 for the live FPS run."
    );

    let size = Size2 {
        rows: ROWS,
        cols: COLS,
    };
    let mut term = Term::new(Config::default(), &size, EventProxy);
    let mut parser: Processor = Processor::new();
    let prof = WorkloadProfile::default();
    let mut wl = Workload::new(prof);

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
        "[headline] snapshot: {} rows x {} cols, {} non-empty rows, {} visible glyphs",
        snap.len(),
        COLS,
        nonempty_rows,
        total_glyphs
    );
    // Also prove the feeder-thread path (generate + parse off-main + snapshot
    // under the FairMutex) — 1 s of real feeding at the default rate.
    let session = Session::spawn(prof.seed, prof.bytes_per_sec);
    std::thread::sleep(Duration::from_millis(1000));
    let fed = session.bytes_fed.load(Ordering::Relaxed);
    let live_rows = {
        let t = session.term.lock();
        snapshot(&t)
            .iter()
            .filter(|r| !r.text.trim_end().is_empty())
            .count()
    };
    drop(session);
    eprintln!(
        "[headline] feeder thread: {fed} bytes parsed in ~1 s (want ~{}), {} non-empty rows",
        prof.bytes_per_sec, live_rows
    );

    let ok = snap.len() == ROWS
        && nonempty_rows > 0
        && total_glyphs > 0
        && fed > (prof.bytes_per_sec / 4) as u64
        && live_rows > 0;
    eprintln!(
        "RESULT: {}",
        if ok {
            "PASS (render-data + feeder-thread paths live on the pin)"
        } else {
            "FAIL"
        }
    );
    std::process::exit(if ok { 0 } else { 1 });
}

fn run_headless_interactive() {
    eprintln!("{}", harness::banner());
    eprintln!(
        "[headline] HEADLESS interactive self-test (pty /bin/cat, no display). \
         Add NICE_MAIN_RUN=1 for the live keystroke-latency window."
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

    // Expect >=12 bytes back: kernel echo ("hello\r\n") + cat's line (ONLCR).
    let t0 = std::time::Instant::now();
    while ps.bytes_echoed.load(Ordering::Relaxed) < 12 && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(50));

    let bytes = ps.bytes_echoed.load(Ordering::Relaxed);
    let dirty = ps.dirty.load(Ordering::Acquire);
    let snap = snapshot(&ps.term.lock());
    let hello_rows = snap.iter().filter(|r| r.text.contains("hello")).count();

    eprintln!(
        "[headline] interactive self-test: bytes_echoed={bytes} (want >=12: echo + cat), \
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
// LIVE GPUI runs.
// =========================================================================

mod gui {
    use super::*;
    use gpui::{
        canvas, div, fill, font, point, prelude::*, px, rgb, size, App, AppContext, Bounds,
        Context, FocusHandle, Font, Hsla, IntoElement, KeyDownEvent, Keystroke, Pixels, Render,
        SharedString, Styled, TextAlign, TextRun, TitlebarOptions, Window,
        WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
    };
    use gpui_macos::metal_renderer::nice_draw_metrics;
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::NSView;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    fn metal_draw_count() -> u64 {
        nice_draw_metrics::DRAW_COUNT.load(Ordering::Relaxed)
    }

    // ---- per-run metric stores (ported from the 0.2.2 bin) ---------------

    /// Grid snapshot cost per render (lock + copy), ms.
    static SNAPSHOT_MS: Mutex<Vec<f64>> = Mutex::new(Vec::new());
    /// Whole `render()` body cost per render (element build incl. snapshot), ms.
    static BUILD_MS: Mutex<Vec<f64>> = Mutex::new(Vec::new());
    /// Canvas paint-closure cost per frame (shape_line + paint_quad + glyph
    /// paint), ms.
    static PAINT_MS: Mutex<Vec<f64>> = Mutex::new(Vec::new());

    fn push_ms(store: &Mutex<Vec<f64>>, ms: f64) {
        let mut v = store.lock().unwrap();
        if v.len() < (1 << 20) {
            v.push(ms);
        }
    }

    fn stats_line(name: &str, store: &Mutex<Vec<f64>>) -> String {
        let mut v = store.lock().unwrap().clone();
        let n = v.len();
        let (p50, p95, p99) = harness::percentiles(&mut v);
        let max = v.last().copied().unwrap_or(0.0);
        format!("{name}: n={n} p50 {p50:.3} ms | p95 {p95:.3} | p99 {p99:.3} | max {max:.3}")
    }

    /// Reduce the hook's per-draw CPU durations (ns) to a printable line.
    fn draw_cpu_line() -> String {
        let mut draw_ms: Vec<f64> = nice_draw_metrics::DRAW_DUR_NS
            .lock()
            .unwrap()
            .iter()
            .map(|&ns| ns as f64 / 1.0e6)
            .collect();
        let n = draw_ms.len();
        let (p50, p95, p99) = harness::percentiles(&mut draw_ms);
        let max = draw_ms.last().copied().unwrap_or(0.0);
        format!(
            "MetalRenderer::draw CPU (zed-main pin, in-process): n={n} p50 {p50:.3} ms | \
             p95 {p95:.3} | p99 {p99:.3} | max {max:.3} | total draws {} \
             [0.2.2: p50 0.076-0.145 ms | Nice Metal.Draw: 1.19/2.41 ms p50/p95]",
            metal_draw_count()
        )
    }

    /// Mark an NSView + its backing CAMetalLayer as needing display so the
    /// next CA commit fires `displayLayer:` -> gpui request-frame ->
    /// `Window::present()` -> `MetalRenderer::draw`, independent of the
    /// display-link state. Still load-bearing on main: the link stops when
    /// the window is occluded (window_did_change_occlusion_state), and even
    /// when running, a vsync `step` would quantize the echo->present latency
    /// to frame phase — the kick presents on the CA commit instead (the same
    /// semantics the 0.2.2 spike-5 measurement used).
    fn kick_view_display(ns_view: *mut NSView) {
        if ns_view.is_null() {
            return;
        }
        unsafe {
            let view: &NSView = &*ns_view;
            view.setNeedsDisplay(true);
            let layer: *mut AnyObject = msg_send![view, layer];
            if !layer.is_null() {
                let _: () = msg_send![layer, setNeedsDisplay];
            }
        }
    }

    /// Display + max refresh of the screen hosting `ns_view`'s window
    /// (hot-plug guard, ported from the 0.2.2 bin).
    fn screen_info_of_view(ns_view: *mut NSView) -> (i64, String) {
        if ns_view.is_null() {
            return (0, "<no view>".to_string());
        }
        unsafe {
            let view: &NSView = &*ns_view;
            let Some(window) = view.window() else {
                return (0, "<no window>".to_string());
            };
            match window.screen() {
                Some(screen) => {
                    let fps = screen.maximumFramesPerSecond() as i64;
                    let name = screen.localizedName().to_string();
                    let f = screen.frame();
                    (
                        fps,
                        format!(
                            "{name} [{}x{} @ {},{}]",
                            f.size.width as i64,
                            f.size.height as i64,
                            f.origin.x as i64,
                            f.origin.y as i64
                        ),
                    )
                }
                None => (0, "<no screen>".to_string()),
            }
        }
    }

    fn rgb_to_hsla(v: u32) -> Hsla {
        rgb(v).into()
    }

    /// Build the paint canvas for one grid snapshot (ported; single font, no
    /// styles/images/dot — the audited default path).
    fn grid_canvas(snap: Vec<RowSnap>, grid_font: Font, frac: f32, record: bool) -> impl IntoElement {
        canvas(
            move |_bounds, _window, _cx| {},
            move |bounds: Bounds<Pixels>, _state, window: &mut Window, cx: &mut App| {
                let t_paint0 = harness::clock::now();
                let line_h = px(LINE_PX);
                let font_size = px(FONT_PX);
                let origin_x = bounds.origin.x;
                let top = bounds.origin.y;

                // Window background.
                window.paint_quad(fill(bounds, rgb(DEFAULT_BG)));

                for (i, row) in snap.iter().enumerate() {
                    let y = top + px(i as f32 * LINE_PX + frac);

                    // Per-cell text runs (shape_line coalesces same-style).
                    let runs: Vec<TextRun> = row
                        .cells
                        .iter()
                        .map(|(len, fg)| TextRun {
                            len: *len,
                            font: grid_font.clone(),
                            color: rgb_to_hsla(*fg),
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        })
                        .collect();

                    let text: SharedString = row.text.clone().into();
                    let shaped = window.text_system().shape_line(text, font_size, &runs, None);

                    // Cell advance from the shaped row width (monospace).
                    let row_cols = row.cells.len();
                    let cell_w = if row_cols > 0 {
                        shaped.width / (row_cols as f32)
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

                    // main API delta vs 0.2.2: paint takes TextAlign + an
                    // optional align width and returns a Result.
                    let _ = shaped.paint(point(origin_x, y), line_h, TextAlign::Left, None, window, cx);
                }

                if record {
                    push_ms(
                        &PAINT_MS,
                        harness::clock::ms_between(t_paint0, harness::clock::now()),
                    );
                }
            },
        )
        .size_full()
    }

    // =====================================================================
    // Streaming FPS view (headline measurement 1 + 2).
    // =====================================================================

    struct TermView {
        session: Session,
        grid_font: Font,
        frame: u64,
        start_tick: u64,
        deadline_secs: f64,
        mem_idle_mib: f64,
        mem_steady_mib: f64,
        mem_peak_mib: f64,
        seed: u64,
        bps: usize,
        ns_view: *mut NSView,
        cpu0: Option<harness::cpu::CpuSample>,
        draws0: u64,
        screen0: Option<(i64, String)>,
        /// (elapsed_s, phys MiB) series persisted into the CSV.
        mem_series: Vec<(f64, f64)>,
    }

    impl TermView {
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

        fn finalize_and_exit(&mut self, reason: &str) -> ! {
            self.sample_mem();
            if self.mem_idle_mib == 0.0 {
                self.mem_idle_mib = self.mem_steady_mib;
            }

            let streams = harness::drain_frame_streams();
            // Single stack: GPUI's composite IS the terminal present.
            let g = harness::interval_stats(&streams.gpui_composite, 16.6);
            let elapsed = self.elapsed_secs();

            // Hot-plug guard.
            let screen_now = screen_info_of_view(self.ns_view);
            let mut display_desc = match &self.screen0 {
                Some((fps, name)) => format!("{name} (max {fps} Hz)"),
                None => "<unknown>".to_string(),
            };
            if let Some((fps0, name0)) = &self.screen0 {
                if screen_now.0 != *fps0 || screen_now.1 != *name0 {
                    eprintln!(
                        "[headline] ⚠️ DISPLAY CHANGED MID-RUN: start='{name0}' ({fps0} Hz) -> \
                         exit='{}' ({} Hz). Numbers are CONTAMINATED — re-run on a single \
                         stable display.",
                        screen_now.1, screen_now.0
                    );
                    display_desc =
                        format!("{name0} -> {} (CHANGED MID-RUN — CONTAMINATED)", screen_now.1);
                }
            }

            let csv = "./gpui-term-main-headline.csv";
            let _ = write_csv(
                Path::new(csv),
                &streams,
                &display_desc,
                self.seed,
                self.bps,
                &self.mem_series,
            );

            eprintln!("\n============== headline (zed-main pin) LIVE RESULT ({reason}) ==============");
            eprintln!("architecture : Path B — single GPUI Metal stack, alacritty_terminal VT core,");
            eprintln!("               rendered via public shape_line().paint() + paint_quad()");
            eprintln!("stack        : zed-main-patched @ 10b0795 (+bg-luminance +headline-hook patches)");
            eprintln!(
                "build        : {} | display: {display_desc}",
                if cfg!(debug_assertions) { "DEBUG" } else { "RELEASE" },
            );
            eprintln!(
                "workload     : synthetic Claude-stream, seed={} ~{} B/s, {}x{} grid \
                 (fed {} B via feeder thread)",
                self.seed,
                self.bps,
                COLS,
                ROWS,
                self.session.bytes_fed.load(Ordering::Relaxed)
            );
            eprintln!("duration     : {elapsed:.1} s, {} composited frames", g.samples);
            eprintln!("-- frame interval (single stack = terminal present) --");
            eprintln!(
                "  p50 {:.2} ms ({:.1} fps) | p95 {:.2} ms | p99 {:.2} ms | max {:.2} ms | \
                 cliffs>16.6ms {} | cliffs>{:.1}ms(=1.5xp50) {}",
                g.p50_ms, g.fps_p50, g.p95_ms, g.p99_ms, g.max_ms, g.cliffs, g.cliff_auto_ms,
                g.cliffs_auto
            );
            eprintln!("-- render busy-cost --");
            eprintln!("  {}", stats_line("snapshot(lock+copy)   ", &SNAPSHOT_MS));
            eprintln!("  {}", stats_line("render-body(build)    ", &BUILD_MS));
            eprintln!("  {}", stats_line("paint-closure(shape+quads)", &PAINT_MS));
            eprintln!("-- per-draw CPU (headline measurement 2) --");
            eprintln!("  {}", draw_cpu_line());
            eprintln!(
                "  (this run: {} draws after measurement start; \"Draw\" signpost live under \
                 subsystem dev.nickanderssohn.gpui-term-main, category present)",
                metal_draw_count().saturating_sub(self.draws0)
            );
            eprintln!("-- cpu / energy (proc_pid_rusage deltas over the measurement window) --");
            match (&self.cpu0, harness::cpu::sample()) {
                (Some(t0), Some(t1)) => {
                    eprintln!("  {}", harness::cpu::delta_summary(t0, &t1, elapsed))
                }
                _ => eprintln!("  (proc_pid_rusage unavailable)"),
            }
            eprintln!("-- memory (phys_footprint; whole process) --");
            eprintln!(
                "  idle {:.1} MiB | steady {:.1} MiB | peak {:.1} MiB",
                self.mem_idle_mib, self.mem_steady_mib, self.mem_peak_mib
            );
            eprintln!("-- reference (same harness; 0.2.2 = spikes/phase0-poc bin gpui-term) --");
            eprintln!("  0.2.2 single-window release : p50 16.7 / p95 ~17.1-17.4 ms (~60 fps)");
            eprintln!("  0.2.2 draw CPU              : p50 0.076-0.145 ms");
            eprintln!("  Nice (SwiftTerm) Metal.Draw : 1.19 / 2.41 ms p50/p95");
            eprintln!("  raw CSV: {csv}");
            eprintln!("=============================================================================");
            std::process::exit(0);
        }
    }

    impl Render for TermView {
        fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let t_render0 = harness::clock::now();
            harness::stamp_gpui_frame();
            self.frame += 1;

            if self.frame == 1 {
                // Capture gpui's NSView (raw-window-handle): screen info +
                // (interactive-mode-shared) present kick machinery.
                if let Ok(handle) = HasWindowHandle::window_handle(window) {
                    if let RawWindowHandle::AppKit(appkit) = handle.as_raw() {
                        self.ns_view = appkit.ns_view.as_ptr() as *mut NSView;
                    }
                }
                // Idle baseline, then clear the warm-up frame so the measured
                // window starts clean.
                let (phys, _) = harness::mem::sample();
                self.mem_idle_mib = harness::mem::mib(phys);
                self.mem_peak_mib = self.mem_idle_mib;
                self.start_tick = harness::clock::now();
                harness::reset_frame_streams();
                self.cpu0 = harness::cpu::sample();
                nice_draw_metrics::DRAW_DUR_NS.lock().unwrap().clear();
                self.draws0 = metal_draw_count();
                let (fps, name) = screen_info_of_view(self.ns_view);
                eprintln!("[headline] window on display: {name} (max {fps} Hz)");
                self.screen0 = Some((fps, name));
            }

            self.sample_mem();
            if self.frame % 15 == 0 {
                self.mem_series.push((self.elapsed_secs(), self.mem_steady_mib));
            }
            if self.elapsed_secs() >= self.deadline_secs {
                self.finalize_and_exit("measurement window elapsed");
            }

            // Snapshot the grid under a SHORT FairMutex lock (parsing happens
            // on the feeder thread).
            let t_snap0 = harness::clock::now();
            let snap = snapshot(&self.session.term.lock());
            push_ms(
                &SNAPSHOT_MS,
                harness::clock::ms_between(t_snap0, harness::clock::now()),
            );

            // Animated sub-pixel vertical offset — exercises fractional glyph
            // placement + full re-paint every frame (identical to 0.2.2).
            let frac = (self.frame as f32 * 0.7) % LINE_PX;

            // RAF drives the continuous composite — the measurement clock.
            window.request_animation_frame();

            let el = grid_canvas(snap, self.grid_font.clone(), frac, true);
            push_ms(
                &BUILD_MS,
                harness::clock::ms_between(t_render0, harness::clock::now()),
            );
            el
        }
    }

    /// Raw per-sample CSV (frame intervals + memory), same schema as the
    /// 0.2.2 bin's single-window CSV (stack tag marks the zed-main pin).
    fn write_csv(
        path: &Path,
        streams: &harness::FrameStreams,
        display: &str,
        seed: u64,
        bps: usize,
        mem_series: &[(f64, f64)],
    ) -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::File::create(path)?;
        writeln!(f, "# headline run metadata (zed-main pin 10b0795 + patches)")?;
        writeln!(f, "# display={display}")?;
        writeln!(
            f,
            "# build={} seed={seed} bytes_per_sec={bps} windows=1 streaming=1",
            if cfg!(debug_assertions) { "debug" } else { "release" },
        )?;
        writeln!(f, "metric,stack,phase,build,idx,value,unit")?;
        for (idx, w) in streams.gpui_composite.windows(2).enumerate() {
            let ms = harness::clock::ms_between(w[0], w[1]);
            writeln!(f, "frame_interval,gpui-main-native,load,poc,{idx},{ms},ms")?;
        }
        for (idx, (secs, mib)) in mem_series.iter().enumerate() {
            writeln!(f, "mem_phys,gpui-main-native,{secs:.1}s,poc,{idx},{mib:.2},MiB")?;
        }
        Ok(())
    }

    pub fn run_live() {
        let prof = WorkloadProfile::default();
        let deadline = std::env::var("NICE_MAIN_SECS")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(20.0);

        eprintln!(
            "[headline] LIVE single-stack GPUI-native terminal on the zed-main pin: 1 window, \
             synthetic workload seed={} ~{} B/s, ~{deadline:.0}s then auto-exit with the \
             FPS/draw-CPU/memory summary (NICE_MAIN_SECS overrides). Keep the window frontmost \
             AND focused — main frame-caps inactive windows to ~30 fps.",
            prof.seed, prof.bytes_per_sec
        );

        gpui_platform::application().run(move |cx: &mut App| {
            cx.activate(true);
            cx.on_window_closed(|_cx, _id| std::process::exit(0)).detach();

            let session = Session::spawn(prof.seed, prof.bytes_per_sec);
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
                            "Nice Phase-0 — headline (zed-main pin, Path B)".into(),
                        ),
                        appears_transparent: false,
                        traffic_light_position: None,
                    }),
                    kind: WindowKind::Normal,
                    is_resizable: true,
                    focus: true,
                    show: true,
                    ..Default::default()
                },
                move |_window, cx| {
                    cx.new(move |cx| {
                        // GUARANTEED auto-exit (ported watchdog): the render-
                        // path deadline only runs while frames tick; the
                        // watchdog thread cannot starve (App Nap immune).
                        let weak = cx.weak_entity();
                        let mut async_cx = cx.to_async();
                        harness::watchdog::arm(
                            Duration::from_secs_f64(deadline + 3.0),
                            "headline streaming",
                            move || {
                                let done =
                                    weak.update(&mut async_cx, |view: &mut TermView, _| {
                                        view.finalize_and_exit("deadline (watchdog)")
                                    });
                                if done.is_err() {
                                    eprintln!(
                                        "[headline] watchdog: view entity gone; exiting \
                                         without a summary"
                                    );
                                    std::process::exit(2);
                                }
                            },
                        );
                        TermView {
                            session,
                            grid_font: font("Menlo"),
                            frame: 0,
                            start_tick: 0,
                            deadline_secs: deadline,
                            mem_idle_mib: 0.0,
                            mem_steady_mib: 0.0,
                            mem_peak_mib: 0.0,
                            seed: prof.seed,
                            bps: prof.bytes_per_sec,
                            ns_view: std::ptr::null_mut(),
                            cpu0: None,
                            draws0: 0,
                            screen0: None,
                            mem_series: Vec::new(),
                        }
                    })
                },
            )
            .unwrap();
        });
    }

    // =====================================================================
    // INTERACTIVE keystroke-latency mode (headline measurement 3 / spike 5).
    // =====================================================================

    /// Map a GPUI keystroke to raw pty bytes (ported: plain printables +
    /// Return + a couple of controls; deliberately NO kitty/CSI-u encoder).
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

    /// Interactive keystroke-latency view: NO workload, NO RAF — renders only
    /// when the pty reader pings the wake channel (echo arrived) or the OS
    /// itself asks for a frame. One echo batch => one cx.notify() + one
    /// present kick => one damage-gated `MetalRenderer::draw` ("Draw"
    /// signpost). No cursor, no blink timer — zero timer-driven redraws by
    /// construction.
    ///
    /// Keepalive note (main vs 0.2.2): 0.2.2 needed a vendored-gpui patch
    /// (NICE_POC_DAMAGE_ONLY) to stop a 1 s stream of unchanged-scene presents
    /// after every input. On main that keepalive only engages while input
    /// arrives at >= 60 events/s for 100 ms (InputRateTracker) — below that
    /// rate draws stay damage-gated with NO zed-side patch. Keep the injection
    /// harness below ~60 keys/s (it is) and verify draws ~= echo batches in
    /// the summary.
    struct InteractiveView {
        pty: PtySession,
        focus_handle: FocusHandle,
        grid_font: Font,
        frame: u64,
        keys_sent: u64,
        start_tick: u64,
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

        fn kick_platform_display(&self) {
            kick_view_display(self.ns_view);
        }

        fn finalize_and_exit(&self, reason: &str) -> ! {
            let secs = if self.start_tick == 0 {
                0.0
            } else {
                harness::clock::ms_between(self.start_tick, harness::clock::now()) / 1000.0
            };
            let mut draw_ms: Vec<f64> = nice_draw_metrics::DRAW_DUR_NS
                .lock()
                .unwrap()
                .iter()
                .map(|&ns| ns as f64 / 1.0e6)
                .collect();
            let (p50, p95, _p99) = harness::percentiles(&mut draw_ms);
            eprintln!(
                "[headline interactive] {reason} after {secs:.1}s: metal draws {} (real \
                 present submissions, incl. window-open) | scene rebuilds {} | pty bytes \
                 echoed {} | keys sent {} | draw CPU p50 {p50:.3} / p95 {p95:.3} ms \
                 (demand-driven: no RAF, no workload, no cursor/blink timer; main's 1s \
                 keepalive engages only at >=60 input events/s)",
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
                window.focus(&self.focus_handle, cx);
                if let Ok(handle) = HasWindowHandle::window_handle(window) {
                    if let RawWindowHandle::AppKit(appkit) = handle.as_raw() {
                        self.ns_view = appkit.ns_view.as_ptr() as *mut NSView;
                    }
                }
            }
            self.pty.dirty.store(false, Ordering::Release);

            let snap = snapshot(&self.pty.term.lock());
            // frac = 0: no sub-pixel animation (needs a redraw clock this mode
            // deliberately lacks). NOTE: no request_animation_frame anywhere.
            div()
                .size_full()
                .track_focus(&self.focus_handle)
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, _cx| {
                    this.on_key(ev);
                }))
                .child(grid_canvas(snap, self.grid_font.clone(), 0.0, false))
        }
    }

    pub fn run_interactive() {
        let deadline = std::env::var("NICE_MAIN_SECS")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(120.0);

        eprintln!(
            "[headline] INTERACTIVE keystroke-latency mode on the zed-main pin: one window, \
             /bin/cat behind a REAL pty, no workload, no RAF — type (or let the harness \
             CGEventPostToPid) and every kernel echo triggers one demand-driven Metal draw \
             (notify + setNeedsDisplay kick -> displayLayer -> present). \"Draw\" signposts \
             under dev.nickanderssohn.gpui-term-main/present. Auto-exit with a one-line \
             summary after ~{deadline:.0}s (NICE_MAIN_SECS overrides). Keep the injection \
             rate under ~60 keys/s or main's InputRateTracker keepalive un-gates presents \
             for 1 s stretches."
        );

        gpui_platform::application().run(move |cx: &mut App| {
            cx.activate(true);
            cx.on_window_closed(|_cx, _id| std::process::exit(0)).detach();

            let (wake_tx, mut wake_rx) = futures::channel::mpsc::unbounded::<()>();
            let pty = PtySession::spawn(Some(wake_tx)).expect("failed to spawn /bin/cat behind a pty");

            let bounds = Bounds::centered(
                None,
                size(px(COLS as f32 * FONT_PX * 0.62), px(ROWS as f32 * LINE_PX + 40.0)),
                cx,
            );
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        window_background: WindowBackgroundAppearance::Opaque,
                        titlebar: Some(TitlebarOptions {
                            title: Some(
                                "Nice Phase-0 — headline INTERACTIVE (pty: /bin/cat)".into(),
                            ),
                            appears_transparent: false,
                            traffic_light_position: None,
                        }),
                        kind: WindowKind::Normal,
                        is_resizable: true,
                        focus: true,
                        show: true,
                        ..Default::default()
                    },
                    |_window, cx| {
                        cx.new(|cx| {
                            // Echo wakeups: pty reader thread -> unbounded
                            // channel -> this foreground task -> cx.notify()
                            // (rebuild) + kick_platform_display() (the CA
                            // commit that actually PRESENTS it). A channel,
                            // not a poller: the wakeup must not quantize the
                            // measured keystroke latency.
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

                            // Deadline via the guaranteed-fire watchdog (the
                            // gpui executor timer starves under App Nap in a
                            // fully idle app — see harness::watchdog).
                            let weak = cx.weak_entity();
                            let mut async_cx = cx.to_async();
                            harness::watchdog::arm(
                                Duration::from_secs_f64(deadline),
                                "headline interactive",
                                move || {
                                    let done = weak.update(
                                        &mut async_cx,
                                        |view: &mut InteractiveView, _| {
                                            view.finalize_and_exit("deadline (watchdog)")
                                        },
                                    );
                                    if done.is_err() {
                                        eprintln!(
                                            "[headline interactive] watchdog: view entity \
                                             gone; exiting without a summary"
                                        );
                                        std::process::exit(2);
                                    }
                                },
                            );

                            InteractiveView {
                                pty,
                                focus_handle: cx.focus_handle(),
                                grid_font: font("Menlo"),
                                frame: 0,
                                keys_sent: 0,
                                start_tick: 0,
                                ns_view: std::ptr::null_mut(),
                            }
                        })
                    },
                )
                .unwrap();

            // Focus the grid immediately so keys land without a click.
            window
                .update(cx, |view, window, cx| {
                    window.focus(&view.focus_handle, cx);
                })
                .ok();
        });
    }
}

fn main() {
    // Register the os_signpost emitters with the patched renderer hook before
    // anything can draw (harmless headless — just two atomic stores).
    install_draw_hooks();

    let live = matches!(std::env::var("NICE_MAIN_RUN").as_deref(), Ok("1") | Ok("true"));
    let interactive = matches!(
        std::env::var("NICE_MAIN_INTERACTIVE").as_deref(),
        Ok("1") | Ok("true")
    );
    match (live, interactive) {
        (true, true) => gui::run_interactive(),
        (true, false) => gui::run_live(),
        (false, true) => run_headless_interactive(),
        (false, false) => run_headless(),
    }
}
