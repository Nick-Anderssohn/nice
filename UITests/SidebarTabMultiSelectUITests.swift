//
//  SidebarTabMultiSelectUITests.swift
//  NiceUITests
//
//  End-to-end coverage for the multi-tab sidebar selection wiring:
//  the modifier-aware tap handler in `TabRow`, the dynamic context
//  menu (rename hidden / "Close N Tabs" label when count > 1), the
//  Esc collapse, the empty-area click collapse, the prune from
//  external dissolve, the keyboard-nav re-sync, and the actual
//  "close many" cascade. The selection model itself is unit-tested
//  in `SidebarTabSelectionTests`; these tests verify the SwiftUI /
//  AppKit wiring around it.
//
//  How we read selection state from XCUITest: each `TabRow` adds a
//  hidden zero-size sibling element with identifier
//  `sidebar.selectedTab.<id>` that EXISTS iff the row is in the
//  multi-selection set, and is absent otherwise. We assert on the
//  marker's `.exists` (which is reliable across the AppKit
//  accessibility bridge) instead of reading a `.value` off the row
//  itself — `.accessibilityElement(children: .contain)` on the row
//  doesn't surface a value to XCUIElement, and `.accessibilityValue`
//  on a zero-size hidden marker doesn't either. The marker namespace
//  is deliberately separate from `sidebar.tab.*` so existing
//  prefix-based queries in `NiceUITests` don't accidentally pick it
//  up.
//

import XCTest

final class SidebarTabMultiSelectUITests: NiceUITestCase {

    private var fakeHomeURL: URL?

    override func tearDownWithError() throws {
        if let url = fakeHomeURL {
            try? FileManager.default.removeItem(at: url)
        }
        fakeHomeURL = nil
        try super.tearDownWithError()
    }

    // MARK: - Tap routing

    /// Plain click on one tab, then Cmd-click another. Both rows
    /// should report `selected`. Baseline proving Cmd-click extends
    /// the selection set without dropping the previously-clicked tab.
    func testCmdClick_addsTabToSelection() throws {
        let app = launchAppForMultiSelect()
        let ids = addTerminalTabs(in: app, count: 2)

        clickRow(row(in: app, id: ids[0]))
        waitForSelected(in: app, rowId: ids[0])

        cmdClickRow(row(in: app, id: ids[1]), in: app)
        waitForSelected(in: app, rowId: ids[1])
        XCTAssertTrue(
            isSelected(in: app, rowId: ids[0]),
            "Cmd-click must not deselect the previously-clicked tab"
        )
    }

    /// Plain click after a multi-selection collapses back to just the
    /// clicked row.
    func testPlainClick_collapsesMultiSelection() throws {
        let app = launchAppForMultiSelect()
        let ids = addTerminalTabs(in: app, count: 3)

        clickRow(row(in: app, id: ids[0]))
        cmdClickRow(row(in: app, id: ids[1]), in: app)
        cmdClickRow(row(in: app, id: ids[2]), in: app)
        waitForSelected(in: app, rowId: ids[2])

        clickRow(row(in: app, id: ids[1]))
        waitForUnselected(in: app, rowId: ids[0])
        waitForUnselected(in: app, rowId: ids[2])
        XCTAssertTrue(
            isSelected(in: app, rowId: ids[1]),
            "Plain-click target must remain selected after the collapse"
        )
    }

    // MARK: - Context menu shape

    /// Right-clicking a tab that's part of the multi-selection shows
    /// `Close N Tabs` (not `Close Tab`) and hides the `Rename Tab`
    /// menu item entirely. The dynamic label and conditional rename
    /// are the user-visible payoff of this feature.
    func testRightClickInsideSelection_showsCloseNTabs_andHidesRename() throws {
        let app = launchAppForMultiSelect()
        let ids = addTerminalTabs(in: app, count: 2)

        clickRow(row(in: app, id: ids[0]))
        let rowB = row(in: app, id: ids[1])
        cmdClickRow(rowB, in: app)
        waitForSelected(in: app, rowId: ids[1])

        rowB.rightClick()

        let closeTwo = app.menuItems["Close 2 Tabs"]
        XCTAssertTrue(
            closeTwo.waitForExistence(timeout: 3),
            "Expected 'Close 2 Tabs' menu item when 2 tabs are selected"
        )
        XCTAssertFalse(
            app.menuItems["Rename Tab"].exists,
            "Rename Tab must be hidden when more than one tab is selected"
        )
        XCTAssertFalse(
            app.menuItems["Close Tab"].exists,
            "Singular 'Close Tab' must not appear alongside the plural label"
        )

        // Dismiss the menu so subsequent tests don't inherit it.
        app.typeKey(.escape, modifierFlags: [])
    }

    /// Right-clicking a tab that's NOT in the current selection
    /// shows the singular menu — `Rename Tab` + `Close Tab` — because
    /// `selectionIds(forRightClickOn:)` returns just `[clickedId]`
    /// when the click is outside the selection (pure read; no snap
    /// during view-builder eval). The visible snap mutation only
    /// happens inside the menu Button actions via
    /// `snapIfRightClickOutside`, so dismissing the menu without
    /// picking an action leaves the prior multi-selection intact —
    /// matches the equivalent contract in
    /// `FileBrowserContextMenu.onWillAct`.
    func testRightClickOutsideSelection_showsSingularMenu_andSnapOnAction() throws {
        let app = launchAppForMultiSelect()
        let ids = addTerminalTabs(in: app, count: 3)

        let rowA = row(in: app, id: ids[0])
        let rowB = row(in: app, id: ids[1])
        let rowC = row(in: app, id: ids[2])

        clickRow(rowA)
        cmdClickRow(rowB, in: app)
        waitForSelected(in: app, rowId: ids[1])

        rowC.rightClick()

        XCTAssertTrue(
            app.menuItems["Close Tab"].waitForExistence(timeout: 3),
            "Expected singular 'Close Tab' on right-click outside selection"
        )
        XCTAssertTrue(
            app.menuItems["Rename Tab"].exists,
            "Rename Tab must remain available for single-tab right-clicks"
        )
        XCTAssertFalse(
            app.menuItems["Close 2 Tabs"].exists ||
            app.menuItems["Close 3 Tabs"].exists,
            "Plural close label must not appear for a single-tab right-click"
        )

        // Pick the singular Close action — this fires
        // snapIfRightClickOutside, snapping the selection to rowC and
        // closing it. We can't easily check selection after rowC is
        // gone, so instead verify the snap by asserting only one row
        // closed (rowA and rowB stay) — proving the close acted on
        // [clickedId] alone, not on the prior {rowA, rowB} set.
        app.menuItems["Close Tab"].click()

        let cGone = NSPredicate(format: "exists == false")
        let waitC = XCTNSPredicateExpectation(predicate: cGone, object: rowC)
        XCTAssertEqual(
            XCTWaiter.wait(for: [waitC], timeout: 10), .completed,
            "rowC should close after the singular Close Tab action"
        )
        XCTAssertTrue(rowA.exists, "rowA must survive — close acted on rowC alone")
        XCTAssertTrue(rowB.exists, "rowB must survive — close acted on rowC alone")
    }

    // MARK: - Multi-close cascade
    //
    // The "busy tabs in a multi-close batch surface the unified
    // alert with a `.tabs(...)` scope" contract is pinned at the
    // unit layer by `CloseRequestCoordinatorMultiCloseTests`:
    //   - `test_requestCloseTabs_mixedBatch_killsIdle_andStagesOnlyBusyInPending`
    //   - `test_requestCloseTabs_allBusy_killsNoneAndStagesAll`
    //   - `test_confirmPendingClose_tabsScope_killsEveryBusyTab_andClearsField`
    //   - `test_cancelPendingClose_tabsScope_clearsField_leavesBusyTabsAlive`
    //
    // A UITest version was attempted by typing `sleep 60\n` into
    // each spawned terminal to make the foreground-child busy check
    // trigger, but `app.typeText` + SwiftTerm focus + zsh exec
    // races kept the busy detection from firing reliably. The unit
    // tests cover the partition logic, the alert state machine,
    // and the confirm/cancel pair deterministically.

    /// Selecting two idle tabs and choosing "Close 2 Tabs" closes
    /// both — the rows disappear from the sidebar. Idle terminal tabs
    /// have no foreground child so the busy-aggregate alert never
    /// fires; the close cascade runs synchronously.
    func testCloseMultipleIdleTabs_closesAll() throws {
        let app = launchAppForMultiSelect()
        let ids = addTerminalTabs(in: app, count: 2)

        let rowA = row(in: app, id: ids[0])
        let rowB = row(in: app, id: ids[1])
        clickRow(rowA)
        cmdClickRow(rowB, in: app)
        waitForSelected(in: app, rowId: ids[1])

        rowA.rightClick()
        let closeTwo = app.menuItems["Close 2 Tabs"]
        XCTAssertTrue(closeTwo.waitForExistence(timeout: 3))
        closeTwo.click()

        // Both rows should disappear. Use waitForNonExistence with a
        // generous timeout — the close cascade involves SIGTERM →
        // pty drain → paneExited delegate → finalizeDissolvedTab.
        let goneA = NSPredicate(format: "exists == false")
        let goneB = NSPredicate(format: "exists == false")
        XCTAssertEqual(
            XCTWaiter.wait(for: [
                XCTNSPredicateExpectation(predicate: goneA, object: rowA),
                XCTNSPredicateExpectation(predicate: goneB, object: rowB),
            ], timeout: 10),
            .completed,
            "Both selected tab rows should disappear after Close 2 Tabs"
        )
    }

    // MARK: - Cross-group shift-range (S2)

    /// Shift-click that spans the Terminals group and a Claude tab
    /// in a separate project group selects the entire range across
    /// both groups. The model unit test
    /// `test_extend_acrossProjectGroups_selectsContiguousRun` proves
    /// the model handles cross-group order; this UITest pins the
    /// view-layer wiring (that `tabs.navigableSidebarTabIds` actually
    /// orders Terminals before projects in the rendered sidebar).
    func testShiftClick_acrossTerminalsAndProject_selectsEntireRun() throws {
        let socketPath = makeUniqueSocketPath()
        try? FileManager.default.removeItem(atPath: socketPath)
        let app = launchAppForMultiSelect(socketPath: socketPath)

        // Spawn one extra terminal in the Terminals group.
        let extraTerminalIds = addTerminalTabs(in: app, count: 1)
        let extraTerminal = extraTerminalIds[0]

        // Spawn a Claude tab in a fresh project via the socket.
        let projectCwd = makeFakeHome().appendingPathComponent("project").path
        try? FileManager.default.createDirectory(
            atPath: projectCwd, withIntermediateDirectories: true
        )
        let claudeTab = try createClaudeTabViaSocket(
            in: app, socketPath: socketPath, cwd: projectCwd
        )

        // Shift-click from the extra terminal across to the Claude
        // tab — visible order is Main terminal, extra terminal,
        // Claude tab; range from extra → Claude must include both.
        clickRow(row(in: app, id: extraTerminal))
        waitForSelected(in: app, rowId: extraTerminal)

        shiftClickRow(row(in: app, id: claudeTab), in: app)

        waitForSelected(in: app, rowId: extraTerminal)
        waitForSelected(in: app, rowId: claudeTab)
        // The Main terminal sits BEFORE the extra terminal in the
        // visible order, so it should NOT be in the range — proves
        // we extended from the anchor (extra) to the target (Claude),
        // not from the start of the order.
        XCTAssertFalse(
            isSelected(in: app, rowId: "sidebar.terminals"),
            "Main terminal lies outside the [extraTerminal, claudeTab] range"
        )
    }

    // MARK: - Selection clearing (S3)

    /// Clicking the empty area below the last sidebar row collapses
    /// any active multi-selection back to just the active tab. The
    /// `tabList` ScrollView's inner stack carries a
    /// `.contentShape(Rectangle()).onTapGesture { collapse }` that
    /// only fires for clicks no row absorbed.
    func testEmptySidebarClick_collapsesMultiSelection() throws {
        let app = launchAppForMultiSelect()
        let ids = addTerminalTabs(in: app, count: 2)

        let rowA = row(in: app, id: ids[0])
        let rowB = row(in: app, id: ids[1])
        clickRow(rowA)
        cmdClickRow(rowB, in: app)
        waitForSelected(in: app, rowId: ids[0])
        waitForSelected(in: app, rowId: ids[1])

        // Click ~3 row-heights below the last spawned row. That lands
        // in the ScrollView's empty padding well below any row's
        // hit-test area. Same trick as
        // FileBrowserSelectionUITests.testClickEmptyFileBrowserSpace.
        rowB.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 4.0))
            .click()

        // After the collapse, exactly one row stays selected — the
        // active one (rowB, since it was the most-recently-clicked).
        waitForUnselected(in: app, rowId: ids[0])
        XCTAssertTrue(
            isSelected(in: app, rowId: ids[1]),
            "Active row must remain selected after empty-area collapse"
        )
    }

    // MARK: - Startup seeding regression

    /// Regression: at app launch, the active sidebar tab must already
    /// be in the multi-selection set, so the FIRST user interaction
    /// can be a Shift-click without it degenerating to a plain
    /// replace. The bug: `tabs.activeTabId` is restored / seeded
    /// before any user click, but `selectedTabIds` (session-only,
    /// not persisted) starts empty — without the
    /// `.onChange(of: activeTabId, initial: true)` sync in
    /// `SidebarView`, the active tab is visibly highlighted via
    /// `isActive` but isn't actually in the selection set, so
    /// `extend` falls back to its empty-anchor branch and selects
    /// only the clicked row.
    func testStartup_activeTabSeededIntoSelection_shiftClickWorksImmediately() throws {
        let app = launchAppForMultiSelect()
        // Spawn one extra terminal tab. The Main terminal stays the
        // active tab (the spawn doesn't necessarily reactivate it,
        // but for this test the only thing that matters is that SOME
        // tab is active and seeded into selection on launch).
        let ids = addTerminalTabs(in: app, count: 1)

        // Pick whichever tab is currently active by reading the
        // sidebar.tab.<id>.selected marker. Exactly one must exist
        // at this point (the active tab, seeded by the
        // `.onChange(of: activeTabId, initial: true)` observer in
        // SidebarView).
        let initiallySelected = currentlySelectedRowIds(in: app)
        XCTAssertEqual(
            initiallySelected.count, 1,
            "Exactly one tab must be selected at launch (the active "
            + "one), got: \(initiallySelected)"
        )

        // Shift-click the spawned tab without any plain click first.
        // With the seeding, this extends from active → spawned and
        // selects both. Without the seeding, it degenerates to a
        // plain replace and only the spawned tab ends up selected.
        shiftClickRow(row(in: app, id: ids[0]), in: app)

        waitForSelected(in: app, rowId: ids[0])
        // The previously-active row must STILL be selected — proving
        // the shift-click extended a range rather than replacing.
        for id in initiallySelected where id != ids[0] {
            XCTAssertTrue(
                isSelected(in: app, rowId: id),
                "Initially-active row \(id) should still be selected "
                + "after Shift-click extends the range"
            )
        }
    }

    // MARK: - Esc collapse

    /// Esc collapses an active multi-selection back down to just the
    /// active tab. The Esc monitor in `SidebarView` gates strictly on
    /// `count > 1`, so verifying the collapse from a 2-tab selection
    /// also implicitly verifies that gate (we wouldn't see the
    /// transition otherwise).
    func testEsc_collapsesMultiSelectionToActive() throws {
        let app = launchAppForMultiSelect()
        let ids = addTerminalTabs(in: app, count: 2)

        clickRow(row(in: app, id: ids[0]))
        cmdClickRow(row(in: app, id: ids[1]), in: app)
        // After cmd-click, B is the active (most-recently-clicked) tab.
        waitForSelected(in: app, rowId: ids[1])
        XCTAssertTrue(isSelected(in: app, rowId: ids[0]))

        // Esc collapses to the active row.
        app.typeKey(.escape, modifierFlags: [])
        waitForUnselected(in: app, rowId: ids[0])
        XCTAssertTrue(
            isSelected(in: app, rowId: ids[1]),
            "Active row must remain selected after Esc collapse"
        )
    }

    // MARK: - External dissolve (S4)
    //
    // The end-to-end "tab dissolves outside our menu and the
    // multi-selection set is pruned cleanly" contract is pinned at
    // the unit layer by `AppStateTabSelectionTests`:
    //   - `test_closingTab_prunesFromMultiSelection`
    //   - `test_closingTab_clearsAnchorWhenAnchorWasTheClosedTab`
    //   - `test_closingTab_keepsAnchorWhenAnchorSurvives`
    //
    // Those drive the same `finalizeDissolvedTab` cascade an external
    // pane-exit would (`paneExited` → `onTabBecameEmpty` →
    // `finalizeDissolvedTab` → `tabSelection.prune`). A UITest
    // version was attempted with `app.typeText("exit\n")` but the
    // first-responder timing across cmd-click + typeText made it
    // brittle — the unit tests are the canonical pin.

    // MARK: - Programmatic active-tab change (S5)

    /// External nav (keyboard ⌘⌥↓ → `selectNextSidebarTab`, socket
    /// newtab, `+` button, etc.) must collapse any multi-selection
    /// to just the new active tab. The startup regression test
    /// covers the `initial: true` fire of
    /// `.onChange(of: activeTabId, initial: true)`; this test
    /// covers the steady-state observer firing on a programmatic
    /// active-tab change.
    func testKeyboardNextTab_collapsesMultiSelectionToNewActive() throws {
        let app = launchAppForMultiSelect()
        let ids = addTerminalTabs(in: app, count: 2)

        // Multi-select the two extra terminals.
        clickRow(row(in: app, id: ids[0]))
        cmdClickRow(row(in: app, id: ids[1]), in: app)
        waitForSelected(in: app, rowId: ids[0])
        waitForSelected(in: app, rowId: ids[1])

        // ⌘⌥↓ moves to the next sidebar tab. The active tab moves
        // OUTSIDE the current selection, which triggers the
        // `syncActiveTabId` "external nav resets multi-selection"
        // collapse. After the collapse, exactly one tab is selected
        // — whichever the keyboard shortcut landed on.
        app.typeKey(.downArrow, modifierFlags: [.command, .option])

        // Wait for the multi-selection to shrink to a single row.
        let oneSelected = NSPredicate(block: { _, _ in
            self.currentlySelectedRowIds(in: app).count == 1
        })
        XCTAssertEqual(
            XCTWaiter.wait(
                for: [XCTNSPredicateExpectation(predicate: oneSelected, object: nil)],
                timeout: 5
            ),
            .completed,
            "Keyboard tab-switch must collapse the multi-selection to "
            + "one row (the new active tab)."
        )
    }

    // MARK: - Persistence (S6)

    /// Multi-selection is session-only and must NOT survive an
    /// app restart. Pinning this catches a future regression where
    /// someone wires `tabSelection` mutations through
    /// `onTreeMutation` → `WindowSession.scheduleSessionSave` and
    /// the selection set ends up in `sessions.json`.
    func testPersistence_multiSelectionDoesNotSurviveRestart() throws {
        let home = makeFakeHome()
        let app = launchAppForMultiSelect(reuseHome: home)
        let ids = addTerminalTabs(in: app, count: 2)

        clickRow(row(in: app, id: ids[0]))
        cmdClickRow(row(in: app, id: ids[1]), in: app)
        waitForSelected(in: app, rowId: ids[0])
        waitForSelected(in: app, rowId: ids[1])

        // Clean quit so persisted state lands on disk.
        app.terminate()

        // Re-launch with the SAME fake home so session restore
        // recovers the spawned terminal tabs.
        let app2 = launchAppForMultiSelect(reuseHome: home)

        // The spawned rows should restore. Wait for at least one
        // spawned id to come back so we know restore completed.
        XCTAssertTrue(
            row(in: app2, id: ids[1]).waitForExistence(timeout: 10),
            "Restored sidebar must include the spawned tabs"
        )

        // After restore, exactly one row is selected — the active
        // one. The other formerly-selected row must NOT be in the
        // multi-selection set.
        let selected = currentlySelectedRowIds(in: app2)
        XCTAssertEqual(
            selected.count, 1,
            "Multi-selection must NOT survive restart; exactly one "
            + "row (the active one) should be selected, got: \(selected)"
        )
    }

    // MARK: - Marker namespace invariant (N4)

    /// The selection marker namespace (`sidebar.selectedTab.*`) must
    /// stay disjoint from the row namespace (`sidebar.tab.*`) so
    /// existing prefix-based row queries in `NiceUITests` don't
    /// hit-test our hidden zero-size markers. Catches a regression
    /// where someone moves the marker back under the row prefix.
    func testMarkerNamespace_isDisjointFromRowNamespace() throws {
        let app = launchAppForMultiSelect()
        let ids = addTerminalTabs(in: app, count: 2)

        clickRow(row(in: app, id: ids[0]))
        cmdClickRow(row(in: app, id: ids[1]), in: app)
        waitForSelected(in: app, rowId: ids[0])
        waitForSelected(in: app, rowId: ids[1])

        // Any element whose identifier begins with `sidebar.tab.`
        // must NOT also be a selection marker. The two namespaces
        // share no string-prefix overlap; either can grow children
        // without colliding with the other.
        let rowQuery = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "sidebar.tab.")
        )
        for i in 0..<rowQuery.count {
            let id = rowQuery.element(boundBy: i).identifier
            XCTAssertFalse(
                id.hasPrefix("sidebar.selectedTab."),
                "Row-prefix query picked up a selection marker: \(id)"
            )
        }
        // And vice versa — no row identifier may live in the marker
        // namespace.
        let markerQuery = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "sidebar.selectedTab.")
        )
        XCTAssertGreaterThanOrEqual(
            markerQuery.count, 2,
            "At least the two cmd-clicked rows must surface markers"
        )
        for i in 0..<markerQuery.count {
            let id = markerQuery.element(boundBy: i).identifier
            XCTAssertFalse(
                id.hasPrefix("sidebar.tab."),
                "Marker-prefix query picked up a row: \(id)"
            )
        }
    }

    // MARK: - Plumbing

    /// Launch the app pointing at a sandboxed HOME so it can't touch
    /// the user's real `sessions.json` or dotfiles. Mirrors the
    /// pattern in `FileBrowserSelectionUITests.launchWithSeed`.
    /// `socketPath` is set when the test needs to drive `claude
    /// newtab` over the control socket; `reuseHome` lets the
    /// persistence-restart test launch into the same on-disk state
    /// twice.
    private func launchAppForMultiSelect(
        socketPath: String? = nil,
        reuseHome: URL? = nil
    ) -> XCUIApplication {
        let home = reuseHome ?? makeFakeHome()
        let app = XCUIApplication()
        app.launchArguments += ["-ApplePersistenceIgnoreState", "YES"]
        app.launchEnvironment["HOME"] = home.path
        app.launchEnvironment["NICE_APPLICATION_SUPPORT_ROOT"] =
            home.appendingPathComponent("Library/Application Support").path
        app.launchEnvironment["NICE_MAIN_CWD"] = home.path
        if let socketPath {
            app.launchEnvironment["NICE_SOCKET_PATH"] = socketPath
            app.launchEnvironment["NICE_CLAUDE_OVERRIDE"] = "/bin/cat"
        }
        let hostEnv = ProcessInfo.processInfo.environment
        if let user = hostEnv["USER"] { app.launchEnvironment["USER"] = user }
        if let logname = hostEnv["LOGNAME"] {
            app.launchEnvironment["LOGNAME"] = logname
        }
        app.launch()
        track(app)

        XCTAssertTrue(
            app.descendants(matching: .any)["sidebar.terminals"]
                .waitForExistence(timeout: 10),
            "App must reach the steady-state sidebar before tests run"
        )
        return app
    }

    private func makeFakeHome() -> URL {
        if let url = fakeHomeURL { return url }
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-tab-multiselect-\(UUID().uuidString)",
                isDirectory: true
            )
        try? FileManager.default.createDirectory(
            at: url, withIntermediateDirectories: true
        )
        fakeHomeURL = url
        return url
    }

    /// Per-test socket path — the socket file lives in the OS temp
    /// dir and is removed both by the test (via the `try?
    /// FileManager.default.removeItem` at the call site) and by the
    /// app's own bind (which unlinks before binding).
    ///
    /// Unix-domain socket paths cap at 104 chars on macOS; the temp
    /// dir alone is ~50 chars, so we keep the per-test suffix tiny
    /// (a fixed name suffices since UITests are serialized via the
    /// worktree lock and the OS-level `unlink` clears any leftover
    /// from a previous run).
    private func makeUniqueSocketPath() -> String {
        let dir = FileManager.default.temporaryDirectory.path
        return (dir as NSString)
            .appendingPathComponent("nice-ms.sock")
    }

    /// Click the Terminals group's `+` button `count` times to spawn
    /// `count` extra terminal tabs in the sidebar (on top of the
    /// auto-seeded Main tab). Returns their `sidebar.tab.<id>` row
    /// identifiers in creation order.
    private func addTerminalTabs(in app: XCUIApplication, count: Int) -> [String] {
        let addButton = app.descendants(matching: .any)["sidebar.group.terminals.add"]
        XCTAssertTrue(
            addButton.waitForExistence(timeout: 5),
            "Terminals group `+` button must exist before we can spawn tabs"
        )

        var newRowIds: [String] = []
        var seen = Set(currentTabRowIds(in: app))

        for i in 0..<count {
            addButton.click()
            let appeared = XCTNSPredicateExpectation(
                predicate: NSPredicate(block: { _, _ in
                    !Set(self.currentTabRowIds(in: app))
                        .subtracting(seen).isEmpty
                }),
                object: nil
            )
            XCTAssertEqual(
                XCTWaiter.wait(for: [appeared], timeout: 5), .completed,
                "Expected a new sidebar.tab.* row to appear after `+` click #\(i + 1)"
            )
            let now = Set(currentTabRowIds(in: app))
            let added = now.subtracting(seen)
            XCTAssertEqual(
                added.count, 1,
                "`+` click should produce exactly one new tab row"
            )
            if let id = added.first {
                newRowIds.append(id)
                seen.insert(id)
            }
        }
        return newRowIds
    }

    /// Drive `createTabFromMainTerminal` over the control socket
    /// (the same path as `claude` invocations from the user's
    /// shell). Returns the new tab row's accessibility identifier.
    /// Mirrors `NiceUITests.createTabViaSocket`'s shape.
    @discardableResult
    private func createClaudeTabViaSocket(
        in app: XCUIApplication,
        socketPath: String,
        cwd: String
    ) throws -> String {
        let json = #"{"action":"claude","cwd":"\#(cwd)","args":[],"tabId":"","paneId":""}"#
        try sendSocketLine(json, to: socketPath)

        // Snapshot existing tab rows; the new socket-spawned row is
        // whichever id appears that wasn't there before.
        let before = Set(currentTabRowIds(in: app))
        let appeared = XCTNSPredicateExpectation(
            predicate: NSPredicate(block: { _, _ in
                !Set(self.currentTabRowIds(in: app)).subtracting(before).isEmpty
            }),
            object: nil
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [appeared], timeout: 5), .completed,
            "Expected a new sidebar tab after socket newtab"
        )
        let after = Set(currentTabRowIds(in: app))
        let added = after.subtracting(before)
        guard let id = added.first else {
            XCTFail("Socket newtab produced no detectable row")
            return ""
        }
        return id
    }

    /// Write one JSON line to the `nice` control socket and drain the
    /// reply (so the server's write succeeds before we close the
    /// fd). Lifted from `NiceUITests.sendSocketLine` so this file
    /// stays self-contained — same protocol shape, same drain.
    private func sendSocketLine(_ json: String, to socketPath: String) throws {
        let fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else {
            throw NSError(domain: "MultiSelectUITests", code: Int(errno))
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
            throw NSError(domain: "MultiSelectUITests", code: Int(errno))
        }

        let payload = Array((json + "\n").utf8)
        let written = Darwin.write(fd, payload, payload.count)
        guard written == payload.count else {
            throw NSError(domain: "MultiSelectUITests", code: Int(errno))
        }

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

    /// Snapshot of every `sidebar.tab.*` row identifier currently in
    /// the sidebar (excluding the icon / title / titleField / button
    /// children that share the prefix). The selection marker lives
    /// in its own `sidebar.selectedTab.*` namespace so it doesn't
    /// need exclusion here.
    private func currentTabRowIds(in app: XCUIApplication) -> [String] {
        let query = app.descendants(matching: .any).matching(
            NSPredicate(format: """
                identifier BEGINSWITH %@ \
                AND NOT (identifier CONTAINS %@) \
                AND NOT (identifier CONTAINS %@) \
                AND NOT (identifier CONTAINS %@) \
                AND NOT (identifier CONTAINS %@) \
                AND NOT (identifier CONTAINS %@) \
                AND NOT (identifier CONTAINS %@)
                """,
                "sidebar.tab.",
                ".claudeIcon",
                ".terminalIcon",
                ".title",
                ".titleField",
                ".renameTab",
                ".closeTab"
            )
        )
        var ids: [String] = []
        for i in 0..<query.count {
            ids.append(query.element(boundBy: i).identifier)
        }
        return ids
    }

    private func row(in app: XCUIApplication, id: String) -> XCUIElement {
        let row = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier == %@", id)
        ).element(boundBy: 0)
        XCTAssertTrue(
            row.waitForExistence(timeout: 5),
            "Expected sidebar tab row \(id) to exist"
        )
        return row
    }

    /// Click the row's leading edge (avoid the title centroid, which
    /// has its own tap gesture for inline rename — same trick as
    /// `clickSidebarRow` in `NiceUITests`).
    private func clickRow(_ row: XCUIElement) {
        row.coordinate(withNormalizedOffset: CGVector(dx: 0.05, dy: 0.5)).click()
    }

    /// Cmd-click the row's leading edge. `XCUIElement.perform(
    /// withKeyModifiers:_:)` is a CLASS method on `XCUIElement` and
    /// holds the modifier down across the block — the documented way
    /// to do modifier-key clicks on macOS XCUITest.
    private func cmdClickRow(_ row: XCUIElement, in app: XCUIApplication) {
        let coord = row.coordinate(withNormalizedOffset: CGVector(dx: 0.05, dy: 0.5))
        XCUIElement.perform(withKeyModifiers: .command) {
            coord.click()
        }
    }

    /// Shift-click the row's leading edge.
    private func shiftClickRow(_ row: XCUIElement, in app: XCUIApplication) {
        let coord = row.coordinate(withNormalizedOffset: CGVector(dx: 0.05, dy: 0.5))
        XCUIElement.perform(withKeyModifiers: .shift) {
            coord.click()
        }
    }

    /// Ids of the rows whose selection marker is currently in the
    /// accessibility tree. The marker identifier is
    /// `sidebar.selectedTab.<tabId>` — strip the prefix to recover
    /// the tab id, then translate to the row's full identifier
    /// (`sidebar.terminals` for Main, `sidebar.tab.<id>` otherwise).
    private func currentlySelectedRowIds(in app: XCUIApplication) -> [String] {
        let prefix = "sidebar.selectedTab."
        let query = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", prefix)
        )
        var ids: [String] = []
        for i in 0..<query.count {
            let markerId = query.element(boundBy: i).identifier
            let tabId = String(markerId.dropFirst(prefix.count))
            if tabId == "terminals-main" {
                ids.append("sidebar.terminals")
            } else {
                ids.append("sidebar.tab.\(tabId)")
            }
        }
        return ids
    }

    /// `sidebar.selectedTab.<tabId>` exists iff the row is currently
    /// in the multi-selection set. `rowId` is the row element's full
    /// identifier (`sidebar.tab.<tabId>` or the legacy
    /// `sidebar.terminals` for the Main terminal tab).
    private func selectedMarkerId(forRowId rowId: String) -> String {
        if rowId == "sidebar.terminals" {
            return "sidebar.selectedTab.terminals-main"
        }
        // Strip "sidebar.tab." prefix to recover the bare tab id.
        let tabId = String(rowId.dropFirst("sidebar.tab.".count))
        return "sidebar.selectedTab.\(tabId)"
    }

    /// True when the row is currently in the multi-selection set, by
    /// the marker's existence in the accessibility tree.
    private func isSelected(in app: XCUIApplication, rowId: String) -> Bool {
        app.descendants(matching: .any)[selectedMarkerId(forRowId: rowId)].exists
    }

    /// Block until the row's selection marker exists. Selection
    /// updates flow through `@Observable` → SwiftUI render → AppKit
    /// accessibility refresh, so the wait absorbs that latency
    /// without us having to know the exact debounce.
    private func waitForSelected(
        in app: XCUIApplication,
        rowId: String,
        timeout: TimeInterval = 3,
        file: StaticString = #file,
        line: UInt = #line
    ) {
        let marker = app.descendants(matching: .any)[selectedMarkerId(forRowId: rowId)]
        XCTAssertTrue(
            marker.waitForExistence(timeout: timeout),
            "Expected row \(rowId) to become selected within \(timeout)s",
            file: file, line: line
        )
    }

    /// Block until the row's selection marker no longer exists.
    private func waitForUnselected(
        in app: XCUIApplication,
        rowId: String,
        timeout: TimeInterval = 3,
        file: StaticString = #file,
        line: UInt = #line
    ) {
        let marker = app.descendants(matching: .any)[selectedMarkerId(forRowId: rowId)]
        let predicate = NSPredicate(format: "exists == false")
        let exp = XCTNSPredicateExpectation(predicate: predicate, object: marker)
        XCTAssertEqual(
            XCTWaiter.wait(for: [exp], timeout: timeout), .completed,
            "Expected row \(rowId) to become unselected within \(timeout)s",
            file: file, line: line
        )
    }
}
