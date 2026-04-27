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

    /// Name-descending flips both buckets (dirs and files) but keeps
    /// dirs above files. Same pivot rule, opposite intra-bucket order.
    func test_entries_nameDescending_reversesEachBucket() throws {
        try touchFile("regular.txt")
        try touchFile("a_file.swift")
        try makeDir("Z_dir")
        try makeDir("M_dir")

        let names = FileBrowserListing
            .entries(at: tempDir, showHidden: true, criterion: .name, ascending: false)
            .map { $0.lastPathComponent }

        XCTAssertEqual(names, ["Z_dir", "M_dir", "regular.txt", "a_file.swift"])
    }

    /// Date-modified ascending = oldest first. Files written earlier
    /// in test setup land above files written later, regardless of
    /// alphabetical order. Dirs-first still holds.
    func test_entries_dateModifiedAscending_oldestFirstWithinBucket() throws {
        let oldFile = try touchFile("z_old.txt")
        let newFile = try touchFile("a_new.txt")
        try setModificationDate(.init(timeIntervalSince1970: 1_000_000), on: oldFile)
        try setModificationDate(.init(timeIntervalSince1970: 2_000_000), on: newFile)

        let names = FileBrowserListing
            .entries(at: tempDir, showHidden: true, criterion: .dateModified, ascending: true)
            .map { $0.lastPathComponent }

        XCTAssertEqual(names, ["z_old.txt", "a_new.txt"],
                       "Date-asc must put older mtime first even when alpha-order disagrees.")
    }

    /// Date-modified descending = newest first.
    func test_entries_dateModifiedDescending_newestFirstWithinBucket() throws {
        let oldFile = try touchFile("a_old.txt")
        let newFile = try touchFile("z_new.txt")
        try setModificationDate(.init(timeIntervalSince1970: 1_000_000), on: oldFile)
        try setModificationDate(.init(timeIntervalSince1970: 2_000_000), on: newFile)

        let names = FileBrowserListing
            .entries(at: tempDir, showHidden: true, criterion: .dateModified, ascending: false)
            .map { $0.lastPathComponent }

        XCTAssertEqual(names, ["z_new.txt", "a_old.txt"],
                       "Date-desc must put newer mtime first.")
    }

    /// When two entries share an mtime (common after a `git
    /// checkout`), the order must be deterministic. Tie-break is
    /// always A→Z by name regardless of direction so two same-mtime
    /// neighbors don't swap when the user toggles direction.
    func test_entries_dateModifiedTiebreak_byNameAscendingEvenWhenDescending() throws {
        let aFile = try touchFile("a.txt")
        let bFile = try touchFile("b.txt")
        let sameDate = Date(timeIntervalSince1970: 1_500_000)
        try setModificationDate(sameDate, on: aFile)
        try setModificationDate(sameDate, on: bFile)

        let asc = FileBrowserListing
            .entries(at: tempDir, showHidden: true, criterion: .dateModified, ascending: true)
            .map { $0.lastPathComponent }
        let desc = FileBrowserListing
            .entries(at: tempDir, showHidden: true, criterion: .dateModified, ascending: false)
            .map { $0.lastPathComponent }

        XCTAssertEqual(asc, ["a.txt", "b.txt"])
        XCTAssertEqual(desc, ["a.txt", "b.txt"],
                       "Same-mtime entries must keep stable alpha order under both directions.")
    }

    /// Dirs-first invariant must hold regardless of criterion. Even
    /// if the file's mtime is newer than the dir's, the dir comes
    /// first under date-desc — sort applies *within* each bucket.
    func test_entries_dirsAlwaysAboveFiles_evenUnderDateModified() throws {
        let dir = tempDir.appendingPathComponent("oldDir", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: false)
        let file = try touchFile("newFile.txt")
        try setModificationDate(.init(timeIntervalSince1970: 1_000_000), on: dir)
        try setModificationDate(.init(timeIntervalSince1970: 2_000_000), on: file)

        let names = FileBrowserListing
            .entries(at: tempDir, showHidden: true, criterion: .dateModified, ascending: false)
            .map { $0.lastPathComponent }

        XCTAssertEqual(names.first, "oldDir",
                       "Dirs-first must hold even when a file's mtime is newer than the dir's.")
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

    // MARK: - visibleOrder

    /// Standardise a path the same way `URL.standardizedFileURL.path`
    /// would, so test inputs match what `contentsOfDirectory` returns
    /// (macOS canonicalises `/var` → `/private/var` on tmp paths).
    private func canonicalPath(_ url: URL) -> String {
        url.standardizedFileURL.resolvingSymlinksInPath().path
    }

    private var canonicalRoot: String { canonicalPath(tempDir) }

    func test_visibleOrder_flatRoot_listsRootThenChildren() throws {
        try touchFile("a.txt")
        try touchFile("b.txt")
        try makeDir("Z_dir")

        let order = FileBrowserListing.visibleOrder(
            rootPath: canonicalRoot,
            expandedPaths: [canonicalRoot],
            showHidden: true
        )

        XCTAssertEqual(order.first, canonicalRoot)
        XCTAssertEqual(order.dropFirst().map { ($0 as NSString).lastPathComponent },
                       ["Z_dir", "a.txt", "b.txt"])
    }

    func test_visibleOrder_collapsedSubdir_omitsItsChildren() throws {
        try makeDir("subdir")
        let inner = tempDir.appendingPathComponent("subdir/inner.txt")
        FileManager.default.createFile(atPath: inner.path, contents: Data())

        // Subdir is NOT in the expanded set.
        let order = FileBrowserListing.visibleOrder(
            rootPath: canonicalRoot,
            expandedPaths: [canonicalRoot],
            showHidden: true
        )

        let innerCanonical = canonicalPath(inner)
        XCTAssertFalse(order.contains(innerCanonical),
                       "Children of a collapsed directory must not appear in the visible order.")
    }

    func test_visibleOrder_expandedSubdir_includesChildrenInDirsFirstOrder() throws {
        try makeDir("subdir")
        let subdir = tempDir.appendingPathComponent("subdir")
        FileManager.default.createFile(
            atPath: subdir.appendingPathComponent("zfile.txt").path, contents: Data()
        )
        try FileManager.default.createDirectory(
            at: subdir.appendingPathComponent("anest", isDirectory: true),
            withIntermediateDirectories: false
        )

        // Use the path `entries(at:)` actually produces for subdir
        // — that's the same path the production tree stores in
        // `expandedPaths` when the user clicks the disclosure
        // triangle. Avoids any /var vs /private/var canonicalisation
        // mismatch in the test setup.
        let rootURL = URL(fileURLWithPath: tempDir.path)
        let subdirChild = FileBrowserListing.entries(at: rootURL, showHidden: true)
            .first { $0.lastPathComponent == "subdir" }
        let subdirPath = try XCTUnwrap(subdirChild?.path)

        let order = FileBrowserListing.visibleOrder(
            rootPath: tempDir.path,
            expandedPaths: [tempDir.path, subdirPath],
            showHidden: true
        )

        let names = order.map { ($0 as NSString).lastPathComponent }
        // Root, subdir, then dirs-first within subdir.
        XCTAssertEqual(Array(names.prefix(4)),
                       [tempDir.lastPathComponent, "subdir", "anest", "zfile.txt"])
    }

    func test_visibleOrder_missingRoot_returnsEmpty() {
        let nonexistent = tempDir.appendingPathComponent("does-not-exist", isDirectory: true)
        let order = FileBrowserListing.visibleOrder(
            rootPath: nonexistent.path,
            expandedPaths: [nonexistent.path],
            showHidden: true
        )
        XCTAssertTrue(order.isEmpty)
    }

    func test_visibleOrder_respectsShowHidden() throws {
        try touchFile(".hidden.txt")
        try touchFile("visible.txt")

        let withHidden = FileBrowserListing.visibleOrder(
            rootPath: canonicalRoot,
            expandedPaths: [canonicalRoot],
            showHidden: true
        )
        let withoutHidden = FileBrowserListing.visibleOrder(
            rootPath: canonicalRoot,
            expandedPaths: [canonicalRoot],
            showHidden: false
        )

        XCTAssertTrue(withHidden.contains { ($0 as NSString).lastPathComponent == ".hidden.txt" })
        XCTAssertFalse(withoutHidden.contains { ($0 as NSString).lastPathComponent == ".hidden.txt" })
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

    /// Force a known modification date so date-sort tests don't race
    /// the wall clock. The default `touchFile` helper creates files
    /// fast enough that consecutive calls share the same second on
    /// some filesystems, which would let the test pass spuriously.
    private func setModificationDate(_ date: Date, on url: URL) throws {
        try FileManager.default.setAttributes(
            [.modificationDate: date],
            ofItemAtPath: url.path
        )
    }
}
