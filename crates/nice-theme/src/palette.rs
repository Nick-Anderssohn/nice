//! The hand-tuned chrome palettes, ported verbatim from
//! `Sources/Nice/Theme/Palette.swift`.
//!
//! Round-2 restyle plan 5 merged the chrome selection into the terminal-theme
//! selection, so the only chrome halves that stay HAND-TUNED are the ones whose
//! terminal ids pair with them; every other theme derives its chrome from the
//! terminal colors ([`crate::derive`]). What survives here is exactly that
//! hand-tuned set:
//!
//! * [`Palette::Nice`] — light & dark literal tables ([`NICE_LIGHT`],
//!   [`NICE_DARK`]).
//! * [`Palette::CatppuccinLatte`] — light-only ([`CATPPUCCIN_LATTE`]).
//! * [`Palette::CatppuccinMocha`] — dark-only ([`CATPPUCCIN_MOCHA`]).
//!
//! The old `.macOS` system-semantic palette (and the [`SlotColor::System`]
//! `NSColor` plumbing behind it) retired with the merge — every chrome slot is
//! now a concrete sRGB literal or a derived sRGB value, never a paint-time
//! system color.
//!
//! Latte/Mocha's single-scheme nature is [`Palette::matches`], ported from
//! `Palette.matches(scheme:)` (`Tweaks.swift:51-57`).
//!
//! Semantic slot names mirror `Palette.swift`'s slots (`niceBg`, `niceInk`, …),
//! NOT SwiftUI view names. See [`Slots`].

use crate::color::Srgba;

/// The color scheme axis. Mirrors SwiftUI's `ColorScheme` (only the two cases
/// Nice uses).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColorScheme {
    /// Light appearance (`NSApp.appearance` pinned to `.aqua`).
    Light,
    /// Dark appearance (`NSApp.appearance` pinned to `.darkAqua`).
    Dark,
}

/// The visual language of the chrome. Ported from `enum Palette`
/// (`Tweaks.swift:33-34`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Palette {
    /// The custom oklch-derived Nice literals.
    Nice,
    /// Catppuccin Latte — light-only.
    CatppuccinLatte,
    /// Catppuccin Mocha — dark-only.
    CatppuccinMocha,
}

impl Palette {
    /// Every palette, in declaration order. Ports `CaseIterable`
    /// (`Tweaks.swift:33`).
    pub const ALL: [Palette; 3] = [
        Palette::Nice,
        Palette::CatppuccinLatte,
        Palette::CatppuccinMocha,
    ];

    /// The Swift `rawValue` (used as the persisted key). Ported from the enum
    /// case names (`Tweaks.swift:34`).
    pub const fn raw_value(self) -> &'static str {
        match self {
            Palette::Nice => "nice",
            Palette::CatppuccinLatte => "catppuccinLatte",
            Palette::CatppuccinMocha => "catppuccinMocha",
        }
    }

    /// Whether this palette belongs in the chrome picker for `scheme`.
    /// `.nice` adapts to either scheme; the Catppuccin variants are
    /// single-scheme by design (Latte light-only, Mocha dark-only). Verbatim
    /// from `Palette.matches(scheme:)` (`Tweaks.swift:51-57`).
    pub const fn matches(self, scheme: ColorScheme) -> bool {
        match self {
            Palette::Nice => true,
            Palette::CatppuccinLatte => match scheme {
                ColorScheme::Light => true,
                ColorScheme::Dark => false,
            },
            Palette::CatppuccinMocha => match scheme {
                ColorScheme::Dark => true,
                ColorScheme::Light => false,
            },
        }
    }
}

/// A palette slot's value: a concrete sRGB literal (Nice / Catppuccin, or the
/// [`crate::derive`] output). Every slot resolves to a literal after the round-2
/// merge retired the paint-time `NSColor` system slots.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SlotColor {
    /// A precomputed sRGB literal.
    Srgb(Srgba),
}

/// Alpha the translucent `chrome` slot applies to the `background` color: CSS
/// `color-mix(in oklch, var(--bg) 70%, transparent)` → straight alpha `0.70`
/// (`Palette.swift:243-247`).
pub const CHROME_OPACITY: f32 = 0.70;

/// The semantic color slots of one `(palette, scheme)` chrome table. Field
/// names mirror `Palette.swift`'s slots (`niceBg` → `background`, `niceInk` →
/// `ink`, …), NOT SwiftUI view names. Each field cites its Swift `func niceX`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slots {
    /// `niceBg` — window background (`Palette.swift:76`).
    pub background: SlotColor,
    /// `niceBg2` — secondary background (`Palette.swift:90`).
    pub background2: SlotColor,
    /// `niceBg3` — tertiary / terminal surface (`Palette.swift:104`).
    pub background3: SlotColor,
    /// `nicePanel` — panel / card surface (`Palette.swift:118`).
    pub panel: SlotColor,
    /// `niceInk` — primary text (`Palette.swift:134`).
    pub ink: SlotColor,
    /// `niceInk2` — secondary text (`Palette.swift:148`).
    pub ink2: SlotColor,
    /// `niceInk3` — tertiary text (`Palette.swift:162`).
    pub ink3: SlotColor,
    /// `niceLine` — divider (`Palette.swift:178`).
    pub line: SlotColor,
    /// `niceLineStrong` — strong divider (`Palette.swift:192`).
    pub line_strong: SlotColor,
    /// `niceUserBubble` — user message bubble (`Palette.swift:229`).
    pub user_bubble: SlotColor,
    /// `niceChrome` — translucent chrome = `background` @ [`CHROME_OPACITY`]
    /// (`Palette.swift:245`).
    pub chrome: SlotColor,
}

/// Nice palette, light scheme. Literals verbatim from the `.nice` light arms of
/// `Palette.swift`.
pub const NICE_LIGHT: Slots = Slots {
    background: SlotColor::Srgb(Srgba::rgb(0.989, 0.978, 0.970)), // Palette.swift:82
    background2: SlotColor::Srgb(Srgba::rgb(0.965, 0.952, 0.942)), // Palette.swift:96
    background3: SlotColor::Srgb(Srgba::rgb(0.934, 0.919, 0.907)), // Palette.swift:110
    panel: SlotColor::Srgb(Srgba::rgb(1.000, 0.992, 0.986)),      // Palette.swift:124
    ink: SlotColor::Srgb(Srgba::rgb(0.091, 0.074, 0.060)),        // Palette.swift:140
    ink2: SlotColor::Srgb(Srgba::rgb(0.273, 0.257, 0.244)),       // Palette.swift:154
    ink3: SlotColor::Srgb(Srgba::rgb(0.494, 0.475, 0.461)),       // Palette.swift:168
    line: SlotColor::Srgb(Srgba::rgb(0.857, 0.841, 0.829)),       // Palette.swift:184
    line_strong: SlotColor::Srgb(Srgba::rgb(0.735, 0.715, 0.699)), // Palette.swift:198
    user_bubble: SlotColor::Srgb(Srgba::rgb(0.939, 0.918, 0.902)), // Palette.swift:235
    chrome: SlotColor::Srgb(Srgba::new(0.989, 0.978, 0.970, CHROME_OPACITY)), // Palette.swift:251
};

/// Nice palette, dark scheme. Literals verbatim from the `.nice` dark arms of
/// `Palette.swift`.
pub const NICE_DARK: Slots = Slots {
    background: SlotColor::Srgb(Srgba::rgb(0.080, 0.066, 0.055)), // Palette.swift:81
    background2: SlotColor::Srgb(Srgba::rgb(0.058, 0.045, 0.035)), // Palette.swift:95
    background3: SlotColor::Srgb(Srgba::rgb(0.037, 0.026, 0.019)), // Palette.swift:109
    panel: SlotColor::Srgb(Srgba::rgb(0.097, 0.083, 0.072)),      // Palette.swift:123
    ink: SlotColor::Srgb(Srgba::rgb(0.956, 0.946, 0.938)),        // Palette.swift:139
    ink2: SlotColor::Srgb(Srgba::rgb(0.693, 0.679, 0.667)),       // Palette.swift:153
    ink3: SlotColor::Srgb(Srgba::rgb(0.460, 0.441, 0.427)),       // Palette.swift:167
    line: SlotColor::Srgb(Srgba::rgb(0.172, 0.157, 0.145)),       // Palette.swift:183
    line_strong: SlotColor::Srgb(Srgba::rgb(0.252, 0.236, 0.223)), // Palette.swift:197
    user_bubble: SlotColor::Srgb(Srgba::rgb(0.134, 0.119, 0.108)), // Palette.swift:234
    chrome: SlotColor::Srgb(Srgba::new(0.080, 0.066, 0.055, CHROME_OPACITY)), // Palette.swift:250
};

/// Catppuccin Latte (light-only). Literals verbatim from the `.catppuccinLatte`
/// arms of `Palette.swift`.
pub const CATPPUCCIN_LATTE: Slots = Slots {
    background: SlotColor::Srgb(Srgba::rgb(0.937, 0.945, 0.961)), // Palette.swift:84
    background2: SlotColor::Srgb(Srgba::rgb(0.902, 0.914, 0.937)), // Palette.swift:98
    background3: SlotColor::Srgb(Srgba::rgb(0.863, 0.878, 0.910)), // Palette.swift:112
    panel: SlotColor::Srgb(Srgba::rgb(0.937, 0.945, 0.961)),      // Palette.swift:126
    ink: SlotColor::Srgb(Srgba::rgb(0.298, 0.310, 0.412)),        // Palette.swift:142
    ink2: SlotColor::Srgb(Srgba::rgb(0.361, 0.373, 0.467)),       // Palette.swift:156
    ink3: SlotColor::Srgb(Srgba::rgb(0.424, 0.435, 0.522)),       // Palette.swift:170
    line: SlotColor::Srgb(Srgba::rgb(0.800, 0.816, 0.855)),       // Palette.swift:186
    line_strong: SlotColor::Srgb(Srgba::rgb(0.737, 0.753, 0.800)), // Palette.swift:200
    user_bubble: SlotColor::Srgb(Srgba::rgb(0.800, 0.816, 0.855)), // Palette.swift:237
    chrome: SlotColor::Srgb(Srgba::new(0.937, 0.945, 0.961, CHROME_OPACITY)), // Palette.swift:253
};

/// Catppuccin Mocha (dark-only). Literals verbatim from the `.catppuccinMocha`
/// arms of `Palette.swift`.
pub const CATPPUCCIN_MOCHA: Slots = Slots {
    background: SlotColor::Srgb(Srgba::rgb(0.118, 0.118, 0.180)), // Palette.swift:86
    background2: SlotColor::Srgb(Srgba::rgb(0.094, 0.094, 0.145)), // Palette.swift:100
    background3: SlotColor::Srgb(Srgba::rgb(0.067, 0.067, 0.106)), // Palette.swift:114
    panel: SlotColor::Srgb(Srgba::rgb(0.118, 0.118, 0.180)),      // Palette.swift:128
    ink: SlotColor::Srgb(Srgba::rgb(0.804, 0.839, 0.957)),        // Palette.swift:144
    ink2: SlotColor::Srgb(Srgba::rgb(0.729, 0.761, 0.871)),       // Palette.swift:158
    ink3: SlotColor::Srgb(Srgba::rgb(0.651, 0.678, 0.784)),       // Palette.swift:172
    line: SlotColor::Srgb(Srgba::rgb(0.192, 0.196, 0.267)),       // Palette.swift:188
    line_strong: SlotColor::Srgb(Srgba::rgb(0.271, 0.278, 0.353)), // Palette.swift:202
    user_bubble: SlotColor::Srgb(Srgba::rgb(0.192, 0.196, 0.267)), // Palette.swift:239
    chrome: SlotColor::Srgb(Srgba::new(0.118, 0.118, 0.180, CHROME_OPACITY)), // Palette.swift:255
};

/// The literal slot table for a valid `(palette, scheme)` pair, or `None` for
/// the two single-scheme Catppuccin combos [`Palette::matches`] rejects
/// (Latte+dark, Mocha+light).
pub const fn slots(palette: Palette, scheme: ColorScheme) -> Option<Slots> {
    match (palette, scheme) {
        (Palette::Nice, ColorScheme::Light) => Some(NICE_LIGHT),
        (Palette::Nice, ColorScheme::Dark) => Some(NICE_DARK),
        (Palette::CatppuccinLatte, ColorScheme::Light) => Some(CATPPUCCIN_LATTE),
        (Palette::CatppuccinLatte, ColorScheme::Dark) => None,
        (Palette::CatppuccinMocha, ColorScheme::Dark) => Some(CATPPUCCIN_MOCHA),
        (Palette::CatppuccinMocha, ColorScheme::Light) => None,
    }
}

#[cfg(test)]
mod tests {
    //! Literal-equality fixtures. Every expected value below is an independent
    //! transcription from the cited `Palette.swift` line (double-entry against
    //! the tables above), so a fat-fingered literal in either place fails the
    //! build. See crates/README.md "Fixture-provenance convention".
    use super::*;

    /// Unwrap an sRGB slot (test-only helper).
    fn srgb(slot: SlotColor) -> Srgba {
        let SlotColor::Srgb(s) = slot;
        s
    }

    #[test]
    fn nice_light_matches_swift() {
        assert_eq!(srgb(NICE_LIGHT.background), Srgba::rgb(0.989, 0.978, 0.970)); // Palette.swift:82
        assert_eq!(srgb(NICE_LIGHT.background2), Srgba::rgb(0.965, 0.952, 0.942)); // Palette.swift:96
        assert_eq!(srgb(NICE_LIGHT.background3), Srgba::rgb(0.934, 0.919, 0.907)); // Palette.swift:110
        assert_eq!(srgb(NICE_LIGHT.panel), Srgba::rgb(1.000, 0.992, 0.986)); // Palette.swift:124
        assert_eq!(srgb(NICE_LIGHT.ink), Srgba::rgb(0.091, 0.074, 0.060)); // Palette.swift:140
        assert_eq!(srgb(NICE_LIGHT.ink2), Srgba::rgb(0.273, 0.257, 0.244)); // Palette.swift:154
        assert_eq!(srgb(NICE_LIGHT.ink3), Srgba::rgb(0.494, 0.475, 0.461)); // Palette.swift:168
        assert_eq!(srgb(NICE_LIGHT.line), Srgba::rgb(0.857, 0.841, 0.829)); // Palette.swift:184
        assert_eq!(srgb(NICE_LIGHT.line_strong), Srgba::rgb(0.735, 0.715, 0.699)); // Palette.swift:198
        assert_eq!(srgb(NICE_LIGHT.user_bubble), Srgba::rgb(0.939, 0.918, 0.902)); // Palette.swift:235
        assert_eq!(
            srgb(NICE_LIGHT.chrome),
            Srgba::new(0.989, 0.978, 0.970, 0.70)
        ); // Palette.swift:251
    }

    #[test]
    fn nice_dark_matches_swift() {
        assert_eq!(srgb(NICE_DARK.background), Srgba::rgb(0.080, 0.066, 0.055)); // Palette.swift:81
        assert_eq!(srgb(NICE_DARK.background2), Srgba::rgb(0.058, 0.045, 0.035)); // Palette.swift:95
        assert_eq!(srgb(NICE_DARK.background3), Srgba::rgb(0.037, 0.026, 0.019)); // Palette.swift:109
        assert_eq!(srgb(NICE_DARK.panel), Srgba::rgb(0.097, 0.083, 0.072)); // Palette.swift:123
        assert_eq!(srgb(NICE_DARK.ink), Srgba::rgb(0.956, 0.946, 0.938)); // Palette.swift:139
        assert_eq!(srgb(NICE_DARK.ink2), Srgba::rgb(0.693, 0.679, 0.667)); // Palette.swift:153
        assert_eq!(srgb(NICE_DARK.ink3), Srgba::rgb(0.460, 0.441, 0.427)); // Palette.swift:167
        assert_eq!(srgb(NICE_DARK.line), Srgba::rgb(0.172, 0.157, 0.145)); // Palette.swift:183
        assert_eq!(srgb(NICE_DARK.line_strong), Srgba::rgb(0.252, 0.236, 0.223)); // Palette.swift:197
        assert_eq!(srgb(NICE_DARK.user_bubble), Srgba::rgb(0.134, 0.119, 0.108)); // Palette.swift:234
        assert_eq!(
            srgb(NICE_DARK.chrome),
            Srgba::new(0.080, 0.066, 0.055, 0.70)
        ); // Palette.swift:250
    }

    #[test]
    fn catppuccin_latte_matches_swift() {
        assert_eq!(srgb(CATPPUCCIN_LATTE.background), Srgba::rgb(0.937, 0.945, 0.961)); // Palette.swift:84
        assert_eq!(srgb(CATPPUCCIN_LATTE.background2), Srgba::rgb(0.902, 0.914, 0.937)); // Palette.swift:98
        assert_eq!(srgb(CATPPUCCIN_LATTE.background3), Srgba::rgb(0.863, 0.878, 0.910)); // Palette.swift:112
        assert_eq!(srgb(CATPPUCCIN_LATTE.panel), Srgba::rgb(0.937, 0.945, 0.961)); // Palette.swift:126
        assert_eq!(srgb(CATPPUCCIN_LATTE.ink), Srgba::rgb(0.298, 0.310, 0.412)); // Palette.swift:142
        assert_eq!(srgb(CATPPUCCIN_LATTE.ink2), Srgba::rgb(0.361, 0.373, 0.467)); // Palette.swift:156
        assert_eq!(srgb(CATPPUCCIN_LATTE.ink3), Srgba::rgb(0.424, 0.435, 0.522)); // Palette.swift:170
        assert_eq!(srgb(CATPPUCCIN_LATTE.line), Srgba::rgb(0.800, 0.816, 0.855)); // Palette.swift:186
        assert_eq!(srgb(CATPPUCCIN_LATTE.line_strong), Srgba::rgb(0.737, 0.753, 0.800)); // Palette.swift:200
        assert_eq!(srgb(CATPPUCCIN_LATTE.user_bubble), Srgba::rgb(0.800, 0.816, 0.855)); // Palette.swift:237
        assert_eq!(
            srgb(CATPPUCCIN_LATTE.chrome),
            Srgba::new(0.937, 0.945, 0.961, 0.70)
        ); // Palette.swift:253
    }

    #[test]
    fn catppuccin_mocha_matches_swift() {
        assert_eq!(srgb(CATPPUCCIN_MOCHA.background), Srgba::rgb(0.118, 0.118, 0.180)); // Palette.swift:86
        assert_eq!(srgb(CATPPUCCIN_MOCHA.background2), Srgba::rgb(0.094, 0.094, 0.145)); // Palette.swift:100
        assert_eq!(srgb(CATPPUCCIN_MOCHA.background3), Srgba::rgb(0.067, 0.067, 0.106)); // Palette.swift:114
        assert_eq!(srgb(CATPPUCCIN_MOCHA.panel), Srgba::rgb(0.118, 0.118, 0.180)); // Palette.swift:128
        assert_eq!(srgb(CATPPUCCIN_MOCHA.ink), Srgba::rgb(0.804, 0.839, 0.957)); // Palette.swift:144
        assert_eq!(srgb(CATPPUCCIN_MOCHA.ink2), Srgba::rgb(0.729, 0.761, 0.871)); // Palette.swift:158
        assert_eq!(srgb(CATPPUCCIN_MOCHA.ink3), Srgba::rgb(0.651, 0.678, 0.784)); // Palette.swift:172
        assert_eq!(srgb(CATPPUCCIN_MOCHA.line), Srgba::rgb(0.192, 0.196, 0.267)); // Palette.swift:188
        assert_eq!(srgb(CATPPUCCIN_MOCHA.line_strong), Srgba::rgb(0.271, 0.278, 0.353)); // Palette.swift:202
        assert_eq!(srgb(CATPPUCCIN_MOCHA.user_bubble), Srgba::rgb(0.192, 0.196, 0.267)); // Palette.swift:239
        assert_eq!(
            srgb(CATPPUCCIN_MOCHA.chrome),
            Srgba::new(0.118, 0.118, 0.180, 0.70)
        ); // Palette.swift:255
    }

    #[test]
    fn chrome_slot_is_background_at_chrome_opacity() {
        // niceChrome == niceBg with alpha dropped to 0.70 (Palette.swift:245).
        for slots in [NICE_LIGHT, NICE_DARK, CATPPUCCIN_LATTE, CATPPUCCIN_MOCHA] {
            let bg = srgb(slots.background);
            let chrome = srgb(slots.chrome);
            assert_eq!(chrome, Srgba::new(bg.r, bg.g, bg.b, CHROME_OPACITY));
        }
        assert_eq!(CHROME_OPACITY, 0.70); // Palette.swift:250-251
    }

    #[test]
    fn matches_mirrors_swift() {
        // Tweaks.swift:51-57.
        assert!(Palette::Nice.matches(ColorScheme::Light));
        assert!(Palette::Nice.matches(ColorScheme::Dark));
        assert!(Palette::CatppuccinLatte.matches(ColorScheme::Light));
        assert!(!Palette::CatppuccinLatte.matches(ColorScheme::Dark));
        assert!(!Palette::CatppuccinMocha.matches(ColorScheme::Light));
        assert!(Palette::CatppuccinMocha.matches(ColorScheme::Dark));
    }

    #[test]
    fn slots_resolves_valid_combos_only() {
        // Valid combos return a table; the two off-scheme Catppuccin combos
        // return None (mirrors matches()).
        assert_eq!(slots(Palette::Nice, ColorScheme::Light), Some(NICE_LIGHT));
        assert_eq!(slots(Palette::Nice, ColorScheme::Dark), Some(NICE_DARK));
        assert_eq!(
            slots(Palette::CatppuccinLatte, ColorScheme::Light),
            Some(CATPPUCCIN_LATTE)
        );
        assert_eq!(slots(Palette::CatppuccinLatte, ColorScheme::Dark), None);
        assert_eq!(
            slots(Palette::CatppuccinMocha, ColorScheme::Dark),
            Some(CATPPUCCIN_MOCHA)
        );
        assert_eq!(slots(Palette::CatppuccinMocha, ColorScheme::Light), None);

        // slots() is Some exactly where matches() is true.
        for &p in &Palette::ALL {
            for scheme in [ColorScheme::Light, ColorScheme::Dark] {
                assert_eq!(p.matches(scheme), slots(p, scheme).is_some());
            }
        }
    }

    #[test]
    fn palette_raw_values_match_swift() {
        // Tweaks.swift:33-34 — enum String rawValues.
        assert_eq!(Palette::Nice.raw_value(), "nice");
        assert_eq!(Palette::CatppuccinLatte.raw_value(), "catppuccinLatte");
        assert_eq!(Palette::CatppuccinMocha.raw_value(), "catppuccinMocha");
    }
}
