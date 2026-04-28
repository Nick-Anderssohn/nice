//
//  AppStateFileBrowserTests.swift
//  NiceUnitTests
//
//  Coverage for the file-browser surface on `AppState`:
//    • `toggleSidebarMode()` — flip between .tabs and .files.
//    • `toggleFileBrowserHiddenFiles()` — gating logic. Two guards,
//      both load-bearing for the ⌘⇧. shortcut: must be in files
//      mode, and the underlying store-level "if exists" check must
//      not allocate. Together they make the shortcut a true no-op
//      from tabs mode and from tabs that have never opened the
//      file browser.
//    • `fileBrowserHeaderTitle(forTab:)` — the rule that prevents
//      the file-browser view from knowing about
//      `terminalsProjectId`. Three branches: unknown tab, Terminals
//      tab, real-project tab.
//    • `finalizeDissolvedTab` cleanup — closing a tab must drop
//      its `FileBrowserState` so long-lived windows don't leak.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class AppStateFileBrowserTests: XCTestCase {

    private var appState: AppState!

    override func setUp() {
        super.setUp()
        appState = AppState()
    }

    override func tearDown() {
        appState = nil
        super.tearDown()
    }

    // MARK: - toggleSidebarMode

    func test_toggleSidebarMode_flipsBetweenTabsAndFiles() {
        XCTAssertEqual(appState.sidebar.sidebarMode, .tabs, "Default starts in tabs mode.")

        appState.sidebar.toggleSidebarMode()
        XCTAssertEqual(appState.sidebar.sidebarMode, .files)

        appState.sidebar.toggleSidebarMode()
        XCTAssertEqual(appState.sidebar.sidebarMode, .tabs)
    }

    // MARK: - toggleFileBrowserHiddenFiles gating

    func test_toggleHiddenFiles_inTabsMode_isNoop() {
        // Even with a state already existing, the shortcut must
        // refuse to act when the user is looking at the tabs view.
        // Otherwise pressing ⌘⇧. from tabs mode flips a preference
        // for a feature they aren't currently looking at.
        let tabId = injectClaudeTab()
        appState.tabs.activeTabId = tabId
        let state = appState.fileBrowserStore.ensureState(forTab: tabId, cwd: "/tmp/proj")
        let before = state.showHidden
        XCTAssertEqual(appState.sidebar.sidebarMode, .tabs)

        appState.toggleFileBrowserHiddenFiles()

        XCTAssertEqual(state.showHidden, before,
                       "Must not flip when sidebarMode is .tabs.")
    }

    func test_toggleHiddenFiles_inFilesMode_withoutEnsureState_doesNotAllocate() {
        // Critical: pressing the shortcut must not lazily create a
        // FileBrowserState as a side effect. Otherwise users who
        // never open the file browser still accumulate state per tab
        // every time the shortcut fires.
        let tabId = injectClaudeTab()
        appState.tabs.activeTabId = tabId
        appState.sidebar.sidebarMode = .files
        XCTAssertNil(appState.fileBrowserStore.states[tabId])

        appState.toggleFileBrowserHiddenFiles()

        XCTAssertNil(appState.fileBrowserStore.states[tabId],
                     "Must NOT allocate. The shortcut is silent until the user opens the file browser at least once.")
    }

    func test_toggleHiddenFiles_inFilesMode_withState_flips() {
        let tabId = injectClaudeTab()
        appState.tabs.activeTabId = tabId
        appState.sidebar.sidebarMode = .files
        let state = appState.fileBrowserStore.ensureState(forTab: tabId, cwd: "/tmp/proj")
        let before = state.showHidden

        appState.toggleFileBrowserHiddenFiles()

        XCTAssertEqual(state.showHidden, !before)
    }

    func test_toggleHiddenFiles_withNilActiveTab_isNoop() {
        // Manufacture the no-active-tab state. The Main terminal tab
        // is seeded by AppState's init, so explicitly clear it.
        appState.tabs.activeTabId = nil
        appState.sidebar.sidebarMode = .files

        // Must not crash, must not allocate.
        appState.toggleFileBrowserHiddenFiles()

        XCTAssertTrue(appState.fileBrowserStore.states.isEmpty)
    }

    // MARK: - fileBrowserHeaderTitle

    func test_fileBrowserHeaderTitle_unknownTab_returnsFiles() {
        XCTAssertEqual(appState.tabs.fileBrowserHeaderTitle(forTab: "no-such-tab"),
                       "Files",
                       "An unknown tab has no project to name; fall back to a generic label.")
    }

    func test_fileBrowserHeaderTitle_terminalsProjectTab_returnsTabTitle() {
        // The pinned Terminals project's name is generic ("Terminals")
        // — not useful as a header. The rule is to fall back to the
        // tab's own title there.
        let mainId = TabModel.mainTerminalTabId
        XCTAssertNotNil(appState.tabs.tab(for: mainId),
                        "The Main terminal tab is seeded into the Terminals project at init.")

        XCTAssertEqual(appState.tabs.fileBrowserHeaderTitle(forTab: mainId),
                       appState.tabs.tab(for: mainId)?.title)
    }

    func test_fileBrowserHeaderTitle_realProjectTab_returnsProjectName() {
        let tabId = injectClaudeTab(projectName: "MyCoolProject")
        XCTAssertEqual(appState.tabs.fileBrowserHeaderTitle(forTab: tabId),
                       "MyCoolProject")
    }

    // MARK: - Per-tab isolation across active-tab switches

    /// Integration through real `AppState`: two tabs, mutate one,
    /// switch active tab back and forth, confirm each tab's state
    /// is preserved independently. Catches regressions in the
    /// store→view wiring where tab-switching might leak state
    /// between rows or reset the inactive tab on return.
    func test_perTabIsolation_acrossActiveTabSwitch() {
        let tabA = injectClaudeTab(projectName: "A")
        let tabB = injectClaudeTab(projectName: "B")

        // Open the file browser for both tabs; mutate A only.
        let stateA = appState.fileBrowserStore.ensureState(forTab: tabA, cwd: "/tmp/A")
        stateA.expandedPaths.insert("/tmp/A/Sources")
        stateA.showHidden = true

        let stateB = appState.fileBrowserStore.ensureState(forTab: tabB, cwd: "/tmp/B")
        stateB.showHidden = false  // distinguish from A

        // Switch active tab to B and back to A — purely a flip on
        // appState, no view code involved. The store contract says
        // the same instances come back unchanged.
        appState.tabs.activeTabId = tabA
        appState.tabs.activeTabId = tabB
        appState.tabs.activeTabId = tabA

        let stateAAgain = appState.fileBrowserStore.ensureState(forTab: tabA, cwd: "/tmp/A")
        let stateBAgain = appState.fileBrowserStore.ensureState(forTab: tabB, cwd: "/tmp/B")

        XCTAssertTrue(stateAAgain === stateA,
                      "Switching away and back must return the same instance — state must not be re-seeded.")
        XCTAssertTrue(stateBAgain === stateB)
        XCTAssertTrue(stateAAgain.expandedPaths.contains("/tmp/A/Sources"),
                      "Tab A's expansion must survive a round-trip through tab B.")
        XCTAssertTrue(stateAAgain.showHidden, "A's hidden=true must survive.")
        XCTAssertFalse(stateBAgain.showHidden, "B's hidden=false must survive.")
        XCTAssertFalse(stateAAgain.expandedPaths.contains("/tmp/B/Sources"),
                       "A's expansion set must not pick up B's entries — store must keep them keyed apart.")
    }

    // MARK: - finalizeDissolvedTab cleanup

    func test_closingTab_removesFileBrowserState() {
        // Memory leak guard: dissolving a tab must drop its file-
        // browser state from the store. Without this, a long-lived
        // window accumulates state objects for every tab the user
        // ever opened.
        let tabId = injectClaudeTab()
        _ = appState.fileBrowserStore.ensureState(forTab: tabId, cwd: "/tmp/proj")
        XCTAssertNotNil(appState.fileBrowserStore.states[tabId])

        appState.closer.requestCloseTab(tabId: tabId)

        XCTAssertNil(appState.fileBrowserStore.states[tabId],
                     "finalizeDissolvedTab must call fileBrowserStore.removeState so closed tabs don't linger.")
    }

    // MARK: - Helpers

    /// Insert a Claude tab into a fresh project. Mirrors the helper
    /// shape used in `AppStateRenameTabTests` and
    /// `AppStateCloseProjectTests`.
    @discardableResult
    private func injectClaudeTab(projectName: String = "TestProject") -> String {
        let uid = UUID().uuidString
        let tabId = "t-\(uid)"
        let claudePaneId = "\(tabId)-claude"
        let terminalPaneId = "\(tabId)-t1"
        let tab = Tab(
            id: tabId,
            title: "New tab",
            cwd: "/tmp/\(projectName)",
            branch: nil,
            panes: [
                Pane(id: claudePaneId, title: "Claude", kind: .claude),
                Pane(id: terminalPaneId, title: "Terminal 1", kind: .terminal),
            ],
            activePaneId: claudePaneId,
            claudeSessionId: "session-\(tabId)"
        )
        let project = Project(
            id: "p-\(uid)",
            name: projectName,
            path: "/tmp/\(projectName)",
            tabs: [tab]
        )
        appState.tabs.projects.append(project)
        return tabId
    }
}
