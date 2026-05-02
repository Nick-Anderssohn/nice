//
//  SessionStoreFlushOrderingTests.swift
//  NiceUnitTests
//
//  Pins the cross-window ordering guarantees of the upsert/debounce/flush
//  loop. The session-tracking feature relies on them: when claude rotates
//  its session id (via /clear, /compact, /branch) and the user
//  immediately quits, the in-flight debounce must NOT clobber the latest
//  in-memory state when `flush()` runs from `willTerminate`.
//
//  Companion to `SessionStoreTests.test_flush_afterUpsert_persistsLatestStateForSameWindow`,
//  which covers the same property for a single window. This file covers
//  the multi-window case (multiple WindowSessions in one process each
//  upserting their own slot in interleaved order).
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class SessionStoreFlushOrderingTests: XCTestCase {

    private var supportRoot: URL!
    private var originalAppSupport: String?

    override func setUp() {
        super.setUp()
        supportRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-sessionstore-flush-\(UUID().uuidString)", isDirectory: true
            )
        try? FileManager.default.createDirectory(
            at: supportRoot, withIntermediateDirectories: true
        )
        originalAppSupport =
            ProcessInfo.processInfo.environment["NICE_APPLICATION_SUPPORT_ROOT"]
        setenv("NICE_APPLICATION_SUPPORT_ROOT", supportRoot.path, 1)
    }

    override func tearDown() {
        if let originalAppSupport {
            setenv("NICE_APPLICATION_SUPPORT_ROOT", originalAppSupport, 1)
        } else {
            unsetenv("NICE_APPLICATION_SUPPORT_ROOT")
        }
        try? FileManager.default.removeItem(at: supportRoot)
        super.tearDown()
    }

    func test_interleavedUpsertsAcrossWindows_flushPersistsLatestForEach() throws {
        // Three windows, each upserted twice in interleaved order.
        // The second upsert for each window changes the session id of
        // its only tab (mirrors a /clear rotation landing while another
        // window's debounce is in flight). After flush, every window's
        // latest id must win.
        let store = SessionStore()

        // Round 1 — initial state.
        store.upsert(window: makeWindow(id: "w1", tabSessionId: "S1-INIT"))
        store.upsert(window: makeWindow(id: "w2", tabSessionId: "S2-INIT"))
        store.upsert(window: makeWindow(id: "w3", tabSessionId: "S3-INIT"))

        // Round 2 — interleaved rotations, all landing inside the
        // 500ms debounce window before any write has been issued.
        store.upsert(window: makeWindow(id: "w2", tabSessionId: "S2-NEW"))
        store.upsert(window: makeWindow(id: "w1", tabSessionId: "S1-NEW"))
        store.upsert(window: makeWindow(id: "w3", tabSessionId: "S3-NEW"))

        // Synchronous flush from willTerminate.
        store.flush()

        // Spin past the debounce window in case the cancellation is
        // broken — a stale work item firing after flush would rewrite
        // the file with a partial / out-of-order state.
        let elapsed = XCTestExpectation(description: "debounce window elapsed")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) {
            elapsed.fulfill()
        }
        wait(for: [elapsed], timeout: 2.0)

        let restored = SessionStore().load()
        let byId = Dictionary(uniqueKeysWithValues:
            restored.windows.map { ($0.id, $0) })

        XCTAssertEqual(
            byId["w1"]?.projects.first?.tabs.first?.claudeSessionId, "S1-NEW",
            "w1's latest in-memory id must survive flush"
        )
        XCTAssertEqual(
            byId["w2"]?.projects.first?.tabs.first?.claudeSessionId, "S2-NEW"
        )
        XCTAssertEqual(
            byId["w3"]?.projects.first?.tabs.first?.claudeSessionId, "S3-NEW"
        )
        XCTAssertEqual(restored.windows.count, 3)
    }

    func test_lateUpsertAfterFlush_doesNotResurrectStaleState() throws {
        // Sequence:
        //   1. upsert w1 (state A) — schedules debounce
        //   2. flush — writes A, cancels pending
        //   3. upsert w1 (state B) — schedules a new debounce
        //   4. flush again — writes B, cancels pending
        // The debounce from step 3 must not fire after step 4 with
        // stale (or doubly-written) state. Verifies the cancellation
        // path is symmetric across the same-window case.
        let store = SessionStore()

        store.upsert(window: makeWindow(id: "w1", tabSessionId: "A"))
        store.flush()

        store.upsert(window: makeWindow(id: "w1", tabSessionId: "B"))
        store.flush()

        let elapsed = XCTestExpectation(description: "debounce window elapsed")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) {
            elapsed.fulfill()
        }
        wait(for: [elapsed], timeout: 2.0)

        let restored = SessionStore().load()
        XCTAssertEqual(
            restored.windows.first?.projects.first?.tabs.first?.claudeSessionId,
            "B",
            "Latest flushed state must survive — no late debounce can revert it."
        )
    }

    // MARK: - helpers

    private func makeWindow(id: String, tabSessionId: String) -> PersistedWindow {
        let tab = PersistedTab(
            id: "tab-\(id)",
            title: "Tab",
            cwd: "/tmp",
            branch: nil,
            claudeSessionId: tabSessionId,
            activePaneId: "pane-\(id)",
            panes: [PersistedPane(id: "pane-\(id)", title: "Claude", kind: .claude)]
        )
        let project = PersistedProject(
            id: "project-\(id)", name: "Project \(id)",
            path: "/tmp", tabs: [tab]
        )
        return PersistedWindow(
            id: id, activeTabId: tab.id,
            sidebarCollapsed: false, projects: [project]
        )
    }
}
