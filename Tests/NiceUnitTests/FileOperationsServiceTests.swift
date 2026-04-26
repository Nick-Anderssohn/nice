//
//  FileOperationsServiceTests.swift
//  NiceUnitTests
//
//  Real-filesystem coverage for `FileOperationsService` — the pure
//  worker behind the file-browser context menu's Copy / Cut / Paste /
//  Move to Trash. Each test stands up a fresh `nice-fileop-test-*`
//  temp dir and tears it down on exit so suites stay isolated.
//
//  Trash is exercised via a fake `Trasher` so the suite doesn't
//  depend on the test runner's user-Trash being writable, and so the
//  tests don't accumulate cruft in the user's actual Trash.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class FileOperationsServiceTests: XCTestCase {

    private var tempDir: URL!

    override func setUp() {
        super.setUp()
        tempDir = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-fileop-test-\(UUID().uuidString)",
                isDirectory: true
            )
        try? FileManager.default.createDirectory(
            at: tempDir, withIntermediateDirectories: true
        )
    }

    override func tearDown() {
        if let tempDir {
            try? FileManager.default.removeItem(at: tempDir)
        }
        tempDir = nil
        super.tearDown()
    }

    // MARK: - Copy

    func test_copy_intoEmptyDir_writesAllFiles() throws {
        let src = makeFile("a.txt", body: "alpha")
        let src2 = makeFile("b.txt", body: "beta")
        let dest = makeDir("dest")
        let service = FileOperationsService()

        let op = try service.copy(items: [src, src2], into: dest, origin: origin())

        XCTAssertTrue(fileExists(dest.appendingPathComponent("a.txt")))
        XCTAssertTrue(fileExists(dest.appendingPathComponent("b.txt")))
        // Sources still there.
        XCTAssertTrue(fileExists(src))
        XCTAssertTrue(fileExists(src2))
        if case let .copy(items, _) = op {
            XCTAssertEqual(items.count, 2)
        } else {
            XCTFail("expected .copy")
        }
    }

    func test_copy_recursivelyCopiesDirectory() throws {
        let folder = makeDir("folder")
        FileManager.default.createFile(
            atPath: folder.appendingPathComponent("inside.txt").path,
            contents: Data("hi".utf8)
        )
        let dest = makeDir("dest")
        let service = FileOperationsService()

        _ = try service.copy(items: [folder], into: dest, origin: origin())

        let copied = dest.appendingPathComponent("folder").appendingPathComponent("inside.txt")
        XCTAssertTrue(fileExists(copied))
    }

    func test_copy_collidingName_appendsCopySuffix() throws {
        let src = makeFile("foo.txt", body: "x")
        let dest = makeDir("dest")
        FileManager.default.createFile(
            atPath: dest.appendingPathComponent("foo.txt").path,
            contents: Data("existing".utf8)
        )
        let service = FileOperationsService()

        let op = try service.copy(items: [src], into: dest, origin: origin())

        XCTAssertTrue(fileExists(dest.appendingPathComponent("foo copy.txt")))
        if case let .copy(items, _) = op {
            XCTAssertEqual(items.first?.destination.lastPathComponent, "foo copy.txt")
        } else {
            XCTFail("expected .copy")
        }
    }

    func test_copy_collidingNameTwice_appendsCopy2() throws {
        let src = makeFile("foo.txt", body: "x")
        let dest = makeDir("dest")
        FileManager.default.createFile(
            atPath: dest.appendingPathComponent("foo.txt").path, contents: Data()
        )
        FileManager.default.createFile(
            atPath: dest.appendingPathComponent("foo copy.txt").path, contents: Data()
        )
        let service = FileOperationsService()

        _ = try service.copy(items: [src], into: dest, origin: origin())

        XCTAssertTrue(fileExists(dest.appendingPathComponent("foo copy 2.txt")))
    }

    func test_copy_collidingDirectory_appendsCopy() throws {
        let folder = makeDir("folder")
        let dest = makeDir("dest")
        try FileManager.default.createDirectory(
            at: dest.appendingPathComponent("folder", isDirectory: true),
            withIntermediateDirectories: false
        )
        let service = FileOperationsService()

        _ = try service.copy(items: [folder], into: dest, origin: origin())

        XCTAssertTrue(isDir(dest.appendingPathComponent("folder copy")))
    }

    func test_copy_recordIncludesAllSourceDestPairs() throws {
        let a = makeFile("a.txt")
        let b = makeFile("b.txt")
        let dest = makeDir("dest")

        let op = try FileOperationsService().copy(items: [a, b], into: dest, origin: origin())

        guard case let .copy(items, _) = op else { return XCTFail("expected .copy") }
        XCTAssertEqual(items.map { $0.source }, [a, b])
        XCTAssertEqual(
            items.map { $0.destination.lastPathComponent },
            ["a.txt", "b.txt"]
        )
    }

    // MARK: - Move

    func test_move_intoEmptyDir_relocatesFiles() throws {
        let src = makeFile("file.txt")
        let dest = makeDir("dest")

        _ = try FileOperationsService().move(items: [src], into: dest, origin: origin())

        XCTAssertFalse(fileExists(src))
        XCTAssertTrue(fileExists(dest.appendingPathComponent("file.txt")))
    }

    func test_move_collidingName_appendsCopySuffix() throws {
        let src = makeFile("file.txt")
        let dest = makeDir("dest")
        FileManager.default.createFile(
            atPath: dest.appendingPathComponent("file.txt").path,
            contents: Data("existing".utf8)
        )

        let op = try FileOperationsService().move(items: [src], into: dest, origin: origin())

        XCTAssertTrue(fileExists(dest.appendingPathComponent("file copy.txt")))
        if case let .move(items, _) = op {
            XCTAssertEqual(items.first?.destination.lastPathComponent, "file copy.txt")
        } else {
            XCTFail("expected .move")
        }
    }

    // MARK: - Trash

    func test_trash_movesItemToTrash_capturesNewURL() throws {
        let src = makeFile("delete-me.txt")
        let trashLocation = tempDir.appendingPathComponent("Trash", isDirectory: true)
        try FileManager.default.createDirectory(at: trashLocation, withIntermediateDirectories: true)
        let trasher = FakeTrasher(trashRoot: trashLocation)
        let service = FileOperationsService(trasher: trasher)

        let op = try service.trash(items: [src], origin: origin())

        if case let .trash(items, _) = op {
            XCTAssertEqual(items.count, 1)
            XCTAssertEqual(items.first?.original, src)
            XCTAssertTrue(fileExists(items.first!.trashed))
        } else {
            XCTFail("expected .trash")
        }
    }

    func test_trash_multipleItems_returnsRecordWithAllPairs() throws {
        let a = makeFile("a.txt")
        let b = makeFile("b.txt")
        let trashLocation = makeDir("Trash")
        let trasher = FakeTrasher(trashRoot: trashLocation)
        let service = FileOperationsService(trasher: trasher)

        let op = try service.trash(items: [a, b], origin: origin())

        if case let .trash(items, _) = op {
            XCTAssertEqual(items.count, 2)
            XCTAssertEqual(items.map { $0.original }, [a, b])
        } else {
            XCTFail("expected .trash")
        }
    }

    // MARK: - Inverse

    func test_apply_inverseOfCopy_deletesAllCopiedDests() throws {
        let src = makeFile("a.txt")
        let dest = makeDir("dest")
        let service = FileOperationsService()
        let op = try service.copy(items: [src], into: dest, origin: origin())

        _ = try service.undo(op)

        XCTAssertFalse(fileExists(dest.appendingPathComponent("a.txt")))
        // Source still there.
        XCTAssertTrue(fileExists(src))
    }

    func test_apply_inverseOfMove_movesItemsBackToOrigin() throws {
        let src = makeFile("a.txt", body: "hi")
        let dest = makeDir("dest")
        let service = FileOperationsService()
        let op = try service.move(items: [src], into: dest, origin: origin())
        XCTAssertFalse(fileExists(src))

        _ = try service.undo(op)

        XCTAssertTrue(fileExists(src))
        XCTAssertFalse(fileExists(dest.appendingPathComponent("a.txt")))
    }

    func test_apply_inverseOfTrash_restoresFromTrashURL() throws {
        let src = makeFile("a.txt", body: "hi")
        let trashLocation = makeDir("Trash")
        let trasher = FakeTrasher(trashRoot: trashLocation)
        let service = FileOperationsService(trasher: trasher)
        let op = try service.trash(items: [src], origin: origin())
        XCTAssertFalse(fileExists(src))

        _ = try service.undo(op)

        XCTAssertTrue(fileExists(src))
    }

    func test_apply_inverseOfTrash_missingTrashURL_throwsDriftError() throws {
        let src = makeFile("a.txt")
        let trashLocation = makeDir("Trash")
        let trasher = FakeTrasher(trashRoot: trashLocation)
        let service = FileOperationsService(trasher: trasher)
        let op = try service.trash(items: [src], origin: origin())
        // Empty the trash out from under us.
        if case let .trash(items, _) = op {
            try FileManager.default.removeItem(at: items[0].trashed)
        }

        XCTAssertThrowsError(try service.undo(op)) { err in
            guard let foe = err as? FileOperationError else {
                return XCTFail("expected FileOperationError, got \(err)")
            }
            if case .trashedItemMissing = foe { /* ok */ }
            else { XCTFail("expected .trashedItemMissing, got \(foe)") }
        }
    }

    // MARK: - Collision naming

    func test_nextAvailableName_skipsExistingNumberedSiblings() {
        let dest = makeDir("dest")
        let src = tempDir.appendingPathComponent("foo.txt")
        FileManager.default.createFile(atPath: dest.appendingPathComponent("foo.txt").path, contents: Data())
        FileManager.default.createFile(atPath: dest.appendingPathComponent("foo copy.txt").path, contents: Data())
        FileManager.default.createFile(atPath: dest.appendingPathComponent("foo copy 2.txt").path, contents: Data())

        let resolved = FileOperationsService().nextAvailableName(for: src, in: dest)

        XCTAssertEqual(resolved.lastPathComponent, "foo copy 3.txt")
    }

    func test_nextAvailableName_preservesExtension() {
        let dest = makeDir("dest")
        let src = tempDir.appendingPathComponent("archive.tar.gz")
        FileManager.default.createFile(
            atPath: dest.appendingPathComponent("archive.tar.gz").path, contents: Data()
        )

        let resolved = FileOperationsService().nextAvailableName(for: src, in: dest)

        // We split at the last dot, so `.gz` is the extension. The
        // base preserves `archive.tar`.
        XCTAssertEqual(resolved.lastPathComponent, "archive.tar copy.gz")
    }

    func test_nextAvailableName_directoryHasNoExtension() {
        let dest = makeDir("dest")
        let src = tempDir.appendingPathComponent("folder")
        try? FileManager.default.createDirectory(
            at: dest.appendingPathComponent("folder", isDirectory: true),
            withIntermediateDirectories: false
        )

        let resolved = FileOperationsService().nextAvailableName(for: src, in: dest)

        XCTAssertEqual(resolved.lastPathComponent, "folder copy")
    }

    func test_splitName_dotfileNoExtension() {
        let (base, ext) = FileOperationsService.splitNameAndExtension(".zshrc")
        XCTAssertEqual(base, ".zshrc")
        XCTAssertEqual(ext, "")
    }

    func test_splitName_dotfileWithExtension() {
        let (base, ext) = FileOperationsService.splitNameAndExtension(".zshrc.bak")
        XCTAssertEqual(base, ".zshrc")
        XCTAssertEqual(ext, "bak")
    }

    func test_splitName_normalFile() {
        let (base, ext) = FileOperationsService.splitNameAndExtension("foo.txt")
        XCTAssertEqual(base, "foo")
        XCTAssertEqual(ext, "txt")
    }

    // MARK: - Real Trasher smoke test

    /// Exercises the production `FileManagerTrasher` against the
    /// real `FileManager.trashItem` to catch regressions in the
    /// recycle path that fakes wouldn't surface. The test cleans
    /// up the resulting Trash entry to avoid littering the user's
    /// Trash. Skipped on hosts that don't allow Trash interaction.
    func test_fileManagerTrasher_movesFileToRealTrash_andReturnsResultingURL() throws {
        let src = makeFile("real-trash-smoke-\(UUID().uuidString).txt", body: "x")
        defer {
            // Belt-and-braces: if the trash failed and the file is
            // still there, remove it so the test doesn't leak.
            try? FileManager.default.removeItem(at: src)
        }

        let trasher = FileManagerTrasher()
        let trashed: [URL]
        do {
            trashed = try trasher.recycle([src])
        } catch {
            throw XCTSkip("FileManagerTrasher unavailable on this host: \(error)")
        }

        XCTAssertEqual(trashed.count, 1)
        XCTAssertFalse(FileManager.default.fileExists(atPath: src.path))
        let trashedURL = try XCTUnwrap(trashed.first)
        XCTAssertTrue(FileManager.default.fileExists(atPath: trashedURL.path))

        // Cleanup: remove the trashed file from the user's Trash so
        // we leave no trace.
        try? FileManager.default.removeItem(at: trashedURL)
    }

    // MARK: - Multi-source / batch behaviour

    func test_copy_twoSourcesWithSameName_distinctDestinations() throws {
        let a = makeFile("a/foo.txt", body: "aa")
        let b = makeFile("b/foo.txt", body: "bb")
        let dest = makeDir("dest")

        let op = try FileOperationsService().copy(
            items: [a, b], into: dest, origin: origin()
        )

        if case let .copy(items, _) = op {
            let names = items.map { $0.destination.lastPathComponent }
            XCTAssertEqual(Set(names), ["foo.txt", "foo copy.txt"],
                           "Two same-named sources in one batch must land at distinct names.")
        } else {
            XCTFail("expected .copy")
        }
    }

    func test_copy_partialFailureMidBatch_leavesEarlierCopiesInPlace_throws() throws {
        let a = makeFile("a.txt", body: "1")
        let missing = tempDir.appendingPathComponent("ghost.txt")
        let dest = makeDir("dest")
        let service = FileOperationsService()

        XCTAssertThrowsError(try service.copy(
            items: [a, missing], into: dest, origin: origin()
        )) { err in
            guard let foe = err as? FileOperationError,
                  case .sourceMissing = foe else {
                return XCTFail("expected .sourceMissing")
            }
        }

        XCTAssertTrue(fileExists(dest.appendingPathComponent("a.txt")),
                      "Earlier successful copies remain after a mid-batch drift failure.")
    }

    // MARK: - Unicode + spaces

    func test_copy_unicodeName_preservedThroughCollisionRename() throws {
        let src = makeFile("café 文件.txt", body: "data")
        let dest = makeDir("dest")
        FileManager.default.createFile(
            atPath: dest.appendingPathComponent("café 文件.txt").path,
            contents: Data("existing".utf8)
        )

        _ = try FileOperationsService().copy(items: [src], into: dest, origin: origin())

        XCTAssertTrue(fileExists(dest.appendingPathComponent("café 文件 copy.txt")))
    }

    func test_copy_pathWithSpaces_roundtrips() throws {
        let src = makeFile("a folder/with spaces.txt", body: "data")
        let dest = makeDir("dest")

        _ = try FileOperationsService().copy(items: [src], into: dest, origin: origin())

        XCTAssertTrue(fileExists(dest.appendingPathComponent("with spaces.txt")))
    }

    // MARK: - Helpers

    private func origin(tabId: String? = "tab-1") -> FileOperationOrigin {
        FileOperationOrigin(windowSessionId: "win-1", tabId: tabId)
    }

    @discardableResult
    private func makeFile(_ name: String, body: String = "") -> URL {
        let url = tempDir.appendingPathComponent(name)
        // Auto-create any intermediate parent dirs so callers can
        // pass nested paths like "a/foo.txt" without separate setup.
        try? FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        FileManager.default.createFile(atPath: url.path, contents: Data(body.utf8))
        return url
    }

    @discardableResult
    private func makeDir(_ name: String) -> URL {
        let url = tempDir.appendingPathComponent(name, isDirectory: true)
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    private func fileExists(_ url: URL) -> Bool {
        FileManager.default.fileExists(atPath: url.path)
    }

    private func isDir(_ url: URL) -> Bool {
        var d: ObjCBool = false
        return FileManager.default.fileExists(atPath: url.path, isDirectory: &d) && d.boolValue
    }
}

/// In-test stand-in for the real Trasher that "moves" items to a
/// subdir of the test's temp directory rather than the user's actual
/// Trash. Mirrors the contract: returns the new URLs in input order,
/// throws on unexpected failure.
final class FakeTrasher: Trasher {
    private let trashRoot: URL
    private let fileManager: FileManager

    init(trashRoot: URL, fileManager: FileManager = .default) {
        self.trashRoot = trashRoot
        self.fileManager = fileManager
    }

    func recycle(_ urls: [URL]) throws -> [URL] {
        var out: [URL] = []
        for url in urls {
            // Use a UUID prefix so two trashes of the same name don't
            // collide inside the fake trash.
            let dest = trashRoot
                .appendingPathComponent(UUID().uuidString, isDirectory: true)
            try fileManager.createDirectory(at: dest, withIntermediateDirectories: true)
            let target = dest.appendingPathComponent(url.lastPathComponent)
            try fileManager.moveItem(at: url, to: target)
            out.append(target)
        }
        return out
    }
}
