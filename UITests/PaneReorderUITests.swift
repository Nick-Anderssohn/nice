//
//  PaneReorderUITests.swift
//  NiceUITests
//
//  Phase B (draggable-panes-v2): drag-to-reorder pane pills in the top
//  toolbar. The headline invariant — the one that sank the first attempt —
//  is that dragging a pill reorders it WITHOUT moving the window, even
//  though the toolbar band is otherwise window-draggable
//  (WindowDragRegion.DragView.mouseDownCanMoveWindow == true).
//
//  `testDragOnPillReordersAndDoesNotMoveWindow` is the deliberate
//  differential partner of WindowDragUITests.testEmptyToolbarDragMovesWindow:
//  same press/drag idiom, but started on a pill instead of empty chrome,
//  with the opposite required outcome (window must NOT move). The two
//  together prove the no-move result isn't vacuous.
//

import XCTest

final class PaneReorderUITests: NiceUITestCase {

    private var fakeHomeURL: URL?

    override func tearDownWithError() throws {
        if let url = fakeHomeURL {
            try? FileManager.default.removeItem(at: url)
            fakeHomeURL = nil
        }
        try super.tearDownWithError()
    }

    // MARK: - Harness (mirrors WindowDragUITests)

    private func fakeHomePath() -> String {
        if let url = fakeHomeURL { return url.path }
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-uitest-panereorder-\(UUID().uuidString)",
                isDirectory: true
            )
        try? FileManager.default.createDirectory(
            at: url, withIntermediateDirectories: true
        )
        fakeHomeURL = url
        return url.path
    }

    @discardableResult
    private func launchApp() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments += ["-ApplePersistenceIgnoreState", "YES"]
        let home = fakeHomePath()
        app.launchEnvironment["HOME"] = home
        app.launchEnvironment["NICE_APPLICATION_SUPPORT_ROOT"] =
            (home as NSString).appendingPathComponent("Library/Application Support")
        let hostEnv = ProcessInfo.processInfo.environment
        if let user = hostEnv["USER"]       { app.launchEnvironment["USER"]    = user }
        if let logname = hostEnv["LOGNAME"] { app.launchEnvironment["LOGNAME"] = logname }
        app.launch()
        track(app)
        return app
    }

    // MARK: - Pill helpers

    /// All pane-pill container buttons. Pills carry id `tab.pill.<id>` and
    /// the `.isButton` trait; their `.title`/`.titleField` sub-elements are
    /// not buttons, and the close control is `tab.close.<id>`, so querying
    /// buttons by the `tab.pill.` prefix yields exactly the pill containers.
    private func pillButtons(_ app: XCUIApplication) -> [XCUIElement] {
        app.buttons.matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "tab.pill.")
        ).allElementsBoundByIndex
    }

    /// Pane-pill ids in left-to-right display order, by frame.minX.
    private func orderedPaneIds(_ app: XCUIApplication) -> [String] {
        pillButtons(app)
            .filter { $0.exists }
            .sorted { $0.frame.minX < $1.frame.minX }
            .map { $0.identifier }
    }

    /// Add panes until the strip shows `count` pills (taps `tab.add`).
    private func growTo(_ count: Int, in app: XCUIApplication) {
        let add = app.buttons["tab.add"]
        XCTAssertTrue(add.waitForExistence(timeout: 5), "add-pane button missing")
        var guardCounter = 0
        while pillButtons(app).count < count {
            add.click()
            guardCounter += 1
            // Give the new pill a moment to mount before re-counting.
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

    // MARK: - Spike / headline guard

    /// SPIKE BASELINE (Step 0): with three pills, press-drag the first pill
    /// rightward past the second and assert the WINDOW DID NOT MOVE.
    ///
    /// Run this first against the current build (before any `.onDrag`
    /// wiring) to learn the baseline: does a pill press on `main`'s
    /// cooperative drag model already get blocked from moving the window
    /// (pills are opaque interactive SwiftUI views in front of the drag
    /// region), or does it inherit the window drag? The answer dictates
    /// what the reorder wiring must do. Once `.onDrag` reorder lands, this
    /// also gains a reorder assertion (see the integration test below).
    func testDragOnPillDoesNotMoveWindow() throws {
        // Approach under test: keep `.hiddenTitleBar` but set
        // `window.isMovable = false` (AppShellView), which disables the
        // native title-bar drag for the whole band — so a press-drag on a
        // pill can't move the window. The pill's own `.onDrag` claims the
        // drag, so the toolbar's window-drag gesture yields. (Empty-chrome
        // drag is restored separately by `windowDragGesture` in
        // WindowToolbarView — a SwiftUI `DragGesture` → `performDrag`.)
        // This test asserts the pill case: a press-drag started on a pill
        // must NOT move the window. Its differential partner,
        // WindowDragUITests.testEmptyToolbarDragMovesWindow, proves empty
        // chrome still drags — so a passing pair isn't vacuous.
        let app = launchApp()
        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 5))
        waitForFirstPill(app)
        growTo(3, in: app)

        let pills = orderedPaneIds(app)
        XCTAssertGreaterThanOrEqual(pills.count, 3, "need >= 3 pills to drag")

        let initial = window.frame
        let p0 = app.buttons[pills[0]]
        let p1 = app.buttons[pills[1]]
        XCTAssertTrue(p0.exists && p1.exists)

        // Same idiom as testEmptyToolbarDragMovesWindow, but started on a
        // pill: press the first pill's center, drag past the second pill's
        // midpoint.
        let start = p0.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        let end = p1.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.5))
        start.press(forDuration: 0.05, thenDragTo: end)

        // Negative assertion: give any (erroneous) window move time to
        // settle, then require the origin to be unchanged.
        Thread.sleep(forTimeInterval: 1.0)
        let after = window.frame
        XCTAssertEqual(
            after.origin, initial.origin,
            "Dragging a pill must NOT move the window (origin moved from \(initial.origin) to \(after.origin))"
        )
    }
}
