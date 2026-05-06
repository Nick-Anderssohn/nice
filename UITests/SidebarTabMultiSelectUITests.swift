//
//  SidebarTabMultiSelectUITests.swift
//  NiceUITests
//
//  End-to-end coverage for the multi-tab sidebar selection wiring:
//  the modifier-aware tap handler in `TabRow`, the dynamic context
//  menu (rename hidden / "Close N Tabs" label when count > 1), the
//  Esc collapse, and the actual "close many" cascade. The selection
//  model itself is unit-tested in `SidebarTabSelectionTests`; these
//  tests verify the SwiftUI / AppKit wiring around it.
//
//  How we read selection state from XCUITest: each `TabRow` adds a
//  hidden zero-size sibling element with identifier
//  `sidebar.tab.<id>.selected` that EXISTS iff the row is in the
//  multi-selection set, and is absent otherwise. We assert on the
//  marker's `.exists` (which is reliable across the AppKit
//  accessibility bridge) instead of reading a `.value` off the row
//  itself — `.accessibilityElement(children: .contain)` on the row
//  doesn't surface a value to XCUIElement, and `.accessibilityValue`
//  on a zero-size hidden marker doesn't either.
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

    // MARK: - Tests

    /// Plain click on one tab, then Cmd-click another. Both rows
    /// should report `selected`. Baseline proving Cmd-click extends
    /// the selection set without dropping the previously-clicked tab.
    func testCmdClick_addsTabToSelection() throws {
        let app = launchAppForMultiSelect()
        let ids = addTerminalTabs(in: app, count: 2)

        let rowA = row(in: app, id: ids[0])
        let rowB = row(in: app, id: ids[1])

        clickRow(rowA)
        waitForSelected(in: app, rowId: ids[0])

        cmdClickRow(rowB, in: app)
        waitForSelected(in: app, rowId: ids[1])
        // First row must still be selected — Cmd-click extends, it
        // doesn't replace.
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

        let rowA = row(in: app, id: ids[0])
        let rowB = row(in: app, id: ids[1])
        let rowC = row(in: app, id: ids[2])

        clickRow(rowA)
        cmdClickRow(rowB, in: app)
        cmdClickRow(rowC, in: app)
        waitForSelected(in: app, rowId: ids[1])

        // Plain click on rowB collapses everything to it.
        clickRow(rowB)
        waitForUnselected(in: app, rowId: ids[0])
        waitForUnselected(in: app, rowId: ids[2])
        XCTAssertTrue(
            isSelected(in: app, rowId: ids[1]),
            "Plain-click target must remain selected after the collapse"
        )
        // Silence unused-warning
        _ = (rowA, rowC)
    }

    /// Right-clicking a tab that's part of the multi-selection shows
    /// `Close N Tabs` (not `Close Tab`) and hides the `Rename Tab`
    /// menu item entirely. The dynamic label and conditional rename
    /// are the user-visible payoff of this feature.
    func testRightClickInsideSelection_showsCloseNTabs_andHidesRename() throws {
        let app = launchAppForMultiSelect()
        let ids = addTerminalTabs(in: app, count: 2)

        let rowA = row(in: app, id: ids[0])
        let rowB = row(in: app, id: ids[1])
        clickRow(rowA)
        cmdClickRow(rowB, in: app)
        waitForSelected(in: app, rowId: ids[1])

        // Right-click on a row that's already in the selection — the
        // menu should act on the whole set.
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

        // Right-click rowC — outside the selection.
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
        let waitA = XCTNSPredicateExpectation(predicate: goneA, object: rowA)
        let waitB = XCTNSPredicateExpectation(predicate: goneB, object: rowB)
        XCTAssertEqual(
            XCTWaiter.wait(for: [waitA, waitB], timeout: 10),
            .completed,
            "Both selected tab rows should disappear after Close 2 Tabs"
        )
    }

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
        let spawned = row(in: app, id: ids[0])
        shiftClickRow(spawned, in: app)

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

    /// Esc collapses an active multi-selection back down to just the
    /// active tab. The Esc monitor in `SidebarView` gates strictly on
    /// `count > 1`, so verifying the collapse from a 2-tab selection
    /// also implicitly verifies that gate (we wouldn't see the
    /// transition otherwise).
    func testEsc_collapsesMultiSelectionToActive() throws {
        let app = launchAppForMultiSelect()
        let ids = addTerminalTabs(in: app, count: 2)

        let rowA = row(in: app, id: ids[0])
        let rowB = row(in: app, id: ids[1])

        clickRow(rowA)
        cmdClickRow(rowB, in: app)
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
        _ = (rowA, rowB)
    }

    // MARK: - Plumbing

    /// Launch the app pointing at a sandboxed HOME so it can't touch
    /// the user's real `sessions.json` or dotfiles. Mirrors the
    /// pattern in `FileBrowserSelectionUITests.launchWithSeed`.
    private func launchAppForMultiSelect() -> XCUIApplication {
        let home = makeFakeHome()
        let app = XCUIApplication()
        app.launchArguments += ["-ApplePersistenceIgnoreState", "YES"]
        app.launchEnvironment["HOME"] = home.path
        app.launchEnvironment["NICE_APPLICATION_SUPPORT_ROOT"] =
            home.appendingPathComponent("Library/Application Support").path
        app.launchEnvironment["NICE_MAIN_CWD"] = home.path
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

    /// Click the Terminals group's `+` button `count` times to spawn
    /// `count` extra terminal tabs in the sidebar (on top of the
    /// auto-seeded Main tab). Returns their `sidebar.tab.<id>` row
    /// identifiers in creation order — useful for tests that need
    /// stable handles to specific rows.
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
            // Wait for a brand-new sidebar.tab.* row to appear.
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
            // One click should add exactly one new row.
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

    /// Snapshot of every `sidebar.tab.*` row identifier currently in
    /// the sidebar (excluding the icon / title / titleField / button
    /// / selection-marker children that share the prefix). Used by
    /// `addTerminalTabs` to detect newly-spawned rows.
    private func currentTabRowIds(in app: XCUIApplication) -> [String] {
        let query = app.descendants(matching: .any).matching(
            NSPredicate(format: """
                identifier BEGINSWITH %@ \
                AND NOT (identifier CONTAINS %@) \
                AND NOT (identifier CONTAINS %@) \
                AND NOT (identifier CONTAINS %@) \
                AND NOT (identifier CONTAINS %@) \
                AND NOT (identifier CONTAINS %@) \
                AND NOT (identifier CONTAINS %@) \
                AND NOT (identifier ENDSWITH %@)
                """,
                "sidebar.tab.",
                ".claudeIcon",
                ".terminalIcon",
                ".title",
                ".titleField",
                ".renameTab",
                ".closeTab",
                ".selected"
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

    /// Shift-click the row's leading edge. Same modifier-hold pattern
    /// as `cmdClickRow`, just `.shift` instead of `.command`.
    private func shiftClickRow(_ row: XCUIElement, in app: XCUIApplication) {
        let coord = row.coordinate(withNormalizedOffset: CGVector(dx: 0.05, dy: 0.5))
        XCUIElement.perform(withKeyModifiers: .shift) {
            coord.click()
        }
    }

    /// Ids of the rows whose `.selected` marker is currently in the
    /// accessibility tree. Used by the launch-state regression test
    /// to confirm the seeding observer ran.
    private func currentlySelectedRowIds(in app: XCUIApplication) -> [String] {
        // The marker identifier is `sidebar.tab.<tabId>.selected` —
        // strip the `.selected` suffix to recover the row id, then
        // canonicalize the Main terminal back to `sidebar.terminals`
        // (the row uses the legacy id while the marker always uses
        // the `sidebar.tab.<tabId>` form).
        let query = app.descendants(matching: .any).matching(
            NSPredicate(format: """
                identifier BEGINSWITH %@ \
                AND identifier ENDSWITH %@
                """,
                "sidebar.tab.",
                ".selected"
            )
        )
        var ids: [String] = []
        for i in 0..<query.count {
            let markerId = query.element(boundBy: i).identifier
            // Trim the `.selected` suffix.
            let rowId = String(markerId.dropLast(".selected".count))
            // Canonicalize Main terminal.
            if rowId == "sidebar.tab.terminals-main" {
                ids.append("sidebar.terminals")
            } else {
                ids.append(rowId)
            }
        }
        return ids
    }

    /// `sidebar.tab.<id>.selected` exists iff the row is currently in
    /// the multi-selection set. `rowId` is the row element's full
    /// identifier (`sidebar.tab.<tabId>` or the legacy
    /// `sidebar.terminals` for the Main terminal tab).
    private func selectedMarkerId(forRowId rowId: String) -> String {
        if rowId == "sidebar.terminals" {
            return "sidebar.tab.terminals-main.selected"
        }
        return "\(rowId).selected"
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
