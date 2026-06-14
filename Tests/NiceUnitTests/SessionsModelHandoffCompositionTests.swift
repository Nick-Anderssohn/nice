//
//  SessionsModelHandoffCompositionTests.swift
//  NiceUnitTests
//
//  Pins the pure helpers introduced alongside the handoff feature:
//    • `SessionsModel.handoffTitle(forOriginatingTitle:)`
//    • `SessionsModel.handoffPrompt(handoffFile:instructions:)`
//    • `SessionsModel.handoffExtraArgs(model:effort:prompt:)`
//
//  Both are `nonisolated static`, so these tests need no AppState or
//  SessionsModel instance — they can call the helpers directly without any
//  actor hopping. Style mirrors SessionsModelHandoffRequestTests.
//

import Foundation
import XCTest
@testable import Nice

// No @MainActor needed — both helpers are nonisolated static.
final class SessionsModelHandoffCompositionTests: XCTestCase {

    // MARK: - handoffPrompt

    func test_handoffPrompt_emptyInstructions_containsHandoffFileAndDefaultDirective() {
        let file = "/tmp/proj/.claude/handoff/h.md"
        let result = SessionsModel.handoffPrompt(handoffFile: file, instructions: "")

        XCTAssertTrue(result.contains(file),
                      "prompt must reference the handoff file path")
        XCTAssertTrue(result.hasSuffix("Do not start working yet — once you have read it, wait for the user to tell you how to proceed."),
                      "empty instructions must fall back to the default wait-for-user directive")
    }

    func test_handoffPrompt_nonEmptyInstructions_containsCustomText_andNotDefaultDirective() {
        let file = "/tmp/proj/.claude/handoff/h.md"
        let customInstructions = "focus on the UI layer"
        let result = SessionsModel.handoffPrompt(
            handoffFile: file,
            instructions: customInstructions
        )

        XCTAssertTrue(result.contains(customInstructions),
                      "non-empty instructions must appear verbatim in the prompt")
        XCTAssertFalse(result.contains("wait for the user to tell you how to proceed"),
                       "custom instructions must replace, not join, the default directive")
    }

    func test_handoffPrompt_whitespaceOnlyInstructions_fallsBackToDefaultDirective() {
        // "   \n\t " is non-empty as a raw String but blank after trimming —
        // the previously-untested branch that would have let whitespace
        // bleed through as the directive.
        let file = "/tmp/proj/.claude/handoff/h.md"
        let result = SessionsModel.handoffPrompt(
            handoffFile: file,
            instructions: "   \n\t "
        )

        XCTAssertTrue(result.hasSuffix("Do not start working yet — once you have read it, wait for the user to tell you how to proceed."),
                      "whitespace-only instructions must fall back to the default directive")
        XCTAssertFalse(result.contains("   \n\t "),
                       "whitespace-only instructions must not appear in the prompt")
    }

    // MARK: - handoffTitle

    func test_handoffTitle_nilInput_returnsHandoffSession() {
        XCTAssertEqual(
            SessionsModel.handoffTitle(forOriginatingTitle: nil),
            "[HANDOFF] Session",
            "nil title must produce \"[HANDOFF] Session\""
        )
    }

    func test_handoffTitle_plainTitle_prefixesCorrectly() {
        XCTAssertEqual(
            SessionsModel.handoffTitle(forOriginatingTitle: "Fix top bar"),
            "[HANDOFF] Fix top bar",
            "plain title must be prefixed with \"[HANDOFF] \""
        )
    }

    func test_handoffTitle_alreadyPrefixedTitle_doesNotStackPrefix() {
        // A handoff fired from an existing "[HANDOFF] Fix top bar" tab
        // must produce "[HANDOFF] Fix top bar", not "[HANDOFF] [HANDOFF] Fix top bar".
        XCTAssertEqual(
            SessionsModel.handoffTitle(forOriginatingTitle: "[HANDOFF] Fix top bar"),
            "[HANDOFF] Fix top bar",
            "already-prefixed title must not stack the prefix"
        )
    }

    func test_handoffTitle_whitespaceOnlyInput_returnsHandoffSession() {
        // "   " trims to "" → falls back to "Session", not "[HANDOFF]    ".
        // This is the previously-untested edge case.
        XCTAssertEqual(
            SessionsModel.handoffTitle(forOriginatingTitle: "   "),
            "[HANDOFF] Session",
            "whitespace-only title must fall back to \"[HANDOFF] Session\""
        )
    }

    func test_handoffTitle_prefixPlusWhitespace_returnsHandoffSession() {
        // "[HANDOFF]    " strips to "    " which trims to "" → "Session".
        XCTAssertEqual(
            SessionsModel.handoffTitle(forOriginatingTitle: "[HANDOFF]    "),
            "[HANDOFF] Session",
            "\"[HANDOFF]\" + whitespace must fall back to \"[HANDOFF] Session\""
        )
    }

    // MARK: - handoffExtraArgs

    private let samplePrompt = "Read the handoff notes at /tmp/h.md. Wait."

    func test_handoffExtraArgs_bothPresent_emitsFlagsThenPromptInOrder() {
        // Order matters: flags must precede the positional prompt, and
        // --model must precede --effort to match the documented launch line
        // `claude --session-id <id> --model <m> --effort <e> "<prompt>"`.
        XCTAssertEqual(
            SessionsModel.handoffExtraArgs(
                model: "claude-opus-4-8",
                effort: "xhigh",
                prompt: samplePrompt
            ),
            ["--model", "claude-opus-4-8", "--effort", "xhigh", samplePrompt]
        )
    }

    func test_handoffExtraArgs_emptyModel_omitsModelFlag() {
        XCTAssertEqual(
            SessionsModel.handoffExtraArgs(model: "", effort: "xhigh", prompt: samplePrompt),
            ["--effort", "xhigh", samplePrompt],
            "an empty model must omit --model entirely (no empty-string arg)"
        )
    }

    func test_handoffExtraArgs_emptyEffort_omitsEffortFlag() {
        XCTAssertEqual(
            SessionsModel.handoffExtraArgs(model: "claude-opus-4-8", effort: "", prompt: samplePrompt),
            ["--model", "claude-opus-4-8", samplePrompt],
            "an empty effort must omit --effort entirely (no empty-string arg)"
        )
    }

    func test_handoffExtraArgs_bothEmpty_isJustThePrompt() {
        // The pre-feature behavior: a handoff with neither value known
        // launches exactly as before — `claude --session-id <id> "<prompt>"`.
        XCTAssertEqual(
            SessionsModel.handoffExtraArgs(model: "", effort: "", prompt: samplePrompt),
            [samplePrompt],
            "both empty must reproduce the original single-positional-arg launch"
        )
    }

    func test_handoffExtraArgs_promptIsAlwaysLast() {
        // Guards the invariant the launch path depends on: claude auto-runs
        // the trailing positional arg, so the prompt must never be reordered
        // ahead of a flag.
        for (model, effort) in [("opus", "max"), ("", "high"), ("sonnet", ""), ("", "")] {
            let args = SessionsModel.handoffExtraArgs(model: model, effort: effort, prompt: samplePrompt)
            XCTAssertEqual(args.last, samplePrompt,
                           "prompt must be the final element for model=\(model) effort=\(effort)")
        }
    }

    // MARK: - handoffExtraArgs composed into the real launch command
    //
    // handoffExtraArgs only matters because of the command line it produces
    // once TabPtySession.buildClaudeExecCommand splices it after
    // `--session-id`. Both are pure `nonisolated static`, so the composed
    // line is observable here (only the runtime pty spawn isn't). These pin
    // the end-to-end ordering + per-arg single-quoting the feature depends
    // on — a reorder of the flags relative to --session-id, or a quoting
    // regression, would slip past the per-helper tests.

    func test_composedLaunchCommand_modelAndEffort_afterSessionId_promptLast() {
        let args = SessionsModel.handoffExtraArgs(
            model: "claude-opus-4-8", effort: "xhigh", prompt: samplePrompt
        )
        let cmd = TabPtySession.buildClaudeExecCommand(
            claude: "/usr/local/bin/claude",
            mode: .new(id: "sess-1"),
            extraClaudeArgs: args,
            isOverride: false
        )
        XCTAssertEqual(
            cmd,
            "exec '/usr/local/bin/claude' --session-id 'sess-1' '--model' 'claude-opus-4-8' '--effort' 'xhigh' '\(samplePrompt)'"
        )
    }

    func test_composedLaunchCommand_neitherFlag_matchesPreFeatureLine() {
        // Back-compat: no model/effort ⇒ byte-for-byte the original
        // single-positional launch line (`claude --session-id <id> '<prompt>'`).
        let args = SessionsModel.handoffExtraArgs(model: "", effort: "", prompt: samplePrompt)
        let cmd = TabPtySession.buildClaudeExecCommand(
            claude: "/usr/local/bin/claude",
            mode: .new(id: "sess-1"),
            extraClaudeArgs: args,
            isOverride: false
        )
        XCTAssertEqual(
            cmd,
            "exec '/usr/local/bin/claude' --session-id 'sess-1' '\(samplePrompt)'"
        )
    }
}
