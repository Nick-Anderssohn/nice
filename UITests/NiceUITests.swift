//
//  NiceUITests.swift
//  NiceUITests
//
//  XCUITest suite covering terminal lifecycle, companion pill management,
//  and the Main Terminal "Quit NICE?" alert. All tabs are created
//  dynamically via the control socket — no seed data required.
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

    /// 2. Creating a tab via socket adds a sidebar row.
    func testSocketNewTabCreatesSidebarRow() throws {
        let socketPath = Self.testSocketPath
        try? FileManager.default.removeItem(atPath: socketPath)
        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude(),
            "NICE_SOCKET_PATH": socketPath
        ])
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.mainTerminal"]
                .waitForExistence(timeout: 5)
        )

        try createTabViaSocket(in: app, socketPath: socketPath)
    }

    /// 3. Selecting a tab surfaces its companion pill.
    func testSelectTabShowsCompanionPill() throws {
        let socketPath = Self.testSocketPath
        try? FileManager.default.removeItem(atPath: socketPath)
        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude(),
            "NICE_SOCKET_PATH": socketPath
        ])
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.mainTerminal"]
                .waitForExistence(timeout: 5)
        )

        let rowId = try createTabViaSocket(in: app, socketPath: socketPath)
        let row = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier == %@", rowId)
        ).element(boundBy: 0)
        row.click()

        let pill = firstDescendant(in: app, withIdentifierPrefix: "companion.pill.")
        XCTAssertNotNil(pill, "Expected a companion.pill.* element after selecting a tab")
        XCTAssertTrue(
            pill!.waitForExistence(timeout: 5),
            "companion.pill.* should become visible after selecting a tab"
        )
    }

    /// 4. Tapping "+" adds a pill — count goes up by exactly one.
    func testAddCompanionPill() throws {
        let socketPath = Self.testSocketPath
        try? FileManager.default.removeItem(atPath: socketPath)
        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude(),
            "NICE_SOCKET_PATH": socketPath
        ])
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.mainTerminal"]
                .waitForExistence(timeout: 5)
        )

        let rowId = try createTabViaSocket(in: app, socketPath: socketPath)
        let row = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier == %@", rowId)
        ).element(boundBy: 0)
        row.click()

        let firstPill = firstDescendant(in: app, withIdentifierPrefix: "companion.pill.")
        XCTAssertNotNil(firstPill)
        XCTAssertTrue(firstPill!.waitForExistence(timeout: 5))

        let before = countElements(in: app, withIdentifierPrefix: "companion.pill.")
        XCTAssertGreaterThanOrEqual(before, 1)

        let addButton = app.buttons["companion.add"]
        XCTAssertTrue(addButton.waitForExistence(timeout: 5))
        addButton.click()

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

    /// 5. Closing a pill removes exactly one.
    func testCloseCompanionPill() throws {
        let socketPath = Self.testSocketPath
        try? FileManager.default.removeItem(atPath: socketPath)
        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude(),
            "NICE_SOCKET_PATH": socketPath
        ])
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.mainTerminal"]
                .waitForExistence(timeout: 5)
        )

        let rowId = try createTabViaSocket(in: app, socketPath: socketPath)
        let row = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier == %@", rowId)
        ).element(boundBy: 0)
        row.click()

        XCTAssertTrue(
            firstDescendant(in: app, withIdentifierPrefix: "companion.pill.")?
                .waitForExistence(timeout: 5) ?? false
        )

        let addButton = app.buttons["companion.add"]
        XCTAssertTrue(addButton.waitForExistence(timeout: 5))
        addButton.click()
        addButton.click()

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

        let closeQuery = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "companion.close.")
        )
        XCTAssertGreaterThan(closeQuery.count, 0)
        closeQuery.element(boundBy: 0).click()

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
        let socketPath = Self.testSocketPath
        try? FileManager.default.removeItem(atPath: socketPath)
        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude(),
            "NICE_SOCKET_PATH": socketPath
        ])

        let mainRow = app.descendants(matching: .any)["sidebar.mainTerminal"]
        XCTAssertTrue(mainRow.waitForExistence(timeout: 5))

        try createTabViaSocket(in: app, socketPath: socketPath)

        mainRow.click()

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
            "Expected Quit NICE? sheet with Cancel + Quit buttons after typing 'exit' in Main Terminal"
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
    }

    private func iconElement(in app: XCUIApplication, identifier: String) -> XCUIElement {
        let query = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier == %@", identifier)
        )
        return query.element(boundBy: 0)
    }

    /// 7. Ctrl+D in a Claude tab's chat pane exits fake-claude. The
    /// sidebar swaps the status dot for the terminal glyph.
    func testClaudeExitFlipsTabToTerminalIcon() throws {
        let socketPath = Self.testSocketPath
        try? FileManager.default.removeItem(atPath: socketPath)
        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude(),
            "NICE_SOCKET_PATH": socketPath
        ])
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.mainTerminal"]
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

    /// 8. After Claude exits, a promoteTab message over the control
    /// socket flips the tab back to Claude-mode.
    func testSocketPromoteFlow() throws {
        let socketPath = Self.testSocketPath
        try? FileManager.default.removeItem(atPath: socketPath)
        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude(),
            "NICE_SOCKET_PATH": socketPath
        ])
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.mainTerminal"]
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
}
