//
//  PaneClaimTransferTests.swift
//  NiceUnitTests
//
//  Phase A coverage for the `PaneClaim` tri-state that structurally
//  fixes BUG A — tearing off / migrating a pane whose pty spawn was
//  still deferred (restored at startup, never focused) used to silently
//  no-op because the old claim API folded "not spawned yet" and "already
//  dead" into a single swallowed nil.
//
//  Three groups:
//    • `SessionsModel.claimPaneForTransfer` resolves to `.live` /
//      `.notSpawned(cwd:)` / `.gone` correctly.
//    • Tearing off an UNSPAWNED terminal pane seeds a nil entry + a cwd
//      and the destination adopt SPAWNS it.
//    • Migrating an UNSPAWNED terminal pane into a SESSION-LESS target
//      tab is NOT a silent no-op (skeptic ISSUE 1/3) — a session is
//      created and the pane spawned.
//    • A `.notSpawned` Claude tear-off / migration lands a
//      `[claude, companion]` tab in resume-deferred mode (graft 5).
//

import AppKit
import XCTest
@testable import Nice

@MainActor
final class PaneClaimTransferTests: XCTestCase {

    private var services: NiceServices!
    private var winA: AppState!
    private var winB: AppState!
    private var windowA: NSWindow!
    private var windowB: NSWindow!

    override func setUp() {
        super.setUp()
        services = NiceServices()
        winA = AppState(services: services, initialSidebarCollapsed: false,
                        initialMainCwd: nil, windowSessionId: "win-A",
                        store: FakeSessionStore())
        winB = AppState(services: services, initialSidebarCollapsed: false,
                        initialMainCwd: nil, windowSessionId: "win-B",
                        store: FakeSessionStore())
        windowA = NSWindow()
        windowB = NSWindow()
        services.registry.register(appState: winA, window: windowA)
        services.registry.register(appState: winB, window: windowB)
    }

    override func tearDown() {
        windowA = nil; windowB = nil
        winA = nil; winB = nil; services = nil
        super.tearDown()
    }

    // MARK: - Helpers

    /// A throwaway live entry (the pty never spawns — the view is never
    /// laid out in a window — so this is cheap and deterministic).
    private func makePaneEntry(paneId: String) -> TabPtySession.PaneEntry {
        let view = NiceTerminalView(frame: .zero)
        let delegate = ProcessTerminationDelegate(
            role: .pane(tabId: "t", paneId: paneId),
            onExit: { _, _ in }
        )
        return TabPtySession.PaneEntry(view: view, kind: .terminal, delegate: delegate)
    }

    /// Seed a terminal tab with a single pane into `app`, WITHOUT
    /// spawning any pty session (so the pane is modelled-but-deferred).
    /// `/tmp` exists on disk so `resolvedSpawnCwd` returns it verbatim.
    private func seedDeferredTerminalTab(
        into app: AppState, projectId: String, tabId: String,
        paneId: String, cwd: String
    ) {
        let tab = Tab(id: tabId, title: "T", cwd: cwd,
                      panes: [Pane(id: paneId, title: "Terminal 1", kind: .terminal)],
                      activePaneId: paneId)
        app.tabs.projects = [
            app.tabs.projects[0],
            Project(id: projectId, name: projectId.uppercased(), path: cwd, tabs: [tab])
        ]
    }

    /// Publish a handle whose claim closure routes through the real
    /// `claimPaneForTransfer` (matching production).
    private func publishDrag(from source: AppState, tabId: String, paneId: String) {
        services.livePaneRegistry.publish(.init(
            paneId: paneId,
            sourceWindowSessionId: source.windowSession.windowSessionId,
            sourceTabId: tabId,
            claim: { [weak source] in
                source?.sessions.claimPaneForTransfer(tabId: tabId, paneId: paneId) ?? .gone
            }
        ))
    }

    /// Run a tear-off and return the pairing token the controller minted
    /// (captured from the deferred `openWindow` closure). Pumps the main
    /// runloop so the `DispatchQueue.main.async` open fires; consume the
    /// seed by the returned token.
    @discardableResult
    private func tearOffCapturingToken(
        paneId: String,
        from sourceSessionId: String,
        at point: NSPoint = NSPoint(x: 100, y: 100)
    ) -> String? {
        var capturedToken: String?
        let opened = expectation(description: "openWindow called")
        PaneTearOffController(services: services).tearOff(
            paneId: paneId, sourceWindowSessionId: sourceSessionId,
            at: point,
            openWindow: { token in capturedToken = token; opened.fulfill() }
        )
        wait(for: [opened], timeout: 1.0)
        return capturedToken
    }

    // MARK: - claimPaneForTransfer tri-state

    func test_claimPaneForTransfer_live_forSpawnedPane() {
        // A genuinely spawned pane: stand up a real headless session and
        // adopt a fabricated PaneEntry so `hasPane` is true (NOT the
        // syntheticSpawnedPanes seam — that feeds `paneIsSpawned`, not
        // `hasPane`, which is what `claimPaneForTransfer` checks).
        seedDeferredTerminalTab(into: winA, projectId: "a", tabId: "a-tab",
                                paneId: "pA", cwd: "/tmp")
        // Stand up a real headless session with a throwaway initial pane
        // (so makeSession doesn't infer+spawn `pA` itself), then adopt a
        // fabricated entry under `pA` so it is genuinely hosted —
        // `hasPane("pA")` is true via a real entry, NOT the
        // syntheticSpawnedPanes seam.
        _ = winA.sessions.makeSession(for: "a-tab", cwd: "/tmp",
                                      initialTerminalPaneId: "seed-throwaway")
        winA.sessions.adoptLivePane(tabId: "a-tab", paneId: "pA",
                                    entry: makePaneEntry(paneId: "pA"))
        XCTAssertEqual(winA.sessions.ptySessions["a-tab"]?.hasPane("pA"), true,
                       "Precondition: pane has a live entry.")

        let claim = winA.sessions.claimPaneForTransfer(tabId: "a-tab", paneId: "pA")
        guard case .live = claim else {
            return XCTFail("Spawned pane must claim as .live, got \(claim)")
        }
        // The detach is one-shot: the entry left the source session.
        XCTAssertEqual(winA.sessions.ptySessions["a-tab"]?.hasPane("pA"), false,
                       ".live detaches the entry from the source session.")
    }

    func test_claimPaneForTransfer_notSpawned_forModelledButEntrylessPane() {
        seedDeferredTerminalTab(into: winA, projectId: "a", tabId: "a-tab",
                                paneId: "pA", cwd: "/tmp")
        // No session at all — pane is purely modelled.
        XCTAssertNil(winA.sessions.ptySessions["a-tab"])

        let claim = winA.sessions.claimPaneForTransfer(tabId: "a-tab", paneId: "pA")
        guard case .notSpawned(let cwd) = claim else {
            return XCTFail("Deferred pane must claim as .notSpawned, got \(claim)")
        }
        // cwd matches the source model's resolvedSpawnCwd.
        let tab = winA.tabs.tab(for: "a-tab")!
        let pane = tab.panes.first { $0.id == "pA" }!
        XCTAssertEqual(cwd, winA.tabs.resolvedSpawnCwd(for: tab, pane: pane))
        XCTAssertEqual(cwd, "/tmp", "Resolves to the on-disk tab cwd.")
    }

    func test_claimPaneForTransfer_gone_forAbsentPane() {
        let claim = winA.sessions.claimPaneForTransfer(tabId: "ghost-tab", paneId: "nope")
        guard case .gone = claim else {
            return XCTFail("Absent pane id must claim as .gone, got \(claim)")
        }
    }

    // MARK: - Tear-off of an UNSPAWNED terminal pane

    func test_tearOff_unspawnedTerminal_seedsNilEntryAndCwd_destinationSpawns() {
        // winA: a deferred terminal pane in a project section (NOT the
        // terminals section, so it lands via adoptTerminalPaneAsNewTab).
        seedDeferredTerminalTab(into: winA, projectId: "a", tabId: "a-tab",
                                paneId: "pA", cwd: "/tmp")
        winA.tabs.activeTabId = "a-tab"
        XCTAssertNil(winA.sessions.ptySessions["a-tab"],
                     "Precondition: source pane is unspawned (no session).")
        publishDrag(from: winA, tabId: "a-tab", paneId: "pA")

        guard let token = tearOffCapturingToken(paneId: "pA", from: "win-A") else {
            return XCTFail("Expected openWindow to fire with a pairing token.")
        }

        guard let seed = services.consumeTearOffSeed(token: token) else {
            return XCTFail("Expected a tear-off seed.")
        }
        XCTAssertNil(seed.entry, "An unspawned pane seeds a nil entry.")
        XCTAssertEqual(seed.cwd, "/tmp", "Seed carries the resolved spawn cwd.")
        XCTAssertEqual(seed.kind, .terminal)

        // Drive the destination adopt exactly as the seed consumer would
        // for a project-section terminal (nil entry → fresh spawn).
        let newTabId = winB.sessions.adoptTerminalPaneAsNewTab(
            entry: seed.entry, paneId: seed.paneId, title: seed.title,
            projectId: seed.projectId, projectName: seed.projectName,
            projectPath: seed.projectPath, spawnCwd: seed.cwd
        )
        XCTAssertNotNil(newTabId)
        XCTAssertEqual(winB.sessions.ptySessions[newTabId!]?.hasPane("pA"), true,
                       "Destination must SPAWN the unspawned pane (BUG A fix).")
    }

    func test_tearOff_unspawnedTerminalsSectionPane_mainTerminalSpawnsInCwd() {
        // A deferred pane added to the TERMINALS section tears off and
        // REPLACES the destination's pristine Main terminal — and must
        // spawn fresh in the carried cwd.
        let mainTabId = TabModel.mainTerminalTabId
        let deferredId = "terminals-deferred"
        // winA Main tab already exists (seeded in start()). Add a second,
        // deferred pane and make it active; no session for it.
        winA.tabs.mutateTab(id: mainTabId) { tab in
            tab.panes.append(Pane(id: deferredId, title: "Terminal 2",
                                  kind: .terminal, cwd: "/tmp"))
            tab.activePaneId = deferredId
        }
        winA.tabs.activeTabId = mainTabId
        XCTAssertEqual(winA.sessions.ptySessions[mainTabId]?.hasPane(deferredId), nil)
        publishDrag(from: winA, tabId: mainTabId, paneId: deferredId)

        guard let token = tearOffCapturingToken(
            paneId: deferredId, from: "win-A"
        ) else {
            return XCTFail("Expected openWindow to fire with a pairing token.")
        }
        guard let seed = services.consumeTearOffSeed(token: token) else {
            return XCTFail("Expected a tear-off seed.")
        }
        XCTAssertNil(seed.entry)
        XCTAssertEqual(seed.projectId, TabModel.terminalsProjectId)
        XCTAssertEqual(seed.cwd, "/tmp")

        winB.sessions.adoptTerminalPaneAsMainTerminal(
            entry: seed.entry, paneId: seed.paneId, title: seed.title,
            spawnCwd: seed.cwd
        )
        XCTAssertEqual(winB.sessions.ptySessions[mainTabId]?.hasPane(deferredId), true,
                       "Main terminal must spawn the unspawned torn-off pane.")
    }

    // MARK: - Migration into a SESSION-LESS target tab (skeptic ISSUE 1/3)

    func test_migration_unspawnedTerminal_intoSessionlessTarget_isNotNoOp() {
        // Source: a deferred terminal pane (no session).
        seedDeferredTerminalTab(into: winA, projectId: "a", tabId: "a-tab",
                                paneId: "pA", cwd: "/tmp")
        // Target: a terminal tab that EXISTS in the model but has NO pty
        // session yet (the session-less target the skeptic flagged).
        let targetTab = Tab(id: "b-tab", title: "B", cwd: "/tmp",
                            panes: [Pane(id: "pX", title: "Terminal 1", kind: .terminal)],
                            activePaneId: "pX")
        winB.tabs.projects = [winB.tabs.projects[0],
                              Project(id: "b", name: "B", path: "/tmp", tabs: [targetTab])]
        XCTAssertNil(winB.sessions.ptySessions["b-tab"],
                     "Precondition: target tab has no pty session.")
        publishDrag(from: winA, tabId: "a-tab", paneId: "pA")

        let moved = PaneMigrationCoordinator(services: services).commitCrossWindowMove(
            into: winB, targetTabId: "b-tab", relativeToPaneId: "pX", placeAfter: true
        )
        XCTAssertTrue(moved, "Migration of a deferred pane must not be a no-op.")

        // The pane model landed in the target strip.
        XCTAssertEqual(winB.tabs.tab(for: "b-tab")?.panes.map(\.id), ["pX", "pA"])
        // A session was CREATED for the previously session-less target,
        // and the pane was SPAWNED in it (the ISSUE 1/3 fix).
        XCTAssertNotNil(winB.sessions.ptySessions["b-tab"],
                        "A session must be created for the session-less target.")
        XCTAssertEqual(winB.sessions.ptySessions["b-tab"]?.hasPane("pA"), true,
                       "The migrated unspawned pane must be spawned in the target.")
        // Gone from the source.
        XCTAssertNil(winA.tabs.tab(for: "a-tab")?.panes.first { $0.id == "pA" })
    }

    // MARK: - Claude .notSpawned -> resume-deferred new tab (graft 5)

    func test_adoptClaudePane_nilEntry_landsResumeDeferredTab() {
        // Directly exercise the nil-entry Claude adopt (the .notSpawned
        // Claude tear-off / migration branch). Assert the new tab has the
        // [claude, companion] shape, claudeSessionId carried, and the
        // Claude pane was instantiated in RESUME-DEFERRED mode (an armed
        // plain-shell pendingSpawn carrying `claude --resume <id>` in its
        // env, NOT a live-adopted process and NOT a fresh `exec claude`).
        let newTabId = winB.sessions.adoptClaudePaneAsNewTab(
            entry: nil, paneId: "cA", title: "Repo",
            claudeSessionId: "sess-77",
            projectId: "p-repo", projectName: "REPO", projectPath: "/tmp"
        )
        XCTAssertNotNil(newTabId)
        let tab = winB.tabs.tab(for: newTabId!)
        XCTAssertEqual(tab?.claudeSessionId, "sess-77", "Session id carried across.")
        XCTAssertEqual(tab?.panes.count, 2, "Canonical [claude, companion] shape.")
        XCTAssertEqual(tab?.panes.first?.kind, .claude)
        XCTAssertEqual(tab?.panes.first?.id, "cA")
        XCTAssertEqual(tab?.panes.last?.kind, .terminal)
        XCTAssertEqual(tab?.activePaneId, "cA")
        assertResumeDeferred(tabId: newTabId!, claudePaneId: "cA", sessionId: "sess-77")
    }

    /// Pin that `claudePaneId` on `tabId` was instantiated in
    /// `.resumeDeferred(id:)` mode: a Claude-kind entry exists (the tab
    /// hosts it) but it is ARMED-DEFERRED — `pendingSpawn` is a plain
    /// `/bin/zsh -il` (no `exec claude ...`) whose env pre-types
    /// `claude --resume <sessionId>`. This is the same shape restore
    /// uses, and is the observable proof that the deferred Claude pane
    /// will resume the right session on first focus rather than having
    /// been dropped.
    private func assertResumeDeferred(
        tabId: String, claudePaneId: String, sessionId: String
    ) {
        let session = winB.sessions.ptySessions[tabId]
        XCTAssertEqual(session?.hasPane(claudePaneId), true,
                       "The deferred Claude pane must be hosted (not dropped).")
        guard let view = session?.view(forPane: claudePaneId) else {
            return XCTFail("Expected a hosted view for the Claude pane.")
        }
        guard let pending = view.pendingSpawn else {
            return XCTFail("Resume-deferred pane must be ARMED (pendingSpawn set), not fired.")
        }
        XCTAssertEqual(pending.args, ["-il"],
                       "Resume-deferred spawns a plain login shell, not `exec claude`.")
        XCTAssertTrue(
            pending.environment?.contains("NICE_PREFILL_COMMAND=claude --resume \(sessionId)") ?? false,
            "Env must pre-type `claude --resume <id>` (resume-deferred contract)."
        )
    }

    func test_migration_unspawnedClaude_landsResumeDeferredTab() {
        // End-to-end: a deferred Claude pane migrated into winB lands as a
        // resume-deferred new tab rather than being dropped after extract.
        var claudePane = Pane(id: "cA", title: "Claude", kind: .claude)
        claudePane.isClaudeRunning = false
        // Source project lives at a DISTINCT path from winB's target
        // project so `ensureProjectByPath` (matches by path) recreates the
        // source identity in winB rather than reusing winB's existing
        // /tmp project — mirrors CrossWindowMoveTests'
        // test_claudePane_becomesNewTabUnderMatchingProject.
        let srcTab = Tab(id: "a-claude", title: "Repo", cwd: "/tmp/repo",
                         panes: [claudePane,
                                 Pane(id: "cA-t1", title: "Terminal 1", kind: .terminal)],
                         activePaneId: "cA", claudeSessionId: "sess-88")
        winA.tabs.projects = [winA.tabs.projects[0],
                              Project(id: "p-repo", name: "REPO", path: "/tmp/repo", tabs: [srcTab])]
        // No session for the Claude tab — the pane is unspawned.
        XCTAssertNil(winA.sessions.ptySessions["a-claude"])

        let targetTab = Tab(id: "b-tab", title: "B", cwd: "/tmp",
                            panes: [Pane(id: "pX", title: "Terminal 1", kind: .terminal)],
                            activePaneId: "pX")
        winB.tabs.projects = [winB.tabs.projects[0],
                              Project(id: "b", name: "B", path: "/tmp", tabs: [targetTab])]
        publishDrag(from: winA, tabId: "a-claude", paneId: "cA")

        let moved = PaneMigrationCoordinator(services: services).commitCrossWindowMove(
            into: winB, targetTabId: "b-tab", relativeToPaneId: "pX", placeAfter: true
        )
        XCTAssertTrue(moved)
        // Claude landed as its own tab (not the terminal strip).
        XCTAssertEqual(winB.tabs.tab(for: "b-tab")?.panes.map(\.id), ["pX"],
                       "Claude must not join the terminal strip.")
        let proj = winB.tabs.projects.first { $0.path == "/tmp/repo" && $0.id == "p-repo" }
        let newTab = proj?.tabs.first
        XCTAssertNotNil(newTab)
        XCTAssertEqual(newTab?.claudeSessionId, "sess-88")
        XCTAssertEqual(newTab?.panes.first?.kind, .claude)
        XCTAssertEqual(newTab?.panes.count, 2)
        // Instantiated resume-deferred (will resume on first focus), not
        // dropped after extract.
        assertResumeDeferred(tabId: newTab!.id, claudePaneId: "cA", sessionId: "sess-88")
        // Gone from the source.
        XCTAssertFalse(winA.tabs.tab(for: "a-claude")?.panes.contains { $0.id == "cA" } ?? true)
    }
}
