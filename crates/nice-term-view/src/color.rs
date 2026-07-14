//! The full terminal color model: resolving an `alacritty_terminal`
//! [`Color`] to a concrete `0xRRGGBB` value the paint path threads through
//! gpui's `rgb()` helper. Ported from the phase-0 aa-gamma spike
//! (`spikes/phase0-poc/aa-gamma/gpui-term-main/src/main.rs` `color_rgb` /
//! `xterm256`), the primary reference for this renderer.
//!
//! This is the whole model `alacritty_terminal` can emit:
//!
//! * **Named** colors — the 16 ANSI slots + default fg/bg. These are the only
//!   entries a [`TerminalTheme`] configures; they resolve through its table.
//! * **Indexed** SGR (`38;5;n` / `48;5;n`) — indices 0–15 alias the themed 16;
//!   indices 16–231 are the standard xterm 6×6×6 cube and 232–255 the 24-step
//!   grayscale ramp, both **computed** (not themed), matching every xterm.
//! * **Spec** — 24-bit truecolor (`38;2;r;g;b`), passed straight through.
//!
//! Text attributes (inverse/bold/dim/underline) are a later slice; this module
//! is pure color resolution and never inspects cell flags.

use alacritty_terminal::vte::ansi::{Color, NamedColor};

use crate::theme::TerminalTheme;

/// Resolve a 256-color palette index to `0xRRGGBB`.
///
/// * `0..=15` — the themed ANSI slots (index into the theme's 16-entry table).
/// * `16..=231` — the xterm 6×6×6 color cube: component 0 stays 0, otherwise
///   `v * 40 + 55` (the canonical non-linear step).
/// * `232..=255` — the 24-step grayscale ramp: `(i - 232) * 10 + 8` per channel.
pub fn xterm256(i: u8, theme: &TerminalTheme) -> u32 {
    match i {
        0..=15 => theme.ansi[i as usize].to_u32(),
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

/// Resolve any [`Color`] to `0xRRGGBB` against `theme`. `is_fg` disambiguates
/// how a default/unknown named color resolves (foreground vs background) — the
/// same fallback the spike's `color_rgb` uses.
pub fn resolve_color(color: Color, theme: &TerminalTheme, is_fg: bool) -> u32 {
    match color {
        // 24-bit truecolor, passed straight through.
        Color::Spec(rgb) => ((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | (rgb.b as u32),
        // 256-color indexed (0–15 themed, 16–255 computed).
        Color::Indexed(i) => xterm256(i, theme),
        // Named: the 16 ANSI slots + default fg/bg through the theme table.
        Color::Named(n) => match n {
            NamedColor::Foreground => theme.foreground.to_u32(),
            NamedColor::Background => theme.background.to_u32(),
            NamedColor::Black => theme.ansi[0].to_u32(),
            NamedColor::Red => theme.ansi[1].to_u32(),
            NamedColor::Green => theme.ansi[2].to_u32(),
            NamedColor::Yellow => theme.ansi[3].to_u32(),
            NamedColor::Blue => theme.ansi[4].to_u32(),
            NamedColor::Magenta => theme.ansi[5].to_u32(),
            NamedColor::Cyan => theme.ansi[6].to_u32(),
            NamedColor::White => theme.ansi[7].to_u32(),
            NamedColor::BrightBlack => theme.ansi[8].to_u32(),
            NamedColor::BrightRed => theme.ansi[9].to_u32(),
            NamedColor::BrightGreen => theme.ansi[10].to_u32(),
            NamedColor::BrightYellow => theme.ansi[11].to_u32(),
            NamedColor::BrightBlue => theme.ansi[12].to_u32(),
            NamedColor::BrightMagenta => theme.ansi[13].to_u32(),
            NamedColor::BrightCyan => theme.ansi[14].to_u32(),
            NamedColor::BrightWhite => theme.ansi[15].to_u32(),
            // Dim/foreground-adjacent and any future named color: fall back to
            // the default fg/bg per slot (spike `color_rgb` behavior).
            _ => {
                if is_fg {
                    theme.foreground.to_u32()
                } else {
                    theme.background.to_u32()
                }
            }
        },
    }
}

/// WCAG 2.0 relative luminance of an `0xRRGGBB` color: linearize each sRGB
/// channel (`c/255`, then `c/12.92` below 0.03928 else `((c+0.055)/1.055)^2.4`)
/// and weight `0.2126 R + 0.7152 G + 0.0722 B`. Ranges `0.0` (black) to `1.0`
/// (white).
pub fn relative_luminance(rgb: u32) -> f32 {
    let channel = |shift: u32| {
        let c = ((rgb >> shift) & 0xff) as f32 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

/// WCAG 2.0 contrast ratio between two `0xRRGGBB` colors: `(Lmax + 0.05) /
/// (Lmin + 0.05)` over their relative luminances, in `1.0..=21.0`.
pub fn contrast_ratio(a: u32, b: u32) -> f32 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Minimum WCAG contrast the accent must clear against the covered
/// character's own color to serve as the focused block color. WCAG AA for
/// large text / UI components; below it the block falls back to the
/// self-contrasting fg/bg swap.
pub const SMART_CURSOR_MIN_CONTRAST: f32 = 3.0;

/// Minimum contrast the accent must clear against the cell BACKGROUND for the
/// block itself to read against the page. Deliberately looser than
/// [`SMART_CURSOR_MIN_CONTRAST`]: that bar suits thin glyph strokes, but a
/// solid block is visible well below it — and mid-tone accents (Claude coral
/// on the light theme is ≈2.5–2.9:1 vs the page) sit just under 3.0, which
/// made the resting caret lose its accent everywhere on light themes.
pub const SMART_CURSOR_MIN_BLOCK_CONTRAST: f32 = 2.0;

/// `(block_color, glyph_color)` for the focused solid block cursor over a cell.
/// `has_ink == false` ⇒ `glyph_color` is unused by the caller (blank cell).
///
/// An iTerm2-style *smart* cursor color: the caret keeps its fixed **accent
/// identity** whenever the accent is readable in context, and degrades to a
/// self-contrasting **swap** only when the accent would clash with the glyph
/// underneath:
///
/// * **Inked cell** (`has_ink`): use the `accent` as the block iff it clears
///   [`SMART_CURSOR_MIN_CONTRAST`] against the character's own color
///   (`cell_fg`) and [`SMART_CURSOR_MIN_BLOCK_CONTRAST`] against the cell
///   background (`cell_bg`) — so the accent block is legible and the glyph
///   redrawn over it can still reach contrast. The glyph
///   then takes whichever of `{cell_bg, 0x000000, 0xFFFFFF}` has the highest
///   `contrast_ratio` against the chosen block. Otherwise FALL BACK TO THE
///   SWAP: block = `cell_fg`, glyph = `cell_bg` (the reverse-video pair — the
///   9be7152 behavior — self-contrasting by construction, so it reads exactly
///   as the cell's text does, inverted).
/// * **Blank cell** (`!has_ink`): no glyph to reveal, so block = `accent` when
///   it contrasts with `cell_bg`, else `cell_fg` (the caret never vanishes into
///   the page). `glyph_color` is irrelevant; the swap glyph is returned for
///   consistency.
///
/// Net effect: the cursor keeps its accent identity whenever the accent is
/// readable in context — notably on EMPTY cells, the most common resting
/// state — and degrades to the self-contrasting swap when the accent would
/// clash with the character under it (e.g. a mid-salmon accent over the
/// theme's light default foreground).
pub fn smart_cursor_colors(cell_fg: u32, cell_bg: u32, accent: u32, has_ink: bool) -> (u32, u32) {
    // The self-contrasting fallback: xterm/Alacritty reverse video.
    let swap = (cell_fg, cell_bg);

    if !has_ink {
        // No glyph under the caret. Keep the accent when it reads against the
        // page, else the cell foreground so the caret is always visible. The
        // glyph value is unused by the caller — return the swap glyph.
        let block = if contrast_ratio(accent, cell_bg) >= SMART_CURSOR_MIN_BLOCK_CONTRAST {
            accent
        } else {
            cell_fg
        };
        return (block, swap.1);
    }

    // Inked cell: the accent is usable only if it stands out from BOTH the
    // glyph and the cell background — else the redrawn glyph (or the block
    // against its surroundings) would muddy.
    if contrast_ratio(accent, cell_fg) >= SMART_CURSOR_MIN_CONTRAST
        && contrast_ratio(accent, cell_bg) >= SMART_CURSOR_MIN_BLOCK_CONTRAST
    {
        let block = accent;
        // Glyph = whichever candidate reads best over the accent block. The
        // cell's own background wins when it can (a "punched hole"); pure
        // black/white guarantee a legible floor for any accent.
        let glyph = [cell_bg, 0x000000, 0xFFFFFF]
            .into_iter()
            .max_by(|&a, &b| {
                contrast_ratio(a, block)
                    .partial_cmp(&contrast_ratio(b, block))
                    .expect("finite WCAG contrast ratios are always comparable")
            })
            .expect("candidate list is non-empty");
        (block, glyph)
    } else {
        swap
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_low_16_are_themed() {
        let theme = TerminalTheme::nice_default_dark();
        for i in 0u8..16 {
            assert_eq!(xterm256(i, &theme), theme.ansi[i as usize].to_u32());
        }
    }

    #[test]
    fn indexed_cube_corners_match_xterm_formula() {
        let theme = TerminalTheme::nice_default_dark();
        // Independent transcription of the 6×6×6 cube formula (v==0 -> 0 else
        // v*40+55), sampled at the eight corners.
        assert_eq!(xterm256(16, &theme), 0x000000); // (0,0,0)
        assert_eq!(xterm256(21, &theme), 0x0000ff); // (0,0,5)
        assert_eq!(xterm256(46, &theme), 0x00ff00); // (0,5,0)
        assert_eq!(xterm256(51, &theme), 0x00ffff); // (0,5,5)
        assert_eq!(xterm256(196, &theme), 0xff0000); // (5,0,0)
        assert_eq!(xterm256(201, &theme), 0xff00ff); // (5,0,5)
        assert_eq!(xterm256(226, &theme), 0xffff00); // (5,5,0)
        assert_eq!(xterm256(231, &theme), 0xffffff); // (5,5,5)
    }

    #[test]
    fn indexed_grayscale_ramp_matches_xterm_formula() {
        let theme = TerminalTheme::nice_default_dark();
        // (i-232)*10+8 per channel.
        assert_eq!(xterm256(232, &theme), 0x080808);
        assert_eq!(xterm256(240, &theme), 0x585858);
        assert_eq!(xterm256(255, &theme), 0xeeeeee);
    }

    #[test]
    fn truecolor_passes_through() {
        let theme = TerminalTheme::nice_default_dark();
        let c = Color::Spec(alacritty_terminal::vte::ansi::Rgb { r: 18, g: 52, b: 86 });
        assert_eq!(resolve_color(c, &theme, true), 0x123456);
    }

    #[test]
    fn named_resolves_through_theme() {
        let theme = TerminalTheme::nice_default_dark();
        assert_eq!(
            resolve_color(Color::Named(NamedColor::Red), &theme, true),
            theme.ansi[1].to_u32()
        );
        assert_eq!(
            resolve_color(Color::Named(NamedColor::Foreground), &theme, true),
            theme.foreground.to_u32()
        );
        assert_eq!(
            resolve_color(Color::Named(NamedColor::Background), &theme, false),
            theme.background.to_u32()
        );
    }

    #[test]
    fn contrast_ratio_black_white_is_21() {
        assert!((contrast_ratio(0x000000, 0xffffff) - 21.0).abs() < 1e-3);
        // Order-independent, and identical colors have ratio 1.0.
        assert!((contrast_ratio(0xffffff, 0x000000) - 21.0).abs() < 1e-3);
        assert!((contrast_ratio(0x123456, 0x123456) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn relative_luminance_is_monotonic() {
        assert!((relative_luminance(0x000000) - 0.0).abs() < 1e-6);
        assert!((relative_luminance(0xffffff) - 1.0).abs() < 1e-6);
        let mid = relative_luminance(0x808080);
        assert!(mid > 0.0 && mid < 1.0);
    }

    #[test]
    fn smart_cursor_salmon_over_light_fg_falls_back_to_swap() {
        // The user's reported case: a mid-salmon accent over the theme's light
        // default foreground on the dark page. contrast(salmon, fg) ≈ 1.97 < 3.0,
        // so the accent clashes with the character — fall back to the swap.
        let (block, glyph) = smart_cursor_colors(0xd8d8d8, 0x090705, 0xe08070, true);
        assert_eq!(block, 0xd8d8d8); // block == cell_fg (the swap)
        assert_eq!(glyph, 0x090705); // glyph == cell_bg (self-contrasting)
    }

    #[test]
    fn smart_cursor_salmon_blank_cell_keeps_accent() {
        // Blank cell on the dark page: contrast(salmon, bg) ≈ 7.17 ≥ 3.0, so the
        // caret keeps its accent identity at rest.
        let accent = 0xe08070;
        let (block, glyph) = smart_cursor_colors(0xd8d8d8, 0x090705, accent, false);
        assert_eq!(block, accent);
        // Accent branch taken — the returned glyph must clear the threshold too.
        assert!(contrast_ratio(glyph, block) >= SMART_CURSOR_MIN_CONTRAST);
    }

    #[test]
    fn smart_cursor_dark_blue_over_light_fg_on_dark_bg() {
        // Honest math: contrast(blue, fg=0xd8d8d8) ≈ 7.40 ≥ 3.0 PASSES, but
        // contrast(blue, bg=0x090705) ≈ 1.91 < 2.0 (the block bar) FAILS — the
        // accent must clear both, so this falls back to the swap (the accent
        // would vanish into the dark page around the block).
        let (block, glyph) = smart_cursor_colors(0xd8d8d8, 0x090705, 0x2030a0, true);
        assert_eq!(block, 0xd8d8d8); // swap: block == cell_fg
        assert_eq!(glyph, 0x090705); // swap: glyph == cell_bg
    }

    #[test]
    fn smart_cursor_inked_accent_branch_picks_max_contrast_glyph() {
        // Dark-blue accent that DOES clear both: over a light glyph on a white
        // cell bg. contrast(blue, fg) ≈ 7.40 and contrast(blue, white) ≈ 10.54,
        // both ≥ 3.0 → accent block. The glyph is the max-contrast candidate.
        let accent = 0x2030a0;
        let (block, glyph) = smart_cursor_colors(0xd8d8d8, 0xffffff, accent, true);
        assert_eq!(block, accent);
        // white (or cell_bg == white) beats black over the dark-blue block.
        assert!(contrast_ratio(glyph, block) >= SMART_CURSOR_MIN_CONTRAST);
    }

    #[test]
    fn smart_cursor_coral_on_light_theme_keeps_accent() {
        // The light (Claude-sync) theme regression: coral vs the light page is
        // ≈2.8–3.0:1 — under the 3.0 glyph bar but well over the 2.0 block bar.
        // The resting caret must stay coral, not fall back to the near-black fg.
        let (fg, bg, coral) = (0x3d3929, 0xfaf9f5, 0xd97757);
        let (block, _glyph) = smart_cursor_colors(fg, bg, coral, false);
        assert_eq!(block, coral);
        // Inked: contrast(coral, dark fg) ≥ 3.0 and the block bar passes too,
        // so text under the caret also keeps the coral block, with a
        // max-contrast glyph on top.
        let (block, glyph) = smart_cursor_colors(fg, bg, coral, true);
        assert_eq!(block, coral);
        assert!(contrast_ratio(glyph, block) >= SMART_CURSOR_MIN_CONTRAST);
    }

    #[test]
    fn smart_cursor_blank_accent_near_bg_uses_fg() {
        // Accent ≈ cell background (identical here): contrast(accent, bg) == 1.0
        // < 3.0, so the blank caret uses the cell foreground and never vanishes
        // into the page.
        let (block, _glyph) = smart_cursor_colors(0xd8d8d8, 0x090705, 0x090705, false);
        assert_eq!(block, 0xd8d8d8); // block == cell_fg
    }
}
