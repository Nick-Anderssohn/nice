//
//  TearOffHookUITests.swift
//  NiceUITests
//
//  End-to-end coverage for the tear-off bug fixes, driven through the
//  UITest-only programmatic tear-off hook (`--uitest-tearoff-hook` →
//  hidden button `test.tearOffActivePane`). XCUITest can't synthesize the
//  cross-window "drag onto empty desktop" gesture that normally triggers
//  a tear-off, so the hook performs a REAL `PaneTearOffController.tearOff`
//  on the active tab's active pane — producing a genuine second window.
//
//  Covers:
//    • Bug 1: a terminal torn off the TERMINALS section becomes the new
//      window's Main terminal — exactly ONE "TERMINALS" sidebar section
//      in the new window (no duplicate section).
//    • Bug 3: after the tear-off, the ORIGINAL window's now-active pane
//      renders a terminal (the `mainContent.hostedPane` element exists),
//      not a blank background.
//
//  Other agents (bugs 2 & 4) reuse the same hook: launch with
//  `--uitest-tearoff-hook`, grow the strip, click the
//  `test.tearOffActivePane` button to open a real second window.
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

    // MARK: - Bug 2: traffic-light alignment in the torn-off window

    /// Tear off a pane via the hook and assert the NEW window's
    /// traffic-light (close) button sits at the SAME window-relative
    /// position as the original window's — i.e. the `TrafficLightNudger`
    /// inset applied to the torn-off window just like a normal window.
    /// Bug 2 left the torn-off window's buttons at the default macOS
    /// position (flush to the corner) because AppKit re-laid them out
    /// after the nudge and nothing re-applied it.
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

        // Give the new window's chrome time to settle (the nudge fix
        // re-applies on a short deferred schedule + on window move).
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
    /// buttons within ~40pt of the window's top-left corner.
    private func closeButton(in window: XCUIElement) -> XCUIElement {
        let w = window.frame
        let corner = window.buttons.allElementsBoundByIndex.filter {
            guard $0.exists else { return false }
            // Exclude our own pill / app-chrome buttons by identifier.
            if $0.identifier.hasPrefix("tab.") { return false }
            let dx = $0.frame.minX - w.minX
            let dy = $0.frame.minY - w.minY
            return dx >= 0 && dx < 50 && dy >= 0 && dy < 40
        }.sorted { $0.frame.minX < $1.frame.minX }
        return corner.first ?? window.buttons.firstMatch
    }

    // MARK: - Bug 4: pill drag must not move the torn-off window

    /// Tear off a pane, add pills to the NEW window, then drag a pill in
    /// the new window and assert the window did NOT move. Bug 4: in a
    /// torn-off window the window-drag veto failed and a pill drag dragged
    /// the whole window. Mirrors `PaneReorderUITests`' record-frame /
    /// drag / assert-unchanged pattern, scoped to the torn-off window.
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

        // Bring the new window to front and add pills to it (so there are
        // multiple pills to reorder — the user's exact repro: "add a
        // couple more tabs to that new window").
        newWindow.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.02)).click()
        Thread.sleep(forTimeInterval: 0.4)
        let newAdd = newWindow.buttons["tab.add"]
        if newAdd.waitForExistence(timeout: 4) {
            newAdd.click(); Thread.sleep(forTimeInterval: 0.3)
            newAdd.click(); Thread.sleep(forTimeInterval: 0.3)
        }

        let newPills = newWindow.buttons.matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "tab.pill.")
        ).allElementsBoundByIndex.filter { $0.exists }
            .sorted { $0.frame.minX < $1.frame.minX }
        XCTAssertGreaterThanOrEqual(newPills.count, 2,
                                    "need >= 2 pills in the torn-off window to drag")

        let before = newWindow.frame
        let start = newPills[0].coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        let end = newPills[1].coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.5))
        start.press(forDuration: 0.05, thenDragTo: end)

        // Give any (erroneous) window move time to settle, then require
        // the origin unchanged.
        Thread.sleep(forTimeInterval: 1.0)
        let after = newWindow.frame
        XCTAssertEqual(
            after.origin, before.origin,
            "Dragging a pill in the torn-off window must NOT move the window (bug 4): moved from \(before.origin) to \(after.origin)"
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
