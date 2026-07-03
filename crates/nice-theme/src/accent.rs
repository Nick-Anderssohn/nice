//! Accent presets, ported verbatim from `AccentPreset` in
//! `Sources/Nice/State/Tweaks.swift`.
//!
//! The `#rrggbb` hex strings are the source of record (from the `ACCENTS` array
//! in tweaks.jsx — `Tweaks.swift:16-17,124-132`); the sRGB value is derived
//! from the hex exactly as Swift's `Color(hex:)` does (`Tweaks.swift:158-169`):
//! each byte / 255.

use crate::color::Srgba;

/// The five user-selectable accent swatches. Ported from `enum AccentPreset`
/// (`Tweaks.swift:117-118`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccentPreset {
    /// Default accent, `#c96442` (`Tweaks.swift:126`).
    Terracotta,
    /// `#3b82f6` (`Tweaks.swift:127`).
    Ocean,
    /// `#10b981` (`Tweaks.swift:128`).
    Fern,
    /// `#7c3aed` (`Tweaks.swift:129`).
    Iris,
    /// `#1f2937` (`Tweaks.swift:130`).
    Graphite,
}

impl AccentPreset {
    /// Every preset, in declaration order. Ports `CaseIterable`
    /// (`Tweaks.swift:117`).
    pub const ALL: [AccentPreset; 5] = [
        AccentPreset::Terracotta,
        AccentPreset::Ocean,
        AccentPreset::Fern,
        AccentPreset::Iris,
        AccentPreset::Graphite,
    ];

    /// The Swift `rawValue` (used as the persisted `accentKey` value). Ported
    /// from the enum case names (`Tweaks.swift:117`).
    pub const fn raw_value(self) -> &'static str {
        match self {
            AccentPreset::Terracotta => "terracotta",
            AccentPreset::Ocean => "ocean",
            AccentPreset::Fern => "fern",
            AccentPreset::Iris => "iris",
            AccentPreset::Graphite => "graphite",
        }
    }

    /// The `#rrggbb` hex string, verbatim from `AccentPreset.hex`
    /// (`Tweaks.swift:124-132`). This is the source of record for the accent.
    pub const fn hex(self) -> &'static str {
        match self {
            AccentPreset::Terracotta => "#c96442", // Tweaks.swift:126
            AccentPreset::Ocean => "#3b82f6",      // Tweaks.swift:127
            AccentPreset::Fern => "#10b981",       // Tweaks.swift:128
            AccentPreset::Iris => "#7c3aed",       // Tweaks.swift:129
            AccentPreset::Graphite => "#1f2937",   // Tweaks.swift:130
        }
    }

    /// The raw 8-bit `(r, g, b)` bytes decoded from [`hex`](Self::hex).
    pub const fn rgb8(self) -> (u8, u8, u8) {
        match self {
            AccentPreset::Terracotta => (0xc9, 0x64, 0x42),
            AccentPreset::Ocean => (0x3b, 0x82, 0xf6),
            AccentPreset::Fern => (0x10, 0xb9, 0x81),
            AccentPreset::Iris => (0x7c, 0x3a, 0xed),
            AccentPreset::Graphite => (0x1f, 0x29, 0x37),
        }
    }

    /// The opaque sRGB color, derived from the hex exactly as Swift's
    /// `Color(hex:)` (`Tweaks.swift:158-169`): each byte / 255.
    pub fn color(self) -> Srgba {
        let (r, g, b) = self.rgb8();
        Srgba::rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
    }
}

/// Accent-driven selection-tint alpha in the light scheme, mirroring the CSS
/// `color-mix(in oklch, var(--accent) 14%, transparent)` ratio. Applied to the
/// user's chosen accent to tint selections; palette-agnostic. `Palette.swift:216`.
pub const SELECTION_TINT_ALPHA_LIGHT: f32 = 0.14;

/// Accent-driven selection-tint alpha in the dark scheme (`22%`).
/// `Palette.swift:216`.
pub const SELECTION_TINT_ALPHA_DARK: f32 = 0.22;

#[cfg(test)]
mod tests {
    //! Provenance-cited fixtures for the accent presets. See crates/README.md
    //! "Fixture-provenance convention".
    use super::*;

    #[test]
    fn hex_matches_swift() {
        // Tweaks.swift:124-132.
        assert_eq!(AccentPreset::Terracotta.hex(), "#c96442"); // Tweaks.swift:126
        assert_eq!(AccentPreset::Ocean.hex(), "#3b82f6"); // Tweaks.swift:127
        assert_eq!(AccentPreset::Fern.hex(), "#10b981"); // Tweaks.swift:128
        assert_eq!(AccentPreset::Iris.hex(), "#7c3aed"); // Tweaks.swift:129
        assert_eq!(AccentPreset::Graphite.hex(), "#1f2937"); // Tweaks.swift:130
    }

    #[test]
    fn raw_values_match_swift() {
        // Tweaks.swift:117-118 — enum String rawValues.
        assert_eq!(AccentPreset::Terracotta.raw_value(), "terracotta");
        assert_eq!(AccentPreset::Ocean.raw_value(), "ocean");
        assert_eq!(AccentPreset::Fern.raw_value(), "fern");
        assert_eq!(AccentPreset::Iris.raw_value(), "iris");
        assert_eq!(AccentPreset::Graphite.raw_value(), "graphite");
    }

    #[test]
    fn rgb8_decodes_the_hex() {
        // Each byte must equal the corresponding hex pair (independent
        // transcription of the hex above).
        assert_eq!(AccentPreset::Terracotta.rgb8(), (201, 100, 66)); // #c96442
        assert_eq!(AccentPreset::Ocean.rgb8(), (59, 130, 246)); // #3b82f6
        assert_eq!(AccentPreset::Fern.rgb8(), (16, 185, 129)); // #10b981
        assert_eq!(AccentPreset::Iris.rgb8(), (124, 58, 237)); // #7c3aed
        assert_eq!(AccentPreset::Graphite.rgb8(), (31, 41, 55)); // #1f2937
    }

    #[test]
    fn color_is_bytes_over_255() {
        // Mirrors Color(hex:) — Tweaks.swift:158-169.
        for preset in AccentPreset::ALL {
            let (r, g, b) = preset.rgb8();
            assert_eq!(
                preset.color(),
                Srgba::rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
            );
        }
    }

    #[test]
    fn terracotta_cross_checks_nice_accent_literal() {
        // Palette.swift:59 hardcodes the same terracotta as (0.788, 0.392,
        // 0.259) to 3 decimals; the hex-derived value must round to it.
        let c = AccentPreset::Terracotta.color();
        let round3 = |x: f32| (x * 1000.0).round() / 1000.0;
        assert_eq!(round3(c.r), 0.788);
        assert_eq!(round3(c.g), 0.392);
        assert_eq!(round3(c.b), 0.259);
    }

    #[test]
    fn selection_tint_alphas_match_swift() {
        assert_eq!(SELECTION_TINT_ALPHA_LIGHT, 0.14); // Palette.swift:216
        assert_eq!(SELECTION_TINT_ALPHA_DARK, 0.22); // Palette.swift:216
    }
}
