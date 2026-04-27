//
//  TabPtySessionExecArgsTests.swift
//  NiceUnitTests
//
//  Pins the arg list `addTerminalPane` hands to `/bin/zsh`:
//    • plain pane           → `["-il"]`            (login-interactive)
//    • editor / custom cmd  → `["-ilc", "exec …"]` (replace shell with cmd)
//
//  The `exec` form is load-bearing — without it, quitting vim/nvim
//  drops the user back into a zsh prompt instead of closing the pane,
//  and SIGWINCH/SIGINT have to traverse a parent zsh that doesn't
//  forward them. Regressions here are very visible (extra prompt
//  flash, "press any key to close" UX) so this test is cheap insurance
//  that the carve-out isn't accidentally rewritten back inline.
//

import XCTest
@testable import Nice

final class TabPtySessionExecArgsTests: XCTestCase {

    func test_buildExecArgs_nil_returnsLoginShell() {
        XCTAssertEqual(
            TabPtySession.buildExecArgs(command: nil),
            ["-il"]
        )
    }

    func test_buildExecArgs_simpleCommand_wrapsInExec() {
        XCTAssertEqual(
            TabPtySession.buildExecArgs(command: "vim '/tmp/x.md'"),
            ["-ilc", "exec vim '/tmp/x.md'"]
        )
    }

    func test_buildExecArgs_commandWithEditorArgs_passesThroughVerbatim() {
        // `nvim -p` opens files in tabs. The args must reach nvim
        // unchanged — no extra quoting, no shell wrapping that
        // re-tokenises them.
        XCTAssertEqual(
            TabPtySession.buildExecArgs(command: "nvim -p '/Users/me/foo.swift'"),
            ["-ilc", "exec nvim -p '/Users/me/foo.swift'"]
        )
    }

    func test_buildExecArgs_emptyCommand_stillWrapsInExec() {
        // Edge case: empty-string command means "exec nothing", which
        // zsh will reject at runtime. Test pins the structural
        // contract — we don't silently switch to `-il` on empty.
        XCTAssertEqual(
            TabPtySession.buildExecArgs(command: ""),
            ["-ilc", "exec "]
        )
    }
}
