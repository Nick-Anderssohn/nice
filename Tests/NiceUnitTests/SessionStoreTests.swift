//
//  SessionStoreTests.swift
//  NiceUnitTests
//
//  Exercises the `SessionStore` persistence layer: upsert/replace,
//  debounce/flush, empty-window pruning, version-mismatch handling,
//  and JSON round-trip preservation. Isolated via
//  `NICE_APPLICATION_SUPPORT_ROOT` so the tests never read or clobber
//  the user's real `~/Library/Application Support/Nice/sessions.json`.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class SessionStoreTests: XCTestCase {

    private var supportRoot: URL!
    private var originalAppSupport: String?

    override func setUp() {
        super.setUp()
        supportRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-sessionstore-\(UUID().uuidString)", isDirectory: true
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

    private var sessionsFile: URL {
        supportRoot.appendingPathComponent("Nice/sessions.json")
    }

    // MARK: - init / load

    func test_init_onMissingFile_loadReturnsEmpty() {
        let store = SessionStore()
        XCTAssertEqual(store.load().windows, [])
        XCTAssertEqual(store.load().version, PersistedState.currentVersion)
    }

    func test_init_onCorruptFile_loadReturnsEmptyWithoutCrash() throws {
        try FileManager.default.createDirectory(
            at: sessionsFile.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try Data("not json {{".utf8).write(to: sessionsFile)

        let store = SessionStore()
        XCTAssertEqual(store.load().windows, [],
                       "A corrupt sessions.json must decode to empty so the app still launches.")
    }

    func test_init_onShapeMismatch_discardsPayload() throws {
        // A file whose top-level shape can't satisfy PersistedState's
        // required fields (e.g. the whole `version` key missing) must
        // fall through to empty so a bad save doesn't make the app
        // unlaunchable. (The current decoder doesn't gate on the
        // `version` value itself — the only real guard is "did the
        // struct decode at all" — so this covers the practical failure
        // mode, not the aspirational version-check in the comments.)
        try FileManager.default.createDirectory(
            at: sessionsFile.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        let malformed = #"{"windows":"not-an-array"}"#
        try Data(malformed.utf8).write(to: sessionsFile)

        let store = SessionStore()
        XCTAssertEqual(store.load().windows, [],
                       "A structurally invalid payload must decode to empty rather than crashing the launch.")
    }

    // MARK: - upsert

    func test_upsert_appendsNewWindow() {
        let store = SessionStore()
        let window = makeWindow(id: "w1", tabs: [])
        store.upsert(window: window)
        store.flush()

        XCTAssertEqual(store.load().windows.count, 1)
        XCTAssertEqual(store.load().windows.first?.id, "w1")
    }

    func test_upsert_replacesExistingWindowById() {
        let store = SessionStore()
        store.upsert(window: makeWindow(id: "w1", tabs: [makePersistedTab(id: "t1")]))
        store.flush()

        store.upsert(window: makeWindow(id: "w1", tabs: [
            makePersistedTab(id: "t1"),
            makePersistedTab(id: "t2"),
        ]))
        store.flush()

        let windows = store.load().windows
        XCTAssertEqual(windows.count, 1,
                       "Upsert must replace by id, not append a duplicate.")
        XCTAssertEqual(windows.first?.projects.flatMap(\.tabs).count, 2)
    }

    func test_upsert_multipleWindows_coexistByDistinctId() {
        let store = SessionStore()
        store.upsert(window: makeWindow(id: "w1", tabs: [makePersistedTab(id: "t1")]))
        store.upsert(window: makeWindow(id: "w2", tabs: [makePersistedTab(id: "t2")]))
        store.flush()

        let ids = store.load().windows.map(\.id).sorted()
        XCTAssertEqual(ids, ["w1", "w2"])
    }

    // MARK: - flush

    func test_flush_writesSynchronously() throws {
        let store = SessionStore()
        store.upsert(window: makeWindow(id: "w1", tabs: []))
        // Before flush, the file may or may not exist yet — the
        // debounce hasn't fired. After flush, it MUST exist with the
        // correct content.
        store.flush()

        XCTAssertTrue(
            FileManager.default.fileExists(atPath: sessionsFile.path),
            "flush() must write the file synchronously so willTerminate never loses state.")
        let freshStore = SessionStore()
        XCTAssertEqual(freshStore.load().windows.first?.id, "w1",
                       "flushed state must be readable by a new store instance.")
    }

    func test_flush_afterUpsert_persistsLatestStateForSameWindow() throws {
        // Upsert a window, flush, upsert the *same* window with new
        // tabs, flush again. A new SessionStore instance must see the
        // second upsert's tabs — this exercises the full
        // "upsert + scheduleWrite + flush cancels pending" loop without
        // relying on private internals. If flush didn't cancel the
        // prior pending work, a stale in-flight write could clobber
        // the second flush after the debounce fired.
        let store = SessionStore()
        store.upsert(window: makeWindow(id: "w1", tabs: [makePersistedTab(id: "t1")]))
        store.flush()
        store.upsert(window: makeWindow(id: "w1", tabs: [
            makePersistedTab(id: "t1"), makePersistedTab(id: "t2"),
        ]))
        store.flush()

        // Spin past the debounce window to give any stale cancelled
        // work item a chance to fire. If the cancellation is broken,
        // the file gets rewritten with the pre-second-flush state.
        let expectation = XCTestExpectation(description: "debounce window elapsed")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) {
            expectation.fulfill()
        }
        wait(for: [expectation], timeout: 2.0)

        let freshStore = SessionStore()
        XCTAssertEqual(freshStore.load().windows.count, 1)
        let tabs = freshStore.load().windows.first?.projects.flatMap(\.tabs) ?? []
        XCTAssertEqual(tabs.count, 2,
                       "After the second flush, the latest state must survive — a pending cancelled write must not resurrect the first upsert's state.")
    }

    // MARK: - pruneEmptyWindows

    func test_pruneEmptyWindows_dropsZeroTabWindowsExceptKeep() {
        let store = SessionStore()
        store.upsert(window: makeWindow(id: "keep", tabs: []))
        store.upsert(window: makeWindow(id: "empty", tabs: []))
        store.upsert(window: makeWindow(id: "full", tabs: [makePersistedTab(id: "t1")]))
        store.flush()

        store.pruneEmptyWindows(keeping: "keep")
        store.flush()

        let ids = store.load().windows.map(\.id).sorted()
        XCTAssertEqual(ids, ["full", "keep"],
                       "pruneEmptyWindows keeps the caller's slot and anything with tabs; drops the rest.")
    }

    func test_pruneEmptyWindows_isNoOpWhenNothingToPrune() {
        let store = SessionStore()
        store.upsert(window: makeWindow(id: "w1", tabs: [makePersistedTab(id: "t1")]))
        store.flush()
        let before = store.load()

        store.pruneEmptyWindows(keeping: "w1")
        store.flush()

        XCTAssertEqual(store.load(), before,
                       "pruning when all windows are non-empty must not write.")
    }

    // MARK: - round-trip

    func test_roundTrip_preservesEveryField() throws {
        let panes = [
            PersistedPane(id: "p1", title: "Claude", kind: .claude),
            PersistedPane(id: "p2", title: "zsh", kind: .terminal),
        ]
        let tab = PersistedTab(
            id: "t1",
            title: "Fix top bar height",
            cwd: "/Users/nick/Projects/nice",
            branch: "main",
            claudeSessionId: "e4f1a2b3-c0d4-4e5f-9a0b-1c2d3e4f5a6b",
            activePaneId: "p1",
            panes: panes
        )
        let project = PersistedProject(
            id: "nice", name: "Nice", path: "/Users/nick/Projects/nice", tabs: [tab]
        )
        let window = PersistedWindow(
            id: "w1", activeTabId: "t1", sidebarCollapsed: true, projects: [project]
        )

        do {
            let store = SessionStore()
            store.upsert(window: window)
            store.flush()
        }

        let restored = SessionStore().load().windows.first
        XCTAssertEqual(restored, window,
                       "Encoder + decoder must preserve every persisted field verbatim.")
    }

    func test_roundTrip_preservesNilOptionals() throws {
        // Terminal-only tabs carry `claudeSessionId == nil`; the
        // v2 → v3 bump was precisely to make that optional survive.
        let tab = PersistedTab(
            id: "t1",
            title: "Main",
            cwd: "/tmp",
            branch: nil,
            claudeSessionId: nil,
            activePaneId: nil,
            panes: []
        )
        let window = PersistedWindow(
            id: "w1", activeTabId: nil, sidebarCollapsed: false,
            projects: [PersistedProject(id: "terminals", name: "Terminals", path: "/tmp", tabs: [tab])]
        )
        do {
            let store = SessionStore()
            store.upsert(window: window)
            store.flush()
        }
        let restored = SessionStore().load().windows.first
        XCTAssertEqual(restored, window)
    }

    // MARK: - Per-pane cwd round-trip

    func test_persistedPane_roundTripsCwd() throws {
        // Per-pane cwd is what restores split-pane fidelity — each
        // pane comes back where the user left it.
        let panes = [
            PersistedPane(id: "p1", title: "zsh", kind: .terminal, cwd: "/usr"),
            PersistedPane(id: "p2", title: "zsh", kind: .terminal, cwd: "/var/log"),
        ]
        let tab = PersistedTab(
            id: "t1", title: "Splits", cwd: "/tmp", branch: nil,
            claudeSessionId: nil, activePaneId: "p1", panes: panes
        )
        let window = makeWindow(id: "w1", tabs: [tab])

        do {
            let store = SessionStore()
            store.upsert(window: window)
            store.flush()
        }
        let restored = SessionStore().load().windows.first
        let restoredPanes = restored?.projects.first?.tabs.first?.panes
        XCTAssertEqual(restoredPanes?.map(\.cwd), ["/usr", "/var/log"],
                       "Per-pane cwd must survive save → load round-trip.")
    }

    func test_persistedPane_decodesWithoutCwdField_backwardsCompat() throws {
        // Sessions written by Nice builds before per-pane cwd
        // existed don't have a `cwd` key on each pane. The decoder
        // must tolerate that — Codable's optional-property synthesis
        // does the right thing, but we lock the behavior in so a
        // future migration that drops the optionality is forced to
        // think about it.
        let json = #"""
        {
            "version": 3,
            "windows": [{
                "id": "w1",
                "activeTabId": "t1",
                "sidebarCollapsed": false,
                "projects": [{
                    "id": "terminals",
                    "name": "Terminals",
                    "path": "/tmp",
                    "tabs": [{
                        "id": "t1",
                        "title": "Main",
                        "cwd": "/Users/nick",
                        "branch": null,
                        "claudeSessionId": null,
                        "activePaneId": "p1",
                        "panes": [
                            {"id": "p1", "title": "zsh", "kind": "terminal"}
                        ]
                    }]
                }]
            }]
        }
        """#
        let data = Data(json.utf8)
        let decoded = try JSONDecoder().decode(PersistedState.self, from: data)
        let pane = decoded.windows.first?.projects.first?.tabs.first?.panes.first
        XCTAssertEqual(pane?.id, "p1")
        XCTAssertNil(pane?.cwd, "Missing cwd field must decode as nil, not crash.")
    }

    // MARK: - helpers

    private func makeWindow(id: String, tabs: [PersistedTab]) -> PersistedWindow {
        let project = PersistedProject(
            id: "p", name: "Project", path: "/tmp", tabs: tabs
        )
        return PersistedWindow(
            id: id, activeTabId: tabs.first?.id,
            sidebarCollapsed: false,
            projects: tabs.isEmpty ? [] : [project]
        )
    }

    private func makePersistedTab(id: String) -> PersistedTab {
        PersistedTab(
            id: id, title: id, cwd: "/tmp", branch: nil,
            claudeSessionId: nil, activePaneId: nil,
            panes: []
        )
    }
}
