//! AA/gamma spike (rank-1) — GPUI-native terminal scene on CURRENT GPUI main.
//!
//! Renders the deterministic AA/gamma test scene (scene.bin, produced by
//! ../scene/gen_scene.py) through GPUI's REAL mac Metal pipeline at a pinned
//! zed git rev, reads the pixels back via GPUI's own first-party visual-test
//! capture (`VisualTestAppContext::capture_screenshot` → `MetalRenderer::
//! render_to_image`, which draws the scene into the layer's next drawable with
//! the production shaders/blend and `get_bytes()`s it — exactly the
//! "framebufferOnly=false + readback" this spike calls for, no swizzling
//! needed), writes PNG + meta.json, and exits. No pty, no shell: the scene
//! bytes are fed straight into the alacritty_terminal parser.
//!
//! REQUIRES A DISPLAY (opens one real window). Never run from a sandboxed
//! subagent; the main session runs it per aa-gamma/RUNBOOK.md.
//!
//! Pinned zed rev: 10b07951838e422722e34641f4a9c0bfec9037ff (main, 2026-07-01).
//!
//! Axes (CLI):
//!   --theme light|dark        Nice's built-in default terminal themes
//!   --smoothing off|on        macOS AppleFontSmoothing pref, read by GPUI
//!                             main's new fg-luminance dilation path
//!                             (gpui_macos/src/text_system.rs:218-253).
//!                             off = Nice parity (Nice ships fontSmoothing=false);
//!                             on  = GPUI-main out-of-the-box default.
//!   --scene PATH --out DIR    scene bytes in, PNGs out
//!
//! Font: both sides of the A/B load the SAME font file —
//! /System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts/
//! SF-Mono-Regular.otf (family "SF Mono", ps SFMono-Regular) — the first hit
//! of Nice's shipping font chain (TabPtySession.terminalFont: SFMono-Regular
//! 13pt). Cell geometry replicates SwiftTerm's computeFontDimensions exactly:
//! w = ceil(advance('W')*scale)/scale = 8.5pt, h = ceil(asc+desc+leading) =
//! 16pt @2x (probed; overridable via --cell-w/--cell-h).

use std::io::Read;
use std::path::PathBuf;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point as TermPoint};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};

// ---- Nice's shipping terminal themes ---------------------------------------
// Source: Sources/Nice/Theme/BuiltInTerminalThemes.swift (niceDefaultLight /
// niceDefaultDark). ansi[16] in standard order (0-7 normal, 8-15 bright).

struct Palette {
    bg: u32,
    fg: u32,
    ansi: [u32; 16],
}

const NICE_LIGHT: Palette = Palette {
    bg: 0xfffcfc,
    fg: 0x17130f,
    ansi: [
        0x17130f, 0xb74020, 0x308130, 0xa6710d, 0x2860af, 0x9b3b98, 0x23859b, 0x7e766c,
        0x5c5348, 0xd44c25, 0x389f38, 0xc48c18, 0x3475cd, 0xb547af, 0x289cb2, 0x17130f,
    ],
};

const NICE_DARK: Palette = Palette {
    bg: 0x090705,
    fg: 0xf4f0ef,
    ansi: [
        0x090705, 0xc23621, 0x25bc24, 0xadad27, 0x496ee1, 0xd338d3, 0x33bbc8, 0xcbcccd,
        0x818383, 0xfc5b47, 0x31e722, 0xead423, 0x6c8dff, 0xf965f8, 0x64e6e6, 0xf4f0ef,
    ],
};

const FONT_REGULAR: &str =
    "/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts/SF-Mono-Regular.otf";
const FONT_BOLD: &str =
    "/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts/SF-Mono-Bold.otf";

// ---- alacritty scene state --------------------------------------------------

#[derive(Clone, Copy)]
struct TermSize {
    rows: usize,
    cols: usize,
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

#[derive(Clone)]
struct EventProxy;
impl EventListener for EventProxy {
    fn send_event(&self, _event: Event) {}
}

#[derive(Clone)]
struct CellSnap {
    ch: char,
    fg: u32,
    bg: Option<u32>, // Some(..) when != default bg
    bold: bool,
    underline: bool,
}

fn xterm256(i: u8, pal: &Palette) -> u32 {
    match i {
        0..=15 => pal.ansi[i as usize],
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

fn color_rgb(c: Color, pal: &Palette, is_fg: bool) -> u32 {
    match c {
        Color::Spec(rgb) => ((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | (rgb.b as u32),
        Color::Indexed(i) => xterm256(i, pal),
        Color::Named(n) => match n {
            NamedColor::Foreground => pal.fg,
            NamedColor::Background => pal.bg,
            NamedColor::Black => pal.ansi[0],
            NamedColor::Red => pal.ansi[1],
            NamedColor::Green => pal.ansi[2],
            NamedColor::Yellow => pal.ansi[3],
            NamedColor::Blue => pal.ansi[4],
            NamedColor::Magenta => pal.ansi[5],
            NamedColor::Cyan => pal.ansi[6],
            NamedColor::White => pal.ansi[7],
            NamedColor::BrightBlack => pal.ansi[8],
            NamedColor::BrightRed => pal.ansi[9],
            NamedColor::BrightGreen => pal.ansi[10],
            NamedColor::BrightYellow => pal.ansi[11],
            NamedColor::BrightBlue => pal.ansi[12],
            NamedColor::BrightMagenta => pal.ansi[13],
            NamedColor::BrightCyan => pal.ansi[14],
            NamedColor::BrightWhite => pal.ansi[15],
            _ => {
                if is_fg {
                    pal.fg
                } else {
                    pal.bg
                }
            }
        },
    }
}

fn snapshot(term: &Term<EventProxy>, pal: &Palette) -> Vec<Vec<CellSnap>> {
    let rows = term.screen_lines();
    let cols = term.columns();
    let mut out = Vec::with_capacity(rows);
    for line in 0..rows {
        let mut row = Vec::with_capacity(cols);
        for col in 0..cols {
            let cell = &term.grid()[TermPoint::new(Line(line as i32), Column(col))];
            let mut fg = color_rgb(cell.fg, pal, true);
            let mut bg = color_rgb(cell.bg, pal, false);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let ch = if cell.c == '\0' { ' ' } else { cell.c };
            row.push(CellSnap {
                ch,
                fg,
                bg: if bg != pal.bg { Some(bg) } else { None },
                bold: cell.flags.contains(Flags::BOLD),
                underline: cell.flags.contains(Flags::UNDERLINE),
            });
        }
        out.push(row);
    }
    out
}

// ---- AppleFontSmoothing pref (GPUI main's dilation gate) --------------------
// gpui_macos::text_system::font_smoothing_allowed_by_user() reads the
// AppleFontSmoothing pref for the current application ONCE (OnceLock). Only an
// explicit 0 disables the new default-on fg-luminance smoothing dilation. For
// an unbundled binary the "application" domain is the process name, so this
// writes ~/Library/Preferences/gpui-term-main.plist (harmless; runbook notes
// cleanup). Must run before the first glyph is rasterized.

fn set_apple_font_smoothing(enabled: bool) {
    use core_foundation::base::TCFType;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use core_foundation_sys::preferences::{
        kCFPreferencesCurrentApplication, CFPreferencesAppSynchronize, CFPreferencesSetAppValue,
    };
    let key = CFString::new("AppleFontSmoothing");
    let value = CFNumber::from(if enabled { 2i64 } else { 0i64 });
    unsafe {
        CFPreferencesSetAppValue(
            key.as_concrete_TypeRef(),
            value.as_CFTypeRef(),
            kCFPreferencesCurrentApplication,
        );
        CFPreferencesAppSynchronize(kCFPreferencesCurrentApplication);
    }
}

// ---- args -------------------------------------------------------------------

struct Args {
    scene: PathBuf,
    out: PathBuf,
    theme: String,     // "light" | "dark"
    smoothing: String, // "off" | "on"
    font_family: String,
    font_px: f32,
    cell_w: f32,
    cell_h: f32,
    cols: usize,
    rows: usize,
}

fn parse_args() -> Args {
    let mut a = Args {
        scene: PathBuf::new(),
        out: PathBuf::new(),
        theme: "light".into(),
        smoothing: "off".into(),
        font_family: "SF Mono".into(),
        font_px: 13.0,
        cell_w: 8.5,
        cell_h: 16.0,
        cols: 60,
        rows: 16,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| panic!("missing value for {k}"));
        match k.as_str() {
            "--scene" => a.scene = PathBuf::from(val()),
            "--out" => a.out = PathBuf::from(val()),
            "--theme" => a.theme = val(),
            "--smoothing" => a.smoothing = val(),
            "--font-family" => a.font_family = val(),
            "--font-px" => a.font_px = val().parse().unwrap(),
            "--cell-w" => a.cell_w = val().parse().unwrap(),
            "--cell-h" => a.cell_h = val().parse().unwrap(),
            "--cols" => a.cols = val().parse().unwrap(),
            "--rows" => a.rows = val().parse().unwrap(),
            other => panic!("unknown arg: {other}"),
        }
    }
    assert!(
        !a.scene.as_os_str().is_empty() && !a.out.as_os_str().is_empty(),
        "usage: gpui-term-main --scene scene.bin --out DIR --theme light|dark [--smoothing off|on]"
    );
    assert!(a.theme == "light" || a.theme == "dark", "--theme light|dark");
    assert!(
        a.smoothing == "off" || a.smoothing == "on",
        "--smoothing off|on"
    );
    a
}

// ---- GPUI scene view ---------------------------------------------------------

use gpui::{
    canvas, fill, point, px, rgb, size, App, AppContext, Bounds, Context, Font, FontFeatures,
    FontStyle, FontWeight, IntoElement, Pixels, Render, SharedString, Styled, TextAlign, TextRun,
    VisualTestAppContext, Window, WindowBounds, WindowOptions,
};

const INSET: f32 = 10.0; // pt inset of the grid from the canvas origin

/// f32 bits of the shaped advance of 'W' at font_px, written by the paint
/// closure on the first frame (shape_line lives on the window's text system).
static ADVANCE_W_BITS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

struct SceneView {
    grid: std::sync::Arc<Vec<Vec<CellSnap>>>,
    bg: u32,
    font_family: SharedString,
    font_px: f32,
    cell_w: f32,
    cell_h: f32,
}

fn scene_font(family: SharedString, bold: bool) -> Font {
    Font {
        family,
        features: FontFeatures::default(),
        weight: if bold {
            FontWeight::BOLD
        } else {
            FontWeight::NORMAL
        },
        style: FontStyle::Normal,
        fallbacks: None,
    }
}

impl Render for SceneView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let grid = self.grid.clone();
        let bg = self.bg;
        let family = self.font_family.clone();
        let font_px = self.font_px;
        let cell_w = self.cell_w;
        let cell_h = self.cell_h;

        canvas(
            move |_bounds, _window, _cx| {},
            move |bounds: Bounds<Pixels>, _state, window: &mut Window, cx: &mut App| {
                // Whole-canvas background = theme bg (padding around the grid
                // is diff-neutral on both sides).
                window.paint_quad(fill(bounds, rgb(bg)));

                // Font-identity probe: SF Mono advance('W') @13px ≈ 8.0361.
                let probe = window.text_system().shape_line(
                    SharedString::new_static("W"),
                    px(font_px),
                    &[TextRun {
                        len: 1,
                        font: scene_font(family.clone(), false),
                        color: gpui::black(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    None,
                );
                let w: f32 = probe.width.into();
                ADVANCE_W_BITS.store(w.to_bits(), std::sync::atomic::Ordering::Relaxed);

                let ox = bounds.origin.x + px(INSET);
                let oy = bounds.origin.y + px(INSET);

                for (r, row) in grid.iter().enumerate() {
                    let y = oy + px(r as f32 * cell_h);

                    // Cell backgrounds first (coalesce same-color runs).
                    let mut col = 0usize;
                    while col < row.len() {
                        if let Some(bgc) = row[col].bg {
                            let start = col;
                            while col < row.len() && row[col].bg == Some(bgc) {
                                col += 1;
                            }
                            let x = ox + px(start as f32 * cell_w);
                            let w = px((col - start) as f32 * cell_w);
                            window.paint_quad(fill(
                                Bounds {
                                    origin: point(x, y),
                                    size: size(w, px(cell_h)),
                                },
                                rgb(bgc),
                            ));
                        } else {
                            col += 1;
                        }
                    }

                    // Glyphs: one shape_line per cell, painted at the exact
                    // cell origin — mirrors SwiftTerm's per-cell quad placement
                    // at its snapped cell pitch (fractional advances do NOT
                    // accumulate on either side).
                    for (c, cell) in row.iter().enumerate() {
                        if cell.ch == ' ' && !cell.underline {
                            continue;
                        }
                        let mut buf = [0u8; 4];
                        let s: &str = cell.ch.encode_utf8(&mut buf);
                        let text: SharedString = SharedString::new(s.to_string());
                        let run = TextRun {
                            len: s.len(),
                            font: scene_font(family.clone(), cell.bold),
                            color: rgb(cell.fg).into(),
                            background_color: None,
                            underline: if cell.underline {
                                Some(gpui::UnderlineStyle {
                                    thickness: px(1.0),
                                    color: Some(rgb(cell.fg).into()),
                                    wavy: false,
                                })
                            } else {
                                None
                            },
                            strikethrough: None,
                        };
                        let shaped =
                            window
                                .text_system()
                                .shape_line(text, px(font_px), &[run], None);
                        let x = ox + px(c as f32 * cell_w);
                        shaped
                            .paint(point(x, y), px(cell_h), TextAlign::Left, None, window, cx)
                            .unwrap();
                    }
                }
            },
        )
        .size_full()
    }
}

// ---- main --------------------------------------------------------------------

fn main() {
    let args = parse_args();

    // Must precede the first rasterization: gpui caches the pref in a OnceLock.
    set_apple_font_smoothing(args.smoothing == "on");

    // Scene bytes -> alacritty grid.
    let mut scene_bytes = Vec::new();
    std::fs::File::open(&args.scene)
        .expect("open scene")
        .read_to_end(&mut scene_bytes)
        .expect("read scene");

    let pal = if args.theme == "dark" {
        &NICE_DARK
    } else {
        &NICE_LIGHT
    };

    let sz = TermSize {
        rows: args.rows,
        cols: args.cols,
    };
    let mut term = Term::new(Config::default(), &sz, EventProxy);
    let mut parser: Processor = Processor::new();
    parser.advance(&mut term, &scene_bytes);
    let grid = std::sync::Arc::new(snapshot(&term, pal));

    std::fs::create_dir_all(&args.out).expect("create out dir");

    // GPUI visual-test context on the REAL mac platform (Metal pipeline).
    let mut cx = VisualTestAppContext::new(gpui_platform::current_platform(false));

    // Load the exact same font files the SwiftTerm fixture registers.
    let mut font_bytes: Vec<std::borrow::Cow<'static, [u8]>> = Vec::new();
    for p in [FONT_REGULAR, FONT_BOLD] {
        match std::fs::read(p) {
            Ok(b) => font_bytes.push(std::borrow::Cow::Owned(b)),
            Err(e) => eprintln!("[gpui-term-main] WARN: cannot read {p}: {e}"),
        }
    }
    cx.text_system()
        .add_fonts(font_bytes)
        .expect("add SF Mono fonts");

    // Window sized grid + margins; extra bottom slack in case window chrome
    // steals content height. The grid draws at a fixed INSET from the top-left
    // of the root canvas; the diff tool aligns by cross-correlation anyway.
    let win_w = args.cols as f32 * args.cell_w + 2.0 * INSET;
    let win_h = args.rows as f32 * args.cell_h + INSET + 50.0;

    let grid_for_view = grid.clone();
    let family: SharedString = SharedString::new(args.font_family.clone());
    let (bg, font_px, cell_w, cell_h) = (pal.bg, args.font_px, args.cell_w, args.cell_h);

    let window = cx
        .update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(100.0), px(100.0)),
                        size: size(px(win_w), px(win_h)),
                    })),
                    focus: false,
                    show: true,
                    ..Default::default()
                },
                |_window, cx| {
                    cx.new(|_cx| SceneView {
                        grid: grid_for_view,
                        bg,
                        font_family: family,
                        font_px,
                        cell_w,
                        cell_h,
                    })
                },
            )
        })
        .expect("open window");

    cx.run_until_parked();
    let scale = cx
        .update_window(window.into(), |_, window, _| {
            window.refresh();
            window.scale_factor()
        })
        .expect("refresh window");
    cx.run_until_parked();

    let img = cx
        .capture_screenshot(window.into())
        .expect("capture screenshot");

    // Written by the paint closure on the first painted frame.
    let advance_w = f32::from_bits(ADVANCE_W_BITS.load(std::sync::atomic::Ordering::Relaxed));
    eprintln!(
        "[gpui-term-main] font '{}' @{}px: advance('W') = {:.4} px (SF Mono expects ~8.0361)",
        args.font_family, args.font_px, advance_w
    );

    let label = format!("gpui-main-{}-smoothing-{}", args.theme, args.smoothing);
    let png_path = args.out.join(format!("{label}.png"));
    write_png(&png_path, img.width(), img.height(), img.as_raw());

    let meta_path = args.out.join(format!("{label}.meta.json"));
    let meta = format!(
        "{{\n  \"side\": \"gpui-main\",\n  \"zed_rev\": \"10b07951838e422722e34641f4a9c0bfec9037ff\",\n  \"theme\": \"{}\",\n  \"smoothing\": \"{}\",\n  \"font_family\": \"{}\",\n  \"font_px\": {},\n  \"cell_w_pt\": {},\n  \"cell_h_pt\": {},\n  \"cols\": {},\n  \"rows\": {},\n  \"inset_pt\": {},\n  \"scale_factor\": {},\n  \"advance_w_px\": {:.4},\n  \"image_w\": {},\n  \"image_h\": {},\n  \"bg\": \"#{:06x}\",\n  \"fg\": \"#{:06x}\"\n}}\n",
        args.theme,
        args.smoothing,
        args.font_family,
        args.font_px,
        args.cell_w,
        args.cell_h,
        args.cols,
        args.rows,
        INSET,
        scale,
        advance_w,
        img.width(),
        img.height(),
        pal.bg,
        pal.fg,
    );
    std::fs::write(&meta_path, meta).expect("write meta");

    eprintln!(
        "[gpui-term-main] wrote {} ({}x{} @ scale {}) and {}",
        png_path.display(),
        img.width(),
        img.height(),
        scale,
        meta_path.display()
    );
    std::process::exit(0);
}

/// Encode RGBA with our own png dep (gpui's `image` re-export may lack the
/// png encoder feature).
fn write_png(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) {
    let f = std::fs::File::create(path).expect("create png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().expect("png header");
    w.write_image_data(rgba).expect("png data");
}
