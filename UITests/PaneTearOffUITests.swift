//
//  PaneTearOffUITests.swift
//  NiceUITests
//
//  Drives the REAL tear-off gesture end-to-end: press-drag a pane pill
//  off the window onto empty desktop and assert a new window opens to
//  receive the torn-off pane. This is the one test that exercises the
//  AppKit `PaneDragSource` path SwiftUI can't reach — the
//  `draggingSession(_:endedAt:operation:)` callback firing with
//  `operation == []` and a release point outside every window.
//
//  Measures Spike A's open question: can XCUITest's synthesized drag
//  drive `NSDraggingSource`'s ended-outside path? The pill drag uses the
//  same press/drag idiom as `PaneReorderUITests`, but releases OUTSIDE
//  the pinned window frame instead of over a sibling pill. The window is
//  pinned via `NICE_UITEST_WINDOW_FRAME` so the release coordinate is
//  deterministically off-window (to the right of the right edge, where a
//  standard dev screen has desktop).
//
//  The migration controller + seed-consumption path is already covered
//  end-to-end by `PaneTearOffControllerTests` (unit); this test pins the
//  remaining behavioural unknown: that the gesture actually triggers it.
//

import XCTest

final class PaneTearOffUITests: NiceUITestCase {

    private var fakeHomeURL: URL?

    override func tearDownWithError() throws {
        if let url = fakeHomeURL {
            try? FileManager.default.removeItem(at: url)
            fakeHomeURL = nil
        }
        try super.tearDownWithError()
    }

    // MARK: - Harness (mirrors WindowDragUITests / PaneReorderUITests)

    private func fakeHomePath() -> String {
        if let url = fakeHomeURL { return url.path }
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-uitest-tearoff-\(UUID().uuidString)",
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

    // MARK: - Pill helpers (mirror PaneReorderUITests)

    private func pillButtons(_ app: XCUIApplication) -> [XCUIElement] {
        app.buttons.matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "tab.pill.")
        ).allElementsBoundByIndex
    }

    private func orderedPaneIds(_ app: XCUIApplication) -> [String] {
        pillButtons(app)
            .filter { $0.exists }
            .sorted { $0.frame.minX < $1.frame.minX }
            .map { $0.identifier }
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

    // MARK: - Tear-off

    /// Press-drag a pill to a point OUTSIDE the pinned window frame and
    /// assert a second window opens (the torn-off pane's new home). Two
    /// pills are grown first so the SOURCE window survives the tear-off
    /// (its remaining pill keeps it alive) and the window count goes
    /// cleanly 1 → 2.
    func testDragPillToEmptyDesktopOpensNewWindow() throws {
        // Pin a sub-screen frame with desktop to the right of the right
        // edge (x: 150 + width 820 = 970; a standard dev screen is wider).
        let app = launchApp(windowFrame: CGRect(x: 150, y: 180, width: 820, height: 600))
        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 5))
        waitForFirstPill(app)
        growTo(2, in: app)

        XCTAssertEqual(app.windows.count, 1, "should start with exactly one window")
        let ids = orderedPaneIds(app)
        XCTAssertGreaterThanOrEqual(ids.count, 2, "need >= 2 pills so the source window survives")

        // Drag the last pill off the right edge onto empty desktop:
        // ~200pt past the window's right edge, near the top-bar row.
        let pill = app.buttons[ids.last!]
        XCTAssertTrue(pill.exists)
        let start = pill.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        let end = window
            .coordinate(withNormalizedOffset: CGVector(dx: 1.0, dy: 0.05))
            .withOffset(CGVector(dx: 200, dy: 0))
        start.press(forDuration: 0.08, thenDragTo: end)

        // A new window should open to receive the torn-off pane.
        let opened = XCTNSPredicateExpectation(
            predicate: NSPredicate { _, _ in app.windows.count >= 2 },
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [opened], timeout: 6), .completed,
            "Dragging a pill onto empty desktop should open a new window (got \(app.windows.count))"
        )
    }

    /// A pill drag that ends INSIDE the window (released over the body)
    /// must (a) NOT tear off — tear-off only fires when released outside
    /// every window — and (b) leave the window-drag gate cleared, so a
    /// subsequent empty-chrome drag still moves the window. (b) guards the
    /// most dangerous failure mode of the gate: getting stuck raised after
    /// a pill drag, which would silently freeze window dragging. The
    /// precise `.withdraw` vs `.ignore` classification is unit-tested in
    /// `PaneDragEndTests`; here we only assert the two robust outcomes.
    func testPillDragReleasedInWindowDoesNotTearOffAndLeavesWindowDraggable() throws {
        let app = launchApp(windowFrame: CGRect(x: 150, y: 180, width: 820, height: 600))
        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 5))
        waitForFirstPill(app)
        growTo(2, in: app)
        XCTAssertEqual(app.windows.count, 1)

        // Drag a pill straight down into the window body (inside the
        // window frame, well below the pill strip → not a drop target) and
        // release. Expected: withdraw — pane stays, no new window.
        let pill = app.buttons[orderedPaneIds(app).last!]
        let pillStart = pill.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        let inWindow = pillStart.withOffset(CGVector(dx: 0, dy: 240))
        pillStart.press(forDuration: 0.08, thenDragTo: inWindow)

        // Give any (erroneous) tear-off time to open a window; require none.
        Thread.sleep(forTimeInterval: 1.0)
        XCTAssertEqual(
            app.windows.count, 1,
            "Releasing a pill inside the window must withdraw, not tear off"
        )

        // Now the gate must be cleared: an empty-chrome drag still moves
        // the window. Same idiom as WindowDragUITests — a point ~120pt from
        // the right edge, ~26pt down, lands in the strip's empty trailing
        // area (a window-drag surface, not a widget).
        let frame = window.frame
        let dx = (frame.width - 120) / frame.width
        let dy = 26.0 / frame.height
        let chrome = window.coordinate(withNormalizedOffset: CGVector(dx: dx, dy: dy))
        let initial = window.frame
        chrome.press(forDuration: 0.05, thenDragTo: chrome.withOffset(CGVector(dx: 70, dy: 70)))

        let moved = XCTNSPredicateExpectation(
            predicate: NSPredicate { _, _ in window.frame.origin != initial.origin },
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [moved], timeout: 2), .completed,
            "Empty-chrome drag must still move the window after a pill drag (gate must have cleared)"
        )
    }
}
