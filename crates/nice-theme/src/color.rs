//! Color value types shared by the palette tables.
//!
//! `Srgba` is a plain gamma-encoded sRGB color with straight alpha, mirroring
//! Swift's `Color(.sRGB, red:green:blue:opacity:)` (`Palette.swift`).
//! Components are `f32` in `0.0..=1.0` — the same representation gpui's `Rgba`
//! uses, so the R9 adapter (which lives downstream, NOT here — see the crate's
//! no-gpui rule) converts without loss.

/// A gamma-encoded sRGB color with straight (non-premultiplied) alpha.
///
/// Ported verbatim from the `Color(.sRGB, red:green:blue:opacity:)` literals in
/// `Sources/Nice/Theme/Palette.swift`. `Palette.swift`'s header records that
/// those literals were produced offline by an oklch → sRGB conversion; the
/// literals are the design system of record, so we port them as-is and do NOT
/// reimplement the conversion (there is no Swift conversion function to verify
/// against).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Srgba {
    /// Red channel, `0.0..=1.0`.
    pub r: f32,
    /// Green channel, `0.0..=1.0`.
    pub g: f32,
    /// Blue channel, `0.0..=1.0`.
    pub b: f32,
    /// Straight alpha, `0.0..=1.0`.
    pub a: f32,
}

impl Srgba {
    /// A color with an explicit alpha.
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    /// An opaque color (`a == 1.0`).
    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }
}
