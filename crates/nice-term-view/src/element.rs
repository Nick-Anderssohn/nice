//! `TerminalElement` — the low-level paint element for one terminal frame.
//!
//! It paints from the view-owned, **damage-gated row cache** ([`GridCache`],
//! fix round r5b): building the element locks the [`TerminalSessionHandle`]'s
//! `Term` `FairMutex` only long enough to read-and-reset `Term::damage()` and
//! re-copy the **damaged** viewport rows (the lock is **never** held across a
//! paint), and paint re-plans only those rows — undamaged rows reuse their
//! cached cells and plans across frames. Pre-r5b every draw copied the full
//! rows × cols grid under the lock and re-planned every row; on a 3350×916
//! window that made a 1-row keystroke echo (or the 1/s prompt clock) cost a
//! ~50-row rebuild — measured 2026-07-10 vs Swift Nice: idle 60 s 0.42 vs
//! 0.05 cpu_s, typing 120 cps 7.67 vs 2.68 cpu_s (SwiftTerm repaints only
//! damaged rows; this cache is the same lever under gpui's immediate-mode
//! scene, which still *paints* every row each frame). Drawing goes through
//! gpui's public `canvas` paint API, exactly the shape proven in the phase-0
//! aa-gamma spike (`spikes/phase0-poc/aa-gamma/gpui-term-main/src/main.rs`):
//!
//! * whole-viewport background fill, then per-row coalesced **background quads**
//!   (`paint_quad`) for every cell whose resolved background differs from the
//!   theme default;
//! * per-row **batched foreground glyph runs** — contiguous same-style
//!   printable-ASCII cells shape and paint as ONE `shape_line().paint()` (one
//!   scene layer), with `force_width = cell_w` re-snapping every glyph to its
//!   cell slot (fix round r5: the earlier one-ShapedLine-PER-CELL pass pushed
//!   one scene layer per cell, and `BoundsTree::insert` — which degrades to a
//!   full-tree walk per insert for disjoint rects, O(n²) per frame over
//!   n ≈ inked cells — measured as 79% of a 51 s input-flood whole-app freeze;
//!   see [`plan_row`]). Each run still carries `background_color` so the
//!   patched bg-luminance composition curve engages (the whole reason Path B
//!   was gated on this renderer);
//! * a **block cursor** in the accent color — solid when focused, hollow when
//!   not.
//!
//! It covers the full per-cell paint model: the color model — 16 themed ANSI,
//! 256 computed cube/ramp, 24-bit truecolor (see [`crate::color`]) — plus text
//! attributes (inverse-video with exact per-channel inversion, bold, italic,
//! dim, underline, strikethrough), wide glyphs / emoji, selection rendering from
//! the core's selection state, and procedural box-drawing + block elements
//! (U+2500–259F, see [`crate::boxdraw`]). Procedural cells whose geometry is
//! provably x-uniform (`─ ━ ═ █ ▄ ░` …) batch into per-run band quads (fix
//! round r5c — one `paint_quad` per band per RUN; the 2026-07-10 typing
//! profile put 217/335 forced pre-key-dispatch draw samples in per-cell
//! procedural `paint_quad → BoundsTree::insert`, dominated by ~420-column
//! prompt `─` rules); x-structured glyphs stay per-cell.
//!
//! ## Row-quantized, top-anchored layout (T4, revised)
//!
//! The grid is **anchored to the top** of the element bounds: row 0's top edge
//! sits flush at `bounds.origin.y`, so the gap between the chrome and the first
//! terminal row is a constant (the host's fixed top inset) regardless of the
//! view height. Any sub-row remainder falls at the **bottom** of the view,
//! above the host's bottom inset, where it is clipped by a content mask
//! (`with_content_mask`) — a grid taller than the view loses its bottommost
//! rows. The origin is *computed* from `bounds`, never remembered, so the top
//! edge cannot drift during a live resize.
//!
//! This is a **deliberate divergence from prod** (Swift Nice's
//! `TerminalContainerView`, which bottom-anchors so the prompt-gap at the
//! bottom stays constant during a resize): Nick prefers a stable top edge, and
//! accepts that the sub-row wander — up to `cell_h − 1` px — now shows below
//! the prompt instead, jittering the bottom row's position during a live
//! resize. Scroll offset is read from the core's display offset
//! (line-quantized; the `TerminalSessionHandle` owns the wheel/trackpad
//! stepping).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point as GridPoint};
use alacritty_terminal::selection::SelectionRange;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, TermDamage};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor};

use gpui::{
    canvas, fill, point, prelude::*, px, rgb, size, App, Bounds, Canvas, ContentMask, Entity,
    FocusHandle, Font, FontFeatures, FontStyle, FontWeight, Hsla, PathBuilder, Pixels, Rgba,
    SharedString, StrikethroughStyle, TextAlign, TextRun, UnderlineStyle, Window,
};

use nice_theme::Srgba;

use crate::boxdraw::{self, apple_approx_coverage, Prim, Segment};
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

/// Constant gap reserved below the grid when fitting rows to a content height
/// ([`fit_grid`]), in logical px. Mirrors `TerminalContainerView.bottomInset`
/// (Nice ships `0`: the fit uses the full content height). Since the top-anchor
/// switch this only shrinks the fit — the painted grid starts at the element's
/// top regardless ([`grid_top_y`]); the sub-row remainder plus this gap land at
/// the bottom.
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
///
/// `PartialEq` is load-bearing (fix round r5b): the [`GridCache`] compares a
/// re-copied row against its cached copy to decide whether the row's plan is
/// stale — alacritty over-damages (the cursor line is damaged on every
/// `Term::damage()` read), and the equality check downgrades that to "copied
/// but not re-planned".
#[derive(Clone, Copy, PartialEq, Eq)]
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

/// Cells per batched [`GlyphRun`], max (fix round r5). zed's `BatchedTextRun`
/// sizes for ~10-cell runs; 16 keeps the layer/`BoundsTree::insert` win intact
/// (a 400-column uniform row is ≤25 runs, not 400 layers) while bounding, as
/// belt-and-suspenders under the `force_width` re-snap's 1 px tolerance, how far
/// any sub-tolerance shaping drift can extend, and keeping shape-cache keys
/// short enough to re-hit on TUI redraws of repeated content.
const MAX_RUN_CELLS: usize = 16;

/// The cell attributes that must be equal for two adjacent cells to share one
/// batched run — zed `BatchedTextRun::can_append`'s key, over Nice's resolved
/// paint data: same resolved fg, same resolved bg (`None` == theme default; the
/// bg becomes the run's `TextRun.background_color`, which drives the patched
/// bg-luminance composition curve, so it MUST split runs), same font face
/// (bold/italic), same decorations.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct RunStyle {
    fg: u32,
    bg: Option<u32>,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
}

impl RunStyle {
    fn of(cell: &PaintCell) -> Self {
        RunStyle {
            fg: cell.fg,
            bg: cell.bg,
            bold: cell.bold,
            italic: cell.italic,
            underline: cell.underline,
            strikethrough: cell.strikethrough,
        }
    }
}

/// One batched foreground run: `cells` contiguous same-[`RunStyle`] cells
/// starting at `start_col`, shaped as ONE line and painted as ONE scene layer
/// (see [`plan_row`] / `paint_glyph_run`).
///
/// `text` is a [`SharedString`] (fix round r5b): runs now live across frames in
/// the [`GridCache`], so paint clones an `Arc` per run per frame instead of
/// heap-allocating a fresh string — and hands `shape_line` the exact key shape
/// gpui's cross-frame `LineLayoutCache` memoizes on.
#[derive(Clone, PartialEq, Eq, Debug)]
struct GlyphRun {
    start_col: usize,
    /// Grid cells covered. Equals `text.chars().count()` (one char per cell —
    /// wide glyphs are single-cell runs whose spacer paints nothing).
    cells: usize,
    text: SharedString,
    style: RunStyle,
}

impl GlyphRun {
    fn single(col: usize, ch: char, style: RunStyle) -> Self {
        GlyphRun {
            start_col: col,
            cells: 1,
            text: SharedString::from(ch.to_string()),
            style,
        }
    }
}

/// An in-progress batch inside [`plan_row`]: accumulates into a plain `String`
/// (a [`SharedString`] cannot be appended to) and freezes into its final
/// [`GlyphRun`] once at flush.
struct RunBuilder {
    start_col: usize,
    cells: usize,
    text: String,
    style: RunStyle,
}

impl RunBuilder {
    fn single(col: usize, ch: char, style: RunStyle) -> Self {
        RunBuilder {
            start_col: col,
            cells: 1,
            text: ch.to_string(),
            style,
        }
    }

    fn finish(self) -> GlyphRun {
        GlyphRun {
            start_col: self.start_col,
            cells: self.cells,
            text: SharedString::from(self.text),
            style: self.style,
        }
    }
}

/// The planner's verdict for one procedural (box-drawing / block-element)
/// cell, produced by the injected `procedural` closure (fix round r5c): the
/// paint-site closure runs [`boxdraw::procedural_glyph`] and then
/// [`boxdraw::x_uniform_bands`] over the result, so [`plan_row`] can batch the
/// provably-tileable glyphs without re-deriving geometry.
#[derive(Debug)]
enum ProcGlyph {
    /// The glyph's ink is a set of full-cell-width horizontal bands
    /// ([`boxdraw::x_uniform_bands`] returned `Some`): n adjacent same-style
    /// cells tile into ONE set of n-cells-wide band quads, pixel-identically.
    Uniform(Vec<Prim>),
    /// x-structured geometry (corners, tees, verticals, dashes, arcs …) —
    /// paints per cell from its full fills + segments, exactly as before r5c.
    Cell(boxdraw::Glyph),
}

/// One paint item of a planned row: a batched glyph run, a per-cell procedural
/// box-drawing / block-element glyph (painted from geometry — see
/// [`crate::boxdraw`]), or a batched run of x-uniform procedural cells (fix
/// round r5c — see [`plan_row`]).
#[derive(Debug, PartialEq)]
enum RowItem {
    Run(GlyphRun),
    Procedural {
        col: usize,
        glyph: boxdraw::Glyph,
        fg: u32,
        /// The cell's resolved non-default bg (`None` == theme default) — the
        /// shade-coverage curve composites against the effective bg.
        bg: Option<u32>,
    },
    /// `cells` contiguous procedural cells whose glyphs share these exact
    /// full-width `bands` (all `x0 == 0`, `x1 == cell width`, cell-local
    /// device px) and resolved colors: painted as ONE `paint_quad` per band
    /// for the WHOLE run (the band widened ×`cells`), pixel-identical to the
    /// per-cell fills by [`boxdraw::x_uniform_bands`]'s tiling proof. This is
    /// the r5c typing-flood lever: the 2026-07-10 120 cps typing sample put
    /// 217 of 335 forced pre-key-dispatch draw samples inside
    /// `paint_quad → BoundsTree::insert` — the per-cell quads of the prompt's
    /// ~420-column `─` rules (plain text already coalesces via [`bg_spans`] /
    /// [`GlyphRun`]s) — so a full-width rule must plan to ~1 quad, not ~420.
    ///
    /// Deliberately NOT capped at [`MAX_RUN_CELLS`]: the cap exists to bound
    /// `force_width` shaping drift, and band quads are exact integer
    /// geometry with nothing to drift — capping would reintroduce 26 quads
    /// where 1 suffices.
    ProceduralRun {
        start_col: usize,
        cells: usize,
        bands: Vec<Prim>,
        fg: u32,
        /// Resolved non-default bg (`None` == theme default) — translucent
        /// bands (shades) composite against the effective bg, which the
        /// batching key holds constant across the run.
        bg: Option<u32>,
    },
}

/// An in-progress [`RowItem::ProceduralRun`] inside [`plan_row`]. Extends only
/// while the next cell is contiguous and matches on `(bands, fg, bg)` — the
/// band-geometry compare (not the char) is the batch key, because equal bands
/// are the actual tiling precondition (the char is just where they came from).
struct ProcRunBuilder {
    start_col: usize,
    cells: usize,
    bands: Vec<Prim>,
    fg: u32,
    bg: Option<u32>,
}

impl ProcRunBuilder {
    fn finish(self) -> RowItem {
        RowItem::ProceduralRun {
            start_col: self.start_col,
            cells: self.cells,
            bands: self.bands,
            fg: self.fg,
            bg: self.bg,
        }
    }
}

/// Plan one row's foreground paint: batch contiguous same-style cells into
/// [`GlyphRun`]s (fix round r5 — the input-flood freeze's primary lever; the
/// 2026-07-10 flood sample measured 79% of a 51 s whole-app freeze inside
/// `BoundsTree::insert`, one insert per per-cell scene layer, quadratic over
/// disjoint grid rects, force-drawn before every queued key by gpui's
/// `dispatch_key_event` while the window is dirty).
///
/// Batching may only reduce HOW MANY layers paint, never change WHAT paints, so
/// every split guard vs the per-cell pass is explicit:
///
/// * **skips break runs** — wide-glyph spacers, the solid-cursor cell, and
///   undecorated blanks paint nothing (exactly as before) and end the current
///   run: cells across a gap are not contiguous.
/// * **procedural cells** (box drawing / block elements) stay geometry-painted
///   and end the current text run. Since r5c they batch AMONG THEMSELVES when
///   [`boxdraw::x_uniform_bands`] proves the glyph tileable: contiguous cells
///   with identical `(bands, fg, resolved bg)` coalesce into one
///   [`RowItem::ProceduralRun`] (one quad per band per run — see that
///   variant's docs for the profile evidence); x-structured glyphs
///   (`ProcGlyph::Cell`) keep the per-cell [`RowItem::Procedural`] path and
///   split any procedural run in progress, exactly like a style change.
/// * **wide glyphs** (the next cell is their spacer) get a single-cell run of
///   their own: their font advance spans two columns, which would poison the
///   `force_width` per-cell re-snap for any glyph batched after them (zed's
///   batches end at wide glyphs the same way, via the spacer contiguity break).
/// * **non-ASCII glyphs** get a single-cell run: the per-cell pass shaped every
///   char in isolation, so no cross-cell shaping (Arabic joining, BiDi
///   reordering, combining forms) could ever engage; isolating everything
///   outside printable ASCII keeps that contract while still batching what a
///   flood is made of (shell/TUI output). Wide CJK/emoji land here anyway via
///   the wide-glyph guard.
/// * **style changes and the [`MAX_RUN_CELLS`] cap** flush mid-row; a fresh run
///   starts at the very next cell. (The cap applies to shaped text runs only —
///   see [`RowItem::ProceduralRun`] for why procedural runs are uncapped.)
///
/// `procedural` is injected because [`boxdraw::procedural_glyph`] needs the
/// frame's pixel-snapped cell geometry (tests inject a fake): same per-cell
/// call, same falls-through-to-the-font semantics when it returns `None`.
fn plan_row(
    row: &[PaintCell],
    solid_cursor_col: Option<usize>,
    mut procedural: impl FnMut(char) -> Option<ProcGlyph>,
) -> Vec<RowItem> {
    fn flush(batch: &mut Option<RunBuilder>, items: &mut Vec<RowItem>) {
        if let Some(run) = batch.take() {
            items.push(RowItem::Run(run.finish()));
        }
    }
    fn flush_proc(batch: &mut Option<ProcRunBuilder>, items: &mut Vec<RowItem>) {
        if let Some(run) = batch.take() {
            items.push(run.finish());
        }
    }

    let mut items = Vec::new();
    let mut batch: Option<RunBuilder> = None;
    // At most one of `batch` / `proc_batch` is live at a time (opening either
    // flushes the other), so item order stays column-monotonic.
    let mut proc_batch: Option<ProcRunBuilder> = None;
    for (c, cell) in row.iter().enumerate() {
        // The trailing half of a wide glyph paints no glyph of its own — and it
        // breaks the run (the wide lead cell must stay the LAST cell of its run;
        // see the wide-glyph guard below).
        if cell.wide_spacer {
            flush(&mut batch, &mut items);
            flush_proc(&mut proc_batch, &mut items);
            continue;
        }
        // A solid cursor covers its cell; skip the glyph so it does not paint
        // over the block (inverse-video caret text is a later slice). The skip
        // is also a run break — the cells on either side are not contiguous.
        if solid_cursor_col == Some(c) {
            flush(&mut batch, &mut items);
            flush_proc(&mut proc_batch, &mut items);
            continue;
        }
        // Box-drawing / block elements: painted procedurally so line glyphs
        // join seamlessly (see [`crate::boxdraw`]). x-uniform glyphs batch into
        // a ProceduralRun (r5c); the rest stay per-cell.
        if let Some(pg) = procedural(cell.ch) {
            flush(&mut batch, &mut items);
            match pg {
                ProcGlyph::Cell(glyph) => {
                    flush_proc(&mut proc_batch, &mut items);
                    items.push(RowItem::Procedural {
                        col: c,
                        glyph,
                        fg: cell.fg,
                        bg: cell.bg,
                    });
                }
                ProcGlyph::Uniform(bands) => match &mut proc_batch {
                    // Extend only when contiguous with IDENTICAL band geometry
                    // and resolved colors — any difference is a visible seam,
                    // so it splits (mirroring the text-run RunStyle key).
                    Some(run)
                        if run.start_col + run.cells == c
                            && run.fg == cell.fg
                            && run.bg == cell.bg
                            && run.bands == bands =>
                    {
                        run.cells += 1;
                    }
                    _ => {
                        flush_proc(&mut proc_batch, &mut items);
                        proc_batch = Some(ProcRunBuilder {
                            start_col: c,
                            cells: 1,
                            bands,
                            fg: cell.fg,
                            bg: cell.bg,
                        });
                    }
                },
            }
            continue;
        }
        // Any non-procedural cell ends a procedural run (not contiguous).
        flush_proc(&mut proc_batch, &mut items);
        // A blank cell with no decoration is fully described by its bg quad;
        // only paint a run when there is ink or a decoration.
        if cell.ch == ' ' && !cell.underline && !cell.strikethrough {
            flush(&mut batch, &mut items);
            continue;
        }

        let style = RunStyle::of(cell);
        let wide = row.get(c + 1).is_some_and(|next| next.wide_spacer);
        let simple_ascii = cell.ch == ' ' || cell.ch.is_ascii_graphic();
        if wide || !simple_ascii {
            // Isolation guards (see the doc comment): a single-cell run keeps
            // this glyph's shaping and placement identical to the per-cell pass.
            flush(&mut batch, &mut items);
            items.push(RowItem::Run(GlyphRun::single(c, cell.ch, style)));
            continue;
        }
        match &mut batch {
            Some(run)
                if run.style == style
                    && run.start_col + run.cells == c
                    && run.cells < MAX_RUN_CELLS =>
            {
                run.text.push(cell.ch);
                run.cells += 1;
            }
            _ => {
                flush(&mut batch, &mut items);
                batch = Some(RunBuilder::single(c, cell.ch, style));
            }
        }
    }
    flush(&mut batch, &mut items);
    flush_proc(&mut proc_batch, &mut items);
    items
}

/// The per-row coalesced background spans `(start_col, end_col_exclusive,
/// color)` — one quad each, exactly the spike's coalescing the paint loop used
/// to compute inline every frame. Pure over the row's resolved cells (selection
/// and inverse-video are already baked into [`PaintCell::bg`]), so it caches in
/// the [`GridCache`] row plan with the same invalidation as the glyph items.
fn bg_spans(row: &[PaintCell]) -> Vec<(usize, usize, u32)> {
    let mut spans = Vec::new();
    let mut col = 0usize;
    while col < row.len() {
        if let Some(bgc) = row[col].bg {
            let start = col;
            while col < row.len() && row[col].bg == Some(bgc) {
                col += 1;
            }
            spans.push((start, col, bgc));
        } else {
            col += 1;
        }
    }
    spans
}

/// Everything besides per-cell grid content that the cached row snapshots
/// depend on (fix round r5b). Rebuilt each frame and compared against the
/// cache's copy: **any** difference full-invalidates every cached row —
/// correctness over cleverness, per the r5b brief.
///
/// Field-by-field WHY:
/// * `term_ptr` — a respawn swaps in a fresh `Term`; its cells share nothing
///   with the cache. (Belt-and-suspenders: a fresh `Term` also reports
///   `TermDamage::Full` on its first read, so an ABA'd allocation is harmless.)
/// * `theme` — every [`PaintCell`] color is resolved THROUGH the theme
///   (16 ANSI + default fg/bg + the selection tint), so a live re-color (R21)
///   changes cells whose grid content is untouched.
/// * `screen_rows` / `cols` — a resize re-shapes every row (alacritty also
///   marks a resize fully damaged; this keeps the cache's vectors sized).
/// * `display_offset` — viewport row `vr` shows buffer line
///   `vr - display_offset`; a scroll re-maps every row. (alacritty marks
///   `scroll_display` fully damaged too — again belt-and-suspenders.)
/// * `selection` — the resolved selection range is baked into cell backgrounds,
///   and alacritty's damage explicitly does NOT track selection (see
///   `Term::damage`'s docs); this compare is the only thing keeping a
///   drag-select repaint honest.
#[derive(Clone, PartialEq)]
struct SnapshotKey {
    term_ptr: usize,
    theme: TerminalTheme,
    screen_rows: usize,
    cols: usize,
    display_offset: usize,
    selection: Option<SelectionRange>,
}

/// The frame's row-invalidation verdict, distilled from `Term::damage()` plus
/// the core's out-of-band flag (the in-place ED(2) erase) before it reaches
/// [`GridCache::reconcile`]. `Rows` are **viewport** row indices (alacritty's
/// `TermDamageIterator` already adds the display offset and drops off-screen
/// damage).
enum RowDamage {
    Full,
    Rows(Vec<usize>),
}

/// The pixel-snapped procedural-glyph geometry a row plan was computed under
/// (device px). Box-drawing [`RowItem::Procedural`] items bake this geometry
/// into their fills/segments, so a font zoom or a backing-scale change (window
/// dragged across displays) must re-plan — this is how a **font/metrics
/// change** invalidates plans (cells don't depend on the font; shaping happens
/// per frame from the element's live font fields).
#[derive(Clone, Copy, PartialEq, Eq)]
struct PlanGeometry {
    cw_px: i32,
    ch_px: i32,
    light_px: i32,
}

/// One row's cached paint plan: the coalesced background spans plus the
/// batched glyph / procedural items. Recomputed only while its row is dirty.
#[derive(Default, Debug, PartialEq)]
struct RowPlan {
    /// `(start_col, end_col_exclusive, color)` — see [`bg_spans`].
    spans: Vec<(usize, usize, u32)>,
    items: Vec<RowItem>,
}

/// Cross-frame, per-view render cache — fix round r5b's damage-gated rows.
///
/// The r5 run-batching fix removed the O(n²) scene build, but every draw still
/// re-copied the FULL grid under the `Term` `FairMutex` and re-planned every
/// row: measured 2026-07-10 (3350×916 window, vs Swift Nice on identical
/// workloads) — idle 60 s 0.42 vs 0.05 cpu_s, typing 30 cps 4.43 vs 3.21,
/// 120 cps 7.67 vs 2.68. A keystroke echo (1 row) or the 1/s prompt clock paid
/// a ~50-row × ~400-col rebuild. SwiftTerm repaints only damaged rows; this
/// cache is the equivalent lever under gpui's immediate-mode paint: every draw
/// still PAINTS every row (the scene is rebuilt per frame by design), but only
/// **damaged** rows are re-copied under the lock and re-planned — undamaged
/// rows reuse their cached [`PaintCell`]s and [`RowPlan`]s, and per-run shaping
/// stays memoized across frames by gpui's `LineLayoutCache` (keyed on
/// text + runs + `force_width`, all of which the cached plans hold stable).
///
/// Invalidation, enumerated (miss one ⇒ stale pixels):
/// * **cells** — `Term::damage()` per-line verdicts, read-and-reset under the
///   same lock as the row copy ([`TerminalElement::new`] is the *sole* damage
///   consumer; one `TerminalView` per session handle is the standing wiring);
/// * **whole grid** — `TermDamage::Full`, the core's forced-full flag (in-place
///   ED(2)), or ANY [`SnapshotKey`] change (respawn / theme / resize / scroll
///   offset / selection — see the key's field docs);
/// * **plans only** — the solid-cursor glyph-skip cell moving (cursor motion,
///   focus flip solid↔hollow, IME composition start/end) re-plans exactly the
///   old + new cursor rows via [`GridCache::reconcile`]'s `glyph_skip`
///   parameter, and a [`PlanGeometry`] change (font zoom / display scale)
///   re-plans every row in [`GridCache::ensure_plans`].
///
/// Deliberately NOT invalidation inputs: the accent (cursor block + preedit
/// paint fresh each frame), the font family/size (shaped fresh each frame from
/// the element's fields; only the pixel-snapped [`PlanGeometry`] is baked into
/// plans), and the preedit text (an overlay, never in the grid).
#[derive(Default)]
pub struct GridCache {
    key: Option<SnapshotKey>,
    rows: Vec<Vec<PaintCell>>,
    plans: Vec<RowPlan>,
    /// Per-row "plan is stale" flags, set by [`reconcile`](Self::reconcile) and
    /// cleared by [`ensure_plans`](Self::ensure_plans) (paint-time, because
    /// planning needs the frame's [`PlanGeometry`]).
    plan_dirty: Vec<bool>,
    /// The solid-cursor glyph-skip cell `(viewport_row, col)` the current plans
    /// were computed under (`None` ⇒ no cell skipped: hidden/hollow cursor, or
    /// composing — the preedit overlay paints over the glyph instead).
    planned_skip: Option<(usize, usize)>,
    /// The [`PlanGeometry`] the current plans were computed under.
    plan_geom: Option<PlanGeometry>,
    /// Row-copy scratch buffer, kept to reuse its allocation across frames.
    scratch: Vec<PaintCell>,
}

impl GridCache {
    /// Drop everything (the unspawned-session path — nothing to paint).
    fn clear(&mut self) {
        self.key = None;
        self.rows.clear();
        self.plans.clear();
        self.plan_dirty.clear();
        self.planned_skip = None;
    }

    /// Bring the cached rows up to date for this frame. Called under the `Term`
    /// lock with `fill_row(vr, out)` reading viewport row `vr`'s cells — the
    /// whole point of r5b's lock-hold shrink is that `fill_row` runs for
    /// damaged rows ONLY (plus a full pass on any [`SnapshotKey`] change).
    ///
    /// A re-copied row whose cells compare equal to the cached copy keeps its
    /// plan (alacritty over-damages: the cursor line is damaged on every
    /// `Term::damage()` read, and INSERT mode reports `Full` per read — the
    /// equality check downgrades both to "copied, not re-planned").
    fn reconcile(
        &mut self,
        key: SnapshotKey,
        damage: RowDamage,
        glyph_skip: Option<(usize, usize)>,
        mut fill_row: impl FnMut(usize, &mut Vec<PaintCell>),
    ) {
        let full = matches!(damage, RowDamage::Full) || self.key.as_ref() != Some(&key);
        let n = key.screen_rows;
        if self.rows.len() != n {
            self.rows.resize_with(n, Vec::new);
            self.plans.resize_with(n, RowPlan::default);
            self.plan_dirty.resize(n, true);
        }
        if full {
            for vr in 0..n {
                self.refresh_row(vr, &mut fill_row);
            }
        } else if let RowDamage::Rows(rows) = &damage {
            for &vr in rows {
                // Defensive clamp: damage rows are already viewport-filtered by
                // alacritty, but a stale index must never panic a paint.
                if vr < n {
                    self.refresh_row(vr, &mut fill_row);
                }
            }
        }
        // The solid-cursor glyph skip is a PLAN input, not a cell input: the
        // skipped cell's glyph must not paint over the block, so when the skip
        // moves, BOTH the row it left (its glyph reappears) and the row it
        // entered (its glyph disappears) re-plan — even though neither row's
        // cells changed. Covers cursor motion between rows, focus solid↔hollow
        // flips, and IME composition start/end (skip ⇒ None while composing).
        if self.planned_skip != glyph_skip {
            if let Some((row, _)) = self.planned_skip {
                if row < n {
                    self.plan_dirty[row] = true;
                }
            }
            if let Some((row, _)) = glyph_skip {
                if row < n {
                    self.plan_dirty[row] = true;
                }
            }
            self.planned_skip = glyph_skip;
        }
        self.key = Some(key);
    }

    /// Re-copy row `vr` into the cache; mark its plan dirty iff the cells
    /// actually changed.
    fn refresh_row(&mut self, vr: usize, fill_row: &mut impl FnMut(usize, &mut Vec<PaintCell>)) {
        self.scratch.clear();
        fill_row(vr, &mut self.scratch);
        if self.scratch != self.rows[vr] {
            std::mem::swap(&mut self.scratch, &mut self.rows[vr]);
            self.plan_dirty[vr] = true;
        }
    }

    /// Re-plan every dirty row (paint-time: planning needs the frame's
    /// pixel-snapped [`PlanGeometry`] for procedural glyphs). A geometry change
    /// (font zoom, display-scale change) dirties every plan first.
    fn ensure_plans(
        &mut self,
        geom: PlanGeometry,
        mut procedural: impl FnMut(char) -> Option<ProcGlyph>,
    ) {
        if self.plan_geom != Some(geom) {
            self.plan_geom = Some(geom);
            for dirty in &mut self.plan_dirty {
                *dirty = true;
            }
        }
        for vr in 0..self.rows.len() {
            if self.plan_dirty[vr] {
                let skip_col = self
                    .planned_skip
                    .filter(|(row, _)| *row == vr)
                    .map(|(_, col)| col);
                self.plans[vr] = RowPlan {
                    spans: bg_spans(&self.rows[vr]),
                    items: plan_row(&self.rows[vr], skip_col, &mut procedural),
                };
                self.plan_dirty[vr] = false;
            }
        }
    }
}

/// A paint-ready snapshot of one terminal frame. Owns everything it draws, so
/// the `Term` lock is released before this is handed to the paint pipeline.
pub struct TerminalElement {
    /// The view-owned cross-frame row cache (fix round r5b): reconciled against
    /// `Term::damage()` in [`TerminalElement::new`], planned + painted from in
    /// `paint`. `Rc<RefCell>` for the same reason as `paint_bounds` — the view
    /// keeps it alive across frames without paint ever re-entering the entity.
    cache: Rc<RefCell<GridCache>>,
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
    /// Whether a change in the painted bounds schedules a pty grid refit on the
    /// view (M2 Item E). Mirrors `TerminalView::auto_refit`; when false (the
    /// fixed-grid scenario embeddings) paint only publishes the bounds.
    auto_refit: bool,
}

/// Y (logical px) of the top of grid row 0 under the top-anchored layout (T4,
/// revised): row 0 sits flush at the element's top edge, so this is simply
/// `bounds.origin.y` — the sub-row remainder (and a grid taller than the view)
/// falls at the bottom, where the paint-time content mask clips it. A
/// deliberate divergence from prod's bottom-anchored `TerminalContainerView`
/// (see the module doc). Kept as the single origin authority, shared with the
/// view's mouse hit-testing and its `bounds_for_range` IME anchor so the
/// candidate window lands where the row paints.
pub fn grid_top_y(bounds: Bounds<Pixels>) -> f32 {
    f32::from(bounds.origin.y)
}

/// Grid dimensions `(rows, cols)` that fill a content area `content_w × content_h`
/// (logical px) at cell `metrics`, clamped to at least 1×1. The bottom gap
/// ([`TERMINAL_BOTTOM_GAP`]) is reserved from the height. This is the re-metric
/// fit the view applies to the pty on a font change (T11): when the cell box grows
/// or shrinks under zoom, the same window holds a different number of cells, and
/// the new `(rows, cols)` are pushed to the pty via the R3/R4 resize path (which
/// drives SIGWINCH). The niceties-zoom self-test recomputes with this same
/// function so its expected-fit assertion cannot drift from the view.
pub fn fit_grid(content_w: f32, content_h: f32, metrics: TerminalMetrics) -> (u16, u16) {
    let usable_h = (content_h - TERMINAL_BOTTOM_GAP).max(0.0);
    let cols = (content_w / metrics.cell_w).floor().max(1.0) as u16;
    let rows = (usable_h / metrics.cell_h).floor().max(1.0) as u16;
    (rows, cols)
}

impl TerminalElement {
    /// Build the frame over `handle`'s session grid, reconciling the
    /// view-owned [`GridCache`] against the `Term`'s damage (fix round r5b).
    ///
    /// Locks the shared `Term` only long enough to read-and-reset its damage
    /// and re-copy the **damaged** viewport rows into the cache (pre-r5b every
    /// frame copied all rows × cols under this lock — the dominant per-draw
    /// cost the 2026-07-10 measurements attribute the idle/typing CPU gap vs
    /// Swift to), then releases it. This is the sole consumer of
    /// `Term::damage()` (one `TerminalView` — hence one element per frame —
    /// per session handle is the standing wiring; a second consumer would
    /// starve this one). `caret_solid` is the focus verdict the view computes
    /// (`is_focused && window active`) — passed in so this crate keeps no
    /// separate focus flag. If the session has not spawned, the cache is
    /// cleared and the element paints just the background.
    #[allow(clippy::too_many_arguments)]
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
        auto_refit: bool,
        cache: Rc<RefCell<GridCache>>,
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

        let cursor = match handle.term() {
            Some(term_arc) => {
                let mut grid_cache = cache.borrow_mut();
                let mut term = term_arc.lock();
                // The core's out-of-band flag (in-place ED(2) mutated the grid
                // where alacritty's damage cannot see it) must be taken while
                // THIS lock is held — the feeder raises it mid-parse under the
                // same lock, so takes and raises can never interleave with a
                // half-seen grid (see `Session::take_forced_full_damage`).
                let forced_full = handle.take_forced_full_damage();
                // Read-and-reset the damage under the same hold as the row
                // copies below: nothing can dirty the grid in between, so the
                // verdict exactly covers what the copies will see.
                let damage = if forced_full {
                    let _ = term.damage(); // keep last_cursor tracking coherent
                    RowDamage::Full
                } else {
                    match term.damage() {
                        TermDamage::Full => RowDamage::Full,
                        // Column bounds are ignored: whole-row granularity is
                        // the r5b contract (correctness over cleverness).
                        TermDamage::Partial(lines) => {
                            RowDamage::Rows(lines.map(|l| l.line).collect())
                        }
                    }
                };
                term.reset_damage();

                let screen_rows = term.screen_lines();
                let cols = term.columns();
                let content = term.renderable_content();
                let display_offset = content.display_offset;
                // Resolved selection (buffer coords). `SelectionRange` is
                // `Copy`; it rides the `SnapshotKey` because alacritty's damage
                // deliberately does NOT track selection (`Term::damage` docs).
                let selection: Option<SelectionRange> = content.selection;
                let cursor = viewport_cursor(&content, screen_rows, cols, caret_solid);

                let key = SnapshotKey {
                    term_ptr: Arc::as_ptr(term_arc) as *const () as usize,
                    theme: theme.clone(),
                    screen_rows,
                    cols,
                    display_offset,
                    selection,
                };
                // The solid-cursor glyph-skip cell (a PLAN input — see
                // `GridCache`): only a solid, non-composing caret suppresses
                // its cell's glyph; while composing the preedit overlay paints
                // over the glyph instead.
                let composing = ime.preedit.is_some();
                let glyph_skip = if composing {
                    None
                } else {
                    cursor
                        .as_ref()
                        .filter(|cur| cur.solid)
                        .map(|cur| (cur.row, cur.col))
                };
                let selection_color = theme
                    .selection
                    .map(|c| c.to_u32())
                    .unwrap_or(DEFAULT_SELECTION);
                grid_cache.reconcile(key, damage, glyph_skip, |vr, out| {
                    fill_row(
                        &term,
                        theme,
                        default_bg,
                        selection,
                        selection_color,
                        display_offset as i32,
                        cols,
                        vr,
                        out,
                    )
                });
                cursor
            }
            None => {
                cache.borrow_mut().clear();
                None
            }
        };

        Self {
            cache,
            default_bg,
            foreground,
            cursor,
            accent: accent_rgba,
            font_family,
            font_px,
            metrics,
            ime,
            paint_bounds,
            auto_refit,
        }
    }

    /// Paint the snapshot. Order is background quads → cursor block → glyph
    /// runs, so an empty focused cursor cell shows solid accent and every other
    /// cell's glyph paints over its own background.
    fn paint(self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let TerminalElement {
            cache,
            default_bg,
            foreground,
            cursor,
            accent,
            font_family,
            font_px,
            metrics,
            ime,
            paint_bounds,
            auto_refit,
        } = self;
        // Exclusive for the whole paint: `ensure_plans` below re-plans the rows
        // `new`'s reconcile marked dirty, then the paint loops read the plans.
        // Nothing inside paint re-enters the view (the refit is `cx.defer`red),
        // so the borrow cannot conflict.
        let mut cache = cache.borrow_mut();

        // Publish this frame's grid bounds for the view's mouse hit-testing (read
        // on the next mouse event). Same `bounds` the IME anchor + `grid_top_y`
        // use, so a click resolves to the cell that was actually painted there.
        let prev_bounds = paint_bounds.replace(Some(bounds));

        // M2 Item E — window resize → pty grid refit. When the painted bounds
        // changed (a live window resize, a layout change, or the first paint),
        // schedule the view's `schedule_refit` OUTSIDE this paint pass
        // (`cx.defer`; updating the view entity mid-paint would re-enter it).
        // `schedule_refit` coalesces a live-resize burst behind the Swift-parity
        // resize debounce (one TIOCSWINSZ/SIGWINCH per window, not one per row
        // crossing; the bootstrap first fit applies synchronously) and at fire
        // time reads the freshly-published `paint_bounds`, so it fits what the
        // newest frame painted; the `last_pty_fit` guard drops sub-cell bounds
        // deltas and breaks the resize → SIGWINCH → repaint feedback loop (an
        // unchanged-bounds repaint never even schedules). The T4 top-anchored
        // `grid_top_y` math is untouched — once the grid tracks the view, only
        // the sub-row remainder shows at the bottom.
        if auto_refit && prev_bounds != Some(bounds) {
            let view = ime.view.clone();
            cx.defer(move |cx| {
                view.update(cx, |view, cx| view.schedule_refit(cx));
            });
        }

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
        // Top-anchored (T4, revised): row 0 starts flush at the element's top,
        // so the top gap is constant during a resize (nothing is remembered). A
        // grid taller than the view overruns the bottom edge and the content
        // mask below clips it; a shorter grid leaves the theme-bg remainder at
        // the bottom. Deliberate divergence from prod's bottom-anchored
        // `TerminalContainerView` — see the module doc.
        let oy = px(grid_top_y(bounds));

        // Clip to the element bounds so the sub-row remainder / over-tall grid is
        // hidden at the BOTTOM.
        window.with_content_mask(Some(ContentMask { bounds }), move |window| {
            // Whole viewport background (padding around the grid is theme bg too).
            window.paint_quad(fill(bounds, rgb(default_bg)));

            // Pixel-snapped cell geometry for procedural glyphs, mirroring the
            // spike's block-element path: integer device px so aliased line fills
            // are crisp and identical column-to-column. Computed BEFORE the paint
            // loops because it is also the plan-geometry key: `ensure_plans`
            // re-plans the rows `new`'s reconcile dirtied (and every row when
            // this geometry changed — font zoom / display-scale move), leaving
            // undamaged rows' cached plans untouched (fix round r5b; pre-r5b
            // every draw re-planned all rows — see `GridCache`).
            let scale = window.scale_factor();
            let cw_px = (cw * scale).round();
            let ch_px = (ch * scale).round();
            let cw_px_i = (cw_px as i32).max(1);
            let ch_px_i = (ch_px as i32).max(1);
            let light_px = (scale.round() as i32).max(1);
            let ox_f: f32 = ox.into();
            let oy_f: f32 = oy.into();
            cache.ensure_plans(
                PlanGeometry {
                    cw_px: cw_px_i,
                    ch_px: ch_px_i,
                    light_px,
                },
                // Classify each procedural glyph for the planner (r5c): prove
                // x-uniformity over the laid-out geometry so `plan_row` can
                // batch tileable glyphs into ProceduralRuns (see that
                // variant's docs). Runs per re-plan of a dirty row only; the
                // verdict is baked into the cached plan.
                |chr| {
                    let glyph = boxdraw::procedural_glyph(chr as u32, cw_px_i, ch_px_i, light_px)?;
                    Some(match boxdraw::x_uniform_bands(&glyph, cw_px_i) {
                        Some(bands) => ProcGlyph::Uniform(bands),
                        None => ProcGlyph::Cell(glyph),
                    })
                },
            );

            // Per-row coalesced background quads (cached spans — see `bg_spans`).
            for (r, plan) in cache.plans.iter().enumerate() {
                let y = oy + px(r as f32 * ch);
                for &(start, end, bgc) in &plan.spans {
                    let x = ox + px(start as f32 * cw);
                    let w = px((end - start) as f32 * cw);
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(x, y),
                            size: size(w, px(ch)),
                        },
                        rgb(bgc),
                    ));
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

            // Foreground: batched same-style glyph runs (fix round r5 — the
            // input-flood freeze's primary lever), read from the r5b row cache.
            // The pre-r5 pass shaped and painted one ShapedLine PER CELL; every
            // `ShapedLine::paint` pushes its own scene layer and every
            // `Scene::push_layer` inserts into gpui's `BoundsTree`, whose insert
            // degrades to a full-tree walk for disjoint rects (the fast path
            // needs the new rect to INTERSECT the current max leaf; grid cells
            // never do) — O(n²) per frame over n ≈ inked cells (~21-29k on an
            // ultrawide). The 2026-07-10 flood sample (leg B3, 400 cps) measured
            // 79% of a 51 s whole-app freeze inside `BoundsTree::insert`,
            // force-drawn before EVERY queued key by gpui's `dispatch_key_event`
            // while the window was dirty. Batching (zed's proven
            // `BatchedTextRun` pattern) cuts shape_line calls, layers, and
            // BoundsTree inserts ~10-30x; `plan_row` holds the split guards —
            // including the solid-cursor glyph skip, planned from
            // `GridCache::planned_skip` — and `paint_glyph_run` the force_width
            // grid re-snap. Per-run shaping stays memoized across frames by
            // gpui's `LineLayoutCache` (the cached plans hold its
            // text+runs+force_width key stable).
            //
            // Batched runs must never engage cross-cell OpenType shaping: the
            // per-cell pass shaped every char in isolation, so ligatures ("->"
            // via `calt` in fonts like Fira Code, "fi" via `liga`) could never
            // form — and a formed ligature would collapse glyphs and break the
            // per-cell force_width re-snap. Force both OFF for run shaping;
            // isolated glyphs render identically either way, and neither feature
            // changes advances (cell metrics stay valid).
            let run_features = FontFeatures(Arc::new(vec![
                ("calt".to_string(), 0),
                ("liga".to_string(), 0),
            ]));

            for (r, plan) in cache.plans.iter().enumerate() {
                let y = oy + px(r as f32 * ch);
                for item in &plan.items {
                    match item {
                        RowItem::Procedural { col, glyph, fg, bg } => {
                            let cell_x_px = (ox_f * scale).round() + *col as f32 * cw_px;
                            let cell_y_px = ((oy_f + r as f32 * ch) * scale).round();
                            paint_procedural(
                                window,
                                glyph,
                                cell_x_px,
                                cell_y_px,
                                scale,
                                *fg,
                                bg.unwrap_or(default_bg),
                            );
                        }
                        RowItem::ProceduralRun {
                            start_col,
                            cells,
                            bands,
                            fg,
                            bg,
                        } => {
                            let run_x_px = (ox_f * scale).round() + *start_col as f32 * cw_px;
                            let cell_y_px = ((oy_f + r as f32 * ch) * scale).round();
                            paint_procedural_run(
                                window,
                                bands,
                                *cells,
                                run_x_px,
                                cell_y_px,
                                cw_px,
                                scale,
                                *fg,
                                bg.unwrap_or(default_bg),
                            );
                        }
                        RowItem::Run(run) => paint_glyph_run(
                            window,
                            cx,
                            run,
                            &font_family,
                            &run_features,
                            font_px,
                            default_bg,
                            cw,
                            ch,
                            ox,
                            y,
                        ),
                    }
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

/// Paint one batched [`RowItem::ProceduralRun`]: each full-cell-width band is
/// ONE quad spanning the whole run (fix round r5c). Tiling equivalence with
/// the per-cell path: cell `i` of the run would paint the band at
/// `run_x_px + i·cw_px` with width `x1 − x0 == cw_px` — integer-valued device
/// px, so consecutive cells' rects abut exactly (no AA overlap, no gap) and
/// their union IS this one rect. Translucent bands (shades) therefore
/// composite once per pixel either way, and the coverage curve input
/// `(coverage, fg, effective_bg)` is constant across the run by the batch key.
#[allow(clippy::too_many_arguments)]
fn paint_procedural_run(
    window: &mut Window,
    bands: &[Prim],
    cells: usize,
    run_x_px: f32,
    cell_y_px: f32,
    cw_px: f32,
    scale: f32,
    fg: u32,
    effective_bg: u32,
) {
    let fg_hsla: Hsla = rgb(fg).into();
    for b in bands {
        // Bands are full-width by construction (x0 == 0, x1 == cw_px); the
        // width is still computed from the band's own edges so the equivalence
        // above holds even if the invariant loosens.
        let x = (run_x_px + b.x0 as f32) / scale;
        let y = (cell_y_px + b.y0 as f32) / scale;
        let w = ((cells as f32 - 1.0) * cw_px + (b.x1 - b.x0) as f32) / scale;
        let h = (b.y1 - b.y0) as f32 / scale;
        let bounds = Bounds {
            origin: point(px(x), px(y)),
            size: size(px(w), px(h)),
        };
        if b.coverage >= 1.0 {
            window.paint_quad(fill(bounds, fg_hsla));
        } else {
            let cov = apple_approx_coverage(b.coverage, fg, effective_bg);
            window.paint_quad(fill(bounds, fg_hsla.opacity(cov)));
        }
    }
}

/// Shape + paint one batched [`GlyphRun`] at its grid origin: ONE `shape_line`
/// and ONE `ShapedLine::paint` (⇒ one scene layer, one `BoundsTree::insert`)
/// for the whole run — the r5 freeze fix's primary lever (see [`plan_row`]).
///
/// Grid alignment: shaping passes `force_width = cell_w`, so gpui's
/// `apply_force_width_to_layout` re-snaps every base glyph to its **absolute**
/// cell slot within the run (the mechanism zed's terminal ships on) — font
/// advances and kerning cannot accumulate drift across the run. The run carries
/// one `background_color` (the style key splits runs on bg), so the patched
/// bg-luminance composition curve engages exactly as the per-cell pass did —
/// the mechanism the whole Path-B decision was gated on.
#[allow(clippy::too_many_arguments)]
fn paint_glyph_run(
    window: &mut Window,
    cx: &mut App,
    run: &GlyphRun,
    font_family: &SharedString,
    run_features: &FontFeatures,
    font_px: f32,
    default_bg: u32,
    cw: f32,
    ch: f32,
    ox: Pixels,
    y: Pixels,
) {
    let decoration = Some(rgb(run.style.fg).into());
    let effective_bg = run.style.bg.unwrap_or(default_bg);
    let text_run = TextRun {
        len: run.text.len(),
        font: Font {
            family: font_family.clone(),
            features: run_features.clone(),
            weight: if run.style.bold {
                FontWeight::BOLD
            } else {
                FontWeight::NORMAL
            },
            style: if run.style.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
            fallbacks: None,
        },
        color: rgb(run.style.fg).into(),
        background_color: Some(rgb(effective_bg).into()),
        underline: run.style.underline.then(|| UnderlineStyle {
            thickness: px(DECORATION_THICKNESS),
            color: decoration,
            wavy: false,
        }),
        strikethrough: run.style.strikethrough.then(|| StrikethroughStyle {
            thickness: px(DECORATION_THICKNESS),
            color: decoration,
        }),
    };
    let text: SharedString = SharedString::from(run.text.clone());
    let shaped = window
        .text_system()
        .shape_line(text, px(font_px), &[text_run], Some(px(cw)));
    let x = ox + px(run.start_col as f32 * cw);
    let _ = shaped.paint(point(x, y), px(ch), TextAlign::Left, None, window, cx);
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

/// Copy ONE visible viewport row's cells out of a locked `Term` into owned
/// paint data — the per-row body of the pre-r5b full-grid `snapshot`, split
/// out so [`GridCache::reconcile`] runs it for damaged rows only. Byte-for-byte
/// the same resolution pipeline (inverse-video, dim, hidden, selection).
/// Generic over the `Term`'s listener so the caller never has to name
/// `nice-term-core`'s private proxy type.
#[allow(clippy::too_many_arguments)]
fn fill_row<T: EventListener>(
    term: &Term<T>,
    theme: &TerminalTheme,
    default_bg: u32,
    selection: Option<SelectionRange>,
    selection_color: u32,
    display_offset: i32,
    cols: usize,
    vr: usize,
    out: &mut Vec<PaintCell>,
) {
    out.clear();
    out.reserve(cols);
    // Viewport row `vr` maps to buffer line `vr - display_offset`.
    let line = Line(vr as i32 - display_offset);
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
        out.push(PaintCell {
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
}

/// The block cursor's viewport placement: buffer line `b` shows at viewport row
/// `b + display_offset`; when scrolled up it can fall below the viewport
/// (skip). Hidden cursor => none. Unchanged from the pre-r5b `snapshot` tail.
fn viewport_cursor(
    content: &alacritty_terminal::term::RenderableContent<'_>,
    screen_rows: usize,
    cols: usize,
    caret_solid: bool,
) -> Option<CursorPaint> {
    if content.cursor.shape == CursorShape::Hidden {
        return None;
    }
    let cp = content.cursor.point;
    let vr = cp.line.0 + content.display_offset as i32;
    if vr >= 0 && (vr as usize) < screen_rows && cp.column.0 < cols {
        Some(CursorPaint {
            row: vr as usize,
            col: cp.column.0,
            solid: caret_solid,
        })
    } else {
        None
    }
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

    // ---- Run batching (fix round r5, lever 1) --------------------------------
    //
    // These pin `plan_row`'s split guards: batching may only reduce how many
    // layers paint, never change WHAT paints, so every boundary the per-cell
    // pass produced implicitly (skips, style changes, wide glyphs, the cursor
    // cell) must survive as an explicit run split.

    fn pc(ch: char) -> PaintCell {
        PaintCell {
            ch,
            fg: 0xffffff,
            bg: None,
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            wide_spacer: false,
        }
    }

    fn no_procedural(_: char) -> Option<ProcGlyph> {
        None
    }

    /// The planned runs as `(start_col, text, cells)`, ignoring procedural items.
    fn runs(items: &[RowItem]) -> Vec<(usize, String, usize)> {
        items
            .iter()
            .filter_map(|item| match item {
                RowItem::Run(r) => Some((r.start_col, r.text.to_string(), r.cells)),
                RowItem::Procedural { .. } | RowItem::ProceduralRun { .. } => None,
            })
            .collect()
    }

    #[test]
    fn uniform_ascii_row_coalesces_to_one_run() {
        // The batching win itself: what was 7 shape_line + 7 scene layers
        // (7 BoundsTree inserts) is now exactly one.
        let row: Vec<PaintCell> = "prompt$".chars().map(pc).collect();
        let items = plan_row(&row, None, no_procedural);
        assert_eq!(runs(&items), vec![(0, "prompt$".to_string(), 7)]);
    }

    #[test]
    fn style_change_splits_at_the_boundary() {
        // fg is part of the run key (it is also the decoration color).
        let mut row: Vec<PaintCell> = "aaabbb".chars().map(pc).collect();
        for cell in &mut row[3..] {
            cell.fg = 0xff0000;
        }
        let items = plan_row(&row, None, no_procedural);
        assert_eq!(
            runs(&items),
            vec![(0, "aaa".to_string(), 3), (3, "bbb".to_string(), 3)]
        );
    }

    #[test]
    fn background_change_splits_runs() {
        // bg becomes the run's TextRun.background_color — the input to the
        // patched bg-luminance composition curve — so it must never be shared
        // across cells whose resolved bg differs.
        let mut row: Vec<PaintCell> = "abcd".chars().map(pc).collect();
        row[2].bg = Some(0x204060);
        row[3].bg = Some(0x204060);
        let items = plan_row(&row, None, no_procedural);
        assert_eq!(
            runs(&items),
            vec![(0, "ab".to_string(), 2), (2, "cd".to_string(), 2)]
        );
    }

    #[test]
    fn bold_change_splits_runs() {
        let mut row: Vec<PaintCell> = "abcd".chars().map(pc).collect();
        row[0].bold = true;
        row[1].bold = true;
        let items = plan_row(&row, None, no_procedural);
        assert_eq!(
            runs(&items),
            vec![(0, "ab".to_string(), 2), (2, "cd".to_string(), 2)]
        );
    }

    #[test]
    fn undecorated_blank_breaks_the_run_and_paints_nothing() {
        // "ab cd": the blank is fully described by its bg quad (unchanged), and
        // the two words are not contiguous, so they are separate runs.
        let row: Vec<PaintCell> = "ab cd".chars().map(pc).collect();
        let items = plan_row(&row, None, no_procedural);
        assert_eq!(
            runs(&items),
            vec![(0, "ab".to_string(), 2), (3, "cd".to_string(), 2)]
        );
    }

    #[test]
    fn decorated_blank_joins_its_run() {
        // An underlined space has ink to paint (the underline), and its style
        // matches its underlined neighbours — one run, one continuous underline.
        let mut row: Vec<PaintCell> = "a b".chars().map(pc).collect();
        for cell in &mut row {
            cell.underline = true;
        }
        let items = plan_row(&row, None, no_procedural);
        assert_eq!(runs(&items), vec![(0, "a b".to_string(), 3)]);
    }

    #[test]
    fn solid_cursor_cell_is_skipped_and_splits() {
        // The glyph under a solid cursor must not paint over the block (parity
        // with the per-cell pass), and the neighbours must not batch across it.
        let row: Vec<PaintCell> = "abcde".chars().map(pc).collect();
        let items = plan_row(&row, Some(2), no_procedural);
        assert_eq!(
            runs(&items),
            vec![(0, "ab".to_string(), 2), (3, "de".to_string(), 2)]
        );
    }

    #[test]
    fn wide_glyph_gets_a_single_cell_run_and_spacer_paints_nothing() {
        // [a][你][spacer][b]: the wide glyph's advance spans two columns, so it
        // must end its run (or poison the force_width re-snap for glyphs after
        // it); its spacer paints no glyph of its own.
        let mut row: Vec<PaintCell> = vec![pc('a'), pc('你'), pc(' '), pc('b')];
        row[2].wide_spacer = true;
        let items = plan_row(&row, None, no_procedural);
        assert_eq!(
            runs(&items),
            vec![
                (0, "a".to_string(), 1),
                (1, "你".to_string(), 1),
                (3, "b".to_string(), 1),
            ]
        );
        assert_eq!(items.len(), 3, "the spacer must plan nothing at all");
    }

    #[test]
    fn non_ascii_is_isolated_per_cell() {
        // The per-cell pass shaped every char alone, so no cross-cell shaping
        // (ligatures, Arabic joining, BiDi reordering) could ever engage — a
        // non-ASCII char therefore never batches with its neighbours.
        let row: Vec<PaintCell> = vec![pc('a'), pc('é'), pc('b')];
        let items = plan_row(&row, None, no_procedural);
        assert_eq!(
            runs(&items),
            vec![
                (0, "a".to_string(), 1),
                (1, "é".to_string(), 1),
                (2, "b".to_string(), 1),
            ]
        );
    }

    #[test]
    fn run_cap_splits_contiguously() {
        // Belt-and-suspenders cap under the force_width re-snap: a long uniform
        // row splits at MAX_RUN_CELLS with the next run starting at the very
        // next column — no cell lost, no cell doubled.
        let row: Vec<PaintCell> = std::iter::repeat('x').take(20).map(pc).collect();
        let items = plan_row(&row, None, no_procedural);
        assert_eq!(
            runs(&items),
            vec![
                (0, "x".repeat(MAX_RUN_CELLS), MAX_RUN_CELLS),
                (MAX_RUN_CELLS, "x".repeat(4), 4),
            ]
        );
    }

    #[test]
    fn procedural_cell_interrupts_runs_and_carries_its_colors() {
        // Box-drawing stays geometry-painted (seamless joins) and splits the
        // neighbouring runs; its resolved fg/bg ride along for the shade curve.
        let mut row: Vec<PaintCell> = vec![pc('a'), pc('─'), pc('b')];
        row[1].fg = 0x00ff00;
        row[1].bg = Some(0x101010);
        let items = plan_row(&row, None, |chr| {
            (chr == '─').then(|| ProcGlyph::Cell(boxdraw::Glyph::default()))
        });
        assert_eq!(items.len(), 3);
        assert_eq!(
            items[1],
            RowItem::Procedural {
                col: 1,
                glyph: boxdraw::Glyph::default(),
                fg: 0x00ff00,
                bg: Some(0x101010),
            }
        );
        assert_eq!(
            runs(&items),
            vec![(0, "a".to_string(), 1), (2, "b".to_string(), 1)]
        );
    }

    // ---- Procedural run batching (fix round r5c, lever A) ---------------------
    //
    // These pin the ProceduralRun batching contract: x-uniform glyphs coalesce
    // per (bands, fg, bg) into one item; ANY difference — colors, band
    // geometry, an x-structured neighbour, the cursor skip, a gap — splits.
    // The 2026-07-10 typing profile motivates the whole thing: 217/335 forced
    // pre-key-dispatch draw samples sat in paint_quad → BoundsTree::insert,
    // dominated by ~420 one-cell quads per visible prompt `─` rule.

    /// A one-band x-uniform verdict; `y0` differentiates band geometries so a
    /// test can hand different chars provably-different bands.
    fn band(y0: i32) -> Vec<Prim> {
        vec![Prim {
            x0: 0,
            y0,
            x1: 8,
            y1: y0 + 2,
            coverage: 1.0,
        }]
    }

    /// The test procedural classifier: `─`/`═` are x-uniform (distinct bands),
    /// `│` is x-structured (per-cell), everything else falls through to text.
    fn proc_classifier(chr: char) -> Option<ProcGlyph> {
        match chr {
            '─' => Some(ProcGlyph::Uniform(band(7))),
            '═' => Some(ProcGlyph::Uniform(band(5))),
            '│' => Some(ProcGlyph::Cell(boxdraw::Glyph::default())),
            _ => None,
        }
    }

    /// The planned procedural runs as `(start_col, cells)`.
    fn proc_runs(items: &[RowItem]) -> Vec<(usize, usize)> {
        items
            .iter()
            .filter_map(|item| match item {
                RowItem::ProceduralRun {
                    start_col, cells, ..
                } => Some((*start_col, *cells)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn uniform_procedural_run_batches_to_one_item() {
        // THE r5c win: a rule of N same-style `─` cells is ONE item (⇒ one
        // paint_quad per band per run), not N per-cell fills.
        let row: Vec<PaintCell> = "─────".chars().map(pc).collect();
        let items = plan_row(&row, None, proc_classifier);
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0],
            RowItem::ProceduralRun {
                start_col: 0,
                cells: 5,
                bands: band(7),
                fg: 0xffffff,
                bg: None,
            }
        );
    }

    #[test]
    fn procedural_runs_have_no_length_cap() {
        // MAX_RUN_CELLS bounds force_width shaping drift; band quads have
        // nothing to drift, so a 420-column prompt rule must stay ONE item
        // (capping would hand back 27 quads where 1 suffices).
        let row: Vec<PaintCell> = std::iter::repeat('─').take(420).map(pc).collect();
        let items = plan_row(&row, None, proc_classifier);
        assert_eq!(proc_runs(&items), vec![(0, 420)]);
        assert!(420 > MAX_RUN_CELLS, "the point: way past the text-run cap");
    }

    #[test]
    fn procedural_run_splits_on_fg_and_bg_changes() {
        // fg feeds the band color and bg the shade-composite curve — a shared
        // run would repaint one side's colors with the other's.
        let mut row: Vec<PaintCell> = "────".chars().map(pc).collect();
        row[2].fg = 0xff0000;
        row[3].fg = 0xff0000;
        let items = plan_row(&row, None, proc_classifier);
        assert_eq!(proc_runs(&items), vec![(0, 2), (2, 2)]);

        let mut row: Vec<PaintCell> = "────".chars().map(pc).collect();
        row[0].bg = Some(0x204060);
        let items = plan_row(&row, None, proc_classifier);
        assert_eq!(proc_runs(&items), vec![(0, 1), (1, 3)]);
    }

    #[test]
    fn different_band_geometries_do_not_merge() {
        // `─` and `═` are both x-uniform but with different bands — batching
        // them would repaint one as the other. The batch key is the geometry.
        let row: Vec<PaintCell> = "──══".chars().map(pc).collect();
        let items = plan_row(&row, None, proc_classifier);
        assert_eq!(proc_runs(&items), vec![(0, 2), (2, 2)]);
        assert!(matches!(
            &items[0],
            RowItem::ProceduralRun { bands, .. } if *bands == band(7)
        ));
        assert!(matches!(
            &items[1],
            RowItem::ProceduralRun { bands, .. } if *bands == band(5)
        ));
    }

    #[test]
    fn non_uniform_procedural_stays_per_cell() {
        // x-structured glyphs (verticals, corners, tees …) keep the per-cell
        // Procedural path — three `│` are three items, no run.
        let row: Vec<PaintCell> = "│││".chars().map(pc).collect();
        let items = plan_row(&row, None, proc_classifier);
        assert_eq!(items.len(), 3);
        assert!(items
            .iter()
            .all(|item| matches!(item, RowItem::Procedural { .. })));
        assert_eq!(proc_runs(&items), vec![]);
    }

    #[test]
    fn uniform_next_to_non_uniform_splits() {
        // A `│` in the middle of a rule (a tee-less junction) ends the run on
        // both sides; the runs stay column-exact around it.
        let row: Vec<PaintCell> = "──│──".chars().map(pc).collect();
        let items = plan_row(&row, None, proc_classifier);
        assert_eq!(items.len(), 3);
        assert_eq!(proc_runs(&items), vec![(0, 2), (3, 2)]);
        assert!(matches!(items[1], RowItem::Procedural { col: 2, .. }));
    }

    #[test]
    fn solid_cursor_splits_a_procedural_run() {
        // The block cursor covers its cell: the rule's fill under it must not
        // paint (per-cell parity), and the neighbours must not batch across.
        let row: Vec<PaintCell> = "────".chars().map(pc).collect();
        let items = plan_row(&row, Some(1), proc_classifier);
        assert_eq!(proc_runs(&items), vec![(0, 1), (2, 2)]);
    }

    #[test]
    fn text_and_procedural_runs_interleave_column_exactly() {
        // Mixed content: each kind of run ends where the other begins; nothing
        // is lost, doubled, or reordered.
        let row: Vec<PaintCell> = "ab──cd".chars().map(pc).collect();
        let items = plan_row(&row, None, proc_classifier);
        assert_eq!(
            runs(&items),
            vec![(0, "ab".to_string(), 2), (4, "cd".to_string(), 2)]
        );
        assert_eq!(proc_runs(&items), vec![(2, 2)]);
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn undecorated_blank_splits_a_procedural_run() {
        // "── ──": the blank paints nothing and the two rule halves are not
        // contiguous — two runs, not one 5-cell run bridging the gap.
        let row: Vec<PaintCell> = "── ──".chars().map(pc).collect();
        let items = plan_row(&row, None, proc_classifier);
        assert_eq!(proc_runs(&items), vec![(0, 2), (3, 2)]);
    }

    // ---- Background spans (r5b — cached per-row plan) -------------------------

    #[test]
    fn bg_spans_coalesce_contiguous_same_color_only() {
        // The exact coalescing the paint loop used to run inline every frame:
        // contiguous same-bg cells share one span; default-bg (None) cells and
        // color changes split. `(start, end_exclusive, color)`.
        let mut row: Vec<PaintCell> = "abcdef".chars().map(pc).collect();
        row[1].bg = Some(0x111111);
        row[2].bg = Some(0x111111);
        row[3].bg = Some(0x222222);
        row[5].bg = Some(0x111111);
        assert_eq!(
            bg_spans(&row),
            vec![(1, 3, 0x111111), (3, 4, 0x222222), (5, 6, 0x111111)]
        );
        assert_eq!(bg_spans(&"plain".chars().map(pc).collect::<Vec<_>>()), vec![]);
    }

    // ---- Damage-gated row cache (fix round r5b) -------------------------------
    //
    // These pin `GridCache`'s invalidation contract with NO gpui and NO `Term`:
    // `reconcile` takes a synthetic key/damage/glyph-skip and a fill closure
    // standing in for the under-the-lock row copy, so the tests can observe
    // exactly which rows were copied (the closure's call log) and which plans
    // went stale (`plan_dirty`). The r5b bar: a keystroke echo or the 1/s
    // prompt clock re-copies/re-plans 1-2 rows, never the whole grid (the
    // pre-r5b full rebuild measured 0.42 cpu_s idle / 7.67 cpu_s @120cps vs
    // Swift's 0.05 / 2.68) — while resize/theme/selection/scroll/cursor changes
    // must still invalidate everything they touch (miss one ⇒ stale pixels).

    const GEOM: PlanGeometry = PlanGeometry {
        cw_px: 8,
        ch_px: 16,
        light_px: 1,
    };

    fn test_key(screen_rows: usize, cols: usize) -> SnapshotKey {
        SnapshotKey {
            term_ptr: 0xA110C,
            theme: TerminalTheme::nice_default_dark(),
            screen_rows,
            cols,
            display_offset: 0,
            selection: None,
        }
    }

    /// A fill closure over `content` (one char per row, repeated `cols` times),
    /// logging each row it is asked to copy into `log`.
    fn filler<'a>(
        content: &'a [char],
        cols: usize,
        log: &'a mut Vec<usize>,
    ) -> impl FnMut(usize, &mut Vec<PaintCell>) + 'a {
        move |vr, out| {
            log.push(vr);
            out.clear();
            out.extend(std::iter::repeat(pc(content[vr])).take(cols));
        }
    }

    /// Seed a clean 3-row cache: full reconcile + plans built, nothing dirty.
    fn seeded_cache(content: &[char]) -> GridCache {
        let mut cache = GridCache::default();
        let mut log = Vec::new();
        cache.reconcile(
            test_key(content.len(), 4),
            RowDamage::Full,
            None,
            filler(content, 4, &mut log),
        );
        cache.ensure_plans(GEOM, no_procedural);
        assert!(cache.plan_dirty.iter().all(|d| !d), "seed must end clean");
        cache
    }

    #[test]
    fn partial_damage_copies_only_the_damaged_row_and_replans_it() {
        // THE r5b win: a one-row echo re-copies one row under the lock (the
        // fill closure runs once) and re-plans one row — the other rows'
        // cells AND plans are reused untouched.
        let mut cache = seeded_cache(&['a', 'b', 'c']);
        let mut log = Vec::new();
        cache.reconcile(
            test_key(3, 4),
            RowDamage::Rows(vec![1]),
            None,
            filler(&['a', 'X', 'c'], 4, &mut log),
        );
        assert_eq!(log, vec![1], "only the damaged row is copied under the lock");
        assert_eq!(cache.plan_dirty, vec![false, true, false]);

        cache.ensure_plans(GEOM, no_procedural);
        assert_eq!(
            runs(&cache.plans[1].items),
            vec![(0, "XXXX".to_string(), 4)],
            "the damaged row's plan reflects the new cells"
        );
        assert_eq!(
            runs(&cache.plans[0].items),
            vec![(0, "aaaa".to_string(), 4)],
            "undamaged rows keep their cached plan"
        );
    }

    #[test]
    fn damaged_row_with_identical_cells_is_not_replanned() {
        // alacritty over-damages (the cursor line is damaged on EVERY
        // `Term::damage()` read): the copied-row equality check downgrades
        // that to "copied, not re-planned", so a parked cursor costs no
        // plan/alloc churn per frame.
        let mut cache = seeded_cache(&['a', 'b', 'c']);
        let mut log = Vec::new();
        cache.reconcile(
            test_key(3, 4),
            RowDamage::Rows(vec![2]),
            None,
            filler(&['a', 'b', 'c'], 4, &mut log),
        );
        assert_eq!(log, vec![2], "the damaged row is still verified by copy");
        assert_eq!(
            cache.plan_dirty,
            vec![false, false, false],
            "identical cells must not dirty the plan"
        );
    }

    #[test]
    fn any_snapshot_key_change_refills_every_row() {
        // Full invalidation triggers, each exercised as the ONLY change:
        // resize (rows/cols), theme recolor, selection change, scroll offset,
        // term identity (respawn). Correctness over cleverness — any key
        // change refills all rows (and re-plans those whose cells changed).
        let base = ['a', 'b', 'c'];
        let mut variants: Vec<(&str, SnapshotKey)> = Vec::new();
        let mut k = test_key(3, 4);
        k.cols = 5;
        variants.push(("cols change (resize)", k));
        let mut k = test_key(3, 4);
        k.theme = TerminalTheme::nice_default_light();
        variants.push(("theme change (R21 recolor)", k));
        let mut k = test_key(3, 4);
        k.selection = Some(SelectionRange::new(
            GridPoint::new(Line(0), Column(0)),
            GridPoint::new(Line(1), Column(2)),
            false,
        ));
        variants.push(("selection change (not in alacritty damage)", k));
        let mut k = test_key(3, 4);
        k.display_offset = 2;
        variants.push(("display-offset change (scroll re-maps rows)", k));
        let mut k = test_key(3, 4);
        k.term_ptr = 0xB0B;
        variants.push(("term identity change (respawn)", k));

        for (what, key) in variants {
            let mut cache = seeded_cache(&base);
            let mut log = Vec::new();
            let cols = key.cols;
            cache.reconcile(
                key,
                // Deliberately NO row damage: the key change alone must refill.
                RowDamage::Rows(vec![]),
                None,
                filler(&base, cols, &mut log),
            );
            assert_eq!(log, vec![0, 1, 2], "{what} must refill every row");
        }
    }

    #[test]
    fn row_count_change_resizes_and_dirties_the_new_rows() {
        let mut cache = seeded_cache(&['a', 'b', 'c']);
        let content = ['a', 'b', 'c', 'd', 'e'];
        let mut log = Vec::new();
        cache.reconcile(
            test_key(5, 4),
            RowDamage::Rows(vec![]),
            None,
            filler(&content, 4, &mut log),
        );
        assert_eq!(log, vec![0, 1, 2, 3, 4]);
        assert_eq!(cache.rows.len(), 5);
        assert_eq!(cache.plans.len(), 5);
        // Rows 0-2 refilled to identical cells (plans reusable); 3-4 are new.
        assert_eq!(cache.plan_dirty, vec![false, false, false, true, true]);
    }

    #[test]
    fn cursor_moving_between_rows_replans_exactly_the_old_and_new_rows() {
        // The solid-cursor glyph skip is a PLAN input with no cell change:
        // the row the block left must re-show its glyph, the row it entered
        // must suppress one — and nothing is re-copied under the lock.
        let mut cache = seeded_cache(&['a', 'b', 'c']);
        let mut log = Vec::new();
        cache.reconcile(
            test_key(3, 4),
            RowDamage::Rows(vec![]),
            Some((0, 2)),
            filler(&['a', 'b', 'c'], 4, &mut log),
        );
        assert_eq!(log, Vec::<usize>::new(), "a cursor move copies nothing under the lock");
        assert_eq!(cache.plan_dirty, vec![true, false, false]);
        cache.ensure_plans(GEOM, no_procedural);
        assert_eq!(
            runs(&cache.plans[0].items),
            vec![(0, "aa".to_string(), 2), (3, "a".to_string(), 1)],
            "the entered row's plan skips the solid-cursor cell"
        );

        let mut log = Vec::new();
        cache.reconcile(
            test_key(3, 4),
            RowDamage::Rows(vec![]),
            Some((2, 1)),
            filler(&['a', 'b', 'c'], 4, &mut log),
        );
        assert_eq!(
            cache.plan_dirty,
            vec![true, false, true],
            "old cursor row AND new cursor row re-plan"
        );
        cache.ensure_plans(GEOM, no_procedural);
        assert_eq!(
            runs(&cache.plans[0].items),
            vec![(0, "aaaa".to_string(), 4)],
            "the left row's glyph reappears"
        );
    }

    #[test]
    fn focus_or_composing_flip_replans_the_cursor_row() {
        // solid→hollow (focus loss / window deactivation) and composing
        // start (preedit overlay paints over the glyph) both collapse the
        // glyph skip to None — same invalidation path, no cell change.
        let mut cache = seeded_cache(&['a', 'b', 'c']);
        let mut log = Vec::new();
        cache.reconcile(
            test_key(3, 4),
            RowDamage::Rows(vec![]),
            Some((1, 0)),
            filler(&['a', 'b', 'c'], 4, &mut log),
        );
        assert_eq!(cache.plan_dirty, vec![false, true, false]);
        cache.ensure_plans(GEOM, no_procedural);

        let mut log = Vec::new();
        cache.reconcile(
            test_key(3, 4),
            RowDamage::Rows(vec![]),
            None, // hollow / hidden / composing: no glyph suppressed
            filler(&['a', 'b', 'c'], 4, &mut log),
        );
        assert_eq!(
            cache.plan_dirty,
            vec![false, true, false],
            "only the row the skip vacated re-plans"
        );
    }

    #[test]
    fn geometry_change_replans_every_row_without_recopying() {
        // A font zoom / display-scale change re-bakes procedural geometry into
        // the plans (paint-time key) but never touches the cells: observed via
        // the procedural probe, which planning consults once per plannable
        // cell — and an unchanged geometry consults zero times (all clean).
        let mut cache = seeded_cache(&['a', 'b', 'c']);

        let count = std::cell::Cell::new(0usize);
        cache.ensure_plans(GEOM, |_| {
            count.set(count.get() + 1);
            None
        });
        assert_eq!(count.get(), 0, "clean cache + same geometry plans nothing");

        cache.ensure_plans(
            PlanGeometry {
                cw_px: 16,
                ch_px: 32,
                light_px: 2,
            },
            |_| {
                count.set(count.get() + 1);
                None
            },
        );
        assert_eq!(count.get(), 3 * 4, "new geometry re-plans every cell of every row");
    }

    #[test]
    fn clear_empties_the_cache_for_an_unspawned_session() {
        let mut cache = seeded_cache(&['a', 'b', 'c']);
        cache.clear();
        assert!(cache.rows.is_empty());
        assert!(cache.plans.is_empty());
        assert!(cache.key.is_none());
        // And a later spawn reconciles from scratch (Full path via key mismatch).
        let mut log = Vec::new();
        cache.reconcile(
            test_key(2, 4),
            RowDamage::Rows(vec![]),
            None,
            filler(&['x', 'y'], 4, &mut log),
        );
        assert_eq!(log, vec![0, 1]);
    }

    #[test]
    fn out_of_range_damage_rows_are_ignored() {
        // Defensive: alacritty's iterator is already viewport-filtered, but a
        // stale index must never panic a paint.
        let mut cache = seeded_cache(&['a', 'b', 'c']);
        let mut log = Vec::new();
        cache.reconcile(
            test_key(3, 4),
            RowDamage::Rows(vec![7]),
            None,
            filler(&['a', 'b', 'c'], 4, &mut log),
        );
        assert_eq!(log, Vec::<usize>::new());
        assert_eq!(cache.plan_dirty, vec![false, false, false]);
    }
}
