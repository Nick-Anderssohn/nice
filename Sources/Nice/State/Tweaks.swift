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
    case nice, macOS, catppuccinLatte, catppuccinMocha

    public var id: String { rawValue }

    public var displayName: String {
        switch self {
        case .nice:            "Nice"
        case .macOS:           "macOS"
        case .catppuccinLatte: "Catppuccin Latte"
        case .catppuccinMocha: "Catppuccin Mocha"
        }
    }

    /// Whether this palette belongs in the chrome picker for `scheme`.
    /// `.nice` and `.macOS` adapt to either scheme; the Catppuccin
    /// variants are single-scheme by design (Latte is light-only, Mocha
    /// dark-only) so they only appear in the picker that matches.
    public func matches(scheme: SwiftUI.ColorScheme) -> Bool {
        switch self {
        case .nice, .macOS:    true
        case .catppuccinLatte: scheme == .light
        case .catppuccinMocha: scheme == .dark
        }
    }
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

// MARK: - Editor command

/// A user-configured (or auto-detected) terminal editor that can open
/// files from the File Explorer in a new pane. The `command` is parsed
/// by zsh, so callers can include arguments (e.g. `nvim -p`,
/// `emacs -nw`). Identity is by UUID so renames don't break extension
/// mappings.
struct EditorCommand: Identifiable, Hashable, Codable, Sendable {
    let id: UUID
    var name: String
    var command: String
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
    /// Legacy single-choice theme key. Preserved as a read-only source
    /// during migration, then removed from UserDefaults so the new
    /// `scheme` / `chromeLightPaletteKey` / `chromeDarkPaletteKey`
    /// entries become the only persistent story.
    static let themeKey         = "theme"
    static let syncKey          = "syncWithOS"
    static let accentKey        = "accent"
    static let gpuRenderingKey  = "gpuRendering"
    static let smoothScrollingKey = "smoothScrolling"
    static let schemeKey               = "scheme"
    static let chromeLightPaletteKey   = "chromeLightPalette"
    static let chromeDarkPaletteKey    = "chromeDarkPalette"
    static let terminalThemeLightKey = "terminalThemeLightId"
    static let terminalThemeDarkKey  = "terminalThemeDarkId"
    static let terminalFontFamilyKey = "terminalFontFamily"
    static let editorCommandsKey       = "editorCommands"
    static let extensionEditorMapKey   = "extensionEditorMap"

    /// Default terminal-theme ids. These are the ones in
    /// `BuiltInTerminalThemes`; keep in sync or fresh installs will fall
    /// through to the catalog's "unknown id" fallback.
    static let defaultTerminalThemeLightId = "catppuccin-latte"
    static let defaultTerminalThemeDarkId  = "catppuccin-mocha"

    /// The active color scheme. Pinned to `.aqua` / `.darkAqua` via
    /// `NSApp.appearance` on every set so AppKit chrome and SwiftUI's
    /// `@Environment(\.colorScheme)` stay in lockstep with this value.
    @Published var scheme: ColorScheme {
        didSet {
            UserDefaults.standard.set(Self.encodeScheme(scheme), forKey: Self.schemeKey)
            NSApp?.appearance = Self.nsAppearance(for: scheme)
        }
    }

    /// Chrome palette used when `scheme == .light`. Read via
    /// `activeChromePalette` everywhere in the app; the view layer
    /// never branches on `scheme` itself.
    @Published var chromeLightPalette: Palette {
        didSet {
            UserDefaults.standard.set(chromeLightPalette.rawValue, forKey: Self.chromeLightPaletteKey)
        }
    }

    /// Chrome palette used when `scheme == .dark`.
    @Published var chromeDarkPalette: Palette {
        didSet {
            UserDefaults.standard.set(chromeDarkPalette.rawValue, forKey: Self.chromeDarkPaletteKey)
        }
    }

    @Published var syncWithOS: Bool {
        didSet {
            UserDefaults.standard.set(syncWithOS, forKey: Self.syncKey)
            if syncWithOS { reconcileWithOS() }
        }
    }

    /// Legacy single-value accessor for call sites that still read
    /// `tweaks.theme.palette` / `.scheme`. Setters decompose into
    /// `(scheme, both chrome slots)` so old tests and call patterns
    /// keep working; removing them is a follow-up cleanup.
    var theme: ThemeChoice {
        get {
            switch (activeChromePalette, scheme) {
            case (.nice, .light):            return .niceLight
            case (.nice, .dark):             return .niceDark
            case (.macOS, .light):           return .macLight
            case (.macOS, .dark):            return .macDark
            case (.catppuccinLatte, .dark):  return .niceDark
            case (.catppuccinLatte, _):      return .niceLight
            case (.catppuccinMocha, .light): return .niceLight
            case (.catppuccinMocha, _):      return .niceDark
            case (.nice, _):                 return .niceLight
            case (.macOS, _):                return .macLight
            }
        }
        set {
            scheme = newValue.scheme
            chromeLightPalette = newValue.palette
            chromeDarkPalette = newValue.palette
        }
    }

    /// The chrome palette to use right now, picked from the matching
    /// scheme slot. Chrome views should always read through this
    /// helper rather than branching on scheme themselves.
    var activeChromePalette: Palette {
        scheme == .light ? chromeLightPalette : chromeDarkPalette
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

    /// User-configured terminal editors that can open files from the
    /// File Explorer in a new pane. Each entry has a stable UUID so
    /// extension mappings survive renames.
    @Published var editorCommands: [EditorCommand] {
        didSet { persistEditorCommands() }
    }

    /// Maps a normalised file extension (lowercase, no leading dot) to
    /// an `EditorCommand.id`. Mappings are pruned automatically when
    /// the referenced editor is removed via `removeEditor(id:)`.
    @Published var extensionEditorMap: [String: UUID] {
        didSet { persistExtensionEditorMap() }
    }

    /// Injectable OS scheme source — real builds read
    /// `AppleInterfaceStyle`, tests substitute a stub.
    var osSchemeProvider: () -> ColorScheme

    /// The `UserDefaults` domain to persist editor settings into.
    /// Pre-existing properties (theme, accent, terminal-theme ids, …)
    /// still write to `.standard` directly via their `didSet`s — this
    /// new ivar is the start of routing those through an injectable
    /// domain instead. New code that calls `persistEditorCommands` and
    /// `persistExtensionEditorMap` writes here so tests can isolate
    /// without scrubbing `.standard`.
    private let defaults: UserDefaults

    /// Retains the distributed-notification observer so it outlives init.
    private var osObserver: NSObjectProtocol?

    init(
        defaults: UserDefaults = .standard,
        osSchemeProvider: @escaping () -> ColorScheme = Tweaks.readOSScheme,
        installOSObserver: Bool = true
    ) {
        self.osSchemeProvider = osSchemeProvider
        self.defaults = defaults

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

        let editors = Self.loadEditorCommands(defaults: defaults)
        let extMap  = Self.loadExtensionEditorMap(defaults: defaults)

        let migrated = Self.loadOrMigrate(defaults: defaults, osScheme: osSchemeProvider())

        self.scheme = migrated.scheme
        self.chromeLightPalette = migrated.chromeLightPalette
        self.chromeDarkPalette = migrated.chromeDarkPalette
        self.syncWithOS = migrated.syncWithOS
        self.accent = accent
        self.gpuRendering = gpu
        self.smoothScrolling = smooth
        self.terminalThemeLightId = terminalLight
        self.terminalThemeDarkId = terminalDark
        self.terminalFontFamily = fontFamily
        self.editorCommands = editors
        self.extensionEditorMap = extMap

        NSApp?.appearance = Self.nsAppearance(for: migrated.scheme)

        if installOSObserver {
            installAppearanceObserver()
        }

        // Catch-up: if syncWithOS was persisted as true but the OS flipped
        // while the app was closed, this aligns us on launch.
        if migrated.syncWithOS {
            reconcileWithOS()
        }
    }

    // No deinit: `Tweaks` is installed as a `@StateObject` at the App
    // root and lives for the whole process lifetime, so the observer
    // token is released implicitly at exit. Touching `osObserver` from a
    // nonisolated deinit would require hopping back onto the main actor,
    // which isn't allowed in Swift 6 deinits.

    // MARK: - Theme transitions

    /// Legacy helper kept for tests that poke a single ThemeChoice
    /// through the old API. New UI sets `scheme` / `chromeLightPalette`
    /// / `chromeDarkPalette` directly via SwiftUI bindings; this
    /// function just forwards to the `theme` computed setter.
    func userPicked(_ choice: ThemeChoice) {
        if syncWithOS, choice.scheme != osSchemeProvider() {
            syncWithOS = false
        }
        theme = choice
    }

    /// Align `scheme` with the OS scheme when `syncWithOS` is on.
    /// No-op when sync is off.
    func reconcileWithOS() {
        guard syncWithOS else { return }
        let osScheme = osSchemeProvider()
        if scheme != osScheme {
            scheme = osScheme
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

    /// Snapshot of every value that `init()` needs to seed, populated
    /// by reading the new per-scheme keys if they exist and falling
    /// back to legacy `theme` / `"system" | "light" | "dark"` values
    /// (or a fresh-install default) otherwise. After reading, the
    /// legacy `themeKey` is deleted from `defaults` so subsequent
    /// launches take the fast path.
    struct MigratedTheme {
        var scheme: ColorScheme
        var chromeLightPalette: Palette
        var chromeDarkPalette: Palette
        var syncWithOS: Bool
    }

    static func loadOrMigrate(
        defaults: UserDefaults,
        osScheme: ColorScheme
    ) -> MigratedTheme {
        // Happy path: new per-scheme keys already present. Read them
        // and we're done — this is what every launch after the first
        // post-upgrade one hits.
        if
            let lightRaw = defaults.string(forKey: Self.chromeLightPaletteKey),
            let darkRaw = defaults.string(forKey: Self.chromeDarkPaletteKey),
            let light = Palette(rawValue: lightRaw),
            let dark = Palette(rawValue: darkRaw)
        {
            let schemeRaw = defaults.string(forKey: Self.schemeKey)
            let scheme: ColorScheme
            if let schemeRaw, let decoded = Self.decodeScheme(schemeRaw) {
                scheme = decoded
            } else {
                scheme = osScheme
            }
            let sync: Bool = defaults.object(forKey: Self.syncKey) as? Bool ?? true
            return MigratedTheme(
                scheme: scheme,
                chromeLightPalette: light,
                chromeDarkPalette: dark,
                syncWithOS: sync
            )
        }

        // Migrate legacy theme key — either the ThemeChoice rawValue
        // form (niceLight / niceDark / macLight / macDark) from the
        // post-v1 codebase, or the older "system" / "light" / "dark"
        // strings from pre-v1. After migration the legacy key is
        // cleared so it can't leak into future reads.
        defer { defaults.removeObject(forKey: Self.themeKey) }

        let hasSyncKey = defaults.object(forKey: Self.syncKey) != nil
        let legacySync: Bool = hasSyncKey ? defaults.bool(forKey: Self.syncKey) : true
        let legacyRaw = defaults.string(forKey: Self.themeKey)

        if let legacyRaw, let parsed = ThemeChoice(rawValue: legacyRaw) {
            // The user previously had a single-choice theme. Honour
            // their palette pick by seeding both chrome slots with
            // it; their scheme pick becomes the current scheme (sync
            // is preserved from its own key, defaulting to false so
            // we don't flip the pinned scheme next to the OS).
            return MigratedTheme(
                scheme: parsed.scheme,
                chromeLightPalette: parsed.palette,
                chromeDarkPalette: parsed.palette,
                syncWithOS: hasSyncKey ? legacySync : false
            )
        }

        switch legacyRaw {
        case "system":
            return MigratedTheme(
                scheme: osScheme,
                chromeLightPalette: .macOS,
                chromeDarkPalette: .macOS,
                syncWithOS: true
            )
        case "light":
            return MigratedTheme(
                scheme: .light,
                chromeLightPalette: .nice,
                chromeDarkPalette: .nice,
                syncWithOS: false
            )
        case "dark":
            return MigratedTheme(
                scheme: .dark,
                chromeLightPalette: .nice,
                chromeDarkPalette: .nice,
                syncWithOS: false
            )
        default:
            // Fresh install: Catppuccin Latte for light, Mocha for dark,
            // synced with the OS. Pairs with the matching terminal-theme
            // defaults above so the whole app opens in Catppuccin for a
            // new user — our preferred out-of-the-box look.
            return MigratedTheme(
                scheme: osScheme,
                chromeLightPalette: .catppuccinLatte,
                chromeDarkPalette: .catppuccinMocha,
                syncWithOS: true
            )
        }
    }

    // MARK: - Scheme encoding / NSAppearance helpers

    static func encodeScheme(_ scheme: ColorScheme) -> String {
        switch scheme {
        case .light: return "light"
        case .dark:  return "dark"
        @unknown default: return "light"
        }
    }

    static func decodeScheme(_ raw: String) -> ColorScheme? {
        switch raw {
        case "light": return .light
        case "dark":  return .dark
        default: return nil
        }
    }

    static func nsAppearance(for scheme: ColorScheme) -> NSAppearance {
        switch scheme {
        case .light: return NSAppearance(named: .aqua) ?? NSAppearance()
        case .dark:  return NSAppearance(named: .darkAqua) ?? NSAppearance()
        @unknown default: return NSAppearance()
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

    // MARK: - Editor commands

    /// Lower-cased, dot-stripped form so `MD`, `.md`, and `md` all
    /// resolve to the same mapping key.
    static func normalizeExtension(_ ext: String) -> String {
        var s = ext
        if s.hasPrefix(".") { s.removeFirst() }
        return s.lowercased()
    }

    func editor(for id: UUID) -> EditorCommand? {
        editorCommands.first { $0.id == id }
    }

    func editor(forExtension ext: String) -> EditorCommand? {
        let key = Self.normalizeExtension(ext)
        guard let id = extensionEditorMap[key] else { return nil }
        return editor(for: id)
    }

    func addEditor(_ editor: EditorCommand) {
        editorCommands.append(editor)
    }

    func updateEditor(id: UUID, name: String, command: String) {
        guard let idx = editorCommands.firstIndex(where: { $0.id == id }) else { return }
        editorCommands[idx].name = name
        editorCommands[idx].command = command
    }

    /// Removes the editor and any extension mappings that pointed at it.
    /// Single enforcement point for the "no orphaned mappings" invariant —
    /// UI must go through this rather than mutating `editorCommands` directly.
    func removeEditor(id: UUID) {
        editorCommands.removeAll { $0.id == id }
        for (ext, mapped) in extensionEditorMap where mapped == id {
            extensionEditorMap.removeValue(forKey: ext)
        }
    }

    func setMapping(extension ext: String, editorId: UUID) {
        extensionEditorMap[Self.normalizeExtension(ext)] = editorId
    }

    func removeMapping(forExtension ext: String) {
        extensionEditorMap.removeValue(forKey: Self.normalizeExtension(ext))
    }

    private func persistEditorCommands() {
        if let data = try? JSONEncoder().encode(editorCommands) {
            defaults.set(data, forKey: Self.editorCommandsKey)
        }
    }

    private func persistExtensionEditorMap() {
        if let data = try? JSONEncoder().encode(extensionEditorMap) {
            defaults.set(data, forKey: Self.extensionEditorMapKey)
        }
    }

    static func loadEditorCommands(defaults: UserDefaults) -> [EditorCommand] {
        guard let data = defaults.data(forKey: editorCommandsKey),
              let decoded = try? JSONDecoder().decode([EditorCommand].self, from: data)
        else { return [] }
        return decoded
    }

    static func loadExtensionEditorMap(defaults: UserDefaults) -> [String: UUID] {
        guard let data = defaults.data(forKey: extensionEditorMapKey),
              let decoded = try? JSONDecoder().decode([String: UUID].self, from: data)
        else { return [:] }
        return decoded
    }
}
