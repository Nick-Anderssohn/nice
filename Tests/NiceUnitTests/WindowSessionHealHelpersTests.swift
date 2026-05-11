//
//  WindowSessionHealHelpersTests.swift
//  NiceUnitTests
//
//  Direct unit tests for the pure helpers behind the
//  restore-time heal pass: `WindowSession.encodeClaudeBucket`,
//  `WindowSession.readCwdFromTranscript`, and the projects-root
//  injection seam on `WindowSession.healSpawnCwd`. These tests run
//  without `TestHomeSandbox` because the production helpers expose
//  a `projectsRoot:` override on every entry point — each test
//  plants a temp `<root>/<bucket>/<sid>.jsonl` tree and passes the
//  same root to the SUT.
//
//  The end-to-end heal path (persisted-tab → restoreSavedWindow →
//  mutateTab → snapshot) lives in `WindowSessionRestoreTests`; this
//  file pins the per-helper contracts those tests assume.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class WindowSessionHealHelpersTests: XCTestCase {

    // MARK: - encodeClaudeBucket

    func test_encodeClaudeBucket_emptyString() {
        XCTAssertEqual(WindowSession.encodeClaudeBucket(""), "")
    }

    func test_encodeClaudeBucket_plainPath() {
        XCTAssertEqual(
            WindowSession.encodeClaudeBucket("/Users/nick/Projects/notes"),
            "-Users-nick-Projects-notes"
        )
    }

    func test_encodeClaudeBucket_dotsBecomeDashes() {
        // Hidden directories and dotted filenames both flatten — the
        // double-dash run `/.claude` is the production smoking gun.
        XCTAssertEqual(
            WindowSession.encodeClaudeBucket(
                "/Users/nick/Projects/notes/.claude/worktrees/foo"
            ),
            "-Users-nick-Projects-notes--claude-worktrees-foo"
        )
    }

    func test_encodeClaudeBucket_otherPunctuationPassesThrough() {
        // Underscores, hyphens, digits — anything that isn't `/` or
        // `.` — rides through unchanged. Pins the encoding's narrow
        // surface so a future "also encode X" change is loud.
        XCTAssertEqual(
            WindowSession.encodeClaudeBucket("/tmp/foo_bar-2"),
            "-tmp-foo_bar-2"
        )
    }

    // MARK: - readCwdFromTranscript

    func test_readCwdFromTranscript_returnsNilForMissingFile() {
        XCTAssertNil(WindowSession.readCwdFromTranscript(
            at: "/tmp/nice-heal-helper-missing-\(UUID().uuidString)/no.jsonl"
        ))
    }

    func test_readCwdFromTranscript_returnsNilForNonJSONContent() throws {
        let path = try plantFile(lines: [
            "not json at all",
            "still not json",
            "{ unbalanced",
        ])
        XCTAssertNil(WindowSession.readCwdFromTranscript(at: path))
    }

    func test_readCwdFromTranscript_returnsNilWhenNoCwdAnywhere() throws {
        // Valid JSON objects on every line, but none carry `cwd` or a
        // `worktreeSession.worktreePath`. Returns nil so the caller
        // falls back to `resolvedSpawnCwd`.
        let path = try plantFile(lines: [
            #"{"type":"permission-mode","permissionMode":"auto"}"#,
            #"{"type":"file-history-snapshot","isSnapshotUpdate":false}"#,
        ])
        XCTAssertNil(WindowSession.readCwdFromTranscript(at: path))
    }

    func test_readCwdFromTranscript_findsTopLevelCwd() throws {
        let path = try plantFile(lines: [
            #"{"type":"user","cwd":"/Users/nick/Projects/notes","sessionId":"s"}"#,
        ])
        XCTAssertEqual(
            WindowSession.readCwdFromTranscript(at: path),
            "/Users/nick/Projects/notes"
        )
    }

    func test_readCwdFromTranscript_fallsBackToWorktreePath() throws {
        // Head records have no top-level `cwd` field — only a
        // worktree-state with the explicit `worktreePath`. This is
        // the shape of Claude's transcript head before the first user
        // message lands.
        let path = try plantFile(lines: [
            #"{"type":"permission-mode","permissionMode":"auto"}"#,
            #"""
            {"type":"worktree-state","worktreeSession":{"worktreePath":"/Users/nick/Projects/notes/.claude/worktrees/foo","originalCwd":"/Users/nick/Projects/notes"}}
            """#,
        ])
        XCTAssertEqual(
            WindowSession.readCwdFromTranscript(at: path),
            "/Users/nick/Projects/notes/.claude/worktrees/foo"
        )
    }

    func test_readCwdFromTranscript_prefersTopLevelCwdOverWorktreePath() throws {
        // Defensive: if a single record carries BOTH a top-level cwd
        // and a nested worktreeSession.worktreePath (different
        // values), the top-level cwd wins. The per-message format is
        // more authoritative — it reflects where Claude actually is
        // right now, not where the session was originally rooted.
        let path = try plantFile(lines: [
            #"""
            {"type":"user","cwd":"/Users/nick/Projects/notes","worktreeSession":{"worktreePath":"/somewhere/else"}}
            """#,
        ])
        XCTAssertEqual(
            WindowSession.readCwdFromTranscript(at: path),
            "/Users/nick/Projects/notes"
        )
    }

    func test_readCwdFromTranscript_skipsNonObjectLines() throws {
        // The JSON-Lines format Claude uses is records-per-line, but a
        // hand-edited transcript could plausibly contain a bare value
        // (array, string, number) on a line. `as? [String: Any]` cast
        // already filters those — pin the contract.
        let path = try plantFile(lines: [
            #"[1, 2, 3]"#,
            #""bare string""#,
            #"42"#,
            #"{"type":"user","cwd":"/recovered"}"#,
        ])
        XCTAssertEqual(
            WindowSession.readCwdFromTranscript(at: path),
            "/recovered"
        )
    }

    func test_readCwdFromTranscript_skipsEmptyCwdField() throws {
        // `"cwd": ""` is structurally present but semantically absent.
        // The reader must not treat it as a hit — otherwise the heal
        // would adopt an empty path and the downstream existence
        // check would still fail, but only after the JSON read cost.
        let path = try plantFile(lines: [
            #"{"type":"system","cwd":""}"#,
            #"{"type":"user","cwd":"/recovered"}"#,
        ])
        XCTAssertEqual(
            WindowSession.readCwdFromTranscript(at: path),
            "/recovered"
        )
    }

    func test_readCwdFromTranscript_findsCwdAtBoundaryLastLine() throws {
        // Boundary lock: the scan budget is *inclusive* of
        // `transcriptHeadScanLines`. A cwd record at exactly that
        // line is found. If a future refactor changes `prefix(N)` to
        // `prefix(N-1)` or to a 1-based count, this test fails before
        // any integration test does.
        let budget = WindowSession.transcriptHeadScanLines
        var lines = Array(
            repeating: #"{"type":"system"}"#, count: budget - 1
        )
        lines.append(#"{"type":"user","cwd":"/atBoundary"}"#)
        XCTAssertEqual(lines.count, budget)

        let path = try plantFile(lines: lines)
        XCTAssertEqual(
            WindowSession.readCwdFromTranscript(at: path),
            "/atBoundary",
            "cwd record at line \(budget) must be within the scan budget"
        )
    }

    func test_readCwdFromTranscript_ignoresCwdJustBeyondBudget() throws {
        // Companion to the boundary test: a cwd record on the line
        // immediately *after* the budget must NOT be picked up.
        // Together these two tests pin both edges of the
        // `prefix(transcriptHeadScanLines)` slice.
        let budget = WindowSession.transcriptHeadScanLines
        var lines = Array(
            repeating: #"{"type":"system"}"#, count: budget
        )
        lines.append(#"{"type":"user","cwd":"/beyondBoundary"}"#)
        XCTAssertEqual(lines.count, budget + 1)

        let path = try plantFile(lines: lines)
        XCTAssertNil(
            WindowSession.readCwdFromTranscript(at: path),
            "cwd record at line \(budget + 1) must be beyond the scan budget"
        )
    }

    // MARK: - healSpawnCwd projectsRoot injection
    //
    // The end-to-end heal cases live in WindowSessionRestoreTests;
    // here we only pin that the projectsRoot parameter is wired
    // correctly so a test that hands in a temp root really does drive
    // the scan against that root. Catches a future refactor that
    // accidentally re-derives the root from NSHomeDirectory() inside
    // the function body.

    func test_healSpawnCwd_usesInjectedProjectsRoot() throws {
        let root = try tempProjectsRoot()
        let persisted = "/tmp/nice-heal-helper-persisted-\(UUID().uuidString)"
        let recovered = try tempDirectory(prefix: "nice-heal-helper-recovered")
        try plantTranscript(
            in: root,
            bucketCwd: recovered,
            sessionId: "sid-inject",
            withMessageCwd: recovered
        )

        let healed = WindowSession.healSpawnCwd(
            sessionId: "sid-inject",
            persistedCwd: persisted,
            projectsRoot: root
        )
        XCTAssertEqual(
            healed, recovered,
            "healSpawnCwd must scan the injected projects root, not NSHomeDirectory"
        )
    }

    func test_healSpawnCwd_returnsNilWhenInjectedRootIsEmpty() throws {
        let root = try tempProjectsRoot()
        // No transcripts planted — directory exists but is empty.
        let healed = WindowSession.healSpawnCwd(
            sessionId: "sid-empty",
            persistedCwd: "/tmp/nope",
            projectsRoot: root
        )
        XCTAssertNil(
            healed,
            "empty projects root must yield nil with no enumeration crash"
        )
    }

    // MARK: - Helpers

    /// Per-test scratch directory under `/tmp`. Auto-cleaned via
    /// `addTeardownBlock` so heal-helper tests don't accumulate leaked
    /// trees the way the earlier round did.
    private func tempDirectory(prefix: String) throws -> String {
        let path = "/tmp/\(prefix)-\(UUID().uuidString)"
        try FileManager.default.createDirectory(
            atPath: path, withIntermediateDirectories: true
        )
        addTeardownBlock { try? FileManager.default.removeItem(atPath: path) }
        return path
    }

    /// Build a fresh projects-root directory the SUT can scan. The
    /// `projectsRoot:` arg to `healSpawnCwd` doesn't need to live
    /// under `$HOME`; any readable dir works.
    private func tempProjectsRoot() throws -> String {
        try tempDirectory(prefix: "nice-heal-helper-root")
    }

    /// Plant a Claude transcript at the bucket implied by `bucketCwd`
    /// inside `root`. Mirrors the production bucket-encoding
    /// (`/` and `.` → `-`).
    private func plantTranscript(
        in root: String,
        bucketCwd: String,
        sessionId: String,
        withMessageCwd: String
    ) throws {
        let bucket = WindowSession.encodeClaudeBucket(bucketCwd)
        let dir = "\(root)/\(bucket)"
        try FileManager.default.createDirectory(
            atPath: dir, withIntermediateDirectories: true
        )
        let path = "\(dir)/\(sessionId)\(WindowSession.transcriptExtension)"
        let body = #"""
        {"type":"user","cwd":"\#(withMessageCwd)","sessionId":"\#(sessionId)"}
        """# + "\n"
        try body.write(toFile: path, atomically: true, encoding: .utf8)
    }

    /// Plant a transcript file at an auto-cleaned temp path; returns
    /// the path. Lines are joined with `\n` plus a trailing newline so
    /// `split(separator: "\n")` yields the expected count.
    private func plantFile(lines: [String]) throws -> String {
        let dir = try tempDirectory(prefix: "nice-heal-helper-file")
        let path = "\(dir)/transcript\(WindowSession.transcriptExtension)"
        let body = lines.joined(separator: "\n") + "\n"
        try body.write(toFile: path, atomically: true, encoding: .utf8)
        return path
    }
}
