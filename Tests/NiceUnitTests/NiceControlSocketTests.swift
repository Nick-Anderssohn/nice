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

    // MARK: - Helpers

    /// Handler that treats every `.claude` as a newtab and replies
    /// immediately. Matches the client-fd ownership model production
    /// relies on (`reply` closes the fd).
    private let replyNewtabHandler: NiceControlSocket.Handler = { message in
        switch message {
        case let .claude(_, _, _, _, reply):
            reply("newtab")
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
