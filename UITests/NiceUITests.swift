//
//  NiceUITests.swift
//  NiceUITests
//
//  First XCUITest batch covering the terminal-lifecycle wiring added in
//  the recent phases: sidebar seed tabs, companion pill creation /
//  close, and the Main Terminal "Quit NICE?" alert.
//
//  Each test launches a fresh app instance. Tests are ordered cheap →
//  expensive so a failure early on surfaces fast.
//

import XCTest

final class NiceUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    // MARK: - Helpers

    /// Launch the Nice app fresh for a test.
    @discardableResult
    private func launchApp() -> XCUIApplication {
        let app = XCUIApplication()
        app.launch()
        return app
    }

    /// Find the first element whose identifier starts with `prefix` but
    /// doesn't continue with `excludedInfixes` (used to skip nested
    /// children like `sidebar.tab.<id>.claudeIcon` when searching for
    /// the row itself).
    private func firstDescendant(
        in app: XCUIApplication,
        withIdentifierPrefix prefix: String,
        excludingInfixes excluded: [String] = []
    ) -> XCUIElement? {
        let query = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", prefix)
        )
        for i in 0..<query.count {
            let el = query.element(boundBy: i)
            let id = el.identifier
            if excluded.contains(where: { id.contains($0) }) { continue }
            return el
        }
        return nil
    }

    /// Count all elements of any type with the given identifier prefix.
    /// Pill containers surface as `Group` elements (because of
    /// `.accessibilityElement(children: .contain)`), not buttons, so we
    /// cast the net wide and filter by identifier.
    private func countElements(
        in app: XCUIApplication,
        withIdentifierPrefix prefix: String
    ) -> Int {
        let query = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", prefix)
        )
        return query.count
    }

    /// Select the first seed tab in the sidebar. Returns its identifier
    /// (e.g. "sidebar.tab.t1") so callers can build dependent ids.
    @discardableResult
    private func selectFirstSeedTab(in app: XCUIApplication) throws -> String {
        let query = app.descendants(matching: .any).matching(
            NSPredicate(
                format: "identifier BEGINSWITH %@ AND NOT (identifier CONTAINS %@) AND NOT (identifier CONTAINS %@)",
                "sidebar.tab.",
                ".claudeIcon",
                ".terminalIcon"
            )
        )
        let row = query.element(boundBy: 0)
        XCTAssertTrue(
            row.waitForExistence(timeout: 5),
            "Expected at least one seed tab row with identifier prefix 'sidebar.tab.'"
        )
        row.click()
        return row.identifier
    }

    // MARK: - Tests

    /// 1. Smoke — app launches and the Main Terminal row renders.
    func testAppLaunches() throws {
        let app = launchApp()
        let mainRow = app.descendants(matching: .any)["sidebar.mainTerminal"]
        XCTAssertTrue(
            mainRow.waitForExistence(timeout: 5),
            "sidebar.mainTerminal should exist after launch"
        )
    }

    /// 2. Seed data — at least one sidebar.tab.* row is present.
    func testSidebarSeedTabsPresent() throws {
        let app = launchApp()
        // Wait for the main row so the sidebar is materialised.
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.mainTerminal"]
                .waitForExistence(timeout: 5)
        )
        let tabRows = app.descendants(matching: .any).matching(
            NSPredicate(
                format: "identifier BEGINSWITH %@ AND NOT (identifier CONTAINS %@) AND NOT (identifier CONTAINS %@)",
                "sidebar.tab.",
                ".claudeIcon",
                ".terminalIcon"
            )
        )
        XCTAssertGreaterThan(
            tabRows.count, 0,
            "Expected seed data to produce at least one sidebar.tab.* row"
        )
    }

    /// 3. Selecting a seed tab surfaces its companion pill.
    func testSelectSeedTabShowsCompanionPill() throws {
        let app = launchApp()
        _ = try selectFirstSeedTab(in: app)

        let pill = firstDescendant(
            in: app, withIdentifierPrefix: "companion.pill."
        )
        XCTAssertNotNil(pill, "Expected a companion.pill.* element after selecting a tab")
        XCTAssertTrue(
            pill!.waitForExistence(timeout: 5),
            "companion.pill.* should become visible after selecting a tab"
        )
    }

    /// 4. Tapping "+" adds a pill — count goes up by exactly one.
    func testAddCompanionPill() throws {
        let app = launchApp()
        _ = try selectFirstSeedTab(in: app)

        // Wait for at least one pill so the count baseline is stable.
        let firstPill = firstDescendant(
            in: app, withIdentifierPrefix: "companion.pill."
        )
        XCTAssertNotNil(firstPill)
        XCTAssertTrue(firstPill!.waitForExistence(timeout: 5))

        let before = countElements(in: app, withIdentifierPrefix: "companion.pill.")
        XCTAssertGreaterThanOrEqual(before, 1)

        let addButton = app.buttons["companion.add"]
        XCTAssertTrue(addButton.waitForExistence(timeout: 5))
        addButton.click()

        // Wait for the count to tick up.
        let expectation = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "companion.pill.") == before + 1
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [expectation], timeout: 5), .completed,
            "Expected pill count to increase by 1 after tapping companion.add"
        )
    }

    /// 5. Closing a pill removes exactly one — we deliberately have
    /// >1 companion before closing to avoid triggering the
    /// last-companion-exits-tab logic.
    func testCloseCompanionPill() throws {
        let app = launchApp()
        _ = try selectFirstSeedTab(in: app)

        // Baseline: wait for the initial pill.
        XCTAssertTrue(
            firstDescendant(in: app, withIdentifierPrefix: "companion.pill.")?
                .waitForExistence(timeout: 5) ?? false
        )

        // Add two extra pills (seed tabs ship with one) so there's
        // headroom to close one without dissolving the tab.
        let addButton = app.buttons["companion.add"]
        XCTAssertTrue(addButton.waitForExistence(timeout: 5))
        addButton.click()
        addButton.click()

        // Wait for the two new pills to materialise.
        let growthExpectation = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "companion.pill.") >= 3
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [growthExpectation], timeout: 5), .completed,
            "Expected at least 3 pills after tapping add twice"
        )

        let before = countElements(in: app, withIdentifierPrefix: "companion.pill.")
        XCTAssertGreaterThanOrEqual(before, 3)

        // Find a close button and click it.
        let closeQuery = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "companion.close.")
        )
        XCTAssertGreaterThan(closeQuery.count, 0)
        closeQuery.element(boundBy: 0).click()

        // Closing is soft — it writes `exit\n` into the pty and waits
        // for the shell to die. That's observable but async; give it
        // headroom.
        let shrinkExpectation = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "companion.pill.") == before - 1
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [shrinkExpectation], timeout: 10), .completed,
            "Expected pill count to drop by 1 after tapping close"
        )
    }

    /// 6. Exiting the Main Terminal with tabs open surfaces the
    /// "Quit NICE?" alert; Cancel dismisses it.
    func testMainTerminalQuitPromptShowsWithTabs() throws {
        let app = launchApp()

        // Seed data guarantees several tabs exist, so `exit` on the
        // Main Terminal should hit the `showQuitPrompt` branch rather
        // than terminating the app outright.
        let mainRow = app.descendants(matching: .any)["sidebar.mainTerminal"]
        XCTAssertTrue(mainRow.waitForExistence(timeout: 5))
        mainRow.click()

        // Give the terminal pane a chance to focus, then type exit.
        // SwiftTerm's LocalProcessTerminalView is an NSView that
        // becomes first responder when clicked; typing into the key
        // window should route to it.
        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 5))
        // Click roughly in the middle-right of the window (the
        // terminal area, not the sidebar on the left).
        let focusPoint = window.coordinate(withNormalizedOffset: CGVector(dx: 0.7, dy: 0.5))
        focusPoint.click()

        app.typeText("exit\n")

        // SwiftUI's `.alert` on macOS surfaces as a Sheet attached to
        // the app window. Scope the Cancel/Quit lookup to the sheet so
        // we don't match the TouchBar-scoped duplicates macOS auto-
        // generates.
        let sheet = app.windows.firstMatch.sheets.firstMatch
        let alertShown = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                sheet.exists
                    && sheet.buttons["Cancel"].exists
                    && sheet.buttons["Quit"].exists
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [alertShown], timeout: 10), .completed,
            "Expected Quit NICE? sheet with Cancel + Quit buttons after typing 'exit' in Main Terminal"
        )

        sheet.buttons["Cancel"].click()

        // Sheet should dismiss.
        let dismissed = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                !sheet.exists
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [dismissed], timeout: 5), .completed,
            "Cancel should dismiss the Quit NICE? sheet"
        )
    }
}
