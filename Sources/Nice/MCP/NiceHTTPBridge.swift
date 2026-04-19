//
//  NiceHTTPBridge.swift
//  Nice
//
//  Phase 6: a minimal HTTP/1.1 listener built on `Network.framework`
//  that pipes incoming requests to the MCP SDK's
//  `StatefulHTTPServerTransport` and writes back the transport's
//  framework-agnostic `HTTPResponse` (including SSE `.stream` cases).
//
//  The MCP SDK's `StatefulHTTPServerTransport` is framework-agnostic —
//  it only accepts `HTTPRequest` structs and returns `HTTPResponse`
//  enums. It does NOT listen on a socket. So we stand up our own
//  `NWListener` and hand-parse enough HTTP/1.1 to shuttle messages
//  into it. Scope: POST + GET + DELETE, explicit Content-Length
//  bodies, and chunked SSE response bodies.
//
//  We only need to serve a single trusted client (the local `claude`
//  process) on 127.0.0.1, so the parser is intentionally strict/small
//  rather than RFC-complete.
//

import Foundation
import MCP
import Network

/// Listens on 127.0.0.1:<port> and shuttles HTTP/1.1 requests to an
/// underlying `StatefulHTTPServerTransport`. Passing `port: 0` binds
/// an OS-assigned ephemeral port; the bound port is returned from
/// `start()` so callers can advertise it.
actor NiceHTTPBridge {
    private let requestedPort: NWEndpoint.Port
    private let transport: StatefulHTTPServerTransport
    private var listener: NWListener?

    init(port: UInt16, transport: StatefulHTTPServerTransport) {
        self.requestedPort = NWEndpoint.Port(rawValue: port) ?? .any
        self.transport = transport
    }

    /// Begin accepting connections. Returns the OS-assigned port (which
    /// equals the requested port when non-zero). Throws if the listener
    /// cannot bind or fails before becoming ready.
    func start() async throws -> UInt16 {
        let params = NWParameters.tcp
        params.allowLocalEndpointReuse = true
        // Confining to loopback-only is enforced by the transport's
        // OriginValidator + our trust model (unsandboxed personal
        // dev tool). Passing `requiredLocalEndpoint` here conflicts
        // with `NWListener(using:on:)`'s own port argument.

        let listener = try NWListener(using: params, on: requestedPort)
        self.listener = listener

        listener.newConnectionHandler = { [weak self] conn in
            guard let self else {
                conn.cancel()
                return
            }
            Task { await self.accept(conn) }
        }

        // Wait for `.ready` so `listener.port` reflects the OS-assigned
        // port when `requestedPort` was `.any`. `stateUpdateHandler`
        // fires on a background queue, so we guard the continuation
        // with a locked box to avoid double-resume if multiple state
        // transitions race.
        let gate = StartupGate()
        let boundPort: UInt16 = try await withCheckedThrowingContinuation { cont in
            listener.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    gate.resumeOnce {
                        cont.resume(returning: listener.port?.rawValue ?? 0)
                    }
                case .failed(let err):
                    gate.resumeOnce { cont.resume(throwing: err) }
                case .cancelled:
                    gate.resumeOnce { cont.resume(throwing: CancellationError()) }
                default:
                    break
                }
            }
            listener.start(queue: .global(qos: .userInitiated))
        }

        return boundPort
    }

    func stop() {
        listener?.cancel()
        listener = nil
    }

    // MARK: - Connection lifecycle

    private func accept(_ conn: NWConnection) {
        let handler = HTTPConnectionHandler(conn: conn, transport: transport)
        conn.stateUpdateHandler = { state in
            switch state {
            case .ready:
                handler.begin()
            case .failed, .cancelled:
                break
            default:
                break
            }
        }
        conn.start(queue: .global(qos: .userInitiated))
    }
}

/// Single-shot guard around the startup continuation. `NWListener`'s
/// `stateUpdateHandler` fires on a concurrent queue, so the locked
/// flag keeps `.ready` / `.failed` / `.cancelled` from each trying to
/// resume the same continuation.
private final class StartupGate: @unchecked Sendable {
    private let lock = NSLock()
    private var fired = false

    func resumeOnce(_ action: () -> Void) {
        lock.lock()
        let first = !fired
        fired = true
        lock.unlock()
        if first { action() }
    }
}

// MARK: - Per-connection handler

/// Owns a single `NWConnection`, reads one HTTP/1.1 request out of it,
/// hands it to the MCP transport, writes the response, and then closes
/// (or holds the socket open for SSE streaming).
///
/// Not an actor — all state is confined to the connection's dedicated
/// task flow, and `NWConnection` callbacks land on our serial queue.
private final class HTTPConnectionHandler: @unchecked Sendable {
    private let conn: NWConnection
    private let transport: StatefulHTTPServerTransport
    private var buffer = Data()
    private var begun = false

    init(conn: NWConnection, transport: StatefulHTTPServerTransport) {
        self.conn = conn
        self.transport = transport
    }

    func begin() {
        guard !begun else { return }
        begun = true
        receiveMore()
    }

    private func receiveMore() {
        conn.receive(minimumIncompleteLength: 1, maximumLength: 64 * 1024) {
            [weak self] data, _, isComplete, error in
            guard let self else { return }
            if let data, !data.isEmpty {
                self.buffer.append(data)
                if let (request, remainder) = Self.tryParseRequest(
                    from: self.buffer
                ) {
                    self.buffer = remainder
                    self.dispatch(request: request)
                    return
                }
            }
            if isComplete || error != nil {
                self.close()
                return
            }
            self.receiveMore()
        }
    }

    private func dispatch(request: HTTPRequest) {
        Task { [conn, transport] in
            let response = await transport.handleRequest(request)
            await HTTPConnectionHandler.write(
                response: response, to: conn
            )
        }
    }

    private func close() {
        conn.cancel()
    }

    // MARK: HTTP/1.1 parse (very small, strict)

    /// Tries to parse a single HTTP/1.1 request out of `buf`. Returns
    /// the parsed request and the bytes left over after it, or `nil` if
    /// the buffer doesn't hold a complete request yet.
    ///
    /// Only handles identity-coded bodies with an explicit
    /// `Content-Length` header; `Transfer-Encoding: chunked` from the
    /// client is not supported (Claude Code posts plain JSON with
    /// Content-Length).
    static func tryParseRequest(from buf: Data) -> (HTTPRequest, Data)? {
        // Locate header/body boundary.
        let sep = Data([0x0D, 0x0A, 0x0D, 0x0A])  // \r\n\r\n
        guard let sepRange = buf.range(of: sep) else { return nil }

        let headerData = buf.subdata(in: 0..<sepRange.lowerBound)
        guard let headerString = String(data: headerData, encoding: .utf8)
        else { return nil }

        let lines = headerString.components(separatedBy: "\r\n")
        guard let requestLine = lines.first else { return nil }
        let parts = requestLine.split(
            separator: " ", maxSplits: 2, omittingEmptySubsequences: false
        )
        guard parts.count >= 2 else { return nil }
        let method = String(parts[0])
        let rawTarget = String(parts[1])

        // Split path from query — transport only cares about path.
        let path: String
        if let q = rawTarget.firstIndex(of: "?") {
            path = String(rawTarget[..<q])
        } else {
            path = rawTarget
        }

        var headers: [String: String] = [:]
        for line in lines.dropFirst() where !line.isEmpty {
            guard let colon = line.firstIndex(of: ":") else { continue }
            let name = String(line[..<colon])
                .trimmingCharacters(in: .whitespaces)
            let value = String(line[line.index(after: colon)...])
                .trimmingCharacters(in: .whitespaces)
            headers[name] = value
        }

        // Case-insensitive Content-Length lookup.
        let contentLength: Int = {
            for (k, v) in headers where k.lowercased() == "content-length" {
                return Int(v) ?? 0
            }
            return 0
        }()

        let bodyStart = sepRange.upperBound
        let available = buf.count - bodyStart
        guard available >= contentLength else { return nil }

        let body: Data?
        if contentLength > 0 {
            body = buf.subdata(in: bodyStart..<(bodyStart + contentLength))
        } else {
            body = nil
        }
        let remainder = buf.subdata(
            in: (bodyStart + contentLength)..<buf.count
        )
        let req = HTTPRequest(
            method: method, headers: headers, body: body, path: path
        )
        return (req, remainder)
    }

    // MARK: HTTP/1.1 response write

    static func write(response: HTTPResponse, to conn: NWConnection) async {
        switch response {
        case .stream(let stream, let headers):
            await writeStream(stream, headers: headers, to: conn)
        default:
            writeOneShot(response: response, to: conn)
        }
    }

    private static func writeOneShot(response: HTTPResponse, to conn: NWConnection) {
        var headers = response.headers
        let body = response.bodyData
        if body != nil {
            headers["Content-Length"] = String(body!.count)
        } else if case .accepted = response {
            headers["Content-Length"] = "0"
        } else if case .ok = response {
            headers["Content-Length"] = "0"
        }
        headers["Connection"] = "close"

        var head = "HTTP/1.1 \(response.statusCode) \(statusText(response.statusCode))\r\n"
        for (k, v) in headers {
            head += "\(k): \(v)\r\n"
        }
        head += "\r\n"

        var data = Data(head.utf8)
        if let body { data.append(body) }

        conn.send(
            content: data,
            completion: .contentProcessed { _ in
                conn.send(
                    content: nil, contentContext: .finalMessage,
                    isComplete: true, completion: .contentProcessed { _ in
                        conn.cancel()
                    }
                )
            }
        )
    }

    /// SSE writer — writes the response head with chunked transfer
    /// encoding, then pumps each `AsyncThrowingStream` data chunk as a
    /// chunked-encoding frame. Closes the socket when the stream ends.
    private static func writeStream(
        _ stream: AsyncThrowingStream<Data, Error>,
        headers: [String: String],
        to conn: NWConnection
    ) async {
        var headers = headers
        headers["Transfer-Encoding"] = "chunked"
        headers["Connection"] = "close"

        var head = "HTTP/1.1 200 OK\r\n"
        for (k, v) in headers {
            head += "\(k): \(v)\r\n"
        }
        head += "\r\n"

        await sendSync(conn: conn, data: Data(head.utf8))

        do {
            for try await chunk in stream {
                guard !chunk.isEmpty else { continue }
                var frame = Data("\(String(chunk.count, radix: 16))\r\n".utf8)
                frame.append(chunk)
                frame.append(contentsOf: [0x0D, 0x0A])  // \r\n
                await sendSync(conn: conn, data: frame)
            }
        } catch {
            // Stream ended with error — fall through to terminator.
        }

        // Terminating zero-length chunk.
        await sendSync(conn: conn, data: Data("0\r\n\r\n".utf8))
        conn.cancel()
    }

    /// Bridge `NWConnection.send` (completion-based) into async so we
    /// back-pressure-wait before writing the next frame.
    private static func sendSync(conn: NWConnection, data: Data) async {
        await withCheckedContinuation { (cont: CheckedContinuation<Void, Never>) in
            conn.send(
                content: data,
                completion: .contentProcessed { _ in
                    cont.resume()
                }
            )
        }
    }

    private static func statusText(_ code: Int) -> String {
        switch code {
        case 200: return "OK"
        case 202: return "Accepted"
        case 400: return "Bad Request"
        case 403: return "Forbidden"
        case 404: return "Not Found"
        case 405: return "Method Not Allowed"
        case 409: return "Conflict"
        case 421: return "Misdirected Request"
        case 500: return "Internal Server Error"
        default: return "OK"
        }
    }
}
