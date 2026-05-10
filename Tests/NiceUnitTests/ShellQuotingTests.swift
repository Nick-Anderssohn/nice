//
//  ShellQuotingTests.swift
//  NiceUnitTests
//
//  Exercises the two helpers in ShellQuoting.swift that every pty
//  spawn and drag-drop path leans on: `shellSingleQuote` (used to
//  wrap exec args in TabPtySession's `zsh -ilc` line) and
//  `shellBackslashEscape` (used by NiceTerminalView to type a
//  dropped file path into a running pane). A bad encoding here
//  silently corrupts every shell command Nice builds, so even a
//  5-line regression suite pays for itself.
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

    // MARK: - shellBackslashEscape

    func test_backslashEscape_emptyString_returnsEmpty() {
        // Key behavioral difference from `shellSingleQuote`, which
        // returns `''` — the per-character form has no wrapping
        // pair so empty in stays empty out.
        XCTAssertEqual(shellBackslashEscape(""), "")
    }

    func test_backslashEscape_pureAlnum_unchanged() {
        XCTAssertEqual(shellBackslashEscape("hello"), "hello")
        XCTAssertEqual(shellBackslashEscape("HELLO123"), "HELLO123")
    }

    func test_backslashEscape_plainPath_unchanged() {
        XCTAssertEqual(
            shellBackslashEscape("/Users/nick/file.txt"),
            "/Users/nick/file.txt"
        )
    }

    func test_backslashEscape_safeSet_passesThrough() {
        // Each char in the documented safe set must be emitted
        // as-is. Iterated explicitly so a regression on any one
        // character names the offender in the failure message.
        for ch in ["a", "Z", "0", "9", ".", "_", "/", "+", ":", "=", "@", ",", "-"] {
            XCTAssertEqual(
                shellBackslashEscape(ch), ch,
                "expected \(ch.debugDescription) to pass through"
            )
        }
    }

    func test_backslashEscape_unsafeSet_eachCharPrefixed() {
        // Every other printable ASCII metachar gets one backslash.
        // Listed verbatim to match the helper's doc comment 1:1.
        let unsafe: [String] = [
            " ", "!", "\"", "#", "$", "%", "&", "'", "(", ")",
            "*", ";", "<", ">", "?", "[", "\\", "]", "^", "`",
            "{", "|", "}", "~",
        ]
        for ch in unsafe {
            XCTAssertEqual(
                shellBackslashEscape(ch), "\\" + ch,
                "expected \(ch.debugDescription) to be backslash-escaped"
            )
        }
    }

    func test_backslashEscape_nonASCII_unchanged() {
        // Non-ASCII has no shell metasyntax meaning; pass through
        // verbatim so display and round-trip stay byte-clean.
        XCTAssertEqual(shellBackslashEscape("café🫠.png"), "café🫠.png")
    }

    func test_backslashEscape_nonASCIIWithSpace_onlySpaceEscaped() {
        XCTAssertEqual(
            shellBackslashEscape("café 🫠.png"),
            #"café\ 🫠.png"#
        )
    }

    func test_backslashEscape_realWorldMacPath() {
        XCTAssertEqual(
            shellBackslashEscape("/Users/nick/Documents/My File (final).txt"),
            #"/Users/nick/Documents/My\ File\ \(final\).txt"#
        )
    }

    func test_backslashEscape_singleBackslash_escapedToDouble() {
        // Input is one literal backslash; output is two.
        XCTAssertEqual(shellBackslashEscape(#"\"#), #"\\"#)
    }

    func test_backslashEscape_singleQuote_prefixed() {
        XCTAssertEqual(shellBackslashEscape("it's.txt"), #"it\'s.txt"#)
    }

    func test_backslashEscape_mixedSequence_orderingPreserved() {
        // Confirms each unsafe char is escaped in place — no
        // batching, reordering, or doubling — and safe chars stay
        // adjacent to their escaped neighbors.
        XCTAssertEqual(
            shellBackslashEscape("a b'c(d)e"),
            #"a\ b\'c\(d\)e"#
        )
    }

    func test_backslashEscape_tempImagePath_unchanged() {
        // The drop-handler's image-fallback path lands here:
        // `<caches>/Nice/dropped-images/<UUID>.png`. Pure alnum,
        // `/`, `.`, `-` — must round-trip untouched so the
        // inferior process sees the same bytes the file system
        // produced.
        let path = "/private/var/folders/ab/cd/T/Nice/dropped-images/" +
            "DEAD-BEEF-1234-5678.png"
        XCTAssertEqual(shellBackslashEscape(path), path)
    }

    // MARK: - shellSingleQuote round-trip

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
