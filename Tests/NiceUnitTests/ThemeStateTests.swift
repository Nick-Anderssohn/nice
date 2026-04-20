//
//  ThemeStateTests.swift
//  NiceUnitTests
//
//  Unit tests for the theme state model in Sources/Nice/State/Tweaks.swift.
//
//  Every test creates its own isolated `UserDefaults(suiteName:)` so state
//  doesn't leak between tests. Tests never install the real distributed
//  notification observer — we always pass `installOSObserver: false`.
//
//  `Tweaks`'s `didSet` observers on `theme` / `syncWithOS` / `accent`
//  hardcode `UserDefaults.standard` rather than the injected `defaults`,
//  and the `theme` didSet + init also mutate `NSApp?.appearance`. So the
//  suite isolation above doesn't cover everything — setUp snapshots those
//  four values and tearDown restores them, leaving the test host's
//  user-visible theme settings untouched after the suite runs.
//

import AppKit
import SwiftUI
import XCTest
@testable import Nice

@MainActor
final class ThemeStateTests: XCTestCase {

    // MARK: - Per-test UserDefaults isolation

    private var suiteName: String!
    private var defaults: UserDefaults!

    // Snapshot of the global state that `Tweaks` mutates outside the
    // injected `defaults` suite, captured before each test runs.
    private var savedStandardTheme: String?
    private var savedStandardSync: Bool?
    private var savedStandardAccent: String?
    private var savedAppAppearance: NSAppearance?

    override func setUp() {
        super.setUp()
        suiteName = "test-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)

        savedStandardTheme = UserDefaults.standard.string(forKey: Tweaks.themeKey)
        savedStandardSync = UserDefaults.standard.object(forKey: Tweaks.syncKey) as? Bool
        savedStandardAccent = UserDefaults.standard.string(forKey: Tweaks.accentKey)
        savedAppAppearance = NSApp?.appearance
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suiteName)
        defaults = nil
        suiteName = nil

        restoreStandardDefault(Tweaks.themeKey, savedStandardTheme)
        restoreStandardDefault(Tweaks.syncKey, savedStandardSync)
        restoreStandardDefault(Tweaks.accentKey, savedStandardAccent)
        NSApp?.appearance = savedAppAppearance

        super.tearDown()
    }

    private func restoreStandardDefault(_ key: String, _ value: Any?) {
        if let value {
            UserDefaults.standard.set(value, forKey: key)
        } else {
            UserDefaults.standard.removeObject(forKey: key)
        }
    }

    // MARK: - Helpers

    /// Stubbed OS-scheme provider. Tests mutate `stubbedOSScheme` to
    /// simulate the OS flipping between light and dark.
    private final class OSSchemeStub {
        var stubbedOSScheme: ColorScheme = .light
    }

    private func makeTweaks(
        os: ColorScheme = .light,
        stub: OSSchemeStub? = nil
    ) -> (Tweaks, OSSchemeStub) {
        let s = stub ?? OSSchemeStub()
        s.stubbedOSScheme = os
        let tweaks = Tweaks(
            defaults: defaults,
            osSchemeProvider: { s.stubbedOSScheme },
            installOSObserver: false
        )
        return (tweaks, s)
    }

    // MARK: - ThemeChoice: pure property tests

    func test_themeChoice_palette_partitionsCorrectly() {
        XCTAssertEqual(ThemeChoice.niceLight.palette, .nice)
        XCTAssertEqual(ThemeChoice.niceDark.palette,  .nice)
        XCTAssertEqual(ThemeChoice.macLight.palette,  .macOS)
        XCTAssertEqual(ThemeChoice.macDark.palette,   .macOS)
    }

    func test_themeChoice_scheme_partitionsCorrectly() {
        XCTAssertEqual(ThemeChoice.niceLight.scheme, .light)
        XCTAssertEqual(ThemeChoice.macLight.scheme,  .light)
        XCTAssertEqual(ThemeChoice.niceDark.scheme,  .dark)
        XCTAssertEqual(ThemeChoice.macDark.scheme,   .dark)
    }

    func test_themeChoice_counterpart_flipsSchemeWithinFamily() {
        XCTAssertEqual(ThemeChoice.niceLight.counterpart, .niceDark)
        XCTAssertEqual(ThemeChoice.niceDark.counterpart,  .niceLight)
        XCTAssertEqual(ThemeChoice.macLight.counterpart,  .macDark)
        XCTAssertEqual(ThemeChoice.macDark.counterpart,   .macLight)

        // Double-counterpart is identity (stays in same palette family).
        for c in ThemeChoice.allCases {
            XCTAssertEqual(c.counterpart.counterpart, c,
                           "double-counterpart should be identity for \(c)")
            XCTAssertEqual(c.counterpart.palette, c.palette,
                           "counterpart should stay in same palette family for \(c)")
        }
    }

    func test_themeChoice_nsAppearance_matchesScheme() {
        XCTAssertEqual(ThemeChoice.niceLight.nsAppearance.name, .aqua)
        XCTAssertEqual(ThemeChoice.macLight.nsAppearance.name,  .aqua)
        XCTAssertEqual(ThemeChoice.niceDark.nsAppearance.name,  .darkAqua)
        XCTAssertEqual(ThemeChoice.macDark.nsAppearance.name,   .darkAqua)
    }

    func test_themeChoice_label_formatsCorrectly() {
        XCTAssertEqual(ThemeChoice.niceLight.label, "Light")
        XCTAssertEqual(ThemeChoice.niceDark.label,  "Dark")
        XCTAssertEqual(ThemeChoice.macLight.label,  "macOS Light")
        XCTAssertEqual(ThemeChoice.macDark.label,   "macOS Dark")
    }

    // MARK: - Tweaks.userPicked

    func test_userPicked_syncOff_pinsExactly() {
        let (tweaks, stub) = makeTweaks(os: .light)
        tweaks.syncWithOS = false
        // OS is dark but sync is off — we should still get exactly what
        // the user picked.
        stub.stubbedOSScheme = .dark

        tweaks.userPicked(.macLight)
        XCTAssertEqual(tweaks.theme, .macLight)

        tweaks.userPicked(.niceDark)
        XCTAssertEqual(tweaks.theme, .niceDark)

        tweaks.userPicked(.macDark)
        XCTAssertEqual(tweaks.theme, .macDark)
    }

    func test_userPicked_syncOn_OSMatches_setsExactly() {
        let (tweaks, stub) = makeTweaks(os: .light)
        // Force a known starting state with sync on and OS=light.
        stub.stubbedOSScheme = .light
        tweaks.syncWithOS = true

        // Picking a light-scheme theme matches OS — gets set exactly.
        tweaks.userPicked(.macLight)
        XCTAssertEqual(tweaks.theme, .macLight)

        tweaks.userPicked(.niceLight)
        XCTAssertEqual(tweaks.theme, .niceLight)
    }

    func test_userPicked_syncOn_OSMismatch_disablesSyncAndPinsExactly() {
        let (tweaks, stub) = makeTweaks(os: .dark)
        stub.stubbedOSScheme = .dark
        tweaks.syncWithOS = true

        // User picks a light theme while OS is dark and sync is on.
        // New behavior: treat the click as an explicit override — turn
        // sync off and pin the theme to exactly what was picked.
        tweaks.userPicked(.niceLight)
        XCTAssertEqual(tweaks.theme, .niceLight)
        XCTAssertFalse(tweaks.syncWithOS)

        // Turn sync back on to test the macOS family path in isolation.
        tweaks.syncWithOS = true
        // syncToggleOn reconciles; OS is dark so theme becomes niceDark.
        XCTAssertEqual(tweaks.theme, .niceDark)
        XCTAssertTrue(tweaks.syncWithOS)

        tweaks.userPicked(.macLight)
        XCTAssertEqual(tweaks.theme, .macLight)
        XCTAssertFalse(tweaks.syncWithOS)

        // And the dark-picked-while-OS-light variant disables sync too.
        stub.stubbedOSScheme = .light
        tweaks.syncWithOS = true
        // Reconcile on toggle-on aligns theme to light — macLight stays.
        XCTAssertEqual(tweaks.theme, .macLight)
        tweaks.userPicked(.niceDark)
        XCTAssertEqual(tweaks.theme, .niceDark)
        XCTAssertFalse(tweaks.syncWithOS)
    }

    // MARK: - Tweaks.reconcileWithOS

    func test_reconcile_syncOff_isNoop() {
        let (tweaks, stub) = makeTweaks(os: .light)
        tweaks.syncWithOS = false
        tweaks.theme = .niceLight

        // OS flips to dark; reconcile should be a no-op because sync is off.
        stub.stubbedOSScheme = .dark
        tweaks.reconcileWithOS()
        XCTAssertEqual(tweaks.theme, .niceLight)
    }

    func test_reconcile_syncOn_aligned_isNoop() {
        let (tweaks, stub) = makeTweaks(os: .light)
        stub.stubbedOSScheme = .light
        tweaks.syncWithOS = true
        tweaks.theme = .niceLight // already aligned

        tweaks.reconcileWithOS()
        XCTAssertEqual(tweaks.theme, .niceLight)
    }

    func test_reconcile_syncOn_misaligned_flipsCounterpart_niceFamily() {
        let (tweaks, stub) = makeTweaks(os: .light)
        stub.stubbedOSScheme = .light
        tweaks.syncWithOS = true
        tweaks.theme = .niceDark // misaligned

        tweaks.reconcileWithOS()
        XCTAssertEqual(tweaks.theme, .niceLight)

        // Flip OS back to dark; reconcile flips counterpart again.
        stub.stubbedOSScheme = .dark
        tweaks.reconcileWithOS()
        XCTAssertEqual(tweaks.theme, .niceDark)
    }

    func test_reconcile_syncOn_misaligned_flipsCounterpart_macFamily() {
        let (tweaks, stub) = makeTweaks(os: .dark)
        stub.stubbedOSScheme = .dark
        tweaks.syncWithOS = true
        tweaks.theme = .macLight // misaligned

        tweaks.reconcileWithOS()
        XCTAssertEqual(tweaks.theme, .macDark)

        stub.stubbedOSScheme = .light
        tweaks.reconcileWithOS()
        XCTAssertEqual(tweaks.theme, .macLight)
    }

    // MARK: - Tweaks.syncWithOS toggle

    func test_syncToggleOn_whenMisaligned_reconcilesImmediately() {
        let (tweaks, stub) = makeTweaks(os: .dark)
        stub.stubbedOSScheme = .dark
        tweaks.syncWithOS = false
        tweaks.theme = .niceLight // misaligned with OS=dark

        // Flipping sync on should trigger reconcile and flip to niceDark.
        tweaks.syncWithOS = true
        XCTAssertEqual(tweaks.theme, .niceDark)
        XCTAssertTrue(tweaks.syncWithOS)
    }

    func test_syncToggleOn_whenAligned_staysPut() {
        let (tweaks, stub) = makeTweaks(os: .light)
        stub.stubbedOSScheme = .light
        tweaks.syncWithOS = false
        tweaks.theme = .macLight // already aligned with OS=light

        tweaks.syncWithOS = true
        XCTAssertEqual(tweaks.theme, .macLight)
    }

    func test_syncToggleOff_freezesCurrentTheme() {
        let (tweaks, stub) = makeTweaks(os: .light)
        stub.stubbedOSScheme = .light
        tweaks.syncWithOS = true
        tweaks.theme = .niceLight

        // Flip sync off — OS changing should no longer affect theme.
        tweaks.syncWithOS = false
        stub.stubbedOSScheme = .dark
        tweaks.reconcileWithOS()
        XCTAssertEqual(tweaks.theme, .niceLight,
                       "reconcile with sync off should not change theme")
    }

    // MARK: - UserDefaults migration (Tweaks.loadOrMigrate)

    func test_migration_legacySystem_mapsToMacOSChromeWithSync() {
        defaults.set("system", forKey: Tweaks.themeKey)

        let light = Tweaks.loadOrMigrate(defaults: defaults, osScheme: .light)
        XCTAssertEqual(light.scheme, .light)
        XCTAssertEqual(light.chromeLightPalette, .macOS)
        XCTAssertEqual(light.chromeDarkPalette, .macOS)
        XCTAssertTrue(light.syncWithOS)

        defaults.set("system", forKey: Tweaks.themeKey) // the first call defer-removed it
        let dark = Tweaks.loadOrMigrate(defaults: defaults, osScheme: .dark)
        XCTAssertEqual(dark.scheme, .dark)
        XCTAssertEqual(dark.chromeDarkPalette, .macOS)
        XCTAssertTrue(dark.syncWithOS)
    }

    func test_migration_legacyLight_mapsToNiceChromePinned() {
        defaults.set("light", forKey: Tweaks.themeKey)
        let m = Tweaks.loadOrMigrate(defaults: defaults, osScheme: .dark)
        XCTAssertEqual(m.scheme, .light)
        XCTAssertEqual(m.chromeLightPalette, .nice)
        XCTAssertEqual(m.chromeDarkPalette, .nice)
        XCTAssertFalse(m.syncWithOS)
    }

    func test_migration_legacyDark_mapsToNiceChromePinned() {
        defaults.set("dark", forKey: Tweaks.themeKey)
        let m = Tweaks.loadOrMigrate(defaults: defaults, osScheme: .light)
        XCTAssertEqual(m.scheme, .dark)
        XCTAssertEqual(m.chromeLightPalette, .nice)
        XCTAssertEqual(m.chromeDarkPalette, .nice)
        XCTAssertFalse(m.syncWithOS)
    }

    func test_migration_freshInstall_osLight_mapsToCatppuccinWithSync() {
        XCTAssertNil(defaults.object(forKey: Tweaks.themeKey))
        let m = Tweaks.loadOrMigrate(defaults: defaults, osScheme: .light)
        XCTAssertEqual(m.scheme, .light)
        XCTAssertEqual(m.chromeLightPalette, .catppuccinLatte)
        XCTAssertEqual(m.chromeDarkPalette, .catppuccinMocha)
        XCTAssertTrue(m.syncWithOS)
    }

    func test_migration_freshInstall_osDark_mapsToCatppuccinWithSync() {
        XCTAssertNil(defaults.object(forKey: Tweaks.themeKey))
        let m = Tweaks.loadOrMigrate(defaults: defaults, osScheme: .dark)
        XCTAssertEqual(m.scheme, .dark)
        XCTAssertEqual(m.chromeLightPalette, .catppuccinLatte)
        XCTAssertEqual(m.chromeDarkPalette, .catppuccinMocha)
        XCTAssertTrue(m.syncWithOS)
    }

    func test_migration_themeChoice_macDark() {
        defaults.set("macDark", forKey: Tweaks.themeKey)
        defaults.set(true,      forKey: Tweaks.syncKey)

        let m = Tweaks.loadOrMigrate(defaults: defaults, osScheme: .light)
        XCTAssertEqual(m.scheme, .dark)
        XCTAssertEqual(m.chromeLightPalette, .macOS)
        XCTAssertEqual(m.chromeDarkPalette, .macOS)
        XCTAssertTrue(m.syncWithOS)
    }

    func test_migration_themeChoice_niceLight_unsynced() {
        defaults.set("niceLight", forKey: Tweaks.themeKey)
        defaults.set(false,       forKey: Tweaks.syncKey)
        let m = Tweaks.loadOrMigrate(defaults: defaults, osScheme: .dark)
        XCTAssertEqual(m.scheme, .light)
        XCTAssertEqual(m.chromeLightPalette, .nice)
        XCTAssertEqual(m.chromeDarkPalette, .nice)
        XCTAssertFalse(m.syncWithOS)
    }

    func test_migration_removesLegacyThemeKey() {
        defaults.set("macDark", forKey: Tweaks.themeKey)
        _ = Tweaks.loadOrMigrate(defaults: defaults, osScheme: .light)
        XCTAssertNil(
            defaults.object(forKey: Tweaks.themeKey),
            "Migration should delete the legacy theme key so subsequent reads take the fast path."
        )
    }

    func test_migration_newChromeKeys_roundtrip() {
        defaults.set("nice",  forKey: Tweaks.chromeLightPaletteKey)
        defaults.set("macOS", forKey: Tweaks.chromeDarkPaletteKey)
        defaults.set("dark",  forKey: Tweaks.schemeKey)
        defaults.set(false,   forKey: Tweaks.syncKey)
        let m = Tweaks.loadOrMigrate(defaults: defaults, osScheme: .light)
        XCTAssertEqual(m.scheme, .dark)
        XCTAssertEqual(m.chromeLightPalette, .nice)
        XCTAssertEqual(m.chromeDarkPalette, .macOS)
        XCTAssertFalse(m.syncWithOS)
    }

    // MARK: - Init integration

    func test_init_readsFromDefaults() {
        defaults.set("macLight", forKey: Tweaks.themeKey)
        defaults.set(false,      forKey: Tweaks.syncKey)

        let tweaks = Tweaks(
            defaults: defaults,
            osSchemeProvider: { .dark }, // OS says dark — should NOT affect
                                         // because sync is off
            installOSObserver: false
        )
        XCTAssertEqual(tweaks.theme, .macLight)
        XCTAssertFalse(tweaks.syncWithOS)
    }

    func test_init_whenSyncOnAndMisaligned_reconcilesOnLaunch() {
        // Persisted: niceLight + sync=true. OS is dark. On launch,
        // Tweaks.init should call reconcileWithOS and flip to niceDark.
        defaults.set("niceLight", forKey: Tweaks.themeKey)
        defaults.set(true,        forKey: Tweaks.syncKey)

        let tweaks = Tweaks(
            defaults: defaults,
            osSchemeProvider: { .dark },
            installOSObserver: false
        )
        XCTAssertEqual(tweaks.theme, .niceDark)
        XCTAssertTrue(tweaks.syncWithOS)
    }
}
