//
//  Tweaks.swift
//  Nice
//
//  Phase 5: user-controlled theme + accent model, plus a tiny hex-to-Color
//  helper. The `Tweaks` observable object is the single source of truth
//  for both values and persists them to `UserDefaults` on every set so a
//  relaunch restores them without any extra plumbing.
//
//  Source of truth for the palette: the `ACCENTS` array in
//  /tmp/nice-design/nice/project/nice/tweaks.jsx (five swatches).
//

import SwiftUI

// MARK: - Theme choice

enum ThemeChoice: String, CaseIterable, Identifiable {
    case system, light, dark

    var id: String { rawValue }

    var label: String {
        self == .system ? "Match system" : rawValue.capitalized
    }

    /// `nil` for `.system` so `.preferredColorScheme(nil)` falls through to
    /// the OS appearance.
    var scheme: ColorScheme? {
        switch self {
        case .system: nil
        case .light:  .light
        case .dark:   .dark
        }
    }
}

// MARK: - Accent preset

enum AccentPreset: String, CaseIterable, Identifiable {
    case terracotta, ocean, fern, iris, graphite

    var id: String { rawValue }
    var label: String { rawValue.capitalized }

    /// Hex values from `ACCENTS` in tweaks.jsx.
    var hex: String {
        switch self {
        case .terracotta: "#c96442"
        case .ocean:      "#3b82f6"
        case .fern:       "#10b981"
        case .iris:       "#7c3aed"
        case .graphite:   "#1f2937"
        }
    }

    var color: Color { Color(hex: hex) }
}

// MARK: - Color hex init

extension Color {
    /// sRGB color from a `#rrggbb` (or `rrggbb`) string. Invalid strings
    /// decode to black; the call site is expected to supply a literal so
    /// we don't bother surfacing errors.
    init(hex: String) {
        let scrubbed = hex.trimmingCharacters(in: CharacterSet(charactersIn: "#"))
        var n: UInt64 = 0
        Scanner(string: scrubbed).scanHexInt64(&n)
        self.init(
            .sRGB,
            red:   Double((n >> 16) & 0xff) / 255,
            green: Double((n >> 8)  & 0xff) / 255,
            blue:  Double(n         & 0xff) / 255,
            opacity: 1.0
        )
    }
}

// MARK: - Tweaks store

/// Observable store owning the two user-controlled theme values. Both
/// `theme` and `accent` write through to `UserDefaults` on every set so a
/// relaunch restores them, and both are `@Published` so every view that
/// reads them via `@EnvironmentObject` repaints on change.
@MainActor
final class Tweaks: ObservableObject {
    static let themeKey  = "theme"
    static let accentKey = "accent"

    @Published var theme: ThemeChoice {
        didSet { UserDefaults.standard.set(theme.rawValue, forKey: Self.themeKey) }
    }

    @Published var accent: AccentPreset {
        didSet { UserDefaults.standard.set(accent.rawValue, forKey: Self.accentKey) }
    }

    init() {
        let defaults = UserDefaults.standard
        let rawTheme = defaults.string(forKey: Self.themeKey) ?? ThemeChoice.system.rawValue
        let rawAccent = defaults.string(forKey: Self.accentKey) ?? AccentPreset.terracotta.rawValue
        self.theme = ThemeChoice(rawValue: rawTheme) ?? .system
        self.accent = AccentPreset(rawValue: rawAccent) ?? .terracotta
    }
}
