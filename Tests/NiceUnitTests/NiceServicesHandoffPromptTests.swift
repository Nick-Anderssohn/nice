//
//  NiceServicesHandoffPromptTests.swift
//  NiceUnitTests
//
//  Pins `NiceServices.consumeHandoffSkillPromptSlot()` — the one-shot
//  gate that ensures the "Install the Nice Handoff skill?" alert fires
//  at most once per process lifetime regardless of how many windows call
//  it. Mirrors the style of NiceServicesCleanupTests.
//
//  Also pins `Tweaks.installHandoffSkill` and
//  `Tweaks.handoffSkillPromptSeen` round-trip through UserDefaults,
//  using an isolated UserDefaults suite so the tests never touch the
//  developer's real UserDefaults domain. Mirrors TweaksEditorsTests.
//

import Foundation
import XCTest
@testable import Nice

// MARK: - NiceServices one-shot gate

@MainActor
final class NiceServicesHandoffPromptTests: XCTestCase {

    func test_consumeHandoffSkillPromptSlot_returnsTrueOnFirstCall() {
        // Fresh NiceServices instance — handoffSkillPromptFired starts false.
        let services = NiceServices()
        XCTAssertTrue(
            services.consumeHandoffSkillPromptSlot(),
            "first call must return true so the alert fires exactly once"
        )
    }

    func test_consumeHandoffSkillPromptSlot_returnsFalseOnSecondCall() {
        let services = NiceServices()
        _ = services.consumeHandoffSkillPromptSlot() // first call, consume
        XCTAssertFalse(
            services.consumeHandoffSkillPromptSlot(),
            "second call must return false — one-shot gate must not fire again"
        )
    }

    func test_consumeHandoffSkillPromptSlot_alwaysFalseAfterFirst() {
        let services = NiceServices()
        _ = services.consumeHandoffSkillPromptSlot()
        for _ in 0..<5 {
            XCTAssertFalse(
                services.consumeHandoffSkillPromptSlot(),
                "every call after the first must return false"
            )
        }
    }

    func test_separateServicesInstances_eachHaveIndependentGate() {
        // Two NiceServices instances (two process lifetimes simulated)
        // must each start with a fresh gate. The real app creates one
        // NiceServices per launch; this guards against accidental
        // class-level (static) state leaking across instances.
        let a = NiceServices()
        let b = NiceServices()
        XCTAssertTrue(a.consumeHandoffSkillPromptSlot(),
                      "instance A: first call must return true")
        XCTAssertTrue(b.consumeHandoffSkillPromptSlot(),
                      "instance B: first call must return true (independent gate)")
    }
}

// MARK: - Tweaks handoff flags

@MainActor
final class TweaksHandoffFlagsTests: XCTestCase {

    // MARK: - installHandoffSkill

    func test_installHandoffSkill_defaultsFalse() {
        // Fresh suite — key never written.
        let tweaks = makeTweaks()
        XCTAssertFalse(tweaks.installHandoffSkill,
                       "installHandoffSkill must default to false on a fresh install")
    }

    func test_installHandoffSkill_persistsTrueToUserDefaults() {
        let suite = freshSuite()
        let tweaks = makeTweaks(suite: suite)

        tweaks.installHandoffSkill = true

        // Round-trip: a second Tweaks reading from the same suite must
        // see the persisted value.
        let tweaks2 = makeTweaks(suite: suite)
        XCTAssertTrue(tweaks2.installHandoffSkill,
                      "installHandoffSkill = true must round-trip through UserDefaults")
    }

    func test_installHandoffSkill_persistsFalseToUserDefaults() {
        let suite = freshSuite()
        let tweaks = makeTweaks(suite: suite)
        tweaks.installHandoffSkill = true   // write true first…
        tweaks.installHandoffSkill = false  // …then flip back

        let tweaks2 = makeTweaks(suite: suite)
        XCTAssertFalse(tweaks2.installHandoffSkill,
                       "installHandoffSkill = false must round-trip through UserDefaults")
    }

    // MARK: - handoffSkillPromptSeen

    func test_handoffSkillPromptSeen_defaultsFalse() {
        let tweaks = makeTweaks()
        XCTAssertFalse(tweaks.handoffSkillPromptSeen,
                       "handoffSkillPromptSeen must default to false (prompt unseen on fresh install)")
    }

    func test_handoffSkillPromptSeen_persistsTrueToUserDefaults() {
        let suite = freshSuite()
        let tweaks = makeTweaks(suite: suite)

        tweaks.handoffSkillPromptSeen = true

        let tweaks2 = makeTweaks(suite: suite)
        XCTAssertTrue(tweaks2.handoffSkillPromptSeen,
                      "handoffSkillPromptSeen = true must round-trip through UserDefaults")
    }

    func test_handoffSkillPromptSeen_persistsFalseToUserDefaults() {
        let suite = freshSuite()
        let tweaks = makeTweaks(suite: suite)
        tweaks.handoffSkillPromptSeen = true
        tweaks.handoffSkillPromptSeen = false

        let tweaks2 = makeTweaks(suite: suite)
        XCTAssertFalse(tweaks2.handoffSkillPromptSeen,
                       "handoffSkillPromptSeen = false must round-trip through UserDefaults")
    }

    func test_twoFlagsAreIndependent() {
        // Setting one flag must not affect the other.
        let suite = freshSuite()
        let tweaks = makeTweaks(suite: suite)
        tweaks.handoffSkillPromptSeen = true
        // installHandoffSkill is still false.
        XCTAssertFalse(tweaks.installHandoffSkill,
                       "setting handoffSkillPromptSeen must not affect installHandoffSkill")

        tweaks.installHandoffSkill = true
        // Both are now true.
        XCTAssertTrue(tweaks.handoffSkillPromptSeen)
        XCTAssertTrue(tweaks.installHandoffSkill)

        let tweaks2 = makeTweaks(suite: suite)
        XCTAssertTrue(tweaks2.handoffSkillPromptSeen)
        XCTAssertTrue(tweaks2.installHandoffSkill)
    }

    // MARK: - helpers

    private func makeTweaks(suite: UserDefaults? = nil) -> Tweaks {
        Tweaks(
            defaults: suite ?? freshSuite(),
            osSchemeProvider: { .light },
            installOSObserver: false
        )
    }

    private func freshSuite() -> UserDefaults {
        UserDefaults(suiteName: "tweaks-handoff-\(UUID().uuidString)")!
    }
}
