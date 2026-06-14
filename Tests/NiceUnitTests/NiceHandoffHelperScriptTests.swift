//
//  NiceHandoffHelperScriptTests.swift
//  NiceUnitTests
//
//  Executes the installed `nice-handoff.sh` helper end-to-end against a
//  bound Unix-domain socket and asserts the JSON payload it posts. The
//  substring checks in SkillInstallerTests pin the script *source*; these
//  pin its *behavior* — that the 3rd positional `model` arg and the
//  `CLAUDE_EFFORT` env var actually land in the emitted JSON, that an
//  unset effort / empty model serialize as "", and that the shared
//  `_nice_esc` escaping survives a value containing a double quote.
//
//  Mirrors the ClaudeHookInstallerTests execution harness (TestSocketListener
//  + captureFirstLine), which exercises the sibling nice-claude-hook.sh.
//
//  The helper waits up to ~2s for a reply from the socket; the listener
//  reads the payload and closes without replying, so the helper exits
//  non-zero ("no reply"). That's irrelevant here — the payload is captured
//  before the close, and the payload is what these tests assert.
//

import Darwin
import Foundation
import XCTest
@testable import Nice

final class NiceHandoffHelperScriptTests: XCTestCase {

    private var tmpRoot: URL!
    private var skillDir: URL!
    private var helperDir: URL!

    override func setUpWithError() throws {
        tmpRoot = URL(fileURLWithPath: NSTemporaryDirectory(), isDirectory: true)
            .appendingPathComponent("nice-handoff-helper-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmpRoot, withIntermediateDirectories: true)
        skillDir = tmpRoot.appendingPathComponent("claude/skills/nice-handoff")
        helperDir = tmpRoot.appendingPathComponent("nice")
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: tmpRoot)
    }

    // MARK: - Tests

    func test_helper_emitsModelAndEffort_inPayload() throws {
        let helper = try installedHelperPath()
        let socketPath = Self.makeSocketPath()
        let listener = try TestSocketListener(path: socketPath)
        defer { listener.cleanup() }

        let line = captureFirstLine(listener: listener) {
            self.runHelper(
                at: helper,
                args: ["/tmp/p/.claude/handoff/h.md", "keep going", "claude-opus-4-8"],
                env: [
                    "NICE_SOCKET": socketPath,
                    "NICE_PANE_ID": "pane-1",
                    "CLAUDE_EFFORT": "xhigh",
                ]
            )
        }

        let payload = try parsePayload(XCTUnwrap(line, "helper must post a payload"))
        XCTAssertEqual(payload["action"] as? String, "handoff")
        XCTAssertEqual(payload["model"] as? String, "claude-opus-4-8",
                       "the 3rd positional arg must land in the payload's model field")
        XCTAssertEqual(payload["effort"] as? String, "xhigh",
                       "CLAUDE_EFFORT must land in the payload's effort field")
        XCTAssertEqual(payload["instructions"] as? String, "keep going")
        XCTAssertEqual(payload["handoffFile"] as? String, "/tmp/p/.claude/handoff/h.md")
        XCTAssertEqual(payload["paneId"] as? String, "pane-1")
    }

    func test_helper_unsetEffortAndEmptyModel_serializeAsEmptyStrings() throws {
        let helper = try installedHelperPath()
        let socketPath = Self.makeSocketPath()
        let listener = try TestSocketListener(path: socketPath)
        defer { listener.cleanup() }

        let line = captureFirstLine(listener: listener) {
            self.runHelper(
                at: helper,
                args: ["/tmp/p/.claude/handoff/h.md", "", ""],
                // No CLAUDE_EFFORT in the env (implicit default), empty model arg.
                env: [
                    "NICE_SOCKET": socketPath,
                    "NICE_PANE_ID": "pane-2",
                ]
            )
        }

        let payload = try parsePayload(XCTUnwrap(line, "helper must post a payload"))
        XCTAssertEqual(payload["model"] as? String, "",
                       "an empty model arg must serialize as \"\" so Nice omits --model")
        XCTAssertEqual(payload["effort"] as? String, "",
                       "an unset CLAUDE_EFFORT must serialize as \"\" so Nice omits --effort")
    }

    func test_helper_escapesDoubleQuoteInModel_payloadStaysValidJSON() throws {
        // Locks the shared _nice_esc escaping contract for the new fields:
        // a model value with an embedded double quote must round-trip
        // through escape → JSON → parse to the exact original string. A
        // broken escape would produce invalid JSON (parse returns nil).
        let helper = try installedHelperPath()
        let socketPath = Self.makeSocketPath()
        let listener = try TestSocketListener(path: socketPath)
        defer { listener.cleanup() }

        let line = captureFirstLine(listener: listener) {
            self.runHelper(
                at: helper,
                args: ["/tmp/p/h.md", "", #"weird"model"#],
                env: [
                    "NICE_SOCKET": socketPath,
                    "NICE_PANE_ID": "pane-3",
                    "CLAUDE_EFFORT": "max",
                ]
            )
        }

        let raw = try XCTUnwrap(line, "helper must post a payload")
        let payload = try parsePayload(raw)
        XCTAssertEqual(payload["model"] as? String, #"weird"model"#,
                       "a double-quoted model must round-trip exactly; payload was: \(raw)")
        XCTAssertEqual(payload["effort"] as? String, "max")
    }

    // MARK: - Helpers

    private func installedHelperPath() throws -> String {
        SkillInstaller.install(skillDir: skillDir, helperDir: helperDir)
        let url = helperDir.appendingPathComponent("nice-handoff.sh")
        XCTAssertTrue(FileManager.default.fileExists(atPath: url.path),
                      "precondition: helper must be installed")
        return url.path
    }

    private static func makeSocketPath() -> String {
        // /tmp (not tmpRoot under /var/folders) — sun_path is 104 bytes and
        // the temp-folder prefix alone overflows it. Per-test UUID isolates
        // parallel runs. Mirrors ClaudeHookInstallerTests.
        "/tmp/nice-handoff-test-\(UUID().uuidString.prefix(8)).sock"
    }

    private func parsePayload(_ line: String) throws -> [String: Any] {
        let data = try XCTUnwrap(line.data(using: .utf8))
        let obj = try JSONSerialization.jsonObject(with: data)
        return try XCTUnwrap(obj as? [String: Any],
                             "captured line must be a JSON object: \(line)")
    }

    @discardableResult
    private func runHelper(at path: String, args: [String], env: [String: String]) -> Int32 {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: path)
        proc.arguments = args
        // The helper reads $PWD for the cwd field; bash sets it at startup,
        // but pin it (and the process cwd) deterministically so the script's
        // `set -u` never trips on an unset PWD under a replaced environment.
        var fullEnv = env
        fullEnv["PWD"] = fullEnv["PWD"] ?? tmpRoot.path
        proc.environment = fullEnv
        proc.currentDirectoryURL = tmpRoot
        proc.standardOutput = Pipe()
        proc.standardError = Pipe()
        try? proc.run()
        proc.waitUntilExit()
        return proc.terminationStatus
    }

    /// Run the helper with a listener bound at its NICE_SOCKET path; return
    /// the first line the helper posted, or nil if nothing arrived within
    /// the timeout. The listener accepts on a background queue so the
    /// main-thread helper invocation can drive the connection. Mirrors
    /// ClaudeHookInstallerTests.captureFirstLine.
    private func captureFirstLine(
        listener: TestSocketListener,
        timeout: TimeInterval = 2.0,
        runScript: () -> Void
    ) -> String? {
        let group = DispatchGroup()
        var captured: String?
        group.enter()
        DispatchQueue.global(qos: .userInitiated).async {
            captured = listener.acceptOne(timeout: timeout)
            group.leave()
        }
        runScript()
        _ = group.wait(timeout: .now() + timeout + 0.5)
        return captured
    }
}
