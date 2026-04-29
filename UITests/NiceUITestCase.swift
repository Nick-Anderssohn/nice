//
//  NiceUITestCase.swift
//  NiceUITests
//
//  Base class for every Nice UITest. Owns the lifecycle bookkeeping
//  the suite needs to behave as a good citizen — currently, the
//  spawned app must be terminated cleanly between tests so its pane
//  zsh children don't leak as PPID==1 orphans.
//
//  Without explicit `XCUIApplication.terminate()` in tearDown the
//  XCUITest harness SIGKILLs the host app between tests, and SIGKILL
//  bypasses `applicationWillTerminate` — the path that drives Nice's
//  own pty cleanup. Each leaked zsh holds a pty slot; macOS caps
//  `kern.tty.ptmx_max` at 511 so the orphans starve real
//  `forkpty()` calls after enough test runs (this was the original
//  cause of the restored-secondary-pane-hangs bug, see
//  `docs/done/`).
//
//  The shape: subclasses call `track(app)` immediately after
//  `app.launch()`. The base class records the most recent app and
//  terminates it in `tearDownWithError`. Tests that explicitly
//  terminate the app mid-test (relaunch suites) just call
//  `track(newApp)` again — the base class always tears down the
//  most recent registration, and `terminate()` on an already-
//  terminated app is a no-op.
//

import XCTest

class NiceUITestCase: XCTestCase {

    private var trackedApp: XCUIApplication?

    override func setUpWithError() throws {
        try super.setUpWithError()
        continueAfterFailure = false
    }

    override func tearDownWithError() throws {
        trackedApp?.terminate()
        trackedApp = nil
        try super.tearDownWithError()
    }

    /// Register `app` for cleanup. Call once after every
    /// `app.launch()` (including subsequent launches in a single
    /// test). The previous registration is replaced — explicit-
    /// terminate-then-relaunch tests work correctly.
    func track(_ app: XCUIApplication) {
        trackedApp = app
    }
}
