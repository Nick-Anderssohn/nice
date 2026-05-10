//
//  FileBrowserRenameValidatorTests.swift
//  NiceUnitTests
//
//  Coverage for the pure rename validator. Each `validate(...)` case
//  is pinned with a temp-directory fixture so the sibling-collision
//  branch exercises real `FileManager` semantics.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class FileBrowserRenameValidatorTests: XCTestCase {

    private var tmpDir: URL!

    override func setUp() {
        super.setUp()
        // One unique temp dir per test so collision-check fixtures
        // don't pollute each other.
        tmpDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("rename-tests-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(
            at: tmpDir, withIntermediateDirectories: true
        )
    }

    override func tearDown() {
        if let tmpDir { try? FileManager.default.removeItem(at: tmpDir) }
        super.tearDown()
    }

    // MARK: - canRename

    func test_canRename_isFalse_forFilesystemRoot() {
        XCTAssertFalse(FileBrowserRenameValidator.canRename(
            URL(fileURLWithPath: "/")
        ))
    }

    func test_canRename_isTrue_forAnythingElse() {
        XCTAssertTrue(FileBrowserRenameValidator.canRename(
            URL(fileURLWithPath: "/Users/nick/Projects/foo.txt")
        ))
        XCTAssertTrue(FileBrowserRenameValidator.canRename(
            URL(fileURLWithPath: "/private/etc")
        ))
    }

    // MARK: - validate

    func test_validate_emptyDraft_returnsEmpty() {
        let url = seedFile("foo.txt")
        XCTAssertEqual(
            FileBrowserRenameValidator.validate(originalURL: url, draft: "", fileManager: .default),
            .empty
        )
        // Whitespace-only also empty.
        XCTAssertEqual(
            FileBrowserRenameValidator.validate(originalURL: url, draft: "   ", fileManager: .default),
            .empty
        )
    }

    func test_validate_unchangedDraft_returnsUnchanged() {
        let url = seedFile("foo.txt")
        XCTAssertEqual(
            FileBrowserRenameValidator.validate(originalURL: url, draft: "foo.txt", fileManager: .default),
            .unchanged
        )
        // Trailing whitespace is trimmed.
        XCTAssertEqual(
            FileBrowserRenameValidator.validate(originalURL: url, draft: " foo.txt ", fileManager: .default),
            .unchanged
        )
    }

    func test_validate_draftWithSlash_returnsContainsSlash() {
        let url = seedFile("foo.txt")
        XCTAssertEqual(
            FileBrowserRenameValidator.validate(originalURL: url, draft: "bar/baz.txt", fileManager: .default),
            .containsSlash
        )
    }

    func test_validate_draftWithColon_returnsContainsSlash() {
        // We treat `:` the same as `/` since both are illegal in a
        // single path component on macOS.
        let url = seedFile("foo.txt")
        XCTAssertEqual(
            FileBrowserRenameValidator.validate(originalURL: url, draft: "bar:baz.txt", fileManager: .default),
            .containsSlash
        )
    }

    func test_validate_draftCollidesWithSibling_returnsWouldCollide() {
        let url = seedFile("foo.txt")
        _ = seedFile("bar.txt")  // pre-existing sibling
        let result = FileBrowserRenameValidator.validate(
            originalURL: url, draft: "bar.txt", fileManager: .default
        )
        guard case let .wouldCollide(candidate) = result else {
            XCTFail("expected .wouldCollide, got \(result)"); return
        }
        XCTAssertEqual(candidate.lastPathComponent, "bar.txt")
        XCTAssertEqual(candidate.deletingLastPathComponent().path, tmpDir.path)
    }

    func test_validate_filesystemRoot_returnsIsFilesystemRoot() {
        // Defense-in-depth: the trigger gate should already block,
        // but if a call ever slips through the validator catches it.
        let result = FileBrowserRenameValidator.validate(
            originalURL: URL(fileURLWithPath: "/"),
            draft: "newroot",
            fileManager: .default
        )
        XCTAssertEqual(result, .isFilesystemRoot)
    }

    func test_validate_okDraft_returnsOkWithDestinationURL() {
        let url = seedFile("foo.txt")
        let result = FileBrowserRenameValidator.validate(
            originalURL: url, draft: "renamed.txt", fileManager: .default
        )
        guard case let .ok(destination) = result else {
            XCTFail("expected .ok, got \(result)"); return
        }
        XCTAssertEqual(destination.lastPathComponent, "renamed.txt")
        XCTAssertEqual(destination.deletingLastPathComponent().path, tmpDir.path)
    }

    // MARK: - isExtensionChange

    func test_isExtensionChange_extensionDiffers_isTrue() {
        XCTAssertTrue(FileBrowserRenameValidator.isExtensionChange(
            originalName: "foo.txt", newName: "foo.md"
        ))
    }

    func test_isExtensionChange_basenameOnly_isFalse() {
        XCTAssertFalse(FileBrowserRenameValidator.isExtensionChange(
            originalName: "foo.txt", newName: "bar.txt"
        ))
    }

    func test_isExtensionChange_addedOrRemovedExtension_isTrue() {
        XCTAssertTrue(FileBrowserRenameValidator.isExtensionChange(
            originalName: "foo", newName: "foo.txt"
        ))
        XCTAssertTrue(FileBrowserRenameValidator.isExtensionChange(
            originalName: "foo.txt", newName: "foo"
        ))
    }

    func test_isExtensionChange_dotfileToDotfileWithExt_isTrue() {
        // ".zshrc" has no extension (whole name is base); ".zshrc.bak"
        // has extension "bak". So this is an extension change.
        XCTAssertTrue(FileBrowserRenameValidator.isExtensionChange(
            originalName: ".zshrc", newName: ".zshrc.bak"
        ))
    }

    func test_isExtensionChange_dotfileRenameWithinDotfile_isFalse() {
        // ".zshrc" → ".gitignore": both have no extension, just
        // different basenames. Not an extension change.
        XCTAssertFalse(FileBrowserRenameValidator.isExtensionChange(
            originalName: ".zshrc", newName: ".gitignore"
        ))
    }

    // MARK: - Helpers

    @discardableResult
    private func seedFile(_ name: String) -> URL {
        let url = tmpDir.appendingPathComponent(name)
        FileManager.default.createFile(atPath: url.path, contents: Data())
        return url
    }
}
