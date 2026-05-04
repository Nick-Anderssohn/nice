//
//  ClaudeHookInstallerTests.swift
//  NiceUnitTests
//
//  Tests lock down the install / merge behavior that determines whether
//  claude actually routes SessionStart payloads back to Nice:
//  - script lands under the configured dir, is executable, and forwards
//    `session_id` only when `source` is a rotation event
//    (clear/compact/branch); silent on startup/resume so we don't churn
//    the persistence layer with redundant updates
//  - settings.json merges cleanly with pre-existing hooks (user's own
//    hooks preserved; Nice's entry added once under the SessionStart
//    group key)
//  - reinstall is idempotent — no duplicate entries, no churn writes
//  - malformed settings.json bails out instead of silently overwriting
//    the user's content
//
//  Tests pass paths directly via the `install(scriptDir:settingsURL:)`
//  surface — no env-var dance, no process-global state to race under
//  parallel testing, no risk of pollution if the test process inherits
//  a real `~/.claude/`.
//

import Darwin
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

    func test_install_scriptExitsZero_whenSocketUnreachable() throws {
        // Script must fail-open (exit 0) when NICE_SOCKET points at a
        // dead path — claude's hook timeout would otherwise punish the
        // user. End-to-end forwarding is covered by the source-gating
        // tests below, which run a real listener.
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        let scriptURL = scriptDir.appendingPathComponent("nice-claude-hook.sh")
        let payload = sessionStartPayload(source: "clear", sessionId: UUID().uuidString)

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

    // MARK: - Source forwarding
    //
    // The script forwards on every SessionStart source rather than
    // gating client-side. `/branch` reports `source: "resume"` in
    // current Claude Code, so a resume-excluding gate would silently
    // drop branch rotations. `/compact` reports the same id (no
    // rotation), but the receiver's `if newId != claudeSessionId`
    // short-circuit makes those redundant forwards a no-op. The
    // tradeoff: one extra socket round-trip per session start, vs.
    // robustness against Claude introducing new sources or
    // re-mapping existing ones.

    func test_script_forwardsEverySessionStartSource() throws {
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        let scriptURL = scriptDir.appendingPathComponent("nice-claude-hook.sh")

        // Socket path lives in /tmp (not the test's tmpRoot under
        // /var/folders/...) because macOS sun_path is 104 bytes and
        // the temp-folder prefix alone is over 100 chars — bind would
        // truncate and collide. Per-test UUID suffix keeps parallel
        // suites isolated.
        let socketPath = "/tmp/nice-hook-test-\(UUID().uuidString.prefix(8)).sock"
        let listener = try TestSocketListener(path: socketPath)
        defer { listener.cleanup() }

        // Every documented SessionStart source. All must forward.
        // The receiver de-dupes by comparing against the tab's stored
        // id, so forwarding the same id twice is a true no-op.
        let sources = ["startup", "resume", "clear", "compact", "branch"]

        for (i, source) in sources.enumerated() {
            let sessionId = UUID().uuidString.lowercased()
            let payload = sessionStartPayload(source: source, sessionId: sessionId)

            let captured = captureFirstLine(
                listener: listener,
                runScript: {
                    let (exit, _) = self.runScript(
                        at: scriptURL.path,
                        env: [
                            "NICE_SOCKET": socketPath,
                            "NICE_PANE_ID": "pane-\(i)",
                        ],
                        stdin: payload
                    )
                    XCTAssertEqual(exit, 0,
                                   "script must exit 0 for source=\(source)")
                },
                timeout: 1.5
            )

            let line = try XCTUnwrap(
                captured,
                "source=\(source) must forward a session_update"
            )
            XCTAssertTrue(line.contains("\"action\":\"session_update\""),
                          "forwarded payload missing action: \(line)")
            XCTAssertTrue(line.contains("\"sessionId\":\"\(sessionId)\""),
                          "forwarded payload missing sessionId: \(line)")
            XCTAssertTrue(line.contains("\"paneId\":\"pane-\(i)\""),
                          "forwarded payload missing paneId: \(line)")
            // The source field is load-bearing for /branch detection in
            // SessionsModel.handleClaudeSessionUpdate. Forwarding it
            // verbatim lets the receiver distinguish a /branch
            // (source=resume + id-change) from a /clear with the same
            // shape.
            XCTAssertTrue(line.contains("\"source\":\"\(source)\""),
                          "forwarded payload missing source=\(source): \(line)")
        }
    }

    func test_script_forwardsEmptySource_whenPayloadOmitsField() throws {
        // Defensive: a future Claude version that drops the `source`
        // field (or a malformed payload that omits it) must not break
        // the script. The sed regex falls through to an empty match,
        // and the receiver normalizes "" back to nil — which the
        // handler treats as "id-update only, no parent tab" rather
        // than misclassifying as a /branch.
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        let scriptURL = scriptDir.appendingPathComponent("nice-claude-hook.sh")

        let socketPath = "/tmp/nice-hook-test-\(UUID().uuidString.prefix(8)).sock"
        let listener = try TestSocketListener(path: socketPath)
        defer { listener.cleanup() }

        let sessionId = UUID().uuidString.lowercased()
        // SessionStart payload with NO source field at all — what
        // older Claude versions sent before `source` was added.
        let payload = #"""
        {"hook_event_name":"SessionStart","session_id":"\#(sessionId)","cwd":"/private/tmp"}
        """#

        let captured = captureFirstLine(
            listener: listener,
            runScript: {
                let (exit, _) = self.runScript(
                    at: scriptURL.path,
                    env: [
                        "NICE_SOCKET": socketPath,
                        "NICE_PANE_ID": "pane-x",
                    ],
                    stdin: payload
                )
                XCTAssertEqual(exit, 0)
            },
            timeout: 1.5
        )

        let line = try XCTUnwrap(
            captured,
            "missing source must still forward the session_update"
        )
        XCTAssertTrue(line.contains("\"sessionId\":\"\(sessionId)\""))
        XCTAssertTrue(line.contains("\"source\":\"\""),
                      "missing source serializes as empty string for the receiver to normalize: \(line)")
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
        let ss = hooksGroup(settings, name: ClaudeHookInstaller.hookEventName)
        XCTAssertEqual(ss.count, 1)
        XCTAssertEqual(commandOf(ss[0]), expectedScriptPath())
    }

    func test_install_preservesUserHooks() throws {
        // User's existing settings include unrelated event groups
        // (UserPromptSubmit, PreToolUse) and a sibling under
        // SessionStart. Nice's entry is added alongside, never
        // replacing user content.
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
                "SessionStart": [
                    [
                        "hooks": [
                            ["type": "command", "command": "/user/own/session-start.sh"],
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
        let ss = hooksGroup(settings, name: ClaudeHookInstaller.hookEventName)
        XCTAssertEqual(ss.count, 2)
        XCTAssertEqual(commandOf(ss[0]), "/user/own/session-start.sh",
                       "user's existing SessionStart hook must be preserved")
        XCTAssertEqual(commandOf(ss[1]), expectedScriptPath(),
                       "Nice's entry must be appended after the user's")
        let pre = hooksGroup(settings, name: "PreToolUse")
        XCTAssertEqual(pre.count, 1)
        XCTAssertEqual(commandOf(pre[0]), "/user/pre-tool.sh")
        let ups = hooksGroup(settings, name: "UserPromptSubmit")
        XCTAssertEqual(ups.count, 1, "user's UserPromptSubmit hook untouched")
        XCTAssertEqual(commandOf(ups[0]), "/user/own/hook.sh")
    }

    func test_install_preservesMultipleSessionStartGroups() throws {
        // Two pre-existing SessionStart groups, each with their own
        // hook. After install both must still be present (in order)
        // and Nice's entry must be appended last as a third group.
        let userSettings: [String: Any] = [
            "hooks": [
                "SessionStart": [
                    ["hooks": [["type": "command", "command": "/first.sh"]]],
                    ["hooks": [["type": "command", "command": "/second.sh"]]],
                ],
            ],
        ]
        try writeSettings(userSettings)

        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)

        let ss = hooksGroup(try readSettings(),
                            name: ClaudeHookInstaller.hookEventName)
        XCTAssertEqual(ss.count, 3)
        XCTAssertEqual(commandOf(ss[0]), "/first.sh")
        XCTAssertEqual(commandOf(ss[1]), "/second.sh")
        XCTAssertEqual(commandOf(ss[2]), expectedScriptPath())
    }

    func test_install_preservesSiblingHooksInsideSameGroup() throws {
        // A single SessionStart group with multiple inner `hooks`
        // entries — none of them ours. The dedup check at
        // `mergeHookSettings` walks `inner.contains`, so this
        // exercises the inner walk.
        let userSettings: [String: Any] = [
            "hooks": [
                "SessionStart": [
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

        let ss = hooksGroup(try readSettings(),
                            name: ClaudeHookInstaller.hookEventName)
        XCTAssertEqual(ss.count, 2,
                       "Nice appended as a new group, not folded into the user's")
        // First group: user's two hooks intact.
        let firstInner = ss[0]["hooks"] as? [[String: Any]] ?? []
        XCTAssertEqual(firstInner.count, 2)
        XCTAssertEqual(firstInner[0]["command"] as? String, "/a.sh")
        XCTAssertEqual(firstInner[1]["command"] as? String, "/b.sh")
        XCTAssertEqual(commandOf(ss[1]), expectedScriptPath())
    }

    func test_install_isIdempotent() throws {
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        let ss = hooksGroup(try readSettings(),
                            name: ClaudeHookInstaller.hookEventName)
        XCTAssertEqual(ss.count, 1, "repeated install must not duplicate entries")
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

    // MARK: - Path invariant
    //
    // Claude's hook runner word-splits the command string, so any space
    // in the path would silently break hook execution (the comment at
    // ClaudeHookInstaller.swift:204-210 explains why). Pin both the
    // dotdir name and the installed script filename so a future rename
    // that adds a space surfaces here instead of breaking session
    // tracking in production.

    func test_defaultScriptDir_lastPathComponent_hasNoSpaces() {
        let component = ClaudeHookInstaller.defaultScriptDir().lastPathComponent
        XCTAssertFalse(
            component.contains(" "),
            "defaultScriptDir's tail must have no spaces — claude's hook runner would word-split."
        )
    }

    func test_install_writesScriptWithNoSpacesInName() throws {
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        let entries = try FileManager.default
            .contentsOfDirectory(atPath: scriptDir.path)
        let shFiles = entries.filter { $0.hasSuffix(".sh") }
        XCTAssertEqual(shFiles.count, 1, "expected exactly one installed script")
        XCTAssertFalse(
            shFiles[0].contains(" "),
            "installed script filename must have no spaces — claude's hook runner would word-split."
        )
    }

    // MARK: - Hook removal pinning
    //
    // If the user manually deletes Nice's entry from settings.json (no
    // tooling, just edits the file), the next Nice launch silently
    // re-adds it. This is intentional — Nice can't tell the difference
    // between "user removed" and "settings.json was never written" so
    // it always re-installs. Pin that contract.

    func test_install_reAddsEntry_whenUserRemovedItButKeptOtherContent() throws {
        // Step 1: initial install lands Nice's entry.
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        var settings = try readSettings()
        XCTAssertEqual(
            hooksGroup(settings, name: ClaudeHookInstaller.hookEventName).count, 1
        )

        // Step 2: user manually rewrites settings.json — Nice's entry
        // gone, but other content preserved (e.g. a sibling SessionStart
        // hook the user added between launches).
        settings["someUserKey"] = "preserve"
        settings["hooks"] = [
            ClaudeHookInstaller.hookEventName: [
                ["hooks": [["type": "command", "command": "/user/keep.sh"]]],
            ],
        ]
        try writeSettings(settings)

        // Step 3: relaunch (== second install). Nice's entry must come
        // back AND the user's other content must survive intact.
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)

        let after = try readSettings()
        XCTAssertEqual(after["someUserKey"] as? String, "preserve",
                       "non-hook user content must survive the re-install")
        let ss = hooksGroup(after, name: ClaudeHookInstaller.hookEventName)
        XCTAssertEqual(ss.count, 2, "user's hook + re-added Nice hook")
        XCTAssertEqual(commandOf(ss[0]), "/user/keep.sh",
                       "user's sibling hook must still be present")
        XCTAssertEqual(commandOf(ss[1]), expectedScriptPath(),
                       "Nice's entry must be re-added")
    }

    // MARK: - UserPromptSubmit migration
    //
    // Pre-952865c Nice builds registered the hook under
    // UserPromptSubmit. The current installer adds the new SessionStart
    // entry and ALSO strips leftover UPS entries pointing at our
    // script path so upgraders don't carry the redundant registration
    // forever. User-authored UPS hooks are preserved; empty groups
    // are dropped; the UPS key is removed entirely if it ends up empty.

    func test_install_doesNotCreateEmptyUserPromptSubmitKey() throws {
        // Settings.json with no UserPromptSubmit key at all. After
        // install the cleanup pass must not invent an empty UPS key
        // just to leave it dangling.
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        let settings = try readSettings()
        let allHooks = settings["hooks"] as? [String: Any] ?? [:]
        XCTAssertNil(allHooks[ClaudeHookInstaller.legacyHookEventName],
                     "no UserPromptSubmit key should be created when none existed")
    }

    func test_install_removesOurStaleUPSEntry_andDropsEmptyKey() throws {
        // Realistic minimal pre-952865c shape: the user's settings
        // contains exactly the registration the old installer wrote —
        // a single UPS group with our script as its only inner hook.
        // After install, UPS key is gone entirely (no empty husk).
        let stalePath = expectedScriptPath()
        let userSettings: [String: Any] = [
            "hooks": [
                ClaudeHookInstaller.legacyHookEventName: [
                    ["hooks": [["type": "command", "command": stalePath]]],
                ],
            ],
        ]
        try writeSettings(userSettings)

        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)

        let settings = try readSettings()
        let allHooks = try XCTUnwrap(settings["hooks"] as? [String: Any])
        XCTAssertNil(allHooks[ClaudeHookInstaller.legacyHookEventName],
                     "empty UserPromptSubmit key must be removed entirely")
        let ss = hooksGroup(settings, name: ClaudeHookInstaller.hookEventName)
        XCTAssertEqual(ss.count, 1)
        XCTAssertEqual(commandOf(ss[0]), expectedScriptPath())
    }

    func test_install_removesOurUPSEntry_keepsSiblingInSameGroup() throws {
        // A single UPS group with two inner hooks — ours and a
        // user-authored sibling. Cleanup must remove only ours; the
        // user's sibling stays inside the same group.
        let userSettings: [String: Any] = [
            "hooks": [
                ClaudeHookInstaller.legacyHookEventName: [
                    [
                        "hooks": [
                            ["type": "command", "command": expectedScriptPath()],
                            ["type": "command", "command": "/user/own.sh"],
                        ],
                    ],
                ],
            ],
        ]
        try writeSettings(userSettings)

        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)

        let ups = hooksGroup(try readSettings(),
                             name: ClaudeHookInstaller.legacyHookEventName)
        XCTAssertEqual(ups.count, 1, "user's group must be preserved")
        let inner = try XCTUnwrap(ups[0]["hooks"] as? [[String: Any]])
        XCTAssertEqual(inner.count, 1, "only Nice's inner entry should be removed")
        XCTAssertEqual(inner[0]["command"] as? String, "/user/own.sh")
    }

    func test_install_removesOurUPSGroup_keepsSeparateUserGroup() throws {
        // Two UPS groups — ours and the user's, each with a single
        // inner hook. Cleanup removes our entire group (it empties
        // out) but leaves the user's untouched.
        let userSettings: [String: Any] = [
            "hooks": [
                ClaudeHookInstaller.legacyHookEventName: [
                    ["hooks": [["type": "command", "command": expectedScriptPath()]]],
                    ["hooks": [["type": "command", "command": "/user/own.sh"]]],
                ],
            ],
        ]
        try writeSettings(userSettings)

        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)

        let ups = hooksGroup(try readSettings(),
                             name: ClaudeHookInstaller.legacyHookEventName)
        XCTAssertEqual(ups.count, 1, "only the user's group should remain")
        XCTAssertEqual(commandOf(ups[0]), "/user/own.sh")
    }

    func test_install_migratesFromUPSToSessionStart_realisticUpgrade() throws {
        // The realistic upgrade scenario: settings.json carries the
        // pre-952865c registration (UPS pointing at our script) plus
        // unrelated user content. After install: UPS gone,
        // SessionStart present, unrelated content intact. Then a
        // second install must be a true no-op (mtime unchanged) —
        // proves the migration is idempotent once it has run.
        let userSettings: [String: Any] = [
            "hooks": [
                ClaudeHookInstaller.legacyHookEventName: [
                    ["hooks": [["type": "command", "command": expectedScriptPath()]]],
                ],
            ],
            "someUserKey": "keep",
        ]
        try writeSettings(userSettings)

        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)

        let after = try readSettings()
        XCTAssertEqual(after["someUserKey"] as? String, "keep",
                       "unrelated user content must survive the migration")
        let allHooks = try XCTUnwrap(after["hooks"] as? [String: Any])
        XCTAssertNil(allHooks[ClaudeHookInstaller.legacyHookEventName],
                     "stale UserPromptSubmit registration must be gone")
        let ss = hooksGroup(after, name: ClaudeHookInstaller.hookEventName)
        XCTAssertEqual(ss.count, 1)
        XCTAssertEqual(commandOf(ss[0]), expectedScriptPath(),
                       "SessionStart entry must be present after migration")

        // Idempotency: a second install on the post-migration file
        // must skip the disk write entirely. Same pattern as
        // test_install_secondCallDoesNotRewriteSettings, but with a
        // file that exercised the migration on the first call.
        let firstMtime = try mtime(of: settingsURL)
        Thread.sleep(forTimeInterval: 0.05)
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        XCTAssertEqual(try mtime(of: settingsURL), firstMtime,
                       "second install after migration must not rewrite the file")
    }

    func test_install_cleansStaleUPS_whenSessionStartAlreadyPresent() throws {
        // Both states coexist on entry — could happen if a previous
        // install crashed mid-migration, the user hand-edited
        // settings.json, or some future second installer ran without
        // the cleanup. The cleanup must still run, AND the SessionStart
        // entry must not be duplicated. This pins the
        // alreadyHasSessionStart=true && hasStaleUPS=true branch
        // explicitly — the realistic upgrade test only exercises
        // alreadyHasSessionStart=false, so the early-out condition
        // could regress without a dedicated test here.
        let userSettings: [String: Any] = [
            "hooks": [
                ClaudeHookInstaller.hookEventName: [
                    ["hooks": [["type": "command", "command": expectedScriptPath()]]],
                ],
                ClaudeHookInstaller.legacyHookEventName: [
                    ["hooks": [["type": "command", "command": expectedScriptPath()]]],
                ],
            ],
        ]
        try writeSettings(userSettings)

        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)

        let after = try readSettings()
        let allHooks = try XCTUnwrap(after["hooks"] as? [String: Any])
        XCTAssertNil(allHooks[ClaudeHookInstaller.legacyHookEventName],
                     "stale UPS must be removed even when SessionStart already present")
        let ss = hooksGroup(after, name: ClaudeHookInstaller.hookEventName)
        XCTAssertEqual(ss.count, 1, "SessionStart entry must not be duplicated")
        XCTAssertEqual(commandOf(ss[0]), expectedScriptPath())
    }

    func test_install_removesOurUPSEntry_fromMiddle_preservesOrder() throws {
        // Our entry sandwiched between two user-authored siblings
        // inside a single group's inner array. Surviving siblings must
        // keep their original order — `removeAll` is order-preserving
        // in Swift, this test pins that behavior so a future switch
        // to a non-stable removal couldn't quietly reorder the user's
        // hooks.
        let userSettings: [String: Any] = [
            "hooks": [
                ClaudeHookInstaller.legacyHookEventName: [
                    [
                        "hooks": [
                            ["type": "command", "command": "/user/a.sh"],
                            ["type": "command", "command": expectedScriptPath()],
                            ["type": "command", "command": "/user/b.sh"],
                        ],
                    ],
                ],
            ],
        ]
        try writeSettings(userSettings)

        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)

        let ups = hooksGroup(try readSettings(),
                             name: ClaudeHookInstaller.legacyHookEventName)
        XCTAssertEqual(ups.count, 1)
        let inner = try XCTUnwrap(ups[0]["hooks"] as? [[String: Any]])
        XCTAssertEqual(inner.count, 2)
        XCTAssertEqual(inner[0]["command"] as? String, "/user/a.sh",
                       "first sibling must keep position 0")
        XCTAssertEqual(inner[1]["command"] as? String, "/user/b.sh",
                       "third sibling must move to position 1, in original order")
    }

    func test_install_removesOurUPSGroup_fromMiddle_preservesOrder() throws {
        // Our group sandwiched between two user-authored groups under
        // UserPromptSubmit. Surviving groups must keep their original
        // order — reverse-iteration with remove(at:) is order-preserving,
        // this test pins it so a future iteration-direction flip can't
        // silently reorder the user's hooks.
        let userSettings: [String: Any] = [
            "hooks": [
                ClaudeHookInstaller.legacyHookEventName: [
                    ["hooks": [["type": "command", "command": "/user/first.sh"]]],
                    ["hooks": [["type": "command", "command": expectedScriptPath()]]],
                    ["hooks": [["type": "command", "command": "/user/last.sh"]]],
                ],
            ],
        ]
        try writeSettings(userSettings)

        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)

        let ups = hooksGroup(try readSettings(),
                             name: ClaudeHookInstaller.legacyHookEventName)
        XCTAssertEqual(ups.count, 2, "only our middle group should be removed")
        XCTAssertEqual(commandOf(ups[0]), "/user/first.sh",
                       "user's first group must stay first")
        XCTAssertEqual(commandOf(ups[1]), "/user/last.sh",
                       "user's last group must keep its relative order")
    }

    func test_install_preservesMalformedUPSValue() throws {
        // The UPS value isn't an array of groups — it's a string. A
        // shape we don't recognize. Install must NOT touch it. To
        // exercise the write path (so we know the value survives a
        // round-trip through serialize+deserialize, not just the
        // early-out), we leave SessionStart absent so a write is
        // genuinely needed.
        let userSettings: [String: Any] = [
            "hooks": [
                ClaudeHookInstaller.legacyHookEventName: "not an array, totally malformed",
            ],
        ]
        try writeSettings(userSettings)

        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)

        let after = try readSettings()
        let allHooks = try XCTUnwrap(after["hooks"] as? [String: Any])
        XCTAssertEqual(
            allHooks[ClaudeHookInstaller.legacyHookEventName] as? String,
            "not an array, totally malformed",
            "malformed UPS value must round-trip verbatim — cleanup is a no-op for unrecognized shapes"
        )
        let ss = hooksGroup(after, name: ClaudeHookInstaller.hookEventName)
        XCTAssertEqual(ss.count, 1, "SessionStart entry must still be added")
        XCTAssertEqual(commandOf(ss[0]), expectedScriptPath())
    }

    func test_install_preservesMalformedInnerHooksShape_alongsideCleanup() throws {
        // Two UPS groups: one has a malformed `"hooks"` (a string
        // instead of an array of dicts), the other is a normal group
        // containing our entry. After install: the malformed group
        // passes through verbatim (the `guard ... else { continue }`
        // skips it without dropping it); our entry is removed from the
        // normal group; that group empties out and is dropped, leaving
        // the malformed group as the sole survivor under UPS.
        let userSettings: [String: Any] = [
            "hooks": [
                ClaudeHookInstaller.legacyHookEventName: [
                    ["hooks": "this is not an array of dicts"],
                    ["hooks": [["type": "command", "command": expectedScriptPath()]]],
                ],
            ],
        ]
        try writeSettings(userSettings)

        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)

        let ups = hooksGroup(try readSettings(),
                             name: ClaudeHookInstaller.legacyHookEventName)
        XCTAssertEqual(ups.count, 1, "malformed group must be preserved")
        XCTAssertEqual(ups[0]["hooks"] as? String,
                       "this is not an array of dicts",
                       "malformed inner hooks value must round-trip verbatim")
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

    /// Build a SessionStart-shape JSON payload as claude would emit it.
    /// The script's sed regexes target `source` and `session_id`; the
    /// other fields are present for shape realism but ignored.
    private func sessionStartPayload(source: String, sessionId: String) -> String {
        return #"""
        {"hook_event_name":"SessionStart","source":"\#(source)","session_id":"\#(sessionId)","cwd":"/private/tmp","transcript_path":"/x.jsonl"}
        """#
    }

    /// Run `runScript` with a listener bound at the script's NICE_SOCKET
    /// path; return the first line the script wrote, or nil if nothing
    /// arrived within the timeout. The listener thread runs on a
    /// background queue so the main-thread script invocation can drive
    /// the connection.
    private func captureFirstLine(
        listener: TestSocketListener,
        runScript: () -> Void,
        timeout: TimeInterval = 2.0
    ) -> String? {
        let group = DispatchGroup()
        var captured: String?
        group.enter()
        DispatchQueue.global(qos: .userInitiated).async {
            captured = listener.acceptOne(timeout: timeout)
            group.leave()
        }
        runScript()
        // Even a "no forward" case must give the listener a moment to
        // confirm no connection arrived. Bound by `timeout` either way.
        _ = group.wait(timeout: .now() + timeout + 0.5)
        return captured
    }
}

/// Test-only Unix-domain socket listener. Binds at `path` on init,
/// listens with backlog 1, and accepts a single connection on demand
/// via `acceptOne(timeout:)`. Cleans up the socket file on deinit.
final class TestSocketListener {
    private let fd: Int32
    private let path: String

    init(path: String) throws {
        self.path = path
        // Best-effort: clear any stale file at the path so bind() can
        // succeed. A leftover regular file or socket would EADDRINUSE.
        try? FileManager.default.removeItem(atPath: path)

        fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else {
            throw NSError(domain: "TestSocketListener", code: Int(errno),
                          userInfo: [NSLocalizedDescriptionKey:
                                        "socket() failed: errno=\(errno)"])
        }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        path.withCString { cstr in
            withUnsafeMutableBytes(of: &addr.sun_path) { buf in
                let dst = buf.baseAddress!.assumingMemoryBound(to: CChar.self)
                strncpy(dst, cstr, buf.count - 1)
            }
        }
        let bindResult = withUnsafePointer(to: &addr) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                Darwin.bind(fd, sa,
                            socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard bindResult == 0 else {
            let e = errno
            Darwin.close(fd)
            throw NSError(domain: "TestSocketListener", code: Int(e),
                          userInfo: [NSLocalizedDescriptionKey:
                                        "bind(\(path)) failed: errno=\(e)"])
        }
        guard Darwin.listen(fd, 1) == 0 else {
            let e = errno
            Darwin.close(fd)
            throw NSError(domain: "TestSocketListener", code: Int(e),
                          userInfo: [NSLocalizedDescriptionKey:
                                        "listen() failed: errno=\(e)"])
        }
    }

    deinit {
        Darwin.close(fd)
    }

    /// Explicit teardown so callers can choose deterministic cleanup
    /// timing rather than relying on Swift's deinit ordering. Safe to
    /// call multiple times.
    func cleanup() {
        try? FileManager.default.removeItem(atPath: path)
    }

    /// Accept one connection and read until newline or EOF. Returns the
    /// first line (newline trimmed), or nil if no connection arrived
    /// before `timeout`. Blocking — call from a background queue.
    func acceptOne(timeout: TimeInterval) -> String? {
        var pfd = pollfd(fd: fd, events: Int16(POLLIN), revents: 0)
        let ms = Int32(timeout * 1000)
        let pollRc = withUnsafeMutablePointer(to: &pfd) {
            Darwin.poll($0, 1, ms)
        }
        guard pollRc > 0, pfd.revents & Int16(POLLIN) != 0 else {
            return nil
        }
        let clientFd = Darwin.accept(fd, nil, nil)
        guard clientFd >= 0 else { return nil }
        defer { Darwin.close(clientFd) }

        var buf = [UInt8](repeating: 0, count: 4096)
        var collected: [UInt8] = []
        while true {
            let n = Darwin.read(clientFd, &buf, buf.count)
            if n <= 0 { break }
            collected.append(contentsOf: buf[..<n])
            if collected.contains(0x0A) { break }
        }
        if let nl = collected.firstIndex(of: 0x0A) {
            collected = Array(collected[..<nl])
        }
        return String(decoding: collected, as: UTF8.self)
    }
}
