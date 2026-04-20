//
//  GhosttyThemeParserTests.swift
//  NiceUnitTests
//
//  Unit tests for Sources/Nice/Theme/GhosttyThemeParser.swift — verifies
//  the Ghostty `key = value` theme-file parser produces a correctly
//  populated `TerminalTheme` and throws deterministic, well-labelled
//  errors for malformed or incomplete input.
//

import Foundation
import XCTest
@testable import Nice

@MainActor
final class GhosttyThemeParserTests: XCTestCase {

    // MARK: - Fixtures

    private let testURL = URL(fileURLWithPath: "/tmp/test.ghostty")

    private func canonicalDraculaGhosttyFile() -> String {
        """
        background = 282a36
        foreground = f8f8f2
        cursor-color = f8f8f2
        selection-background = 44475a
        palette = 0=#21222c
        palette = 1=#ff5555
        palette = 2=#50fa7b
        palette = 3=#f1fa8c
        palette = 4=#bd93f9
        palette = 5=#ff79c6
        palette = 6=#8be9fd
        palette = 7=#f8f8f2
        palette = 8=#6272a4
        palette = 9=#ff6e6e
        palette = 10=#69ff94
        palette = 11=#ffffa5
        palette = 12=#d6acff
        palette = 13=#ff92df
        palette = 14=#a4ffff
        palette = 15=#ffffff
        """
    }

    /// Build a minimally-valid ghostty file with all 16 palette entries and
    /// required keys. Useful for tests that want to mutate one piece.
    private func minimalValidFile(
        includeBackground: Bool = true,
        includeForeground: Bool = true,
        includeCursor: Bool = true,
        includeSelection: Bool = true,
        extraLines: [String] = []
    ) -> String {
        var lines: [String] = []
        if includeBackground { lines.append("background = 1d1f21") }
        if includeForeground { lines.append("foreground = c5c8c6") }
        if includeCursor { lines.append("cursor-color = c5c8c6") }
        if includeSelection { lines.append("selection-background = 373b41") }
        for i in 0..<16 {
            lines.append("palette = \(i)=#000000")
        }
        lines.append(contentsOf: extraLines)
        return lines.joined(separator: "\n")
    }

    // MARK: - 1. Full Dracula round-trip

    func test_parsesFullDraculaFile() throws {
        let url = URL(fileURLWithPath: "/tmp/dracula.ghostty")
        let theme = try GhosttyThemeParser.parse(
            canonicalDraculaGhosttyFile(),
            id: "dracula",
            displayName: "Dracula",
            url: url
        )

        XCTAssertEqual(theme.id, "dracula")
        XCTAssertEqual(theme.displayName, "Dracula")
        XCTAssertEqual(theme.scope, .either)
        XCTAssertEqual(theme.source, .imported(url: url))

        XCTAssertEqual(theme.background, ThemeColor(hex: "#282a36")!)
        XCTAssertEqual(theme.foreground, ThemeColor(hex: "#f8f8f2")!)
        XCTAssertEqual(theme.cursor, ThemeColor(hex: "#f8f8f2")!)
        XCTAssertEqual(theme.selection, ThemeColor(hex: "#44475a")!)

        let expectedPalette: [ThemeColor] = [
            ThemeColor(hex: "#21222c")!,
            ThemeColor(hex: "#ff5555")!,
            ThemeColor(hex: "#50fa7b")!,
            ThemeColor(hex: "#f1fa8c")!,
            ThemeColor(hex: "#bd93f9")!,
            ThemeColor(hex: "#ff79c6")!,
            ThemeColor(hex: "#8be9fd")!,
            ThemeColor(hex: "#f8f8f2")!,
            ThemeColor(hex: "#6272a4")!,
            ThemeColor(hex: "#ff6e6e")!,
            ThemeColor(hex: "#69ff94")!,
            ThemeColor(hex: "#ffffa5")!,
            ThemeColor(hex: "#d6acff")!,
            ThemeColor(hex: "#ff92df")!,
            ThemeColor(hex: "#a4ffff")!,
            ThemeColor(hex: "#ffffff")!,
        ]
        XCTAssertEqual(theme.ansi, expectedPalette)
        XCTAssertEqual(theme.ansi.count, 16)
    }

    // MARK: - 2. Missing palette entries

    func test_missingPaletteEntries_throws() {
        let source = """
        background = 1d1f21
        foreground = c5c8c6
        palette = 0=#000000
        palette = 1=#111111
        palette = 2=#222222
        palette = 3=#333333
        palette = 4=#444444
        palette = 6=#666666
        palette = 8=#888888
        palette = 9=#999999
        palette = 10=#aaaaaa
        palette = 11=#bbbbbb
        palette = 12=#cccccc
        palette = 13=#dddddd
        palette = 14=#eeeeee
        palette = 15=#ffffff
        """
        XCTAssertThrowsError(
            try GhosttyThemeParser.parse(source, id: "x", displayName: "X", url: testURL)
        ) { error in
            guard let err = error as? GhosttyThemeParser.ParseError else {
                XCTFail("wrong error type: \(error)")
                return
            }
            XCTAssertEqual(err, .missingPalette(indices: [5, 7]))
        }
    }

    // MARK: - 3. Missing background

    func test_missingBackground_throws() {
        let source = minimalValidFile(includeBackground: false)
        XCTAssertThrowsError(
            try GhosttyThemeParser.parse(source, id: "x", displayName: "X", url: testURL)
        ) { error in
            XCTAssertEqual(
                error as? GhosttyThemeParser.ParseError,
                .missingRequiredKey(key: "background")
            )
        }
    }

    // MARK: - 4. Missing foreground

    func test_missingForeground_throws() {
        let source = minimalValidFile(includeForeground: false)
        XCTAssertThrowsError(
            try GhosttyThemeParser.parse(source, id: "x", displayName: "X", url: testURL)
        ) { error in
            XCTAssertEqual(
                error as? GhosttyThemeParser.ParseError,
                .missingRequiredKey(key: "foreground")
            )
        }
    }

    // MARK: - 5. Unknown keys ignored

    func test_ignoresUnknownKeys() throws {
        let source = minimalValidFile(extraLines: [
            "custom-key = whatever",
            "another-unknown = 12345",
        ])
        let theme = try GhosttyThemeParser.parse(
            source, id: "x", displayName: "X", url: testURL
        )
        XCTAssertEqual(theme.background, ThemeColor(hex: "#1d1f21")!)
    }

    // MARK: - 6. Blank lines + comments

    func test_blankLinesAndComments() throws {
        let source = """

        # This is a comment at the top

        background = 1d1f21

        # comment between entries
        foreground = c5c8c6
        cursor-color = c5c8c6
        selection-background = 373b41

        # palette follows

        palette = 0=#000000
        palette = 1=#111111
        palette = 2=#222222
        palette = 3=#333333
        palette = 4=#444444
        palette = 5=#555555
        palette = 6=#666666
        palette = 7=#777777
        palette = 8=#888888
        palette = 9=#999999
        palette = 10=#aaaaaa
        palette = 11=#bbbbbb
        palette = 12=#cccccc
        palette = 13=#dddddd
        palette = 14=#eeeeee
        palette = 15=#ffffff

        # trailing comment
        """
        let theme = try GhosttyThemeParser.parse(
            source, id: "x", displayName: "X", url: testURL
        )
        XCTAssertEqual(theme.background, ThemeColor(hex: "#1d1f21")!)
        XCTAssertEqual(theme.ansi.count, 16)
        XCTAssertEqual(theme.ansi[15], ThemeColor(hex: "#ffffff")!)
    }

    // MARK: - 7. Hex with and without `#`

    func test_hexWithAndWithoutHash() throws {
        let withHash = """
        background = #1d1f21
        foreground = #c5c8c6
        cursor-color = #c5c8c6
        selection-background = #373b41
        palette = 0=#000000
        palette = 1=#111111
        palette = 2=#222222
        palette = 3=#333333
        palette = 4=#444444
        palette = 5=#555555
        palette = 6=#666666
        palette = 7=#777777
        palette = 8=#888888
        palette = 9=#999999
        palette = 10=#aaaaaa
        palette = 11=#bbbbbb
        palette = 12=#cccccc
        palette = 13=#dddddd
        palette = 14=#eeeeee
        palette = 15=#ffffff
        """
        let withoutHash = """
        background = 1d1f21
        foreground = c5c8c6
        cursor-color = c5c8c6
        selection-background = 373b41
        palette = 0=000000
        palette = 1=111111
        palette = 2=222222
        palette = 3=333333
        palette = 4=444444
        palette = 5=555555
        palette = 6=666666
        palette = 7=777777
        palette = 8=888888
        palette = 9=999999
        palette = 10=aaaaaa
        palette = 11=bbbbbb
        palette = 12=cccccc
        palette = 13=dddddd
        palette = 14=eeeeee
        palette = 15=ffffff
        """
        let a = try GhosttyThemeParser.parse(
            withHash, id: "x", displayName: "X", url: testURL
        )
        let b = try GhosttyThemeParser.parse(
            withoutHash, id: "x", displayName: "X", url: testURL
        )
        XCTAssertEqual(a, b)
    }

    // MARK: - 8. Whitespace trimming

    func test_trimsWhitespace() throws {
        let source = """
          background  =   1d1f21
            foreground =c5c8c6
        cursor-color   =   c5c8c6
        selection-background = 373b41
            palette   =   0=#000000
        palette =  1=#111111
        palette = 2=#222222
        palette = 3=#333333
        palette = 4=#444444
        palette = 5=#555555
        palette = 6=#666666
        palette = 7=#777777
        palette = 8=#888888
        palette = 9=#999999
        palette = 10=#aaaaaa
        palette = 11=#bbbbbb
        palette = 12=#cccccc
        palette = 13=#dddddd
        palette = 14=#eeeeee
        palette = 15=#ffffff
        """
        let theme = try GhosttyThemeParser.parse(
            source, id: "x", displayName: "X", url: testURL
        )
        XCTAssertEqual(theme.background, ThemeColor(hex: "#1d1f21")!)
        XCTAssertEqual(theme.foreground, ThemeColor(hex: "#c5c8c6")!)
        XCTAssertEqual(theme.ansi[0], ThemeColor(hex: "#000000")!)
    }

    // MARK: - 9. Case-insensitive hex

    func test_caseInsensitiveHex() throws {
        let upper = minimalValidFile().replacingOccurrences(
            of: "background = 1d1f21",
            with: "background = 1D1F21"
        )
        let lower = minimalValidFile()
        let a = try GhosttyThemeParser.parse(
            upper, id: "x", displayName: "X", url: testURL
        )
        let b = try GhosttyThemeParser.parse(
            lower, id: "x", displayName: "X", url: testURL
        )
        XCTAssertEqual(a, b)
    }

    // MARK: - 10. Invalid hex — too short

    func test_invalidHex_throws_tooShort() {
        // `background =` is on line 1; ensure reporting matches.
        let source = """
        background = abc
        foreground = c5c8c6
        """
        XCTAssertThrowsError(
            try GhosttyThemeParser.parse(source, id: "x", displayName: "X", url: testURL)
        ) { error in
            XCTAssertEqual(
                error as? GhosttyThemeParser.ParseError,
                .invalidHex(value: "abc", lineNumber: 1)
            )
        }
    }

    // MARK: - 11. Invalid hex — too long

    func test_invalidHex_throws_tooLong() {
        let source = """
        foreground = c5c8c6
        background = abcdef12
        """
        XCTAssertThrowsError(
            try GhosttyThemeParser.parse(source, id: "x", displayName: "X", url: testURL)
        ) { error in
            XCTAssertEqual(
                error as? GhosttyThemeParser.ParseError,
                .invalidHex(value: "abcdef12", lineNumber: 2)
            )
        }
    }

    // MARK: - 12. Invalid hex — non-hex chars

    func test_invalidHex_throws_nonHexChars() {
        let source = """
        background = zzzzzz
        foreground = c5c8c6
        """
        XCTAssertThrowsError(
            try GhosttyThemeParser.parse(source, id: "x", displayName: "X", url: testURL)
        ) { error in
            XCTAssertEqual(
                error as? GhosttyThemeParser.ParseError,
                .invalidHex(value: "zzzzzz", lineNumber: 1)
            )
        }
    }

    // MARK: - 13. Duplicate palette — last wins

    func test_duplicatePaletteEntry_lastWins() throws {
        // Start with a minimal valid file where index 0 = #000000, then
        // re-declare palette 0 twice more. Final value should be #222222.
        var lines: [String] = [
            "background = 1d1f21",
            "foreground = c5c8c6",
            "palette = 0=#111111",
        ]
        for i in 1..<16 {
            lines.append("palette = \(i)=#000000")
        }
        lines.append("palette = 0=#222222")
        let source = lines.joined(separator: "\n")

        let theme = try GhosttyThemeParser.parse(
            source, id: "x", displayName: "X", url: testURL
        )
        XCTAssertEqual(theme.ansi[0], ThemeColor(hex: "222222")!)
    }

    // MARK: - 14. Palette index out of range

    func test_paletteIndexOutOfRange_throws() {
        // Line 3 carries the out-of-range palette.
        let source = """
        background = 1d1f21
        foreground = c5c8c6
        palette = 99=#ffffff
        """
        XCTAssertThrowsError(
            try GhosttyThemeParser.parse(source, id: "x", displayName: "X", url: testURL)
        ) { error in
            XCTAssertEqual(
                error as? GhosttyThemeParser.ParseError,
                .paletteIndexOutOfRange(index: 99, lineNumber: 3)
            )
        }
    }

    // MARK: - 15. Optional cursor + selection are nil when absent

    func test_optionalCursorAndSelection_nil() throws {
        let source = minimalValidFile(
            includeCursor: false,
            includeSelection: false
        )
        let theme = try GhosttyThemeParser.parse(
            source, id: "x", displayName: "X", url: testURL
        )
        XCTAssertNil(theme.cursor)
        XCTAssertNil(theme.selection)
    }

    // MARK: - 16. Imported source and scope

    func test_importedSourceAndScope() throws {
        let url = URL(fileURLWithPath: "/tmp/foo.ghostty")
        let theme = try GhosttyThemeParser.parse(
            minimalValidFile(),
            id: "foo",
            displayName: "Foo",
            url: url
        )
        XCTAssertEqual(theme.scope, .either)
        XCTAssertEqual(theme.source, .imported(url: url))
    }
}
