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

final class NiceUITests: NiceUITestCase {

    /// Per-test fake HOME. Redirects `NSHomeDirectory()` (and everything
    /// downstream: Main Terminal cwd, SessionStore's Application Support
    /// root, the zsh chain-back probes in MainTerminalShellInject) away
    /// from the real `$HOME` so the spawned app never touches protected
    /// subdirectories like `~/Documents` / `~/Downloads` / `~/Music`.
    /// Without this, the DerivedData test build — which has no TCC
    /// grants of its own — triggers a fresh permission prompt on every
    /// run when the user's real dotfiles or their plugins fan out into
    /// those folders.
    private var fakeHomeURL: URL?

    override func tearDownWithError() throws {
        if let url = fakeHomeURL {
            try? FileManager.default.removeItem(at: url)
            fakeHomeURL = nil
        }
        try super.tearDownWithError()
    }

    // MARK: - Helpers

    /// Lazily create a per-test temp directory and return its path. The
    /// directory is real (so zsh can cd into it) but empty, so the
    /// `[[ -f "$HOME/.zshrc" ]] && source ...` chain-backs in
    /// `MainTerminalShellInject` silently skip.
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

    /// Seed `HOME`, plus USER/LOGNAME. `app.launchEnvironment` replaces
    /// — not merges with — the host process's env, so SwiftTerm's
    /// `getEnvironmentVariables()` would otherwise drop USER/LOGNAME on
    /// the floor and zsh prompt frameworks can misbehave without them.
    private func applySandboxEnv(to app: XCUIApplication) {
        let home = fakeHomePath()
        app.launchEnvironment["HOME"] = home
        // `FileManager.url(for: .applicationSupportDirectory)` bypasses
        // `$HOME` and resolves via the user record, so `SessionStore`
        // would still read the user's real `sessions.json` and the
        // test-launched app would restore their live Claude sessions.
        // Pin the Application Support root inside the fake HOME.
        app.launchEnvironment["NICE_APPLICATION_SUPPORT_ROOT"] =
            (home as NSString).appendingPathComponent("Library/Application Support")
        let hostEnv = ProcessInfo.processInfo.environment
        if let user = hostEnv["USER"] {
            app.launchEnvironment["USER"] = user
        }
        if let logname = hostEnv["LOGNAME"] {
            app.launchEnvironment["LOGNAME"] = logname
        }
    }

    @discardableResult
    private func launchApp() -> XCUIApplication {
        let app = XCUIApplication()
        // Suppress AppKit state restoration. Stale per-scene state
        // saved by an earlier run (or a killed Nice process) can
        // replay as "no windows were open," which SwiftUI honours and
        // never opens the default WindowGroup window — the test then
        // sees a running app with zero children.
        app.launchArguments += ["-ApplePersistenceIgnoreState", "YES"]
        applySandboxEnv(to: app)
        app.launch()
        track(app)
        return app
    }

    @discardableResult
    private func launchApp(extraEnv: [String: String]) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments += ["-ApplePersistenceIgnoreState", "YES"]
        applySandboxEnv(to: app)
        for (k, v) in extraEnv {
            app.launchEnvironment[k] = v
        }
        app.launch()
        track(app)
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

        // The "claude" action replies with one line ("newtab" /
        // "inplace" / "inplace <uuid>"). Drain until the newline (or
        // peer close) so the server's write succeeds instead of
        // SIGPIPE-crashing the app when we close the fd. A short read
        // timeout keeps the test from hanging if the handler drops the
        // message. Payload-less actions close cleanly on EOF here.
        var tv = timeval(tv_sec: 2, tv_usec: 0)
        _ = setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv,
                       socklen_t(MemoryLayout<timeval>.size))
        var buf = [UInt8](repeating: 0, count: 256)
        while true {
            let n = Darwin.read(fd, &buf, buf.count)
            if n <= 0 { break }
            if buf[..<n].contains(0x0A) { break }
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
        // Pill identifiers nest child elements under suffixes like
        // `.title`, `.titleField`, `.renamePane`, `.closePane`. A bare
        // `BEGINSWITH "tab.pill."` predicate would count both the
        // outer pill and each of its children — so an active pill
        // contributes 2-5 hits depending on hover/edit state. Strip
        // those child identifiers from the count so callers asking
        // "how many pills?" get the answer they expect. The list is
        // shared with `panePillIds` / `firstPanePillId` so a new pill
        // child suffix is added in one place.
        let query = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", prefix)
        )
        var count = 0
        for i in 0..<query.count {
            let id = query.element(boundBy: i).identifier
            if Self.panePillChildSuffixes.contains(where: { id.hasSuffix($0) }) {
                continue
            }
            count += 1
        }
        return count
    }

    /// Create a tab via the control socket and wait for it to appear in
    /// the sidebar. Returns the tab row's accessibility identifier.
    ///
    /// Protocol note: commit 8ec1644 unified the socket API — there is
    /// no standalone `"newtab"` action anymore. The only inbound action
    /// is `"claude"`, and an empty `tabId` tells the app "open a new
    /// sidebar tab" (see `AppState.handleClaudeSocketRequest`, which
    /// replies `newtab` and calls `createTabFromMainTerminal` whenever
    /// `tabId` is empty).
    @discardableResult
    private func createTabViaSocket(
        in app: XCUIApplication,
        socketPath: String,
        cwd: String? = nil
    ) throws -> String {
        let cwd = cwd ?? fakeHomePath()
        let json = #"{"action":"claude","cwd":"\#(cwd)","args":[],"tabId":"","paneId":""}"#
        try sendSocketLine(json, to: socketPath)

        let tabQuery = app.descendants(matching: .any).matching(
            NSPredicate(
                format: "identifier BEGINSWITH %@ AND NOT (identifier CONTAINS %@) AND NOT (identifier CONTAINS %@) AND NOT (identifier CONTAINS %@)",
                "sidebar.tab.",
                ".claudeIcon",
                ".terminalIcon",
                ".title"
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

    /// Click a sidebar tab row at an offset that lands on the row's
    /// leading padding / icon, not the title label. The title has its
    /// own tap gesture that enters rename mode when the tab is already
    /// active (see `SidebarView.titleView`) — default `.click()` on the
    /// row's accessibility element resolves to the title centroid and
    /// accidentally starts an edit, swallowing any subsequent typing.
    private func clickSidebarRow(in app: XCUIApplication, rowIdentifier: String) {
        let row = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier == %@", rowIdentifier)
        ).element(boundBy: 0)
        row.coordinate(withNormalizedOffset: CGVector(dx: 0.05, dy: 0.5)).click()
    }

    /// Suffixes added to `tab.pill.<paneId>` for nested elements (title
    /// label, edit field, context menu items). These must be stripped
    /// when scanning for pill identifiers so a child element doesn't
    /// masquerade as a pane id.
    private static let panePillChildSuffixes = [
        ".title",
        ".titleField",
        ".renamePane",
        ".closePane"
    ]

    private func isPanePillRoot(_ identifier: String) -> Bool {
        guard identifier.hasPrefix("tab.pill.") else { return false }
        return !Self.panePillChildSuffixes.contains(where: { identifier.hasSuffix($0) })
    }

    /// Scan the app for the first `tab.pill.*` element and return the pane
    /// id portion of its identifier. Returns nil if no pill exists.
    private func firstPanePillId(in app: XCUIApplication) -> String? {
        let query = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "tab.pill.")
        )
        for i in 0..<query.count {
            let id = query.element(boundBy: i).identifier
            guard isPanePillRoot(id) else { continue }
            return String(id.dropFirst("tab.pill.".count))
        }
        return nil
    }

    /// Collect all current `tab.pill.*` pane ids (in query order).
    private func panePillIds(in app: XCUIApplication) -> [String] {
        let query = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "tab.pill.")
        )
        var ids: [String] = []
        for i in 0..<query.count {
            let full = query.element(boundBy: i).identifier
            guard isPanePillRoot(full) else { continue }
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

    /// Closing every pane of the Terminals tab empties that tab but must
    /// not quit the app when other user projects are still alive. The
    /// Terminals sidebar group's `+` button remains available so the
    /// user can add a fresh terminal tab.
    ///
    /// Pre-`7c8c0aa` the last-pane exit surfaced a "Quit NICE?" sheet;
    /// that behavior was replaced when Terminals became a multi-tab
    /// group (tabs can always be re-added from the group `+`). This
    /// test pins the new contract so neither the sheet nor a silent
    /// app-terminate creeps back in.
    func testTerminalsCloseLastPaneKeepsAppAliveWithOtherProjects() throws {
        let socketPath = Self.testSocketPath
        try? FileManager.default.removeItem(atPath: socketPath)
        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude(),
            "NICE_SOCKET_PATH": socketPath
        ])

        let terminalsRow = app.descendants(matching: .any)["sidebar.terminals"]
        XCTAssertTrue(terminalsRow.waitForExistence(timeout: 5))

        // Seed a user session so a non-Terminals project tab exists.
        // Without this, closing Terminals' only tab would empty every
        // project and legitimately terminate the app.
        try createTabViaSocket(in: app, socketPath: socketPath)

        // Select the Terminals tab (the Main terminal inside it).
        terminalsRow.click()

        let pillReady = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "tab.pill.") >= 1
            }),
            object: nil
        )
        XCTAssertEqual(XCTWaiter.wait(for: [pillReady], timeout: 5), .completed)

        // Snapshot which pills belong to the Terminals tab *now*, while
        // it's still the active tab. Once we close them all, the active
        // tab switches away and the toolbar repaints with the other
        // project's pills — we'd lose the Terminals pill ids otherwise.
        let terminalsPillIds = panePillIds(in: app)
        XCTAssertFalse(
            terminalsPillIds.isEmpty,
            "Terminals tab should have at least one pane pill after selecting it"
        )

        // Close every pane in the active Terminals tab via its close
        // button. The last close dissolves the tab entirely.
        for id in terminalsPillIds {
            let closeButton = app.descendants(matching: .any)
                .matching(NSPredicate(format: "identifier == %@", "tab.close.\(id)"))
                .element(boundBy: 0)
            XCTAssertTrue(closeButton.waitForExistence(timeout: 5))
            closeButton.click()
        }

        // No Quit sheet should appear — other projects still have
        // tabs, so the app stays alive.
        let sheet = app.windows.firstMatch.sheets.firstMatch
        let noSheet = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in !sheet.exists }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [noSheet], timeout: 3), .completed,
            "No Quit sheet should appear when Terminals empties while another project has tabs"
        )

        // The Terminals sidebar group row stays (pinned project) with
        // its `+` button available so the user can add another terminal
        // tab on demand.
        let terminalsGroupAdd = app.descendants(matching: .any)["sidebar.group.terminals.add"]
        XCTAssertTrue(
            terminalsGroupAdd.waitForExistence(timeout: 5),
            "Terminals sidebar group's `+` button must remain available after its last tab dissolves"
        )

        // App is still running — the XCUIApplication state should be
        // running (not terminated). Use a quick query that would throw
        // on a dead app.
        XCTAssertEqual(
            app.state, .runningForeground,
            "App must stay alive; other projects still have tabs"
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

        let pill = firstDescendant(
            in: app,
            withIdentifierPrefix: "tab.pill.",
            excludingInfixes: Self.panePillChildSuffixes
        )
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

        clickSidebarRow(in: app, rowIdentifier: rowId)

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

    // MARK: - Session-id rotation across restart

    /// End-to-end: a session-id rotation that lands while the app is
    /// running must survive a clean quit + relaunch. Exercises the
    /// full path: socket → SessionsModel.handleClaudeSessionUpdate →
    /// TabModel mutation → SessionStore debounce → flush on quit →
    /// SessionStore.read on fresh launch.
    ///
    /// The hook script itself is unit-tested separately (see
    /// ClaudeHookInstallerTests). The seam under test here is the
    /// socket → store → resume path, so the test bypasses the hook
    /// and forces the rotation by sending `session_update` directly.
    /// Fake claude is `/bin/cat` (the existing `fakeClaude()` helper)
    /// — we never need real claude to fire the hook in this test.
    func testSessionIdRotation_persistsAcrossRestart() throws {
        let socketPath = Self.testSocketPath
        try? FileManager.default.removeItem(atPath: socketPath)

        // === Phase 1: launch, create tab, force rotation, verify
        // persisted, quit cleanly ===
        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude(),
            "NICE_SOCKET_PATH": socketPath,
        ])
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        let sidebarRowId = try createTabViaSocket(in: app, socketPath: socketPath)
        let createdTabId = self.tabId(from: sidebarRowId)

        // sessions.json sandbox path. scripts/test.sh patches
        // PRODUCT_BUNDLE_IDENTIFIER (to dev.nickanderssohn.nice-dev)
        // but deliberately leaves PRODUCT_NAME as "Nice" so the
        // Swift module name stays stable for `@testable import Nice`.
        // SessionStore keys the support folder off CFBundleName, so
        // the file lands under "Nice/", not "Nice Dev/".
        let sessionsFile = (fakeHomePath() as NSString)
            .appendingPathComponent(
                "Library/Application Support/Nice/sessions.json"
            )

        // Wait for the initial save (debounced 500ms) to land. Both
        // the claudeSessionId and the claude paneId come from the
        // persisted file — driving the rotation through the UI's
        // toolbar pills would require parsing pane kinds out of
        // identifiers that don't carry them.
        let initialPersisted = waitForPersistedSessionId(
            at: sessionsFile, tabId: createdTabId, timeout: 5
        )
        XCTAssertNotNil(
            initialPersisted,
            "Initial claudeSessionId must be persisted after tab creation"
        )
        let claudePaneId = try XCTUnwrap(
            findClaudePaneId(in: sessionsFile, tabId: createdTabId),
            "Persisted tab must contain a claude pane"
        )

        // Force a rotation: the hook would do this on /clear,
        // /branch, etc. — we send the same socket message directly.
        let postRotationId = UUID().uuidString.lowercased()
        let updateJSON = #"{"action":"session_update","paneId":"\#(claudePaneId)","sessionId":"\#(postRotationId)"}"#
        try sendSocketLine(updateJSON, to: socketPath)

        // Wait for the persisted file to reflect the rotated id.
        let rotated = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.readPersistedSessionId(at: sessionsFile, tabId: createdTabId)
                    == postRotationId
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [rotated], timeout: 5), .completed,
            "sessions.json must reflect the rotated id within 5s"
        )

        // Clean quit so flush() runs synchronously and any pending
        // debounce is cancelled with the latest state on disk.
        app.terminate()

        // === Phase 2: relaunch into the same sandbox; the persisted
        // id must be the rotated one, not the initial one ===
        let app2 = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude(),
            "NICE_SOCKET_PATH": socketPath,
        ])
        XCTAssertTrue(
            app2.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        // The on-disk state is the contract under test — read it
        // directly rather than driving the app to surface it.
        let resumedId = readPersistedSessionId(
            at: sessionsFile, tabId: createdTabId
        )
        XCTAssertEqual(
            resumedId, postRotationId,
            "After relaunch, the persisted claudeSessionId must be the post-rotation uuid"
        )
    }

    /// Find the Claude pane id from the persisted sessions.json by
    /// walking windows → projects → tabs (matched by `tabId`) →
    /// panes and returning the first pane with `kind == "claude"`.
    /// Returns nil if the file isn't there yet, doesn't parse, or
    /// the tab has no claude pane.
    ///
    /// Reading from disk (rather than from the toolbar's
    /// `tab.pill.<paneId>` accessibility identifiers) is deliberate:
    /// the toolbar pill identifier doesn't expose pane kind, and the
    /// `.claudeIcon` identifier lives on the sidebar row keyed by
    /// `tabId`, not paneId — which made the earlier UI-based lookup
    /// return a tabId-shaped string instead of a paneId.
    private func findClaudePaneId(
        in sessionsFile: String, tabId: String
    ) -> String? {
        guard let data = try? Data(contentsOf: URL(fileURLWithPath: sessionsFile)),
              let root = try? JSONSerialization.jsonObject(with: data)
                as? [String: Any],
              let windows = root["windows"] as? [[String: Any]]
        else { return nil }
        for window in windows {
            for project in (window["projects"] as? [[String: Any]]) ?? [] {
                for tab in (project["tabs"] as? [[String: Any]]) ?? []
                where (tab["id"] as? String) == tabId {
                    let panes = (tab["panes"] as? [[String: Any]]) ?? []
                    for pane in panes where (pane["kind"] as? String) == "claude" {
                        return pane["id"] as? String
                    }
                }
            }
        }
        return nil
    }

    /// Read `sessions.json` and return the `claudeSessionId` for
    /// `tabId`, scanning every window/project. Returns nil if the
    /// file doesn't exist, doesn't parse, or the tab isn't there.
    private func readPersistedSessionId(
        at path: String, tabId: String
    ) -> String? {
        guard let data = try? Data(contentsOf: URL(fileURLWithPath: path)),
              let root = try? JSONSerialization.jsonObject(with: data)
                as? [String: Any],
              let windows = root["windows"] as? [[String: Any]]
        else { return nil }
        for window in windows {
            let projects = (window["projects"] as? [[String: Any]]) ?? []
            for project in projects {
                let tabs = (project["tabs"] as? [[String: Any]]) ?? []
                for tab in tabs where (tab["id"] as? String) == tabId {
                    return tab["claudeSessionId"] as? String
                }
            }
        }
        return nil
    }

    /// Block until `readPersistedSessionId` returns non-nil for
    /// `tabId`, or `timeout` seconds elapse. Returns the value or
    /// nil on timeout.
    private func waitForPersistedSessionId(
        at path: String, tabId: String, timeout: TimeInterval
    ) -> String? {
        let appeared = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.readPersistedSessionId(at: path, tabId: tabId) != nil
            }),
            object: nil
        )
        guard XCTWaiter.wait(for: [appeared], timeout: timeout) == .completed
        else { return nil }
        return readPersistedSessionId(at: path, tabId: tabId)
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

    // MARK: - Settings window

    /// Regression: clicking the sidebar settings gear must open the
    /// Settings window. Pre-fix the gear called a stale `showSettingsWindow:`
    /// selector path that silently failed; this guards the
    /// `@Environment(\.openSettings)` wiring.
    func testSettingsGearOpensSettingsWindow() throws {
        let app = launchApp()
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        let gear = app.descendants(matching: .any)["sidebar.settings"]
        XCTAssertTrue(
            gear.waitForExistence(timeout: 5),
            "Sidebar settings gear should exist"
        )
        gear.click()

        let settingsRoot = app.descendants(matching: .any)["settings.root"]
        XCTAssertTrue(
            settingsRoot.waitForExistence(timeout: 5),
            "Clicking the sidebar settings gear must open the Settings window"
        )
    }

    // MARK: - Typing `claude` in a pane (regression tests)
    //
    // These exercise the full zsh-shadow → control-socket → AppState
    // path. Every interactive `claude` invocation — in the built-in
    // Terminals tab or in a companion terminal inside an existing
    // Claude tab — must post a `newtab` message and produce a fresh
    // sidebar session. The invariant "at most one Claude pane per tab"
    // depends on the companion path going this way too, rather than
    // promoting the current tab.

    /// Focus the main terminal area of the app window. XCUITest sends
    /// keystrokes to whichever view has focus, so every typing test
    /// needs this first. 0.6 / 0.5 lands well right of the 240pt
    /// sidebar regardless of window size.
    private func focusMainTerminal(in app: XCUIApplication) {
        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 5))
        window.coordinate(withNormalizedOffset: CGVector(dx: 0.6, dy: 0.5))
            .click()
    }

    /// Regression: typing `claude` in the built-in Terminals tab must
    /// fire the `newtab` control-socket path (creating a new sidebar
    /// session). A `sidebar.tab.*` row must appear after typing, and
    /// the app must still be running.
    func testTypeClaudeInTerminalsFiresNewtab() throws {
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

        // Give zsh a moment to finish loading .zshrc and defining the
        // `claude()` shadow function.
        Thread.sleep(forTimeInterval: 1.0)

        focusMainTerminal(in: app)
        app.typeText("claude\n")

        // The shadow should post a newtab message over the socket,
        // which the app processes on MainActor, creating a new sidebar
        // row. Filter out the sub-element icon identifiers.
        let tabQuery = app.descendants(matching: .any).matching(
            NSPredicate(
                format: "identifier BEGINSWITH %@ AND NOT (identifier CONTAINS %@) AND NOT (identifier CONTAINS %@) AND NOT (identifier CONTAINS %@)",
                "sidebar.tab.",
                ".claudeIcon",
                ".terminalIcon",
                ".title"
            )
        )
        let appeared = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in tabQuery.count >= 1 }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [appeared], timeout: 10), .completed,
            "Typing `claude` in Terminals should create a new sidebar session via newtab"
        )

        // Sanity: app is still alive (pre-fix bug caused a crash here).
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"].exists,
            "App should still be running with sidebar visible"
        )
    }

    /// Regression: typing `claude` repeatedly in the Terminals tab must
    /// not destabilise the control socket. Each invocation opens a new
    /// sidebar session; the final count must match, and the app must
    /// remain responsive. This is the closest structural test for the
    /// socket-event storm crash short of driving claude's real subshell
    /// behaviour.
    func testTypeClaudeMultipleTimesInTerminalsIsStable() throws {
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
        Thread.sleep(forTimeInterval: 1.0)

        let tabQuery = app.descendants(matching: .any).matching(
            NSPredicate(
                format: "identifier BEGINSWITH %@ AND NOT (identifier CONTAINS %@) AND NOT (identifier CONTAINS %@) AND NOT (identifier CONTAINS %@)",
                "sidebar.tab.",
                ".claudeIcon",
                ".terminalIcon",
                ".title"
            )
        )

        let rounds = 3
        for i in 1...rounds {
            // Each round needs to drop back into the Terminals tab so
            // the next `claude` invocation lands in its zsh pane.
            clickSidebarRow(in: app, rowIdentifier: "sidebar.terminals")
            Thread.sleep(forTimeInterval: 0.3)
            focusMainTerminal(in: app)
            app.typeText("claude\n")

            let expected = i
            let reached = XCTNSPredicateExpectation(
                predicate: NSPredicate(block: { _, _ in tabQuery.count >= expected }),
                object: nil
            )
            XCTAssertEqual(
                XCTWaiter.wait(for: [reached], timeout: 10), .completed,
                "Round \(i): expected >= \(expected) sidebar session rows after typing claude"
            )
        }

        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"].exists,
            "App must survive repeated `claude` invocations"
        )
    }

    /// Regression: the "at most one Claude pane per tab" invariant.
    /// Typing `claude` in a companion terminal inside an existing
    /// Claude tab must open a NEW sidebar session (via `newtab`), not
    /// add a second Claude pane to the current tab. Before the fix,
    /// the shadow fired `promoteTab` which flipped the terminal pane
    /// to claude and appended a fresh terminal — leaving the original
    /// tab with two Claude panes, the state that produced the
    /// sidebar/toolbar status-dot drift.
    func testTypeClaudeInCompanionFiresNewtab() throws {
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

        // Create a Claude tab via the socket; the new tab becomes active
        // and shows two pane pills (claude + companion terminal).
        let originalRowId = try createTabViaSocket(in: app, socketPath: socketPath)
        let originalTid = tabId(from: originalRowId)
        iconElement(in: app, identifier: originalRowId).click()

        // Wait for the original tab's two pane pills to render.
        let twoPills = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "tab.pill.") == 2
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [twoPills], timeout: 5), .completed,
            "Precondition: Claude tab should have exactly two pills (claude + companion terminal)."
        )

        // Click the companion terminal pill to make it active, then
        // focus the main area and type `claude`. The shadow must hit
        // `newtab`, not the removed `promoteTab`.
        let pillIds = panePillIds(in: app)
        XCTAssertEqual(pillIds.count, 2)
        let terminalPillId = pillIds.first { !$0.hasSuffix("-claude") }
            ?? pillIds[1]
        let terminalPill = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", "tab.pill.\(terminalPillId)"))
            .element(boundBy: 0)
        XCTAssertTrue(terminalPill.waitForExistence(timeout: 3))
        terminalPill.click()

        // Give zsh a moment to load .zshrc and define the `claude()`
        // shadow function inside the companion terminal's pty.
        Thread.sleep(forTimeInterval: 1.0)

        focusMainTerminal(in: app)
        app.typeText("claude\n")

        // A second sidebar session must appear.
        let tabQuery = app.descendants(matching: .any).matching(
            NSPredicate(
                format: "identifier BEGINSWITH %@ AND NOT (identifier CONTAINS %@) AND NOT (identifier CONTAINS %@) AND NOT (identifier CONTAINS %@)",
                "sidebar.tab.",
                ".claudeIcon",
                ".terminalIcon",
                ".title"
            )
        )
        let appeared = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in tabQuery.count >= 2 }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [appeared], timeout: 10), .completed,
            "Typing `claude` in a companion terminal must open a new sidebar session via newtab."
        )

        // Original tab must still exist and still have exactly two
        // pane pills — no promotion, no extra Claude pane.
        iconElement(in: app, identifier: originalRowId).click()
        let stillTwoPills = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                self.countElements(in: app, withIdentifierPrefix: "tab.pill.") == 2
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [stillTwoPills], timeout: 5), .completed,
            "Original tab must still have exactly two pane pills — the \"one Claude pane per tab\" invariant forbids a third."
        )
        XCTAssertTrue(
            iconElement(in: app, identifier: "sidebar.tab.\(originalTid).claudeIcon").exists,
            "Original tab should still have its (single) Claude pane visible."
        )
    }

    /// Regression: typing `claude` in the Main Terminal when the cwd
    /// isn't under any existing project must create a **new project
    /// group** in the sidebar — not append the Claude tab to the pinned
    /// Terminals group. The bug: `addTabToProjects` longest-prefix-
    /// matched the cwd against every project including Terminals
    /// (whose path is seeded from the Main Terminal cwd, i.e. $HOME),
    /// so any claude invocation under $HOME got stuffed under Terminals.
    ///
    /// Observable signal: each project group renders an Add button with
    /// identifier `sidebar.group.<project.id>.add`. Pre-invocation only
    /// `sidebar.group.terminals.add` exists; after a successful fix a
    /// second `sidebar.group.p-*.add` must appear.
    func testTypeClaudeInTerminalsCreatesNewProjectGroup() throws {
        let socketPath = Self.testSocketPath
        try? FileManager.default.removeItem(atPath: socketPath)
        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude(),
            "NICE_SOCKET_PATH": socketPath,
        ])
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        // Precondition: only the Terminals group exists at launch.
        // Counts both the container (`sidebar.group.terminals`) and
        // its add button (`sidebar.group.terminals.add`), which share
        // the prefix.
        let baselineGroupQuery = app.descendants(matching: .any).matching(
            NSPredicate(
                format: "identifier BEGINSWITH %@ AND NOT (identifier ENDSWITH %@)",
                "sidebar.group.",
                ".add"
            )
        )
        XCTAssertEqual(
            baselineGroupQuery.count, 1,
            "Launch state should have exactly one project group (Terminals)"
        )
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.group.terminals"].exists,
            "Terminals group container must exist as the identifier baseline"
        )

        Thread.sleep(forTimeInterval: 1.0)
        focusMainTerminal(in: app)
        app.typeText("claude\n")

        // A fresh project group (non-Terminals) must appear — otherwise
        // the claude tab got bucketed into Terminals despite the
        // `addTabToProjects` exclusion filter.
        //
        // Non-Terminals groups hide their `+` button at opacity 0 until
        // hover, which also removes it from the a11y tree — so the test
        // targets the group container identifier (`sidebar.group.<id>`)
        // rather than the `.add` child, which would only materialize
        // under the cursor.
        let newGroupQuery = app.descendants(matching: .any).matching(
            NSPredicate(
                format: "identifier BEGINSWITH %@ AND NOT (identifier ENDSWITH %@) AND NOT (identifier CONTAINS %@)",
                "sidebar.group.",
                ".add",
                "terminals"
            )
        )
        let appeared = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in newGroupQuery.count >= 1 }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [appeared], timeout: 10), .completed,
            "Typing `claude` in Terminals should create a new project group — not append a Claude tab to the pinned Terminals group"
        )
    }

    // MARK: - Settings appearance pane

    /// Opens the Settings window and navigates to the Appearance pane.
    /// Returns the app; the caller can query theme cells or the sync
    /// toggle against it.
    private func openAppearancePane(_ app: XCUIApplication) {
        let gear = app.descendants(matching: .any)["sidebar.settings"]
        XCTAssertTrue(gear.waitForExistence(timeout: 5))
        gear.click()

        XCTAssertTrue(
            app.descendants(matching: .any)["settings.root"]
                .waitForExistence(timeout: 5),
            "Settings window must open before navigating panes"
        )

        // Target the sidebar row by its stable identifier — the pane
        // title on the right also renders "Appearance" as a plain
        // staticText, so a raw-label lookup would be ambiguous.
        let row = app.descendants(matching: .any)["settings.section.appearance"]
        XCTAssertTrue(row.waitForExistence(timeout: 3))
        row.click()
    }

    /// The Appearance pane must offer the sync toggle, scheme picker,
    /// and two per-scheme chrome palette pickers. Guards against the
    /// old 2×2 `ThemeButtonGrid` cells leaking back (they shouldn't
    /// exist any more) and against rename drift on the new ids.
    func testSettingsAppearance_showsSchemeAndPerSchemeChromePickers() throws {
        let app = launchApp()
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        openAppearancePane(app)

        for id in [
            "settings.theme.sync",
            "settings.appearance.scheme",
            "settings.appearance.chromeLight",
            "settings.appearance.chromeDark",
        ] {
            XCTAssertTrue(
                app.descendants(matching: .any)[id].waitForExistence(timeout: 3),
                "Missing Appearance control \(id)"
            )
        }

        for legacyId in [
            "settings.theme.cell.niceLight",
            "settings.theme.cell.niceDark",
            "settings.theme.cell.macLight",
            "settings.theme.cell.macDark",
        ] {
            XCTAssertFalse(
                app.descendants(matching: .any)[legacyId].exists,
                "Legacy 2×2 theme grid cell \(legacyId) should be gone"
            )
        }
    }

    /// Legacy `-theme niceLight` launch arg must still seed the
    /// Appearance pane to its matching chrome state. Migration on
    /// launch converts the old single-choice value to
    /// `chromeLightPalette == .nice` (+ `chromeDarkPalette == .nice`),
    /// and the scheme picker lands on Light because that's what
    /// niceLight scheme is.
    func testSettingsAppearance_legacyThemeLaunchArgStillSeedsState() throws {
        let app = XCUIApplication()
        app.launchArguments += [
            "-ApplePersistenceIgnoreState", "YES",
            "-theme", "niceLight",
            "-syncWithOS", "NO",
        ]
        applySandboxEnv(to: app)
        app.launch()
        track(app)

        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        openAppearancePane(app)

        // The segmented scheme picker exposes the selected value
        // directly; chrome pickers expose their current selection
        // through `.value`. Both forms must reflect migration.
        let scheme = app.descendants(matching: .any)["settings.appearance.scheme"]
        XCTAssertTrue(scheme.waitForExistence(timeout: 3))

        let chromeLight = app.descendants(matching: .any)["settings.appearance.chromeLight"]
        XCTAssertTrue(chromeLight.waitForExistence(timeout: 3))
        XCTAssertTrue(
            (chromeLight.value as? String)?.lowercased().contains("nice") ?? false,
            "Legacy niceLight should migrate chromeLightPalette → Nice; got value=\(chromeLight.value ?? "nil")"
        )
    }

    // MARK: - Settings terminal pane

    private func openTerminalPane(_ app: XCUIApplication) {
        let gear = app.descendants(matching: .any)["sidebar.settings"]
        XCTAssertTrue(gear.waitForExistence(timeout: 5))
        gear.click()
        XCTAssertTrue(
            app.descendants(matching: .any)["settings.root"]
                .waitForExistence(timeout: 5),
            "Settings window must open before navigating panes"
        )
        // Terminal-theme controls live under the Appearance section
        // since the two were merged.
        let row = app.descendants(matching: .any)["settings.section.appearance"]
        XCTAssertTrue(row.waitForExistence(timeout: 3))
        row.click()
    }

    /// The Terminal pane must surface both per-scheme theme pickers
    /// and the Ghostty import button. Accessibility ids here are the
    /// hook for future finer-grained picker-contents tests, which we
    /// don't attempt to drive through the menu-style picker popup.
    func testSettingsTerminal_showsPerSchemePickersAndImportButton() throws {
        let app = launchApp()
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        openTerminalPane(app)

        for id in [
            "settings.terminal.lightPicker",
            "settings.terminal.darkPicker",
            "settings.terminal.import",
        ] {
            XCTAssertTrue(
                app.descendants(matching: .any)[id].waitForExistence(timeout: 3),
                "Missing Terminal pane control \(id)"
            )
        }
    }

    /// Launch-arg-seeded terminal theme ids must render on the
    /// per-scheme pickers. Guards the wiring from UserDefaults →
    /// `Tweaks.terminalThemeLightId` / `...DarkId` → SwiftUI
    /// picker selection.
    func testSettingsTerminal_pickersReflectSeededIds() throws {
        let app = XCUIApplication()
        app.launchArguments += [
            "-ApplePersistenceIgnoreState", "YES",
            "-terminalThemeLightId", "solarized-light",
            "-terminalThemeDarkId",  "dracula",
        ]
        applySandboxEnv(to: app)
        app.launch()
        track(app)

        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        openTerminalPane(app)

        let lightPicker = app.descendants(matching: .any)["settings.terminal.lightPicker"]
        XCTAssertTrue(lightPicker.waitForExistence(timeout: 3))
        XCTAssertTrue(
            (lightPicker.value as? String)?.lowercased().contains("solarized") ?? false,
            "Light-slot picker should show Solarized Light; got value=\(lightPicker.value ?? "nil")"
        )

        let darkPicker = app.descendants(matching: .any)["settings.terminal.darkPicker"]
        XCTAssertTrue(darkPicker.waitForExistence(timeout: 3))
        XCTAssertTrue(
            (darkPicker.value as? String)?.lowercased().contains("dracula") ?? false,
            "Dark-slot picker should show Dracula; got value=\(darkPicker.value ?? "nil")"
        )
    }

    // MARK: - Settings font pane

    private func openFontPane(_ app: XCUIApplication) {
        let gear = app.descendants(matching: .any)["sidebar.settings"]
        XCTAssertTrue(gear.waitForExistence(timeout: 5))
        gear.click()
        XCTAssertTrue(
            app.descendants(matching: .any)["settings.root"]
                .waitForExistence(timeout: 5)
        )
        let row = app.descendants(matching: .any)["settings.section.font"]
        XCTAssertTrue(row.waitForExistence(timeout: 3))
        row.click()
    }

    /// The Font pane must expose the terminal font family picker.
    /// Guards against the picker going missing or its id drifting;
    /// the curated candidate list itself is covered by unit tests.
    func testSettingsFont_showsTerminalFontFamilyPicker() throws {
        let app = launchApp()
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5)
        )

        openFontPane(app)

        let picker = app.descendants(matching: .any)["settings.font.terminalFamily"]
        XCTAssertTrue(
            picker.waitForExistence(timeout: 3),
            "Terminal font family picker must be present in the Font pane"
        )
    }

    // MARK: - Multi-window isolation

    /// ⌘N opens a second window, and per-window state (terminal pills,
    /// keyboard-shortcut dispatch) is isolated between them. Proving
    /// isolation without sockets: each window starts with one Terminals
    /// pill (total 2 across both); pressing ⌘T in the focused window
    /// adds exactly one pane (total 3, not 4) — if state leaked, both
    /// windows would react.
    func testMultiWindowIsolation() throws {
        // No NICE_SOCKET_PATH override: that env var would force both
        // windows to bind the same socket path. This test exercises the
        // default per-window path (nice-<pid>-<uuid>.sock).
        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude()
        ])

        let firstSidebar = app.descendants(matching: .any)["sidebar.terminals"]
        XCTAssertTrue(
            firstSidebar.waitForExistence(timeout: 5),
            "First window's Terminals row should appear on launch"
        )

        // SwiftUI accessibility exposes each pill multiple times (view
        // + hosting layer), so compare unique ids rather than raw
        // element counts.
        func uniquePillIds() -> Set<String> { Set(panePillIds(in: app)) }

        let initialWindowCount = app.windows.count
        let initialPills = uniquePillIds()
        XCTAssertEqual(
            initialPills.count, 1,
            "First window should have exactly one unique terminal pill on launch (got \(initialPills))"
        )

        // WindowGroup auto-binds ⌘N to File > New Window on macOS.
        app.typeKey("n", modifierFlags: .command)

        let twoWindows = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                app.windows.count > initialWindowCount
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [twoWindows], timeout: 5), .completed,
            "⌘N should open a second window"
        )

        // Each window carries its own AppState with its own Terminals
        // tab and its own initial pill. A shared AppState would still
        // surface a single pill id across both windows.
        let bothPills = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                uniquePillIds().count == 2
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [bothPills], timeout: 5), .completed,
            "Each window should have its own terminal pill id (got \(uniquePillIds()))"
        )

        // ⌘T routes to the focused window via WindowRegistry. Exactly
        // one new pill id should appear. If the shortcut leaked to both
        // windows we'd see 4 unique ids; if it produced no pill (wrong
        // AppState targeted) we'd still see 2.
        app.typeKey("t", modifierFlags: .command)

        let threePills = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                uniquePillIds().count == 3
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [threePills], timeout: 5), .completed,
            "⌘T should add exactly one pill to the focused window (got \(uniquePillIds()))"
        )
    }

    // MARK: - Per-window-close persistence (off-main flush end-to-end)

    /// End-to-end pin for the per-window-close persistence path: the
    /// user closes a single window (red traffic light) while the app
    /// keeps running. `WindowSession.tearDown` runs `store.upsert` +
    /// `store.flush` on `SessionStore`'s background `ioQueue`, and the
    /// flush blocks main on a semaphore until the off-main write
    /// completes. The test:
    ///   1. Launches Nice (window A) into a sandboxed Application
    ///      Support root.
    ///   2. Opens a second window (B) via ⌘N.
    ///   3. Creates a Claude tab in window B via the socket so its
    ///      persisted snapshot is distinguishable.
    ///   4. Waits for the first debounced save to land sessions.json.
    ///   5. Closes window B via the close button, confirming the
    ///      "live panes" alert if it appears.
    ///   6. Polls sessions.json until the post-close write lands. If
    ///      the off-main `ioQueue` write never runs (e.g. a future
    ///      regression that drops the queue or starves it under
    ///      QoS pressure on a busy CI box), this poll times out.
    /// Note: the test does NOT call `app.terminate()` — that path
    /// already has coverage via `testSessionIdRotation_persistsAcrossRestart`.
    /// What's unique here is verifying the non-terminate flush trigger
    /// reaches disk through the new `ioQueue` plumbing.
    func testPerWindowClose_flushesOffMainWriteToDisk() throws {
        let socketPath = Self.testSocketPath
        try? FileManager.default.removeItem(atPath: socketPath)

        let app = launchApp(extraEnv: [
            "NICE_CLAUDE_OVERRIDE": fakeClaude(),
            "NICE_SOCKET_PATH": socketPath,
        ])
        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 5),
            "Window A should appear on launch."
        )
        let windowsBefore = app.windows.count

        // Open window B.
        app.typeKey("n", modifierFlags: .command)
        let twoWindows = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                app.windows.count > windowsBefore
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [twoWindows], timeout: 5), .completed,
            "⌘N must open window B."
        )

        // Create a tab in the focused window (B) so the per-window
        // snapshot has identifiable content. createTabViaSocket waits
        // for the row to appear; we don't need its return value, just
        // the resulting state mutation that the close should persist.
        _ = try createTabViaSocket(in: app, socketPath: socketPath)

        let sessionsFile = (fakeHomePath() as NSString)
            .appendingPathComponent(
                "Library/Application Support/Nice/sessions.json"
            )

        // Wait for the initial debounced save to land. Without this,
        // a fast close-then-poll could observe the file before any
        // write has happened and conclude (incorrectly) that the
        // close-time flush worked.
        let initialSave = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                FileManager.default.fileExists(atPath: sessionsFile)
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [initialSave], timeout: 5), .completed,
            "Debounced save must land sessions.json within 5s."
        )
        XCTAssertGreaterThanOrEqual(
            readPersistedWindowCount(at: sessionsFile), 1,
            "Pre-close: at least one window must be persisted."
        )

        // Settle: any debounce in flight from createTabViaSocket
        // should land before we sample `preCloseMtime`. Without this
        // settle, a debounced write that fires *after* preCloseMtime
        // capture but *before* the close-time flush could be the only
        // source of mtime advancement, masking a flush regression.
        let settle = XCTestExpectation(description: "debounce settled")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.7) {
            settle.fulfill()
        }
        wait(for: [settle], timeout: 2.0)

        let preCloseMtime = (try? FileManager.default.attributesOfItem(
            atPath: sessionsFile
        )[.modificationDate] as? Date) ?? .distantPast

        // Close the focused window (B) via the red traffic light.
        // `XCUIIdentifierCloseWindow` is the standard accessibility
        // identifier AppKit assigns to the close button.
        let focused = app.windows.element(boundBy: 0)
        let closeButton = focused.buttons[XCUIIdentifierCloseWindow]
        XCTAssertTrue(
            closeButton.waitForExistence(timeout: 2),
            "Close button must be reachable on window B."
        )
        closeButton.click()

        // The seed terminal pane triggers the live-panes confirmation.
        // Confirm with "Close" so the close proceeds; if no dialog
        // appears (e.g. no live panes), this branch is a fast no-op.
        let confirm = app.dialogs.firstMatch.buttons["Close"]
        if confirm.waitForExistence(timeout: 2) {
            confirm.click()
        }

        // Window count drops back to the pre-⌘N count once the close
        // delivers willClose → tearDown → registry removal.
        let backToOne = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                app.windows.count == windowsBefore
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [backToOne], timeout: 5), .completed,
            "Closing window B should leave only the original window(s)."
        )

        // The post-close flush enqueues a write on the off-main
        // `ioQueue` and blocks main via semaphore. Poll until the
        // file is updated past `preCloseMtime`. A regression that
        // bypassed `ioQueue` (or one that broke the semaphore and let
        // `flush` return before the write completed) would either
        // leave the file stale or write nothing.
        let postCloseWrite = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                guard let m = (try? FileManager.default.attributesOfItem(
                    atPath: sessionsFile
                )[.modificationDate] as? Date) else { return false }
                return m > preCloseMtime
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [postCloseWrite], timeout: 5), .completed,
            "Per-window-close must reach disk via ioQueue within 5s."
        )

        // Sanity: the file is still well-formed JSON. A regression
        // that wrote partial data on close would corrupt this.
        let data = try Data(contentsOf: URL(fileURLWithPath: sessionsFile))
        XCTAssertNoThrow(
            try JSONSerialization.jsonObject(with: data),
            "Post-close sessions.json must be valid JSON."
        )
    }

    /// Count windows in `sessions.json`. Returns 0 if the file is
    /// missing or fails to parse.
    private func readPersistedWindowCount(at path: String) -> Int {
        guard let data = try? Data(contentsOf: URL(fileURLWithPath: path)),
              let root = try? JSONSerialization.jsonObject(with: data)
                as? [String: Any],
              let windows = root["windows"] as? [[String: Any]]
        else { return 0 }
        return windows.count
    }

    // MARK: - Inline tab rename

    /// Tapping the active tab's title swaps the row's `Text` for a
    /// `TextField` (`sidebar.tab.<id>.titleField`). Typing a new name
    /// and pressing Return commits the rename and restores the `Text`
    /// branch with the updated label. Guards against regressions where
    /// the edit-mode swap fails (which is what hid today behind "I
    /// couldn't tell I was in edit mode" — the field not appearing at
    /// all looks identical to the field appearing unstyled).
    func testTapActiveTabTitleEntersEditModeAndCommits() throws {
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
        let fieldId = "sidebar.tab.\(tid).titleField"

        // The freshly-created tab is already active. Clicking the row
        // centroid hits the title Text's inner `.onTapGesture`, which
        // only triggers edit mode on the active tab. If the SwiftUI
        // gesture priority regressed (parent row's selectTab gesture
        // swallowing the tap), the field below never appears and the
        // test fails — guarding the exact path that hid today behind
        // "I couldn't tell I was in edit mode."
        let row = iconElement(in: app, identifier: rowId)
        XCTAssertTrue(row.waitForExistence(timeout: 5))
        row.click()

        let fieldElement = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", fieldId))
            .element(boundBy: 0)
        let fieldAppeared = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in fieldElement.exists }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [fieldAppeared], timeout: 5), .completed,
            "Clicking the active tab's title must swap in `.titleField`."
        )

        // Replace the title and commit with Return. `typeText` targets
        // the focused field directly — the TextField grabs focus on
        // appearance via `$titleFocused = true`.
        let newName = "Renamed by UI test"
        app.typeKey("a", modifierFlags: .command)
        app.typeText(newName)
        app.typeKey(XCUIKeyboardKey.return.rawValue, modifierFlags: [])

        let fieldDismissed = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in !fieldElement.exists }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [fieldDismissed], timeout: 3), .completed,
            "Pressing Return must commit the rename and dismiss the field."
        )

        // The renamed title now renders as a static text under the
        // sidebar row. Any staticTexts query matching the new name is a
        // reliable signal the `renameTab` / `Text` swap-back landed.
        let renamedText = app.staticTexts[newName]
        let titleUpdated = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in renamedText.exists }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [titleUpdated], timeout: 5), .completed,
            "Sidebar row should display the renamed title after commit."
        )
    }

    // MARK: - Inline pane pill rename

    /// Tapping the active pane pill's title swaps the pill's `Text` for
    /// a `TextField` (`tab.pill.<paneId>.titleField`). Typing a new
    /// name and pressing Return commits the rename and the pill renders
    /// the new label. Mirrors `testTapActiveTabTitleEntersEditModeAndCommits`
    /// in shape — same gating logic, different identifier namespace.
    func testTapActivePanePillTitleEntersEditModeAndCommits() throws {
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

        guard let paneId = firstPanePillId(in: app) else {
            XCTFail("No tab.pill.* element found")
            return
        }
        let titleId = "tab.pill.\(paneId).title"
        let fieldId = "tab.pill.\(paneId).titleField"

        // The launch-time pane pill is already active; `onAppear`
        // stamps `activatedAt`, and waiting on `pillReady` above ensures
        // we're well past `NSEvent.doubleClickInterval` (~0.5s) so the
        // first tap on the title qualifies as a deliberate rename.
        let title = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", titleId))
            .element(boundBy: 0)
        XCTAssertTrue(title.waitForExistence(timeout: 5))
        title.click()

        let fieldElement = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier == %@", fieldId))
            .element(boundBy: 0)
        let fieldAppeared = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in fieldElement.exists }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [fieldAppeared], timeout: 5), .completed,
            "Clicking the active pane pill title must swap in `.titleField`."
        )

        let newName = "Renamed pane"
        app.typeKey("a", modifierFlags: .command)
        app.typeText(newName)
        app.typeKey(XCUIKeyboardKey.return.rawValue, modifierFlags: [])

        let fieldDismissed = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in !fieldElement.exists }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [fieldDismissed], timeout: 3), .completed,
            "Pressing Return must commit the rename and dismiss the field."
        )

        let renamedText = app.staticTexts[newName]
        let titleUpdated = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in renamedText.exists }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [titleUpdated], timeout: 5), .completed,
            "Pane pill should display the renamed title after commit."
        )
    }

    // MARK: - Toolbar overflow chevron

    /// Adding panes one at a time eventually causes the overflow chevron
    /// (`tab.overflow`) to appear. Bounded loop; fails fast if the
    /// chevron's overflow detection ever stops wiring up.
    ///
    /// Note: only one UITest covers the chevron directly because
    /// `PaneStripGeometryTests` already exercises the overflow /
    /// offscreen / attention math exhaustively in pure Swift. This
    /// test's job is just to prove the geometry struct is actually
    /// wired into the view hierarchy.
    func testOverflowChevronAppearsAfterEnoughPanes() throws {
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

        // Sanity — chevron should NOT be visible with the single
        // launch-time pane.
        XCTAssertFalse(
            app.descendants(matching: .any)["tab.overflow"].exists,
            "Overflow chevron should not render when one pill fits trivially"
        )

        // Drive overflow: keep clicking `+` until the chevron appears.
        // Generous budget for wide monitors; tight per-iteration wait so
        // a regression fails in seconds, not minutes.
        let addButton = app.buttons["tab.add"]
        XCTAssertTrue(addButton.waitForExistence(timeout: 5))
        let chevron = app.descendants(matching: .any)["tab.overflow"]

        let budget = 20
        for _ in 0..<budget {
            addButton.click()
            if chevron.waitForExistence(timeout: 0.3) { break }
        }

        XCTAssertTrue(
            chevron.exists,
            "Overflow chevron must appear after adding up to \(budget) panes"
        )
    }

}
