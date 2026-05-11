//
//  RelaunchScenarioTests.swift
//  NiceUnitTests
//
//  End-to-end scenarios for the multi-window quit/relaunch dance
//  built on top of `RelaunchHarness`. Sits above the unit-level
//  tests (`WindowSessionTearDownTests`,
//  `SessionLifecycleControllerTests`) — those pin individual
//  methods; these pin the user-observable outcome ("you closed 2 of
//  3 windows then quit — your relaunch should bring back exactly
//  the survivor with its tabs").
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class RelaunchScenarioTests: XCTestCase {

    // MARK: - The headline scenario

    /// 3 windows, close 2 via the red traffic light, ⌘Q with the
    /// third still open, relaunch. The relaunch must produce exactly
    /// one window holding the survivor's tabs. This is the exact
    /// real-world repro that motivated the Phase 1 production fix.
    func test_threeWindows_closeTwo_quit_relaunch_keepsSurvivorWithItsTabs() {
        let harness = RelaunchHarness()
        harness.preSeedStore([
            makeWindow(id: "w1", sessionId: "sid-A", tabId: "tab-A"),
            makeWindow(id: "w2", sessionId: "sid-B", tabId: "tab-B"),
            makeWindow(id: "w3", sessionId: "sid-C", tabId: "tab-C"),
        ])
        harness.launch(windowIds: ["w1", "w2", "w3"])

        // User closes the two outer windows via the red traffic
        // light, leaving the middle one open. Index 0 (w1) first, then
        // what was w3 is now at index 1.
        harness.userCloseWindow(at: 0)
        harness.userCloseWindow(at: 1)

        // ⌘Q with w2 still open.
        harness.quit()

        let restored = harness.relaunch()

        XCTAssertEqual(restored.count, 1,
                       "Exactly one window must come back — the survivor.")
        XCTAssertEqual(restored.first?.tabs.tab(for: "tab-B")?.claudeSessionId, "sid-B",
                       "The survivor's Claude tab + session id must round-trip through the dance.")
        XCTAssertEqual(harness.store.state.windows.map(\.id), ["w2"],
                       "On-disk state must hold only w2 — w1 and w3 were user-closed and gone for good.")
    }

    // MARK: - No closes — every window survives

    /// 3 windows, ⌘Q with all of them open. Every window must come
    /// back with its tabs. This is the path the user expects when
    /// they just quit and reopen.
    func test_threeWindows_quitWithoutClosing_relaunch_bringsBackAll() {
        let harness = RelaunchHarness()
        harness.preSeedStore([
            makeWindow(id: "w1", sessionId: "sid-A", tabId: "tab-A"),
            makeWindow(id: "w2", sessionId: "sid-B", tabId: "tab-B"),
            makeWindow(id: "w3", sessionId: "sid-C", tabId: "tab-C"),
        ])
        harness.launch(windowIds: ["w1", "w2", "w3"])

        harness.quit()

        let restored = harness.relaunch()

        XCTAssertEqual(restored.count, 3,
                       "All three windows must come back — none were user-closed.")
        XCTAssertEqual(harness.store.state.windows.map(\.id).sorted(),
                       ["w1", "w2", "w3"],
                       "Every window's snapshot must remain on disk after the quit cascade.")
        // Each restored AppState adopted one of the saved slots —
        // collectively they cover the full tab set.
        let restoredTabIds = Set(restored.flatMap { state in
            state.tabs.projects.flatMap { $0.tabs.map(\.id) }
        })
        XCTAssertTrue(restoredTabIds.isSuperset(of: ["tab-A", "tab-B", "tab-C"]),
                      "Every saved Claude tab must materialize in some restored window.")
    }

    // MARK: - Close all three — fresh next launch

    /// 3 windows, close ALL of them via the red traffic light, then
    /// ⌘Q. Next launch must see zero saved windows — production
    /// would then seed a single fresh window. The harness models the
    /// persistence layer, so the assertion is on what's left on
    /// disk; the fresh-seed step is production view-layer behavior.
    func test_threeWindows_closeAll_quit_relaunch_storeIsEmpty() {
        let harness = RelaunchHarness()
        harness.preSeedStore([
            makeWindow(id: "w1", sessionId: "sid-A", tabId: "tab-A"),
            makeWindow(id: "w2", sessionId: "sid-B", tabId: "tab-B"),
            makeWindow(id: "w3", sessionId: "sid-C", tabId: "tab-C"),
        ])
        harness.launch(windowIds: ["w1", "w2", "w3"])

        // Close all three.
        harness.userCloseWindow(at: 0)
        harness.userCloseWindow(at: 0)
        harness.userCloseWindow(at: 0)

        // ⌘Q with no live windows — the cascade is empty.
        harness.quit()

        let restored = harness.relaunch()

        XCTAssertEqual(restored.count, 0,
                       "Zero saved windows → harness returns no AppStates; production would seed a fresh one.")
        XCTAssertTrue(harness.store.state.windows.isEmpty,
                      "Every entry must be gone from disk after a close-all + quit.")
    }

    // MARK: - titleManuallySet survives the dance

    /// A pane the user manually renamed must come back with
    /// `titleManuallySet == true`. Otherwise auto-title logic
    /// downstream would clobber the user's chosen name on the next
    /// OSC emit. The flag's JSON shape (`nil` when false → false
    /// when restored, `true` when true) is the contract this pins.
    func test_manuallyRenamedPaneTitle_survivesQuitAndRelaunch() {
        let harness = RelaunchHarness()
        // Pre-seed a window whose Claude pane was manually renamed.
        // `titleManuallySet: true` on persist; the restore decoder
        // must surface that flag on the rebuilt Pane.
        let claudePaneId = "claude-pane-renamed"
        let renamedTab = PersistedTab(
            id: "tab-renamed",
            title: "Sprint planning",
            cwd: "/tmp",
            branch: nil,
            claudeSessionId: "sid-renamed",
            activePaneId: claudePaneId,
            panes: [
                PersistedPane(
                    id: claudePaneId,
                    title: "Sprint planning",
                    kind: .claude,
                    titleManuallySet: true
                ),
            ],
            titleManuallySet: true
        )
        let window = PersistedWindow(
            id: "w-renamed",
            activeTabId: "tab-renamed",
            sidebarCollapsed: false,
            projects: [
                PersistedProject(
                    id: TabModel.terminalsProjectId,
                    name: "Terminals",
                    path: "/tmp",
                    tabs: []
                ),
                PersistedProject(
                    id: "proj-renamed",
                    name: "RENAMED",
                    path: "/tmp/renamed",
                    tabs: [renamedTab]
                ),
            ]
        )
        harness.preSeedStore([window])
        harness.launch(windowIds: ["w-renamed"])

        // ⌘Q with the renamed tab still open — `.appTerminating`
        // re-snapshots the window. The flag must round-trip.
        harness.quit()

        let restored = harness.relaunch()

        XCTAssertEqual(restored.count, 1, "Single saved window must come back.")
        let tab = try? XCTUnwrap(restored.first?.tabs.tab(for: "tab-renamed"))
        XCTAssertEqual(tab?.titleManuallySet, true,
                       "Tab-level `titleManuallySet` must survive quit → relaunch.")
        let claudePane = tab?.panes.first(where: { $0.id == claudePaneId })
        XCTAssertEqual(claudePane?.titleManuallySet, true,
                       "Pane-level `titleManuallySet` must survive quit → relaunch — otherwise the next OSC emit re-clobbers the user-chosen name.")
    }

    // MARK: - Helpers

    /// Compact helper to fabricate a saved window with one Claude
    /// tab and the standard empty-Terminals project. Tests above
    /// only care about identity + the pane round-trip; the helper
    /// keeps that boilerplate out of the bodies.
    private func makeWindow(id: String, sessionId: String, tabId: String) -> PersistedWindow {
        let claudePaneId = "\(tabId)-claude"
        let tab = PersistedTab(
            id: tabId,
            title: tabId,
            cwd: "/tmp",
            branch: nil,
            claudeSessionId: sessionId,
            activePaneId: claudePaneId,
            panes: [
                PersistedPane(id: claudePaneId, title: "Claude", kind: .claude),
            ]
        )
        return PersistedWindow(
            id: id,
            activeTabId: tabId,
            sidebarCollapsed: false,
            projects: [
                PersistedProject(
                    id: TabModel.terminalsProjectId,
                    name: "Terminals",
                    path: "/tmp",
                    tabs: []
                ),
                PersistedProject(
                    id: "proj-\(id)",
                    name: id.uppercased(),
                    path: "/tmp/\(id)",
                    tabs: [tab]
                ),
            ]
        )
    }
}
