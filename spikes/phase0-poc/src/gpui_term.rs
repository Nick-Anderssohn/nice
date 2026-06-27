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

#![allow(dead_code)]

#[path = "harness.rs"]
mod harness;

use std::path::Path;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point as TermPoint};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
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

// =========================================================================
// LIVE GPUI run.
// =========================================================================

mod gui {
    use super::*;
    use gpui::{
        canvas, fill, font, point, px, rgb, size, App, Application, Bounds, Context, Font, Hsla,
        AppContext, IntoElement, Pixels, Render, SharedString, Styled, TextRun, TitlebarOptions, Window,
        WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
    };

    struct TermView {
        term: Term<EventProxy>,
        parser: Processor,
        wl: Workload,
        per_frame_bytes: usize,
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
        fn new(deadline_secs: f64) -> Self {
            let size = Size { rows: ROWS, cols: COLS };
            let term = Term::new(Config::default(), &size, EventProxy);
            let prof = WorkloadProfile::default();
            let wl = Workload::new(prof);
            Self {
                term,
                parser: Processor::new(),
                wl,
                per_frame_bytes: (prof.bytes_per_sec / 60).max(64),
                base_font: font("Menlo"),
                frame: 0,
                start_tick: 0,
                deadline_secs,
                mem_idle_mib: 0.0,
                mem_steady_mib: 0.0,
                mem_peak_mib: 0.0,
                seed: prof.seed,
                bps: prof.bytes_per_sec,
            }
        }

        fn elapsed_secs(&self) -> f64 {
            if self.start_tick == 0 {
                0.0
            } else {
                harness::clock::ms_between(self.start_tick, harness::clock::now()) / 1000.0
            }
        }

        fn pump(&mut self) {
            let chunk = self.wl.stream(self.per_frame_bytes);
            self.parser.advance(&mut self.term, &chunk);
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

            let scheme = "gpui-native-single-stack";
            let csv = format!("./gpui-term-{scheme}.csv");
            let _ = write_csv(Path::new(&csv), &streams);

            eprintln!("\n================ gpui-term LIVE RESULT ({reason}) ================");
            eprintln!("architecture : Path B — single GPUI Metal stack, alacritty_terminal VT core,");
            eprintln!("               rendered via public shape_line().paint() + paint_quad()");
            eprintln!("workload     : synthetic Claude-stream, seed={} ~{} B/s, {}x{} grid", self.seed, self.bps, COLS, ROWS);
            eprintln!("duration     : {:.1} s, {} composited frames", self.elapsed_secs(), g.samples);
            eprintln!("-- frame interval (single stack = terminal present) --");
            eprintln!(
                "  p50 {:.2} ms ({:.1} fps) | p95 {:.2} ms | p99 {:.2} ms | cliffs>16.6ms {}",
                g.p50_ms, g.fps_p50, g.p95_ms, g.p99_ms, g.cliffs
            );
            eprintln!("-- memory (phys_footprint) --");
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
            harness::stamp_gpui_frame();
            self.frame += 1;

            if self.frame == 1 {
                // Capture an idle baseline, then clear the warm-up frame so the
                // measured window starts clean.
                let (phys, _) = harness::mem::sample();
                self.mem_idle_mib = harness::mem::mib(phys);
                self.mem_peak_mib = self.mem_idle_mib;
                self.start_tick = harness::clock::now();
                harness::reset_frame_streams();
            }

            self.pump();
            self.sample_mem();

            if self.elapsed_secs() >= self.deadline_secs {
                self.finalize_and_exit("measurement window elapsed");
            }

            // Snapshot the grid (owned) so the 'static paint closure can render
            // it without borrowing `self`.
            let snap = snapshot(&self.term);
            let base = self.base_font.clone();
            // Animated sub-pixel vertical offset → exercises fractional glyph
            // placement + full re-paint every frame (the sub-line scroll path).
            let frac = (self.frame as f32 * 0.7) % LINE_PX;

            // Keep compositing continuously — this RAF is the measurement clock.
            window.request_animation_frame();

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
    }

    fn rgb_to_hsla(v: u32) -> Hsla {
        rgb(v).into()
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

    pub fn run_live() {
        let deadline = std::env::var("NICE_POC_SECS")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(18.0);

        eprintln!(
            "[gpui-term] LIVE single-stack GPUI-native terminal: streaming ~{deadline:.0}s then \
             auto-exit with an FPS/memory summary. (NICE_POC_SECS overrides.)"
        );

        Application::new().run(move |cx: &mut App| {
            cx.activate(true);
            cx.on_window_closed(|_cx| std::process::exit(0)).detach();

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
                        title: Some("Nice Phase-0 — GPUI-native terminal (Path B)".into()),
                        appears_transparent: false,
                        traffic_light_position: None,
                    }),
                    kind: WindowKind::Normal,
                    is_resizable: true,
                    ..Default::default()
                },
                |_window, cx| cx.new(|_cx| TermView::new(deadline)),
            )
            .unwrap();
        });
    }
}

fn main() {
    let live = matches!(std::env::var("NICE_POC_RUN").as_deref(), Ok("1") | Ok("true"));
    if live {
        gui::run_live();
    } else {
        run_headless();
    }
}
