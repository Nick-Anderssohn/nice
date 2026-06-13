//
//  WindowDragUITests.swift
//  NiceUITests
//
//  Regression net for the window-chrome refactor: asserts that empty
//  top-bar pixels still drag (and double-click-zoom) the window.
//  Native title-bar drag is OFF on every Nice window (`isMovable =
//  false`), so the behaviour now comes entirely from `ChromeEventRouter`
//  — one process-wide local `NSEvent` monitor that classifies each press,
//  resolves an empty-chrome press to a `ChromeDragStripView` marker, and
//  drives `performDrag` / the double-click action itself. The
//  user-visible invariant is unchanged: drag from a non-widget pixel in
//  the top 52pt → window moves; drag from a widget (pill / button) →
//  the widget activates and the window doesn't move.
//
//  Lives at the UITest layer because the router's live side
//  (per-press hit-test class-walk, `NSWindow.performDrag`, `performZoom`)
//  needs a real window + real events. The pure routing table is unit-
//  tested in `ChromeEventRouterTests`; the `mouseDownCanMoveWindow ==
//  false` marker contract in `WindowToolbarDragRegionTests`. This suite
//  certifies the actual end-to-end behaviour those two can't reach — and
//  is the make-or-break gate for ISSUE 5 (the class-walk being narrower
//  than the deleted attribute-walk): if an empty-chrome press ever fails
//  to resolve to the strip, one of these tests goes red.
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

    private func launchApp(windowFrame: CGRect? = nil) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments += ["-ApplePersistenceIgnoreState", "YES"]
        let home = fakeHomePath()
        app.launchEnvironment["HOME"] = home
        app.launchEnvironment["NICE_APPLICATION_SUPPORT_ROOT"] =
            (home as NSString).appendingPathComponent("Library/Application Support")
        // Pin a deterministic, sub-screen starting frame so the zoom test
        // always begins un-zoomed (see AppShellView's matching env hook).
        if let f = windowFrame {
            app.launchEnvironment["NICE_UITEST_WINDOW_FRAME"] =
                "\(f.origin.x),\(f.origin.y),\(f.width),\(f.height)"
        }
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

    /// Double-click an empty pixel in the top bar → the window zooms to
    /// fill the screen, so it grows. `ChromeEventRouter` handles this: its
    /// process-wide local monitor classifies the press as the empty-chrome
    /// strip and runs `DoubleClickTitleBarAction` on `clickCount == 2`
    /// (AppKit's own title-bar hit-test doesn't reliably cross into the
    /// SwiftUI-embedded `ChromeDragStripView` — see ChromeEventRouter.swift).
    ///
    /// The window is launched at a deterministic sub-screen frame
    /// (`NICE_UITEST_WINDOW_FRAME`) so it always starts un-zoomed. Without
    /// that, a prior run's saved window state can relaunch the window
    /// already maximized; a window opened directly at its zoom frame has
    /// no distinct "user" frame, so `performZoom` is a no-op and the size
    /// never changes — the test's old intermittent failure on re-runs.
    func testEmptyToolbarDoubleClickZoomsWindow() throws {
        let app = launchApp(windowFrame: CGRect(x: 120, y: 120, width: 1100, height: 720))
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

        // Zoom enlarges the window to the largest screen-fitting frame, so
        // it grows in both dimensions from the sub-screen start.
        let zoomed = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                let f = window.frame
                return f.width > initial.width && f.height > initial.height
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [zoomed], timeout: 2),
            .completed,
            "Window did not grow after double-clicking an empty top-bar pixel — zoom should have fired (initial \(initial.size), now \(window.frame.size))"
        )
    }

    /// Dragging an EMPTY pixel in the sidebar's 52pt top strip moves the
    /// window — the sidebar analog of `testEmptyToolbarDragMovesWindow`.
    /// Native title-bar drag is off (`isMovable = false`), so the move comes
    /// from `ChromeEventRouter`: the press hit-tests to the strip's
    /// `ChromeDragStripView` marker and the router `performDrag`s the window.
    /// This guards that the sidebar strip stays wired (it regressed once when
    /// `isMovable = false` landed without a replacement drag path).
    func testSidebarTopStripDragMovesWindow() throws {
        let app = launchApp(windowFrame: CGRect(x: 120, y: 120, width: 1100, height: 720))
        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 5))

        // The leftmost sidebar header control; the empty drag strip sits
        // to its left, still within the 52pt top row. It's an
        // `Image().onTapGesture` (not a `Button`), so query by identifier
        // across any element type rather than `app.buttons`.
        let modeButton = app.descendants(matching: .any)["sidebar.mode.tabs"]
        XCTAssertTrue(modeButton.waitForExistence(timeout: 5), "sidebar header not mounted")

        let initial = window.frame
        // 60pt left of the mode button, at its vertical center → empty top
        // strip, clear of both the header buttons (to the right) and the
        // traffic lights (far to the left).
        let start = modeButton
            .coordinate(withNormalizedOffset: CGVector(dx: 0, dy: 0.5))
            .withOffset(CGVector(dx: -60, dy: 0))
        start.press(forDuration: 0.05, thenDragTo: start.withOffset(CGVector(dx: 60, dy: 60)))

        let moved = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in window.frame.origin != initial.origin }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [moved], timeout: 2), .completed,
            "Dragging the sidebar's top strip must move the window (origin stayed \(initial.origin))"
        )
        XCTAssertEqual(window.frame.size, initial.size, "size must not change during a drag")
    }
}
