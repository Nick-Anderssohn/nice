//
//  FileBrowserDropResolverTests.swift
//  NiceUnitTests
//
//  Unit tests for `FileBrowserDropResolver` — the pure rules behind
//  the file-explorer's drag-to-folder behaviour. Tests cover the
//  cases the user cares about: folder-into-self / folder-into-
//  descendant rejection, parent-equals-dest no-op, multi-source
//  partial no-op, and Option / cross-volume → copy resolution.
//

import AppKit
import XCTest
@testable import Nice

@MainActor
final class FileBrowserDropResolverTests: XCTestCase {

    // MARK: - canDrop

    func test_canDrop_acceptsSibling() {
        let src = URL(fileURLWithPath: "/tmp/a/file.txt")
        let dest = URL(fileURLWithPath: "/tmp/b")
        XCTAssertTrue(FileBrowserDropResolver.canDrop(sources: [src], into: dest))
    }

    func test_canDrop_rejectsEmptySources() {
        let dest = URL(fileURLWithPath: "/tmp/b")
        XCTAssertFalse(FileBrowserDropResolver.canDrop(sources: [], into: dest))
    }

    func test_canDrop_rejectsSelfDrop() {
        let folder = URL(fileURLWithPath: "/tmp/a")
        XCTAssertFalse(
            FileBrowserDropResolver.canDrop(sources: [folder], into: folder)
        )
    }

    func test_canDrop_rejectsDescendantDrop() {
        let src = URL(fileURLWithPath: "/tmp/a")
        let dest = URL(fileURLWithPath: "/tmp/a/sub")
        XCTAssertFalse(
            FileBrowserDropResolver.canDrop(sources: [src], into: dest)
        )
    }

    func test_canDrop_rejectsDeepDescendantDrop() {
        let src = URL(fileURLWithPath: "/tmp/a")
        let dest = URL(fileURLWithPath: "/tmp/a/sub/inner")
        XCTAssertFalse(
            FileBrowserDropResolver.canDrop(sources: [src], into: dest)
        )
    }

    func test_canDrop_acceptsSiblingPrefixedFolder() {
        // `/tmp/abc` shares a prefix with `/tmp/a` but isn't a
        // descendant. The resolver must guard against substring-style
        // matches that ignore the path separator.
        let src = URL(fileURLWithPath: "/tmp/a")
        let dest = URL(fileURLWithPath: "/tmp/abc")
        XCTAssertTrue(
            FileBrowserDropResolver.canDrop(sources: [src], into: dest)
        )
    }

    func test_canDrop_rejectsParentEqualsDest() {
        let src = URL(fileURLWithPath: "/tmp/a/file.txt")
        let dest = URL(fileURLWithPath: "/tmp/a")
        XCTAssertFalse(
            FileBrowserDropResolver.canDrop(sources: [src], into: dest)
        )
    }

    func test_canDrop_acceptsBatchEvenIfOneSourceAlreadyHere() {
        // One source already lives in `dest`; another doesn't. Finder
        // accepts the drop and no-ops on the in-place item; we do too.
        let alreadyHere = URL(fileURLWithPath: "/tmp/a/file.txt")
        let needsMove = URL(fileURLWithPath: "/tmp/other/file2.txt")
        let dest = URL(fileURLWithPath: "/tmp/a")
        XCTAssertTrue(
            FileBrowserDropResolver.canDrop(
                sources: [alreadyHere, needsMove],
                into: dest
            )
        )
    }

    func test_canDrop_rejectsBatchWhenAllSourcesAlreadyHere() {
        let a = URL(fileURLWithPath: "/tmp/x/a.txt")
        let b = URL(fileURLWithPath: "/tmp/x/b.txt")
        let dest = URL(fileURLWithPath: "/tmp/x")
        XCTAssertFalse(
            FileBrowserDropResolver.canDrop(sources: [a, b], into: dest)
        )
    }

    func test_canDrop_rejectsBatchContainingSelfDrop() {
        // If any source would form a cycle (self / descendant), the
        // whole drop is rejected — partial-success would leave the
        // user with an unexpected mix.
        let valid = URL(fileURLWithPath: "/tmp/other/file.txt")
        let cycle = URL(fileURLWithPath: "/tmp/a")
        let dest = URL(fileURLWithPath: "/tmp/a/sub")
        XCTAssertFalse(
            FileBrowserDropResolver.canDrop(sources: [valid, cycle], into: dest)
        )
    }

    // MARK: - operation

    func test_operation_sameVolumeNoModifier_isMove() {
        let op = FileBrowserDropResolver.operation(
            modifierFlags: [],
            sameVolume: true
        )
        XCTAssertEqual(op, .move)
    }

    func test_operation_sameVolumeOptionHeld_isCopy() {
        let op = FileBrowserDropResolver.operation(
            modifierFlags: .option,
            sameVolume: true
        )
        XCTAssertEqual(op, .copy)
    }

    func test_operation_crossVolume_isCopyWithoutModifier() {
        let op = FileBrowserDropResolver.operation(
            modifierFlags: [],
            sameVolume: false
        )
        XCTAssertEqual(op, .copy)
    }

    func test_operation_crossVolume_optionHeld_stillCopy() {
        let op = FileBrowserDropResolver.operation(
            modifierFlags: .option,
            sameVolume: false
        )
        XCTAssertEqual(op, .copy)
    }

    func test_operation_ignoresOtherModifiers() {
        // Cmd / Shift / Control alone shouldn't flip move → copy on
        // a same-volume drop. Only Option does.
        for mod in [NSEvent.ModifierFlags.command, .shift, .control] {
            let op = FileBrowserDropResolver.operation(
                modifierFlags: mod,
                sameVolume: true
            )
            XCTAssertEqual(op, .move, "modifier \(mod) should not flip to copy")
        }
    }
}
