//
//  Tweaks.swift
//  Nice
//
//  User-controlled theme + accent model, plus a tiny hex-to-Color helper.
//  The `Tweaks` observable object is the single source of truth for both
//  values and persists them to `UserDefaults` on every set so a relaunch
//  restores them without any extra plumbing.
//
//  Themes are a product of two axes: `Palette` (nice | macOS) and
//  `ColorScheme` (light | dark), expressed as a 4-case `ThemeChoice`.
//  A separate `syncWithOS` boolean keeps the scheme axis bound to the
//  OS's current light/dark setting while leaving the user free to pick a
//  palette family independently.
//
//  Source of truth for the accent palette: the `ACCENTS` array in
//  /tmp/nice-design/nice/project/nice/tweaks.jsx (five swatches).
//

import AppKit
import Combine
import SwiftUI

// MARK: - Palette

/// The visual language of the chrome. `.nice` uses the custom oklch-derived
/// literals in Palette.swift; `.macOS` delegates to system semantic colors
/// (`NSColor.windowBackgroundColor`, `.labelColor`, …) and a wallpaper-
/// tinted `NSVisualEffectView` sidebar — Xcode/Finder/Mail-parity.
///
/// Public because the `Color.niceX(scheme:palette:)` helpers in Palette.swift
/// live in a public extension and need Palette as a default-argument value.
public enum Palette: String, CaseIterable, Identifiable, Sendable {
    case nice, macOS

    public var id: String { rawValue }
}

// MARK: - Theme choice

/// The full theme, one of four products of (Palette × ColorScheme).
enum ThemeChoice: String, CaseIterable, Identifiable {
    case niceLight, niceDark, macLight, macDark

    var id: String { rawValue }

    var palette: Palette {
        switch self {
        case .niceLight, .niceDark: .nice
        case .macLight,  .macDark:  .macOS
        }
    }

    var scheme: ColorScheme {
        switch self {
        case .niceLight, .macLight: .light
        case .niceDark,  .macDark:  .dark
        }
    }

    /// `NSAppearance` equivalent — always non-nil because we always pin
    /// AppKit's chrome (NSAlert, NSOpenPanel, etc.) to an explicit flavor.
    /// OS-following is achieved by toggling `theme` in-place via the
    /// `syncWithOS` observer, not by unpinning appearance.
    var nsAppearance: NSAppearance {
        switch scheme {
        case .light: NSAppearance(named: .aqua) ?? NSAppearance()
        case .dark:  NSAppearance(named: .darkAqua) ?? NSAppearance()
        @unknown default: NSAppearance()
        }
    }

    /// Same palette, flipped scheme. Used by `syncWithOS` reconciliation.
    var counterpart: ThemeChoice {
        switch self {
        case .niceLight: .niceDark
        case .niceDark:  .niceLight
        case .macLight:  .macDark
        case .macDark:   .macLight
        }
    }

    /// Human-readable label for the settings picker.
    var label: String {
        switch self {
        case .niceLight: "Light"
        case .niceDark:  "Dark"
        case .macLight:  "macOS Light"
        case .macDark:   "macOS Dark"
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

    /// NSColor counterpart of `color`. Used where AppKit APIs (like
    /// SwiftTerm's `caretColor`) need the accent natively. Built from
    /// the same `hex` source so the two paths stay in sync.
    var nsColor: NSColor {
        let scrubbed = hex.trimmingCharacters(in: CharacterSet(charactersIn: "#"))
        var n: UInt64 = 0
        Scanner(string: scrubbed).scanHexInt64(&n)
        return NSColor(
            srgbRed: CGFloat((n >> 16) & 0xff) / 255,
            green:   CGFloat((n >> 8)  & 0xff) / 255,
            blue:    CGFloat(n         & 0xff) / 255,
            alpha:   1.0
        )
    }
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

/// Observable store owning the theme + accent values. `theme`, `syncWithOS`,
/// and `accent` write through to `UserDefaults` on every set so a relaunch
/// restores them, and all three are `@Published` so every view that reads
/// them via `@EnvironmentObject` repaints on change.
///
/// Invariant: when `syncWithOS == true`, `theme.scheme` equals the current
/// OS scheme. `reconcileWithOS()` enforces this and is called at init, when
/// `syncWithOS` flips to true, and from the OS-scheme notification handler.
@MainActor
final class Tweaks: ObservableObject {
    static let themeKey         = "theme"
    static let syncKey          = "syncWithOS"
    static let accentKey        = "accent"
    static let gpuRenderingKey  = "gpuRendering"
    static let smoothScrollingKey = "smoothScrolling"
    static let terminalThemeLightKey = "terminalThemeLightId"
    static let terminalThemeDarkKey  = "terminalThemeDarkId"
    static let terminalFontFamilyKey = "terminalFontFamily"

    /// Default terminal-theme ids. These are the ones in
    /// `BuiltInTerminalThemes`; keep in sync or fresh installs will fall
    /// through to the catalog's "unknown id" fallback.
    static let defaultTerminalThemeLightId = "nice-default-light"
    static let defaultTerminalThemeDarkId  = "nice-default-dark"

    @Published var theme: ThemeChoice {
        didSet {
            UserDefaults.standard.set(theme.rawValue, forKey: Self.themeKey)
            NSApp?.appearance = theme.nsAppearance
        }
    }

    @Published var syncWithOS: Bool {
        didSet {
            UserDefaults.standard.set(syncWithOS, forKey: Self.syncKey)
            if syncWithOS { reconcileWithOS() }
        }
    }

    @Published var accent: AccentPreset {
        didSet { UserDefaults.standard.set(accent.rawValue, forKey: Self.accentKey) }
    }

    /// Toggles the SwiftTerm Metal renderer for every live terminal pane.
    /// Default `true`: the upstream Metal path landed in SwiftTerm
    /// PR #484 and falls back to CoreGraphics automatically when Metal
    /// isn't available (`MetalError.deviceUnavailable` — VMs, some CI).
    @Published var gpuRendering: Bool {
        didSet { UserDefaults.standard.set(gpuRendering, forKey: Self.gpuRenderingKey) }
    }

    /// Pixel-precise trackpad scrolling on the Metal path. Default
    /// `true`. Has no effect when `gpuRendering` is off (CG falls back
    /// to line-snap behavior). Mouse wheels without precise deltas
    /// keep using the existing line-velocity scroll either way.
    @Published var smoothScrolling: Bool {
        didSet { UserDefaults.standard.set(smoothScrolling, forKey: Self.smoothScrollingKey) }
    }

    /// Terminal theme id used when the active scheme is light.
    /// Resolved via `TerminalThemeCatalog.theme(withId:)` at apply
    /// time; an unknown id (e.g. a deleted imported theme) falls back
    /// to Nice Default (Light).
    @Published var terminalThemeLightId: String {
        didSet {
            UserDefaults.standard.set(terminalThemeLightId, forKey: Self.terminalThemeLightKey)
        }
    }

    /// Terminal theme id used when the active scheme is dark.
    @Published var terminalThemeDarkId: String {
        didSet {
            UserDefaults.standard.set(terminalThemeDarkId, forKey: Self.terminalThemeDarkKey)
        }
    }

    /// PostScript name of the terminal font, or nil to use the default
    /// chain (SF Mono → JetBrains Mono NL → system monospaced).
    @Published var terminalFontFamily: String? {
        didSet {
            if let terminalFontFamily {
                UserDefaults.standard.set(terminalFontFamily, forKey: Self.terminalFontFamilyKey)
            } else {
                UserDefaults.standard.removeObject(forKey: Self.terminalFontFamilyKey)
            }
        }
    }

    /// Injectable OS scheme source — real builds read
    /// `AppleInterfaceStyle`, tests substitute a stub.
    var osSchemeProvider: () -> ColorScheme

    /// Retains the distributed-notification observer so it outlives init.
    private var osObserver: NSObjectProtocol?

    init(
        defaults: UserDefaults = .standard,
        osSchemeProvider: @escaping () -> ColorScheme = Tweaks.readOSScheme,
        installOSObserver: Bool = true
    ) {
        self.osSchemeProvider = osSchemeProvider

        let accentRaw = defaults.string(forKey: Self.accentKey) ?? AccentPreset.ocean.rawValue
        let accent = AccentPreset(rawValue: accentRaw) ?? .ocean

        // Default GPU rendering on; explicit `false` from a previous
        // launch sticks. `bool(forKey:)` returns `false` for an absent
        // key, so check `object(forKey:)` to distinguish unset from
        // explicit-off.
        let gpu: Bool = defaults.object(forKey: Self.gpuRenderingKey) == nil
            ? true
            : defaults.bool(forKey: Self.gpuRenderingKey)
        let smooth: Bool = defaults.object(forKey: Self.smoothScrollingKey) == nil
            ? true
            : defaults.bool(forKey: Self.smoothScrollingKey)

        let terminalLight = defaults.string(forKey: Self.terminalThemeLightKey)
            ?? Self.defaultTerminalThemeLightId
        let terminalDark = defaults.string(forKey: Self.terminalThemeDarkKey)
            ?? Self.defaultTerminalThemeDarkId
        let fontFamily = defaults.string(forKey: Self.terminalFontFamilyKey)

        let (theme, sync) = Self.loadOrMigrate(defaults: defaults, osScheme: osSchemeProvider())

        self.theme = theme
        self.syncWithOS = sync
        self.accent = accent
        self.gpuRendering = gpu
        self.smoothScrolling = smooth
        self.terminalThemeLightId = terminalLight
        self.terminalThemeDarkId = terminalDark
        self.terminalFontFamily = fontFamily

        NSApp?.appearance = theme.nsAppearance

        if installOSObserver {
            installAppearanceObserver()
        }

        // Catch-up: if syncWithOS was persisted as true but the OS flipped
        // while the app was closed, this aligns us on launch.
        if sync {
            reconcileWithOS()
        }
    }

    // No deinit: `Tweaks` is installed as a `@StateObject` at the App
    // root and lives for the whole process lifetime, so the observer
    // token is released implicitly at exit. Touching `osObserver` from a
    // nonisolated deinit would require hopping back onto the main actor,
    // which isn't allowed in Swift 6 deinits.

    // MARK: - Theme transitions

    /// Called when the user taps a theme button in settings.
    ///
    /// When `syncWithOS` is on and the clicked choice matches the OS
    /// scheme, we stay synced and just update the family — the scheme
    /// axis is still driven by the OS.
    ///
    /// When `syncWithOS` is on and the clicked choice's scheme does
    /// *not* match the OS, we treat the click as an explicit override:
    /// sync is turned off and the theme is pinned to exactly what the
    /// user picked. This is less surprising than silently flipping to
    /// the counterpart (which can produce a "click did nothing" feel
    /// when the counterpart is already the current theme).
    ///
    /// Ordering matters: set `syncWithOS = false` before `theme` so
    /// that the `syncWithOS` didSet observer — which only reconciles
    /// when sync flips ON — doesn't fire, and the subsequent theme
    /// assignment writes through cleanly.
    func userPicked(_ choice: ThemeChoice) {
        if syncWithOS, choice.scheme != osSchemeProvider() {
            syncWithOS = false
            theme = choice
        } else {
            theme = choice
        }
    }

    /// Align `theme.scheme` with the OS scheme when `syncWithOS` is on.
    /// No-op when sync is off.
    func reconcileWithOS() {
        guard syncWithOS else { return }
        let osScheme = osSchemeProvider()
        if theme.scheme != osScheme {
            theme = theme.counterpart
        }
    }

    // MARK: - OS appearance wiring

    /// Reads the live OS appearance by sniffing the `AppleInterfaceStyle`
    /// default. Absent key ⇒ light. Pinning `NSApp.appearance` to an
    /// explicit flavor doesn't affect this value (it's a global default
    /// owned by the OS, not the app).
    nonisolated static func readOSScheme() -> ColorScheme {
        UserDefaults.standard.string(forKey: "AppleInterfaceStyle") == "Dark" ? .dark : .light
    }

    private func installAppearanceObserver() {
        osObserver = DistributedNotificationCenter.default().addObserver(
            forName: NSNotification.Name("AppleInterfaceThemeChangedNotification"),
            object: nil,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.reconcileWithOS()
            }
        }
    }

    // MARK: - Load & migrate

    /// Returns `(theme, syncWithOS)` from defaults, migrating legacy
    /// `"system" | "light" | "dark"` values written by earlier versions of
    /// the app. Legacy mapping:
    ///
    ///   system → macLight/macDark (per OS) + syncWithOS = true
    ///   light  → niceLight + syncWithOS = false (pinned, user's explicit choice)
    ///   dark   → niceDark  + syncWithOS = false (pinned, user's explicit choice)
    ///
    /// Fresh install: macLight or macDark (per OS) + syncWithOS = true.
    /// The macOS palette is the default because it integrates with the
    /// system's Desktop Tinting and matches Xcode/Finder/Mail out of the
    /// box; users who want the nice palette can pick it explicitly.
    static func loadOrMigrate(
        defaults: UserDefaults,
        osScheme: ColorScheme
    ) -> (ThemeChoice, Bool) {
        let raw = defaults.string(forKey: Self.themeKey)
        let hasSyncKey = defaults.object(forKey: Self.syncKey) != nil

        if let raw, let parsed = ThemeChoice(rawValue: raw) {
            let sync = hasSyncKey ? defaults.bool(forKey: Self.syncKey) : false
            return (parsed, sync)
        }

        switch raw {
        case "system":
            // Legacy "follow system" maps to macOS palette + sync on,
            // starting from whichever scheme the OS currently is.
            return (osScheme == .dark ? .macDark : .macLight, true)
        case "light":
            return (.niceLight, false)
        case "dark":
            return (.niceDark, false)
        default:
            // Fresh install — macOS palette, synced with current OS.
            return (osScheme == .dark ? .macDark : .macLight, true)
        }
    }

    // MARK: - Terminal theme resolution

    /// Resolves the terminal-theme id for the given scheme against the
    /// catalog. Falls back to the Nice Default for that scheme when
    /// the persisted id isn't found — happens naturally when a user
    /// deletes an imported theme that was selected in either slot.
    func effectiveTerminalTheme(
        for scheme: ColorScheme,
        catalog: TerminalThemeCatalog
    ) -> TerminalTheme {
        let id: String
        let fallbackId: String
        switch scheme {
        case .light:
            id = terminalThemeLightId
            fallbackId = Self.defaultTerminalThemeLightId
        case .dark:
            id = terminalThemeDarkId
            fallbackId = Self.defaultTerminalThemeDarkId
        @unknown default:
            id = terminalThemeLightId
            fallbackId = Self.defaultTerminalThemeLightId
        }
        if let theme = catalog.theme(withId: id) { return theme }
        // Known fallback id is always a built-in; force-unwrap is safe
        // unless someone renames a built-in without updating the
        // `defaultTerminalThemeXId` constants, which is exactly the
        // kind of drift the unit tests catch.
        return catalog.theme(withId: fallbackId)!
    }
}
