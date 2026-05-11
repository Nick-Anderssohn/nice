//
//  FileBrowserRenameUITests.swift
//  NiceUITests
//
//  End-to-end coverage for the file-browser inline rename feature.
//  Mirrors the launch / fake-home pattern of
//  `FileBrowserContextMenuUITests` — seeds a project under a
//  sandboxed `HOME`, drives the right-click "Rename" menu item, and
//  asserts the on-disk outcome via direct `FileManager` checks.
//
//  Slow-second-click and the cross-window CWD-impact alert are
//  covered by the unit suites — both are awkward in XCUITest (the
//  former needs a precisely-timed second click; the latter pops a
//  modal `NSAlert` that XCUITest can't always reach into reliably).
//

import XCTest

final class FileBrowserRenameUITests: NiceUITestCase {

    private var fakeHomeURL: URL?
    private var projectRoot: URL?

    override func tearDownWithError() throws {
        if let url = fakeHomeURL {
            try? FileManager.default.removeItem(at: url)
        }
        fakeHomeURL = nil
        projectRoot = nil
        try super.tearDownWithError()
    }

    // MARK: - Tests

    /// Right-click → Rename → type new name → Return. The file on
    /// disk must be at the new path; the old path must be gone.
    func testRightClickRename_typeAndReturn_renamesFile() throws {
        let (app, file, project) = launchWithSeed()
        showFileBrowser(in: app)

        let row = waitForRow(in: app, atPath: file.path)
        row.rightClick()

        let renameItem = app.menuItems["Rename"]
        XCTAssertTrue(renameItem.waitForExistence(timeout: 5))
        renameItem.click()

        let field = waitForRenameField(in: app, atPath: file.path)
        // The production field pre-selects the basename portion only
        // (Finder-style) on its first `becomeFirstResponder`, so a
        // user can type the new basename and keep `.txt`. We can't
        // rely on that here: `waitForRenameField` clicks the field
        // to force focus (the production `DispatchQueue.main.async`
        // focus hop loses to XCUITest's tight event sequence on the
        // slow CI VM), and the click moves the cursor — destroying
        // the pre-selection. Compensate by selecting all and typing
        // the full new name; the on-disk outcome is what we're
        // actually asserting. Pre-selection lives on as an
        // implementation-level invariant of
        // `FileBrowserRenameField.becomeFirstResponder`.
        app.typeKey("a", modifierFlags: .command)
        field.typeText("renamed.txt")
        app.typeKey(.return, modifierFlags: [])

        let renamed = project.appendingPathComponent("renamed.txt")
        let deadline = Date().addingTimeInterval(5)
        while Date() < deadline {
            if FileManager.default.fileExists(atPath: renamed.path),
               !FileManager.default.fileExists(atPath: file.path) {
                return
            }
            usleep(100_000)
        }
        XCTFail("expected rename to land on disk: old=\(file.path), new=\(renamed.path)")
    }

    /// Pressing Escape while editing must revert the row to its
    /// original name without renaming on disk.
    func testRightClickRename_escapeReverts() throws {
        let (app, file, _) = launchWithSeed()
        showFileBrowser(in: app)

        let row = waitForRow(in: app, atPath: file.path)
        row.rightClick()
        let renameItem = app.menuItems["Rename"]
        XCTAssertTrue(renameItem.waitForExistence(timeout: 5))
        renameItem.click()

        let field = waitForRenameField(in: app, atPath: file.path)
        field.typeText("garbage")
        app.typeKey(.escape, modifierFlags: [])

        // Original file still there, no renamed sibling created.
        XCTAssertTrue(FileManager.default.fileExists(atPath: file.path))
        // The row at the original path is still in the tree.
        XCTAssertTrue(waitForRow(in: app, atPath: file.path).exists)
    }

    /// A draft containing `/` is illegal in a single path component;
    /// the field must stay open (i.e. no commit) and the file must
    /// still exist at the original path.
    func testRightClickRename_typeSlash_staysInEditMode() throws {
        let (app, file, _) = launchWithSeed()
        showFileBrowser(in: app)

        let row = waitForRow(in: app, atPath: file.path)
        row.rightClick()
        let renameItem = app.menuItems["Rename"]
        XCTAssertTrue(renameItem.waitForExistence(timeout: 5))
        renameItem.click()

        let field = waitForRenameField(in: app, atPath: file.path)
        field.typeText("foo/bar.txt")
        app.typeKey(.return, modifierFlags: [])

        // Field must still be present (Return didn't commit due to
        // the slash). The original file must still exist.
        XCTAssertTrue(field.exists)
        XCTAssertTrue(FileManager.default.fileExists(atPath: file.path))
    }

    // MARK: - Helpers

    private func waitForRenameField(in app: XCUIApplication, atPath path: String) -> XCUIElement {
        let id = "fileBrowser.row.\(path).renameField"
        let element = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier == %@", id)
        ).firstMatch
        XCTAssertTrue(
            element.waitForExistence(timeout: 5),
            "Expected rename field \(id) to exist within 5s."
        )
        // The rename field's first-responder grant comes from a
        // `DispatchQueue.main.async` hop inside `makeNSView`. In
        // production users take hundreds of ms between clicking
        // "Rename" and starting to type, so by then the hop has
        // fired and any menu-dismissal focus dance has settled —
        // the field reliably has focus.
        //
        // XCUITest hits `typeText` within a few ms. On the slow
        // GitHub-Actions macOS VM, AppKit's menu-dismissal focus
        // restoration can land *after* our async, leaving focus
        // permanently on the row's parent Group; sleeping more
        // doesn't help (focus isn't delayed, it's elsewhere).
        // Earlier attempts to poll for focus via
        // `value(forKey: "hasKeyboardFocus")` also failed because
        // the live `XCUIElement` proxy returns cached snapshot
        // data and reports stale "no focus" indefinitely.
        //
        // Click the field directly: the mouseDown puts the field
        // into first-responder state synchronously and keystrokes
        // land correctly. Side effect: the click moves the cursor,
        // so any basename pre-selection installed by the
        // production `becomeFirstResponder` is gone — callers that
        // care must drive their own selection (e.g. Cmd-A) before
        // typing.
        element.click()
        return element
    }

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
        projectRoot = project

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
                "nice-fb-rename-\(UUID().uuidString)",
                isDirectory: true
            )
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
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
}
