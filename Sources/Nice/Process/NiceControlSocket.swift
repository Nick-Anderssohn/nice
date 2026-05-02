//
//  NiceControlSocket.swift
//  Nice
//
//  Phase 7: a tiny Unix-domain-socket listener that lets the Main
//  Terminal's shadowed `claude` zsh function ask the app to open a new
//  tab. One newline-delimited JSON message per client, then close.
//
//  Raw POSIX (`socket`/`bind`/`listen`/`accept` + `DispatchSource`) is
//  used rather than `NWListener` with `NWEndpoint.unix(path:)` because
//  (a) the payload is a single short line per connection, so
//  NWConnection's buffering/state machine adds no value, and (b) we
//  want precise control over `unlink` before bind and `chmod 0600`
//  after bind.
//
//  The class is deliberately *not* `@MainActor`. DispatchSource event
//  handlers fire on a background queue; a MainActor class capturing
//  `self` in the handler trips Swift 6's actor-isolation assertions.
//  Instead, the handler type is `@Sendable` and the caller is
//  responsible for hopping to MainActor if it wants to touch app state
//  — see the `AppState.init()` call site.
//
//  Self-healing: the listener rebuilds itself if the accept
//  DispatchSource cancels unexpectedly (fd error, OOM) or the socket
//  file is unlinked externally (a periodic stat() catches that case).
//  Rebuilding reuses `self.path`, so NICE_SOCKET in existing shells
//  stays correct across restarts.
//

import Darwin
import Foundation

/// Discriminated payload carried on the control socket. Produced by the
/// parser in `readClient`, consumed by `AppState.init`'s handler, which
/// dispatches each case to the appropriate MainActor method.
enum SocketMessage: Sendable {
    /// `claude` shadow asking Nice whether to open a new sidebar tab
    /// (the default) or promote the sending pane in place. `tabId` /
    /// `paneId` identify the sending pty — both are empty strings for
    /// the Main Terminals tab, which always gets a "newtab" response.
    /// The handler calls `reply` exactly once with one of:
    ///   - "newtab"            — wrapper returns without exec-ing claude
    ///   - "inplace"           — wrapper `exec`s claude with the user's args
    ///   - "inplace <session>" — wrapper `exec`s `claude --session-id <session>`
    /// The reply closure owns closing the client fd.
    case claude(
        cwd: String,
        args: [String],
        tabId: String,
        paneId: String,
        reply: @Sendable (String) -> Void
    )

    /// Claude Code SessionStart hook reporting the active session UUID
    /// for the sending pane. Fires synchronously with each in-process
    /// rotation (/clear, /compact, /branch / --fork-session); the
    /// installer script source-gates the event so startup/resume
    /// don't churn the persistence layer. Fire-and-forget — no reply
    /// is written and the client fd is closed before dispatch.
    case sessionUpdate(paneId: String, sessionId: String)
}

final class NiceControlSocket: @unchecked Sendable {
    typealias Handler = @Sendable (SocketMessage) -> Void

    /// Path of the bound socket file. Exported via `NICE_SOCKET` into
    /// the Main Terminal so the shadowed `claude()` zsh function can
    /// `nc -U "$NICE_SOCKET"` into it.
    let path: String

    // All mutable state below is accessed from `stateQueue`, with one
    // documented exception: `handler` is set exactly once from
    // `start(handler:)` before the first dispatch source resumes and
    // is never rewritten, so `readClient`'s read of it from a global
    // queue is safe without a lock. Rebuilds of the accept source
    // reuse the same handler closure.
    private let stateQueue = DispatchQueue(label: "nice.control-socket.state")
    private let healthCheckInterval: TimeInterval
    private let initialRestartDelay: TimeInterval

    private var fd: Int32 = -1
    private var acceptSource: DispatchSourceRead?
    private var healthCheckTimer: DispatchSourceTimer?
    private var handler: Handler = { _ in }
    private var isStopping = false
    private var restartAttempt = 0

    /// Allocates only — the socket path is derived from the process
    /// pid plus a UUID so multiple sockets (one per window) can coexist
    /// within the same process, and so the path is known immediately and
    /// can be injected into the Main Terminal's env before
    /// `start(handler:)` is called. `NICE_SOCKET_PATH` still overrides
    /// for UI tests.
    ///
    /// `healthCheckInterval` is the period of the periodic `stat(path)`
    /// that detects an externally-unlinked socket file and triggers a
    /// rebind. 30s is plenty for production — socket-file loss is rare
    /// and a user hitting `claude` hits `nc -U ... -w 2`'s timeout at
    /// most once. Unit tests pass a smaller value (e.g. 0.05s) to
    /// exercise the path without real-time waits.
    ///
    /// `initialRestartDelay` is the base delay for the exponential
    /// backoff used between rebind attempts (caps at 5s). The default
    /// keeps a visible gap between retries when something is genuinely
    /// broken; tests override it so retries happen in tens of ms.
    init(
        healthCheckInterval: TimeInterval = 30,
        initialRestartDelay: TimeInterval = 0.5
    ) {
        self.healthCheckInterval = healthCheckInterval
        self.initialRestartDelay = initialRestartDelay
        if let override = ProcessInfo.processInfo.environment["NICE_SOCKET_PATH"] {
            self.path = override
        } else {
            let suffix = UUID().uuidString.prefix(8)
            self.path = NSTemporaryDirectory() + "nice-\(getpid())-\(suffix).sock"
        }
    }

    /// Bind, listen, and start accepting connections on a background
    /// queue. Also arms the periodic file-presence health check. Safe
    /// to call once; subsequent calls are no-ops.
    func start(handler: @escaping Handler) throws {
        try stateQueue.sync {
            guard fd < 0 else { return }
            self.handler = handler
            try bindAndListenLocked()
            startHealthCheckTimerLocked()
            NSLog("NiceControlSocket: listening on \(path)")
        }
    }

    /// Stop accepting, close the socket fd, cancel the health-check
    /// timer, and unlink the file. Safe to call multiple times.
    func stop() {
        stateQueue.sync {
            isStopping = true
            healthCheckTimer?.cancel()
            healthCheckTimer = nil
            acceptSource?.cancel()
            acceptSource = nil
            fd = -1
            _ = unlink(path)
        }
    }

    /// Test hook: force-cancel the accept source as if the kernel had
    /// dropped it. The self-healing path should rebuild the listener
    /// without any external trigger. Internal so the XCTest target
    /// (`@testable import Nice`) can call it; production code never
    /// should.
    func _testForceCancelAcceptSource() {
        stateQueue.sync {
            acceptSource?.cancel()
        }
    }

    // MARK: - Bind / listen (runs on stateQueue)

    private func bindAndListenLocked() throws {
        let serverFd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard serverFd >= 0 else {
            throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
        }
        // Clear any stale socket file — either from a prior crashed
        // run or from the listener we're replacing right now.
        _ = unlink(path)

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
        let bindResult = withUnsafePointer(to: &addr) { ptr -> Int32 in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                bind(serverFd, sa, size)
            }
        }
        guard bindResult == 0 else {
            let err = errno
            close(serverFd)
            throw POSIXError(POSIXErrorCode(rawValue: err) ?? .EIO)
        }

        // Defense in depth — $TMPDIR is already per-user, but force-set
        // 0600 so nothing else on the system can connect.
        _ = chmod(path, 0o600)

        guard listen(serverFd, 8) == 0 else {
            let err = errno
            close(serverFd)
            _ = unlink(path)
            throw POSIXError(POSIXErrorCode(rawValue: err) ?? .EIO)
        }

        self.fd = serverFd

        let src = DispatchSource.makeReadSource(
            fileDescriptor: serverFd,
            queue: .global(qos: .userInitiated)
        )
        // Capture a weak reference so the source outliving the
        // NiceControlSocket (briefly, during teardown) is a no-op.
        src.setEventHandler { [weak self] in
            guard let self else { return }
            let client = accept(serverFd, nil, nil)
            guard client >= 0 else { return }
            DispatchQueue.global(qos: .userInitiated).async {
                self.readClient(client)
            }
        }
        src.setCancelHandler { [weak self] in
            close(serverFd)
            self?.stateQueue.async {
                self?.handleAcceptSourceCancelledLocked()
            }
        }
        src.resume()
        acceptSource = src
    }

    // MARK: - Self-healing (runs on stateQueue)

    private func handleAcceptSourceCancelledLocked() {
        // If stop() set isStopping (or we cancelled the source
        // ourselves for reasons unrelated to restart), do nothing.
        if isStopping { return }
        // The old serverFd is already closed by the cancel handler.
        // Drop our reference and schedule a rebind.
        fd = -1
        acceptSource = nil
        scheduleRestartLocked()
    }

    private func scheduleRestartLocked() {
        // Exponential backoff capped at 5s: with the default base of
        // 500ms the sequence is 500, 1000, 2000, 4000, 5000, 5000, …
        let exp = min(restartAttempt, 20) // prevent pow overflow
        let delaySec = min(initialRestartDelay * pow(2.0, Double(exp)), 5.0)
        restartAttempt += 1
        let delayUs = Int(delaySec * 1_000_000)
        stateQueue.asyncAfter(deadline: .now() + .microseconds(delayUs)) { [weak self] in
            self?.attemptRestartLocked()
        }
    }

    private func attemptRestartLocked() {
        guard !isStopping else { return }
        guard fd < 0 else { return } // Another path already rebuilt.
        do {
            try bindAndListenLocked()
            NSLog("NiceControlSocket: restarted at \(path)")
            restartAttempt = 0
        } catch {
            NSLog("NiceControlSocket: rebind failed: \(error); will retry (attempt \(restartAttempt))")
            scheduleRestartLocked()
        }
    }

    private func startHealthCheckTimerLocked() {
        let timer = DispatchSource.makeTimerSource(queue: stateQueue)
        timer.schedule(
            deadline: .now() + healthCheckInterval,
            repeating: healthCheckInterval
        )
        timer.setEventHandler { [weak self] in
            self?.checkSocketFilePresenceLocked()
        }
        timer.resume()
        healthCheckTimer = timer
    }

    private func checkSocketFilePresenceLocked() {
        if isStopping { return }
        // If we're already in the middle of a rebuild, the restart
        // loop will recreate the file; nothing to do here.
        guard let source = acceptSource, fd >= 0 else { return }
        var st = stat()
        if stat(path, &st) != 0 {
            // File is missing (typically ENOENT). Cancel the current
            // accept source and let the normal restart path kick in
            // so we don't duplicate the rebuild logic.
            NSLog("NiceControlSocket: socket file missing at \(path); rebuilding")
            source.cancel()
            acceptSource = nil
        }
    }

    // MARK: - Connection handling (runs on a background queue)

    private func readClient(_ client: Int32) {
        // Closing the client fd is the reply closure's job once we
        // dispatch a `.claude` message — the handler runs on a different
        // queue (MainActor) and we need the fd open until it writes a
        // response. Any early-return path below must close manually.
        var buffer = Data()
        var chunk = [UInt8](repeating: 0, count: 4096)
        while buffer.count < 64 * 1024 {
            let n = chunk.withUnsafeMutableBufferPointer { buf -> Int in
                read(client, buf.baseAddress, buf.count)
            }
            if n <= 0 { break }
            buffer.append(contentsOf: chunk[0..<n])
            if buffer.contains(0x0A) { break }
        }
        guard !buffer.isEmpty else { close(client); return }

        if let nl = buffer.firstIndex(of: 0x0A) {
            buffer = buffer.subdata(in: buffer.startIndex..<nl)
        }

        guard
            let obj = try? JSONSerialization.jsonObject(with: buffer) as? [String: Any],
            let action = obj["action"] as? String
        else {
            close(client)
            return
        }
        let args = (obj["args"] as? [String]) ?? []
        switch action {
        case "claude":
            guard let cwd = obj["cwd"] as? String else {
                close(client)
                return
            }
            let tabId = (obj["tabId"] as? String) ?? ""
            let paneId = (obj["paneId"] as? String) ?? ""
            let reply: @Sendable (String) -> Void = { line in
                let payload = line + "\n"
                payload.withCString { p in
                    _ = write(client, p, strlen(p))
                }
                close(client)
            }
            handler(.claude(
                cwd: cwd, args: args, tabId: tabId, paneId: paneId, reply: reply
            ))
        case "session_update":
            guard let paneId = obj["paneId"] as? String, !paneId.isEmpty,
                  let sessionId = obj["sessionId"] as? String, !sessionId.isEmpty
            else {
                close(client)
                return
            }
            // Hook is fire-and-forget: close before dispatch so the
            // helper script's `nc` returns promptly even if the
            // MainActor handler is backed up.
            close(client)
            handler(.sessionUpdate(paneId: paneId, sessionId: sessionId))
        default:
            // Unknown action — log and drop, matching the silent-drop
            // behavior used elsewhere for malformed payloads.
            NSLog("NiceControlSocket: unknown action '\(action)' — ignoring")
            close(client)
            return
        }
    }
}
