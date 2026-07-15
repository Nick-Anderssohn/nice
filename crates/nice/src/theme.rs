//! The token → `gpui::Rgba` colour adapter.
//!
//! `nice-theme` is deliberately gpui-free (crates/README.md "Layering rule"):
//! it exposes plain [`Srgba`] literals and semantic [`SlotColor`] slots, and the
//! **app** owns the tiny conversion into `gpui`'s colour type. `app.rs` already
//! carries a private copy of this adapter for the chrome band's two slots; this
//! module is the shared home the R10/R11 chrome components (StatusDot, the
//! context menu, and slice 3's sidebar views) convert through, so the mapping
//! lives in exactly one place for the growing set of chrome that needs it.

use gpui::Rgba;
use nice_theme::color::Srgba;
use nice_theme::palette::SlotColor;

/// Convert a nice-theme [`Srgba`] to a `gpui` [`Rgba`]. Both are gamma-encoded
/// sRGB with straight alpha and identical `f32` `0.0..=1.0` channels, so this is
/// a lossless field copy.
pub(crate) fn srgba_to_rgba(c: Srgba) -> Rgba {
    Rgba {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
}

/// Resolve a [`SlotColor`] to a concrete [`Srgba`]. Every chrome slot is an sRGB
/// literal after the round-2 merge (hand-tuned or derived), so this is a plain
/// unwrap — the paint-time `NSColor` system slots that used to need a fallback
/// here are gone.
pub(crate) fn slot_srgba(slot: SlotColor) -> Srgba {
    let SlotColor::Srgb(c) = slot;
    c
}

/// Resolve a [`SlotColor`] straight to a `gpui` [`Rgba`] — [`slot_srgba`] then
/// [`srgba_to_rgba`].
pub(crate) fn slot_to_rgba(slot: SlotColor) -> Rgba {
    srgba_to_rgba(slot_srgba(slot))
}

/// The same [`Srgba`] with its straight alpha replaced by `alpha` (clamped to
/// `0.0..=1.0`) — used to derive translucent chrome fills (hover tints,
/// separators) from an opaque slot colour.
pub(crate) fn srgba_with_alpha(c: Srgba, alpha: f32) -> Srgba {
    Srgba::new(c.r, c.g, c.b, alpha.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgba_to_rgba_is_a_field_copy() {
        let c = Srgba::new(0.1, 0.2, 0.3, 0.4);
        let g = srgba_to_rgba(c);
        assert_eq!((g.r, g.g, g.b, g.a), (0.1, 0.2, 0.3, 0.4));
    }

    #[test]
    fn slot_srgba_passes_through_literals() {
        let c = Srgba::rgb(0.5, 0.6, 0.7);
        assert_eq!(slot_srgba(SlotColor::Srgb(c)), c);
    }

    #[test]
    fn srgba_with_alpha_replaces_alpha_and_clamps() {
        let c = Srgba::rgb(0.2, 0.4, 0.6);
        assert_eq!(srgba_with_alpha(c, 0.06), Srgba::new(0.2, 0.4, 0.6, 0.06));
        assert_eq!(srgba_with_alpha(c, 2.0).a, 1.0);
        assert_eq!(srgba_with_alpha(c, -1.0).a, 0.0);
    }
}
