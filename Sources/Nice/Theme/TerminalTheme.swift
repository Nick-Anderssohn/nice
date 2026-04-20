//
//  TerminalTheme.swift
//  Nice
//
//  A terminal color theme — bg, fg, cursor, selection, and the 16
//  ANSI palette entries (regular 0–7 then bright 8–15). Decoupled
//  from chrome (`Palette`): picking "Dracula" here recolors only the
//  terminal pane, not the sidebar or window background.
//
//  Colors are stored as 8-bit sRGB triples because that's how every
//  source format speaks (Ghostty's `#rrggbb`, iTerm2 .itermcolors,
//  the canonical theme specs we lift from). Conversion to NSColor
//  and SwiftTerm.Color happens at the use site.
//
//  The struct is Sendable (ThemeColor is a trivial value type with
//  Sendable fields), so themes can cross actor boundaries without
//  ceremony — a convenience the existing `NiceANSIPalette` lacks.
//

import AppKit
import Foundation
import SwiftTerm
import SwiftUI

/// sRGB color with implicit alpha = 1.0. 8-bit per channel matches
/// every input format — richer precision is never used in practice
/// and would force lossy conversions from hex sources.
struct ThemeColor: Hashable, Sendable {
    var red: UInt8
    var green: UInt8
    var blue: UInt8

    init(_ red: UInt8, _ green: UInt8, _ blue: UInt8) {
        self.red = red
        self.green = green
        self.blue = blue
    }

    /// Accepts `#rrggbb` or `rrggbb` (case-insensitive). Returns nil
    /// for anything that isn't exactly 6 hex digits after stripping a
    /// leading `#`.
    init?(hex: String) {
        var s = hex.trimmingCharacters(in: .whitespaces)
        if s.hasPrefix("#") { s.removeFirst() }
        guard s.count == 6 else { return nil }
        guard let value = UInt32(s, radix: 16) else { return nil }
        self.red = UInt8((value >> 16) & 0xff)
        self.green = UInt8((value >> 8) & 0xff)
        self.blue = UInt8(value & 0xff)
    }

    var nsColor: NSColor {
        NSColor(
            srgbRed: CGFloat(red) / 255,
            green: CGFloat(green) / 255,
            blue: CGFloat(blue) / 255,
            alpha: 1
        )
    }

    /// SwiftTerm's `Color` initializer takes 16-bit per channel.
    /// Widen via `v * 257` (== `(v << 8) | v`), matching the helper
    /// `NiceANSIPalette.c` uses.
    var swiftTermColor: SwiftTerm.Color {
        SwiftTerm.Color(
            red: UInt16(red) * 257,
            green: UInt16(green) * 257,
            blue: UInt16(blue) * 257
        )
    }
}

/// A complete terminal theme. `id` is the stable slug persisted in
/// `Tweaks.terminalThemeLightId` / `...DarkId`; `displayName` is for
/// the Settings picker; `scope` drives which picker(s) this theme
/// appears in.
struct TerminalTheme: Identifiable, Hashable, Sendable {
    let id: String
    let displayName: String
    let scope: Scope
    let background: ThemeColor
    let foreground: ThemeColor
    /// Nil => caret follows `Tweaks.accent`. Non-nil => overrides
    /// accent (themes like Dracula carry their canonical cursor).
    let cursor: ThemeColor?
    /// Nil => SwiftTerm's default selection color.
    let selection: ThemeColor?
    /// Exactly 16 entries: indices 0–7 normal, 8–15 bright.
    let ansi: [ThemeColor]
    let source: Source

    enum Scope: Sendable {
        /// Designed for light mode. Appears only in the light-mode picker.
        case light
        /// Designed for dark mode. Appears only in the dark-mode picker.
        case dark
        /// Works in either scheme. Appears in both pickers. Imported
        /// themes default to this because the file format doesn't
        /// record authorial intent.
        case either
    }

    enum Source: Hashable, Sendable {
        case builtIn
        case imported(url: URL)
    }

    /// Whether this theme should appear in the picker for the given
    /// scheme. `.either` matches both sides.
    func matches(scheme: SwiftUI.ColorScheme) -> Bool {
        switch (scope, scheme) {
        case (.either, _): return true
        case (.light, .light): return true
        case (.dark, .dark): return true
        default: return false
        }
    }
}

