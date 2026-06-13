//
//  LivePaneRegistryTests.swift
//  NiceUnitTests
//
//  Unit tests for `LivePaneRegistry` — the process-wide side channel
//  that hands a live pane between windows. Pins the one-shot `claim`
//  semantics (a stray second drop can't double-migrate), `withdraw` (an
//  intra-window reorder / cancelled drag never invokes the claim
//  closure), and the `PaneClaim` tri-state — including that a handle is
//  consumed one-shot even for a `.gone` claim, so it can't linger.
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
            claim: { .live(entry) }
        ))
        let handle = reg.handle(forPaneId: "p")
        XCTAssertEqual(handle?.sourceWindowSessionId, "w1")
        XCTAssertEqual(handle?.sourceTabId, "t")
    }

    func test_claim_isOneShot_claimClosureRunsOnce() {
        let reg = LivePaneRegistry()
        var claimCount = 0
        let entry = makePaneEntry()
        reg.publish(.init(
            paneId: "p", sourceWindowSessionId: "w1", sourceTabId: "t",
            claim: { claimCount += 1; return .live(entry) }
        ))

        let first = reg.claim(paneId: "p")
        XCTAssertNotNil(first, "First claim returns the (handle, claim) tuple.")
        if case .live = first?.claim {} else {
            XCTFail("First claim must surface the .live entry.")
        }
        XCTAssertEqual(claimCount, 1)

        let second = reg.claim(paneId: "p")
        XCTAssertNil(second, "Second claim is nil — handle was removed.")
        XCTAssertEqual(claimCount, 1, "Claim closure must not run twice.")
        XCTAssertNil(reg.handle(forPaneId: "p"))
    }

    func test_withdraw_dropsHandle_withoutClaiming() {
        let reg = LivePaneRegistry()
        var claimCount = 0
        let entry = makePaneEntry()
        reg.publish(.init(
            paneId: "p", sourceWindowSessionId: "w1", sourceTabId: "t",
            claim: { claimCount += 1; return .live(entry) }
        ))

        reg.withdraw(paneId: "p")
        XCTAssertNil(reg.handle(forPaneId: "p"))
        XCTAssertNil(reg.claim(paneId: "p"))
        XCTAssertEqual(claimCount, 0, "withdraw must never invoke the claim closure.")
    }

    func test_claim_consumesHandleOneShot_evenWhenClaimIsGone() {
        // A pane that already exited: the claim closure returns `.gone`.
        // The handle must still be consumed one-shot so it can't linger,
        // AND the registered-handle case still returns the tuple (with a
        // `.gone` claim) rather than nil — so a caller can distinguish
        // "already dead" from "never published" (which alone returns nil).
        let reg = LivePaneRegistry()
        reg.publish(.init(
            paneId: "p", sourceWindowSessionId: "w1", sourceTabId: "t",
            claim: { .gone }
        ))
        let result = reg.claim(paneId: "p")
        XCTAssertNotNil(result, "A registered handle returns the tuple even for .gone.")
        if case .gone = result?.claim {} else {
            XCTFail("Claim must surface .gone.")
        }
        XCTAssertNil(reg.handle(forPaneId: "p"), "Handle is consumed one-shot.")
        XCTAssertNil(reg.claim(paneId: "p"), "A never-published id returns nil.")
    }

    func test_claim_surfacesNotSpawned() {
        // A deferred pane resolves to `.notSpawned(cwd:)`; the claim must
        // surface the carried cwd so the destination spawns in place.
        let reg = LivePaneRegistry()
        reg.publish(.init(
            paneId: "p", sourceWindowSessionId: "w1", sourceTabId: "t",
            claim: { .notSpawned(cwd: "/tmp/work") }
        ))
        let result = reg.claim(paneId: "p")
        if case .notSpawned(let cwd) = result?.claim {
            XCTAssertEqual(cwd, "/tmp/work")
        } else {
            XCTFail("Claim must surface .notSpawned with the carried cwd.")
        }
        XCTAssertNil(reg.handle(forPaneId: "p"))
    }

    func test_publish_overwritesPriorHandleForSamePane() {
        let reg = LivePaneRegistry()
        let entry = makePaneEntry()
        reg.publish(.init(paneId: "p", sourceWindowSessionId: "old", sourceTabId: "t", claim: { .live(entry) }))
        reg.publish(.init(paneId: "p", sourceWindowSessionId: "new", sourceTabId: "t", claim: { .live(entry) }))
        XCTAssertEqual(reg.handle(forPaneId: "p")?.sourceWindowSessionId, "new")
    }
}
