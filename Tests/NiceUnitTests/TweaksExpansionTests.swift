//
//  TweaksExpansionTests.swift
//  NiceUnitTests
//
//  Covers the Tweaks helpers that `TweaksTerminalResolverTests` doesn't
//  touch: `ThemeChoice.counterpart`, `AccentPreset` hex → Color/NSColor
//  parity, and the `syncWithOS` observer that keeps `scheme` aligned
//  with the OS appearance.
//

import AppKit
import SwiftUI
import XCTest
@testable import Nice

@MainActor
final class TweaksExpansionTests: XCTestCase {

    // MARK: - ThemeChoice.counterpart

    func test_counterpart_preservesPalette() {
        // counterpart is used by syncWithOS when the OS flips light/dark.
        // The user's palette choice (nice vs macOS) must NOT change —
        // only the scheme flips. A bug here would swap palettes every
        // time the user's OS appearance changes.
        XCTAssertEqual(ThemeChoice.niceLight.counterpart, .niceDark)
        XCTAssertEqual(ThemeChoice.niceDark.counterpart, .niceLight)
        XCTAssertEqual(ThemeChoice.macLight.counterpart, .macDark)
        XCTAssertEqual(ThemeChoice.macDark.counterpart, .macLight)
    }

    func test_counterpart_isInvolution() {
        // Double-counterpart must return to the original — flipping the
        // scheme twice is a no-op.
        for choice in ThemeChoice.allCases {
            XCTAssertEqual(choice.counterpart.counterpart, choice,
                           "counterpart.counterpart must equal self for \(choice).")
        }
    }

    // MARK: - AccentPreset hex → NSColor

    func test_accentHex_allPresetsAreWellFormed() {
        for preset in AccentPreset.allCases {
            let hex = preset.hex
            XCTAssertTrue(hex.hasPrefix("#"), "Hex must start with #: \(hex)")
            XCTAssertEqual(hex.count, 7, "Expected #rrggbb format: \(hex)")
            let digits = String(hex.dropFirst())
            XCTAssertTrue(digits.allSatisfy(\.isHexDigit),
                          "Hex body must be hex digits only: \(hex)")
        }
    }

    func test_accentNSColor_parsesHexCorrectly() {
        // terracotta #c96442 → RGB(201, 100, 66). Use sRGB component
        // accessors to avoid picking up color-space mismatches.
        let ns = AccentPreset.terracotta.nsColor
        let srgb = ns.usingColorSpace(.sRGB)
        XCTAssertNotNil(srgb)
        XCTAssertEqual(Int(round(srgb!.redComponent * 255)), 0xC9)
        XCTAssertEqual(Int(round(srgb!.greenComponent * 255)), 0x64)
        XCTAssertEqual(Int(round(srgb!.blueComponent * 255)), 0x42)
        XCTAssertEqual(srgb!.alphaComponent, 1.0)
    }

    func test_accentNSColor_forEveryPreset_parsesWithoutNil() {
        // Any hex Scanner can't parse would yield a zero color with
        // alpha 1 — visually identical to black. Catch that regression.
        for preset in AccentPreset.allCases {
            let ns = preset.nsColor.usingColorSpace(.sRGB)
            XCTAssertNotNil(ns)
            // Expect non-zero color (no preset is pure black #000000).
            let r = ns!.redComponent, g = ns!.greenComponent, b = ns!.blueComponent
            XCTAssertFalse(r == 0 && g == 0 && b == 0,
                           "\(preset) decoded to black; hex parsing likely failed.")
        }
    }

    // MARK: - syncWithOS observer

    private func makeTweaks(
        osScheme: @escaping () -> ColorScheme,
        defaults: UserDefaults
    ) -> Tweaks {
        Tweaks(
            defaults: defaults,
            osSchemeProvider: osScheme,
            installOSObserver: false
        )
    }

    func test_reconcileWithOS_whenSyncOn_flipsSchemeToOS() {
        let defaults = freshDefaults()
        defer { cleanDefaults(defaults) }

        var osScheme: ColorScheme = .light
        let tweaks = makeTweaks(osScheme: { osScheme }, defaults: defaults)
        tweaks.syncWithOS = true
        tweaks.scheme = .light
        XCTAssertEqual(tweaks.scheme, .light)

        // Simulate the OS flipping to dark, then call the same hook the
        // DistributedNotificationCenter observer would fire.
        osScheme = .dark
        tweaks.reconcileWithOS()
        XCTAssertEqual(tweaks.scheme, .dark,
                       "With syncWithOS on, reconcileWithOS must track the OS scheme.")
    }

    func test_reconcileWithOS_whenSyncOff_isNoOp() {
        let defaults = freshDefaults()
        defer { cleanDefaults(defaults) }

        var osScheme: ColorScheme = .light
        let tweaks = makeTweaks(osScheme: { osScheme }, defaults: defaults)
        tweaks.syncWithOS = false
        tweaks.scheme = .light

        osScheme = .dark
        tweaks.reconcileWithOS()
        XCTAssertEqual(tweaks.scheme, .light,
                       "With syncWithOS off the user's scheme must stay pinned regardless of OS changes.")
    }

    func test_settingSyncOn_immediatelyReconcilesWithOS() {
        // Flipping syncWithOS from false to true must pull the scheme
        // into alignment in the same tick — not wait for the next OS
        // notification. The didSet observer fires reconcileWithOS().
        let defaults = freshDefaults()
        defer { cleanDefaults(defaults) }

        let tweaks = makeTweaks(osScheme: { .dark }, defaults: defaults)
        tweaks.syncWithOS = false
        tweaks.scheme = .light
        XCTAssertEqual(tweaks.scheme, .light)

        tweaks.syncWithOS = true
        XCTAssertEqual(tweaks.scheme, .dark,
                       "Enabling syncWithOS must immediately align scheme to the OS.")
    }

    // MARK: - helpers

    private func freshDefaults() -> UserDefaults {
        UserDefaults(suiteName: "tweaks-expansion-\(UUID().uuidString)")!
    }

    /// Tweaks' didSet observers write to `.standard` regardless of the
    /// injected defaults. Scrub the keys so these tests don't leak
    /// state into the next test (or the user's real defaults if the
    /// suite crashes before teardown).
    private func cleanDefaults(_ defaults: UserDefaults) {
        for key in [
            Tweaks.schemeKey,
            Tweaks.syncKey,
            Tweaks.accentKey,
            Tweaks.chromeLightPaletteKey,
            Tweaks.chromeDarkPaletteKey,
            Tweaks.terminalThemeLightKey,
            Tweaks.terminalThemeDarkKey,
            Tweaks.terminalFontFamilyKey,
            Tweaks.gpuRenderingKey,
            Tweaks.smoothScrollingKey,
        ] {
            UserDefaults.standard.removeObject(forKey: key)
        }
        if let name = defaults.dictionaryRepresentation().keys.first {
            _ = name
        }
        // Suite cleanup.
        defaults.dictionaryRepresentation().keys.forEach {
            defaults.removeObject(forKey: $0)
        }
    }
}
