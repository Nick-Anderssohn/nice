//
//  TabPtySessionClaudeArgsTests.swift
//  NiceUnitTests
//
//  Pins down the `exec <claude> ...` command-line assembly that
//  TabPtySession hands to `zsh -ilc`. Regressions here silently break
//  session resumption (wrong flag order eats the UUID), newly created
//  sessions (missing `--session-id` means the CLI picks its own and
//  Nice can't resume later), and the override branch (NICE_CLAUDE_OVERRIDE
//  must suppress every injected flag).
//

import XCTest
@testable import Nice

final class TabPtySessionClaudeArgsTests: XCTestCase {

    // MARK: - Modes

    func test_none_noSessionFlag_noExtraArgs() {
        let cmd = TabPtySession.buildClaudeExecCommand(
            claude: "/usr/local/bin/claude",
            mode: .none,
            extraClaudeArgs: [],
            isOverride: false
        )
        XCTAssertEqual(cmd, "exec '/usr/local/bin/claude'")
    }

    func test_none_withExtraArgs_appendedQuoted() {
        let cmd = TabPtySession.buildClaudeExecCommand(
            claude: "/usr/local/bin/claude",
            mode: .none,
            extraClaudeArgs: ["--foo", "bar baz"],
            isOverride: false
        )
        XCTAssertEqual(cmd, "exec '/usr/local/bin/claude' '--foo' 'bar baz'")
    }

    func test_new_emitsSessionIdBeforeExtraArgs() {
        // Order is load-bearing: --session-id <uuid> must come before
        // the user's extra args or the UUID would be parsed as the
        // value of their trailing flag.
        let cmd = TabPtySession.buildClaudeExecCommand(
            claude: "/usr/local/bin/claude",
            mode: .new(id: "abc-123"),
            extraClaudeArgs: ["--model", "opus"],
            isOverride: false
        )
        XCTAssertEqual(cmd, "exec '/usr/local/bin/claude' --session-id 'abc-123' '--model' 'opus'")
    }

    func test_resume_emitsResumeFlag_dropsExtraArgs() {
        // Resume paths ignore extraClaudeArgs by design — the transcript
        // already carries the original session's flags. Passing model
        // overrides on resume would silently diverge from the recorded
        // session.
        let cmd = TabPtySession.buildClaudeExecCommand(
            claude: "/usr/local/bin/claude",
            mode: .resume(id: "abc-123"),
            extraClaudeArgs: ["--model", "opus"],
            isOverride: false
        )
        XCTAssertEqual(cmd, "exec '/usr/local/bin/claude' --resume 'abc-123'")
    }

    func test_resumeDeferred_emitsOnlyExec() {
        // Deferred resume doesn't run claude at all — it spawns a plain
        // shell and pre-types the resume command. The helper should
        // return just the exec prefix (the caller doesn't use this
        // branch's output).
        let cmd = TabPtySession.buildClaudeExecCommand(
            claude: "/usr/local/bin/claude",
            mode: .resumeDeferred(id: "abc-123"),
            extraClaudeArgs: [],
            isOverride: false
        )
        XCTAssertEqual(cmd, "exec '/usr/local/bin/claude'")
    }

    // MARK: - Override branch

    func test_override_suppressesSessionFlag() {
        // NICE_CLAUDE_OVERRIDE lets a developer redirect claude through
        // a wrapper (e.g. an llm-costs logger). In that mode, Nice must
        // NOT inject --session-id because the wrapper owns the argv.
        let cmd = TabPtySession.buildClaudeExecCommand(
            claude: "/usr/local/bin/claude",
            mode: .new(id: "abc-123"),
            extraClaudeArgs: ["--model", "opus"],
            isOverride: true
        )
        XCTAssertEqual(cmd, "exec '/usr/local/bin/claude'")
    }

    func test_override_suppressesResumeFlag() {
        let cmd = TabPtySession.buildClaudeExecCommand(
            claude: "/usr/local/bin/claude",
            mode: .resume(id: "abc-123"),
            extraClaudeArgs: [],
            isOverride: true
        )
        XCTAssertEqual(cmd, "exec '/usr/local/bin/claude'")
    }

    // MARK: - Quoting

    func test_pathWithSpaces_quotedCorrectly() {
        let cmd = TabPtySession.buildClaudeExecCommand(
            claude: "/Users/dev user/bin/claude",
            mode: .none,
            extraClaudeArgs: [],
            isOverride: false
        )
        XCTAssertEqual(cmd, "exec '/Users/dev user/bin/claude'")
    }

    func test_pathWithSingleQuote_usesEscapeSequence() {
        // Extremely rare in practice but trivially survivable; this
        // pins the `'\''` escape down so any regression in
        // shellSingleQuote's integration surfaces here too.
        let cmd = TabPtySession.buildClaudeExecCommand(
            claude: "/Users/dev's/claude",
            mode: .none,
            extraClaudeArgs: [],
            isOverride: false
        )
        XCTAssertEqual(cmd, #"exec '/Users/dev'\''s/claude'"#)
    }

    func test_extraArgWithShellMetacharacters_passesThroughLiterally() {
        // Inside single quotes, `$`, backtick, etc. are literal. The
        // shell must receive them verbatim — not as parameter/command
        // expansions.
        let cmd = TabPtySession.buildClaudeExecCommand(
            claude: "/claude",
            mode: .none,
            extraClaudeArgs: ["$HOME", "`whoami`"],
            isOverride: false
        )
        XCTAssertEqual(cmd, "exec '/claude' '$HOME' '`whoami`'")
    }
}
