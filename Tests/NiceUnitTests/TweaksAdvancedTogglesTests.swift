//
//  TweaksAdvancedTogglesTests.swift
//  NiceUnitTests
//
//  Coverage for the `hardwareAcceleration` and `smoothScrolling`
//  properties on `Tweaks`:
//    • hardwareAcceleration defaults ON when the key is absent,
//    • smoothScrolling is opt-in: defaults OFF when the key is absent,
//      but an explicit value (including a legacy ON) is honored,
//    • an explicit OFF persists across re-init (no reset-to-default),
//    • round-trip: a write through the injected `defaults` is visible
//      to a second `Tweaks` constructed on the same suite,
//    • the persistence keys are the legacy strings that shipped before
//      the Settings toggle existed.
//
//  All tests use an isolated UserDefaults suite so they don't touch the
//  user's real `.standard` domain.
//

import Foundation
import SwiftUI
import XCTest
@testable import Nice

@MainActor
final class TweaksAdvancedTogglesTests: XCTestCase {

    // MARK: - Default values

    func test_hardwareAcceleration_defaultsToTrue_whenKeyAbsent() {
        let suite = freshSuite()
        defer { wipeSuite(suite) }

        let tweaks = makeTweaks(defaults: suite)
        XCTAssertTrue(tweaks.hardwareAcceleration,
                      "hardwareAcceleration must default ON when the key is absent")
    }

    func test_smoothScrolling_defaultsToFalse_whenKeyAbsent() {
        let suite = freshSuite()
        defer { wipeSuite(suite) }

        let tweaks = makeTweaks(defaults: suite)
        XCTAssertFalse(tweaks.smoothScrolling,
                       "smoothScrolling is opt-in and must default OFF when the key is absent")
    }

    func test_smoothScrolling_explicitTrue_isHonored() {
        // A legacy ON (saved when the old toggle existed) must survive — we only
        // change the absent-key default, we don't force existing prefs off.
        let suite = freshSuite()
        defer { wipeSuite(suite) }

        suite.set(true, forKey: Tweaks.smoothScrollingKey)
        let tweaks = makeTweaks(defaults: suite)
        XCTAssertTrue(tweaks.smoothScrolling,
                      "An explicit smoothScrolling=true must be honored, not reset to OFF")
    }

    // MARK: - Explicit OFF survives re-init

    func test_hardwareAcceleration_explicitFalse_survivesReinit() {
        // Seed the suite directly before constructing Tweaks so the
        // "absent key → default ON" branch is bypassed on init.
        let suite = freshSuite()
        defer { wipeSuite(suite) }

        suite.set(false, forKey: Tweaks.hardwareAccelerationKey)
        let tweaks = makeTweaks(defaults: suite)
        XCTAssertFalse(tweaks.hardwareAcceleration,
                       "Explicit false pre-seeded in defaults must not be reset to true on init")
    }

    func test_smoothScrolling_explicitFalse_survivesReinit() {
        let suite = freshSuite()
        defer { wipeSuite(suite) }

        suite.set(false, forKey: Tweaks.smoothScrollingKey)
        let tweaks = makeTweaks(defaults: suite)
        XCTAssertFalse(tweaks.smoothScrolling,
                       "Explicit false pre-seeded in defaults must not be reset to true on init")
    }

    // MARK: - Round-trip persistence

    func test_hardwareAcceleration_roundTrip_persistsThroughInjectedDefaults() {
        // Verifies TASK 1: the didSet writes through the injected `defaults`,
        // not `.standard`, so a second Tweaks on the same suite sees the write.
        let suite = freshSuite()
        defer { wipeSuite(suite) }

        let first = makeTweaks(defaults: suite)
        first.hardwareAcceleration = false

        let second = makeTweaks(defaults: suite)
        XCTAssertFalse(second.hardwareAcceleration,
                       "hardwareAcceleration=false must round-trip through the injected defaults suite")
    }

    func test_smoothScrolling_roundTrip_persistsThroughInjectedDefaults() {
        let suite = freshSuite()
        defer { wipeSuite(suite) }

        let first = makeTweaks(defaults: suite)
        first.smoothScrolling = false

        let second = makeTweaks(defaults: suite)
        XCTAssertFalse(second.smoothScrolling,
                       "smoothScrolling=false must round-trip through the injected defaults suite")
    }

    // MARK: - Legacy persistence keys

    func test_hardwareAccelerationKey_isLegacyString() {
        // The key must stay "gpuRendering" so existing user prefs survive
        // after the Settings toggle was introduced. Changing the key would
        // silently reset every user's hardware-acceleration setting to ON.
        XCTAssertEqual(Tweaks.hardwareAccelerationKey, "gpuRendering")
    }

    func test_smoothScrollingKey_isLegacyString() {
        XCTAssertEqual(Tweaks.smoothScrollingKey, "smoothScrolling")
    }

    // MARK: - helpers

    private func makeTweaks(defaults: UserDefaults) -> Tweaks {
        Tweaks(
            defaults: defaults,
            osSchemeProvider: { .light },
            installOSObserver: false
        )
    }

    private func freshSuite() -> UserDefaults {
        UserDefaults(suiteName: "tweaks-advanced-toggles-\(UUID().uuidString)")!
    }

    private func wipeSuite(_ suite: UserDefaults) {
        suite.dictionaryRepresentation().keys.forEach {
            suite.removeObject(forKey: $0)
        }
    }
}
