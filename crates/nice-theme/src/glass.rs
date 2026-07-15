//! "Line over glass" and "fill over glass" color primitives for the 2026-07
//! restyle (plan `docs/plans/restyle/02-sidebar-flatten.md`).
//!
//! Plan 2's flattened sidebar shares the window-body surface instead of
//! floating as its own panel, so its hairline divider and its active/hover
//! fills can no longer be the opaque theme `line` slot (that slot assumes a
//! panel sits on top of a distinct background and would look wrong once the
//! surface is shared, and wrong again once plan 3 makes the window
//! translucent). Both scale a scheme-fixed white/ink value by alpha instead of
//! reading a palette slot — hence "glass": they read correctly whether the
//! surface behind them is the opaque terminal background (plan 2) or, later,
//! an actually-translucent one (plan 3).
//!
//! Values are cited verbatim from `docs/design/restyle-mocks.html`'s
//! `--hairline` / `--fill-active` custom properties, which are scheme-scoped
//! and deliberately NOT palette slots (see the mock's own comment: "over-glass
//! hairlines + fills (scheme-scoped, not theme slots)").

use crate::color::Srgba;
use crate::palette::ColorScheme;

/// The over-glass hairline color for `scheme`. Cites
/// `docs/design/restyle-mocks.html`'s `--hairline`:
/// `rgba(255,255,255,.08)` (dark) / `rgba(23,19,15,.10)` (light — the same rgb
/// as the `ink` slot, at 10% alpha).
pub fn glass_line(scheme: ColorScheme) -> Srgba {
    match scheme {
        ColorScheme::Dark => Srgba::new(1.0, 1.0, 1.0, 0.08),
        ColorScheme::Light => Srgba::new(23.0 / 255.0, 19.0 / 255.0, 15.0 / 255.0, 0.10),
    }
}

/// The over-glass active/hover fill color for `scheme` — the companion to
/// [`glass_line`], used for the sidebar's active-row / hover / footer-icon
/// fills (NOT the hairline's alpha family). Cites
/// `docs/design/restyle-mocks.html`'s `--fill-active`:
/// `rgba(255,255,255,.06)` (dark) / `rgba(23,19,15,.05)` (light).
pub fn glass_fill(scheme: ColorScheme) -> Srgba {
    match scheme {
        ColorScheme::Dark => Srgba::new(1.0, 1.0, 1.0, 0.06),
        ColorScheme::Light => Srgba::new(23.0 / 255.0, 19.0 / 255.0, 15.0 / 255.0, 0.05),
    }
}

#[cfg(test)]
mod tests {
    //! Literal-equality fixtures citing the mock's `--hairline` /
    //! `--fill-active` custom properties. See crates/README.md
    //! "Fixture-provenance convention".
    use super::*;

    #[test]
    fn glass_line_matches_the_mock_hairline() {
        // docs/design/restyle-mocks.html: --hairline: rgba(255,255,255,.08) (dark).
        assert_eq!(glass_line(ColorScheme::Dark), Srgba::new(1.0, 1.0, 1.0, 0.08));
        // docs/design/restyle-mocks.html: --hairline: rgba(23,19,15,.10) (light).
        assert_eq!(
            glass_line(ColorScheme::Light),
            Srgba::new(23.0 / 255.0, 19.0 / 255.0, 15.0 / 255.0, 0.10)
        );
    }

    #[test]
    fn glass_fill_matches_the_mock_fill_active() {
        // docs/design/restyle-mocks.html: --fill-active: rgba(255,255,255,.06) (dark).
        assert_eq!(glass_fill(ColorScheme::Dark), Srgba::new(1.0, 1.0, 1.0, 0.06));
        // docs/design/restyle-mocks.html: --fill-active: rgba(23,19,15,.05) (light).
        assert_eq!(
            glass_fill(ColorScheme::Light),
            Srgba::new(23.0 / 255.0, 19.0 / 255.0, 15.0 / 255.0, 0.05)
        );
    }

    #[test]
    fn glass_line_and_glass_fill_are_distinct_alpha_families() {
        // Plan 2 decision: the mode-switcher / active-row fill is explicitly
        // NOT the glass-line 8%/10% family.
        for scheme in [ColorScheme::Dark, ColorScheme::Light] {
            assert_ne!(glass_line(scheme).a, glass_fill(scheme).a);
        }
    }

    #[test]
    fn light_variants_share_the_ink_slot_rgb() {
        // docs/design/restyle-mocks.html light --ink: #17130f == (23,19,15) —
        // both over-glass helpers tint with the ink rgb at their own alpha.
        let line = glass_line(ColorScheme::Light);
        let fill = glass_fill(ColorScheme::Light);
        assert_eq!((line.r, line.g, line.b), (fill.r, fill.g, fill.b));
        assert_eq!((line.r, line.g, line.b), (23.0 / 255.0, 19.0 / 255.0, 15.0 / 255.0));
    }
}
