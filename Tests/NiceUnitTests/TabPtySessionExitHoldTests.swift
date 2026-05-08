//
//  TabPtySessionExitHoldTests.swift
//  NiceUnitTests
//
//  Pins down the pure helpers that drive `TabPtySession.handlePaneExit`'s
//  hold-vs-drop decision: `shouldHoldOnExit` (which exits warrant
//  keeping the SwiftTerm view mounted) and `paneExitFooter` (the dim
//  footer line written into the held pane's buffer). These exits used
//  to dissolve the tab before the user could read the error — `claude
//  -w foo` outside a git repo printed "fatal: not a git repository"
//  and the tab popped out so fast it looked like Nice crashed. The
//  hold path keeps the scrollback alive; these tests guard the policy
//  so a future refactor can't quietly regress to the dissolve-fast
//  behaviour.
//

import XCTest
@testable import Nice

final class TabPtySessionExitHoldTests: XCTestCase {

    // MARK: - shouldHoldOnExit

    func test_cleanExit_drops() {
        // exit 0 is deliberate (`exit`, `/exit` from claude, `vim`
        // saved + quit). The user wants the pane gone — holding on
        // these would create an infinite supply of "[Process exited
        // (status 0)]" carcasses the user has to manually close.
        XCTAssertFalse(
            TabPtySession.shouldHoldOnExit(
                exitCode: 0, intentionallyTerminated: false
            )
        )
    }

    func test_nonZeroExit_holds() {
        // The repro case from the original bug: `claude -w foo`
        // outside a git repo exits with `fatal: not a git repository`
        // and a non-zero status. Hold so the user can read it.
        XCTAssertTrue(
            TabPtySession.shouldHoldOnExit(
                exitCode: 1, intentionallyTerminated: false
            )
        )
    }

    func test_unusualNonZeroExit_holds() {
        // 127 is "command not found" — what zsh reports when Claude's
        // binary path resolves to nothing, or when an editor pane was
        // spawned with a binary that isn't installed. Same path as 1
        // for the policy.
        XCTAssertTrue(
            TabPtySession.shouldHoldOnExit(
                exitCode: 127, intentionallyTerminated: false
            )
        )
    }

    func test_signalExit_nilCode_holds() {
        // `exitCode == nil` means the child died from a signal with
        // no waitstatus available — could be the OS, an external
        // `kill`, parent process group hangup. Nice didn't ask for
        // it via the UI, so hold so the user sees what happened.
        XCTAssertTrue(
            TabPtySession.shouldHoldOnExit(
                exitCode: nil, intentionallyTerminated: false
            )
        )
    }

    func test_intentionalKill_zeroExit_drops() {
        // Trivially true (clean exit drops anyway), but written out so
        // the intent of the flag is explicit when scanning the test
        // matrix.
        XCTAssertFalse(
            TabPtySession.shouldHoldOnExit(
                exitCode: 0, intentionallyTerminated: true
            )
        )
    }

    func test_intentionalKill_nonZeroExit_drops() {
        // Cmd+W on a still-busy tab SIGHUPs the shell, which often
        // exits with a non-zero status (interactive shells signal
        // their handler-installed cleanup paths via the exit code).
        // Without the intentional flag we'd hold a "[Process exited
        // (status 129)]" footer the user explicitly asked to dismiss.
        XCTAssertFalse(
            TabPtySession.shouldHoldOnExit(
                exitCode: 129, intentionallyTerminated: true
            )
        )
    }

    func test_intentionalKill_signalExit_drops() {
        // The SIGKILL fallback in `terminatePane`'s deferred path:
        // child caught SIGHUP, ignored it, got SIGKILL'd half a
        // second later. nil exit code; still our doing.
        XCTAssertFalse(
            TabPtySession.shouldHoldOnExit(
                exitCode: nil, intentionallyTerminated: true
            )
        )
    }

    // MARK: - paneExitFooter

    func test_footer_claudePane_withStatus() {
        let footer = TabPtySession.paneExitFooter(
            kind: .claude, exitCode: 1
        )
        XCTAssertEqual(
            footer,
            "\r\n\u{1b}[2m[claude exited (status 1)]\u{1b}[0m\r\n"
        )
    }

    func test_footer_terminalPane_withStatus() {
        // Terminal panes don't say "claude" — too misleading for
        // panes hosting plain shells, editors, or arbitrary commands.
        let footer = TabPtySession.paneExitFooter(
            kind: .terminal, exitCode: 127
        )
        XCTAssertEqual(
            footer,
            "\r\n\u{1b}[2m[Process exited (status 127)]\u{1b}[0m\r\n"
        )
    }

    func test_footer_signalExit_nilCode_namesSignal() {
        // No status number → use a different word. "killed by signal"
        // is the closest lay-readable description; the actual signal
        // number isn't available at this layer.
        let footer = TabPtySession.paneExitFooter(
            kind: .claude, exitCode: nil
        )
        XCTAssertEqual(
            footer,
            "\r\n\u{1b}[2m[claude exited (killed by signal)]\u{1b}[0m\r\n"
        )
    }

    func test_footer_includesLeadingCR() {
        // The cursor's column when the process died is unknown — `\r`
        // snaps it to column 0 before the footer prints, otherwise a
        // process exiting mid-line would render the footer indented.
        let footer = TabPtySession.paneExitFooter(
            kind: .terminal, exitCode: 1
        )
        XCTAssertTrue(footer.hasPrefix("\r\n"),
                      "Footer must start with \\r\\n; otherwise a process that died mid-line indents the footer.")
    }

    func test_footer_endsWithReset() {
        // ANSI reset at the end so the dim attribute doesn't bleed
        // into anything written to the buffer afterwards. Today there
        // shouldn't be anything (the pty is dead) but cheap insurance.
        let footer = TabPtySession.paneExitFooter(
            kind: .terminal, exitCode: 0
        )
        XCTAssertTrue(footer.contains("\u{1b}[0m"),
                      "Footer must include the ESC[0m reset.")
    }
}
