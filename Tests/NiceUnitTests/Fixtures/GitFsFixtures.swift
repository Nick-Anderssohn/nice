//
//  GitFsFixtures.swift
//  NiceUnitTests
//
//  Real-filesystem builders for tests that exercise `findGitRoot` /
//  worktree-detection logic. Instances own a temp directory under
//  `NSTemporaryDirectory()`; call `cleanup()` in `tearDown`.
//

import Foundation

@MainActor
final class GitFsFixtures {

    /// Root directory all `make…` calls plant under. Tests treat it as
    /// the filesystem; `findGitRoot` walks it the same way it walks
    /// the real one.
    let root: URL

    init(label: String = "fixture") {
        root = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-\(label)-\(UUID().uuidString)", isDirectory: true
            )
        try? FileManager.default.createDirectory(
            at: root, withIntermediateDirectories: true
        )
    }

    /// Plant a `.git` directory at `<root>/<relativePath>` and return
    /// the absolute path of the containing dir (suitable for use as a
    /// cwd).
    func makeGitRepo(at relativePath: String) -> String {
        let dir = root.appendingPathComponent(relativePath, isDirectory: true)
        try? FileManager.default.createDirectory(
            at: dir, withIntermediateDirectories: true
        )
        let dotGit = dir.appendingPathComponent(".git", isDirectory: true)
        try? FileManager.default.createDirectory(
            at: dotGit, withIntermediateDirectories: true
        )
        return dir.path
    }

    /// Create a plain directory (no `.git`) under the root. Exists on
    /// disk so the cwd is real, but `findGitRoot` walks past it to an
    /// enclosing repo (or returns nil when none is planted).
    func makeDir(at relativePath: String) -> String {
        let dir = root.appendingPathComponent(relativePath, isDirectory: true)
        try? FileManager.default.createDirectory(
            at: dir, withIntermediateDirectories: true
        )
        return dir.path
    }

    /// Plant a `.git` *file* (the marker git uses for worktrees and
    /// submodules) so we can test the worktree pre-strip without
    /// also tripping the inner repo as a self-contained git root.
    func makeWorktreeMarker(at relativePath: String) -> String {
        let dir = root.appendingPathComponent(relativePath, isDirectory: true)
        try? FileManager.default.createDirectory(
            at: dir, withIntermediateDirectories: true
        )
        let dotGit = dir.appendingPathComponent(".git")
        try? "gitdir: /placeholder\n".write(
            to: dotGit, atomically: true, encoding: .utf8
        )
        return dir.path
    }

    func cleanup() {
        try? FileManager.default.removeItem(at: root)
    }
}
