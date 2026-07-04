//! `TerminalElement` — the low-level paint element for one terminal frame.
//!
//! It is built from a brief, owned snapshot of the [`TerminalSessionHandle`]'s
//! grid (locked, copied, unlocked — the `Term` `FairMutex` is **never** held
//! across a paint) and drawn through gpui's public `canvas` paint API, exactly
//! the shape proven in the phase-0 aa-gamma spike
//! (`spikes/phase0-poc/aa-gamma/gpui-term-main/src/main.rs`):
//!
//! * whole-viewport background fill, then per-row coalesced **background quads**
//!   (`paint_quad`) for every cell whose resolved background differs from the
//!   theme default;
//! * per-cell **foreground glyph runs** (`shape_line().paint()`), each carrying
//!   `background_color` so the patched bg-luminance composition curve engages
//!   (the whole reason Path B was gated on this renderer);
//! * a **block cursor** in the accent color — solid when focused, hollow when
//!   not.
//!
//! It covers the full per-cell paint model: the color model — 16 themed ANSI,
//! 256 computed cube/ramp, 24-bit truecolor (see [`crate::color`]) — plus text
//! attributes (inverse-video with exact per-channel inversion, bold, italic,
//! dim, underline, strikethrough), wide glyphs / emoji, selection rendering from
//! the core's selection state, and procedural box-drawing + block elements
//! (U+2500–259F, see [`crate::boxdraw`]).
//!
//! ## Row-quantized, bottom-anchored layout (T4)
//!
//! The grid is **anchored to the bottom** of the element bounds (minus a stable
//! [`TERMINAL_BOTTOM_GAP`]), so the bottom row's baseline is pinned regardless of
//! the view height — during a live resize the prompt line never jitters (the
//! origin is *computed* from `bounds`, not remembered). Any sub-row remainder
//! falls at the **top** of the view, where it is clipped by a content mask
//! (`with_content_mask`) so a grid taller than the view loses its topmost rows
//! under the chrome, exactly like `TerminalContainerView` (Nice's Swift host).
//! Scroll offset is read from the core's display offset (line-quantized; the
//! `TerminalSessionHandle` owns the wheel/trackpad stepping).

use std::cell::Cell;
use std::rc::Rc;

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point as GridPoint};
use alacritty_terminal::selection::SelectionRange;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor};

use gpui::{
    canvas, fill, point, prelude::*, px, rgb, size, App, Bounds, Canvas, ContentMask, Entity,
    FocusHandle, Font, FontFeatures, FontStyle, FontWeight, Hsla, PathBuilder, Pixels, Rgba,
    SharedString, StrikethroughStyle, TextAlign, TextRun, UnderlineStyle, Window,
};

use nice_theme::Srgba;

use crate::boxdraw::{self, apple_approx_coverage, Segment};
use crate::color::resolve_color;
use crate::input::TermInputHandler;
use crate::session_handle::TerminalSessionHandle;
use crate::theme::TerminalTheme;
use crate::view::TerminalView;

/// The R5 IME wiring the view hands to the element each frame: the focus handle
/// + view entity the platform [`TermInputHandler`] is registered against during
/// paint, plus the current preedit to paint inline at the grid cursor.
///
/// Threading this through the element (rather than a separate overlay) keeps the
/// input-handler registration and the marked-text overlay on the exact grid
/// geometry the element already computes — the candidate anchor
/// (`bounds_for_range`) and the painted preedit then agree by construction.
pub struct ImeInput {
    /// The view's focus handle — the input handler is active only while it holds
    /// focus (`window.handle_input`).
    pub focus_handle: FocusHandle,
    /// The view whose IME state the handler reads/drives.
    pub view: Entity<TerminalView>,
    /// `Some((preedit_text, selected_byte_range))` while composing; drives the
    /// inline underline overlay + block-cursor suppression at the cursor cell.
    pub preedit: Option<(SharedString, std::ops::Range<usize>)>,
}

/// Default selection tint used when the theme carries no `selection` colour
/// (a neutral mid-grey). Themes ship an explicit value; this only guards the
/// `None` case so selection is always visible.
const DEFAULT_SELECTION: u32 = 0x3a3a3a;

/// Constant gap kept below the last grid row, in logical px — the bottom-anchor
/// inset. Mirrors `TerminalContainerView.bottomInset` (Nice ships `0`: the grid
/// sits flush with the content area's bottom). Exposed so a scenario can predict
/// the pinned bottom-row position; bump it for a breathing-room inset.
pub const TERMINAL_BOTTOM_GAP: f32 = 0.0;

/// Underline / strikethrough decoration thickness, logical px (a hairline that
/// reads at 13pt; the exact vertical position comes from gpui's font metrics).
const DECORATION_THICKNESS: f32 = 1.0;

/// Cell geometry in logical `px`. Font resolution / zoom is R7; this slice
/// takes the cell box as a fixed input (the caller sets it to match its font).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalMetrics {
    /// Cell advance width, logical px.
    pub cell_w: f32,
    /// Cell (line) height, logical px.
    pub cell_h: f32,
}

impl TerminalMetrics {
    /// Construct metrics from an explicit cell box.
    pub const fn new(cell_w: f32, cell_h: f32) -> Self {
        Self { cell_w, cell_h }
    }
}

/// One cell's fully-resolved paint data: the glyph, its resolved fg (after
/// inverse-video / dim), an optional non-default bg (after inverse-video /
/// selection), and the text attributes that affect the run.
#[derive(Clone, Copy)]
struct PaintCell {
    ch: char,
    fg: u32,
    /// `Some` iff the resolved background differs from the theme default (an
    /// explicit quad is painted only then — matching the spike's coalescing).
    bg: Option<u32>,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    /// The trailing half of a wide glyph (`WIDE_CHAR_SPACER`). Its background is
    /// still painted (it shares the lead cell's bg / selection), but its glyph is
    /// **not** — the lead cell's wide glyph already spans both columns.
    wide_spacer: bool,
}

/// Where + how to paint the block cursor.
#[derive(Clone, Copy)]
struct CursorPaint {
    row: usize,
    col: usize,
    /// Focused caret is a solid block; unfocused is a hollow outline.
    solid: bool,
}

/// A paint-ready snapshot of one terminal frame. Owns everything it draws, so
/// the `Term` lock is released before this is handed to the paint pipeline.
pub struct TerminalElement {
    rows: Vec<Vec<PaintCell>>,
    default_bg: u32,
    /// The theme foreground, used as the inline preedit (marked-text) glyph color.
    foreground: u32,
    cursor: Option<CursorPaint>,
    accent: Rgba,
    font_family: SharedString,
    font_px: f32,
    metrics: TerminalMetrics,
    /// The R5 IME wiring: input-handler registration + inline preedit paint.
    ime: ImeInput,
    /// The cell the view reads for pixel→cell hit-testing in its mouse handlers.
    /// Paint writes this frame's grid `bounds` into it (the same `bounds` the
    /// candidate anchor and `grid_top_y` use), so the next mouse event hit-tests
    /// against exactly what was painted. `Rc<Cell>` (not an entity re-borrow) so
    /// paint never re-enters the view — see [`crate::view::TerminalView`].
    paint_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

/// Y (logical px) of the top of grid row 0 under the bottom-anchored layout (T4)
/// for element `bounds` holding `rows` grid rows. The grid's bottom edge is
/// pinned at `bounds.bottom − TERMINAL_BOTTOM_GAP` and the top origin derived, so
/// the value can be negative (grid taller than the view). Shared with the view's
/// `bounds_for_range` anchor so the candidate window lands where the row paints.
pub fn grid_top_y(bounds: Bounds<Pixels>, metrics: TerminalMetrics, rows: usize) -> f32 {
    let grid_h = rows as f32 * metrics.cell_h;
    f32::from(bounds.origin.y) + f32::from(bounds.size.height) - TERMINAL_BOTTOM_GAP - grid_h
}

impl TerminalElement {
    /// Build the frame snapshot from `handle`'s session grid.
    ///
    /// Locks the shared `Term` only long enough to copy the visible cells +
    /// cursor into owned data, then releases it. `caret_solid` is the focus
    /// verdict the view computes (`is_focused && window active`) — passed in so
    /// this crate keeps no separate focus flag. If the session has not spawned,
    /// the element paints just the background.
    pub fn new(
        handle: &TerminalSessionHandle,
        theme: &TerminalTheme,
        accent: Srgba,
        font_family: SharedString,
        font_px: f32,
        metrics: TerminalMetrics,
        caret_solid: bool,
        ime: ImeInput,
        paint_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    ) -> Self {
        let default_bg = theme.background.to_u32();
        let foreground = theme.foreground.to_u32();
        // Caret color: the theme's cursor override, else the accent token (R2)
        // — exactly `TerminalTheme.swift`'s "nil => caret follows accent".
        let accent_rgba = match theme.cursor {
            Some(c) => Rgba {
                r: c.r as f32 / 255.0,
                g: c.g as f32 / 255.0,
                b: c.b as f32 / 255.0,
                a: 1.0,
            },
            None => Rgba {
                r: accent.r,
                g: accent.g,
                b: accent.b,
                a: 1.0,
            },
        };

        let (rows, cursor) = match handle.term() {
            Some(term_arc) => {
                let term = term_arc.lock();
                snapshot(&term, theme, default_bg, caret_solid)
            }
            None => (Vec::new(), None),
        };

        Self {
            rows,
            default_bg,
            foreground,
            cursor,
            accent: accent_rgba,
            font_family,
            font_px,
            metrics,
            ime,
            paint_bounds,
        }
    }

    /// Paint the snapshot. Order is background quads → cursor block → glyph
    /// runs, so an empty focused cursor cell shows solid accent and every other
    /// cell's glyph paints over its own background.
    fn paint(self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let TerminalElement {
            rows,
            default_bg,
            foreground,
            cursor,
            accent,
            font_family,
            font_px,
            metrics,
            ime,
            paint_bounds,
        } = self;

        // Publish this frame's grid bounds for the view's mouse hit-testing (read
        // on the next mouse event). Same `bounds` the IME anchor + `grid_top_y`
        // use, so a click resolves to the cell that was actually painted there.
        paint_bounds.set(Some(bounds));

        // Register the platform IME input handler for this frame (paint-phase
        // only, active while our focus handle holds focus) — the ime-spike's
        // `handle_input` pattern. `element_bounds` is this grid's bounds, so
        // `bounds_for_range` can anchor the candidate window at the cursor cell.
        let ImeInput {
            focus_handle,
            view,
            preedit,
        } = ime;
        window.handle_input(
            &focus_handle,
            TermInputHandler {
                view: view.clone(),
                element_bounds: bounds,
            },
            cx,
        );
        // While composing, the inline preedit overlay stands in for the block
        // cursor at the cursor cell (matching the ime-spike / Terminal.app).
        let composing = preedit.is_some();

        let cw = metrics.cell_w;
        let ch = metrics.cell_h;
        let ox = bounds.origin.x;
        // Bottom-anchored (T4): pin the grid's bottom edge at `bounds.bottom −
        // gap` and *derive* the top origin, so the prompt line cannot jitter on
        // resize (nothing is remembered). A grid taller than the view gets a
        // negative origin — its top rows fall above the view and the content mask
        // below clips them; a shorter grid leaves the theme-bg remainder on top.
        let oy = px(grid_top_y(bounds, metrics, rows.len()));

        // Clip to the element bounds so the sub-row remainder / over-tall grid is
        // hidden at the TOP, matching `TerminalContainerView` (Nice's Swift host).
        window.with_content_mask(Some(ContentMask { bounds }), move |window| {
            // Whole viewport background (padding around the grid is theme bg too).
            window.paint_quad(fill(bounds, rgb(default_bg)));

            // Per-row coalesced background quads.
            for (r, row) in rows.iter().enumerate() {
                let y = oy + px(r as f32 * ch);
                let mut col = 0usize;
                while col < row.len() {
                    if let Some(bgc) = row[col].bg {
                        let start = col;
                        while col < row.len() && row[col].bg == Some(bgc) {
                            col += 1;
                        }
                        let x = ox + px(start as f32 * cw);
                        let w = px((col - start) as f32 * cw);
                        window.paint_quad(fill(
                            Bounds {
                                origin: point(x, y),
                                size: size(w, px(ch)),
                            },
                            rgb(bgc),
                        ));
                    } else {
                        col += 1;
                    }
                }
            }

            // Block cursor — suppressed while composing (the preedit overlay
            // below stands in for it at the cursor cell).
            if let Some(cur) = &cursor {
                if !composing {
                    let x = ox + px(cur.col as f32 * cw);
                    let y = oy + px(cur.row as f32 * ch);
                    if cur.solid {
                        window.paint_quad(fill(
                            Bounds {
                                origin: point(x, y),
                                size: size(px(cw), px(ch)),
                            },
                            accent,
                        ));
                    } else {
                        paint_hollow_cursor(window, x, y, cw, ch, accent);
                    }
                }
            }

            // Foreground: one `shape_line` per painted cell at its exact cell
            // origin (fractional advances never accumulate), or a procedural fill
            // for the box-drawing / block-element range.
            let scale = window.scale_factor();
            // Pixel-snapped cell geometry for procedural glyphs, mirroring the
            // spike's block-element path: integer device px so aliased line fills
            // are crisp and identical column-to-column.
            let cw_px = (cw * scale).round();
            let ch_px = (ch * scale).round();
            let cw_px_i = (cw_px as i32).max(1);
            let ch_px_i = (ch_px as i32).max(1);
            let light_px = (scale.round() as i32).max(1);
            let ox_f: f32 = ox.into();
            let oy_f: f32 = oy.into();

            for (r, row) in rows.iter().enumerate() {
                let y = oy + px(r as f32 * ch);
                for (c, cell) in row.iter().enumerate() {
                    // The trailing half of a wide glyph paints no glyph of its own.
                    if cell.wide_spacer {
                        continue;
                    }
                    // A solid cursor covers its cell; skip the glyph so it does not
                    // paint over the block. Inverse-video caret text is a later slice.
                    // (While composing the block is suppressed, so the glyph paints
                    // and the preedit overlay lands on top.)
                    if !composing {
                        if let Some(cur) = &cursor {
                            if cur.solid && cur.row == r && cur.col == c {
                                continue;
                            }
                        }
                    }

                    let effective_bg = cell.bg.unwrap_or(default_bg);

                    // Box-drawing / block elements: painted procedurally so line
                    // glyphs join seamlessly (see [`crate::boxdraw`]).
                    if let Some(glyph) =
                        boxdraw::procedural_glyph(cell.ch as u32, cw_px_i, ch_px_i, light_px)
                    {
                        let cell_x_px = (ox_f * scale).round() + c as f32 * cw_px;
                        let cell_y_px = ((oy_f + r as f32 * ch) * scale).round();
                        paint_procedural(
                            window,
                            &glyph,
                            cell_x_px,
                            cell_y_px,
                            scale,
                            cell.fg,
                            effective_bg,
                        );
                        continue;
                    }

                    // A blank cell with no decoration is fully described by its bg
                    // quad; only paint a run when there is ink or a decoration.
                    if cell.ch == ' ' && !cell.underline && !cell.strikethrough {
                        continue;
                    }

                    let decoration = Some(rgb(cell.fg).into());
                    let mut buf = [0u8; 4];
                    let s: &str = cell.ch.encode_utf8(&mut buf);
                    let run = TextRun {
                        len: s.len(),
                        font: cell_font(font_family.clone(), cell.bold, cell.italic),
                        color: rgb(cell.fg).into(),
                        // Carry the cell background so the patched bg-luminance
                        // composition curve engages (spike `appleApprox` path).
                        // This is the mechanism the whole Path-B decision was
                        // gated on.
                        background_color: Some(rgb(effective_bg).into()),
                        underline: cell.underline.then(|| UnderlineStyle {
                            thickness: px(DECORATION_THICKNESS),
                            color: decoration,
                            wavy: false,
                        }),
                        strikethrough: cell.strikethrough.then(|| StrikethroughStyle {
                            thickness: px(DECORATION_THICKNESS),
                            color: decoration,
                        }),
                    };
                    let text: SharedString = SharedString::from(s.to_string());
                    let shaped = window
                        .text_system()
                        .shape_line(text, px(font_px), &[run], None);
                    let x = ox + px(c as f32 * cw);
                    let _ = shaped.paint(point(x, y), px(ch), TextAlign::Left, None, window, cx);
                }
            }

            // Inline preedit (marked text) overlay at the grid cursor cell. The
            // IME composition never enters the grid model (G1 item 1). The whole
            // preedit is underlined thin, the IME's selected sub-range underlined
            // thick, with a composition caret at the selection start — and a
            // subtle accent strip behind it so it reads as "not committed".
            if let (Some((preedit_text, sel_bytes)), Some(cur)) = (&preedit, &cursor) {
                let cur_x = ox + px(cur.col as f32 * cw);
                let cur_y = oy + px(cur.row as f32 * ch);
                let fg: Hsla = rgb(foreground).into();
                let deco: Hsla = accent.into();
                let underline = |thickness: f32| {
                    Some(UnderlineStyle {
                        thickness: px(thickness),
                        color: Some(deco),
                        wavy: false,
                    })
                };
                let font = cell_font(font_family.clone(), false, false);
                let seg = |len: usize, thick: bool| TextRun {
                    len,
                    font: font.clone(),
                    color: fg,
                    background_color: None,
                    underline: underline(if thick { 2.0 } else { 1.0 }),
                    strikethrough: None,
                };
                let len = preedit_text.len();
                let start = sel_bytes.start.min(len);
                let end = sel_bytes.end.min(len).max(start);
                let runs: Vec<TextRun> = [
                    seg(start, false),
                    seg(end - start, true),
                    seg(len - end, false),
                ]
                .into_iter()
                .filter(|run| run.len > 0)
                .collect();
                let runs = if runs.is_empty() {
                    vec![seg(len, false)]
                } else {
                    runs
                };
                let shaped =
                    window
                        .text_system()
                        .shape_line(preedit_text.clone(), px(font_px), &runs, None);
                window.paint_quad(fill(
                    Bounds {
                        origin: point(cur_x, cur_y),
                        size: size(shaped.width, px(ch)),
                    },
                    deco.opacity(0.20),
                ));
                let _ = shaped.paint(point(cur_x, cur_y), px(ch), TextAlign::Left, None, window, cx);
                let caret_x = cur_x + shaped.x_for_index(start);
                window.paint_quad(fill(
                    Bounds {
                        origin: point(caret_x, cur_y),
                        size: size(px(2.0), px(ch)),
                    },
                    deco,
                ));
            }
        });
    }
}

/// Paint one procedural (box-drawing / block-element) glyph. `cell_x_px` /
/// `cell_y_px` are the cell's pixel-snapped device-px origin; the glyph's own
/// coordinates are cell-local device px, converted back to logical px by
/// dividing by `scale`. Solid fills paint opaque `fg`; shades run their coverage
/// through the bg-luminance curve so they composite exactly like glyph coverage.
fn paint_procedural(
    window: &mut Window,
    glyph: &boxdraw::Glyph,
    cell_x_px: f32,
    cell_y_px: f32,
    scale: f32,
    fg: u32,
    effective_bg: u32,
) {
    let fg_hsla: Hsla = rgb(fg).into();
    for f in &glyph.fills {
        let x = (cell_x_px + f.x0 as f32) / scale;
        let y = (cell_y_px + f.y0 as f32) / scale;
        let w = (f.x1 - f.x0) as f32 / scale;
        let h = (f.y1 - f.y0) as f32 / scale;
        let bounds = Bounds {
            origin: point(px(x), px(y)),
            size: size(px(w), px(h)),
        };
        if f.coverage >= 1.0 {
            window.paint_quad(fill(bounds, fg_hsla));
        } else {
            let cov = apple_approx_coverage(f.coverage, fg, effective_bg);
            window.paint_quad(fill(bounds, fg_hsla.opacity(cov)));
        }
    }
    for seg in &glyph.segments {
        match *seg {
            Segment::Line { x0, y0, x1, y1, width } => {
                let mut pb = PathBuilder::stroke(px(width / scale));
                pb.move_to(point(px((cell_x_px + x0) / scale), px((cell_y_px + y0) / scale)));
                pb.line_to(point(px((cell_x_px + x1) / scale), px((cell_y_px + y1) / scale)));
                if let Ok(path) = pb.build() {
                    window.paint_path(path, fg_hsla);
                }
            }
            Segment::Quad { x0, y0, cx, cy, x1, y1, width } => {
                let mut pb = PathBuilder::stroke(px(width / scale));
                pb.move_to(point(px((cell_x_px + x0) / scale), px((cell_y_px + y0) / scale)));
                pb.curve_to(
                    point(px((cell_x_px + x1) / scale), px((cell_y_px + y1) / scale)),
                    point(px((cell_x_px + cx) / scale), px((cell_y_px + cy) / scale)),
                );
                if let Ok(path) = pb.build() {
                    window.paint_path(path, fg_hsla);
                }
            }
        }
    }
}

impl IntoElement for TerminalElement {
    type Element = Canvas<()>;

    fn into_element(self) -> Self::Element {
        canvas(
            |_, _, _| {},
            move |bounds, _state, window, cx| self.paint(bounds, window, cx),
        )
        .size_full()
    }
}

/// Paint a hollow (unfocused) block caret: four 1px-logical accent edges around
/// the cell, leaving its interior showing the underlying cell.
fn paint_hollow_cursor(window: &mut Window, x: Pixels, y: Pixels, cw: f32, ch: f32, accent: Rgba) {
    let t = px(1.0);
    // Top, bottom, left, right.
    window.paint_quad(fill(
        Bounds {
            origin: point(x, y),
            size: size(px(cw), t),
        },
        accent,
    ));
    window.paint_quad(fill(
        Bounds {
            origin: point(x, y + px(ch) - t),
            size: size(px(cw), t),
        },
        accent,
    ));
    window.paint_quad(fill(
        Bounds {
            origin: point(x, y),
            size: size(t, px(ch)),
        },
        accent,
    ));
    window.paint_quad(fill(
        Bounds {
            origin: point(x + px(cw) - t, y),
            size: size(t, px(ch)),
        },
        accent,
    ));
}

/// The monospace font for a cell run, honouring the bold / italic attributes.
fn cell_font(family: SharedString, bold: bool, italic: bool) -> Font {
    Font {
        family,
        features: FontFeatures::default(),
        weight: if bold {
            FontWeight::BOLD
        } else {
            FontWeight::NORMAL
        },
        style: if italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        },
        fallbacks: None,
    }
}

/// Component-wise sRGB inverse (`255 - c` per channel, exact in u8). This is the
/// fork's `NSColor.inverseColor()` — the exact per-channel inversion the spike
/// residual port replicates for inverse-video default colours (NOT a fg/bg swap).
fn invert_rgb(c: u32) -> u32 {
    0x00ff_ffff ^ c
}

/// Is this attribute slot a terminal **default** colour? SwiftTerm collapses
/// `Named(Foreground)` / `Named(Background)` to one `.defaultColor` per slot,
/// which is what its inverse-video `.defaultInvertedColor` rule keys on.
fn is_default_color(c: AnsiColor) -> bool {
    matches!(
        c,
        AnsiColor::Named(NamedColor::Foreground) | AnsiColor::Named(NamedColor::Background)
    )
}

/// Dim / faint (SGR 2): blend the foreground 50 % toward its background, fully
/// opaque. Ported from the fork's `NSColor.dimmedColor(towards:)` — the opaque
/// blend keeps adjacent box-drawing cells tiling without seams.
fn dim_rgb(fg: u32, bg: u32) -> u32 {
    let mix = |shift: u32| {
        let f = (fg >> shift) & 0xff;
        let b = (bg >> shift) & 0xff;
        (f + b) / 2
    };
    (mix(16) << 16) | (mix(8) << 8) | mix(0)
}

/// Copy the visible viewport (cells + cursor) out of a locked `Term` into owned
/// paint data. Generic over the `Term`'s listener so the caller never has to
/// name `nice-term-core`'s private proxy type.
fn snapshot<T: EventListener>(
    term: &Term<T>,
    theme: &TerminalTheme,
    default_bg: u32,
    caret_solid: bool,
) -> (Vec<Vec<PaintCell>>, Option<CursorPaint>) {
    let content = term.renderable_content();
    let display_offset = content.display_offset as i32;
    // Resolved selection range (buffer coords), if any. `SelectionRange` is
    // `Copy`, so we own it and drop the `content` borrow's dependency early.
    let selection: Option<SelectionRange> = content.selection;
    let selection_color = theme
        .selection
        .map(|c| c.to_u32())
        .unwrap_or(DEFAULT_SELECTION);
    let screen_rows = term.screen_lines();
    let cols = term.columns();

    let mut rows = Vec::with_capacity(screen_rows);
    for vr in 0..screen_rows {
        // Viewport row `vr` maps to buffer line `vr - display_offset`.
        let line = Line(vr as i32 - display_offset);
        let mut row = Vec::with_capacity(cols);
        for col in 0..cols {
            let cell = &term.grid()[GridPoint::new(line, Column(col))];
            let flags = cell.flags;

            // Inverse video (SGR 7), the SwiftTerm way: swap the fg / bg attribute
            // slots, then a *default* colour in the swapped fg slot resolves to
            // the inverse of the native foreground, and in the swapped bg slot to
            // the inverse of the native background — NOT the plain swapped colour
            // (the spike row-12 residual: `mapColor(.defaultInvertedColor)`).
            let inverse = flags.contains(Flags::INVERSE);
            let (fg_attr, bg_attr) = if inverse {
                (cell.bg, cell.fg)
            } else {
                (cell.fg, cell.bg)
            };
            let mut fg = if inverse && is_default_color(fg_attr) {
                invert_rgb(theme.foreground.to_u32())
            } else {
                resolve_color(fg_attr, theme, true)
            };
            let mut bg_resolved = if inverse && is_default_color(bg_attr) {
                invert_rgb(theme.background.to_u32())
            } else {
                resolve_color(bg_attr, theme, false)
            };

            // Dim / faint (SGR 2): fade the foreground toward its background.
            if flags.contains(Flags::DIM) {
                fg = dim_rgb(fg, bg_resolved);
            }
            // Hidden (SGR 8): paint the glyph in its own background (invisible).
            if flags.contains(Flags::HIDDEN) {
                fg = bg_resolved;
            }

            // Selection: the highlighted cells' background is replaced by the
            // theme selection colour (SwiftTerm `selectionBackgroundColor`); the
            // foreground text stays and paints over it.
            let selected = selection
                .map(|s| s.contains(GridPoint::new(line, Column(col))))
                .unwrap_or(false);
            if selected {
                bg_resolved = selection_color;
            }

            let bg = if bg_resolved != default_bg {
                Some(bg_resolved)
            } else {
                None
            };
            let ch = if cell.c == '\0' { ' ' } else { cell.c };
            row.push(PaintCell {
                ch,
                fg,
                bg,
                bold: flags.contains(Flags::BOLD),
                italic: flags.contains(Flags::ITALIC),
                underline: flags.intersects(Flags::ALL_UNDERLINES),
                strikethrough: flags.contains(Flags::STRIKEOUT),
                wide_spacer: flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
            });
        }
        rows.push(row);
    }

    // Cursor: buffer line `b` shows at viewport row `b + display_offset`; when
    // scrolled up it can fall below the viewport (skip). Hidden cursor => none.
    let cursor = if content.cursor.shape != CursorShape::Hidden {
        let cp = content.cursor.point;
        let vr = cp.line.0 + display_offset;
        if vr >= 0 && (vr as usize) < screen_rows && cp.column.0 < cols {
            Some(CursorPaint {
                row: vr as usize,
                col: cp.column.0,
                solid: caret_solid,
            })
        } else {
            None
        }
    } else {
        None
    };

    (rows, cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invert_is_exact_per_channel() {
        // 255 - c per channel (the fork's inverseColor), exact in u8.
        assert_eq!(invert_rgb(0x000000), 0xffffff);
        assert_eq!(invert_rgb(0xffffff), 0x000000);
        assert_eq!(invert_rgb(0x123456), 0xedcba9);
        // The dark theme's default bg inverts to a near-white — the inverse-video
        // bar colour the `term-render` scenario asserts.
        assert_eq!(invert_rgb(0x090705), 0xf6f8fa);
    }

    #[test]
    fn dim_is_midpoint_toward_background() {
        // (fg + bg) / 2 per channel.
        assert_eq!(dim_rgb(0xffffff, 0x000000), 0x7f7f7f);
        assert_eq!(dim_rgb(0x000000, 0xffffff), 0x7f7f7f);
        assert_eq!(dim_rgb(0x204060, 0x204060), 0x204060);
    }

    #[test]
    fn only_named_defaults_are_default_colors() {
        assert!(is_default_color(AnsiColor::Named(NamedColor::Foreground)));
        assert!(is_default_color(AnsiColor::Named(NamedColor::Background)));
        assert!(!is_default_color(AnsiColor::Named(NamedColor::Red)));
        assert!(!is_default_color(AnsiColor::Indexed(200)));
        assert!(!is_default_color(AnsiColor::Spec(
            alacritty_terminal::vte::ansi::Rgb { r: 1, g: 2, b: 3 }
        )));
    }
}
