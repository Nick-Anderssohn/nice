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

final class FileBrowserContextMenuUITests: NiceUITestCase {

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

    /// Regression: trash a file inside an expanded subdir, collapse
    /// the subdir, ⌘Z to undo, re-expand. The restored file must
    /// appear immediately — without forcing the user to collapse and
    /// re-expand the *parent* of the subdir to invalidate a stale
    /// row-level cache.
    ///
    /// Bug history: each `FileTreeRow` cached its `children` listing
    /// in `@State`. The watcher that kept that cache fresh was
    /// stopped on collapse, so any change while collapsed (an undo
    /// move-back, or an external Finder edit) didn't invalidate the
    /// cache. The expand handler then skipped the reload because
    /// `children != nil`, leaving the user looking at a stale tree.
    /// Fix was to *always* reload on expand.
    func testTrashInsideSubdir_collapseUndoExpand_showsRestoredFile() throws {
        let (app, _, project) = launchWithSeed()
        let subdir = project.appendingPathComponent("sub", isDirectory: true)
        try FileManager.default.createDirectory(
            at: subdir, withIntermediateDirectories: false
        )
        let nested = subdir.appendingPathComponent("inside.txt")
        FileManager.default.createFile(atPath: nested.path, contents: Data())

        showFileBrowser(in: app)

        // Expand the subdir so its child row materialises.
        let subRow = waitForRow(in: app, atPath: subdir.path)
        subRow.click()
        let nestedRow = waitForRow(in: app, atPath: nested.path)

        // Trash the nested file.
        nestedRow.rightClick()
        let trash = app.menuItems["Move to Trash"]
        XCTAssertTrue(trash.waitForExistence(timeout: 5))
        trash.click()

        // Collapse the subdir while the trash is in effect.
        let nestedAfterTrash = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier == %@", "fileBrowser.row.\(nested.path)")
        ).firstMatch
        XCTAssertFalse(
            nestedAfterTrash.waitForExistence(timeout: 3),
            "Trashed nested file row should disappear."
        )
        // Click subdir row again to collapse.
        waitForRow(in: app, atPath: subdir.path).click()

        // Undo the trash. File is back on disk under the (collapsed)
        // subdir; the watcher there is stopped because the subdir is
        // collapsed.
        app.typeKey("z", modifierFlags: [.command])

        // Re-expand the subdir. The restored file must appear *now*,
        // not after a separate parent collapse / re-expand cycle.
        waitForRow(in: app, atPath: subdir.path).click()
        let nestedRestored = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier == %@", "fileBrowser.row.\(nested.path)")
        ).firstMatch
        XCTAssertTrue(
            nestedRestored.waitForExistence(timeout: 5),
            "Re-expanding the subdir after an undo must surface the restored file."
        )
    }

    /// Right-clicking a directory must hide Open and Open With —
    /// those entries don't make sense for folders.
    func testRightClickFolder_omitsOpenAndOpenWith() throws {
        let (app, _, project) = launchWithSeed()
        let sub = makeDir("sub", in: project)

        showFileBrowser(in: app)
        waitForRow(in: app, atPath: sub.path).rightClick()

        XCTAssertTrue(app.menuItems["Reveal in Finder"].waitForExistence(timeout: 5))
        XCTAssertFalse(
            app.menuItems["Open"].exists,
            "Open must be hidden on directory rows."
        )
        XCTAssertFalse(
            app.menuItems["Open With"].exists,
            "Open With must be hidden on directory rows."
        )
        XCTAssertTrue(app.menuItems["Copy"].exists)
        XCTAssertTrue(app.menuItems["Cut"].exists)
        XCTAssertTrue(app.menuItems["Move to Trash"].exists)

        app.typeKey(.escape, modifierFlags: [])
    }

    /// Copy a file → right-click a folder → Paste. The file must
    /// land inside that folder; the original stays in place.
    func testCopyFile_pasteIntoFolder_landsInTargetFolder() throws {
        let (app, file, project) = launchWithSeed()
        let dest = makeDir("dest", in: project)

        showFileBrowser(in: app)

        waitForRow(in: app, atPath: file.path).rightClick()
        clickMenuItem("Copy", in: app)

        waitForRow(in: app, atPath: dest.path).rightClick()
        clickMenuItem("Paste", in: app)

        waitForFileExistence(at: dest.appendingPathComponent("file.txt"))
        XCTAssertTrue(
            FileManager.default.fileExists(atPath: file.path),
            "Copy must leave the source in place."
        )
    }

    /// Right-clicking a *file* and choosing Paste must resolve the
    /// destination to that file's parent directory — Finder behaviour.
    func testPasteIntoFile_resolvesToParentDirectory() throws {
        let (app, file, project) = launchWithSeed()
        let sub = makeDir("sub", in: project)
        let nested = sub.appendingPathComponent("nested.txt")
        FileManager.default.createFile(atPath: nested.path, contents: Data())

        showFileBrowser(in: app)

        waitForRow(in: app, atPath: file.path).rightClick()
        clickMenuItem("Copy", in: app)

        // Expand sub so the nested row materialises, then right-click
        // the nested file (not the folder).
        waitForRow(in: app, atPath: sub.path).click()
        waitForRow(in: app, atPath: nested.path).rightClick()
        clickMenuItem("Paste", in: app)

        // Pasted file should land in `sub/` (the parent of the
        // right-clicked file), not next to nested at the root.
        waitForFileExistence(at: sub.appendingPathComponent("file.txt"))
        XCTAssertFalse(
            FileManager.default.fileExists(
                atPath: project.appendingPathComponent("file copy.txt").path
            ),
            "Paste must NOT land at root when the right-click target is a file inside a subdirectory."
        )
    }

    /// Pasting where the destination already has a file with that
    /// name auto-renames the copy with a `" copy"` suffix.
    func testCollisionPaste_autoRenamesWithCopySuffix() throws {
        let (app, file, project) = launchWithSeed()
        let dest = makeDir("dest", in: project)
        // Pre-populate dest with file.txt to force a collision.
        FileManager.default.createFile(
            atPath: dest.appendingPathComponent("file.txt").path,
            contents: Data("existing".utf8)
        )

        showFileBrowser(in: app)

        waitForRow(in: app, atPath: file.path).rightClick()
        clickMenuItem("Copy", in: app)

        waitForRow(in: app, atPath: dest.path).rightClick()
        clickMenuItem("Paste", in: app)

        waitForFileExistence(at: dest.appendingPathComponent("file copy.txt"))
        // Original collision target wasn't clobbered.
        let existing = try? String(
            contentsOf: dest.appendingPathComponent("file.txt"), encoding: .utf8
        )
        XCTAssertEqual(existing, "existing", "Collision must not overwrite the existing file.")
    }

    /// Cut a file → paste into another folder → file moves (source
    /// gone, destination gets it).
    func testCutFile_paste_movesFile() throws {
        let (app, file, project) = launchWithSeed()
        let dest = makeDir("dest", in: project)

        showFileBrowser(in: app)

        waitForRow(in: app, atPath: file.path).rightClick()
        clickMenuItem("Cut", in: app)

        waitForRow(in: app, atPath: dest.path).rightClick()
        clickMenuItem("Paste", in: app)

        waitForFileExistence(at: dest.appendingPathComponent("file.txt"))
        waitForFileMissing(at: file)
    }

    /// Cut a folder containing children → paste into another folder
    /// → entire tree relocates.
    func testCutFolder_paste_movesEntireTree() throws {
        let (app, _, project) = launchWithSeed()
        let src = makeDir("src", in: project)
        FileManager.default.createFile(
            atPath: src.appendingPathComponent("inside.txt").path,
            contents: Data("data".utf8)
        )
        let dest = makeDir("dest", in: project)

        showFileBrowser(in: app)

        waitForRow(in: app, atPath: src.path).rightClick()
        clickMenuItem("Cut", in: app)

        waitForRow(in: app, atPath: dest.path).rightClick()
        clickMenuItem("Paste", in: app)

        waitForFileExistence(at: dest.appendingPathComponent("src/inside.txt"))
        waitForFileMissing(at: src)
    }

    /// Trash a folder containing a child file → ⌘Z must restore the
    /// folder *and* the child intact, not just the directory entry.
    func testTrashFolder_undoRestoresChildrenIntact() throws {
        let (app, _, project) = launchWithSeed()
        let folder = makeDir("dir", in: project)
        let nested = folder.appendingPathComponent("inside.txt")
        FileManager.default.createFile(
            atPath: nested.path, contents: Data("data".utf8)
        )

        showFileBrowser(in: app)

        waitForRow(in: app, atPath: folder.path).rightClick()
        clickMenuItem("Move to Trash", in: app)
        waitForFileMissing(at: folder)

        app.typeKey("z", modifierFlags: [.command])

        waitForFileExistence(at: nested)
        let body = try? String(contentsOf: nested, encoding: .utf8)
        XCTAssertEqual(body, "data", "Restored child must keep its contents.")
    }

    /// Trash → ⌘Z restores → ⌘⇧Z must re-trash. Verifies the redo
    /// keypath is wired.
    func testCmdShiftZ_redoesTrashedFile() throws {
        let (app, file, _) = launchWithSeed()
        showFileBrowser(in: app)

        waitForRow(in: app, atPath: file.path).rightClick()
        clickMenuItem("Move to Trash", in: app)
        waitForFileMissing(at: file)

        app.typeKey("z", modifierFlags: [.command])
        waitForFileExistence(at: file)

        app.typeKey("z", modifierFlags: [.command, .shift])
        waitForFileMissing(at: file)
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
        track(app)
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

    /// Click a context-menu item by its title. Some titles (Copy,
    /// Cut, Paste) collide with menu-bar entries — `app.menuItems[title]`
    /// then matches multiple elements and `.click()` refuses to act.
    /// Resolve by iterating all matches and clicking the first
    /// hittable one (the menu bar entries are in collapsed menus and
    /// aren't hittable; the visible context popup is).
    private func clickMenuItem(_ title: String, in app: XCUIApplication) {
        let predicate = NSPredicate(format: "label == %@ OR title == %@", title, title)
        let deadline = Date().addingTimeInterval(5)
        while Date() < deadline {
            let matches = app.menuItems.matching(predicate).allElementsBoundByIndex
            for item in matches where item.isHittable {
                item.click()
                return
            }
            Thread.sleep(forTimeInterval: 0.05)
        }
        XCTFail("No hittable context-menu item titled '\(title)' found within 5s.")
    }

    /// Create a subdirectory inside the test project. Returns the
    /// folder's URL so tests can build paths under it.
    @discardableResult
    private func makeDir(_ name: String, in project: URL) -> URL {
        let url = project.appendingPathComponent(name, isDirectory: true)
        try? FileManager.default.createDirectory(
            at: url, withIntermediateDirectories: true
        )
        return url
    }

    /// Spin until `url` exists on disk, or fail the test. File ops
    /// dispatched through the menu run synchronously on the main
    /// thread, but we poll with a short timeout to absorb
    /// XCUIApplication command-relay latency between the menu click
    /// and the action firing.
    private func waitForFileExistence(
        at url: URL,
        timeout: TimeInterval = 5,
        file: StaticString = #file,
        line: UInt = #line
    ) {
        let deadline = Date().addingTimeInterval(timeout)
        while !FileManager.default.fileExists(atPath: url.path) {
            if Date() > deadline {
                XCTFail(
                    "File never appeared at \(url.path) within \(timeout)s",
                    file: file, line: line
                )
                return
            }
            Thread.sleep(forTimeInterval: 0.05)
        }
    }

    /// Inverse of `waitForFileExistence` — spin until the path is
    /// gone (or fail). Used after Trash and Cut+Paste to confirm
    /// the source went away.
    private func waitForFileMissing(
        at url: URL,
        timeout: TimeInterval = 5,
        file: StaticString = #file,
        line: UInt = #line
    ) {
        let deadline = Date().addingTimeInterval(timeout)
        while FileManager.default.fileExists(atPath: url.path) {
            if Date() > deadline {
                XCTFail(
                    "File still exists at \(url.path) after \(timeout)s",
                    file: file, line: line
                )
                return
            }
            Thread.sleep(forTimeInterval: 0.05)
        }
    }
}
