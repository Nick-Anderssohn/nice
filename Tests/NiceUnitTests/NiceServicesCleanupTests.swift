//
//  NiceServicesCleanupTests.swift
//  NiceUnitTests
//
//  Exercises `NiceServices.tempFileDecision`, the classifier that
//  decides which `$TMPDIR` leftovers to reap. The important regression
//  this guards against: running `Nice Dev` alongside prod `Nice` must
//  not wipe prod's `nice-zdotdir-<pid>` directory — if it does, prod's
//  zsh children spawn with `ZDOTDIR` pointing at nothing and silently
//  drop every alias defined in `~/.zshrc`.
//

import XCTest
@testable import Nice

final class NiceServicesCleanupTests: XCTestCase {

    private func decide(
        _ filename: String,
        alive: Set<pid_t> = []
    ) -> NiceServices.TempFileDecision {
        NiceServices.tempFileDecision(filename: filename) { alive.contains($0) }
    }

    // MARK: - Ignore anything that isn't ours

    func test_ignoresUnrelatedFiles() {
        XCTAssertEqual(decide("random-file.txt"), .ignore)
        XCTAssertEqual(decide(".DS_Store"), .ignore)
        XCTAssertEqual(decide("nice-without-pid"), .ignore)
        XCTAssertEqual(decide("not-nice-123.sock"), .ignore)
    }

    // MARK: - Zdotdir

    func test_zdotdir_liveOwner_isKept() {
        XCTAssertEqual(decide("nice-zdotdir-4242", alive: [4242]), .keep)
    }

    func test_zdotdir_deadOwner_isRemoved() {
        XCTAssertEqual(decide("nice-zdotdir-4242", alive: []), .remove)
    }

    /// The current process is (by definition) alive, so our own
    /// zdotdir must never be swept — the next step of init writes into
    /// it.
    func test_zdotdir_selfPid_isKept() {
        XCTAssertEqual(
            decide("nice-zdotdir-\(getpid())", alive: [getpid()]),
            .keep
        )
    }

    func test_zdotdir_unparseablePid_isIgnored() {
        XCTAssertEqual(decide("nice-zdotdir-notanumber"), .ignore)
        XCTAssertEqual(decide("nice-zdotdir-"), .ignore)
    }

    // MARK: - Control socket

    func test_socket_liveOwner_isKept() {
        XCTAssertEqual(decide("nice-4242-C0FFEE.sock", alive: [4242]), .keep)
    }

    func test_socket_deadOwner_isRemoved() {
        XCTAssertEqual(decide("nice-4242-C0FFEE.sock", alive: []), .remove)
    }

    func test_socket_missingSuffix_isIgnored() {
        // Matches the `nice-<pid>-` prefix but isn't a socket file.
        XCTAssertEqual(decide("nice-4242-scratch"), .ignore)
    }

    func test_socket_missingPidSegment_isIgnored() {
        XCTAssertEqual(decide("nice-.sock"), .ignore)
        XCTAssertEqual(decide("nice-abc.sock"), .ignore)
    }
}
