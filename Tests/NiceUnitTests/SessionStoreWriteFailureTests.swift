//
//  SessionStoreWriteFailureTests.swift
//  NiceUnitTests
//
//  Pins atomic-write failure recovery: when a flush can't write the
//  sessions.json file (read-only directory, full disk, parent-not-a-
//  directory, etc.), the previous file must remain byte-identical and
//  no exception must escape. `SessionStore.write` uses `try?` and
//  `Data.write(.atomic)`, so a failed write is silently swallowed and
//  the prior file (which the atomic rename never replaced) is intact.
//
//  The failure injection here uses `chmod 0500` on the parent dir to
//  block creation of the atomic-write temp file. If this turns out to
//  be ineffective on a future macOS / APFS combination, the fallback
//  is to swap the parent for a regular file (mirrors the pattern in
//  ClaudeHookInstallerTests' failure-doesn't-throw test).
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class SessionStoreWriteFailureTests: XCTestCase {

    private var supportRoot: URL!
    private var originalAppSupport: String?

    override func setUp() {
        super.setUp()
        supportRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-sessionstore-write-fail-\(UUID().uuidString)",
                isDirectory: true
            )
        try? FileManager.default.createDirectory(
            at: supportRoot, withIntermediateDirectories: true
        )
        originalAppSupport =
            ProcessInfo.processInfo.environment["NICE_APPLICATION_SUPPORT_ROOT"]
        setenv("NICE_APPLICATION_SUPPORT_ROOT", supportRoot.path, 1)
    }

    override func tearDown() {
        // Best-effort: restore writability before cleanup so removeItem
        // doesn't fail. chmod 0700 is enough; -R covers the off-chance
        // we left a nested directory locked.
        _ = try? FileManager.default.setAttributes(
            [.posixPermissions: 0o700],
            ofItemAtPath: niceDir.path
        )
        if let originalAppSupport {
            setenv("NICE_APPLICATION_SUPPORT_ROOT", originalAppSupport, 1)
        } else {
            unsetenv("NICE_APPLICATION_SUPPORT_ROOT")
        }
        try? FileManager.default.removeItem(at: supportRoot)
        super.tearDown()
    }

    private var niceDir: URL {
        supportRoot.appendingPathComponent("Nice", isDirectory: true)
    }

    private var sessionsFile: URL {
        niceDir.appendingPathComponent("sessions.json")
    }

    func test_flushFailure_leavesPriorFileUntouched_andDoesNotThrow() throws {
        // Step 1: successful flush establishes baseline state A on disk.
        do {
            let store = SessionStore()
            store.upsert(window: makeWindow(id: "w1", sessionId: "STATE-A"))
            store.flush()
        }
        let priorBytes = try Data(contentsOf: sessionsFile)
        XCTAssertFalse(priorBytes.isEmpty, "precondition: state A must be on disk")

        // Step 2: lock the parent dir so the atomic-write temp file
        // can't be created. `chmod 0500` = read + execute only; no
        // write permission means open(O_CREAT) fails.
        try FileManager.default.setAttributes(
            [.posixPermissions: 0o500],
            ofItemAtPath: niceDir.path
        )

        // Step 3: attempt to flush state B. This must NOT throw or
        // crash; SessionStore.write uses `try?`, so the failure is
        // silent at the production level. The test verifies that
        // silence by inspecting the file afterward.
        do {
            let store = SessionStore()
            store.upsert(window: makeWindow(id: "w1", sessionId: "STATE-B"))
            store.flush()
        }

        // Step 4: restore writability so we can read the file.
        try FileManager.default.setAttributes(
            [.posixPermissions: 0o700],
            ofItemAtPath: niceDir.path
        )

        // The atomic write either succeeded fully or didn't run at
        // all — the prior file must be byte-identical to the
        // pre-failure snapshot.
        let postBytes = try Data(contentsOf: sessionsFile)
        XCTAssertEqual(
            postBytes, priorBytes,
            "Failed atomic write must leave the prior sessions.json intact."
        )

        // And: a fresh SessionStore reads state A back, confirming
        // the on-disk recovery is observable end-to-end.
        let fresh = SessionStore()
        let restoredId = fresh.load().windows.first?
            .projects.first?.tabs.first?.claudeSessionId
        XCTAssertEqual(
            restoredId, "STATE-A",
            "After a failed flush, a fresh SessionStore must read the prior state."
        )
    }

    // MARK: - helpers

    private func makeWindow(id: String, sessionId: String) -> PersistedWindow {
        let tab = PersistedTab(
            id: "tab-\(id)", title: "Tab", cwd: "/tmp", branch: nil,
            claudeSessionId: sessionId, activePaneId: "pane-\(id)",
            panes: [PersistedPane(id: "pane-\(id)", title: "Claude", kind: .claude)]
        )
        let project = PersistedProject(
            id: "project-\(id)", name: "P", path: "/tmp", tabs: [tab]
        )
        return PersistedWindow(
            id: id, activeTabId: tab.id,
            sidebarCollapsed: false, projects: [project]
        )
    }
}
