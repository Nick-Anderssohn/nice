//
//  AdoptTerminalPaneAsMainTerminalTests.swift
//  NiceUnitTests
//
//  Bug 1: a terminal torn off the pinned TERMINALS section must REPLACE
//  the receiving window's pristine auto-seeded Main terminal — exactly
//  one TERMINALS section, with the torn-off pane as the Main tab's single
//  pane. These tests drive the model/session path the new-window seed
//  consumer (`AppShellHost.task`) routes to:
//  `SessionsModel.adoptTerminalPaneAsMainTerminal`.
//
//  Two windows are registered in a shared `NiceServices.registry`. winA
//  hosts a terminal in its TERMINALS section; we tear it off via
//  `PaneTearOffController`, then feed the enqueued seed into winB exactly
//  as the seed consumer would — but routed to the Main-terminal adopt for
//  the terminals-project case.
//

import AppKit
import XCTest
@testable import Nice

@MainActor
final class AdoptTerminalPaneAsMainTerminalTests: XCTestCase {

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

    /// Mimic `AppState.start()` for the Terminals project's Main tab:
    /// give it a live pty session so the pristine pane behaves like a
    /// real (deferred-armed) seed.
    private func seedMainTerminalSession(in app: AppState) {
        let mainTab = app.tabs.projects[0].tabs[0]
        _ = app.sessions.makeSession(
            for: mainTab.id, cwd: mainTab.cwd,
            initialTerminalPaneId: mainTab.activePaneId
        )
    }

    /// Publish a live-pane handle so `PaneTearOffController` can claim it.
    private func publishDrag(from source: AppState, tabId: String, paneId: String) {
        services.livePaneRegistry.publish(.init(
            paneId: paneId,
            sourceWindowSessionId: source.windowSession.windowSessionId,
            sourceTabId: tabId,
            claim: { [weak source] in
                source?.sessions.detachLivePane(tabId: tabId, paneId: paneId)
            }
        ))
    }

    // MARK: - Tests

    func test_terminalsSectionTearOff_replacesMainTerminal_singleSection() {
        // winA: add a SECOND terminal pane to the Terminals Main tab so
        // tearing one off leaves the source Main tab alive.
        seedMainTerminalSession(in: winA)
        let mainTabId = TabModel.mainTerminalTabId
        let tornId = "terminals-main-torn"
        winA.tabs.mutateTab(id: mainTabId) { tab in
            tab.panes.append(Pane(id: tornId, title: "Terminal 2", kind: .terminal))
            tab.activePaneId = tornId
        }
        // The Terminals Main tab is the active tab when tearing off.
        winA.tabs.activeTabId = mainTabId
        // Arm a live entry for the torn pane on the EXISTING Main session
        // (makeSession would no-op since the session already exists).
        _ = winA.sessions.ptySessions[mainTabId]?.addTerminalPane(id: tornId, cwd: "/tmp")
        XCTAssertEqual(winA.sessions.ptySessions[mainTabId]?.hasPane(tornId), true,
                       "Precondition: torn pane has a live entry to claim")
        publishDrag(from: winA, tabId: mainTabId, paneId: tornId)

        // winB is the receiving window with its pristine seeded Main.
        seedMainTerminalSession(in: winB)
        let winBSeededPaneId = winB.tabs.projects[0].tabs[0].activePaneId

        // Tear off — enqueues a seed carrying the Terminals project id.
        PaneTearOffController(services: services).tearOff(
            paneId: tornId,
            sourceWindowSessionId: "win-A",
            at: NSPoint(x: 100, y: 100),
            openWindow: {}
        )
        guard let seed = services.consumeTearOffSeed() else {
            return XCTFail("Expected a tear-off seed")
        }
        XCTAssertEqual(seed.projectId, TabModel.terminalsProjectId,
                       "A terminals-section terminal carries the terminals project id")

        // Consume into winB exactly as the seed consumer routes the
        // terminals-project case.
        winB.sessions.adoptTerminalPaneAsMainTerminal(
            entry: seed.entry, paneId: seed.paneId, title: seed.title
        )

        // Exactly one project carries the terminals id (no duplicate
        // TERMINALS section).
        let terminalsProjects = winB.tabs.projects.filter {
            $0.id == TabModel.terminalsProjectId
        }
        XCTAssertEqual(terminalsProjects.count, 1,
                       "Receiving window must have exactly one TERMINALS section")

        // The Main tab now hosts the torn-off pane as its single pane and
        // keeps its id/title.
        let mainTab = winB.tabs.tab(for: mainTabId)
        XCTAssertNotNil(mainTab)
        XCTAssertEqual(mainTab?.title, "Main")
        XCTAssertEqual(mainTab?.panes.map(\.id), [tornId],
                       "Main tab hosts only the torn-off pane")
        XCTAssertEqual(mainTab?.activePaneId, tornId)
        XCTAssertEqual(winB.tabs.activeTabId, mainTabId)

        // The live entry was adopted into the Main tab's session; the
        // pristine seeded pane's pty was retired.
        XCTAssertEqual(winB.sessions.ptySessions[mainTabId]?.hasPane(tornId), true,
                       "Torn-off live entry adopted into Main tab session")
        if let seededId = winBSeededPaneId {
            XCTAssertEqual(
                winB.sessions.ptySessions[mainTabId]?.hasPane(seededId), false,
                "Pristine seeded pane's pty must be retired"
            )
        }
    }
}
