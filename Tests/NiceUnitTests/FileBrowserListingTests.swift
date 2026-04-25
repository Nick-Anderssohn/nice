//
//  FileBrowserListingTests.swift
//  NiceUnitTests
//
//  Real-filesystem coverage for `FileBrowserListing.entries(at:showHidden:)`
//  — the filter + sort + IO that the file browser's `FileTreeRow`
//  uses to populate each expanded directory. Asserts against a
//  fresh temp tree so the contract holds for what the user actually
//  sees on screen, not just for in-memory mocks.
//

import Foundation
import XCTest
@testable import Nice

final class FileBrowserListingTests: XCTestCase {

    private var tempDir: URL!

    override func setUpWithError() throws {
        tempDir = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-listing-test-\(UUID().uuidString)",
                isDirectory: true
            )
        try FileManager.default.createDirectory(
            at: tempDir, withIntermediateDirectories: true
        )
    }

    override func tearDownWithError() throws {
        if let tempDir {
            try? FileManager.default.removeItem(at: tempDir)
        }
        tempDir = nil
    }

    // MARK: - Sort order

    /// Dirs come before files; within each bucket, case-insensitive
    /// alphabetical so M_dir < Z_dir and a_file < B_file regardless
    /// of how the filesystem returns them.
    func test_entries_sortsDirsFirstThenAlphaCaseInsensitive() throws {
        try touchFile("regular.txt")
        try touchFile("a_file.swift")
        try makeDir("Z_dir")
        try makeDir("M_dir")

        let names = FileBrowserListing
            .entries(at: tempDir, showHidden: true)
            .map { $0.lastPathComponent }

        XCTAssertEqual(names, ["M_dir", "Z_dir", "a_file.swift", "regular.txt"])
    }

    // MARK: - Hidden filter

    func test_entries_showHiddenFalse_filtersDotPrefixedNames() throws {
        try touchFile(".hidden.txt")
        try touchFile("visible.txt")
        try makeDir(".git")
        try makeDir("Sources")

        let names = FileBrowserListing
            .entries(at: tempDir, showHidden: false)
            .map { $0.lastPathComponent }

        XCTAssertEqual(names, ["Sources", "visible.txt"],
                       "Dotfiles must be filtered when showHidden is false.")
    }

    func test_entries_showHiddenTrue_includesEverything() throws {
        try touchFile(".hidden.txt")
        try touchFile("visible.txt")
        try makeDir(".git")
        try makeDir("Sources")

        let names = FileBrowserListing
            .entries(at: tempDir, showHidden: true)
            .map { $0.lastPathComponent }

        XCTAssertEqual(names, [".git", "Sources", ".hidden.txt", "visible.txt"],
                       "showHidden=true must include dot-prefixed names.")
    }

    /// Files Finder-flagged as invisible (`isHidden` resource value)
    /// must filter even if they don't have a dot prefix. Catches a
    /// regression where the filter only checks the dot-prefix.
    func test_entries_showHiddenFalse_filtersIsHiddenFlaggedFiles() throws {
        let invisible = try touchFile("plainName.txt")
        var values = URLResourceValues()
        values.isHidden = true
        var url = invisible
        try url.setResourceValues(values)
        try touchFile("regular.txt")

        let names = FileBrowserListing
            .entries(at: tempDir, showHidden: false)
            .map { $0.lastPathComponent }

        XCTAssertEqual(names, ["regular.txt"],
                       "Files with the isHidden resource flag must be filtered even without a dot prefix.")
    }

    // MARK: - Fallback

    func test_entries_missingPath_returnsEmpty() {
        let nonexistent = tempDir.appendingPathComponent("does-not-exist", isDirectory: true)

        let result = FileBrowserListing.entries(at: nonexistent, showHidden: true)

        XCTAssertTrue(result.isEmpty,
                      "A missing path must return [] rather than throw — a deeper row that vanishes mid-render shouldn't take the tree down.")
    }

    func test_entries_emptyDirectory_returnsEmpty() {
        // tempDir is already empty.
        let result = FileBrowserListing.entries(at: tempDir, showHidden: true)
        XCTAssertTrue(result.isEmpty)
    }

    // MARK: - Helpers

    @discardableResult
    private func touchFile(_ name: String) throws -> URL {
        let url = tempDir.appendingPathComponent(name)
        FileManager.default.createFile(atPath: url.path, contents: Data())
        return url
    }

    private func makeDir(_ name: String) throws {
        try FileManager.default.createDirectory(
            at: tempDir.appendingPathComponent(name, isDirectory: true),
            withIntermediateDirectories: false
        )
    }
}
