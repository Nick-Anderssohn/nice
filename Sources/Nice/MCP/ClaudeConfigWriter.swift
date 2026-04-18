//
//  ClaudeConfigWriter.swift
//  Nice
//
//  Phase 6: writes a temp `.mcp.json` exposing the in-process Nice MCP
//  server to `claude` processes spawned by `TabPtySession`. The file's
//  path is passed as `--mcp-config <path>` to the claude binary so each
//  tab's Claude can call back into Nice via the HTTP MCP endpoint on
//  127.0.0.1:<port>.
//
//  Per Claude Code's config format, HTTP servers are identified purely
//  by a `url` key — no `type` field and no path suffix. Claude Code
//  infers HTTP transport from the URL scheme.
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
                "nice": ["url": "http://127.0.0.1:\(port)"]
            ]
        ]
        let data = try JSONSerialization.data(
            withJSONObject: payload, options: [.prettyPrinted]
        )
        try data.write(to: file, options: .atomic)
        return file
    }
}
