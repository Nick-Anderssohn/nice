//
//  TerminalDelegateBridgeTests.swift
//  NiceUnitTests
//
//  Pure-function tests for `ProcessTerminationDelegate.parseOsc7Path`
//  — the parser that turns SwiftTerm's raw OSC 7 payload (`file://
//  hostname/path`, sometimes percent-encoded) into the absolute path
//  Nice persists on `Pane.cwd`. Wire-format quirks live here; the
//  rest of the OSC 7 pipeline is tested via the zsh end-to-end test
//  in `MainTerminalShellInjectTests`.
//

import Foundation
import XCTest
@testable import Nice

final class TerminalDelegateBridgeTests: XCTestCase {

    func test_parseOsc7Path_fileUrlWithHost() {
        let path = ProcessTerminationDelegate.parseOsc7Path(
            "file://Mac.local/Users/nick/Projects/nice"
        )
        XCTAssertEqual(path, "/Users/nick/Projects/nice")
    }

    func test_parseOsc7Path_fileUrlWithoutHost() {
        // `file:///Users/nick` — three slashes, empty host. Some
        // shells emit this form (zsh's apple-terminal integration
        // included). Must still resolve to the path.
        let path = ProcessTerminationDelegate.parseOsc7Path(
            "file:///Users/nick"
        )
        XCTAssertEqual(path, "/Users/nick")
    }

    func test_parseOsc7Path_percentEncodedSpace() {
        // The injected emitter percent-encodes spaces. The Swift-side
        // parser must decode them so the persisted cwd matches the
        // real filesystem path the user can `chdir(2)` into.
        let path = ProcessTerminationDelegate.parseOsc7Path(
            "file://Mac.local/Users/nick/My%20Stuff"
        )
        XCTAssertEqual(path, "/Users/nick/My Stuff")
    }

    func test_parseOsc7Path_percentEncodedPercent() {
        // A literal `%` in a path encodes to `%25`. Decoding must
        // round-trip to a single `%`.
        let path = ProcessTerminationDelegate.parseOsc7Path(
            "file://Mac.local/tmp/100%25done"
        )
        XCTAssertEqual(path, "/tmp/100%done")
    }

    func test_parseOsc7Path_bareAbsolutePath_passThrough() {
        // Permissive fallback: a shell that emits a raw absolute path
        // (no scheme) still updates the cwd. Mirrors what other
        // terminals do.
        let path = ProcessTerminationDelegate.parseOsc7Path("/var/log")
        XCTAssertEqual(path, "/var/log")
    }

    func test_parseOsc7Path_nonFileScheme_returnsNil() {
        // `http://` payload could only be a host injecting noise into
        // the pty; refuse it so we don't store garbage.
        XCTAssertNil(
            ProcessTerminationDelegate.parseOsc7Path("http://example.com/x")
        )
    }

    func test_parseOsc7Path_emptyString_returnsNil() {
        XCTAssertNil(ProcessTerminationDelegate.parseOsc7Path(""))
    }

    func test_parseOsc7Path_unparseable_returnsNil() {
        // Wholly malformed input — no scheme, no leading slash — must
        // not be promoted to a "cwd."
        XCTAssertNil(
            ProcessTerminationDelegate.parseOsc7Path("not a url at all")
        )
    }
}
