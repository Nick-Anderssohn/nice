//
//  TabModelProjectRepairTests.swift
//  NiceUnitTests
//
//  Self-heal pass for the persisted project structure. Locks in that
//  `repairProjectStructure` promotes sub-directory project paths to
//  their git roots, moves tabs that live in a nested repo into a
//  project anchored at that nested repo, merges projects that converge
//  on the same path, drops empty non-Terminals projects, and never
//  touches the pinned Terminals group.
//
//  Each test plants real `.git` markers under a per-test temp dir so
//  `findGitRoot` walks the actual filesystem (matches production).
//  Hand-seeds `appState.tabs.projects` for full control over starting state
//  rather than going through `createTabFromMainTerminal` (which would
//  pre-route the tab via the new bucketing logic and obscure what the
//  repair pass is doing on its own).
//

import Darwin
import Foundation
import XCTest
@testable import Nice

@MainActor
final class TabModelProjectRepairTests: XCTestCase {

    private var appState: AppState!
    private var homeSandbox: TestHomeSandbox!
    private var gitFsRoot: URL!
    private let mainCwd = "/tmp/nice-test-home-repair"

    override func setUp() {
        super.setUp()
        homeSandbox = TestHomeSandbox()
        setenv("NICE_CLAUDE_OVERRIDE", "/bin/cat", 1)
        gitFsRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent(
                "nice-repair-\(UUID().uuidString)", isDirectory: true
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

    // MARK: - Tab moves

    /// A tab whose cwd lives in a nested repo must be relocated into
    /// a project anchored at that nested repo, even though its
    /// containing project's path is the outer repo.
    func test_repair_movesNestedTabIntoOwnProject() throws {
        let outer = makeGitRepo(at: "outer")
        let nested = makeGitRepo(at: "outer/nested-1")

        appState.tabs.projects.append(Project(
            id: "outer", name: "OUTER", path: outer,
            tabs: [
                makeTab(id: "outer-seed", cwd: outer),
                makeTab(id: "stray-nested", cwd: nested),
            ]
        ))

        appState.tabs.repairProjectStructure()

        let outerProject = try XCTUnwrap(
            appState.tabs.projects.first { $0.id == "outer" },
            "Outer project must survive — its seed tab still belongs to it"
        )
        XCTAssertEqual(outerProject.tabs.count, 1,
                       "Only the nested-cwd tab should have been moved")
        XCTAssertEqual(outerProject.tabs.first?.id, "outer-seed")

        let nestedProject = try XCTUnwrap(
            appState.tabs.projects.first { $0.path == nested },
            "A new project anchored at the nested repo must exist"
        )
        XCTAssertNotEqual(nestedProject.id, TabModel.terminalsProjectId,
                          "Nested project must not collide with the Terminals id")
        XCTAssertNotEqual(nestedProject.id, "outer",
                          "Nested project must have its own id, not reuse outer's")
        XCTAssertTrue(nestedProject.id.hasPrefix("p-nested-1-"),
                      "Generated id must follow the `p-<name>-<suffix>` convention")
        XCTAssertEqual(nestedProject.name, "NESTED-1")
        XCTAssertEqual(nestedProject.tabs.count, 1)
        XCTAssertEqual(nestedProject.tabs.first?.id, "stray-nested")
    }

    /// Pass 1 (promotion) and Pass 2 (tab move) must compose: a
    /// project rooted at `outer/sub` (a sub-dir of repo `outer`)
    /// holding a tab whose cwd is in a *nested* repo `outer/sub/nested`
    /// must end with the project promoted to `outer` and the nested-
    /// cwd tab relocated to its own `outer/sub/nested` project.
    func test_repair_promotionThenMoveCompose() throws {
        let outer = makeGitRepo(at: "outer")
        let _ = makeDir(at: "outer/sub")
        let nested = makeGitRepo(at: "outer/sub/nested")
        let subPath = "\(gitFsRoot.path)/outer/sub"

        appState.tabs.projects.append(Project(
            id: "p-sub-original", name: "SUB", path: subPath,
            tabs: [
                makeTab(id: "sub-seed", cwd: subPath),
                makeTab(id: "deep-nested", cwd: nested),
            ]
        ))

        appState.tabs.repairProjectStructure()

        let promoted = try XCTUnwrap(
            appState.tabs.projects.first { $0.id == "p-sub-original" },
            "Original project id must survive promotion"
        )
        XCTAssertEqual(promoted.path, outer,
                       "Pass 1 must promote outer/sub to outer")
        XCTAssertEqual(promoted.name, "OUTER")
        XCTAssertEqual(promoted.tabs.map(\.id), ["sub-seed"],
                       "After promotion, sub-seed's anchor matches outer; deep-nested moves out")

        let nestedProject = try XCTUnwrap(
            appState.tabs.projects.first { $0.path == nested },
            "Pass 2 must create a project for the nested-cwd tab"
        )
        XCTAssertEqual(nestedProject.tabs.map(\.id), ["deep-nested"])
    }

    /// A tab whose cwd is gone from disk (e.g. a worktree the user
    /// `rm -rf`'d) stays where it is — repair has no anchor to
    /// compute against, and `resolvedSpawnCwd` already handles the
    /// missing-cwd case at spawn time.
    func test_repair_skipsTabsWithMissingCwd() throws {
        let repo = makeGitRepo(at: "repo")
        let missing = "\(gitFsRoot.path)/repo/.claude/worktrees/deleted-\(UUID().uuidString)"
        XCTAssertFalse(FileManager.default.fileExists(atPath: missing),
                       "Precondition: missing cwd must not exist on disk")

        appState.tabs.projects.append(Project(
            id: "repo", name: "REPO", path: repo,
            tabs: [makeTab(id: "ghost", cwd: missing)]
        ))

        appState.tabs.repairProjectStructure()

        let project = try XCTUnwrap(
            appState.tabs.projects.first { $0.id == "repo" },
            "Repo project must remain because its tab can't be re-bucketed"
        )
        XCTAssertEqual(project.tabs.count, 1)
        XCTAssertEqual(project.tabs.first?.id, "ghost")
    }

    // MARK: - Promotion

    /// A project whose `path` is a sub-directory of a git repo gets
    /// promoted: its `path` becomes the git root and its `name`
    /// becomes the git root's last path component, uppercased. Tabs
    /// whose cwd is inside the same repo stay put.
    func test_repair_promotesSubdirProjectToGitRoot() throws {
        let repo = makeGitRepo(at: "repo")
        let deep = makeDir(at: "repo/src/deep")

        appState.tabs.projects.append(Project(
            id: "p-deep-123", name: "DEEP", path: deep,
            tabs: [makeTab(id: "deep-tab", cwd: deep)]
        ))

        appState.tabs.repairProjectStructure()

        XCTAssertEqual(
            appState.tabs.projects.filter { $0.id != TabModel.terminalsProjectId }.count,
            1,
            "Promotion shouldn't create or drop projects on its own"
        )
        let promoted = try XCTUnwrap(
            appState.tabs.projects.first { $0.id == "p-deep-123" },
            "Project id must be preserved across promotion"
        )
        XCTAssertEqual(promoted.path, repo,
                       "Path must be promoted to the git root")
        XCTAssertEqual(promoted.name, "REPO",
                       "Name must follow the new git root's last component")
        XCTAssertEqual(promoted.tabs.count, 1,
                       "Tab inside the same repo must stay put")
        XCTAssertEqual(promoted.tabs.first?.id, "deep-tab")
    }

    // MARK: - Merge

    /// Two non-Terminals projects whose paths converge on the same
    /// expanded path must be merged into the lower-index project.
    /// Common case: promotion rewrites two separate sub-dir projects
    /// to the same git root.
    func test_repair_mergesDuplicateProjectsAtSameGitRoot() throws {
        let repo = makeGitRepo(at: "repo")

        appState.tabs.projects.append(Project(
            id: "first", name: "REPO", path: repo,
            tabs: [makeTab(id: "first-tab", cwd: repo)]
        ))
        appState.tabs.projects.append(Project(
            id: "second", name: "REPO", path: repo,
            tabs: [makeTab(id: "second-tab", cwd: repo)]
        ))

        appState.tabs.repairProjectStructure()

        let nonTerminals = appState.tabs.projects.filter {
            $0.id != TabModel.terminalsProjectId
        }
        XCTAssertEqual(nonTerminals.count, 1,
                       "Duplicate at the same path must be merged into one")

        let canonical = try XCTUnwrap(
            appState.tabs.projects.first { $0.id == "first" },
            "Lowest-index duplicate wins as canonical"
        )
        XCTAssertEqual(canonical.tabs.count, 2,
                       "Canonical project must inherit tabs from the merged dupe")
        XCTAssertEqual(
            canonical.tabs.map(\.id),
            ["first-tab", "second-tab"],
            "Tab order: canonical's own tabs first, then the merged dupe's"
        )
        XCTAssertNil(appState.tabs.projects.first { $0.id == "second" },
                     "Merged dupe must be removed")
    }

    // MARK: - Empty cleanup

    /// A non-Terminals project that ends up with zero tabs after
    /// repair must be dropped from the sidebar. Terminals stays even
    /// if it ends up empty (it's pinned, not data-driven).
    func test_repair_dropsEmptyProjectsButPreservesTerminals() throws {
        appState.tabs.projects.append(Project(
            id: "abandoned", name: "GHOST", path: "/tmp/no-tabs-here", tabs: []
        ))

        let terminalsBeforeId = appState.tabs.projects.first?.id

        appState.tabs.repairProjectStructure()

        XCTAssertEqual(appState.tabs.projects.first?.id, terminalsBeforeId,
                       "Terminals must remain pinned at index 0")
        XCTAssertEqual(appState.tabs.projects.first?.id, TabModel.terminalsProjectId)
        XCTAssertNil(appState.tabs.projects.first { $0.id == "abandoned" },
                     "Empty non-Terminals project must be dropped")
    }

    /// Belt-and-suspenders: the Terminals project's path is the Main
    /// Terminal cwd, which has no `.git`. None of the four passes
    /// should rewrite Terminals' path/name, move its tabs, merge it
    /// into another project, or remove it.
    func test_repair_leavesTerminalsProjectAlone() throws {
        let terminalsBefore = try XCTUnwrap(
            appState.tabs.projects.first { $0.id == TabModel.terminalsProjectId }
        )
        let beforePath = terminalsBefore.path
        let beforeName = terminalsBefore.name
        let beforeTabIds = terminalsBefore.tabs.map(\.id)

        appState.tabs.repairProjectStructure()

        let terminalsAfter = try XCTUnwrap(
            appState.tabs.projects.first { $0.id == TabModel.terminalsProjectId }
        )
        XCTAssertEqual(terminalsAfter.path, beforePath)
        XCTAssertEqual(terminalsAfter.name, beforeName)
        XCTAssertEqual(terminalsAfter.tabs.map(\.id), beforeTabIds)
    }

    // MARK: - Idempotence

    /// Running repair twice on a structure that needed real changes
    /// produces no further mutations on the second pass — projects
    /// (ids, paths, names) and tab membership are stable.
    func test_repair_isIdempotent() throws {
        let outer = makeGitRepo(at: "outer")
        let nested = makeGitRepo(at: "outer/nested-1")
        let deep = makeDir(at: "outer/src/deep")

        appState.tabs.projects.append(Project(
            id: "outer", name: "OUTER", path: outer,
            tabs: [
                makeTab(id: "outer-seed", cwd: outer),
                makeTab(id: "stray-nested", cwd: nested),
                makeTab(id: "deep-sub", cwd: deep),
            ]
        ))
        appState.tabs.projects.append(Project(
            id: "p-deep-123", name: "DEEP", path: deep, tabs: []
        ))

        appState.tabs.repairProjectStructure()
        let snapshot = appState.tabs.projects.map { project -> ProjectSnapshot in
            ProjectSnapshot(
                id: project.id,
                name: project.name,
                path: project.path,
                tabIds: project.tabs.map(\.id)
            )
        }

        appState.tabs.repairProjectStructure()
        let after = appState.tabs.projects.map { project -> ProjectSnapshot in
            ProjectSnapshot(
                id: project.id,
                name: project.name,
                path: project.path,
                tabIds: project.tabs.map(\.id)
            )
        }

        XCTAssertEqual(after, snapshot,
                       "Second repair pass must not mutate a repaired structure")
    }

    // MARK: - Helpers

    private struct ProjectSnapshot: Equatable {
        let id: String
        let name: String
        let path: String
        let tabIds: [String]
    }

    /// Build a Tab with a single terminal pane. Avoids spinning up a
    /// pty (repair never spawns) so tests stay fast and pure.
    private func makeTab(id: String, cwd: String) -> Tab {
        Tab(
            id: id,
            title: id,
            cwd: cwd,
            panes: [Pane(id: "\(id)-p0", title: "zsh", kind: .terminal)],
            activePaneId: "\(id)-p0"
        )
    }

    /// Plant a `.git` directory at `<gitFsRoot>/<relativePath>` and
    /// return the absolute path of the containing dir.
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

    /// Plain directory under the test root — exists on disk but has
    /// no `.git`, so `findGitRoot` walks past it to the enclosing
    /// repo.
    private func makeDir(at relativePath: String) -> String {
        let dir = gitFsRoot.appendingPathComponent(relativePath, isDirectory: true)
        try? FileManager.default.createDirectory(
            at: dir, withIntermediateDirectories: true
        )
        return dir.path
    }
}
