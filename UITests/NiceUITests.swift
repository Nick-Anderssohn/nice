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

    /// Launch the Nice app fresh for a test with additional env vars
    /// applied via `launchEnvironment`. Used by the claude-exit /
    /// socket-promote tests to swap the real `claude` binary for a
    /// scripted stub.
    @discardableResult
    private func launchApp(extraEnv: [String: String]) -> XCUIApplication {
        let app = XCUIApplication()
        for (k, v) in extraEnv {
            app.launchEnvironment[k] = v
        }
        app.launch()
        return app
    }

    /// Returns the path to a system binary that reads stdin until EOF
    /// and exits on Ctrl+D. Used as the `NICE_CLAUDE_OVERRIDE` value
    /// so the chat pane runs a predictable, controllable process.
    /// `/bin/cat` is globally accessible (no sandbox issues) and exits
    /// cleanly on EOF. `TabPtySession` skips `--mcp-config` args when
    /// the override env var is set, so cat receives no file arguments.
    private func fakeClaude() -> String {
        "/bin/cat"
    }

    /// Socket path shared between the test runner and the Nice app via
    /// `NICE_SOCKET_PATH`. Placed inside the test runner's own container
    /// directory: the sandboxed runner can connect here, and the
    /// unsandboxed Nice app can bind here.
    private static let testSocketPath: String = {
        let dir = FileManager.default.temporaryDirectory.path
        return (dir as NSString).appendingPathComponent("nice-xctest.sock")
    }()

    /// Send a single newline-delimited JSON message over a Unix-domain
    /// socket using POSIX APIs directly. Avoids spawning `nc` which
    /// inherits the test runner's sandbox and may not be able to
    /// connect to paths outside the container.
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

    /// Look up an icon element by identifier using a `BEGINSWITH`
    /// predicate. Plain subscript access via
    /// `app.descendants(matching: .any)["<id>"]` is flaky when the
    /// parent row uses `.accessibilityElement(children: .contain)` —
    /// the predicate-based query matches the same way existing tests
    /// look up sidebar rows and succeeds consistently.
    private func iconElement(in app: XCUIApplication, identifier: String) -> XCUIElement {
        let query = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier == %@", identifier)
        )
        return query.element(boundBy: 0)
    }

    /// 7. Ctrl+D in a Claude tab's chat pane trips the fake-claude's
    /// `cat > /dev/null`, which exits on EOF. The pty read loop sees
    /// the close and fires `onChatExit` → `claudePaneExited`, which
    /// flips `hasClaudePane` to false. The sidebar swaps the status
    /// dot for the terminal glyph.
    func testClaudeExitFlipsTabToTerminalIcon() throws {
        let fakePath = fakeClaude()
        print("DEBUG fakePath=\(fakePath)")
        let app = launchApp(extraEnv: ["NICE_CLAUDE_OVERRIDE": fakePath])

        // Wait for the sidebar to materialise, then tap t1.
        let tabRow = iconElement(in: app, identifier: "sidebar.tab.t1")
        XCTAssertTrue(
            tabRow.waitForExistence(timeout: 5),
            "sidebar.tab.t1 should exist (seed)"
        )
        tabRow.click()

        // Claude-icon visible → tab is in Claude-mode.
        let claudeIcon = iconElement(in: app, identifier: "sidebar.tab.t1.claudeIcon")
        XCTAssertTrue(
            claudeIcon.waitForExistence(timeout: 5),
            "sidebar.tab.t1.claudeIcon should appear while fake-claude is running"
        )

        // Focus the chat pane (left-of-center within the main split).
        // Mirror the existing Main-Terminal test's focus pattern; the
        // chat pane is the leftmost of the two panes in a Claude tab,
        // so dx=0.4 stays inside it.
        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 5))
        window.coordinate(withNormalizedOffset: CGVector(dx: 0.4, dy: 0.5)).click()

        // Send Ctrl+D (EOF). `cat > /dev/null` exits, fake-claude
        // returns, the pty closes, onChatExit fires.
        app.typeKey("d", modifierFlags: .control)

        let terminalIcon = iconElement(in: app, identifier: "sidebar.tab.t1.terminalIcon")
        XCTAssertTrue(
            terminalIcon.waitForExistence(timeout: 10),
            "sidebar.tab.t1.terminalIcon should appear after fake-claude exits"
        )
        XCTAssertFalse(
            claudeIcon.exists,
            "sidebar.tab.t1.claudeIcon should be gone once hasClaudePane flips false"
        )
    }

    /// 8. After Claude exits, a `promoteTab` message over the control
    /// socket should flip the tab back to Claude-mode without the user
    /// lifting a finger. Proves the full wire: bash shadow → nc →
    /// `NiceControlSocket.readClient` → `AppState.promoteTabToClaude`
    /// → `TabPtySession.promoteCompanionToChat` → `hasClaudePane = true`.
    func testSocketPromoteFlow() throws {
        let fakePath = fakeClaude()
        let socketPath = Self.testSocketPath
        // Remove stale socket from a prior run so NiceControlSocket
        // can bind cleanly (it unlinks before bind, but belt+suspenders).
        try? FileManager.default.removeItem(atPath: socketPath)
        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakePath,
            "NICE_SOCKET_PATH": socketPath
        ])

        let tabRow = iconElement(in: app, identifier: "sidebar.tab.t1")
        XCTAssertTrue(tabRow.waitForExistence(timeout: 5))
        tabRow.click()

        let claudeIcon = iconElement(in: app, identifier: "sidebar.tab.t1.claudeIcon")
        XCTAssertTrue(
            claudeIcon.waitForExistence(timeout: 5),
            "precondition: claudeIcon visible before exit"
        )

        // Drive the exit exactly as in test 7.
        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 5))
        window.coordinate(withNormalizedOffset: CGVector(dx: 0.4, dy: 0.5)).click()
        app.typeKey("d", modifierFlags: .control)

        let terminalIcon = iconElement(in: app, identifier: "sidebar.tab.t1.terminalIcon")
        XCTAssertTrue(
            terminalIcon.waitForExistence(timeout: 10),
            "precondition: terminalIcon visible after fake-claude exits"
        )

        // Send promoteTab for t1 over the well-known test socket.
        // The handler infers the companion from `activeCompanionId`.
        let json = #"{"action":"promoteTab","tabId":"t1","args":[]}"#
        try sendSocketLine(json, to: socketPath)

        // Icon should flip back to the claude dot purely as a
        // consequence of the socket message (no keystrokes, no clicks
        // after sending).
        XCTAssertTrue(
            claudeIcon.waitForExistence(timeout: 10),
            "sidebar.tab.t1.claudeIcon should reappear after promoteTab over socket"
        )
    }
}
