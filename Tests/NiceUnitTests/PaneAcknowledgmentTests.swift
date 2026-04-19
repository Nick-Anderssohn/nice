//
//  PaneAcknowledgmentTests.swift
//  NiceUnitTests
//
//  Tests the waiting-pulse acknowledgment model on `Pane`. The sidebar and
//  toolbar status dots pulse for `.waiting` only while
//  `waitingAcknowledged == false`; entering `.waiting` sets the flag based
//  on whether the user is currently looking at the pane, and visiting the
//  pane afterwards flips it true. These tests cover the pure state
//  transitions — the AppState-level coordination (navigating between
//  tabs) is exercised by the view layer at runtime.
//

import XCTest
@testable import Nice

final class PaneAcknowledgmentTests: XCTestCase {

    // MARK: - applyStatusTransition

    func test_enteringWaiting_whileBeingViewed_marksAcknowledged() {
        var pane = Pane(id: "p", title: "Claude", kind: .claude)
        pane.applyStatusTransition(to: .waiting, isCurrentlyBeingViewed: true)

        XCTAssertEqual(pane.status, .waiting)
        XCTAssertTrue(pane.waitingAcknowledged,
                      "A waiting state that arrives while the user is on the pane should not pulse.")
    }

    func test_enteringWaiting_whileNotBeingViewed_stayUnacknowledged() {
        var pane = Pane(id: "p", title: "Claude", kind: .claude)
        pane.applyStatusTransition(to: .waiting, isCurrentlyBeingViewed: false)

        XCTAssertEqual(pane.status, .waiting)
        XCTAssertFalse(pane.waitingAcknowledged,
                       "A waiting state that arrives while the user is elsewhere should pulse.")
    }

    func test_transitioningOutOfWaiting_clearsAcknowledgment() {
        var pane = Pane(id: "p", title: "Claude", kind: .claude)
        pane.applyStatusTransition(to: .waiting, isCurrentlyBeingViewed: true)
        XCTAssertTrue(pane.waitingAcknowledged)

        pane.applyStatusTransition(to: .thinking, isCurrentlyBeingViewed: true)
        XCTAssertEqual(pane.status, .thinking)
        XCTAssertFalse(pane.waitingAcknowledged,
                       "Leaving waiting must reset the flag so a future waiting event can pulse.")
    }

    func test_transitioningOutOfWaiting_toIdle_clearsAcknowledgment() {
        var pane = Pane(id: "p", title: "Claude", kind: .claude)
        pane.applyStatusTransition(to: .waiting, isCurrentlyBeingViewed: false)
        // Simulate user later viewing it.
        pane.markAcknowledgedIfWaiting()
        XCTAssertTrue(pane.waitingAcknowledged)

        pane.applyStatusTransition(to: .idle, isCurrentlyBeingViewed: false)
        XCTAssertEqual(pane.status, .idle)
        XCTAssertFalse(pane.waitingAcknowledged)
    }

    func test_sameStatusReassignment_isNoop_preservesAcknowledgment() {
        var pane = Pane(id: "p", title: "Claude", kind: .claude)
        pane.applyStatusTransition(to: .waiting, isCurrentlyBeingViewed: false)
        XCTAssertFalse(pane.waitingAcknowledged)

        // User acknowledges.
        pane.markAcknowledgedIfWaiting()
        XCTAssertTrue(pane.waitingAcknowledged)

        // Another .waiting report (identical status) should not wipe the
        // acknowledgment — the user has already seen the state.
        pane.applyStatusTransition(to: .waiting, isCurrentlyBeingViewed: false)
        XCTAssertTrue(pane.waitingAcknowledged,
                      "Repeated waiting reports must not re-raise the pulse once the user has acknowledged it.")
    }

    func test_reentryToWaiting_recomputesAgainstCurrentViewing() {
        var pane = Pane(id: "p", title: "Claude", kind: .claude)

        // First waiting event while user was elsewhere — pulses, user
        // later comes and looks.
        pane.applyStatusTransition(to: .waiting, isCurrentlyBeingViewed: false)
        pane.markAcknowledgedIfWaiting()
        XCTAssertTrue(pane.waitingAcknowledged)

        // Thinking in between wipes the flag.
        pane.applyStatusTransition(to: .thinking, isCurrentlyBeingViewed: true)
        XCTAssertFalse(pane.waitingAcknowledged)

        // Second waiting event while user is NOT on the pane — should
        // pulse again. The prior acknowledgment must not linger.
        pane.applyStatusTransition(to: .waiting, isCurrentlyBeingViewed: false)
        XCTAssertFalse(pane.waitingAcknowledged,
                       "A fresh waiting event after thinking must re-raise the pulse when the user isn't looking.")
    }

    func test_enteringThinking_doesNotSetAcknowledged() {
        var pane = Pane(id: "p", title: "Claude", kind: .claude)
        pane.applyStatusTransition(to: .thinking, isCurrentlyBeingViewed: true)

        XCTAssertEqual(pane.status, .thinking)
        XCTAssertFalse(pane.waitingAcknowledged,
                       "Thinking doesn't use the acknowledgment flag; it always pulses.")
    }

    // MARK: - markAcknowledgedIfWaiting

    func test_markAcknowledgedIfWaiting_whileIdle_isNoop() {
        var pane = Pane(id: "p", title: "Claude", kind: .claude)
        pane.markAcknowledgedIfWaiting()
        XCTAssertFalse(pane.waitingAcknowledged)
    }

    func test_markAcknowledgedIfWaiting_whileThinking_isNoop() {
        var pane = Pane(id: "p", title: "Claude", kind: .claude)
        pane.applyStatusTransition(to: .thinking, isCurrentlyBeingViewed: false)

        pane.markAcknowledgedIfWaiting()
        XCTAssertFalse(pane.waitingAcknowledged,
                       "The flag only matters in the waiting state.")
    }

    func test_markAcknowledgedIfWaiting_whileWaiting_setsTrue() {
        var pane = Pane(id: "p", title: "Claude", kind: .claude)
        pane.applyStatusTransition(to: .waiting, isCurrentlyBeingViewed: false)
        XCTAssertFalse(pane.waitingAcknowledged)

        pane.markAcknowledgedIfWaiting()
        XCTAssertTrue(pane.waitingAcknowledged)
    }

    func test_markAcknowledgedIfWaiting_isIdempotent() {
        var pane = Pane(id: "p", title: "Claude", kind: .claude)
        pane.applyStatusTransition(to: .waiting, isCurrentlyBeingViewed: true)
        XCTAssertTrue(pane.waitingAcknowledged)

        pane.markAcknowledgedIfWaiting()
        pane.markAcknowledgedIfWaiting()
        XCTAssertTrue(pane.waitingAcknowledged)
    }

    // MARK: - Tab aggregation

    func test_tab_waitingAcknowledged_mirrorsActivePane() {
        var claudePane = Pane(id: "claude", title: "Claude", kind: .claude)
        claudePane.applyStatusTransition(to: .waiting, isCurrentlyBeingViewed: true)
        let terminalPane = Pane(id: "term", title: "Terminal", kind: .terminal)

        var tab = Tab(
            id: "t", title: "Session", status: .waiting, cwd: "/",
            panes: [claudePane, terminalPane], activePaneId: "claude"
        )
        XCTAssertTrue(tab.waitingAcknowledged,
                      "When Claude is the active pane, the tab dot follows its acknowledgment.")

        // Flip focus to the terminal pane — its flag is false, so the
        // tab surface acts as "not acknowledged" (matches existing model
        // where tab.status already mirrors the active pane).
        tab.activePaneId = "term"
        XCTAssertFalse(tab.waitingAcknowledged)
    }

    func test_tab_waitingAcknowledged_noActivePane_isFalse() {
        let tab = Tab(
            id: "t", title: "", status: .idle, cwd: "/", panes: [], activePaneId: nil
        )
        XCTAssertFalse(tab.waitingAcknowledged)
    }
}
