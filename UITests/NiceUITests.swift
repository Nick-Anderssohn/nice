//
//  NiceUITests.swift
//  NiceUITests
//
//  XCUITest suite covering the refactored pane-pill model: every Claude
//  or terminal pane renders as a pill in the upper toolbar
//  (`tab.pill.<paneId>`), the built-in "Terminals" sidebar row
//  (`sidebar.terminals`) is selected on launch, and the `+` button
//  (`tab.add`) adds a terminal pane to the currently-selected tab. User
//  sessions are still created dynamically via the control socket — no
//  seed data required.
//

import XCTest

final class NiceUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    // MARK: - Helpers

    @discardableResult
    private func launchApp() -> XCUIApplication {
        let app = XCUIApplication()
        app.launch()
        return app
    }

    @discardableResult
    private func launchApp(extraEnv: [String: String]) -> XCUIApplication {
        let app = XCUIApplication()
        for (k, v) in extraEnv {
            app.launchEnvironment[k] = v
        }
        app.launch()
        return app
    }

    private func fakeClaude() -> String { "/bin/cat" }

    private static let testSocketPath: String = {
        let dir = FileManager.default.temporaryDirectory.path
        return (dir as NSString).appendingPathComponent("nice-xctest.sock")
    }()

    private func sendSocketLine(_ json: String, to socketPath: String) throws {
        let fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else {
            throw NSError(domain: "NiceUITests", code: Int(errno),
                          userInfo: [NSLocalizedDescriptionKey: "socket() failed: errno=\(errno)"])
        }
        defer { Darwin.close(fd) }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        socketPath.withCString { cstr in
            withUnsafeMutableBytes(of: &addr.sun_path) { buf in
                let dst = buf.baseAddress!.assumingMemoryBound(to: CChar.self)
                strncpy(dst, cstr, buf.count)
            }
        }

        let connectResult = withUnsafePointer(to: &addr) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sockPtr in
                Darwin.connect(fd, sockPtr, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard connectResult == 0 else {
            throw NSError(domain: "NiceUITests", code: Int(errno),
                          userInfo: [NSLocalizedDescriptionKey: "connect(\(socketPath)) failed: errno=\(errno)"])
        }

        let payload = Array((json + "\n").utf8)
        let written = Darwin.write(fd, payload, payload.count)
        guard written == payload.count else {
            throw NSError(domain: "NiceUITests", code: Int(errno),
                          userInfo: [NSLocalizedDescriptionKey: "write() failed: wrote \(written)/\(payload.count)"])
        }
    }

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

    private func countElements(
        in app: XCUIApplication,
        withIdentifierPrefix prefix: String
    ) -> Int {
        let query = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", prefix)
        )
        return query.count
    }

    /// Create a tab via the control socket and wait for it to appear in
    /// the sidebar. Returns the tab row's accessibility identifier.
    @discardableResult
    private func createTabViaSocket(
        in app: XCUIApplication,
        socketPath: String,
        cwd: String = NSHomeDirectory()
    ) throws -> String {
        let json = #"{"action":"newtab","cwd":"\#(cwd)","args":[]}"#
        try sendSocketLine(json, to: socketPath)

        let tabQuery = app.descendants(matching: .any).matching(
            NSPredicate(
                format: "identifier BEGINSWITH %@ AND NOT (identifier CONTAINS %@) AND NOT (identifier CONTAINS %@)",
                "sidebar.tab.",
                ".claudeIcon",
                ".terminalIcon"
            )
        )
        let appeared = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in tabQuery.count >= 1 }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [appeared], timeout: 5), .completed,
            "Expected a sidebar tab to appear after newtab socket message"
        )
        return tabQuery.element(boundBy: 0).identifier
    }

    /// Extract the tab ID from a sidebar row identifier like "sidebar.tab.t1234".
    private func tabId(from sidebarIdentifier: String) -> String {
        String(sidebarIdentifier.dropFirst("sidebar.tab.".count))
    }

    private func iconElement(in app: XCUIApplication, identifier: String) -> XCUIElement {
        let query = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier == %@", identifier)
        )
        return query.element(boundBy: 0)
    }

    /// Scan the app for the first `tab.pill.*` element and return the pane
    /// id portion of its identifier. Returns nil if no pill exists.
    private func firstPanePillId(in app: XCUIApplication) -> String? {
        let query = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "tab.pill.")
        )
        guard query.count > 0 else { return nil }
        let id = query.element(boundBy: 0).identifier
        return String(id.dropFirst("tab.pill.".count))
    }

    /// Collect all current `tab.pill.*` pane ids (in query order).
    private func panePillIds(in app: XCUIApplication) -> [String] {
        let query = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "tab.pill.")
        )
        var ids: [String] = []
        for i in 0..<query.count {
            let full = query.element(boundBy: i).identifier
            ids.append(String(full.dropFirst("tab.pill.".count)))
        }
        return ids
    }

    // MARK: - Startup & Terminals row

    /// Smoke — app launches and the Terminals built-in row renders.
    func testAppLaunches() throws {
        let app = launchApp()
        let terminalsRow = app.descendants(matching: .any)["sidebar.terminals"]
        XCTAssertTrue(
            terminalsRow.waitForExistence(timeout: 5),
            "sidebar.terminals should exist after launch"
        )
    }

    /// Launch state — the Terminals tab is selected and shows exactly
    /// one pane pill in the toolbar.
    func testStartupShowsSingleTerminalTab() throws {
        let app = launchApp()
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        let pillAppeared = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "tab.pill.") >= 1
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [pillAppeared], timeout: 5), .completed,
            "Expected at least one tab.pill.* after launch"
        )
        XCTAssertEqual(
            countElements(in: app, withIdentifierPrefix: "tab.pill."),
            1,
            "Terminals tab should start with exactly one pane pill"
        )
    }

    /// Regression — no element should carry the old `companion.*`
    /// identifier prefix at startup.
    func testStartupNoCompanionPaneExists() throws {
        let app = launchApp()
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        // Give the toolbar a beat to render before asserting absence.
        let pillReady = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "tab.pill.") >= 1
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [pillReady], timeout: 5), .completed
        )

        XCTAssertEqual(
            countElements(in: app, withIdentifierPrefix: "companion."),
            0,
            "No companion.* element should exist after the refactor"
        )
    }

    // MARK: - Terminals tab add/switch/close

    /// Clicking `tab.add` adds a second pill to the Terminals tab.
    func testTerminalsAddPane() throws {
        let app = launchApp()
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        let pillReady = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "tab.pill.") >= 1
            }),
            object: nil
        )
        XCTAssertEqual(XCTWaiter.wait(for: [pillReady], timeout: 5), .completed)

        let before = countElements(in: app, withIdentifierPrefix: "tab.pill.")

        let addButton = app.buttons["tab.add"]
        XCTAssertTrue(addButton.waitForExistence(timeout: 5))
        addButton.click()

        let grew = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "tab.pill.") == before + 1
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [grew], timeout: 5), .completed,
            "Expected a second tab.pill.* after clicking tab.add"
        )
    }

    /// With two pills, clicking each alternately should not crash and
    /// both pills should stay present.
    func testTerminalsSwitchPanes() throws {
        let app = launchApp()
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        let addButton = app.buttons["tab.add"]
        XCTAssertTrue(addButton.waitForExistence(timeout: 5))
        addButton.click()

        let twoPills = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "tab.pill.") >= 2
            }),
            object: nil
        )
        XCTAssertEqual(XCTWaiter.wait(for: [twoPills], timeout: 5), .completed)

        let ids = panePillIds(in: app)
        XCTAssertGreaterThanOrEqual(ids.count, 2, "Need two pane ids to alternate between")

        let pill1 = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", "tab.pill.\(ids[0])"))
            .element(boundBy: 0)
        let pill2 = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", "tab.pill.\(ids[1])"))
            .element(boundBy: 0)

        pill1.click()
        pill2.click()
        pill1.click()

        // Verify no crash: both pills still present.
        XCTAssertTrue(pill1.exists, "pill 1 should still exist after alternating")
        XCTAssertTrue(pill2.exists, "pill 2 should still exist after alternating")
    }

    /// Close an inactive pane: add a second pane, keep pane 1 active,
    /// click close on pane 2.
    func testTerminalsCloseInactivePane() throws {
        let app = launchApp()
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        let pillReady = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "tab.pill.") >= 1
            }),
            object: nil
        )
        XCTAssertEqual(XCTWaiter.wait(for: [pillReady], timeout: 5), .completed)

        let ids0 = panePillIds(in: app)
        XCTAssertEqual(ids0.count, 1)
        let firstId = ids0[0]

        let addButton = app.buttons["tab.add"]
        XCTAssertTrue(addButton.waitForExistence(timeout: 5))
        addButton.click()

        let twoPills = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "tab.pill.") == 2
            }),
            object: nil
        )
        XCTAssertEqual(XCTWaiter.wait(for: [twoPills], timeout: 5), .completed)

        let ids = panePillIds(in: app)
        XCTAssertEqual(ids.count, 2)
        let secondId = ids.first(where: { $0 != firstId }) ?? ids[1]

        // Activate pane 1, so pane 2 is inactive.
        let pill1 = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", "tab.pill.\(firstId)"))
            .element(boundBy: 0)
        pill1.click()

        // Hover the inactive pill so its close button becomes hit-testable.
        let pill2 = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", "tab.pill.\(secondId)"))
            .element(boundBy: 0)
        pill2.hover()

        let closeBtn = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", "tab.close.\(secondId)"))
            .element(boundBy: 0)
        XCTAssertTrue(closeBtn.waitForExistence(timeout: 5), "close button for pane 2 should exist on hover")
        closeBtn.click()

        let gone = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                !app.descendants(matching: .any)[ "tab.pill.\(secondId)"].exists
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [gone], timeout: 5), .completed,
            "Inactive pane 2 pill should disappear after clicking its close button"
        )
    }

    /// Close the active pane: add a second pane (it becomes active by
    /// default in most flows), click close on it.
    func testTerminalsCloseActivePane() throws {
        let app = launchApp()
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        let pillReady = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "tab.pill.") >= 1
            }),
            object: nil
        )
        XCTAssertEqual(XCTWaiter.wait(for: [pillReady], timeout: 5), .completed)

        let ids0 = panePillIds(in: app)
        let firstId = ids0[0]

        let addButton = app.buttons["tab.add"]
        XCTAssertTrue(addButton.waitForExistence(timeout: 5))
        addButton.click()

        let twoPills = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "tab.pill.") == 2
            }),
            object: nil
        )
        XCTAssertEqual(XCTWaiter.wait(for: [twoPills], timeout: 5), .completed)

        let ids = panePillIds(in: app)
        let secondId = ids.first(where: { $0 != firstId }) ?? ids[1]

        // Make sure pane 2 is active by clicking it.
        let pill2 = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", "tab.pill.\(secondId)"))
            .element(boundBy: 0)
        pill2.click()

        // Close button for the active pane is hit-testable without a
        // hover because the pill is active.
        let closeBtn = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", "tab.close.\(secondId)"))
            .element(boundBy: 0)
        XCTAssertTrue(closeBtn.waitForExistence(timeout: 5), "close button for active pane 2 should exist")
        closeBtn.click()

        let gone = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                !app.descendants(matching: .any)[ "tab.pill.\(secondId)"].exists
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [gone], timeout: 5), .completed,
            "Active pane 2 pill should disappear after clicking its close button"
        )
    }

    /// Closing the last pane of the Terminals tab while other user
    /// sessions exist surfaces the "Quit NICE?" sheet. Cancel dismisses
    /// the sheet and the tab is reseeded with a fresh pill.
    func testTerminalsCloseLastPaneShowsQuitAlert() throws {
        let socketPath = Self.testSocketPath
        try? FileManager.default.removeItem(atPath: socketPath)
        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude(),
            "NICE_SOCKET_PATH": socketPath
        ])

        let terminalsRow = app.descendants(matching: .any)["sidebar.terminals"]
        XCTAssertTrue(terminalsRow.waitForExistence(timeout: 5))

        // Seed a user session so non-builtin tabs exist.
        try createTabViaSocket(in: app, socketPath: socketPath)

        // Select the Terminals tab.
        terminalsRow.click()

        let pillReady = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "tab.pill.") >= 1
            }),
            object: nil
        )
        XCTAssertEqual(XCTWaiter.wait(for: [pillReady], timeout: 5), .completed)

        // If the Terminals tab only has one pane, the close button isn't
        // shown (canClose == false). Add another so both pills have close
        // buttons, then close them until zero.
        let addButton = app.buttons["tab.add"]
        XCTAssertTrue(addButton.waitForExistence(timeout: 5))
        addButton.click()

        let twoPills = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "tab.pill.") == 2
            }),
            object: nil
        )
        XCTAssertEqual(XCTWaiter.wait(for: [twoPills], timeout: 5), .completed)

        // Close the first of the two pills (active pill has its close
        // button hit-testable).
        let ids = panePillIds(in: app)
        XCTAssertEqual(ids.count, 2)
        let activePill = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", "tab.pill.\(ids[1])"))
            .element(boundBy: 0)
        activePill.click()

        let firstClose = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", "tab.close.\(ids[1])"))
            .element(boundBy: 0)
        XCTAssertTrue(firstClose.waitForExistence(timeout: 5))
        firstClose.click()

        let onePill = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "tab.pill.") == 1
            }),
            object: nil
        )
        XCTAssertEqual(XCTWaiter.wait(for: [onePill], timeout: 5), .completed)

        // Exactly one pill left. `canClose` goes back to false, so closing
        // the last pane via the close button isn't directly possible
        // through the UI. Drive it via the terminal: focus the pane and
        // type `exit\n` — that's the same trigger path the old
        // Main Terminal used.
        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 5))
        let focusPoint = window.coordinate(withNormalizedOffset: CGVector(dx: 0.7, dy: 0.5))
        focusPoint.click()
        app.typeText("exit\n")

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
            "Expected Quit NICE? sheet with Cancel + Quit buttons when Terminals' last pane exits"
        )

        sheet.buttons["Cancel"].click()

        let dismissed = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in !sheet.exists }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [dismissed], timeout: 5), .completed,
            "Cancel should dismiss the Quit NICE? sheet"
        )

        // After cancel, the Terminals tab should have a fresh pill again.
        let reseeded = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "tab.pill.") >= 1
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [reseeded], timeout: 10), .completed,
            "A new tab.pill.* should appear after Cancel"
        )
    }

    // MARK: - Session creation (control socket newtab)

    /// Creating a tab via the socket adds a sidebar row.
    func testSocketNewTabCreatesSidebarRow() throws {
        let socketPath = Self.testSocketPath
        try? FileManager.default.removeItem(atPath: socketPath)
        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude(),
            "NICE_SOCKET_PATH": socketPath
        ])
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        try createTabViaSocket(in: app, socketPath: socketPath)
    }

    /// A freshly-created user session has both a Claude pane pill and
    /// a terminal pane pill in the toolbar.
    func testNewSessionShowsClaudeAndTerminalPills() throws {
        let socketPath = Self.testSocketPath
        try? FileManager.default.removeItem(atPath: socketPath)
        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude(),
            "NICE_SOCKET_PATH": socketPath
        ])
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        let rowId = try createTabViaSocket(in: app, socketPath: socketPath)
        let row = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier == %@", rowId)
        ).element(boundBy: 0)
        row.click()

        let twoPills = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "tab.pill.") >= 2
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [twoPills], timeout: 5), .completed,
            "Expected two tab.pill.* elements (claude + terminal) after socket newtab"
        )
    }

    /// Selecting a session surfaces its pane pills.
    func testSelectTabShowsCompanionPill() throws {
        let socketPath = Self.testSocketPath
        try? FileManager.default.removeItem(atPath: socketPath)
        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude(),
            "NICE_SOCKET_PATH": socketPath
        ])
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        let rowId = try createTabViaSocket(in: app, socketPath: socketPath)
        let row = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier == %@", rowId)
        ).element(boundBy: 0)
        row.click()

        let pill = firstDescendant(in: app, withIdentifierPrefix: "tab.pill.")
        XCTAssertNotNil(pill, "Expected a tab.pill.* element after selecting a session")
        XCTAssertTrue(
            pill!.waitForExistence(timeout: 5),
            "tab.pill.* should become visible after selecting a session"
        )
    }

    // MARK: - Claude pane lifecycle

    /// Ctrl+D in a Claude tab's chat pane exits fake-claude. The sidebar
    /// swaps the status dot for the terminal glyph.
    func testClaudeExitFlipsTabToTerminalIcon() throws {
        let socketPath = Self.testSocketPath
        try? FileManager.default.removeItem(atPath: socketPath)
        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude(),
            "NICE_SOCKET_PATH": socketPath
        ])
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        let rowId = try createTabViaSocket(in: app, socketPath: socketPath)
        let tid = tabId(from: rowId)

        let tabRow = iconElement(in: app, identifier: rowId)
        tabRow.click()

        let claudeIcon = iconElement(in: app, identifier: "sidebar.tab.\(tid).claudeIcon")
        XCTAssertTrue(
            claudeIcon.waitForExistence(timeout: 5),
            "claudeIcon should appear while fake-claude is running"
        )

        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 5))
        window.coordinate(withNormalizedOffset: CGVector(dx: 0.4, dy: 0.5)).click()

        app.typeKey("d", modifierFlags: .control)

        let terminalIcon = iconElement(in: app, identifier: "sidebar.tab.\(tid).terminalIcon")
        XCTAssertTrue(
            terminalIcon.waitForExistence(timeout: 10),
            "terminalIcon should appear after fake-claude exits"
        )
        XCTAssertFalse(
            claudeIcon.exists,
            "claudeIcon should be gone once hasClaudePane flips false"
        )
    }

    /// Closing the Claude pill (via its `tab.close.*`) keeps the session
    /// but removes the Claude pane and flips the sidebar icon to the
    /// terminal glyph.
    func testCloseClaudePillKeepsSession() throws {
        let socketPath = Self.testSocketPath
        try? FileManager.default.removeItem(atPath: socketPath)
        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude(),
            "NICE_SOCKET_PATH": socketPath
        ])
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        let rowId = try createTabViaSocket(in: app, socketPath: socketPath)
        let tid = tabId(from: rowId)

        let tabRow = iconElement(in: app, identifier: rowId)
        tabRow.click()

        let claudeIcon = iconElement(in: app, identifier: "sidebar.tab.\(tid).claudeIcon")
        XCTAssertTrue(
            claudeIcon.waitForExistence(timeout: 5),
            "claudeIcon should appear while the Claude pane is alive"
        )

        // Wait for both pills (claude + terminal) to be visible.
        let twoPills = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "tab.pill.") >= 2
            }),
            object: nil
        )
        XCTAssertEqual(XCTWaiter.wait(for: [twoPills], timeout: 5), .completed)

        // Find the Claude pill — it's the one whose sibling close button
        // can be located and clicked. We can't directly introspect kind
        // from the pill identifier, but we can close each in turn and
        // observe the side effects. Simpler: close every `tab.close.*`
        // and rely on the assertion that the sidebar row flips.
        //
        // In practice, the Claude pane is active by default after newtab,
        // so clicking the first visible close button closes the Claude
        // pane.
        let closeQuery = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "tab.close.")
        )
        XCTAssertGreaterThan(closeQuery.count, 0, "Expected at least one tab.close.* button")
        closeQuery.element(boundBy: 0).click()

        // The sidebar row should remain and flip to terminalIcon.
        let terminalIcon = iconElement(in: app, identifier: "sidebar.tab.\(tid).terminalIcon")
        XCTAssertTrue(
            terminalIcon.waitForExistence(timeout: 10),
            "terminalIcon should appear after the Claude pill is closed"
        )
        XCTAssertFalse(
            claudeIcon.exists,
            "claudeIcon should be gone after closing the Claude pill"
        )
        XCTAssertTrue(
            iconElement(in: app, identifier: rowId).exists,
            "sidebar row for the session should still exist"
        )
    }

    // MARK: - Promotion

    /// After Claude exits, a promoteTab message over the control socket
    /// flips the tab back to Claude-mode (claudeIcon reappears).
    func testSocketPromoteFlow() throws {
        let socketPath = Self.testSocketPath
        try? FileManager.default.removeItem(atPath: socketPath)
        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude(),
            "NICE_SOCKET_PATH": socketPath
        ])
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        let rowId = try createTabViaSocket(in: app, socketPath: socketPath)
        let tid = tabId(from: rowId)

        let tabRow = iconElement(in: app, identifier: rowId)
        tabRow.click()

        let claudeIcon = iconElement(in: app, identifier: "sidebar.tab.\(tid).claudeIcon")
        XCTAssertTrue(
            claudeIcon.waitForExistence(timeout: 5),
            "precondition: claudeIcon visible before exit"
        )

        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 5))
        window.coordinate(withNormalizedOffset: CGVector(dx: 0.4, dy: 0.5)).click()
        app.typeKey("d", modifierFlags: .control)

        let terminalIcon = iconElement(in: app, identifier: "sidebar.tab.\(tid).terminalIcon")
        XCTAssertTrue(
            terminalIcon.waitForExistence(timeout: 10),
            "precondition: terminalIcon visible after fake-claude exits"
        )

        let promoteJson = #"{"action":"promoteTab","tabId":"\#(tid)","args":[]}"#
        try sendSocketLine(promoteJson, to: socketPath)

        XCTAssertTrue(
            claudeIcon.waitForExistence(timeout: 10),
            "claudeIcon should reappear after promoteTab over socket"
        )
    }

    // MARK: - Regression guard

    /// Exercise a handful of actions (add, switch, close) and assert no
    /// `companion.*` identifier ever appears during or after the flow.
    func testNoCompanionPaneEverRenders() throws {
        let app = launchApp()
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        // Baseline.
        XCTAssertEqual(
            countElements(in: app, withIdentifierPrefix: "companion."), 0,
            "No companion.* at launch"
        )

        // Add a pane.
        let addButton = app.buttons["tab.add"]
        XCTAssertTrue(addButton.waitForExistence(timeout: 5))
        addButton.click()

        let twoPills = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "tab.pill.") >= 2
            }),
            object: nil
        )
        XCTAssertEqual(XCTWaiter.wait(for: [twoPills], timeout: 5), .completed)
        XCTAssertEqual(
            countElements(in: app, withIdentifierPrefix: "companion."), 0,
            "No companion.* after adding a pane"
        )

        // Switch between the two pills.
        let ids = panePillIds(in: app)
        XCTAssertEqual(ids.count, 2)
        let pill1 = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", "tab.pill.\(ids[0])"))
            .element(boundBy: 0)
        let pill2 = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", "tab.pill.\(ids[1])"))
            .element(boundBy: 0)
        pill1.click()
        pill2.click()
        XCTAssertEqual(
            countElements(in: app, withIdentifierPrefix: "companion."), 0,
            "No companion.* after switching panes"
        )

        // Close the active pane.
        let closeBtn = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", "tab.close.\(ids[1])"))
            .element(boundBy: 0)
        if closeBtn.waitForExistence(timeout: 3) {
            closeBtn.click()
            let shrunk = XCTNSPredicateExpectation(
                predicate: NSPredicate(block: { _, _ in
                    self.countElements(in: app, withIdentifierPrefix: "tab.pill.") == 1
                }),
                object: nil
            )
            XCTAssertEqual(XCTWaiter.wait(for: [shrunk], timeout: 5), .completed)
        }
        XCTAssertEqual(
            countElements(in: app, withIdentifierPrefix: "companion."), 0,
            "No companion.* after closing a pane"
        )
    }
}
