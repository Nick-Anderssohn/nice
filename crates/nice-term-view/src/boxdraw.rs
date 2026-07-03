//! Procedural box-drawing (U+2500–257F) and block elements (U+2580–259F).
//!
//! These glyphs are painted from geometry, not from the font, so adjacent line
//! cells join **seamlessly** — a font-rendered `─`/`│` leaves hairline gaps at
//! the cell seams and antialiases differently on each side, which the aa-gamma
//! spike measured as the residual on rows 4/5/14
//! (`spikes/phase0-poc/aa-gamma/RESULTS-spike1-20260701.md`). This module is the
//! paint-level fix that residual note flagged "if ever wanted": it is now wanted.
//!
//! Two ports feed it:
//!
//! * **Block elements U+2580–259F** — the spike's proven `BlockElementRenderer`
//!   port (`spikes/phase0-poc/aa-gamma/gpui-term-main/src/main.rs` `block_rects`):
//!   eighth-cell rectangles, shades (`░▒▓`) as full-cell fills at coverage
//!   .25/.5/.75. This is verbatim the "+287/−17" residual work the plan cites.
//! * **Box-drawing lines U+2500–257F** — ported from our own MIT-licensed
//!   SwiftTerm fork's `BoxDrawingRenderer` (`/Users/nick/Projects/SwiftTerm/
//!   Sources/SwiftTerm/Apple/BoxDrawingRenderer.swift`). The fork draws into a
//!   bottom-origin `CGContext` and flips in `box()`; because that flip maps
//!   `y = 0` to the **cell top**, the `linesChar` coordinates port **directly**
//!   as top-origin (y-down) rectangles here — no flip (unit-tested below).
//!
//! Everything is computed in **integer device pixels** (aliased, pixel-aligned)
//! so lines are crisp and identical column-to-column. The paint site
//! ([`crate::element`]) converts device px back to logical px by dividing by the
//! window scale factor, exactly as the spike's block-element path does.

/// One axis-aligned fill in cell-local **device px**, top-origin (`y` grows
/// down). `coverage` is the ink fraction: `1.0` for a solid line/block (painted
/// opaque), `< 1.0` for a shade (`░▒▓`) run through the bg-luminance curve at
/// paint time exactly like glyph coverage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Prim {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub coverage: f32,
}

/// A stroked segment (arcs + diagonals) in cell-local **device px**, top-origin.
/// Straight lines and rounded corners are the only non-rectangular box-drawing
/// glyphs; they are stroked through gpui's path API rather than filled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Segment {
    /// A straight stroke `from → to`.
    Line {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        width: f32,
    },
    /// A quadratic bezier `from → (ctrl) → to` — the rounded-corner arcs.
    Quad {
        x0: f32,
        y0: f32,
        cx: f32,
        cy: f32,
        x1: f32,
        y1: f32,
        width: f32,
    },
}

/// The procedural layout of one glyph: opaque/shaded fills + stroked segments,
/// all cell-local device px (top-origin).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Glyph {
    pub fills: Vec<Prim>,
    pub segments: Vec<Segment>,
}

/// Lowest / highest code point this module draws procedurally.
pub(crate) const LOWER: u32 = 0x2500;
pub(crate) const UPPER: u32 = 0x259F;

/// Lay out `cp` procedurally into `cw_px × ch_px` device pixels, using `light_px`
/// as the light-line stroke thickness (the caller folds the minimum-stroke floor
/// in). Returns `None` for code points outside U+2500–259F.
pub(crate) fn procedural_glyph(cp: u32, cw_px: i32, ch_px: i32, light_px: i32) -> Option<Glyph> {
    if !(LOWER..=UPPER).contains(&cp) {
        return None;
    }
    if cp >= 0x2580 {
        return Some(block_glyph(cp, cw_px, ch_px));
    }
    Some(box_glyph(cp, cw_px, ch_px, light_px))
}

// ---- block elements U+2580–259F (spike BlockElementRenderer port) -----------

/// A rectangle in cell **eighths** — `(x0, x1, y0, y1)` with `y` from the cell
/// top — plus its fill coverage. Mirrors the spike's `BlockRect`.
#[derive(Clone, Copy)]
struct Eighth {
    x0: u8,
    x1: u8,
    y0: u8,
    y1: u8,
    coverage: f32,
}

const fn e(x0: u8, x1: u8, y0: u8, y1: u8, coverage: f32) -> Eighth {
    Eighth { x0, x1, y0, y1, coverage }
}

const FULL: Eighth = e(0, 8, 0, 8, 1.0);
const QUAD_UL: Eighth = e(0, 4, 0, 4, 1.0);
const QUAD_UR: Eighth = e(4, 8, 0, 4, 1.0);
const QUAD_LL: Eighth = e(0, 4, 4, 8, 1.0);
const QUAD_LR: Eighth = e(4, 8, 4, 8, 1.0);

const fn upper(n: u8) -> Eighth {
    e(0, 8, 0, n, 1.0)
}
const fn lower(n: u8) -> Eighth {
    e(0, 8, 8 - n, 8, 1.0)
}
const fn left(n: u8) -> Eighth {
    e(0, n, 0, 8, 1.0)
}
const fn right(n: u8) -> Eighth {
    e(8 - n, 8, 0, 8, 1.0)
}

/// The eighth-rects for one block-element code point (verbatim the spike's
/// `block_rects` table; U+2580–259F).
fn block_eighths(cp: u32) -> &'static [Eighth] {
    const B2580: [Eighth; 1] = [upper(4)];
    const B2581: [Eighth; 1] = [lower(1)];
    const B2582: [Eighth; 1] = [lower(2)];
    const B2583: [Eighth; 1] = [lower(3)];
    const B2584: [Eighth; 1] = [lower(4)];
    const B2585: [Eighth; 1] = [lower(5)];
    const B2586: [Eighth; 1] = [lower(6)];
    const B2587: [Eighth; 1] = [lower(7)];
    const B2588: [Eighth; 1] = [FULL];
    const B2589: [Eighth; 1] = [left(7)];
    const B258A: [Eighth; 1] = [left(6)];
    const B258B: [Eighth; 1] = [left(5)];
    const B258C: [Eighth; 1] = [left(4)];
    const B258D: [Eighth; 1] = [left(3)];
    const B258E: [Eighth; 1] = [left(2)];
    const B258F: [Eighth; 1] = [left(1)];
    const B2590: [Eighth; 1] = [right(4)];
    const B2591: [Eighth; 1] = [e(0, 8, 0, 8, 0.25)];
    const B2592: [Eighth; 1] = [e(0, 8, 0, 8, 0.5)];
    const B2593: [Eighth; 1] = [e(0, 8, 0, 8, 0.75)];
    const B2594: [Eighth; 1] = [upper(1)];
    const B2595: [Eighth; 1] = [right(1)];
    const B2596: [Eighth; 1] = [QUAD_LL];
    const B2597: [Eighth; 1] = [QUAD_LR];
    const B2598: [Eighth; 1] = [QUAD_UL];
    const B2599: [Eighth; 3] = [QUAD_UL, QUAD_LL, QUAD_LR];
    const B259A: [Eighth; 2] = [QUAD_UL, QUAD_LR];
    const B259B: [Eighth; 3] = [QUAD_UL, QUAD_UR, QUAD_LL];
    const B259C: [Eighth; 3] = [QUAD_UL, QUAD_UR, QUAD_LR];
    const B259D: [Eighth; 1] = [QUAD_UR];
    const B259E: [Eighth; 2] = [QUAD_UR, QUAD_LL];
    const B259F: [Eighth; 3] = [QUAD_UR, QUAD_LL, QUAD_LR];
    match cp {
        0x2580 => &B2580,
        0x2581 => &B2581,
        0x2582 => &B2582,
        0x2583 => &B2583,
        0x2584 => &B2584,
        0x2585 => &B2585,
        0x2586 => &B2586,
        0x2587 => &B2587,
        0x2588 => &B2588,
        0x2589 => &B2589,
        0x258A => &B258A,
        0x258B => &B258B,
        0x258C => &B258C,
        0x258D => &B258D,
        0x258E => &B258E,
        0x258F => &B258F,
        0x2590 => &B2590,
        0x2591 => &B2591,
        0x2592 => &B2592,
        0x2593 => &B2593,
        0x2594 => &B2594,
        0x2595 => &B2595,
        0x2596 => &B2596,
        0x2597 => &B2597,
        0x2598 => &B2598,
        0x2599 => &B2599,
        0x259A => &B259A,
        0x259B => &B259B,
        0x259C => &B259C,
        0x259D => &B259D,
        0x259E => &B259E,
        0x259F => &B259F,
        _ => &[],
    }
}

/// Convert one eighth-rect to a device-px [`Prim`] (top-origin). Min edges floor,
/// max edges ceil — the aliased pixel-alignment the spike derived (its CG
/// bottom-origin flip algebraically simplifies to floor-top / ceil-bottom here).
fn eighth_to_prim(e: Eighth, cw_px: i32, ch_px: i32) -> Prim {
    let cw = cw_px as f32;
    let ch = ch_px as f32;
    let x0 = (e.x0 as f32 * cw / 8.0).floor() as i32;
    let x1 = (e.x1 as f32 * cw / 8.0).ceil() as i32;
    let y0 = (e.y0 as f32 * ch / 8.0).floor() as i32;
    let y1 = (e.y1 as f32 * ch / 8.0).ceil() as i32;
    // Quantize coverage to 8-bit BEFORE the curve, matching the spike (CG stores
    // alpha .25 as 64/255); the curve itself is applied at paint time.
    let coverage = (e.coverage * 255.0).round() / 255.0;
    Prim { x0, y0, x1, y1, coverage }
}

fn block_glyph(cp: u32, cw_px: i32, ch_px: i32) -> Glyph {
    let fills = block_eighths(cp)
        .iter()
        .map(|&r| eighth_to_prim(r, cw_px, ch_px))
        .collect();
    Glyph {
        fills,
        segments: Vec::new(),
    }
}

// ---- box-drawing lines U+2500–257F (SwiftTerm BoxDrawingRenderer port) -------

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineStyle {
    None,
    Light,
    Heavy,
    Double,
}

#[derive(Clone, Copy)]
struct Lines {
    up: LineStyle,
    right: LineStyle,
    down: LineStyle,
    left: LineStyle,
}

const fn lines(up: LineStyle, right: LineStyle, down: LineStyle, left: LineStyle) -> Lines {
    Lines { up, right, down, left }
}

use LineStyle::{Double as D, Heavy as H, Light as L, None as N};

/// Accumulates the device-px primitives for one box-drawing cell.
struct Canvas {
    cw: i32,
    ch: i32,
    light: i32,
    fills: Vec<Prim>,
    segments: Vec<Segment>,
}

impl Canvas {
    fn heavy(&self) -> i32 {
        (self.light * 2).max(1)
    }

    /// Fill an axis-aligned box (top-origin, clamped to the cell). Degenerate
    /// boxes are dropped. Mirrors the fork's `BoxDrawingCanvas.box`, minus the
    /// bottom-origin flip (see the module docs).
    fn boxfill(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        let x0 = x0.clamp(0, self.cw);
        let x1 = x1.clamp(0, self.cw);
        let y0 = y0.clamp(0, self.ch);
        let y1 = y1.clamp(0, self.ch);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        self.fills.push(Prim {
            x0,
            y0,
            x1,
            y1,
            coverage: 1.0,
        });
    }

    fn hline(&mut self, x1: i32, x2: i32, y: i32, thickness: i32) {
        self.boxfill(x1, y, x2, y + thickness);
    }

    fn vline(&mut self, y1: i32, y2: i32, x: i32, thickness: i32) {
        self.boxfill(x, y1, x + thickness, y2);
    }

    fn hline_middle(&mut self, thickness: i32) {
        let y = sub_clamped(self.ch, thickness) / 2;
        self.hline(0, self.cw, y, thickness);
    }

    fn vline_middle(&mut self, thickness: i32) {
        let x = sub_clamped(self.cw, thickness) / 2;
        self.vline(0, self.ch, x, thickness);
    }
}

fn sub_clamped(value: i32, subtract: i32) -> i32 {
    (value - subtract).max(0)
}

fn add_clamped(value: i32, add: i32, max_value: i32) -> i32 {
    (value + add).min(max_value)
}

fn box_glyph(cp: u32, cw_px: i32, ch_px: i32, light_px: i32) -> Glyph {
    let light = light_px.max(1);
    let heavy = (light * 2).max(1);
    let mut c = Canvas {
        cw: cw_px,
        ch: ch_px,
        light,
        fills: Vec::new(),
        segments: Vec::new(),
    };

    match cp {
        0x2500 => lines_char(lines(N, L, N, L), &mut c),
        0x2501 => lines_char(lines(N, H, N, H), &mut c),
        0x2502 => lines_char(lines(L, N, L, N), &mut c),
        0x2503 => lines_char(lines(H, N, H, N), &mut c),
        0x2504 => dash_h(3, light, (light).max(4), &mut c),
        0x2505 => dash_h(3, heavy, (light).max(4), &mut c),
        0x2506 => dash_v(3, light, (light).max(4), &mut c),
        0x2507 => dash_v(3, heavy, (light).max(4), &mut c),
        0x2508 => dash_h(4, light, (light).max(4), &mut c),
        0x2509 => dash_h(4, heavy, (light).max(4), &mut c),
        0x250a => dash_v(4, light, (light).max(4), &mut c),
        0x250b => dash_v(4, heavy, (light).max(4), &mut c),
        0x250c => lines_char(lines(N, L, L, N), &mut c),
        0x250d => lines_char(lines(N, H, L, N), &mut c),
        0x250e => lines_char(lines(N, L, H, N), &mut c),
        0x250f => lines_char(lines(N, H, H, N), &mut c),
        0x2510 => lines_char(lines(N, N, L, L), &mut c),
        0x2511 => lines_char(lines(N, N, L, H), &mut c),
        0x2512 => lines_char(lines(N, N, H, L), &mut c),
        0x2513 => lines_char(lines(N, N, H, H), &mut c),
        0x2514 => lines_char(lines(L, L, N, N), &mut c),
        0x2515 => lines_char(lines(L, H, N, N), &mut c),
        0x2516 => lines_char(lines(H, L, N, N), &mut c),
        0x2517 => lines_char(lines(H, H, N, N), &mut c),
        0x2518 => lines_char(lines(L, N, N, L), &mut c),
        0x2519 => lines_char(lines(L, N, N, H), &mut c),
        0x251a => lines_char(lines(H, N, N, L), &mut c),
        0x251b => lines_char(lines(H, N, N, H), &mut c),
        0x251c => lines_char(lines(L, L, L, N), &mut c),
        0x251d => lines_char(lines(L, H, L, N), &mut c),
        0x251e => lines_char(lines(H, L, L, N), &mut c),
        0x251f => lines_char(lines(L, L, H, N), &mut c),
        0x2520 => lines_char(lines(H, L, H, N), &mut c),
        0x2521 => lines_char(lines(H, H, L, N), &mut c),
        0x2522 => lines_char(lines(L, H, H, N), &mut c),
        0x2523 => lines_char(lines(H, H, H, N), &mut c),
        0x2524 => lines_char(lines(L, N, L, L), &mut c),
        0x2525 => lines_char(lines(L, N, L, H), &mut c),
        0x2526 => lines_char(lines(H, N, L, L), &mut c),
        0x2527 => lines_char(lines(L, N, H, L), &mut c),
        0x2528 => lines_char(lines(H, N, H, L), &mut c),
        0x2529 => lines_char(lines(H, N, L, H), &mut c),
        0x252a => lines_char(lines(L, N, H, H), &mut c),
        0x252b => lines_char(lines(H, N, H, H), &mut c),
        0x252c => lines_char(lines(N, L, L, L), &mut c),
        0x252d => lines_char(lines(N, L, L, H), &mut c),
        0x252e => lines_char(lines(N, H, L, L), &mut c),
        0x252f => lines_char(lines(N, H, L, H), &mut c),
        0x2530 => lines_char(lines(N, L, H, L), &mut c),
        0x2531 => lines_char(lines(N, L, H, H), &mut c),
        0x2532 => lines_char(lines(N, H, H, L), &mut c),
        0x2533 => lines_char(lines(N, H, H, H), &mut c),
        0x2534 => lines_char(lines(L, L, N, L), &mut c),
        0x2535 => lines_char(lines(L, L, N, H), &mut c),
        0x2536 => lines_char(lines(L, H, N, L), &mut c),
        0x2537 => lines_char(lines(L, H, N, H), &mut c),
        0x2538 => lines_char(lines(H, L, N, L), &mut c),
        0x2539 => lines_char(lines(H, L, N, H), &mut c),
        0x253a => lines_char(lines(H, H, N, L), &mut c),
        0x253b => lines_char(lines(H, H, N, H), &mut c),
        0x253c => lines_char(lines(L, L, L, L), &mut c),
        0x253d => lines_char(lines(L, L, L, H), &mut c),
        0x253e => lines_char(lines(L, H, L, L), &mut c),
        0x253f => lines_char(lines(L, H, L, H), &mut c),
        0x2540 => lines_char(lines(H, L, L, L), &mut c),
        0x2541 => lines_char(lines(L, L, H, L), &mut c),
        0x2542 => lines_char(lines(H, L, H, L), &mut c),
        0x2543 => lines_char(lines(H, L, L, H), &mut c),
        0x2544 => lines_char(lines(H, H, L, L), &mut c),
        0x2545 => lines_char(lines(L, L, H, H), &mut c),
        0x2546 => lines_char(lines(L, H, H, L), &mut c),
        0x2547 => lines_char(lines(H, H, L, H), &mut c),
        0x2548 => lines_char(lines(L, H, H, H), &mut c),
        0x2549 => lines_char(lines(H, L, H, H), &mut c),
        0x254a => lines_char(lines(H, H, H, L), &mut c),
        0x254b => lines_char(lines(H, H, H, H), &mut c),
        0x254c => dash_h(2, light, light, &mut c),
        0x254d => dash_h(2, heavy, heavy, &mut c),
        0x254e => dash_v(2, light, heavy, &mut c),
        0x254f => dash_v(2, heavy, heavy, &mut c),
        0x2550 => lines_char(lines(N, D, N, D), &mut c),
        0x2551 => lines_char(lines(D, N, D, N), &mut c),
        0x2552 => lines_char(lines(N, D, L, N), &mut c),
        0x2553 => lines_char(lines(N, L, D, N), &mut c),
        0x2554 => lines_char(lines(N, D, D, N), &mut c),
        0x2555 => lines_char(lines(N, N, L, D), &mut c),
        0x2556 => lines_char(lines(N, N, D, L), &mut c),
        0x2557 => lines_char(lines(N, N, D, D), &mut c),
        0x2558 => lines_char(lines(L, D, N, N), &mut c),
        0x2559 => lines_char(lines(D, L, N, N), &mut c),
        0x255a => lines_char(lines(D, D, N, N), &mut c),
        0x255b => lines_char(lines(L, N, N, D), &mut c),
        0x255c => lines_char(lines(D, N, N, L), &mut c),
        0x255d => lines_char(lines(D, N, N, D), &mut c),
        0x255e => lines_char(lines(L, D, L, N), &mut c),
        0x255f => lines_char(lines(D, L, D, N), &mut c),
        0x2560 => lines_char(lines(D, D, D, N), &mut c),
        0x2561 => lines_char(lines(L, N, L, D), &mut c),
        0x2562 => lines_char(lines(D, N, D, L), &mut c),
        0x2563 => lines_char(lines(D, N, D, D), &mut c),
        0x2564 => lines_char(lines(N, D, L, D), &mut c),
        0x2565 => lines_char(lines(N, L, D, L), &mut c),
        0x2566 => lines_char(lines(N, D, D, D), &mut c),
        0x2567 => lines_char(lines(L, D, N, D), &mut c),
        0x2568 => lines_char(lines(D, L, N, L), &mut c),
        0x2569 => lines_char(lines(D, D, N, D), &mut c),
        0x256a => lines_char(lines(L, D, L, D), &mut c),
        0x256b => lines_char(lines(D, L, D, L), &mut c),
        0x256c => lines_char(lines(D, D, D, D), &mut c),
        // Rounded corners: two straight arms + a quarter-round bezier between the
        // two connecting-edge midpoints, control at the cell center (its convex
        // side faces the rounded corner). Faithful to the fork's arc intent.
        0x256d => arc(N, L, L, N, &mut c), // ╭ right + down
        0x256e => arc(N, N, L, L, &mut c), // ╮ down + left
        0x256f => arc(L, N, N, L, &mut c), // ╯ up + left
        0x2570 => arc(L, L, N, N, &mut c), // ╰ up + right
        0x2571 => diagonal(true, false, &mut c),  // ╱
        0x2572 => diagonal(false, true, &mut c),  // ╲
        0x2573 => diagonal(true, true, &mut c),   // ╳
        0x2574 => lines_char(lines(N, N, N, L), &mut c),
        0x2575 => lines_char(lines(L, N, N, N), &mut c),
        0x2576 => lines_char(lines(N, L, N, N), &mut c),
        0x2577 => lines_char(lines(N, N, L, N), &mut c),
        0x2578 => lines_char(lines(N, N, N, H), &mut c),
        0x2579 => lines_char(lines(H, N, N, N), &mut c),
        0x257a => lines_char(lines(N, H, N, N), &mut c),
        0x257b => lines_char(lines(N, N, H, N), &mut c),
        0x257c => lines_char(lines(N, H, N, L), &mut c),
        0x257d => lines_char(lines(L, N, H, N), &mut c),
        0x257e => lines_char(lines(N, L, N, H), &mut c),
        0x257f => lines_char(lines(H, N, L, N), &mut c),
        _ => {}
    }

    Glyph {
        fills: c.fills,
        segments: c.segments,
    }
}

/// The general junction glyph (light/heavy/double arms). Ported directly from
/// the fork's `linesChar`; the arm-extent maths (`upBottom` … `rightLeft`) make
/// the four arms meet cleanly at the cell center so junctions and crossings line
/// up seamlessly with their neighbours.
fn lines_char(l: Lines, c: &mut Canvas) {
    let light = c.light.max(1);
    let heavy = c.heavy();

    let h_light_top = sub_clamped(c.ch, light) / 2;
    let h_light_bottom = add_clamped(h_light_top, light, c.ch);
    let h_heavy_top = sub_clamped(c.ch, heavy) / 2;
    let h_heavy_bottom = add_clamped(h_heavy_top, heavy, c.ch);
    let h_double_top = sub_clamped(h_light_top, light);
    let h_double_bottom = add_clamped(h_light_bottom, light, c.ch);

    let v_light_left = sub_clamped(c.cw, light) / 2;
    let v_light_right = add_clamped(v_light_left, light, c.cw);
    let v_heavy_left = sub_clamped(c.cw, heavy) / 2;
    let v_heavy_right = add_clamped(v_heavy_left, heavy, c.cw);
    let v_double_left = sub_clamped(v_light_left, light);
    let v_double_right = add_clamped(v_light_right, light, c.cw);

    let up_bottom = if l.left == H || l.right == H {
        h_heavy_bottom
    } else if l.left != l.right || l.down == l.up {
        if l.left == D || l.right == D {
            h_double_bottom
        } else {
            h_light_bottom
        }
    } else if l.left == N && l.right == N {
        h_light_bottom
    } else {
        h_light_top
    };

    let down_top = if l.left == H || l.right == H {
        h_heavy_top
    } else if l.left != l.right || l.up == l.down {
        if l.left == D || l.right == D {
            h_double_top
        } else {
            h_light_top
        }
    } else if l.left == N && l.right == N {
        h_light_top
    } else {
        h_light_bottom
    };

    let left_right = if l.up == H || l.down == H {
        v_heavy_right
    } else if l.up != l.down || l.left == l.right {
        if l.up == D || l.down == D {
            v_double_right
        } else {
            v_light_right
        }
    } else if l.up == N && l.down == N {
        v_light_right
    } else {
        v_light_left
    };

    let right_left = if l.up == H || l.down == H {
        v_heavy_left
    } else if l.up != l.down || l.right == l.left {
        if l.up == D || l.down == D {
            v_double_left
        } else {
            v_light_left
        }
    } else if l.up == N && l.down == N {
        v_light_left
    } else {
        v_light_right
    };

    match l.up {
        N => {}
        L => c.boxfill(v_light_left, 0, v_light_right, up_bottom),
        H => c.boxfill(v_heavy_left, 0, v_heavy_right, up_bottom),
        D => {
            let left_bottom = if l.left == D { h_light_top } else { up_bottom };
            let right_bottom = if l.right == D { h_light_top } else { up_bottom };
            c.boxfill(v_double_left, 0, v_light_left, left_bottom);
            c.boxfill(v_light_right, 0, v_double_right, right_bottom);
        }
    }

    match l.right {
        N => {}
        L => c.boxfill(right_left, h_light_top, c.cw, h_light_bottom),
        H => c.boxfill(right_left, h_heavy_top, c.cw, h_heavy_bottom),
        D => {
            let top_left = if l.up == D { v_light_right } else { right_left };
            let bottom_left = if l.down == D { v_light_right } else { right_left };
            c.boxfill(top_left, h_double_top, c.cw, h_light_top);
            c.boxfill(bottom_left, h_light_bottom, c.cw, h_double_bottom);
        }
    }

    match l.down {
        N => {}
        L => c.boxfill(v_light_left, down_top, v_light_right, c.ch),
        H => c.boxfill(v_heavy_left, down_top, v_heavy_right, c.ch),
        D => {
            let left_top = if l.left == D { h_light_bottom } else { down_top };
            let right_top = if l.right == D { h_light_bottom } else { down_top };
            c.boxfill(v_double_left, left_top, v_light_left, c.ch);
            c.boxfill(v_light_right, right_top, v_double_right, c.ch);
        }
    }

    match l.left {
        N => {}
        L => c.boxfill(0, h_light_top, left_right, h_light_bottom),
        H => c.boxfill(0, h_heavy_top, left_right, h_heavy_bottom),
        D => {
            let top_right = if l.up == D { v_light_left } else { left_right };
            let bottom_right = if l.down == D { v_light_left } else { left_right };
            c.boxfill(0, h_double_top, top_right, h_light_top);
            c.boxfill(0, h_light_bottom, bottom_right, h_double_bottom);
        }
    }
}

fn dash_h(count: i32, thickness: i32, desired_gap: i32, c: &mut Canvas) {
    if !(2..=4).contains(&count) {
        return;
    }
    if c.cw < count * 2 {
        c.hline_middle(c.light.max(1));
        return;
    }
    let gap = desired_gap.min(c.cw / (2 * count));
    let total_dash = c.cw - count * gap;
    let dash = total_dash / count;
    let remaining = total_dash % count;
    let y = sub_clamped(c.ch, thickness) / 2;
    let mut x = gap / 2;
    let mut extra = remaining;
    for _ in 0..count {
        let mut x1 = x + dash;
        if extra > 0 {
            extra -= 1;
            x1 += 1;
        }
        c.hline(x, x1, y, thickness);
        x = x1 + gap;
    }
}

fn dash_v(count: i32, thickness: i32, desired_gap: i32, c: &mut Canvas) {
    if !(2..=4).contains(&count) {
        return;
    }
    if c.ch < count * 2 {
        c.vline_middle(c.light.max(1));
        return;
    }
    let gap = desired_gap.min(c.ch / (2 * count));
    let total_dash = c.ch - count * gap;
    let dash = total_dash / count;
    let remaining = total_dash % count;
    let x = sub_clamped(c.cw, thickness) / 2;
    let mut y = 0;
    let mut extra = remaining;
    for _ in 0..count {
        let mut y1 = y + dash;
        if extra > 0 {
            extra -= 1;
            y1 += 1;
        }
        c.vline(y, y1, x, thickness);
        y = y1 + gap;
    }
}

/// A rounded corner connecting two of {up,right,down,left}. Draws each present
/// arm as a straight stem to the cell center, then a quarter-round bezier joining
/// the two connecting-edge midpoints (control at the center, so its convex side
/// faces the rounded corner).
fn arc(up: LineStyle, right: LineStyle, down: LineStyle, left: LineStyle, c: &mut Canvas) {
    let light = c.light.max(1);
    let cx = c.cw as f32 / 2.0;
    let cy = c.ch as f32 / 2.0;
    let x_left = sub_clamped(c.cw, light) / 2;
    let y_top = sub_clamped(c.ch, light) / 2;

    // Straight stems from each present arm's edge to the center band.
    if up != N {
        c.vline(0, y_top, x_left, light);
    }
    if down != N {
        c.vline(add_clamped(y_top, light, c.ch), c.ch, x_left, light);
    }
    if left != N {
        c.hline(0, x_left, y_top, light);
    }
    if right != N {
        c.hline(add_clamped(x_left, light, c.cw), c.cw, y_top, light);
    }

    // The two connecting edge midpoints, joined by a quad bulging to the center.
    let edge = |style: LineStyle, x: f32, y: f32| -> Option<(f32, f32)> {
        if style != N {
            Some((x, y))
        } else {
            None
        }
    };
    let pts: Vec<(f32, f32)> = [
        edge(up, cx, 0.0),
        edge(right, c.cw as f32, cy),
        edge(down, cx, c.ch as f32),
        edge(left, 0.0, cy),
    ]
    .into_iter()
    .flatten()
    .collect();
    if pts.len() == 2 {
        c.segments.push(Segment::Quad {
            x0: pts[0].0,
            y0: pts[0].1,
            cx,
            cy,
            x1: pts[1].0,
            y1: pts[1].1,
            width: light as f32,
        });
    }
}

fn diagonal(up_right: bool, down_right: bool, c: &mut Canvas) {
    let w = c.cw as f32;
    let h = c.ch as f32;
    let width = (c.light.max(1) as f32) + 1.0;
    if up_right {
        // ╱ bottom-left → top-right.
        c.segments.push(Segment::Line {
            x0: 0.0,
            y0: h,
            x1: w,
            y1: 0.0,
            width,
        });
    }
    if down_right {
        // ╲ top-left → bottom-right.
        c.segments.push(Segment::Line {
            x0: 0.0,
            y0: 0.0,
            x1: w,
            y1: h,
            width,
        });
    }
}

// ---- bg-luminance composition curve (spike port) ----------------------------

/// Display-gamma Rec-709 luminance of a `0xRRGGBB` color.
fn luminance(c: u32) -> f32 {
    let r = ((c >> 16) & 0xff) as f32 / 255.0;
    let g = ((c >> 8) & 0xff) as f32 / 255.0;
    let b = (c & 0xff) as f32 / 255.0;
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// The SwiftTerm `terminal_text_fragment_gray` composition curve (`.appleApprox`
/// params, kitty "1.7 30"), applied CPU-side to a uniform block-fill coverage —
/// the same curve the patched glyph shader runs, so shades composite exactly like
/// glyph coverage. Verbatim the spike's `apple_approx_coverage`.
pub(crate) fn apple_approx_coverage(cov: f32, fg: u32, bg: u32) -> f32 {
    let mix_factor = ((1.0 - luminance(fg) + luminance(bg)) * 0.5).clamp(0.0, 1.0);
    let curved = cov + (cov.powf(1.0 / 1.7) - cov) * mix_factor;
    (curved * 1.30).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The device-px cell used across the geometry tests: 8×16 logical px at a
    /// 2× backing scale, light stroke 2 device px (the `term-render` metrics).
    const CW: i32 = 16;
    const CH: i32 = 32;
    const LIGHT: i32 = 2;

    fn fills(cp: u32) -> Vec<Prim> {
        procedural_glyph(cp, CW, CH, LIGHT).unwrap().fills
    }

    /// Does any fill cover the device-px point `(x, y)`?
    fn covered(prims: &[Prim], x: i32, y: i32) -> bool {
        prims
            .iter()
            .any(|p| x >= p.x0 && x < p.x1 && y >= p.y0 && y < p.y1)
    }

    #[test]
    fn out_of_range_is_none() {
        assert!(procedural_glyph(0x24ff, CW, CH, LIGHT).is_none());
        assert!(procedural_glyph(0x25a0, CW, CH, LIGHT).is_none());
        assert!(procedural_glyph('A' as u32, CW, CH, LIGHT).is_none());
    }

    // ---- block-element orientation (top-origin, verbatim spike table) --------

    #[test]
    fn full_block_covers_whole_cell() {
        let p = fills(0x2588);
        assert!(covered(&p, CW / 2, CH / 2));
        assert!(covered(&p, 1, 1));
        assert!(covered(&p, CW - 1, CH - 1));
    }

    #[test]
    fn upper_half_block_is_top() {
        // ▀ U+2580 = upper 4 eighths → top half filled, bottom half empty.
        let p = fills(0x2580);
        assert!(covered(&p, CW / 2, CH / 4), "top must be filled");
        assert!(!covered(&p, CW / 2, 3 * CH / 4), "bottom must be empty");
    }

    #[test]
    fn lower_half_block_is_bottom() {
        // ▄ U+2584 = lower 4 eighths → bottom half filled, top half empty.
        let p = fills(0x2584);
        assert!(!covered(&p, CW / 2, CH / 4), "top must be empty");
        assert!(covered(&p, CW / 2, 3 * CH / 4), "bottom must be filled");
    }

    #[test]
    fn left_half_block_is_left() {
        // ▌ U+258C = left 4 eighths → left half filled, right half empty.
        let p = fills(0x258c);
        assert!(covered(&p, CW / 4, CH / 2), "left must be filled");
        assert!(!covered(&p, 3 * CW / 4, CH / 2), "right must be empty");
    }

    #[test]
    fn shades_are_graded_coverage() {
        // ░▒▓ U+2591/2/3 = full-cell fills at increasing coverage.
        let light = fills(0x2591)[0].coverage;
        let medium = fills(0x2592)[0].coverage;
        let dark = fills(0x2593)[0].coverage;
        assert!(light < medium && medium < dark, "{light} {medium} {dark}");
        assert!(light > 0.0 && dark < 1.0);
        // Each shade fills the whole cell.
        for cp in [0x2591u32, 0x2592, 0x2593] {
            let p = fills(cp);
            assert!(covered(&p, CW / 2, CH / 2));
        }
    }

    // ---- box-drawing orientation + seamless-join geometry --------------------

    #[test]
    fn horizontal_line_sits_in_the_vertical_middle() {
        // ─ U+2500 spans the full width at mid-height, nothing at top/bottom.
        let p = fills(0x2500);
        assert!(covered(&p, 1, CH / 2), "left edge on the line");
        assert!(covered(&p, CW - 1, CH / 2), "right edge on the line");
        assert!(!covered(&p, CW / 2, 1), "nothing near the top");
        assert!(!covered(&p, CW / 2, CH - 1), "nothing near the bottom");
    }

    #[test]
    fn vertical_line_sits_in_the_horizontal_middle() {
        // │ U+2502 spans the full height at mid-width.
        let p = fills(0x2502);
        assert!(covered(&p, CW / 2, 1), "top on the line");
        assert!(covered(&p, CW / 2, CH - 1), "bottom on the line");
        assert!(!covered(&p, 1, CH / 2), "nothing near the left");
        assert!(!covered(&p, CW - 1, CH / 2), "nothing near the right");
    }

    #[test]
    fn half_line_up_is_top_only() {
        // ╵ U+2575: a stub going UP → ink in the top half, not the bottom.
        let p = fills(0x2575);
        assert!(covered(&p, CW / 2, CH / 4), "top stub present");
        assert!(!covered(&p, CW / 2, 3 * CH / 4), "no bottom stub");
    }

    #[test]
    fn half_line_down_is_bottom_only() {
        // ╷ U+2577: a stub going DOWN → ink in the bottom half, not the top.
        let p = fills(0x2577);
        assert!(!covered(&p, CW / 2, CH / 4), "no top stub");
        assert!(covered(&p, CW / 2, 3 * CH / 4), "bottom stub present");
    }

    #[test]
    fn top_left_corner_goes_right_and_down() {
        // ┌ U+250C connects RIGHT + DOWN.
        let p = fills(0x250c);
        assert!(covered(&p, 3 * CW / 4, CH / 2), "right arm present");
        assert!(covered(&p, CW / 2, 3 * CH / 4), "down arm present");
        assert!(!covered(&p, CW / 4, CH / 2), "no left arm");
        assert!(!covered(&p, CW / 2, CH / 4), "no up arm");
    }

    #[test]
    fn bottom_left_corner_goes_right_and_up() {
        // └ U+2514 connects RIGHT + UP.
        let p = fills(0x2514);
        assert!(covered(&p, 3 * CW / 4, CH / 2), "right arm present");
        assert!(covered(&p, CW / 2, CH / 4), "up arm present");
        assert!(!covered(&p, CW / 4, CH / 2), "no left arm");
        assert!(!covered(&p, CW / 2, 3 * CH / 4), "no down arm");
    }

    #[test]
    fn cross_has_all_four_arms() {
        // ┼ U+253C connects all four directions.
        let p = fills(0x253c);
        assert!(covered(&p, CW / 2, CH / 4), "up");
        assert!(covered(&p, CW / 2, 3 * CH / 4), "down");
        assert!(covered(&p, CW / 4, CH / 2), "left");
        assert!(covered(&p, 3 * CW / 4, CH / 2), "right");
    }

    #[test]
    fn arcs_and_diagonals_produce_strokes() {
        // The rounded corners + diagonals render as stroked segments, not fills.
        for cp in [0x256du32, 0x256e, 0x256f, 0x2570] {
            let g = procedural_glyph(cp, CW, CH, LIGHT).unwrap();
            assert!(
                g.segments.iter().any(|s| matches!(s, Segment::Quad { .. })),
                "arc U+{cp:04X} should have a rounded segment"
            );
        }
        let cross = procedural_glyph(0x2573, CW, CH, LIGHT).unwrap();
        assert_eq!(cross.segments.len(), 2, "╳ is two diagonals");
    }

    #[test]
    fn apple_approx_is_identity_curve_for_full_coverage() {
        // Solid fills (coverage 1.0) survive the curve unchanged.
        assert!((apple_approx_coverage(1.0, 0xffffff, 0x000000) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn apple_approx_boosts_partial_coverage() {
        // A .5 shade on a neutral (white-on-black) pair is lifted by the ×1.30.
        let c = apple_approx_coverage(0.5, 0xffffff, 0x000000);
        assert!(c > 0.5 && c <= 1.0, "{c}");
    }
}
