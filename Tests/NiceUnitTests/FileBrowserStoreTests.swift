//
//  FileBrowserStoreTests.swift
//  NiceUnitTests
//
//  Lifecycle coverage for `FileBrowserStore` — the per-window
//  `[Tab.id: FileBrowserState]` catalog. The store's contract is
//  simple but load-bearing for two features:
//    1. The ⌘⇧. shortcut behavior depends on
//       `toggleHiddenFilesIfExists` returning `false` (no allocation,
//       no published change) when the user hasn't opened the file
//       browser for that tab yet.
//    2. `AppState.finalizeDissolvedTab` calls `removeState` to
//       prevent state leaks across long sessions.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class FileBrowserStoreTests: XCTestCase {

    // MARK: - ensureState

    func test_ensureState_createsFreshStateOnFirstCall() {
        let store = FileBrowserStore()
        XCTAssertNil(store.states["t1"])

        let state = store.ensureState(forTab: "t1", cwd: "/tmp/proj")

        XCTAssertEqual(state.rootPath, "/tmp/proj")
        XCTAssertNotNil(store.states["t1"])
    }

    func test_ensureState_returnsSameInstanceOnSecondCall() {
        let store = FileBrowserStore()
        let first = store.ensureState(forTab: "t1", cwd: "/tmp/proj")
        let second = store.ensureState(forTab: "t1", cwd: "/tmp/proj")

        XCTAssertTrue(first === second,
                      "Repeated calls for the same tab must return the same object so views observing it see in-place mutations.")
    }

    func test_ensureState_secondCall_ignoresNewCwdArg() {
        // The cwd parameter is a *seed* used only on first creation.
        // Once a state exists, we return it as-is — even if the caller
        // passed a different cwd. The state's own `rootPath` carries
        // the user's current navigation, which the seed must not undo.
        let store = FileBrowserStore()
        _ = store.ensureState(forTab: "t1", cwd: "/tmp/original")
        let again = store.ensureState(forTab: "t1", cwd: "/tmp/different")

        XCTAssertEqual(again.rootPath, "/tmp/original",
                       "Subsequent calls must not reset the user's navigation. cwd is seed-only.")
    }

    func test_ensureState_distinctTabs_getDistinctStates() {
        let store = FileBrowserStore()
        let a = store.ensureState(forTab: "t1", cwd: "/tmp/a")
        let b = store.ensureState(forTab: "t2", cwd: "/tmp/b")

        XCTAssertFalse(a === b)
        XCTAssertEqual(a.rootPath, "/tmp/a")
        XCTAssertEqual(b.rootPath, "/tmp/b")
    }

    // MARK: - removeState

    func test_removeState_dropsExistingEntry() {
        let store = FileBrowserStore()
        _ = store.ensureState(forTab: "t1", cwd: "/tmp/proj")
        store.removeState(forTab: "t1")

        XCTAssertNil(store.states["t1"])
    }

    func test_removeState_unknownTab_isNoop() {
        let store = FileBrowserStore()
        store.removeState(forTab: "nope") // no entry → no crash, no allocation
        XCTAssertTrue(store.states.isEmpty)
    }

    func test_removeState_thenEnsureState_seedsFreshFromNewCwd() {
        // After a tab closes and reopens (same id, hypothetically),
        // ensureState should re-seed from the new cwd. Important for
        // the contract: removeState fully erases the tab's
        // navigation, so the next session starts clean.
        let store = FileBrowserStore()
        _ = store.ensureState(forTab: "t1", cwd: "/tmp/old")
        store.removeState(forTab: "t1")
        let next = store.ensureState(forTab: "t1", cwd: "/tmp/new")

        XCTAssertEqual(next.rootPath, "/tmp/new")
    }

    // MARK: - toggleHiddenFilesIfExists

    func test_toggleHiddenFilesIfExists_noState_returnsFalseAndAllocatesNothing() {
        let store = FileBrowserStore()

        let didToggle = store.toggleHiddenFilesIfExists(forTab: "t1")

        XCTAssertFalse(didToggle,
                       "Without an existing state, the call must report no-op so the ⌘⇧. shortcut is silent in tabs mode.")
        XCTAssertNil(store.states["t1"],
                     "Critical: must NOT lazy-create a state. Otherwise the shortcut allocates state for tabs the user never opened in files mode.")
    }

    func test_toggleHiddenFilesIfExists_withState_flipsAndReturnsTrue() {
        let store = FileBrowserStore()
        let state = store.ensureState(forTab: "t1", cwd: "/tmp/proj")
        let before = state.showHidden

        let didToggle = store.toggleHiddenFilesIfExists(forTab: "t1")

        XCTAssertTrue(didToggle)
        XCTAssertEqual(state.showHidden, !before)
    }

    func test_toggleHiddenFilesIfExists_twice_restoresOriginalValue() {
        let store = FileBrowserStore()
        let state = store.ensureState(forTab: "t1", cwd: "/tmp/proj")
        let original = state.showHidden

        _ = store.toggleHiddenFilesIfExists(forTab: "t1")
        _ = store.toggleHiddenFilesIfExists(forTab: "t1")

        XCTAssertEqual(state.showHidden, original)
    }
}
