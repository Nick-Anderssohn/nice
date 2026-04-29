//
//  FileBrowserSelectionUITests.swift
//  NiceUITests
//
//  End-to-end coverage for file-browser row selection visibility and
//  the "click off a row clears selection" behaviour. The selection
//  model itself is unit-tested in `FileBrowserSelectionTests`; these
//  tests verify the SwiftUI wiring around it: the `.isSelected` /
//  `accessibilityValue` exposure, the outer `.onTapGesture` on the
//  file browser container, and the window-level `NSEvent` monitor
//  that clears selection for clicks outside the file browser frame.
//
//  Why `accessibilityValue` and not `XCUIElement.isSelected`:
//  `.isSelected` doesn't reliably surface to `XCUIElement.isSelected`
//  on macOS (same workaround as the Settings theme cell). The bit is
//  mirrored into `accessibilityValue` as `"selected"` / `"unselected"`
//  so we can read it via `.value`.
//

import XCTest

final class FileBrowserSelectionUITests: NiceUITestCase {

    private var fakeHomeURL: URL?

    override func tearDownWithError() throws {
        if let url = fakeHomeURL {
            try? FileManager.default.removeItem(at: url)
        }
        fakeHomeURL = nil
        try super.tearDownWithError()
    }

    // MARK: - Tests

    /// Click a file row — the row's `accessibilityValue` flips to
    /// `"selected"`. Baseline that the trait/value plumbing works.
    func testClickFileRow_marksRowSelected() throws {
        let (app, file, _) = launchWithSeed()

        showFileBrowser(in: app)

        let row = waitForRow(in: app, atPath: file.path)
        XCTAssertEqual(
            row.value as? String, "unselected",
            "Row must start unselected before any click."
        )

        row.click()

        waitForValue(on: row, equals: "selected", timeout: 3)
    }

    /// Select a file, then click empty space inside the file browser
    /// (below the last row). The outer `.onTapGesture` on
    /// `FileBrowserContent`'s container fires for taps that no row /
    /// disclosure / button absorbs and clears the selection.
    func testClickEmptyFileBrowserSpace_clearsSelection() throws {
        let (app, file, _) = launchWithSeed()

        showFileBrowser(in: app)

        let row = waitForRow(in: app, atPath: file.path)
        row.click()
        waitForValue(on: row, equals: "selected", timeout: 3)

        // Click ~3 row-heights below the file row — still inside the
        // file browser's ScrollView, but past the only seeded file, so
        // it lands on truly empty `LazyVStack` space.
        row.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 4.0))
            .click()

        waitForValue(on: row, equals: "unselected", timeout: 3)
    }

    /// Select a file, then click in the main content area (outside
    /// the sidebar entirely). The window-level `NSEvent` monitor
    /// installed by `FileBrowserContent` sees the click is outside
    /// the file browser's frame and clears the selection.
    func testClickOutsideSidebar_clearsSelection() throws {
        let (app, file, _) = launchWithSeed()

        showFileBrowser(in: app)

        let row = waitForRow(in: app, atPath: file.path)
        row.click()
        waitForValue(on: row, equals: "selected", timeout: 3)

        // Click well to the right of the sidebar (the sidebar
        // defaults to ~240 pt; 0.75 across a typical window lands
        // squarely in the terminal pane).
        app.windows.firstMatch
            .coordinate(withNormalizedOffset: CGVector(dx: 0.75, dy: 0.5))
            .click()

        waitForValue(on: row, equals: "unselected", timeout: 3)
    }

    // MARK: - Plumbing
    //
    // Mirrors `FileBrowserContextMenuUITests` — each UITest file
    // duplicates this so it can stand the app up against its own
    // sandboxed HOME.

    /// Launch the app pointing at a fresh test project under a
    /// sandboxed HOME, with `NICE_MAIN_CWD` pinned to that project so
    /// the Main terminal tab — and therefore the file browser — roots
    /// there. Seeds one `file.txt` so there's a row to click.
    private func launchWithSeed() -> (XCUIApplication, URL, URL) {
        let home = makeFakeHome()
        let project = home.appendingPathComponent("project", isDirectory: true)
        try? FileManager.default.createDirectory(
            at: project, withIntermediateDirectories: true
        )
        let file = project.appendingPathComponent("file.txt")
        FileManager.default.createFile(
            atPath: file.path, contents: Data("hello".utf8)
        )

        let app = XCUIApplication()
        app.launchArguments += ["-ApplePersistenceIgnoreState", "YES"]
        app.launchEnvironment["HOME"] = home.path
        app.launchEnvironment["NICE_APPLICATION_SUPPORT_ROOT"] =
            home.appendingPathComponent("Library/Application Support").path
        app.launchEnvironment["NICE_MAIN_CWD"] = project.path
        let hostEnv = ProcessInfo.processInfo.environment
        if let user = hostEnv["USER"] { app.launchEnvironment["USER"] = user }
        if let logname = hostEnv["LOGNAME"] {
            app.launchEnvironment["LOGNAME"] = logname
        }
        app.launch()
        track(app)
        return (app, file, project)
    }

    private func makeFakeHome() -> URL {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-fb-selection-\(UUID().uuidString)",
                isDirectory: true
            )
        try? FileManager.default.createDirectory(
            at: url, withIntermediateDirectories: true
        )
        fakeHomeURL = url
        return url
    }

    private func showFileBrowser(in app: XCUIApplication) {
        XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 10))
        let button = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier == %@", "sidebar.mode.files")
        ).firstMatch
        XCTAssertTrue(
            button.waitForExistence(timeout: 10),
            "sidebar.mode.files button must exist before we can switch sidebars."
        )
        button.click()
    }

    private func waitForRow(in app: XCUIApplication, atPath path: String) -> XCUIElement {
        let id = "fileBrowser.row.\(path)"
        let element = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier == %@", id)
        ).firstMatch
        XCTAssertTrue(
            element.waitForExistence(timeout: 10),
            "Expected file browser row \(id) to exist within 10s."
        )
        return element
    }

    /// Spin until `element.value` matches `expected` or fail. Selection
    /// updates flow through `@Published` → SwiftUI render → AppKit
    /// accessibility refresh, so a small poll absorbs that latency
    /// without needing to know the exact debounce.
    private func waitForValue(
        on element: XCUIElement,
        equals expected: String,
        timeout: TimeInterval,
        file: StaticString = #file,
        line: UInt = #line
    ) {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if (element.value as? String) == expected { return }
            Thread.sleep(forTimeInterval: 0.05)
        }
        XCTFail(
            "Expected element value to become \"\(expected)\" within \(timeout)s; last seen \"\(element.value as? String ?? "nil")\".",
            file: file, line: line
        )
    }
}
