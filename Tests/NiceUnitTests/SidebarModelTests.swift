//
//  SidebarModelTests.swift
//  NiceUnitTests
//
//  Direct coverage for SidebarModel's three toggle paths and the
//  peek-clearing path. The fields are plain `var`s so the tests
//  read/write directly; no AppState is required.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class SidebarModelTests: XCTestCase {

    // MARK: - toggleSidebar

    func test_toggleSidebar_flipsCollapsed() {
        let s = SidebarModel(initialCollapsed: false, initialMode: .tabs)
        s.toggleSidebar()
        XCTAssertTrue(s.sidebarCollapsed)
        s.toggleSidebar()
        XCTAssertFalse(s.sidebarCollapsed)
    }

    func test_toggleSidebar_doesNotChangeMode() {
        let s = SidebarModel(initialCollapsed: false, initialMode: .files)
        s.toggleSidebar()
        XCTAssertEqual(s.sidebarMode, .files)
    }

    // MARK: - toggleSidebarMode

    func test_toggleSidebarMode_tabsToFiles() {
        let s = SidebarModel(initialCollapsed: false, initialMode: .tabs)
        s.toggleSidebarMode()
        XCTAssertEqual(s.sidebarMode, .files)
    }

    func test_toggleSidebarMode_filesToTabs() {
        let s = SidebarModel(initialCollapsed: false, initialMode: .files)
        s.toggleSidebarMode()
        XCTAssertEqual(s.sidebarMode, .tabs)
    }

    func test_toggleSidebarMode_doesNotChangeCollapsed() {
        let s = SidebarModel(initialCollapsed: true, initialMode: .tabs)
        s.toggleSidebarMode()
        XCTAssertTrue(s.sidebarCollapsed,
                      "Mode toggle must not change the collapsed flag.")
    }

    // MARK: - endSidebarPeek

    func test_endSidebarPeek_clearsPeekFlag() {
        let s = SidebarModel(initialCollapsed: true, initialMode: .tabs)
        s.sidebarPeeking = true
        s.endSidebarPeek()
        XCTAssertFalse(s.sidebarPeeking)
    }

    func test_endSidebarPeek_isNoOpWhenAlreadyClear() {
        let s = SidebarModel(initialCollapsed: false, initialMode: .tabs)
        XCTAssertFalse(s.sidebarPeeking)
        s.endSidebarPeek()
        XCTAssertFalse(s.sidebarPeeking)
    }

    // MARK: - sidebarPeeking direct write

    func test_sidebarPeeking_canBeSetIndependently() {
        // The keyboard monitor pokes `sidebarPeeking = true` directly
        // after a sidebar-tab dispatch; the model exposes a plain
        // `var` so this should just work.
        let s = SidebarModel(initialCollapsed: true, initialMode: .tabs)
        s.sidebarPeeking = true
        XCTAssertTrue(s.sidebarPeeking)
        s.sidebarPeeking = false
        XCTAssertFalse(s.sidebarPeeking)
    }
}
