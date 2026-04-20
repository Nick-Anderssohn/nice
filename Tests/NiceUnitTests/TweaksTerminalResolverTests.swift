//
//  TweaksTerminalResolverTests.swift
//  NiceUnitTests
//
//  Covers `Tweaks.effectiveTerminalTheme(for:catalog:)` and
//  `Tweaks.activeChromePalette` — the two helpers the Settings UI and
//  `AppShellView` observers rely on to translate persisted ids and
//  per-scheme slots into the currently-active theme / palette.
//

import SwiftUI
import XCTest
@testable import Nice

@MainActor
final class TweaksTerminalResolverTests: XCTestCase {

    private var defaults: UserDefaults!
    private var suiteName: String!
    private var catalog: TerminalThemeCatalog!

    override func setUp() async throws {
        try await super.setUp()
        suiteName = "tweaks-resolver-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)
        let tempDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("NiceTweaksResolver-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
        catalog = TerminalThemeCatalog(supportDirectory: tempDir)
    }

    override func tearDown() async throws {
        defaults.removePersistentDomain(forName: suiteName)
        try? FileManager.default.removeItem(at: catalog.supportDirectory)
        // Tweaks' didSets write to `.standard` regardless of the
        // injected store, so we clean those keys up here rather than
        // leave pollution for sibling tests.
        for key in [
            Tweaks.schemeKey,
            Tweaks.chromeLightPaletteKey,
            Tweaks.chromeDarkPaletteKey,
            Tweaks.terminalThemeLightKey,
            Tweaks.terminalThemeDarkKey,
            Tweaks.terminalFontFamilyKey,
        ] {
            UserDefaults.standard.removeObject(forKey: key)
        }
        defaults = nil
        suiteName = nil
        catalog = nil
        try await super.tearDown()
    }

    private func makeTweaks() -> Tweaks {
        Tweaks(
            defaults: defaults,
            osSchemeProvider: { .light },
            installOSObserver: false
        )
    }

    // MARK: - effectiveTerminalTheme

    func test_effectiveTerminalTheme_resolvesLightSlot_whenSchemeIsLight() {
        let tweaks = makeTweaks()
        tweaks.terminalThemeLightId = "solarized-light"
        tweaks.terminalThemeDarkId = "dracula"
        tweaks.scheme = .light
        XCTAssertEqual(
            tweaks.effectiveTerminalTheme(for: .light, catalog: catalog).id,
            "solarized-light"
        )
    }

    func test_effectiveTerminalTheme_resolvesDarkSlot_whenSchemeIsDark() {
        let tweaks = makeTweaks()
        tweaks.terminalThemeLightId = "solarized-light"
        tweaks.terminalThemeDarkId = "dracula"
        tweaks.scheme = .dark
        XCTAssertEqual(
            tweaks.effectiveTerminalTheme(for: .dark, catalog: catalog).id,
            "dracula"
        )
    }

    func test_effectiveTerminalTheme_unknownId_fallsBackToCatppuccinDefault() {
        let tweaks = makeTweaks()
        tweaks.terminalThemeLightId = "this-theme-was-deleted"
        tweaks.terminalThemeDarkId  = "also-gone"

        XCTAssertEqual(
            tweaks.effectiveTerminalTheme(for: .light, catalog: catalog).id,
            "catppuccin-latte"
        )
        XCTAssertEqual(
            tweaks.effectiveTerminalTheme(for: .dark, catalog: catalog).id,
            "catppuccin-mocha"
        )
    }

    // MARK: - activeChromePalette

    func test_activeChromePalette_returnsLightSlot_whenSchemeIsLight() {
        let tweaks = makeTweaks()
        tweaks.chromeLightPalette = .nice
        tweaks.chromeDarkPalette = .macOS
        tweaks.scheme = .light
        XCTAssertEqual(tweaks.activeChromePalette, .nice)
    }

    func test_activeChromePalette_returnsDarkSlot_whenSchemeIsDark() {
        let tweaks = makeTweaks()
        tweaks.chromeLightPalette = .nice
        tweaks.chromeDarkPalette = .macOS
        tweaks.scheme = .dark
        XCTAssertEqual(tweaks.activeChromePalette, .macOS)
    }

    // MARK: - Persistence of new keys

    func test_schemeChange_persistsToNewKey() {
        let tweaks = makeTweaks()
        tweaks.scheme = .dark
        XCTAssertEqual(
            UserDefaults.standard.string(forKey: Tweaks.schemeKey),
            "dark"
        )
    }

    func test_chromePaletteChange_persistsToNewKeys() {
        let tweaks = makeTweaks()
        tweaks.chromeLightPalette = .nice
        tweaks.chromeDarkPalette = .macOS
        XCTAssertEqual(
            UserDefaults.standard.string(forKey: Tweaks.chromeLightPaletteKey),
            "nice"
        )
        XCTAssertEqual(
            UserDefaults.standard.string(forKey: Tweaks.chromeDarkPaletteKey),
            "macOS"
        )
    }

    func test_terminalThemeIdChange_persists() {
        let tweaks = makeTweaks()
        tweaks.terminalThemeLightId = "solarized-light"
        XCTAssertEqual(
            UserDefaults.standard.string(forKey: Tweaks.terminalThemeLightKey),
            "solarized-light"
        )
    }

    func test_terminalFontFamily_nilClearsDefault() {
        let tweaks = makeTweaks()
        tweaks.terminalFontFamily = "Menlo-Regular"
        XCTAssertEqual(
            UserDefaults.standard.string(forKey: Tweaks.terminalFontFamilyKey),
            "Menlo-Regular"
        )
        tweaks.terminalFontFamily = nil
        XCTAssertNil(
            defaults.object(forKey: Tweaks.terminalFontFamilyKey),
            "Setting font family to nil should remove the UserDefaults entry so the helper falls back to the default chain."
        )
    }
}
