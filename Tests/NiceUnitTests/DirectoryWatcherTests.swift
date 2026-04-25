//
//  DirectoryWatcherTests.swift
//  NiceUnitTests
//
//  End-to-end coverage for `DirectoryWatcher` against a real temp
//  directory: kqueue source mask, debounce, cancel-handler FD close,
//  and the "calling start() again replaces any prior watch" contract.
//
//  These tests are the integration seam between the file browser's
//  per-row watcher and the actual filesystem. A regression here —
//  e.g. dropping `.write` from the event mask, switching the queue
//  off `.main`, or losing the defensive `stop()` inside `start()` —
//  would make the sidebar silently stop reflecting external file
//  changes. Unit tests on the row's view code can't catch any of
//  that.
//
//  Timing notes:
//    • `DirectoryWatcher.scheduleDebounced` waits 120ms before
//      invoking the user's `onChange`. Tests use a generous ~1s
//      timeout to absorb CI variance.
//    • Tests run on @MainActor; the watcher's source is bound to
//      `.main`, so the dispatch hop happens on the same queue and
//      `XCTestExpectation.wait(for:timeout:)` drains it correctly.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class DirectoryWatcherTests: XCTestCase {

    private var tempDir: URL!
    private var watcher: DirectoryWatcher!

    override func setUpWithError() throws {
        tempDir = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-watcher-test-\(UUID().uuidString)",
                isDirectory: true
            )
        try FileManager.default.createDirectory(
            at: tempDir, withIntermediateDirectories: true
        )
        watcher = DirectoryWatcher()
    }

    override func tearDownWithError() throws {
        watcher?.stop()
        watcher = nil
        if let tempDir {
            try? FileManager.default.removeItem(at: tempDir)
        }
        tempDir = nil
    }

    // MARK: - Real filesystem → callback

    func test_creatingFile_firesCallback() throws {
        let fired = expectation(description: "watcher fires on file create")
        watcher.start(path: tempDir.path) { fired.fulfill() }

        try touchFile("new.txt")

        wait(for: [fired], timeout: 1.0)
    }

    func test_deletingFile_firesCallback() throws {
        let url = try touchFile("doomed.txt")
        let fired = expectation(description: "watcher fires on file delete")
        watcher.start(path: tempDir.path) { fired.fulfill() }

        try FileManager.default.removeItem(at: url)

        wait(for: [fired], timeout: 1.0)
    }

    func test_renamingFile_firesCallback() throws {
        let url = try touchFile("before.txt")
        let fired = expectation(description: "watcher fires on rename")
        watcher.start(path: tempDir.path) { fired.fulfill() }

        try FileManager.default.moveItem(
            at: url,
            to: tempDir.appendingPathComponent("after.txt")
        )

        wait(for: [fired], timeout: 1.0)
    }

    // MARK: - Debounce

    /// A burst of writes within the 120ms debounce window must
    /// coalesce to a single `onChange` call. Otherwise an editor's
    /// save-with-multiple-syscalls would trigger a flurry of
    /// reloads.
    func test_burstOfChangesWithinDebounceWindow_firesCallbackOnce() throws {
        let fired = expectation(description: "debounced callback fires exactly once")
        fired.assertForOverFulfill = true

        watcher.start(path: tempDir.path) { fired.fulfill() }

        // Three writes well within the 120ms window.
        try touchFile("a.txt")
        try touchFile("b.txt")
        try touchFile("c.txt")

        wait(for: [fired], timeout: 1.0)
    }

    // MARK: - stop()

    func test_stop_preventsCallbackOnSubsequentMutation() throws {
        var callbackCount = 0
        watcher.start(path: tempDir.path) { callbackCount += 1 }
        watcher.stop()

        try touchFile("after-stop.txt")

        // Run the main loop briefly so any in-flight dispatch can
        // drain. If `stop()` is broken, the count goes to 1.
        let drained = expectation(description: "main loop drained")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { drained.fulfill() }
        wait(for: [drained], timeout: 1.0)

        XCTAssertEqual(callbackCount, 0,
                       "stop() must cancel the source and prevent the callback from firing on later mutations.")
    }

    // MARK: - Idempotent start

    /// Calling `start(path: B, ...)` after `start(path: A, ...)` must
    /// replace the watch — mutations to A should NOT fire after the
    /// second start. Catches a regression where the defensive
    /// `stop()` at the top of `start` is dropped.
    func test_secondStart_replacesPriorWatch() throws {
        let dirA = tempDir.appendingPathComponent("A", isDirectory: true)
        let dirB = tempDir.appendingPathComponent("B", isDirectory: true)
        try FileManager.default.createDirectory(at: dirA, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: dirB, withIntermediateDirectories: true)

        var aFires = 0
        var bFires = 0
        watcher.start(path: dirA.path) { aFires += 1 }
        watcher.start(path: dirB.path) { bFires += 1 }

        // Mutate B; expect bFires to go up. Mutate A; expect aFires
        // to stay at 0 because the second start replaced the watch.
        let bFired = expectation(description: "B's watcher fires")
        watcher.stop() // capture into a fresh start to attach the expectation
        watcher.start(path: dirB.path) {
            bFires += 1
            bFired.fulfill()
        }
        FileManager.default.createFile(
            atPath: dirB.appendingPathComponent("b1.txt").path,
            contents: Data()
        )
        wait(for: [bFired], timeout: 1.0)

        // Now mutate A and confirm A's old callback is dead.
        FileManager.default.createFile(
            atPath: dirA.appendingPathComponent("a1.txt").path,
            contents: Data()
        )
        let drained = expectation(description: "main loop drained for A check")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { drained.fulfill() }
        wait(for: [drained], timeout: 1.0)

        XCTAssertEqual(aFires, 0,
                       "A's callback must be replaced by B's; mutating A after the swap should be silent.")
    }

    // MARK: - Bad input

    func test_startOnMissingPath_doesNotCrash() {
        let nope = tempDir.appendingPathComponent("does-not-exist", isDirectory: true)
        // open(O_EVTONLY) on a missing path returns -1; the guard
        // inside start() must silently skip without crashing.
        watcher.start(path: nope.path) { }
        // No assertion — the contract is "no crash, no callback,
        // safe to teardown." The teardown in tearDownWithError will
        // exercise stop() against a watcher that never started.
    }

    // MARK: - Helpers

    @discardableResult
    private func touchFile(_ name: String) throws -> URL {
        let url = tempDir.appendingPathComponent(name)
        FileManager.default.createFile(atPath: url.path, contents: Data())
        return url
    }
}
