//
//  ClaudeConfigWriter.swift
//  Nice
//
//  Phase 6: writes a temp `.mcp.json` exposing the in-process Nice MCP
//  server to `claude` processes spawned by `TabPtySession`. The file's
//  path is passed as `--mcp-config <path>` to the claude binary so each
//  tab's Claude can call back into Nice via the HTTP MCP endpoint on
//  127.0.0.1:<port>/mcp.
//
//  Claude Code's current MCP config schema requires an explicit `type`
//  field ("http" | "sse" | "stdio"); the URL-only form produces
//  "Invalid MCP configuration: … Does not adhere to MCP server
//  configuration schema". We emit `type: "http"` alongside the URL.
//

import Foundation

enum ClaudeConfigWriter {
    /// Write a temp `.mcp.json` exposing the Nice MCP server. Returns
    /// the absolute path of the resulting file, suitable for passing to
    /// `claude --mcp-config <path>`.
    static func writeConfig(port: Int) throws -> URL {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("nice-mcp", isDirectory: true)
        try FileManager.default.createDirectory(
            at: dir, withIntermediateDirectories: true
        )
        let file = dir.appendingPathComponent("\(UUID().uuidString).json")
        let payload: [String: Any] = [
            "mcpServers": [
                "nice": [
                    "type": "http",
                    "url": "http://127.0.0.1:\(port)/mcp",
                ]
            ]
        ]
        let data = try JSONSerialization.data(
            withJSONObject: payload, options: [.prettyPrinted]
        )
        try data.write(to: file, options: .atomic)
        return file
    }
}
