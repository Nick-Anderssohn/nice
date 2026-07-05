//! # nice-theme
//!
//! Nice's design system as pure Rust data — no behavior, no UI, and **no
//! `gpui` dependency** (crates/README.md "Layering rule"). Everything here is
//! ported verbatim from the Swift sources and pinned by literal-equality tests
//! that cite their Swift provenance (crates/README.md "Fixture-provenance
//! convention").
//!
//! Modules:
//!
//! * [`color`] — [`Srgba`], the plain sRGB value type the palette tables use.
//! * [`palette`] — the chrome palettes (sRGB literals + macOS system-semantic
//!   slots), from `Sources/Nice/Theme/Palette.swift`.
//! * [`accent`] — [`AccentPreset`] (five swatches), from
//!   `Sources/Nice/State/Tweaks.swift`.
//! * [`typography`] — the three font aliases, from
//!   `Sources/Nice/Theme/Typography.swift`.
//! * [`chrome_geometry`] — top-bar / sidebar / traffic-light / card geometry,
//!   from `WindowChrome.swift` and `AppShellView.swift`.
//! * [`status`] — status-dot visual tokens (the 8pt dot, the "waiting" blue,
//!   the ring/breathe pulse constants), from `StatusDot.swift`.
//!
//! The tiny adapter from these plain types into gpui color types lives
//! downstream (crates/nice, R9), NOT here — that is what keeps this crate
//! gpui-free and unit-testable by plain arithmetic.

pub mod accent;
pub mod chrome_geometry;
pub mod color;
pub mod palette;
pub mod status;
pub mod typography;

pub use accent::AccentPreset;
pub use color::Srgba;
pub use palette::{ColorScheme, Palette, SlotColor, Slots, SystemColor};
pub use status::{BreathePulse, RingPulse};
pub use typography::TypographyAlias;
