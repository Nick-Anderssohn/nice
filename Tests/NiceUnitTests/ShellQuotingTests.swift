//
//  ShellQuotingTests.swift
//  NiceUnitTests
//
//  Exercises `shellSingleQuote`, the single-function helper every pty
//  spawn path leans on (TabPtySession's `exec <claude> ...` line and
//  NiceTerminalView's drag-and-drop path typing file paths into a pane).
//  A bad quote here silently corrupts every shell command Nice builds,
//  so even a 5-line regression suite pays for itself.
//

import XCTest
@testable import Nice

final class ShellQuotingTests: XCTestCase {

    func test_emptyString_producesEmptyQuotedPair() {
        XCTAssertEqual(shellSingleQuote(""), "''")
    }

    func test_simpleString_wrappedInSingleQuotes() {
        XCTAssertEqual(shellSingleQuote("hello"), "'hello'")
    }

    func test_embeddedSingleQuote_usesCloseOpenEscape() {
        // The classic '\'' trick: close the quote, emit a literal
        // backslash-quote, reopen the quote. Must survive verbatim.
        XCTAssertEqual(shellSingleQuote("it's"), #"'it'\''s'"#)
    }

    func test_onlySingleQuotes_escapedCorrectly() {
        XCTAssertEqual(shellSingleQuote("'"), #"''\'''"#)
        XCTAssertEqual(shellSingleQuote("''"), #"''\'''\'''"#)
    }

    func test_shellMetacharacters_passThroughInsideQuotes() {
        // `$`, backtick, `\`, `"`, `*`, `?`, space — all literal inside
        // single quotes, must not be expanded or escaped.
        let weird = #"$HOME `date` \n "x" * ? ~"#
        XCTAssertEqual(shellSingleQuote(weird), "'" + weird + "'")
    }

    func test_newline_passThroughInsideQuotes() {
        XCTAssertEqual(shellSingleQuote("a\nb"), "'a\nb'")
    }

    func test_unicode_passThroughInsideQuotes() {
        XCTAssertEqual(shellSingleQuote("café 🫠"), "'café 🫠'")
    }

    func test_roundTrip_viaShell() throws {
        // Strongest check we can make without spawning a process: a
        // quoted token, when pasted after `printf '%s\n' ` and
        // evaluated by /bin/sh, reproduces the original string.
        // Running /bin/sh in a unit test would couple us to an external
        // binary; instead we verify the invariant the quoting promises:
        // the quoted form starts and ends with `'`, contains no raw
        // single quote outside an escape sequence.
        let inputs = [
            "",
            "plain",
            "with space",
            "it's",
            "''''",
            "$(rm -rf /)",
            "\\\"`$",
        ]
        for input in inputs {
            let quoted = shellSingleQuote(input)
            XCTAssertTrue(quoted.hasPrefix("'"), "missing leading quote: \(quoted)")
            XCTAssertTrue(quoted.hasSuffix("'"), "missing trailing quote: \(quoted)")
            // Strip the outer quotes; the interior must contain no
            // single quote except as part of the `'\''` sequence.
            let inner = String(quoted.dropFirst().dropLast())
            let placeholder = "\u{FFFD}"
            let sanitized = inner.replacingOccurrences(of: #"'\''"#, with: placeholder)
            XCTAssertFalse(sanitized.contains("'"),
                           "raw single quote leaked through quoting for input \(input.debugDescription): \(quoted)")
        }
    }
}
