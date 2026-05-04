//
//  NiceServicesTearOffTests.swift
//  NiceUnitTests
//
//  Tests for the audit-Phase-1 tear-off plumbing on `NiceServices`:
//  destination-tag matching, TTL drop, deferred source persistence,
//  and abandoned-slot recovery. The view-migration side
//  (`detachPane` / `attachPane`) is exercised by other tests; here
//  the pane has no live view (`nil`) and we only verify the data
//  layer + slot lifecycle.
//
//  Registry-dependent paths (`completeTearOff`,
//  `recoverAbandonedTearOff`'s rollback) attach the source AppState
//  to a borderless NSWindow so `services.registry.register` and
//  `appState(forSessionId:)` resolve correctly. The window is
//  released-when-closed=false so XCTest's memory checker doesn't
//  trip on teardown.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class NiceServicesTearOffTests: XCTestCase {

    private var services: NiceServices!
    private var source: AppState!
    private var sourceWindow: NSWindow!
    private var sourceMutationCount = 0

    override func setUp() {
        super.setUp()
        services = NiceServices()
        source = AppState()
        // Inline (instead of a helper method on self) so strict-
        // concurrency setUp doesn't trip on sending `self` to a
        // MainActor-isolated method.
        let w = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 100, height: 100),
            styleMask: [.borderless], backing: .buffered, defer: false
        )
        // Without this, ARC + NSWindow's legacy default crashes
        // XCTest's memory checker in objc_release at teardown.
        w.isReleasedWhenClosed = false
        sourceWindow = w
        services.registry.register(appState: source, window: w)
        // Hook source persistence so tests can assert it WAS or
        // WASN'T fired (the audit's deferred-persistence invariant).
        sourceMutationCount = 0
        source.sessions.onSessionMutation = { [unowned self] in
            self.sourceMutationCount += 1
        }
    }

    override func tearDown() {
        sourceWindow?.close()
        sourceWindow = nil
        source = nil
        services = nil
        super.tearDown()
    }

    // MARK: - requestPaneTearOff

    func test_requestPaneTearOff_returnsDestId_andStashesPending() {
        let tabId = seedTerminalsTab(in: source, paneIds: ["t-a", "t-b"])
        let destId = services.requestPaneTearOff(
            from: source,
            tabId: tabId,
            paneId: "t-a",
            cursorScreenPoint: CGPoint(x: 100, y: 200),
            pillOriginOffset: CGSize(width: 12, height: 8)
        )
        XCTAssertNotNil(destId)
        XCTAssertNotNil(services.pendingTearOff)
        XCTAssertEqual(services.pendingTearOff?.destinationWindowSessionId, destId)
        XCTAssertEqual(services.pendingTearOff?.cursorScreenPoint.x, 100)
        XCTAssertEqual(services.pendingTearOff?.cursorScreenPoint.y, 200)
        XCTAssertEqual(services.pendingTearOff?.pillOriginOffset.width, 12)
        XCTAssertEqual(services.pendingTearOff?.pillOriginOffset.height, 8)
        XCTAssertEqual(services.pendingTearOff?.payload.paneId, "t-a")
    }

    func test_requestPaneTearOff_mutatesSource_butDefersPersistence() {
        let tabId = seedTerminalsTab(in: source, paneIds: ["t-a", "t-b"])
        _ = services.requestPaneTearOff(
            from: source, tabId: tabId, paneId: "t-a",
            cursorScreenPoint: .zero, pillOriginOffset: .zero
        )
        // Pane removed from source tab.
        XCTAssertEqual(
            source.tabs.tab(for: tabId)?.panes.map(\.id),
            ["t-b"]
        )
        // Persistence has NOT been scheduled — that's the audit's
        // "defer source save until destination absorbs" requirement.
        XCTAssertEqual(sourceMutationCount, 0)
    }

    func test_requestPaneTearOff_unknownTab_returnsNil_doesNothing() {
        let destId = services.requestPaneTearOff(
            from: source, tabId: "no-such-tab", paneId: "x",
            cursorScreenPoint: .zero, pillOriginOffset: .zero
        )
        XCTAssertNil(destId)
        XCTAssertNil(services.pendingTearOff)
    }

    // MARK: - consumeTearOff

    func test_consumeTearOff_strictTagMatch_returnsAndClears() {
        let tabId = seedTerminalsTab(in: source, paneIds: ["t-a"])
        let destId = services.requestPaneTearOff(
            from: source, tabId: tabId, paneId: "t-a",
            cursorScreenPoint: .zero, pillOriginOffset: .zero
        )!
        let pending = services.consumeTearOff(forWindowSessionId: destId)
        XCTAssertNotNil(pending)
        XCTAssertNil(services.pendingTearOff)
    }

    func test_consumeTearOff_tagMismatch_returnsNilAndKeepsSlot() {
        let tabId = seedTerminalsTab(in: source, paneIds: ["t-a"])
        _ = services.requestPaneTearOff(
            from: source, tabId: tabId, paneId: "t-a",
            cursorScreenPoint: .zero, pillOriginOffset: .zero
        )
        let pending = services.consumeTearOff(forWindowSessionId: "not-the-dest")
        XCTAssertNil(pending)
        XCTAssertNotNil(services.pendingTearOff)
    }

    func test_consumeTearOff_pastTTL_returnsNilAndDropsSlot() {
        let tabId = seedTerminalsTab(in: source, paneIds: ["t-a"])
        let destId = services.requestPaneTearOff(
            from: source, tabId: tabId, paneId: "t-a",
            cursorScreenPoint: .zero, pillOriginOffset: .zero
        )!
        // Force the entry to look stale by rewriting the slot with a
        // backdated `createdAt`. This is the only seam — the public
        // API doesn't accept a clock — so tests exercise the same
        // record shape with the createdAt at TTL+1s in the past.
        guard var stale = services.pendingTearOff else {
            return XCTFail("pendingTearOff missing after request")
        }
        stale = staleCopy(of: stale, secondsAgo: NiceServices.tearOffTTL + 0.5)
        services.pendingTearOff = stale

        let pending = services.consumeTearOff(forWindowSessionId: destId)
        XCTAssertNil(pending)
        XCTAssertNil(services.pendingTearOff,
                     "stale slot should be dropped on consume even with matching tag")
    }

    // MARK: - completeTearOff

    func test_completeTearOff_firesSourceSessionMutation() {
        let tabId = seedTerminalsTab(in: source, paneIds: ["t-a", "t-b"])
        let destId = services.requestPaneTearOff(
            from: source, tabId: tabId, paneId: "t-a",
            cursorScreenPoint: .zero, pillOriginOffset: .zero
        )!
        let pending = services.consumeTearOff(forWindowSessionId: destId)!
        // requestPaneTearOff did NOT fire it (deferred); completeTearOff
        // is what schedules persistence once the destination absorbed.
        XCTAssertEqual(sourceMutationCount, 0)
        services.completeTearOff(pending)
        XCTAssertEqual(sourceMutationCount, 1)
    }

    // MARK: - recoverAbandonedTearOff

    func test_recoverAbandonedTearOff_freshSlot_isNoOp() {
        let tabId = seedTerminalsTab(in: source, paneIds: ["t-a", "t-b"])
        _ = services.requestPaneTearOff(
            from: source, tabId: tabId, paneId: "t-a",
            cursorScreenPoint: .zero, pillOriginOffset: .zero
        )
        services.recoverAbandonedTearOff()
        // Fresh entry — recovery shouldn't reclaim. Pane stays out
        // of source until the destination either absorbs or the slot
        // ages out.
        XCTAssertNotNil(services.pendingTearOff)
        XCTAssertEqual(
            source.tabs.tab(for: tabId)?.panes.map(\.id),
            ["t-b"]
        )
    }

    func test_recoverAbandonedTearOff_staleSlot_reInsertsPaneIntoSource() {
        let tabId = seedTerminalsTab(in: source, paneIds: ["t-a", "t-b"])
        _ = services.requestPaneTearOff(
            from: source, tabId: tabId, paneId: "t-a",
            cursorScreenPoint: .zero, pillOriginOffset: .zero
        )
        // Source had t-a removed; verify before recovery.
        XCTAssertEqual(
            source.tabs.tab(for: tabId)?.panes.map(\.id),
            ["t-b"]
        )
        // Force the slot stale, then run recovery.
        guard var slot = services.pendingTearOff else {
            return XCTFail("pendingTearOff missing")
        }
        slot = staleCopy(of: slot, secondsAgo: NiceServices.tearOffTTL + 0.5)
        services.pendingTearOff = slot

        services.recoverAbandonedTearOff()
        XCTAssertNil(services.pendingTearOff)
        // Pane is back in the source tab.
        let panes = source.tabs.tab(for: tabId)?.panes.map(\.id) ?? []
        XCTAssertTrue(panes.contains("t-a"))
        // Source persistence WAS fired by recovery — the source
        // model just changed and needs a save.
        XCTAssertEqual(sourceMutationCount, 1)
    }

    // MARK: - Fixtures

    @discardableResult
    private func seedTerminalsTab(
        in app: AppState, paneIds: [String]
    ) -> String {
        let tabId = "tab-\(UUID().uuidString.prefix(6))"
        let panes = paneIds.map { Pane(id: $0, title: $0, kind: .terminal) }
        let tab = Tab(
            id: tabId, title: "Tab", cwd: "/tmp/\(tabId)",
            panes: panes, activePaneId: panes.first?.id
        )
        app.tabs.projects.append(Project(
            id: "proj-\(tabId)", name: tabId.uppercased(),
            path: "/tmp/\(tabId)", tabs: [tab]
        ))
        return tabId
    }

    /// Build a copy of `pending` with `createdAt` rewound by
    /// `secondsAgo`. There's no public clock seam on `NiceServices`,
    /// so tests rewrite the slot record directly.
    private func staleCopy(of pending: PendingTearOff, secondsAgo: TimeInterval) -> PendingTearOff {
        PendingTearOff(
            payload: pending.payload,
            view: pending.view,
            pane: pending.pane,
            sourceTab: pending.sourceTab,
            originAppStateId: pending.originAppStateId,
            destinationWindowSessionId: pending.destinationWindowSessionId,
            projectAnchor: pending.projectAnchor,
            cursorScreenPoint: pending.cursorScreenPoint,
            pillOriginOffset: pending.pillOriginOffset,
            pendingLaunchState: pending.pendingLaunchState,
            createdAt: Date(timeIntervalSinceNow: -secondsAgo)
        )
    }
}
