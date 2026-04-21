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
}
