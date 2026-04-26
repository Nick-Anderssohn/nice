//
//  FileBrowserContextMenuUITests.swift
//  NiceUITests
//
//  End-to-end coverage for the file-browser right-click menu.
//  Stands the app up against a sandboxed `HOME`, seeds a test
//  project under that home so the file browser has something to
//  navigate into, then drives the right-click menu via XCUITest.
//
//  These tests verify the SwiftUI binding — the underlying file ops,
//  undo stack, and selection model are covered exhaustively by the
//  unit / integration suites in NiceUnitTests.
//

import XCTest

final class FileBrowserContextMenuUITests: XCTestCase {

    private var fakeHomeURL: URL?
    private var projectRoot: URL?

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    override func tearDownWithError() throws {
        if let url = fakeHomeURL {
            try? FileManager.default.removeItem(at: url)
        }
        fakeHomeURL = nil
        projectRoot = nil
    }

    // MARK: - Tests

    /// Smoke test: open the file browser sidebar, right-click a
    /// seeded file, and assert that the new menu items appear.
    /// Doesn't click any of them — that's covered by the trash/undo
    /// test below.
    func testRightClickFile_showsExpectedMenuItems() throws {
        let (app, file, _) = launchWithSeed()

        showFileBrowser(in: app)

        let row = waitForRow(in: app, atPath: file.path)
        row.rightClick()

        // SwiftUI contextMenu items render as NSMenuItems; we find
        // them by their button title under `app.menuItems`.
        XCTAssertTrue(app.menuItems["Open"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.menuItems["Open With"].exists)
        XCTAssertTrue(app.menuItems["Reveal in Finder"].exists)
        XCTAssertTrue(app.menuItems["Copy"].exists)
        XCTAssertTrue(app.menuItems["Copy Path"].exists)
        XCTAssertTrue(app.menuItems["Cut"].exists)
        // Paste is hidden when there's nothing on the pasteboard;
        // do not assert presence here.
        XCTAssertTrue(app.menuItems["Move to Trash"].exists)

        // Dismiss the menu so it doesn't block subsequent UI work.
        app.typeKey(.escape, modifierFlags: [])
    }

    /// Right-click a file → Move to Trash → file disappears from
    /// the tree → ⌘Z restores it. End-to-end coverage of the
    /// trash + undo flow.
    func testTrashFile_removesFromTree_andCmdZRestoresIt() throws {
        let (app, file, _) = launchWithSeed()

        showFileBrowser(in: app)
        let row = waitForRow(in: app, atPath: file.path)
        row.rightClick()

        let trash = app.menuItems["Move to Trash"]
        XCTAssertTrue(trash.waitForExistence(timeout: 5))
        trash.click()

        // The file row is gone from the tree.
        let rowAfter = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier == %@", "fileBrowser.row.\(file.path)")
        ).firstMatch
        XCTAssertFalse(
            rowAfter.waitForExistence(timeout: 3),
            "Trashed file row should disappear from the file tree."
        )

        // ⌘Z restores it (the file browser's kqueue watcher picks
        // up the move-back and reloads).
        app.typeKey("z", modifierFlags: [.command])

        let rowRestored = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier == %@", "fileBrowser.row.\(file.path)")
        ).firstMatch
        XCTAssertTrue(
            rowRestored.waitForExistence(timeout: 5),
            "⌘Z must restore the trashed file."
        )
    }

    // MARK: - Plumbing

    /// Launch the app pointing at a fresh test project under a
    /// sandboxed HOME, with `NICE_MAIN_CWD` pinned to that project
    /// so the Main terminal tab — and therefore the file browser —
    /// roots there. Seeds one `file.txt` so there's something to
    /// right-click.
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
        return (app, file, project)
    }

    private func makeFakeHome() -> URL {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-fb-context-\(UUID().uuidString)",
                isDirectory: true
            )
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        fakeHomeURL = url
        return url
    }

    /// Switch the sidebar to file-browser mode by clicking the
    /// dedicated header button. Equivalent to ⌘⇧B but doesn't
    /// depend on the keyboard monitor having installed before our
    /// keystroke arrives.
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

    /// Find a file-browser row by absolute path, waiting up to 10s
    /// for it to materialise (the file browser may need to expand
    /// the root directory after switching modes).
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
