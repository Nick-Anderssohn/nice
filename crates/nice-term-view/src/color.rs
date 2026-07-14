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

/// The color to draw the cursor cell's glyph in, reverse-video over an opaque
/// `accent` block: whichever of black / white contrasts more with `accent`.
/// Deliberately NOT the cell's own background (the zed-style "punched hole"):
/// a dark page color over a mid-tone accent clears WCAG thresholds yet still
/// reads muddy at cell size. Max-contrast black/white keeps the glyph crisp
/// against any accent — the worst case, a mid-gray accent, is still ≈4.5:1.
pub fn cursor_text_color(accent: u32) -> u32 {
    if contrast_ratio(0xFFFFFF, accent) >= contrast_ratio(0x000000, accent) {
        0xFFFFFF
    } else {
        0x000000
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
    fn cursor_text_color_is_white_on_dark_accents() {
        assert_eq!(cursor_text_color(0x000000), 0xffffff);
        assert_eq!(cursor_text_color(0x090705), 0xffffff);
        assert_eq!(cursor_text_color(0x2030a0), 0xffffff);
    }

    #[test]
    fn cursor_text_color_is_black_on_light_accents() {
        assert_eq!(cursor_text_color(0xffffff), 0x000000);
        assert_eq!(cursor_text_color(0xf0f0f0), 0x000000);
        // The reported readability case: over a mid salmon accent the old
        // cell-bg choice kept the theme's dark page color (muddy); the
        // max-contrast rule picks pure black.
        assert_eq!(cursor_text_color(0xe08070), 0x000000);
    }

    #[test]
    fn cursor_text_color_midgray_accent_stays_legible() {
        // Worst case for black-or-white: a mid-gray accent. Whichever side is
        // picked must still clear WCAG AA for large text (3.0).
        let accent = 0x777777;
        let chosen = cursor_text_color(accent);
        assert!(chosen == 0xffffff || chosen == 0x000000);
        assert!(contrast_ratio(chosen, accent) >= 3.0);
    }
}
