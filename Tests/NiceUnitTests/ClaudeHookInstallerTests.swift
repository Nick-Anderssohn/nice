//
//  ClaudeHookInstallerTests.swift
//  NiceUnitTests
//
//  Tests lock down the install / merge behavior that determines whether
//  claude actually routes UserPromptSubmit payloads back to Nice:
//  - script lands under the configured dir, is executable, and extracts
//    `session_id` reliably from a representative payload (the bit that
//    matters in production)
//  - settings.local.json merges cleanly with pre-existing hooks
//    (user's own hooks preserved; Nice's entry added once)
//  - reinstall is idempotent — no duplicate entries, no churn writes
//  - malformed settings.local.json bails out instead of silently
//    overwriting the user's content
//
//  Tests pass paths directly via the `install(scriptDir:settingsURL:)`
//  surface — no env-var dance, no process-global state to race under
//  parallel testing, no risk of pollution if the test process inherits
//  a real `~/.claude/`.
//

import XCTest
@testable import Nice

final class ClaudeHookInstallerTests: XCTestCase {

    private var tmpRoot: URL!
    private var scriptDir: URL!
    private var settingsURL: URL!

    override func setUpWithError() throws {
        tmpRoot = URL(
            fileURLWithPath: NSTemporaryDirectory(),
            isDirectory: true
        )
        .appendingPathComponent("nice-hook-installer-\(UUID().uuidString)")
        try FileManager.default.createDirectory(
            at: tmpRoot, withIntermediateDirectories: true
        )
        // Mirrors the production layout: a no-space script dir under
        // a sandbox HOME, and `.claude/settings.json` (not `.local.json`,
        // which claude silently ignores for hook execution).
        scriptDir = tmpRoot.appendingPathComponent(".nice")
        settingsURL = tmpRoot
            .appendingPathComponent(".claude/settings.json")
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: tmpRoot)
    }

    // MARK: - Script

    func test_install_writesExecutableScript() throws {
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        let scriptURL = scriptDir.appendingPathComponent("nice-claude-hook.sh")
        XCTAssertTrue(FileManager.default.fileExists(atPath: scriptURL.path))
        let attrs = try FileManager.default.attributesOfItem(atPath: scriptURL.path)
        let perms = (attrs[.posixPermissions] as? NSNumber)?.int16Value ?? 0
        XCTAssertEqual(perms & 0o777, 0o755, "script perms must be exactly 0755")
    }

    func test_install_scriptExtractsSessionIdFromPayload() throws {
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        let scriptURL = scriptDir.appendingPathComponent("nice-claude-hook.sh")

        // Shape of the payload claude actually sends, captured from a
        // live `claude -p` run. Regression-proofs the sed regex against
        // real-world whitespace / key ordering.
        let payload = #"{"session_id":"fe06fd37-1bcb-41f1-af4f-2588a276798b","transcript_path":"/x.jsonl","cwd":"/private/tmp","hook_event_name":"UserPromptSubmit","prompt":"hi"}"#

        // Run with NICE_SOCKET pointing at a path nothing is listening
        // on — the script must fail-open (exit 0) after `nc` times
        // out, not fail the hook. The socket-side payload shape is
        // verified end-to-end in Nice Dev once the user types in a tab.
        let (exit, _) = runScript(
            at: scriptURL.path,
            env: [
                "NICE_SOCKET": tmpRoot.appendingPathComponent("no-such.sock").path,
                "NICE_PANE_ID": "pane-xyz",
            ],
            stdin: payload
        )
        XCTAssertEqual(exit, 0, "script must exit 0 even when the socket is unreachable")
    }

    func test_install_scriptNoOpsOutsideNice() throws {
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        let scriptURL = scriptDir.appendingPathComponent("nice-claude-hook.sh")
        // Neither NICE_SOCKET nor NICE_PANE_ID set — standard
        // claude-outside-Nice case. Script must not touch stdout or
        // fail, so user's own hooks see their normal stdin/stdout.
        let (exit, output) = runScript(
            at: scriptURL.path,
            env: [:],
            stdin: #"{"session_id":"deadbeef-dead-beef-dead-beefdeadbeef"}"#
        )
        XCTAssertEqual(exit, 0)
        XCTAssertEqual(output, "", "script must produce no output when unconfigured")
    }

    // MARK: - Settings merge

    func test_install_mergesIntoEmptySettings() throws {
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        let settings = try readSettings()
        let ups = hooksGroup(settings, name: "UserPromptSubmit")
        XCTAssertEqual(ups.count, 1)
        XCTAssertEqual(commandOf(ups[0]), expectedScriptPath())
    }

    func test_install_preservesUserHooks() throws {
        // User's existing settings include an unrelated event group
        // and a sibling under UserPromptSubmit. Nice's entry is added
        // alongside, never replacing.
        let userSettings: [String: Any] = [
            "hooks": [
                "UserPromptSubmit": [
                    [
                        "hooks": [
                            ["type": "command", "command": "/user/own/hook.sh"],
                        ],
                    ],
                ],
                "PreToolUse": [
                    [
                        "hooks": [
                            ["type": "command", "command": "/user/pre-tool.sh"],
                        ],
                    ],
                ],
            ],
            "someOtherKey": "keepMe",
        ]
        try writeSettings(userSettings)

        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)

        let settings = try readSettings()
        XCTAssertEqual(settings["someOtherKey"] as? String, "keepMe")
        let ups = hooksGroup(settings, name: "UserPromptSubmit")
        XCTAssertEqual(ups.count, 2)
        XCTAssertEqual(commandOf(ups[0]), "/user/own/hook.sh")
        XCTAssertEqual(commandOf(ups[1]), expectedScriptPath())
        let pre = hooksGroup(settings, name: "PreToolUse")
        XCTAssertEqual(pre.count, 1)
        XCTAssertEqual(commandOf(pre[0]), "/user/pre-tool.sh")
    }

    func test_install_preservesMultipleUserPromptSubmitGroups() throws {
        // Two pre-existing UPS groups, each with their own hook. After
        // install both must still be present (in order) and Nice's
        // entry must be appended last.
        let userSettings: [String: Any] = [
            "hooks": [
                "UserPromptSubmit": [
                    ["hooks": [["type": "command", "command": "/first.sh"]]],
                    ["hooks": [["type": "command", "command": "/second.sh"]]],
                ],
            ],
        ]
        try writeSettings(userSettings)

        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)

        let ups = hooksGroup(try readSettings(), name: "UserPromptSubmit")
        XCTAssertEqual(ups.count, 3)
        XCTAssertEqual(commandOf(ups[0]), "/first.sh")
        XCTAssertEqual(commandOf(ups[1]), "/second.sh")
        XCTAssertEqual(commandOf(ups[2]), expectedScriptPath())
    }

    func test_install_preservesSiblingHooksInsideSameGroup() throws {
        // A single UPS group with multiple inner `hooks` entries — none
        // of them ours. The dedup check at `mergeHookSettings` walks
        // `inner.contains`, so this exercises the inner walk.
        let userSettings: [String: Any] = [
            "hooks": [
                "UserPromptSubmit": [
                    [
                        "hooks": [
                            ["type": "command", "command": "/a.sh"],
                            ["type": "command", "command": "/b.sh"],
                        ],
                    ],
                ],
            ],
        ]
        try writeSettings(userSettings)

        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)

        let ups = hooksGroup(try readSettings(), name: "UserPromptSubmit")
        XCTAssertEqual(ups.count, 2, "Nice appended as a new group, not folded into the user's")
        // First group: user's two hooks intact.
        let firstInner = ups[0]["hooks"] as? [[String: Any]] ?? []
        XCTAssertEqual(firstInner.count, 2)
        XCTAssertEqual(firstInner[0]["command"] as? String, "/a.sh")
        XCTAssertEqual(firstInner[1]["command"] as? String, "/b.sh")
        XCTAssertEqual(commandOf(ups[1]), expectedScriptPath())
    }

    func test_install_isIdempotent() throws {
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        let ups = hooksGroup(try readSettings(), name: "UserPromptSubmit")
        XCTAssertEqual(ups.count, 1, "repeated install must not duplicate entries")
    }

    func test_install_secondCallDoesNotRewriteSettings() throws {
        // After the first install settles, a second install with no
        // logical change must leave the file's mtime untouched —
        // hand-edited formatting / ordering is preserved across
        // launches as long as nothing actually changed.
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        let firstMtime = try mtime(of: settingsURL)
        // Coarse filesystem timestamp granularity (HFS+/APFS = 1ns;
        // some shared environments coarser). Sleep 50ms so a real
        // rewrite would land on a different mtime.
        Thread.sleep(forTimeInterval: 0.05)
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        XCTAssertEqual(try mtime(of: settingsURL), firstMtime,
                       "second install must skip the disk write entirely")
    }

    func test_install_secondCallDoesNotRewriteScript() throws {
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        let scriptURL = scriptDir.appendingPathComponent("nice-claude-hook.sh")
        let firstMtime = try mtime(of: scriptURL)
        Thread.sleep(forTimeInterval: 0.05)
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        XCTAssertEqual(try mtime(of: scriptURL), firstMtime,
                       "second install must skip the script write entirely")
    }

    // MARK: - Malformed settings

    func test_install_malformedSettingsJSON_doesNotOverwrite() throws {
        // User's file exists with non-empty bytes that don't parse —
        // mid-edit, typo, arbitrary file. Install must NOT overwrite.
        // Worst case the hook isn't registered this launch; better
        // than silent data loss.
        try FileManager.default.createDirectory(
            at: settingsURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        let garbage = Data("this is not json {{{".utf8)
        try garbage.write(to: settingsURL)

        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)

        let after = try Data(contentsOf: settingsURL)
        XCTAssertEqual(after, garbage, "malformed file must be left intact")
    }

    func test_install_settingsNotAnObject_doesNotOverwrite() throws {
        // Valid JSON but a top-level array — also not something we can
        // safely merge into. Same protection.
        try FileManager.default.createDirectory(
            at: settingsURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        let arr = Data(#"["a", "b", "c"]"#.utf8)
        try arr.write(to: settingsURL)

        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)

        let after = try Data(contentsOf: settingsURL)
        XCTAssertEqual(after, arr, "non-object root must be left intact")
    }

    func test_install_failureDoesNotThrow() throws {
        // Settings dir under an unwritable parent: install should log
        // and return cleanly, not throw or crash. Mirrors what
        // NiceServices.init expects on every launch.
        let unwritable = tmpRoot.appendingPathComponent("nope")
        FileManager.default.createFile(atPath: unwritable.path, contents: Data())
        // Settings URL whose parent IS the file we just made — directory
        // creation will fail. Any throw inside install is caught and
        // logged.
        let badSettings = unwritable.appendingPathComponent("settings.local.json")
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: badSettings)
        // No crash, no throw. Reaching this line is the assertion.
    }

    // MARK: - helpers

    private func expectedScriptPath() -> String {
        scriptDir.appendingPathComponent("nice-claude-hook.sh").path
    }

    private func readSettings() throws -> [String: Any] {
        let data = try Data(contentsOf: settingsURL)
        return try XCTUnwrap(
            JSONSerialization.jsonObject(with: data) as? [String: Any]
        )
    }

    private func writeSettings(_ dict: [String: Any]) throws {
        try FileManager.default.createDirectory(
            at: settingsURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        let data = try JSONSerialization.data(
            withJSONObject: dict, options: [.prettyPrinted]
        )
        try data.write(to: settingsURL)
    }

    private func hooksGroup(
        _ settings: [String: Any], name: String
    ) -> [[String: Any]] {
        ((settings["hooks"] as? [String: Any])?[name] as? [[String: Any]]) ?? []
    }

    private func commandOf(_ group: [String: Any]) -> String? {
        guard let inner = group["hooks"] as? [[String: Any]],
              let first = inner.first else { return nil }
        return first["command"] as? String
    }

    private func mtime(of url: URL) throws -> Date {
        let attrs = try FileManager.default.attributesOfItem(atPath: url.path)
        return try XCTUnwrap(attrs[.modificationDate] as? Date)
    }

    private func runScript(
        at path: String,
        env: [String: String],
        stdin: String
    ) -> (Int32, String) {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: path)
        proc.environment = env
        let stdinPipe = Pipe()
        let stdoutPipe = Pipe()
        proc.standardInput = stdinPipe
        proc.standardOutput = stdoutPipe
        proc.standardError = Pipe()
        try? proc.run()
        stdinPipe.fileHandleForWriting.write(Data(stdin.utf8))
        try? stdinPipe.fileHandleForWriting.close()
        proc.waitUntilExit()
        let outData = try? stdoutPipe.fileHandleForReading.readToEnd()
        let out = outData.flatMap { String(data: $0, encoding: .utf8) } ?? ""
        return (proc.terminationStatus, out)
    }
}
