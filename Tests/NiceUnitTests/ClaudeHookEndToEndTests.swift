//
//  ClaudeHookEndToEndTests.swift
//  NiceUnitTests
//
//  End-to-end coverage of the SessionStart hook → control socket →
//  SessionsModel.handleClaudeSessionUpdate → TabModel.adoptTabCwd
//  chain. Every layer in that chain has unit-test coverage of its
//  own (script-side sed extraction in ClaudeHookInstallerTests,
//  socket-side parsing in NiceControlSocketTests, handler-side
//  dispatch in AppStateClaudeSessionUpdateTests, pane policy in
//  TabModelCwdResolutionTests). Each layer's tests stub the
//  adjacent layers, so a regression in a seam — the JSON contract
//  between script and socket, the MainActor hop in
//  `SessionsModel.startSocketListener`, or the receiver's empty-
//  string normalization for `cwd` — would slip through every per-
//  layer test without ever lighting a red.
//
//  The single test below runs the real installed shell script
//  against a real `NiceControlSocket` arming a real `SessionsModel`
//  attached to a real `AppState`. The assertion target is the
//  observable end state — `tab.cwd` after the hook fires — which
//  exercises every seam in one shot.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class ClaudeHookEndToEndTests: XCTestCase {

    private var homeSandbox: TestHomeSandbox!
    private var tmpRoot: URL!
    private var priorSocketPath: String?

    override func setUp() {
        super.setUp()
        homeSandbox = TestHomeSandbox()
        tmpRoot = URL(
            fileURLWithPath: NSTemporaryDirectory(), isDirectory: true
        )
        .appendingPathComponent("nice-hook-e2e-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(
            at: tmpRoot, withIntermediateDirectories: true
        )
        // Stash any pre-existing override so we restore exactly what
        // the parent test runner set. Most CI configs won't have one;
        // a local developer running with a custom socket path would.
        priorSocketPath = ProcessInfo.processInfo.environment["NICE_SOCKET_PATH"]
    }

    override func tearDown() {
        if let priorSocketPath {
            setenv("NICE_SOCKET_PATH", priorSocketPath, 1)
        } else {
            unsetenv("NICE_SOCKET_PATH")
        }
        try? FileManager.default.removeItem(at: tmpRoot)
        tmpRoot = nil
        homeSandbox.teardown()
        homeSandbox = nil
        super.tearDown()
    }

    /// Drive the full chain in one shot: install the shell script,
    /// bootstrap a real socket through `SessionsModel.bootstrapSocket`
    /// + `startSocketListener` (the exact pair `AppState.start()` runs
    /// in production), invoke the script with a SessionStart payload
    /// carrying a new worktree cwd, and wait for the resulting
    /// `tab.cwd` mutation to land via the MainActor-hop dispatch.
    ///
    /// What this catches that the per-layer tests don't:
    ///   • The script's outgoing JSON envelope (action, sessionId,
    ///     paneId, source, cwd keys) matching the socket parser's
    ///     `case "session_update"` extraction — a key rename on
    ///     either side breaks the splice but not the layer tests.
    ///   • The empty-string-to-nil normalization for cwd inside the
    ///     socket parser interacting correctly with the handler's
    ///     `guard let newCwd, !newCwd.isEmpty` short-circuit — both
    ///     layers test their slice in isolation; this case verifies
    ///     they line up on a real non-empty value.
    ///   • The MainActor hop in `startSocketListener`'s handler
    ///     wrapper (`Task { @MainActor in ... }`) actually delivering
    ///     to the model before the test's poll loop times out.
    ///   • `adoptTabCwd`'s pane-follow policy firing through the
    ///     real handler rather than a manual call — guards a refactor
    ///     that bypassed the adoption (e.g. by setting `tab.cwd`
    ///     directly) without lighting up the unit tests.
    func test_hookForwardsCwdAllTheWayToTabCwd() throws {
        // Per ClaudeHookInstallerTests:102-107: macOS sun_path is 104
        // bytes, and the per-user temp folder prefix alone is over 100
        // chars. Force the socket into /tmp so bind() doesn't silently
        // truncate. UUID suffix isolates parallel test runs.
        let socketPath = "/tmp/nice-hook-e2e-\(UUID().uuidString.prefix(8)).sock"
        setenv("NICE_SOCKET_PATH", socketPath, 1)

        // services == nil — AppState's convenience init disables
        // persistence and skips `start()`'s production-only paths
        // (NiceServices wiring, deferred restore). Sub-models are
        // fully constructed and the session-update callback chain
        // through `onSessionMutation → scheduleSessionSave` runs
        // (the save itself is a persistenceEnabled-gated no-op).
        let appState = AppState()
        defer { appState.sessions.tearDown() }

        // Seed a Claude tab so the handler has a paneId to find.
        // The new cwd must NOT equal the seed cwd or the handler's
        // `adoptTabCwd` short-circuits before any visible mutation —
        // which would also pass the test for the wrong reason.
        let seedCwd = "/tmp/nice-hook-e2e-old-\(UUID().uuidString.prefix(8))"
        let targetCwd = "/tmp/nice-hook-e2e-new-\(UUID().uuidString.prefix(8))"
        let seeded = TabModelFixtures.seedClaudeTab(
            into: appState.tabs,
            projectId: "p-e2e", tabId: "t-e2e",
            sessionId: "sid-pre",
            projectPath: seedCwd
        )

        // Bootstrap + arm the socket listener — the same pair
        // `AppState.start()` calls in production. We skip
        // `AppState.start()` itself because it also spawns a real
        // Main Terminal pty (and triggers restore I/O) which this
        // test doesn't need; the per-`start()` call sequence here is
        // the load-bearing slice.
        appState.sessions.bootstrapSocket(zdotdirPath: nil, userZDotDir: nil)
        appState.sessions.startSocketListener()

        // Sanity-check the env injection landed at the path we set.
        // If a future change rewires the socket-path allocator and
        // ignores `NICE_SOCKET_PATH`, the script's `nc -U` would race
        // against the wrong file and the test would flake instead of
        // failing cleanly here.
        XCTAssertEqual(
            appState.sessions.controlSocketExtraEnv["NICE_SOCKET"],
            socketPath,
            "NICE_SOCKET_PATH override must propagate into the socket env"
        )

        // Install the real hook script into our sandbox.
        let scriptDir = tmpRoot.appendingPathComponent(".nice")
        let settingsURL = tmpRoot.appendingPathComponent(".claude/settings.json")
        ClaudeHookInstaller.install(scriptDir: scriptDir, settingsURL: settingsURL)
        let scriptURL = scriptDir.appendingPathComponent("nice-claude-hook.sh")
        XCTAssertTrue(
            FileManager.default.fileExists(atPath: scriptURL.path),
            "installer must have written the hook script"
        )

        // Realistic SessionStart payload as Claude Code emits it.
        // `source: "resume"` plus a changed `session_id` would also
        // exercise the /branch materialization path; we deliberately
        // pick `clear` here so only the cwd-update slice fires.
        let newSessionId = UUID().uuidString.lowercased()
        let payload = #"""
        {"hook_event_name":"SessionStart","source":"clear","session_id":"\#(newSessionId)","cwd":"\#(targetCwd)","transcript_path":"/x.jsonl"}
        """#

        // Drive the hook script.
        let (exit, _) = runScript(
            at: scriptURL.path,
            env: [
                "NICE_SOCKET": socketPath,
                "NICE_PANE_ID": seeded.claudePaneId,
            ],
            stdin: payload
        )
        XCTAssertEqual(exit, 0, "hook script must exit 0")

        // Poll for the cwd update — the socket dispatches on a
        // background queue and the handler hops to MainActor via
        // `Task { @MainActor in ... }`. A fixed `Thread.sleep` would
        // either flake on a slow CI runner or pad happy-path time;
        // poll with a tight tick instead.
        let ok = waitFor(timeout: 1.5) {
            appState.tabs.tab(for: seeded.tabId)?.cwd == targetCwd
        }
        XCTAssertTrue(
            ok,
            "tab.cwd must update to the hook payload's cwd; got "
            + "\(appState.tabs.tab(for: seeded.tabId)?.cwd ?? "<nil>")"
        )
        // Session id rotation rides the same dispatch — if the
        // handler ran at all, this also flipped. Pinning it here
        // catches a future regression that delivers `cwd` only
        // (e.g. someone refactors `handleClaudeSessionUpdate` and
        // calls `updateTabCwd` without `updateClaudeSessionId`).
        XCTAssertEqual(
            appState.tabs.tab(for: seeded.tabId)?.claudeSessionId,
            newSessionId,
            "session id rotation must also land via the same dispatch"
        )
    }

    // MARK: - Helpers

    /// Run the installed shell script with the supplied env / stdin
    /// and return its exit status. Mirrors the helper of the same
    /// name in `ClaudeHookInstallerTests` — duplicated here rather
    /// than extracted because the two test classes live in the same
    /// target and the helper is short.
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

    /// Poll `condition` every 20 ms until it returns true or
    /// `timeout` elapses. Mirrors the same-named helper in
    /// `NiceControlSocketTests` — short enough that duplicating beats
    /// extracting into a shared test utility for one caller.
    private func waitFor(
        timeout: TimeInterval, condition: () -> Bool
    ) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            // Hand the runloop a tick so MainActor work scheduled
            // from the socket's background queue can drain.
            RunLoop.current.run(until: Date().addingTimeInterval(0.02))
        }
        return condition()
    }
}
