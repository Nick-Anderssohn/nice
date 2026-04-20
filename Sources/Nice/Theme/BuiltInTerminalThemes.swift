//
//  BuiltInTerminalThemes.swift
//  Nice
//
//  The bundled terminal themes. Values are lifted from each theme's
//  canonical published spec (Ghostty's bundled .conf files under
//  iTerm2-Color-Schemes are the preferred source, since they're the
//  format we'll later parse for user-imported themes).
//
//  Each theme's `ansi` array has exactly 16 entries in standard ANSI
//  order: indices 0–7 regular (black, red, green, yellow, blue,
//  magenta, cyan, white) then 8–15 bright.
//
//  When adding or editing a theme, cross-check against the upstream
//  spec — the inline comments exist so a typo is visible at a glance.
//

import Foundation

enum BuiltInTerminalThemes {
    static let all: [TerminalTheme] = [
        niceDefaultLight,
        niceDefaultDark,
        solarizedLight,
        solarizedDark,
        dracula,
        nord,
        gruvboxLight,
        gruvboxDark,
        catppuccinLatte,
        catppuccinMocha,
        tokyoNight,
        oneDark,
    ]

    // MARK: - Nice defaults

    /// Ported from `NiceANSIPalette.lightPalette`. Background/foreground
    /// pinned to the near-white / near-black values used by niceBg3 /
    /// niceInk in light mode.
    static let niceDefaultLight = TerminalTheme(
        id: "nice-default-light",
        displayName: "Nice Default (Light)",
        scope: .light,
        background: ThemeColor(0xff, 0xfc, 0xfc),
        foreground: ThemeColor(0x17, 0x13, 0x0f),
        cursor: nil,
        selection: nil,
        ansi: [
            ThemeColor(0x17, 0x13, 0x0f), // 0  black = niceInk
            ThemeColor(0xb7, 0x40, 0x20), // 1  red
            ThemeColor(0x30, 0x81, 0x30), // 2  green
            ThemeColor(0xa6, 0x71, 0x0d), // 3  yellow (amber)
            ThemeColor(0x28, 0x60, 0xaf), // 4  blue
            ThemeColor(0x9b, 0x3b, 0x98), // 5  magenta
            ThemeColor(0x23, 0x85, 0x9b), // 6  cyan
            ThemeColor(0x7e, 0x76, 0x6c), // 7  white (muted gray)
            ThemeColor(0x5c, 0x53, 0x48), // 8  bright black
            ThemeColor(0xd4, 0x4c, 0x25), // 9  bright red
            ThemeColor(0x38, 0x9f, 0x38), // 10 bright green
            ThemeColor(0xc4, 0x8c, 0x18), // 11 bright yellow
            ThemeColor(0x34, 0x75, 0xcd), // 12 bright blue
            ThemeColor(0xb5, 0x47, 0xaf), // 13 bright magenta
            ThemeColor(0x28, 0x9c, 0xb2), // 14 bright cyan
            ThemeColor(0x17, 0x13, 0x0f), // 15 bright white — stays dark on light bg
        ],
        source: .builtIn
    )

    /// Ported from `NiceANSIPalette.darkPalette`. Background/foreground
    /// pinned to the near-black / near-white values used by niceBg3 /
    /// niceInk in dark mode.
    static let niceDefaultDark = TerminalTheme(
        id: "nice-default-dark",
        displayName: "Nice Default (Dark)",
        scope: .dark,
        background: ThemeColor(0x09, 0x07, 0x05),
        foreground: ThemeColor(0xf4, 0xf0, 0xef),
        cursor: nil,
        selection: nil,
        ansi: [
            ThemeColor(0x09, 0x07, 0x05), // 0  black = niceBg3
            ThemeColor(0xc2, 0x36, 0x21), // 1  red
            ThemeColor(0x25, 0xbc, 0x24), // 2  green
            ThemeColor(0xad, 0xad, 0x27), // 3  yellow
            ThemeColor(0x49, 0x6e, 0xe1), // 4  blue
            ThemeColor(0xd3, 0x38, 0xd3), // 5  magenta
            ThemeColor(0x33, 0xbb, 0xc8), // 6  cyan
            ThemeColor(0xcb, 0xcc, 0xcd), // 7  white
            ThemeColor(0x81, 0x83, 0x83), // 8  bright black
            ThemeColor(0xfc, 0x5b, 0x47), // 9  bright red
            ThemeColor(0x31, 0xe7, 0x22), // 10 bright green
            ThemeColor(0xea, 0xd4, 0x23), // 11 bright yellow
            ThemeColor(0x6c, 0x8d, 0xff), // 12 bright blue
            ThemeColor(0xf9, 0x65, 0xf8), // 13 bright magenta
            ThemeColor(0x64, 0xe6, 0xe6), // 14 bright cyan
            ThemeColor(0xf4, 0xf0, 0xef), // 15 bright white = niceInk
        ],
        source: .builtIn
    )

    // MARK: - Solarized
    // Ethan Schoonover, https://ethanschoonover.com/solarized/
    //   base03  #002b36   base02  #073642   base01  #586e75   base00  #657b83
    //   base0   #839496   base1   #93a1a1   base2   #eee8d5   base3   #fdf6e3
    //   yellow  #b58900   orange  #cb4b16   red     #dc322f   magenta #d33682
    //   violet  #6c71c4   blue    #268bd2   cyan    #2aa198   green   #859900
    // ANSI mapping per spec: 0=base02, 7=base2, 8=base03, 15=base3,
    // bright-red=orange, bright-green=base01, bright-yellow=base00,
    // bright-blue=base0, bright-magenta=violet, bright-cyan=base1.

    static let solarizedLight = TerminalTheme(
        id: "solarized-light",
        displayName: "Solarized Light",
        scope: .light,
        background: ThemeColor(0xfd, 0xf6, 0xe3), // base3
        foreground: ThemeColor(0x65, 0x7b, 0x83), // base00
        cursor: ThemeColor(0x58, 0x6e, 0x75),     // base01 (spec's "content" cursor)
        selection: ThemeColor(0xee, 0xe8, 0xd5),  // base2
        ansi: [
            ThemeColor(0x07, 0x36, 0x42), // 0  black   = base02
            ThemeColor(0xdc, 0x32, 0x2f), // 1  red
            ThemeColor(0x85, 0x99, 0x00), // 2  green
            ThemeColor(0xb5, 0x89, 0x00), // 3  yellow
            ThemeColor(0x26, 0x8b, 0xd2), // 4  blue
            ThemeColor(0xd3, 0x36, 0x82), // 5  magenta
            ThemeColor(0x2a, 0xa1, 0x98), // 6  cyan
            ThemeColor(0xee, 0xe8, 0xd5), // 7  white   = base2
            ThemeColor(0x00, 0x2b, 0x36), // 8  bright black   = base03
            ThemeColor(0xcb, 0x4b, 0x16), // 9  bright red     = orange
            ThemeColor(0x58, 0x6e, 0x75), // 10 bright green   = base01
            ThemeColor(0x65, 0x7b, 0x83), // 11 bright yellow  = base00
            ThemeColor(0x83, 0x94, 0x96), // 12 bright blue    = base0
            ThemeColor(0x6c, 0x71, 0xc4), // 13 bright magenta = violet
            ThemeColor(0x93, 0xa1, 0xa1), // 14 bright cyan    = base1
            ThemeColor(0xfd, 0xf6, 0xe3), // 15 bright white   = base3
        ],
        source: .builtIn
    )

    static let solarizedDark = TerminalTheme(
        id: "solarized-dark",
        displayName: "Solarized Dark",
        scope: .dark,
        background: ThemeColor(0x00, 0x2b, 0x36), // base03
        foreground: ThemeColor(0x83, 0x94, 0x96), // base0
        cursor: ThemeColor(0x93, 0xa1, 0xa1),     // base1 (spec's "content" cursor)
        selection: ThemeColor(0x07, 0x36, 0x42),  // base02
        ansi: [
            ThemeColor(0x07, 0x36, 0x42), // 0  black   = base02
            ThemeColor(0xdc, 0x32, 0x2f), // 1  red
            ThemeColor(0x85, 0x99, 0x00), // 2  green
            ThemeColor(0xb5, 0x89, 0x00), // 3  yellow
            ThemeColor(0x26, 0x8b, 0xd2), // 4  blue
            ThemeColor(0xd3, 0x36, 0x82), // 5  magenta
            ThemeColor(0x2a, 0xa1, 0x98), // 6  cyan
            ThemeColor(0xee, 0xe8, 0xd5), // 7  white   = base2
            ThemeColor(0x00, 0x2b, 0x36), // 8  bright black   = base03
            ThemeColor(0xcb, 0x4b, 0x16), // 9  bright red     = orange
            ThemeColor(0x58, 0x6e, 0x75), // 10 bright green   = base01
            ThemeColor(0x65, 0x7b, 0x83), // 11 bright yellow  = base00
            ThemeColor(0x83, 0x94, 0x96), // 12 bright blue    = base0
            ThemeColor(0x6c, 0x71, 0xc4), // 13 bright magenta = violet
            ThemeColor(0x93, 0xa1, 0xa1), // 14 bright cyan    = base1
            ThemeColor(0xfd, 0xf6, 0xe3), // 15 bright white   = base3
        ],
        source: .builtIn
    )

    // MARK: - Dracula
    // https://draculatheme.com/contribute — values via Ghostty spec.

    static let dracula = TerminalTheme(
        id: "dracula",
        displayName: "Dracula",
        scope: .dark,
        background: ThemeColor(0x28, 0x2a, 0x36),
        foreground: ThemeColor(0xf8, 0xf8, 0xf2),
        cursor: ThemeColor(0xf8, 0xf8, 0xf2),
        selection: ThemeColor(0x44, 0x47, 0x5a),
        ansi: [
            ThemeColor(0x21, 0x22, 0x2c), // 0  black
            ThemeColor(0xff, 0x55, 0x55), // 1  red
            ThemeColor(0x50, 0xfa, 0x7b), // 2  green
            ThemeColor(0xf1, 0xfa, 0x8c), // 3  yellow
            ThemeColor(0xbd, 0x93, 0xf9), // 4  blue
            ThemeColor(0xff, 0x79, 0xc6), // 5  magenta
            ThemeColor(0x8b, 0xe9, 0xfd), // 6  cyan
            ThemeColor(0xf8, 0xf8, 0xf2), // 7  white
            ThemeColor(0x62, 0x72, 0xa4), // 8  bright black
            ThemeColor(0xff, 0x6e, 0x6e), // 9  bright red
            ThemeColor(0x69, 0xff, 0x94), // 10 bright green
            ThemeColor(0xff, 0xff, 0xa5), // 11 bright yellow
            ThemeColor(0xd6, 0xac, 0xff), // 12 bright blue
            ThemeColor(0xff, 0x92, 0xdf), // 13 bright magenta
            ThemeColor(0xa4, 0xff, 0xff), // 14 bright cyan
            ThemeColor(0xff, 0xff, 0xff), // 15 bright white
        ],
        source: .builtIn
    )

    // MARK: - Nord
    // https://www.nordtheme.com/docs/colors-and-palettes — values via Ghostty spec.

    static let nord = TerminalTheme(
        id: "nord",
        displayName: "Nord",
        scope: .dark,
        background: ThemeColor(0x2e, 0x34, 0x40),
        foreground: ThemeColor(0xd8, 0xde, 0xe9),
        cursor: ThemeColor(0xd8, 0xde, 0xe9),
        selection: ThemeColor(0x43, 0x4c, 0x5e),
        ansi: [
            ThemeColor(0x3b, 0x42, 0x52), // 0  black
            ThemeColor(0xbf, 0x61, 0x6a), // 1  red
            ThemeColor(0xa3, 0xbe, 0x8c), // 2  green
            ThemeColor(0xeb, 0xcb, 0x8b), // 3  yellow
            ThemeColor(0x81, 0xa1, 0xc1), // 4  blue
            ThemeColor(0xb4, 0x8e, 0xad), // 5  magenta
            ThemeColor(0x88, 0xc0, 0xd0), // 6  cyan
            ThemeColor(0xe5, 0xe9, 0xf0), // 7  white
            ThemeColor(0x4c, 0x56, 0x6a), // 8  bright black
            ThemeColor(0xbf, 0x61, 0x6a), // 9  bright red
            ThemeColor(0xa3, 0xbe, 0x8c), // 10 bright green
            ThemeColor(0xeb, 0xcb, 0x8b), // 11 bright yellow
            ThemeColor(0x81, 0xa1, 0xc1), // 12 bright blue
            ThemeColor(0xb4, 0x8e, 0xad), // 13 bright magenta
            ThemeColor(0x8f, 0xbc, 0xbb), // 14 bright cyan
            ThemeColor(0xec, 0xef, 0xf4), // 15 bright white
        ],
        source: .builtIn
    )

    // MARK: - Gruvbox
    // morhetz/gruvbox medium-contrast palette — values via Ghostty spec.

    static let gruvboxLight = TerminalTheme(
        id: "gruvbox-light",
        displayName: "Gruvbox Light",
        scope: .light,
        background: ThemeColor(0xfb, 0xf1, 0xc7),
        foreground: ThemeColor(0x3c, 0x38, 0x36),
        cursor: ThemeColor(0x3c, 0x38, 0x36),
        selection: ThemeColor(0xeb, 0xdb, 0xb2),
        ansi: [
            ThemeColor(0xfb, 0xf1, 0xc7), // 0  black (bg0)
            ThemeColor(0xcc, 0x24, 0x1d), // 1  red
            ThemeColor(0x98, 0x97, 0x1a), // 2  green
            ThemeColor(0xd7, 0x99, 0x21), // 3  yellow
            ThemeColor(0x45, 0x85, 0x88), // 4  blue
            ThemeColor(0xb1, 0x62, 0x86), // 5  magenta
            ThemeColor(0x68, 0x9d, 0x6a), // 6  cyan (aqua)
            ThemeColor(0x7c, 0x6f, 0x64), // 7  white (fg4)
            ThemeColor(0x92, 0x83, 0x74), // 8  bright black (gray)
            ThemeColor(0x9d, 0x00, 0x06), // 9  bright red
            ThemeColor(0x79, 0x74, 0x0e), // 10 bright green
            ThemeColor(0xb5, 0x76, 0x14), // 11 bright yellow
            ThemeColor(0x07, 0x66, 0x78), // 12 bright blue
            ThemeColor(0x8f, 0x3f, 0x71), // 13 bright magenta
            ThemeColor(0x42, 0x7b, 0x58), // 14 bright cyan (aqua)
            ThemeColor(0x3c, 0x38, 0x36), // 15 bright white (fg0)
        ],
        source: .builtIn
    )

    static let gruvboxDark = TerminalTheme(
        id: "gruvbox-dark",
        displayName: "Gruvbox Dark",
        scope: .dark,
        background: ThemeColor(0x28, 0x28, 0x28),
        foreground: ThemeColor(0xeb, 0xdb, 0xb2),
        cursor: ThemeColor(0xeb, 0xdb, 0xb2),
        selection: ThemeColor(0x66, 0x5c, 0x54),
        ansi: [
            ThemeColor(0x28, 0x28, 0x28), // 0  black (bg0)
            ThemeColor(0xcc, 0x24, 0x1d), // 1  red
            ThemeColor(0x98, 0x97, 0x1a), // 2  green
            ThemeColor(0xd7, 0x99, 0x21), // 3  yellow
            ThemeColor(0x45, 0x85, 0x88), // 4  blue
            ThemeColor(0xb1, 0x62, 0x86), // 5  magenta
            ThemeColor(0x68, 0x9d, 0x6a), // 6  cyan (aqua)
            ThemeColor(0xa8, 0x99, 0x84), // 7  white (fg4)
            ThemeColor(0x92, 0x83, 0x74), // 8  bright black (gray)
            ThemeColor(0xfb, 0x49, 0x34), // 9  bright red
            ThemeColor(0xb8, 0xbb, 0x26), // 10 bright green
            ThemeColor(0xfa, 0xbd, 0x2f), // 11 bright yellow
            ThemeColor(0x83, 0xa5, 0x98), // 12 bright blue
            ThemeColor(0xd3, 0x86, 0x9b), // 13 bright magenta
            ThemeColor(0x8e, 0xc0, 0x7c), // 14 bright cyan (aqua)
            ThemeColor(0xeb, 0xdb, 0xb2), // 15 bright white (fg0)
        ],
        source: .builtIn
    )

    // MARK: - Catppuccin
    // https://github.com/catppuccin/catppuccin — values via Ghostty spec.

    static let catppuccinLatte = TerminalTheme(
        id: "catppuccin-latte",
        displayName: "Catppuccin Latte",
        scope: .light,
        background: ThemeColor(0xef, 0xf1, 0xf5),
        foreground: ThemeColor(0x4c, 0x4f, 0x69),
        cursor: ThemeColor(0xdc, 0x8a, 0x78),     // Rosewater
        selection: ThemeColor(0xac, 0xb0, 0xbe),  // Surface2
        ansi: [
            ThemeColor(0x5c, 0x5f, 0x77), // 0  black
            ThemeColor(0xd2, 0x0f, 0x39), // 1  red
            ThemeColor(0x40, 0xa0, 0x2b), // 2  green
            ThemeColor(0xdf, 0x8e, 0x1d), // 3  yellow
            ThemeColor(0x1e, 0x66, 0xf5), // 4  blue
            ThemeColor(0xea, 0x76, 0xcb), // 5  magenta (pink)
            ThemeColor(0x17, 0x92, 0x99), // 6  cyan (teal)
            ThemeColor(0xac, 0xb0, 0xbe), // 7  white
            ThemeColor(0x6c, 0x6f, 0x85), // 8  bright black
            ThemeColor(0xde, 0x29, 0x3e), // 9  bright red
            ThemeColor(0x49, 0xaf, 0x3d), // 10 bright green
            ThemeColor(0xee, 0xa0, 0x2d), // 11 bright yellow
            ThemeColor(0x45, 0x6e, 0xff), // 12 bright blue
            ThemeColor(0xfe, 0x85, 0xd8), // 13 bright magenta
            ThemeColor(0x2d, 0x9f, 0xa8), // 14 bright cyan
            ThemeColor(0xbc, 0xc0, 0xcc), // 15 bright white
        ],
        source: .builtIn
    )

    static let catppuccinMocha = TerminalTheme(
        id: "catppuccin-mocha",
        displayName: "Catppuccin Mocha",
        scope: .dark,
        background: ThemeColor(0x1e, 0x1e, 0x2e),
        foreground: ThemeColor(0xcd, 0xd6, 0xf4),
        cursor: ThemeColor(0xf5, 0xe0, 0xdc),     // Rosewater
        selection: ThemeColor(0x58, 0x5b, 0x70),  // Surface2
        ansi: [
            ThemeColor(0x45, 0x47, 0x5a), // 0  black
            ThemeColor(0xf3, 0x8b, 0xa8), // 1  red
            ThemeColor(0xa6, 0xe3, 0xa1), // 2  green
            ThemeColor(0xf9, 0xe2, 0xaf), // 3  yellow
            ThemeColor(0x89, 0xb4, 0xfa), // 4  blue
            ThemeColor(0xf5, 0xc2, 0xe7), // 5  magenta (pink)
            ThemeColor(0x94, 0xe2, 0xd5), // 6  cyan (teal)
            ThemeColor(0xa6, 0xad, 0xc8), // 7  white
            ThemeColor(0x58, 0x5b, 0x70), // 8  bright black
            ThemeColor(0xf3, 0x77, 0x99), // 9  bright red
            ThemeColor(0x89, 0xd8, 0x8b), // 10 bright green
            ThemeColor(0xeb, 0xd3, 0x91), // 11 bright yellow
            ThemeColor(0x74, 0xa8, 0xfc), // 12 bright blue
            ThemeColor(0xf2, 0xae, 0xde), // 13 bright magenta
            ThemeColor(0x6b, 0xd7, 0xca), // 14 bright cyan
            ThemeColor(0xba, 0xc2, 0xde), // 15 bright white
        ],
        source: .builtIn
    )

    // MARK: - Tokyo Night
    // enkia/tokyo-night-vscode-theme — values via Ghostty's TokyoNight Night.

    static let tokyoNight = TerminalTheme(
        id: "tokyo-night",
        displayName: "Tokyo Night",
        scope: .dark,
        background: ThemeColor(0x1a, 0x1b, 0x26),
        foreground: ThemeColor(0xc0, 0xca, 0xf5),
        cursor: ThemeColor(0xc0, 0xca, 0xf5),
        selection: ThemeColor(0x28, 0x34, 0x57),
        ansi: [
            ThemeColor(0x15, 0x16, 0x1e), // 0  black
            ThemeColor(0xf7, 0x76, 0x8e), // 1  red
            ThemeColor(0x9e, 0xce, 0x6a), // 2  green
            ThemeColor(0xe0, 0xaf, 0x68), // 3  yellow
            ThemeColor(0x7a, 0xa2, 0xf7), // 4  blue
            ThemeColor(0xbb, 0x9a, 0xf7), // 5  magenta
            ThemeColor(0x7d, 0xcf, 0xff), // 6  cyan
            ThemeColor(0xa9, 0xb1, 0xd6), // 7  white
            ThemeColor(0x41, 0x48, 0x68), // 8  bright black
            ThemeColor(0xf7, 0x76, 0x8e), // 9  bright red
            ThemeColor(0x9e, 0xce, 0x6a), // 10 bright green
            ThemeColor(0xe0, 0xaf, 0x68), // 11 bright yellow
            ThemeColor(0x7a, 0xa2, 0xf7), // 12 bright blue
            ThemeColor(0xbb, 0x9a, 0xf7), // 13 bright magenta
            ThemeColor(0x7d, 0xcf, 0xff), // 14 bright cyan
            ThemeColor(0xc0, 0xca, 0xf5), // 15 bright white
        ],
        source: .builtIn
    )

    // MARK: - One Dark
    // Atom's One Dark — values via Ghostty's "Atom One Dark".

    static let oneDark = TerminalTheme(
        id: "one-dark",
        displayName: "One Dark",
        scope: .dark,
        background: ThemeColor(0x21, 0x25, 0x2b),
        foreground: ThemeColor(0xab, 0xb2, 0xbf),
        cursor: ThemeColor(0xab, 0xb2, 0xbf),
        selection: ThemeColor(0x32, 0x38, 0x44),
        ansi: [
            ThemeColor(0x21, 0x25, 0x2b), // 0  black
            ThemeColor(0xe0, 0x6c, 0x75), // 1  red
            ThemeColor(0x98, 0xc3, 0x79), // 2  green
            ThemeColor(0xe5, 0xc0, 0x7b), // 3  yellow
            ThemeColor(0x61, 0xaf, 0xef), // 4  blue
            ThemeColor(0xc6, 0x78, 0xdd), // 5  magenta
            ThemeColor(0x56, 0xb6, 0xc2), // 6  cyan
            ThemeColor(0xab, 0xb2, 0xbf), // 7  white
            ThemeColor(0x76, 0x76, 0x76), // 8  bright black
            ThemeColor(0xe0, 0x6c, 0x75), // 9  bright red
            ThemeColor(0x98, 0xc3, 0x79), // 10 bright green
            ThemeColor(0xe5, 0xc0, 0x7b), // 11 bright yellow
            ThemeColor(0x61, 0xaf, 0xef), // 12 bright blue
            ThemeColor(0xc6, 0x78, 0xdd), // 13 bright magenta
            ThemeColor(0x56, 0xb6, 0xc2), // 14 bright cyan
            ThemeColor(0xab, 0xb2, 0xbf), // 15 bright white
        ],
        source: .builtIn
    )
}
