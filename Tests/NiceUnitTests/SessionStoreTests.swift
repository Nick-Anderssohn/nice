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

    // MARK: - threading + queue routing

    func test_debouncedWrite_runsOnIOQueueAfterDebounceWindow() {
        // Two contracts in one test, on purpose — they fail
        // together if you regress them together:
        //   1. The debounced write runs on `SessionStore.ioQueueLabel`
        //      (specifically that queue, not main, not some other
        //      background queue).
        //   2. It does *not* fire before the 500ms debounce window
        //      elapses. A regression that fired the write on main
        //      synchronously inside `upsert` would fail (1); a
        //      regression that dropped the asyncAfter and fired
        //      immediately on ioQueue would fail (2).
        let recorder = RecordingSessionStoreIO()
        let writeFired = expectation(description: "debounced writer invoked")
        recorder.onWrite = { writeFired.fulfill() }
        let store = SessionStore(io: recorder)
        let upsertedAt = Date()
        store.upsert(window: makeWindow(id: "w1", tabs: []))
        wait(for: [writeFired], timeout: 2.0)

        let writes = recorder.writes
        XCTAssertEqual(writes.count, 1)
        let write = try! XCTUnwrap(writes.first)
        XCTAssertEqual(write.queueLabel, SessionStore.ioQueueLabel,
                       "Debounced write must run on SessionStore.ioQueue.")
        XCTAssertGreaterThanOrEqual(
            write.firedAt.timeIntervalSince(upsertedAt), 0.4,
            "Debounced write must respect the 500ms debounce window (≥0.4s lower bound to absorb scheduling jitter)."
        )
    }

    func test_flush_runsWriteOnIOQueue_andBlocksUntilWriteCompletes() {
        // Two contracts:
        //   1. `flush()` runs the write on `ioQueue` (not main).
        //   2. `flush()` blocks the caller until the write actually
        //      completes — pinned by injecting a sleeping writer and
        //      asserting that `flush()` returned no earlier than the
        //      writer's `completedAt`. A regression that removed the
        //      semaphore would let `flush()` return before the writer
        //      finished its sleep, failing the timing assertion.
        let writerSleep: TimeInterval = 0.2
        let recorder = RecordingSessionStoreIO(writeDelay: writerSleep)
        let store = SessionStore(io: recorder)
        store.upsert(window: makeWindow(id: "w1", tabs: []))
        let beforeFlush = Date()
        store.flush()
        let afterFlush = Date()

        let writes = recorder.writes
        XCTAssertGreaterThanOrEqual(writes.count, 1)
        let lastWrite = try! XCTUnwrap(writes.last)
        XCTAssertEqual(lastWrite.queueLabel, SessionStore.ioQueueLabel,
                       "flush() must dispatch the write to ioQueue, not run it on main.")
        XCTAssertGreaterThanOrEqual(
            afterFlush.timeIntervalSince(beforeFlush), writerSleep - 0.05,
            "flush() must block until the off-main write completes — the call returned faster than the writer's injected sleep."
        )
        XCTAssertGreaterThanOrEqual(
            afterFlush, lastWrite.completedAt,
            "flush() returned before the last write completed; the semaphore is broken."
        )
    }

    func test_upsertThenFlush_writesExactlyOnce() {
        // A single upsert + flush must produce exactly one on-disk
        // write — the cancelled debounce work item must NOT also fire
        // a duplicate. Today's tests only check final content; this
        // pins write count, which catches "the cancellation is broken
        // but the writes happened to coalesce to the same content."
        let recorder = RecordingSessionStoreIO()
        let store = SessionStore(io: recorder)
        store.upsert(window: makeWindow(id: "w1", tabs: []))
        store.flush()
        // Spin past the debounce window: if the cancelled work item
        // fires anyway, we'd see a second write here.
        let elapsed = expectation(description: "debounce window elapsed")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.7) { elapsed.fulfill() }
        wait(for: [elapsed], timeout: 2.0)

        XCTAssertEqual(recorder.writes.count, 1,
                       "upsert + flush must write exactly once; a double-write means flush didn't cancel the debounce.")
    }

    func test_flushAfterInFlightDebounce_writesInOrder_latestSnapshotWins() {
        // Sequence:
        //   1. upsert(A) — schedules debounce on main.
        //   2. wait past debounce so the work item fires and
        //      enqueues write(A) onto ioQueue.
        //   3. immediately call upsert(B) + flush() — flush enqueues
        //      write(B); the serial ioQueue runs A first, then B.
        // Assert: two writes, A then B (in enqueue order), and the
        // final on-disk snapshot is B's.
        let recorder = RecordingSessionStoreIO(writeDelay: 0.05)
        let store = SessionStore(io: recorder)

        store.upsert(window: makeWindow(id: "w1", tabs: [makePersistedTab(id: "A")]))
        // Wait past debounce so write(A) is enqueued on ioQueue but
        // (because of writeDelay) hasn't completed yet.
        let debounceFired = expectation(description: "debounce timer fired")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.55) { debounceFired.fulfill() }
        wait(for: [debounceFired], timeout: 2.0)

        store.upsert(window: makeWindow(id: "w1", tabs: [makePersistedTab(id: "B")]))
        store.flush()

        let writes = recorder.writes
        XCTAssertEqual(writes.count, 2,
                       "Debounced write(A) plus flush write(B) — both must land.")
        let firstTabs = writes[0].snapshot.windows.first?.projects.flatMap(\.tabs) ?? []
        let secondTabs = writes[1].snapshot.windows.first?.projects.flatMap(\.tabs) ?? []
        XCTAssertEqual(firstTabs.map(\.id), ["A"], "ioQueue is serial: write(A) must complete first.")
        XCTAssertEqual(secondTabs.map(\.id), ["B"], "Then write(B) from flush.")
    }

    func test_rapidUpserts_coalesceToSingleWrite() {
        // Backpressure / pile-up: 100 rapid upserts must coalesce
        // through the debounce-cancellation path to a single write
        // when followed by a flush. A regression that broke
        // cancel-on-rescheduling would write up to 100 times.
        let recorder = RecordingSessionStoreIO()
        let store = SessionStore(io: recorder)
        for i in 0..<100 {
            store.upsert(window: makeWindow(id: "w\(i % 5)", tabs: []))
        }
        store.flush()

        XCTAssertEqual(recorder.writes.count, 1,
                       "100 rapid upserts + flush must coalesce to exactly one write.")
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

    func test_decodesFutureVersionWithUnknownFields_forwardCompat() throws {
        // A v4 file written by a newer Nice build (with extra fields
        // sprinkled at every level) must still decode under the current
        // v3 code so a user who downgrades doesn't lose their windows.
        // Codable's default behavior is "ignore unknown keys" — pin
        // that contract so a future migration that adopts a stricter
        // decoder has to think about backward compatibility.
        let json = #"""
        {
            "version": 4,
            "futureRoot": "ignore me",
            "windows": [{
                "id": "w1",
                "activeTabId": "t1",
                "sidebarCollapsed": false,
                "futureWindow": 42,
                "projects": [{
                    "id": "p1",
                    "name": "Project",
                    "path": "/tmp",
                    "futureProject": ["a", "b"],
                    "tabs": [{
                        "id": "t1",
                        "title": "Main",
                        "cwd": "/tmp",
                        "branch": null,
                        "claudeSessionId": "session-uuid",
                        "activePaneId": "pane-1",
                        "futureTab": {"nested": true},
                        "panes": [
                            {
                                "id": "pane-1",
                                "title": "Claude",
                                "kind": "claude",
                                "cwd": "/tmp",
                                "futurePane": "ignored"
                            }
                        ]
                    }]
                }]
            }]
        }
        """#
        let data = Data(json.utf8)
        let decoded = try JSONDecoder().decode(PersistedState.self, from: data)
        XCTAssertEqual(decoded.windows.count, 1)
        let window = try XCTUnwrap(decoded.windows.first)
        XCTAssertEqual(window.id, "w1")
        let tab = try XCTUnwrap(window.projects.first?.tabs.first)
        XCTAssertEqual(tab.claudeSessionId, "session-uuid",
                       "Forward-compat must preserve the v3 fields verbatim, not just survive the decode.")
        XCTAssertEqual(tab.panes.first?.kind, .claude)
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

/// Test double for `SessionStoreIO`. Records each `write` invocation
/// with its queue context and timestamps, and lets the test inject a
/// fixed sleep before the write returns (used to pin "flush blocks
/// until writer completes" without racing the scheduler).
///
/// `@unchecked Sendable` because synchronization is via `NSLock` —
/// the protocol requires `Sendable` for capture across the I/O queue.
final class RecordingSessionStoreIO: SessionStoreIO, @unchecked Sendable {
    struct WriteRecord {
        /// Snapshot the writer was invoked with.
        let snapshot: PersistedState
        /// Label of the dispatch queue the write ran on. Tests assert
        /// this is `SessionStore.ioQueueLabel`, never main.
        let queueLabel: String
        /// `Thread.isMainThread` at write time. Belt-and-suspenders
        /// alongside `queueLabel`.
        let isMain: Bool
        /// When the write closure was entered.
        let firedAt: Date
        /// When the write closure returned (after any injected
        /// `writeDelay`). Used by tests that pin `flush()`'s
        /// blocking semantics.
        let completedAt: Date
    }

    private let lock = NSLock()
    private var _writes: [WriteRecord] = []
    private let writeDelay: TimeInterval

    /// Optional callback fired (under the lock) after each write is
    /// recorded. Tests use this to fulfill an `XCTestExpectation` so
    /// they don't busy-wait.
    var onWrite: (@Sendable () -> Void)?

    init(writeDelay: TimeInterval = 0) {
        self.writeDelay = writeDelay
    }

    func write(_ state: PersistedState, to url: URL) {
        let firedAt = Date()
        let label = String(cString: __dispatch_queue_get_label(nil))
        let isMain = Thread.isMainThread
        if writeDelay > 0 {
            Thread.sleep(forTimeInterval: writeDelay)
        }
        let completedAt = Date()
        lock.lock()
        _writes.append(WriteRecord(
            snapshot: state,
            queueLabel: label,
            isMain: isMain,
            firedAt: firedAt,
            completedAt: completedAt
        ))
        let cb = onWrite
        lock.unlock()
        cb?()
    }

    func read(from url: URL) -> PersistedState {
        return .empty
    }

    var writes: [WriteRecord] {
        lock.lock(); defer { lock.unlock() }
        return _writes
    }
}
