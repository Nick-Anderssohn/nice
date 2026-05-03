//
//  PaneDragWindowMoveUITests.swift
//  NiceUITests
//
//  End-to-end regression test for the "drag a pill drags the window"
//  bug: unit tests on synthetic fixtures couldn't catch this — the
//  failure mode depends on production-specific NSView properties
//  (`isOpaque`, `mouseDownCanMoveWindow` defaults) that vary based
//  on the actual SwiftUI tree the running app composes. Driving the
//  real app is the only way to assert the user-facing contract:
//  click-drag on a pane pill drags the PANE, not the window.
//

import XCTest

final class PaneDragWindowMoveUITests: NiceUITestCase {

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
                "nice-uitest-home-\(UUID().uuidString)",
                isDirectory: true
            )
        try? FileManager.default.createDirectory(
            at: url, withIntermediateDirectories: true
        )
        fakeHomeURL = url
        return url.path
    }

    private func launchSandboxed() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments += ["-ApplePersistenceIgnoreState", "YES"]
        let home = fakeHomePath()
        app.launchEnvironment["HOME"] = home
        app.launchEnvironment["NICE_APPLICATION_SUPPORT_ROOT"] =
            (home as NSString).appendingPathComponent("Library/Application Support")
        let hostEnv = ProcessInfo.processInfo.environment
        if let user = hostEnv["USER"] {
            app.launchEnvironment["USER"] = user
        }
        if let logname = hostEnv["LOGNAME"] {
            app.launchEnvironment["LOGNAME"] = logname
        }
        app.launch()
        track(app)
        return app
    }

    /// Click-and-drag a pane pill a short distance to the right —
    /// staying within the pill strip so no tear-off, no reorder
    /// across windows, no system-level drop side effects. While the
    /// drag is in flight the window's frame must not change. If the
    /// pill's hit-test leaf reports `mouseDownCanMoveWindow == true`
    /// AppKit's title-bar tracker engages on `mouseDown` and drags
    /// the window with the cursor — that is the bug we're pinning.
    func testDraggingPaneDoesNotMoveTheWindow() throws {
        let app = launchSandboxed()

        // Wait for the launch state to settle: the Terminals row must
        // exist (sidebar fully rendered) and at least one pane pill
        // must be present (toolbar fully rendered).
        let terminalsRow = app.descendants(matching: .any)["sidebar.terminals"]
        XCTAssertTrue(
            terminalsRow.waitForExistence(timeout: 5),
            "Sidebar didn't render after launch."
        )

        let pillQuery = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "tab.pill.")
        )
        XCTAssertTrue(
            pillQuery.element(boundBy: 0).waitForExistence(timeout: 5),
            "No pane pill rendered after launch."
        )

        // Add a second pill so a small drag stays inside the strip
        // (the drag's destination lands over another pill, which the
        // strip handles as a same-position reorder — a no-op — so the
        // test has no side effects beyond mouse motion).
        let addButton = app.descendants(matching: .any)["tab.add"]
        XCTAssertTrue(
            addButton.waitForExistence(timeout: 2),
            "+ button missing — can't add a second pill."
        )
        addButton.click()

        // Wait for the second pill to appear.
        let twoPillsExpectation = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in pillQuery.count >= 2 }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [twoPillsExpectation], timeout: 5),
            .completed,
            "Second pill never appeared."
        )

        // Capture the window's pre-drag frame. `app.windows.firstMatch`
        // is stable because we're dragging within the strip — no
        // tear-off, so no second window is spawned.
        let window = app.windows.firstMatch
        XCTAssertTrue(
            window.exists,
            "App has no window to assert against."
        )
        let frameBefore = window.frame

        // Drag the first pill 60pt to the right. 60pt comfortably
        // exceeds the pan-gesture's 4pt slop AND AppKit's drag-
        // initiation threshold, so anything that engages on
        // `mouseDown` will visibly act on the drag motion.
        let firstPill = pillQuery.element(boundBy: 0)
        XCTAssertTrue(firstPill.exists, "First pill vanished mid-test.")
        let start = firstPill.coordinate(withNormalizedOffset:
            CGVector(dx: 0.5, dy: 0.5))
        let end = start.withOffset(CGVector(dx: 60, dy: 0))
        // `press(forDuration:thenDragTo:)` issues mouseDown, holds,
        // moves to the destination, then mouseUp — synthesising a
        // real click-drag that AppKit's title-bar tracker would
        // notice if it engaged.
        start.press(forDuration: 0.1, thenDragTo: end)

        let frameAfter = window.frame
        XCTAssertEqual(
            frameBefore, frameAfter,
            """
            Window moved during a pane-pill drag — \
            before=\(frameBefore), after=\(frameAfter). The pill's \
            hit-test leaf is reporting `mouseDownCanMoveWindow == \
            true` and AppKit's title-bar tracker is engaging on \
            `mouseDown`, pre-empting the pan recogniser.
            """
        )
    }
}
