//
//  FileBrowserStateTests.swift
//  NiceUnitTests
//
//  Pure-state coverage for `FileBrowserState`:
//    • `defaultShowHidden(forCwd:)` — the home-vs-elsewhere heuristic
//      that determines initial dotfile visibility. Subtle: tilde
//      expansion, trailing slashes, sub-paths.
//    • `init(rootPath:)` — must seed `expandedPaths` with the root
//      so the tree shows children on first render.
//    • `rootPath` `didSet` — every reroot must add the new root to
//      `expandedPaths`. This is what makes breadcrumb up-nav and
//      double-click reroot show the new root expanded.
//    • `toggleExpansion(of:)` — symmetric add/remove.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class FileBrowserStateTests: XCTestCase {

    // MARK: - defaultShowHidden(forCwd:)

    func test_defaultShowHidden_inHomeDir_isFalse() {
        let home = NSHomeDirectory()
        XCTAssertFalse(FileBrowserState.defaultShowHidden(forCwd: home),
                       "$HOME should default to hidden-off so the dotfile flood doesn't dominate the user's first view.")
    }

    func test_defaultShowHidden_homeDirWithTrailingSlash_isFalse() {
        let home = NSHomeDirectory() + "/"
        XCTAssertFalse(FileBrowserState.defaultShowHidden(forCwd: home),
                       "URL standardization must canonicalize trailing slashes so '~/' and '~' agree.")
    }

    func test_defaultShowHidden_tildePath_isFalse() {
        XCTAssertFalse(FileBrowserState.defaultShowHidden(forCwd: "~"),
                       "Tilde must expand to $HOME before comparison.")
    }

    func test_defaultShowHidden_subdirOfHome_isTrue() {
        let sub = NSHomeDirectory() + "/Projects"
        XCTAssertTrue(FileBrowserState.defaultShowHidden(forCwd: sub),
                      "Anything strictly below $HOME is a project-shaped location where dotfiles are content.")
    }

    func test_defaultShowHidden_unrelatedPath_isTrue() {
        XCTAssertTrue(FileBrowserState.defaultShowHidden(forCwd: "/tmp/some-project"))
    }

    func test_defaultShowHidden_filesystemRoot_isTrue() {
        // Root isn't $HOME, so it gets the show-hidden default. (Not a
        // happy path — included to lock the "exact match only" rule.)
        XCTAssertTrue(FileBrowserState.defaultShowHidden(forCwd: "/"))
    }

    // MARK: - init

    func test_init_seedsExpandedPathsWithRoot() {
        let state = FileBrowserState(rootPath: "/tmp/proj")
        XCTAssertTrue(state.expandedPaths.contains("/tmp/proj"),
                      "Root must be expanded by default — `didSet` doesn't fire from `init`, so the constructor must seed explicitly.")
    }

    func test_init_seedsShowHiddenFromCwd() {
        let homeState = FileBrowserState(rootPath: NSHomeDirectory())
        XCTAssertFalse(homeState.showHidden,
                       "Home tabs default to hidden-off.")

        let projectState = FileBrowserState(rootPath: "/tmp/proj")
        XCTAssertTrue(projectState.showHidden,
                      "Non-home tabs default to hidden-on.")
    }

    // MARK: - rootPath didSet

    func test_rootPath_didSet_addsNewRootToExpandedPaths() {
        let state = FileBrowserState(rootPath: "/tmp/proj")
        state.rootPath = "/tmp/proj/Sources"

        XCTAssertTrue(state.expandedPaths.contains("/tmp/proj/Sources"),
                      "Reroot (breadcrumb up-nav, double-click, header click) must auto-expand the new root so its children render immediately.")
    }

    func test_rootPath_didSet_preservesPriorExpansionInOtherSubtrees() {
        // Reroot doesn't *clear* expandedPaths — it just adds the new
        // root. Previously-expanded paths stay in the set, even
        // though the tree's `.id(state.rootPath)` rebuild won't
        // render them under the new root.
        let state = FileBrowserState(rootPath: "/tmp/proj")
        state.expandedPaths.insert("/tmp/proj/Sources")
        state.rootPath = "/elsewhere"

        XCTAssertTrue(state.expandedPaths.contains("/tmp/proj/Sources"),
                      "Reroot must not erase existing expansion entries.")
        XCTAssertTrue(state.expandedPaths.contains("/elsewhere"),
                      "Reroot must add the new root.")
    }

    // MARK: - toggleExpansion

    func test_toggleExpansion_addsThenRemoves() {
        let state = FileBrowserState(rootPath: "/tmp/proj")
        XCTAssertFalse(state.expandedPaths.contains("/tmp/proj/Sources"))

        state.toggleExpansion(of: "/tmp/proj/Sources")
        XCTAssertTrue(state.expandedPaths.contains("/tmp/proj/Sources"))

        state.toggleExpansion(of: "/tmp/proj/Sources")
        XCTAssertFalse(state.expandedPaths.contains("/tmp/proj/Sources"))
    }

    func test_toggleExpansion_doesNotAffectOtherEntries() {
        let state = FileBrowserState(rootPath: "/tmp/proj")
        state.expandedPaths.insert("/tmp/proj/Sources")
        state.toggleExpansion(of: "/tmp/proj/Tests")

        XCTAssertTrue(state.expandedPaths.contains("/tmp/proj/Sources"),
                      "Toggling one path must not touch others.")
        XCTAssertTrue(state.expandedPaths.contains("/tmp/proj/Tests"))
    }
}
