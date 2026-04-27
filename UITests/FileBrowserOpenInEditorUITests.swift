//
//  FileBrowserOpenInEditorUITests.swift
//  NiceUITests
//
//  End-to-end coverage for the "Open in Editor Pane" flow. Stands the
//  app up against a sandboxed `HOME`, seeds editor + extension-mapping
//  state through the `NICE_TEST_EDITOR_SEED` env var (cfprefsd doesn't
//  honour the sandboxed HOME for prefs lookup, so a plist on disk
//  wouldn't be visible to the launched app), then drives the right-
//  click "Open in Editor Pane" submenu and asserts a new pane pill
//  appears in the toolbar.
//
//  Why right-click rather than double-click: FileTreeRow uses a
//  custom 280 ms tap window (deliberately tighter than macOS's
//  ~500 ms default for snappier expand/collapse feedback), but
//  XCUIElement's click events are throttled by XCUITest's "wait
//  until idle" between actions, so two `click()` calls land outside
//  that window and aren't recognised as a double-click. The right-
//  click submenu invokes the same `AppState.openInEditorPane` path
//  the double-click handler does — `FileTreeRow.doubleClick` just
//  calls `appState.openFromDoubleClick(url:)` which routes to the
//  same orchestrator — so the menu test covers the spawn pipeline.
//  The double-click routing decision (mapped → editor, unmapped →
//  NSWorkspace) is covered by `AppStateOpenInEditorPaneTests`.
//
//  The test uses `sleep 30` as the stub editor command so the spawned
//  pane stays alive long enough to verify without depending on `vim`
//  being installed on the test machine. Process teardown happens at
//  app shutdown.
//

import XCTest

final class FileBrowserOpenInEditorUITests: XCTestCase {

    private var fakeHomeURL: URL?

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    override func tearDownWithError() throws {
        if let url = fakeHomeURL {
            try? FileManager.default.removeItem(at: url)
        }
        fakeHomeURL = nil
    }

    // MARK: - Tests

    /// Smoke test that the seed env var actually plumbs into the
    /// running app's Tweaks. After seeding "TestEditor", the right-
    /// click "Open in Editor Pane" submenu should list it. If this
    /// passes, the seed pipeline is OK and any failure in the double-
    /// click test below is a routing/spawn problem rather than a
    /// configuration-not-loaded problem.
    func testSeedEnvVar_appearsInOpenInEditorPaneSubmenu() throws {
        let editorId = UUID()
        let editor = EditorEntry(id: editorId, name: "TestEditor", command: "sleep 30")
        let (app, file, _) = launchWithSeed(editors: [editor], mappings: [:])

        showFileBrowser(in: app)

        let row = waitForRow(in: app, atPath: file.path)
        row.rightClick()

        let submenu = app.menuItems["Open in Editor Pane"]
        XCTAssertTrue(
            submenu.waitForExistence(timeout: 5),
            "Open in Editor Pane submenu must exist on file rows."
        )
        submenu.hover()

        let editorItem = app.menuItems["TestEditor"]
        XCTAssertTrue(
            editorItem.waitForExistence(timeout: 5),
            "Seeded 'TestEditor' must appear in the submenu — proves NICE_TEST_EDITOR_SEED reached the app."
        )
        app.typeKey(.escape, modifierFlags: [])
    }

    /// Right-click a file → "Open in Editor Pane" → click the seeded
    /// "TestEditor" entry. A new pane pill must appear in the
    /// toolbar — proving the orchestration path (`AppState.
    /// openInEditorPane` → `addPane` → `TabPtySession.addTerminalPane`
    /// with `command:` → toolbar repaint) holds end-to-end.
    func testOpenInEditorPaneSubmenu_clickEditor_spawnsPane() throws {
        let editorId = UUID()
        let editor = EditorEntry(id: editorId, name: "TestEditor", command: "sleep 30")
        let (app, file, _) = launchWithSeed(editors: [editor], mappings: [:])

        showFileBrowser(in: app)

        let baselinePillCount = paneToolbarPillCount(in: app)

        let row = waitForRow(in: app, atPath: file.path)
        row.rightClick()

        let submenu = app.menuItems["Open in Editor Pane"]
        XCTAssertTrue(submenu.waitForExistence(timeout: 5))
        submenu.hover()

        let editorItem = app.menuItems["TestEditor"]
        XCTAssertTrue(editorItem.waitForExistence(timeout: 5))
        // Hover the editor item itself before clicking. Without this,
        // XCUITest clicks the snapshot frame captured while the submenu
        // was still animating open, which on CI lands outside the
        // item's final position once macOS auto-positions the submenu.
        // Hovering forces a fresh layout query, and the coordinate
        // click then targets the element's current center.
        editorItem.hover()
        editorItem.coordinate(
            withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)
        ).click()

        // Pane spawn is synchronous from the menu click down to
        // addPane; poll briefly to absorb XCUITest command-relay
        // latency before asserting the pill arrived.
        let deadline = Date().addingTimeInterval(10)
        while Date() < deadline {
            if paneToolbarPillCount(in: app) > baselinePillCount {
                return
            }
            Thread.sleep(forTimeInterval: 0.1)
        }
        XCTFail(
            "Expected a new pane pill within 10 s. Pill count stayed at \(baselinePillCount)."
        )
    }

    // MARK: - Plumbing

    private struct EditorEntry {
        let id: UUID
        let name: String
        let command: String
    }

    /// Launch the app with a sandboxed HOME, a seeded test project at
    /// `<HOME>/project`, a `file.txt` to right-click, and the supplied
    /// editor list + extension mappings written into the dev bundle's
    /// prefs plist so `Tweaks` picks them up on init.
    private func launchWithSeed(
        editors: [EditorEntry],
        mappings: [String: UUID]
    ) -> (XCUIApplication, URL, URL) {
        let home = makeFakeHome()
        let project = home.appendingPathComponent("project", isDirectory: true)
        try? FileManager.default.createDirectory(
            at: project, withIntermediateDirectories: true
        )
        let file = project.appendingPathComponent("file.txt")
        FileManager.default.createFile(
            atPath: file.path, contents: Data("hello".utf8)
        )

        let seedJSON = try! buildSeedJSON(editors: editors, mappings: mappings)

        let app = XCUIApplication()
        app.launchArguments += ["-ApplePersistenceIgnoreState", "YES"]
        app.launchEnvironment["HOME"] = home.path
        app.launchEnvironment["NICE_APPLICATION_SUPPORT_ROOT"] =
            home.appendingPathComponent("Library/Application Support").path
        app.launchEnvironment["NICE_MAIN_CWD"] = project.path
        // Tweaks.loadEditorCommands / loadExtensionEditorMap honour
        // this env var when set, returning the seeded values directly
        // and bypassing UserDefaults. Avoids the cfprefsd-doesn't-
        // honour-HOME problem that would otherwise leave the app
        // launching with no editor configuration.
        app.launchEnvironment["NICE_TEST_EDITOR_SEED"] = seedJSON
        let hostEnv = ProcessInfo.processInfo.environment
        if let user = hostEnv["USER"] { app.launchEnvironment["USER"] = user }
        if let logname = hostEnv["LOGNAME"] {
            app.launchEnvironment["LOGNAME"] = logname
        }
        app.launch()
        return (app, file, project)
    }

    /// Encode the seed payload in the shape `Tweaks.TestSeed`
    /// expects: `{ "editorCommands": [{id,name,command}, …],
    /// "extensionEditorMap": {ext: uuid-string, …} }`. The test
    /// target doesn't import Nice's source so we build the JSON by
    /// hand rather than using EditorCommand directly.
    private func buildSeedJSON(
        editors: [EditorEntry],
        mappings: [String: UUID]
    ) throws -> String {
        let editorJSON: [[String: String]] = editors.map {
            ["id": $0.id.uuidString, "name": $0.name, "command": $0.command]
        }
        let mapJSON = mappings.mapValues(\.uuidString)
        let payload: [String: Any] = [
            "editorCommands": editorJSON,
            "extensionEditorMap": mapJSON,
        ]
        let data = try JSONSerialization.data(withJSONObject: payload)
        return String(decoding: data, as: UTF8.self)
    }

    private func makeFakeHome() -> URL {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-fb-editor-\(UUID().uuidString)",
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

    /// Count `tab.pill.*` elements currently in the window. Lets the
    /// negative test pin "no new pane appeared" without needing to
    /// know the exact pill ids.
    private func paneToolbarPillCount(in app: XCUIApplication) -> Int {
        let predicate = NSPredicate(format: "identifier BEGINSWITH 'tab.pill.'")
        return app.descendants(matching: .any)
            .matching(predicate)
            .count
    }
}
