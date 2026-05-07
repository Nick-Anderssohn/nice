//
//  TabPtySessionClaudeEnvTests.swift
//  NiceUnitTests
//
//  Pins down the env-injection contract for Claude panes. The
//  UserPromptSubmit hook only fires its socket message when
//  NICE_SOCKET + NICE_PANE_ID are both in the env, so a regression
//  here silently breaks /clear, /compact, and /branch session-id
//  tracking — the saved id goes stale and the next quit/relaunch
//  resumes the wrong session. Asserting per-mode here would have
//  caught the `.new`/`.resume` gap that shipped to the dev build.
//

import XCTest
@testable import Nice

final class TabPtySessionClaudeEnvTests: XCTestCase {

    // MARK: - Always-injected keys

    func test_new_injectsTabPaneAndSocket() {
        let env = TabPtySession.buildClaudeExtraEnv(
            mode: .new(id: "session-uuid"),
            tabId: "tab-1",
            paneId: "tab-1-claude",
            socketPath: "/tmp/nice.sock",
            zdotdirPath: "/tmp/zdotdir",
            userZDotDir: nil
        )
        XCTAssertEqual(env["NICE_TAB_ID"], "tab-1")
        XCTAssertEqual(env["NICE_PANE_ID"], "tab-1-claude")
        XCTAssertEqual(env["NICE_SOCKET"], "/tmp/nice.sock")
        XCTAssertEqual(env["TERM_PROGRAM"], "ghostty")
    }

    func test_resume_injectsTabPaneAndSocket() {
        let env = TabPtySession.buildClaudeExtraEnv(
            mode: .resume(id: "session-uuid"),
            tabId: "tab-2",
            paneId: "tab-2-claude",
            socketPath: "/tmp/nice.sock",
            zdotdirPath: "/tmp/zdotdir",
            userZDotDir: nil
        )
        XCTAssertEqual(env["NICE_TAB_ID"], "tab-2")
        XCTAssertEqual(env["NICE_PANE_ID"], "tab-2-claude")
        XCTAssertEqual(env["NICE_SOCKET"], "/tmp/nice.sock")
    }

    func test_resumeDeferred_injectsTabPaneAndSocket() {
        let env = TabPtySession.buildClaudeExtraEnv(
            mode: .resumeDeferred(id: "session-uuid"),
            tabId: "tab-3",
            paneId: "tab-3-claude",
            socketPath: "/tmp/nice.sock",
            zdotdirPath: "/tmp/zdotdir",
            userZDotDir: nil
        )
        XCTAssertEqual(env["NICE_TAB_ID"], "tab-3")
        XCTAssertEqual(env["NICE_PANE_ID"], "tab-3-claude")
        XCTAssertEqual(env["NICE_SOCKET"], "/tmp/nice.sock")
    }

    func test_none_injectsTabPaneAndSocket() {
        let env = TabPtySession.buildClaudeExtraEnv(
            mode: .none,
            tabId: "tab-4",
            paneId: "tab-4-claude",
            socketPath: "/tmp/nice.sock",
            zdotdirPath: "/tmp/zdotdir",
            userZDotDir: nil
        )
        XCTAssertEqual(env["NICE_TAB_ID"], "tab-4")
        XCTAssertEqual(env["NICE_PANE_ID"], "tab-4-claude")
        XCTAssertEqual(env["NICE_SOCKET"], "/tmp/nice.sock")
    }

    // MARK: - resumeDeferred-only keys

    func test_resumeDeferred_injectsZdotdirAndPrefill() {
        let env = TabPtySession.buildClaudeExtraEnv(
            mode: .resumeDeferred(id: "abc-123"),
            tabId: "tab-5",
            paneId: "tab-5-claude",
            socketPath: "/tmp/nice.sock",
            zdotdirPath: "/tmp/zdotdir",
            userZDotDir: nil
        )
        XCTAssertEqual(env["ZDOTDIR"], "/tmp/zdotdir")
        XCTAssertEqual(env["NICE_PREFILL_COMMAND"], "claude --resume abc-123")
    }

    func test_new_doesNotInjectZdotdirOrPrefill() {
        let env = TabPtySession.buildClaudeExtraEnv(
            mode: .new(id: "abc-123"),
            tabId: "tab-6",
            paneId: "tab-6-claude",
            socketPath: "/tmp/nice.sock",
            zdotdirPath: "/tmp/zdotdir",
            userZDotDir: nil
        )
        XCTAssertNil(env["ZDOTDIR"])
        XCTAssertNil(env["NICE_PREFILL_COMMAND"])
    }

    func test_resume_doesNotInjectZdotdirOrPrefill() {
        let env = TabPtySession.buildClaudeExtraEnv(
            mode: .resume(id: "abc-123"),
            tabId: "tab-7",
            paneId: "tab-7-claude",
            socketPath: "/tmp/nice.sock",
            zdotdirPath: "/tmp/zdotdir",
            userZDotDir: nil
        )
        XCTAssertNil(env["ZDOTDIR"])
        XCTAssertNil(env["NICE_PREFILL_COMMAND"])
    }

    // MARK: - Optional inputs

    func test_nilSocketPath_omitsNiceSocket() {
        let env = TabPtySession.buildClaudeExtraEnv(
            mode: .new(id: "x"),
            tabId: "t",
            paneId: "p",
            socketPath: nil,
            zdotdirPath: "/tmp/zdotdir",
            userZDotDir: nil
        )
        XCTAssertNil(env["NICE_SOCKET"])
    }

    func test_resumeDeferred_nilZdotdir_omitsZdotdirButKeepsPrefill() {
        let env = TabPtySession.buildClaudeExtraEnv(
            mode: .resumeDeferred(id: "abc-123"),
            tabId: "t",
            paneId: "p",
            socketPath: "/tmp/nice.sock",
            zdotdirPath: nil,
            userZDotDir: nil
        )
        XCTAssertNil(env["ZDOTDIR"])
        XCTAssertEqual(env["NICE_PREFILL_COMMAND"], "claude --resume abc-123")
    }

    // MARK: - NICE_USER_ZDOTDIR (paired with ZDOTDIR for the
    // resumeDeferred path so the synthetic .zshenv stub can resolve
    // the user's intended ZDOTDIR).

    func test_resumeDeferred_setsNiceUserZdotdir() {
        let env = TabPtySession.buildClaudeExtraEnv(
            mode: .resumeDeferred(id: "abc-123"),
            tabId: "t",
            paneId: "p",
            socketPath: "/tmp/nice.sock",
            zdotdirPath: "/tmp/zdotdir",
            userZDotDir: "/Users/nick/.config/zsh"
        )
        XCTAssertEqual(env["NICE_USER_ZDOTDIR"], "/Users/nick/.config/zsh")
    }

    func test_resumeDeferred_nilUserZdotdir_setsEmptyString() {
        // Empty string is the on-the-wire signal for "Nice didn't
        // inherit a ZDOTDIR; fall back to sourcing ~/.zshenv". The
        // shell stub keys off `[[ -n ... ]]` which treats "" the same
        // as unset, so this is the cleanest contract.
        let env = TabPtySession.buildClaudeExtraEnv(
            mode: .resumeDeferred(id: "abc-123"),
            tabId: "t",
            paneId: "p",
            socketPath: "/tmp/nice.sock",
            zdotdirPath: "/tmp/zdotdir",
            userZDotDir: nil
        )
        XCTAssertEqual(env["NICE_USER_ZDOTDIR"], "")
    }

    func test_nonResumeDeferred_omitsNiceUserZdotdir() {
        // For .none/.new/.resume the Claude pane execs claude directly
        // — no zsh injection happens, so NICE_USER_ZDOTDIR is moot.
        // Keeping it out of those envs avoids leaking a Nice-internal
        // var into Claude's process env unnecessarily.
        for mode in [
            TabPtySession.ClaudeSessionMode.none,
            .new(id: "x"),
            .resume(id: "x"),
        ] {
            let env = TabPtySession.buildClaudeExtraEnv(
                mode: mode,
                tabId: "t",
                paneId: "p",
                socketPath: "/tmp/nice.sock",
                zdotdirPath: "/tmp/zdotdir",
                userZDotDir: "/Users/nick/.config/zsh"
            )
            XCTAssertNil(env["NICE_USER_ZDOTDIR"], "mode \(mode) leaked NICE_USER_ZDOTDIR")
        }
    }
}
