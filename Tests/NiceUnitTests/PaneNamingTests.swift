//
//  PaneNamingTests.swift
//  NiceUnitTests
//
//  Tests for the monotonic "Terminal N" auto-naming system and the
//  `renamePane` mutation. Covers:
//
//  - `addPane` hands out monotonically increasing numbers even after
//    a pane is closed (no number reuse).
//  - `renamePane` trims whitespace, rejects empty input, fires
//    `onTreeMutation`, and leaves other panes untouched.
//  - Hydration: a `PersistedTab` with `nextTerminalIndex == nil` and
//    parseable pane titles recomputes the counter correctly.
//  - Floor: all-unparseable pane titles hydrate to `nextTerminalIndex == 1`.
//  - Round-trip: `nextTerminalIndex` survives encode/decode through
//    `PersistedTab`.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class PaneNamingTests: XCTestCase {

    private var appState: AppState!
    private var homeSandbox: TestHomeSandbox!

    override func setUp() {
        super.setUp()
        homeSandbox = TestHomeSandbox()
        appState = AppState()
    }

    override func tearDown() {
        appState = nil
        homeSandbox.teardown()
        homeSandbox = nil
        super.tearDown()
    }

    // MARK: - addPane monotonic numbering

    /// Build [T1, T2, T3], close T2, add a new pane → expect "Terminal 4".
    func test_addPane_isMonotonicAfterClosingAPane() {
        let tabId = TabModel.mainTerminalTabId

        // The seed Main tab already contains "Terminal 1" with
        // nextTerminalIndex == 2 from the init.
        let pane2id = appState.sessions.addPane(tabId: tabId, kind: .terminal)!
        let pane3id = appState.sessions.addPane(tabId: tabId, kind: .terminal)!

        let tab = appState.tabs.tab(for: tabId)!
        XCTAssertEqual(tab.panes[1].title, "Terminal 2")
        XCTAssertEqual(tab.panes[2].title, "Terminal 3")

        // Close Terminal 2 by simulating paneExited (remove from model).
        // Use mutateTab directly so we don't need a live pty.
        appState.tabs.mutateTab(id: tabId) { t in
            t.panes.removeAll { $0.id == pane2id }
        }
        _ = pane3id // silence unused warning; T3 stays in the tab

        let pane4id = appState.sessions.addPane(tabId: tabId, kind: .terminal)!
        let tabAfter = appState.tabs.tab(for: tabId)!
        let newPane = tabAfter.panes.first { $0.id == pane4id }!
        XCTAssertEqual(newPane.title, "Terminal 4",
                       "Closing T2 must not reuse the number — next add should be T4.")
        XCTAssertEqual(tabAfter.nextTerminalIndex, 5,
                       "Closing a pane must not decrement the counter.")
    }

    /// Right after launch, the seeded Main tab must show "Terminal 1"
    /// (not the legacy "zsh") and have the counter primed at 2 so the
    /// first user-driven add lands on "Terminal 2".
    func test_seedMainTab_initialPaneTitleIsTerminal1() {
        let mainTab = appState.tabs.tab(for: TabModel.mainTerminalTabId)!
        XCTAssertEqual(mainTab.panes.first?.title, "Terminal 1",
                       "Initial Terminals/Main pane must be named 'Terminal 1', not 'zsh'.")
        XCTAssertEqual(mainTab.nextTerminalIndex, 2,
                       "Seed counter must start at 2 so the first add becomes Terminal 2.")
    }

    /// Even when a caller passes an explicit `title:` (e.g. the file
    /// browser's "open in editor" path), the counter still advances —
    /// the slot is consumed regardless of name. Locks down the policy
    /// so a refactor that skipped the increment for explicit titles
    /// would fail loudly here.
    func test_addPane_explicitTitleStillIncrementsCounter() {
        let tabId = TabModel.mainTerminalTabId
        let counterBefore = appState.tabs.tab(for: tabId)!.nextTerminalIndex
        let panesBefore = appState.tabs.tab(for: tabId)!.panes.count

        // Look up the new pane by its position rather than its returned
        // id — `addPane` and the seed both stamp ids from
        // `Date().timeIntervalSince1970 * 1000`, so a sub-millisecond
        // gap between AppState init and this call could collide.
        _ = appState.sessions.addPane(
            tabId: tabId, kind: .terminal, title: "vim foo.swift"
        )

        let tab = appState.tabs.tab(for: tabId)!
        XCTAssertEqual(tab.panes.count, panesBefore + 1,
                       "addPane must append a pane.")
        XCTAssertEqual(tab.panes.last?.title, "vim foo.swift",
                       "Explicit title must be used verbatim, not auto-overridden.")
        XCTAssertEqual(tab.nextTerminalIndex, counterBefore + 1,
                       "Explicit title must still advance the counter.")
    }

    /// After adding three panes, `nextTerminalIndex` should be 4 (seed
    /// starts at 2, each add increments once).
    func test_addPane_incrementsCounter() {
        let tabId = TabModel.mainTerminalTabId

        _ = appState.sessions.addPane(tabId: tabId, kind: .terminal)
        _ = appState.sessions.addPane(tabId: tabId, kind: .terminal)
        _ = appState.sessions.addPane(tabId: tabId, kind: .terminal)

        let tab = appState.tabs.tab(for: tabId)!
        XCTAssertEqual(tab.nextTerminalIndex, 5,
                       "Seed starts at 2; three adds → counter reaches 5.")
    }

    // MARK: - renamePane

    func test_renamePane_changesTitle() {
        let tabId = TabModel.mainTerminalTabId
        let paneId = appState.tabs.tab(for: tabId)!.panes[0].id

        appState.tabs.renamePane(tabId: tabId, paneId: paneId, to: "logs")

        let tab = appState.tabs.tab(for: tabId)!
        XCTAssertEqual(tab.panes[0].title, "logs")
    }

    func test_renamePane_trimsWhitespace() {
        let tabId = TabModel.mainTerminalTabId
        let paneId = appState.tabs.tab(for: tabId)!.panes[0].id

        appState.tabs.renamePane(tabId: tabId, paneId: paneId, to: "  padded  ")

        XCTAssertEqual(appState.tabs.tab(for: tabId)!.panes[0].title, "padded")
    }

    /// Submitting an empty title in the pill editor releases the
    /// manual-set lock and resets the pane to its per-kind auto-
    /// default. Terminal panes consume the next slot from the
    /// monotonic counter — same policy `addPane` uses, so a future
    /// addPane never collides with the freshly-reset pane's name.
    func test_renamePane_emptyInput_resetsToAutoDefault_clearsFlag() {
        let tabId = TabModel.mainTerminalTabId
        let paneId = appState.tabs.tab(for: tabId)!.panes[0].id
        let counterBefore = appState.tabs.tab(for: tabId)!.nextTerminalIndex

        // Lock the title with a manual rename.
        appState.tabs.renamePane(tabId: tabId, paneId: paneId, to: "logs")
        XCTAssertTrue(
            appState.tabs.tab(for: tabId)!.panes[0].titleManuallySet,
            "Pre-condition: a non-empty rename must flip titleManuallySet."
        )

        // Empty submit — release the lock and reset.
        appState.tabs.renamePane(tabId: tabId, paneId: paneId, to: "  ")

        let tab = appState.tabs.tab(for: tabId)!
        let pane = tab.panes[0]
        XCTAssertFalse(
            pane.titleManuallySet,
            "Empty submit must clear the manual-set lock."
        )
        XCTAssertEqual(
            pane.title, "Terminal \(counterBefore)",
            "Empty submit must reset to the auto-default consuming the next counter slot."
        )
        XCTAssertEqual(
            tab.nextTerminalIndex, counterBefore + 1,
            "The reset path must advance the monotonic counter."
        )
    }

    /// A non-empty rename flips `titleManuallySet` so subsequent OSC
    /// titles in `paneTitleChanged` can't overwrite the user's pick.
    func test_renamePane_setsTitleManuallySet() {
        let tabId = TabModel.mainTerminalTabId
        let paneId = appState.tabs.tab(for: tabId)!.panes[0].id

        appState.tabs.renamePane(tabId: tabId, paneId: paneId, to: "build")

        let pane = appState.tabs.tab(for: tabId)!.panes[0]
        XCTAssertEqual(pane.title, "build")
        XCTAssertTrue(
            pane.titleManuallySet,
            "renamePane with non-empty input must flip titleManuallySet so OSC titles can't clobber the user's choice."
        )
    }

    func test_renamePane_firesOnTreeMutation() {
        let tabId = TabModel.mainTerminalTabId
        let paneId = appState.tabs.tab(for: tabId)!.panes[0].id
        var fired = false
        appState.tabs.onTreeMutation = { fired = true }

        appState.tabs.renamePane(tabId: tabId, paneId: paneId, to: "new name")

        XCTAssertTrue(fired, "renamePane must call onTreeMutation when the title changes.")
    }

    func test_renamePane_doesNotFireOnTreeMutationWhenNoChange() {
        let tabId = TabModel.mainTerminalTabId
        let paneId = appState.tabs.tab(for: tabId)!.panes[0].id

        // First rename locks the title (title change + flag flip both
        // count as changes). The SECOND rename to the same value is the
        // true no-op we're pinning here.
        appState.tabs.renamePane(tabId: tabId, paneId: paneId, to: "logs")

        var fired = false
        appState.tabs.onTreeMutation = { fired = true }
        appState.tabs.renamePane(tabId: tabId, paneId: paneId, to: "logs")

        XCTAssertFalse(fired,
                       "onTreeMutation must not fire when the title and lock state are both unchanged.")
    }

    func test_renamePane_doesNotTouchOtherPanes() {
        let tabId = TabModel.mainTerminalTabId

        // Inject a second pane directly into the model with a
        // deterministic id so this test is immune to the
        // millisecond-granularity id scheme used by addPane.
        let pane1id = appState.tabs.tab(for: tabId)!.panes[0].id
        let pane2id = "\(tabId)-p-test-stable"
        appState.tabs.mutateTab(id: tabId) { tab in
            tab.panes.append(Pane(id: pane2id, title: "Terminal 2", kind: .terminal))
        }
        let pane2TitleBefore = appState.tabs.tab(for: tabId)!.panes.first {
            $0.id == pane2id
        }!.title

        appState.tabs.renamePane(tabId: tabId, paneId: pane1id, to: "renamed")

        let pane2After = appState.tabs.tab(for: tabId)!.panes.first {
            $0.id == pane2id
        }!
        XCTAssertEqual(pane2After.title, pane2TitleBefore,
                       "Renaming pane 1 must not affect pane 2's title.")
    }

    // MARK: - Hydration: nextTerminalIndex from pane titles

    /// A `PersistedTab` with `nextTerminalIndex == nil` and panes
    /// `[Terminal 1, Terminal 2, "logs"]` should hydrate with
    /// `nextTerminalIndex == 3`.
    func test_hydration_computesCounterFromPaneTitles() {
        let pt = makePersistedTabWithPanes(
            titles: ["Terminal 1", "Terminal 2", "logs"],
            savedCounter: nil
        )
        let fake = FakeSessionStore()
        let tabs = TabModel(initialMainCwd: "/tmp/pane-naming-test")
        let sessions = SessionsModel(tabs: tabs)
        let sidebar = SidebarModel(initialCollapsed: false, initialMode: .tabs)
        let ws = WindowSession(
            tabs: tabs, sessions: sessions, sidebar: sidebar,
            windowSessionId: "w1",
            persistenceEnabled: false,
            store: fake,
            claimLedger: WindowClaimLedger()
        )

        let pi = tabs.ensureProject(
            id: "proj-hydrate", name: "Hydrate", path: "/tmp/pane-naming-test"
        )
        ws.addRestoredTabModel(pt, toProjectIndex: pi)

        let tab = tabs.tab(for: pt.id)!
        XCTAssertEqual(tab.nextTerminalIndex, 3,
                       "max(T1=1, T2=2) + 1 = 3; should be computed from pane titles.")

        sessions.tearDown()
    }

    /// A `PersistedTab` with `nextTerminalIndex == nil` and all
    /// unparseable pane titles should floor to `nextTerminalIndex == 1`.
    func test_hydration_floorsToOneWhenNoParsableTitle() {
        let pt = makePersistedTabWithPanes(
            titles: ["logs", "zsh"],
            savedCounter: nil
        )
        let fake = FakeSessionStore()
        let tabs = TabModel(initialMainCwd: "/tmp/pane-naming-test")
        let sessions = SessionsModel(tabs: tabs)
        let sidebar = SidebarModel(initialCollapsed: false, initialMode: .tabs)
        let ws = WindowSession(
            tabs: tabs, sessions: sessions, sidebar: sidebar,
            windowSessionId: "w2",
            persistenceEnabled: false,
            store: fake,
            claimLedger: WindowClaimLedger()
        )

        let pi = tabs.ensureProject(
            id: "proj-floor", name: "Floor", path: "/tmp/pane-naming-test"
        )
        ws.addRestoredTabModel(pt, toProjectIndex: pi)

        let tab = tabs.tab(for: pt.id)!
        XCTAssertEqual(tab.nextTerminalIndex, 1,
                       "No parseable Terminal-N titles → floor at 1.")

        sessions.tearDown()
    }

    /// A `PersistedTab` with a non-nil `nextTerminalIndex` uses the
    /// saved value directly, ignoring pane titles.
    func test_hydration_usesSavedCounterWhenPresent() {
        let pt = makePersistedTabWithPanes(
            titles: ["Terminal 1"],
            savedCounter: 7
        )
        let fake = FakeSessionStore()
        let tabs = TabModel(initialMainCwd: "/tmp/pane-naming-test")
        let sessions = SessionsModel(tabs: tabs)
        let sidebar = SidebarModel(initialCollapsed: false, initialMode: .tabs)
        let ws = WindowSession(
            tabs: tabs, sessions: sessions, sidebar: sidebar,
            windowSessionId: "w3",
            persistenceEnabled: false,
            store: fake,
            claimLedger: WindowClaimLedger()
        )

        let pi = tabs.ensureProject(
            id: "proj-saved", name: "Saved", path: "/tmp/pane-naming-test"
        )
        ws.addRestoredTabModel(pt, toProjectIndex: pi)

        let tab = tabs.tab(for: pt.id)!
        XCTAssertEqual(tab.nextTerminalIndex, 7,
                       "Saved counter (7) must take priority over pane-title computation.")

        sessions.tearDown()
    }

    // MARK: - Persistence round-trip

    /// `nextTerminalIndex` must survive encode/decode through `PersistedTab`.
    func test_persistedTab_roundTripsNextTerminalIndex() throws {
        let pt = PersistedTab(
            id: "t-rt",
            title: "Round Trip",
            cwd: "/tmp",
            branch: nil,
            claudeSessionId: nil,
            activePaneId: nil,
            panes: [],
            titleManuallySet: nil,
            nextTerminalIndex: 42
        )

        let encoder = JSONEncoder()
        let data = try encoder.encode(pt)
        let decoded = try JSONDecoder().decode(PersistedTab.self, from: data)

        XCTAssertEqual(decoded.nextTerminalIndex, 42,
                       "nextTerminalIndex must survive JSON encode/decode.")
    }

    /// Older session files that omit `nextTerminalIndex` must decode to
    /// `nil` (so hydration falls back to the pane-title computation).
    func test_persistedTab_decodesNilWhenFieldAbsent() throws {
        // Minimal JSON without nextTerminalIndex key.
        let json = """
        {
            "id": "t-old",
            "title": "Old Tab",
            "cwd": "/tmp",
            "activePaneId": null,
            "panes": [],
            "claudeSessionId": null,
            "branch": null
        }
        """
        let data = json.data(using: .utf8)!
        let decoded = try JSONDecoder().decode(PersistedTab.self, from: data)

        XCTAssertNil(decoded.nextTerminalIndex,
                     "nextTerminalIndex absent in older JSON must decode as nil.")
    }

    // MARK: - Snapshot round-trip via snapshotPersistedWindow

    /// `nextTerminalIndex` set on a live Tab must appear in the
    /// snapshot so it reaches disk.
    func test_snapshot_preservesNextTerminalIndex() {
        let tabId = TabModel.mainTerminalTabId

        // Drive the counter up by adding panes.
        _ = appState.sessions.addPane(tabId: tabId, kind: .terminal)
        _ = appState.sessions.addPane(tabId: tabId, kind: .terminal)

        let liveTab = appState.tabs.tab(for: tabId)!
        let expectedCounter = liveTab.nextTerminalIndex

        let snap = appState.windowSession.snapshotPersistedWindow()
        guard let terminalsProject = snap.projects.first(where: {
            $0.id == TabModel.terminalsProjectId
        }), let persistedTab = terminalsProject.tabs.first(where: {
            $0.id == tabId
        }) else {
            XCTFail("Main terminal tab must be in the snapshot")
            return
        }

        XCTAssertEqual(persistedTab.nextTerminalIndex, expectedCounter,
                       "nextTerminalIndex must survive the snapshot path.")
    }

    /// A renamed pane title must reach the snapshot so the rename
    /// survives encode/decode/restore. Closes the loop on
    /// `renamePane` → `onTreeMutation` → `snapshotPersistedWindow` →
    /// `PersistedPane.title`.
    func test_snapshot_preservesRenamedPaneTitle() {
        let tabId = TabModel.mainTerminalTabId
        let paneId = appState.tabs.tab(for: tabId)!.panes[0].id

        appState.tabs.renamePane(tabId: tabId, paneId: paneId, to: "logs")

        let snap = appState.windowSession.snapshotPersistedWindow()
        guard let project = snap.projects.first(where: {
            $0.id == TabModel.terminalsProjectId
        }), let persistedTab = project.tabs.first(where: { $0.id == tabId }),
              let persistedPane = persistedTab.panes.first(where: { $0.id == paneId })
        else {
            XCTFail("Renamed pane must be in the snapshot")
            return
        }

        XCTAssertEqual(persistedPane.title, "logs",
                       "Renamed title must propagate through snapshotPersistedWindow.")
    }

    // MARK: - Tab.recoverNextTerminalIndex (pure helper)

    /// The pure regex/floor helper exposed on `Tab`. Tested directly
    /// so the grammar is locked down without standing up a
    /// WindowSession.
    func test_recoverNextTerminalIndex_takesMaxPlusOne() {
        XCTAssertEqual(
            Tab.recoverNextTerminalIndex(
                fromPaneTitles: ["Terminal 1", "Terminal 2", "logs"]
            ),
            3
        )
    }

    func test_recoverNextTerminalIndex_floorsAtOne() {
        XCTAssertEqual(
            Tab.recoverNextTerminalIndex(fromPaneTitles: ["logs", "zsh"]),
            1, "No parseable Terminal-N titles → floor at 1."
        )
        XCTAssertEqual(
            Tab.recoverNextTerminalIndex(fromPaneTitles: []),
            1, "Empty pane list → floor at 1."
        )
    }

    func test_recoverNextTerminalIndex_caseAndWhitespaceTolerant() {
        // The grammar is `^terminal\s+(\d+)$` case-insensitive.
        XCTAssertEqual(
            Tab.recoverNextTerminalIndex(fromPaneTitles: ["terminal 5"]),
            6, "Lowercase 'terminal' must parse."
        )
        XCTAssertEqual(
            Tab.recoverNextTerminalIndex(fromPaneTitles: ["Terminal   7"]),
            8, "Multiple spaces between 'Terminal' and the number must parse."
        )
        XCTAssertEqual(
            Tab.recoverNextTerminalIndex(fromPaneTitles: ["Terminal42"]),
            1, "No whitespace between 'Terminal' and digits must NOT parse."
        )
    }

    // MARK: - PersistedPane.titleManuallySet round-trip

    /// `Pane.titleManuallySet` must survive snapshot → encode → decode →
    /// `addRestoredTabModel`, otherwise the user's lock evaporates on
    /// app relaunch and OSC titles immediately clobber the renamed
    /// pane's pill. Mirrors `test_persistedTab_decodesNilWhenFieldAbsent`
    /// for `PersistedTab.titleManuallySet`.
    func test_renamePane_persistedFlag_survivesRoundTrip() throws {
        let tabId = TabModel.mainTerminalTabId
        let paneId = appState.tabs.tab(for: tabId)!.panes[0].id
        appState.tabs.renamePane(tabId: tabId, paneId: paneId, to: "build")
        XCTAssertTrue(
            appState.tabs.tab(for: tabId)!.panes[0].titleManuallySet,
            "Pre-condition: rename must set the flag."
        )

        // Snapshot → JSON → decode → re-hydrate via the public restore
        // path to exercise both the writer and reader sides at once.
        let snap = appState.windowSession.snapshotPersistedWindow()
        let data = try JSONEncoder().encode(snap)
        let decoded = try JSONDecoder().decode(PersistedWindow.self, from: data)

        guard let proj = decoded.projects.first(where: {
            $0.id == TabModel.terminalsProjectId
        }), let pTab = proj.tabs.first(where: { $0.id == tabId }),
              let pPane = pTab.panes.first(where: { $0.id == paneId })
        else {
            XCTFail("Renamed pane must be in the round-tripped snapshot")
            return
        }
        XCTAssertEqual(
            pPane.titleManuallySet, true,
            "PersistedPane.titleManuallySet must survive JSON encode/decode."
        )

        // Hydrate a fresh AppState from the decoded snapshot so we
        // verify the reader side too: `nil → false` and `true → true`.
        let fresh = AppState()
        // Drop the Main tab so we can restore into a known project.
        fresh.tabs.projects.removeAll()
        fresh.tabs.projects.append(Project(
            id: TabModel.terminalsProjectId,
            name: "Terminals", path: "/tmp", tabs: []
        ))
        _ = fresh.windowSession.addRestoredTabModel(pTab, toProjectIndex: 0)
        let hydratedPane = fresh.tabs.tab(for: tabId)?
            .panes.first(where: { $0.id == paneId })
        XCTAssertEqual(
            hydratedPane?.titleManuallySet, true,
            "addRestoredTabModel must hydrate the flag back onto Pane."
        )
    }

    /// Older session files written before `titleManuallySet` existed
    /// must decode to `nil` so callers' `?? false` hydrates them as
    /// "not manually set" without crashing on a missing key. Mirror
    /// of `test_persistedTab_decodesNilWhenFieldAbsent`.
    func test_persistedPane_decodesNilWhenFieldAbsent() throws {
        let json = """
        {
            "id": "p-old",
            "title": "Terminal 1",
            "kind": "terminal",
            "cwd": null
        }
        """
        let data = json.data(using: .utf8)!
        let decoded = try JSONDecoder().decode(PersistedPane.self, from: data)

        XCTAssertNil(decoded.titleManuallySet,
                     "titleManuallySet absent in older JSON must decode as nil.")
    }

    /// Round-trip the optional flag in both `true` and `nil` shapes
    /// to pin the encoder side. (`nil` is what `snapshotPersistedWindow`
    /// writes when the flag is false, to keep snapshot JSON small.)
    func test_persistedPane_roundTripsTitleManuallySet() throws {
        let pp = PersistedPane(
            id: "p-rt", title: "build", kind: .terminal,
            cwd: nil, titleManuallySet: true
        )
        let data = try JSONEncoder().encode(pp)
        let decoded = try JSONDecoder().decode(PersistedPane.self, from: data)
        XCTAssertEqual(decoded.titleManuallySet, true)

        let nilPp = PersistedPane(
            id: "p-rt2", title: "Terminal 1", kind: .terminal,
            cwd: nil, titleManuallySet: nil
        )
        let nilData = try JSONEncoder().encode(nilPp)
        let nilDecoded = try JSONDecoder().decode(PersistedPane.self, from: nilData)
        XCTAssertNil(nilDecoded.titleManuallySet)
    }

    // MARK: - Helpers

    private func makePersistedTabWithPanes(
        titles: [String],
        savedCounter: Int?
    ) -> PersistedTab {
        let tabId = "t-\(UUID().uuidString)"
        let panes = titles.enumerated().map { i, title in
            PersistedPane(id: "\(tabId)-p\(i)", title: title, kind: .terminal)
        }
        return PersistedTab(
            id: tabId,
            title: "Test Tab",
            cwd: "/tmp/pane-naming-test",
            branch: nil,
            claudeSessionId: nil,
            activePaneId: panes.first?.id,
            panes: panes,
            titleManuallySet: nil,
            nextTerminalIndex: savedCounter
        )
    }
}
