//
//  SessionsModelClaudeThemeSyncTests.swift
//  NiceUnitTests
//
//  Integration coverage for the wire between the per-window
//  `SessionThemeCache.syncClaudeTheme` flag and `makeSession`: when sync
//  is on, a created session is handed the `--settings <pointer>` path
//  (which materializes the pointer file); when off, it is not. The
//  isolated arg-emission (buildClaudeExecCommand / buildClaudeExtraEnv)
//  and JSON generation are covered elsewhere — this pins the gating wire
//  that joins them.
//
//  Doubles as a hermeticity regression test: makeSession's
//  `ClaudeThemeSync.settingsFlagPath()` is NOT injectable, so it must
//  resolve its path off the redirected `$HOME` (TestHomeSandbox) rather
//  than NSHomeDirectory — otherwise it scribbles the developer's real
//  ~/.nice. Asserting the file lands under the sandbox HOME proves the
//  ClaudeThemeSync.homeBase() seam works through the production path.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class SessionsModelClaudeThemeSyncTests: XCTestCase {

    private var sandbox: TestHomeSandbox!
    private var savedSyncClaudeThemeDefault: Any?

    override func setUp() {
        super.setUp()
        sandbox = TestHomeSandbox()
        // The AppState.init seed test below mutates `Tweaks.syncClaudeTheme`,
        // which persists to `UserDefaults.standard` — and `test.sh` runs
        // under the *dev* bundle id, the very domain the running Nice Dev
        // app reads. Save the key here and restore it in tearDown so a test
        // can never flip the developer's real "Sync Claude Code theme"
        // toggle. (NiceServices() builds its own Tweaks on .standard, so an
        // ephemeral suite can't be injected — save/restore is the seam.)
        savedSyncClaudeThemeDefault =
            UserDefaults.standard.object(forKey: Tweaks.syncClaudeThemeKey)
    }

    override func tearDown() {
        if let savedSyncClaudeThemeDefault {
            UserDefaults.standard.set(
                savedSyncClaudeThemeDefault, forKey: Tweaks.syncClaudeThemeKey
            )
        } else {
            UserDefaults.standard.removeObject(forKey: Tweaks.syncClaudeThemeKey)
        }
        sandbox.teardown()
        sandbox = nil
        super.tearDown()
    }

    /// The theme-pointer file as resolved under the redirected HOME — the
    /// same `$HOME` ClaudeThemeSync.homeBase() reads, so this is exactly
    /// where a hermetic write must land.
    private func sandboxPointerFile() throws -> URL {
        let home = try XCTUnwrap(ProcessInfo.processInfo.environment["HOME"])
        return URL(fileURLWithPath: home)
            .appendingPathComponent(".nice/claude-theme-settings.json")
    }

    func test_makeSession_writesPointerUnderSandbox_whenSyncOn() throws {
        // Cache seeds OFF, so enable sync first (as AppShellHost.onAppear does).
        let sessions = SessionsModel(tabs: TabModel(initialMainCwd: "/tmp"))
        sessions.updateSyncClaudeTheme(true)

        // Clear the pointer written by enabling so the assertion isolates
        // makeSession's own settingsFlagPath() call.
        let file = try sandboxPointerFile()
        try? FileManager.default.removeItem(at: file)

        _ = sessions.makeSession(for: "tab-theme-sync-on", cwd: "/tmp")

        XCTAssertTrue(
            FileManager.default.fileExists(atPath: file.path),
            "sync on: makeSession must materialize the --settings pointer under the sandbox HOME"
        )
        let json = try JSONSerialization.jsonObject(with: Data(contentsOf: file)) as? [String: Any]
        XCTAssertEqual(json?["theme"] as? String, "custom:nice",
                       "pointer file must select the Nice-managed custom theme")
    }

    func test_makeSession_omitsPointer_whenSyncOff() throws {
        let sessions = SessionsModel(tabs: TabModel(initialMainCwd: "/tmp"))
        sessions.updateSyncClaudeTheme(false)   // does not itself write
        _ = sessions.makeSession(for: "tab-theme-sync-off", cwd: "/tmp")

        XCTAssertFalse(
            FileManager.default.fileExists(atPath: try sandboxPointerFile().path),
            "sync off: makeSession must not call settingsFlagPath, so no pointer file is written"
        )
    }

    // MARK: - AppState.init seed ordering (the restore-time regression)

    func test_appStateInit_seedsSyncClaudeThemeFromTweaks_beforeStart() {
        // The regression this guards: restored Claude panes are spawned in
        // AppState.start()/restoreSavedWindow via makeSession, which runs
        // BEFORE AppShellHost.onAppear reconciles the per-window sync flag.
        // AppState.init must therefore seed `themeCache.syncClaudeTheme`
        // from the persisted Tweaks toggle itself — otherwise restored
        // panes read the cache's OFF placeholder and spawn without
        // --settings. Deleting that init seed line must FAIL this test.
        //
        // (Composes with `test_makeSession_writesPointerUnderSandbox_whenSyncOn`,
        // which pins flag-on → makeSession → pointer; this pins init →
        // flag-on, closing the init→restore ordering end to end.)
        let onServices = NiceServices()
        onServices.tweaks.syncClaudeTheme = true
        let onState = AppState(
            services: onServices,
            initialSidebarCollapsed: false,
            initialMainCwd: "/tmp",
            windowSessionId: UUID().uuidString
        )
        XCTAssertTrue(
            onState.sessions.themeCache.syncClaudeTheme,
            "AppState.init must seed syncClaudeTheme=true from Tweaks before start()/restore spawns panes; removing the init seed reintroduces the restore-time --settings regression."
        )

        // Inverse: an opted-out user must have the cache seeded OFF, so
        // restored panes get no --settings and nothing is written.
        let offServices = NiceServices()
        offServices.tweaks.syncClaudeTheme = false
        let offState = AppState(
            services: offServices,
            initialSidebarCollapsed: false,
            initialMainCwd: "/tmp",
            windowSessionId: UUID().uuidString
        )
        XCTAssertFalse(
            offState.sessions.themeCache.syncClaudeTheme,
            "Opted-out: AppState.init must seed sync OFF."
        )
    }
}
