//
//  NiceControlSocketTests.swift
//  NiceUnitTests
//
//  Verifies the self-healing behavior of NiceControlSocket: when the
//  accept DispatchSource is cancelled (fd error / OOM) or the socket
//  file is unlinked externally, the listener rebuilds itself at the
//  same path so the zsh `claude()` shadow keeps working without the
//  user noticing. `stop()` must not trigger that rebuild loop.
//
//  Tests talk to the socket with raw AF_UNIX `connect` rather than
//  shelling out to `nc`, so they're hermetic and don't depend on the
//  host's netcat variant.
//

import Darwin
import Foundation
import XCTest
@testable import Nice

final class NiceControlSocketTests: XCTestCase {

    // MARK: - Tests

    func test_restartsAfterAcceptSourceCancel() throws {
        // Long health-check so only the cancel path is under test.
        let socket = NiceControlSocket(
            healthCheckInterval: 60,
            initialRestartDelay: 0.02
        )
        try socket.start(handler: replyNewtabHandler)
        defer { socket.stop() }

        XCTAssertEqual(sendClaudeMessage(to: socket.path), "newtab",
                       "socket should respond before the forced cancel")

        socket._testForceCancelAcceptSource()

        let ok = waitFor(timeout: 2.0) {
            sendClaudeMessage(to: socket.path) == "newtab"
        }
        XCTAssertTrue(ok, "socket should self-heal after accept-source cancel")
    }

    func test_restartsWhenSocketFileRemoved() throws {
        let socket = NiceControlSocket(
            healthCheckInterval: 0.05,
            initialRestartDelay: 0.02
        )
        try socket.start(handler: replyNewtabHandler)
        defer { socket.stop() }

        XCTAssertEqual(sendClaudeMessage(to: socket.path), "newtab")

        XCTAssertEqual(unlink(socket.path), 0, "could not unlink socket for test")
        XCTAssertFalse(FileManager.default.fileExists(atPath: socket.path),
                       "precondition: socket file should be gone after unlink")

        let ok = waitFor(timeout: 2.0) {
            FileManager.default.fileExists(atPath: socket.path)
                && sendClaudeMessage(to: socket.path) == "newtab"
        }
        XCTAssertTrue(ok, "health check should rebuild the listener after the file is removed")
    }

    func test_stopPreventsRestart() throws {
        let socket = NiceControlSocket(
            healthCheckInterval: 0.05,
            initialRestartDelay: 0.02
        )
        try socket.start(handler: replyNewtabHandler)

        let path = socket.path
        XCTAssertEqual(sendClaudeMessage(to: path), "newtab")

        socket.stop()

        XCTAssertFalse(FileManager.default.fileExists(atPath: path),
                       "stop() should unlink the socket file")

        // If stop() failed to suppress restarts, the health check or
        // a pending cancel-handler dispatch would bring the file back.
        // Wait comfortably past several health-check intervals.
        Thread.sleep(forTimeInterval: 0.5)

        XCTAssertFalse(FileManager.default.fileExists(atPath: path),
                       "socket file must not reappear after stop()")
        XCTAssertNil(sendClaudeMessage(to: path),
                     "no listener should respond after stop()")
    }

    // MARK: - session_update parsing

    func test_sessionUpdate_dispatchesParsedFields() throws {
        let captured = CapturedSessionUpdates()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendRaw(
            to: socket.path,
            payload: #"{"action":"session_update","paneId":"P1","sessionId":"S1"}"#
        )

        let got = captured.waitForOne(timeout: 1.0)
        XCTAssertEqual(got?.paneId, "P1")
        XCTAssertEqual(got?.sessionId, "S1")
        XCTAssertNil(
            got?.source,
            "missing source field must surface as nil so SessionsModel treats it as id-only"
        )
    }

    func test_sessionUpdate_parsesSourceField() throws {
        // /branch reports source="resume". The parser must hand the
        // value through to the handler so SessionsModel can route the
        // rotation as a branch (resume + id-change) rather than a
        // plain id update.
        let captured = CapturedSessionUpdates()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendRaw(
            to: socket.path,
            payload: #"{"action":"session_update","paneId":"P1","sessionId":"S1","source":"resume"}"#
        )

        let got = captured.waitForOne(timeout: 1.0)
        XCTAssertEqual(got?.source, "resume")
    }

    func test_sessionUpdate_emptySourceNormalizesToNil() throws {
        // The hook script emits source="" when claude's payload omits
        // the field (sed regex falls through to empty). The parser
        // normalizes that to nil so handler-side comparisons stay
        // simple — `source == "resume"` shouldn't have to also exclude
        // the empty-string case.
        let captured = CapturedSessionUpdates()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendRaw(
            to: socket.path,
            payload: #"{"action":"session_update","paneId":"P1","sessionId":"S1","source":""}"#
        )

        let got = captured.waitForOne(timeout: 1.0)
        XCTAssertNil(got?.source)
    }

    func test_sessionUpdate_missingPaneId_dropsSilently() throws {
        let captured = CapturedSessionUpdates()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendRaw(
            to: socket.path,
            payload: #"{"action":"session_update","sessionId":"S1"}"#
        )
        // Give the dispatch path a beat to run if it were going to.
        Thread.sleep(forTimeInterval: 0.1)
        XCTAssertEqual(captured.count, 0)
    }

    func test_sessionUpdate_emptyStrings_dropsSilently() throws {
        let captured = CapturedSessionUpdates()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendRaw(
            to: socket.path,
            payload: #"{"action":"session_update","paneId":"","sessionId":""}"#
        )
        Thread.sleep(forTimeInterval: 0.1)
        XCTAssertEqual(captured.count, 0,
                       "empty paneId/sessionId must not dispatch")
    }

    func test_sessionUpdate_nonStringFields_dropsSilently() throws {
        let captured = CapturedSessionUpdates()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendRaw(
            to: socket.path,
            payload: #"{"action":"session_update","paneId":42,"sessionId":["S"]}"#
        )
        Thread.sleep(forTimeInterval: 0.1)
        XCTAssertEqual(captured.count, 0,
                       "non-string paneId/sessionId must not dispatch")
    }

    // MARK: - Cwd parsing
    //
    // The cwd field arrived in session_update payloads when the
    // SessionStart hook started forwarding Claude's actual working
    // directory — covers the bare `claude -w` (auto-named worktree)
    // path that `extractWorktreeName` can't predict, plus future
    // `/worktree` rotations. Parser contract mirrors the `source`
    // normalization: absent / empty / non-string → nil so the
    // downstream cwd-update short-circuit catches every "don't know"
    // variant without branching.

    func test_sessionUpdate_parsesCwdField() throws {
        let captured = CapturedSessionUpdates()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendRaw(
            to: socket.path,
            payload: #"{"action":"session_update","paneId":"P1","sessionId":"S1","cwd":"/Users/nick/Projects/notes/.claude/worktrees/foo"}"#
        )

        let got = captured.waitForOne(timeout: 1.0)
        XCTAssertEqual(
            got?.cwd,
            "/Users/nick/Projects/notes/.claude/worktrees/foo",
            "cwd field must arrive verbatim so SessionsModel can keep tab.cwd aligned"
        )
    }

    func test_sessionUpdate_missingCwd_isNil() throws {
        // The pre-cwd hook script (still on disk during an upgrade
        // window) omits the field entirely. Must surface as nil so
        // downstream comparisons can skip the cwd-update path cleanly.
        let captured = CapturedSessionUpdates()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendRaw(
            to: socket.path,
            payload: #"{"action":"session_update","paneId":"P1","sessionId":"S1"}"#
        )

        let got = captured.waitForOne(timeout: 1.0)
        XCTAssertNil(got?.cwd, "missing cwd key must arrive as nil")
    }

    func test_sessionUpdate_emptyCwdNormalizesToNil() throws {
        // The hook script emits cwd="" when Claude's payload omits the
        // field (sed regex falls through to empty). Parser collapses
        // the empty case to nil so the updateTabCwd helper can keep
        // its "nil → no-op" rule and not also have to special-case
        // empty strings.
        let captured = CapturedSessionUpdates()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendRaw(
            to: socket.path,
            payload: #"{"action":"session_update","paneId":"P1","sessionId":"S1","cwd":""}"#
        )

        let got = captured.waitForOne(timeout: 1.0)
        XCTAssertNil(got?.cwd, "empty cwd string must collapse to nil")
    }

    func test_sessionUpdate_nullCwdIsNil() throws {
        // Defensive: a hand-rolled hook (or a future Claude variant)
        // could ship `"cwd": null`. The `as? String` cast yields nil,
        // and the optional-chain plus emptiness guard keep that case
        // from crashing or dispatching a phantom update.
        let captured = CapturedSessionUpdates()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendRaw(
            to: socket.path,
            payload: #"{"action":"session_update","paneId":"P1","sessionId":"S1","cwd":null}"#
        )

        let got = captured.waitForOne(timeout: 1.0)
        XCTAssertNil(got?.cwd)
    }

    func test_sessionUpdate_nonStringCwdIsNil() throws {
        // Same defensive contract as `null` — a number or array in the
        // cwd slot fails the String cast and surfaces as nil. The
        // session_update itself still dispatches because paneId /
        // sessionId remain valid; only the cwd plumbing degrades.
        let captured = CapturedSessionUpdates()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendRaw(
            to: socket.path,
            payload: #"{"action":"session_update","paneId":"P1","sessionId":"S1","cwd":42}"#
        )

        let got = captured.waitForOne(timeout: 1.0)
        XCTAssertEqual(got?.paneId, "P1",
                       "non-string cwd must not block the session_update dispatch")
        XCTAssertNil(got?.cwd, "non-string cwd value must surface as nil")
    }

    func test_unknownAction_dropsSilently() throws {
        // Default branch in `readClient`'s switch — covered here
        // because it's the same parser surface as the new
        // session_update case and one bad-action test guards both.
        let captured = CapturedSessionUpdates()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendRaw(
            to: socket.path,
            payload: #"{"action":"frobnicate","x":"y"}"#
        )
        Thread.sleep(forTimeInterval: 0.1)
        XCTAssertEqual(captured.count, 0)
    }

    // MARK: - Helpers

    /// Handler that treats every `.claude` as a newtab and replies
    /// immediately. Matches the client-fd ownership model production
    /// relies on (`reply` closes the fd).
    private let replyNewtabHandler: NiceControlSocket.Handler = { message in
        switch message {
        case let .claude(_, _, _, _, reply):
            reply("newtab")
        case .sessionUpdate:
            // Fire-and-forget; not exercised by these tests.
            break
        }
    }

    /// Thread-safe collector for `.sessionUpdate` payloads dispatched
    /// to the test's socket handler. The socket fires its handler from
    /// a background queue, so a plain Swift array would race the test
    /// thread reading `count`.
    private final class CapturedSessionUpdates: @unchecked Sendable {
        struct Update {
            let paneId: String
            let sessionId: String
            let source: String?
            let cwd: String?
        }
        private let lock = NSLock()
        private var updates: [Update] = []
        private let signal = DispatchSemaphore(value: 0)

        var handler: NiceControlSocket.Handler {
            { [weak self] message in
                switch message {
                case let .sessionUpdate(paneId, sessionId, source, cwd):
                    self?.append(.init(
                        paneId: paneId,
                        sessionId: sessionId,
                        source: source,
                        cwd: cwd
                    ))
                case let .claude(_, _, _, _, reply):
                    // Tests don't exercise .claude here, but the handler
                    // must close the client fd via `reply` to match
                    // production's contract.
                    reply("newtab")
                }
            }
        }

        var count: Int {
            lock.lock(); defer { lock.unlock() }
            return updates.count
        }

        func waitForOne(timeout: TimeInterval) -> Update? {
            guard signal.wait(timeout: .now() + timeout) == .success else {
                return nil
            }
            lock.lock(); defer { lock.unlock() }
            return updates.first
        }

        private func append(_ u: Update) {
            lock.lock(); updates.append(u); lock.unlock()
            signal.signal()
        }
    }

    /// Send a raw newline-terminated payload to `path` and close the
    /// fd. Used for fire-and-forget messages like `session_update`
    /// where the socket doesn't write a reply.
    private func sendRaw(to path: String, payload: String) {
        let clientFd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard clientFd >= 0 else { return }
        defer { close(clientFd) }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let capacity = MemoryLayout.size(ofValue: addr.sun_path)
        withUnsafeMutablePointer(to: &addr.sun_path) { tuple in
            tuple.withMemoryRebound(to: CChar.self, capacity: capacity) { p in
                _ = path.withCString { src in
                    strncpy(p, src, capacity - 1)
                }
            }
        }
        let size = socklen_t(MemoryLayout<sockaddr_un>.size)
        let connectResult = withUnsafePointer(to: &addr) { ptr -> Int32 in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                connect(clientFd, sa, size)
            }
        }
        guard connectResult == 0 else { return }
        let line = payload + "\n"
        _ = line.withCString { p -> Int in
            write(clientFd, p, strlen(p))
        }
    }

    /// Connect to `path`, send a single `{"action":"claude",...}`
    /// payload, read one newline-terminated line, return its contents.
    /// Returns nil if any POSIX step fails (unreachable socket, closed
    /// connection, etc.) — that's the signal tests use to detect "not
    /// yet recovered."
    private func sendClaudeMessage(to path: String, timeout: TimeInterval = 0.5) -> String? {
        let clientFd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard clientFd >= 0 else { return nil }
        defer { close(clientFd) }

        var tv = timeval(
            tv_sec: Int(timeout),
            tv_usec: __darwin_suseconds_t((timeout - floor(timeout)) * 1_000_000)
        )
        _ = setsockopt(
            clientFd, SOL_SOCKET, SO_RCVTIMEO,
            &tv, socklen_t(MemoryLayout<timeval>.size)
        )
        _ = setsockopt(
            clientFd, SOL_SOCKET, SO_SNDTIMEO,
            &tv, socklen_t(MemoryLayout<timeval>.size)
        )

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let capacity = MemoryLayout.size(ofValue: addr.sun_path)
        withUnsafeMutablePointer(to: &addr.sun_path) { tuple in
            tuple.withMemoryRebound(to: CChar.self, capacity: capacity) { p in
                _ = path.withCString { src in
                    strncpy(p, src, capacity - 1)
                }
            }
        }

        let size = socklen_t(MemoryLayout<sockaddr_un>.size)
        let connectResult = withUnsafePointer(to: &addr) { ptr -> Int32 in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                connect(clientFd, sa, size)
            }
        }
        guard connectResult == 0 else { return nil }

        let payload = #"{"action":"claude","cwd":"/tmp","args":[],"tabId":"","paneId":""}"# + "\n"
        let written = payload.withCString { p -> Int in
            write(clientFd, p, strlen(p))
        }
        guard written > 0 else { return nil }

        var buffer = Data()
        var chunk = [UInt8](repeating: 0, count: 256)
        while buffer.count < 1024 {
            let n = chunk.withUnsafeMutableBufferPointer { buf -> Int in
                read(clientFd, buf.baseAddress, buf.count)
            }
            if n <= 0 { break }
            buffer.append(contentsOf: chunk[0..<n])
            if buffer.contains(0x0A) { break }
        }
        guard !buffer.isEmpty else { return nil }
        if let nl = buffer.firstIndex(of: 0x0A) {
            buffer = buffer.subdata(in: buffer.startIndex..<nl)
        }
        return String(data: buffer, encoding: .utf8)
    }

    /// Poll `condition` every 20ms up to `timeout`. Beats fixed sleeps
    /// for avoiding flake without slowing the happy path.
    private func waitFor(timeout: TimeInterval, condition: () -> Bool) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            Thread.sleep(forTimeInterval: 0.02)
        }
        return condition()
    }
}
