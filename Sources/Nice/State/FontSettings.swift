//
//  FontSettings.swift
//  Nice
//
//  User-controlled terminal and sidebar font sizes. Mirrors the `Tweaks`
//  pattern: an `@MainActor ObservableObject` whose mutations write
//  through to `UserDefaults` immediately, with an injectable `defaults`
//  parameter so unit tests can stand it up against an isolated suite.
//
//  Two values:
//    • `terminalFontSize` — exact pt size used for the SwiftTerm view's
//      monospace font. Anchored: Cmd+/- zoom steps this by ±1pt.
//    • `sidebarFontSize`  — base pt for the sidebar. Other sidebar
//      elements (headers, pills, footer icons) scale proportionally via
//      `sidebarSize(_:)` so the design's internal ratios (e.g. 11pt
//      headers vs 12pt tab titles) are preserved at any base.
//
//  `zoom(by:)` is the "Cmd+/-" operation: moves terminal by an integer
//  delta, then recomputes sidebar as `round(oldSidebar × newTerminal /
//  oldTerminal)`. The ratio the user set in the Font pane is preserved
//  within rounding.
//

import Foundation
import CoreGraphics

@MainActor
final class FontSettings: ObservableObject {
    static let terminalKey = "terminalFontSize"
    static let sidebarKey  = "sidebarFontSize"

    /// Both defaults are 12pt — matches the pre-feature hardcoded
    /// terminal font, and the sidebar's "tab title" element (the 1.0
    /// ratio anchor for proportional scaling).
    static let defaultSize: CGFloat = 12

    /// Allowed size range. 8pt is the smallest size at which JetBrainsMono
    /// is still legible; 32pt is large enough for accessibility zoom
    /// without forcing SwiftTerm to reflow into single-digit column counts.
    static let minSize: CGFloat = 8
    static let maxSize: CGFloat = 32

    @Published var terminalFontSize: CGFloat {
        didSet { defaults.set(Double(terminalFontSize), forKey: Self.terminalKey) }
    }

    @Published var sidebarFontSize: CGFloat {
        didSet { defaults.set(Double(sidebarFontSize), forKey: Self.sidebarKey) }
    }

    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        self.terminalFontSize = Self.loadClamped(defaults, key: Self.terminalKey)
        self.sidebarFontSize  = Self.loadClamped(defaults, key: Self.sidebarKey)
    }

    // MARK: - Derived sizes

    /// Scale a default sidebar pt size by the user's current sidebar
    /// base, preserving the element's ratio against the 12pt anchor.
    /// Rounded to an integer pt so glyphs land on clean pixel bounds.
    /// Clamped to ≥1pt so extreme low bases don't produce 0pt text.
    func sidebarSize(_ defaultPt: CGFloat) -> CGFloat {
        max(1, (sidebarFontSize * defaultPt / Self.defaultSize).rounded())
    }

    // MARK: - Mutation

    /// Global zoom step, as used by Cmd+= / Cmd+-. Terminal is the
    /// anchor — it moves by exactly `delta` pt, clamped. Sidebar is
    /// scaled to preserve the current terminal:sidebar ratio:
    ///
    ///   newSidebar = round(oldSidebar × newTerminal / oldTerminal)
    ///
    /// A symmetric round-trip (Cmd+= then Cmd+-) may drift by 0–1pt
    /// because of the double rounding; that's acceptable at the small
    /// integer pt sizes in range and prevents runaway ratio drift.
    func zoom(by delta: CGFloat) {
        let oldTerminal = terminalFontSize
        let newTerminal = Self.clamp(oldTerminal + delta)
        guard newTerminal != oldTerminal else { return }
        let newSidebar = Self.clamp((sidebarFontSize * newTerminal / oldTerminal).rounded())
        terminalFontSize = newTerminal
        sidebarFontSize = newSidebar
    }

    /// Snap both sizes to the 12pt defaults.
    func resetToDefaults() {
        terminalFontSize = Self.defaultSize
        sidebarFontSize  = Self.defaultSize
    }

    // MARK: - Load / clamp

    private static func loadClamped(_ defaults: UserDefaults, key: String) -> CGFloat {
        let raw = defaults.object(forKey: key) as? Double ?? Double(Self.defaultSize)
        return clamp(CGFloat(raw))
    }

    private static func clamp(_ v: CGFloat) -> CGFloat {
        min(max(v, minSize), maxSize)
    }
}
