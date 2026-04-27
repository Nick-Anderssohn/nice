//
//  FileBrowserSortSettingsTests.swift
//  NiceUnitTests
//
//  Persistence + defaulting coverage for the file browser's sort
//  preference store. Each test runs against an isolated
//  `UserDefaults(suiteName:)` so state doesn't leak across cases.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class FileBrowserSortSettingsTests: XCTestCase {

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

    // MARK: - Defaults

    /// Fresh install: name-ascending. Matches today's behavior so an
    /// upgrade from a pre-sort version reads identically on first
    /// launch.
    func test_freshInstall_defaultsToNameAscending() {
        let s = FileBrowserSortSettings(defaults: defaults)
        XCTAssertEqual(s.criterion, .name)
        XCTAssertTrue(s.ascending)
    }

    // MARK: - Persistence

    func test_criterion_persists() {
        do {
            let s = FileBrowserSortSettings(defaults: defaults)
            s.criterion = .dateModified
        }
        let reloaded = FileBrowserSortSettings(defaults: defaults)
        XCTAssertEqual(reloaded.criterion, .dateModified)
    }

    func test_ascending_persists() {
        do {
            let s = FileBrowserSortSettings(defaults: defaults)
            s.ascending = false
        }
        let reloaded = FileBrowserSortSettings(defaults: defaults)
        XCTAssertFalse(reloaded.ascending)
    }

    /// Independence: one knob doesn't reset the other on change. Both
    /// have their own UserDefaults key, so flipping direction must
    /// not clobber the criterion choice.
    func test_directionFlip_doesNotResetCriterion() {
        let s = FileBrowserSortSettings(defaults: defaults)
        s.criterion = .dateModified
        s.ascending = false
        XCTAssertEqual(s.criterion, .dateModified)
        XCTAssertFalse(s.ascending)

        let reloaded = FileBrowserSortSettings(defaults: defaults)
        XCTAssertEqual(reloaded.criterion, .dateModified)
        XCTAssertFalse(reloaded.ascending)
    }

    // MARK: - Fallbacks

    /// Stored raw string that doesn't map to a known case (e.g. left
    /// over from a removed criterion in a future version) must fall
    /// back to `.name` rather than crash or carry a nil value.
    func test_unknownStoredCriterion_fallsBackToName() {
        defaults.set("size", forKey: FileBrowserSortSettings.criterionKey)
        let s = FileBrowserSortSettings(defaults: defaults)
        XCTAssertEqual(s.criterion, .name)
    }

    /// `bool(forKey:)` returns `false` for an unset key, which would
    /// silently flip a fresh install to descending. Guard the explicit
    /// `object(forKey:) as? Bool` path in `init`.
    func test_missingAscendingKey_defaultsToTrue() {
        // Set only the criterion; ascending key absent.
        defaults.set("dateModified", forKey: FileBrowserSortSettings.criterionKey)
        let s = FileBrowserSortSettings(defaults: defaults)
        XCTAssertTrue(s.ascending,
                      "An unset ascending key must default to true, not to bool(forKey:)'s implicit false.")
    }
}
