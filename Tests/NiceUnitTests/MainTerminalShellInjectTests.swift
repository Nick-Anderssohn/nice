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

    /// `.zprofile`, `.zlogin`, and `.zshrc` chain back through
    /// `$NICE_USER_ZDOTDIR` — the env var the synthetic `.zshenv`
    /// resolves to whatever ZDOTDIR the user actually intended (their
    /// `~/.zshenv`-set custom path, or `$HOME` when they haven't
    /// customized). Sourcing through that var is what lets XDG-style
    /// zsh layouts (e.g. `~/.config/zsh`) work and what stops shell
    /// tools (p10k, oh-my-zsh, nvm…) from scribbling on our temp dir.
    func test_chainBacks_sourceFromUserZDotDir() throws {
        let dir = try MainTerminalShellInject.make()
        addTeardownBlock { try? FileManager.default.removeItem(at: dir) }

        // .zprofile and .zlogin source through $NICE_USER_ZDOTDIR
        // (still set when those stubs run). .zshrc is special — it
        // first stashes the resolved value into NICE_RESOLVED_USER_ZDOTDIR
        // and unsets NICE_USER_ZDOTDIR before sourcing user's .zshrc, so
        // its source line uses the resolved name.
        for (filename, varName) in [
            (".zprofile", "NICE_USER_ZDOTDIR"),
            (".zlogin", "NICE_USER_ZDOTDIR"),
            (".zshrc", "NICE_RESOLVED_USER_ZDOTDIR"),
        ] {
            let body = try String(
                contentsOf: dir.appendingPathComponent(filename), encoding: .utf8
            )
            let sourced = filename
            XCTAssertTrue(
                body.contains(#"source "$\#(varName)/\#(sourced)""#),
                "\(filename) must source $\(varName)/\(sourced) so XDG-style ZDOTDIR layouts and ~/.zshenv-set values are honored.")
        }
    }

    /// `.zshenv` is special: it discovers the user's intended ZDOTDIR
    /// (preferring `$NICE_USER_ZDOTDIR` from Nice's launch env, falling
    /// back to sourcing `~/.zshenv` ourselves), then restores `$ZDOTDIR`
    /// to our temp dir so zsh keeps reading our other stubs.
    func test_zshenv_discoversUserZDotDir() throws {
        let dir = try MainTerminalShellInject.make()
        addTeardownBlock { try? FileManager.default.removeItem(at: dir) }

        let body = try String(
            contentsOf: dir.appendingPathComponent(".zshenv"), encoding: .utf8
        )
        // Prefer the launch-env value when present.
        XCTAssertTrue(
            body.contains(#"if [[ -n "$NICE_USER_ZDOTDIR" ]]; then"#),
            ".zshenv must branch on NICE_USER_ZDOTDIR to honor launchctl/parent-set values.")
        // Fall back to sourcing ~/.zshenv to honor XDG-style layouts.
        XCTAssertTrue(
            body.contains(#"source "$HOME/.zshenv""#),
            ".zshenv must source ~/.zshenv as the fallback discovery path so users who set ZDOTDIR there are honored.")
        // Must restore $ZDOTDIR after discovery so zsh still reads our
        // .zprofile/.zshrc stubs (otherwise our injection unwinds early).
        XCTAssertTrue(
            body.contains(#"export ZDOTDIR="$NICE_TEMP_ZDOTDIR""#),
            ".zshenv must restore ZDOTDIR to our temp value so zsh keeps reading our stubs.")
        // Stash the resolved value for the later stubs.
        XCTAssertTrue(
            body.contains(#"export NICE_USER_ZDOTDIR="$USER_ZDOTDIR""#),
            ".zshenv must persist the resolved value back into NICE_USER_ZDOTDIR for .zprofile/.zshrc.")
    }

    /// `.zshrc` must restore `$ZDOTDIR` to the user's intended value
    /// BEFORE sourcing their `.zshrc` so init-time probes
    /// (`${ZDOTDIR:-$HOME}/.zcompdump`, etc.) resolve to the user's
    /// real path. The full source line then comes after, then our
    /// claude()/OSC 7 hooks layer on top.
    func test_zshrc_restoresUserZDotDirBeforeSourcing() throws {
        let body = try readZshrc()
        guard let restoreIdx = body.range(of: #"export ZDOTDIR="$NICE_RESOLVED_USER_ZDOTDIR""#)?.lowerBound,
              let sourceIdx = body.range(of: #"source "$NICE_RESOLVED_USER_ZDOTDIR/.zshrc""#)?.lowerBound,
              let claudeIdx = body.range(of: "claude() {")?.lowerBound
        else {
            XCTFail("Required markers missing from .zshrc body; structure regression."); return
        }
        XCTAssertTrue(
            restoreIdx < sourceIdx,
            ".zshrc must restore ZDOTDIR BEFORE sourcing user's .zshrc — init-time tools probe ${ZDOTDIR:-$HOME} during the source.")
        XCTAssertTrue(
            sourceIdx < claudeIdx,
            ".zshrc must source user's .zshrc BEFORE installing claude()/OSC 7 so our hooks layer on top of (and survive) anything the user defines.")
        XCTAssertTrue(
            body.contains("unset NICE_USER_ZDOTDIR"),
            ".zshrc must clear NICE_USER_ZDOTDIR so it doesn't leak to subprocesses.")
        // When the user's ZDOTDIR resolves to $HOME, prefer unsetting
        // ZDOTDIR (matching the standard convention that "no ZDOTDIR"
        // means "$HOME"). Trailing-slash normalization applied so
        // /Users/nick/ vs /Users/nick still take the unset branch.
        XCTAssertTrue(
            body.contains(#"if [[ "$NICE_RESOLVED_USER_ZDOTDIR" == "${HOME%/}" ]]; then"#)
                && body.contains("unset ZDOTDIR"),
            ".zshrc must unset (not export) ZDOTDIR when the resolved value matches $HOME.")
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

    // MARK: - End-to-end ZDOTDIR resolution
    //
    // These tests exercise the full .zshenv/.zprofile/.zshrc cooperation
    // by launching real zsh against a per-launch ZDOTDIR with various
    // user-side ZDOTDIR configurations. They are the regression test for
    // the original bug — Powerlevel10k / nvm / oh-my-zsh writing to our
    // temp dir instead of the user's real config — and also pin the
    // bonus fixes for ~/.zshenv-set ZDOTDIR and ~/.zprofile sourcing.

    /// Run zsh once inside the synthetic ZDOTDIR with a controlled HOME,
    /// return its stdout. `loginShell: true` runs `-ilc` so .zprofile /
    /// .zlogin lookups fire (mirrors users who configure Nice to spawn
    /// login shells). NICE_USER_ZDOTDIR is always set — empty string
    /// when nil — to faithfully match production, which always sets
    /// the var (the shell stub treats `[[ -n "" ]]` and `[[ -n
    /// <unset> ]]` identically, but pinning the empty-string contract
    /// keeps us honest if the stub is ever rewritten with `[[ -v ... ]]`).
    private func runZshUnderInjection(
        homeDir: String,
        niceUserZDotDir: String?,
        commands: String,
        loginShell: Bool = false
    ) throws -> String {
        let dir = try MainTerminalShellInject.make()
        addTeardownBlock { try? FileManager.default.removeItem(at: dir) }

        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/bin/zsh")
        proc.arguments = [loginShell ? "-ilc" : "-ic", commands]
        proc.environment = [
            "ZDOTDIR": dir.path,
            "HOME": homeDir,
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
            "HOST": "test.local",
            "NICE_USER_ZDOTDIR": niceUserZDotDir ?? "",
        ]
        proc.currentDirectoryURL = URL(fileURLWithPath: homeDir)
        let stdout = Pipe()
        proc.standardOutput = stdout
        proc.standardError = Pipe()
        try proc.run()
        proc.waitUntilExit()
        let data = (try? stdout.fileHandleForReading.readToEnd()) ?? Data()
        return String(data: data, encoding: .utf8) ?? ""
    }

    /// THE bug: oh-my-zsh sets `ZSH_COMPDUMP="${ZDOTDIR:-$HOME}/.zcompdump-..."`
    /// at the top of its load — *while the user's `.zshrc` is being
    /// sourced from inside our `.zshrc`*. If we restore ZDOTDIR after
    /// sourcing user's `.zshrc`, that resolution sees our temp dir
    /// and oh-my-zsh's compdump rebuilds every launch (slow startup,
    /// silently lost on app exit). p10k's `source "${ZDOTDIR:-$HOME}/.p10k.zsh"`
    /// inside user's `.zshrc` has the same shape. This test pins
    /// "ZDOTDIR is the user's value DURING user's .zshrc, not just
    /// at the prompt afterwards."
    func test_endToEnd_userZshrc_seesRestoredZDotDirDuringInit() throws {
        let sandbox = TestHomeSandbox()
        defer { sandbox.teardown() }
        guard let home = ProcessInfo.processInfo.environment["HOME"] else {
            XCTFail("HOME not set after sandbox setup"); return
        }

        // Inside ~/.zshrc, write to ${ZDOTDIR:-$HOME}/.during-zshrc-marker
        // and capture the value of ZDOTDIR at that moment. If our restore
        // happens after sourcing user's .zshrc, the marker lands in our
        // temp dir (which gets cleaned up) and the captured value is the
        // temp path, not <unset>.
        let userZshrc = """
            touch "${ZDOTDIR:-$HOME}/.during-zshrc-marker"
            print -r -- "DURING_ZSHRC_ZDOTDIR=${ZDOTDIR-<unset>}"
            """
        try userZshrc.write(toFile: home + "/.zshrc", atomically: true, encoding: .utf8)

        let out = try runZshUnderInjection(
            homeDir: home,
            niceUserZDotDir: nil,
            commands: "true"
        )

        XCTAssertTrue(
            out.contains("DURING_ZSHRC_ZDOTDIR=<unset>"),
            "ZDOTDIR must be restored BEFORE sourcing user's .zshrc, so tools loaded during init (oh-my-zsh ZSH_COMPDUMP, plugin caches) probe the right path. Output: <\(out)>"
        )
        XCTAssertTrue(
            FileManager.default.fileExists(atPath: home + "/.during-zshrc-marker"),
            "Files written via ${ZDOTDIR:-$HOME}/... during user's .zshrc must land in real $HOME. Marker missing — landed in our soon-to-be-deleted temp dir.")
    }

    /// Default case: no NICE_USER_ZDOTDIR, no custom ZDOTDIR in
    /// `~/.zshenv`. The injection should resolve ZDOTDIR to $HOME so
    /// tools probing `${ZDOTDIR:-$HOME}/.p10k.zsh` write to the real
    /// home. This is the primary fix from the original bug.
    func test_endToEnd_defaultUser_zdotdirResolvesToHome() throws {
        let sandbox = TestHomeSandbox()
        defer { sandbox.teardown() }
        guard let home = ProcessInfo.processInfo.environment["HOME"] else {
            XCTFail("HOME not set after sandbox setup"); return
        }

        // Have the running shell write to ${ZDOTDIR:-$HOME}/.p10k.zsh
        // (mimics what `p10k configure` does on completion) and also
        // print the final ZDOTDIR. The bug is that the write would
        // land in our temp dir; the fix is that it lands in $HOME.
        let out = try runZshUnderInjection(
            homeDir: home,
            niceUserZDotDir: nil,
            commands: """
                touch "${ZDOTDIR:-$HOME}/.p10k.zsh"
                print -r -- "FINAL_ZDOTDIR=${ZDOTDIR-<unset>}"
                """
        )

        // ZDOTDIR should be unset (matching the standard convention
        // when $HOME resolves) — zsh's parameter expansion treats
        // unset and "" identically when there's no `:` in the form.
        XCTAssertTrue(
            out.contains("FINAL_ZDOTDIR=<unset>"),
            "Default user: expected ZDOTDIR to be unset by .zshrc restore. Output: <\(out)>"
        )

        // The .p10k.zsh sentinel should land in $HOME, not our temp dir.
        let expected = home + "/.p10k.zsh"
        XCTAssertTrue(
            FileManager.default.fileExists(atPath: expected),
            "Expected p10k.zsh to land at \(expected) (the real home), not our temp dir."
        )
    }

    /// XDG-style: user has `export ZDOTDIR=~/.config/zsh` in their
    /// `~/.zshenv`. The injection should source that .zshenv during
    /// discovery, find the override, and end up resolving ZDOTDIR to
    /// the custom path. Today (pre-fix) this case is silently broken.
    func test_endToEnd_xdgStyle_zdotdirHonoredFromZshenv() throws {
        let sandbox = TestHomeSandbox()
        defer { sandbox.teardown() }
        guard let home = ProcessInfo.processInfo.environment["HOME"] else {
            XCTFail("HOME not set after sandbox setup"); return
        }
        let custom = home + "/.config/zsh"
        try FileManager.default.createDirectory(
            atPath: custom, withIntermediateDirectories: true
        )

        // ~/.zshenv sets the custom ZDOTDIR, mimicking the XDG layout.
        // ~/.config/zsh/.zshrc prints a sentinel so we can confirm
        // it's the file actually being sourced.
        try #"export ZDOTDIR="$HOME/.config/zsh""#
            .write(toFile: home + "/.zshenv", atomically: true, encoding: .utf8)
        try #"echo NICE-XDG-ZSHRC-LOADED"#
            .write(toFile: custom + "/.zshrc", atomically: true, encoding: .utf8)

        let out = try runZshUnderInjection(
            homeDir: home,
            niceUserZDotDir: nil,
            commands: #"print -r -- "FINAL_ZDOTDIR=$ZDOTDIR""#
        )

        XCTAssertTrue(
            out.contains("NICE-XDG-ZSHRC-LOADED"),
            "Custom ZDOTDIR's .zshrc must be sourced. Output: <\(out)>"
        )
        XCTAssertTrue(
            out.contains("FINAL_ZDOTDIR=\(custom)"),
            "ZDOTDIR must be restored to the user's intended XDG path. Output: <\(out)>"
        )
    }

    /// Login-shell bonus fix: today, Nice's stub `.zprofile` is empty,
    /// so login-shell users silently lose their `~/.zprofile`. After
    /// this PR our `.zprofile` chains through `$NICE_USER_ZDOTDIR/.zprofile`.
    func test_endToEnd_loginShell_sourcesUserZprofile() throws {
        let sandbox = TestHomeSandbox()
        defer { sandbox.teardown() }
        guard let home = ProcessInfo.processInfo.environment["HOME"] else {
            XCTFail("HOME not set after sandbox setup"); return
        }
        try #"echo NICE-ZPROFILE-LOADED"#
            .write(toFile: home + "/.zprofile", atomically: true, encoding: .utf8)

        let out = try runZshUnderInjection(
            homeDir: home,
            niceUserZDotDir: nil,
            commands: "true",
            loginShell: true
        )

        XCTAssertTrue(
            out.contains("NICE-ZPROFILE-LOADED"),
            "Login shells must source the user's ~/.zprofile through the synthetic stub. Output: <\(out)>"
        )
    }

    /// launchctl-style: Nice's own process inherited a ZDOTDIR from its
    /// launch env. We pass it through as NICE_USER_ZDOTDIR; the shell
    /// should restore that value verbatim (no need to source ~/.zshenv).
    func test_endToEnd_launchctlStyle_zdotdirHonoredFromEnv() throws {
        let sandbox = TestHomeSandbox()
        defer { sandbox.teardown() }
        guard let home = ProcessInfo.processInfo.environment["HOME"] else {
            XCTFail("HOME not set after sandbox setup"); return
        }
        let custom = home + "/launchctl-zsh"
        try FileManager.default.createDirectory(
            atPath: custom, withIntermediateDirectories: true
        )
        try #"echo NICE-LAUNCHCTL-ZSHRC-LOADED"#
            .write(toFile: custom + "/.zshrc", atomically: true, encoding: .utf8)

        let out = try runZshUnderInjection(
            homeDir: home,
            niceUserZDotDir: custom,
            commands: #"print -r -- "FINAL_ZDOTDIR=$ZDOTDIR""#
        )

        XCTAssertTrue(
            out.contains("NICE-LAUNCHCTL-ZSHRC-LOADED"),
            "launchctl-style: custom ZDOTDIR's .zshrc must be sourced. Output: <\(out)>"
        )
        XCTAssertTrue(
            out.contains("FINAL_ZDOTDIR=\(custom)"),
            "launchctl-style: ZDOTDIR must be restored from NICE_USER_ZDOTDIR. Output: <\(out)>"
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
