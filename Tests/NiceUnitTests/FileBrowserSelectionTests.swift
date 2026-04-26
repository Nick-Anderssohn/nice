//
//  FileBrowserSelectionTests.swift
//  NiceUnitTests
//
//  Coverage for the multi-row selection model that backs the file
//  browser sidebar's right-click-on-selection behaviour. The model
//  is pure logic — no SwiftUI / filesystem involvement — so the
//  tests stand it up directly with hardcoded path strings.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class FileBrowserSelectionTests: XCTestCase {

    // MARK: - replace

    func test_replace_setsExactly() {
        let s = FileBrowserSelection()
        s.replace(with: ["/a", "/b"])

        XCTAssertEqual(s.selectedPaths, ["/a", "/b"])
        XCTAssertEqual(s.lastClickedPath, "/b")
    }

    func test_replace_explicitAnchor_overridesDefault() {
        let s = FileBrowserSelection()
        s.replace(with: ["/a", "/b"], anchor: "/a")

        XCTAssertEqual(s.lastClickedPath, "/a")
    }

    // MARK: - toggle

    func test_toggle_addsAbsentPath() {
        let s = FileBrowserSelection()
        s.replace(with: ["/a"])

        s.toggle("/b")

        XCTAssertEqual(s.selectedPaths, ["/a", "/b"])
        XCTAssertEqual(s.lastClickedPath, "/b")
    }

    func test_toggle_removesPresentPath() {
        let s = FileBrowserSelection()
        s.replace(with: ["/a", "/b"])

        s.toggle("/b")

        XCTAssertEqual(s.selectedPaths, ["/a"])
        XCTAssertEqual(s.lastClickedPath, "/b",
                       "anchor still moves to the toggled row, even on remove")
    }

    // MARK: - extend

    func test_extend_inclusiveBetweenLastAndCurrent() {
        let order = ["/a", "/b", "/c", "/d", "/e"]
        let s = FileBrowserSelection()
        s.replace(with: ["/b"])

        s.extend(through: "/d", visibleOrder: order)

        XCTAssertEqual(s.selectedPaths, ["/b", "/c", "/d"])
        XCTAssertEqual(s.lastClickedPath, "/b",
                       "shift-extend must not move the anchor")
    }

    func test_extend_currentBeforeLastReversesRange() {
        let order = ["/a", "/b", "/c", "/d", "/e"]
        let s = FileBrowserSelection()
        s.replace(with: ["/d"])

        s.extend(through: "/b", visibleOrder: order)

        XCTAssertEqual(s.selectedPaths, ["/b", "/c", "/d"])
    }

    func test_extend_emptyAnchor_treatsAsReplace() {
        let s = FileBrowserSelection()
        // No prior click; lastClickedPath is nil.

        s.extend(through: "/c", visibleOrder: ["/a", "/b", "/c"])

        XCTAssertEqual(s.selectedPaths, ["/c"])
        XCTAssertEqual(s.lastClickedPath, "/c")
    }

    func test_extend_targetMissingFromOrder_fallsBackToReplace() {
        let order = ["/a", "/b"]
        let s = FileBrowserSelection()
        s.replace(with: ["/a"])

        s.extend(through: "/c", visibleOrder: order)

        XCTAssertEqual(s.selectedPaths, ["/c"])
    }

    // MARK: - selectionPaths(forRightClickOn:)

    func test_rightClick_insideSelection_returnsAll() {
        let s = FileBrowserSelection()
        s.replace(with: ["/a", "/b", "/c"])

        let paths = s.selectionPaths(forRightClickOn: "/b")

        XCTAssertEqual(Set(paths), ["/a", "/b", "/c"])
        // selection unchanged
        XCTAssertEqual(s.selectedPaths, ["/a", "/b", "/c"])
    }

    func test_rightClick_outsideSelection_replacesAndReturnsOne() {
        let s = FileBrowserSelection()
        s.replace(with: ["/a", "/b"])

        let paths = s.selectionPaths(forRightClickOn: "/c")

        XCTAssertEqual(paths, ["/c"])
        XCTAssertEqual(s.selectedPaths, ["/c"])
    }

    func test_rightClick_emptySelection_replacesAndReturnsOne() {
        let s = FileBrowserSelection()

        let paths = s.selectionPaths(forRightClickOn: "/x")

        XCTAssertEqual(paths, ["/x"])
        XCTAssertEqual(s.selectedPaths, ["/x"])
    }

    // MARK: - clear

    func test_clear_resetsBoth() {
        let s = FileBrowserSelection()
        s.replace(with: ["/a", "/b"])

        s.clear()

        XCTAssertTrue(s.selectedPaths.isEmpty)
        XCTAssertNil(s.lastClickedPath)
    }
}
