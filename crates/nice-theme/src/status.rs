//! Status-dot visual tokens, ported verbatim from
//! `Sources/Nice/Views/StatusDot.swift`. These are the cited constants the R10
//! `StatusDot` component (in `crates/nice`) paints — the 8pt dot, the fixed
//! "waiting" blue, and the two repeat-forever pulse animations (an expanding
//! outer ring and a breathing inner dot).
//!
//! Layering note: this module is **pure design data** with no `gpui` and no
//! `nice-model` dependency. The mapping from a `TabStatus` to *which* of these
//! tokens applies — and the token → `gpui` animation/colour adaptation — lives
//! in the `StatusDot` component downstream, which is what keeps this crate
//! gpui-free (crates/README.md "Layering rule"). The "thinking" dot colour is
//! the user's accent ([`crate::AccentPreset`]) and the "idle" dot colour is the
//! active palette's `ink3` slot ([`crate::Slots::ink3`]); neither is duplicated
//! here — only the tokens the Swift `StatusDot` hardcodes are.

use crate::color::Srgba;

/// Diameter (pt) of the inner status dot. `StatusDot.swift:19` (`var size = 8`).
pub const DOT_SIZE: f32 = 8.0;

/// Padding (pt) added around the dot for the outer frame the ring expands
/// within — the SwiftUI `.frame(width: size + 4, …)` (`StatusDot.swift:70,93`).
/// The frame is `DOT_SIZE + DOT_FRAME_PADDING` on a side.
pub const DOT_FRAME_PADDING: f32 = 4.0;

/// The "waiting" dot colour — an sRGB approximation of `oklch(0.65 0.14 250)`,
/// verbatim from `StatusDot.swift:33` (`Color(.sRGB, red: 0.48, green: 0.58,
/// blue: 0.86)`). This is the one dot colour Swift hardcodes; "thinking" uses
/// the user's accent and "idle" uses the palette's `ink3`, so those are not
/// duplicated here.
pub const WAITING_DOT: Srgba = Srgba::rgb(0.48, 0.58, 0.86);

/// The expanding outer ring animation for one status. The ring starts at the
/// dot's outer frame (scale `1.0`, opacity [`start_opacity`](Self::start_opacity))
/// and grows to [`max_scale`](Self::max_scale) while fading to `0`, over
/// [`duration_secs`](Self::duration_secs), repeating forever without
/// autoreversing (`StatusDot.swift:91-102`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RingPulse {
    /// Scale the ring reaches at the end of each cycle. `StatusDot.swift:94`
    /// (`pulsing ? (waiting ? 2.0 : 1.6) : 1.0`).
    pub max_scale: f32,
    /// Ring opacity at the start of each cycle; it fades to `0` by the end.
    /// `StatusDot.swift:95` (`pulsing ? 0.0 : (waiting ? 0.7 : 0.6)`).
    pub start_opacity: f32,
    /// One cycle's duration (s) — the SwiftUI `easeOut(duration:)`
    /// repeat-forever period. `StatusDot.swift:99`
    /// (`.easeOut(duration: waiting ? 1.2 : 1.6)`).
    pub duration_secs: f32,
}

/// The breathing inner-dot animation for one status. The dot's opacity
/// oscillates between [`min_opacity`](Self::min_opacity) and
/// [`BREATHE_MAX_OPACITY`] with an ease-in-out that **autoreverses**, so one
/// there-and-back cycle takes `2 × duration_secs` (`StatusDot.swift:104-113`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BreathePulse {
    /// The dimmest opacity of the breathing dot. `StatusDot.swift:107`
    /// (`pulsing ? 1.0 : (waiting ? 0.4 : 0.5)`); it brightens to
    /// [`BREATHE_MAX_OPACITY`].
    pub min_opacity: f32,
    /// One HALF-cycle's duration (s) — the SwiftUI `easeInOut(duration:)` value,
    /// which then autoreverses, so a full min→max→min breath is `2 ×` this
    /// (the module header's "0.5↔1.0 opacity at 1.4s" is `2 × 0.7`).
    /// `StatusDot.swift:111` (`.easeInOut(duration: waiting ? 0.9 : 0.7)`).
    pub duration_secs: f32,
}

/// The brightest opacity the breathing inner dot reaches. `StatusDot.swift:107`
/// (`pulsing ? 1.0 : …`).
pub const BREATHE_MAX_OPACITY: f32 = 1.0;

/// Outer-ring pulse for the "thinking" status: ring grows to `1.6×` from
/// opacity `0.6`, over `1.6 s`. `StatusDot.swift:94,95,99` (the non-`waiting`
/// arms).
pub const THINKING_RING: RingPulse = RingPulse {
    max_scale: 1.6,
    start_opacity: 0.6,
    duration_secs: 1.6,
};

/// Outer-ring pulse for the "waiting" status: ring grows to `2.0×` from opacity
/// `0.7`, over `1.2 s`. `StatusDot.swift:94,95,99` (the `waiting` arms).
pub const WAITING_RING: RingPulse = RingPulse {
    max_scale: 2.0,
    start_opacity: 0.7,
    duration_secs: 1.2,
};

/// Inner-dot breathe for the "thinking" status: opacity `0.5 ↔ 1.0`, half-cycle
/// `0.7 s`. `StatusDot.swift:107,111` (the non-`waiting` arms).
pub const THINKING_BREATHE: BreathePulse = BreathePulse {
    min_opacity: 0.5,
    duration_secs: 0.7,
};

/// Inner-dot breathe for the "waiting" status: opacity `0.4 ↔ 1.0`, half-cycle
/// `0.9 s`. `StatusDot.swift:107,111` (the `waiting` arms).
pub const WAITING_BREATHE: BreathePulse = BreathePulse {
    min_opacity: 0.4,
    duration_secs: 0.9,
};

#[cfg(test)]
mod tests {
    //! Provenance-cited fixtures for the status-dot tokens. Every expected value
    //! is an independent transcription from the cited `StatusDot.swift` line
    //! (double-entry against the constants above). See crates/README.md
    //! "Fixture-provenance convention".
    use super::*;

    #[test]
    fn dot_geometry_matches_swift() {
        assert_eq!(DOT_SIZE, 8.0); // StatusDot.swift:19
        assert_eq!(DOT_FRAME_PADDING, 4.0); // StatusDot.swift:70,93 (size + 4)
    }

    #[test]
    fn waiting_dot_colour_matches_swift() {
        // StatusDot.swift:33 — Color(.sRGB, red: 0.48, green: 0.58, blue: 0.86).
        assert_eq!(WAITING_DOT, Srgba::rgb(0.48, 0.58, 0.86));
        assert_eq!(WAITING_DOT.a, 1.0);
    }

    #[test]
    fn thinking_ring_matches_swift() {
        // StatusDot.swift:94,95,99 — the non-waiting arms.
        assert_eq!(THINKING_RING.max_scale, 1.6);
        assert_eq!(THINKING_RING.start_opacity, 0.6);
        assert_eq!(THINKING_RING.duration_secs, 1.6);
    }

    #[test]
    fn waiting_ring_matches_swift() {
        // StatusDot.swift:94,95,99 — the waiting arms.
        assert_eq!(WAITING_RING.max_scale, 2.0);
        assert_eq!(WAITING_RING.start_opacity, 0.7);
        assert_eq!(WAITING_RING.duration_secs, 1.2);
    }

    #[test]
    fn thinking_breathe_matches_swift() {
        // StatusDot.swift:107,111 — the non-waiting arms.
        assert_eq!(THINKING_BREATHE.min_opacity, 0.5);
        assert_eq!(THINKING_BREATHE.duration_secs, 0.7);
    }

    #[test]
    fn waiting_breathe_matches_swift() {
        // StatusDot.swift:107,111 — the waiting arms.
        assert_eq!(WAITING_BREATHE.min_opacity, 0.4);
        assert_eq!(WAITING_BREATHE.duration_secs, 0.9);
    }

    #[test]
    fn breathe_max_opacity_matches_swift() {
        assert_eq!(BREATHE_MAX_OPACITY, 1.0); // StatusDot.swift:107 (pulsing ? 1.0)
    }
}
