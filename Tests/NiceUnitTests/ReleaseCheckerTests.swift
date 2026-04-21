//
//  ReleaseCheckerTests.swift
//  NiceUnitTests
//
//  Covers the two pieces that drive the toolbar "Update available"
//  pill:
//    • `SemanticVersion` parse + compare — bad string → nil; `v` prefix
//      stripped; component-wise integer compare (no "0.1.10 < 0.1.9"
//      lexicographic bug); missing components treated as 0.
//    • `ReleaseChecker` — reacts to injected fetcher results:
//      updateAvailable flips on only when newer; equal/older don't
//      flip; thrown errors leave the previous state intact; the last-
//      seen tag is cached in the injected `UserDefaults` so a fresh
//      instance can seed from it.
//
//  Uses an isolated `UserDefaults(suiteName:)` per test (same pattern
//  as `FontSettingsTests`) so cached tags never leak across tests or
//  into the user's real defaults.
//

import Foundation
import XCTest
@testable import Nice

// MARK: - SemanticVersion

final class SemanticVersionTests: XCTestCase {

    func test_parsesPlainDotted() {
        XCTAssertEqual(SemanticVersion("0.1.5")?.components, [0, 1, 5])
        XCTAssertEqual(SemanticVersion("10.20.30")?.components, [10, 20, 30])
    }

    func test_stripsLeadingV() {
        XCTAssertEqual(SemanticVersion("v0.1.5"), SemanticVersion("0.1.5"))
        XCTAssertEqual(SemanticVersion("V2.0"), SemanticVersion("2.0"))
    }

    func test_trimsWhitespace() {
        XCTAssertEqual(SemanticVersion("  v1.2.3  "), SemanticVersion("1.2.3"))
    }

    func test_missingComponentsAreZero() {
        XCTAssertEqual(SemanticVersion("1"), SemanticVersion("1.0.0"))
        XCTAssertEqual(SemanticVersion("1.2"), SemanticVersion("1.2.0"))
    }

    func test_componentWiseCompare_notLexicographic() {
        let nine = SemanticVersion("0.1.9")!
        let ten  = SemanticVersion("0.1.10")!
        XCTAssertTrue(nine < ten, "0.1.9 must be less than 0.1.10")
        XCTAssertFalse(ten < nine)
    }

    func test_equalityWithDifferingLengths() {
        XCTAssertTrue(SemanticVersion("1.0")! == SemanticVersion("1.0.0")!)
        XCTAssertFalse(SemanticVersion("1.0")! < SemanticVersion("1.0.0")!)
    }

    func test_rejectsGarbage() {
        XCTAssertNil(SemanticVersion(""))
        XCTAssertNil(SemanticVersion("v"))
        XCTAssertNil(SemanticVersion("1.a.3"))
        XCTAssertNil(SemanticVersion("1..3"))
        XCTAssertNil(SemanticVersion("beta"))
        XCTAssertNil(SemanticVersion("-1.0.0"))
    }
}

// MARK: - ReleaseChecker

@MainActor
final class ReleaseCheckerTests: XCTestCase {

    private var suiteName: String!
    private var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        suiteName = "test-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suiteName)
        defaults = nil
        suiteName = nil
        super.tearDown()
    }

    // MARK: fetcher → state

    func test_newerTag_flipsUpdateAvailable() async {
        let checker = ReleaseChecker(
            currentVersion: "0.1.4",
            fetcher: StubFetcher(result: .success("v0.1.5")),
            defaults: defaults
        )
        XCTAssertFalse(checker.updateAvailable)
        await checker.checkNow()
        XCTAssertTrue(checker.updateAvailable)
        XCTAssertEqual(checker.latestVersion, "v0.1.5")
    }

    func test_equalTag_leavesUpdateAvailableFalse() async {
        let checker = ReleaseChecker(
            currentVersion: "0.1.5",
            fetcher: StubFetcher(result: .success("v0.1.5")),
            defaults: defaults
        )
        await checker.checkNow()
        XCTAssertFalse(checker.updateAvailable)
    }

    func test_olderTag_leavesUpdateAvailableFalse() async {
        let checker = ReleaseChecker(
            currentVersion: "0.2.0",
            fetcher: StubFetcher(result: .success("v0.1.9")),
            defaults: defaults
        )
        await checker.checkNow()
        XCTAssertFalse(checker.updateAvailable)
    }

    func test_fetcherThrow_leavesPreviousStateIntact() async {
        // Seed a known-good state first.
        defaults.set("v0.2.0", forKey: ReleaseChecker.lastKnownLatestVersionKey)
        let checker = ReleaseChecker(
            currentVersion: "0.1.4",
            fetcher: StubFetcher(result: .failure(DummyError.network)),
            defaults: defaults
        )
        XCTAssertTrue(checker.updateAvailable, "cache seed should light the flag immediately")
        await checker.checkNow()
        // Fetch failed; cached state must not be wiped.
        XCTAssertTrue(checker.updateAvailable)
        XCTAssertEqual(checker.latestVersion, "v0.2.0")
    }

    func test_successfulFetch_writesCacheKey() async {
        let checker = ReleaseChecker(
            currentVersion: "0.1.4",
            fetcher: StubFetcher(result: .success("v0.1.5")),
            defaults: defaults
        )
        await checker.checkNow()
        XCTAssertEqual(
            defaults.string(forKey: ReleaseChecker.lastKnownLatestVersionKey),
            "v0.1.5"
        )
    }

    func test_freshInstanceSeedsFromCache_beforeAnyNetworkCall() {
        defaults.set("v9.9.9", forKey: ReleaseChecker.lastKnownLatestVersionKey)
        let checker = ReleaseChecker(
            currentVersion: "0.1.4",
            fetcher: StubFetcher(result: .failure(DummyError.network)),
            defaults: defaults
        )
        // No checkNow() call — everything the pill needs must come
        // from the cached tag alone.
        XCTAssertTrue(checker.updateAvailable)
        XCTAssertEqual(checker.latestVersion, "v9.9.9")
    }

    func test_unparseableTag_doesNotCrashAndLeavesFlagFalse() async {
        let checker = ReleaseChecker(
            currentVersion: "0.1.4",
            fetcher: StubFetcher(result: .success("not-a-version")),
            defaults: defaults
        )
        await checker.checkNow()
        XCTAssertFalse(checker.updateAvailable)
        // But we still cache the raw string so we don't re-spam the
        // user with the same bad response — next run seeds it, and
        // the flag stays false because we still can't parse it.
        XCTAssertEqual(
            defaults.string(forKey: ReleaseChecker.lastKnownLatestVersionKey),
            "not-a-version"
        )
    }

    // MARK: - Helpers

    private struct StubFetcher: ReleaseFetcher {
        let result: Result<String, Error>
        func fetchLatestTag() async throws -> String {
            try result.get()
        }
    }

    private enum DummyError: Error {
        case network
    }
}
