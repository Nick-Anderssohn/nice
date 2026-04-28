//
//  AppStateLaunchOverlayTests.swift
//  NiceUnitTests
//
//  Covers the "Launching…" placeholder lifecycle on AppState:
//  `registerPaneLaunch` seeds `.pending`, the grace window flips it to
//  `.visible`, and `clearPaneLaunch` removes the entry.
//
//  Tests set `launchOverlayGraceSeconds = 0` so the promotion runs
//  synchronously — no DispatchQueue spin required. One dedicated test
//  exercises the async branch with a real (small) delay to make sure
//  the production path still works.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class AppStateLaunchOverlayTests: XCTestCase {

    private var appState: AppState!
    private var homeSandbox: TestHomeSandbox!

    override func setUp() {
        super.setUp()
        homeSandbox = TestHomeSandbox()
        appState = AppState()
        // Zero grace means the pending → visible promotion fires
        // inline inside `registerPaneLaunch`, so tests that don't
        // care about the async path can assert against the final
        // state immediately.
        appState.sessions.launchOverlayGraceSeconds = 0
    }

    override func tearDown() {
        appState = nil
        homeSandbox.teardown()
        homeSandbox = nil
        super.tearDown()
    }

    func test_registerPaneLaunch_zeroGrace_immediatelyVisible() {
        appState.sessions.registerPaneLaunch(paneId: "p1", command: "claude -w foo")

        XCTAssertEqual(
            appState.sessions.paneLaunchStates["p1"],
            .visible(command: "claude -w foo"),
            "With a zero-second grace the overlay is promoted immediately."
        )
    }

    func test_clearPaneLaunch_removesVisibleEntry() {
        appState.sessions.registerPaneLaunch(paneId: "p1", command: "claude")
        XCTAssertEqual(appState.sessions.paneLaunchStates["p1"], .visible(command: "claude"))

        appState.sessions.clearPaneLaunch(paneId: "p1")

        XCTAssertNil(
            appState.sessions.paneLaunchStates["p1"],
            "First-byte clear must remove the entry entirely so the overlay stops rendering."
        )
    }

    func test_clearPaneLaunch_beforeTimerFires_suppressesOverlay() {
        // Non-zero grace so the timer is real, then clear before it
        // fires. The overlay must never reach `.visible`.
        appState.sessions.launchOverlayGraceSeconds = 0.2
        appState.sessions.registerPaneLaunch(paneId: "p1", command: "claude")
        XCTAssertEqual(appState.sessions.paneLaunchStates["p1"], .pending(command: "claude"))

        appState.sessions.clearPaneLaunch(paneId: "p1")

        // Wait past the grace window and verify the promotion DIDN'T
        // bring the entry back — the `.pending` guard inside the
        // closure must early-exit when the state is already nil.
        let exp = expectation(description: "grace window elapsed")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { exp.fulfill() }
        wait(for: [exp], timeout: 1.0)

        XCTAssertNil(
            appState.sessions.paneLaunchStates["p1"],
            "A cleared pane must stay cleared even after the grace timer fires."
        )
    }

    func test_registerPaneLaunch_asyncPath_promotesAfterGrace() {
        appState.sessions.launchOverlayGraceSeconds = 0.15
        appState.sessions.registerPaneLaunch(paneId: "p1", command: "claude -w slow")

        XCTAssertEqual(
            appState.sessions.paneLaunchStates["p1"],
            .pending(command: "claude -w slow"),
            "Before the grace window elapses the state is .pending — overlay stays hidden."
        )

        let exp = expectation(description: "overlay promoted")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { exp.fulfill() }
        wait(for: [exp], timeout: 1.0)

        XCTAssertEqual(
            appState.sessions.paneLaunchStates["p1"],
            .visible(command: "claude -w slow"),
            "After the grace window the entry is promoted to .visible."
        )
    }

    func test_registerPaneLaunch_replacesPriorEntry() {
        // A second register for the same paneId replaces the first.
        // Defends against in-place pane promotion (e.g. .resumeDeferred
        // → running-Claude) re-using an id that already had state.
        appState.sessions.registerPaneLaunch(paneId: "p1", command: "claude")
        XCTAssertEqual(appState.sessions.paneLaunchStates["p1"], .visible(command: "claude"))

        appState.sessions.registerPaneLaunch(paneId: "p1", command: "claude --resume")

        XCTAssertEqual(
            appState.sessions.paneLaunchStates["p1"],
            .visible(command: "claude --resume"),
            "Re-registering must overwrite the command string, not stack entries."
        )
    }

    func test_paneExited_clearsLaunchState() {
        // Seed a minimal project + tab so paneExited has something to
        // remove; the overlay bookkeeping runs regardless of whether
        // the tab itself dissolves.
        let paneId = "p-exit"
        let tab = Tab(
            id: "t1",
            title: "t",
            cwd: "/tmp",
            branch: nil,
            panes: [Pane(id: paneId, title: "Claude", kind: .claude)],
            activePaneId: paneId,
            claudeSessionId: nil
        )
        appState.tabs.projects.append(
            Project(id: "p", name: "P", path: "/tmp", tabs: [tab])
        )

        appState.sessions.registerPaneLaunch(paneId: paneId, command: "claude")
        XCTAssertEqual(appState.sessions.paneLaunchStates[paneId], .visible(command: "claude"))

        appState.sessions.paneExited(tabId: "t1", paneId: paneId, exitCode: 0)

        XCTAssertNil(
            appState.sessions.paneLaunchStates[paneId],
            "A pane that exits — even silently, before emitting any byte — must not leave a stale overlay entry behind."
        )
    }
}
