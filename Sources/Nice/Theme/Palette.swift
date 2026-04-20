//
//  Palette.swift
//  Nice
//
//  Source of truth for the *nice* palette: the `<style>` block in
//  /tmp/nice-design/nice/project/Nice.html — the CSS variables defined
//  under `.theme-light` and `.theme-dark` scopes (plus `--accent`).
//
//  The design mock uses `oklch(L C H)` values. Swift's `Color(.sRGB, ...)`
//  initializer doesn't accept oklch directly, so each nice-palette value
//  below was converted to sRGB via Björn Ottosson's standard oklch ->
//  oklab -> linear sRGB -> gamma-encoded sRGB pipeline, rounded to 3
//  decimals. (Verified against https://oklch.com for spot-checks; drift
//  is < 0.5%.)
//
//  When the design CSS changes, re-run the conversion and update the
//  corresponding literals below. Keep this file and the CSS in sync.
//
//  The *macOS* palette is not a literal table — it delegates to system
//  semantic colors (`NSColor.windowBackgroundColor`, `.labelColor`, …)
//  so chrome matches Finder / Mail / Xcode pixel-for-pixel. Those NSColor
//  values resolve dynamically against the current `NSApp.appearance`,
//  which Tweaks pins to `.aqua` / `.darkAqua` based on `theme.scheme`.
//

import AppKit
import SwiftTerm
import SwiftUI

// MARK: - Palette environment key

private struct PaletteKey: EnvironmentKey {
    static let defaultValue: Palette = .nice
}

public extension EnvironmentValues {
    /// The active chrome palette for the current view subtree. Set at the
    /// app root from `Tweaks.theme.palette`; consumed by the `Color.niceX`
    /// helpers that need to branch between the nice literals and the
    /// system semantic colors. Defaults to `.nice` so preview/test views
    /// that don't install the environment key still render correctly.
    var palette: Palette {
        get { self[PaletteKey.self] }
        set { self[PaletteKey.self] = newValue }
    }
}

// MARK: - Color helpers

public extension SwiftUI.Color {

    // MARK: - Accent

    /// Default terracotta accent (`#c96442`). Kept as a static fallback
    /// for previews and contexts without a `Tweaks` environment object.
    /// Runtime code should prefer `tweaks.accent.color` (or
    /// `Color.niceAccentDynamic`) so the whole tree repaints when the
    /// user picks a different swatch.
    static let niceAccent = Self(.sRGB, red: 0.788, green: 0.392, blue: 0.259, opacity: 1.0)

    /// Resolves the user's currently-selected accent preset from
    /// `UserDefaults`. Falls back to terracotta if the key is missing or
    /// unrecognised. Used by helpers (e.g. `niceSelDynamic`) that don't
    /// already hold a `Tweaks` reference.
    // Note: within this extension on `SwiftUI.Color` we use `Self`
    // instead of `Color` because SwiftTerm (imported for `NiceANSIPalette`
    // below) also exports a `Color` — unqualified refs are ambiguous.
    @MainActor static var niceAccentDynamic: Self {
        let raw = UserDefaults.standard.string(forKey: Tweaks.accentKey)
            ?? AccentPreset.terracotta.rawValue
        return AccentPreset(rawValue: raw)?.color ?? AccentPreset.terracotta.color
    }

    // MARK: - Backgrounds

    static func niceBg(_ scheme: ColorScheme, _ palette: Palette = .nice) -> Self {
        switch palette {
        case .macOS: Self(nsColor: .windowBackgroundColor)
        case .nice:
            scheme == .dark
                ? Self(.sRGB, red: 0.080, green: 0.066, blue: 0.055, opacity: 1.0)
                : Self(.sRGB, red: 0.989, green: 0.978, blue: 0.970, opacity: 1.0)
        case .catppuccinLatte:
            Self(.sRGB, red: 0.937, green: 0.945, blue: 0.961, opacity: 1.0)
        case .catppuccinMocha:
            Self(.sRGB, red: 0.118, green: 0.118, blue: 0.180, opacity: 1.0)
        }
    }

    static func niceBg2(_ scheme: ColorScheme, _ palette: Palette = .nice) -> Self {
        switch palette {
        case .macOS: Self(nsColor: .underPageBackgroundColor)
        case .nice:
            scheme == .dark
                ? Self(.sRGB, red: 0.058, green: 0.045, blue: 0.035, opacity: 1.0)
                : Self(.sRGB, red: 0.965, green: 0.952, blue: 0.942, opacity: 1.0)
        case .catppuccinLatte:
            Self(.sRGB, red: 0.902, green: 0.914, blue: 0.937, opacity: 1.0)
        case .catppuccinMocha:
            Self(.sRGB, red: 0.094, green: 0.094, blue: 0.145, opacity: 1.0)
        }
    }

    static func niceBg3(_ scheme: ColorScheme, _ palette: Palette = .nice) -> Self {
        switch palette {
        case .macOS: Self(nsColor: .textBackgroundColor)
        case .nice:
            scheme == .dark
                ? Self(.sRGB, red: 0.037, green: 0.026, blue: 0.019, opacity: 1.0)
                : Self(.sRGB, red: 0.934, green: 0.919, blue: 0.907, opacity: 1.0)
        case .catppuccinLatte:
            Self(.sRGB, red: 0.863, green: 0.878, blue: 0.910, opacity: 1.0)
        case .catppuccinMocha:
            Self(.sRGB, red: 0.067, green: 0.067, blue: 0.106, opacity: 1.0)
        }
    }

    static func nicePanel(_ scheme: ColorScheme, _ palette: Palette = .nice) -> Self {
        switch palette {
        case .macOS: Self(nsColor: .windowBackgroundColor)
        case .nice:
            scheme == .dark
                ? Self(.sRGB, red: 0.097, green: 0.083, blue: 0.072, opacity: 1.0)
                : Self(.sRGB, red: 1.000, green: 0.992, blue: 0.986, opacity: 1.0)
        case .catppuccinLatte:
            Self(.sRGB, red: 0.937, green: 0.945, blue: 0.961, opacity: 1.0)
        case .catppuccinMocha:
            Self(.sRGB, red: 0.118, green: 0.118, blue: 0.180, opacity: 1.0)
        }
    }

    // MARK: - Ink (foreground text)

    static func niceInk(_ scheme: ColorScheme, _ palette: Palette = .nice) -> Self {
        switch palette {
        case .macOS: Self(nsColor: .labelColor)
        case .nice:
            scheme == .dark
                ? Self(.sRGB, red: 0.956, green: 0.946, blue: 0.938, opacity: 1.0)
                : Self(.sRGB, red: 0.091, green: 0.074, blue: 0.060, opacity: 1.0)
        case .catppuccinLatte:
            Self(.sRGB, red: 0.298, green: 0.310, blue: 0.412, opacity: 1.0)
        case .catppuccinMocha:
            Self(.sRGB, red: 0.804, green: 0.839, blue: 0.957, opacity: 1.0)
        }
    }

    static func niceInk2(_ scheme: ColorScheme, _ palette: Palette = .nice) -> Self {
        switch palette {
        case .macOS: Self(nsColor: .secondaryLabelColor)
        case .nice:
            scheme == .dark
                ? Self(.sRGB, red: 0.693, green: 0.679, blue: 0.667, opacity: 1.0)
                : Self(.sRGB, red: 0.273, green: 0.257, blue: 0.244, opacity: 1.0)
        case .catppuccinLatte:
            Self(.sRGB, red: 0.361, green: 0.373, blue: 0.467, opacity: 1.0)
        case .catppuccinMocha:
            Self(.sRGB, red: 0.729, green: 0.761, blue: 0.871, opacity: 1.0)
        }
    }

    static func niceInk3(_ scheme: ColorScheme, _ palette: Palette = .nice) -> Self {
        switch palette {
        case .macOS: Self(nsColor: .tertiaryLabelColor)
        case .nice:
            scheme == .dark
                ? Self(.sRGB, red: 0.460, green: 0.441, blue: 0.427, opacity: 1.0)
                : Self(.sRGB, red: 0.494, green: 0.475, blue: 0.461, opacity: 1.0)
        case .catppuccinLatte:
            Self(.sRGB, red: 0.424, green: 0.435, blue: 0.522, opacity: 1.0)
        case .catppuccinMocha:
            Self(.sRGB, red: 0.651, green: 0.678, blue: 0.784, opacity: 1.0)
        }
    }

    // MARK: - Lines / dividers

    static func niceLine(_ scheme: ColorScheme, _ palette: Palette = .nice) -> Self {
        switch palette {
        case .macOS: Self(nsColor: .separatorColor)
        case .nice:
            scheme == .dark
                ? Self(.sRGB, red: 0.172, green: 0.157, blue: 0.145, opacity: 1.0)
                : Self(.sRGB, red: 0.857, green: 0.841, blue: 0.829, opacity: 1.0)
        case .catppuccinLatte:
            Self(.sRGB, red: 0.800, green: 0.816, blue: 0.855, opacity: 1.0)
        case .catppuccinMocha:
            Self(.sRGB, red: 0.192, green: 0.196, blue: 0.267, opacity: 1.0)
        }
    }

    static func niceLineStrong(_ scheme: ColorScheme, _ palette: Palette = .nice) -> Self {
        switch palette {
        case .macOS: Self(nsColor: .gridColor)
        case .nice:
            scheme == .dark
                ? Self(.sRGB, red: 0.252, green: 0.236, blue: 0.223, opacity: 1.0)
                : Self(.sRGB, red: 0.735, green: 0.715, blue: 0.699, opacity: 1.0)
        case .catppuccinLatte:
            Self(.sRGB, red: 0.737, green: 0.753, blue: 0.800, opacity: 1.0)
        case .catppuccinMocha:
            Self(.sRGB, red: 0.271, green: 0.278, blue: 0.353, opacity: 1.0)
        }
    }

    // MARK: - Selection / bubble / chrome

    /// CSS: `color-mix(in oklch, var(--accent) 14%, transparent)` (light),
    /// `22%` in dark. Approximated here by applying the accent with the
    /// same alpha against a transparent base.
    ///
    /// Preview-safe fallback — bakes in the static terracotta accent.
    /// Runtime code should prefer `niceSel(_:accent:)` below so the
    /// selection tint follows the user's chosen swatch.
    static func niceSel(_ scheme: ColorScheme) -> Self {
        let alpha: Double = scheme == .dark ? 0.22 : 0.14
        return Self(.sRGB, red: 0.788, green: 0.392, blue: 0.259, opacity: alpha)
    }

    /// Accent-driven selection tint for runtime use. Mirrors the CSS
    /// mix ratios (14% light / 22% dark) but applies them to whichever
    /// accent swatch the user has chosen. Palette-agnostic — selection
    /// always tints with the user's accent regardless of chrome palette.
    static func niceSel(_ scheme: ColorScheme, accent: Self) -> Self {
        let alpha: Double = scheme == .dark ? 0.22 : 0.14
        return accent.opacity(alpha)
    }

    static func niceUserBubble(_ scheme: ColorScheme, _ palette: Palette = .nice) -> Self {
        switch palette {
        case .macOS: Self(nsColor: .controlBackgroundColor)
        case .nice:
            scheme == .dark
                ? Self(.sRGB, red: 0.134, green: 0.119, blue: 0.108, opacity: 1.0)
                : Self(.sRGB, red: 0.939, green: 0.918, blue: 0.902, opacity: 1.0)
        case .catppuccinLatte:
            Self(.sRGB, red: 0.800, green: 0.816, blue: 0.855, opacity: 1.0)
        case .catppuccinMocha:
            Self(.sRGB, red: 0.192, green: 0.196, blue: 0.267, opacity: 1.0)
        }
    }

    /// CSS: `color-mix(in oklch, var(--bg) 70%, transparent)`. We mirror
    /// that by taking `--bg` and dropping alpha to 0.7.
    static func niceChrome(_ scheme: ColorScheme, _ palette: Palette = .nice) -> Self {
        switch palette {
        case .macOS: Self(nsColor: .windowBackgroundColor).opacity(0.70)
        case .nice:
            scheme == .dark
                ? Self(.sRGB, red: 0.080, green: 0.066, blue: 0.055, opacity: 0.70)
                : Self(.sRGB, red: 0.989, green: 0.978, blue: 0.970, opacity: 0.70)
        case .catppuccinLatte:
            Self(.sRGB, red: 0.937, green: 0.945, blue: 0.961, opacity: 0.70)
        case .catppuccinMocha:
            Self(.sRGB, red: 0.118, green: 0.118, blue: 0.180, opacity: 0.70)
        }
    }

    // MARK: - NSColor helpers for SwiftTerm

    /// `niceBg3` expressed as `NSColor` for feeding SwiftTerm's
    /// `nativeBackgroundColor`. For the macOS palette we return a
    /// dynamic semantic color that resolves against the pinned
    /// `NSApp.appearance` at draw time.
    static func niceBg3NS(_ scheme: ColorScheme, _ palette: Palette = .nice) -> NSColor {
        switch palette {
        case .macOS: .textBackgroundColor
        case .nice:
            scheme == .dark
                ? NSColor(srgbRed: 0.037, green: 0.026, blue: 0.019, alpha: 1.0)
                : NSColor(srgbRed: 0.934, green: 0.919, blue: 0.907, alpha: 1.0)
        case .catppuccinLatte:
            NSColor(srgbRed: 0.863, green: 0.878, blue: 0.910, alpha: 1.0)
        case .catppuccinMocha:
            NSColor(srgbRed: 0.067, green: 0.067, blue: 0.106, alpha: 1.0)
        }
    }

    /// `nicePanel` expressed as `NSColor` for the Claude pane terminal.
    static func nicePanelNS(_ scheme: ColorScheme, _ palette: Palette = .nice) -> NSColor {
        switch palette {
        case .macOS: .windowBackgroundColor
        case .nice:
            scheme == .dark
                ? NSColor(srgbRed: 0.097, green: 0.083, blue: 0.072, alpha: 1.0)
                : NSColor(srgbRed: 1.000, green: 0.992, blue: 0.986, alpha: 1.0)
        case .catppuccinLatte:
            NSColor(srgbRed: 0.937, green: 0.945, blue: 0.961, alpha: 1.0)
        case .catppuccinMocha:
            NSColor(srgbRed: 0.118, green: 0.118, blue: 0.180, alpha: 1.0)
        }
    }

    /// `niceInk` expressed as `NSColor` for feeding SwiftTerm's
    /// `nativeForegroundColor`.
    static func niceInkNS(_ scheme: ColorScheme, _ palette: Palette = .nice) -> NSColor {
        switch palette {
        case .macOS: .labelColor
        case .nice:
            scheme == .dark
                ? NSColor(srgbRed: 0.956, green: 0.946, blue: 0.938, alpha: 1.0)
                : NSColor(srgbRed: 0.091, green: 0.074, blue: 0.060, alpha: 1.0)
        case .catppuccinLatte:
            NSColor(srgbRed: 0.298, green: 0.310, blue: 0.412, alpha: 1.0)
        case .catppuccinMocha:
            NSColor(srgbRed: 0.804, green: 0.839, blue: 0.957, alpha: 1.0)
        }
    }
}

// MARK: - ANSI 16-color palettes for SwiftTerm

/// SwiftTerm's default palette targets a dark terminal background. Against
/// Nice's `niceBg3` — especially in light mode where the background is
/// near-white — defaults like bright-white become invisible. The palettes
/// below replace the 16-entry ANSI table with values harmonized to the
/// respective `niceBg3` per theme.
///
/// ANSI colors are palette-agnostic — we only vary on scheme (light/dark)
/// because the contrast problem is the same whether the terminal sits on
/// `.niceBg3` or `NSColor.textBackgroundColor` (both are near-black in
/// dark mode, near-white in light).
///
/// Values are 8-bit per channel; SwiftTerm's `Color(red:green:blue:)`
/// takes 16-bit, so the helper scales by 257 (the standard 8 → 16-bit
/// widen: `v * 257 == (v << 8) | v`).
enum NiceANSIPalette {
    static func colors(for scheme: ColorScheme) -> [SwiftTerm.Color] {
        scheme == .dark ? darkPalette : lightPalette
    }

    private static func c(_ r: UInt16, _ g: UInt16, _ b: UInt16) -> SwiftTerm.Color {
        SwiftTerm.Color(red: r * 257, green: g * 257, blue: b * 257)
    }

    /// Terminal.app-style palette, slightly shifted so indices 0 / 15
    /// (black / bright-white) land on `niceBg3` / `niceInk` for dark mode.
    /// Computed (not `static let`) so the array ctor doesn't need to
    /// satisfy Swift 6 `Sendable` rules for the non-Sendable SwiftTerm
    /// `Color` type. Called only when a tab gets themed, so the rebuild
    /// cost is negligible.
    private static var darkPalette: [SwiftTerm.Color] {
        [
            c(9,   7,   5),      // 0 black      ≈ niceBg3
            c(194, 54,  33),     // 1 red
            c(37,  188, 36),     // 2 green
            c(173, 173, 39),     // 3 yellow
            c(73,  110, 225),    // 4 blue
            c(211, 56,  211),    // 5 magenta
            c(51,  187, 200),    // 6 cyan
            c(203, 204, 205),    // 7 white
            c(129, 131, 131),    // 8 bright black
            c(252, 91,  71),     // 9 bright red
            c(49,  231, 34),     // 10 bright green
            c(234, 212, 35),     // 11 bright yellow
            c(108, 141, 255),    // 12 bright blue
            c(249, 101, 248),    // 13 bright magenta
            c(100, 230, 230),    // 14 bright cyan
            c(244, 240, 239),    // 15 bright white ≈ niceInk
        ]
    }

    /// Light-mode palette: darker hues so text remains legible on the
    /// near-white `niceBg3`. Index 0 maps to niceInk; index 15 is a
    /// neutral deep ink so "bright white" ANSI sequences don't disappear.
    private static var lightPalette: [SwiftTerm.Color] {
        [
            c(23,  19,  15),     // 0 black        = niceInk
            c(183, 64,  32),     // 1 red
            c(48,  129, 48),     // 2 green
            c(166, 113, 13),     // 3 yellow (amber)
            c(40,  96,  175),    // 4 blue
            c(155, 59,  152),    // 5 magenta
            c(35,  133, 155),    // 6 cyan
            c(126, 118, 108),    // 7 white (muted gray)
            c(92,  83,  72),     // 8 bright black
            c(212, 76,  37),     // 9 bright red
            c(56,  159, 56),     // 10 bright green
            c(196, 140, 24),     // 11 bright yellow
            c(52,  117, 205),    // 12 bright blue
            c(181, 71,  175),    // 13 bright magenta
            c(40,  156, 178),    // 14 bright cyan
            c(23,  19,  15),     // 15 bright white — stays dark on light bg
        ]
    }
}
