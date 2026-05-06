//
//  CloseRequestCoordinatorMultiCloseTests.swift
//  NiceUnitTests
//
//  Direct coverage for `requestCloseTabs(ids:)` and the `.tabs` scope
//  through `confirmPendingClose` / `cancelPendingClose`. The model-
//  layer pieces (selection set + anchor + active mirror) are pinned
//  in `SidebarTabSelectionTests`; this file pins the multi-tab close
//  state machine — idle/busy partitioning, the singular-id fast
//  path, the alert-already-in-flight early return, and the unified
//  `.tabs` scope replacing the old sibling `PendingMultiCloseRequest`.
//
//  Mirrors the shape of `CloseRequestCoordinatorPaneTests` — same
//  fixture (`AppState()` + `TabModelFixtures.seedClaudeTab` + status
//  flips) so unspawned panes' `terminatePane` calls are no-ops we
//  observe via `tab(for:) == nil` after a synchronous dissolve.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class CloseRequestCoordinatorMultiCloseTests: XCTestCase {

    private var appState: AppState!
    private var homeSandbox: TestHomeSandbox!

    override func setUp() {
        super.setUp()
        homeSandbox = TestHomeSandbox()
        appState = AppState()
    }

    override func tearDown() {
        appState = nil
        homeSandbox.teardown()
        homeSandbox = nil
        super.tearDown()
    }

    // MARK: - Single-id fast path

    func test_requestCloseTabs_singleId_forwardsToSingularPath() {
        // The fast path must produce the same `.tab(tabId:)` scope
        // a normal right-click → Close would, so the alert wording
        // and confirm/cancel pair stay singular for a one-tab batch.
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )
        TabModelFixtures.setClaudeStatusOnEveryTab(
            in: appState.tabs, projectId: "p1", status: .thinking
        )

        appState.closer.requestCloseTabs(ids: ["t1"])

        guard case .tab(let tabId) = appState.closer.pendingCloseRequest?.scope else {
            XCTFail("Expected .tab scope from single-id fast path")
            return
        }
        XCTAssertEqual(tabId, "t1")
    }

    // MARK: - All-idle batch

    func test_requestCloseTabs_allIdle_killsAllSynchronously_noAlert() {
        // Idle Claude/terminal panes are all unspawned in the
        // fixture, so `hardKillTab`'s synchronous-dissolve branch
        // fires for every tab. We can't directly observe the
        // SIGTERM, but we CAN observe the sync dissolve via
        // `tab(for:) == nil` and the absence of any alert.
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1",
            tabId: "t2", appendToExisting: true
        )

        appState.closer.requestCloseTabs(ids: ["t1", "t2"])

        XCTAssertNil(
            appState.closer.pendingCloseRequest,
            "All-idle batch must close synchronously without staging an alert."
        )
        XCTAssertNil(appState.tabs.tab(for: "t1"))
        XCTAssertNil(appState.tabs.tab(for: "t2"))
    }

    // MARK: - Mixed batch

    func test_requestCloseTabs_mixedBatch_killsIdle_andStagesOnlyBusyInPending() {
        // Three tabs; only `t2` is busy. Idle tabs (`t1`, `t3`)
        // close synchronously *before* the alert goes up — that's
        // the asymmetry the user agreed to (one combined alert for
        // busy survivors, idle ones don't get a "do you really want
        // to" prompt). Pin both halves: idle gone, busy staged with
        // exactly one summary line.
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1",
            tabId: "t2", appendToExisting: true
        )
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1",
            tabId: "t3", appendToExisting: true
        )
        // Mark only t2 as thinking by mutating its claude pane
        // directly — `setClaudeStatusOnEveryTab` would flip all three.
        flipClaudeStatus(in: "p1", tabId: "t2", to: .thinking)

        appState.closer.requestCloseTabs(ids: ["t1", "t2", "t3"])

        XCTAssertNil(appState.tabs.tab(for: "t1"),
                     "Idle tab must close immediately, before the alert.")
        XCTAssertNil(appState.tabs.tab(for: "t3"),
                     "Idle tab must close immediately, before the alert.")
        XCTAssertNotNil(appState.tabs.tab(for: "t2"),
                        "Busy tab must survive until the user confirms.")

        guard case .tabs(let tabIds) = appState.closer.pendingCloseRequest?.scope else {
            XCTFail("Expected .tabs scope after mixed batch")
            return
        }
        XCTAssertEqual(tabIds, ["t2"],
                       "Pending request must list only the busy survivors.")
        XCTAssertEqual(appState.closer.pendingCloseRequest?.busyPanes.count, 1,
                       "One summary line per busy tab.")
    }

    func test_requestCloseTabs_allBusy_killsNoneAndStagesAll() {
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1",
            tabId: "t2", appendToExisting: true
        )
        TabModelFixtures.setClaudeStatusOnEveryTab(
            in: appState.tabs, projectId: "p1", status: .thinking
        )

        appState.closer.requestCloseTabs(ids: ["t1", "t2"])

        XCTAssertNotNil(appState.tabs.tab(for: "t1"))
        XCTAssertNotNil(appState.tabs.tab(for: "t2"))
        guard case .tabs(let tabIds) = appState.closer.pendingCloseRequest?.scope else {
            XCTFail("Expected .tabs scope")
            return
        }
        XCTAssertEqual(Set(tabIds), ["t1", "t2"])
    }

    // MARK: - Alert-already-in-flight (defensive early return)

    func test_requestCloseTabs_singularAlertAlreadyPending_dropsRequest() {
        // Stage a singular `.tab` alert by closing one busy tab;
        // then call `requestCloseTabs` with two more thinking tabs.
        // The defensive early-return drops the new request silently
        // (logged, not asserted on stdout) and the previously-staged
        // singular alert is preserved.
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1",
            tabId: "t2", appendToExisting: true
        )
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1",
            tabId: "t3", appendToExisting: true
        )
        TabModelFixtures.setClaudeStatusOnEveryTab(
            in: appState.tabs, projectId: "p1", status: .thinking
        )
        appState.closer.requestCloseTab(tabId: "t1")
        XCTAssertNotNil(appState.closer.pendingCloseRequest)
        guard case .tab = appState.closer.pendingCloseRequest?.scope else {
            XCTFail("Setup precondition: singular .tab alert must be staged.")
            return
        }

        appState.closer.requestCloseTabs(ids: ["t2", "t3"])

        guard case .tab(let tabId) = appState.closer.pendingCloseRequest?.scope else {
            XCTFail("Singular alert must be preserved; multi-close was dropped.")
            return
        }
        XCTAssertEqual(tabId, "t1")
        XCTAssertNotNil(appState.tabs.tab(for: "t2"),
                        "Dropped multi-close must NOT close any tabs.")
        XCTAssertNotNil(appState.tabs.tab(for: "t3"))
    }

    func test_requestCloseTabs_multiAlertAlreadyPending_dropsSecondRequest() {
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1",
            tabId: "t2", appendToExisting: true
        )
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1",
            tabId: "t3", appendToExisting: true
        )
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1",
            tabId: "t4", appendToExisting: true
        )
        TabModelFixtures.setClaudeStatusOnEveryTab(
            in: appState.tabs, projectId: "p1", status: .thinking
        )

        // Stage a multi-close alert with {t1, t2}.
        appState.closer.requestCloseTabs(ids: ["t1", "t2"])
        guard case .tabs(let firstIds) = appState.closer.pendingCloseRequest?.scope else {
            XCTFail("Setup: first multi-close must be staged")
            return
        }
        XCTAssertEqual(Set(firstIds), ["t1", "t2"])

        // Second call with different ids should be silently dropped.
        appState.closer.requestCloseTabs(ids: ["t3", "t4"])

        guard case .tabs(let stillFirstIds) = appState.closer.pendingCloseRequest?.scope else {
            XCTFail("First multi-close must survive the dropped second call")
            return
        }
        XCTAssertEqual(Set(stillFirstIds), ["t1", "t2"],
                       "Pending request must NOT be replaced.")
        XCTAssertNotNil(appState.tabs.tab(for: "t3"),
                        "Dropped multi-close must NOT close any tabs.")
        XCTAssertNotNil(appState.tabs.tab(for: "t4"))
    }

    // MARK: - Unknown / empty inputs

    func test_requestCloseTabs_unknownIdInBatch_isSkippedViaTabForGuard() {
        // The `tab(for:)` lookup at the top of the partition loop
        // silently skips ids that aren't in the tree. With ids.count > 1
        // the singular fast path doesn't trigger, so we exercise the
        // multi-close partition loop directly.
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )
        TabModelFixtures.setClaudeStatusOnEveryTab(
            in: appState.tabs, projectId: "p1", status: .thinking
        )

        appState.closer.requestCloseTabs(ids: ["t1", "ghost"])

        guard case .tabs(let tabIds) = appState.closer.pendingCloseRequest?.scope else {
            XCTFail("Expected .tabs scope")
            return
        }
        XCTAssertEqual(tabIds, ["t1"], "Ghost id must be silently skipped.")
    }

    func test_requestCloseTabs_emptyIds_isNoOp() {
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )

        appState.closer.requestCloseTabs(ids: [])

        XCTAssertNil(appState.closer.pendingCloseRequest)
        XCTAssertNotNil(appState.tabs.tab(for: "t1"))
    }

    // MARK: - confirmPendingClose with .tabs scope

    func test_confirmPendingClose_tabsScope_killsEveryBusyTab_andClearsField() {
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1",
            tabId: "t2", appendToExisting: true
        )
        TabModelFixtures.setClaudeStatusOnEveryTab(
            in: appState.tabs, projectId: "p1", status: .thinking
        )
        appState.closer.requestCloseTabs(ids: ["t1", "t2"])
        XCTAssertNotNil(appState.closer.pendingCloseRequest)

        appState.closer.confirmPendingClose()

        XCTAssertNil(appState.closer.pendingCloseRequest,
                     "Confirm must clear the pending request before dispatching kills.")
        XCTAssertNil(appState.tabs.tab(for: "t1"),
                     ".tabs confirm must hard-kill every listed busy tab.")
        XCTAssertNil(appState.tabs.tab(for: "t2"))
    }

    func test_cancelPendingClose_tabsScope_clearsField_leavesBusyTabsAlive() {
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1", tabId: "t1"
        )
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p1",
            tabId: "t2", appendToExisting: true
        )
        TabModelFixtures.setClaudeStatusOnEveryTab(
            in: appState.tabs, projectId: "p1", status: .thinking
        )
        appState.closer.requestCloseTabs(ids: ["t1", "t2"])

        appState.closer.cancelPendingClose()

        XCTAssertNil(appState.closer.pendingCloseRequest)
        XCTAssertNotNil(appState.tabs.tab(for: "t1"),
                        "Cancel must leave busy tabs running.")
        XCTAssertNotNil(appState.tabs.tab(for: "t2"))
    }

    // MARK: - Helpers

    /// Flip just one tab's Claude pane to `status`; the bulk helper
    /// `setClaudeStatusOnEveryTab` would change all tabs in the
    /// project, which mixed-batch tests don't want.
    private func flipClaudeStatus(in projectId: String, tabId: String, to status: TabStatus) {
        var projects = appState.tabs.projects
        guard let pi = projects.firstIndex(where: { $0.id == projectId }),
              let ti = projects[pi].tabs.firstIndex(where: { $0.id == tabId })
        else { return }
        for pxi in projects[pi].tabs[ti].panes.indices
        where projects[pi].tabs[ti].panes[pxi].kind == .claude {
            projects[pi].tabs[ti].panes[pxi].status = status
        }
        appState.tabs.projects = projects
    }
}
