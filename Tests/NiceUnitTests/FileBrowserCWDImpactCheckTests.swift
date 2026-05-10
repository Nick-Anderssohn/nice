//
//  FileBrowserCWDImpactCheckTests.swift
//  NiceUnitTests
//
//  Coverage for the pure CWD-invalidation algorithm. Snapshot is
//  built directly in each test so the algorithm is exercised without
//  having to stand up windows.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class FileBrowserCWDImpactCheckTests: XCTestCase {

    func test_exactMatch_isAffected() {
        let snapshot = makeSnapshot(cwds: ["/Users/nick/Projects/nice"])
        let affected = FileBrowserCWDImpactCheck.affectedBy(
            rename: "/Users/nick/Projects/nice",
            snapshot: snapshot
        )
        XCTAssertEqual(affected.count, 1)
        XCTAssertEqual(affected.first?.cwd, "/Users/nick/Projects/nice")
    }

    func test_ancestor_isAffected() {
        let snapshot = makeSnapshot(cwds: ["/Users/nick/Projects/nice/src"])
        let affected = FileBrowserCWDImpactCheck.affectedBy(
            rename: "/Users/nick/Projects/nice",
            snapshot: snapshot
        )
        XCTAssertEqual(affected.count, 1)
    }

    func test_siblingPrefix_isNotAffected() {
        // /a/b should NOT match cwd=/a/bc — the trailing-slash guard
        // prevents the false-positive prefix match.
        let snapshot = makeSnapshot(cwds: ["/Users/nick/Projects/nicely"])
        let affected = FileBrowserCWDImpactCheck.affectedBy(
            rename: "/Users/nick/Projects/nice",
            snapshot: snapshot
        )
        XCTAssertTrue(affected.isEmpty)
    }

    func test_unrelated_isNotAffected() {
        let snapshot = makeSnapshot(cwds: ["/Users/nick/Documents", "/tmp"])
        let affected = FileBrowserCWDImpactCheck.affectedBy(
            rename: "/Users/nick/Projects/nice",
            snapshot: snapshot
        )
        XCTAssertTrue(affected.isEmpty)
    }

    func test_trailingSlashOnOldPath_isNormalized() {
        let snapshot = makeSnapshot(cwds: ["/Users/nick/Projects/nice/src"])
        let affected = FileBrowserCWDImpactCheck.affectedBy(
            rename: "/Users/nick/Projects/nice/",  // note trailing slash
            snapshot: snapshot
        )
        XCTAssertEqual(affected.count, 1)
    }

    func test_filesystemRoot_isExcluded() {
        // Renaming "/" is always a no-op (gated upstream); even if
        // the validator runs, we should return empty rather than
        // matching every entry on earth.
        let snapshot = makeSnapshot(cwds: ["/Users/nick", "/tmp", "/private"])
        let affected = FileBrowserCWDImpactCheck.affectedBy(
            rename: "/", snapshot: snapshot
        )
        XCTAssertTrue(affected.isEmpty)
    }

    func test_multipleEntries_allMatchingReturned() {
        // Two panes in the renamed folder, one outside, one in a
        // sibling. The sibling and outside should NOT be returned.
        let snapshot = PaneCWDSnapshot(entries: [
            ref(cwd: "/proj/foo", paneId: "p1", kind: .terminal),
            ref(cwd: "/proj/foo/sub", paneId: "p2", kind: .claude),
            ref(cwd: "/proj/foobar", paneId: "p3", kind: .terminal),
            ref(cwd: "/elsewhere", paneId: "p4", kind: .terminal),
        ])
        let affected = FileBrowserCWDImpactCheck.affectedBy(
            rename: "/proj/foo", snapshot: snapshot
        )
        let ids = Set(affected.map { $0.paneId })
        XCTAssertEqual(ids, ["p1", "p2"])
        // Kinds are preserved on the returned refs.
        XCTAssertEqual(
            affected.first { $0.paneId == "p2" }?.kind,
            .claude
        )
    }

    func test_normalizePath_stripsTrailingSlash() {
        XCTAssertEqual(
            FileBrowserCWDImpactCheck.normalizePath("/foo/bar/"),
            "/foo/bar"
        )
        // Root stays as `/` (no trailing-slash to strip).
        XCTAssertEqual(
            FileBrowserCWDImpactCheck.normalizePath("/"),
            "/"
        )
        // No-op for paths without a trailing slash.
        XCTAssertEqual(
            FileBrowserCWDImpactCheck.normalizePath("/foo/bar"),
            "/foo/bar"
        )
    }

    // MARK: - Helpers

    private func makeSnapshot(cwds: [String]) -> PaneCWDSnapshot {
        PaneCWDSnapshot(entries: cwds.enumerated().map { i, cwd in
            ref(cwd: cwd, paneId: "p\(i)", kind: .terminal)
        })
    }

    private func ref(
        cwd: String,
        paneId: String,
        kind: PaneKind,
        windowSessionId: String = "win-1",
        tabId: String = "tab-1"
    ) -> PaneCWDRef {
        PaneCWDRef(
            windowSessionId: windowSessionId,
            tabId: tabId,
            paneId: paneId,
            kind: kind,
            cwd: cwd
        )
    }
}
