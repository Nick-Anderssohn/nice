//
//  MultiWindowRestoreUITests.swift
//  NiceUITests
//
//  Drives the real SwiftUI scene-restoration interaction that
//  unit tests can't reach:
//
//   • `WindowGroup` posting `willCloseNotification` for every live
//     window during `app.terminate(_:)` (the Phase 1 regression
//     vector). Asserts that the willTerminate cascade preserves
//     every snapshot regardless of the scene-teardown burst.
//
//   • The launch-time openWindow(id: "main") fan-out actually
//     spawning N windows for N saved entries in `sessions.json`.
//
//   • End-to-end JSON round-trip across a process kill/restart with
//     real `SessionStore.shared` reading and writing through the
//     sandboxed Application Support directory.
//
//  Test seeds a 2-window `sessions.json` into the
//  `NICE_APPLICATION_SUPPORT_ROOT` sandbox, launches Nice Dev,
//  confirms both windows + tabs come up, terminates (the AppKit
//  willTerminate path), relaunches, confirms both windows + tabs
//  come back. If any step regresses, the assertion firing here is
//  the loudest possible signal — the user observable surface is
//  what this test mimics.
//

import XCTest

final class MultiWindowRestoreUITests: NiceUITestCase {

    /// Pre-seed two windows with distinct sentinel tab ids, launch,
    /// quit, relaunch, assert both come back. The unit-level Phase
    /// 1 / Phase 4 tests cover the persistence layer in isolation;
    /// this test pins the SwiftUI scene-graph wiring those tests
    /// can't reach.
    func test_twoSeededWindows_quit_relaunch_bothReturn() throws {
        let homePath = makeFakeHome()
        let supportRoot = (homePath as NSString)
            .appendingPathComponent("Library/Application Support")
        try seedSessionsJson(
            at: supportRoot,
            windows: [
                makePersistedWindowJSON(id: "w1", tabId: "uitest-tab-A"),
                makePersistedWindowJSON(id: "w2", tabId: "uitest-tab-B"),
            ]
        )

        // === Launch 1: both seeded windows must come up ===
        let app = launchAppWithSandbox(homePath: homePath)
        try waitForTab(id: "uitest-tab-A", in: app, timeout: 10)
        try waitForTab(id: "uitest-tab-B", in: app, timeout: 5)
        XCTAssertGreaterThanOrEqual(
            app.windows.count, 2,
            "Both seeded windows must come up — the openWindow(id: \"main\") fan-out drives the second window."
        )

        // Clean terminate: triggers the willTerminate cascade. The
        // Phase 1 fix is what keeps both snapshots on disk through
        // the SwiftUI scene-teardown willCloseNotification burst.
        app.terminate()

        // Both snapshots must still be on disk for the next launch
        // to find them. Reading the file directly is the contract
        // under test.
        let sessionsPath = (supportRoot as NSString)
            .appendingPathComponent("Nice/sessions.json")
        let postQuitIds = readWindowTabIds(at: sessionsPath)
        XCTAssertEqual(
            postQuitIds.sorted(), ["uitest-tab-A", "uitest-tab-B"],
            "After ⌘Q with both windows open, sessions.json must hold both windows' tabs."
        )

        // === Launch 2: both windows must reopen ===
        let app2 = launchAppWithSandbox(homePath: homePath)
        try waitForTab(id: "uitest-tab-A", in: app2, timeout: 10)
        try waitForTab(id: "uitest-tab-B", in: app2, timeout: 5)
        XCTAssertGreaterThanOrEqual(
            app2.windows.count, 2,
            "Both windows must come back on relaunch."
        )
    }

    // MARK: - Sandbox / launch helpers

    private var fakeHomeURL: URL?

    private func makeFakeHome() -> String {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-multi-window-uitest-\(UUID().uuidString)",
                isDirectory: true
            )
        try? FileManager.default.createDirectory(
            at: url, withIntermediateDirectories: true
        )
        fakeHomeURL = url
        return url.path
    }

    /// Mirrors the pattern in `NiceUITests.swift`: sandbox HOME and
    /// `NICE_APPLICATION_SUPPORT_ROOT` to a tmp dir, suppress
    /// AppKit state restoration, plus `NICE_CLAUDE_OVERRIDE = /bin/cat`
    /// so the seeded Claude panes don't try to spawn the real binary.
    @discardableResult
    private func launchAppWithSandbox(homePath: String) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments += ["-ApplePersistenceIgnoreState", "YES"]
        app.launchEnvironment["HOME"] = homePath
        app.launchEnvironment["NICE_APPLICATION_SUPPORT_ROOT"] =
            (homePath as NSString).appendingPathComponent("Library/Application Support")
        // /bin/cat satisfies the Claude shadow without trying to spawn
        // a real LLM session — the tab content reads "claude --resume
        // <uuid>" pre-typed at a prompt, and no further interaction
        // is needed for the visibility assertions below.
        app.launchEnvironment["NICE_CLAUDE_OVERRIDE"] = "/bin/cat"
        let hostEnv = ProcessInfo.processInfo.environment
        if let user = hostEnv["USER"] { app.launchEnvironment["USER"] = user }
        if let logname = hostEnv["LOGNAME"] {
            app.launchEnvironment["LOGNAME"] = logname
        }
        app.launch()
        track(app)
        return app
    }

    // MARK: - sessions.json seeding

    /// Build the on-disk `sessions.json` the next app launch will
    /// read. The `Nice/` subfolder name matches what `SessionStore`
    /// constructs from `CFBundleName` (and `scripts/test.sh`
    /// deliberately leaves `PRODUCT_NAME` as "Nice" — see the
    /// `testSessionIdRotation_persistsAcrossRestart` setup comment).
    private func seedSessionsJson(
        at supportRoot: String,
        windows: [[String: Any]]
    ) throws {
        let dir = (supportRoot as NSString).appendingPathComponent("Nice")
        try FileManager.default.createDirectory(
            atPath: dir, withIntermediateDirectories: true
        )
        let payload: [String: Any] = [
            "version": 3,
            "windows": windows,
        ]
        let data = try JSONSerialization.data(
            withJSONObject: payload,
            options: [.prettyPrinted, .sortedKeys]
        )
        let path = (dir as NSString).appendingPathComponent("sessions.json")
        try data.write(to: URL(fileURLWithPath: path), options: .atomic)
    }

    /// Shape of a seeded window: one Claude tab whose `claudeSessionId`
    /// is a synthetic uuid (we don't need it to round-trip a real
    /// session; the assertion is on tab/pane presence). The empty
    /// Terminals project keeps the restored sidebar layout
    /// recognisable to the user.
    private func makePersistedWindowJSON(
        id: String, tabId: String
    ) -> [String: Any] {
        let claudePaneId = "\(tabId)-claude"
        return [
            "id": id,
            "activeTabId": tabId,
            "sidebarCollapsed": false,
            "projects": [
                [
                    "id": "terminals",
                    "name": "Terminals",
                    "path": "/tmp",
                    "tabs": [],
                ] as [String: Any],
                [
                    "id": "proj-\(id)",
                    "name": id.uppercased(),
                    "path": "/tmp/\(id)",
                    "tabs": [
                        [
                            "id": tabId,
                            "title": "Claude tab",
                            "cwd": "/tmp",
                            "branch": NSNull(),
                            "claudeSessionId": UUID().uuidString.lowercased(),
                            "activePaneId": claudePaneId,
                            "panes": [
                                [
                                    "id": claudePaneId,
                                    "title": "Claude",
                                    "kind": "claude",
                                ],
                            ],
                        ] as [String: Any],
                    ],
                ] as [String: Any],
            ],
        ]
    }

    // MARK: - Wait / read

    /// Block until any descendant of `app` has accessibility
    /// identifier `sidebar.tab.<tabId>.title` (sidebar row title
    /// element installed by `SidebarView`). Cross-window:
    /// `descendants(matching:)` traverses every open window.
    private func waitForTab(
        id tabId: String, in app: XCUIApplication, timeout: TimeInterval
    ) throws {
        let element = app.descendants(matching: .any)["sidebar.tab.\(tabId).title"]
        XCTAssertTrue(
            element.waitForExistence(timeout: timeout),
            "Expected sidebar row for tab \(tabId) to appear within \(timeout)s — restore path is broken."
        )
    }

    /// Read every saved window's first non-terminals tab id from
    /// `sessions.json` so the test can assert on identity after a
    /// terminate. Returns an empty array if the file isn't readable.
    private func readWindowTabIds(at path: String) -> [String] {
        guard let data = try? Data(contentsOf: URL(fileURLWithPath: path)),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let windows = root["windows"] as? [[String: Any]]
        else { return [] }
        var ids: [String] = []
        for window in windows {
            let projects = (window["projects"] as? [[String: Any]]) ?? []
            for project in projects where (project["id"] as? String) != "terminals" {
                for tab in (project["tabs"] as? [[String: Any]]) ?? [] {
                    if let tid = tab["id"] as? String {
                        ids.append(tid)
                    }
                }
            }
        }
        return ids
    }
}
