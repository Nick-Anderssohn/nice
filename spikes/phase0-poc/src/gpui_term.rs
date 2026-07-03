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
//!     (single stack) that IS the terminal present (spike 5 latency);
//!   * gpui::nice_poc_metrics — per-draw CPU durations (always), GPU command-
//!     buffer durations (NICE_POC_GPU_TS=1), shape-cache hit counters, and
//!     atlas texture/tile/upload counters (spikes 6 + 10).
//!
//! §13 spikes 6/7/9/10 prep (2026-07-02) — all live modes stay display-gated
//! behind NICE_POC_RUN=1; every new env defaults OFF (previous modes are
//! bit-identical with the flags unset):
//!   * Spike 6 (release per-frame cost + energy):
//!       - render busy-time stamps (snapshot / render-body / paint closure)
//!         + per-draw CPU cost + optional GPU time + shape-cache hit rate +
//!         proc_pid_rusage CPU/wakeup/energy deltas, all in the summary.
//!       - NICE_POC_ENERGY_STATE=idle|dot — the powermetrics three-state
//!         protocol: `idle` = window open, no feed, no RAF (demand-driven,
//!         ~zero draws); `dot` = no feed but ONE animating 12px chrome dot
//!         (RAF at refresh — GPUI's whole-scene repaint idle cost).
//!   * Spike 7 (real-trace workload):
//!       - NICE_POC_TRACE=<file.nicetrace> — replay a captured real pty byte
//!         trace timing-faithfully instead of the synthetic generator
//!         (format + capture procedure: harness::trace + the pty-capture bin).
//!       - NICE_POC_TRACE_SPEED=<f> (default 1.0), NICE_POC_TRACE_LOOP=1,
//!         NICE_POC_TRACE_MODE=drain (max-rate drain test: wall-clock to
//!         quiescent + max frame interval).
//!   * Spike 9 (scrollback / resize-reflow / selection under streaming):
//!       - NICE_POC_SCROLLBACK=<n> (alacritty scrolling_history, default 10000)
//!       - NICE_POC_SCROLL_CHURN=1 (history prefill + per-frame display-offset
//!         churn), NICE_POC_RESIZE_STORM=1 (+NICE_POC_RESIZE_MS, default 400:
//!         periodic Term reflow-resize + real NSWindow setFrame), and
//!         NICE_POC_SELECTION=1 (programmatic selection drag/re-anchor each
//!         frame, held across scrollback eviction; rendered as inverse cells)
//!       - NICE_POC_PREFILL_LINES=<n> — history prefill line count (defaults
//!         to the scrollback limit when scroll/selection churn is on)
//!       - kill-signal instrument: per-resize reflow stall ms in the summary.
//!       - headless: NICE_POC_SPIKE9=1 cargo run --bin gpui-term — no-display
//!         reflow/scroll/selection self-test incl. memory at 3 scrollback
//!         limits (the spike-8 open question).
//!   * Spike 10 (atlas pressure):
//!       - NICE_POC_ATLAS=1 — synthetic kitty-style animation (30 fps 512x512
//!         distinct frames via paint_image) + 12 static sixel-stand-in images
//!         painted every frame; stale animation frames are drop_image()d
//!         (NICE_POC_ATLAS_RETAIN=1 to never drop = growth failure demo).
//!       - NICE_POC_GLYPH_SWEEP=1 — feeder streams an unbounded distinct-glyph
//!         sweep (unicode ranges + bold/italic SGR) to grow the GLYPH atlas.
//!       - NICE_POC_STYLES=1 — map SGR bold/italic to real font variants in
//!         paint (auto-on under the sweep; off by default to keep the audited
//!         numbers reproducible).
//!       - summary prints gpui::nice_poc_metrics atlas counters (textures/
//!         bytes allocated+freed, tiles inserted/removed, upload traffic).
//!   * §13 harness fixes: cliff threshold self-calibrated at 1.5 x median
//!     (printed next to the legacy 16.6ms count), display/build/seed/flags
//!     persisted as CSV `#` comments, per-sample memory rows in the CSV, and
//!     the main.rs hot-plug guard ported (display re-checked at finalize).

#![allow(dead_code)]

#[path = "harness.rs"]
mod harness;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as TermPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
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
    /// (utf8-byte-len, fg-rgb, style-bits) per cell, in column order.
    /// style-bits: 1 = bold, 2 = italic — always 0 unless `styles` was
    /// requested (NICE_POC_STYLES / glyph sweep), so the default paint path
    /// is byte-identical to the audited rev.
    cells: Vec<(usize, u32, u8)>,
    bgs: Vec<BgRun>,
}

/// Snapshot the VISIBLE viewport (honoring the grid's display offset — the
/// scrollback scroll position) plus the active selection (rendered inverse).
/// With offset 0, no selection, and `styles == false` this is behaviorally
/// identical to the audited snapshot.
fn snapshot(term: &Term<EventProxy>, styles: bool) -> Vec<RowSnap> {
    let rows = term.screen_lines();
    let cols = term.columns();
    let display_offset = term.grid().display_offset() as i32;
    let sel_range = term.selection.as_ref().and_then(|s| s.to_range(term));
    let mut out = Vec::with_capacity(rows);
    for line in 0..rows {
        let buffer_line = Line(line as i32 - display_offset);
        let mut text = String::with_capacity(cols);
        let mut cells = Vec::with_capacity(cols);
        let mut bgs: Vec<BgRun> = Vec::new();
        for col in 0..cols {
            let point = TermPoint::new(buffer_line, Column(col));
            let cell = &term.grid()[point];
            let selected = sel_range.map_or(false, |r| r.contains(point));
            let inverse = cell.flags.contains(Flags::INVERSE) ^ selected;
            let mut fg = color_rgb(cell.fg, DEFAULT_FG);
            let mut bg = color_rgb(cell.bg, DEFAULT_BG);
            if inverse {
                std::mem::swap(&mut fg, &mut bg);
            }
            let style_bits = if styles {
                (cell.flags.contains(Flags::BOLD) as u8)
                    | ((cell.flags.contains(Flags::ITALIC) as u8) << 1)
            } else {
                0
            };
            let ch = if cell.c == '\0' { ' ' } else { cell.c };
            let mut buf = [0u8; 4];
            let s = ch.encode_utf8(&mut buf);
            text.push_str(s);
            cells.push((s.len(), fg, style_bits));
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
// Spike 10 — distinct-glyph sweep + synthetic image ("kitty"/"sixel") frames.
// =========================================================================

/// Unicode ranges swept for ever-new distinct glyphs (Latin/punct, Latin-ext,
/// box drawing, block elements, kana, a broad CJK slab, emoji). The CJK +
/// emoji ranges alone are tens of thousands of codepoints, so a run at the
/// default byte rate keeps minting NEW atlas tiles for its whole duration.
const SWEEP_RANGES: &[(u32, u32)] = &[
    (0x0021, 0x007E),   // ASCII printable
    (0x00A1, 0x017F),   // Latin-1 supplement + Latin extended-A
    (0x2500, 0x257F),   // box drawing
    (0x2580, 0x259F),   // block elements
    (0x3041, 0x30FF),   // hiragana + katakana
    (0x4E00, 0x9FFF),   // CJK unified ideographs (~21k glyphs)
    (0x1F300, 0x1F64F), // emoji (polychrome path where supported)
];

/// Deterministic distinct-glyph sweep chunk (~`target` bytes): consecutive
/// codepoints from the ranges above, wrapped in rotating SGR bold/italic
/// (real style variants multiply atlas keys when NICE_POC_STYLES is on).
/// `counter` persists across calls so glyphs keep advancing, not repeating.
fn sweep_chunk(counter: &mut u64, target: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(target + 64);
    while out.len() < target {
        let style = match *counter % 4 {
            0 => &b"\x1b[0m"[..],
            1 => &b"\x1b[1m"[..],
            2 => &b"\x1b[3m"[..],
            _ => &b"\x1b[1;3m"[..],
        };
        out.extend_from_slice(style);
        // One line of ~32 consecutive codepoints from a rotating range.
        let (lo, hi) = SWEEP_RANGES[(*counter as usize / 7) % SWEEP_RANGES.len()];
        let span = (hi - lo + 1) as u64;
        for i in 0..32u64 {
            let cp = lo + ((counter.wrapping_mul(31).wrapping_add(i * 3)) % span) as u32;
            if let Some(ch) = char::from_u32(cp) {
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
        }
        out.extend_from_slice(b"\x1b[0m\r\n");
        *counter += 1;
    }
    out
}

/// Procedurally generate one distinct BGRA image (bytes differ per `t`), as
/// gpui's `RenderImage` expects (BGRA bytes in an `image::RgbaImage`
/// container — the same convention gpui's own img loaders use before upload).
fn gen_image(w: u32, h: u32, t: u64) -> std::sync::Arc<gpui::RenderImage> {
    let mut bytes = vec![0u8; (w * h * 4) as usize];
    let mut rng = harness::Rng::new(t.wrapping_mul(0x9E37_79B9).wrapping_add(w as u64) | 1);
    let speckle = (rng.next_u64() & 0xFF) as u32;
    for y in 0..h {
        let row = &mut bytes[(y * w * 4) as usize..((y + 1) * w * 4) as usize];
        for x in 0..w {
            let i = (x * 4) as usize;
            // Moving gradient + per-frame phase so every frame's bytes differ.
            let phase = (t as u32).wrapping_mul(7);
            row[i] = ((x + phase) & 0xFF) as u8; // B
            row[i + 1] = ((y + phase / 2) & 0xFF) as u8; // G
            row[i + 2] = ((x ^ y ^ speckle) & 0xFF) as u8; // R
            row[i + 3] = 0xFF; // A
        }
    }
    let buffer = image::RgbaImage::from_raw(w, h, bytes).expect("image buffer");
    std::sync::Arc::new(gpui::RenderImage::new(vec![image::Frame::new(buffer)]))
}

/// Spike 10 image-pressure state: a 30 fps 512x512 "kitty animation" (every
/// frame is a brand-new image = a brand-new polychrome atlas tile) plus 12
/// static "sixel" stand-ins painted every frame. Stale animation frames are
/// released with `window.drop_image` (=> atlas `remove()`) unless `retain`.
struct ImgPressure {
    statics: Vec<std::sync::Arc<gpui::RenderImage>>,
    anim: VecDeque<std::sync::Arc<gpui::RenderImage>>,
    /// NICE_POC_ATLAS_RETAIN=1: never drop — demonstrates unbounded growth.
    retained: Vec<std::sync::Arc<gpui::RenderImage>>,
    retain: bool,
    last_emit: Option<Instant>,
    pub emitted: u64,
    pub dropped: u64,
}

const ANIM_SIZE: u32 = 512;
const ANIM_KEEP: usize = 2;
const STATIC_SIZES: [u32; 12] = [64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256, 288];

impl ImgPressure {
    fn new(retain: bool) -> Self {
        let statics = STATIC_SIZES
            .iter()
            .enumerate()
            .map(|(i, &s)| gen_image(s, s, 0x5EED_0000 + i as u64))
            .collect();
        ImgPressure {
            statics,
            anim: VecDeque::new(),
            retained: Vec::new(),
            retain,
            last_emit: None,
            emitted: 0,
            dropped: 0,
        }
    }

    /// Advance the animation clock (30 fps) and return (images to paint,
    /// images to drop_image this frame). Paint order: statics then the
    /// newest animation frame.
    #[allow(clippy::type_complexity)]
    fn tick(
        &mut self,
    ) -> (
        Vec<std::sync::Arc<gpui::RenderImage>>,
        Option<std::sync::Arc<gpui::RenderImage>>,
        Vec<std::sync::Arc<gpui::RenderImage>>,
    ) {
        let due = self
            .last_emit
            .map_or(true, |t| t.elapsed() >= Duration::from_millis(33));
        if due {
            self.last_emit = Some(Instant::now());
            self.emitted += 1;
            self.anim.push_back(gen_image(ANIM_SIZE, ANIM_SIZE, self.emitted));
        }
        let mut drops = Vec::new();
        while self.anim.len() > ANIM_KEEP {
            let old = self.anim.pop_front().unwrap();
            if self.retain {
                self.retained.push(old);
            } else {
                self.dropped += 1;
                drops.push(old);
            }
        }
        (
            self.statics.clone(),
            self.anim.back().cloned(),
            drops,
        )
    }
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

fn env_flag(key: &str) -> bool {
    matches!(std::env::var(key).as_deref(), Ok("1") | Ok("true"))
}

/// Spike 6 powermetrics/energy states (NICE_POC_ENERGY_STATE).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EnergyState {
    /// Window open, no feed, NO RAF — demand-driven, ~zero draws.
    Idle,
    /// No feed, but one 12px chrome dot animates via RAF at refresh — GPUI's
    /// whole-scene repaint model with a single animating chrome element (the
    /// audit's named idle-cost risk for a laptop app).
    Dot,
}

/// §13 spikes 6/7/9/10 run configuration (all default OFF = the audited
/// behavior, bit-for-bit).
#[derive(Clone)]
struct SpikeCfg {
    // -- spike 7: real-trace workload ------------------------------------
    trace_path: Option<String>,
    trace: Option<Arc<harness::trace::Trace>>,
    trace_speed: f64,
    trace_drain: bool,
    trace_loop: bool,
    // -- spike 6: energy states -------------------------------------------
    energy_state: Option<EnergyState>,
    // -- spike 9: scrollback / resize-reflow / selection -------------------
    scrollback: usize,
    scroll_churn: bool,
    resize_storm: bool,
    resize_ms: u64,
    selection_churn: bool,
    prefill_lines: usize,
    // -- spike 10: atlas pressure ------------------------------------------
    atlas: bool,
    atlas_retain: bool,
    glyph_sweep: bool,
    /// Map SGR bold/italic to real font variants in paint (auto-on under the
    /// glyph sweep; default off so the audited numbers stay reproducible).
    styles: bool,
}

impl SpikeCfg {
    fn from_env() -> Self {
        let trace_path = std::env::var("NICE_POC_TRACE").ok().filter(|s| !s.is_empty());
        let glyph_sweep = env_flag("NICE_POC_GLYPH_SWEEP");
        let scroll_churn = env_flag("NICE_POC_SCROLL_CHURN");
        let selection_churn = env_flag("NICE_POC_SELECTION");
        let scrollback = env_usize("NICE_POC_SCROLLBACK").unwrap_or(10_000);
        SpikeCfg {
            trace_path,
            trace: None, // loaded lazily by the live/headless entry points
            trace_speed: std::env::var("NICE_POC_TRACE_SPEED")
                .ok()
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|v| *v > 0.0)
                .unwrap_or(1.0),
            trace_drain: matches!(
                std::env::var("NICE_POC_TRACE_MODE").as_deref(),
                Ok("drain") | Ok("max")
            ),
            trace_loop: env_flag("NICE_POC_TRACE_LOOP"),
            energy_state: match std::env::var("NICE_POC_ENERGY_STATE").as_deref() {
                Ok("idle") => Some(EnergyState::Idle),
                Ok("dot") => Some(EnergyState::Dot),
                _ => None,
            },
            scrollback,
            scroll_churn,
            resize_storm: env_flag("NICE_POC_RESIZE_STORM"),
            resize_ms: env_usize("NICE_POC_RESIZE_MS").unwrap_or(400).max(50) as u64,
            selection_churn,
            prefill_lines: env_usize("NICE_POC_PREFILL_LINES").unwrap_or(
                if scroll_churn || selection_churn {
                    scrollback
                } else {
                    0
                },
            ),
            atlas: env_flag("NICE_POC_ATLAS"),
            atlas_retain: env_flag("NICE_POC_ATLAS_RETAIN"),
            glyph_sweep,
            styles: env_flag("NICE_POC_STYLES") || glyph_sweep,
        }
    }

    fn load_trace(&mut self) -> Result<(), String> {
        if let Some(p) = &self.trace_path {
            let t = harness::trace::Trace::load(Path::new(p))
                .map_err(|e| format!("NICE_POC_TRACE={p}: {e}"))?;
            eprintln!(
                "[gpui-term] trace loaded: {p} — {} records, {} bytes, {:.1}s native duration",
                t.records.len(),
                t.total_bytes,
                t.duration_secs()
            );
            self.trace = Some(Arc::new(t));
        }
        Ok(())
    }

    /// Short tag describing the active special modes (CSV naming + summary);
    /// None = the plain audited workload.
    fn tag(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if self.trace.is_some() || self.trace_path.is_some() {
            parts.push(if self.trace_drain {
                "trace-drain".into()
            } else {
                "trace".into()
            });
        }
        match self.energy_state {
            Some(EnergyState::Idle) => parts.push("energy-idle".into()),
            Some(EnergyState::Dot) => parts.push("energy-dot".into()),
            None => {}
        }
        if self.scroll_churn {
            parts.push("scroll".into());
        }
        if self.resize_storm {
            parts.push("resize".into());
        }
        if self.selection_churn {
            parts.push("select".into());
        }
        if self.atlas {
            parts.push("atlas".into());
        }
        if self.glyph_sweep {
            parts.push("sweep".into());
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("+"))
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
    /// mach tick when this session's finite feed (trace replay/drain)
    /// completed; 0 while still feeding. Always 0 for endless feeds.
    feed_done: Arc<AtomicU64>,
    /// Whether this session's feed is finite (trace, non-loop) — the
    /// coordinator waits on exactly these before the early "quiescent"
    /// finalize in trace/drain runs.
    expect_done: bool,
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

/// What a session's feeder thread streams (spikes 6/7/10 additions; the
/// original behaviors map to `Heartbeat` (old bps==0) and `Synthetic`).
enum FeedSpec {
    /// NOTHING is ever fed (spike 6 energy states) — the feeder parks.
    Idle,
    /// One short heartbeat line per second (idle-with-live-session).
    Heartbeat,
    /// The deterministic synthetic workload at `bps` bytes/s (the audited
    /// default when `bps == WorkloadProfile::default().bytes_per_sec`).
    Synthetic { bps: usize },
    /// Spike 10: unbounded distinct-glyph sweep at `bps` bytes/s.
    Sweep { bps: usize },
    /// Spike 7: replay a captured real pty trace. `speed` scales time
    /// (2.0 = twice as fast); `drain` ignores timestamps and feeds at max
    /// rate; `loop_replay` restarts the trace when it ends (endless feed).
    Trace {
        trace: Arc<harness::trace::Trace>,
        speed: f64,
        drain: bool,
        loop_replay: bool,
    },
}

struct Session {
    term: Arc<FairMutex<Term<EventProxy>>>,
    /// Set by the feeder after each parse; consumed by the notify poller.
    dirty: Arc<AtomicBool>,
    bytes_fed: Arc<AtomicU64>,
    /// mach tick when a FINITE feed (trace, non-loop) completed; 0 otherwise.
    feed_done: Arc<AtomicU64>,
    /// Wall ms the feeder spent in the max-rate drain (f64 bits; 0 = n/a).
    drain_ms: Arc<AtomicU64>,
    /// Wall ms spent prefilling scrollback history (f64 bits; 0 = none).
    prefill_ms: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    feeder: Option<std::thread::JoinHandle<()>>,
}

impl Session {
    /// Original entry point (kept so K=1/multi synthetic runs read the same):
    /// `bytes_per_sec == 0` => heartbeat, else synthetic at that rate.
    fn spawn(index: usize, seed: u64, bytes_per_sec: usize) -> Self {
        let spec = if bytes_per_sec == 0 {
            FeedSpec::Heartbeat
        } else {
            FeedSpec::Synthetic {
                bps: bytes_per_sec,
            }
        };
        Self::spawn_spec(index, seed, spec, 10_000, 0)
    }

    /// Spawn with an explicit feed spec + scrollback limit + history prefill
    /// (spikes 6/7/9/10). `scrolling_history` is the alacritty Config
    /// scrollback limit; `prefill_lines` > 0 fills that many history lines at
    /// max rate BEFORE the paced feed starts (spike 9 scroll/selection runs).
    fn spawn_spec(
        index: usize,
        seed: u64,
        spec: FeedSpec,
        scrolling_history: usize,
        prefill_lines: usize,
    ) -> Self {
        let size = Size {
            rows: ROWS,
            cols: COLS,
        };
        let config = Config {
            scrolling_history,
            ..Config::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, EventProxy)));
        let dirty = Arc::new(AtomicBool::new(false));
        let bytes_fed = Arc::new(AtomicU64::new(0));
        let feed_done = Arc::new(AtomicU64::new(0));
        let drain_ms = Arc::new(AtomicU64::new(0));
        let prefill_ms = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let feeder = {
            let term = Arc::clone(&term);
            let dirty = Arc::clone(&dirty);
            let bytes_fed = Arc::clone(&bytes_fed);
            let feed_done = Arc::clone(&feed_done);
            let drain_ms = Arc::clone(&drain_ms);
            let prefill_ms_out = Arc::clone(&prefill_ms);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name(format!("poc-feeder-{index}"))
                .spawn(move || {
                    let mut parser: Processor = Processor::new();

                    // ---- optional scrollback prefill (spike 9) -----------
                    if prefill_lines > 0 {
                        let t0 = Instant::now();
                        let mut fed_lines = 0usize;
                        let mut chunk: Vec<u8> = Vec::with_capacity(64 * 1024);
                        while fed_lines < prefill_lines && !stop.load(Ordering::Relaxed) {
                            chunk.clear();
                            let batch = (prefill_lines - fed_lines).min(512);
                            for i in 0..batch {
                                let n = fed_lines + i;
                                chunk.extend_from_slice(
                                    format!(
                                        "prefill {n:06} \
                                         abcdefghijklmnopqrstuvwxyz0123456789 \
                                         the quick brown fox jumps over the lazy dog\r\n"
                                    )
                                    .as_bytes(),
                                );
                            }
                            {
                                let mut t = term.lock();
                                parser.advance(&mut *t, &chunk);
                            }
                            bytes_fed.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                            fed_lines += batch;
                        }
                        dirty.store(true, Ordering::Release);
                        let ms = t0.elapsed().as_secs_f64() * 1000.0;
                        prefill_ms_out.store(ms.to_bits(), Ordering::Relaxed);
                        eprintln!(
                            "[gpui-term] w{index} prefilled {fed_lines} history lines in {ms:.0} ms"
                        );
                    }

                    match spec {
                        FeedSpec::Idle => {
                            while !stop.load(Ordering::Relaxed) {
                                std::thread::sleep(Duration::from_millis(200));
                            }
                        }
                        FeedSpec::Heartbeat => {
                            let mut beat = 0u64;
                            while !stop.load(Ordering::Relaxed) {
                                beat += 1;
                                let line =
                                    format!("\r\x1b[2K[idle w{index}] heartbeat {beat}");
                                {
                                    let mut t = term.lock();
                                    parser.advance(&mut *t, line.as_bytes());
                                }
                                bytes_fed.fetch_add(line.len() as u64, Ordering::Relaxed);
                                dirty.store(true, Ordering::Release);
                                std::thread::sleep(Duration::from_secs(1));
                            }
                        }
                        FeedSpec::Synthetic { bps } => {
                            let mut wl = Workload::new(WorkloadProfile {
                                seed,
                                ..WorkloadProfile::default()
                            });
                            let per_tick =
                                ((bps * FEED_TICK_MS as usize) / 1000).max(64);
                            while !stop.load(Ordering::Relaxed) {
                                let t0 = Instant::now();
                                // Generate OUTSIDE the lock; hold it only to parse.
                                let chunk = wl.stream(per_tick);
                                {
                                    let mut t = term.lock();
                                    parser.advance(&mut *t, &chunk);
                                }
                                bytes_fed.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                                dirty.store(true, Ordering::Release);
                                if let Some(rest) = Duration::from_millis(FEED_TICK_MS)
                                    .checked_sub(t0.elapsed())
                                {
                                    std::thread::sleep(rest);
                                }
                            }
                        }
                        FeedSpec::Sweep { bps } => {
                            let mut counter = seed;
                            let per_tick =
                                ((bps * FEED_TICK_MS as usize) / 1000).max(64);
                            while !stop.load(Ordering::Relaxed) {
                                let t0 = Instant::now();
                                let chunk = sweep_chunk(&mut counter, per_tick);
                                {
                                    let mut t = term.lock();
                                    parser.advance(&mut *t, &chunk);
                                }
                                bytes_fed.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                                dirty.store(true, Ordering::Release);
                                if let Some(rest) = Duration::from_millis(FEED_TICK_MS)
                                    .checked_sub(t0.elapsed())
                                {
                                    std::thread::sleep(rest);
                                }
                            }
                        }
                        FeedSpec::Trace {
                            trace,
                            speed,
                            drain,
                            loop_replay,
                        } => {
                            if drain {
                                // Max-rate drain: ignore timestamps, feed as
                                // fast as the parser accepts (per-record
                                // locking keeps the render loop schedulable —
                                // FairMutex hands the lock over fairly).
                                let t0 = Instant::now();
                                'drain: for rec in &trace.records {
                                    if stop.load(Ordering::Relaxed) {
                                        break 'drain;
                                    }
                                    {
                                        let mut t = term.lock();
                                        parser.advance(&mut *t, &rec.data);
                                    }
                                    bytes_fed
                                        .fetch_add(rec.data.len() as u64, Ordering::Relaxed);
                                    dirty.store(true, Ordering::Release);
                                }
                                let ms = t0.elapsed().as_secs_f64() * 1000.0;
                                drain_ms.store(ms.to_bits(), Ordering::Relaxed);
                                feed_done.store(harness::clock::now(), Ordering::Release);
                                eprintln!(
                                    "[gpui-term] w{index} trace DRAIN complete: {} bytes in \
                                     {ms:.0} ms ({:.1} MB/s parse throughput)",
                                    trace.total_bytes,
                                    trace.total_bytes as f64 / 1.0e6 / (ms / 1000.0)
                                );
                            } else {
                                // Timing-faithful replay (speed-scaled).
                                let t0 = Instant::now();
                                let mut base_ns = 0u64;
                                'replay: loop {
                                    for rec in &trace.records {
                                        if stop.load(Ordering::Relaxed) {
                                            break 'replay;
                                        }
                                        let target_ns = base_ns
                                            + (rec.offset_ns as f64 / speed) as u64;
                                        loop {
                                            let el = t0.elapsed().as_nanos() as u64;
                                            if el >= target_ns
                                                || stop.load(Ordering::Relaxed)
                                            {
                                                break;
                                            }
                                            let rem_ns = target_ns - el;
                                            std::thread::sleep(Duration::from_nanos(
                                                rem_ns.min(5_000_000),
                                            ));
                                        }
                                        {
                                            let mut t = term.lock();
                                            parser.advance(&mut *t, &rec.data);
                                        }
                                        bytes_fed.fetch_add(
                                            rec.data.len() as u64,
                                            Ordering::Relaxed,
                                        );
                                        dirty.store(true, Ordering::Release);
                                    }
                                    if !loop_replay {
                                        break 'replay;
                                    }
                                    // Seamless rebase for the next pass.
                                    base_ns = t0.elapsed().as_nanos() as u64;
                                }
                                if !loop_replay {
                                    feed_done
                                        .store(harness::clock::now(), Ordering::Release);
                                    eprintln!(
                                        "[gpui-term] w{index} trace replay complete \
                                         ({:.1}s wall)",
                                        t0.elapsed().as_secs_f64()
                                    );
                                }
                            }
                        }
                    }
                })
                .expect("failed to spawn session feeder thread")
        };

        Session {
            term,
            dirty,
            bytes_fed,
            feed_done,
            drain_ms,
            prefill_ms,
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
    let snap = snapshot(&term, false);
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
    let snap = snapshot(&ps.term.lock(), false);
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

/// HEADLESS spike 9 self-test (`NICE_POC_SPIKE9=1`, no display): the VT-core
/// half of scrollback / resize-reflow / selection — memory at 3 scrollback
/// limits (the spike-8 open question), the full-history reflow stall
/// (§13 kill-signal: multi-hundred-ms), scroll-position churn, and a
/// selection held across scrollback eviction. The live run adds frame pacing
/// + the real NSWindow resize on top of this machinery.
fn run_headless_spike9() {
    eprintln!("{}", harness::banner());
    eprintln!("[gpui-term] HEADLESS spike 9 self-test (scrollback/reflow/selection; no display).");

    fn fill_lines(term: &mut Term<EventProxy>, parser: &mut Processor, n: usize) {
        let mut chunk: Vec<u8> = Vec::with_capacity(64 * 1024);
        let mut fed = 0usize;
        while fed < n {
            chunk.clear();
            let batch = (n - fed).min(512);
            for i in 0..batch {
                chunk.extend_from_slice(
                    format!(
                        "prefill {:06} abcdefghijklmnopqrstuvwxyz0123456789 \
                         the quick brown fox jumps over the lazy dog\r\n",
                        fed + i
                    )
                    .as_bytes(),
                );
            }
            parser.advance(term, &chunk);
            fed += batch;
        }
    }

    let size = Size { rows: ROWS, cols: COLS };

    // ---- memory at 3 scrollback limits (spike 8 open question) ----------
    eprintln!("-- memory vs scrollback limit (history full; parser-side only, no atlas) --");
    for limit in [1_000usize, 10_000, 100_000] {
        let (before, _) = harness::mem::sample();
        let t0 = Instant::now();
        {
            let mut term = Term::new(
                Config { scrolling_history: limit, ..Config::default() },
                &size,
                EventProxy,
            );
            let mut parser: Processor = Processor::new();
            fill_lines(&mut term, &mut parser, limit + ROWS);
            let (after, _) = harness::mem::sample();
            eprintln!(
                "  scrollback {limit:>6}: fill {} lines in {:>5.0} ms | phys_footprint \
                 {:>7.1} -> {:>7.1} MiB (delta {:+.1})",
                limit + ROWS,
                t0.elapsed().as_secs_f64() * 1000.0,
                harness::mem::mib(before),
                harness::mem::mib(after),
                harness::mem::mib(after) - harness::mem::mib(before),
            );
            assert_eq!(term.grid().history_size(), limit, "history not at limit");
        } // term dropped before the next limit's baseline sample
    }

    // ---- reflow stall (10k history — the §13 kill-signal number) --------
    let mut term = Term::new(
        Config { scrolling_history: 10_000, ..Config::default() },
        &size,
        EventProxy,
    );
    let mut parser: Processor = Processor::new();
    fill_lines(&mut term, &mut parser, 10_000 + ROWS);
    eprintln!("-- Term::resize reflow stall (full 10k-line history rewrap per resize) --");
    let mut stalls: Vec<f64> = Vec::new();
    for _ in 0..2 {
        for (cols, rows) in [(100usize, 34usize), (80, 28), (120, 40)] {
            let t0 = Instant::now();
            term.resize(Size { rows, cols });
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            eprintln!("  resize -> {cols}x{rows}: {ms:.1} ms");
            stalls.push(ms);
        }
    }
    let max_stall = stalls.iter().cloned().fold(0.0f64, f64::max);
    eprintln!(
        "  max reflow stall {max_stall:.1} ms  [KILL-SIGNAL threshold: multi-hundred-ms]"
    );

    // ---- scroll churn ----------------------------------------------------
    let t0 = Instant::now();
    term.scroll_display(Scroll::Top);
    let top_off = term.grid().display_offset();
    for i in 0..1000 {
        term.scroll_display(Scroll::Delta(if i % 2 == 0 { -7 } else { 5 }));
    }
    term.scroll_display(Scroll::Bottom);
    eprintln!(
        "-- scroll churn: Top (offset {top_off}) + 1000 Delta ops + Bottom in {:.1} ms --",
        t0.elapsed().as_secs_f64() * 1000.0
    );

    // ---- selection across eviction ---------------------------------------
    let top = term.grid().topmost_line();
    let anchor = TermPoint::new(Line(top.0 + 2), Column(0));
    term.selection = Some(Selection::new(SelectionType::Simple, anchor, Side::Left));
    if let Some(sel) = &mut term.selection {
        sel.update(TermPoint::new(Line(0), Column(20)), Side::Right);
    }
    let before = term.selection.as_ref().and_then(|s| s.to_range(&term));
    assert!(before.is_some(), "fresh selection must resolve to a range");
    // Stream PAST the anchor: 2k more lines rotate the anchor line out of the
    // (full) history. Selection must stay SANE (rotated or cleared, no panic).
    fill_lines(&mut term, &mut parser, 2_000);
    let after = term.selection.as_ref().and_then(|s| s.to_range(&term));
    eprintln!(
        "-- selection across eviction: before={:?} -> after 2k more lines={:?} (both \
           non-panicking outcomes are sane; alacritty rotates/clamps) --",
        before.map(|r| (r.start.line.0, r.end.line.0)),
        after.map(|r| (r.start.line.0, r.end.line.0)),
    );

    let snap = snapshot(&term, false);
    let nonempty = snap.iter().filter(|r| !r.text.trim_end().is_empty()).count();
    let ok = nonempty > 0 && !stalls.is_empty();
    eprintln!(
        "RESULT: {}",
        if ok {
            "PASS (spike 9 VT-core machinery live; see stall numbers above)"
        } else {
            "FAIL"
        }
    );
    std::process::exit(if ok { 0 } else { 1 });
}

/// HEADLESS spike 7 self-test / drain measurement (`NICE_POC_TRACE=<file>`
/// without `NICE_POC_RUN`): load a nicetrace, max-rate drain it through the
/// alacritty parser (the parse half of the §13 "cat a 10 MB trace" test), and
/// spot-check the timing-faithful replay pacing. `NICE_POC_TRACE=selftest`
/// synthesizes a small deterministic trace first (no capture needed).
fn run_headless_trace() {
    eprintln!("{}", harness::banner());
    let arg = std::env::var("NICE_POC_TRACE").unwrap_or_default();
    let path = if arg == "selftest" {
        let p = std::env::temp_dir().join("nice-poc-trace-selftest.nicetrace");
        let mut w = harness::trace::TraceWriter::create(&p).expect("create selftest trace");
        let mut wl = Workload::new(WorkloadProfile::default());
        for i in 0..100u64 {
            let chunk = wl.stream(4096);
            w.record_at(i * 5_000_000, &chunk).expect("write record"); // 5 ms apart
        }
        let (recs, bytes, _) = w.finish().expect("finish selftest trace");
        eprintln!("[gpui-term] synthesized selftest trace: {recs} records / {bytes} bytes -> {}", p.display());
        p
    } else {
        std::path::PathBuf::from(&arg)
    };

    let trace = match harness::trace::Trace::load(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("RESULT: FAIL (trace load: {e})");
            std::process::exit(1);
        }
    };
    eprintln!(
        "[gpui-term] trace: {} — {} records / {} bytes / {:.2}s native duration",
        path.display(),
        trace.records.len(),
        trace.total_bytes,
        trace.duration_secs()
    );

    // Max-rate drain through the parser (no display, no render).
    let size = Size { rows: ROWS, cols: COLS };
    let mut term = Term::new(
        Config { scrolling_history: 10_000, ..Config::default() },
        &size,
        EventProxy,
    );
    let mut parser: Processor = Processor::new();
    let t0 = Instant::now();
    for rec in &trace.records {
        parser.advance(&mut term, &rec.data);
    }
    let drain_ms = t0.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "-- max-rate drain (parse half): {} bytes in {drain_ms:.1} ms ({:.1} MB/s) --",
        trace.total_bytes,
        trace.total_bytes as f64 / 1.0e6 / (drain_ms / 1000.0)
    );

    // Pacing spot-check on short traces only (a real session would replay in
    // real time — the LIVE run does that; here we just prove the mechanism).
    if trace.duration_secs() <= 3.0 && !trace.records.is_empty() {
        let speed = 2.0;
        let t0 = Instant::now();
        for rec in &trace.records {
            let target = Duration::from_nanos((rec.offset_ns as f64 / speed) as u64);
            if let Some(rest) = target.checked_sub(t0.elapsed()) {
                std::thread::sleep(rest);
            }
        }
        let wall = t0.elapsed().as_secs_f64();
        let expect = trace.duration_secs() / speed;
        eprintln!(
            "-- paced replay spot-check (x{speed}): wall {wall:.2}s vs native/speed \
             {expect:.2}s --"
        );
    }

    let snap = snapshot(&term, false);
    let nonempty = snap.iter().filter(|r| !r.text.trim_end().is_empty()).count();
    let ok = !trace.records.is_empty() && trace.total_bytes > 0 && nonempty > 0;
    eprintln!(
        "RESULT: {}",
        if ok {
            "PASS (trace load -> parse -> grid path live; drain number above)"
        } else {
            "FAIL"
        }
    );
    std::process::exit(if ok { 0 } else { 1 });
}

/// HEADLESS spike 10 self-test (`NICE_POC_ATLAS=1` or `NICE_POC_GLYPH_SWEEP=1`
/// without `NICE_POC_RUN`): proves the image-generation + sweep paths with no
/// display. The atlas counters themselves only move in the LIVE run (a Metal
/// atlas needs a window).
fn run_headless_atlas() {
    eprintln!("{}", harness::banner());
    eprintln!("[gpui-term] HEADLESS spike 10 self-test (image gen + glyph sweep; no display).");

    // Image pressure state: 12 statics + a few 30fps animation ticks.
    let mut img = ImgPressure::new(false);
    assert_eq!(img.statics.len(), 12, "want 12 static images");
    let s0 = img.statics[0].size(0);
    assert_eq!(s0.width.0 as u32, STATIC_SIZES[0], "static size mismatch");
    let mut drops_total = 0usize;
    for _ in 0..4 {
        let (statics, anim, drops) = img.tick();
        assert_eq!(statics.len(), 12);
        assert!(anim.is_some(), "animation frame present");
        drops_total += drops.len();
        std::thread::sleep(Duration::from_millis(40));
    }
    let anim_size = 512 * 512 * 4;
    eprintln!(
        "  images: {} statics + {} anim frames emitted ({} bytes/frame), {} dropped \
         after keep={ANIM_KEEP}",
        img.statics.len(),
        img.emitted,
        anim_size,
        drops_total
    );
    assert!(img.emitted >= 3, "30fps clock should emit >=3 frames in 4 ticks/160ms");
    assert!(drops_total >= 1, "stale frames should be dropped");

    // Glyph sweep: distinct-codepoint growth through the parser.
    let mut counter = 42u64;
    let size = Size { rows: ROWS, cols: COLS };
    let mut term = Term::new(Config::default(), &size, EventProxy);
    let mut parser: Processor = Processor::new();
    let mut distinct: std::collections::HashSet<char> = std::collections::HashSet::new();
    for _ in 0..50 {
        let chunk = sweep_chunk(&mut counter, 512);
        distinct.extend(
            String::from_utf8_lossy(&chunk)
                .chars()
                .filter(|c| !c.is_control() && *c != '[' && !c.is_ascii_digit()),
        );
        parser.advance(&mut term, &chunk);
    }
    let snap = snapshot(&term, true);
    let nonempty = snap.iter().filter(|r| !r.text.trim_end().is_empty()).count();
    eprintln!(
        "  sweep: {} distinct codepoints across 50 chunks; grid non-empty rows {nonempty}",
        distinct.len()
    );
    let ok = distinct.len() > 200 && nonempty > 0;
    eprintln!(
        "RESULT: {}",
        if ok {
            "PASS (image gen + sweep paths live; atlas counters need the live run)"
        } else {
            "FAIL"
        }
    );
    std::process::exit(if ok { 0 } else { 1 });
}

/// HEADLESS watchdog self-test (`NICE_POC_WATCHDOG_SELFTEST=1`, no display):
/// proves the guaranteed-fire deadline end-to-end with the main thread parked
/// in `dispatch_main()` — a main thread that services ONLY the libdispatch
/// main queue, with zero windows/events/timers (the exact starvation shape
/// that hung the idle energy state live). PASS = the watchdog thread's
/// dispatch_async_f enqueue reached the main thread and ran the callback.
/// If the mechanism is broken, the watchdog's own hard-exit(3) fires ~21 s
/// later, so this test can never hang.
fn run_headless_watchdog() {
    eprintln!("{}", harness::banner());
    eprintln!(
        "[gpui-term] HEADLESS watchdog self-test: arming a 1s deadline, then parking the \
         main thread in dispatch_main() (no runloop sources, no events)."
    );
    unsafe extern "C" {
        fn dispatch_main() -> !;
    }
    let armed = Instant::now();
    harness::watchdog::arm(Duration::from_secs(1), "watchdog-selftest", move || {
        eprintln!(
            "[gpui-term] watchdog fired on the main thread {:.2}s after arm (want ~1s)",
            armed.elapsed().as_secs_f64()
        );
        let ok = armed.elapsed() < Duration::from_secs(10);
        eprintln!(
            "RESULT: {}",
            if ok {
                "PASS (deadline watchdog: thread -> main-queue enqueue -> main-thread callback)"
            } else {
                "FAIL (fired but far past the deadline)"
            }
        );
        std::process::exit(if ok { 0 } else { 1 });
    });
    unsafe { dispatch_main() }
}

// =========================================================================
// LIVE GPUI run.
// =========================================================================

mod gui {
    use super::*;
    use gpui::{
        canvas, div, fill, font, point, prelude::*, px, rgb, size, App, Application, Bounds,
        ClickEvent, Context, Corners, FocusHandle, Font, FontStyle, FontWeight, Hsla, AppContext,
        IntoElement, KeyDownEvent, Keystroke, Pixels, Render, RenderImage, SharedString, Styled,
        TextRun, TitlebarOptions, Window, WindowBackgroundAppearance, WindowBounds, WindowKind,
        WindowOptions,
    };
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::NSView;
    use objc2_foundation::{NSRect, NSSize};
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

    // ---- spike 6/9 per-run metric stores (w0 only records into these) ----

    /// Grid snapshot cost per w0 render (lock + copy), ms.
    static SNAPSHOT_MS: Mutex<Vec<f64>> = Mutex::new(Vec::new());
    /// Whole `render()` body cost per w0 render (element build incl. snapshot
    /// + spike drivers), ms. The canvas paint runs later in the frame.
    static BUILD_MS: Mutex<Vec<f64>> = Mutex::new(Vec::new());
    /// Canvas paint-closure cost per w0 frame (shape_line + paint_quad +
    /// glyph paint + images), ms.
    static PAINT_MS: Mutex<Vec<f64>> = Mutex::new(Vec::new());
    /// Spike 9: wall ms of each `Term::resize` (history reflow stall).
    static RESIZE_STALL_MS: Mutex<Vec<f64>> = Mutex::new(Vec::new());
    /// Spike 9 counters.
    static SCROLL_OPS: AtomicU64 = AtomicU64::new(0);
    static SEL_REANCHORS: AtomicU64 = AtomicU64::new(0);
    static SEL_EVICTED_FRAMES: AtomicU64 = AtomicU64::new(0);

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

    /// The four style variants of the grid font (NICE_POC_STYLES / sweep).
    #[derive(Clone)]
    struct FontSet {
        base: Font,
        bold: Font,
        italic: Font,
        bold_italic: Font,
    }

    impl FontSet {
        fn new(family: &'static str) -> Self {
            let base = font(family);
            let mut bold = base.clone();
            bold.weight = FontWeight::BOLD;
            let mut italic = base.clone();
            italic.style = FontStyle::Italic;
            let mut bold_italic = bold.clone();
            bold_italic.style = FontStyle::Italic;
            FontSet {
                base,
                bold,
                italic,
                bold_italic,
            }
        }

        fn pick(&self, bits: u8) -> &Font {
            match bits & 3 {
                1 => &self.bold,
                2 => &self.italic,
                3 => &self.bold_italic,
                _ => &self.base,
            }
        }
    }

    /// Mark an NSView + its backing CAMetalLayer as needing display so the
    /// next CA commit fires `displayLayer:` -> gpui request-frame ->
    /// `Window::present()` -> `MetalRenderer::draw`, independent of the
    /// display-link state. (The interactive mode's load-bearing present kick,
    /// shared here so BACKGROUND multi-session windows actually present their
    /// demand-driven redraws too — before this fix they rebuilt scenes on
    /// notify but never presented when their display link was stopped.)
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

    /// Display + max refresh of the screen hosting `ns_view`'s window (the
    /// main.rs hot-plug guard, ported — §13 harness fix).
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

    /// Resize the NSWindow hosting `ns_view` (spike 9 resize storm — a real
    /// window-frame change so GPUI relayouts and the drawable resizes).
    fn set_window_size(ns_view: *mut NSView, w: f64, h: f64) {
        if ns_view.is_null() {
            return;
        }
        unsafe {
            let view: &NSView = &*ns_view;
            let Some(window) = view.window() else { return };
            let mut frame: NSRect = window.frame();
            // Keep the top-left corner fixed (AppKit origin is bottom-left).
            frame.origin.y += frame.size.height - h;
            frame.size = NSSize::new(w, h);
            window.setFrame_display(frame, true);
        }
    }

    struct TermView {
        /// Window index (0 = the measurement coordinator).
        index: usize,
        /// Streaming windows RAF-render continuously; background windows are
        /// demand-driven (dirty-flag notify poller).
        streaming: bool,
        cfg: MultiCfg,
        spike: SpikeCfg,
        /// The session's Term + feeder thread (parsing happens there, NOT in
        /// render — spike 8 restructure).
        session: Session,
        fonts: FontSet,
        frame: u64,
        start_tick: u64,
        deadline_secs: f64,
        mem_idle_mib: f64,
        mem_steady_mib: f64,
        mem_peak_mib: f64,
        seed: u64,
        bps: usize,
        /// GPUI's NSView (raw-window-handle), captured on first render —
        /// used for the bg present kick, screen info, and the resize storm.
        ns_view: *mut NSView,
        // -- spike 6 baselines (captured at measurement start, w0 only) ----
        cpu0: Option<harness::cpu::CpuSample>,
        shape0: (u64, u64, u64),
        atlas0_mono: [u64; 7],
        atlas0_poly: [u64; 7],
        screen0: Option<(i64, String)>,
        /// (elapsed_s, phys MiB) series persisted into the CSV (§13 fix).
        mem_series: Vec<(f64, f64)>,
        // -- spike 9 drivers ------------------------------------------------
        scroll_dir: i32,
        last_resize: Option<Instant>,
        resize_phase: usize,
        // -- spike 10 -------------------------------------------------------
        img: Option<ImgPressure>,
    }

    impl TermView {
        #[allow(clippy::too_many_arguments)]
        fn new(
            index: usize,
            streaming: bool,
            cfg: MultiCfg,
            spike: SpikeCfg,
            session: Session,
            deadline_secs: f64,
            seed: u64,
            bps: usize,
        ) -> Self {
            let img = (index == 0 && spike.atlas).then(|| ImgPressure::new(spike.atlas_retain));
            Self {
                index,
                streaming,
                cfg,
                spike,
                session,
                fonts: FontSet::new("Menlo"),
                frame: 0,
                start_tick: 0,
                deadline_secs,
                mem_idle_mib: 0.0,
                mem_steady_mib: 0.0,
                mem_peak_mib: 0.0,
                seed,
                bps,
                ns_view: std::ptr::null_mut(),
                cpu0: None,
                shape0: (0, 0, 0),
                atlas0_mono: [0; 7],
                atlas0_poly: [0; 7],
                screen0: None,
                mem_series: Vec::new(),
                scroll_dir: 1,
                last_resize: None,
                resize_phase: 0,
                img,
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

        fn kick_platform_display(&self) {
            kick_view_display(self.ns_view);
        }

        /// Spike 9: per-frame scrollback scroll-position churn — a triangle
        /// wave over the full history depth (±3 lines/frame) with a periodic
        /// snap back to Bottom (the sticky-bottom jump).
        fn drive_scroll(&mut self) {
            let mut t = self.session.term.lock();
            if self.frame % 900 == 0 {
                t.scroll_display(Scroll::Bottom);
                self.scroll_dir = 1;
            } else {
                let hist = t.grid().history_size();
                let off = t.grid().display_offset();
                if self.scroll_dir > 0 && off + 3 >= hist {
                    self.scroll_dir = -1;
                } else if self.scroll_dir < 0 && off <= 3 {
                    self.scroll_dir = 1;
                }
                t.scroll_display(Scroll::Delta(self.scroll_dir * 3));
            }
            SCROLL_OPS.fetch_add(1, Ordering::Relaxed);
        }

        /// Spike 9: programmatic selection churn under streaming — re-anchor
        /// deep in history every ~4s (so ongoing streaming EVICTS the anchor
        /// line), otherwise sweep the selection end across the viewport (the
        /// drag analog). Rendered inverse by `snapshot`, so it has real paint
        /// cost. NOTE: input-level drags (real NSEvent mouse) are the IME/
        /// input spike's turf; this drives the same alacritty `Selection` +
        /// grid-rotation code the fork's hardest patches live in.
        fn drive_selection(&mut self) {
            let mut t = self.session.term.lock();
            let cols = t.columns();
            if self.frame % 240 == 1 || t.selection.is_none() {
                let top = t.grid().topmost_line();
                let anchor = TermPoint::new(Line(top.0 + 2), Column(0));
                t.selection = Some(Selection::new(SelectionType::Simple, anchor, Side::Left));
                SEL_REANCHORS.fetch_add(1, Ordering::Relaxed);
            } else {
                let line = Line((self.frame % ROWS as u64) as i32);
                let col = Column((self.frame as usize * 7) % cols);
                if let Some(sel) = &mut t.selection {
                    sel.update(TermPoint::new(line, col), Side::Right);
                }
            }
            // Count frames where the held selection no longer resolves to a
            // range (anchor evicted from history / cleared by grid rotation).
            let gone = t
                .selection
                .as_ref()
                .and_then(|s| s.to_range(&t))
                .is_none();
            if gone {
                SEL_EVICTED_FRAMES.fetch_add(1, Ordering::Relaxed);
            }
        }

        /// Spike 9: resize storm — every NICE_POC_RESIZE_MS cycle the Term
        /// through 4 sizes (a full-history reflow each time; the stall is
        /// timed) and resize the real NSWindow to match (GPUI relayout +
        /// drawable resize).
        fn drive_resize(&mut self) {
            const SIZES: [(usize, usize); 4] = [(120, 40), (100, 34), (80, 28), (100, 34)];
            let due = self
                .last_resize
                .map_or(self.frame > 30, |t| {
                    t.elapsed() >= Duration::from_millis(self.spike.resize_ms)
                });
            if !due {
                return;
            }
            self.last_resize = Some(Instant::now());
            self.resize_phase = (self.resize_phase + 1) % SIZES.len();
            let (cols, rows) = SIZES[self.resize_phase];
            let t0 = Instant::now();
            {
                let mut t = self.session.term.lock();
                t.resize(Size { rows, cols });
            }
            push_ms(&RESIZE_STALL_MS, t0.elapsed().as_secs_f64() * 1000.0);
            set_window_size(
                self.ns_view,
                cols as f64 * (FONT_PX * 0.62) as f64,
                rows as f64 * LINE_PX as f64 + 40.0,
            );
        }

        fn finalize_and_exit(&mut self, reason: &str) -> ! {
            // Final memory sample first — idle/demand-driven runs may not have
            // rendered (and therefore sampled) for long stretches.
            self.sample_mem();
            if self.mem_idle_mib == 0.0 {
                self.mem_idle_mib = self.mem_steady_mib;
            }

            let streams = harness::drain_frame_streams();
            // Single stack: GPUI's composite IS the terminal present. One vsync
            // on this panel = 16.67 ms (60 Hz); cliff threshold matches §10.
            let g = harness::interval_stats(&streams.gpui_composite, 16.6);
            let elapsed = self.elapsed_secs();

            // Hot-plug guard (ported from main.rs — §13 harness fix).
            let screen_now = screen_info_of_view(self.ns_view);
            let mut display_desc = match &self.screen0 {
                Some((fps, name)) => format!("{name} (max {fps} Hz)"),
                None => "<unknown>".to_string(),
            };
            if let Some((fps0, name0)) = &self.screen0 {
                if screen_now.0 != *fps0 || screen_now.1 != *name0 {
                    eprintln!(
                        "[gpui-term] ⚠️ DISPLAY CHANGED MID-RUN: start='{name0}' ({fps0} Hz) \
                         -> exit='{}' ({} Hz). Numbers are CONTAMINATED — re-run on a single \
                         stable display.",
                        screen_now.1, screen_now.0
                    );
                    display_desc = format!(
                        "{name0} -> {} (CHANGED MID-RUN — CONTAMINATED)",
                        screen_now.1
                    );
                }
            }

            let multi = self.cfg.windows > 1;
            let tag = self.spike.tag();
            let csv = match (&tag, multi) {
                (None, false) => "./gpui-term-gpui-native-single-stack.csv".to_string(),
                (None, true) => format!(
                    "./gpui-term-multi-{}w{}s.csv",
                    self.cfg.windows, self.cfg.streaming
                ),
                (Some(t), false) => format!("./gpui-term-{t}.csv"),
                (Some(t), true) => format!(
                    "./gpui-term-{t}-{}w{}s.csv",
                    self.cfg.windows, self.cfg.streaming
                ),
            };
            let meta = CsvMeta {
                display: display_desc.clone(),
                seed: self.seed,
                bps: self.bps,
                cfg: self.cfg,
                tag: tag.clone().unwrap_or_else(|| "none".to_string()),
                scrollback: self.spike.scrollback,
                trace: self.spike.trace_path.clone(),
                mem_series: std::mem::take(&mut self.mem_series),
            };
            let _ = if multi {
                write_multi_csv(Path::new(&csv), &meta)
            } else {
                write_csv(Path::new(&csv), &streams, &meta)
            };

            eprintln!("\n================ gpui-term LIVE RESULT ({reason}) ================");
            eprintln!("architecture : Path B — single GPUI Metal stack, alacritty_terminal VT core,");
            eprintln!("               rendered via public shape_line().paint() + paint_quad()");
            eprintln!(
                "build        : {} | display: {display_desc} | gpui_txn={} damage_only={}",
                if cfg!(debug_assertions) { "DEBUG" } else { "RELEASE" },
                env_flag("NICE_POC_GPUI_TXN") as u8,
                env_flag("NICE_POC_DAMAGE_ONLY") as u8,
            );
            match (&self.spike.trace, self.spike.energy_state) {
                (Some(t), _) => eprintln!(
                    "workload     : REAL TRACE {} — {} records / {} bytes / {:.1}s native, \
                     speed x{}, mode={}{} ({}x{} grid)",
                    self.spike.trace_path.as_deref().unwrap_or("?"),
                    t.records.len(),
                    t.total_bytes,
                    t.duration_secs(),
                    self.spike.trace_speed,
                    if self.spike.trace_drain { "DRAIN (max-rate)" } else { "timing-faithful" },
                    if self.spike.trace_loop { " loop" } else { "" },
                    COLS,
                    ROWS
                ),
                (None, Some(state)) => eprintln!(
                    "workload     : NONE — energy state {:?} ({})",
                    state,
                    match state {
                        EnergyState::Idle => "window open, no feed, no RAF",
                        EnergyState::Dot => "no feed; one 12px chrome dot animating via RAF",
                    }
                ),
                (None, None) if self.spike.glyph_sweep => eprintln!(
                    "workload     : distinct-glyph sweep, seed={} ~{} B/s, {}x{} grid (styles=on)",
                    self.seed, self.bps, COLS, ROWS
                ),
                (None, None) => eprintln!(
                    "workload     : synthetic Claude-stream, seed={} ~{} B/s, {}x{} grid",
                    self.seed, self.bps, COLS, ROWS
                ),
            }
            eprintln!(
                "spike flags  : {} | scrollback={}{}",
                tag.as_deref().unwrap_or("none"),
                self.spike.scrollback,
                {
                    let pf = f64::from_bits(self.session.prefill_ms.load(Ordering::Relaxed));
                    if self.spike.prefill_lines > 0 {
                        format!(" prefill={} lines ({pf:.0} ms)", self.spike.prefill_lines)
                    } else {
                        String::new()
                    }
                }
            );
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
            eprintln!("duration     : {elapsed:.1} s, {} composited frames", g.samples);
            eprintln!("-- frame interval (single stack = terminal present) --");
            eprintln!(
                "  p50 {:.2} ms ({:.1} fps) | p95 {:.2} ms | p99 {:.2} ms | max {:.2} ms | \
                 cliffs>16.6ms {} | cliffs>{:.1}ms(=1.5xp50) {}",
                g.p50_ms, g.fps_p50, g.p95_ms, g.p99_ms, g.max_ms, g.cliffs, g.cliff_auto_ms,
                g.cliffs_auto
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

            // ---- spike 6: per-frame cost decomposition + CPU/energy -------
            eprintln!("-- render busy-cost (w0; spike 6) --");
            eprintln!("  {}", stats_line("snapshot(lock+copy)   ", &SNAPSHOT_MS));
            eprintln!("  {}", stats_line("render-body(build)    ", &BUILD_MS));
            eprintln!("  {}", stats_line("paint-closure(shape+quads)", &PAINT_MS));
            {
                let mut draw_ms: Vec<f64> = gpui::nice_poc_metrics::DRAW_DUR_NS
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|&ns| ns as f64 / 1.0e6)
                    .collect();
                let n = draw_ms.len();
                let (p50, p95, p99) = harness::percentiles(&mut draw_ms);
                let max = draw_ms.last().copied().unwrap_or(0.0);
                eprintln!(
                    "  MetalRenderer::draw CPU (all windows): n={n} p50 {p50:.3} ms | p95 \
                     {p95:.3} | p99 {p99:.3} | max {max:.3} | total draws {} \
                     [comparable to Nice Metal.Draw 1.19/2.41 ms p50/p95]",
                    metal_draw_count()
                );
                let mut gpu_ms: Vec<f64> = gpui::nice_poc_metrics::GPU_DUR_NS
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|&ns| ns as f64 / 1.0e6)
                    .collect();
                if gpu_ms.is_empty() {
                    eprintln!(
                        "  GPU time: (not recorded — set NICE_POC_GPU_TS=1 for \
                         MTLCommandBuffer GPUStart/EndTime deltas)"
                    );
                } else {
                    let n = gpu_ms.len();
                    let (p50, p95, p99) = harness::percentiles(&mut gpu_ms);
                    let max = gpu_ms.last().copied().unwrap_or(0.0);
                    eprintln!(
                        "  GPU time per command buffer: n={n} p50 {p50:.3} ms | p95 {p95:.3} | \
                         p99 {p99:.3} | max {max:.3}"
                    );
                }
                let (c, p, m) = gpui::nice_poc_metrics::shape_cache_stats();
                let (dc, dp, dm) = (
                    c.saturating_sub(self.shape0.0),
                    p.saturating_sub(self.shape0.1),
                    m.saturating_sub(self.shape0.2),
                );
                let total = dc + dp + dm;
                eprintln!(
                    "  shape cache (LineLayoutCache): hit-current {dc} | hit-prev-frame {dp} | \
                     miss(fresh CoreText) {dm} | hit rate {:.1}%",
                    if total > 0 {
                        (dc + dp) as f64 / total as f64 * 100.0
                    } else {
                        0.0
                    }
                );
            }
            eprintln!("-- cpu / energy (proc_pid_rusage deltas over the measurement window) --");
            match (&self.cpu0, harness::cpu::sample()) {
                (Some(t0), Some(t1)) => {
                    eprintln!("  {}", harness::cpu::delta_summary(t0, &t1, elapsed))
                }
                _ => eprintln!("  (proc_pid_rusage unavailable)"),
            }

            // ---- spike 10: atlas pressure ---------------------------------
            {
                use gpui::nice_poc_metrics::{ATLAS_MONO, ATLAS_POLY};
                let mib = |b: u64| b as f64 / (1024.0 * 1024.0);
                let line = |name: &str, s: [u64; 7], b: [u64; 7]| {
                    let d: Vec<u64> = s.iter().zip(b.iter()).map(|(a, b)| a - b).collect();
                    eprintln!(
                        "  {name}: tex +{}/-{} (live {} = {:.1} MiB) | tiles +{}/-{} | \
                         upload {:.1} MiB   [this run]",
                        d[0],
                        d[1],
                        s[0] as i64 - s[1] as i64,
                        mib(s[2] - s[3]),
                        d[4],
                        d[5],
                        mib(d[6])
                    );
                };
                eprintln!(
                    "-- atlas (gpui::nice_poc_metrics; live = process-cumulative alloc-freed) --"
                );
                line("mono(A8 glyphs)   ", ATLAS_MONO.snapshot(), self.atlas0_mono);
                line("poly(BGRA images) ", ATLAS_POLY.snapshot(), self.atlas0_poly);
                if let Some(img) = &self.img {
                    eprintln!(
                        "  image pressure: anim frames emitted {} | drop_image()d {} | \
                         retain={} | statics {} painted/frame",
                        img.emitted,
                        img.dropped,
                        self.spike.atlas_retain as u8,
                        img.statics.len()
                    );
                }
            }

            // ---- spike 9 ---------------------------------------------------
            if self.spike.resize_storm || self.spike.scroll_churn || self.spike.selection_churn {
                eprintln!("-- scrollback / reflow / selection (spike 9) --");
                if self.spike.resize_storm {
                    eprintln!(
                        "  {}   [KILL-SIGNAL: multi-hundred-ms stalls]",
                        stats_line("Term::resize reflow stall", &RESIZE_STALL_MS)
                    );
                }
                if self.spike.scroll_churn {
                    eprintln!(
                        "  scroll churn: {} scroll_display ops (triangle over {} history \
                         lines, snap-to-bottom every 900 frames)",
                        SCROLL_OPS.load(Ordering::Relaxed),
                        self.spike.scrollback
                    );
                }
                if self.spike.selection_churn {
                    eprintln!(
                        "  selection churn: {} re-anchors | {} frames with the held selection \
                         resolved to None (anchor evicted/cleared)",
                        SEL_REANCHORS.load(Ordering::Relaxed),
                        SEL_EVICTED_FRAMES.load(Ordering::Relaxed)
                    );
                }
            }

            // ---- spike 7: drain summary ------------------------------------
            if self.spike.trace.is_some() {
                let drain = f64::from_bits(self.session.drain_ms.load(Ordering::Relaxed));
                if self.spike.trace_drain && drain > 0.0 {
                    eprintln!("-- max-rate drain (spike 7) --");
                    eprintln!(
                        "  w0 parse wall-clock to quiescent: {drain:.0} ms | max frame \
                         interval during run: {:.2} ms",
                        g.max_ms
                    );
                }
            }

            eprintln!("-- memory (phys_footprint; whole process, incl. every session) --");
            eprintln!(
                "  idle {:.1} MiB | steady {:.1} MiB | peak {:.1} MiB (scrollback limit {})",
                self.mem_idle_mib, self.mem_steady_mib, self.mem_peak_mib, self.spike.scrollback
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

    /// If every finite (trace) feed has completed, return the LATEST
    /// completion tick — the coordinator finalizes ~1 s after it.
    fn all_finite_feeds_done() -> Option<u64> {
        let slots = win_slots();
        let mut latest = 0u64;
        let mut any = false;
        for slot in slots {
            if slot.expect_done {
                any = true;
                let t = slot.feed_done.load(Ordering::Acquire);
                if t == 0 {
                    return None;
                }
                latest = latest.max(t);
            }
        }
        if any { Some(latest) } else { None }
    }

    impl Render for TermView {
        fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let t_render0 = harness::clock::now();
            if self.index == 0 {
                harness::stamp_gpui_frame();
            }
            stamp_window_frame(self.index);
            self.frame += 1;

            if self.frame == 1 {
                // Capture gpui's NSView (every window): bg-window present
                // kick, screen info, resize storm.
                if let Ok(handle) = HasWindowHandle::window_handle(window) {
                    if let RawWindowHandle::AppKit(appkit) = handle.as_raw() {
                        self.ns_view = appkit.ns_view.as_ptr() as *mut NSView;
                    }
                }
            }

            if self.index == 0 && self.frame == 1 {
                // Capture an idle baseline, then clear the warm-up frame so the
                // measured window starts clean.
                let (phys, _) = harness::mem::sample();
                self.mem_idle_mib = harness::mem::mib(phys);
                self.mem_peak_mib = self.mem_idle_mib;
                self.start_tick = harness::clock::now();
                harness::reset_frame_streams();
                clear_window_frames();
                // Spike 6 baselines: CPU/energy + vendored-gpui metric zeroes.
                self.cpu0 = harness::cpu::sample();
                gpui::nice_poc_metrics::DRAW_DUR_NS.lock().unwrap().clear();
                gpui::nice_poc_metrics::GPU_DUR_NS.lock().unwrap().clear();
                self.shape0 = gpui::nice_poc_metrics::shape_cache_stats();
                self.atlas0_mono = gpui::nice_poc_metrics::ATLAS_MONO.snapshot();
                self.atlas0_poly = gpui::nice_poc_metrics::ATLAS_POLY.snapshot();
                // Display recorded per run (hot-plug guard, ported from main.rs).
                let (fps, name) = screen_info_of_view(self.ns_view);
                eprintln!("[gpui-term] window on display: {name} (max {fps} Hz)");
                self.screen0 = Some((fps, name));
            }

            if self.index == 0 {
                self.sample_mem();
                if self.frame % 15 == 0 {
                    self.mem_series.push((self.elapsed_secs(), self.mem_steady_mib));
                }
                // Spike 9 drivers (all default off; each holds the FairMutex
                // briefly on the main thread — contention with the feeder is
                // part of what's being measured).
                if self.spike.scroll_churn {
                    self.drive_scroll();
                }
                if self.spike.selection_churn {
                    self.drive_selection();
                }
                if self.spike.resize_storm {
                    self.drive_resize();
                }
                // Spike 7: a finite trace feed finalizes ~1 s after the last
                // byte parsed ("quiescent"), before the deadline.
                if let Some(done_at) = all_finite_feeds_done() {
                    if harness::clock::ms_between(done_at, harness::clock::now()) > 1000.0 {
                        self.finalize_and_exit("trace complete (quiescent +1s)");
                    }
                }
                if self.elapsed_secs() >= self.deadline_secs {
                    self.finalize_and_exit("measurement window elapsed");
                }
            }

            // Snapshot the grid under a SHORT FairMutex lock (parsing happens
            // on the session's feeder thread now — spike 8 restructure). The
            // snapshot is owned so the 'static paint closure can render it
            // without borrowing `self`.
            let t_snap0 = harness::clock::now();
            let snap = snapshot(&self.session.term.lock(), self.spike.styles);
            if self.index == 0 {
                push_ms(
                    &SNAPSHOT_MS,
                    harness::clock::ms_between(t_snap0, harness::clock::now()),
                );
            }
            let fonts = self.fonts.clone();
            // Animated sub-pixel vertical offset → exercises fractional glyph
            // placement + full re-paint every frame (the sub-line scroll path).
            // Static in the energy states (nothing should animate but the dot).
            let frac = if self.spike.energy_state.is_some() {
                0.0
            } else {
                (self.frame as f32 * 0.7) % LINE_PX
            };

            // Spike 10 image pressure (w0 only): advance the 30 fps animation,
            // hand the paint closure the images to paint + the stale frames to
            // drop_image (=> atlas remove()).
            let images = self.img.as_mut().map(|img| img.tick());
            // Spike 6 `dot` energy state: one animating chrome dot.
            let dot_frame = (self.spike.energy_state == Some(EnergyState::Dot))
                .then_some(self.frame);

            // Streaming windows composite continuously — this RAF is the
            // measurement clock (unchanged from the audited rev). Background
            // windows are demand-driven: the notify poller in run_live turns
            // the feeder's dirty flag into cx.notify() + a present kick.
            if self.streaming {
                window.request_animation_frame();
            }

            let el = grid_canvas(
                snap,
                fonts,
                frac,
                CanvasExtras {
                    record: self.index == 0,
                    images,
                    dot_frame,
                },
            );
            if self.index == 0 {
                push_ms(
                    &BUILD_MS,
                    harness::clock::ms_between(t_render0, harness::clock::now()),
                );
            }
            el
        }
    }

    fn rgb_to_hsla(v: u32) -> Hsla {
        rgb(v).into()
    }

    /// Per-frame extras threaded into the paint closure (all None/false for
    /// the interactive view and the plain audited run).
    struct CanvasExtras {
        /// Record the paint-closure duration into PAINT_MS (w0 only).
        record: bool,
        /// Spike 10: (static images, newest animation frame, frames to
        /// drop_image this frame).
        images: Option<(
            Vec<std::sync::Arc<RenderImage>>,
            Option<std::sync::Arc<RenderImage>>,
            Vec<std::sync::Arc<RenderImage>>,
        )>,
        /// Spike 6 `dot` energy state: paint one animating 12px dot.
        dot_frame: Option<u64>,
    }

    impl CanvasExtras {
        fn none() -> Self {
            CanvasExtras {
                record: false,
                images: None,
                dot_frame: None,
            }
        }
    }

    /// Build the paint canvas for one grid snapshot. Shared by the workload
    /// view (`TermView`) and the interactive view (`InteractiveView`).
    fn grid_canvas(
        snap: Vec<RowSnap>,
        fonts: FontSet,
        frac: f32,
        extras: CanvasExtras,
    ) -> impl IntoElement {
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

                    // Build per-cell text runs (shape_line coalesces same-style).
                    let runs: Vec<TextRun> = row
                        .cells
                        .iter()
                        .map(|(len, fg, style)| TextRun {
                            len: *len,
                            font: fonts.pick(*style).clone(),
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
                    // Divide by THIS row's cell count — the resize storm makes
                    // the grid narrower than the COLS constant.
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

                    let _ = shaped.paint(point(origin_x, y), line_h, window, cx);
                }

                // ---- spike 10: image pressure over the grid ---------------
                if let Some((statics, anim, drops)) = &extras.images {
                    // 12 static "sixels" in a 3-wide grid along the right edge.
                    let right = bounds.origin.x + bounds.size.width;
                    for (i, img) in statics.iter().enumerate() {
                        let side = px(STATIC_SIZES[i % STATIC_SIZES.len()] as f32 / 2.0);
                        let col = (i % 3) as f32;
                        let row = (i / 3) as f32;
                        let rect = Bounds {
                            origin: point(
                                right - px(3.0 * 150.0) + px(col * 150.0),
                                top + px(280.0 + row * 90.0),
                            ),
                            size: size(side, side),
                        };
                        let _ = window.paint_image(
                            rect,
                            Corners::default(),
                            img.clone(),
                            0,
                            false,
                        );
                    }
                    // The 512x512 animation frame, top-right, painted at 256pt
                    // (1:1 device pixels at 2x scale).
                    if let Some(img) = anim {
                        let rect = Bounds {
                            origin: point(right - px(266.0), top + px(10.0)),
                            size: size(px(256.0), px(256.0)),
                        };
                        let _ = window.paint_image(
                            rect,
                            Corners::default(),
                            img.clone(),
                            0,
                            false,
                        );
                    }
                    // Release stale animation frames => atlas remove().
                    for d in drops {
                        let _ = window.drop_image(d.clone());
                    }
                }

                // ---- spike 6 `dot` energy state: one animating chrome dot --
                if let Some(f) = extras.dot_frame {
                    let x = 8.0 + ((f % 120) as f32) * 2.0;
                    let hue = ((f % 255) as u32) << 16 | 0x0059F5;
                    let rect = Bounds {
                        origin: point(origin_x + px(x), top + px(4.0)),
                        size: size(px(12.0), px(12.0)),
                    };
                    window.paint_quad(fill(rect, rgb(hue)));
                }

                if extras.record {
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

    // =====================================================================
    // Activity badge — a dot + `NN KB/s` throughput label in the window
    // chrome. Renders in the accent style while terminal output is arriving
    // and dims to the idle style after ~2 s of silence; clicking it toggles
    // between the full (dot + label) and compact (dot-only) presentation,
    // which persists across relaunch.
    // =====================================================================

    /// Chrome-bar height (px) reserved above the grid for the top bar that
    /// hosts the session label + activity badge.
    const CHROME_BAR_PX: f32 = 28.0;

    /// Rolling window (ms) the throughput rate is averaged over (~1 s).
    const BADGE_WINDOW_MS: f64 = 1000.0;
    /// Silence (ms since the last byte) after which the badge dims to idle.
    const BADGE_IDLE_MS: f64 = 2000.0;
    /// While within this many ms of the last byte the view keeps requesting
    /// animation frames, so the KB/s figure decays and the badge crosses the
    /// idle threshold even with no further echoes; past it the interactive
    /// view falls back to fully demand-driven (no RAF), as before.
    const BADGE_TICK_MS: f64 = 2200.0;

    // Chrome colors, chosen to sit with the dark terminal palette above.
    const CHROME_BAR_BG: u32 = 0x0016_1616; // a hair lighter than DEFAULT_BG
    const CHROME_BAR_BORDER: u32 = 0x0028_2828;
    const CHROME_LABEL: u32 = 0x0080_8080; // muted top-bar label
    const BADGE_ACCENT: u32 = 0x008A_E234; // palette bright-green (active dot)
    const BADGE_ACCENT_TEXT: u32 = DEFAULT_FG; // active label = terminal fg
    const BADGE_IDLE_DOT: u32 = 0x0055_5753; // palette bright-black (dim dot)
    const BADGE_IDLE_TEXT: u32 = 0x0060_6060;

    /// Full (dot + label) vs compact (dot-only) badge presentation. Persisted
    /// across relaunch so the user's chosen density survives a restart.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum BadgePresentation {
        Full,
        Compact,
    }

    impl BadgePresentation {
        fn toggled(self) -> Self {
            match self {
                BadgePresentation::Full => BadgePresentation::Compact,
                BadgePresentation::Compact => BadgePresentation::Full,
            }
        }

        fn as_str(self) -> &'static str {
            match self {
                BadgePresentation::Full => "full",
                BadgePresentation::Compact => "compact",
            }
        }

        fn parse(s: &str) -> Option<Self> {
            match s.trim() {
                "full" => Some(BadgePresentation::Full),
                "compact" => Some(BadgePresentation::Compact),
                _ => None,
            }
        }
    }

    /// Directory the badge presentation is persisted in (under Application
    /// Support, keyed to the PoC bundle id — never touches prod Nice state).
    fn badge_state_dir() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        let mut p = PathBuf::from(home);
        p.push("Library/Application Support/dev.nickanderssohn.nice-poc");
        Some(p)
    }

    fn load_badge_presentation_from(dir: &Path) -> BadgePresentation {
        std::fs::read_to_string(dir.join("badge-presentation"))
            .ok()
            .and_then(|s| BadgePresentation::parse(&s))
            .unwrap_or(BadgePresentation::Full)
    }

    fn save_badge_presentation_to(dir: &Path, p: BadgePresentation) {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(dir.join("badge-presentation"), p.as_str());
    }

    fn load_badge_presentation() -> BadgePresentation {
        badge_state_dir()
            .map(|d| load_badge_presentation_from(&d))
            .unwrap_or(BadgePresentation::Full)
    }

    fn save_badge_presentation(p: BadgePresentation) {
        if let Some(dir) = badge_state_dir() {
            save_badge_presentation_to(&dir, p);
        }
    }

    /// Rolling terminal-output throughput tracker. Samples a cumulative byte
    /// counter over a ~1 s window (mach-tick timestamps, consistent with the
    /// rest of the harness) and reports the rate + whether output is currently
    /// arriving (for the active/idle style).
    struct ThroughputMeter {
        /// (mach tick, cumulative bytes) within the rolling window.
        samples: VecDeque<(u64, u64)>,
        /// Last cumulative byte count seen (to detect fresh output).
        last_bytes: u64,
        /// mach tick of the most recent byte increase; 0 = nothing yet.
        last_activity: u64,
        /// True once at least one sample has been taken.
        primed: bool,
    }

    impl ThroughputMeter {
        fn new() -> Self {
            ThroughputMeter {
                samples: VecDeque::new(),
                last_bytes: 0,
                last_activity: 0,
                primed: false,
            }
        }

        /// Record the cumulative byte counter at `now`, evicting samples that
        /// have aged out of the rolling window.
        fn sample(&mut self, now: u64, cumulative: u64) {
            if self.primed && cumulative > self.last_bytes {
                self.last_activity = now;
            }
            self.last_bytes = cumulative;
            self.primed = true;
            self.samples.push_back((now, cumulative));
            while self.samples.len() > 1 {
                let front = self.samples.front().unwrap().0;
                if harness::clock::ms_between(front, now) > BADGE_WINDOW_MS {
                    self.samples.pop_front();
                } else {
                    break;
                }
            }
        }

        /// Throughput in KB/s over the retained window (0 with < 2 samples or
        /// no byte delta).
        fn kb_per_sec(&self) -> f64 {
            if self.samples.len() < 2 {
                return 0.0;
            }
            let (t0, b0) = *self.samples.front().unwrap();
            let (t1, b1) = *self.samples.back().unwrap();
            let dt_s = harness::clock::ms_between(t0, t1) / 1000.0;
            if dt_s <= 0.0 {
                return 0.0;
            }
            (b1.saturating_sub(b0) as f64 / dt_s) / 1024.0
        }

        /// Output is "arriving" if the last byte increase was within `ms`.
        fn active_within(&self, now: u64, ms: f64) -> bool {
            self.last_activity != 0 && harness::clock::ms_between(self.last_activity, now) < ms
        }
    }

    /// Interactive keystroke-latency view: NO synthetic workload, NO RAF while
    /// idle — this view renders when the pty reader pings the wake channel
    /// (echo arrived) or the OS itself asks for a frame (open/resize/
    /// occlusion/activation). So the vendored-gpui "Draw" signpost is
    /// inherently damage-gated: one echo batch => one cx.notify() => one
    /// render => one Draw interval.
    ///
    /// The activity badge adds a bounded exception: for up to ~2.2 s after the
    /// last byte the render requests animation frames so the throughput figure
    /// decays and the badge dims; once silent it stops requesting frames and
    /// the view is fully demand-driven again (no timers when idle).
    ///
    /// Cursor/blink: this bin renders NO cursor at all (the snapshot paints
    /// only cell text + backgrounds) and owns NO timers — there are zero
    /// timer-driven redraws in this mode by construction.
    struct InteractiveView {
        pty: PtySession,
        focus_handle: FocusHandle,
        fonts: FontSet,
        frame: u64,
        keys_sent: u64,
        start_tick: u64,
        /// GPUI's NSView (from raw-window-handle), captured on first render.
        /// Used to kick AppKit's damage path per echo — see
        /// `kick_platform_display`. Main-thread only (gpui entities are).
        ns_view: *mut NSView,
        /// Rolling terminal-output throughput for the activity badge.
        throughput: ThroughputMeter,
        /// Full (dot + label) vs compact (dot-only) badge; persisted.
        badge: BadgePresentation,
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
            // Shared with the multi-session background windows (spike 8 fix):
            // marks the view + its backing CAMetalLayer as needing display.
            kick_view_display(self.ns_view);
        }

        /// Flip the badge between full and compact, persist the choice, and
        /// repaint. Wired to the badge's click handler.
        fn toggle_badge(&mut self, cx: &mut Context<Self>) {
            self.badge = self.badge.toggled();
            save_badge_presentation(self.badge);
            cx.notify();
            self.kick_platform_display();
        }

        /// Build the top chrome bar: a session label on the left and the
        /// activity badge on the right, styled to match the terminal below.
        fn chrome_bar(&self, now: u64, cx: &mut Context<Self>) -> impl IntoElement {
            let active = self.throughput.active_within(now, BADGE_IDLE_MS);
            let kbps = self.throughput.kb_per_sec().round() as u64;
            let dot_color = if active { BADGE_ACCENT } else { BADGE_IDLE_DOT };
            let text_color = if active {
                BADGE_ACCENT_TEXT
            } else {
                BADGE_IDLE_TEXT
            };

            let dot = div()
                .size(px(8.0))
                .rounded_full()
                .bg(rgb(dot_color));

            let mut badge = div()
                .id("activity-badge")
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .px(px(6.0))
                .py(px(2.0))
                .rounded_md()
                .cursor_pointer()
                .on_click(cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                    this.toggle_badge(cx);
                }))
                .child(dot);
            if self.badge == BadgePresentation::Full {
                badge = badge.child(
                    div()
                        .font_family("Menlo")
                        .text_size(px(11.0))
                        .text_color(rgb(text_color))
                        .whitespace_nowrap()
                        .child(SharedString::from(format!("{kbps} KB/s"))),
                );
            }

            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .h(px(CHROME_BAR_PX))
                .w_full()
                .flex_none()
                .px(px(10.0))
                .bg(rgb(CHROME_BAR_BG))
                .border_b_1()
                .border_color(rgb(CHROME_BAR_BORDER))
                .child(
                    div()
                        .font_family("Menlo")
                        .text_size(px(11.0))
                        .text_color(rgb(CHROME_LABEL))
                        .whitespace_nowrap()
                        .child(SharedString::from("pty: /bin/cat")),
                )
                .child(badge)
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

            // Sample terminal-output throughput (cumulative pty echo bytes)
            // for the activity badge.
            let now = harness::clock::now();
            self.throughput
                .sample(now, self.pty.bytes_echoed.load(Ordering::Relaxed));

            // While output is recent, keep requesting frames so the KB/s
            // figure decays and the badge crosses the idle threshold even
            // without further echoes. Past that window the view is fully
            // demand-driven again (no RAF, no timers) — the mode's invariant.
            if self.throughput.active_within(now, BADGE_TICK_MS) {
                window.request_animation_frame();
            }

            let chrome = self.chrome_bar(now, cx);
            let snap = snapshot(&self.pty.term.lock(), false);
            // frac = 0: no sub-pixel scroll animation (that path needs a
            // continuous redraw clock, which this mode deliberately lacks).
            div()
                .size_full()
                .flex()
                .flex_col()
                .track_focus(&self.focus_handle)
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, _cx| {
                    this.on_key(ev);
                }))
                .child(chrome)
                .child(div().flex_1().child(grid_canvas(
                    snap,
                    self.fonts.clone(),
                    0.0,
                    CanvasExtras::none(),
                )))
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
                size(
                    px(COLS as f32 * FONT_PX * 0.62),
                    // Grow the window by the chrome-bar height so the badge bar
                    // does not steal any grid rows.
                    px(ROWS as f32 * LINE_PX + 40.0 + CHROME_BAR_PX),
                ),
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

                        // The deadline must NOT live in render() (a demand-
                        // driven window may not render for long stretches) —
                        // and NOT on a gpui executor timer either: that
                        // starved live in the idle energy state (App Nap
                        // timer coalescing in a fully idle app; this mode
                        // only ever survived because real input kept the app
                        // un-napped). Shared guaranteed-fire watchdog thread
                        // instead (harness::watchdog).
                        let weak = cx.weak_entity();
                        let mut async_cx = cx.to_async();
                        harness::watchdog::arm(
                            Duration::from_secs_f64(deadline),
                            "gpui-term interactive",
                            move || {
                                let done = weak.update(
                                    &mut async_cx,
                                    |view: &mut InteractiveView, _| {
                                        view.finalize_and_exit("deadline (watchdog)")
                                    },
                                );
                                if done.is_err() {
                                    eprintln!(
                                        "[gpui-term interactive] watchdog: view entity \
                                         gone; exiting without a summary"
                                    );
                                    std::process::exit(2);
                                }
                            },
                        );

                        InteractiveView {
                            pty,
                            focus_handle: cx.focus_handle(),
                            fonts: FontSet::new("Menlo"),
                            frame: 0,
                            keys_sent: 0,
                            start_tick: 0,
                            ns_view: std::ptr::null_mut(),
                            throughput: ThroughputMeter::new(),
                            // Restore the persisted density (full vs compact).
                            badge: load_badge_presentation(),
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

    /// HEADLESS self-test for the activity-badge logic (no display): the
    /// full/compact toggle + persistence round trip, the throughput math, and
    /// the active/idle transition. Mirrors the file's other `run_headless_*`
    /// self-tests; wired to `NICE_POC_BADGE_SELFTEST`.
    pub fn run_badge_selftest() -> ! {
        eprintln!("{}", harness::banner());
        eprintln!("[gpui-term] HEADLESS activity-badge self-test (no display).");

        let mut ok = true;
        macro_rules! check {
            ($cond:expr, $msg:expr) => {{
                let c = $cond;
                eprintln!("  [{}] {}", if c { "PASS" } else { "FAIL" }, $msg);
                ok &= c;
            }};
        }

        // -- presentation toggle + string round trip --------------------------
        check!(
            BadgePresentation::Full.toggled() == BadgePresentation::Compact
                && BadgePresentation::Compact.toggled() == BadgePresentation::Full,
            "toggle flips full <-> compact"
        );
        check!(
            BadgePresentation::parse(BadgePresentation::Full.as_str())
                == Some(BadgePresentation::Full)
                && BadgePresentation::parse(BadgePresentation::Compact.as_str())
                    == Some(BadgePresentation::Compact),
            "as_str/parse round trip"
        );
        check!(BadgePresentation::parse("garbage").is_none(), "parse rejects garbage");

        // -- persistence round trip (temp dir; never the real state file) -----
        let dir = std::env::temp_dir().join(format!("nice-poc-badge-selftest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        check!(
            load_badge_presentation_from(&dir) == BadgePresentation::Full,
            "missing state file defaults to full"
        );
        save_badge_presentation_to(&dir, BadgePresentation::Compact);
        check!(
            load_badge_presentation_from(&dir) == BadgePresentation::Compact,
            "compact persists + reloads (survives relaunch)"
        );
        save_badge_presentation_to(&dir, BadgePresentation::Full);
        check!(
            load_badge_presentation_from(&dir) == BadgePresentation::Full,
            "toggled-back full persists + reloads"
        );
        let _ = std::fs::remove_dir_all(&dir);

        // -- throughput meter -------------------------------------------------
        let mut m = ThroughputMeter::new();
        let t0 = harness::clock::now();
        m.sample(t0, 0);
        check!(m.kb_per_sec() == 0.0, "single sample => 0 KB/s");
        check!(!m.active_within(t0, BADGE_IDLE_MS), "no bytes => idle");
        std::thread::sleep(Duration::from_millis(50));
        let t1 = harness::clock::now();
        m.sample(t1, 100 * 1024); // ~100 KiB over ~50 ms
        let kbps = m.kb_per_sec();
        check!(kbps > 0.0, "byte increase => positive KB/s");
        check!(m.active_within(t1, BADGE_IDLE_MS), "fresh bytes => active");
        // Activity is time-since-last-increase: a tiny threshold reads idle
        // while a generous one still reads active (the 2 s dim in miniature).
        std::thread::sleep(Duration::from_millis(30));
        let t2 = harness::clock::now();
        m.sample(t2, 100 * 1024); // same cumulative => no fresh output
        check!(!m.active_within(t2, 10.0), "silence beyond threshold => idle");
        check!(m.active_within(t2, 60_000.0), "still active within a generous window");
        eprintln!("  (measured burst throughput ~{kbps:.0} KB/s over the sampled window)");

        eprintln!(
            "RESULT: {}",
            if ok { "PASS (activity-badge logic)" } else { "FAIL" }
        );
        std::process::exit(if ok { 0 } else { 1 });
    }

    /// Run metadata persisted into the CSV as leading `#` comment lines
    /// (§13 harness fix: screen info / seed / build profile / flags), plus
    /// the per-sample memory series appended as `mem_phys` rows.
    struct CsvMeta {
        display: String,
        seed: u64,
        bps: usize,
        cfg: MultiCfg,
        tag: String,
        scrollback: usize,
        trace: Option<String>,
        mem_series: Vec<(f64, f64)>,
    }

    fn write_csv_header(f: &mut impl std::io::Write, meta: &CsvMeta) -> std::io::Result<()> {
        writeln!(f, "# gpui-term run metadata (parse rows below; lines starting '#' are comments)")?;
        writeln!(f, "# display={}", meta.display)?;
        writeln!(
            f,
            "# build={} seed={} bytes_per_sec={} windows={} streaming={} bg_bps={}",
            if cfg!(debug_assertions) { "debug" } else { "release" },
            meta.seed,
            meta.bps,
            meta.cfg.windows,
            meta.cfg.streaming,
            meta.cfg.bg_bps
        )?;
        writeln!(
            f,
            "# mode={} scrollback={} gpui_txn={} damage_only={} trace={}",
            meta.tag,
            meta.scrollback,
            env_flag("NICE_POC_GPUI_TXN") as u8,
            env_flag("NICE_POC_DAMAGE_ONLY") as u8,
            meta.trace.as_deref().unwrap_or("-")
        )?;
        writeln!(f, "metric,stack,phase,build,idx,value,unit")?;
        Ok(())
    }

    fn write_mem_rows(f: &mut impl std::io::Write, meta: &CsvMeta) -> std::io::Result<()> {
        for (idx, (secs, mib)) in meta.mem_series.iter().enumerate() {
            // value = phys_footprint MiB; the elapsed seconds ride in `phase`.
            writeln!(f, "mem_phys,gpui-native,{secs:.1}s,poc,{idx},{mib:.2},MiB")?;
        }
        Ok(())
    }

    /// Raw per-sample CSV (single-stack frame intervals + memory), §H.1-shaped.
    fn write_csv(
        path: &Path,
        streams: &harness::FrameStreams,
        meta: &CsvMeta,
    ) -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::File::create(path)?;
        write_csv_header(&mut f, meta)?;
        let ts = &streams.gpui_composite;
        for (idx, w) in ts.windows(2).enumerate() {
            let ms = harness::clock::ms_between(w[0], w[1]);
            writeln!(f, "frame_interval,gpui-native,load,poc,{idx},{ms},ms")?;
        }
        write_mem_rows(&mut f, meta)?;
        Ok(())
    }

    /// Multi-window raw CSV (spike 8): per-window frame intervals, same schema
    /// with the window index + role folded into the `stack` column
    /// (`gpui-native-w<N>-stream` / `gpui-native-w<N>-bg`).
    fn write_multi_csv(path: &Path, meta: &CsvMeta) -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::File::create(path)?;
        write_csv_header(&mut f, meta)?;
        for (w, slot) in win_slots().iter().enumerate() {
            let ts = slot.frames.lock().unwrap().clone();
            let kind = if slot.streaming { "stream" } else { "bg" };
            for (idx, win) in ts.windows(2).enumerate() {
                let ms = harness::clock::ms_between(win[0], win[1]);
                writeln!(f, "frame_interval,gpui-native-w{w}-{kind},load,poc,{idx},{ms},ms")?;
            }
        }
        write_mem_rows(&mut f, meta)?;
        Ok(())
    }

    pub fn run_live() {
        let mut spike = SpikeCfg::from_env();
        if let Err(e) = spike.load_trace() {
            eprintln!("[gpui-term] FATAL: {e}");
            std::process::exit(1);
        }
        let mut cfg = MultiCfg::from_env();
        if spike.energy_state.is_some() {
            // Energy states are single-window by definition (the powermetrics
            // three-state protocol).
            cfg = MultiCfg {
                windows: 1,
                streaming: 1,
                bg_bps: 0,
            };
        }
        let prof = WorkloadProfile::default();

        // Deadline: explicit NICE_POC_SECS wins; a finite (non-loop) trace
        // replay defaults to its own native duration (+3 s tail; the run
        // finalizes ~1 s after quiescent anyway); everything else keeps 18 s.
        let deadline = std::env::var("NICE_POC_SECS")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or_else(|| match &spike.trace {
                Some(t) if !spike.trace_loop && !spike.trace_drain => {
                    t.duration_secs() / spike.trace_speed + 3.0
                }
                Some(_) if spike.trace_drain => 60.0,
                _ => 18.0,
            });

        eprintln!(
            "[gpui-term] LIVE single-stack GPUI-native terminal: {} window(s) \
             ({} streaming, {} background @ {} B/s), ~{deadline:.0}s then auto-exit \
             with an FPS/memory summary. Spike flags: {}. (NICE_POC_SECS / \
             NICE_POC_WINDOWS / NICE_POC_STREAMING / NICE_POC_BG_BPS override; \
             spikes 6/7/9/10 env in the README.)",
            cfg.windows,
            cfg.streaming,
            cfg.windows - cfg.streaming,
            cfg.bg_bps,
            spike.tag().as_deref().unwrap_or("none"),
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
                let (spec, expect_done) = if spike.energy_state.is_some() {
                    (FeedSpec::Idle, false)
                } else if streaming {
                    match &spike.trace {
                        Some(t) => (
                            FeedSpec::Trace {
                                trace: Arc::clone(t),
                                speed: spike.trace_speed,
                                drain: spike.trace_drain,
                                loop_replay: spike.trace_loop,
                            },
                            !spike.trace_loop,
                        ),
                        None if spike.glyph_sweep => {
                            (FeedSpec::Sweep { bps: prof.bytes_per_sec }, false)
                        }
                        None => (FeedSpec::Synthetic { bps: prof.bytes_per_sec }, false),
                    }
                } else if cfg.bg_bps == 0 {
                    (FeedSpec::Heartbeat, false)
                } else {
                    (FeedSpec::Synthetic { bps: cfg.bg_bps }, false)
                };
                let prefill = if streaming { spike.prefill_lines } else { 0 };
                let session = Session::spawn_spec(
                    i,
                    prof.seed + i as u64,
                    spec,
                    spike.scrollback,
                    prefill,
                );
                slots.push(WinSlot {
                    streaming,
                    bps,
                    bytes_fed: Arc::clone(&session.bytes_fed),
                    frames: Mutex::new(Vec::new()),
                    feed_done: Arc::clone(&session.feed_done),
                    expect_done,
                });
                sessions.push(session);
            }
            let _ = WIN_SLOTS.set(slots);

            let win_size = size(
                px(COLS as f32 * FONT_PX * 0.62),
                px(ROWS as f32 * LINE_PX + 40.0),
            );

            for (i, session) in sessions.into_iter().enumerate() {
                // Energy `dot` renders via RAF (that's the point: one
                // animating chrome element = whole-scene repaint at refresh);
                // `idle` is fully demand-driven (no RAF, ~no draws).
                let streaming = match spike.energy_state {
                    Some(EnergyState::Idle) => false,
                    Some(EnergyState::Dot) => true,
                    None => i < cfg.streaming,
                };
                let bps = if i < cfg.streaming { prof.bytes_per_sec } else { cfg.bg_bps };
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
                let spike_i = spike.clone();
                let deadline_i = deadline;
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
                                // feeder's dirty flag into cx.notify() + a
                                // platform present kick (spike 8 fix: notify
                                // alone rebuilds the scene but never PRESENTS
                                // when the window's display link is stopped).
                                let dirty = Arc::clone(&session.dirty);
                                let executor = cx.background_executor().clone();
                                cx.spawn(async move |this, cx| {
                                    loop {
                                        executor.timer(Duration::from_millis(100)).await;
                                        if dirty.swap(false, Ordering::AcqRel)
                                            && this
                                                .update(cx, |view: &mut TermView, cx| {
                                                    cx.notify();
                                                    view.kick_platform_display();
                                                })
                                                .is_err()
                                        {
                                            break;
                                        }
                                    }
                                })
                                .detach();
                            }
                            if i == 0 {
                                // GUARANTEED auto-exit for EVERY live mode
                                // (2026-07-02 hang fix): the render-path
                                // deadline only runs while frames tick, and a
                                // gpui executor timer starved live in the
                                // fully idle window (App Nap coalescing — see
                                // harness::watchdog). The watchdog thread
                                // cannot starve; streaming runs normally exit
                                // via the render path first (+3 s grace here).
                                let weak = cx.weak_entity();
                                let mut async_cx = cx.to_async();
                                harness::watchdog::arm(
                                    Duration::from_secs_f64(deadline_i + 3.0),
                                    "gpui-term",
                                    move || {
                                        let done = weak.update(
                                            &mut async_cx,
                                            |view: &mut TermView, _| {
                                                view.finalize_and_exit("deadline (watchdog)")
                                            },
                                        );
                                        if done.is_err() {
                                            eprintln!(
                                                "[gpui-term] watchdog: coordinator entity \
                                                 gone; exiting without a summary"
                                            );
                                            std::process::exit(2);
                                        }
                                    },
                                );
                            }
                            TermView::new(
                                i,
                                streaming,
                                cfg,
                                spike_i,
                                session,
                                deadline_i,
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
        (false, false) => {
            // Headless self-tests for the new spike machinery (no display).
            if env_flag("NICE_POC_BADGE_SELFTEST") {
                gui::run_badge_selftest()
            } else if env_flag("NICE_POC_WATCHDOG_SELFTEST") {
                run_headless_watchdog()
            } else if env_flag("NICE_POC_SPIKE9") {
                run_headless_spike9()
            } else if env_flag("NICE_POC_ATLAS") || env_flag("NICE_POC_GLYPH_SWEEP") {
                run_headless_atlas()
            } else if std::env::var("NICE_POC_TRACE").is_ok() {
                run_headless_trace()
            } else {
                run_headless()
            }
        }
    }
}
