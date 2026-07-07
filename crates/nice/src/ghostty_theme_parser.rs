//! The Ghostty `key = value` terminal-theme file parser (R22).
//!
//! Ported verbatim from `Sources/Nice/Theme/GhosttyThemeParser.swift:29-137`
//! (the grammar) + `TerminalTheme.swift:42-50` (the `#rrggbb` / `rrggbb` hex
//! decode). The format is line-oriented; unknown keys are silently ignored so
//! future Ghostty additions do not break imports. The **caller** owns the theme
//! `id` / `display_name` / `scope` / `source` — the parser owns colors only and
//! produces a render-subset [`nice_term_view::TerminalTheme`] (the app crate
//! attaches the catalog metadata; nothing here leaks into `nice-term-*`,
//! TRANCHE-2-NOTES §4).
//!
//! Boundary note: this is an **app-crate** concern. The parser's output is a
//! plain [`nice_term_view::TerminalTheme`] value constructed app-side; the view
//! crate takes it as a parameter and has no hex parser of its own.

#![allow(dead_code)] // Consumed by slice 2 (catalog import) + R23's UI.

use nice_term_view::{TerminalColor, TerminalTheme};

/// The typed parse failure. Ports `GhosttyThemeParser.ParseError`
/// (`GhosttyThemeParser.swift:15-24`) case-for-case. R23 owns the
/// human-readable mapping (`ImportErrorWrapper`); R22 exports the typed error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GhosttyParseError {
    /// One or more palette indices in `0..16` were not provided. `indices` is
    /// sorted ascending (Swift emits `(0..<16).filter { … }`, already sorted).
    MissingPalette { indices: Vec<usize> },
    /// `background` or `foreground` was not provided (both are required).
    MissingRequiredKey { key: String },
    /// A hex color could not be decoded. `line` is **1-indexed**.
    InvalidHex { value: String, line: usize },
    /// A `palette = N=#…` line had a palette index outside `0..16`. `index` is
    /// signed because a negative literal (`palette = -1=#…`) reaches here as-is;
    /// `line` is 1-indexed.
    PaletteIndexOutOfRange { index: i64, line: usize },
}

/// Trim ASCII horizontal whitespace (space + tab) from both ends — matching
/// Swift's `CharacterSet.whitespaces` (space + tab, NOT newlines; lines are
/// already newline-split before this runs).
fn trim_ws(s: &str) -> &str {
    s.trim_matches(|c: char| c == ' ' || c == '\t')
}

/// Decode `#rrggbb` or `rrggbb` (case-insensitive, whitespace-trimmed) into a
/// [`TerminalColor`]. Exactly 6 hex digits after stripping one leading `#`, else
/// `None`. No 3-digit shorthand, no `rgb()`, no named colors, no alpha. Ports
/// `ThemeColor.init?(hex:)` (`TerminalTheme.swift:42-50`). The view crate has no
/// hex parser, so this reusable helper lives app-side.
pub fn parse_hex6(hex: &str) -> Option<TerminalColor> {
    let trimmed = trim_ws(hex);
    // Strip exactly one leading '#' (Swift's `removeFirst()` runs once).
    let body = trimmed.strip_prefix('#').unwrap_or(trimmed);
    // Swift checks `s.count == 6` on Characters; hex digits are ASCII, so char
    // count == byte count for any string `from_str_radix` would accept, but we
    // count chars to reject a 6-byte multibyte string exactly as Swift does.
    if body.chars().count() != 6 {
        return None;
    }
    let value = u32::from_str_radix(body, 16).ok()?;
    Some(TerminalColor::new(
        ((value >> 16) & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        (value & 0xff) as u8,
    ))
}

/// Parse a Ghostty theme file into the render-subset [`TerminalTheme`]. The
/// caller supplies `id`/`display_name`/`scope`; here we decode colors only and
/// set no metadata. Ports `GhosttyThemeParser.parse` (`GhosttyThemeParser.swift:
/// 29-137`) — same grammar, same deterministic post-loop validation order.
pub fn parse_ghostty_theme(source: &str) -> Result<TerminalTheme, GhosttyParseError> {
    let mut background: Option<TerminalColor> = None;
    let mut foreground: Option<TerminalColor> = None;
    let mut cursor: Option<TerminalColor> = None;
    let mut selection: Option<TerminalColor> = None;
    let mut palette: [Option<TerminalColor>; 16] = [None; 16];

    // Split on LF OR CR, keeping empty substrings so CRLF-terminated files parse
    // (the empty substring between \r and \n is a blank line, skipped below).
    // Line numbers are 1-indexed for errors.
    for (index, raw_line) in source.split(|c| c == '\n' || c == '\r').enumerate() {
        let line_number = index + 1;
        let trimmed = trim_ws(raw_line);
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }

        // Split on the FIRST '='; no '=' ⇒ the line is silently skipped.
        let Some(eq) = trimmed.find('=') else {
            continue;
        };
        let key = trim_ws(&trimmed[..eq]);
        let value = trim_ws(&trimmed[eq + 1..]);

        match key {
            "background" => {
                background = Some(parse_hex6(value).ok_or_else(|| {
                    GhosttyParseError::InvalidHex {
                        value: value.to_string(),
                        line: line_number,
                    }
                })?);
            }
            "foreground" => {
                foreground = Some(parse_hex6(value).ok_or_else(|| {
                    GhosttyParseError::InvalidHex {
                        value: value.to_string(),
                        line: line_number,
                    }
                })?);
            }
            "cursor-color" => {
                cursor = Some(parse_hex6(value).ok_or_else(|| {
                    GhosttyParseError::InvalidHex {
                        value: value.to_string(),
                        line: line_number,
                    }
                })?);
            }
            "selection-background" => {
                selection = Some(parse_hex6(value).ok_or_else(|| {
                    GhosttyParseError::InvalidHex {
                        value: value.to_string(),
                        line: line_number,
                    }
                })?);
            }
            "palette" => {
                // `N=#rrggbb` or `N=rrggbb`: split the VALUE on its first inner
                // '='. No inner '=' ⇒ InvalidHex (whole value).
                let Some(inner_eq) = value.find('=') else {
                    return Err(GhosttyParseError::InvalidHex {
                        value: value.to_string(),
                        line: line_number,
                    });
                };
                let index_str = trim_ws(&value[..inner_eq]);
                let hex_str = trim_ws(&value[inner_eq + 1..]);
                // Index must parse as an integer (else InvalidHex, whole value).
                let Ok(palette_index) = index_str.parse::<i64>() else {
                    return Err(GhosttyParseError::InvalidHex {
                        value: value.to_string(),
                        line: line_number,
                    });
                };
                // …and be in 0..16 (else PaletteIndexOutOfRange).
                if !(0..16).contains(&palette_index) {
                    return Err(GhosttyParseError::PaletteIndexOutOfRange {
                        index: palette_index,
                        line: line_number,
                    });
                }
                // The hex part must decode (else InvalidHex, the hex substring).
                let Some(color) = parse_hex6(hex_str) else {
                    return Err(GhosttyParseError::InvalidHex {
                        value: hex_str.to_string(),
                        line: line_number,
                    });
                };
                palette[palette_index as usize] = Some(color);
            }
            // Any other key ⇒ silently ignored (Swift `default:`), so future
            // Ghostty keys never break an import.
            _ => continue,
        }
    }

    // Deterministic post-loop validation: required keys first (background then
    // foreground), then palette completeness — a stable message regardless of
    // where the problem sits in the file (Swift `:109-124`).
    let background = background.ok_or_else(|| GhosttyParseError::MissingRequiredKey {
        key: "background".to_string(),
    })?;
    let foreground = foreground.ok_or_else(|| GhosttyParseError::MissingRequiredKey {
        key: "foreground".to_string(),
    })?;

    let missing: Vec<usize> = (0..16).filter(|i| palette[*i].is_none()).collect();
    if !missing.is_empty() {
        return Err(GhosttyParseError::MissingPalette { indices: missing });
    }

    let ansi: [TerminalColor; 16] = std::array::from_fn(|i| palette[i].unwrap());

    // The caller sets scope = Either (imported themes appear in both pickers).
    Ok(TerminalTheme {
        background,
        foreground,
        cursor,
        selection,
        ansi,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(r: u8, g: u8, b: u8) -> TerminalColor {
        TerminalColor::new(r, g, b)
    }

    // ---- parse_hex6 ----------------------------------------------------------

    #[test]
    fn hex6_accepts_hash_and_bare_and_is_case_insensitive() {
        assert_eq!(parse_hex6("#ff8800"), Some(c(0xff, 0x88, 0x00)));
        assert_eq!(parse_hex6("ff8800"), Some(c(0xff, 0x88, 0x00)));
        assert_eq!(parse_hex6("#FF8800"), Some(c(0xff, 0x88, 0x00)));
        // Whitespace-trimmed.
        assert_eq!(parse_hex6("  #abcdef  "), Some(c(0xab, 0xcd, 0xef)));
    }

    #[test]
    fn hex6_rejects_bad_lengths_and_non_hex() {
        assert_eq!(parse_hex6("#fff"), None); // 3-digit shorthand unsupported
        assert_eq!(parse_hex6("#fffff"), None); // 5 digits
        assert_eq!(parse_hex6("#fffffff"), None); // 7 digits
        assert_eq!(parse_hex6("##ffffff"), None); // only one '#' stripped ⇒ 7
        assert_eq!(parse_hex6("zzzzzz"), None); // non-hex
        assert_eq!(parse_hex6("rgb(0,0,0)"), None); // no rgb() form
        assert_eq!(parse_hex6(""), None);
    }

    // ---- happy path ----------------------------------------------------------

    /// A well-formed fixture exercising a comment line, a blank line, CRLF
    /// endings, an unknown key (ignored), and both `#rrggbb` and bare `rrggbb`
    /// value forms → the exact theme.
    #[test]
    fn parses_wellformed_fixture_with_comments_crlf_and_unknown_keys() {
        let mut src = String::new();
        src.push_str("# Example Ghostty theme\r\n");
        src.push_str("\r\n"); // blank line
        src.push_str("background = #101112\r\n");
        src.push_str("foreground = e0e1e2\r\n"); // bare rrggbb
        src.push_str("cursor-color = #abcdef\r\n");
        src.push_str("selection-background = 445566\r\n");
        src.push_str("font-family = Menlo\r\n"); // unknown key ⇒ ignored
        for i in 0..16u8 {
            // palette entries with a distinct, index-derived color.
            src.push_str(&format!("palette = {i}=#{i:02x}{i:02x}{i:02x}\r\n"));
        }

        let theme = parse_ghostty_theme(&src).expect("well-formed fixture parses");
        assert_eq!(theme.background, c(0x10, 0x11, 0x12));
        assert_eq!(theme.foreground, c(0xe0, 0xe1, 0xe2));
        assert_eq!(theme.cursor, Some(c(0xab, 0xcd, 0xef)));
        assert_eq!(theme.selection, Some(c(0x44, 0x55, 0x66)));
        for i in 0..16usize {
            let v = i as u8;
            assert_eq!(theme.ansi[i], c(v, v, v), "ansi[{i}]");
        }
    }

    #[test]
    fn cursor_and_selection_are_optional() {
        let mut src = String::from("background = #000000\nforeground = #ffffff\n");
        for i in 0..16u8 {
            src.push_str(&format!("palette = {i}=#000000\n"));
        }
        let theme = parse_ghostty_theme(&src).expect("parses without cursor/selection");
        assert_eq!(theme.cursor, None);
        assert_eq!(theme.selection, None);
    }

    #[test]
    fn line_with_no_equals_is_silently_skipped() {
        let mut src = String::from("this line has no equals sign\n");
        src.push_str("background = #000000\nforeground = #ffffff\n");
        for i in 0..16u8 {
            src.push_str(&format!("palette = {i}=#010203\n"));
        }
        let theme = parse_ghostty_theme(&src).expect("no-equals line skipped, rest parses");
        assert_eq!(theme.background, c(0, 0, 0));
        assert_eq!(theme.ansi[5], c(1, 2, 3));
    }

    // ---- deterministic validation order --------------------------------------

    /// Missing `background` is reported BEFORE missing `foreground` and before
    /// `MissingPalette`, even when all three are absent.
    #[test]
    fn missing_background_reported_before_foreground_and_palette() {
        // Nothing at all provided.
        let err = parse_ghostty_theme("# empty\n").unwrap_err();
        assert_eq!(
            err,
            GhosttyParseError::MissingRequiredKey {
                key: "background".to_string()
            }
        );
    }

    /// With `background` present but `foreground` and palette absent, foreground
    /// is reported before `MissingPalette`.
    #[test]
    fn missing_foreground_reported_before_palette() {
        let err = parse_ghostty_theme("background = #000000\n").unwrap_err();
        assert_eq!(
            err,
            GhosttyParseError::MissingRequiredKey {
                key: "foreground".to_string()
            }
        );
    }

    /// With both required keys present but some palette entries absent,
    /// `MissingPalette` carries the SORTED missing indices.
    #[test]
    fn missing_palette_lists_sorted_missing_indices() {
        // Provide only indices 0, 1, 15 — expect 2..=14 missing, ascending.
        let src = "background = #000000\nforeground = #ffffff\n\
                   palette = 0=#000000\npalette = 1=#000000\npalette = 15=#000000\n";
        let err = parse_ghostty_theme(src).unwrap_err();
        let expected: Vec<usize> = (2..=14).collect();
        assert_eq!(err, GhosttyParseError::MissingPalette { indices: expected });
    }

    // ---- InvalidHex / PaletteIndexOutOfRange line numbers ---------------------

    #[test]
    fn invalid_background_hex_reports_1indexed_line() {
        // Line 1 is a comment, line 2 blank, line 3 the bad background.
        let src = "# header\n\nbackground = nothex\n";
        let err = parse_ghostty_theme(src).unwrap_err();
        assert_eq!(
            err,
            GhosttyParseError::InvalidHex {
                value: "nothex".to_string(),
                line: 3
            }
        );
    }

    #[test]
    fn palette_index_16_is_out_of_range_with_line() {
        let src = "background = #000000\nforeground = #ffffff\npalette = 16=#000000\n";
        let err = parse_ghostty_theme(src).unwrap_err();
        assert_eq!(
            err,
            GhosttyParseError::PaletteIndexOutOfRange { index: 16, line: 3 }
        );
    }

    #[test]
    fn palette_negative_index_is_out_of_range() {
        let src = "background = #000000\nforeground = #ffffff\npalette = -1=#000000\n";
        let err = parse_ghostty_theme(src).unwrap_err();
        assert_eq!(
            err,
            GhosttyParseError::PaletteIndexOutOfRange { index: -1, line: 3 }
        );
    }

    #[test]
    fn palette_nonnumeric_index_is_invalid_hex_of_whole_value() {
        let src = "background = #000000\nforeground = #ffffff\npalette = x=#000000\n";
        let err = parse_ghostty_theme(src).unwrap_err();
        assert_eq!(
            err,
            GhosttyParseError::InvalidHex {
                value: "x=#000000".to_string(),
                line: 3
            }
        );
    }

    #[test]
    fn palette_value_with_no_inner_equals_is_invalid_hex() {
        let src = "background = #000000\nforeground = #ffffff\npalette = 0#000000\n";
        let err = parse_ghostty_theme(src).unwrap_err();
        assert_eq!(
            err,
            GhosttyParseError::InvalidHex {
                value: "0#000000".to_string(),
                line: 3
            }
        );
    }

    #[test]
    fn palette_bad_hex_reports_the_hex_substring_not_whole_value() {
        let src = "background = #000000\nforeground = #ffffff\npalette = 0=nothex\n";
        let err = parse_ghostty_theme(src).unwrap_err();
        assert_eq!(
            err,
            GhosttyParseError::InvalidHex {
                value: "nothex".to_string(),
                line: 3
            }
        );
    }
}
