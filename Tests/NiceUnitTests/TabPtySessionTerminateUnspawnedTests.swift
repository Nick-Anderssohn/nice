//
//  TabPtySessionTerminateUnspawnedTests.swift
//  NiceUnitTests
//
//  Pins down `TabPtySession.terminatePane(id:)` for panes whose
//  `armDeferredSpawn` was captured but never fired — i.e. the
//  view never got a real frame inside a window so
//  `firePendingSpawnIfReady` is still gated. The original bug:
//  a sidebar tab restored from a previous session with a
//  `.resumeDeferred` Claude pane that the user never focused
//  could not be closed via right-click → Close. The pane's
//  entry existed (so `paneIsSpawned` saw it) but `shellPid == 0`,
//  so the `pid > 0` guard inside `terminatePane` silently
//  returned without firing `onPaneExit`. The model never saw
//  the exit, the pane stayed in `tab.panes`, the tab welded to
//  the sidebar.
//
//  These tests construct a `TabPtySession` with no window (the
//  NiceTerminalView frame stays `.zero`) and assert that
//  `terminatePane` cancels the pending spawn, fires `onPaneExit`
//  synchronously, and leaves the entry removable via
//  `removePane`. They also pin the held-pane and live-pane
//  branches as a regression boundary so a future refactor that
//  reorders the fast paths can't quietly skip the new branch.
//

import AppKit
import XCTest
@testable import Nice

@MainActor
final class TabPtySessionTerminateUnspawnedTests: XCTestCase {

    // Shared callback recorder. Reset in `setUp`. The session also
    // requires an `onPaneTitleChange` callback at construction; that
    // path is not exercised by these tests, so the helper passes a
    // no-op rather than a recorder.
    private var paneExitCalls: [(paneId: String, code: Int32?)] = []

    override func setUp() {
        super.setUp()
        paneExitCalls = []
    }

    // MARK: - The bug repro

    func test_terminatePane_resumeDeferredClaude_neverFired_firesOnPaneExit() {
        // The exact repro: a restored Claude tab with a deferred-resume
        // pane that the user never focused. `initialClaudePaneId` +
        // `.resumeDeferred` runs `spawnClaudePane` from `init`, which
        // calls `armDeferredSpawn`. The view's frame is .zero and it
        // has no window, so the gate stays closed.
        let paneId = "claude-pane"
        let session = makeSession(
            initialClaudePaneId: paneId,
            claudeSessionMode: .resumeDeferred(id: "00000000-0000-0000-0000-000000000000")
        )

        guard let view = session.view(forPane: paneId) as? NiceTerminalView else {
            return XCTFail("Expected a NiceTerminalView for the resumeDeferred Claude pane")
        }
        XCTAssertNotNil(view.pendingSpawn,
                        "armDeferredSpawn should have captured args for the never-focused tab")
        XCTAssertFalse(view.hasFiredPendingSpawn,
                       "Gate must stay closed while frame is .zero and view is detached")
        XCTAssertFalse(view.process.running,
                       "No child should be forked before the gate fires")
        XCTAssertTrue(session.hasPane(paneId),
                      "Entry must exist immediately on creation, even though spawn is deferred")

        session.terminatePane(id: paneId)

        XCTAssertEqual(paneExitCalls.count, 1,
                       "terminatePane must synthesize a single onPaneExit for an unspawned pane — without it the model never sees the close and the tab welds to the sidebar")
        XCTAssertEqual(paneExitCalls.first?.paneId, paneId)
        XCTAssertNil(paneExitCalls.first?.code,
                     "No real child ever ran; nil exit code mirrors the held-pane fast path's nil-when-absent contract")
        XCTAssertNil(view.pendingSpawn,
                     "Pending spawn must be cancelled so a layout pass mid-teardown can't fork a child after we've declared the pane gone")
        XCTAssertFalse(view.hasFiredPendingSpawn,
                       "We cancelled, we did not fire — leaving the gate closed is the whole point")
    }

    func test_terminatePane_unspawnedTerminalPane_firesOnPaneExit() {
        // Generic case: any pane created via `addTerminalPane` is
        // armed-deferred until layout. Same fast path; this test
        // pins that the fix isn't accidentally Claude-only.
        let session = makeSession()
        let paneId = "terminal-pane"
        _ = session.addTerminalPane(id: paneId)

        guard let view = session.view(forPane: paneId) as? NiceTerminalView else {
            return XCTFail("Expected a NiceTerminalView for the addTerminalPane pane")
        }
        XCTAssertNotNil(view.pendingSpawn)
        XCTAssertFalse(view.hasFiredPendingSpawn)

        session.terminatePane(id: paneId)

        XCTAssertEqual(paneExitCalls.map(\.paneId), [paneId])
        XCTAssertNil(paneExitCalls.first?.code)
        XCTAssertNil(view.pendingSpawn)
    }

    func test_terminatePane_thenWindowAttach_doesNotForkChild() {
        // Pin the load-bearing claim in `terminatePane`'s comment:
        // after cancellation, a layout pass mid-teardown (the bug's
        // trigger — AppKit can attach the view to a sized window
        // even as we're tearing down) must NOT fire the gate. This
        // is the real invariant; the `pendingSpawn == nil` checks
        // in the other tests are a proxy. Without the cancel, the
        // contentView assignment below would call setFrameSize with
        // a non-zero size + viewDidMoveToWindow, both of which call
        // `firePendingSpawnIfReady` — which would happily fork
        // /bin/zsh in the test process if `pendingSpawn` survived.
        let session = makeSession()
        let paneId = "no-resurrection"
        _ = session.addTerminalPane(id: paneId)

        guard let view = session.view(forPane: paneId) as? NiceTerminalView else {
            return XCTFail("Expected a NiceTerminalView")
        }
        XCTAssertNotNil(view.pendingSpawn, "preflight: spawn must be armed before the test acts")

        session.terminatePane(id: paneId)

        // Now simulate the layout pass that triggered the bug:
        // attach the view to a sized window's contentView, which
        // fires setFrameSize(realSize) + viewDidMoveToWindow.
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 200, height: 200),
            styleMask: [.titled],
            backing: .buffered,
            defer: false
        )
        window.isReleasedWhenClosed = false
        window.contentView = view

        XCTAssertFalse(
            view.process.running,
            "Layout pass after cancellation must not fork a child — that was the regression vector"
        )
        XCTAssertFalse(
            view.hasFiredPendingSpawn,
            "Gate must remain unfired post-cancel; firePendingSpawnIfReady short-circuits when pendingSpawn == nil"
        )
        XCTAssertNil(view.pendingSpawn,
                     "Cancellation must persist across the layout pass — nothing should re-arm the gate")
    }

    func test_terminatePane_thenRemovePane_clearsEntry() {
        // The real `paneExited` handler in SessionsModel calls
        // `removePane` after recording the exit. Mirror that here so
        // the test pins the full single-removal-point invariant
        // (`TabPtySession.swift:48-62`): after the synthesized exit
        // and removePane, the pane is fully gone from the session
        // dict — no orphan entry that a recycled pane id could
        // misroute through.
        let session = makeSession()
        let paneId = "pane-clear"
        _ = session.addTerminalPane(id: paneId)
        XCTAssertTrue(session.hasPane(paneId))

        session.terminatePane(id: paneId)
        session.removePane(id: paneId)

        XCTAssertFalse(session.hasPane(paneId),
                       "removePane after the synthesized exit must drop the entry so a future pane id reuse starts clean")
    }

    // MARK: - Branch boundaries

    func test_terminatePane_unknownId_isNoOp() {
        // The outermost `guard var entry = entries[id] else { return }`
        // must keep its no-op semantics for ids that were never
        // created (or already removed). Otherwise a stray
        // double-close from the UI would synthesize a phantom exit.
        let session = makeSession()

        session.terminatePane(id: "never-created")

        XCTAssertTrue(paneExitCalls.isEmpty,
                      "Unknown pane id must not synthesize an exit")
    }

    // MARK: - Helpers

    /// Build a `TabPtySession` with our recording callbacks. Defaults
    /// avoid touching the filesystem or invoking any real claude
    /// binary; the test never lets a deferred spawn fire so no shell
    /// is forked.
    private func makeSession(
        initialClaudePaneId: String? = nil,
        claudeSessionMode: TabPtySession.ClaudeSessionMode = .none
    ) -> TabPtySession {
        TabPtySession(
            tabId: "test-tab",
            cwd: NSTemporaryDirectory(),
            claudeBinary: nil,
            extraClaudeArgs: [],
            initialClaudePaneId: initialClaudePaneId,
            initialTerminalPaneId: nil,
            socketPath: nil,
            zdotdirPath: nil,
            userZDotDir: nil,
            claudeSessionMode: claudeSessionMode,
            onPaneExit: { [weak self] paneId, code in
                self?.paneExitCalls.append((paneId, code))
            },
            onPaneTitleChange: { _, _ in }
        )
    }
}
