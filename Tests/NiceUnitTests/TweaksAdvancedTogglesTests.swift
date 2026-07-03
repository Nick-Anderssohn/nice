//
//  TweaksAdvancedTogglesTests.swift
//  NiceUnitTests
//
//  Coverage for the `smoothScrolling` property on `Tweaks`:
//    • smoothScrolling is opt-in: defaults OFF when the key is absent,
//      but an explicit value (including a legacy ON) is honored,
//    • an explicit OFF persists across re-init (no reset-to-default),
//    • round-trip: a write through the injected `defaults` is visible
//      to a second `Tweaks` constructed on the same suite,
//    • the persistence key is the legacy string that shipped before
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

    func test_smoothScrolling_explicitFalse_survivesReinit() {
        let suite = freshSuite()
        defer { wipeSuite(suite) }

        suite.set(false, forKey: Tweaks.smoothScrollingKey)
        let tweaks = makeTweaks(defaults: suite)
        XCTAssertFalse(tweaks.smoothScrolling,
                       "Explicit false pre-seeded in defaults must not be reset to true on init")
    }

    // MARK: - Round-trip persistence

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

    func test_smoothScrollingKey_isLegacyString() {
        XCTAssertEqual(Tweaks.smoothScrollingKey, "smoothScrolling")
    }

    // MARK: - Activity badge presentation

    func test_activityBadgeCompact_defaultsToFalse_whenKeyAbsent() {
        let suite = freshSuite()
        defer { wipeSuite(suite) }

        let tweaks = makeTweaks(defaults: suite)
        XCTAssertFalse(tweaks.activityBadgeCompact,
                       "The badge defaults to the full dot + label presentation")
    }

    func test_activityBadgeCompact_roundTrips_persistsThroughInjectedDefaults() {
        let suite = freshSuite()
        defer { wipeSuite(suite) }

        let first = makeTweaks(defaults: suite)
        first.activityBadgeCompact = true

        let second = makeTweaks(defaults: suite)
        XCTAssertTrue(second.activityBadgeCompact,
                      "A compact choice must survive across relaunch")
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
