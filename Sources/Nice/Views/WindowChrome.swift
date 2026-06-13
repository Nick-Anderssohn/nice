//
//  WindowChrome.swift
//  Nice
//
//  Single source of truth for the geometry of Nice's custom
//  `.hiddenTitleBar` top bar. The app draws its own chrome band instead
//  of using a native title bar, so several views independently need to
//  know how tall that band is and where the native traffic lights end up
//  once `TrafficLightPlacer` insets them into the sidebar card.
//
//  Before this type those magic numbers were copy-pasted across
//  `AppShellView`, `WindowToolbarView`, and `WindowDragRegion` and drifted
//  silently. Everything chrome-geometry-related lives here now so a change
//  in one place can't desync the others.
//

import CoreGraphics

enum WindowChrome {
    /// Height in points of the custom top-bar band. Matches the classic
    /// hidden-title-bar chrome height and is the row that
    /// `WindowDragRegion` makes behave like a native title bar (drag to
    /// move, double-click to zoom). Used for the toolbar height, the
    /// window-background band, the sidebar card's traffic-light spacer,
    /// and the zoom monitor's hit gate.
    static let topBarHeight: CGFloat = 52

    // MARK: - Traffic-light geometry

    /// Horizontal offset `TrafficLightPlacer` applies to the native window
    /// buttons, shifting them inward so they sit inside the sidebar card
    /// with breathing room (Xcode-style). The placer adds this UNIFORMLY to
    /// each button's OWN native default leading x, which preserves the
    /// OS-native inter-button pitch (23pt on macOS 26, 20pt on macOS ≤ 15)
    /// and reproduces today's shipping pixels on every OS version. 8pt
    /// clears the sidebar card's 8pt rounded corner (the card's leading
    /// edge is window-x 6).
    static let trafficLightNudgeX: CGFloat = 8

    /// Vertical offset that the OLD nudger applied to the native button y.
    /// Negative moved the buttons down in window-content coordinates
    /// (AppKit's y grows upward). Now DOCUMENTARY only: `TrafficLightPlacer`
    /// computes y ABSOLUTELY as `trafficLightCenterFromTop` (26pt from the
    /// window top), which is OS-version-independent. On macOS 26 the two
    /// agree exactly — default y (577) + nudgeY (-10) = 567 = the absolute
    /// target — so this constant records the equivalence rather than
    /// driving the math. Kept so the relationship is discoverable.
    static let trafficLightNudgeY: CGFloat = -10

    /// Window-y of the traffic-light visual centers, measured from the
    /// window top. Equals `topBarHeight / 2` — THE shared top-bar row that
    /// the toolbar pills / `+`, the sidebar mode / collapse icons, and the
    /// collapsed-cap restore button all center on, so the lights line up
    /// with Nice's custom bar chrome (not macOS's default ~16pt). The
    /// placer targets this absolutely; OS-version-independent.
    static let trafficLightCenterFromTop: CGFloat = 26

    /// The macOS ≤ 15 default leading origin (x) of the close button in
    /// window-content coordinates before any nudge. NOTE: this is the
    /// stale-macOS value (on macOS 26 the live default is 9) and is NO
    /// LONGER read for button placement — `TrafficLightPlacer` captures
    /// each button's OWN live default instead, so it stays OS-robust. This
    /// constant now feeds ONLY `trafficLightReservedWidth` below, where it
    /// acts as a conservative upper bound on the cluster's leading inset
    /// for the collapsed-cap layout reserve.
    static let trafficLightDefaultLeading: CGFloat = 20

    /// Width in points spanned by the three standard window buttons,
    /// from the close button's leading edge to the zoom button's
    /// trailing edge (14pt diameter each, 20pt center-to-center → the
    /// span is 60 − 20 + 14 = 54).
    static let trafficLightClusterWidth: CGFloat = 54

    /// Leading width the collapsed cap must reserve before its restore
    /// button so the nudged traffic lights clear it. Derived from the
    /// nudge so it tracks any change to the inset instead of being a
    /// separately-tuned magic number (default leading + nudge + cluster
    /// span = 20 + 8 + 54 = 82). This is an intentional stale-macOS layout
    /// HEURISTIC, deliberately decoupled from the OS-robust placer: it uses
    /// the macOS ≤ 15 constants as a conservative upper bound, so on
    /// macOS 26 (real cluster extent 17–77) it over-reserves ~5pt — a
    /// cosmetic gap on the collapsed cap, never a clip or overlap. The
    /// placer's true button positions are computed separately from each
    /// button's live default, so this approximation can't misplace a button.
    static var trafficLightReservedWidth: CGFloat {
        trafficLightDefaultLeading + trafficLightNudgeX + trafficLightClusterWidth
    }
}
