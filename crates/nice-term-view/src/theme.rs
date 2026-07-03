//! The terminal-theme value type the renderer consumes — the render half of
//! T12, shaped exactly like `Sources/Nice/Theme/TerminalTheme.swift`.
//!
//! A [`TerminalTheme`] configures **only** the 16 named ANSI entries plus
//! `background` / `foreground` / `cursor` / `selection`, per scheme. It does
//! **not** carry the 256-color cube/grayscale ramp (indices 16–255) or 24-bit
//! truecolor — those are computed / passed straight through at resolve time
//! (see [`crate::color`]), matching today's `TerminalTheme.swift` (its Ghostty
//! importer rejects palette indices ≥ 16; that boundary is kept here).
//!
//! Colors are stored as 8-bit sRGB triples because that is how every source
//! format speaks (Ghostty's `#rrggbb`, iTerm2 `.itermcolors`, the canonical
//! theme specs) — mirroring `TerminalTheme.swift`'s `ThemeColor`. The catalog /
//! import UI and the full built-in theme set are R22; this crate ships only the
//! two Nice defaults the renderer's self-test needs.

/// An 8-bit sRGB color with implicit alpha = 1.0 — the `ThemeColor` shape from
/// `TerminalTheme.swift` (8-bit per channel matches every input format).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalColor {
    /// Red channel, 0–255.
    pub r: u8,
    /// Green channel, 0–255.
    pub g: u8,
    /// Blue channel, 0–255.
    pub b: u8,
}

impl TerminalColor {
    /// A color from its three 8-bit channels (mirrors `ThemeColor(_:_:_:)`).
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// A color from a packed `0xRRGGBB` value.
    pub const fn from_u32(rgb: u32) -> Self {
        Self {
            r: ((rgb >> 16) & 0xff) as u8,
            g: ((rgb >> 8) & 0xff) as u8,
            b: (rgb & 0xff) as u8,
        }
    }

    /// The packed `0xRRGGBB` value — the form the paint path threads through
    /// gpui's `rgb()` helper.
    pub const fn to_u32(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }
}

/// A complete terminal theme — the render-relevant subset of
/// `TerminalTheme.swift`'s `TerminalTheme` (id / displayName / scope / source
/// are catalog concerns owned by R22, not the renderer).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalTheme {
    /// The default background (painted behind every cell + the whole viewport).
    pub background: TerminalColor,
    /// The default foreground (default text color).
    pub foreground: TerminalColor,
    /// The cursor color. `None` => the caret follows the accent token (R2) —
    /// exactly `TerminalTheme.swift`'s "nil => caret follows `Tweaks.accent`".
    pub cursor: Option<TerminalColor>,
    /// The selection color. `None` => the renderer's default selection tint
    /// (selection *rendering* is a later slice; the value rides here now).
    pub selection: Option<TerminalColor>,
    /// The 16 ANSI palette entries, indices 0–7 normal then 8–15 bright — the
    /// only palette entries a theme configures.
    pub ansi: [TerminalColor; 16],
}

impl TerminalTheme {
    /// Nice's built-in dark terminal theme. Ported verbatim from
    /// `BuiltInTerminalThemes.niceDefaultDark` (`BuiltInTerminalThemes.swift`),
    /// the same literals the phase-0 aa-gamma spike used as `NICE_DARK`.
    pub const fn nice_default_dark() -> Self {
        Self {
            background: TerminalColor::new(0x09, 0x07, 0x05), // niceDefaultDark background
            foreground: TerminalColor::new(0xf4, 0xf0, 0xef), // niceDefaultDark foreground
            cursor: None,                                     // caret follows accent
            selection: Some(TerminalColor::new(0x3a, 0x34, 0x30)), // niceDefaultDark selection
            ansi: [
                TerminalColor::new(0x09, 0x07, 0x05), // 0  black = niceBg3
                TerminalColor::new(0xc2, 0x36, 0x21), // 1  red
                TerminalColor::new(0x25, 0xbc, 0x24), // 2  green
                TerminalColor::new(0xad, 0xad, 0x27), // 3  yellow
                TerminalColor::new(0x49, 0x6e, 0xe1), // 4  blue
                TerminalColor::new(0xd3, 0x38, 0xd3), // 5  magenta
                TerminalColor::new(0x33, 0xbb, 0xc8), // 6  cyan
                TerminalColor::new(0xcb, 0xcc, 0xcd), // 7  white
                TerminalColor::new(0x81, 0x83, 0x83), // 8  bright black
                TerminalColor::new(0xfc, 0x5b, 0x47), // 9  bright red
                TerminalColor::new(0x31, 0xe7, 0x22), // 10 bright green
                TerminalColor::new(0xea, 0xd4, 0x23), // 11 bright yellow
                TerminalColor::new(0x6c, 0x8d, 0xff), // 12 bright blue
                TerminalColor::new(0xf9, 0x65, 0xf8), // 13 bright magenta
                TerminalColor::new(0x64, 0xe6, 0xe6), // 14 bright cyan
                TerminalColor::new(0xf4, 0xf0, 0xef), // 15 bright white = niceInk
            ],
        }
    }

    /// Nice's built-in light terminal theme. Ported verbatim from
    /// `BuiltInTerminalThemes.niceDefaultLight` (`BuiltInTerminalThemes.swift`),
    /// the same literals the phase-0 aa-gamma spike used as `NICE_LIGHT`.
    pub const fn nice_default_light() -> Self {
        Self {
            background: TerminalColor::new(0xff, 0xfc, 0xfc), // niceDefaultLight background
            foreground: TerminalColor::new(0x17, 0x13, 0x0f), // niceDefaultLight foreground
            cursor: None,                                     // caret follows accent
            selection: Some(TerminalColor::new(0xe8, 0xdf, 0xd6)), // niceDefaultLight selection
            ansi: [
                TerminalColor::new(0x17, 0x13, 0x0f), // 0  black = niceInk
                TerminalColor::new(0xb7, 0x40, 0x20), // 1  red
                TerminalColor::new(0x30, 0x81, 0x30), // 2  green
                TerminalColor::new(0xa6, 0x71, 0x0d), // 3  yellow (amber)
                TerminalColor::new(0x28, 0x60, 0xaf), // 4  blue
                TerminalColor::new(0x9b, 0x3b, 0x98), // 5  magenta
                TerminalColor::new(0x23, 0x85, 0x9b), // 6  cyan
                TerminalColor::new(0x7e, 0x76, 0x6c), // 7  white (muted gray)
                TerminalColor::new(0x5c, 0x53, 0x48), // 8  bright black
                TerminalColor::new(0xd4, 0x4c, 0x25), // 9  bright red
                TerminalColor::new(0x38, 0x9f, 0x38), // 10 bright green
                TerminalColor::new(0xc4, 0x8c, 0x18), // 11 bright yellow
                TerminalColor::new(0x34, 0x75, 0xcd), // 12 bright blue
                TerminalColor::new(0xb5, 0x47, 0xaf), // 13 bright magenta
                TerminalColor::new(0x28, 0x9c, 0xb2), // 14 bright cyan
                TerminalColor::new(0x17, 0x13, 0x0f), // 15 bright white — stays dark on light bg
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    //! Provenance-cited fixtures (crates/README.md "Fixture-provenance
    //! convention"): each expected value is an independent transcription of the
    //! cited `BuiltInTerminalThemes.swift` line, so a fat-fingered literal in
    //! either the theme table or the fixture fails the build.
    use super::*;

    #[test]
    fn to_from_u32_round_trips() {
        let c = TerminalColor::new(0x09, 0x07, 0x05);
        assert_eq!(c.to_u32(), 0x090705);
        assert_eq!(TerminalColor::from_u32(0x090705), c);
        assert_eq!(TerminalColor::from_u32(0xf4f0ef), TerminalColor::new(0xf4, 0xf0, 0xef));
    }

    #[test]
    fn nice_dark_matches_swift() {
        let t = TerminalTheme::nice_default_dark();
        // BuiltInTerminalThemes.swift niceDefaultDark bg/fg/selection.
        assert_eq!(t.background, TerminalColor::new(9, 7, 5));
        assert_eq!(t.foreground, TerminalColor::new(244, 240, 239));
        assert_eq!(t.cursor, None);
        assert_eq!(t.selection, Some(TerminalColor::new(58, 52, 48)));
        // A representative slice of the 16 ANSI entries (0, 1, 7, 8, 15).
        assert_eq!(t.ansi[0], TerminalColor::new(9, 7, 5)); // black = niceBg3
        assert_eq!(t.ansi[1], TerminalColor::new(194, 54, 33)); // red
        assert_eq!(t.ansi[7], TerminalColor::new(203, 204, 205)); // white
        assert_eq!(t.ansi[8], TerminalColor::new(129, 131, 131)); // bright black
        assert_eq!(t.ansi[15], TerminalColor::new(244, 240, 239)); // bright white = niceInk
    }

    #[test]
    fn nice_light_matches_swift() {
        let t = TerminalTheme::nice_default_light();
        // BuiltInTerminalThemes.swift niceDefaultLight bg/fg/selection.
        assert_eq!(t.background, TerminalColor::new(255, 252, 252));
        assert_eq!(t.foreground, TerminalColor::new(23, 19, 15));
        assert_eq!(t.cursor, None);
        assert_eq!(t.selection, Some(TerminalColor::new(232, 223, 214)));
        assert_eq!(t.ansi[0], TerminalColor::new(23, 19, 15)); // black = niceInk
        assert_eq!(t.ansi[4], TerminalColor::new(40, 96, 175)); // blue
        assert_eq!(t.ansi[15], TerminalColor::new(23, 19, 15)); // bright white stays dark
    }
}
