//
//  TerminalFontHelperTests.swift
//  NiceUnitTests
//
//  Pin the SF Mono → JetBrains Mono NL → system-monospaced fallback
//  chain in `TabPtySession.terminalFont(named:size:)` against drift
//  from future NSFont API changes.
//

import AppKit
import XCTest
@testable import Nice

@MainActor
final class TerminalFontHelperTests: XCTestCase {

    func test_nilName_returnsValidMonospacedFont() {
        let font = TabPtySession.terminalFont(named: nil, size: 12)
        XCTAssertEqual(font.pointSize, 12)
        XCTAssertTrue(
            Self.isMonospaced(font),
            "Default font chain should land on a monospaced face; got \(font.fontName)"
        )
    }

    func test_knownName_returnsThatFont() {
        // Menlo ships with every macOS release Nice supports, so it's
        // a safe "known good" choice that won't false-fail on CI.
        let font = TabPtySession.terminalFont(named: "Menlo-Regular", size: 14)
        XCTAssertEqual(font.fontName, "Menlo-Regular")
        XCTAssertEqual(font.pointSize, 14)
    }

    func test_unknownName_fallsBackToDefaultChain() {
        let font = TabPtySession.terminalFont(
            named: "DefinitelyNotAFont-XYZ-Regular",
            size: 11
        )
        XCTAssertEqual(font.pointSize, 11)
        XCTAssertTrue(
            Self.isMonospaced(font),
            "Unknown font should fall back to SF Mono / system monospaced; got \(font.fontName)"
        )
    }

    func test_sizePreservedExactly() {
        // Picker range runs 8…32pt; hit both endpoints and a fractional
        // value to catch any lossy rounding in the helper.
        XCTAssertEqual(TabPtySession.terminalFont(named: nil, size: 8).pointSize, 8)
        XCTAssertEqual(TabPtySession.terminalFont(named: nil, size: 32).pointSize, 32)
        XCTAssertEqual(
            TabPtySession.terminalFont(named: nil, size: 12.5).pointSize,
            12.5,
            accuracy: 0.01
        )
    }

    // MARK: -

    /// Ask the font whether every character advances by the same width
    /// — a structural property of monospaced faces and the only way
    /// to recognize `monospacedSystemFont` without relying on a name.
    private static func isMonospaced(_ font: NSFont) -> Bool {
        font.isFixedPitch
    }
}
