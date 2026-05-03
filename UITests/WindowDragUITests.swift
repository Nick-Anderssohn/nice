//
//  WindowDragUITests.swift
//  NiceUITests
//
//  Regression net for the title-bar refactor (Phase A): asserts that
//  empty top-bar pixels still drag the window. The previous architecture
//  used `WindowDragRegion` (mouseDownCanMoveWindow=true) under the
//  toolbar; the refactor moves the toolbar into an
//  `NSTitlebarAccessoryViewController` so AppKit's title-bar drag tracker
//  computes the drag region for us. Either way, the user-visible
//  invariant is the same: drag from a non-widget pixel in the top 52pt
//  → window moves; drag from a widget → button activates and the window
//  doesn't move.
//
//  Lives at the UITest layer because cooperative window drag is an
//  AppKit-internal mechanism (NSWindow.performDrag, the drag-region
//  tracker). A unit test against `mouseDownCanMoveWindow` flags only
//  certifies that *one* layer of the contract is intact; this test
//  certifies the actual behaviour.
//

import XCTest

final class WindowDragUITests: NiceUITestCase {

    private var fakeHomeURL: URL?

    override func tearDownWithError() throws {
        if let url = fakeHomeURL {
            try? FileManager.default.removeItem(at: url)
            fakeHomeURL = nil
        }
        try super.tearDownWithError()
    }

    private func fakeHomePath() -> String {
        if let url = fakeHomeURL { return url.path }
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-uitest-windowdrag-\(UUID().uuidString)",
                isDirectory: true
            )
        try? FileManager.default.createDirectory(
            at: url, withIntermediateDirectories: true
        )
        fakeHomeURL = url
        return url.path
    }

    private func launchApp() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments += ["-ApplePersistenceIgnoreState", "YES"]
        let home = fakeHomePath()
        app.launchEnvironment["HOME"] = home
        app.launchEnvironment["NICE_APPLICATION_SUPPORT_ROOT"] =
            (home as NSString).appendingPathComponent("Library/Application Support")
        let hostEnv = ProcessInfo.processInfo.environment
        if let user = hostEnv["USER"]    { app.launchEnvironment["USER"]    = user    }
        if let logname = hostEnv["LOGNAME"] { app.launchEnvironment["LOGNAME"] = logname }
        app.launch()
        track(app)
        return app
    }

    /// Drag from an empty pixel in the top bar (well past the brand
    /// block, in the strip's right half where the pane strip's empty
    /// scroll area sits) and assert the window moved.
    func testEmptyToolbarDragMovesWindow() throws {
        let app = launchApp()
        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 5))

        // Wait for at least one pane pill so we know the toolbar is
        // mounted; otherwise the window may still be assembling layout.
        let firstPill = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "tab.pill.")
        ).firstMatch
        XCTAssertTrue(firstPill.waitForExistence(timeout: 5))

        let initial = window.frame
        XCTAssertGreaterThan(initial.width, 200, "Window should have a real width")

        // Pick a point ~120pt from the right edge, ~26pt from the top.
        // Past UpdateAvailablePill (which renders 0pt without an update)
        // and into the InlinePaneStrip's trailing empty area, where no
        // widget hit-tests but the toolbar's drag surface does.
        let dx = (initial.width - 120) / initial.width
        let dy = 26.0 / initial.height
        let start = window.coordinate(withNormalizedOffset: CGVector(dx: dx, dy: dy))
        let end = start.withOffset(CGVector(dx: 60, dy: 60))

        start.press(forDuration: 0.05, thenDragTo: end)

        // Allow the drag to settle.
        let settled = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                let f = window.frame
                return f.origin != initial.origin
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [settled], timeout: 2),
            .completed,
            "Window did not move after dragging an empty top-bar pixel"
        )

        let moved = window.frame
        XCTAssertEqual(
            moved.size, initial.size,
            "Window size should not change during a drag"
        )
        let dxMoved = abs(moved.origin.x - initial.origin.x)
        let dyMoved = abs(moved.origin.y - initial.origin.y)
        XCTAssertGreaterThan(
            dxMoved + dyMoved, 5,
            "Expected a meaningful move, got dx=\(dxMoved) dy=\(dyMoved)"
        )
    }

    /// Double-click an empty pixel in the top bar — assert the
    /// window's size changes (zoom toggles between user-size and the
    /// largest screen-fitting size, so any size change confirms zoom
    /// fired). The previous implementation handled this with a
    /// process-wide `NSEvent` monitor (`TitleBarZoomMonitor`); the
    /// refactor moves it to `WindowDragRegion.DragView.mouseDown`'s
    /// `clickCount >= 2` branch. This test would have caught the
    /// regression where double-click did nothing because
    /// `performDrag` was being called on the first click and
    /// swallowing the second.
    func testEmptyToolbarDoubleClickZoomsWindow() throws {
        let app = launchApp()
        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 5))

        let firstPill = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "tab.pill.")
        ).firstMatch
        XCTAssertTrue(firstPill.waitForExistence(timeout: 5))

        let initial = window.frame
        XCTAssertGreaterThan(initial.width, 200)

        let dx = (initial.width - 120) / initial.width
        let dy = 26.0 / initial.height
        let target = window.coordinate(withNormalizedOffset: CGVector(dx: dx, dy: dy))

        target.doubleClick()

        let zoomed = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                let f = window.frame
                return f.size != initial.size
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [zoomed], timeout: 2),
            .completed,
            "Window did not change size after double-clicking an empty top-bar pixel — zoom should have fired"
        )
    }

    // Pill non-draggability is deliberately *not* asserted here —
    // pane pills are out of Phase A's scope. They'll be addressed in
    // Phase B (draggable-panes-v2) where the pill's own `mouseDown`
    // handler disambiguates pill-press → drag-start from
    // pill-press → window-drag, matching the audit's recommended
    // pattern for layered drag sources.
}
