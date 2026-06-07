//
//  LivePaneRegistryTests.swift
//  NiceUnitTests
//
//  Unit tests for `LivePaneRegistry` — the process-wide side channel
//  that hands a live pane between windows. Pins the one-shot `claim`
//  semantics (a stray second drop can't double-migrate) and `withdraw`
//  (an intra-window reorder / cancelled drag never invokes the detach
//  closure).
//

import AppKit
import XCTest
@testable import Nice

@MainActor
final class LivePaneRegistryTests: XCTestCase {

    /// A throwaway live entry. The pty inside never spawns (the view is
    /// never laid out in a window), so this is cheap to build headless.
    private func makePaneEntry(paneId: String = "p") -> TabPtySession.PaneEntry {
        let view = NiceTerminalView(frame: .zero)
        let delegate = ProcessTerminationDelegate(
            role: .pane(tabId: "t", paneId: paneId),
            onExit: { _, _ in }
        )
        return TabPtySession.PaneEntry(view: view, kind: .terminal, delegate: delegate)
    }

    func test_handle_lookup_returnsPublishedHandle() {
        let reg = LivePaneRegistry()
        let entry = makePaneEntry()
        reg.publish(.init(
            paneId: "p", sourceWindowSessionId: "w1", sourceTabId: "t",
            claim: { entry }
        ))
        let handle = reg.handle(forPaneId: "p")
        XCTAssertEqual(handle?.sourceWindowSessionId, "w1")
        XCTAssertEqual(handle?.sourceTabId, "t")
    }

    func test_claim_isOneShot_detachClosureRunsOnce() {
        let reg = LivePaneRegistry()
        var claimCount = 0
        let entry = makePaneEntry()
        reg.publish(.init(
            paneId: "p", sourceWindowSessionId: "w1", sourceTabId: "t",
            claim: { claimCount += 1; return entry }
        ))

        let first = reg.claim(paneId: "p")
        XCTAssertNotNil(first, "First claim returns the entry.")
        XCTAssertEqual(claimCount, 1)

        let second = reg.claim(paneId: "p")
        XCTAssertNil(second, "Second claim is nil — handle was removed.")
        XCTAssertEqual(claimCount, 1, "Detach closure must not run twice.")
        XCTAssertNil(reg.handle(forPaneId: "p"))
    }

    func test_withdraw_dropsHandle_withoutDetaching() {
        let reg = LivePaneRegistry()
        var claimCount = 0
        let entry = makePaneEntry()
        reg.publish(.init(
            paneId: "p", sourceWindowSessionId: "w1", sourceTabId: "t",
            claim: { claimCount += 1; return entry }
        ))

        reg.withdraw(paneId: "p")
        XCTAssertNil(reg.handle(forPaneId: "p"))
        XCTAssertNil(reg.claim(paneId: "p"))
        XCTAssertEqual(claimCount, 0, "withdraw must never invoke the detach closure.")
    }

    func test_claim_removesHandle_evenWhenClosureReturnsNil() {
        // A pane that already exited: the detach closure returns nil.
        // The handle should still be consumed so it can't linger.
        let reg = LivePaneRegistry()
        reg.publish(.init(
            paneId: "p", sourceWindowSessionId: "w1", sourceTabId: "t",
            claim: { nil }
        ))
        XCTAssertNil(reg.claim(paneId: "p"))
        XCTAssertNil(reg.handle(forPaneId: "p"))
    }

    func test_publish_overwritesPriorHandleForSamePane() {
        let reg = LivePaneRegistry()
        let entry = makePaneEntry()
        reg.publish(.init(paneId: "p", sourceWindowSessionId: "old", sourceTabId: "t", claim: { entry }))
        reg.publish(.init(paneId: "p", sourceWindowSessionId: "new", sourceTabId: "t", claim: { entry }))
        XCTAssertEqual(reg.handle(forPaneId: "p")?.sourceWindowSessionId, "new")
    }
}
