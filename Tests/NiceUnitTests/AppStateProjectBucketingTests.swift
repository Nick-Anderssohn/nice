//
//  AppStateProjectBucketingTests.swift
//  NiceUnitTests
//
//  Locks in the contract that `claude` invocations from a Main Terminal
//  pane create a fresh project group in the sidebar when the cwd
//  doesn't belong to any existing non-Terminals project — even when the
//  cwd is under $HOME (the pinned Terminals group's path). Regression
//  test for the bug where the new Claude tab was stuffed under the
//  Terminals group because longest-prefix-matching included Terminals.
//
//  Drives through the public `createTabFromMainTerminal` surface so the
//  full tab-build-and-bucket path is covered, not just the private
//  helper. Like AppStateNavigationTests, this touches the real pty
//  spawn path inside `makeSession`; assertions only read the data
//  model. NICE_CLAUDE_OVERRIDE is set to /bin/cat so claude resolution
//  doesn't hit a login shell or depend on the host having `claude`
//  installed.
//

import Darwin
import Foundation
import XCTest
@testable import Nice

@MainActor
final class AppStateProjectBucketingTests: XCTestCase {

    private var appState: AppState!
    private var homeSandbox: TestHomeSandbox!
    private var gitFsRoot: URL!
    private let mainCwd = "/tmp/nice-test-home"

    override func setUp() {
        super.setUp()
        homeSandbox = TestHomeSandbox()
        setenv("NICE_CLAUDE_OVERRIDE", "/bin/cat", 1)
        gitFsRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-bucketing-\(UUID().uuidString)", isDirectory: true
            )
        try? FileManager.default.createDirectory(
            at: gitFsRoot, withIntermediateDirectories: true
        )
        appState = AppState(
            services: nil,
            initialSidebarCollapsed: false,
            initialMainCwd: mainCwd,
            windowSessionId: UUID().uuidString
        )
    }

    override func tearDown() {
        appState = nil
        unsetenv("NICE_CLAUDE_OVERRIDE")
        try? FileManager.default.removeItem(at: gitFsRoot)
        gitFsRoot = nil
        homeSandbox.teardown()
        homeSandbox = nil
        super.tearDown()
    }

    // MARK: - Regression

    /// The bug: with Terminals.path = "/tmp/nice-test-home", a cwd like
    /// "/tmp/nice-test-home/Projects/zephyr" prefix-matches Terminals
    /// and the new Claude tab was appended to it. The fix excludes
    /// Terminals from the match, so a brand-new project group is created.
    func test_claudeFromMainTerminal_underMainCwd_createsNewProjectGroup() throws {
        let cwd = "\(mainCwd)/Projects/zephyr"

        appState.createTabFromMainTerminal(cwd: cwd, args: [])

        XCTAssertEqual(appState.projects.count, 2,
                       "Expected Terminals + one new project group")

        let terminals = appState.projects.first!
        XCTAssertEqual(terminals.id, AppState.terminalsProjectId)
        XCTAssertEqual(terminals.tabs.count, 1,
                       "Terminals group must not absorb Claude tabs")
        XCTAssertEqual(terminals.tabs.first?.id, AppState.mainTerminalTabId)

        let newProject = try XCTUnwrap(
            appState.projects.first { $0.id != AppState.terminalsProjectId },
            "No non-Terminals project created — the new Claude tab was swallowed by the Terminals group"
        )
        XCTAssertEqual(newProject.name, "ZEPHYR")
        XCTAssertEqual(newProject.path, cwd)
        XCTAssertEqual(newProject.tabs.count, 1)
        let claudeTab = try XCTUnwrap(newProject.tabs.first)
        XCTAssertTrue(claudeTab.panes.contains { $0.kind == .claude },
                      "New tab must carry a Claude pane")
    }

    /// When the cwd is exactly the Main Terminal cwd — i.e. the
    /// Terminals project's own path — we still create a fresh project
    /// rather than swallowing the tab. Without the Terminals-exclusion
    /// filter, the trivial prefix match would win and the tab would
    /// land under Terminals.
    func test_claudeFromMainTerminal_cwdEqualsMainCwd_stillCreatesNewProject() throws {
        appState.createTabFromMainTerminal(cwd: mainCwd, args: [])

        XCTAssertEqual(appState.projects.count, 2)
        let terminals = appState.projects.first!
        XCTAssertEqual(terminals.tabs.count, 1,
                       "Terminals must still have only the Main tab")

        let newProject = try XCTUnwrap(
            appState.projects.first { $0.id != AppState.terminalsProjectId },
            "No non-Terminals project created — Terminals swallowed the new Claude tab"
        )
        XCTAssertEqual(newProject.path, mainCwd)
        XCTAssertEqual(newProject.tabs.count, 1)
    }

    // MARK: - Non-regression

    /// Guards against the fix over-correcting: when a real (non-
    /// Terminals) project's path prefix-matches the cwd, the new tab
    /// must still land in that existing project rather than spawning
    /// a duplicate.
    func test_claudeFromMainTerminal_picksExistingProjectWhenCwdMatches() {
        seedProject(id: "p1", name: "P1", path: "/tmp/p1")

        appState.createTabFromMainTerminal(cwd: "/tmp/p1/sub", args: [])

        XCTAssertEqual(appState.projects.count, 2,
                       "Should reuse p1, not create a third project")
        let p1 = appState.projects.first { $0.id == "p1" }!
        XCTAssertEqual(p1.tabs.count, 2,
                       "New Claude tab must be appended to p1")
        XCTAssertTrue(p1.tabs.last!.panes.contains { $0.kind == .claude })

        let terminals = appState.projects.first!
        XCTAssertEqual(terminals.id, AppState.terminalsProjectId)
        XCTAssertEqual(terminals.tabs.count, 1)
    }

    /// Longest-prefix-match semantics must still hold across the non-
    /// Terminals projects. Given /tmp/p1 and /tmp/p1/nested, a cwd
    /// under /tmp/p1/nested picks the nested project.
    func test_claudeFromMainTerminal_longestPrefixWinsAmongProjects() {
        seedProject(id: "p1", name: "P1", path: "/tmp/p1")
        seedProject(id: "p1-nested", name: "Nested", path: "/tmp/p1/nested")

        appState.createTabFromMainTerminal(
            cwd: "/tmp/p1/nested/x", args: []
        )

        let p1 = appState.projects.first { $0.id == "p1" }!
        let nested = appState.projects.first { $0.id == "p1-nested" }!
        XCTAssertEqual(p1.tabs.count, 1, "Shallower project must not win")
        XCTAssertEqual(nested.tabs.count, 2,
                       "Deeper project is the longest-prefix match")
    }

    // MARK: - Worktree splitting

    /// `-w <name>` relocates the Claude session into
    /// `<cwd>/.claude/worktrees/<name>`. The *project* must still be
    /// named after the parent dir (so repeat `-w` launches bucket
    /// together under one project group), but the tab's cwd — which
    /// seeds the companion terminal's initial dir — must point at the
    /// worktree.
    func test_claudeFromMainTerminal_withWorktreeFlag_splitsProjectAndTabCwd() throws {
        let parent = "\(mainCwd)/Projects/nice"

        appState.createTabFromMainTerminal(cwd: parent, args: ["-w", "worktree-bug"])

        let project = try XCTUnwrap(
            appState.projects.first { $0.id != AppState.terminalsProjectId },
            "A fresh project group should have been created"
        )
        XCTAssertEqual(project.name, "NICE",
                       "Project name must come from the parent dir, not the worktree")
        XCTAssertEqual(project.path, parent,
                       "Project path must be the pre-worktree cwd")

        let tab = try XCTUnwrap(project.tabs.first)
        XCTAssertEqual(tab.cwd, "\(parent)/.claude/worktrees/worktree-bug",
                       "Tab.cwd must point at the worktree so the companion terminal follows the session in")
    }

    /// Long-form `--worktree <name>` behaves the same as `-w <name>`.
    func test_claudeFromMainTerminal_withLongWorktreeFlag_splitsProjectAndTabCwd() throws {
        let parent = "\(mainCwd)/Projects/nice"

        appState.createTabFromMainTerminal(
            cwd: parent, args: ["--worktree", "feature-x"]
        )

        let project = try XCTUnwrap(
            appState.projects.first { $0.id != AppState.terminalsProjectId }
        )
        XCTAssertEqual(project.name, "NICE")
        XCTAssertEqual(project.path, parent)
        XCTAssertEqual(project.tabs.first?.cwd,
                       "\(parent)/.claude/worktrees/feature-x")
    }

    /// When a parent project already exists, `-w` tabs must bucket into
    /// it — both invocations of `claude -w <name>` from the same parent
    /// belong together in the sidebar, not split across worktree-named
    /// projects.
    func test_claudeFromMainTerminal_withWorktreeFlag_bucketsIntoExistingProject() throws {
        seedProject(id: "nice", name: "NICE", path: "/tmp/nice")

        appState.createTabFromMainTerminal(cwd: "/tmp/nice", args: ["-w", "bug"])

        let nice = try XCTUnwrap(appState.projects.first { $0.id == "nice" })
        XCTAssertEqual(nice.tabs.count, 2,
                       "Existing nice project should have absorbed the worktree tab")
        XCTAssertEqual(nice.tabs.last?.cwd,
                       "/tmp/nice/.claude/worktrees/bug")

        XCTAssertNil(appState.projects.first { $0.name == "BUG" },
                     "No BUG project should have been created")
    }

    /// Claude Code sanitizes `/` to `+` when materializing the
    /// worktree directory (so `-w foo/bar` produces
    /// `.claude/worktrees/foo+bar`). Mirror that transformation so
    /// the companion terminal lands in the real directory.
    func test_claudeFromMainTerminal_withWorktreeFlag_slashesInNameReplacedWithPlus() throws {
        let parent = "\(mainCwd)/Projects/nice"

        appState.createTabFromMainTerminal(
            cwd: parent, args: ["-w", "feature/foo/bar"]
        )

        let project = try XCTUnwrap(
            appState.projects.first { $0.id != AppState.terminalsProjectId }
        )
        XCTAssertEqual(project.tabs.first?.cwd,
                       "\(parent)/.claude/worktrees/feature+foo+bar",
                       "Slashes in the worktree name must be replaced with `+` to match Claude Code's directory naming")
    }

    /// No `-w` flag: Tab.cwd matches the project path (pre-existing
    /// behavior). Guards against the split code accidentally applying
    /// a transformation in the non-worktree path.
    func test_claudeFromMainTerminal_withoutWorktreeFlag_tabCwdMatchesProjectPath() throws {
        let cwd = "\(mainCwd)/Projects/plain"

        appState.createTabFromMainTerminal(cwd: cwd, args: [])

        let project = try XCTUnwrap(
            appState.projects.first { $0.id != AppState.terminalsProjectId }
        )
        XCTAssertEqual(project.path, cwd)
        XCTAssertEqual(project.tabs.first?.cwd, cwd)
    }

    // MARK: - extractWorktreeName

    func test_extractWorktreeName_shortFlag() {
        XCTAssertEqual(AppState.extractWorktreeName(from: ["-w", "foo"]), "foo")
    }

    func test_extractWorktreeName_longFlag() {
        XCTAssertEqual(AppState.extractWorktreeName(from: ["--worktree", "foo"]), "foo")
    }

    func test_extractWorktreeName_trailingFlagReturnsNil() {
        XCTAssertNil(AppState.extractWorktreeName(from: ["-w"]))
        XCTAssertNil(AppState.extractWorktreeName(from: ["a", "--worktree"]))
    }

    func test_extractWorktreeName_emptyValueReturnsNil() {
        XCTAssertNil(AppState.extractWorktreeName(from: ["-w", ""]))
    }

    func test_extractWorktreeName_scansPastOtherArgs() {
        XCTAssertEqual(
            AppState.extractWorktreeName(from: ["--model", "sonnet", "-w", "foo"]),
            "foo"
        )
    }

    func test_extractWorktreeName_equalsFormNotRecognized() {
        // Design decision: only space-delimited is supported. `-w=foo`
        // would be a single arg and should return nil.
        XCTAssertNil(AppState.extractWorktreeName(from: ["-w=foo"]))
        XCTAssertNil(AppState.extractWorktreeName(from: ["--worktree=foo"]))
    }

    func test_extractWorktreeName_absentReturnsNil() {
        XCTAssertNil(AppState.extractWorktreeName(from: []))
        XCTAssertNil(AppState.extractWorktreeName(from: ["--model", "sonnet"]))
    }

    // MARK: - resolvedSpawnCwd

    /// When the tab's cwd no longer exists on disk (e.g. a worktree
    /// that the user `rm -rf`'d between launches), fall back to the
    /// containing project's path. Prevents pty spawn failures on
    /// restore.
    func test_resolvedSpawnCwd_fallsBackToProjectPath_whenTabCwdMissing() throws {
        let existingProjectPath = NSTemporaryDirectory()
        let missingWorktree = (existingProjectPath as NSString)
            .appendingPathComponent(".claude/worktrees/deleted-\(UUID().uuidString)")
        XCTAssertFalse(FileManager.default.fileExists(atPath: missingWorktree),
                       "Precondition: worktree path must not exist")

        seedProject(id: "tmp", name: "TMP", path: existingProjectPath)
        let tab = Tab(
            id: "tmp-worktree-tab",
            title: "worktree",
            cwd: missingWorktree,
            panes: [Pane(id: "tmp-worktree-tab-p0", title: "zsh", kind: .terminal)],
            activePaneId: "tmp-worktree-tab-p0"
        )
        let tmpIdx = appState.projects.firstIndex { $0.id == "tmp" }!
        appState.projects[tmpIdx].tabs.append(tab)

        XCTAssertEqual(appState.resolvedSpawnCwd(for: tab), existingProjectPath)
    }

    /// When the tab's cwd exists, the resolver returns it unchanged —
    /// the fallback must not fire in the common case.
    func test_resolvedSpawnCwd_returnsTabCwd_whenItExists() throws {
        let existingDir = NSTemporaryDirectory()
        seedProject(id: "tmp", name: "TMP", path: "/does-not-matter")
        let tab = Tab(
            id: "tmp-real-tab",
            title: "real",
            cwd: existingDir,
            panes: [Pane(id: "tmp-real-tab-p0", title: "zsh", kind: .terminal)],
            activePaneId: "tmp-real-tab-p0"
        )
        let tmpIdx = appState.projects.firstIndex { $0.id == "tmp" }!
        appState.projects[tmpIdx].tabs.append(tab)

        XCTAssertEqual(appState.resolvedSpawnCwd(for: tab), existingDir)
    }

    // MARK: - Restore path fallback

    /// A Claude tab persisted with a worktree cwd can be restored after
    /// the user deletes the worktree directory between app launches.
    /// `addRestoredTabModel` must substitute the containing project's
    /// path so `claude --resume` spawns successfully instead of
    /// failing on the missing directory.
    func test_addRestoredTabModel_missingWorktreeCwd_fallsBackToProjectPath() throws {
        let projectPath = NSTemporaryDirectory()
        let missingWorktree = (projectPath as NSString)
            .appendingPathComponent(".claude/worktrees/deleted-\(UUID().uuidString)")
        XCTAssertFalse(FileManager.default.fileExists(atPath: missingWorktree))

        seedProject(id: "nice", name: "NICE", path: projectPath)
        let projectIdx = try XCTUnwrap(
            appState.projects.firstIndex { $0.id == "nice" }
        )

        let persisted = PersistedTab(
            id: "restored-tab",
            title: "bug",
            cwd: missingWorktree,
            branch: nil,
            claudeSessionId: UUID().uuidString,
            activePaneId: "restored-claude",
            panes: [
                PersistedPane(id: "restored-claude", title: "Claude", kind: .claude),
                PersistedPane(id: "restored-term", title: "Terminal 1", kind: .terminal),
            ]
        )

        let spawn = try XCTUnwrap(
            appState.addRestoredTabModel(persisted, toProjectIndex: projectIdx),
            "Claude tabs must return a pending-spawn tuple"
        )
        XCTAssertEqual(spawn.cwd, projectPath,
                       "Missing worktree must resolve to the project path")
        XCTAssertEqual(spawn.tabId, "restored-tab")
        XCTAssertEqual(spawn.claudePaneId, "restored-claude")
    }

    /// Happy path: when the persisted cwd still exists, the restored
    /// spawn uses it unchanged so `claude --resume` launches in the
    /// worktree the session originally lived in.
    func test_addRestoredTabModel_existingCwd_usesTabCwdUnchanged() throws {
        let existingDir = NSTemporaryDirectory()
        seedProject(id: "nice", name: "NICE", path: "/does-not-matter")
        let projectIdx = try XCTUnwrap(
            appState.projects.firstIndex { $0.id == "nice" }
        )

        let persisted = PersistedTab(
            id: "restored-tab-live",
            title: "live",
            cwd: existingDir,
            branch: nil,
            claudeSessionId: UUID().uuidString,
            activePaneId: "restored-live-claude",
            panes: [
                PersistedPane(id: "restored-live-claude", title: "Claude", kind: .claude),
            ]
        )

        let spawn = try XCTUnwrap(
            appState.addRestoredTabModel(persisted, toProjectIndex: projectIdx)
        )
        XCTAssertEqual(spawn.cwd, existingDir)
    }

    // MARK: - Git-root bucketing

    /// A nested repo under an existing project must not be absorbed
    /// by longest-prefix-matching — it gets its own project anchored
    /// at the inner git root.
    func test_nestedGitRepo_createsSeparateProjectFromOuter() throws {
        let outer = makeGitRepo(at: "outer")
        let nested = makeGitRepo(at: "outer/nested-1")

        seedProject(id: "outer", name: "OUTER", path: outer)

        appState.createTabFromMainTerminal(cwd: nested, args: [])

        let outerProject = try XCTUnwrap(
            appState.projects.first { $0.id == "outer" }
        )
        XCTAssertEqual(outerProject.tabs.count, 1,
                       "Outer must not absorb the nested-repo tab")

        let nestedProject = try XCTUnwrap(
            appState.projects.first {
                $0.id != AppState.terminalsProjectId
                    && $0.id != "outer"
            },
            "A separate project rooted at the nested repo must exist"
        )
        XCTAssertEqual(nestedProject.path, nested)
        XCTAssertEqual(nestedProject.name, "NESTED-1")
        XCTAssertEqual(nestedProject.tabs.count, 1)
    }

    /// A cwd that's a sub-directory of an existing project's git
    /// repo must bucket into that project — git-root anchoring is
    /// what makes intra-repo navigation cluster, not prefix matching.
    func test_subdirOfExistingRepo_bucketsIntoExistingProject() throws {
        let repo = makeGitRepo(at: "repo")
        let sub = makeDir(at: "repo/src/deep")

        seedProject(id: "repo", name: "REPO", path: repo)

        appState.createTabFromMainTerminal(cwd: sub, args: [])

        let repoProject = try XCTUnwrap(
            appState.projects.first { $0.id == "repo" }
        )
        XCTAssertEqual(repoProject.tabs.count, 2,
                       "Sub-dir tab must bucket into the existing repo project")

        XCTAssertNil(
            appState.projects.first {
                $0.id != AppState.terminalsProjectId && $0.id != "repo"
            },
            "No spurious project should have been created for the sub-dir"
        )
    }

    /// First-launch behavior: opening Claude in a sub-directory of a
    /// fresh repo creates a project anchored at the git root, not at
    /// the cwd. Locks in that the new project's `path` is the repo
    /// root so subsequent intra-repo tabs cluster correctly.
    func test_firstCwdInsideRepo_anchorsProjectAtGitRoot() throws {
        let repo = makeGitRepo(at: "repo")
        let sub = makeDir(at: "repo/src/deep")

        appState.createTabFromMainTerminal(cwd: sub, args: [])

        let new = try XCTUnwrap(
            appState.projects.first { $0.id != AppState.terminalsProjectId },
            "A non-Terminals project must be created"
        )
        XCTAssertEqual(new.path, repo,
                       "Project must be anchored at the git root, not the cwd")
        XCTAssertEqual(new.name, "REPO")
        XCTAssertEqual(new.tabs.count, 1)
    }

    /// The rare manual case: the user `cd`'d into a Nice-managed
    /// worktree before invoking `claude`. The new tab should still
    /// bucket into the parent repo's project, matching the bucketing
    /// behavior of `claude -w` (whose pre-worktree cwd is what gets
    /// passed to `addTabToProjects` today).
    func test_cwdInsideNiceWorktree_bucketsIntoParentRepo() throws {
        let repo = makeGitRepo(at: "repo")
        let worktree = makeWorktreeMarker(at: "repo/.claude/worktrees/bug")

        seedProject(id: "repo", name: "REPO", path: repo)

        appState.createTabFromMainTerminal(cwd: worktree, args: [])

        let repoProject = try XCTUnwrap(
            appState.projects.first { $0.id == "repo" }
        )
        XCTAssertEqual(repoProject.tabs.count, 2,
                       "Manual cd into a worktree must still bucket into the parent repo")

        XCTAssertNil(
            appState.projects.first {
                $0.id != AppState.terminalsProjectId && $0.id != "repo"
            },
            "No worktree-named project should have been created"
        )
    }

    // MARK: - Helpers

    /// Plant a `.git` directory under the test's temp filesystem root
    /// so `findGitRoot` walks the real filesystem. Returns the
    /// absolute path of the repo dir (suitable for use as a cwd).
    private func makeGitRepo(at relativePath: String) -> String {
        let dir = gitFsRoot.appendingPathComponent(relativePath, isDirectory: true)
        try? FileManager.default.createDirectory(
            at: dir, withIntermediateDirectories: true
        )
        let dotGit = dir.appendingPathComponent(".git", isDirectory: true)
        try? FileManager.default.createDirectory(
            at: dotGit, withIntermediateDirectories: true
        )
        return dir.path
    }

    /// Create a plain directory (no `.git`) under the test root so
    /// the cwd exists on disk but resolves to an enclosing git root
    /// when one is planted nearby.
    private func makeDir(at relativePath: String) -> String {
        let dir = gitFsRoot.appendingPathComponent(relativePath, isDirectory: true)
        try? FileManager.default.createDirectory(
            at: dir, withIntermediateDirectories: true
        )
        return dir.path
    }

    /// Plant a `.git` *file* (the marker git uses for worktrees and
    /// submodules) so we can test the worktree pre-strip without
    /// also tripping the inner repo as a self-contained git root.
    private func makeWorktreeMarker(at relativePath: String) -> String {
        let dir = gitFsRoot.appendingPathComponent(relativePath, isDirectory: true)
        try? FileManager.default.createDirectory(
            at: dir, withIntermediateDirectories: true
        )
        let dotGit = dir.appendingPathComponent(".git")
        try? "gitdir: /placeholder\n".write(
            to: dotGit, atomically: true, encoding: .utf8
        )
        return dir.path
    }

    /// Append a bare (no-tabs) project to the sidebar. Keeps Terminals
    /// at index 0 to preserve the invariant tests elsewhere depend on.
    private func seedProject(id: String, name: String, path: String) {
        let project = Project(id: id, name: name, path: path, tabs: [
            Tab(
                id: "\(id)-seed", title: "seed", cwd: path,
                panes: [Pane(id: "\(id)-seed-p0", title: "zsh", kind: .terminal)],
                activePaneId: "\(id)-seed-p0"
            )
        ])
        appState.projects.append(project)
    }
}
