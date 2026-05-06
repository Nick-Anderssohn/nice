//
//  AppStateTabSelectionTests.swift
//  NiceUnitTests
//
//  Coverage for the `tabSelection` wiring on `AppState` —
//  specifically that `finalizeDissolvedTab` calls `prune(...)` on
//  the multi-selection model so a dissolved tab can never linger as
//  a stale id in the set. The model-layer prune logic is unit-tested
//  in `SidebarTabSelectionTests`; this file pins the wiring (the
//  sequencing inside `finalizeDissolvedTab` and the call from the
//  external `paneExited` cascade through `onTabBecameEmpty`).
//
//  Mirrors the shape of `AppStateFileBrowserTests`'s
//  `test_closingTab_removesFileBrowserState` — same fixture, same
//  failure-mode the test guards against (long-lived window
//  accumulating stale state).
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class AppStateTabSelectionTests: XCTestCase {

    private var appState: AppState!

    override func setUp() {
        super.setUp()
        appState = AppState()
    }

    override func tearDown() {
        appState = nil
        super.tearDown()
    }

    // MARK: - prune wiring

    func test_closingTab_prunesFromMultiSelection() {
        // Memory + correctness guard: if `prune` doesn't fire from
        // `finalizeDissolvedTab`, a closed tab's id keeps haunting
        // the selection set forever. Worse, the next Shift-click
        // could try to extend a range from a dangling anchor and
        // silently no-op.
        let aId = TabModelFixtures.injectClaudeTab(into: appState.tabs, projectName: "A")
        let bId = TabModelFixtures.injectClaudeTab(into: appState.tabs, projectName: "B")

        appState.tabSelection.replace(with: aId)
        _ = appState.tabSelection.toggle(bId)
        XCTAssertEqual(appState.tabSelection.selectedTabIds, [aId, bId])

        appState.closer.requestCloseTab(tabId: aId)

        XCTAssertEqual(
            appState.tabSelection.selectedTabIds, [bId],
            "finalizeDissolvedTab must call tabSelection.prune so closed "
            + "tabs don't linger in the multi-selection set.")
    }

    func test_closingTab_clearsAnchorWhenAnchorWasTheClosedTab() {
        // After closing the anchor tab, a follow-up Shift-click must
        // hit `extend`'s empty-anchor fallback (which does a plain
        // replace) instead of silently no-op'ing on a stale id.
        let aId = TabModelFixtures.injectClaudeTab(into: appState.tabs, projectName: "A")
        let bId = TabModelFixtures.injectClaudeTab(into: appState.tabs, projectName: "B")

        appState.tabSelection.replace(with: bId)
        _ = appState.tabSelection.toggle(aId)
        // Anchor is now `aId` (toggle moves the anchor to the
        // toggled id).
        XCTAssertEqual(appState.tabSelection.lastClickedTabId, aId)

        appState.closer.requestCloseTab(tabId: aId)

        XCTAssertNil(
            appState.tabSelection.lastClickedTabId,
            "Anchor must clear when its tab dissolves so subsequent "
            + "Shift-click extends from the empty-anchor fallback.")
    }

    func test_closingTab_keepsAnchorWhenAnchorSurvives() {
        // Symmetric: closing a tab that ISN'T the anchor must leave
        // the anchor intact for subsequent Shift-extends.
        let aId = TabModelFixtures.injectClaudeTab(into: appState.tabs, projectName: "A")
        let bId = TabModelFixtures.injectClaudeTab(into: appState.tabs, projectName: "B")

        appState.tabSelection.replace(with: bId)
        _ = appState.tabSelection.toggle(aId)
        // Anchor is `aId`; we'll close `bId` instead.

        appState.closer.requestCloseTab(tabId: bId)

        XCTAssertEqual(appState.tabSelection.lastClickedTabId, aId,
                       "Anchor must survive when a different tab dissolves.")
        XCTAssertEqual(appState.tabSelection.selectedTabIds, [aId])
    }
}
