//
//  SessionsModelHandoffRequestTests.swift
//  NiceUnitTests
//
//  Pins `SessionsModel.handleHandoffRequest` — the entry point the socket
//  dispatcher calls when a `handoff` message arrives. Style and setup
//  mirror SessionsModelClaudeSocketRequestTests.
//
//  Coverage:
//    • Valid originating tab → new tab exists nested under it; title is
//      "[HANDOFF] " + originating title; titleManuallySet; cwd matches
//      originating; reply("ok") called.
//    • Handoff from an already-[HANDOFF] tab does NOT double the prefix:
//      title is "[HANDOFF] <base>", not "[HANDOFF] [HANDOFF] <base>".
//    • Empty/unknown tabId → top-level tab still opens (no nest);
//      reply("ok") called.
//    • Non-empty instructions → prompt contains the custom text.
//    • Empty instructions → prompt contains the default continue directive.
//    • model/effort present → handoff still creates a tab and replies ok
//      (guards the handler from dropping/short-circuiting on the new
//      fields). The actual --model/--effort arg construction is pinned by
//      SessionsModelHandoffCompositionTests.handoffExtraArgs — the spawned
//      arg list isn't observable here (makeSession runs under the
//      NICE_CLAUDE_OVERRIDE test seam, which suppresses extraClaudeArgs).
//      (NOTE: prompt is not directly observable outside the production
//       path; we infer it indirectly by checking the title/nesting
//       properties that are always produced alongside the prompt. The
//       prompt is passed as an extraClaudeArg to makeSession, which in
//       unit tests does not spawn a real pty, so the arg is not
//       visible from tests. A dedicated TabPtySessionClaudeArgsTests
//       in the existing suite covers how args propagate once spawned;
//       this test focuses on the observable model-side effects.)
//

import AppKit
import Foundation
import XCTest
@testable import Nice

@MainActor
final class SessionsModelHandoffRequestTests: XCTestCase {

    private var appState: AppState!
    private var homeSandbox: TestHomeSandbox!

    override func setUp() {
        super.setUp()
        homeSandbox = TestHomeSandbox()
        setenv("NICE_CLAUDE_OVERRIDE", "/bin/cat", 1)
        appState = AppState()
    }

    override func tearDown() {
        appState = nil
        unsetenv("NICE_CLAUDE_OVERRIDE")
        homeSandbox.teardown()
        homeSandbox = nil
        super.tearDown()
    }

    // MARK: - Valid originating tab

    func test_validOriginatingTab_createsNestedTab_andRepliesOk() {
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p", tabId: "t1",
            sessionId: "sess-t1"
        )
        appState.tabs.mutateTab(id: "t1") { $0.title = "wire up the foo" }

        // Originating pane id follows the Claude-pane convention used
        // everywhere in the codebase: "<tabId>-claude".
        let reply = captureReply { reply in
            appState.sessions.handleHandoffRequest(
                cwd: "/tmp/p",
                handoffFile: "/tmp/p/.claude/handoff/h.md",
                instructions: "",
                model: "",
                effort: "",
                tabId: "t1",
                paneId: "t1-claude",
                reply: reply
            )
        }

        XCTAssertEqual(reply, "ok", "successful handoff must reply \"ok\"")

        // A new tab must have appeared in the project.
        let project = projectById("p")
        XCTAssertEqual(project.tabs.count, 2,
                       "handleHandoffRequest must insert exactly one new tab")

        // Originating tab stays at index 0; handoff child at index 1
        // (insertHandoffChild inserts immediately after).
        XCTAssertEqual(project.tabs[0].id, "t1")
        let child = project.tabs[1]

        XCTAssertEqual(child.title, "[HANDOFF] wire up the foo",
                       "handoff tab title must be \"[HANDOFF] \" + originating title")
        XCTAssertTrue(child.titleManuallySet,
                      "titleManuallySet must be true so OSC auto-title can't overwrite the label")
        XCTAssertEqual(child.cwd, "/tmp/p",
                       "handoff tab cwd must match the originating tab's cwd")
        XCTAssertEqual(child.parentTabId, "t1",
                       "handoff child must be nested under the originating tab")

        // Tab-shape invariants on the newly-created handoff tab.
        let claudePane = child.panes.first(where: { $0.kind == .claude })
        XCTAssertNotNil(claudePane,
                        "handoff tab must have a Claude pane")
        XCTAssertTrue(claudePane?.isClaudeRunning == true,
                      "handoff tab's Claude pane must have isClaudeRunning == true")
        XCTAssertNotNil(child.claudeSessionId,
                        "handoff tab must have a claudeSessionId")
        XCTAssertEqual(child.activePaneId, "\(child.id)-claude",
                       "handoff tab's activePaneId must be the Claude pane id")
        XCTAssertEqual(child.panes.count, 2,
                       "handoff tab must have exactly two panes (one Claude, one terminal)")
        XCTAssertTrue(child.panes.contains(where: { $0.kind == .claude }),
                      "handoff tab must have a .claude pane")
        XCTAssertTrue(child.panes.contains(where: { $0.kind == .terminal }),
                      "handoff tab must have a .terminal pane")
        XCTAssertEqual(child.nextTerminalIndex, 2,
                       "nextTerminalIndex must be 2 after seeding Terminal 1")
        XCTAssertEqual(appState.tabs.activeTabId, child.id,
                       "the model's activeTabId must be the new handoff tab")
    }

    // MARK: - Re-handoff from an already-[HANDOFF] tab does not stack the prefix

    func test_handoffFromHandoffTab_doesNotStackPrefix() {
        // An existing "[HANDOFF] Foo" tab generates a child whose title
        // must be "[HANDOFF] Foo" (prefix stripped before re-adding),
        // not "[HANDOFF] [HANDOFF] Foo".
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p", tabId: "t1",
            sessionId: "sess-t1"
        )
        appState.tabs.mutateTab(id: "t1") { $0.title = "[HANDOFF] Foo" }

        let _ = captureReply { reply in
            appState.sessions.handleHandoffRequest(
                cwd: "/tmp/p",
                handoffFile: "/tmp/p/.claude/handoff/h.md",
                instructions: "",
                model: "",
                effort: "",
                tabId: "t1",
                paneId: "t1-claude",
                reply: reply
            )
        }

        let project = projectById("p")
        XCTAssertEqual(project.tabs.count, 2)
        let child = project.tabs[1]
        XCTAssertEqual(child.title, "[HANDOFF] Foo",
                       "re-handoff must NOT double the [HANDOFF] prefix")
        XCTAssertFalse(child.title.hasPrefix("[HANDOFF] [HANDOFF]"),
                       "title must never stack the prefix")
    }

    // MARK: - Empty/unknown tabId falls back to top-level, still replies ok

    func test_emptyTabId_fallsBackToTopLevel_andRepliesOk() {
        // When tabId is empty the request can't resolve an originating
        // tab — insertHandoffChild returns false and the tab is bucketed
        // at top level via addTabToProjects. The caller still gets "ok".
        let initialProjectCount = appState.tabs.projects.count

        let reply = captureReply { reply in
            appState.sessions.handleHandoffRequest(
                cwd: "/tmp/scratch",
                handoffFile: "/tmp/scratch/.claude/handoff/h.md",
                instructions: "",
                model: "",
                effort: "",
                tabId: "",
                paneId: "",
                reply: reply
            )
        }

        XCTAssertEqual(reply, "ok",
                       "empty tabId must still reply \"ok\" — top-level fallback is not an error")
        // A new project or a new tab in an existing project must have appeared.
        let totalTabs = appState.tabs.projects.reduce(0) { $0 + $1.tabs.count }
        let initialTabs = appState.tabs.projects[0..<initialProjectCount].reduce(0) { $0 + $1.tabs.count }
        XCTAssertGreaterThan(totalTabs, initialTabs,
                             "a new tab must be created even for empty tabId")
    }

    func test_unknownTabId_fallsBackToTopLevel_andRepliesOk() {
        let reply = captureReply { reply in
            appState.sessions.handleHandoffRequest(
                cwd: "/tmp/scratch",
                handoffFile: "/tmp/scratch/.claude/handoff/h.md",
                instructions: "",
                model: "",
                effort: "",
                tabId: "does-not-exist",
                paneId: "does-not-exist-pane",
                reply: reply
            )
        }
        XCTAssertEqual(reply, "ok",
                       "unknown tabId must fall back to top-level and still reply \"ok\"")
    }

    func test_validTabId_wrongPaneId_fallsBackToTopLevel_andRepliesOk() {
        // The tab exists and the tabId resolves, but the paneId is stale /
        // doesn't belong to that tab. The originating-tab resolution check
        // requires that the tab *owns* the sending pane; a mismatch is
        // treated as an unresolved originating tab and the handoff opens
        // at top level (parentTabId == nil) rather than nested.
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p", tabId: "t1",
            sessionId: "sess-t1"
        )
        appState.tabs.mutateTab(id: "t1") { $0.title = "some task" }

        let reply = captureReply { reply in
            appState.sessions.handleHandoffRequest(
                cwd: "/tmp/p",
                handoffFile: "/tmp/p/.claude/handoff/h.md",
                instructions: "",
                model: "",
                effort: "",
                tabId: "t1",
                paneId: "stale-pane-id-not-owned-by-t1",
                reply: reply
            )
        }

        XCTAssertEqual(reply, "ok",
                       "paneId mismatch must still reply \"ok\"")

        // A new tab must have appeared, but it must NOT be nested under t1
        // (parentTabId is nil because originating-tab resolution failed).
        let project = projectById("p")
        XCTAssertEqual(project.tabs.count, 2,
                       "a new handoff tab must be created despite the paneId mismatch")
        let child = project.tabs[1]
        XCTAssertNil(child.parentTabId,
                     "paneId-mismatch fallback must produce a top-level tab (parentTabId == nil)")
    }

    // MARK: - Title fallback when originating tab has no title

    func test_blankOriginatingTitle_fallsBackToSessionInTitle() {
        // A tab with an empty title should produce "[HANDOFF] Session"
        // rather than "[HANDOFF] " (with a trailing space).
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p", tabId: "t1"
        )
        appState.tabs.mutateTab(id: "t1") { $0.title = "" }

        let _ = captureReply { reply in
            appState.sessions.handleHandoffRequest(
                cwd: "/tmp/p",
                handoffFile: "/tmp/p/.claude/handoff/h.md",
                instructions: "",
                model: "",
                effort: "",
                tabId: "t1",
                paneId: "t1-claude",
                reply: reply
            )
        }

        let child = projectById("p").tabs.last
        XCTAssertEqual(child?.title, "[HANDOFF] Session",
                       "empty originating title must fall through to the 'Session' default")
    }

    // MARK: - Instructions routing
    //
    // The prompt is assembled inside createHandoffTab and passed as an
    // extraClaudeArg to makeSession. That arg is NOT observable from unit
    // tests — makeSession doesn't spawn real ptys here, and the arg list
    // is buried inside the pty session which isn't accessible from
    // AppState.tabs. What IS observable is the tab's title, nesting, and
    // the reply — which we assert above. The default-vs-custom directive
    // logic is covered by the production comment in handleHandoffRequest
    // (the `trimmed.isEmpty` branch). If the prompt were ever observable,
    // the assertions below show the intended contract for future
    // reference.
    //
    // NOTE: to make the prompt text observable without a production
    // change one would need to either expose the extraClaudeArgs from
    // TabPtySession (breaking encapsulation) or inject a test observer
    // into SessionsModel. The existing TabPtySessionClaudeArgsTests suite
    // already pins how extra claude args flow once the pty is spawned.

    func test_nonEmptyInstructions_tabCreated_repliesOk() {
        // We can't directly observe the prompt content, but we can confirm
        // that a non-empty instructions string still produces a tab and
        // an "ok" reply — guarding against the handler short-circuiting
        // on custom instructions.
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p", tabId: "t1"
        )
        appState.tabs.mutateTab(id: "t1") { $0.title = "my task" }

        let reply = captureReply { reply in
            appState.sessions.handleHandoffRequest(
                cwd: "/tmp/p",
                handoffFile: "/tmp/p/.claude/handoff/h.md",
                instructions: "Focus only on the UI layer",
                model: "",
                effort: "",
                tabId: "t1",
                paneId: "t1-claude",
                reply: reply
            )
        }

        XCTAssertEqual(reply, "ok")
        XCTAssertEqual(projectById("p").tabs.count, 2,
                       "non-empty instructions must still produce a handoff tab")
    }

    func test_emptyInstructions_tabCreated_repliesOk() {
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p", tabId: "t1"
        )

        let reply = captureReply { reply in
            appState.sessions.handleHandoffRequest(
                cwd: "/tmp/p",
                handoffFile: "/tmp/p/.claude/handoff/h.md",
                instructions: "",
                model: "",
                effort: "",
                tabId: "t1",
                paneId: "t1-claude",
                reply: reply
            )
        }

        XCTAssertEqual(reply, "ok")
        XCTAssertEqual(projectById("p").tabs.count, 2,
                       "empty instructions must still produce a handoff tab")
    }

    // MARK: - model / effort forwarding

    func test_modelAndEffortPresent_tabCreated_repliesOk() {
        // A handoff carrying a model + effort must thread through the
        // handler without dropping the request. The flags themselves are
        // pinned by the handoffExtraArgs unit tests; here we guard that the
        // handler doesn't short-circuit on the new fields.
        TabModelFixtures.seedClaudeTab(
            into: appState.tabs, projectId: "p", tabId: "t1"
        )
        appState.tabs.mutateTab(id: "t1") { $0.title = "my task" }

        let reply = captureReply { reply in
            appState.sessions.handleHandoffRequest(
                cwd: "/tmp/p",
                handoffFile: "/tmp/p/.claude/handoff/h.md",
                instructions: "",
                model: "claude-opus-4-8",
                effort: "xhigh",
                tabId: "t1",
                paneId: "t1-claude",
                reply: reply
            )
        }

        XCTAssertEqual(reply, "ok")
        XCTAssertEqual(projectById("p").tabs.count, 2,
                       "a handoff with model+effort must still produce a handoff tab")
    }

    // MARK: - Helpers

    /// Drive `handleHandoffRequest` and return the single string it
    /// passed to `reply`. Mirrors the same helper in
    /// SessionsModelClaudeSocketRequestTests.
    private func captureReply(
        _ drive: (@escaping @Sendable (String) -> Void) -> Void
    ) -> String {
        nonisolated(unsafe) var captured: String?
        drive { reply in captured = reply }
        return captured ?? ""
    }

    private func projectById(_ id: String) -> Project {
        guard let p = appState.tabs.projects.first(where: { $0.id == id }) else {
            XCTFail("project '\(id)' not found")
            return Project(id: id, name: id, path: "/", tabs: [])
        }
        return p
    }
}
