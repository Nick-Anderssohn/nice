//
//  LivePaneMigrationTests.swift
//  NiceUnitTests
//
//  Bookkeeping tests for moving a LIVE terminal pane between two
//  windows (each modelled by its own AppState). Asserts the pty entry
//  leaves the source session, appears in the target session under the
//  right key, its termination delegate is re-pointed at the target tab,
//  and the model-level extract/insert re-focus neighbors correctly. The
//  "real pty kept running" claim is owned by a UITest with a real
//  window; here every pane is armed-but-unspawned (no window → no
//  fork), so this stays deterministic and headless.
//

import AppKit
import XCTest
@testable import Nice

@MainActor
final class LivePaneMigrationTests: XCTestCase {

    /// Seed a project with a single multi-terminal tab into `appState`
    /// and spawn (arm) its pty session with `firstPaneId` live.
    private func seedTerminalTab(
        into appState: AppState,
        projectId: String,
        tabId: String,
        paneIds: [String],
        activePaneId: String
    ) {
        let panes = paneIds.enumerated().map {
            Pane(id: $0.element, title: "Terminal \($0.offset + 1)", kind: .terminal)
        }
        let tab = Tab(
            id: tabId, title: "T", cwd: "/tmp/\(projectId)",
            panes: panes, activePaneId: activePaneId
        )
        appState.tabs.projects = [
            appState.tabs.projects[0],
            Project(id: projectId, name: projectId.uppercased(),
                    path: "/tmp/\(projectId)", tabs: [tab]),
        ]
        // Arm a real TabPtySession hosting the active pane.
        _ = appState.sessions.makeSession(
            for: tabId, cwd: "/tmp/\(projectId)",
            initialTerminalPaneId: activePaneId
        )
    }

    func test_moveTerminalPane_detachAdopt_movesLiveEntryAndModel() {
        let src = AppState()
        let dst = AppState()

        seedTerminalTab(into: src, projectId: "src", tabId: "src-tab",
                        paneIds: ["pA", "pB"], activePaneId: "pA")
        seedTerminalTab(into: dst, projectId: "dst", tabId: "dst-tab",
                        paneIds: ["pX"], activePaneId: "pX")

        let srcSession = src.sessions.ptySessions["src-tab"]
        let dstSession = dst.sessions.ptySessions["dst-tab"]
        XCTAssertEqual(srcSession?.hasPane("pA"), true)

        // 1. Detach the live entry from the source pty session.
        let entry = src.sessions.detachLivePane(tabId: "src-tab", paneId: "pA")
        XCTAssertNotNil(entry)
        XCTAssertEqual(srcSession?.hasPane("pA"), false,
                       "Detached pane must leave the source session.")

        // 2. Remove the pane model from the source tab (re-focus neighbor).
        let removed = src.tabs.extractPane("pA", fromTab: "src-tab")
        XCTAssertEqual(removed?.id, "pA")
        XCTAssertEqual(src.tabs.tab(for: "src-tab")?.panes.map(\.id), ["pB"])
        XCTAssertEqual(src.tabs.tab(for: "src-tab")?.activePaneId, "pB",
                       "Removing the active pane re-focuses a neighbor.")

        // 3. Adopt the live entry into the destination pty session.
        dst.sessions.adoptLivePane(tabId: "dst-tab", paneId: "pA", entry: entry!)
        XCTAssertEqual(dstSession?.hasPane("pA"), true,
                       "Adopted pane must appear in the target session.")

        // 4. Insert the pane model into the destination tab.
        dst.tabs.insertPane(removed!, inTab: "dst-tab", relativeTo: "pX", placeAfter: true)
        XCTAssertEqual(dst.tabs.tab(for: "dst-tab")?.panes.map(\.id), ["pX", "pA"])

        // The migrated entry's delegate now routes to the target tab.
        XCTAssertEqual(
            dstSession?.entries["pA"]?.delegate.routedPane?.tabId, "dst-tab",
            "adoptPane must re-point the delegate at the destination tab."
        )
        XCTAssertEqual(dstSession?.entries["pA"]?.delegate.routedPane?.paneId, "pA")
    }

    func test_detachLivePane_unknownTab_returnsNil() {
        let appState = AppState()
        XCTAssertNil(appState.sessions.detachLivePane(tabId: "ghost", paneId: "p"))
    }

    func test_adoptLivePane_createsSessionWhenAbsent() {
        let src = AppState()
        let dst = AppState()
        seedTerminalTab(into: src, projectId: "src", tabId: "src-tab",
                        paneIds: ["pA"], activePaneId: "pA")
        // Destination tab exists in the model but has NO pty session yet.
        let dstTab = Tab(id: "dst-tab", title: "D", cwd: "/tmp/dst",
                         panes: [Pane(id: "pX", title: "Terminal 1", kind: .terminal)],
                         activePaneId: "pX")
        dst.tabs.projects = [dst.tabs.projects[0],
                             Project(id: "dst", name: "DST", path: "/tmp/dst", tabs: [dstTab])]
        XCTAssertNil(dst.sessions.ptySessions["dst-tab"])

        let entry = src.sessions.detachLivePane(tabId: "src-tab", paneId: "pA")!
        dst.sessions.adoptLivePane(tabId: "dst-tab", paneId: "pA", entry: entry)

        XCTAssertNotNil(dst.sessions.ptySessions["dst-tab"],
                        "adoptLivePane creates a session for a tab that lacked one.")
        XCTAssertEqual(dst.sessions.ptySessions["dst-tab"]?.hasPane("pA"), true)
    }
}
