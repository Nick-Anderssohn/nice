//! Chrome geometry constants. Values are logical points (SwiftUI/AppKit points,
//! which equal gpui px at scale 1).
//!
//! Provenance: the 2026-07 restyle rebased the titlebar constants on
//! `docs/design/restyle-mocks.html` (approved variant Style A, 28pt bar) and its
//! plan set (`docs/plans/restyle/`), which supersede the earlier Swift-parity
//! citations for the top-bar height and the traffic-light layout. The remaining
//! sidebar / card constants still cite their `AppShellView.swift` origins. One
//! block ([`MACOS26_TRAFFIC_LIGHT_LEADINGS`] / [`MACOS26_TRAFFIC_LIGHT_PITCH`])
//! is documentary and cites a project-memory note, because the OS owns those
//! values — see that block's doc comment.

// ---- Top bar ----------------------------------------------------------------

/// Height (pt) of the slim unified titlebar — the true macOS-standard titlebar
/// height, at which the native traffic lights center without repositioning. The
/// 2026-07 restyle dropped it from the old 52pt band to 28pt
/// (`docs/design/restyle-mocks.html`, Style A / 28pt bar; plan
/// `docs/plans/restyle/01-titlebar-restyle.md`).
pub const TOP_BAR_HEIGHT: f32 = 28.0;

// ---- Sidebar ----------------------------------------------------------------

/// Docked-sidebar default width (pt). Per-window and in-memory: resets to this
/// on every launch by design. `AppShellView.swift:129`.
pub const SIDEBAR_DEFAULT_WIDTH: f32 = 240.0;

/// Lower clamp on the user-resizable docked sidebar (pt). `AppShellView.swift:882`.
pub const SIDEBAR_MIN_WIDTH: f32 = 160.0;

/// Upper clamp on the user-resizable docked sidebar (pt). `AppShellView.swift:882`.
pub const SIDEBAR_MAX_WIDTH: f32 = 480.0;

/// Fixed width (pt) of the peek/overlay sidebar, which is never resizable.
/// `AppShellView.swift:824,898`.
pub const SIDEBAR_PEEK_WIDTH: f32 = 240.0;

/// Trailing invisible resize hit-zone width (pt). `AppShellView.swift:855`.
pub const SIDEBAR_RESIZE_HANDLE_WIDTH: f32 = 6.0;

// ---- Traffic lights ---------------------------------------------------------
//
// The 2026-07 restyle stopped repositioning the native window buttons: at the
// true 28pt titlebar the OS centers them itself, so `crate::app::window_options`
// now passes `traffic_light_position: None` and the placer constants
// (nudge / absolute-center / cluster-width) are retired. Only the DOCUMENTARY
// native-defaults block below survives, plus the leading reserve the titlebar
// keeps before the sidebar-collapse toggle so it clears the button cluster.

/// Native diameter (pt) of a standard macOS window button (close / minimize /
/// zoom). Measured live on macOS 26 (14×14). Feeds only the
/// [`traffic_light_reserved_width`] reserve bound now that the buttons sit at
/// their OS-native position. Provenance: project-memory note
/// `reference_traffic_light_geometry_macos26`.
pub const TRAFFIC_LIGHT_DIAMETER: f32 = 14.0;

/// Gap (pt) the titlebar leaves between the traffic-light cluster's trailing
/// edge and the sidebar-collapse toggle that follows it (the Finder/Safari
/// spacing in `docs/design/restyle-mocks.html`, `.lights` right pad + `.tb-btn`
/// leading).
pub const TRAFFIC_LIGHT_TRAILING_GAP: f32 = 14.0;

/// Leading width (pt) the titlebar reserves before its first control so the
/// native traffic-light cluster clears: the zoom button's leading
/// (`MACOS26_TRAFFIC_LIGHT_LEADINGS[2]`) + one [`TRAFFIC_LIGHT_DIAMETER`] +
/// [`TRAFFIC_LIGHT_TRAILING_GAP`] = 83. Re-derived from the native macOS-26
/// leadings (no nudge) after the restyle retired the old placer formula. Cites
/// `docs/design/restyle-mocks.html` + plan `docs/plans/restyle/01-titlebar-restyle.md`.
pub const fn traffic_light_reserved_width() -> f32 {
    MACOS26_TRAFFIC_LIGHT_LEADINGS[2] + TRAFFIC_LIGHT_DIAMETER + TRAFFIC_LIGHT_TRAILING_GAP
}

// ---- macOS 26 native traffic-light defaults ---------------------------------
//
// The live per-button leading origins observed on macOS 26 — 9 / 32 / 55 pt at
// a 23pt inter-button pitch. Since the restyle no longer repositions the
// buttons (`traffic_light_position: None`), these are the values the OS places
// them at; the `chrome` / `sidebar` live scenarios assert the RENDERED frames
// (from `standard_window_button_frames()`) against them, so any OS drift
// surfaces there. `[2]` (the zoom leading) also feeds
// [`traffic_light_reserved_width`].
//
// Provenance: project-memory note `reference_traffic_light_geometry_macos26`
// (there is deliberately NO Swift source line — the Swift code does not
// hardcode them). See crates/README.md.

/// Observed macOS 26 native leading origins (pt) of the close / minimize / zoom
/// buttons, now that the app leaves them at their OS-native position. `[2]` (the
/// zoom leading) feeds [`traffic_light_reserved_width`]; all three are the
/// expected values the live scenarios check the queried frames against.
pub const MACOS26_TRAFFIC_LIGHT_LEADINGS: [f32; 3] = [9.0, 32.0, 55.0];

/// Observed macOS 26 native inter-button pitch (pt). The live scenarios assert
/// the rendered pitch against it; see the block comment above.
pub const MACOS26_TRAFFIC_LIGHT_PITCH: f32 = 23.0;

// ---- Peek overlay (the only surviving elevated sidebar panel) ----------------
//
// The 2026-07 restyle (plan `docs/plans/restyle/02-sidebar-flatten.md`) flattened
// the DOCKED sidebar into the shared window-body surface: its card inset, border,
// distinct fill, and rounding are gone (a single over-glass hairline replaces the
// chrome). The collapsed-sidebar PEEK overlay is the last surface that still reads
// as an elevated floating panel — it floats over live terminal content and needs
// the rounded corner + drop shadow for readability — so only the corner-radius and
// shadow constants below survive, now cited for the peek overlay. The retired
// `CARD_INSET` / `CARD_BORDER_*` constants (and their `card_constants_match_swift`
// provenance test) went with the flattened docked card.

/// Corner radius (pt) of the elevated peek overlay panel. `AppShellView.swift:825`.
pub const CARD_CORNER_RADIUS: f32 = 8.0;

/// Corner radius (pt) of inner chrome elements (pills, footer icon buttons).
/// `AppShellView.swift:1124,1158`.
pub const INNER_CORNER_RADIUS: f32 = 6.0;

/// Peek-overlay drop-shadow blur radius (pt). `AppShellView.swift:838`.
pub const CARD_SHADOW_RADIUS: f32 = 4.0;

/// Peek-overlay drop-shadow vertical offset (pt). `AppShellView.swift:838`.
pub const CARD_SHADOW_Y_OFFSET: f32 = 2.0;

/// Peek-overlay drop-shadow color = black at this opacity. `AppShellView.swift:838`.
pub const CARD_SHADOW_OPACITY: f32 = 0.15;

// The COLLAPSED_CAP_* constants are gone with the cap itself (M2 feel-check
// Item B): the collapsed shell renders one full-width title-bar band, an
// approved divergence from the Swift parity design (`AppShellView.swift:953`).

#[cfg(test)]
mod tests {
    //! Provenance-cited fixtures for the geometry constants. See
    //! crates/README.md "Fixture-provenance convention".
    use super::*;

    #[test]
    fn top_bar_is_the_restyle_slim_titlebar() {
        // docs/design/restyle-mocks.html (Style A / 28pt bar); plan
        // docs/plans/restyle/01-titlebar-restyle.md.
        assert_eq!(TOP_BAR_HEIGHT, 28.0);
    }

    #[test]
    fn sidebar_constants_match_swift() {
        assert_eq!(SIDEBAR_DEFAULT_WIDTH, 240.0); // AppShellView.swift:129
        assert_eq!(SIDEBAR_MIN_WIDTH, 160.0); // AppShellView.swift:882
        assert_eq!(SIDEBAR_MAX_WIDTH, 480.0); // AppShellView.swift:882
        assert_eq!(SIDEBAR_PEEK_WIDTH, 240.0); // AppShellView.swift:824,898
        assert_eq!(SIDEBAR_RESIZE_HANDLE_WIDTH, 6.0); // AppShellView.swift:855
    }

    #[test]
    fn reserved_width_is_derived_from_the_native_leadings() {
        // zoom leading (55) + one button diameter (14) + trailing gap (14) = 83.
        // Re-derived from MACOS26_TRAFFIC_LIGHT_LEADINGS after the restyle retired
        // the old placer formula (docs/design/restyle-mocks.html; plan
        // docs/plans/restyle/01-titlebar-restyle.md).
        assert_eq!(traffic_light_reserved_width(), 83.0);
        assert_eq!(
            traffic_light_reserved_width(),
            MACOS26_TRAFFIC_LIGHT_LEADINGS[2] + TRAFFIC_LIGHT_DIAMETER + TRAFFIC_LIGHT_TRAILING_GAP
        );
    }

    #[test]
    fn macos26_native_defaults_recorded() {
        // Project-memory note reference_traffic_light_geometry_macos26 — the
        // OS-native positions the buttons sit at now that the app passes
        // `traffic_light_position: None`. The live `chrome` / `sidebar` scenarios
        // assert the rendered frames against these.
        assert_eq!(MACOS26_TRAFFIC_LIGHT_LEADINGS, [9.0, 32.0, 55.0]);
        assert_eq!(MACOS26_TRAFFIC_LIGHT_PITCH, 23.0);
        assert_eq!(TRAFFIC_LIGHT_DIAMETER, 14.0);
    }

    #[test]
    fn peek_overlay_constants_match_swift() {
        // The surviving elevated-panel constants after the docked sidebar
        // flattened (plan docs/plans/restyle/02-sidebar-flatten.md) — the peek
        // overlay keeps the rounded corner + drop shadow for readability over
        // live terminal content.
        assert_eq!(CARD_CORNER_RADIUS, 8.0); // AppShellView.swift:825
        assert_eq!(INNER_CORNER_RADIUS, 6.0); // AppShellView.swift:1124,1158
        assert_eq!(CARD_SHADOW_RADIUS, 4.0); // AppShellView.swift:838
        assert_eq!(CARD_SHADOW_Y_OFFSET, 2.0); // AppShellView.swift:838
        assert_eq!(CARD_SHADOW_OPACITY, 0.15); // AppShellView.swift:838
    }
}
