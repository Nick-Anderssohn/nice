//
//  EditorDetectorTests.swift
//  NiceUnitTests
//
//  Coverage for the editor auto-detection parser. Doesn't spawn a
//  shell — instead feeds canned `command -v` output (multi-line, with
//  trailing whitespace, missing entries) to `parseDetected` and
//  checks that the projected `EditorCommand` list matches the
//  curated candidates that "resolved".
//

import Foundation
import XCTest
@testable import Nice

final class EditorDetectorTests: XCTestCase {

    // MARK: - parseDetected

    func test_parseDetected_pickAvailableBinaries() {
        // Output simulates `for c in vim nvim hx; …` where only vim
        // and hx are on PATH.
        let candidates = [
            EditorDetector.Candidate(binary: "vim",  name: "Vim",    command: "vim"),
            EditorDetector.Candidate(binary: "nvim", name: "Neovim", command: "nvim"),
            EditorDetector.Candidate(binary: "hx",   name: "Helix",  command: "hx"),
        ]
        let output = "vim\nhx\n"

        let detected = EditorDetector.parseDetected(output: output, candidates: candidates)

        XCTAssertEqual(detected.map(\.name), ["Vim", "Helix"])
        XCTAssertEqual(detected.map(\.command), ["vim", "hx"])
    }

    func test_parseDetected_emptyOutputReturnsEmptyList() {
        let candidates = [
            EditorDetector.Candidate(binary: "vim", name: "Vim", command: "vim")
        ]
        XCTAssertEqual(
            EditorDetector.parseDetected(output: "", candidates: candidates),
            []
        )
    }

    func test_parseDetected_trimsWhitespaceLines() {
        // Real shells sometimes emit trailing carriage returns or
        // spaces depending on rc-file tweaks. Parser must be lenient.
        let candidates = [
            EditorDetector.Candidate(binary: "vim", name: "Vim", command: "vim")
        ]
        let detected = EditorDetector.parseDetected(
            output: "  vim  \n\n",
            candidates: candidates
        )
        XCTAssertEqual(detected.first?.name, "Vim")
    }

    func test_parseDetected_ignoresUnknownBinaries() {
        // Defensive: if some unrelated string ends up in stdout (e.g.
        // a stray rc-file echo), we skip it instead of inventing an
        // entry.
        let candidates = [
            EditorDetector.Candidate(binary: "vim", name: "Vim", command: "vim")
        ]
        let detected = EditorDetector.parseDetected(
            output: "some-other-binary\nvim\n",
            candidates: candidates
        )
        XCTAssertEqual(detected.map(\.command), ["vim"])
    }

    // MARK: - detectedId

    func test_detectedId_isStableAcrossCalls() {
        // Detected editors aren't persisted, but their ids must
        // round-trip across re-scans within a session — the context
        // menu hands an id back to `openInEditorPane`, and a re-scan
        // can complete in between.
        let a = EditorDetector.detectedId(forBinary: "vim")
        let b = EditorDetector.detectedId(forBinary: "vim")
        XCTAssertEqual(a, b)
    }

    func test_detectedId_differsAcrossBinaries() {
        XCTAssertNotEqual(
            EditorDetector.detectedId(forBinary: "vim"),
            EditorDetector.detectedId(forBinary: "nvim")
        )
    }

    // MARK: - buildProbeScript

    func test_buildProbeScript_listsBinariesSeparatedBySpaces() {
        let candidates = [
            EditorDetector.Candidate(binary: "vim",  name: "Vim",    command: "vim"),
            EditorDetector.Candidate(binary: "nvim", name: "Neovim", command: "nvim"),
        ]
        // Pinning the wire format keeps the runner contract explicit
        // independent of the runner implementation. Regressions here
        // (typo in the loop, missing redirect, …) silently break
        // detection in production.
        XCTAssertEqual(
            EditorDetector.buildProbeScript(candidates: candidates),
            "for c in vim nvim; do command -v \"$c\" >/dev/null 2>&1 && echo \"$c\"; done"
        )
    }

    // MARK: - runDetection (with injected runner)

    func test_runDetection_passesScriptToRunnerAndProjectsResults() {
        // Stub captures the script it was handed and returns canned
        // output. Verifies (a) the runner sees the right script and
        // (b) parseDetected projects the output into EditorCommands.
        let candidates = [
            EditorDetector.Candidate(binary: "vim", name: "Vim",   command: "vim"),
            EditorDetector.Candidate(binary: "hx",  name: "Helix", command: "hx"),
        ]
        let captured = SendableBox<(script: String?, timeout: TimeInterval?)>(value: (nil, nil))
        let runner: ShellRunner = { script, timeout in
            captured.set((script, timeout))
            return "vim\nhx\n"
        }

        let detected = EditorDetector.runDetection(
            candidates: candidates,
            shellRunner: runner,
            timeout: 1.5
        )

        let snapshot = captured.value
        XCTAssertEqual(snapshot.script, EditorDetector.buildProbeScript(candidates: candidates))
        XCTAssertEqual(snapshot.timeout, 1.5)
        XCTAssertEqual(detected.map(\.command), ["vim", "hx"])
    }

    func test_runDetection_runnerThrows_returnsEmptyList() {
        // A misconfigured `.zshrc` or shell-spawn failure must not
        // crash startup. The detector swallows the throw and produces
        // an empty list — the menu falls back to user-configured
        // editors only.
        struct StubError: Error {}
        let runner: ShellRunner = { _, _ in throw StubError() }

        let detected = EditorDetector.runDetection(
            candidates: EditorDetector.candidates,
            shellRunner: runner,
            timeout: 1
        )

        XCTAssertEqual(detected, [])
    }

    // MARK: - performScan

    @MainActor
    func test_performScan_publishesDetectedOnMainActor() async {
        let runner: ShellRunner = { _, _ in "vim\nnvim\n" }
        let detector = EditorDetector(shellRunner: runner)
        XCTAssertTrue(detector.detected.isEmpty)

        await detector.performScan()

        XCTAssertEqual(detector.detected.map(\.command), ["vim", "nvim"])
    }

    @MainActor
    func test_performScan_runnerThrows_publishesEmptyList() async {
        struct StubError: Error {}
        let runner: ShellRunner = { _, _ in throw StubError() }
        let detector = EditorDetector(shellRunner: runner)

        await detector.performScan()

        XCTAssertEqual(detector.detected, [])
    }
}

/// Tiny atomic Sendable box — lets a `@Sendable` closure hand state
/// back to the test body without using inout captures.
private final class SendableBox<T: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var _value: T
    init(value: T) { self._value = value }
    var value: T {
        lock.lock(); defer { lock.unlock() }
        return _value
    }
    func set(_ newValue: T) {
        lock.lock(); defer { lock.unlock() }
        _value = newValue
    }
}
