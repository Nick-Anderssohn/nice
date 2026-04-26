//
//  OpenWithProviderTests.swift
//  NiceUnitTests
//
//  Coverage for `OpenWithProvider` ordering, deduping, and default-
//  app placement logic. Uses the injectable `Lookups` struct to
//  bypass Launch Services so the suite is deterministic on whatever
//  apps the test runner happens to have installed.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class OpenWithProviderTests: XCTestCase {

    private let target = URL(fileURLWithPath: "/tmp/file.swift")
    private let xcode = URL(fileURLWithPath: "/Applications/Xcode.app")
    private let textEdit = URL(fileURLWithPath: "/Applications/TextEdit.app")
    private let bbedit = URL(fileURLWithPath: "/Applications/BBEdit.app")

    func test_entries_defaultAppFirst() {
        let lookups = OpenWithProvider.Lookups(
            allAppsForURL: { _ in [self.textEdit, self.xcode, self.bbedit] },
            defaultAppForURL: { _ in self.xcode },
            displayName: { url in url.deletingPathExtension().lastPathComponent },
            icon: { _ in nil }
        )

        let entries = OpenWithProvider(lookups: lookups).entries(for: target)

        XCTAssertEqual(entries.first?.appURL, xcode)
        XCTAssertEqual(entries.first?.isDefault, true)
    }

    func test_entries_alphabetisedAfterDefault() {
        let lookups = OpenWithProvider.Lookups(
            allAppsForURL: { _ in [self.textEdit, self.xcode, self.bbedit] },
            defaultAppForURL: { _ in self.xcode },
            displayName: { url in url.deletingPathExtension().lastPathComponent },
            icon: { _ in nil }
        )

        let entries = OpenWithProvider(lookups: lookups).entries(for: target)

        // After the default (Xcode), the rest are alphabetical:
        // BBEdit, TextEdit.
        XCTAssertEqual(entries.dropFirst().map { $0.appURL }, [bbedit, textEdit])
    }

    func test_entries_dedupesByBundlePath() {
        let dup = URL(fileURLWithPath: "/Applications/Xcode.app")
        let lookups = OpenWithProvider.Lookups(
            allAppsForURL: { _ in [self.xcode, dup, self.textEdit] },
            defaultAppForURL: { _ in nil },
            displayName: { url in url.deletingPathExtension().lastPathComponent },
            icon: { _ in nil }
        )

        let entries = OpenWithProvider(lookups: lookups).entries(for: target)

        XCTAssertEqual(entries.count, 2)
    }

    func test_entries_emptyForUnknownType_returnsEmpty() {
        let lookups = OpenWithProvider.Lookups(
            allAppsForURL: { _ in [] },
            defaultAppForURL: { _ in nil },
            displayName: { _ in "" },
            icon: { _ in nil }
        )

        let entries = OpenWithProvider(lookups: lookups).entries(for: target)

        XCTAssertTrue(entries.isEmpty)
    }

    func test_entries_singleAppThatIsDefault_returnsOneEntryWithIsDefaultTrue() {
        let lookups = OpenWithProvider.Lookups(
            allAppsForURL: { _ in [self.xcode] },
            defaultAppForURL: { _ in self.xcode },
            displayName: { url in url.deletingPathExtension().lastPathComponent },
            icon: { _ in nil }
        )

        let entries = OpenWithProvider(lookups: lookups).entries(for: target)

        XCTAssertEqual(entries.count, 1, "Default-and-only app must not duplicate.")
        XCTAssertEqual(entries.first?.appURL, xcode)
        XCTAssertEqual(entries.first?.isDefault, true)
    }

    func test_entries_dedupesByBundlePath_evenWhenDefaultAppDuplicated() {
        let dupXcode = URL(fileURLWithPath: "/Applications/Xcode.app")
        let lookups = OpenWithProvider.Lookups(
            allAppsForURL: { _ in [self.xcode, dupXcode, self.textEdit] },
            defaultAppForURL: { _ in self.xcode },
            displayName: { url in url.deletingPathExtension().lastPathComponent },
            icon: { _ in nil }
        )

        let entries = OpenWithProvider(lookups: lookups).entries(for: target)

        XCTAssertEqual(entries.count, 2)
        XCTAssertEqual(entries.first?.isDefault, true)
        XCTAssertEqual(entries.dropFirst().first?.appURL, textEdit)
    }

    func test_entries_defaultAppNotInList_isStillIncluded() {
        // `defaultAppForURL` returns Xcode, but `allAppsForURL`
        // doesn't list it (Launch Services edge case). We must
        // still surface the default at index 0.
        let lookups = OpenWithProvider.Lookups(
            allAppsForURL: { _ in [self.textEdit] },
            defaultAppForURL: { _ in self.xcode },
            displayName: { url in url.deletingPathExtension().lastPathComponent },
            icon: { _ in nil }
        )

        let entries = OpenWithProvider(lookups: lookups).entries(for: target)

        XCTAssertEqual(entries.first?.appURL, xcode)
        XCTAssertEqual(entries.first?.isDefault, true)
        XCTAssertEqual(entries.dropFirst().map { $0.appURL }, [textEdit])
    }
}
