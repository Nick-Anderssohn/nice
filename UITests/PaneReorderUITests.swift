//
//  PaneReorderUITests.swift
//  NiceUITests
//
//  Phase B (draggable-panes-v2): drag-to-reorder pane pills in the top
//  toolbar. The headline invariant — the one that sank the first attempt —
//  is that dragging a pill reorders it WITHOUT moving the window, even
//  though the toolbar band is otherwise window-draggable (a press on empty
//  chrome hit-tests to `ChromeDragStripView` and `ChromeEventRouter` moves
//  the window; a press on a pill hit-tests to a `PaneDragHosting` view and
//  the router passes it through, so the pill drag reorders instead).
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

    // MARK: - Drag helpers

    /// Press-drag from `from`'s normalized offset to `to`'s normalized
    /// offset using the same idiom as the window-drag guard.
    private func drag(
        _ from: XCUIElement, _ fromOffset: CGVector,
        _ to: XCUIElement, _ toOffset: CGVector
    ) {
        let start = from.coordinate(withNormalizedOffset: fromOffset)
        let end = to.coordinate(withNormalizedOffset: toOffset)
        start.press(forDuration: 0.05, thenDragTo: end)
    }

    /// Poll `orderedPaneIds` until it equals `expected` (the reorder is
    /// committed on the next runloop tick after the drop, so it isn't
    /// observable synchronously).
    @discardableResult
    private func waitForOrder(
        _ app: XCUIApplication, _ expected: [String], timeout: TimeInterval = 5
    ) -> Bool {
        let predicate = NSPredicate { _, _ in self.orderedPaneIds(app) == expected }
        let expectation = XCTNSPredicateExpectation(predicate: predicate, object: nil)
        return XCTWaiter.wait(for: [expectation], timeout: timeout) == .completed
    }

    // MARK: - Reorder

    /// Drag pill[0] rightward past pill[1]'s midpoint → it lands after
    /// pill[1]. Asserts both the new order and that the window stayed put.
    func testDragPillRightReorders() throws {
        let app = launchApp()
        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 5))
        waitForFirstPill(app)
        growTo(3, in: app)

        let before = orderedPaneIds(app)
        XCTAssertEqual(before.count, 3, "need exactly 3 pills")
        let initial = window.frame

        drag(app.buttons[before[0]], CGVector(dx: 0.5, dy: 0.5),
             app.buttons[before[1]], CGVector(dx: 0.9, dy: 0.5))

        let expected = [before[1], before[0], before[2]]
        XCTAssertTrue(
            waitForOrder(app, expected),
            "expected \(expected), got \(orderedPaneIds(app))"
        )
        XCTAssertEqual(window.frame.origin, initial.origin, "reorder must not move window")
    }

    /// Drag pill[2] leftward over pill[0]'s left half → it lands before
    /// pill[0].
    func testDragPillLeftReorders() throws {
        let app = launchApp()
        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 5))
        waitForFirstPill(app)
        growTo(3, in: app)

        let before = orderedPaneIds(app)
        XCTAssertEqual(before.count, 3)
        let initial = window.frame

        drag(app.buttons[before[2]], CGVector(dx: 0.5, dy: 0.5),
             app.buttons[before[0]], CGVector(dx: 0.1, dy: 0.5))

        let expected = [before[2], before[0], before[1]]
        XCTAssertTrue(
            waitForOrder(app, expected),
            "expected \(expected), got \(orderedPaneIds(app))"
        )
        XCTAssertEqual(window.frame.origin, initial.origin, "reorder must not move window")
    }

    /// Drag pill[0] past the last pill's trailing edge → it becomes last
    /// (exercises the after-last slot).
    func testDragPillToEnd() throws {
        let app = launchApp()
        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 5))
        waitForFirstPill(app)
        growTo(3, in: app)

        let before = orderedPaneIds(app)
        XCTAssertEqual(before.count, 3)

        drag(app.buttons[before[0]], CGVector(dx: 0.5, dy: 0.5),
             app.buttons[before[2]], CGVector(dx: 0.95, dy: 0.5))

        let expected = [before[1], before[2], before[0]]
        XCTAssertTrue(
            waitForOrder(app, expected),
            "expected \(expected), got \(orderedPaneIds(app))"
        )
    }

    /// The headline differential guard: a press-drag started on a pill
    /// must BOTH reorder the panes AND leave the window in place. Inverse
    /// of WindowDragUITests.testEmptyToolbarDragMovesWindow (same idiom,
    /// started on a pill, opposite required window outcome).
    func testDragOnPillReordersAndDoesNotMoveWindow() throws {
        let app = launchApp()
        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 5))
        waitForFirstPill(app)
        growTo(3, in: app)

        let before = orderedPaneIds(app)
        XCTAssertEqual(before.count, 3)
        let initial = window.frame

        drag(app.buttons[before[0]], CGVector(dx: 0.5, dy: 0.5),
             app.buttons[before[1]], CGVector(dx: 0.9, dy: 0.5))

        let expected = [before[1], before[0], before[2]]
        XCTAssertTrue(
            waitForOrder(app, expected),
            "drag must reorder: expected \(expected), got \(orderedPaneIds(app))"
        )
        XCTAssertEqual(
            window.frame.origin, initial.origin,
            "Dragging a pill must NOT move the window"
        )
    }

    // MARK: - Disambiguation regressions (.onDrag must not eat these)

    /// A plain tap (no drag) still selects the pill.
    func testTapPillStillSelects() throws {
        let app = launchApp()
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 5))
        waitForFirstPill(app)
        growTo(3, in: app)

        let ids = orderedPaneIds(app)
        let target = ids.first { !app.buttons[$0].isSelected } ?? ids[0]
        let previouslySelected = ids.first { $0 != target && app.buttons[$0].isSelected }

        app.buttons[target].click()

        let selected = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "isSelected == true"),
            object: app.buttons[target]
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [selected], timeout: 3), .completed,
            "tapping a pill should select it"
        )
        if let previouslySelected {
            XCTAssertFalse(
                app.buttons[previouslySelected].isSelected,
                "the previously active pill should deselect"
            )
        }
    }

    /// Clicking the title of an already-active pill (after the
    /// double-click gate elapses) still enters rename and commits a new
    /// title. Mirrors NiceUITests.testTapActivePanePillTitleEntersEditModeAndCommits.
    func testTitleClickStillRenames() throws {
        let app = launchApp()
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 5))
        waitForFirstPill(app)
        growTo(3, in: app)

        // The just-added pill is active; `orderedPaneIds` returns full
        // `tab.pill.<paneId>` identifiers, so the title sub-element id is
        // simply that plus `.title`. The grow loop's waits put us well
        // past `NSEvent.doubleClickInterval`, so the first title click
        // qualifies as a deliberate rename.
        let fullId = orderedPaneIds(app).first { app.buttons[$0].isSelected }
            ?? orderedPaneIds(app).last!
        let titleId = "\(fullId).title"
        let fieldId = "\(fullId).titleField"

        let title = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", titleId))
            .element(boundBy: 0)
        XCTAssertTrue(title.waitForExistence(timeout: 5), "title element missing")
        title.click()

        let field = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", fieldId))
            .element(boundBy: 0)
        let fieldAppeared = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in field.exists }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [fieldAppeared], timeout: 5), .completed,
            "clicking the active pill title must swap in `.titleField`"
        )

        let newName = "Renamed pane"
        app.typeKey("a", modifierFlags: .command)
        app.typeText(newName)
        app.typeKey(XCUIKeyboardKey.return.rawValue, modifierFlags: [])

        let renamedText = app.staticTexts[newName]
        let titleUpdated = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in renamedText.exists }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [titleUpdated], timeout: 5), .completed,
            "pill should display the renamed title after commit"
        )
    }

    /// Clicking the close "×" on an active pill still closes that pane.
    func testCloseXStillClosesPane() throws {
        let app = launchApp()
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 5))
        waitForFirstPill(app)
        growTo(3, in: app)

        // Close a pane we added via `tab.add` (a fresh terminal at a bare
        // shell prompt → not "busy" → closes without the confirm alert).
        // `orderedPaneIds` yields full `tab.pill.<paneId>` ids; the close
        // button's id is `tab.close.<paneId>`, so strip the pill prefix.
        let fullId = orderedPaneIds(app).last!
        let paneId = String(fullId.dropFirst("tab.pill.".count))
        app.buttons[fullId].click()                  // activate so the × shows

        let closeX = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", "tab.close.\(paneId)"))
            .element(boundBy: 0)
        XCTAssertTrue(closeX.waitForExistence(timeout: 5), "close × missing on active pill")
        closeX.click()

        let dropped = XCTNSPredicateExpectation(
            predicate: NSPredicate { _, _ in self.pillButtons(app).count == 2 },
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [dropped], timeout: 5), .completed,
            "closing the × should drop the pill count to 2"
        )
    }

    // MARK: - Persistence

    /// A reorder survives an app relaunch (same sandbox HOME /
    /// application-support root → SessionStore restores the new order).
    func testReorderPersistsAcrossRelaunch() throws {
        let app = launchApp()
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 5))
        waitForFirstPill(app)
        growTo(3, in: app)

        let before = orderedPaneIds(app)
        XCTAssertEqual(before.count, 3)

        drag(app.buttons[before[0]], CGVector(dx: 0.5, dy: 0.5),
             app.buttons[before[1]], CGVector(dx: 0.9, dy: 0.5))

        let expected = [before[1], before[0], before[2]]
        XCTAssertTrue(
            waitForOrder(app, expected),
            "expected \(expected), got \(orderedPaneIds(app))"
        )

        app.terminate()                              // flush SessionStore

        let relaunched = launchApp()                 // same cached fakeHome
        XCTAssertTrue(relaunched.windows.firstMatch.waitForExistence(timeout: 5))
        waitForFirstPill(relaunched)
        XCTAssertTrue(
            waitForOrder(relaunched, expected),
            "reordered sequence should persist across relaunch: expected \(expected), got \(orderedPaneIds(relaunched))"
        )
    }
}
