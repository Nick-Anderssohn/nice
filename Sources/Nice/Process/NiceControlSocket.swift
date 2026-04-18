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
//  after bind. The listener is intentionally isolated from the MCP
//  server so it survives if MCP is later removed.
//
//  The class is deliberately *not* `@MainActor`. DispatchSource event
//  handlers fire on a background queue; a MainActor class capturing
//  `self` in the handler trips Swift 6's actor-isolation assertions.
//  Instead, the handler type is `@Sendable` and the caller is
//  responsible for hopping to MainActor if it wants to touch app state
//  — see the `AppState.init()` call site.
//

import Darwin
import Foundation

final class NiceControlSocket: @unchecked Sendable {
    typealias Handler = @Sendable (_ cwd: String, _ args: [String]) -> Void

    /// Path of the bound socket file. Exported via `NICE_SOCKET` into
    /// the Main Terminal so the shadowed `claude()` zsh function can
    /// `nc -U "$NICE_SOCKET"` into it.
    let path: String

    private var fd: Int32 = -1
    private var acceptSource: DispatchSourceRead?
    // Set once from `start(handler:)` before the source resumes, so
    // readClient's read of it from a bg queue is safe without a lock.
    private var handler: Handler = { _, _ in }

    /// Allocates only — the socket path is derived from the process
    /// pid so it's known immediately and can be injected into the Main
    /// Terminal's env before `start(handler:)` is called.
    init() {
        self.path = NSTemporaryDirectory() + "nice-\(getpid()).sock"
    }

    /// Bind, listen, and start accepting connections on a background
    /// queue. Safe to call once; subsequent calls are no-ops.
    func start(handler: @escaping Handler) throws {
        guard fd < 0 else { return }
        self.handler = handler

        let serverFd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard serverFd >= 0 else {
            throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
        }
        // Clear any stale socket file from a prior crashed run.
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
        src.setCancelHandler {
            close(serverFd)
        }
        src.resume()
        acceptSource = src

        NSLog("NiceControlSocket: listening on \(path)")
    }

    /// Stop accepting, close the socket fd, and unlink the file. Safe
    /// to call multiple times.
    func stop() {
        acceptSource?.cancel()
        acceptSource = nil
        fd = -1
        _ = unlink(path)
    }

    // MARK: - Connection handling (runs on a background queue)

    private func readClient(_ client: Int32) {
        defer { close(client) }
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
        guard !buffer.isEmpty else { return }

        if let nl = buffer.firstIndex(of: 0x0A) {
            buffer = buffer.subdata(in: buffer.startIndex..<nl)
        }

        guard
            let obj = try? JSONSerialization.jsonObject(with: buffer) as? [String: Any],
            (obj["action"] as? String) == "newtab",
            let cwd = obj["cwd"] as? String
        else {
            return
        }
        let args = (obj["args"] as? [String]) ?? []
        handler(cwd, args)
    }
}
