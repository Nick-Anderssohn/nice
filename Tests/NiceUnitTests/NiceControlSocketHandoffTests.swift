//
//  NiceControlSocketHandoffTests.swift
//  NiceUnitTests
//
//  Pins the `handoff` case in `NiceControlSocket.readClient`. The parser
//  must:
//    • dispatch `.handoff` with all fields correct when both required
//      fields (`cwd` and `handoffFile`) are present.
//    • drop the connection without dispatching when either required field
//      is missing or empty (same silent-drop contract as the other cases).
//    • normalize absent / empty `instructions`, `tabId`, `paneId`, `model`,
//      and `effort` to `""` so the handler never sees nil for the
//      non-optional String fields. `model`/`effort` are additionally a
//      back-compat surface: an older installed helper omits them, and the
//      request must still dispatch (not drop).
//
//  Test style mirrors NiceControlSocketTests: raw POSIX AF_UNIX connects,
//  a thread-safe collector for dispatched messages, and the same
//  `sendRaw` / `waitFor` helpers. The `handoff` case replies with one
//  line ("ok" / "error: ..."), so the tests also drain the reply to keep
//  the server's write from hitting SIGPIPE.
//

import Darwin
import Foundation
import XCTest
@testable import Nice

final class NiceControlSocketHandoffTests: XCTestCase {

    // MARK: - Valid payload — all fields present

    func test_handoff_validPayloadWithInstructions_dispatchesAllFields() throws {
        let captured = CapturedHandoffs()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendHandoff(
            to: socket.path,
            payload: #"""
            {"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"tab1","paneId":"pane1","instructions":"Focus only on the UI layer","model":"claude-opus-4-8","effort":"xhigh"}
            """#
        )

        let got = captured.waitForOne(timeout: 1.0)
        XCTAssertNotNil(got, "handoff with all fields must dispatch to handler")
        XCTAssertEqual(got?.cwd, "/tmp/work")
        XCTAssertEqual(got?.handoffFile, "/tmp/work/.claude/handoff/h.md")
        XCTAssertEqual(got?.instructions, "Focus only on the UI layer")
        XCTAssertEqual(got?.tabId, "tab1")
        XCTAssertEqual(got?.paneId, "pane1")
        XCTAssertEqual(got?.model, "claude-opus-4-8")
        XCTAssertEqual(got?.effort, "xhigh")
    }

    func test_handoff_validPayload_replyIsOk() throws {
        // The handler calls reply("ok") — the parser must write that reply
        // so the helper script can confirm success. Drain the reply line
        // and check its content.
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: { message in
            if case let .handoff(_, _, _, _, _, _, _, reply) = message {
                reply("ok")
            }
        })
        defer { socket.stop() }

        let reply = sendHandoffAndReadReply(
            to: socket.path,
            payload: #"""
            {"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"t1","paneId":"p1","instructions":""}
            """#
        )
        XCTAssertEqual(reply, "ok",
                       "handler's reply(\"ok\") must be written back to the client")
    }

    // MARK: - Required-field validation — missing or empty fields dropped

    func test_handoff_missingCwd_dropsSilently() throws {
        // `cwd` is required. Missing it must close the connection without
        // dispatching — same contract as the `claude` case dropping on
        // missing `cwd`.
        let captured = CapturedHandoffs()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendRaw(
            to: socket.path,
            payload: #"{"action":"handoff","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"t1","paneId":"p1"}"#
        )
        Thread.sleep(forTimeInterval: 0.1)
        XCTAssertEqual(captured.count, 0, "missing cwd must drop the message")
    }

    func test_handoff_missingHandoffFile_dropsSilently() throws {
        // `handoffFile` is required — no notes path means no seeding.
        let captured = CapturedHandoffs()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendRaw(
            to: socket.path,
            payload: #"{"action":"handoff","cwd":"/tmp/work","tabId":"t1","paneId":"p1"}"#
        )
        Thread.sleep(forTimeInterval: 0.1)
        XCTAssertEqual(captured.count, 0, "missing handoffFile must drop the message")
    }

    func test_handoff_emptyCwd_dropsSilently() throws {
        let captured = CapturedHandoffs()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendRaw(
            to: socket.path,
            payload: #"{"action":"handoff","cwd":"","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"t1","paneId":"p1"}"#
        )
        Thread.sleep(forTimeInterval: 0.1)
        XCTAssertEqual(captured.count, 0, "empty cwd must drop the message")
    }

    func test_handoff_emptyHandoffFile_dropsSilently() throws {
        let captured = CapturedHandoffs()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendRaw(
            to: socket.path,
            payload: #"{"action":"handoff","cwd":"/tmp/work","handoffFile":"","tabId":"t1","paneId":"p1"}"#
        )
        Thread.sleep(forTimeInterval: 0.1)
        XCTAssertEqual(captured.count, 0, "empty handoffFile must drop the message")
    }

    // MARK: - Optional field normalization

    func test_handoff_absentInstructions_normalizesToEmptyString() throws {
        // `instructions` is absent — the parser must normalize to "" so
        // the handler's `trimmed.isEmpty` branch works without an optional.
        let captured = CapturedHandoffs()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendHandoff(
            to: socket.path,
            payload: #"{"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"t1","paneId":"p1"}"#
        )

        let got = captured.waitForOne(timeout: 1.0)
        XCTAssertEqual(got?.instructions, "",
                       "absent instructions must surface as \"\"")
    }

    func test_handoff_emptyInstructions_normalizesToEmptyString() throws {
        // Skill sends explicit empty string when user provided no extra text.
        let captured = CapturedHandoffs()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendHandoff(
            to: socket.path,
            payload: #"{"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"t1","paneId":"p1","instructions":""}"#
        )

        let got = captured.waitForOne(timeout: 1.0)
        XCTAssertEqual(got?.instructions, "",
                       "empty instructions string must surface as \"\"")
    }

    func test_handoff_absentTabId_normalizesToEmptyString() throws {
        // An absent tabId routes to a top-level tab in the handler —
        // normalized to "" so the same empty-string check works everywhere.
        let captured = CapturedHandoffs()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendHandoff(
            to: socket.path,
            payload: #"{"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","paneId":"p1"}"#
        )

        let got = captured.waitForOne(timeout: 1.0)
        XCTAssertEqual(got?.tabId, "",
                       "absent tabId must normalize to \"\"")
    }

    func test_handoff_absentPaneId_normalizesToEmptyString() throws {
        let captured = CapturedHandoffs()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendHandoff(
            to: socket.path,
            payload: #"{"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"t1"}"#
        )

        let got = captured.waitForOne(timeout: 1.0)
        XCTAssertEqual(got?.paneId, "",
                       "absent paneId must normalize to \"\"")
    }

    // MARK: - model / effort parsing

    func test_handoff_modelAndEffortPresent_surfaceVerbatim() throws {
        let captured = CapturedHandoffs()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendHandoff(
            to: socket.path,
            payload: #"{"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"t1","paneId":"p1","model":"claude-sonnet-4-6","effort":"max"}"#
        )

        let got = captured.waitForOne(timeout: 1.0)
        XCTAssertEqual(got?.model, "claude-sonnet-4-6")
        XCTAssertEqual(got?.effort, "max")
    }

    func test_handoff_absentModelAndEffort_dispatchesWithEmptyStrings() throws {
        // Back-compat: an older installed nice-handoff.sh omits both fields.
        // The request must still dispatch (cwd/handoffFile are the only
        // required fields), with model/effort normalized to "".
        let captured = CapturedHandoffs()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendHandoff(
            to: socket.path,
            payload: #"{"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"t1","paneId":"p1"}"#
        )

        let got = captured.waitForOne(timeout: 1.0)
        XCTAssertNotNil(got, "a payload without model/effort must still dispatch")
        XCTAssertEqual(got?.model, "", "absent model must normalize to \"\"")
        XCTAssertEqual(got?.effort, "", "absent effort must normalize to \"\"")
    }

    func test_handoff_emptyModelAndEffort_normalizeToEmptyStrings() throws {
        // The helper sends explicit empty strings when the model is unknown
        // and CLAUDE_EFFORT is unset — same surface as absent.
        let captured = CapturedHandoffs()
        let socket = NiceControlSocket(
            healthCheckInterval: 60, initialRestartDelay: 0.02
        )
        try socket.start(handler: captured.handler)
        defer { socket.stop() }

        sendHandoff(
            to: socket.path,
            payload: #"{"action":"handoff","cwd":"/tmp/work","handoffFile":"/tmp/work/.claude/handoff/h.md","tabId":"t1","paneId":"p1","model":"","effort":""}"#
        )

        let got = captured.waitForOne(timeout: 1.0)
        XCTAssertEqual(got?.model, "")
        XCTAssertEqual(got?.effort, "")
    }

    // MARK: - Thread-safe collector

    /// Thread-safe collector for `.handoff` payloads dispatched to the
    /// test's socket handler. Mirrors `CapturedSessionUpdates` in
    /// NiceControlSocketTests.
    private final class CapturedHandoffs: @unchecked Sendable {
        struct Handoff {
            let cwd: String
            let handoffFile: String
            let instructions: String
            let model: String
            let effort: String
            let tabId: String
            let paneId: String
        }
        private let lock = NSLock()
        private var items: [Handoff] = []
        private let signal = DispatchSemaphore(value: 0)

        var handler: NiceControlSocket.Handler {
            { [weak self] message in
                switch message {
                case let .handoff(cwd, handoffFile, instructions, model, effort, tabId, paneId, reply):
                    reply("ok")
                    self?.append(.init(
                        cwd: cwd,
                        handoffFile: handoffFile,
                        instructions: instructions,
                        model: model,
                        effort: effort,
                        tabId: tabId,
                        paneId: paneId
                    ))
                case let .claude(_, _, _, _, reply):
                    reply("newtab")
                case .sessionUpdate:
                    break
                }
            }
        }

        var count: Int {
            lock.lock(); defer { lock.unlock() }
            return items.count
        }

        func waitForOne(timeout: TimeInterval) -> Handoff? {
            guard signal.wait(timeout: .now() + timeout) == .success else {
                return nil
            }
            lock.lock(); defer { lock.unlock() }
            return items.first
        }

        private func append(_ h: Handoff) {
            lock.lock(); items.append(h); lock.unlock()
            signal.signal()
        }
    }

    // MARK: - Helpers

    /// Send a raw newline-terminated payload and close the fd immediately
    /// (fire-and-forget). For cases where no reply is expected (drop
    /// paths) or where the caller doesn't need the reply.
    private func sendRaw(to path: String, payload: String) {
        let fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return }
        defer { close(fd) }
        suppressSIGPIPE(on: fd)
        guard connectUnix(fd: fd, path: path) else { return }
        let line = payload + "\n"
        _ = line.withCString { p in write(fd, p, strlen(p)) }
    }

    /// Send a handoff payload and keep the fd open long enough to drain
    /// the reply. The handler replies "ok" before closing the fd; we must
    /// drain to avoid SIGPIPE in the server on the write.
    private func sendHandoff(to path: String, payload: String) {
        _ = sendHandoffAndReadReply(to: path, payload: payload)
    }

    private func sendHandoffAndReadReply(
        to path: String, payload: String, timeout: TimeInterval = 0.5
    ) -> String? {
        let fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return nil }
        defer { close(fd) }
        suppressSIGPIPE(on: fd)

        var tv = timeval(
            tv_sec: Int(timeout),
            tv_usec: __darwin_suseconds_t((timeout - floor(timeout)) * 1_000_000)
        )
        _ = setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv,
                       socklen_t(MemoryLayout<timeval>.size))
        _ = setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv,
                       socklen_t(MemoryLayout<timeval>.size))

        guard connectUnix(fd: fd, path: path) else { return nil }

        let line = payload + "\n"
        let written = line.withCString { p in write(fd, p, strlen(p)) }
        guard written > 0 else { return nil }

        // Drain the reply line.
        var buffer = Data()
        var chunk = [UInt8](repeating: 0, count: 256)
        while buffer.count < 1024 {
            let n = chunk.withUnsafeMutableBufferPointer { buf -> Int in
                read(fd, buf.baseAddress, buf.count)
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

    private func connectUnix(fd: Int32, path: String) -> Bool {
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let capacity = MemoryLayout.size(ofValue: addr.sun_path)
        withUnsafeMutablePointer(to: &addr.sun_path) { tuple in
            tuple.withMemoryRebound(to: CChar.self, capacity: capacity) { p in
                _ = path.withCString { src in strncpy(p, src, capacity - 1) }
            }
        }
        let size = socklen_t(MemoryLayout<sockaddr_un>.size)
        return withUnsafePointer(to: &addr) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                connect(fd, sa, size) == 0
            }
        }
    }

    private func suppressSIGPIPE(on fd: Int32) {
        var one: Int32 = 1
        _ = setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE,
                       &one, socklen_t(MemoryLayout<Int32>.size))
    }
}
