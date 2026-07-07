//! The bundled terminal-theme table (R22).
//!
//! Transcribed literal-for-literal from
//! `Sources/Nice/Theme/BuiltInTerminalThemes.swift` — the 12 built-in themes in
//! `all` order (`:21-34`, the fixed picker order; built-ins are never
//! re-sorted). Each row pairs the catalog metadata ([`CatalogEntry`]'s `id` /
//! `display_name` / `scope`) with a render-subset
//! [`nice_term_view::TerminalTheme`] payload (bg/fg/cursor?/selection?/16 ANSI).
//!
//! The two Nice defaults' color payloads come from the view-crate const ctors
//! [`TerminalTheme::nice_default_light`] / [`nice_default_dark`] — single source
//! of truth, already provenance-pinned in `nice-term-view/src/theme.rs:133-177`;
//! they are NOT re-transcribed here. The other 10 are literal tables with a
//! provenance fixture per theme (double-entry, crates/README "Fixture-provenance
//! convention") so a fat-fingered value fails the build.
//!
//! Boundary note (TRANCHE-2-NOTES §4): this is an **app-crate** concern. It
//! composes catalog metadata over the render subset; nothing leaks into
//! `nice-term-*`.

#![allow(dead_code)] // Consumed by slice 2 (catalog table swap).

use crate::terminal_theme_catalog::ThemeScope;
use nice_term_view::{TerminalColor, TerminalTheme};

/// One bundled built-in: the metadata half of a picker row paired with the
/// concrete render theme it resolves to. Slice 2's catalog reads
/// [`built_in_terminal_themes`] to build its lookup table.
#[derive(Clone, Debug)]
pub struct BuiltInTheme {
    /// The stable slug persisted in `terminal_theme_light_id` / `…dark_id`.
    pub id: &'static str,
    /// The human-facing picker name.
    pub display_name: &'static str,
    /// Which scheme(s) this theme is offered in (no built-in is `Either`).
    pub scope: ThemeScope,
    /// The render payload `resolve` returns.
    pub theme: TerminalTheme,
}

const fn c(r: u8, g: u8, b: u8) -> TerminalColor {
    TerminalColor::new(r, g, b)
}

/// The ordered 12-entry built-in table, in `BuiltInTerminalThemes.all` order
/// (`BuiltInTerminalThemes.swift:21-34`). Built-ins are presented in this order;
/// they are never re-sorted.
pub fn built_in_terminal_themes() -> Vec<BuiltInTheme> {
    vec![
        BuiltInTheme {
            id: "nice-default-light",
            display_name: "Nice Default (Light)",
            scope: ThemeScope::Light,
            theme: TerminalTheme::nice_default_light(),
        },
        BuiltInTheme {
            id: "nice-default-dark",
            display_name: "Nice Default (Dark)",
            scope: ThemeScope::Dark,
            theme: TerminalTheme::nice_default_dark(),
        },
        BuiltInTheme {
            id: "solarized-light",
            display_name: "Solarized Light",
            scope: ThemeScope::Light,
            theme: solarized_light(),
        },
        BuiltInTheme {
            id: "solarized-dark",
            display_name: "Solarized Dark",
            scope: ThemeScope::Dark,
            theme: solarized_dark(),
        },
        BuiltInTheme {
            id: "dracula",
            display_name: "Dracula",
            scope: ThemeScope::Dark,
            theme: dracula(),
        },
        BuiltInTheme {
            id: "nord",
            display_name: "Nord",
            scope: ThemeScope::Dark,
            theme: nord(),
        },
        BuiltInTheme {
            id: "gruvbox-light",
            display_name: "Gruvbox Light",
            scope: ThemeScope::Light,
            theme: gruvbox_light(),
        },
        BuiltInTheme {
            id: "gruvbox-dark",
            display_name: "Gruvbox Dark",
            scope: ThemeScope::Dark,
            theme: gruvbox_dark(),
        },
        BuiltInTheme {
            id: "catppuccin-latte",
            display_name: "Catppuccin Latte",
            scope: ThemeScope::Light,
            theme: catppuccin_latte(),
        },
        BuiltInTheme {
            id: "catppuccin-mocha",
            display_name: "Catppuccin Mocha",
            scope: ThemeScope::Dark,
            theme: catppuccin_mocha(),
        },
        BuiltInTheme {
            id: "tokyo-night",
            display_name: "Tokyo Night",
            scope: ThemeScope::Dark,
            theme: tokyo_night(),
        },
        BuiltInTheme {
            id: "one-dark",
            display_name: "One Dark",
            scope: ThemeScope::Dark,
            theme: one_dark(),
        },
    ]
}

// ---- Solarized (BuiltInTerminalThemes.swift:112-168) ------------------------
// Ethan Schoonover, https://ethanschoonover.com/solarized/

/// `BuiltInTerminalThemes.solarizedLight` (`:112-139`).
const fn solarized_light() -> TerminalTheme {
    TerminalTheme {
        background: c(0xfd, 0xf6, 0xe3), // base3
        foreground: c(0x65, 0x7b, 0x83), // base00
        cursor: Some(c(0x58, 0x6e, 0x75)), // base01
        selection: Some(c(0xee, 0xe8, 0xd5)), // base2
        ansi: [
            c(0x07, 0x36, 0x42), // 0  black = base02
            c(0xdc, 0x32, 0x2f), // 1  red
            c(0x85, 0x99, 0x00), // 2  green
            c(0xb5, 0x89, 0x00), // 3  yellow
            c(0x26, 0x8b, 0xd2), // 4  blue
            c(0xd3, 0x36, 0x82), // 5  magenta
            c(0x2a, 0xa1, 0x98), // 6  cyan
            c(0xee, 0xe8, 0xd5), // 7  white = base2
            c(0x00, 0x2b, 0x36), // 8  bright black = base03
            c(0xcb, 0x4b, 0x16), // 9  bright red = orange
            c(0x58, 0x6e, 0x75), // 10 bright green = base01
            c(0x65, 0x7b, 0x83), // 11 bright yellow = base00
            c(0x83, 0x94, 0x96), // 12 bright blue = base0
            c(0x6c, 0x71, 0xc4), // 13 bright magenta = violet
            c(0x93, 0xa1, 0xa1), // 14 bright cyan = base1
            c(0xfd, 0xf6, 0xe3), // 15 bright white = base3
        ],
    }
}

/// `BuiltInTerminalThemes.solarizedDark` (`:141-168`).
const fn solarized_dark() -> TerminalTheme {
    TerminalTheme {
        background: c(0x00, 0x2b, 0x36), // base03
        foreground: c(0x83, 0x94, 0x96), // base0
        cursor: Some(c(0x93, 0xa1, 0xa1)), // base1
        selection: Some(c(0x07, 0x36, 0x42)), // base02
        ansi: [
            c(0x07, 0x36, 0x42), // 0  black = base02
            c(0xdc, 0x32, 0x2f), // 1  red
            c(0x85, 0x99, 0x00), // 2  green
            c(0xb5, 0x89, 0x00), // 3  yellow
            c(0x26, 0x8b, 0xd2), // 4  blue
            c(0xd3, 0x36, 0x82), // 5  magenta
            c(0x2a, 0xa1, 0x98), // 6  cyan
            c(0xee, 0xe8, 0xd5), // 7  white = base2
            c(0x00, 0x2b, 0x36), // 8  bright black = base03
            c(0xcb, 0x4b, 0x16), // 9  bright red = orange
            c(0x58, 0x6e, 0x75), // 10 bright green = base01
            c(0x65, 0x7b, 0x83), // 11 bright yellow = base00
            c(0x83, 0x94, 0x96), // 12 bright blue = base0
            c(0x6c, 0x71, 0xc4), // 13 bright magenta = violet
            c(0x93, 0xa1, 0xa1), // 14 bright cyan = base1
            c(0xfd, 0xf6, 0xe3), // 15 bright white = base3
        ],
    }
}

// ---- Dracula (BuiltInTerminalThemes.swift:173-200) --------------------------

/// `BuiltInTerminalThemes.dracula` (`:173-200`).
const fn dracula() -> TerminalTheme {
    TerminalTheme {
        background: c(0x28, 0x2a, 0x36),
        foreground: c(0xf8, 0xf8, 0xf2),
        cursor: Some(c(0xf8, 0xf8, 0xf2)),
        selection: Some(c(0x44, 0x47, 0x5a)),
        ansi: [
            c(0x21, 0x22, 0x2c), // 0  black
            c(0xff, 0x55, 0x55), // 1  red
            c(0x50, 0xfa, 0x7b), // 2  green
            c(0xf1, 0xfa, 0x8c), // 3  yellow
            c(0xbd, 0x93, 0xf9), // 4  blue
            c(0xff, 0x79, 0xc6), // 5  magenta
            c(0x8b, 0xe9, 0xfd), // 6  cyan
            c(0xf8, 0xf8, 0xf2), // 7  white
            c(0x62, 0x72, 0xa4), // 8  bright black
            c(0xff, 0x6e, 0x6e), // 9  bright red
            c(0x69, 0xff, 0x94), // 10 bright green
            c(0xff, 0xff, 0xa5), // 11 bright yellow
            c(0xd6, 0xac, 0xff), // 12 bright blue
            c(0xff, 0x92, 0xdf), // 13 bright magenta
            c(0xa4, 0xff, 0xff), // 14 bright cyan
            c(0xff, 0xff, 0xff), // 15 bright white
        ],
    }
}

// ---- Nord (BuiltInTerminalThemes.swift:205-232) -----------------------------

/// `BuiltInTerminalThemes.nord` (`:205-232`).
const fn nord() -> TerminalTheme {
    TerminalTheme {
        background: c(0x2e, 0x34, 0x40),
        foreground: c(0xd8, 0xde, 0xe9),
        cursor: Some(c(0xd8, 0xde, 0xe9)),
        selection: Some(c(0x43, 0x4c, 0x5e)),
        ansi: [
            c(0x3b, 0x42, 0x52), // 0  black
            c(0xbf, 0x61, 0x6a), // 1  red
            c(0xa3, 0xbe, 0x8c), // 2  green
            c(0xeb, 0xcb, 0x8b), // 3  yellow
            c(0x81, 0xa1, 0xc1), // 4  blue
            c(0xb4, 0x8e, 0xad), // 5  magenta
            c(0x88, 0xc0, 0xd0), // 6  cyan
            c(0xe5, 0xe9, 0xf0), // 7  white
            c(0x4c, 0x56, 0x6a), // 8  bright black
            c(0xbf, 0x61, 0x6a), // 9  bright red
            c(0xa3, 0xbe, 0x8c), // 10 bright green
            c(0xeb, 0xcb, 0x8b), // 11 bright yellow
            c(0x81, 0xa1, 0xc1), // 12 bright blue
            c(0xb4, 0x8e, 0xad), // 13 bright magenta
            c(0x8f, 0xbc, 0xbb), // 14 bright cyan
            c(0xec, 0xef, 0xf4), // 15 bright white
        ],
    }
}

// ---- Gruvbox (BuiltInTerminalThemes.swift:237-293) --------------------------
// morhetz/gruvbox medium-contrast palette.

/// `BuiltInTerminalThemes.gruvboxLight` (`:237-264`).
const fn gruvbox_light() -> TerminalTheme {
    TerminalTheme {
        background: c(0xfb, 0xf1, 0xc7),
        foreground: c(0x3c, 0x38, 0x36),
        cursor: Some(c(0x3c, 0x38, 0x36)),
        selection: Some(c(0xeb, 0xdb, 0xb2)),
        ansi: [
            c(0xfb, 0xf1, 0xc7), // 0  black (bg0)
            c(0xcc, 0x24, 0x1d), // 1  red
            c(0x98, 0x97, 0x1a), // 2  green
            c(0xd7, 0x99, 0x21), // 3  yellow
            c(0x45, 0x85, 0x88), // 4  blue
            c(0xb1, 0x62, 0x86), // 5  magenta
            c(0x68, 0x9d, 0x6a), // 6  cyan (aqua)
            c(0x7c, 0x6f, 0x64), // 7  white (fg4)
            c(0x92, 0x83, 0x74), // 8  bright black (gray)
            c(0x9d, 0x00, 0x06), // 9  bright red
            c(0x79, 0x74, 0x0e), // 10 bright green
            c(0xb5, 0x76, 0x14), // 11 bright yellow
            c(0x07, 0x66, 0x78), // 12 bright blue
            c(0x8f, 0x3f, 0x71), // 13 bright magenta
            c(0x42, 0x7b, 0x58), // 14 bright cyan (aqua)
            c(0x3c, 0x38, 0x36), // 15 bright white (fg0)
        ],
    }
}

/// `BuiltInTerminalThemes.gruvboxDark` (`:266-293`).
const fn gruvbox_dark() -> TerminalTheme {
    TerminalTheme {
        background: c(0x28, 0x28, 0x28),
        foreground: c(0xeb, 0xdb, 0xb2),
        cursor: Some(c(0xeb, 0xdb, 0xb2)),
        selection: Some(c(0x66, 0x5c, 0x54)),
        ansi: [
            c(0x28, 0x28, 0x28), // 0  black (bg0)
            c(0xcc, 0x24, 0x1d), // 1  red
            c(0x98, 0x97, 0x1a), // 2  green
            c(0xd7, 0x99, 0x21), // 3  yellow
            c(0x45, 0x85, 0x88), // 4  blue
            c(0xb1, 0x62, 0x86), // 5  magenta
            c(0x68, 0x9d, 0x6a), // 6  cyan (aqua)
            c(0xa8, 0x99, 0x84), // 7  white (fg4)
            c(0x92, 0x83, 0x74), // 8  bright black (gray)
            c(0xfb, 0x49, 0x34), // 9  bright red
            c(0xb8, 0xbb, 0x26), // 10 bright green
            c(0xfa, 0xbd, 0x2f), // 11 bright yellow
            c(0x83, 0xa5, 0x98), // 12 bright blue
            c(0xd3, 0x86, 0x9b), // 13 bright magenta
            c(0x8e, 0xc0, 0x7c), // 14 bright cyan (aqua)
            c(0xeb, 0xdb, 0xb2), // 15 bright white (fg0)
        ],
    }
}

// ---- Catppuccin (BuiltInTerminalThemes.swift:298-354) -----------------------
// https://github.com/catppuccin/catppuccin

/// `BuiltInTerminalThemes.catppuccinLatte` (`:298-325`).
const fn catppuccin_latte() -> TerminalTheme {
    TerminalTheme {
        background: c(0xef, 0xf1, 0xf5),
        foreground: c(0x4c, 0x4f, 0x69),
        cursor: Some(c(0xdc, 0x8a, 0x78)), // Rosewater
        selection: Some(c(0xac, 0xb0, 0xbe)), // Surface2
        ansi: [
            c(0x5c, 0x5f, 0x77), // 0  black
            c(0xd2, 0x0f, 0x39), // 1  red
            c(0x40, 0xa0, 0x2b), // 2  green
            c(0xdf, 0x8e, 0x1d), // 3  yellow
            c(0x1e, 0x66, 0xf5), // 4  blue
            c(0xea, 0x76, 0xcb), // 5  magenta (pink)
            c(0x17, 0x92, 0x99), // 6  cyan (teal)
            c(0xac, 0xb0, 0xbe), // 7  white
            c(0x6c, 0x6f, 0x85), // 8  bright black
            c(0xde, 0x29, 0x3e), // 9  bright red
            c(0x49, 0xaf, 0x3d), // 10 bright green
            c(0xee, 0xa0, 0x2d), // 11 bright yellow
            c(0x45, 0x6e, 0xff), // 12 bright blue
            c(0xfe, 0x85, 0xd8), // 13 bright magenta
            c(0x2d, 0x9f, 0xa8), // 14 bright cyan
            c(0xbc, 0xc0, 0xcc), // 15 bright white
        ],
    }
}

/// `BuiltInTerminalThemes.catppuccinMocha` (`:327-354`).
const fn catppuccin_mocha() -> TerminalTheme {
    TerminalTheme {
        background: c(0x1e, 0x1e, 0x2e),
        foreground: c(0xcd, 0xd6, 0xf4),
        cursor: Some(c(0xf5, 0xe0, 0xdc)), // Rosewater
        selection: Some(c(0x58, 0x5b, 0x70)), // Surface2
        ansi: [
            c(0x45, 0x47, 0x5a), // 0  black
            c(0xf3, 0x8b, 0xa8), // 1  red
            c(0xa6, 0xe3, 0xa1), // 2  green
            c(0xf9, 0xe2, 0xaf), // 3  yellow
            c(0x89, 0xb4, 0xfa), // 4  blue
            c(0xf5, 0xc2, 0xe7), // 5  magenta (pink)
            c(0x94, 0xe2, 0xd5), // 6  cyan (teal)
            c(0xa6, 0xad, 0xc8), // 7  white
            c(0x58, 0x5b, 0x70), // 8  bright black
            c(0xf3, 0x77, 0x99), // 9  bright red
            c(0x89, 0xd8, 0x8b), // 10 bright green
            c(0xeb, 0xd3, 0x91), // 11 bright yellow
            c(0x74, 0xa8, 0xfc), // 12 bright blue
            c(0xf2, 0xae, 0xde), // 13 bright magenta
            c(0x6b, 0xd7, 0xca), // 14 bright cyan
            c(0xba, 0xc2, 0xde), // 15 bright white
        ],
    }
}

// ---- Tokyo Night (BuiltInTerminalThemes.swift:359-386) ----------------------
// enkia/tokyo-night-vscode-theme — Ghostty's TokyoNight Night.

/// `BuiltInTerminalThemes.tokyoNight` (`:359-386`).
const fn tokyo_night() -> TerminalTheme {
    TerminalTheme {
        background: c(0x1a, 0x1b, 0x26),
        foreground: c(0xc0, 0xca, 0xf5),
        cursor: Some(c(0xc0, 0xca, 0xf5)),
        selection: Some(c(0x28, 0x34, 0x57)),
        ansi: [
            c(0x15, 0x16, 0x1e), // 0  black
            c(0xf7, 0x76, 0x8e), // 1  red
            c(0x9e, 0xce, 0x6a), // 2  green
            c(0xe0, 0xaf, 0x68), // 3  yellow
            c(0x7a, 0xa2, 0xf7), // 4  blue
            c(0xbb, 0x9a, 0xf7), // 5  magenta
            c(0x7d, 0xcf, 0xff), // 6  cyan
            c(0xa9, 0xb1, 0xd6), // 7  white
            c(0x41, 0x48, 0x68), // 8  bright black
            c(0xf7, 0x76, 0x8e), // 9  bright red
            c(0x9e, 0xce, 0x6a), // 10 bright green
            c(0xe0, 0xaf, 0x68), // 11 bright yellow
            c(0x7a, 0xa2, 0xf7), // 12 bright blue
            c(0xbb, 0x9a, 0xf7), // 13 bright magenta
            c(0x7d, 0xcf, 0xff), // 14 bright cyan
            c(0xc0, 0xca, 0xf5), // 15 bright white
        ],
    }
}

// ---- One Dark (BuiltInTerminalThemes.swift:391-418) -------------------------
// Atom's One Dark — Ghostty's "Atom One Dark".

/// `BuiltInTerminalThemes.oneDark` (`:391-418`).
const fn one_dark() -> TerminalTheme {
    TerminalTheme {
        background: c(0x21, 0x25, 0x2b),
        foreground: c(0xab, 0xb2, 0xbf),
        cursor: Some(c(0xab, 0xb2, 0xbf)),
        selection: Some(c(0x32, 0x38, 0x44)),
        ansi: [
            c(0x21, 0x25, 0x2b), // 0  black
            c(0xe0, 0x6c, 0x75), // 1  red
            c(0x98, 0xc3, 0x79), // 2  green
            c(0xe5, 0xc0, 0x7b), // 3  yellow
            c(0x61, 0xaf, 0xef), // 4  blue
            c(0xc6, 0x78, 0xdd), // 5  magenta
            c(0x56, 0xb6, 0xc2), // 6  cyan
            c(0xab, 0xb2, 0xbf), // 7  white
            c(0x76, 0x76, 0x76), // 8  bright black
            c(0xe0, 0x6c, 0x75), // 9  bright red
            c(0x98, 0xc3, 0x79), // 10 bright green
            c(0xe5, 0xc0, 0x7b), // 11 bright yellow
            c(0x61, 0xaf, 0xef), // 12 bright blue
            c(0xc6, 0x78, 0xdd), // 13 bright magenta
            c(0x56, 0xb6, 0xc2), // 14 bright cyan
            c(0xab, 0xb2, 0xbf), // 15 bright white
        ],
    }
}

#[cfg(test)]
mod tests {
    //! Provenance-cited fixtures (crates/README "Fixture-provenance
    //! convention"): each expected literal below is an INDEPENDENT transcription
    //! of the cited `BuiltInTerminalThemes.swift` line, so a fat-fingered value
    //! in either the table above or the fixture here fails the build. The two
    //! Nice defaults are checked against the view-crate const ctors (their own
    //! provenance lives in `nice-term-view/src/theme.rs`).
    use super::*;

    fn find(id: &str) -> BuiltInTheme {
        built_in_terminal_themes()
            .into_iter()
            .find(|t| t.id == id)
            .unwrap_or_else(|| panic!("built-in {id} missing"))
    }

    /// The `all` order and per-theme id/scope match `BuiltInTerminalThemes.swift:
    /// 21-34` exactly (built-ins are never re-sorted).
    #[test]
    fn all_order_ids_and_scopes_match_swift() {
        let table = built_in_terminal_themes();
        let expected: [(&str, &str, ThemeScope); 12] = [
            ("nice-default-light", "Nice Default (Light)", ThemeScope::Light),
            ("nice-default-dark", "Nice Default (Dark)", ThemeScope::Dark),
            ("solarized-light", "Solarized Light", ThemeScope::Light),
            ("solarized-dark", "Solarized Dark", ThemeScope::Dark),
            ("dracula", "Dracula", ThemeScope::Dark),
            ("nord", "Nord", ThemeScope::Dark),
            ("gruvbox-light", "Gruvbox Light", ThemeScope::Light),
            ("gruvbox-dark", "Gruvbox Dark", ThemeScope::Dark),
            ("catppuccin-latte", "Catppuccin Latte", ThemeScope::Light),
            ("catppuccin-mocha", "Catppuccin Mocha", ThemeScope::Dark),
            ("tokyo-night", "Tokyo Night", ThemeScope::Dark),
            ("one-dark", "One Dark", ThemeScope::Dark),
        ];
        assert_eq!(table.len(), 12);
        for (i, (id, name, scope)) in expected.iter().enumerate() {
            assert_eq!(table[i].id, *id, "row {i} id");
            assert_eq!(table[i].display_name, *name, "row {i} display_name");
            assert_eq!(table[i].scope, *scope, "row {i} scope");
        }
    }

    /// The two Nice defaults' payloads are exactly the view-crate const ctors —
    /// single source of truth, not re-transcribed here.
    #[test]
    fn nice_defaults_reuse_view_crate_ctors() {
        assert_eq!(find("nice-default-light").theme, TerminalTheme::nice_default_light());
        assert_eq!(find("nice-default-dark").theme, TerminalTheme::nice_default_dark());
    }

    // Each check below re-transcribes bg/fg/cursor/selection + a representative
    // slice of the ANSI ramp (0, 1, 7, 8, 15) straight from the cited Swift.

    #[test]
    fn solarized_light_provenance() {
        // BuiltInTerminalThemes.swift:112-139.
        let t = find("solarized-light").theme;
        assert_eq!(t.background, c(0xfd, 0xf6, 0xe3));
        assert_eq!(t.foreground, c(0x65, 0x7b, 0x83));
        assert_eq!(t.cursor, Some(c(0x58, 0x6e, 0x75)));
        assert_eq!(t.selection, Some(c(0xee, 0xe8, 0xd5)));
        assert_eq!(t.ansi[0], c(0x07, 0x36, 0x42));
        assert_eq!(t.ansi[1], c(0xdc, 0x32, 0x2f));
        assert_eq!(t.ansi[7], c(0xee, 0xe8, 0xd5));
        assert_eq!(t.ansi[8], c(0x00, 0x2b, 0x36));
        assert_eq!(t.ansi[15], c(0xfd, 0xf6, 0xe3));
    }

    #[test]
    fn solarized_dark_provenance() {
        // BuiltInTerminalThemes.swift:141-168.
        let t = find("solarized-dark").theme;
        assert_eq!(t.background, c(0x00, 0x2b, 0x36));
        assert_eq!(t.foreground, c(0x83, 0x94, 0x96));
        assert_eq!(t.cursor, Some(c(0x93, 0xa1, 0xa1)));
        assert_eq!(t.selection, Some(c(0x07, 0x36, 0x42)));
        assert_eq!(t.ansi[0], c(0x07, 0x36, 0x42));
        assert_eq!(t.ansi[1], c(0xdc, 0x32, 0x2f));
        assert_eq!(t.ansi[7], c(0xee, 0xe8, 0xd5));
        assert_eq!(t.ansi[8], c(0x00, 0x2b, 0x36));
        assert_eq!(t.ansi[15], c(0xfd, 0xf6, 0xe3));
    }

    #[test]
    fn dracula_provenance() {
        // BuiltInTerminalThemes.swift:173-200.
        let t = find("dracula").theme;
        assert_eq!(t.background, c(0x28, 0x2a, 0x36));
        assert_eq!(t.foreground, c(0xf8, 0xf8, 0xf2));
        assert_eq!(t.cursor, Some(c(0xf8, 0xf8, 0xf2)));
        assert_eq!(t.selection, Some(c(0x44, 0x47, 0x5a)));
        assert_eq!(t.ansi[0], c(0x21, 0x22, 0x2c));
        assert_eq!(t.ansi[1], c(0xff, 0x55, 0x55));
        assert_eq!(t.ansi[7], c(0xf8, 0xf8, 0xf2));
        assert_eq!(t.ansi[8], c(0x62, 0x72, 0xa4));
        assert_eq!(t.ansi[15], c(0xff, 0xff, 0xff));
    }

    #[test]
    fn nord_provenance() {
        // BuiltInTerminalThemes.swift:205-232.
        let t = find("nord").theme;
        assert_eq!(t.background, c(0x2e, 0x34, 0x40));
        assert_eq!(t.foreground, c(0xd8, 0xde, 0xe9));
        assert_eq!(t.cursor, Some(c(0xd8, 0xde, 0xe9)));
        assert_eq!(t.selection, Some(c(0x43, 0x4c, 0x5e)));
        assert_eq!(t.ansi[0], c(0x3b, 0x42, 0x52));
        assert_eq!(t.ansi[1], c(0xbf, 0x61, 0x6a));
        assert_eq!(t.ansi[7], c(0xe5, 0xe9, 0xf0));
        assert_eq!(t.ansi[8], c(0x4c, 0x56, 0x6a));
        assert_eq!(t.ansi[15], c(0xec, 0xef, 0xf4));
    }

    #[test]
    fn gruvbox_light_provenance() {
        // BuiltInTerminalThemes.swift:237-264.
        let t = find("gruvbox-light").theme;
        assert_eq!(t.background, c(0xfb, 0xf1, 0xc7));
        assert_eq!(t.foreground, c(0x3c, 0x38, 0x36));
        assert_eq!(t.cursor, Some(c(0x3c, 0x38, 0x36)));
        assert_eq!(t.selection, Some(c(0xeb, 0xdb, 0xb2)));
        assert_eq!(t.ansi[0], c(0xfb, 0xf1, 0xc7));
        assert_eq!(t.ansi[1], c(0xcc, 0x24, 0x1d));
        assert_eq!(t.ansi[7], c(0x7c, 0x6f, 0x64));
        assert_eq!(t.ansi[8], c(0x92, 0x83, 0x74));
        assert_eq!(t.ansi[15], c(0x3c, 0x38, 0x36));
    }

    #[test]
    fn gruvbox_dark_provenance() {
        // BuiltInTerminalThemes.swift:266-293.
        let t = find("gruvbox-dark").theme;
        assert_eq!(t.background, c(0x28, 0x28, 0x28));
        assert_eq!(t.foreground, c(0xeb, 0xdb, 0xb2));
        assert_eq!(t.cursor, Some(c(0xeb, 0xdb, 0xb2)));
        assert_eq!(t.selection, Some(c(0x66, 0x5c, 0x54)));
        assert_eq!(t.ansi[0], c(0x28, 0x28, 0x28));
        assert_eq!(t.ansi[1], c(0xcc, 0x24, 0x1d));
        assert_eq!(t.ansi[7], c(0xa8, 0x99, 0x84));
        assert_eq!(t.ansi[8], c(0x92, 0x83, 0x74));
        assert_eq!(t.ansi[15], c(0xeb, 0xdb, 0xb2));
    }

    #[test]
    fn catppuccin_latte_provenance() {
        // BuiltInTerminalThemes.swift:298-325.
        let t = find("catppuccin-latte").theme;
        assert_eq!(t.background, c(0xef, 0xf1, 0xf5));
        assert_eq!(t.foreground, c(0x4c, 0x4f, 0x69));
        assert_eq!(t.cursor, Some(c(0xdc, 0x8a, 0x78)));
        assert_eq!(t.selection, Some(c(0xac, 0xb0, 0xbe)));
        assert_eq!(t.ansi[0], c(0x5c, 0x5f, 0x77));
        assert_eq!(t.ansi[1], c(0xd2, 0x0f, 0x39));
        assert_eq!(t.ansi[7], c(0xac, 0xb0, 0xbe));
        assert_eq!(t.ansi[8], c(0x6c, 0x6f, 0x85));
        assert_eq!(t.ansi[15], c(0xbc, 0xc0, 0xcc));
    }

    #[test]
    fn catppuccin_mocha_provenance() {
        // BuiltInTerminalThemes.swift:327-354.
        let t = find("catppuccin-mocha").theme;
        assert_eq!(t.background, c(0x1e, 0x1e, 0x2e));
        assert_eq!(t.foreground, c(0xcd, 0xd6, 0xf4));
        assert_eq!(t.cursor, Some(c(0xf5, 0xe0, 0xdc)));
        assert_eq!(t.selection, Some(c(0x58, 0x5b, 0x70)));
        assert_eq!(t.ansi[0], c(0x45, 0x47, 0x5a));
        assert_eq!(t.ansi[1], c(0xf3, 0x8b, 0xa8));
        assert_eq!(t.ansi[7], c(0xa6, 0xad, 0xc8));
        assert_eq!(t.ansi[8], c(0x58, 0x5b, 0x70));
        assert_eq!(t.ansi[15], c(0xba, 0xc2, 0xde));
    }

    #[test]
    fn tokyo_night_provenance() {
        // BuiltInTerminalThemes.swift:359-386.
        let t = find("tokyo-night").theme;
        assert_eq!(t.background, c(0x1a, 0x1b, 0x26));
        assert_eq!(t.foreground, c(0xc0, 0xca, 0xf5));
        assert_eq!(t.cursor, Some(c(0xc0, 0xca, 0xf5)));
        assert_eq!(t.selection, Some(c(0x28, 0x34, 0x57)));
        assert_eq!(t.ansi[0], c(0x15, 0x16, 0x1e));
        assert_eq!(t.ansi[1], c(0xf7, 0x76, 0x8e));
        assert_eq!(t.ansi[7], c(0xa9, 0xb1, 0xd6));
        assert_eq!(t.ansi[8], c(0x41, 0x48, 0x68));
        assert_eq!(t.ansi[15], c(0xc0, 0xca, 0xf5));
    }

    #[test]
    fn one_dark_provenance() {
        // BuiltInTerminalThemes.swift:391-418.
        let t = find("one-dark").theme;
        assert_eq!(t.background, c(0x21, 0x25, 0x2b));
        assert_eq!(t.foreground, c(0xab, 0xb2, 0xbf));
        assert_eq!(t.cursor, Some(c(0xab, 0xb2, 0xbf)));
        assert_eq!(t.selection, Some(c(0x32, 0x38, 0x44)));
        assert_eq!(t.ansi[0], c(0x21, 0x25, 0x2b));
        assert_eq!(t.ansi[1], c(0xe0, 0x6c, 0x75));
        assert_eq!(t.ansi[7], c(0xab, 0xb2, 0xbf));
        assert_eq!(t.ansi[8], c(0x76, 0x76, 0x76));
        assert_eq!(t.ansi[15], c(0xab, 0xb2, 0xbf));
    }
}
