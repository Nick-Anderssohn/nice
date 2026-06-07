//
//  WindowChrome.swift
//  Nice
//
//  Single source of truth for the geometry of Nice's custom
//  `.hiddenTitleBar` top bar. The app draws its own chrome band instead
//  of using a native title bar, so several views independently need to
//  know how tall that band is and where the native traffic lights end up
//  once `TrafficLightNudger` insets them into the sidebar card.
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

    /// Horizontal offset `TrafficLightNudger` applies to the native
    /// window buttons, shifting them inward so they sit inside the
    /// sidebar card with breathing room (Xcode-style).
    static let trafficLightNudgeX: CGFloat = 8

    /// Vertical offset for the traffic-light nudge. Negative moves the
    /// buttons down in window-content coordinates (AppKit's y grows
    /// upward). dy:-10 places their visual centers at the same window-y
    /// as the sidebar collapse/expand icon (window y≈26 from the top).
    static let trafficLightNudgeY: CGFloat = -10

    /// macOS's default leading origin (x) of the close button in
    /// window-content coordinates before any nudge. The three buttons
    /// start here.
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
    /// span = 20 + 8 + 54 = 82).
    static var trafficLightReservedWidth: CGFloat {
        trafficLightDefaultLeading + trafficLightNudgeX + trafficLightClusterWidth
    }
}
