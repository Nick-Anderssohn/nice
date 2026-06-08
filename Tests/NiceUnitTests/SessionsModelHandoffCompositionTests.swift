//
//  SessionsModelHandoffCompositionTests.swift
//  NiceUnitTests
//
//  Pins the two pure helpers introduced alongside the handoff feature:
//    • `SessionsModel.handoffTitle(forOriginatingTitle:)`
//    • `SessionsModel.handoffPrompt(handoffFile:instructions:)`
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
        XCTAssertTrue(result.hasSuffix("Then continue the work described there."),
                      "empty instructions must fall back to the default directive")
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
        XCTAssertFalse(result.contains("Then continue the work described there."),
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

        XCTAssertTrue(result.hasSuffix("Then continue the work described there."),
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
}
