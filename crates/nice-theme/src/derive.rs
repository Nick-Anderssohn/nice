//! Procedural chrome derivation for merged themes (round-2 restyle plan 5,
//! `docs/plans/restyle/05-theme-merge.md`).
//!
//! With the restyle there is barely any chrome left, so a theme's chrome half
//! no longer needs to be hand-authored for every terminal theme. Only
//! `nice-default-*` (→ [`NICE_LIGHT`]/[`NICE_DARK`]) and `catppuccin-*`
//! (→ [`CATPPUCCIN_LATTE`]/[`CATPPUCCIN_MOCHA`]) keep hand-tuned chrome; every
//! other built-in and every imported Ghostty theme derives its chrome
//! procedurally from the terminal's foreground and background colors via
//! [`derive_chrome`].
//!
//! ## Layering
//!
//! Inputs are plain [`Srgba`] (the nice-theme color type), NOT a
//! `nice-term-view` color — nice-theme must not depend on the terminal view
//! crate (crates/README.md "Layering rule"). The consumer in `crates/nice`
//! (plan 5, slice 2) converts the terminal theme's fg/bg into [`Srgba`] before
//! calling here.
//!
//! ## How the slots are derived
//!
//! Nothing here invents a ratio. Every blend factor below was MEASURED from the
//! hand-tuned Nice palettes ([`NICE_LIGHT`]/[`NICE_DARK`] in
//! [`crate::palette`]) by solving `slot = base + t·(target − base)` per channel
//! and averaging the three channels; the measured factors are pinned as the
//! [`DARK`]/[`LIGHT`] constants and reused verbatim. See the module tests: a
//! round-trip that derives from Nice's own fg/bg reproduces the real Nice slots
//! within tolerance, which is what proves the factors are faithful rather than
//! guessed.
//!
//! * `background` = the terminal background, unchanged.
//! * `panel` / `background2` / `background3` = the background blended along the
//!   background→foreground axis by a small per-scheme factor. The factor is
//!   SIGNED and its sign is intentionally scheme-dependent: Nice's dark palette
//!   recesses `background2`/`background3` AWAY from the light foreground (they
//!   go darker) while its light palette recesses them TOWARD the dark
//!   foreground — so "nudge toward fg" (the plan's shorthand) is literally true
//!   only for light; the binding rule is "match Nice's bg↔bg2/bg3 relationship",
//!   which the signed per-scheme factors do exactly. `panel` is the raised
//!   surface (lighter than the background in both schemes).
//! * `ink` = the terminal foreground, unchanged. `ink2` / `ink3` = the
//!   foreground blended toward the background by the measured Nice ink-ramp
//!   factors, replicating Nice's ink:ink2:ink3 contrast relationship.
//! * `line` / `line_strong` / `user_bubble` = the background blended toward the
//!   foreground by their measured Nice factors (dividers/bubbles sit slightly
//!   inked over the surface).
//! * `chrome` = the background at [`CHROME_OPACITY`] (same rule as every
//!   hand-tuned palette: `chrome` is `background` with alpha dropped to 0.70).
//!
//! The accent slot is NOT part of chrome derivation — the user's accent setting
//! stays independent (plan 5 Decisions).
//!
//! The over-glass hairlines/fills ([`crate::glass`]) are unchanged and are NOT
//! palette slots (they are scheme-scoped alpha-over-surface values), so they are
//! not derived here.
//!
//! Per-theme hand-tuned overrides MAY be added later if a derived palette looks
//! off; that is out of scope for plan 5 (derivation only).
//!
//! [`NICE_LIGHT`]: crate::palette::NICE_LIGHT
//! [`NICE_DARK`]: crate::palette::NICE_DARK
//! [`CATPPUCCIN_LATTE`]: crate::palette::CATPPUCCIN_LATTE
//! [`CATPPUCCIN_MOCHA`]: crate::palette::CATPPUCCIN_MOCHA
//! [`CHROME_OPACITY`]: crate::palette::CHROME_OPACITY

use crate::color::Srgba;
use crate::palette::{SlotColor, Slots, CHROME_OPACITY};

/// The per-scheme blend factors for one chrome derivation. Each is a fraction
/// `t` in a `base + t·(target − base)` blend; surface/line factors blend the
/// background toward the foreground, ink factors blend the foreground toward the
/// background. All values are channel-averaged measurements from the hand-tuned
/// Nice palettes — see the module docs and the round-trip test.
#[derive(Clone, Copy, Debug)]
struct DerivationFactors {
    /// `panel` blended from background toward foreground.
    panel: f32,
    /// `background2` blended from background toward foreground (signed).
    background2: f32,
    /// `background3` blended from background toward foreground (signed).
    background3: f32,
    /// `line` blended from background toward foreground.
    line: f32,
    /// `line_strong` blended from background toward foreground.
    line_strong: f32,
    /// `user_bubble` blended from background toward foreground.
    user_bubble: f32,
    /// `ink2` blended from foreground toward background.
    ink2: f32,
    /// `ink3` blended from foreground toward background.
    ink3: f32,
}

/// Dark-scheme factors, measured (channel-averaged) from [`NICE_DARK`].
///
/// Provenance — solving `slot = bg + t·(fg − bg)` (surfaces/lines) and
/// `slot = fg + t·(bg − fg)` (inks) per channel against `NICE_DARK`
/// (`crates/nice-theme/src/palette.rs`) and averaging the three channels.
/// `background2`/`background3` are negative because Nice's dark surfaces recede
/// away from the light foreground.
///
/// [`NICE_DARK`]: crate::palette::NICE_DARK
const DARK: DerivationFactors = DerivationFactors {
    panel: 0.019_33,
    background2: -0.023_88,
    background3: -0.045_10,
    line: 0.103_45,
    line_strong: 0.193_26,
    user_bubble: 0.060_63,
    ink2: 0.303_52,
    ink3: 0.572_93,
};

/// Light-scheme factors, measured (channel-averaged) from [`NICE_LIGHT`].
///
/// Provenance — same solve as [`DARK`], against `NICE_LIGHT`
/// (`crates/nice-theme/src/palette.rs`). Here `background2`/`background3` are
/// positive (Nice's light surfaces recede toward the dark foreground) and
/// `panel` is negative (the raised surface goes whiter, away from the
/// foreground).
///
/// [`NICE_LIGHT`]: crate::palette::NICE_LIGHT
const LIGHT: DerivationFactors = DerivationFactors {
    panel: -0.015_11,
    background2: 0.028_75,
    background3: 0.065_25,
    line: 0.151_16,
    line_strong: 0.290_53,
    user_bubble: 0.065_59,
    ink2: 0.202_43,
    ink3: 0.444_34,
};

/// WCAG relative luminance of an sRGB color (ignoring alpha). Used to decide
/// whether a terminal theme reads as dark or light so the matching measured
/// factor set is applied. Standard sRGB linearization + Rec. 709 weights
/// (WCAG 2.1 §"relative luminance").
fn relative_luminance(c: Srgba) -> f32 {
    fn linearize(channel: f32) -> f32 {
        if channel <= 0.039_28 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * linearize(c.r) + 0.7152 * linearize(c.g) + 0.0722 * linearize(c.b)
}

/// `base` blended toward `target` by `t` (per channel), opaque. `t` may be
/// negative (blend away from `target`).
fn blend(base: Srgba, target: Srgba, t: f32) -> Srgba {
    Srgba::rgb(
        base.r + t * (target.r - base.r),
        base.g + t * (target.g - base.g),
        base.b + t * (target.b - base.b),
    )
}

/// Derive a full chrome [`Slots`] table from a terminal theme's foreground and
/// background colors.
///
/// Every slot is filled with a concrete [`SlotColor::Srgb`] value (no slot falls
/// back to a stale Nice/Catppuccin constant). The scheme (dark vs light) is detected from the
/// relative luminance of `bg` vs `fg`, selecting the [`DARK`] or [`LIGHT`]
/// measured factor set. Input alpha is ignored; outputs are opaque except
/// `chrome`, which is the background at [`CHROME_OPACITY`].
///
/// [`CHROME_OPACITY`]: crate::palette::CHROME_OPACITY
pub fn derive_chrome(fg: Srgba, bg: Srgba) -> Slots {
    let factors = if relative_luminance(bg) < relative_luminance(fg) {
        DARK
    } else {
        LIGHT
    };

    Slots {
        background: SlotColor::Srgb(Srgba::rgb(bg.r, bg.g, bg.b)),
        background2: SlotColor::Srgb(blend(bg, fg, factors.background2)),
        background3: SlotColor::Srgb(blend(bg, fg, factors.background3)),
        panel: SlotColor::Srgb(blend(bg, fg, factors.panel)),
        ink: SlotColor::Srgb(Srgba::rgb(fg.r, fg.g, fg.b)),
        ink2: SlotColor::Srgb(blend(fg, bg, factors.ink2)),
        ink3: SlotColor::Srgb(blend(fg, bg, factors.ink3)),
        line: SlotColor::Srgb(blend(bg, fg, factors.line)),
        line_strong: SlotColor::Srgb(blend(bg, fg, factors.line_strong)),
        user_bubble: SlotColor::Srgb(blend(bg, fg, factors.user_bubble)),
        chrome: SlotColor::Srgb(Srgba::new(bg.r, bg.g, bg.b, CHROME_OPACITY)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::{
        CATPPUCCIN_LATTE, CATPPUCCIN_MOCHA, NICE_DARK, NICE_LIGHT,
    };

    /// Unwrap an sRGB slot (test-only helper).
    fn srgb(slot: SlotColor) -> Srgba {
        let SlotColor::Srgb(s) = slot;
        s
    }

    /// WCAG 2.1 contrast ratio between two sRGB colors, reusing the same
    /// luminance the derivation uses for scheme detection.
    fn contrast(a: Srgba, b: Srgba) -> f32 {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    fn approx_eq(a: Srgba, b: Srgba, tol: f32) -> bool {
        (a.r - b.r).abs() <= tol && (a.g - b.g).abs() <= tol && (a.b - b.b).abs() <= tol
    }

    /// Every slot the derivation writes, as (name, value) pairs, for iterating.
    fn all_slots(s: &Slots) -> [(&'static str, SlotColor); 11] {
        [
            ("background", s.background),
            ("background2", s.background2),
            ("background3", s.background3),
            ("panel", s.panel),
            ("ink", s.ink),
            ("ink2", s.ink2),
            ("ink3", s.ink3),
            ("line", s.line),
            ("line_strong", s.line_strong),
            ("user_bubble", s.user_bubble),
            ("chrome", s.chrome),
        ]
    }

    #[test]
    fn scheme_detection_orders_by_luminance() {
        // Sanity on the discriminator: a lighter color has higher luminance, so
        // bg<fg ⇒ dark. If this inverted, the reproduction tests below would
        // reproduce the WRONG palette (they pin the discriminator indirectly).
        assert!(relative_luminance(Srgba::rgb(1.0, 1.0, 1.0)) > relative_luminance(Srgba::rgb(0.0, 0.0, 0.0)));
        assert!(
            relative_luminance(srgb(NICE_LIGHT.background))
                > relative_luminance(srgb(NICE_LIGHT.ink))
        );
        assert!(
            relative_luminance(srgb(NICE_DARK.background))
                < relative_luminance(srgb(NICE_DARK.ink))
        );
    }

    #[test]
    fn derives_from_nice_dark_reproduces_the_dark_ramp() {
        // Feeding the derivation Nice's own dark fg/bg must reproduce the
        // hand-tuned NICE_DARK slots within tolerance — this is what proves the
        // measured DARK factors are faithful, not invented (and that dark-scheme
        // detection picked the DARK set).
        let d = derive_chrome(srgb(NICE_DARK.ink), srgb(NICE_DARK.background));
        // background/ink/chrome are exact copies.
        assert_eq!(srgb(d.background), srgb(NICE_DARK.background));
        assert_eq!(srgb(d.ink), srgb(NICE_DARK.ink));
        assert_eq!(
            srgb(d.chrome),
            Srgba::new(
                srgb(NICE_DARK.background).r,
                srgb(NICE_DARK.background).g,
                srgb(NICE_DARK.background).b,
                CHROME_OPACITY
            )
        );
        // The blended slots reproduce Nice's within a small tolerance.
        let tol = 0.02;
        for (name, actual, expected) in [
            ("background2", d.background2, NICE_DARK.background2),
            ("background3", d.background3, NICE_DARK.background3),
            ("panel", d.panel, NICE_DARK.panel),
            ("ink2", d.ink2, NICE_DARK.ink2),
            ("ink3", d.ink3, NICE_DARK.ink3),
            ("line", d.line, NICE_DARK.line),
            ("line_strong", d.line_strong, NICE_DARK.line_strong),
            ("user_bubble", d.user_bubble, NICE_DARK.user_bubble),
        ] {
            assert!(
                approx_eq(srgb(actual), srgb(expected), tol),
                "derived dark {name} {:?} not within {tol} of NICE_DARK {:?}",
                srgb(actual),
                srgb(expected)
            );
        }
    }

    #[test]
    fn derives_from_nice_light_reproduces_the_light_ramp() {
        let d = derive_chrome(srgb(NICE_LIGHT.ink), srgb(NICE_LIGHT.background));
        assert_eq!(srgb(d.background), srgb(NICE_LIGHT.background));
        assert_eq!(srgb(d.ink), srgb(NICE_LIGHT.ink));
        assert_eq!(
            srgb(d.chrome),
            Srgba::new(
                srgb(NICE_LIGHT.background).r,
                srgb(NICE_LIGHT.background).g,
                srgb(NICE_LIGHT.background).b,
                CHROME_OPACITY
            )
        );
        let tol = 0.02;
        for (name, actual, expected) in [
            ("background2", d.background2, NICE_LIGHT.background2),
            ("background3", d.background3, NICE_LIGHT.background3),
            ("panel", d.panel, NICE_LIGHT.panel),
            ("ink2", d.ink2, NICE_LIGHT.ink2),
            ("ink3", d.ink3, NICE_LIGHT.ink3),
            ("line", d.line, NICE_LIGHT.line),
            ("line_strong", d.line_strong, NICE_LIGHT.line_strong),
            ("user_bubble", d.user_bubble, NICE_LIGHT.user_bubble),
        ] {
            assert!(
                approx_eq(srgb(actual), srgb(expected), tol),
                "derived light {name} {:?} not within {tol} of NICE_LIGHT {:?}",
                srgb(actual),
                srgb(expected)
            );
        }
    }

    #[test]
    fn ink_ramp_clears_the_nice_contrast_floor() {
        // Floors pinned from what the hand-tuned Nice palettes achieve for the
        // ink ramp vs. their own background (computed from palette.rs via the
        // same WCAG formula):
        //   NICE_DARK :  ink 16.752 · ink2 8.451 · ink3 3.870
        //   NICE_LIGHT:  ink 17.683 · ink2 9.586 · ink3 4.111
        // Deriving from Nice's own fg/bg must clear these floors (the derivation
        // reproduces Nice's ramp within tolerance, so a 2% margin absorbs the
        // blend rounding). Real terminal themes have lower intrinsic fg/bg
        // contrast than Nice; the per-catalog contrast checks over the actual
        // built-ins live with the consumer wiring (plan 5, slice 2) — this test
        // pins the derivation's OWN contrast behavior against the Nice
        // reference.
        let cases = [
            ("dark", NICE_DARK, [16.752_f32, 8.451, 3.870]),
            ("light", NICE_LIGHT, [17.683_f32, 9.586, 4.111]),
        ];
        for (label, palette, floors) in cases {
            let bg = srgb(palette.background);
            let d = derive_chrome(srgb(palette.ink), bg);
            let ramp = [
                ("ink", srgb(d.ink)),
                ("ink2", srgb(d.ink2)),
                ("ink3", srgb(d.ink3)),
            ];
            for ((name, color), floor) in ramp.into_iter().zip(floors) {
                let got = contrast(color, srgb(d.background));
                assert!(
                    got >= floor * 0.98,
                    "{label} {name} contrast {got:.3} below Nice floor {floor:.3}"
                );
            }
        }
    }

    #[test]
    fn ink_ramp_is_monotonically_ordered() {
        // ink is the most legible, ink3 the least, for any input — the ramp must
        // not cross. Exercised on a dark and a light synthetic input.
        for (fg, bg) in [
            (Srgba::rgb(0.90, 0.85, 0.70), Srgba::rgb(0.10, 0.12, 0.16)),
            (Srgba::rgb(0.18, 0.16, 0.22), Srgba::rgb(0.95, 0.93, 0.88)),
        ] {
            let d = derive_chrome(fg, bg);
            let base = srgb(d.background);
            let c_ink = contrast(srgb(d.ink), base);
            let c_ink2 = contrast(srgb(d.ink2), base);
            let c_ink3 = contrast(srgb(d.ink3), base);
            assert!(c_ink >= c_ink2, "ink {c_ink:.3} < ink2 {c_ink2:.3}");
            assert!(c_ink2 >= c_ink3, "ink2 {c_ink2:.3} < ink3 {c_ink3:.3}");
        }
    }

    #[test]
    fn every_slot_is_a_filled_srgb() {
        // No derived slot may be a deferred system-semantic slot, and the three
        // anchored slots carry exactly their inputs.
        let fg = Srgba::rgb(0.82, 0.86, 0.55);
        let bg = Srgba::rgb(0.09, 0.14, 0.19);
        let d = derive_chrome(fg, bg);
        for (name, slot) in all_slots(&d) {
            assert!(
                matches!(slot, SlotColor::Srgb(_)),
                "slot {name} is not a concrete sRGB value"
            );
        }
        assert_eq!(srgb(d.background), Srgba::rgb(bg.r, bg.g, bg.b));
        assert_eq!(srgb(d.ink), Srgba::rgb(fg.r, fg.g, fg.b));
        assert_eq!(srgb(d.chrome), Srgba::new(bg.r, bg.g, bg.b, CHROME_OPACITY));
    }

    #[test]
    fn no_slot_equals_an_unrelated_palette_constant() {
        // A derived theme must not accidentally reuse a Nice/Catppuccin constant
        // for any slot (the plan forbids stale fallbacks). Derive from a
        // distinctive teal/olive pair that matches no hand-tuned palette and
        // assert every slot differs from that same slot in all four constants.
        let d = derive_chrome(Srgba::rgb(0.61, 0.83, 0.74), Srgba::rgb(0.07, 0.19, 0.23));
        let unrelated = [
            ("NICE_LIGHT", &NICE_LIGHT),
            ("NICE_DARK", &NICE_DARK),
            ("CATPPUCCIN_LATTE", &CATPPUCCIN_LATTE),
            ("CATPPUCCIN_MOCHA", &CATPPUCCIN_MOCHA),
        ];
        for ((name, derived), _idx) in all_slots(&d).into_iter().zip(0..) {
            for (pal_name, pal) in unrelated {
                let their = all_slots(pal)[_idx].1;
                assert_ne!(
                    derived, their,
                    "derived slot {name} equals {pal_name}'s {name} constant"
                );
            }
        }
    }
}
