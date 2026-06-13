//
//  TearOffHookUITests.swift
//  NiceUITests
//
//  End-to-end coverage for the tear-off bug fixes, driven through the
//  UITest-only programmatic tear-off hooks (`--uitest-tearoff-hook` →
//  hidden buttons `test.tearOffActivePane` and `test.tearOffInactivePane`).
//  XCUITest can't synthesize the cross-window "drag onto empty desktop"
//  gesture that normally triggers a tear-off, so a hook performs a REAL
//  `PaneTearOffController.tearOff` — `test.tearOffActivePane` on the active
//  tab's ACTIVE pane, `test.tearOffInactivePane` on its first NON-ACTIVE
//  pane — producing a genuine second window.
//
//  Covers:
//    • Bug 1: a terminal torn off the TERMINALS section becomes the new
//      window's Main terminal — exactly ONE "TERMINALS" sidebar section
//      in the new window (no duplicate section).
//    • Bug 3: after the tear-off, the ORIGINAL window's now-active pane
//      renders a terminal (the `mainContent.hostedPane` element exists),
//      not a blank background.
//    • Bug A (the headline regression net): tearing off a restored-but-
//      never-focused UNSPAWNED pane (`test.tearOffInactivePane` on a
//      seeded two-pane terminal tab after relaunch) opens a real second
//      window and SPAWNS the pane there — pre-Phase-A it silently no-op'd.
//    • Bug B: the traffic-light cluster is monotonic + evenly pitched on
//      both fresh and torn-off windows, and the torn-off window's inset
//      matches the original's (all RELATIVE / pitch-based, never absolute).
//    • Bug C: a pill drag in the torn-off window doesn't move the window
//      (the reorder-and-doesn't-move net is covered window-agnostically by
//      `PaneReorderUITests`; the router fix is identical in any window).
//
//  Other agents reuse the same hooks: launch with `--uitest-tearoff-hook`,
//  grow the strip, click `test.tearOffActivePane` (or seed + relaunch and
//  click `test.tearOffInactivePane`) to open a real second window.
//

import XCTest

final class TearOffHookUITests: NiceUITestCase {

    private var fakeHomeURL: URL?

    override func tearDownWithError() throws {
        if let url = fakeHomeURL {
            try? FileManager.default.removeItem(at: url)
            fakeHomeURL = nil
        }
        try super.tearDownWithError()
    }

    // MARK: - Harness (mirrors PaneTearOffUITests)

    private func fakeHomePath() -> String {
        if let url = fakeHomeURL { return url.path }
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-uitest-tearoffhook-\(UUID().uuidString)",
                isDirectory: true
            )
        try? FileManager.default.createDirectory(
            at: url, withIntermediateDirectories: true
        )
        fakeHomeURL = url
        return url.path
    }

    @discardableResult
    private func launchApp(windowFrame: CGRect) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments += ["-ApplePersistenceIgnoreState", "YES"]
        app.launchArguments += ["--uitest-tearoff-hook"]
        let home = fakeHomePath()
        app.launchEnvironment["HOME"] = home
        app.launchEnvironment["NICE_APPLICATION_SUPPORT_ROOT"] =
            (home as NSString).appendingPathComponent("Library/Application Support")
        app.launchEnvironment["NICE_UITEST_WINDOW_FRAME"] =
            "\(windowFrame.origin.x),\(windowFrame.origin.y),\(windowFrame.width),\(windowFrame.height)"
        let hostEnv = ProcessInfo.processInfo.environment
        if let user = hostEnv["USER"]       { app.launchEnvironment["USER"]    = user }
        if let logname = hostEnv["LOGNAME"] { app.launchEnvironment["LOGNAME"] = logname }
        app.launch()
        track(app)
        return app
    }

    // MARK: - Seeded-sessions harness (mirrors MultiWindowRestoreUITests)

    /// Launch with a pre-seeded `sessions.json` sandbox AND the tear-off
    /// hook. Mirrors `MultiWindowRestoreUITests.launchAppWithSandbox` but
    /// adds `--uitest-tearoff-hook` (kept as a LOCAL private helper to
    /// avoid cross-file coupling). `HOME` / `NICE_APPLICATION_SUPPORT_ROOT`
    /// point at the same sandbox the seeded JSON was written into, so the
    /// next launch reads it and restores the seeded window — that restore
    /// is what leaves a never-focused terminal pane UNSPAWNED (the BUG A
    /// precondition a fresh launch can't produce).
    @discardableResult
    private func launchSeededAppWithHook(
        homePath: String, windowFrame: CGRect
    ) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments += ["-ApplePersistenceIgnoreState", "YES"]
        app.launchArguments += ["--uitest-tearoff-hook"]
        app.launchEnvironment["HOME"] = homePath
        app.launchEnvironment["NICE_APPLICATION_SUPPORT_ROOT"] =
            (homePath as NSString).appendingPathComponent("Library/Application Support")
        // Pin the restored window to a known origin (mirrors `launchApp`)
        // so the torn-off window can be discriminated by origin x via
        // `newlyOpenedWindow(in:originalOriginX:)` rather than relying on
        // unstable XCUITest window ordering (`windows[1]`).
        app.launchEnvironment["NICE_UITEST_WINDOW_FRAME"] =
            "\(windowFrame.origin.x),\(windowFrame.origin.y),\(windowFrame.width),\(windowFrame.height)"
        let hostEnv = ProcessInfo.processInfo.environment
        if let user = hostEnv["USER"] { app.launchEnvironment["USER"] = user }
        if let logname = hostEnv["LOGNAME"] { app.launchEnvironment["LOGNAME"] = logname }
        app.launch()
        track(app)
        return app
    }

    /// Write the on-disk `sessions.json` the next launch will read.
    /// The `Nice/` subfolder name matches what `SessionStore` constructs
    /// from `CFBundleName` (mirrors `MultiWindowRestoreUITests`).
    private func seedSessionsJson(
        at supportRoot: String, windows: [[String: Any]]
    ) throws {
        let dir = (supportRoot as NSString).appendingPathComponent("Nice")
        try FileManager.default.createDirectory(
            atPath: dir, withIntermediateDirectories: true
        )
        let payload: [String: Any] = ["version": 3, "windows": windows]
        let data = try JSONSerialization.data(
            withJSONObject: payload, options: [.prettyPrinted, .sortedKeys]
        )
        let path = (dir as NSString).appendingPathComponent("sessions.json")
        try data.write(to: URL(fileURLWithPath: path), options: .atomic)
    }

    /// One window whose ACTIVE tab is a TERMINAL tab (no `claudeSessionId`
    /// → `WindowSession`'s terminal-spawn branch) holding TWO terminal
    /// panes. `activePaneId` is the FIRST pane, so on restore only that
    /// pane's pty spawns (`makeSession(initialTerminalPaneId:)`); the
    /// SECOND pane "stays lazy until first focus" — i.e. it restores
    /// UNSPAWNED, which is precisely the BUG A precondition the inactive
    /// hook tears off. The tab lives under the reserved Terminals project
    /// so the restored sidebar reads naturally.
    private func makeTerminalTwoPaneWindowJSON() -> [String: Any] {
        let termTabId = "uitest-termtab"
        let p0 = "uitest-term-p0"
        let p1 = "uitest-term-p1"
        return [
            "id": "w-term",
            "activeTabId": termTabId,
            "sidebarCollapsed": false,
            "projects": [
                [
                    "id": "terminals",
                    "name": "Terminals",
                    "path": "/tmp",
                    "tabs": [
                        [
                            "id": termTabId,
                            "title": "Terminals",
                            "cwd": "/tmp",
                            "branch": NSNull(),
                            // ABSENT/NSNull → terminal-spawn branch; NOT
                            // the Claude branch (which would spawn the
                            // Claude pane instead and never leave a
                            // deferred terminal pane).
                            "claudeSessionId": NSNull(),
                            "activePaneId": p0,
                            "panes": [
                                [
                                    "id": p0,
                                    "title": "Terminal 1",
                                    "kind": "terminal",
                                ],
                                [
                                    "id": p1,
                                    "title": "Terminal 2",
                                    "kind": "terminal",
                                ],
                            ],
                        ] as [String: Any],
                    ],
                ] as [String: Any],
            ],
        ]
    }

    // MARK: - Pill helpers (mirror PaneTearOffUITests)

    private func pillButtons(_ app: XCUIApplication) -> [XCUIElement] {
        app.buttons.matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "tab.pill.")
        ).allElementsBoundByIndex
    }

    private func growTo(_ count: Int, in app: XCUIApplication) {
        let add = app.buttons["tab.add"]
        XCTAssertTrue(add.waitForExistence(timeout: 5), "add-pane button missing")
        var guardCounter = 0
        while pillButtons(app).count < count {
            add.click()
            guardCounter += 1
            _ = pillButtons(app).last?.waitForExistence(timeout: 2)
            XCTAssertLessThan(guardCounter, count + 5, "could not grow strip to \(count) pills")
        }
    }

    private func waitForFirstPill(_ app: XCUIApplication) {
        let firstPill = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "tab.pill.")
        ).firstMatch
        XCTAssertTrue(firstPill.waitForExistence(timeout: 5), "no pane pill mounted")
    }

    // MARK: - Test

    /// Tear off a TERMINALS-section terminal via the hook and assert:
    ///   (bug 1) the NEW window has exactly one "TERMINALS" section, and
    ///   (bug 3) the ORIGINAL window's active pane still renders a
    ///   terminal (not blank).
    func testTearOffFromTerminalsSection_singleSection_andNoBlankPane() throws {
        let app = launchApp(windowFrame: CGRect(x: 150, y: 180, width: 900, height: 640))
        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 5))
        waitForFirstPill(app)

        // Two terminal pills in the Terminals Main tab so tearing one off
        // leaves the source window's Main tab alive (its remaining pill
        // keeps a terminal active → bug-3 surface).
        growTo(2, in: app)
        XCTAssertEqual(app.windows.count, 1, "should start with exactly one window")

        // Fire the programmatic tear-off of the active pane.
        let hook = app.buttons["test.tearOffActivePane"]
        XCTAssertTrue(hook.waitForExistence(timeout: 5), "tear-off hook button missing")
        if hook.isHittable {
            hook.click()
        } else {
            // Fall back to a coordinate tap on the element's center.
            hook.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        }

        // A real second window must open to receive the torn-off pane.
        let opened = XCTNSPredicateExpectation(
            predicate: NSPredicate { _, _ in app.windows.count >= 2 },
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [opened], timeout: 8), .completed,
            "Tear-off hook should open a second window (got \(app.windows.count))"
        )

        // Bug 1: the NEW window must show exactly one "TERMINALS" section.
        // Identify the new window as the one NOT pinned to the launch
        // frame (the original window keeps NICE_UITEST_WINDOW_FRAME's
        // origin x≈150). Scope the section-header query to that window.
        let newWindow = newlyOpenedWindow(in: app, originalOriginX: 150)
        XCTAssertNotNil(newWindow, "could not identify the torn-off window")
        if let newWindow {
            // Wait for the new window's sidebar to render its terminals
            // group, then assert there's exactly one.
            let terminalsGroups = newWindow.descendants(matching: .any).matching(
                NSPredicate(format: "identifier == %@", "sidebar.group.terminals")
            )
            let appeared = XCTNSPredicateExpectation(
                predicate: NSPredicate { _, _ in terminalsGroups.count >= 1 },
                object: nil
            )
            XCTAssertEqual(
                XCTWaiter.wait(for: [appeared], timeout: 6), .completed,
                "New window must render a TERMINALS section"
            )
            XCTAssertEqual(
                terminalsGroups.count, 1,
                "New window must have EXACTLY one TERMINALS section (bug 1), got \(terminalsGroups.count)"
            )
            // The Main row (legacy `sidebar.terminals` alias) is present.
            let mainRow = newWindow.descendants(matching: .any)
                .matching(NSPredicate(format: "identifier == %@", "sidebar.terminals"))
                .firstMatch
            XCTAssertTrue(mainRow.waitForExistence(timeout: 4),
                          "New window's Main terminal row missing")
        }

        // Bug 3: the ORIGINAL window's active pane renders a terminal, not
        // a blank background. The hosted-pane element exists only when
        // `mainContent` actually has a pty view for the active pane. Scope
        // the query to the ORIGINAL window (pinned at x≈150).
        let originalWindow = app.windows.allElementsBoundByIndex.first {
            abs($0.frame.origin.x - 150) <= 5
        }
        XCTAssertNotNil(originalWindow, "could not identify the original window")
        if let originalWindow {
            let hosted = originalWindow.descendants(matching: .any)
                .matching(NSPredicate(format: "identifier == %@", "mainContent.hostedPane"))
                .firstMatch
            XCTAssertTrue(
                hosted.waitForExistence(timeout: 6),
                "Original window's active pane must render a terminal, not blank (bug 3)"
            )
        }
    }

    // MARK: - Bug A: tearing off an UNSPAWNED pane spawns it in a new window

    /// The headline BUG A end-to-end net. The active-pane hook can't
    /// express this: a fresh-launch (or active) pane is ALWAYS spawned,
    /// so it never exercises the deferred-spawn path. Here we seed a
    /// two-pane terminal tab, RELAUNCH so the second pane restores
    /// UNSPAWNED (`PaneClaim.notSpawned`), then tear THAT pane off via
    /// `test.tearOffInactivePane`.
    ///
    /// Pre-Phase-A this silently no-op'd — `detachLivePane` returned nil
    /// for a pane with no live pty, so the tear-off bailed and NO second
    /// window opened. Phase A's closed `PaneClaim` type makes the
    /// `.notSpawned(cwd:)` case spawn the pane fresh in the destination,
    /// so a real second window opens AND is non-blank.
    func testTearOffUnspawnedPane_spawnsInNewWindow() throws {
        let homePath = fakeHomePath()
        let supportRoot = (homePath as NSString)
            .appendingPathComponent("Library/Application Support")
        try seedSessionsJson(
            at: supportRoot,
            windows: [makeTerminalTwoPaneWindowJSON()]
        )

        // 1. Launch from the seed. Only p0 (the active pane) spawns; p1
        //    stays lazy/unspawned. Assert p0's pill exists (the active
        //    pane really came up). Pin the restored window at x≈150 so the
        //    torn-off window is discriminated by origin (not window order).
        let app = launchSeededAppWithHook(
            homePath: homePath,
            windowFrame: CGRect(x: 150, y: 180, width: 900, height: 640)
        )
        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 10),
                      "seeded window must come up")
        waitForFirstPill(app)
        // Both panes are MODELLED (two pills), but only p0 is spawned.
        let twoPills = XCTNSPredicateExpectation(
            predicate: NSPredicate { _, _ in self.pillButtons(app).count >= 2 },
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [twoPills], timeout: 6), .completed,
            "seeded two-pane terminal tab must restore both pane pills"
        )
        XCTAssertEqual(app.windows.count, 1, "should start with exactly one window")

        // 2. Tear off the FIRST NON-ACTIVE pane (p1, the never-focused
        //    UNSPAWNED pane).
        let hook = app.buttons["test.tearOffInactivePane"]
        XCTAssertTrue(hook.waitForExistence(timeout: 5),
                      "inactive-pane tear-off hook button missing")
        if hook.isHittable {
            hook.click()
        } else {
            hook.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        }

        // 3. A real SECOND window must open — proving the unspawned pane
        //    tore off and SPAWNED in the destination (pre-Phase-A this
        //    silently no-op'd: no second window at all).
        let opened = XCTNSPredicateExpectation(
            predicate: NSPredicate { _, _ in app.windows.count >= 2 },
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [opened], timeout: 8), .completed,
            "Tearing off an UNSPAWNED pane must open a second window (BUG A): got \(app.windows.count)"
        )

        // 4. The new window must NOT be blank: it must show a pane pill
        //    AND a hosted live terminal (`mainContent.hostedPane` exists
        //    only when a pty view is actually hosted for the active pane —
        //    the same no-blank-pane surface `testTearOffFromTerminalsSection`
        //    asserts). Identify the torn-off window deterministically by its
        //    origin (the original window is pinned at x≈150 above; the
        //    torn-off window repositions via the post-open `setFrameOrigin`,
        //    landing at a distinct origin) — NOT by unstable window ordering.
        //    Scoping the no-blank assertions to the actual torn-off window
        //    closes the false-pass hole where the still-spawned original
        //    would satisfy the predicates too.
        guard let newWindow = newlyOpenedWindow(in: app, originalOriginX: 150) else {
            return XCTFail("could not identify the torn-off window")
        }
        let newPill = newWindow.buttons.matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "tab.pill.")
        ).firstMatch
        XCTAssertTrue(
            newPill.waitForExistence(timeout: 6),
            "torn-off (formerly unspawned) pane must show its pill in the new window"
        )
        let hosted = newWindow.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", "mainContent.hostedPane"))
            .firstMatch
        XCTAssertTrue(
            hosted.waitForExistence(timeout: 8),
            "New window must render a live terminal, not the blank fallback (BUG A spawn-in-destination)"
        )

        // Positively distinguish the two windows: the ORIGINAL window
        // (pinned at x≈150) must still render its own active terminal, so
        // the test isn't merely filtering by origin but confirming BOTH
        // windows are non-blank.
        let originalWindow = app.windows.allElementsBoundByIndex.first {
            abs($0.frame.origin.x - 150) <= 5
        }
        XCTAssertNotNil(originalWindow, "could not identify the original window")
        if let originalWindow {
            let originalHosted = originalWindow.descendants(matching: .any)
                .matching(NSPredicate(format: "identifier == %@", "mainContent.hostedPane"))
                .firstMatch
            XCTAssertTrue(
                originalHosted.waitForExistence(timeout: 6),
                "Original window's active pane must still render a terminal after the tear-off"
            )
        }
    }

    // MARK: - Bug 2: traffic-light alignment in the torn-off window

    /// Tear off a pane via the hook and assert the NEW window's
    /// traffic-light (close) button sits at the SAME window-relative
    /// position as the original window's — i.e. `TrafficLightPlacer`
    /// (owned by each window's `WindowChromeController`) insets the
    /// torn-off window's buttons just like a normal window. Bug 2 left
    /// the torn-off window's buttons at the default macOS position
    /// (flush to the corner) because AppKit re-laid them out after the
    /// nudge and nothing re-applied it. The assertion is deliberately
    /// RELATIVE (torn-off matches original) so it never bakes in an
    /// OS-specific absolute position.
    func testTornOffWindowTrafficLightsMatchOriginalOffset() throws {
        let app = launchApp(windowFrame: CGRect(x: 150, y: 180, width: 900, height: 640))
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 5))
        waitForFirstPill(app)
        growTo(2, in: app)

        let originalCloseOffset = closeButtonOffset(forWindowWithOriginX: 150, in: app)
        XCTAssertNotNil(originalCloseOffset, "could not read original window's close button")

        let hook = app.buttons["test.tearOffActivePane"]
        XCTAssertTrue(hook.waitForExistence(timeout: 5), "tear-off hook button missing")
        hook.click()

        let opened = XCTNSPredicateExpectation(
            predicate: NSPredicate { _, _ in app.windows.count >= 2 }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [opened], timeout: 8), .completed,
                       "Tear-off hook should open a second window")

        // Give the new window's chrome time to settle. `TrafficLightPlacer`
        // re-resolves + re-applies on the buttons' own frame events and on
        // window focus / resize / move (`didMove` specifically covers the
        // tear-off's post-open reposition) — no timer, but the XCUITest
        // round-trip still needs a beat for those events to land.
        Thread.sleep(forTimeInterval: 1.0)

        guard let newWindow = newlyOpenedWindow(in: app, originalOriginX: 150) else {
            return XCTFail("could not identify the torn-off window")
        }
        let newOffset = closeButtonOffset(in: newWindow)
        XCTAssertNotNil(newOffset, "could not read torn-off window's close button")

        if let o = originalCloseOffset, let n = newOffset {
            XCTAssertEqual(n.dx, o.dx, accuracy: 3,
                           "torn-off window's traffic lights must share the original's horizontal inset (bug 2): original=\(o) torn-off=\(n)")
            XCTAssertEqual(n.dy, o.dy, accuracy: 3,
                           "torn-off window's traffic lights must share the original's vertical inset (bug 2): original=\(o) torn-off=\(n)")
        }
    }

    /// The close button's offset from its window's top-left corner.
    /// `nil` if the button or window can't be read.
    private func closeButtonOffset(in window: XCUIElement) -> CGVector? {
        let close = closeButton(in: window)
        guard close.exists else { return nil }
        let w = window.frame
        let c = close.frame
        return CGVector(dx: c.minX - w.minX, dy: c.minY - w.minY)
    }

    private func closeButtonOffset(
        forWindowWithOriginX x: CGFloat, in app: XCUIApplication
    ) -> CGVector? {
        guard let window = app.windows.allElementsBoundByIndex.first(where: {
            abs($0.frame.origin.x - x) <= 5
        }) else { return nil }
        return closeButtonOffset(in: window)
    }

    /// The standard close (red) window button within `window`. XCUITest
    /// surfaces the three standard window buttons as `.button` elements
    /// clustered in the very top-left corner. We pick the leftmost button
    /// in that corner, explicitly excluding our own `tab.close.*` pill
    /// close buttons and any other app chrome (which sit much further
    /// right / lower). The traffic-light cluster is the only set of
    /// buttons within the ~82pt `WindowChrome.trafficLightReservedWidth`
    /// of the window's top-left corner (on macOS 26 the zoom button sits
    /// at dx≈63, so the `dx < 80` band must span the whole cluster).
    private func closeButton(in window: XCUIElement) -> XCUIElement {
        let w = window.frame
        let corner = window.buttons.allElementsBoundByIndex.filter {
            guard $0.exists else { return false }
            // Exclude our own pill / app-chrome buttons by identifier.
            if $0.identifier.hasPrefix("tab.") { return false }
            let dx = $0.frame.minX - w.minX
            let dy = $0.frame.minY - w.minY
            return dx >= 0 && dx < 80 && dy >= 0 && dy < 40
        }.sorted { $0.frame.minX < $1.frame.minX }
        return corner.first ?? window.buttons.firstMatch
    }

    /// The THREE standard window buttons (close / miniaturize / zoom) of
    /// `window`, left-to-right by `frame.minX`. Reuses `closeButton`'s
    /// corner filter (buttons within dx<80, dy<40 of the window top-left,
    /// excluding our own `tab.*` pill ids) but keeps the whole cluster
    /// rather than just the leftmost — so callers can assert order /
    /// pitch across the trio. The `dx < 80` band (below the 82pt
    /// `WindowChrome.trafficLightReservedWidth`) spans the macOS-26
    /// cluster whose zoom button sits at dx≈63 — a tighter `dx < 50`
    /// would drop the zoom button and return only 2 buttons.
    private func cornerButtons(in window: XCUIElement) -> [XCUIElement] {
        let w = window.frame
        return window.buttons.allElementsBoundByIndex.filter {
            guard $0.exists else { return false }
            if $0.identifier.hasPrefix("tab.") { return false }
            let dx = $0.frame.minX - w.minX
            let dy = $0.frame.minY - w.minY
            return dx >= 0 && dx < 80 && dy >= 0 && dy < 40
        }.sorted { $0.frame.minX < $1.frame.minX }
    }

    /// BUG B: assert each window's traffic-light cluster is MONOTONIC
    /// (close < mini < zoom, left-to-right) and EVENLY PITCHED (the
    /// close→mini gap equals the mini→zoom gap) on BOTH a fresh and a
    /// torn-off window. `TrafficLightPlacer` recomputes an absolute target
    /// per frame event from each button's NATIVE default + 8, which
    /// PRESERVES the OS-native pitch — so the cluster stays evenly spaced
    /// on every window. The old capture-then-pin nudger intermittently
    /// left a window with DOUBLED spacing (it re-applied a stale captured
    /// origin after AppKit relaid the buttons). These assertions are
    /// deliberately RELATIVE / pitch-based — NOT a fixed absolute trio
    /// (e.g. NOT 28/48/68) and NOT a fixed pitch — so they survive
    /// OS-version changes to the native button geometry (graft 9).
    func testTrafficLightsAreMonotonicAndEqualPitch() throws {
        let app = launchApp(windowFrame: CGRect(x: 150, y: 180, width: 900, height: 640))
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 5))
        waitForFirstPill(app)
        growTo(2, in: app)

        // Fresh (original) window: monotonic + equal pitch.
        guard let originalWindow = app.windows.allElementsBoundByIndex.first(where: {
            abs($0.frame.origin.x - 150) <= 5
        }) else { return XCTFail("could not identify the original window") }
        assertMonotonicEqualPitch(in: originalWindow, label: "fresh window")

        // Tear off to get a second window, then assert the SAME invariant
        // holds for it (the placer re-resolves + re-applies on the torn-off
        // window's own frame / focus / move events).
        let hook = app.buttons["test.tearOffActivePane"]
        XCTAssertTrue(hook.waitForExistence(timeout: 5), "tear-off hook button missing")
        hook.click()

        let opened = XCTNSPredicateExpectation(
            predicate: NSPredicate { _, _ in app.windows.count >= 2 }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [opened], timeout: 8), .completed,
                       "Tear-off hook should open a second window")
        Thread.sleep(forTimeInterval: 1.0)

        guard let newWindow = newlyOpenedWindow(in: app, originalOriginX: 150) else {
            return XCTFail("could not identify the torn-off window")
        }
        assertMonotonicEqualPitch(in: newWindow, label: "torn-off window")
    }

    /// Read-only (no clicks) assertion shared by both windows: the cluster
    /// has 3 buttons, is left-to-right monotonic, and is evenly pitched
    /// (close→mini ≈ mini→zoom within 3pt). Pitch-based, not absolute.
    private func assertMonotonicEqualPitch(
        in window: XCUIElement, label: String
    ) {
        let buttons = cornerButtons(in: window)
        XCTAssertEqual(
            buttons.count, 3,
            "\(label): expected exactly 3 traffic-light buttons, got \(buttons.count)"
        )
        guard buttons.count == 3 else { return }
        let closeX = buttons[0].frame.minX
        let miniX = buttons[1].frame.minX
        let zoomX = buttons[2].frame.minX
        XCTAssertTrue(
            closeX < miniX && miniX < zoomX,
            "\(label): traffic lights must be MONOTONIC left-to-right (BUG B): close=\(closeX) mini=\(miniX) zoom=\(zoomX)"
        )
        // EQUAL PITCH — the OS-native pitch the placer preserves. Compare
        // the two gaps; do NOT assert an absolute value.
        let pitch1 = miniX - closeX
        let pitch2 = zoomX - miniX
        XCTAssertEqual(
            pitch1, pitch2, accuracy: 3,
            "\(label): traffic lights must be EVENLY PITCHED (BUG B): close→mini=\(pitch1) vs mini→zoom=\(pitch2)"
        )
    }

    // MARK: - Bug 4: pill drag must not move the torn-off window

    /// Tear off a pane, then drag the new window's pane-pill and assert the
    /// window did NOT move. BUG C: in a torn-off window the old window-drag
    /// veto failed and a pill drag dragged the whole window — now structurally
    /// impossible (a pill press hit-tests to `PaneDragHosting`, the router
    /// passes it through). Mirrors `PaneReorderUITests`' record-frame / drag /
    /// assert-unchanged pattern, scoped to the torn-off window.
    func testPillDragInTornOffWindowDoesNotMoveWindow() throws {
        let app = launchApp(windowFrame: CGRect(x: 150, y: 180, width: 900, height: 640))
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 5))
        waitForFirstPill(app)
        growTo(2, in: app)

        let hook = app.buttons["test.tearOffActivePane"]
        XCTAssertTrue(hook.waitForExistence(timeout: 5), "tear-off hook button missing")
        hook.click()

        let opened = XCTNSPredicateExpectation(
            predicate: NSPredicate { _, _ in app.windows.count >= 2 }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [opened], timeout: 8), .completed,
                       "Tear-off hook should open a second window")
        Thread.sleep(forTimeInterval: 0.8)

        guard let newWindow = newlyOpenedWindow(in: app, originalOriginX: 150) else {
            return XCTFail("could not identify the torn-off window")
        }

        // BUG C in a torn-off window: a pane-pill press-drag must never move
        // the window. (The bug: in a torn-off window the old `WindowDragGate`
        // veto failed and a pill drag dragged the whole window.) The torn-off
        // window has one pane-pill — that alone exercises the window-move
        // invariant: the press hit-tests to the pill's `PaneDragHosting`
        // view, `ChromeEventRouter` passes it through (pill precedence), and
        // the window must not move.
        //
        // We do NOT grow the strip via `tab.add` here: the torn-off window
        // opens overlapping the original, so `newWindow`-scoped clicks at
        // overlapping coordinates land on the original in front (tab.add
        // clicks grew the strip by 0), and moving the original far enough to
        // clear it pushes its own off-screen chrome out of reach — the
        // two-window geometry isn't reliably controllable from XCUITest. The
        // window-agnostic REORDER + frame-unchanged net lives in
        // `PaneReorderUITests` (the router fix is identical in any window);
        // the single-pill drag here asserts the window-move invariant in the
        // torn-off context specifically.
        let pill = newWindow.buttons.matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "tab.pill.")
        ).firstMatch
        XCTAssertTrue(pill.waitForExistence(timeout: 5),
                      "torn-off window should show its pane's pill")

        // Focus the torn-off window with a COORDINATE click on its pill row
        // (a coordinate click bypasses element hittability and brings the
        // window forward; dy:0.04 ≈ window-y 26, the pill row).
        newWindow.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.04)).click()
        Thread.sleep(forTimeInterval: 0.4)

        let before = newWindow.frame
        // Press-drag the pill horizontally, staying INSIDE the torn-off
        // window's toolbar (releasing over the window's own chrome snaps the
        // pill back; only a release over empty desktop tears off again). A
        // pill press the router passes through must not drag the window.
        let start = pill.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        let end = start.withOffset(CGVector(dx: 120, dy: 0))
        start.press(forDuration: 0.1, thenDragTo: end)

        // Give any (erroneous) window move time to settle, then require the
        // torn-off window's origin unchanged.
        Thread.sleep(forTimeInterval: 1.0)
        let after = newWindow.frame
        XCTAssertEqual(
            after.origin, before.origin,
            "Dragging a pill in the torn-off window must NOT move the window (BUG C): moved from \(before.origin) to \(after.origin)"
        )
    }

    /// The window whose origin.x differs from the pinned launch frame's
    /// origin (the original window is pinned by NICE_UITEST_WINDOW_FRAME).
    private func newlyOpenedWindow(
        in app: XCUIApplication, originalOriginX: CGFloat
    ) -> XCUIElement? {
        let windows = app.windows.allElementsBoundByIndex
        for w in windows where abs(w.frame.origin.x - originalOriginX) > 5 {
            return w
        }
        // Fallback: if both share an x, pick the second window.
        return windows.count >= 2 ? windows[1] : nil
    }
}
