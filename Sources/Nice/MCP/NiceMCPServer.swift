//
//  NiceMCPServer.swift
//  Nice
//
//  Phase 6: owns the in-process MCP server lifecycle. Registers the
//  tools the `claude` CLI inside each tab can call back into:
//
//    nice.tab.switch    — focus a tab by id or fuzzy title match
//    nice.tab.list      — enumerate all tabs as JSON
//    nice.run           — run a shell command in a tab's terminal pane
//    nice.terminal.open — add a new terminal pane to a tab
//
//  Tab creation is deliberately not an MCP tool — it lives on the
//  control socket (see `NiceControlSocket`) driven by a shadowed
//  `claude()` zsh function in the Terminals tab, so the natural
//  `claude <args>` invocation opens a tab.
//
//  The server itself uses the Swift SDK's `StatefulHTTPServerTransport`
//  (framework-agnostic — just parses/emits `HTTPRequest`/`HTTPResponse`)
//  paired with our local `NiceHTTPBridge` (an `NWListener` on
//  127.0.0.1:7420 that speaks HTTP/1.1 + chunked SSE).
//
//  Tool handlers are `@Sendable async throws` and hop to `@MainActor`
//  before touching `AppState`.
//

import Foundation
import MCP

@MainActor
final class NiceMCPServer: ObservableObject {
    @Published private(set) var isRunning = false
    /// OS-assigned port, published after `start` binds successfully.
    /// Zero while the server is stopped. Multi-window: each window runs
    /// its own server on a distinct ephemeral port so a Claude process
    /// spawned in that window connects only to that window's MCP.
    @Published private(set) var port: Int = 0

    private weak var appState: AppState?
    private var server: Server?
    private var transport: StatefulHTTPServerTransport?
    private var bridge: NiceHTTPBridge?
    private var serverTask: Task<Void, Never>?

    /// Start the server. Idempotent — if `isRunning` is already true,
    /// this is a no-op. Called from `AppState.bootstrap()`.
    func start(appState: AppState) async {
        guard !isRunning else { return }
        self.appState = appState

        let server = Server(
            name: "Nice",
            version: "0.1.0",
            capabilities: .init(tools: .init(listChanged: false))
        )
        let transport = StatefulHTTPServerTransport()

        // Register tool handlers. These run on the server actor; they
        // hop to MainActor before touching AppState.
        await server.withMethodHandler(ListTools.self) { _ in
            ListTools.Result(tools: NiceMCPServer.tools)
        }
        await server.withMethodHandler(CallTool.self) { [weak self] params in
            guard let self else {
                return CallTool.Result(
                    content: [.text(text: "server gone", annotations: nil, _meta: nil)],
                    isError: true
                )
            }
            return try await self.handleCall(params: params)
        }

        // Bind an OS-assigned ephemeral port so multiple windows can
        // each run a server in the same process without colliding.
        let bridge = NiceHTTPBridge(port: 0, transport: transport)
        let boundPort: UInt16
        do {
            boundPort = try await bridge.start()
        } catch {
            NSLog("NiceMCPServer: failed to bind HTTP bridge: \(error)")
            return
        }

        self.server = server
        self.transport = transport
        self.bridge = bridge
        self.port = Int(boundPort)

        // Start the server's receive loop. `Server.start` connects the
        // transport and runs the message loop until the transport is
        // disconnected.
        serverTask = Task { [server, transport] in
            do {
                try await server.start(transport: transport)
                await server.waitUntilCompleted()
            } catch {
                NSLog("NiceMCPServer: server loop ended with error: \(error)")
            }
        }

        isRunning = true
        NSLog("NiceMCPServer: running on 127.0.0.1:\(port)")
    }

    func stop() async {
        serverTask?.cancel()
        serverTask = nil
        await server?.stop()
        await bridge?.stop()
        server = nil
        transport = nil
        port = 0
        bridge = nil
        isRunning = false
    }

    // MARK: - Tool registry

    /// The three static tool definitions advertised via `tools/list`.
    /// Schemas are encoded as the SDK's `Value` JSON form.
    ///
    /// `nonisolated` so the Sendable `ListTools` handler closure can
    /// read it without hopping to the main actor.
    nonisolated static var tools: [Tool] {
        [
            Tool(
                name: "nice.tab.switch",
                description: "Focus an existing tab, either by exact id or by fuzzy-matching its title.",
                inputSchema: .object([
                    "type": .string("object"),
                    "properties": .object([
                        "tabId": .object([
                            "type": .string("string"),
                            "description": .string("Exact tab id."),
                        ]),
                        "titleQuery": .object([
                            "type": .string("string"),
                            "description": .string("Case-insensitive substring to match against tab titles."),
                        ]),
                    ]),
                ])
            ),
            Tool(
                name: "nice.tab.list",
                description: "List every tab across all projects.",
                inputSchema: .object([
                    "type": .string("object"),
                    "properties": .object([:]),
                ])
            ),
            Tool(
                name: "nice.run",
                description: "Run a shell command in a tab's terminal pane. If tabId is omitted, runs in the currently active tab.",
                inputSchema: .object([
                    "type": .string("object"),
                    "required": .array([.string("command")]),
                    "properties": .object([
                        "tabId": .object([
                            "type": .string("string"),
                            "description": .string("Target tab id. Defaults to the active tab."),
                        ]),
                        "command": .object([
                            "type": .string("string"),
                            "description": .string("Shell command to run (appended with a newline)."),
                        ]),
                    ]),
                ])
            ),
            Tool(
                name: "nice.terminal.open",
                description: "Open a new terminal pane in a tab. Use this to reopen a terminal the user accidentally closed, or to add a second shell alongside Claude.",
                inputSchema: .object([
                    "type": .string("object"),
                    "properties": .object([
                        "tabId": .object([
                            "type": .string("string"),
                            "description": .string("Target tab id. Defaults to the active tab."),
                        ]),
                        "cwd": .object([
                            "type": .string("string"),
                            "description": .string("Working directory for the new shell. Defaults to the tab's cwd."),
                        ]),
                        "title": .object([
                            "type": .string("string"),
                            "description": .string("Display title for the new pane's pill. Defaults to \"Terminal N\"."),
                        ]),
                    ]),
                ])
            ),
        ]
    }

    // MARK: - CallTool dispatch

    private func handleCall(params: CallTool.Parameters) async throws -> CallTool.Result {
        let args = params.arguments ?? [:]
        switch params.name {
        case "nice.tab.switch":
            let tabId = args["tabId"]?.stringValue
            let query = args["titleQuery"]?.stringValue
            let resolved = await MainActor.run { [weak appState] in
                appState?.mcpSwitchTab(tabId: tabId, titleQuery: query)
            }
            if let resolved {
                let json = Self.jsonString(["tabId": resolved])
                return CallTool.Result(
                    content: [.text(text: json, annotations: nil, _meta: nil)]
                )
            } else {
                return CallTool.Result(
                    content: [.text(text: "no match", annotations: nil, _meta: nil)],
                    isError: true
                )
            }

        case "nice.tab.list":
            let rows = await MainActor.run { [weak appState] in
                appState?.mcpListTabs() ?? []
            }
            let json = Self.jsonArrayString(rows)
            return CallTool.Result(
                content: [.text(text: json, annotations: nil, _meta: nil)]
            )

        case "nice.run":
            let tabId = args["tabId"]?.stringValue
            let cmd = args["command"]?.stringValue ?? ""
            let ok = await MainActor.run { [weak appState] in
                appState?.mcpRun(tabId: tabId, command: cmd) ?? false
            }
            let json = Self.jsonString(["ok": ok ? "true" : "false"])
            return CallTool.Result(
                content: [.text(text: json, annotations: nil, _meta: nil)],
                isError: ok ? nil : true
            )

        case "nice.terminal.open":
            let tabId = args["tabId"]?.stringValue
            let cwd = args["cwd"]?.stringValue
            let title = args["title"]?.stringValue
            let newId = await MainActor.run { [weak appState] in
                appState?.mcpOpenTerminal(tabId: tabId, cwd: cwd, title: title)
            }
            if let newId {
                let json = Self.jsonString(["paneId": newId])
                return CallTool.Result(
                    content: [.text(text: json, annotations: nil, _meta: nil)]
                )
            } else {
                return CallTool.Result(
                    content: [
                        .text(
                            text: "no valid target tab for nice.terminal.open",
                            annotations: nil, _meta: nil
                        )
                    ],
                    isError: true
                )
            }

        default:
            return CallTool.Result(
                content: [
                    .text(text: "unknown tool: \(params.name)", annotations: nil, _meta: nil)
                ],
                isError: true
            )
        }
    }

    // MARK: - JSON helpers

    private static func jsonString(_ dict: [String: String]) -> String {
        guard let data = try? JSONSerialization.data(
            withJSONObject: dict, options: [.sortedKeys]
        ), let s = String(data: data, encoding: .utf8) else {
            return "{}"
        }
        return s
    }

    private static func jsonArrayString(_ rows: [[String: String]]) -> String {
        guard let data = try? JSONSerialization.data(
            withJSONObject: rows, options: [.sortedKeys]
        ), let s = String(data: data, encoding: .utf8) else {
            return "[]"
        }
        return s
    }
}
