//
//  MainTerminalShellInjectTests.swift
//  NiceUnitTests
//
//  Verifies the ZDOTDIR stubs `MainTerminalShellInject.make()` writes
//  for the Main Terminal (and companion terminal panes). The `.zshrc`
//  body is a ~100-line shell script that does the JSON socket handshake
//  with Nice's control socket — a regression here silently breaks
//  "typing `claude` opens a new tab."
//

import Foundation
import XCTest
@testable import Nice

final class MainTerminalShellInjectTests: XCTestCase {

    // MARK: - File layout

    func test_make_createsAllFourStubs() throws {
        let dir = try MainTerminalShellInject.make()
        addTeardownBlock {
            try? FileManager.default.removeItem(at: dir)
        }

        let fm = FileManager.default
        for name in [".zshenv", ".zprofile", ".zlogin", ".zshrc"] {
            let url = dir.appendingPathComponent(name)
            XCTAssertTrue(fm.fileExists(atPath: url.path),
                          "Expected ZDOTDIR to contain \(name)")
        }
    }

    func test_make_usesPidInPath() throws {
        let dir = try MainTerminalShellInject.make()
        addTeardownBlock { try? FileManager.default.removeItem(at: dir) }

        let expectedSuffix = "nice-zdotdir-\(getpid())"
        XCTAssertEqual(dir.lastPathComponent, expectedSuffix,
                       "ZDOTDIR path should be namespaced by pid to avoid cross-process collisions.")
    }

    // MARK: - Chain-back stubs

    /// Every stub must first source the user's real dotfile — without
    /// this, setting ZDOTDIR loses the user's PATH, aliases, plugins,
    /// and completions.
    func test_chainBacks_sourceRealHomeDotfiles() throws {
        let dir = try MainTerminalShellInject.make()
        addTeardownBlock { try? FileManager.default.removeItem(at: dir) }

        for (filename, sourced) in [
            (".zshenv", ".zshenv"),
            (".zprofile", ".zprofile"),
            (".zlogin", ".zlogin"),
            (".zshrc", ".zshrc"),
        ] {
            let body = try String(
                contentsOf: dir.appendingPathComponent(filename), encoding: .utf8
            )
            XCTAssertTrue(
                body.contains(#"source "$HOME/\#(sourced)""#),
                "\(filename) must source $HOME/\(sourced) so the user keeps their PATH/aliases/plugins.")
        }
    }

    // MARK: - .zshrc shell wrapper contract

    private func readZshrc() throws -> String {
        let dir = try MainTerminalShellInject.make()
        addTeardownBlock { try? FileManager.default.removeItem(at: dir) }
        return try String(
            contentsOf: dir.appendingPathComponent(".zshrc"), encoding: .utf8
        )
    }

    func test_zshrc_definesClaudeFunction() throws {
        let body = try readZshrc()
        XCTAssertTrue(body.contains("claude() {"),
                      "zshrc must shadow `claude` with a function so the wrapper intercepts interactive invocations.")
    }

    func test_zshrc_definesJsonEscapeHelper() throws {
        let body = try readZshrc()
        XCTAssertTrue(body.contains("_nice_json_escape()"),
                      "JSON escape helper is required — payload must survive arbitrary args.")
        // The escape helper must handle backslashes, quotes, and the
        // three ASCII whitespace runs (\n, \r, \t). Omitting any one
        // produces malformed JSON that the Nice socket rejects.
        XCTAssertTrue(body.contains(#"s=${s//\\/\\\\}"#),
                      "escape must replace backslashes first.")
        XCTAssertTrue(body.contains(#"s=${s//\"/\\\"}"#),
                      "escape must replace double quotes.")
        XCTAssertTrue(body.contains("$'\\n'"),
                      "escape must handle embedded newlines.")
    }

    func test_zshrc_handshakePayloadShape() throws {
        let body = try readZshrc()
        // The control socket demands these four keys in this order.
        // Any drift here and Nice's JSON decoder rejects the handshake
        // and falls back to running claude in place.
        XCTAssertTrue(
            body.contains(#""action":"claude""#) || body.contains(#"\"action\":\"claude\""#),
            "payload must label itself as the claude action.")
        XCTAssertTrue(body.contains(#"cwd"#), "payload must include cwd.")
        XCTAssertTrue(body.contains(#"args"#), "payload must include args.")
        XCTAssertTrue(body.contains(#"tabId"#), "payload must include tabId.")
        XCTAssertTrue(body.contains(#"paneId"#), "payload must include paneId.")
    }

    func test_zshrc_usesNcWithSocketPath() throws {
        let body = try readZshrc()
        XCTAssertTrue(body.contains(#"nc -U "$NICE_SOCKET""#),
                      "must speak AF_UNIX to Nice's control socket via nc -U.")
    }

    func test_zshrc_dispatchesNewtabAndInplaceModes() throws {
        let body = try readZshrc()
        XCTAssertTrue(body.contains("newtab)"),
                      "wrapper must handle the `newtab` mode — Nice opened a sidebar tab, shell returns.")
        XCTAssertTrue(body.contains("inplace)"),
                      "wrapper must handle the `inplace` mode — Nice promoted this pane to Claude.")
        XCTAssertTrue(body.contains(#"exec command claude --session-id "$sid""#),
                      "inplace with a minted session id must exec claude --session-id so Nice can resume it later.")
    }

    func test_zshrc_socketUnreachableFallsBackToCommand() throws {
        let body = try readZshrc()
        XCTAssertTrue(body.contains("control socket unreachable"),
                      "must warn when the socket is gone.")
        XCTAssertTrue(body.contains(#"exec command claude "$@""#),
                      "unreachable socket must fall back to running claude directly, not swallow the command.")
    }

    func test_zshrc_nonInteractiveFlagsShortCircuitToCommand() throws {
        let body = try readZshrc()
        // These flags make claude non-interactive (-p / --print) or
        // request help-style output. Handshaking for them would trap
        // output inside a spawned tab the user never sees.
        for flag in ["-p", "--print", "-h", "--help", "--version", "--output-format"] {
            XCTAssertTrue(body.contains(flag),
                          "non-interactive flag \(flag) must be short-circuited to `command claude`.")
        }
    }

    func test_zshrc_nonInteractiveSubcommandsShortCircuit() throws {
        let body = try readZshrc()
        // Subcommands that print to stdout or manage local state; they
        // should never open a new tab.
        for sub in ["mcp", "config", "migrate-installer", "update", "doctor"] {
            XCTAssertTrue(body.contains(sub),
                          "non-interactive subcommand \(sub) must be short-circuited.")
        }
    }

    func test_zshrc_prefillCommandUsesPrintZ() throws {
        let body = try readZshrc()
        // Deferred-resume path: `print -z` pushes onto the line editor
        // so the user sees `claude --resume <uuid>` typed but nothing
        // runs until they hit Enter.
        XCTAssertTrue(body.contains(#"print -z "$NICE_PREFILL_COMMAND""#),
                      "restored Claude tabs rely on print -z to pre-type the resume command without executing it.")
    }

    func test_zshrc_noHandshakeWhenSocketUnset() throws {
        let body = try readZshrc()
        // Tabs spawned outside of Nice (user's own Terminal.app) must
        // pass through unchanged.
        XCTAssertTrue(body.contains(#"if [[ -z "$NICE_SOCKET" ]]"#),
                      "missing NICE_SOCKET must bypass the wrapper entirely.")
    }

    // MARK: - OSC 7 cwd-update emitter

    func test_zshrc_definesOsc7Emitter() throws {
        let body = try readZshrc()
        XCTAssertTrue(body.contains("_nice_emit_cwd_osc7()"),
                      "zshrc must define the OSC 7 emitter so cwd updates flow back to the host.")
    }

    func test_zshrc_emitterHooksIntoChpwdFunctions() throws {
        let body = try readZshrc()
        // chpwd is the per-cd hook. Without this registration, OSC 7
        // never fires after the initial shell start and panes restore
        // to their spawn cwd instead of where the user left them.
        XCTAssertTrue(
            body.contains("chpwd_functions+=(_nice_emit_cwd_osc7)"),
            "emitter must append to chpwd_functions to fire on every cd."
        )
    }

    func test_zshrc_emitterFiresOnceAtShellStart() throws {
        let body = try readZshrc()
        // The hook only fires on cd, not on initial shell start, so
        // an explicit call at the bottom of zshrc captures the spawn
        // cwd before the user has typed anything. The call must be
        // a plain statement (not the function definition or the
        // chpwd_functions registration).
        let plainCall = body.split(separator: "\n").contains { line in
            line.trimmingCharacters(in: .whitespaces) == "_nice_emit_cwd_osc7"
        }
        XCTAssertTrue(
            plainCall,
            "emitter must be invoked as a bare statement somewhere in zshrc to capture spawn cwd."
        )
    }

    func test_zshrc_percentEscapeIsLiteralPattern() throws {
        let body = try readZshrc()
        // A bare `%` in zsh's `${var//pattern/repl}` is the "anchor at
        // end of string" matcher — it would replace the empty position
        // at the end with `%25`, corrupting every persisted cwd. The
        // backslash escape forces literal interpretation.
        // Regression: this exact bug shipped once and persisted bogus
        // paths like "/Users/nick%" until the next launch.
        //
        // Find the actual substitution line (the one starting with
        // `local p=`) and assert on it directly — comments in the
        // surrounding script may legitimately mention the bare form
        // when explaining the bug.
        let assignLine = body.split(separator: "\n").first { line in
            line.contains("local p=") && line.contains("PWD")
        }
        let assign = String(assignLine ?? "")
        XCTAssertTrue(
            assign.contains(#"${PWD//\%/%25}"#),
            "% in the substitution pattern must be backslash-escaped. Got: <\(assign)>"
        )
        XCTAssertFalse(
            assign.contains(#"${PWD//%/%25}"#),
            "bare `%` in the substitution line is the end-of-string anchor. Got: <\(assign)>"
        )
    }

    func test_zshrc_emitterFormat_isOsc7FileUrl() throws {
        let body = try readZshrc()
        // OSC 7 wire format. `\a` (BEL) is the terminator — SwiftTerm
        // accepts it (and ST), and BEL is easier to embed in printf.
        XCTAssertTrue(
            body.contains(#"printf '\e]7;file://%s%s\a'"#),
            "emitter must produce a well-formed OSC 7 file:// URL terminated with BEL."
        )
    }

    /// End-to-end: write the zshrc to disk, launch a real zsh against
    /// it, and confirm the OSC 7 payload contains the actual cwd
    /// without spurious bytes. This is the test that would have caught
    /// the `%` regression — the contract assertions above are belt &
    /// suspenders for the case where someone refactors the script and
    /// changes the substitution form.
    func test_zshrc_emitterProducesCleanOsc7AtRuntime() throws {
        let dir = try MainTerminalShellInject.make()
        addTeardownBlock { try? FileManager.default.removeItem(at: dir) }

        // Run inside a sandbox cwd we control so the captured payload
        // is predictable. Use a no-percent, no-space path so the
        // emitter's encoding can't hide the bug.
        let workCwd = NSTemporaryDirectory() + "nice-osc7-test-\(getpid())"
        try FileManager.default.createDirectory(
            atPath: workCwd, withIntermediateDirectories: true
        )
        addTeardownBlock {
            try? FileManager.default.removeItem(atPath: workCwd)
        }

        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/bin/zsh")
        proc.arguments = ["-ic", "exit"]
        proc.environment = [
            "ZDOTDIR": dir.path,
            "HOME": NSHomeDirectory(),
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
            "HOST": "test.local",
            // Make $PWD predictable; zsh inherits cwd via chdir below.
        ]
        proc.currentDirectoryURL = URL(fileURLWithPath: workCwd)
        let stdout = Pipe()
        proc.standardOutput = stdout
        proc.standardError = Pipe()
        try proc.run()
        proc.waitUntilExit()

        let data = (try? stdout.fileHandleForReading.readToEnd()) ?? Data()
        let bytes = [UInt8](data)

        // Locate `ESC ] 7 ;` and the next BEL. The payload between
        // them is what SwiftTerm would parse.
        guard let oscStart = findSubsequence(bytes, [0x1b, 0x5d, 0x37, 0x3b])
        else {
            XCTFail("zsh did not emit OSC 7. Captured: \(prettyDump(data))")
            return
        }
        let payloadStart = oscStart + 4
        guard let bel = bytes[payloadStart...].firstIndex(of: 0x07) else {
            XCTFail("OSC 7 emission missing BEL terminator. Captured: \(prettyDump(data))")
            return
        }
        let payload = String(
            bytes: bytes[payloadStart..<bel], encoding: .utf8
        ) ?? ""

        // The payload must parse as a file:// URL whose path matches
        // the cwd. URL(string:).path also covers SwiftTerm's contract
        // (it stores the raw payload; Nice's bridge runs URL(string:)
        // when forwarding to AppState).
        XCTAssertTrue(
            payload.hasPrefix("file://"),
            "OSC 7 payload must be a file:// URL. Got: <\(payload)>"
        )
        let parsed = URL(string: payload)
        XCTAssertNotNil(parsed, "Payload must parse as a URL. Got: <\(payload)>")
        // Tolerate /private/var/... vs /var/... symlink resolution
        // differences on macOS — only the trailing path component
        // needs to match the real cwd.
        let lastComponent = (workCwd as NSString).lastPathComponent
        XCTAssertTrue(
            (parsed?.path ?? "").hasSuffix(lastComponent),
            "Payload path must end with the cwd's last component (\(lastComponent)). Got: <\(parsed?.path ?? "")>"
        )
        // Sentinel for the `%` regression: any literal `%` in the
        // decoded path means our zsh substitution leaked an end-of-
        // string anchor match.
        XCTAssertFalse(
            (parsed?.path ?? "").contains("%"),
            "Decoded path must not contain `%`. Got: <\(parsed?.path ?? "")>"
        )
    }

    // MARK: - Helpers

    private func findSubsequence(_ haystack: [UInt8], _ needle: [UInt8]) -> Int? {
        guard needle.count <= haystack.count else { return nil }
        for i in 0...(haystack.count - needle.count) {
            if Array(haystack[i..<(i + needle.count)]) == needle {
                return i
            }
        }
        return nil
    }

    private func prettyDump(_ data: Data) -> String {
        let printable = String(data: data, encoding: .utf8) ?? "<non-utf8>"
        return "\(printable.prefix(200))..."
    }
}
