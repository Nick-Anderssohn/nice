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
    private let mainCwd = "/tmp/nice-test-home"

    override func setUp() {
        super.setUp()
        setenv("NICE_CLAUDE_OVERRIDE", "/bin/cat", 1)
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

    // MARK: - Helpers

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
