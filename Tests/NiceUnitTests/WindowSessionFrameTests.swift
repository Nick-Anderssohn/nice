//
//  WindowSessionFrameTests.swift
//  NiceUnitTests
//
//  Coverage for window-frame persistence: `snapshotPersistedWindow`
//  reads `window.frame` when an `NSWindow` has been wired in by the
//  view layer's `WindowAccessor`, and `restoreSavedWindow` calls
//  `setFrame` on that same window when the adopted snapshot carries
//  a frame.
//
//  Tests construct bare `NSWindow` instances directly — no SwiftUI
//  scene needed — and pass them in via `WindowSession.window`. The
//  test host runs as a plain AppKit app loop (see `NiceAppLauncher`),
//  so `NSWindow.init(contentRect:…)` works out of the box.
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class WindowSessionFrameTests: XCTestCase {

    private var fake: FakeSessionStore!
    private var tabs: TabModel!
    private var sessions: SessionsModel!
    private var sidebar: SidebarModel!
    private var ledger: WindowClaimLedger!

    override func setUp() {
        super.setUp()
        fake = FakeSessionStore()
        tabs = TabModel(initialMainCwd: "/tmp/nice-frame-tests")
        sessions = SessionsModel(tabs: tabs)
        sidebar = SidebarModel(initialCollapsed: false, initialMode: .tabs)
        ledger = WindowClaimLedger()
    }

    override func tearDown() {
        sessions?.tearDown()
        sessions = nil
        tabs = nil
        sidebar = nil
        fake = nil
        ledger = nil
        super.tearDown()
    }

    // MARK: - Snapshot

    func test_snapshot_includesFrame_whenWindowIsSet() {
        // Wire a real NSWindow with a known frame; the snapshot must
        // capture it verbatim. AppShellHost's WindowAccessor sets
        // `windowSession.window` once AppKit hands SwiftUI an
        // NSWindow — this test pins down the read-side contract.
        let frame = NSRect(x: 120, y: 240, width: 800, height: 600)
        let window = makeWindow(frame: frame)
        let ws = makeWindowSession()
        ws.window = window

        let snap = ws.snapshotPersistedWindow()

        let savedFrame = try? XCTUnwrap(snap.frame)
        XCTAssertEqual(savedFrame?.x, 120)
        XCTAssertEqual(savedFrame?.y, 240)
        XCTAssertEqual(savedFrame?.width, 800)
        XCTAssertEqual(savedFrame?.height, 600)
    }

    func test_snapshot_frameNil_whenWindowMissing() {
        // Saves can fire before WindowAccessor wires up the NSWindow
        // (very early in scene-graph init). Those rare snapshots
        // must persist `frame: nil`, not crash, not synthesize a
        // bogus value. The restored window then falls back to
        // SwiftUI's default placement.
        let ws = makeWindowSession()
        XCTAssertNil(ws.window, "Precondition: window starts unset.")
        let snap = ws.snapshotPersistedWindow()
        XCTAssertNil(snap.frame, "Snapshot must persist `frame: nil` when no NSWindow is wired in.")
    }

    func test_snapshot_reflectsLatestWindowFrame() {
        // The user resizes / moves the window; subsequent snapshots
        // must reflect the new frame, not cache the old one. (The
        // weak ref is read on each call — pin that down.)
        let window = makeWindow(frame: NSRect(x: 0, y: 0, width: 400, height: 300))
        let ws = makeWindowSession()
        ws.window = window

        XCTAssertEqual(ws.snapshotPersistedWindow().frame?.width, 400)

        window.setFrame(NSRect(x: 50, y: 60, width: 1024, height: 768), display: false)
        let updated = ws.snapshotPersistedWindow().frame
        XCTAssertEqual(updated?.x, 50)
        XCTAssertEqual(updated?.y, 60)
        XCTAssertEqual(updated?.width, 1024)
        XCTAssertEqual(updated?.height, 768)
    }

    // MARK: - Restore

    func test_restore_appliesSavedFrameToWindow() {
        // Adopt a saved entry that carries a non-nil frame. The
        // window we wire in starts at a different size — restore
        // must call setFrame so the user gets back the size/position
        // they had at quit. AppKit may snap the rect to its
        // constraints (min content size, screen visible bounds), so
        // the assertion compares the post-setFrame value the OS
        // accepted, not the literal saved value.
        let claude = makePersistedClaudeTab(id: "t-frame", sessionId: "sid-frame")
        let saved = PersistedWindow(
            id: "win-frame",
            activeTabId: "t-frame",
            sidebarCollapsed: false,
            projects: [
                makeEmptyTerminalsProject(),
                makePersistedProject(id: "proj-frame", tabs: [claude]),
            ],
            frame: PersistedFrame(x: 200, y: 300, width: 900, height: 700)
        )
        fake.state = PersistedState(
            version: PersistedState.currentVersion, windows: [saved]
        )

        let window = makeWindow(frame: NSRect(x: 0, y: 0, width: 400, height: 300))
        let ws = makeWindowSession(windowSessionId: "win-frame")
        ws.window = window
        ws.restoreSavedWindow()

        // What setFrame actually applies. AppKit may clamp into
        // visible screen bounds on hosts with small displays — but
        // the saved values are well within typical test-host
        // dimensions, so equality should hold.
        XCTAssertEqual(window.frame.origin.x, 200)
        XCTAssertEqual(window.frame.origin.y, 300)
        XCTAssertEqual(window.frame.size.width, 900)
        XCTAssertEqual(window.frame.size.height, 700)
    }

    func test_restore_skipsFrameWhenSavedFrameIsNil() {
        // A v3 session file written before frame persistence existed
        // has `frame == nil`. Restore must not touch the window's
        // frame in that case — the user gets SwiftUI's default
        // placement, same as today's behavior.
        let claude = makePersistedClaudeTab(id: "t-no-frame", sessionId: "sid-nf")
        let saved = PersistedWindow(
            id: "win-no-frame",
            activeTabId: "t-no-frame",
            sidebarCollapsed: false,
            projects: [
                makeEmptyTerminalsProject(),
                makePersistedProject(id: "proj-nf", tabs: [claude]),
            ],
            frame: nil
        )
        fake.state = PersistedState(
            version: PersistedState.currentVersion, windows: [saved]
        )

        let initialFrame = NSRect(x: 17, y: 19, width: 480, height: 320)
        let window = makeWindow(frame: initialFrame)
        let ws = makeWindowSession(windowSessionId: "win-no-frame")
        ws.window = window
        ws.restoreSavedWindow()

        XCTAssertEqual(window.frame, initialFrame,
                       "frame == nil on the saved entry must leave the live window untouched.")
    }

    // MARK: - Helpers

    private func makeWindow(frame: NSRect) -> NSWindow {
        // `.borderless` keeps AppKit's content-area math out of the
        // way — `window.frame` and `setFrame` operate on the same
        // rect we passed in (no titlebar inset). Defer-display so
        // the unit-test process never tries to actually paint.
        // `isReleasedWhenClosed = false` is load-bearing: the default
        // (`true`) makes AppKit autorelease on close, which then
        // dangles when XCTest's post-test autorelease-pool pop
        // sweeps and trips the memory checker. Same convention as
        // WindowRegistryTests.makeWindow.
        let w = NSWindow(
            contentRect: frame,
            styleMask: [.borderless, .resizable],
            backing: .buffered,
            defer: true
        )
        w.isReleasedWhenClosed = false
        return w
    }

    private func makeWindowSession(
        windowSessionId: String = "win-frame-tests"
    ) -> WindowSession {
        let ws = WindowSession(
            tabs: tabs,
            sessions: sessions,
            sidebar: sidebar,
            windowSessionId: windowSessionId,
            persistenceEnabled: true,
            store: fake,
            claimLedger: ledger
        )
        ws.markInitializationComplete()
        return ws
    }

    private func makeEmptyTerminalsProject() -> PersistedProject {
        PersistedProject(
            id: TabModel.terminalsProjectId,
            name: "Terminals",
            path: "/tmp/nice-frame-tests",
            tabs: []
        )
    }

    private func makePersistedClaudeTab(id: String, sessionId: String) -> PersistedTab {
        let claudePaneId = "\(id)-claude"
        return PersistedTab(
            id: id,
            title: "Claude tab",
            cwd: "/tmp/nice-frame-tests",
            branch: nil,
            claudeSessionId: sessionId,
            activePaneId: claudePaneId,
            panes: [
                PersistedPane(id: claudePaneId, title: "Claude", kind: .claude),
            ]
        )
    }

    private func makePersistedProject(id: String, tabs: [PersistedTab]) -> PersistedProject {
        PersistedProject(
            id: id, name: id.uppercased(),
            path: "/tmp/nice-frame-tests/\(id)",
            tabs: tabs
        )
    }
}
