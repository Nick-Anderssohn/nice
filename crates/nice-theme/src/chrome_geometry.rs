//! Chrome geometry constants, ported verbatim from `WindowChrome.swift` and
//! `AppShellView.swift` — every magic number the chrome plans (R9–R11) will
//! need, named once. Values are logical points (SwiftUI/AppKit points, which
//! equal gpui px at scale 1).
//!
//! Provenance is cited per constant. One block ([`MACOS26_TRAFFIC_LIGHT_LEADINGS`]
//! / [`MACOS26_TRAFFIC_LIGHT_PITCH`]) is documentary and cites a project-memory
//! note rather than a Swift line, because the Swift code deliberately does not
//! hardcode those OS-owned values — see that block's doc comment.

// ---- Top bar ----------------------------------------------------------------

/// Height (pt) of the custom hidden-title-bar top band — the row the traffic
/// lights, toolbar pills, and sidebar icons all center on. `WindowChrome.swift:26`.
pub const TOP_BAR_HEIGHT: f32 = 52.0;

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

/// Uniform inward x-nudge applied to each native window button's OWN live
/// default leading x (preserves the OS-native inter-button pitch). 8pt clears
/// the sidebar card's 8pt rounded corner. `WindowChrome.swift:38`.
pub const TRAFFIC_LIGHT_NUDGE_X: f32 = 8.0;

/// Legacy documentary y-nudge (pt), superseded by the absolute
/// [`TRAFFIC_LIGHT_CENTER_FROM_TOP`]. On macOS 26 they agree exactly. Kept so
/// the relationship stays discoverable. `WindowChrome.swift:48`.
pub const TRAFFIC_LIGHT_NUDGE_Y: f32 = -10.0;

/// Window-y (pt) of the traffic-light visual centers, measured from the window
/// top; equals `TOP_BAR_HEIGHT / 2`. The placer targets this absolutely
/// (OS-version-independent). `WindowChrome.swift:56`.
pub const TRAFFIC_LIGHT_CENTER_FROM_TOP: f32 = 26.0;

/// macOS ≤ 15 default close-button leading x (pt). NO LONGER read for button
/// placement (the placer captures each button's live default); feeds only the
/// [`traffic_light_reserved_width`] reserve bound. `WindowChrome.swift:66`.
pub const TRAFFIC_LIGHT_DEFAULT_LEADING: f32 = 20.0;

/// Span (pt) of the three standard window buttons, close-leading →
/// zoom-trailing. `WindowChrome.swift:72`.
pub const TRAFFIC_LIGHT_CLUSTER_WIDTH: f32 = 54.0;

/// Leading width (pt) the collapsed cap reserves for the nudged traffic lights:
/// `default_leading + nudge_x + cluster_width` = 82. A conservative
/// stale-macOS upper bound, decoupled from the OS-robust placer.
/// `WindowChrome.swift:85-87`.
pub const fn traffic_light_reserved_width() -> f32 {
    TRAFFIC_LIGHT_DEFAULT_LEADING + TRAFFIC_LIGHT_NUDGE_X + TRAFFIC_LIGHT_CLUSTER_WIDTH
}

// ---- macOS 26 native traffic-light defaults (documentary) -------------------
//
// The live per-button leading origins observed on macOS 26 — 9 / 32 / 55 pt at
// a 23pt inter-button pitch — recorded for R9's sanity checks. These are
// OS-owned RUNTIME values, NOT a design token: `TrafficLightPlacer` reads each
// button's OWN live default at placement time (WindowChrome.swift:60-66) rather
// than hardcoding them, and R9 must do the same — treat these only as the
// expected values to sanity-check the live query against.
//
// Provenance: project-memory note `reference_traffic_light_geometry_macos26`
// (there is deliberately NO Swift source line — the Swift code does not
// hardcode them). This is the one documented exception to the
// fixture-provenance "cite a Swift line" convention (see crates/README.md).

/// Observed macOS 26 native leading origins (pt) of the close / minimize / zoom
/// buttons. Documentary only — see the block comment above.
pub const MACOS26_TRAFFIC_LIGHT_LEADINGS: [f32; 3] = [9.0, 32.0, 55.0];

/// Observed macOS 26 native inter-button pitch (pt). Documentary only — see the
/// block comment above.
pub const MACOS26_TRAFFIC_LIGHT_PITCH: f32 = 23.0;

// ---- Cards (sidebar / collapsed cap) ----------------------------------------

/// Corner radius (pt) of the floating sidebar & collapsed-cap cards.
/// `AppShellView.swift:825,958`.
pub const CARD_CORNER_RADIUS: f32 = 8.0;

/// Corner radius (pt) of inner chrome elements (pills, etc.).
/// `AppShellView.swift:1124,1158`.
pub const INNER_CORNER_RADIUS: f32 = 6.0;

/// Inset (pt) of the card from the window edges; the card's leading edge sits
/// at window-x 6 (`WindowChrome.swift:37`). `AppShellView.swift:839`.
pub const CARD_INSET: f32 = 6.0;

/// Card border stroke width (pt). `AppShellView.swift:830`.
pub const CARD_BORDER_WIDTH: f32 = 0.5;

/// Card border color = the `line` slot at this opacity. `AppShellView.swift:829`.
pub const CARD_BORDER_OPACITY: f32 = 0.5;

/// Card drop-shadow blur radius (pt). `AppShellView.swift:838`.
pub const CARD_SHADOW_RADIUS: f32 = 4.0;

/// Card drop-shadow vertical offset (pt). `AppShellView.swift:838`.
pub const CARD_SHADOW_Y_OFFSET: f32 = 2.0;

/// Card drop-shadow color = black at this opacity. `AppShellView.swift:838`.
pub const CARD_SHADOW_OPACITY: f32 = 0.15;

/// Collapsed-cap height (pt). `AppShellView.swift:953`.
pub const COLLAPSED_CAP_HEIGHT: f32 = 40.0;

/// Collapsed-cap width (pt) past the traffic-light reserve — room for the
/// restore button and a trailing drag strip. `AppShellView.swift:953`.
pub const COLLAPSED_CAP_TRAILING_WIDTH: f32 = 42.0;

#[cfg(test)]
mod tests {
    //! Provenance-cited fixtures for the geometry constants. See
    //! crates/README.md "Fixture-provenance convention".
    use super::*;

    #[test]
    fn top_bar_and_sidebar_match_swift() {
        assert_eq!(TOP_BAR_HEIGHT, 52.0); // WindowChrome.swift:26
        assert_eq!(SIDEBAR_DEFAULT_WIDTH, 240.0); // AppShellView.swift:129
        assert_eq!(SIDEBAR_MIN_WIDTH, 160.0); // AppShellView.swift:882
        assert_eq!(SIDEBAR_MAX_WIDTH, 480.0); // AppShellView.swift:882
        assert_eq!(SIDEBAR_PEEK_WIDTH, 240.0); // AppShellView.swift:824,898
        assert_eq!(SIDEBAR_RESIZE_HANDLE_WIDTH, 6.0); // AppShellView.swift:855
    }

    #[test]
    fn traffic_light_constants_match_swift() {
        assert_eq!(TRAFFIC_LIGHT_NUDGE_X, 8.0); // WindowChrome.swift:38
        assert_eq!(TRAFFIC_LIGHT_NUDGE_Y, -10.0); // WindowChrome.swift:48
        assert_eq!(TRAFFIC_LIGHT_CENTER_FROM_TOP, 26.0); // WindowChrome.swift:56
        assert_eq!(TRAFFIC_LIGHT_DEFAULT_LEADING, 20.0); // WindowChrome.swift:66
        assert_eq!(TRAFFIC_LIGHT_CLUSTER_WIDTH, 54.0); // WindowChrome.swift:72
    }

    #[test]
    fn reserved_width_matches_swift_derivation() {
        // trafficLightDefaultLeading + trafficLightNudgeX + trafficLightClusterWidth
        // = 20 + 8 + 54 = 82 (WindowChrome.swift:85-87).
        assert_eq!(traffic_light_reserved_width(), 82.0);
    }

    #[test]
    fn center_is_half_the_top_bar() {
        // WindowChrome.swift:52-56 — the center equals topBarHeight / 2.
        assert_eq!(TRAFFIC_LIGHT_CENTER_FROM_TOP, TOP_BAR_HEIGHT / 2.0);
    }

    #[test]
    fn macos26_native_defaults_recorded() {
        // Documentary (project-memory note reference_traffic_light_geometry_macos26).
        assert_eq!(MACOS26_TRAFFIC_LIGHT_LEADINGS, [9.0, 32.0, 55.0]);
        assert_eq!(MACOS26_TRAFFIC_LIGHT_PITCH, 23.0);
    }

    #[test]
    fn card_constants_match_swift() {
        assert_eq!(CARD_CORNER_RADIUS, 8.0); // AppShellView.swift:825,958
        assert_eq!(INNER_CORNER_RADIUS, 6.0); // AppShellView.swift:1124,1158
        assert_eq!(CARD_INSET, 6.0); // AppShellView.swift:839
        assert_eq!(CARD_BORDER_WIDTH, 0.5); // AppShellView.swift:830
        assert_eq!(CARD_BORDER_OPACITY, 0.5); // AppShellView.swift:829
        assert_eq!(CARD_SHADOW_RADIUS, 4.0); // AppShellView.swift:838
        assert_eq!(CARD_SHADOW_Y_OFFSET, 2.0); // AppShellView.swift:838
        assert_eq!(CARD_SHADOW_OPACITY, 0.15); // AppShellView.swift:838
        assert_eq!(COLLAPSED_CAP_HEIGHT, 40.0); // AppShellView.swift:953
        assert_eq!(COLLAPSED_CAP_TRAILING_WIDTH, 42.0); // AppShellView.swift:953
    }
}
